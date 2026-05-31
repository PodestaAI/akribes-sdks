from __future__ import annotations

import asyncio
import inspect
import json
import logging
import random
import uuid
from typing import Any, AsyncGenerator, Callable, Optional

import httpx
from httpx_sse import aconnect_sse

from akribes_sdk.errors import AkribesConnectionError
from akribes_sdk.models import EngineEvent, HubEvent
from akribes_sdk.resources._base import Resource, ProjectResource  # global — SSE /events endpoint
from akribes_sdk._transport import (
    WsHandshakeError,
    transport_preference,
    ws_stream,
)
from akribes_sdk.workflow_events import WorkflowEventT, to_workflow_event

logger = logging.getLogger("akribes_sdk")


# Canonical SDK-wide SSE backoff curve (#1182): exponential with full
# jitter, base 1s, cap 30s. Mirrors `_backoff_s` in `client.py` and
# `retry_backoff` in the Rust SDK.
_BACKOFF_BASE_S = 1.0
_BACKOFF_CAP_S = 30.0


def _sse_backoff_s(attempt: int) -> float:
    if attempt <= 0:
        return 0.0
    exponent = min(attempt - 1, 20)
    exp_s = min(_BACKOFF_BASE_S * (2 ** exponent), _BACKOFF_CAP_S)
    return random.random() * exp_s


_HEARTBEAT_INTERVAL_S = 30.0


def _hub_event_from_obj(obj: dict[str, Any]) -> HubEvent | None:
    """Decode a HubEvent JSON dict into the typed :class:`HubEvent`.

    Shared between the WS frame path (one HubEvent per frame) and the SSE
    batch path (a list of HubEvent dicts per `event: batch`). Returns
    ``None`` for shapes that don't look like a HubEvent so the caller can
    skip non-event frames without raising.
    """
    if not isinstance(obj, dict):
        return None
    ty = obj.get("type")
    payload = obj.get("payload")
    if ty is None:
        return None
    return HubEvent(type=ty, payload=payload)


