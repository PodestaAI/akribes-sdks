from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Any, Literal, TypedDict, Union

from pydantic import BaseModel, ConfigDict, Field, field_validator

# ────────────────────────────────────────────────────────────────────────
# Type aliases
# ────────────────────────────────────────────────────────────────────────

InputField = tuple[str, str]
"""An (name, type) pair describing a workflow input."""

ExecutionStatusValue = Literal["running", "completed", "failed", "cancelled"]

ChannelName = Literal["production", "draft"] | str  # type: ignore[valid-type]
"""Release channel name. ``production`` and ``draft`` are built-in; any string
is accepted for user-defined channels."""


class _Model(BaseModel):
    """Base model: tolerant of extra fields, frozen (immutable), populated by
    name or alias. All SDK wire types inherit from this."""

    model_config = ConfigDict(
        extra="ignore",
        frozen=True,
        populate_by_name=True,
        arbitrary_types_allowed=True,
    )


def _parse_dt(value: Any) -> Any:
    """Parse RFC 3339 timestamps with a trailing ``Z`` (Python 3.10 compat)."""
    if isinstance(value, str):
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    return value


# ────────────────────────────────────────────────────────────────────────
# Errors
# ────────────────────────────────────────────────────────────────────────


class ErrorKind(str, Enum):
    """Classification of execution failure causes."""

    RATE_LIMIT = "RateLimit"
    AUTH_ERROR = "AuthError"
    TOKEN_LIMIT = "TokenLimit"
    # #1296: legacy umbrella retained for back-compat. New producers emit
    # one of the four status-specific kinds below.
    SERVER_ERROR = "ServerError"
    SERVER_ERROR_500 = "ServerError500"
    BAD_GATEWAY_502 = "BadGateway502"
    SERVICE_UNAVAILABLE_503 = "ServiceUnavailable503"
    GATEWAY_TIMEOUT_504 = "GatewayTimeout504"
    NETWORK_ERROR = "NetworkError"
    PARSE_ERROR = "ParseError"
    CANCELLED = "Cancelled"
    TIMEOUT = "Timeout"
    SCRIPT_ERROR = "ScriptError"
    AUTHOR_RAISE = "AuthorRaise"
    SCRIPT_DEPTH_EXCEEDED = "ScriptDepthExceeded"
    PANIC = "Panic"
    INTERNAL = "Internal"

    @property
    def is_transient(self) -> bool:
        return self in (self.RATE_LIMIT, self.SERVER_ERROR, self.NETWORK_ERROR)

    @property
    def is_fatal(self) -> bool:
        return self in (self.AUTH_ERROR, self.TOKEN_LIMIT)

    @property
    def is_user_actionable(self) -> bool:
        """The user (operator or workflow author) can fix this by changing
        config, script, or input. Mirrors the server's
        `ErrorKind::is_user_actionable`."""
        return self in (
            self.AUTH_ERROR,
            self.TOKEN_LIMIT,
            self.TIMEOUT,
            self.SCRIPT_ERROR,
            self.SCRIPT_DEPTH_EXCEEDED,
            self.AUTHOR_RAISE,
        )


# ────────────────────────────────────────────────────────────────────────
# Projects / Scripts / Versions
# ────────────────────────────────────────────────────────────────────────


@dataclass(frozen=True, slots=True)
class Project:
    id: int
    name: str
    created_at: datetime


@dataclass(frozen=True, slots=True)
class Script:
    id: int
    project_id: int
    name: str
    created_at: datetime


class ScriptVersion(_Model):
    id: int
    script_id: int
    source: str
    label: str | None = None
    published_by: str | None = None
    created_at: datetime | None = None

    _v_created = field_validator("created_at", mode="before")(_parse_dt)


@dataclass(frozen=True, slots=True)
class LatestVersion:
    """Response from the ``/latest`` endpoint — a version plus parsed inputs."""

    id: int
    script_id: int
    source: str
    label: str | None
    published_by: str | None
    created_at: datetime
    inputs: list[InputField] = field(default_factory=list)


@dataclass(frozen=True, slots=True)
class BreakingInterest:
    client_id: str
    client_name: str
    channel: str
    lifetime: str
    mismatch: dict[str, Any]


@dataclass(frozen=True, slots=True)
class PublishDryRunResult:
    dry_run: bool
    would_break: int
    breaking_interests: list[BreakingInterest] = field(default_factory=list)


# ────────────────────────────────────────────────────────────────────────
# Channels / Drafts
# ────────────────────────────────────────────────────────────────────────


@dataclass(frozen=True, slots=True)
class ScriptChannel:
    id: int
    script_id: int
    name: str
    version_id: int | None
    updated_at: datetime | None


@dataclass(frozen=True, slots=True)
class Draft:
    source: str
    inputs: list[InputField] = field(default_factory=list)


class SchemaMismatch(_Model):
    missing: list[tuple[str, str]] = Field(default_factory=list)
    wrong_type: list[tuple[str, str, str]] = Field(default_factory=list)
    extra: list[str] = Field(default_factory=list)


@dataclass(frozen=True, slots=True)
class ContractWarning:
    client_id: str
    client_name: str
    channel: str
    mismatch: SchemaMismatch


@dataclass(frozen=True, slots=True)
class PutDraftResponse:
    schema_warnings: list[ContractWarning] = field(default_factory=list)


# ────────────────────────────────────────────────────────────────────────
# Execution
# ────────────────────────────────────────────────────────────────────────


@dataclass(frozen=True, slots=True)
class CostByVersion:
    """One row in :class:`ScriptCost.by_version` (#1063)."""

    version_id: int
    executions: int
    total_cost_usd: float
    avg_cost_usd: float


@dataclass(frozen=True, slots=True)
class CostByScript:
    """One row in :class:`ProjectCost.by_script` (#1063)."""

    script_name: str
    executions: int
    total_cost_usd: float
    avg_cost_usd: float
    unknown_cost_executions: int = 0


@dataclass(frozen=True, slots=True)
class CostByChannel:
    """One row in :class:`ProjectCost.by_channel` / :class:`ScriptCost.by_channel`."""

    channel: str
    executions: int
    total_cost_usd: float
    avg_cost_usd: float
    unknown_cost_executions: int = 0


