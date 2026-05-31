"""Layer 3 of the event story: :class:`RunStream`.

A run-until-done handle that is simultaneously async-iterable (``async for``
yields typed :class:`WorkflowEvent`s) and callback-registry-friendly
(``run.on.output(cb)``). It hides the "subscribe THEN POST then converge"
boilerplate every caller would otherwise rewrite.

Transport: routed through :meth:`Events.stream` which prefers WebSocket
(``GET /events/ws``) and falls back to SSE on handshake failure, controlled
by the ``AKRIBES_TRANSPORT`` env var (``ws`` / ``sse`` / unset for auto).
``RunStream`` itself is transport-agnostic — the WS-vs-SSE choice is made
inside :mod:`akribes_sdk._transport`.

Dispatch strategy
-----------------
Events are buffered as soon as the transport starts delivering them. No
callbacks are fired until the caller reaches an interaction point
(:meth:`RunStream.output` or starting ``async for``). That lets users
register callbacks *after* ``await run_stream(...)`` returns but before
they actually consume — which is the natural Python pattern::

    run = await client.executions.run_stream("s")
    run.on.output(lambda c: print(c.chunk, end=""))
    result = await run.output()   # callbacks flush here, then live events
"""
from __future__ import annotations

import asyncio
import inspect
import logging
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, AsyncIterator, Callable

from akribes_sdk.errors import AkribesError, AkribesTimeoutError
from akribes_sdk.workflow_events import (
    AgentChunk,
    Breakpoint,
    Checkpoint,
    RuntimeEnd,
    RuntimeError as RuntimeErrorEvent,
    RuntimeStart,
    RuntimeStderr,
    RuntimeStdout,
    RuntimeStep,
    RuntimeWorkflowEvent,
    TaskEnd,
    ToolApproval,
    WorkflowError,
    WorkflowEventT,
    category_of,
    reduce_runtime_events_async,
)

if TYPE_CHECKING:
    from akribes_sdk.resources.executions import Executions

logger = logging.getLogger("akribes_sdk")


Callback = Callable[[Any], Any]
Unsubscribe = Callable[[], None]


@dataclass
class _CallbackRegistry:
    """Internal: callback buckets keyed by category/variant."""

    output: list[Callback] = field(default_factory=list)
    task_end: list[Callback] = field(default_factory=list)
    suspend: list[Callback] = field(default_factory=list)
    error: list[Callback] = field(default_factory=list)
    any_: list[Callback] = field(default_factory=list)


async def _maybe_await(cb: Callback, arg: Any) -> None:
    try:
        result = cb(arg)
        if inspect.isawaitable(result):
            await result
    except Exception:
        # Never let a user callback kill the stream.
        logger.exception("RunStream callback raised")


class RunStreamCallbacks:
    """Callback registry attached to a :class:`RunStream`.

    Each method takes a sync OR async callback and returns an
    ``Unsubscribe`` callable. Exceptions inside callbacks are logged but
    never propagate to the stream consumer.
    """

    __slots__ = ("_reg",)

    def __init__(self, registry: _CallbackRegistry) -> None:
        self._reg = registry

    @staticmethod
    def _add(bucket: list[Callback], cb: Callback) -> Unsubscribe:
        bucket.append(cb)

        def _off() -> None:
            try:
                bucket.remove(cb)
            except ValueError:
                pass

        return _off

    def output(self, cb: Callable[[AgentChunk], Any]) -> Unsubscribe:
        """Fire on every :class:`AgentChunk` (streaming token)."""
        return self._add(self._reg.output, cb)  # type: ignore[arg-type]

    def task_end(self, cb: Callable[[TaskEnd], Any]) -> Unsubscribe:
        """Fire when a task finishes (one event per task)."""
        return self._add(self._reg.task_end, cb)  # type: ignore[arg-type]

    def suspend(
        self, cb: Callable[[Checkpoint | ToolApproval | Breakpoint], Any]
    ) -> Unsubscribe:
        """Fire on any human-in-the-loop pause."""
        return self._add(self._reg.suspend, cb)  # type: ignore[arg-type]

    def error(self, cb: Callable[[WorkflowError], Any]) -> Unsubscribe:
        """Fire on a terminal :class:`WorkflowError`."""
        return self._add(self._reg.error, cb)  # type: ignore[arg-type]

    def any(self, cb: Callable[[WorkflowEventT], Any]) -> Unsubscribe:
        """Fire on every event, including :class:`Other` catch-alls."""
        return self._add(self._reg.any_, cb)  # type: ignore[arg-type]