class _Subscription:
    """Async context manager owning a project-scoped client registration + transport.

    Obtained via :meth:`EventsProjectScoped.subscribe`. While active it:

    * POSTs ``/projects/{id}/clients`` to register the SDK client with the
      server (which gates broadcast delivery).
    * Opens an event transport (WS preferred, SSE fallback per
      :func:`akribes_sdk._transport.transport_preference`) that enqueues
      :class:`HubEvent` objects.
    * On the SSE path, runs a 30 s POST /heartbeat loop with capped-exponential
      backoff on failures. On the WS path, the dedicated heartbeat is skipped
      — the server sends a Ping every ~15 s and the `websockets` client
      auto-replies with Pong, which doubles as the liveness signal.

    The :class:`_Subscription` is an async iterator — ``async for evt in sub:``
    yields :class:`HubEvent` as the server delivers them. It is also a context
    manager that cleans up on exit (cancels background tasks, best-effort
    DELETE ``/clients/{client_id}``).
    """

    def __init__(
        self,
        api: Any,
        project_id: int,
        interests: list[dict] | None,
    ) -> None:
        self._api = api
        self._project_id = project_id
        self._interests = interests or []
        self._client_id: str = str(uuid.uuid4())
        self._queue: asyncio.Queue[HubEvent | None] = asyncio.Queue()
        self._heartbeat_task: asyncio.Task[None] | None = None
        self._stream_task: asyncio.Task[None] | None = None
        # Set by `_run_stream` once it decides which transport to use, so
        # `__aenter__` knows whether to start the dedicated heartbeat loop.
        # WS uses native Ping/Pong (server pings every ~15 s, websockets
        # auto-replies) so the POST /heartbeat loop is redundant. See #1368.
        self._transport_chosen: asyncio.Event = asyncio.Event()
        self._active_transport: str | None = None

    async def __aenter__(self) -> "_Subscription":
        # Register the client with the server.
        base = self._api._base_url
        client_name = getattr(self._api, "name", "python-sdk")
        await self._api._request(
            "POST",
            f"{base}/projects/{self._project_id}/clients",
            json={
                "id": self._client_id,
                "name": client_name,
                "interests": self._interests,
            },
        )
        # Kick off the streaming task. It probes WS first (subject to
        # AKRIBES_TRANSPORT) and falls back to SSE on handshake failure.
        self._stream_task = asyncio.create_task(self._run_stream())
        # Wait briefly for the stream to decide which transport it landed on
        # so we only start the dedicated heartbeat loop on the SSE path.
        # If the probe is still running when the timeout fires, default to
        # starting the heartbeat — it's a no-op on top of WS Pings if the
        # transport flips after this point.
        try:
            await asyncio.wait_for(self._transport_chosen.wait(), timeout=2.0)
        except asyncio.TimeoutError:
            pass
        if self._active_transport != "ws":
            self._heartbeat_task = asyncio.create_task(self._run_heartbeat())
        return self

    async def __aexit__(self, *exc: Any) -> None:
        # Cancel background tasks.
        for task in (self._heartbeat_task, self._stream_task):
            if task is not None:
                task.cancel()
                try:
                    await task
                except (asyncio.CancelledError, Exception):
                    pass
        self._heartbeat_task = None
        self._stream_task = None
        # Sentinel to unblock any waiting consumers.
        await self._queue.put(None)
        # Best-effort DELETE /clients/{id}.
        try:
            base = self._api._base_url
            await self._api._request("DELETE", f"{base}/clients/{self._client_id}")
        except Exception:
            pass

    def __aiter__(self) -> "_Subscription":
        return self

    async def __anext__(self) -> HubEvent:
        while True:
            evt = await self._queue.get()
            if evt is None:
                raise StopAsyncIteration
            return evt

    async def _run_heartbeat(self) -> None:
        consecutive_failures = 0
        base = self._api._base_url
        while True:
            try:
                res = await self._api._client._http.post(
                    f"{base}/heartbeat",
                    json={"client_id": self._client_id},
                    headers={
                        **self._api._client._auth_headers(),
                        **self._api._client._propagator_headers(),
                    },
                )
                if res.status_code in (401, 403):
                    logger.warning(
                        "Subscription heartbeat rejected: HTTP %d (token may be revoked)",
                        res.status_code,
                    )
                    # Don't retry on auth failures.
                    return
                if res.status_code >= 400:
                    logger.warning("Subscription heartbeat rejected: HTTP %d", res.status_code)
                    consecutive_failures += 1
                else:
                    consecutive_failures = 0
            except (httpx.ConnectError, httpx.TimeoutException, httpx.HTTPError) as e:
                logger.warning("Subscription heartbeat failed: %s", e)
                consecutive_failures += 1

            backoff = _sse_backoff_s(consecutive_failures)
            await asyncio.sleep(_HEARTBEAT_INTERVAL_S + backoff)

    async def _run_stream(self) -> None:
        """Background event listener: WS-first with SSE fallback.

        Picks the transport once per call to :meth:`__aenter__`:

        * ``AKRIBES_TRANSPORT=ws``  — WS only, propagate on handshake failure.
        * ``AKRIBES_TRANSPORT=sse`` — SSE only, never attempts WS.
        * unset (default)           — try WS, fall back to SSE on the first
          handshake error, then stay on SSE for the lifetime of this
          subscription. Subsequent reconnects use the same transport so we
          don't oscillate.

        The chosen transport is exposed via ``self._active_transport`` so
        :meth:`__aenter__` can skip the dedicated heartbeat loop on WS.
        """
        pref = transport_preference()
        if pref != "sse":
            try:
                await self._run_ws()
                # Clean close (server hung up) — exit the task.
                return
            except WsHandshakeError as exc:
                if pref == "ws":
                    logger.error("WS handshake required (AKRIBES_TRANSPORT=ws) but failed: %s", exc)
                    self._active_transport = "ws"
                    self._transport_chosen.set()
                    raise
                logger.info("WS unavailable (%s) — falling back to SSE", exc)
            except asyncio.CancelledError:
                raise
            except Exception:
                # Once we're actually running on WS (handshake succeeded) and
                # we crash hard mid-stream, don't silently flip to SSE: let
                # `_run_sse`'s own retry logic kick in only if the user
                # explicitly opted out of WS for this session.
                if pref == "ws":
                    self._active_transport = "ws"
                    self._transport_chosen.set()
                    raise
                logger.exception("WS subscription crashed — falling back to SSE")

        self._active_transport = "sse"
        self._transport_chosen.set()
        await self._run_sse()

    async def _run_ws(self) -> None:
        """WS path for :meth:`_run_stream`. Enqueues HubEvents from the WS frame stream.

        Uses a `ready` event from :func:`ws_stream` so the transport selection
        flips to ``"ws"`` the instant the handshake succeeds — even before any
        events flow. ``__aenter__`` needs that signal to know whether to start
        the dedicated POST /heartbeat loop.
        """
        base = self._api._base_url
        token = getattr(self._api, "token", None)
        ws_ready = asyncio.Event()

        async def _watch_ready() -> None:
            try:
                await ws_ready.wait()
            except asyncio.CancelledError:
                return
            self._active_transport = "ws"
            self._transport_chosen.set()

        ready_task = asyncio.create_task(_watch_ready())

        # Reuse the shared helper. It handles its own reconnect/backoff,
        # so this loop just funnels frames into the queue.
        gen = ws_stream(
            base_url=base,
            token=token,
            project_id=self._project_id,
            script_name=None,
            ready=ws_ready,
            on_lag=lambda dropped: logger.warning(
                "Subscription WS lagged: %d events dropped", dropped
            ),
        )
        try:
            async for obj in gen:
                evt = _hub_event_from_obj(obj)
                if evt is not None:
                    await self._queue.put(evt)
        finally:
            ready_task.cancel()
            try:
                await ready_task
            except (asyncio.CancelledError, Exception):
                pass
            await gen.aclose()

    async def _run_sse(self) -> None:
        """Background SSE listener: enqueues HubEvents from the server broadcast."""
        last_event_id: str | None = None
        attempt = 0
        base = self._api._base_url
        token = getattr(self._api, "token", None)

        while True:
            params: dict[str, Any] = {"project_id": str(self._project_id)}
            if last_event_id is not None:
                params["last_event_id"] = last_event_id
            headers: dict[str, str] = {}
            # Bearer rides in the Authorization header so long-lived
            # service-token secrets don't end up in reverse-proxy access
            # logs / OTel `http.url` span attributes.
            if token:
                headers["Authorization"] = f"Bearer {token}"
            if last_event_id is not None:
                headers["Last-Event-ID"] = last_event_id

            try:
                async with aconnect_sse(
                    self._api._client._sse_http,
                    "GET",
                    f"{base}/events",
                    params=params,
                    headers=headers,
                ) as sse:
                    attempt = 0
                    async for event in sse.aiter_sse():
                        if event.id:
                            last_event_id = event.id
                        if event.event not in ("batch", "", "message"):
                            logger.debug("ignoring unknown SSE event type: %r", event.event)
                            continue
                        for item in json.loads(event.data):
                            await self._queue.put(HubEvent(type=item["type"], payload=item["payload"]))
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                attempt += 1
                delay = _sse_backoff_s(attempt)
                logger.warning(
                    "Subscription SSE disconnected (attempt %d), reconnecting in %.2fs: %s",
                    attempt, delay, exc,
                )
                await asyncio.sleep(delay)
                continue

            # Clean close — stop iterating.
            return