@dataclass(frozen=True, slots=True)
class ScriptCost:
    """Cost aggregation for a script (#1193). Canonical name shared with TS.

    The ``CostAggregation`` alias below is kept for back-compat with v0.20.x
    callers; new code should use ``ScriptCost``."""

    total_executions: int
    total_cost_usd: float
    avg_cost_usd: float
    total_input_tokens: int
    total_output_tokens: int
    total_tool_tokens: int = 0
    unknown_cost_executions: int = 0
    by_version: list[dict[str, Any]] = field(default_factory=list)
    by_channel: list[dict[str, Any]] = field(default_factory=list)


# Back-compat alias (#1193). Will be dropped in a future release.
CostAggregation = ScriptCost


@dataclass(frozen=True, slots=True)
class ProjectCost:
    """Cost aggregation across an entire project (#1063).

    Returned by ``client.executions.get_project_cost(...)``. Mirrors TS
    ``ProjectCost`` and Rust ``ProjectCost``."""

    project_id: int
    total_executions: int
    total_cost_usd: float
    avg_cost_usd: float
    total_input_tokens: int
    total_output_tokens: int
    unknown_cost_executions: int = 0
    by_script: list[CostByScript] = field(default_factory=list)
    by_channel: list[CostByChannel] = field(default_factory=list)


@dataclass(frozen=True, slots=True)
class DocumentMeta:
    """Full document metadata returned by ``GET /documents/{id}`` (#1063).

    Mirrors TS inline ``DocumentMeta`` and Rust ``DocumentMeta``."""

    id: str
    filename: str
    content_type: str
    size_bytes: int
    content_hash: str
    conversion_status: str
    created_at: str
    conversion_error: str | None = None


@dataclass(frozen=True, slots=True)
class ReconvertResult:
    """Result of ``POST /documents/{id}/convert`` reconversion (#1063)."""

    status: str


@dataclass(frozen=True, slots=True)
class RunResult:
    execution_id: str
    # Event-log watermark a subscriber should pass as ``last_event_id``
    # on the FIRST ``events.subscribe(...)`` call after this run. Always
    # ``0`` on a fresh spawn — the server's catchup path then replays
    # every buffered event with ``id > 0`` so no event is dropped between
    # the spawn response and the SSE/WS attach (#807). Defaults to ``0``
    # for back-compat with pre-0.21.13 servers that don't return the field.
    since_id: int = 0


class DocumentRef(_Model):
    """A document reference returned when S3 persistence is active."""

    document_id: str
    filename: str


@dataclass(frozen=True, slots=True)
class ExecutionStatus:
    id: str
    project_id: int
    script_name: str
    status: ExecutionStatusValue
    started_at: datetime | None
    finished_at: datetime | None
    version_id: int | None
    channel: str | None
    error: str | None
    error_kind: str | None
    result: dict[str, Any] | None
    documents: dict[str, str | dict[str, str]] | None = None
    triggered_by: str | None = None
    input_tokens: int = 0
    output_tokens: int = 0
    tool_tokens: int = 0
    cost_usd: float | None = None
    # Workflow's declared return TypeRef when statically resolvable from
    # source. Lets clients dispatch straight into a typed renderer instead
    # of inferring from `result`. None for older servers, unparseable
    # source, or workflows whose final expression isn't a resolvable
    # task/flow call.
    result_type: TypeRef | None = None
    # Declared record types from the source the execution ran against,
    # keyed by `type Name:` identifier (#1172). Lets clients render results
    # back to their declared shape (named records, typed columns) instead
    # of falling through to JSON shape inference. ``None`` from older
    # servers; ``{}`` when the source couldn't be parsed.
    type_defs: dict[str, list[Any]] | None = None
    # ID of the parent execution that spawned this one via
    # `spawn_child_execution`. ``None`` for top-level executions.
    parent_execution_id: str | None = None
    # The node ID within the parent execution at which this child was
    # spawned. ``None`` when `parent_execution_id` is ``None``.
    parent_node_id: str | None = None


@dataclass(frozen=True, slots=True)
class ExecutionOutput:
    execution_id: str
    status: ExecutionStatusValue
    error: str | None
    error_kind: str | None
    result: dict[str, Any] | None


class ExecutionChildSummary(_Model):
    """Summary of a child execution spawned via the ``spawn_child_execution``
    callback. Returned by ``GET /executions/{id}/children`` (#1054).

    For v1 the parent-linkage columns are typically NULL; this type is
    forward-looking. Mirrors TS ``ExecutionChildSummary``."""
    id: str
    parent_node_id: str | None = None
    status: str
    started_at: str | None = None
    finished_at: str | None = None
    script_name: str


class ExecutionTaskSummary(_Model):
    """Per-task cost / token / duration breakdown row from
    ``GET /executions/{id}/tasks``. One row per ``execution_tasks`` entry,
    populated as ``TaskEnd`` events arrive. Mirrors TS
    ``ExecutionTaskSummary`` and the ``get_execution_tasks`` server handler."""
    task_name: str
    model: str | None = None
    provider: str | None = None
    input_tokens: int
    output_tokens: int
    cached_input_tokens: int
    cache_write_input_tokens: int
    cost_usd: float | None = None
    duration_ms: int | None = None
    attempt: int
    finished_at: str


class ExecutionTasksResponse(_Model):
    """Envelope returned by ``GET /executions/{id}/tasks``. Mirrors TS
    ``ExecutionTasksResponse``."""
    execution_id: str
    tasks: list[ExecutionTaskSummary]


# ────────────────────────────────────────────────────────────────────────
# Engine events
# ────────────────────────────────────────────────────────────────────────


EngineEventType = Literal[
    "Log", "StateUpdate", "WorkflowStart", "TaskStart", "TaskPrompt", "TaskEnd",
    "AgentOutput", "AgentReasoning", "Suspended", "Resumed", "WorkflowEnd", "Error",
    "NodeStart", "NodeEnd", "Breakpoint", "BreakpointResumed",
    "ToolCallStart", "ToolCallEnd", "McpServerDegraded", "McpServerRecovered",
    "ToolApprovalPending", "VerificationStart", "VerificationResult",
    "ContextCompacted", "ContextOverflow",
    # Durable-execution replay events (v1).
    "LLMResponse", "SubScriptSpawned", "SubScriptResult", "CheckpointResolution",
]


