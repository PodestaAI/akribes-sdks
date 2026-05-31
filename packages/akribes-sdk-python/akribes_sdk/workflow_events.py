"""Typed ``WorkflowEvent`` — Layer 2 of the engine event story.

Layer 1 is the raw :class:`akribes_sdk.models.EngineEvent` (escape hatch).
Layer 2 is this module: frozen ``@dataclass`` variants covering the
high-traffic server variants, plus a forward-compatible :class:`Other`
catch-all for anything the SDK doesn't recognise yet.
Layer 3 is :mod:`akribes_sdk.run_stream` which builds on top of this.

Callers match on ``evt.kind`` (a ``Literal`` discriminator) and get fully
typed snake_case fields without writing their own wire-format adapters.

Example::

    async for evt in client.events.typed_engine_events("my_script"):
        match evt.kind:
            case "agent_chunk":
                print(evt.chunk, end="")
            case "end":
                print("done:", evt.output)
            case "error":
                raise RuntimeError(evt.message)
"""
from __future__ import annotations

import dataclasses
from dataclasses import dataclass, field
from typing import Any, AsyncIterable, AsyncIterator, Callable, Iterable, Iterator, Literal, Union

from akribes_sdk.models import (
    DagPositionTrigger,
    EngineEvent,
    SuspendTrigger,
    TokenUsage,
    _TriggerBase,
    parse_suspend_trigger,
)


WorkflowErrorKind = Literal[
    "RateLimit",
    "AuthError",
    "TokenLimit",
    # #1296: legacy umbrella retained for back-compat; new producers emit
    # one of the four status-specific kinds below.
    "ServerError",
    "ServerError500",
    "BadGateway502",
    "ServiceUnavailable503",
    "GatewayTimeout504",
    "NetworkError",
    "ParseError",
    "Cancelled",
    "ScriptError",
]
"""String tag describing a :class:`WorkflowError`'s failure class.

Note: the SDK also exports :class:`akribes_sdk.models.ErrorKind` as a ``str``
enum with the same underlying values (plus ``is_transient``/``is_fatal``
helpers). The enum remains the canonical name; ``WorkflowErrorKind`` is
the narrower Literal used by the typed-event discriminator.
"""


# ────────────────────────────────────────────────────────────────────────
# Variants
# ────────────────────────────────────────────────────────────────────────


@dataclass(frozen=True, slots=True)
class Start:
    total_tasks: int
    kind: Literal["start"] = "start"


@dataclass(frozen=True, slots=True)
class WorkflowTotals:
    """Aggregate token + cost rollup carried on `End` events (issue #1173).

    Mirrors :class:`akribes_core::event::WorkflowTotals`. Defaults to all-zero
    when reading a legacy (pre-#1173) bare-value `WorkflowEnd` payload.
    """
    total_input_tokens: int = 0
    total_output_tokens: int = 0
    total_cached_input_tokens: int = 0
    total_thinking_tokens: int = 0
    total_tool_tokens: int = 0
    total_cost_usd: float = 0.0
    task_count: int = 0


@dataclass(frozen=True, slots=True)
class End:
    kind: Literal["end"] = "end"
    output: Any = None
    duration_ms: int = 0
    totals: WorkflowTotals = field(default_factory=WorkflowTotals)


@dataclass(frozen=True, slots=True)
class TaskStart:
    task: str
    kind: Literal["task_start"] = "task_start"
    on_error: str | None = None


@dataclass(frozen=True, slots=True)
class TaskEnd:
    task: str
    kind: Literal["task_end"] = "task_end"
    output: Any = None
    duration_ms: int = 0
    usage: TokenUsage | None = None
    variant: str = "success"
    """How the task finished (issue #206).

    Mirrors :class:`akribes_core::event::TaskEndVariant`. ``"success"`` is the
    wire default; ``"unable"`` surfaces when the task's return type was
    ``T | Unable`` and the agent emitted a canonical Unable envelope.
    Pre-#206 servers omit the field entirely — falling back here to
    ``"success"`` matches the Rust ``#[serde(default)]``.

    Typed as ``str`` so a future engine can introduce new discriminants
    (``"partial"`` for #205) without breaking older SDKs; consumers that
    narrow on the value should keep a fall-through for unknowns."""


@dataclass(frozen=True, slots=True)
class AgentChunk:
    task: str
    task_id: str
    chunk: str
    kind: Literal["agent_chunk"] = "agent_chunk"
    agent: str | None = None


@dataclass(frozen=True, slots=True)
class ToolCallStart:
    task: str
    tool: str
    server: str
    kind: Literal["tool_call_start"] = "tool_call_start"
    input: Any = None
    tool_use_id: str = ""
    """LLM-issued ``tool_use_id``. Empty on pre-durable-execution wire
    payloads; populated on events written by v1+ engines so the cache
    layer can key ``ToolCallEnd`` lookups."""