class Events(Resource):

    async def stream(
        self,
        script_name: str | None = None,
        *,
        project_id: int | None = None,
        ready: asyncio.Event | None = None,
        reconnect: bool = True,
        max_reconnect_attempts: int = 5,
    ) -> AsyncGenerator[HubEvent, None]:
        """Yield real-time :class:`HubEvent` objects from the server broadcast.

        Transport selection (#1368)
        ---------------------------
        WebSocket (``GET /events/ws``) is preferred — one bidirectional
        channel, server-driven Ping/Pong keepalive, lower per-event overhead.
        SSE (``GET /events``) is the fallback when the WS handshake fails or
        ``AKRIBES_TRANSPORT=sse`` is set in the environment. ``AKRIBES_TRANSPORT=ws``
        forces WS and propagates the handshake error instead of falling back.

        Reuses the shared SSE HTTP client (on the SSE path) so connections
        are pooled across streams and ``client.close()`` cleans up properly.

        If *ready* is provided, it is set once the transport handshake
        finishes — callers can await it to avoid racing subsequent writes
        (e.g. subscribe-before-POST) against subscription establishment.

        Parameters
        ----------
        reconnect:
            When True (default), transparently reconnects on transport drops
            with capped exponential backoff (base 1s, cap 30s, jitter).
            Sends ``Last-Event-ID`` on every reconnect so the server can
            backfill events emitted during the gap (#1101, #1095).
        max_reconnect_attempts:
            Number of consecutive failed reconnects to tolerate before
            raising :class:`AkribesConnectionError`. Resets on each
            successful connect.
        """
        # Resolve project_id once so both transports use the same value.
        _pid = project_id
        if _pid is None:
            _pid = getattr(self._api, "project_id", None)

        pref = transport_preference()
        if pref != "sse":
            # WS attempt. On a clean fall-through, drop to SSE; on `ws` (forced)
            # propagate the handshake error so callers see a real failure.
            try:
                async for evt in self._ws_stream(
                    script_name=script_name,
                    project_id=_pid,
                    ready=ready,
                    reconnect=reconnect,
                    max_reconnect_attempts=max_reconnect_attempts,
                ):
                    yield evt
                return
            except WsHandshakeError as exc:
                if pref == "ws":
                    raise AkribesConnectionError(f"WS handshake failed: {exc}") from exc
                logger.info("WS unavailable (%s) — falling back to SSE", exc)
            except asyncio.CancelledError:
                raise

        last_event_id: str | None = None
        attempt = 0
        first_connect = True
        ready_fired = ready is None

        while True:
            params: dict[str, Any] = {}
            # project_id is optional for the global events endpoint (hub-level).
            # Callers may pass it to filter; EventsProjectScoped injects it automatically.
            _pid = project_id
            if _pid is None:
                # Attempt to read from _api if it's a _ProjectApiClient (project-scoped usage).
                _pid = getattr(self._api, "project_id", None)
            if _pid is not None:
                params["project_id"] = str(_pid)
            if script_name:
                params["script_name"] = script_name
            if last_event_id is not None:
                params["last_event_id"] = last_event_id

            headers: dict[str, str] = {}
            # Bearer rides in the Authorization header so long-lived
            # service-token secrets don't end up in reverse-proxy access
            # logs / OTel `http.url` span attributes.
            if self._api.token:
                headers["Authorization"] = f"Bearer {self._api.token}"
            if last_event_id is not None:
                # SSE-spec mechanism. The server reads `Last-Event-ID`
                # from either the header or `?last_event_id=` in the
                # query string; sending it in the header keeps the URL
                # short and avoids interleaving with the auth posture.
                headers["Last-Event-ID"] = last_event_id

            try:
                async with aconnect_sse(
                    self._api._sse_http,
                    "GET",
                    f"{self._base_url}/events",
                    params=params,
                    headers=headers,
                ) as sse:
                    # Fire `ready` exactly once across all reconnects.
                    if not ready_fired and ready is not None:
                        ready.set()
                        ready_fired = True
                    attempt = 0  # successful connect — reset backoff
                    first_connect = False
                    async for event in sse.aiter_sse():
                        # Track the last server-supplied event id (= seq).
                        if event.id:
                            last_event_id = event.id
                        # Unnamed events default to "message" per SSE spec.
                        if event.event not in ("batch", "", "message"):
                            logger.debug(
                                "ignoring unknown SSE event type: %r", event.event
                            )
                            continue
                        for item in json.loads(event.data):
                            yield HubEvent(type=item["type"], payload=item["payload"])
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                if not reconnect:
                    raise
                attempt += 1
                if attempt > max_reconnect_attempts:
                    raise AkribesConnectionError(
                        f"SSE stream failed after {attempt - 1} reconnect attempts: {exc}"
                    ) from exc
                delay = _sse_backoff_s(attempt)
                logger.warning(
                    "SSE disconnected (attempt %d/%d), reconnecting in %.2fs: %s",
                    attempt,
                    max_reconnect_attempts,
                    delay,
                    exc,
                )
                await asyncio.sleep(delay)
                continue

            # Stream ended cleanly (server closed without error). The
            # consumer gets a natural `StopAsyncIteration` here; they can
            # re-await `stream()` to reconnect if they want. Auto-reconnect
            # only fires on transport DROPS (the `except` branch above) —
            # this preserves the legacy contract where the generator
            # returns when the server closes the stream.
            return

    async def _ws_stream(
        self,
        *,
        script_name: str | None,
        project_id: int | None,
        ready: asyncio.Event | None,
        reconnect: bool,
        max_reconnect_attempts: int,
    ) -> AsyncGenerator[HubEvent, None]:
        """WS transport for :meth:`stream`. Yields typed :class:`HubEvent`s.

        Raises :class:`WsHandshakeError` on the first failed handshake so the
        caller (``stream``) can fall back to SSE per the env-var policy. Once
        the handshake succeeds, transport-level drops are recovered by
        :func:`ws_stream` itself using the same backoff curve as SSE.
        """
        token = getattr(self._api, "token", None)
        gen = ws_stream(
            base_url=self._base_url,
            token=token,
            project_id=project_id,
            script_name=script_name,
            ready=ready,
            reconnect=reconnect,
            max_reconnect_attempts=max_reconnect_attempts,
            on_lag=lambda dropped: logger.warning(
                "Events WS lagged: %d events dropped", dropped
            ),
        )
        try:
            async for obj in gen:
                evt = _hub_event_from_obj(obj)
                if evt is not None:
                    yield evt
        finally:
            await gen.aclose()

    async def engine_events(
        self,
        script_name: str,
        *,
        ready: asyncio.Event | None = None,
        execution_id: str | None = None,
    ) -> AsyncGenerator[EngineEvent, None]:
        """Async iterator of :class:`EngineEvent`s for *script_name*.

        Preferred over :meth:`on_execution` (callback style) for modern
        ``async for`` consumption and natural cancellation via task
        cancellation.

        When *execution_id* is provided, only events stamped with the
        matching ``execution_id`` are yielded — required when more than
        one caller may concurrently run the same script (the broadcast
        channel is per-script, not per-run, so without filtering both
        callers would see each other's events). Server payloads that
        predate the field (``execution_id`` is omitted) pass through
        for back-compat; current servers always stamp it.

        Example::

            async for evt in client.events.engine_events("summarize"):
                if evt.type == "AgentOutput":
                    print(evt.payload["chunk"], end="")
                elif evt.type == "WorkflowEnd":
                    break
        """
        async for hub_event in self.stream(script_name=script_name, ready=ready):
            if (
                hub_event.type == "Execution"
                and hub_event.payload.get("script_name") == script_name
            ):
                if execution_id is not None:
                    evt_eid = hub_event.payload.get("execution_id")
                    if evt_eid is not None and evt_eid != execution_id:
                        continue
                engine_event = hub_event.payload["event"]
                yield EngineEvent(
                    type=engine_event["type"],
                    payload=engine_event.get("payload"),
                )

    async def typed_engine_events(
        self, script_name: str
    ) -> AsyncGenerator[WorkflowEventT, None]:
        """Like :meth:`engine_events` but yields typed :class:`WorkflowEvent`s.

        Thin wrapper that runs every raw event through
        :func:`akribes_sdk.workflow_events.to_workflow_event`. Unknown wire
        variants are surfaced as :class:`Other` rather than raising, so
        consumers stay forward-compatible with server additions.

        Example::

            async for evt in client.events.typed_engine_events("summarize"):
                if evt.kind == "agent_chunk":
                    print(evt.chunk, end="")
                elif evt.kind == "end":
                    break
        """
        async for raw in self.engine_events(script_name):
            yield to_workflow_event(raw)

    def on_execution(
        self,
        script_name: str,
        callback: Callable[[EngineEvent], Any],
        *,
        on_error: Optional[Callable[[Exception], Any]] = None,
    ) -> Callable[[], bool]:
        """Subscribe to execution events for *script_name*. Returns an unsubscribe callable.

        Parameters
        ----------
        on_error:
            Optional callback invoked when the background listener task dies
            due to an unhandled exception. Receives the exception instance.
        """

        async def _listen() -> None:
            try:
                async for hub_event in self.stream(script_name=script_name):
                    if hub_event.type == "Execution" and hub_event.payload.get("script_name") == script_name:
                        engine_event = hub_event.payload["event"]
                        evt = EngineEvent(type=engine_event["type"], payload=engine_event.get("payload"))
                        if inspect.iscoroutinefunction(callback):
                            await callback(evt)
                        else:
                            callback(evt)
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                logger.exception("on_execution listener for %r crashed", script_name)
                if on_error is not None:
                    if inspect.iscoroutinefunction(on_error):
                        await on_error(exc)
                    else:
                        on_error(exc)

        task = asyncio.ensure_future(_listen())
        return task.cancel

    def on_schema_change(
        self,
        script_name: str,
        callback: Callable[[int, str | None], Any],
        *,
        on_error: Optional[Callable[[Exception], Any]] = None,
    ) -> Callable[[], bool]:
        """Subscribe to schema changes for *script_name*.

        Marks the script as "broken" in the client's contract state so
        pre-dispatch validation in ``run()`` will raise
        :class:`ScriptSchemaChangedError`. Returns an unsubscribe callable.

        Parameters
        ----------
        on_error:
            Optional callback invoked when the background listener task dies
            due to an unhandled exception. Receives the exception instance.
        """

        async def _listen() -> None:
            try:
                async for hub_event in self.stream():
                    if (
                        hub_event.type == "Registry"
                        and hub_event.payload.get("type") == "ScriptUpdated"
                        and hub_event.payload.get("payload", {}).get("script_name") == script_name
                    ):
                        # Mark as broken so run() will raise before POSTing
                        self._api._broken_scripts.add(script_name)
                        inner = hub_event.payload["payload"]
                        if inspect.iscoroutinefunction(callback):
                            await callback(inner["version_id"], inner.get("channel"))
                        else:
                            callback(inner["version_id"], inner.get("channel"))
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                logger.exception("on_schema_change listener for %r crashed", script_name)
                if on_error is not None:
                    if inspect.iscoroutinefunction(on_error):
                        await on_error(exc)
                    else:
                        on_error(exc)

        task = asyncio.ensure_future(_listen())
        return task.cancel

    def on_change(
        self,
        script_name: str,
        callback: Callable[[int, str | None], Any],
        *,
        on_error: Optional[Callable[[Exception], Any]] = None,
    ) -> Callable[[], bool]:
        """Subscribe to version/channel changes for *script_name*. Returns an unsubscribe callable.

        Parameters
        ----------
        on_error:
            Optional callback invoked when the background listener task dies
            due to an unhandled exception. Receives the exception instance.
        """

        async def _listen() -> None:
            try:
                async for hub_event in self.stream():
                    if (
                        hub_event.type == "Registry"
                        and hub_event.payload.get("type") == "ScriptUpdated"
                        and hub_event.payload.get("payload", {}).get("script_name") == script_name
                    ):
                        inner = hub_event.payload["payload"]
                        if inspect.iscoroutinefunction(callback):
                            await callback(inner["version_id"], inner.get("channel"))
                        else:
                            callback(inner["version_id"], inner.get("channel"))
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                logger.exception("on_change listener for %r crashed", script_name)
                if on_error is not None:
                    if inspect.iscoroutinefunction(on_error):
                        await on_error(exc)
                    else:
                        on_error(exc)

        task = asyncio.ensure_future(_listen())
        return task.cancel