class EngineEvent(_Model):
    """A raw engine event from the server.

    The server's discriminated enum is serialized as ``{type, payload}``.
    ``payload`` is typed as ``Any`` because the server uses positional
    (non-dict) payloads for some variants (e.g. ``Log`` is a bare string,
    ``WorkflowStart`` is an int). For most consumers, prefer the typed
    :class:`akribes_sdk.workflow_events.WorkflowEvent` union obtained via
    :func:`akribes_sdk.workflow_events.to_workflow_event` or the SDK's
    ``typed_engine_events()`` / :class:`akribes_sdk.run_stream.RunStream`
    helpers.
    """

    type: str
    payload: Any = None


# ── Typed event variants (optional, via parse_engine_event) ────────────


class _EventBase(_Model):
    pass


@dataclass(frozen=True, slots=True)
class TypeRef:
    """Structural representation of a declared Akribes type. Mirrors the engine's
    `TypeRef` AST node — `inner` parameterizes generics like `list[str]`,
    `choices` populates string-literal union types."""

    name: str
    inner: TypeRef | None = None
    choices: list[str] | None = None


class Duration(TypedDict):
    """Serde-serialized `std::time::Duration` from the engine."""
    secs: int
    nanos: int


@dataclass(frozen=True, slots=True)
class TokenUsage:
    """Token usage from a single LLM call. Field names match the Rust
    `TokenUsage` struct serialized by the engine."""

    input_tokens: int
    output_tokens: int
    model: str
    provider: str
    cached_input_tokens: int
    # Cache-creation (write) tokens. Anthropic-only today; billed by the
    # server at 1.25x base input (5-minute TTL). OpenAI/Gemini emit 0.
    # `= 0` default keeps compatibility with older servers that don't emit
    # this field.
    cache_write_input_tokens: int = 0


class LogEvent(_EventBase):
    type: Literal["Log"] = "Log"
    message: str


class WorkflowStartEvent(_EventBase):
    type: Literal["WorkflowStart"] = "WorkflowStart"
    total_tasks: int


class WorkflowEndEvent(_EventBase):
    """Terminal event of a workflow run.

    Issue #1173: `result` carries the workflow's return value (same
    semantics as the pre-#1173 bare payload); `totals` carries the
    aggregate token + cost rollup across every ``TaskEnd`` in the
    workflow scope. `totals` defaults to all-zero on legacy emissions
    that predate the rollup.
    """
    type: Literal["WorkflowEnd"] = "WorkflowEnd"
    result: Any = None
    total_input_tokens: int = 0
    total_output_tokens: int = 0
    total_cached_input_tokens: int = 0
    total_thinking_tokens: int = 0
    total_tool_tokens: int = 0
    total_cost_usd: float = 0.0
    task_count: int = 0


class TaskStartEvent(_EventBase):
    type: Literal["TaskStart"] = "TaskStart"
    name: str
    on_error: str | None = None


TaskEndVariantValue = Literal["success", "unable", "failed"]
"""Known discriminants of :attr:`TaskEndEvent.variant` (issue #206). A future
engine may add more (e.g. ``"partial"`` for #205). Consumers MUST tolerate
unknown string values — see :attr:`TaskEndEvent.variant` for the wire
contract and forward-compat story.

Mirrors :class:`akribes_core::event::TaskEndVariant` (``snake_case`` wire form)."""


class TaskEndEvent(_EventBase):
    """Emitted when a task completes. The 8 fields mirror the engine's struct
    `EngineEvent::TaskEnd`. `attempt` is 1-indexed (1 = first call succeeded,
    2 = first validation retry succeeded, etc.) and resets on outer
    on_error retries."""

    type: Literal["TaskEnd"] = "TaskEnd"
    task: str
    on_error_label: str | None = None
    value: Any
    value_type: TypeRef | None = None
    duration: Duration
    attempt: int
    usage: TokenUsage | None = None
    variant: str = "success"
    """How the task finished (issue #206). ``"success"`` is the default —
    both the server's ``#[serde(default)]`` and this field's ``= "success"``
    mirror the contract so pre-#206 servers (which omit the field entirely)
    deserialize cleanly.

    Known values today are :data:`TaskEndVariantValue`. Typed as a bare
    ``str`` so future engine versions can introduce new discriminants
    (``"partial"`` for #205) without breaking older SDKs — consumers
    narrowing on the value should fall through for unknowns.
    """


class AgentOutputEvent(_EventBase):
    type: Literal["AgentOutput"] = "AgentOutput"
    task_name: str
    agent_name: str | None = None
    task_id: str
    schema_type: str | None = None
    chunk: str


class ErrorEvent(_EventBase):
    type: Literal["Error"] = "Error"
    message: str
    kind: str
    code: str | None = None
    """Stable diagnostic code (e.g. ``"AKRIBES-E-SCRIPT-DEPTH"``). ``None`` on
    legacy errors without a registered code (#429). Mirrors the
    optional ``code: Option<String>`` on
    ``akribes_core::event::EngineEvent::Error``."""


class ValidationFailureEvent(_EventBase):
    """A structured-output task's response failed validation (#320). Emitted
    in addition to the existing :class:`LogEvent` line on every retry — the
    typed shape lets consumers render the model's actual response, the
    schema-validator's structured error breakdown, and the provider's
    ``stop_reason`` (so a ``max_tokens`` truncation isn't misdiagnosed as a
    schema overflow).

    Mirrors :class:`akribes_core::event::EngineEvent::ValidationFailure`.
    """

    type: Literal["ValidationFailure"] = "ValidationFailure"
    task_name: str
    attempt: int
    """1-indexed attempt number."""
    model_response: str
    """Raw text / JSON-serialized tool input the model emitted."""
    missing_fields: list[str] = Field(default_factory=list)
    extra_fields: list[str] = Field(default_factory=list)
    type_errors: list[str] = Field(default_factory=list)
    stop_reason: str | None = None


# ── Unable record + SuspendTrigger (Wave 4 / EPA S4 + S6) ──────────────


UnableCategoryValue = Literal[
    "input_missing",
    "input_ambiguous",
    "input_conflicts",
    "capability",
    "other",
]
"""Wire string for an :class:`UnableRecord`'s ``category`` field. Mirrors
the Rust-core ``UnableCategory`` enum variants exactly (snake_case wire
form).
"""