class RunStream:
    """Handle for a single workflow execution.

    Obtain one via :meth:`akribes_sdk.resources.executions.Executions.run_stream`.
    The SDK subscribes to the SSE stream *before* issuing ``POST /run`` so
    the initial events are never dropped.

    Consumption patterns::

        # 1. async-for the typed events
        async for evt in run:
            ...

        # 2. callbacks + await output
        run.on.output(lambda c: print(c.chunk, end=""))
        run.on.error(lambda e: logger.error(e.message))
        result = await run.output()

        # 3. cancel early
        await run.cancel()

    ``run.output()`` resolves with the :class:`End` ``output`` payload on a
    clean finish and raises :class:`AkribesError` on :class:`WorkflowError` or
    ``cancel()``. It raises :class:`AkribesTimeoutError` if *timeout* elapses.
    """

    def __init__(self, executions: "Executions", script_name: str) -> None:
        self._executions = executions
        self._script_name = script_name
        # Shared buffer of typed events; all consumers index into it.
        self._events: list[WorkflowEventT] = []
        self._event_signal = asyncio.Event()
        self._listener_task: asyncio.Task[None] | None = None
        self._sse_ready = asyncio.Event()
        self._closed = False
        self._done = asyncio.Event()
        self._final_output: Any = None
        self._final_error: BaseException | None = None
        self._registry = _CallbackRegistry()
        self.on = RunStreamCallbacks(self._registry)
        # Dispatcher state (lazy): the callback dispatcher starts on the
        # first user interaction (output() or __aiter__) to give callers a
        # chance to register callbacks post-construction.
        self._dispatcher_task: asyncio.Task[None] | None = None
        self._dispatch_idx = 0

        self.execution_id: str | None = None
        """Execution id assigned by the server.

        ``None`` only during the narrow window between :class:`RunStream`
        construction and the inner ``POST /run`` returning. Consumers
        obtained via :meth:`Executions.run_stream` always observe a
        populated value because that helper awaits the POST before
        returning the stream. (#1226)
        """

    # ── Internal bootstrap ─────────────────────────────────────────────

    async def _start(
        self,
        *,
        inputs: dict[str, Any] | None,
        channel: str,
        triggered_by: str | None,
        breakpoint_lines: list[int] | None,
    ) -> None:
        """Subscribe to the event stream (WS-first, SSE fallback), then POST to ``/run``."""
        self._listener_task = asyncio.create_task(self._listen())
        try:
            await asyncio.wait_for(self._sse_ready.wait(), timeout=30.0)
        except asyncio.TimeoutError as exc:
            await self.cancel()
            raise AkribesError("Event-stream subscription timed out") from exc

        result = await self._executions.run(
            self._script_name,
            inputs=inputs,
            channel=channel,
            triggered_by=triggered_by,
            breakpoint_lines=breakpoint_lines,
        )
        self.execution_id = result.execution_id

    async def _listen(self) -> None:
        """Pump the SSE stream into :attr:`_events` as typed events.

        Uses the ``ready`` handshake in :meth:`Events.stream` so
        :attr:`_sse_ready` is only set once the HTTP GET has returned 2xx
        and the SSE stream is live — critical to avoid losing the first
        events if the server starts emitting them the instant ``/run``
        returns.

        Filters incoming envelopes by ``execution_id`` once
        :attr:`execution_id` becomes known (after the ``POST /run``
        response lands in :meth:`_start`). Until then every event is
        appended — there's no way to distinguish ours from a concurrent
        run of the same script before the run-row exists. Once the id
        is known, mismatched events are dropped on the fly. Wire
        payloads that predate the ``execution_id`` stamp fall through
        (back-compat against older servers).
        """
        from akribes_sdk.workflow_events import to_workflow_event

        # Use the lower-level `stream` directly so we can inspect the
        # raw envelope's ``execution_id`` for cross-execution filtering.
        # `engine_events` exposes an `execution_id=` kwarg, but we don't
        # know the id at subscription time (the POST /run that returns
        # it happens AFTER subscription) — so we filter inline once the
        # id is known.
        agen = None
        try:
            client = self._executions._api._client
            from akribes_sdk.resources.events import Events
            from akribes_sdk.resources._base import _ApiClient
            events = Events(_ApiClient(client))
            agen = events.stream(
                script_name=self._script_name, ready=self._sse_ready
            )
            async for hub_event in agen:
                if self._closed:
                    return
                if hub_event.type != "Execution":
                    continue
                if hub_event.payload.get("script_name") != self._script_name:
                    continue
                # Once we know our execution_id, drop events from a
                # concurrent run of the same script. Mirrors TS
                # `RunStream.routeRaw`. Pre-#1042 servers don't stamp
                # the field — those events pass through unchanged.
                if self.execution_id is not None:
                    evt_eid = hub_event.payload.get("execution_id")
                    if evt_eid is not None and evt_eid != self.execution_id:
                        continue
                engine_event = hub_event.payload["event"]
                from akribes_sdk.models import EngineEvent
                raw = EngineEvent(
                    type=engine_event["type"],
                    payload=engine_event.get("payload"),
                )
                evt = to_workflow_event(raw)
                self._events.append(evt)
                self._event_signal.set()
                if evt.kind == "end":
                    self._final_output = evt.output  # type: ignore[attr-defined]
                    self._done.set()
                    break
                if evt.kind == "error":
                    msg = evt.message  # type: ignore[attr-defined]
                    self._final_error = AkribesError(msg)
                    self._done.set()
                    break
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            logger.exception("RunStream listener crashed")
            self._final_error = exc
            self._done.set()
        finally:
            # Always release any awaiter and clean up the generator.
            self._sse_ready.set()
            self._event_signal.set()
            if agen is not None:
                try:
                    await agen.aclose()
                except Exception:
                    pass

    async def _wait_for_event_at(self, idx: int) -> bool:
        """Block until an event is available at *idx* or the stream ends.

        Returns True if an event is available at *idx*, False if the stream
        is done with no more events.
        """
        while idx >= len(self._events):
            if self._done.is_set() and idx >= len(self._events):
                return False
            self._event_signal.clear()
            # Wait for either a new event or the done flag.
            await self._event_signal.wait()
        return True

    async def _dispatch_callbacks(self) -> None:
        """Consume :attr:`_events` into the callback registry.

        Started lazily; runs until ``_done`` is set and the buffer is drained.
        """
        reg = self._registry
        while True:
            has = await self._wait_for_event_at(self._dispatch_idx)
            if not has:
                return
            evt = self._events[self._dispatch_idx]
            self._dispatch_idx += 1
            for cb in list(reg.any_):
                await _maybe_await(cb, evt)
            cat = category_of(evt)
            if cat == "output":
                for cb in list(reg.output):
                    await _maybe_await(cb, evt)
            elif evt.kind == "task_end":
                for cb in list(reg.task_end):
                    await _maybe_await(cb, evt)
            elif cat == "suspend":
                for cb in list(reg.suspend):
                    await _maybe_await(cb, evt)
            elif cat == "error":
                for cb in list(reg.error):
                    await _maybe_await(cb, evt)

    def _ensure_dispatcher(self) -> asyncio.Task[None]:
        """Start the callback dispatcher task if not already running, else
        return the existing one so callers can await full drainage."""
        if self._dispatcher_task is None:
            self._dispatcher_task = asyncio.create_task(self._dispatch_callbacks())
        return self._dispatcher_task

    # ── Public API ─────────────────────────────────────────────────────

    def __aiter__(self) -> AsyncIterator[WorkflowEventT]:
        return self._iter()

    async def _iter(self) -> AsyncIterator[WorkflowEventT]:
        # Kick off callback dispatch so registered callbacks fire while the
        # caller is async-for-iterating too.
        self._ensure_dispatcher()
        idx = 0
        while True:
            has = await self._wait_for_event_at(idx)
            if not has:
                return
            yield self._events[idx]
            idx += 1

    async def runtime_events(self) -> AsyncIterator[RuntimeWorkflowEvent]:
        """Async-iterate only the runtime variants in this stream.

        Yields :class:`RuntimeStart` / :class:`RuntimeStdout` /
        :class:`RuntimeStderr` / :class:`RuntimeEnd` / :class:`RuntimeError`
        in arrival order, skipping every other event type. Useful for live
        rendering of streaming container code output without having to
        match on every workflow event.

        Example::

            async for evt in run.runtime_events():
                if isinstance(evt, RuntimeStdout):
                    print(evt.chunk, end="")
        """
        async for evt in self._iter():
            if isinstance(evt, (RuntimeStart, RuntimeStdout, RuntimeStderr, RuntimeEnd, RuntimeErrorEvent)):
                yield evt

    async def runtime_steps(self) -> AsyncIterator[RuntimeStep]:
        """Async-iterate aggregated :class:`RuntimeStep` records.

        Folds the underlying :meth:`runtime_events` stream through
        :func:`reduce_runtime_events_async`, yielding one step per
        completed (or errored) runtime invocation. Each step carries the
        accumulated stdout/stderr plus the terminal exit code / duration
        or error kind / message.

        Example::

            async for step in run.runtime_steps():
                if step.status == "completed":
                    save(step.task_name, step.stdout)
                else:
                    log_error(step.error_kind, step.error_message)
        """
        async for step in reduce_runtime_events_async(self.runtime_events()):
            yield step

    async def output(self, *, timeout: float | None = None) -> Any:
        """Await the final workflow output.

        Starts the callback dispatcher (so previously-registered callbacks
        fire for every buffered + future event). Resolves with
        :class:`End`'s ``output`` payload. Raises :class:`AkribesError` on an
        :class:`WorkflowError`, or :class:`AkribesTimeoutError` on *timeout*.
        """
        dispatcher = self._ensure_dispatcher()
        try:
            if timeout is None:
                await self._done.wait()
            else:
                await asyncio.wait_for(self._done.wait(), timeout=timeout)
        except asyncio.TimeoutError as exc:
            raise AkribesTimeoutError(
                f"RunStream for {self._script_name} timed out after {timeout}s",
                execution_id=self.execution_id or None,
            ) from exc

        # Wait for the dispatcher to finish emitting callbacks for the
        # buffered tail so users see every event before output() returns.
        # Deliberately do NOT swallow CancelledError — cooperative cancellation
        # of the caller must propagate.
        try:
            await asyncio.wait_for(asyncio.shield(dispatcher), timeout=5.0)
        except asyncio.TimeoutError:
            pass
        except asyncio.CancelledError:
            raise
        except Exception:
            logger.debug("dispatcher finished with exception", exc_info=True)

        if self._final_error is not None:
            raise self._final_error
        return self._final_output

    async def cancel(self) -> None:
        """Tear down the SSE subscription without waiting for ``End``.

        Safe to call multiple times. Does not cancel the execution on the
        server — use :meth:`Executions.cancel` for that.
        """
        if self._closed:
            return
        self._closed = True
        if self._listener_task is not None and not self._listener_task.done():
            self._listener_task.cancel()
            try:
                await self._listener_task
            except asyncio.CancelledError:
                pass
            except Exception:
                logger.debug("listener finished with exception", exc_info=True)
        if not self._done.is_set():
            self._final_error = AkribesError("RunStream cancelled")
            self._done.set()
        self._event_signal.set()
        # Drain (or abort) the dispatcher so it doesn't linger as an orphan
        # task after cancel(). With _done set + _event_signal poked it will
        # exit on its own; give it a short window then cancel as a fallback.
        if self._dispatcher_task is not None and not self._dispatcher_task.done():
            try:
                await asyncio.wait_for(self._dispatcher_task, timeout=1.0)
            except asyncio.TimeoutError:
                self._dispatcher_task.cancel()
                try:
                    await self._dispatcher_task
                except (asyncio.CancelledError, Exception):
                    pass
            except asyncio.CancelledError:
                raise
            except Exception:
                logger.debug("dispatcher finished with exception", exc_info=True)

    async def __aenter__(self) -> "RunStream":
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.cancel()


__all__ = [
    "Callback",
    "RunStream",
    "RunStreamCallbacks",
    "Unsubscribe",
]