@dataclass(frozen=True, slots=True)
class ToolCallEnd:
    task: str
    tool: str
    kind: Literal["tool_call_end"] = "tool_call_end"
    output: Any = None
    duration_ms: int = 0
    tool_use_id: str = ""


@dataclass(frozen=True, slots=True)
class Checkpoint:
    name: str
    token: str
    prompt: str
    kind: Literal["checkpoint"] = "checkpoint"
    schema_: Any = None
    """Wire field ``schema`` (renamed to avoid Python keyword clash)."""
    timeout_secs: int | None = None
    trigger: SuspendTrigger = dataclasses.field(default_factory=lambda: DagPositionTrigger())
    """Why the engine suspended — mirrors the Rust-core ``SuspendTrigger``
    enum. Defaults to :class:`DagPositionTrigger` when the server omits the
    field (wire-compat)."""


@dataclass(frozen=True, slots=True)
class ToolApproval:
    token: str
    tool_ref: str
    kind: Literal["tool_approval"] = "tool_approval"
    args: Any = None
    execution_id: str | None = None
    node_id: int | None = None


@dataclass(frozen=True, slots=True)
class Breakpoint:
    token: str
    node_id: int
    kind: Literal["breakpoint"] = "breakpoint"
    env: dict[str, Any] = dataclasses.field(default_factory=dict)


@dataclass(frozen=True, slots=True)
class WorkflowError:
    message: str
    error_kind: WorkflowErrorKind
    kind: Literal["error"] = "error"
    code: str | None = None
    """Stable diagnostic code (e.g. ``"AKRIBES-E-SCRIPT-DEPTH"``) when the
    underlying ``Value::FatalError`` carried one. ``None`` on legacy errors
    without a registered code (#429)."""


@dataclass(frozen=True, slots=True)
class ValidationFailure:
    """Mirror of :class:`akribes_core::event::EngineEvent::ValidationFailure`
    (#320). Emitted on every structured-output validation retry alongside
    the legacy :class:`Log` line. Routed under ``output`` in
    :func:`category_of` so existing ``.on.output()`` subscribers see the
    typed failure record next to their ``AgentChunk`` stream.
    """

    task_name: str
    """The task whose response failed validation."""
    attempt: int
    """1-indexed attempt number."""
    model_response: str
    """Raw text / JSON-serialized tool input the model emitted."""
    kind: Literal["validation_failure"] = "validation_failure"
    missing_fields: list[str] = dataclasses.field(default_factory=list)
    extra_fields: list[str] = dataclasses.field(default_factory=list)
    type_errors: list[str] = dataclasses.field(default_factory=list)
    stop_reason: str | None = None


@dataclass(frozen=True, slots=True)
class LoopStart:
    """A ``loop NAME(...) -> Ret`` block began. Mirror of
    :class:`akribes_core::event::EngineEvent::LoopStart`."""

    name: str
    """Loop name as declared in source."""
    max_turns: int
    """Resolved per-loop turn budget (declared ``max_turns:`` if present,
    else the engine default)."""
    kind: Literal["loop_start"] = "loop_start"


@dataclass(frozen=True, slots=True)
class LoopTurn:
    """One turn of a ``loop`` settled — the provider call returned and
    every dispatched ``tool_use`` block was processed. Mirror of
    :class:`akribes_core::event::EngineEvent::LoopTurn`."""

    name: str
    turn: int
    """1-indexed turn number."""
    kind: Literal["loop_turn"] = "loop_turn"
    tool_calls: list[str] = dataclasses.field(default_factory=list)
    """Names of the tools the model invoked this turn, in dispatch order
    (synthetic ``state_get`` / ``state_update`` / ``return`` plus any
    user ``tools:`` entries)."""


@dataclass(frozen=True, slots=True)
class LoopEnd:
    """A ``loop`` exited. Mirror of
    :class:`akribes_core::event::EngineEvent::LoopEnd`. ``value`` is the
    agent's submitted return value, the final state on a natural
    ``stop_when:`` exit, or a ``FatalError`` envelope when the loop
    exhausted its ``max_turns`` budget."""

    name: str
    turn_count: int
    kind: Literal["loop_end"] = "loop_end"
    value: Any = None


# ── Runtime (container code execution) variants ─────────────────────────