@dataclass(frozen=True, slots=True)
class UnableRecord:
    """Structured "I can't" response from an agent. Mirrors the Rust-core
    :class:`UnableRecord` — the payload inside the canonical wire envelope
    ``{ "unable": { "reason": ..., "missing": [...], "category": ... } }``.

    ``missing`` defaults to ``[]`` on both wire and runtime so callers never
    have to branch on ``None``. ``category`` accepts the five canonical
    wire strings; unknown values pass through as plain ``str`` (the SDK
    doesn't validate the enum — the engine does, before emitting).
    """

    reason: str
    category: str
    missing: list[str] = field(default_factory=list)


@dataclass(frozen=True, slots=True)
class ValidationErrorWire:
    """Wire-format twin of the Rust-core ``ValidationError``. The ``stage``
    discriminator is a string (``"parse"``, ``"schema"``, ``"custom:<rule>"``)
    so SDK consumers don't round-trip through an internal enum.
    """

    stage: str
    message: str
    path: str | None = None


@dataclass(frozen=True, slots=True)
class _TriggerBase:
    """Shared base for every :class:`SuspendTrigger` variant."""


@dataclass(frozen=True, slots=True)
class DagPositionTrigger:
    """The DAG reached an explicit ``checkpoint cp(...)`` call site. Default
    trigger; carries no payload because the checkpoint's own ``expects:``
    schema describes what comes back on resume."""

    kind: str = "DagPosition"


@dataclass(frozen=True, slots=True)
class ValidationExhaustedTrigger:
    """The task's ``on_validation_exhausted:`` property fired — all
    validation retries consumed without producing a payload that passes the
    parse → schema → custom pipeline. The payload surfaces the last failing
    attempt plus its errors so the human can correct it in place."""

    kind: str
    task_name: str
    retry_count: int
    last_attempt: str
    validation_errors: list[ValidationErrorWire] = field(default_factory=list)


@dataclass(frozen=True, slots=True)
class AgentUnableTrigger:
    """A task with a ``T | Unable`` return type produced an ``Unable`` value
    and the flow routed it to a checkpoint via ``on unable <cp>``. The
    payload is always the :class:`UnableRecord` — invariant across Stream 4's
    four ``on unable`` forms."""

    kind: str
    task_name: str
    unable: UnableRecord


@dataclass(frozen=True, slots=True)
class UnknownTrigger:
    """Forward-compat catch-all for :class:`SuspendTrigger` variants the SDK
    doesn't have a typed model for. Preserves the wire ``kind`` plus the
    untouched payload so downstream code can still reach it without crashing
    when the engine ships a new variant.
    """

    wire_kind: str
    """The original wire ``kind`` discriminator, kept verbatim."""
    kind: str = "__unknown__"
    raw: dict[str, Any] = field(default_factory=dict)
    """The full wire payload as a dict, minus the ``kind`` tag."""


SuspendTrigger = Union[
    DagPositionTrigger,
    ValidationExhaustedTrigger,
    AgentUnableTrigger,
    UnknownTrigger,
]
"""Discriminated-union type alias for why the engine suspended. Mirrors the
Rust-core ``SuspendTrigger`` (``#[serde(tag = "kind")]``). Consumers narrow
on ``trigger.kind``; unknown wire variants surface as :class:`UnknownTrigger`
so the SDK never raises on a future engine.
"""


def parse_suspend_trigger(raw: Any) -> SuspendTrigger:
    """Parse a wire :class:`SuspendTrigger` payload into the typed union.

    Unknown ``kind`` discriminants are wrapped in :class:`UnknownTrigger`
    with the original ``kind`` preserved in :attr:`UnknownTrigger.wire_kind`
    and the remaining payload in :attr:`UnknownTrigger.raw`. Malformed
    payloads (non-dict, missing ``kind``) also degrade to
    :class:`UnknownTrigger` rather than raising.
    """
    from akribes_sdk._parsers import (
        parse_dag_position_trigger,
        parse_validation_exhausted_trigger,
        parse_agent_unable_trigger,
    )
    if not isinstance(raw, dict):
        return UnknownTrigger(wire_kind="", raw={})
    kind = raw.get("kind", "")
    try:
        if kind == "DagPosition":
            return parse_dag_position_trigger(raw)
        if kind == "ValidationExhausted":
            return parse_validation_exhausted_trigger(raw)
        if kind == "AgentUnable":
            return parse_agent_unable_trigger(raw)
    except Exception:
        # Malformed payload for a known kind — fall through to Unknown so
        # the stream keeps flowing. The raw dict is preserved.
        pass
    leftover = {k: v for k, v in raw.items() if k != "kind"}
    return UnknownTrigger(wire_kind=str(kind), raw=leftover)


class SuspendedEvent(_EventBase):
    model_config = ConfigDict(
        extra="ignore", frozen=True, populate_by_name=True,
        arbitrary_types_allowed=True, protected_namespaces=(),
    )

    type: Literal["Suspended"] = "Suspended"
    checkpoint_name: str
    token: str
    prompt: str
    schema_: dict[str, Any] | None = Field(default=None, alias="schema")
    timeout_secs: int | None = None
    trigger: SuspendTrigger = Field(default_factory=lambda: DagPositionTrigger())
    """Why we suspended. Mirrors the Rust-core `SuspendTrigger` enum; defaults
    to :class:`DagPositionTrigger` on older wire payloads that omit the field
    (e.g. an older server serializing against a newer SDK, or vice versa)."""

    @field_validator("trigger", mode="before")
    @classmethod
    def _parse_trigger(cls, v: Any) -> Any:
        if isinstance(v, (DagPositionTrigger, ValidationExhaustedTrigger,
                          AgentUnableTrigger, UnknownTrigger)):
            return v
        if v is None:
            return DagPositionTrigger()
        return parse_suspend_trigger(v)


class ToolApprovalPendingEvent(_EventBase):
    type: Literal["ToolApprovalPending"] = "ToolApprovalPending"
    token: str
    tool_ref: str
    args: Any = None
    execution_id: str | None = None
    node_id: int | None = None


# ── Compaction events (three-mode context management, RFC 2026-05-12) ──────


