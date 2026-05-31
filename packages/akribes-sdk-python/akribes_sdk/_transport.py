"""Transport-selection helpers for the events stream (#1368).

The Python SDK started life with a Server-Sent Events stream (`GET /events`).
PR #1458 added a WebSocket sibling (`GET /events/ws`) that carries the same
filtered `HubEvent` payload over a single bidirectional channel with WS-native
ping/pong keepalive — which lets us:

* Drop the per-subscription `POST /heartbeat` loop (the WS handshake itself is
  the "I am still here" signal; the server pings every ~15 s, the client's
  websocket library auto-replies with Pong).
* Avoid SSE's per-event HTTP overhead and frame-buffering quirks.

WS is preferred when available; SSE remains the fallback for transports / proxies
that don't speak WS. Selection is controlled by:

* ``AKRIBES_TRANSPORT=ws``  — force WS, raise on handshake failure.
* ``AKRIBES_TRANSPORT=sse`` — force SSE, never attempt WS.
* unset (default)            — try WS first, fall back to SSE on handshake
  failure (then "sticky" for the lifetime of this generator — we don't keep
  re-probing WS on every reconnect).

Wire shape
----------
The server (`crates/akribes-server/src/handlers/execution/ws.rs`) sends one JSON
object per WS text frame, where each object is either a full `HubEvent` or a
synthetic lag notification ``{"type":"lagged","dropped":N}`` that mirrors the
SSE side-channel.

This module is internal — public API stays on :class:`Events` /
:class:`RunStream`. The transport choice is an implementation detail.
"""
from __future__ import annotations

import asyncio
import json
import logging
import os
import random
from typing import Any, AsyncGenerator, Awaitable, Callable

logger = logging.getLogger("akribes_sdk")


# Mirrors `_backoff_s` in client.py / `_sse_backoff_s` in resources/events.py
# so WS reconnects use exactly the same curve as SSE — the issue's acceptance
# criterion ("Reconnect on WS drop matches the current SSE behaviour … same
# exp-jitter backoff curve via `_backoff_s`"). Duplicated here rather than
# imported to avoid a circular `client → events → _transport → client` cycle.
_BACKOFF_BASE_S = 1.0
_BACKOFF_CAP_S = 30.0


def _backoff_s(attempt: int) -> float:
    if attempt <= 0:
        return 0.0
    exponent = min(attempt - 1, 20)
    exp_s = min(_BACKOFF_BASE_S * (2 ** exponent), _BACKOFF_CAP_S)
    return random.random() * exp_s


# Public env var name. Documented in client.py / events.py docstrings.
_TRANSPORT_ENV = "AKRIBES_TRANSPORT"


def transport_preference() -> str:
    """Return one of ``"ws"``, ``"sse"``, or ``"auto"`` (the default).

    ``"auto"`` means "try WS first, fall back to SSE on handshake failure".
    """
    raw = os.environ.get(_TRANSPORT_ENV, "").strip().lower()
    if raw in ("ws", "websocket", "websockets"):
        return "ws"
    if raw in ("sse",):
        return "sse"
    return "auto"


def http_to_ws_url(base_url: str) -> str:
    """Map an ``http[s]://...`` base URL to its ``ws[s]://...`` counterpart."""
    if base_url.startswith("https://"):
        return "wss://" + base_url[len("https://"):]
    if base_url.startswith("http://"):
        return "ws://" + base_url[len("http://"):]
    # No scheme — assume ws (mostly a fallback for malformed/test inputs).
    return base_url


class WsHandshakeError(Exception):
    """WS handshake failed — caller should fall back to SSE if allowed.

    Wraps the underlying error so callers can decide based on the transport
    preference whether to fall back or propagate.
    """

    def __init__(self, message: str, *, status: int | None = None) -> None:
        super().__init__(message)
        self.status = status