@dataclass(frozen=True, slots=True)
class RuntimeStart:
    """A ``runtime`` block (container code execution) began. Mirror of
    :class:`akribes_core::event::EngineEvent::RuntimeStart`."""

    task: str
    """Variable name on the workflow side that received the runtime call —
    mirrors :attr:`TaskStart.task` for ordinary task blocks."""
    runtime_name: str
    """Source identifier of the ``runtime NAME(...)`` block."""
    language: str
    """One of ``python``, ``bash``, ``node``, ``rust``, ``java``. Bare
    ``str`` so future engines can introduce new languages without breaking
    older SDKs — consumers narrowing should keep a catch-all."""
    kind: Literal["runtime_start"] = "runtime_start"


@dataclass(frozen=True, slots=True)
class RuntimeStdout:
    """A chunk of stdout streamed from the sandboxed runtime. Mirror of
    :class:`akribes_core::event::EngineEvent::RuntimeStdout`. Multiple
    events per invocation, in dispatch order."""

    task: str
    chunk: str
    kind: Literal["runtime_stdout"] = "runtime_stdout"


@dataclass(frozen=True, slots=True)
class RuntimeStderr:
    """A chunk of stderr streamed from the sandboxed runtime. Mirror of
    :class:`akribes_core::event::EngineEvent::RuntimeStderr`. Interleaved
    with :class:`RuntimeStdout` in dispatch order."""

    task: str
    chunk: str
    kind: Literal["runtime_stderr"] = "runtime_stderr"


@dataclass(frozen=True, slots=True)
class RuntimeEnd:
    """The sandboxed runtime exited cleanly. Mirror of
    :class:`akribes_core::event::EngineEvent::RuntimeEnd`."""

    task: str
    exit_code: int
    duration_ms: int
    kind: Literal["runtime_end"] = "runtime_end"


@dataclass(frozen=True, slots=True)
class RuntimeError:
    """The sandbox failed to run the code to completion (timeout, OOM,
    sandbox unavailable, internal error). Mirror of
    :class:`akribes_core::event::EngineEvent::RuntimeError`.

    Mutually exclusive with :class:`RuntimeEnd` on a single invocation."""

    task: str
    error_kind: str
    """Classification, e.g. ``Timeout``, ``OomKilled``,
    ``SandboxUnavailable``, ``Internal``. Bare ``str`` for forward-compat."""
    message: str
    kind: Literal["runtime_error"] = "runtime_error"


# ── Aggregated step (reducer output) ─────────────────────────────────────


RuntimeStatus = Literal["running", "completed", "error"]
"""Status of a :class:`RuntimeStep` as folded by :func:`reduce_runtime_events`."""


@dataclass(frozen=True, slots=True)
class RuntimeStep:
    """Aggregated view of one runtime invocation, folded from the 5 raw
    runtime events by :func:`reduce_runtime_events`.

    A step starts in ``running`` when :class:`RuntimeStart` arrives, accumulates
    stdout/stderr chunks, and settles into ``completed`` on
    :class:`RuntimeEnd` or ``error`` on :class:`RuntimeError`. Mirrors the
    TypeScript SDK's ``RuntimeStep`` (unit 7) so cross-SDK consumers see the
    same shape.

    Unlike the raw events, this is NOT a :class:`WorkflowEvent` variant —
    it's emitted by the reducer for callers who want a single aggregated
    record per invocation instead of the event stream.
    """

    task_name: str
    runtime_name: str
    language: str
    status: RuntimeStatus
    kind: Literal["runtime_step"] = "runtime_step"
    stdout: str = ""
    """Accumulated stdout, in arrival order."""
    stderr: str = ""
    """Accumulated stderr, in arrival order."""
    exit_code: int | None = None
    """Populated on a clean exit (:class:`RuntimeEnd`). ``None`` while
    running or on error."""
    duration_ms: int | None = None
    """Populated on a clean exit. ``None`` while running or on error."""
    error_kind: str | None = None
    """Populated on :class:`RuntimeError`. ``None`` otherwise."""
    error_message: str | None = None
    """Populated on :class:`RuntimeError`. ``None`` otherwise."""


@dataclass(frozen=True, slots=True)
class Other:
    """Catch-all for engine events the SDK doesn't have a typed variant for.

    The original wire ``type`` is preserved as ``type_name`` and the untouched
    payload is exposed as ``payload`` so downstream code can still reach it.
    Guarantees forward-compatibility when the server adds new events.
    """

    type_name: str
    kind: Literal["other"] = "other"
    payload: Any = None


WorkflowEvent = (
    Start
    | End
    | TaskStart
    | TaskEnd
    | AgentChunk
    | ToolCallStart
    | ToolCallEnd
    | Checkpoint
    | ToolApproval
    | Breakpoint
    | WorkflowError
    | ValidationFailure
    | LoopStart
    | LoopTurn
    | LoopEnd
    | RuntimeStart
    | RuntimeStdout
    | RuntimeStderr
    | RuntimeEnd
    | RuntimeError
    | Other
)
"""Union of all typed engine event variants.

Use ``evt.kind`` (a ``Literal`` tag) to narrow; static type checkers
understand the union.
"""