class ContextCompactedEvent(_EventBase):
    """Emitted once per primitive activation of the compaction chain.

    Mirrors :class:`akribes_core::event::EngineEvent::ContextCompacted` —
    fired before/after a compaction step succeeds in shrinking the
    conversation under the configured cap. ``provider_native = True`` means
    Anthropic / OpenAI performed the compaction server-side; the engine
    surfaces the before/after counts from the response. ``strategy`` is the
    primitive name (``drop_thinking_blocks``, ``drop_oldest_tool_results``,
    ``summarize_to_state``, ``provider_native``) or the user task name for a
    custom compactor task.

    See ``docs/superpowers/specs/2026-05-12-compaction-design.md``
    ("Observability + cost") for the contract.
    """

    type: Literal["ContextCompacted"] = "ContextCompacted"
    agent: str
    loop_id: str | None = None
    """UUID of the surrounding ``loop`` block when compaction fires
    mid-loop; ``None`` for compaction outside a loop."""
    turn: int | None = None
    """1-indexed loop turn the compaction fired before, when applicable."""
    threshold_pct: int | None = None
    """Configured percent-of-window threshold (0-100), when the triggering
    rule was ``at_pct``."""
    threshold_abs: int | None = None
    """Configured absolute-token threshold, when the triggering rule was
    ``at_tokens``."""
    strategy: str
    """Primitive name or user task name."""
    before_tokens: int
    after_tokens: int
    provider_native: bool
    cache_ttl: str | None = None
    """Cache TTL applied on the request that produced this compaction.

    For Anthropic provider_native compactions, "1h" — akribes-core
    pins ttl: "1h" via the extended-cache-ttl-2025-04-11 beta
    header on every Anthropic request. For OpenAI native compactions and
    every non-native primitive, None (no TTL-selectable cache tier
    applies). Lets cost dashboards multiply cache-write tokens by the
    correct provider rate — the 5m and 1h tiers price 60% apart
    (issue #1130)."""


class ContextOverflowEvent(_EventBase):
    """Emitted when the compaction chain runs to exhaustion (or when
    ``compaction: none`` and the request would still exceed the model's
    context window).

    Mirrors :class:`akribes_core::event::EngineEvent::ContextOverflow`.
    Carries the chain log so users can diagnose which primitives ran
    before the engine gave up. A ``ContextCompactionExhausted`` :class:`ErrorEvent`
    follows.
    """

    type: Literal["ContextOverflow"] = "ContextOverflow"
    agent: str
    attempted_strategies: list[str] = Field(default_factory=list)
    configured_cap_tokens: int
    model_context_window: int


# ── Runtime (container code execution) events ──────────────────────────


class RuntimeStartEvent(_EventBase):
    """A ``runtime`` block began executing inside the sandbox. One event per
    invocation, before any :class:`RuntimeStdoutEvent` /
    :class:`RuntimeStderrEvent`.

    Mirrors :class:`akribes_core::event::EngineEvent::RuntimeStart`.
    """

    type: Literal["RuntimeStart"] = "RuntimeStart"
    task_name: str
    """Variable name on the workflow side that received the runtime call
    (mirrors :attr:`TaskStartEvent.name` for task blocks)."""
    runtime_name: str
    """Source identifier of the ``runtime NAME(...)`` block."""
    language: str
    """One of ``python``, ``bash``, ``node``, ``rust``, ``java``. Typed as
    bare ``str`` so future engines that introduce new languages don't break
    older SDKs — consumers narrowing should keep a catch-all."""


class RuntimeStdoutEvent(_EventBase):
    """A chunk of stdout streamed from the sandboxed runtime. Multiple
    events per invocation, in dispatch order.

    Mirrors :class:`akribes_core::event::EngineEvent::RuntimeStdout`.
    """

    type: Literal["RuntimeStdout"] = "RuntimeStdout"
    task_name: str
    chunk: str


class RuntimeStderrEvent(_EventBase):
    """A chunk of stderr streamed from the sandboxed runtime. Multiple
    events per invocation, in dispatch order, interleaved with stdout.

    Mirrors :class:`akribes_core::event::EngineEvent::RuntimeStderr`.
    """

    type: Literal["RuntimeStderr"] = "RuntimeStderr"
    task_name: str
    chunk: str


class RuntimeEndEvent(_EventBase):
    """The sandboxed runtime exited cleanly. Carries the wire exit code +
    elapsed duration so consumers can render an exit-code badge without
    waiting for the wrapping :class:`TaskEndEvent`.

    Mirrors :class:`akribes_core::event::EngineEvent::RuntimeEnd`.
    """

    type: Literal["RuntimeEnd"] = "RuntimeEnd"
    task_name: str
    exit_code: int
    duration_ms: int


class RuntimeErrorEvent(_EventBase):
    """The sandbox failed to run the code to completion — timeout, OOM,
    sandbox unavailable, or an internal error. Mutually exclusive with
    :class:`RuntimeEndEvent` on a single invocation.

    Mirrors :class:`akribes_core::event::EngineEvent::RuntimeError`.
    """

    type: Literal["RuntimeError"] = "RuntimeError"
    task_name: str
    kind: str
    """Classification of the failure, e.g. ``Timeout``, ``OomKilled``,
    ``SandboxUnavailable``, ``Internal``. Typed as bare ``str`` for
    forward-compat with future error kinds."""
    message: str


class UnknownEvent(_EventBase):
    """Fallback for engine event variants the SDK does not have a typed model
    for. The original ``type`` and ``payload`` are preserved."""

    type: str
    payload: dict[str, Any] | None = None


TypedEngineEvent = Union[
    LogEvent, WorkflowStartEvent, WorkflowEndEvent, TaskStartEvent, TaskEndEvent,
    AgentOutputEvent, ErrorEvent, ValidationFailureEvent, SuspendedEvent,
    ToolApprovalPendingEvent,
    ContextCompactedEvent, ContextOverflowEvent,
    RuntimeStartEvent, RuntimeStdoutEvent, RuntimeStderrEvent,
    RuntimeEndEvent, RuntimeErrorEvent,
    UnknownEvent,
]