class EventsProjectScoped(ProjectResource):
    """Project-scoped events facade. Mounted on ProjectHandle.events.

    Thin forwarding wrapper: delegates to the global ``Events`` class but
    injects this project's ID as the default ``project_id`` filter.
    Phase 4 will redesign this into a proper subscribe() context manager
    with project-scope enforcement; for now it's a pass-through.
    """

    def _global_events(self) -> Events:
        """Return a global Events instance backed by the same underlying client."""
        from akribes_sdk.resources._base import _ApiClient
        return Events(_ApiClient(self._api._client))

    async def stream(
        self,
        script_name: str | None = None,
        *,
        ready: asyncio.Event | None = None,
        reconnect: bool = True,
        max_reconnect_attempts: int = 5,
    ):
        """Delegate to the global Events stream with this project's context."""
        async for event in self._global_events().stream(
            script_name=script_name,
            ready=ready,
            reconnect=reconnect,
            max_reconnect_attempts=max_reconnect_attempts,
        ):
            yield event

    async def engine_events(self, script_name: str, *, ready: asyncio.Event | None = None):
        """Async iterator of :class:`EngineEvent`s for *script_name*."""
        async for evt in self._global_events().engine_events(script_name, ready=ready):
            yield evt

    def on_execution(self, script_name: str, callback, *, on_error=None):
        return self._global_events().on_execution(script_name, callback, on_error=on_error)

    def on_change(self, script_name: str, callback, *, on_error=None):
        return self._global_events().on_change(script_name, callback, on_error=on_error)

    def on_schema_change(self, script_name: str, callback, *, on_error=None):
        return self._global_events().on_schema_change(script_name, callback, on_error=on_error)

    def subscribe(self, interests: list[dict] | None = None) -> _Subscription:
        """Return a :class:`_Subscription` async context manager for this project.

        Registers the SDK client with the server on enter, starts a heartbeat,
        and begins streaming :class:`HubEvent` objects. Unregisters on exit.

        Example::

            async with proj.events.subscribe(interests=[...]) as sub:
                async for hub_event in sub:
                    ...  # process events
        """
        return _Subscription(self._api, self._api.project_id, interests)