# Helper alias (back-compat name used in other modules).
WorkflowEventT = Union[
    Start,
    End,
    TaskStart,
    TaskEnd,
    AgentChunk,
    ToolCallStart,
    ToolCallEnd,
    Checkpoint,
    ToolApproval,
    Breakpoint,
    WorkflowError,
    ValidationFailure,
    LoopStart,
    LoopTurn,
    LoopEnd,
    RuntimeStart,
    RuntimeStdout,
    RuntimeStderr,
    RuntimeEnd,
    RuntimeError,
    Other,
]

RuntimeWorkflowEvent = Union[
    RuntimeStart,
    RuntimeStdout,
    RuntimeStderr,
    RuntimeEnd,
    RuntimeError,
]
"""Narrowed union of just the five runtime variants. Useful as the element
type of typed-filter helpers like :meth:`RunStream.runtime_events`."""


# ────────────────────────────────────────────────────────────────────────
# Categories
# ────────────────────────────────────────────────────────────────────────


EventCategory = Literal["progress", "output", "tool", "suspend", "error", "other"]


_CATEGORY: dict[str, EventCategory] = {
    "start": "progress",
    "end": "progress",
    "task_start": "progress",
    "task_end": "progress",
    "agent_chunk": "output",
    "validation_failure": "output",
    "tool_call_start": "tool",
    "tool_call_end": "tool",
    "checkpoint": "suspend",
    "tool_approval": "suspend",
    "breakpoint": "suspend",
    "error": "error",
    "loop_start": "progress",
    "loop_turn": "progress",
    "loop_end": "progress",
    # Runtime block lifecycle: start/end frame the invocation,
    # stdout/stderr stream live output, error is terminal.
    "runtime_start": "progress",
    "runtime_end": "progress",
    "runtime_stdout": "output",
    "runtime_stderr": "output",
    "runtime_error": "error",
    "other": "other",
}


def category_of(evt: "WorkflowEventT") -> EventCategory:
    """Bucket a typed event into a high-level category.

    ``progress`` covers lifecycle/task events, ``output`` is streaming text,
    ``tool`` is MCP invocations, ``suspend`` is any human-in-the-loop pause,
    ``error`` is terminal failures, ``other`` catches the rest.
    """
    return _CATEGORY[evt.kind]


# ────────────────────────────────────────────────────────────────────────
# Conversion helpers
# ────────────────────────────────────────────────────────────────────────


def _duration_ms(value: Any) -> int:
    """Convert the server's ``{secs, nanos}`` duration wire shape to ms.

    Accepts plain ints/floats (already-in-ms) as well for robustness.
    Returns ``0`` for anything else.
    """
    if isinstance(value, dict):
        secs = int(value.get("secs", 0) or 0)
        nanos = int(value.get("nanos", 0) or 0)
        return secs * 1000 + round(nanos / 1_000_000)
    if isinstance(value, (int, float)):
        return int(value)
    return 0


def _token_usage(payload: Any) -> TokenUsage | None:
    if not isinstance(payload, dict):
        return None
    try:
        return TokenUsage(**payload)
    except Exception:
        return None


def _parse_trigger(v: Any) -> SuspendTrigger:
    """Parse a wire trigger dict (or None) into a typed :class:`SuspendTrigger`."""
    if isinstance(v, _TriggerBase):
        return v  # type: ignore[return-value]
    if v is None:
        return DagPositionTrigger()
    return parse_suspend_trigger(v)


# ────────────────────────────────────────────────────────────────────────
# Parser dispatch table
# ────────────────────────────────────────────────────────────────────────

_VALID_ERROR_KINDS = frozenset({
    "RateLimit", "AuthError", "TokenLimit",
    # #1296: legacy umbrella + new status-specific kinds.
    "ServerError", "ServerError500", "BadGateway502",
    "ServiceUnavailable503", "GatewayTimeout504",
    "NetworkError", "ParseError", "Cancelled", "ScriptError",
})


def _parse_workflow_start(payload: Any) -> WorkflowEventT:
    total = int(payload) if isinstance(payload, int) else 0
    return Start(total_tasks=total)