_TYPED_EVENTS: dict[str, type[_EventBase]] = {
    "Log": LogEvent,
    "WorkflowStart": WorkflowStartEvent,
    "WorkflowEnd": WorkflowEndEvent,
    "TaskStart": TaskStartEvent,
    "TaskEnd": TaskEndEvent,
    "AgentOutput": AgentOutputEvent,
    "Error": ErrorEvent,
    "ValidationFailure": ValidationFailureEvent,
    "Suspended": SuspendedEvent,
    "ToolApprovalPending": ToolApprovalPendingEvent,
    "ContextCompacted": ContextCompactedEvent,
    "ContextOverflow": ContextOverflowEvent,
    "RuntimeStart": RuntimeStartEvent,
    "RuntimeStdout": RuntimeStdoutEvent,
    "RuntimeStderr": RuntimeStderrEvent,
    "RuntimeEnd": RuntimeEndEvent,
    "RuntimeError": RuntimeErrorEvent,
}


def parse_engine_event(event: EngineEvent | dict[str, Any]) -> TypedEngineEvent:
    """Coerce a raw ``EngineEvent`` into a typed variant.

    Unknown event types fall back to :class:`UnknownEvent`. Payloads that
    don't match the typed variant's shape are wrapped in :class:`UnknownEvent`
    rather than raising, so consumers never crash on server updates.

    Example::

        async for evt in client.events.engine_events("my_script"):
            typed = parse_engine_event(evt)
            match typed:
                case AgentOutputEvent(task_name=name, chunk=chunk):
                    print(f"[{name}] {chunk}", end="")
                case ErrorEvent(message=msg, kind=kind):
                    logger.error("engine error (%s): %s", kind, msg)
    """
    if isinstance(event, EngineEvent):
        ty, payload = event.type, event.payload
    else:
        ty = event.get("type", "")
        payload = event.get("payload")

    cls = _TYPED_EVENTS.get(ty)
    if cls is None:
        return UnknownEvent(type=ty, payload=payload if isinstance(payload, dict) else None)

    # Server payload formats: sometimes a positional tuple/list (e.g. Log is
    # just a string), sometimes a dict. Normalize.
    data: dict[str, Any]
    if cls is LogEvent:
        data = {"message": payload if isinstance(payload, str) else str(payload)}
    elif cls is WorkflowStartEvent:
        data = {"total_tasks": int(payload) if isinstance(payload, int) else 0}
    elif cls is WorkflowEndEvent:
        # Issue #1173: payload may be either `{value, total_input_tokens, ...}`
        # (post-#1173) or the bare value (pre-#1173). Disambiguator:
        # an object with both `value` and any `total_*` key signals the
        # new shape. Anything else is treated as the bare value.
        _agg_keys = (
            "total_input_tokens", "total_output_tokens",
            "total_cached_input_tokens", "total_thinking_tokens",
            "total_tool_tokens", "total_cost_usd", "task_count",
        )
        if isinstance(payload, dict) and "value" in payload and any(k in payload for k in _agg_keys):
            data = {
                "result": payload.get("value"),
                "total_input_tokens": int(payload.get("total_input_tokens") or 0),
                "total_output_tokens": int(payload.get("total_output_tokens") or 0),
                "total_cached_input_tokens": int(payload.get("total_cached_input_tokens") or 0),
                "total_thinking_tokens": int(payload.get("total_thinking_tokens") or 0),
                "total_tool_tokens": int(payload.get("total_tool_tokens") or 0),
                "total_cost_usd": float(payload.get("total_cost_usd") or 0.0),
                "task_count": int(payload.get("task_count") or 0),
            }
        else:
            data = {"result": payload}
    elif cls is TaskStartEvent and isinstance(payload, (list, tuple)):
        data = {"name": payload[0], "on_error": payload[1] if len(payload) > 1 else None}
    elif isinstance(payload, dict):
        data = payload
    else:
        return UnknownEvent(type=ty, payload=None)

    try:
        return cls(**data)  # type: ignore[return-value]
    except Exception:
        return UnknownEvent(type=ty, payload=payload if isinstance(payload, dict) else None)


@dataclass(frozen=True, slots=True)
class ExecutionEvents:
    execution_id: str
    status: str
    complete: bool
    events: list[EngineEvent] = field(default_factory=list)
    next_after_id: int | None = None
    has_more: bool = False


@dataclass(frozen=True, slots=True)
class HubEvent:
    type: str
    payload: dict[str, Any]


BenchEventType = Literal["RunStarted", "ResultRecorded", "RunFinished"]
"""Discriminator for a :class:`BenchEvent`, mirroring the server's
``BenchEvent`` variants."""


@dataclass(frozen=True, slots=True)
class BenchEvent:
    """A live bench-run lifecycle event, carried inside a ``HubEvent`` of
    ``type == "Bench"`` (``hub_event.payload`` is the wire ``BenchEvent``).

    Mirrors the server's ``BenchEvent`` enum
    (``crates/akribes-server/src/models.rs``), which is adjacently tagged
    (``{"type": ..., "payload": {...}}``) with three variants:

    * ``RunStarted`` / ``RunFinished`` carry a :class:`BenchRun` in ``run``.
    * ``ResultRecorded`` carries ``run_id`` plus a :class:`BenchResult`
      in ``result``.

    ``run`` and ``result`` reuse the existing row models; only the field that
    belongs to the active variant is populated (the others stay ``None``).
    """

    type: BenchEventType
    project_id: int
    script_name: str
    run: BenchRun | None = None
    run_id: int | None = None
    result: BenchResult | None = None


# ────────────────────────────────────────────────────────────────────────
# Clients
# ────────────────────────────────────────────────────────────────────────


class ClientInfo(_Model):
    id: str
    name: str
    last_seen: str
    scripts: list[str] = Field(default_factory=list)


@dataclass(frozen=True, slots=True)
class RegisteredInterest:
    script_name: str
    channel: str
    bound_version_id: int | None
    input_schema: list[InputField] = field(default_factory=list)


@dataclass(frozen=True, slots=True)
class RegisterClientResponse:
    interests: list[RegisteredInterest] = field(default_factory=list)


class ContractLockInfo(_Model):
    id: int
    client_id: str
    client_name: str
    script_name: str
    channel: str
    lifetime: str
    drifted: bool
    created_at: str
    input_schema: str
    bound_version_id: int | None = None
    created_by: str | None = None


# ────────────────────────────────────────────────────────────────────────
# Tokens
# ────────────────────────────────────────────────────────────────────────

TokenRole = Literal["admin", "editor", "viewer"]
"""Token permission level. ``admin`` can mint/revoke; ``editor`` can modify
scripts and executions; ``viewer`` is read-only."""