async def ws_stream(
    *,
    base_url: str,
    token: str | None,
    project_id: int | None,
    script_name: str | None,
    execution_id: str | None = None,
    extra_headers: dict[str, str] | None = None,
    ready: asyncio.Event | None = None,
    reconnect: bool = True,
    max_reconnect_attempts: int = 5,
    on_lag: Callable[[int], Awaitable[None] | None] | None = None,
) -> AsyncGenerator[dict[str, Any], None]:
    """Yield raw `HubEvent`-shaped dicts from `GET /events/ws`.

    Mirrors :meth:`akribes_sdk.resources.events.Events.stream` for the WS
    transport. Yields the parsed JSON object from each text frame; the
    caller is responsible for mapping that into :class:`HubEvent` (or its
    own typed shape).

    Lag frames (``{"type":"lagged","dropped":N}``) invoke *on_lag* and are
    NOT yielded — they're transport-level signals, not events.

    Auth
    ----
    The server reads the bearer token from either the ``Authorization`` request
    header or the ``?token=`` query param. We use the query-param path for
    parity with the SSE transport (browsers can't set headers on
    ``EventSource``; SDKs use the same path so the server-side allow-list stays
    in one place).

    Reconnect
    ---------
    Same capped-exponential-backoff curve as SSE (base 1 s, cap 30 s, full
    jitter). On every reconnect we re-send the highest ``seq`` we've seen via
    ``?last_event_id=``; the server logs the cursor (replay itself is #1101).
    """
    # `websockets` is a runtime dep (added to pyproject.toml in this commit).
    # Import locally so a stripped install that somehow omits the extra still
    # boots the SDK; the import error fires only when WS is actually attempted.
    try:
        import warnings as _warnings

        import websockets
        from websockets.exceptions import (
            ConnectionClosed,
            InvalidStatus,
            WebSocketException,
        )
        # `InvalidStatusCode` was renamed to `InvalidStatus` in websockets 13;
        # the old name is still importable but deprecated. Suppress the
        # warning during the optional import so users don't see noise from
        # us probing for compat. Keep both in the except clause so older
        # runtimes still surface a WsHandshakeError instead of crashing on import.
        with _warnings.catch_warnings():
            _warnings.simplefilter("ignore", DeprecationWarning)
            try:
                from websockets.exceptions import InvalidStatusCode  # type: ignore[attr-defined]
            except ImportError:
                InvalidStatusCode = InvalidStatus  # type: ignore[misc, assignment]
    except ImportError as exc:  # pragma: no cover — defensive
        raise WsHandshakeError(
            "websockets library not installed; install akribes-sdk[ws] "
            "or `pip install websockets`"
        ) from exc

    ws_base = http_to_ws_url(base_url.rstrip("/"))
    last_event_id: int | None = None
    attempt = 0
    # `handshake_succeeded` flips True once any connection has produced at
    # least one event (or held the socket open long enough that we trust
    # the server speaks WS). Reconnect / backoff only applies AFTER the
    # first successful handshake — the initial handshake-failure case must
    # raise `WsHandshakeError` so `Events.stream` can fall back to SSE.
    handshake_succeeded = False
    ready_fired = ready is None

    while True:
        params: list[tuple[str, str]] = []
        if project_id is not None:
            params.append(("project_id", str(project_id)))
        if script_name:
            params.append(("script_name", script_name))
        if execution_id:
            params.append(("execution_id", execution_id))
        if token:
            params.append(("token", token))
        if last_event_id is not None:
            params.append(("last_event_id", str(last_event_id)))
        # `urlencode` handles escaping for script names and tokens; the
        # remaining values (project ids, last_event_id) are numeric strings.
        from urllib.parse import urlencode
        query = urlencode(params)
        url = f"{ws_base}/events/ws"
        if query:
            url = f"{url}?{query}"

        # Pass the Authorization header too. The server's `extract_token`
        # accepts both, but some intermediate proxies strip query strings from
        # logs — header keeps the token off URL-format access logs while the
        # query-param mirror keeps browser parity working.
        extra_headers_dict: dict[str, str] = dict(extra_headers or {})
        if token:
            extra_headers_dict.setdefault("Authorization", f"Bearer {token}")

        try:
            # websockets 12+ accepts `additional_headers`; older releases used
            # `extra_headers`. We probe via a try/except to stay compatible on
            # both — the public CI matrix doesn't pin a specific minor.
            try:
                ws_ctx = websockets.connect(  # type: ignore[attr-defined]
                    url,
                    additional_headers=extra_headers_dict or None,
                    ping_interval=20,
                    ping_timeout=20,
                    close_timeout=5,
                    max_size=None,  # event payloads can be large (long agent chunks)
                )
            except TypeError:
                ws_ctx = websockets.connect(  # type: ignore[attr-defined]
                    url,
                    extra_headers=list(extra_headers_dict.items()) or None,
                    ping_interval=20,
                    ping_timeout=20,
                    close_timeout=5,
                    max_size=None,
                )
        except Exception as exc:
            # `websockets.connect(...)` returns a context manager synchronously
            # — failure here is purely client-side (bad URL, dns, etc.).
            raise WsHandshakeError(f"websocket connect setup failed: {exc}") from exc

        try:
            async with ws_ctx as ws:
                # First successful handshake: fire `ready`, reset attempts,
                # commit to the WS transport (no more falling back to SSE).
                if not ready_fired and ready is not None:
                    ready.set()
                    ready_fired = True
                attempt = 0
                handshake_succeeded = True

                async for raw in ws:
                    if isinstance(raw, (bytes, bytearray)):
                        # Server only sends text frames per the wire-shape
                        # contract — ignore the rare binary frame defensively.
                        logger.debug("WS: dropping unexpected binary frame")
                        continue
                    try:
                        obj = json.loads(raw)
                    except json.JSONDecodeError:
                        logger.warning("WS: failed to JSON-decode text frame: %r", raw[:200])
                        continue

                    if isinstance(obj, dict) and obj.get("type") == "lagged":
                        dropped = int(obj.get("dropped") or 0)
                        logger.warning("WS: server reported %d lagged events", dropped)
                        if on_lag is not None:
                            res = on_lag(dropped)
                            if asyncio.iscoroutine(res):
                                await res
                        continue

                    # Capture a `seq` cursor if the payload carries one.
                    # The HubEvent serialization doesn't always include a
                    # top-level `seq`, so this is best-effort; the server
                    # will still backfill via Last-Event-ID once #1101 ships.
                    if isinstance(obj, dict):
                        seq = obj.get("seq")
                        if isinstance(seq, int):
                            last_event_id = seq

                    yield obj  # type: ignore[misc]
        except asyncio.CancelledError:
            raise
        except (InvalidStatus, InvalidStatusCode) as exc:
            # Handshake-time HTTP error — fall back to SSE if the caller
            # wants to. The server returns 401/403 on bad auth, 503 on
            # saturation, 404 on a too-old server.
            status = getattr(getattr(exc, "response", None), "status_code", None) or getattr(exc, "status_code", None)
            raise WsHandshakeError(f"WS handshake rejected: {exc}", status=status) from exc
        except (ConnectionClosed, WebSocketException) as exc:
            # If the handshake never succeeded, treat ANY WS-level error as a
            # handshake failure so `Events.stream` can fall back to SSE.
            # Different websockets versions raise different exceptions for
            # "the server isn't speaking WS at all" (InvalidStatus on a
            # rejected HTTP/1.1 upgrade, ConnectionClosedError on a brutal
            # mid-handshake close, InvalidMessage on a malformed reply) —
            # we collapse them to one outcome.
            if not handshake_succeeded:
                raise WsHandshakeError(f"WS handshake failed: {exc}") from exc
            if not reconnect:
                return
            attempt += 1
            if attempt > max_reconnect_attempts:
                raise
            delay = _backoff_s(attempt)
            logger.warning(
                "WS disconnected (attempt %d/%d), reconnecting in %.2fs: %s",
                attempt,
                max_reconnect_attempts,
                delay,
                exc,
            )
            await asyncio.sleep(delay)
            continue
        except OSError as exc:
            # DNS / refused / reset → fall back to SSE on the first attempt
            # (no server-side handshake even happened, so we treat this
            # like an "unavailable" handshake error rather than retry under
            # the auto policy). If reconnect was already in progress
            # (handshake succeeded earlier), keep the WS path open.
            if not handshake_succeeded:
                raise WsHandshakeError(f"WS transport unavailable: {exc}") from exc
            if not reconnect:
                raise
            attempt += 1
            if attempt > max_reconnect_attempts:
                raise
            delay = _backoff_s(attempt)
            await asyncio.sleep(delay)
            continue
        else:
            # Server closed cleanly — exit the generator. Symmetric with
            # the SSE path's "return on clean close" semantics.
            return


__all__ = [
    "WsHandshakeError",
    "http_to_ws_url",
    "transport_preference",
    "ws_stream",
]