def _parse_workflow_end(payload: Any) -> WorkflowEventT:
    """Parse a `WorkflowEnd` payload, accepting both the new (#1173)
    `{value, total_input_tokens, ...}` shape and the legacy bare-value
    shape. Disambiguator: an object with `value` plus any `total_*`/
    `task_count` key signals the new shape.
    """
    agg_keys = (
        "total_input_tokens", "total_output_tokens",
        "total_cached_input_tokens", "total_thinking_tokens",
        "total_tool_tokens", "total_cost_usd", "task_count",
    )
    if isinstance(payload, dict) and "value" in payload and any(k in payload for k in agg_keys):
        totals = WorkflowTotals(
            total_input_tokens=int(payload.get("total_input_tokens") or 0),
            total_output_tokens=int(payload.get("total_output_tokens") or 0),
            total_cached_input_tokens=int(payload.get("total_cached_input_tokens") or 0),
            total_thinking_tokens=int(payload.get("total_thinking_tokens") or 0),
            total_tool_tokens=int(payload.get("total_tool_tokens") or 0),
            total_cost_usd=float(payload.get("total_cost_usd") or 0.0),
            task_count=int(payload.get("task_count") or 0),
        )
        return End(output=payload.get("value"), duration_ms=0, totals=totals)
    return End(output=payload, duration_ms=0)


def _parse_task_start(payload: Any) -> WorkflowEventT:
    if isinstance(payload, (list, tuple)):
        name = str(payload[0]) if payload else ""
        on_err = payload[1] if len(payload) > 1 else None
        return TaskStart(task=name, on_error=on_err)
    if isinstance(payload, dict):
        return TaskStart(
            task=str(payload.get("name") or payload.get("task") or ""),
            on_error=payload.get("on_error"),
        )
    return Other(type_name="TaskStart", payload=payload)


def _parse_task_prompt(payload: Any) -> WorkflowEventT:
    # TaskPrompt: 2-tuple (task_name, prompt). We surface it as Other —
    # callers that need the prompt can peek at payload.
    return Other(type_name="TaskPrompt", payload=payload)


def _parse_task_end(payload: Any) -> WorkflowEventT:
    # TaskEnd: struct variant on the wire (pre-#206 engines used a tuple in
    # internal tests; SDK accepts both). Struct payload carries `value` +
    # `variant` (issue #206). Missing `variant` defaults to `"success"` to
    # mirror the engine's `#[serde(default)]`.
    if isinstance(payload, (list, tuple)):
        name = str(payload[0]) if payload else ""
        output = payload[2] if len(payload) > 2 else None
        duration = _duration_ms(payload[3]) if len(payload) > 3 else 0
        usage = _token_usage(payload[4]) if len(payload) > 4 else None
        return TaskEnd(task=name, output=output, duration_ms=duration, usage=usage)
    if isinstance(payload, dict):
        raw_variant = payload.get("variant", "success")
        variant = raw_variant if isinstance(raw_variant, str) else "success"
        # `value` is the real wire field. `result` / `output` were
        # never-shipped fallbacks kept for defensive parsing.
        output: Any
        if "value" in payload:
            output = payload["value"]
        elif "result" in payload:
            output = payload["result"]
        else:
            output = payload.get("output")
        return TaskEnd(
            task=str(payload.get("task") or payload.get("name") or ""),
            output=output,
            duration_ms=_duration_ms(payload.get("duration")),
            usage=_token_usage(payload.get("usage")),
            variant=variant,
        )
    return Other(type_name="TaskEnd", payload=payload)


def _parse_agent_output(payload: Any) -> WorkflowEventT:
    if isinstance(payload, dict):
        return AgentChunk(
            task=str(payload.get("task_name", "")),
            agent=payload.get("agent_name"),
            task_id=str(payload.get("task_id", "")),
            chunk=str(payload.get("chunk", "")),
        )
    return Other(type_name="AgentOutput", payload=payload)


def _parse_tool_call_start(payload: Any) -> WorkflowEventT:
    if isinstance(payload, dict):
        return ToolCallStart(
            task=str(payload.get("task_name", "")),
            tool=str(payload.get("tool_name", "")),
            server=str(payload.get("server_name", "")),
            input=payload.get("input"),
        )
    return Other(type_name="ToolCallStart", payload=payload)


def _parse_tool_call_end(payload: Any) -> WorkflowEventT:
    if isinstance(payload, dict):
        return ToolCallEnd(
            task=str(payload.get("task_name", "")),
            tool=str(payload.get("tool_name", "")),
            output=payload.get("output"),
            duration_ms=_duration_ms(payload.get("duration")),
        )
    return Other(type_name="ToolCallEnd", payload=payload)


def _parse_suspended(payload: Any) -> WorkflowEventT:
    # Checkpoint — struct variant with reserved 'schema' key.
    # The 'trigger' field is parsed via _parse_trigger; passing None lets it
    # fall back to DagPositionTrigger for older servers that omit the field.
    if isinstance(payload, dict):
        return Checkpoint(
            name=str(payload.get("checkpoint_name", "")),
            token=str(payload.get("token", "")),
            prompt=str(payload.get("prompt", "")),
            schema_=payload.get("schema"),
            timeout_secs=payload.get("timeout_secs"),
            trigger=_parse_trigger(payload.get("trigger")),
        )
    return Other(type_name="Suspended", payload=payload)