ProjectScope = Union[Literal["*"], list[int]]
"""Project scope for a token: ``"*"`` for wildcard (all projects), or a list
of project IDs."""


@dataclass(frozen=True, slots=True)
class TokenScopes:
    """Scopes carried by a token. Mirrors the server's ``TokenScopes`` struct."""

    projects: ProjectScope
    role: TokenRole
    scripts: list[str] | None = None
    """Optional: restrict the token to specific script names."""
    executions: list[str] | None = None
    """Optional: restrict the token to specific execution IDs (one-off
    read-only sharing)."""
    can_mint: bool = False
    """Whether the token may itself mint child tokens."""

    def to_dict(self) -> dict[str, Any]:
        body: dict[str, Any] = {
            "projects": self.projects,
            "role": self.role,
            "can_mint": self.can_mint,
        }
        if self.scripts is not None:
            body["scripts"] = self.scripts
        if self.executions is not None:
            body["executions"] = self.executions
        return body

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> TokenScopes:
        return cls(
            projects=data["projects"],
            role=data["role"],
            scripts=data.get("scripts"),
            executions=data.get("executions"),
            can_mint=bool(data.get("can_mint", False)),
        )


@dataclass(frozen=True, slots=True)
class TokenInfo:
    """Metadata for a scoped token (the secret itself is never returned)."""

    id: str
    label: str
    user_email: str | None
    scopes: TokenScopes
    minted_by: str
    expires_at: datetime
    revoked: bool
    created_at: datetime
    last_used_at: datetime | None = None


@dataclass(frozen=True, slots=True)
class TokenMinted:
    """Returned once when a token is first minted — includes the raw secret."""

    token: str
    token_id: str
    expires_at: datetime


# ────────────────────────────────────────────────────────────────────────
# Ad-hoc execution / Documents
# ────────────────────────────────────────────────────────────────────────


@dataclass(frozen=True, slots=True)
class AdhocRunResult:
    execution_id: str
    project_id: int
    # See :class:`RunResult.since_id` (#807).
    since_id: int = 0


@dataclass(frozen=True, slots=True)
class S3PresignedRef:
    """Reference to a document via a pre-signed S3 URL."""

    presigned_url: str


@dataclass(frozen=True, slots=True)
class S3CredentialsRef:
    """Reference to a document in S3 via explicit credentials."""

    bucket: str
    key: str
    access_key_id: str
    secret_access_key: str
    region: str | None = None
    session_token: str | None = None


S3DocumentRef = S3PresignedRef | S3CredentialsRef


@dataclass(frozen=True, slots=True)
class ConvertResult:
    """Result of converting a document to markdown."""

    markdown: str


# ────────────────────────────────────────────────────────────────────────
# Graph
# ────────────────────────────────────────────────────────────────────────


@dataclass(frozen=True, slots=True)
class GraphNode:
    id: int
    op_type: str
    op_name: str | None
    target_var: str | None
    reads: list[str]
    line: int
    col: int


@dataclass(frozen=True, slots=True)
class GraphEdge:
    """Execution graph edge. Wire field ``"from"`` maps to ``from_node``."""

    from_node: int
    to: int


@dataclass(frozen=True, slots=True)
class ScriptGraph:
    """Response for ``GET /projects/{id}/scripts/{name}/graph`` (#1189).

    Canonical, cross-SDK name (matches TS ``ScriptGraph``); the legacy
    ``GraphResponse`` alias below is kept for back-compat with v0.20.x
    callers. New code should use ``ScriptGraph``.
    """

    nodes: list[GraphNode]
    edges: list[GraphEdge]


# Back-compat alias — pre-#1189 SDK releases exposed this name. New code
# should reference ``ScriptGraph`` so identifiers match TS + Rust.
GraphResponse = ScriptGraph


# ────────────────────────────────────────────────────────────────────────
# Bench (akribes-native eval substrate)
# ────────────────────────────────────────────────────────────────────────
#
# Mirrors the wire shapes the akribes-server bench handlers emit
# (`crates/akribes-server/src/handlers/bench.rs` + `models.rs`) and the
# Rust/TS SDK bench clients (`crates/akribes-sdk/src/sub/bench.rs`,
# `packages/akribes-sdk-ts/src/sub/bench.ts`). RFC-3339 timestamps cross
# the wire as strings; we keep them as `str` to match the eval/execution
# convention elsewhere in this module rather than coercing to `datetime`.


@dataclass(frozen=True, slots=True)
class Bench:
    """Per-script bench configuration. One row per ``scripts.id``.

    ``judge_script_id`` is nullable while the bench is still being authored.
    Returned by ``GET/POST /projects/{id}/scripts/{name}/bench``.
    """

    id: int
    script_id: int
    judge_channel: str
    config: dict[str, Any]
    created_at: str
    updated_at: str
    judge_script_id: int | None = None


@dataclass(frozen=True, slots=True)
class ProjectBenchSummary:
    """Aggregated per-bench summary for the project evals landing page.

    Returned by ``GET /projects/{id}/benches``.
    """

    bench_id: int
    script_id: int
    script_name: str
    judge_channel: str
    case_count: int
    updated_at: str
    judge_script_id: int | None = None
    judge_script_name: str | None = None
    latest_run_id: int | None = None
    latest_run_status: str | None = None
    latest_run_channel: str | None = None
    latest_run_workflow_version_id: int | None = None
    latest_run_at: str | None = None
    latest_run_mean_score: float | None = None
    latest_run_cost_usd: float | None = None


@dataclass(frozen=True, slots=True)
class BenchRun:
    """A single bench-run row. ``workflow_version_id`` and ``judge_version_id``
    are resolved at trigger time so a later channel publish doesn't change what
    this run represents. Returned by the trigger / list / get run endpoints."""

    id: int
    bench_id: int
    channel: str
    workflow_version_id: int
    judge_version_id: int
    status: str
    triggered_at: str
    triggered_by: str | None = None
    completed_at: str | None = None
    total_cost_usd: float = 0.0
    total_cases: int = 0
    cache_hit_cases: int = 0
    notes: str | None = None
    mcp_session_id: str | None = None
    case_filter: list[str] | None = None
    mean_headline_score: float | None = None
    ok_cases: int | None = None
    status_breakdown: dict[str, int] | None = None
    judge_script_name: str | None = None
    # Pre-flight warnings ("OPENAI_API_KEY missing; …"); only populated on the
    # trigger response, omitted on list/get reads.
    warnings: list[str] = field(default_factory=list)


@dataclass(frozen=True, slots=True)
class BenchResult:
    """One per-case score row for a bench run. Returned by
    ``GET /bench-runs/{id}/results`` (which additionally carries the typed
    ``workflow_output`` + ``error``) and emitted per-case over the SSE stream."""

    id: int
    bench_run_id: int
    case_id: str
    status: str
    created_at: str
    workflow_execution_id: str | None = None
    judge_execution_id: str | None = None
    score: Any | None = None
    headline_score: float | None = None
    cost_usd: float = 0.0
    duration_ms: int | None = None
    cache_hit: bool = False
    input_hash: str | None = None
    error: str | None = None
    # Parsed ``WorkflowEnd`` payload from the workflow execution. ``None`` when
    # the workflow failed/cancelled or this is a cache-hit row. Only the
    # ``/results`` read path populates this; SSE ``result`` events omit it.
    workflow_output: Any | None = None


@dataclass(frozen=True, slots=True)
class BenchCase:
    """Server-side projection of an ``executions`` row with ``kind='case'``.

    Returned by the case CRUD + promote endpoints."""

    id: str
    project_id: int
    script_name: str
    kind: str
    frozen: bool
    created_at: str
    bench_id: int | None = None
    case_name: str | None = None
    inputs: Any | None = None
    expected_output: Any | None = None
    ground_truth: Any | None = None
    input_hash: str | None = None


@dataclass(frozen=True, slots=True)
class CompareCase:
    """Per-case score delta from ``GET /bench-runs/{a}/compare/{b}``."""

    case_id: str
    case_label: str
    flag: str  # improved | regressed | unchanged | missing_a | missing_b
    score_a: float | None = None
    score_b: float | None = None
    delta: float | None = None


@dataclass(frozen=True, slots=True)
class CompareAggregate:
    mean_score_delta: float
    cost_delta_usd: float
    n_regressed: int
    n_improved: int
    n_unchanged: int


@dataclass(frozen=True, slots=True)
class CompareReport:
    """Returned by ``GET /bench-runs/{a}/compare/{b}``."""

    run_a_id: int
    run_b_id: int
    aggregate: CompareAggregate
    per_case: list[CompareCase] = field(default_factory=list)


@dataclass(frozen=True, slots=True)
class DriftedCase:
    """One drifted case from the cases contract-drift report."""

    case_id: str
    label: str
    what_broke: str


@dataclass(frozen=True, slots=True)
class DriftReport:
    """Returned by
    ``GET /projects/{id}/scripts/{name}/bench/cases/contract-drift``.

    ``drifted`` empty ⇒ no drift (clients hide the banner)."""

    drifted: list[DriftedCase] = field(default_factory=list)
    script_version_id: int | None = None
    published_at: str | None = None
    published_by: str | None = None
    summary: str = ""


@dataclass(frozen=True, slots=True)
class BenchRunTagSessionResponse:
    """Receipt returned by ``PATCH /bench-runs/{id}/tag-session``."""

    tagged: bool
    run_id: int
    mcp_session_id: str


# ────────────────────────────────────────────────────────────────────────
# MCP
# ────────────────────────────────────────────────────────────────────────


@dataclass(frozen=True, slots=True)
class McpServerSummary:
    alias: str
    url: str
    origin: str  # "env" | "script" | "db"
    is_registry: bool
    status: str
    tool_count: int


@dataclass(frozen=True, slots=True)
class McpToolSummary:
    qualified_name: str
    server_alias: str
    input_schema: dict[str, Any]
    description: str | None = None


@dataclass(frozen=True, slots=True)
class McpHealth:
    status: str
    last_error: str | None = None
    last_check_at: str | None = None


@dataclass(frozen=True, slots=True)
class McpRefreshResult:
    """Response from ``POST /projects/{id}/mcp/servers/{alias}/refresh``."""

    refreshed: bool
    alias: str
    tool_count: int


@dataclass(frozen=True, slots=True)
class McpDriftResult:
    """Response from ``GET /projects/{id}/mcp/servers/{alias}/drift``."""

    drifted: bool
    added: list[str] = field(default_factory=list)
    removed: list[str] = field(default_factory=list)
    reason: str | None = None


# ────────────────────────────────────────────────────────────────────────
# Documents (claim / upload / ingest)
# ────────────────────────────────────────────────────────────────────────


ConversionStatus = Literal["text", "ready", "converting", "pending", "failed", "unknown"]
"""Server-side conversion status of a document blob.

``text`` and ``ready`` are terminal-success states; ``failed`` is terminal-error;
``converting``/``pending`` are in-flight; ``unknown`` is a schema-drift signal."""


IngestPhase = Literal["claiming", "uploading", "converting", "ready"]
"""Phase of an :meth:`DocumentsClient.ingest` call. Surfaced via the
``on_phase`` callback for UI progress indicators."""


@dataclass(frozen=True, slots=True)
class UploadResult:
    """Wire-format response for a successful claim or upload."""

    document_id: str
    filename: str
    content_hash: str
    conversion_status: ConversionStatus  # type: ignore[valid-type]


@dataclass(frozen=True, slots=True)
class ClaimHit:
    """Result of :meth:`DocumentsClient.claim` when the server has the blob."""

    result: UploadResult
    status: Literal["hit"] = "hit"


@dataclass(frozen=True, slots=True)
class ClaimMiss:
    """Result of :meth:`DocumentsClient.claim` when the server doesn't have
    the blob (or it was previously poisoned)."""

    status: Literal["miss"] = "miss"


ClaimResult = ClaimHit | ClaimMiss
"""Discriminated union: hit (server already has the bytes) or miss (caller
must follow up with an upload)."""


@dataclass(frozen=True, slots=True)
class IngestProgress:
    """Per-page progress while a conversion is in flight on the server.

    ``done`` and ``total`` are page counts (not chunks). ``total = 0`` means
    the server hasn't yet rasterized far enough to know — render an
    indeterminate bar in that case."""

    done: int
    total: int


@dataclass(frozen=True, slots=True)
class SandboxInfo:
    """Per-user sandbox info from ``GET /me/sandbox``."""

    project_id: int