def _parse_tool_approval_pending(payload: Any) -> WorkflowEventT:
    if isinstance(payload, dict):
        return ToolApproval(
            token=str(payload.get("token", "")),
            tool_ref=str(payload.get("tool_ref", "")),
            args=payload.get("args"),
            execution_id=payload.get("execution_id"),
            node_id=payload.get("node_id"),
        )
    return Other(type_name="ToolApprovalPending", payload=payload)


def _parse_breakpoint(payload: Any) -> WorkflowEventT:
    if isinstance(payload, dict):
        env = payload.get("env_snapshot") or {}
        if not isinstance(env, dict):
            env = {}
        node_id_raw = payload.get("node_id", 0)
        try:
            node_id = int(node_id_raw) if node_id_raw is not None else 0
        except (TypeError, ValueError):
            node_id = 0
        return Breakpoint(
            token=str(payload.get("token", "")),
            node_id=node_id,
            env=env,
        )
    return Other(type_name="Breakpoint", payload=payload)


def _parse_error(payload: Any) -> WorkflowEventT:
    if isinstance(payload, dict):
        kind_raw = payload.get("kind", "ServerError")
        error_kind: WorkflowErrorKind = (
            kind_raw if kind_raw in _VALID_ERROR_KINDS else "ServerError"
        )
        code_raw = payload.get("code")
        code = code_raw if isinstance(code_raw, str) else None
        try:
            return WorkflowError(
                message=str(payload.get("message", "")),
                error_kind=error_kind,
                code=code,
            )
        except Exception:
            return Other(type_name="Error", payload=payload)
    return Other(type_name="Error", payload=payload)


def _parse_validation_failure(payload: Any) -> WorkflowEventT:
    # ValidationFailure — struct variant (#320). Emitted on every
    # structured-output retry; consumers branch on `validation_failure`
    # to render the model's actual response and the structured error
    # breakdown next to the existing `Log` line.
    if isinstance(payload, dict):
        try:
            return ValidationFailure(
                task_name=str(payload.get("task_name", "")),
                attempt=int(payload.get("attempt", 0) or 0),
                model_response=str(payload.get("model_response", "")),
                missing_fields=[
                    s for s in payload.get("missing_fields", []) or [] if isinstance(s, str)
                ],
                extra_fields=[
                    s for s in payload.get("extra_fields", []) or [] if isinstance(s, str)
                ],
                type_errors=[
                    s for s in payload.get("type_errors", []) or [] if isinstance(s, str)
                ],
                stop_reason=(
                    payload.get("stop_reason")
                    if isinstance(payload.get("stop_reason"), str)
                    else None
                ),
            )
        except Exception:
            return Other(type_name="ValidationFailure", payload=payload)
    return Other(type_name="ValidationFailure", payload=payload)


def _parse_loop_start(payload: Any) -> WorkflowEventT:
    if isinstance(payload, dict):
        try:
            return LoopStart(
                name=str(payload.get("name", "")),
                max_turns=int(payload.get("max_turns", 0) or 0),
            )
        except Exception:
            return Other(type_name="LoopStart", payload=payload)
    return Other(type_name="LoopStart", payload=payload)


def _parse_loop_turn(payload: Any) -> WorkflowEventT:
    if isinstance(payload, dict):
        tool_calls_raw = payload.get("tool_calls") or []
        tool_calls = [str(tc) for tc in tool_calls_raw if isinstance(tc, str)]
        try:
            return LoopTurn(
                name=str(payload.get("name", "")),
                turn=int(payload.get("turn", 0) or 0),
                tool_calls=tool_calls,
            )
        except Exception:
            return Other(type_name="LoopTurn", payload=payload)
    return Other(type_name="LoopTurn", payload=payload)


def _parse_loop_end(payload: Any) -> WorkflowEventT:
    if isinstance(payload, dict):
        try:
            return LoopEnd(
                name=str(payload.get("name", "")),
                turn_count=int(payload.get("turn_count", 0) or 0),
                value=payload.get("value"),
            )
        except Exception:
            return Other(type_name="LoopEnd", payload=payload)
    return Other(type_name="LoopEnd", payload=payload)


def _parse_runtime_start(payload: Any) -> WorkflowEventT:
    if isinstance(payload, dict):
        try:
            return RuntimeStart(
                task=str(payload.get("task_name", "")),
                runtime_name=str(payload.get("runtime_name", "")),
                language=str(payload.get("language", "")),
            )
        except Exception:
            return Other(type_name="RuntimeStart", payload=payload)
    return Other(type_name="RuntimeStart", payload=payload)


def _parse_runtime_stdout(payload: Any) -> WorkflowEventT:
    if isinstance(payload, dict):
        try:
            return RuntimeStdout(
                task=str(payload.get("task_name", "")),
                chunk=str(payload.get("chunk", "")),
            )
        except Exception:
            return Other(type_name="RuntimeStdout", payload=payload)
    return Other(type_name="RuntimeStdout", payload=payload)


def _parse_runtime_stderr(payload: Any) -> WorkflowEventT:
    if isinstance(payload, dict):
        try:
            return RuntimeStderr(
                task=str(payload.get("task_name", "")),
                chunk=str(payload.get("chunk", "")),
            )
        except Exception:
            return Other(type_name="RuntimeStderr", payload=payload)
    return Other(type_name="RuntimeStderr", payload=payload)


def _parse_runtime_end(payload: Any) -> WorkflowEventT:
    if isinstance(payload, dict):
        try:
            return RuntimeEnd(
                task=str(payload.get("task_name", "")),
                exit_code=int(payload.get("exit_code", 0) or 0),
                duration_ms=int(payload.get("duration_ms", 0) or 0),
            )
        except Exception:
            return Other(type_name="RuntimeEnd", payload=payload)
    return Other(type_name="RuntimeEnd", payload=payload)


def _parse_runtime_error(payload: Any) -> WorkflowEventT:
    if isinstance(payload, dict):
        try:
            return RuntimeError(
                task=str(payload.get("task_name", "")),
                error_kind=str(payload.get("kind", "")),
                message=str(payload.get("message", "")),
            )
        except Exception:
            return Other(type_name="RuntimeError", payload=payload)
    return Other(type_name="RuntimeError", payload=payload)


_PARSERS: dict[str, Callable[[Any], WorkflowEventT]] = {
    "WorkflowStart":       _parse_workflow_start,
    "WorkflowEnd":         _parse_workflow_end,
    "TaskStart":           _parse_task_start,
    "TaskPrompt":          _parse_task_prompt,
    "TaskEnd":             _parse_task_end,
    "AgentOutput":         _parse_agent_output,
    "ToolCallStart":       _parse_tool_call_start,
    "ToolCallEnd":         _parse_tool_call_end,
    "Suspended":           _parse_suspended,
    "ToolApprovalPending": _parse_tool_approval_pending,
    "Breakpoint":          _parse_breakpoint,
    "Error":               _parse_error,
    "ValidationFailure":   _parse_validation_failure,
    "LoopStart":           _parse_loop_start,
    "LoopTurn":            _parse_loop_turn,
    "LoopEnd":             _parse_loop_end,
    "RuntimeStart":        _parse_runtime_start,
    "RuntimeStdout":       _parse_runtime_stdout,
    "RuntimeStderr":       _parse_runtime_stderr,
    "RuntimeEnd":          _parse_runtime_end,
    "RuntimeError":        _parse_runtime_error,
}
"""Wire ``type`` → parser function dispatch table.

Keys are the PascalCase ``type`` strings emitted by the Rust engine.
Values are callables that accept the raw ``payload`` (any shape) and
return a typed :class:`WorkflowEvent` variant or :class:`Other`.
"""


def to_workflow_event(raw: EngineEvent | dict[str, Any]) -> WorkflowEventT:
    """Coerce a raw :class:`EngineEvent` (or dict) into a typed variant.

    The server's ``EngineEvent`` is an externally-tagged enum serialised as
    ``{type, payload}``. Some variants use positional tuple payloads
    (``TaskEnd`` is a 5-tuple), some are bare scalars (``Log`` is a string),
    and most are dicts. This function hides that bumpy surface behind the
    :class:`WorkflowEvent` union.

    Unknown ``type`` values always map to :class:`Other` so upstream
    changes never raise at the SDK boundary.
    """
    if isinstance(raw, EngineEvent):
        ty, payload = raw.type, raw.payload
    else:
        ty = raw.get("type", "")
        payload = raw.get("payload")

    parser = _PARSERS.get(ty)
    if parser is None:
        # Anything else (Log, StateUpdate, Resumed, NodeStart, NodeEnd,
        # BreakpointResumed, McpServerDegraded, McpServerRecovered,
        # VerificationStart, VerificationResult, or future variants):
        return Other(type_name=ty, payload=payload)
    return parser(payload)


# ────────────────────────────────────────────────────────────────────────
# Runtime step reducer
# ────────────────────────────────────────────────────────────────────────


def _runtime_step_fold(
    state: dict[str, dict[str, Any]],
    evt: WorkflowEventT,
) -> RuntimeStep | None:
    """Apply one event to the open-step accumulator.

    Returns a settled :class:`RuntimeStep` when the event closes an open
    invocation (RuntimeEnd / RuntimeError), otherwise ``None``. Mutates
    *state* in place to track stdout/stderr accumulation. Shared core for
    the sync and async reducers.
    """
    if isinstance(evt, RuntimeStart):
        state[evt.task] = {
            "task_name": evt.task,
            "runtime_name": evt.runtime_name,
            "language": evt.language,
            "stdout_parts": [],
            "stderr_parts": [],
        }
    elif isinstance(evt, RuntimeStdout):
        step = state.get(evt.task)
        if step is not None:
            step["stdout_parts"].append(evt.chunk)
    elif isinstance(evt, RuntimeStderr):
        step = state.get(evt.task)
        if step is not None:
            step["stderr_parts"].append(evt.chunk)
    elif isinstance(evt, RuntimeEnd):
        step = state.pop(evt.task, None)
        if step is not None:
            return RuntimeStep(
                task_name=step["task_name"],
                runtime_name=step["runtime_name"],
                language=step["language"],
                status="completed",
                stdout="".join(step["stdout_parts"]),
                stderr="".join(step["stderr_parts"]),
                exit_code=evt.exit_code,
                duration_ms=evt.duration_ms,
            )
    elif isinstance(evt, RuntimeError):
        step = state.pop(evt.task, None)
        if step is not None:
            return RuntimeStep(
                task_name=step["task_name"],
                runtime_name=step["runtime_name"],
                language=step["language"],
                status="error",
                stdout="".join(step["stdout_parts"]),
                stderr="".join(step["stderr_parts"]),
                error_kind=evt.error_kind,
                error_message=evt.message,
            )
    return None


def reduce_runtime_events(
    events: Iterable[WorkflowEventT],
) -> Iterator[RuntimeStep]:
    """Fold a sequence of typed :class:`WorkflowEvent`s into per-invocation
    :class:`RuntimeStep` records.

    For every :class:`RuntimeStart` in *events* the reducer opens a step
    keyed by ``task``. Subsequent :class:`RuntimeStdout` / :class:`RuntimeStderr`
    chunks are concatenated; the matching :class:`RuntimeEnd` settles the step
    to ``completed`` and :class:`RuntimeError` settles it to ``error``. The
    settled step is yielded once.

    Non-runtime events are ignored. Unstarted stdout/stderr/end/error
    events (no preceding :class:`RuntimeStart` for that task) are dropped —
    the engine should never emit them, but the reducer is forgiving rather
    than fatal.

    Mirrors the TypeScript SDK's ``reduceExecutionEvent`` runtime arm
    (unit 7) so cross-SDK consumers see the same fold.

    Example::

        steps = list(reduce_runtime_events([
            RuntimeStart(task="t", runtime_name="r", language="python"),
            RuntimeStdout(task="t", chunk="hello\\n"),
            RuntimeEnd(task="t", exit_code=0, duration_ms=120),
        ]))
        assert steps[0].status == "completed"
        assert steps[0].stdout == "hello\\n"
    """
    state: dict[str, dict[str, Any]] = {}
    for evt in events:
        step = _runtime_step_fold(state, evt)
        if step is not None:
            yield step


async def reduce_runtime_events_async(
    events: AsyncIterable[WorkflowEventT],
) -> AsyncIterator[RuntimeStep]:
    """Async variant of :func:`reduce_runtime_events`.

    Folds an async stream of :class:`WorkflowEvent`s into completed
    :class:`RuntimeStep` records. Each settled step (clean exit or error)
    yields one record. Matches the sync reducer's semantics exactly —
    only the iteration surface differs.

    Use this when consuming :meth:`RunStream.runtime_events` or any other
    async source.
    """
    state: dict[str, dict[str, Any]] = {}
    async for evt in events:
        step = _runtime_step_fold(state, evt)
        if step is not None:
            yield step


__all__ = [
    "AgentChunk",
    "Breakpoint",
    "Checkpoint",
    "End",
    "EventCategory",
    "WorkflowError",
    "LoopStart",
    "LoopTurn",
    "LoopEnd",
    "Other",
    "RuntimeStart",
    "RuntimeStdout",
    "RuntimeStderr",
    "RuntimeEnd",
    "RuntimeError",
    "RuntimeStatus",
    "RuntimeStep",
    "RuntimeWorkflowEvent",
    "Start",
    "TaskEnd",
    "TaskStart",
    "TokenUsage",
    "ToolApproval",
    "ToolCallEnd",
    "ToolCallStart",
    "ValidationFailure",
    "WorkflowErrorKind",
    "WorkflowEvent",
    "WorkflowEventT",
    "category_of",
    "reduce_runtime_events",
    "reduce_runtime_events_async",
    "to_workflow_event",
]
