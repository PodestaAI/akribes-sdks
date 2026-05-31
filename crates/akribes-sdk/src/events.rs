//! Normalized, client-friendly workflow events.
//!
//! The raw wire type is [`akribes_types::event::EngineEvent`] (re-exported as
//! [`crate::EngineEvent`]) — use that for layer-1 consumers who want every
//! variant exactly as the engine emits it. For most workflow-driving
//! consumers, prefer [`WorkflowEvent`]: it types the high-traffic variants
//! with real fields and collapses the long tail into a single [`Other`]
//! variant so the SDK stays forward-compatible when the engine adds new
//! events.
//!
//! [`Other`]: WorkflowEvent::Other

use std::collections::HashMap;
use std::time::Duration;

use akribes_types::event::{EngineEvent, TokenUsage};

use crate::models::engine_event_type_name;
use crate::runtime::{
    RuntimeEndPayload, RuntimeErrorPayload, RuntimeEvent, RuntimeStartPayload, RuntimeStderrPayload,
    RuntimeStdoutPayload,
};
use crate::suspend::SuspendTrigger;
use crate::task_end::TaskEndVariant;

/// A normalized, client-friendly engine event.
///
/// High-traffic variants (`Start`, `End`, `TaskStart`, `TaskEnd`,
/// `AgentChunk`, `ToolCallStart`, `ToolCallEnd`, the three suspensions and
/// `Error`) are typed with real fields. Everything else is collapsed into
/// [`Other`](Self::Other), which preserves the wire type name and the raw
/// JSON payload so consumers can still inspect it.
#[derive(Debug, Clone)]
pub enum WorkflowEvent {
    Start {
        total_tasks: usize,
    },
    End {
        output: serde_json::Value,
        duration: Duration,
        /// Aggregate token + cost rollup across every `TaskEnd` in the
        /// workflow scope (issue #1173). Always present in the typed
        /// projection — when the upstream `EngineEvent::WorkflowEnd`
        /// carries the default `WorkflowTotals` (legacy bare-value
        /// wire shape, or a workflow that ran no `TaskEnd`s), every
        /// field is zero. Consumers can pull total tokens off this
        /// without re-walking the per-task stream.
        totals: akribes_types::event::WorkflowTotals,
    },
    TaskStart {
        task: String,
        on_error: Option<String>,
    },
    TaskEnd {
        task: String,
        output: serde_json::Value,
        duration: Duration,
        usage: Option<TokenUsage>,
        /// How the task finished. `Success` for the ordinary path; `Unable`
        /// when the agent emitted a canonical `{"unable": ...}` envelope and
        /// the flow's `on unable <target>` trailer (or default `fail`) took
        /// over. `Unknown` is the forward-compat catch-all for variants a
        /// newer akribes-core might add — see [`TaskEndVariant`].
        variant: TaskEndVariant,
    },
    AgentChunk {
        task: String,
        agent: Option<String>,
        task_id: String,
        chunk: String,
    },
    ToolCallStart {
        task: String,
        tool: String,
        server: String,
        input: serde_json::Value,
    },
    ToolCallEnd {
        task: String,
        tool: String,
        output: serde_json::Value,
        duration: Duration,
    },
    Checkpoint {
        name: String,
        token: String,
        prompt: String,
        schema: serde_json::Value,
        timeout_secs: Option<u64>,
        /// Why the engine suspended. `DagPosition` for a plain
        /// `checkpoint cp(...)` call site; `ValidationExhausted` /
        /// `AgentUnable` when a task-level gate routed here; `Unknown` for
        /// discriminants added in a newer akribes-core the SDK doesn't yet
        /// know about (forward-compat; see [`crate::suspend`]).
        trigger: SuspendTrigger,
    },
    ToolApproval {
        token: String,
        tool_ref: String,
        args: serde_json::Value,
        execution_id: Option<String>,
        node_id: Option<u64>,
    },
    Breakpoint {
        token: String,
        node_id: u64,
        env: HashMap<String, serde_json::Value>,
    },
    Error {
        message: String,
        kind: akribes_types::error::ErrorKind,
        /// Stable diagnostic code (e.g. `"AKRIBES-E-SCRIPT-DEPTH"`). Mirrored
        /// from `akribes_types::event::EngineEvent::Error.code`. `None` on
        /// legacy errors without a registered code (#429).
        code: Option<String>,
    },
    /// A structured-output task's response failed validation. Mirrors
    /// `akribes_types::event::EngineEvent::ValidationFailure`. Emitted in
    /// addition to the existing `Log` line so consumers without this
    /// variant still render the human-readable summary, but tooling that
    /// knows about the variant can render the model's actual response,
    /// the schema-validator's structured error breakdown, and the
    /// provider's `stop_reason` (so e.g. a `max_tokens` truncation isn't
    /// misdiagnosed as "schema overflow" — see issue #320).
    ValidationFailure {
        task_name: String,
        /// 1-indexed attempt number.
        attempt: u32,
        /// Raw text / JSON the validator saw, exactly as the model emitted.
        model_response: String,
        /// JSON-pointer paths to required fields the schema validator
        /// flagged as absent.
        missing_fields: Vec<String>,
        /// Paths to fields rejected by `additionalProperties: false`.
        extra_fields: Vec<String>,
        /// Human-readable type / value mismatches (e.g. `"expected string,
        /// got null at /name"`).
        type_errors: Vec<String>,
        /// Provider's stop_reason when known. `None` for streaming paths
        /// that don't surface usage.
        stop_reason: Option<String>,
    },
    /// A `runtime` block began dispatching to the sandbox executor.
    /// Mirrors `EngineEvent::RuntimeStart`. `task_name` matches the
    /// wrapping task's name so reducers that group by task continue to
    /// work; `runtime_name` is the source-declared block name.
    RuntimeStart {
        task_name: String,
        runtime_name: String,
        /// `"python" | "bash" | "node" | "rust" | "java"`. Free-form
        /// string on the wire so a future language doesn't require an
        /// SDK release.
        language: String,
    },
    /// One chunk of stdout from a running `runtime` block. Many may fire
    /// per invocation; consumers should accumulate.
    RuntimeStdout { task_name: String, chunk: String },
    /// One chunk of stderr from a running `runtime` block.
    RuntimeStderr { task_name: String, chunk: String },
    /// A `runtime` block completed (the executor returned an
    /// `ExecResult`). A non-zero `exit_code` is still a `RuntimeEnd` —
    /// infrastructure failures (timeout / OOM / unreachable sandbox)
    /// emit [`Self::RuntimeError`] instead.
    RuntimeEnd {
        task_name: String,
        exit_code: i32,
        duration_ms: u64,
    },
    /// A `runtime` block failed to complete. `kind` is a stable wire
    /// string mirroring the engine's `RuntimeError` enum
    /// (`NotConfigured` / `Timeout` / `SandboxUnavailable` / `OomKilled`
    /// / `Internal`); use [`crate::runtime::RuntimeErrorKind::from_wire`]
    /// to pattern-match without re-parsing the string.
    RuntimeError {
        task_name: String,
        kind: String,
        message: String,
    },
    /// Catch-all for variants that don't need dedicated fields in the SDK:
    /// `StateUpdate`, `Log`, `NodeStart`, `NodeEnd`, `Resumed`,
    /// `BreakpointResumed`, `McpServerDegraded`, `McpServerRecovered`,
    /// `TaskPrompt`, `VerificationStart`, `VerificationResult`.
    ///
    /// Preserves the original wire type name and JSON payload for consumers
    /// who want to reach in and pick them apart.
    Other {
        type_name: String,
        payload: serde_json::Value,
    },
}

/// Coarse category tag for a [`WorkflowEvent`], useful for routing (e.g.
/// "show only progress in the status bar", "write tool events to a log").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventCategory {
    Progress,
    Output,
    Tool,
    Suspend,
    Error,
    Other,
}

impl WorkflowEvent {
    /// Coarse routing category for this event.
    pub fn category(&self) -> EventCategory {
        match self {
            Self::Start { .. }
            | Self::End { .. }
            | Self::TaskStart { .. }
            | Self::TaskEnd { .. } => EventCategory::Progress,
            // `RuntimeStart` and `RuntimeEnd` are the bookends for a
            // single sandbox dispatch — they sit alongside `TaskStart` /
            // `TaskEnd` rather than producing user-visible output, so
            // they group with Progress for status-bar consumers.
            Self::RuntimeStart { .. } | Self::RuntimeEnd { .. } => EventCategory::Progress,
            Self::AgentChunk { .. } => EventCategory::Output,
            // Runtime stdout/stderr are the equivalent of `AgentChunk`
            // for the executor path — typed text the consumer is meant
            // to surface to the user as it streams in.
            Self::RuntimeStdout { .. } | Self::RuntimeStderr { .. } => EventCategory::Output,
            Self::ValidationFailure { .. } => EventCategory::Output,
            Self::ToolCallStart { .. } | Self::ToolCallEnd { .. } => EventCategory::Tool,
            Self::Checkpoint { .. } | Self::ToolApproval { .. } | Self::Breakpoint { .. } => {
                EventCategory::Suspend
            }
            Self::Error { .. } => EventCategory::Error,
            // Sandbox infrastructure failure (timeout / OOM / unreachable)
            // is an in-task error — the wrapping `TaskEnd` is what marks
            // the workflow's outcome. Categorise as Error so consumers
            // that filter by category still see it surface.
            Self::RuntimeError { .. } => EventCategory::Error,
            Self::Other { .. } => EventCategory::Other,
        }
    }

    /// Whether this event is terminal for a workflow run. Used by
    /// [`crate::RunStream`] to decide when to stop yielding.
    ///
    /// `Runtime*` events are NOT terminal — they live inside a task that
    /// is itself bookended by `TaskStart` / `TaskEnd`, and the workflow
    /// only ends on the outer `End` / `Error` events.
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::End { .. } | Self::Error { .. })
    }
}

impl From<EngineEvent> for WorkflowEvent {
    fn from(evt: EngineEvent) -> Self {
        // #993 back-compat: legacy emissions wrapped each call-stack
        // level in its own SubScript envelope. The post-#993 emit path
        // already flattens before sending, but old persisted event logs
        // may still nest. Normalize once at the SDK boundary so
        // downstream consumers always see the flat `parent_path` shape.
        let evt = evt.flatten_subscript_chain();
        match evt {
            EngineEvent::WorkflowStart(total_tasks) => Self::Start { total_tasks },

            EngineEvent::WorkflowEnd(payload) => Self::End {
                output: payload.value.to_json(),
                // `WorkflowEnd` on the wire has no duration field — the
                // server-side timing lives in `ExecutionStatus`. Expose
                // `Duration::ZERO` so the struct still has a non-optional
                // field; consumers who need wall-clock duration should use
                // `ExecutionsClient::get`.
                duration: Duration::ZERO,
                totals: payload.totals,
            },

            EngineEvent::TaskStart(task, on_error) => Self::TaskStart { task, on_error },

            EngineEvent::TaskEnd {
                task,
                on_error_label: _,
                value,
                value_type: _,
                duration,
                attempt: _,
                usage,
                variant,
            } => Self::TaskEnd {
                task,
                output: value.to_json(),
                duration,
                usage,
                variant: variant.into(),
            },

            EngineEvent::AgentOutput {
                task_name,
                agent_name,
                task_id,
                schema_type: _,
                chunk,
            } => Self::AgentChunk {
                task: task_name,
                agent: agent_name,
                task_id,
                chunk,
            },

            EngineEvent::ToolCallStart {
                task_name,
                tool_name,
                server_name,
                input,
                ..
            } => Self::ToolCallStart {
                task: task_name,
                tool: tool_name,
                server: server_name,
                input,
            },

            EngineEvent::ToolCallEnd {
                task_name,
                tool_name,
                output,
                duration,
                ..
            } => Self::ToolCallEnd {
                task: task_name,
                tool: tool_name,
                output,
                duration,
            },

            EngineEvent::Suspended {
                checkpoint_name,
                token,
                prompt,
                schema,
                actor_hint: _,
                timeout_secs,
                trigger,
                // The loop context lives on the engine event for the
                // server's persistence path; it is not surfaced through
                // the public WorkflowEvent today (consumers reading the
                // SDK's typed event stream are happy with the existing
                // `Checkpoint` shape — Studio reads the raw EngineEvent
                // when it needs the loop_id). Drop here.
                loop_context: _,
            } => Self::Checkpoint {
                name: checkpoint_name,
                token,
                prompt,
                schema,
                timeout_secs,
                trigger: trigger.into(),
            },

            EngineEvent::ToolApprovalPending {
                execution_id,
                node_id,
                token,
                tool_ref,
                args,
            } => Self::ToolApproval {
                token,
                tool_ref,
                args,
                execution_id,
                node_id,
            },

            EngineEvent::Breakpoint {
                node_id,
                span: _,
                token,
                env_snapshot,
            } => Self::Breakpoint {
                token,
                node_id: node_id as u64,
                env: env_snapshot
                    .into_iter()
                    .map(|(k, v)| (k, v.to_json()))
                    .collect(),
            },

            // Project the engine's richer envelope down to the SDK's
            // legacy two-fields-plus-string-code shape. The full detail
            // (`user_message`, `retry_after_ms`, `source`) is available on
            // the underlying `EngineEvent` for SDK consumers that read the
            // raw event stream.
            EngineEvent::Error { message, kind, code, .. } => Self::Error {
                message,
                kind,
                code: Some(code.as_wire().to_string()),
            },

            EngineEvent::ValidationFailure {
                task_name,
                attempt,
                model_response,
                missing_fields,
                extra_fields,
                type_errors,
                stop_reason,
                truncated: _,
                total_length: _,
            } => Self::ValidationFailure {
                task_name,
                attempt,
                model_response,
                missing_fields,
                extra_fields,
                type_errors,
                stop_reason,
            },

            // Long tail — anything we don't type explicitly becomes `Other`
            // with the wire name and raw JSON preserved. This keeps the SDK
            // forward-compatible: when akribes-core adds a new EngineEvent
            // variant, existing consumers of WorkflowEvent keep compiling
            // and see it as `Other` instead of breaking.
            other => {
                let type_name = engine_event_type_name(&other).to_string();
                let payload = serde_json::to_value(&other).unwrap_or(serde_json::Value::Null);
                Self::Other { type_name, payload }
            }
        }
    }
}

// ── RuntimeEvent → WorkflowEvent ────────────────────────────────────────────
//
// Lets [`WorkflowEvent::from_envelope_json`] (and any future helper) materialise
// typed runtime arms without touching `EngineEvent`. When `EngineEvent` gains
// the corresponding `Runtime*` variants in akribes-core (unit 3), the
// `From<EngineEvent>` impl above can match them directly and call into these
// same arms.

impl From<RuntimeEvent> for WorkflowEvent {
    fn from(evt: RuntimeEvent) -> Self {
        match evt {
            RuntimeEvent::RuntimeStart(RuntimeStartPayload {
                task_name,
                runtime_name,
                language,
            }) => Self::RuntimeStart {
                task_name,
                runtime_name,
                language,
            },
            RuntimeEvent::RuntimeStdout(RuntimeStdoutPayload { task_name, chunk }) => {
                Self::RuntimeStdout { task_name, chunk }
            }
            RuntimeEvent::RuntimeStderr(RuntimeStderrPayload { task_name, chunk }) => {
                Self::RuntimeStderr { task_name, chunk }
            }
            RuntimeEvent::RuntimeEnd(RuntimeEndPayload {
                task_name,
                exit_code,
                duration_ms,
            }) => Self::RuntimeEnd {
                task_name,
                exit_code,
                duration_ms,
            },
            RuntimeEvent::RuntimeError(RuntimeErrorPayload {
                task_name,
                kind,
                message,
            }) => Self::RuntimeError {
                task_name,
                kind,
                message,
            },
        }
    }
}

// ── Wire-envelope decoder ──────────────────────────────────────────────────
//
// Today's main decoder is `From<EngineEvent>`: every JSON envelope is parsed
// to `EngineEvent` first and then projected to `WorkflowEvent`. That works for
// every variant `akribes-core` already knows about, but the engine's
// `Runtime*` variants ship in a sibling unit (#3) — until those land, JSON
// envelopes carrying `"type": "RuntimeStart"` and friends would fail to
// deserialise as `EngineEvent` and surface to the consumer as a parse error.
//
// `from_envelope_json` runs the runtime decoder first: it accepts any
// `{type, payload}` JSON and returns a typed `WorkflowEvent` arm if the
// envelope matches one of the five canonical `Runtime*` types. Anything else
// falls back through `EngineEvent` deserialisation (preserving the existing
// `Other` long-tail behaviour). The result is that the SDK can emit typed
// runtime arms today without waiting on the engine merge, and once unit 3
// lands the `EngineEvent` decoder catches the same envelopes at the same
// place — both paths produce equivalent typed `WorkflowEvent` values.

/// Decode error returned by [`WorkflowEvent::from_envelope_json`].
#[derive(Debug, thiserror::Error)]
pub enum EnvelopeDecodeError {
    /// The envelope's `"type"` was a recognised `Runtime*` tag but the
    /// payload failed to deserialise — wire shape mismatch.
    #[error("invalid Runtime envelope: {0}")]
    Runtime(#[source] serde_json::Error),
    /// The envelope did not match the runtime decoder *or* the engine
    /// decoder. The wrapped error is from the `EngineEvent` parser.
    #[error("failed to decode engine event: {0}")]
    Engine(#[source] serde_json::Error),
}

impl WorkflowEvent {
    /// Decode a raw `{type, payload}` JSON envelope into a typed
    /// [`WorkflowEvent`].
    ///
    /// Tries the `Runtime*` decoder first (5 canonical types from
    /// `crates/akribes-core/src/event.rs`). If the envelope's `"type"` is
    /// not one of those, it falls back to deserialising the JSON as
    /// [`EngineEvent`] and routing through the existing
    /// [`From<EngineEvent>`] projection.
    ///
    /// Returns [`EnvelopeDecodeError::Engine`] if both paths fail. The
    /// runtime decoder only errors when the `"type"` *was* a runtime tag
    /// but the payload shape was wrong — that surfaces as
    /// [`EnvelopeDecodeError::Runtime`] and is not retried via the engine
    /// path (a payload-shape mismatch on a known runtime tag is the only
    /// way the runtime arm could be lossy, so we surface it explicitly).
    pub fn from_envelope_json(value: serde_json::Value) -> Result<Self, EnvelopeDecodeError> {
        // Peek at the wire `"type"` to decide which decoder to invoke.
        // We deliberately don't pre-validate the shape — let the typed
        // decoders error normally.
        let type_tag = value.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if matches!(
            type_tag,
            "RuntimeStart" | "RuntimeStdout" | "RuntimeStderr" | "RuntimeEnd" | "RuntimeError"
        ) {
            let runtime: RuntimeEvent = serde_json::from_value(value)
                .map_err(EnvelopeDecodeError::Runtime)?;
            return Ok(runtime.into());
        }
        let engine: EngineEvent =
            serde_json::from_value(value).map_err(EnvelopeDecodeError::Engine)?;
        Ok(engine.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use akribes_types::ast::Span;
    use akribes_types::error::ErrorKind;
    use akribes_types::value::Value;

    fn span() -> Span {
        Span { line: 1, col: 1, end_line: 1, end_col: 1 }
    }

    #[test]
    fn start_and_end_map_to_progress() {
        let start: WorkflowEvent = EngineEvent::WorkflowStart(5).into();
        assert!(matches!(start, WorkflowEvent::Start { total_tasks: 5 }));
        assert_eq!(start.category(), EventCategory::Progress);

        let end: WorkflowEvent =
            EngineEvent::WorkflowEnd(akribes_types::event::WorkflowEndPayload::new(Value::String("done".into()))).into();
        match end {
            WorkflowEvent::End { output, .. } => {
                assert_eq!(output, serde_json::Value::String("done".into()));
            }
            _ => panic!("expected End"),
        }
    }

    #[test]
    fn agent_output_maps_to_chunk() {
        let evt: WorkflowEvent = EngineEvent::AgentOutput {
            task_name: "summarise".into(),
            agent_name: Some("gpt".into()),
            task_id: "t1".into(),
            schema_type: None,
            chunk: "hi".into(),
        }
        .into();
        match evt {
            WorkflowEvent::AgentChunk { task, agent, task_id, chunk } => {
                assert_eq!(task, "summarise");
                assert_eq!(agent.as_deref(), Some("gpt"));
                assert_eq!(task_id, "t1");
                assert_eq!(chunk, "hi");
            }
            _ => panic!("expected AgentChunk"),
        }
    }

    #[test]
    fn tool_calls_map_to_tool_category() {
        let start: WorkflowEvent = EngineEvent::ToolCallStart {
            task_name: "t".into(),
            tool_name: "web".into(),
            server_name: "s".into(),
            input: serde_json::json!({"q": "hi"}),
            tool_use_id: String::new(),
        }
        .into();
        assert_eq!(start.category(), EventCategory::Tool);

        let end: WorkflowEvent = EngineEvent::ToolCallEnd {
            task_name: "t".into(),
            tool_name: "web".into(),
            tool_use_id: String::new(),
            output: serde_json::json!({"r": "ok"}),
            duration: Duration::from_millis(10),
        }
        .into();
        assert_eq!(end.category(), EventCategory::Tool);
    }

    #[test]
    fn suspended_maps_to_checkpoint() {
        let evt: WorkflowEvent = EngineEvent::Suspended {
            checkpoint_name: "approve".into(),
            token: "tok".into(),
            prompt: "please".into(),
            schema: serde_json::json!({}),
            actor_hint: akribes_types::ast::ActorHint::Any,
            timeout_secs: Some(30),
            trigger: akribes_types::event::SuspendTrigger::DagPosition,
            loop_context: None,
        }
        .into();
        assert_eq!(evt.category(), EventCategory::Suspend);
        match evt {
            WorkflowEvent::Checkpoint { name, token, timeout_secs, trigger, .. } => {
                assert_eq!(name, "approve");
                assert_eq!(token, "tok");
                assert_eq!(timeout_secs, Some(30));
                assert!(matches!(trigger, SuspendTrigger::DagPosition));
            }
            _ => panic!("expected Checkpoint"),
        }
    }

    #[test]
    fn suspended_with_validation_exhausted_trigger_survives_translation() {
        let evt: WorkflowEvent = EngineEvent::Suspended {
            checkpoint_name: "review".into(),
            token: "tok".into(),
            prompt: "please review".into(),
            schema: serde_json::json!({}),
            actor_hint: akribes_types::ast::ActorHint::Human,
            timeout_secs: None,
            trigger: akribes_types::event::SuspendTrigger::ValidationExhausted {
                task_name: "decompose".into(),
                retry_count: 3,
                last_attempt: "{\"bad\":true}".into(),
                validation_errors: vec![akribes_types::event::ValidationErrorWire {
                    stage: "schema".into(),
                    message: "missing number".into(),
                    path: Some("/0".into()),
                }],
            },
            loop_context: None,
        }
        .into();
        match evt {
            WorkflowEvent::Checkpoint { trigger, .. } => match trigger {
                SuspendTrigger::ValidationExhausted {
                    task_name,
                    retry_count,
                    validation_errors,
                    ..
                } => {
                    assert_eq!(task_name, "decompose");
                    assert_eq!(retry_count, 3);
                    assert_eq!(validation_errors.len(), 1);
                    assert_eq!(validation_errors[0].stage, "schema");
                }
                other => panic!("expected ValidationExhausted, got {other:?}"),
            },
            _ => panic!("expected Checkpoint"),
        }
    }

    #[test]
    fn suspended_with_agent_unable_trigger_survives_translation() {
        let evt: WorkflowEvent = EngineEvent::Suspended {
            checkpoint_name: "escalate".into(),
            token: "tok".into(),
            prompt: "take over".into(),
            schema: serde_json::json!({}),
            actor_hint: akribes_types::ast::ActorHint::Human,
            timeout_secs: None,
            trigger: akribes_types::event::SuspendTrigger::AgentUnable {
                task_name: "decompose".into(),
                unable: akribes_types::value::UnableRecord {
                    reason: "image too blurry".into(),
                    missing: vec!["claim_text".into()],
                    category: akribes_types::value::UnableCategory::InputAmbiguous,
                },
            },
            loop_context: None,
        }
        .into();
        match evt {
            WorkflowEvent::Checkpoint { trigger, .. } => match trigger {
                SuspendTrigger::AgentUnable { task_name, unable } => {
                    assert_eq!(task_name, "decompose");
                    assert_eq!(unable.reason, "image too blurry");
                    assert_eq!(unable.category, "input_ambiguous");
                    assert_eq!(unable.missing, vec!["claim_text".to_string()]);
                }
                other => panic!("expected AgentUnable, got {other:?}"),
            },
            _ => panic!("expected Checkpoint"),
        }
    }

    #[test]
    fn tool_approval_has_suspend_category() {
        let evt: WorkflowEvent = EngineEvent::ToolApprovalPending {
            execution_id: Some("exec".into()),
            node_id: Some(1),
            token: "tk".into(),
            tool_ref: "web".into(),
            args: serde_json::json!({}),
        }
        .into();
        assert_eq!(evt.category(), EventCategory::Suspend);
    }

    #[test]
    fn log_has_other_category() {
        let evt: WorkflowEvent = EngineEvent::Log("hello".into()).into();
        assert_eq!(evt.category(), EventCategory::Other);
    }

    #[test]
    fn tool_approval_maps() {
        let evt: WorkflowEvent = EngineEvent::ToolApprovalPending {
            execution_id: Some("e1".into()),
            node_id: Some(42),
            token: "tok".into(),
            tool_ref: "web.search".into(),
            args: serde_json::json!({"q": "hi"}),
        }
        .into();
        match evt {
            WorkflowEvent::ToolApproval { token, tool_ref, node_id, .. } => {
                assert_eq!(token, "tok");
                assert_eq!(tool_ref, "web.search");
                assert_eq!(node_id, Some(42));
            }
            _ => panic!("expected ToolApproval"),
        }
    }

    #[test]
    fn breakpoint_casts_node_id() {
        let mut env = std::collections::HashMap::new();
        env.insert("x".to_string(), Value::Int(7));
        let evt: WorkflowEvent = EngineEvent::Breakpoint {
            node_id: 3usize,
            span: span(),
            token: "tok".into(),
            env_snapshot: env,
        }
        .into();
        match evt {
            WorkflowEvent::Breakpoint { token, node_id, env } => {
                assert_eq!(token, "tok");
                assert_eq!(node_id, 3u64);
                assert_eq!(env.get("x"), Some(&serde_json::json!(7)));
            }
            _ => panic!("expected Breakpoint"),
        }
    }

    #[test]
    fn error_maps_to_error_category() {
        let evt: WorkflowEvent =
            EngineEvent::error_kind(ErrorKind::ScriptError, "boom").into();
        assert_eq!(evt.category(), EventCategory::Error);
        match evt {
            WorkflowEvent::Error { message, kind, .. } => {
                assert_eq!(message, "boom");
                assert_eq!(kind, ErrorKind::ScriptError);
            }
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn task_end_preserves_usage() {
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 20,
            model: "m".into(),
            provider: "p".into(),
            cached_input_tokens: 0,
            cache_write_input_tokens: 0,
            cache_write_5m_input_tokens: 0,
            cache_write_1h_input_tokens: 0,
            stop_reason: None,
            raw_stop_reason: None,
            reasoning_tokens: 0,
        };
        let evt: WorkflowEvent = EngineEvent::TaskEnd {
            task: "t".into(),
            on_error_label: None,
            value: Value::String("ok".into()),
            value_type: None,
            duration: Duration::from_millis(100),
            attempt: 1,
            usage: Some(usage),
            variant: akribes_types::event::TaskEndVariant::Success,
        }
        .into();
        match evt {
            WorkflowEvent::TaskEnd { task, usage, duration, variant, .. } => {
                assert_eq!(task, "t");
                assert_eq!(duration, Duration::from_millis(100));
                assert_eq!(usage.unwrap().input_tokens, 10);
                assert_eq!(variant, TaskEndVariant::Success);
            }
            _ => panic!("expected TaskEnd"),
        }
    }

    #[test]
    fn task_end_propagates_unable_variant() {
        // Wave-1 #206: `variant` on TaskEnd distinguishes Unable from
        // Success without consumers having to re-parse the value payload.
        let evt: WorkflowEvent = EngineEvent::TaskEnd {
            task: "decompose".into(),
            on_error_label: None,
            value: Value::Unable(akribes_types::value::UnableRecord {
                reason: "image too blurry".into(),
                missing: vec![],
                category: akribes_types::value::UnableCategory::InputAmbiguous,
            }),
            value_type: None,
            duration: Duration::from_millis(10),
            attempt: 1,
            usage: None,
            variant: akribes_types::event::TaskEndVariant::Unable,
        }
        .into();
        match evt {
            WorkflowEvent::TaskEnd { variant, .. } => {
                assert_eq!(variant, TaskEndVariant::Unable);
            }
            _ => panic!("expected TaskEnd"),
        }
    }

    // ── Long-tail variants fall into Other ──────────────────────────────────

    fn assert_other_named(evt: EngineEvent, expected: &str) {
        let wf: WorkflowEvent = evt.into();
        match wf {
            WorkflowEvent::Other { type_name, .. } => {
                assert_eq!(type_name, expected);
            }
            _ => panic!("expected Other({}), got {:?}", expected, wf),
        }
    }

    #[test]
    fn log_is_other() {
        assert_other_named(EngineEvent::Log("hi".into()), "Log");
    }

    #[test]
    fn state_update_is_other() {
        assert_other_named(
            EngineEvent::StateUpdate("x".into(), Value::Int(1)),
            "StateUpdate",
        );
    }

    #[test]
    fn node_start_end_are_other() {
        assert_other_named(EngineEvent::NodeStart(0, span()), "NodeStart");
        assert_other_named(
            EngineEvent::NodeEnd {
                node_id: 0,
                span: span(),
                target_var: None,
                value: None,
                duration: Duration::ZERO,
            },
            "NodeEnd",
        );
    }

    #[test]
    fn resumed_is_other() {
        assert_other_named(
            EngineEvent::Resumed {
                checkpoint_name: "c".into(),
                token: "t".into(),
            },
            "Resumed",
        );
    }

    #[test]
    fn breakpoint_resumed_is_other() {
        assert_other_named(
            EngineEvent::BreakpointResumed {
                node_id: 1,
                token: "t".into(),
            },
            "BreakpointResumed",
        );
    }

    #[test]
    fn mcp_degraded_recovered_are_other() {
        assert_other_named(
            EngineEvent::McpServerDegraded {
                alias: "a".into(),
                reason: "r".into(),
            },
            "McpServerDegraded",
        );
        assert_other_named(
            EngineEvent::McpServerRecovered { alias: "a".into() },
            "McpServerRecovered",
        );
    }

    #[test]
    fn task_prompt_is_other() {
        assert_other_named(
            EngineEvent::TaskPrompt("t".into(), "p".into()),
            "TaskPrompt",
        );
    }

    #[test]
    fn verification_events_are_other() {
        assert_other_named(
            EngineEvent::VerificationStart { workflow_name: "w".into() },
            "VerificationStart",
        );
        assert_other_named(
            EngineEvent::VerificationResult {
                workflow_name: "w".into(),
                results: serde_json::json!({}),
                duration: Duration::ZERO,
            },
            "VerificationResult",
        );
    }

    #[test]
    fn other_payload_preserves_type_tag() {
        let evt: WorkflowEvent = EngineEvent::Log("hello".into()).into();
        match evt {
            WorkflowEvent::Other { type_name, payload } => {
                assert_eq!(type_name, "Log");
                assert_eq!(payload["type"], "Log");
                assert_eq!(payload["payload"], "hello");
            }
            _ => panic!("expected Other"),
        }
    }

    #[test]
    fn validation_failure_maps_to_typed_variant() {
        let evt: WorkflowEvent = EngineEvent::ValidationFailure {
            task_name: "decompose".into(),
            attempt: 2,
            model_response: "{}".into(),
            missing_fields: vec!["/claim_text".into()],
            extra_fields: vec![],
            type_errors: vec![],
            stop_reason: Some("max_tokens".into()),
            truncated: false,
            total_length: 2,
        }
        .into();
        match evt {
            WorkflowEvent::ValidationFailure {
                task_name,
                attempt,
                model_response,
                missing_fields,
                extra_fields,
                type_errors,
                stop_reason,
            } => {
                assert_eq!(task_name, "decompose");
                assert_eq!(attempt, 2);
                assert_eq!(model_response, "{}");
                assert_eq!(missing_fields, vec!["/claim_text".to_string()]);
                assert!(extra_fields.is_empty());
                assert!(type_errors.is_empty());
                assert_eq!(stop_reason.as_deref(), Some("max_tokens"));
            }
            other => panic!("expected ValidationFailure, got {:?}", other),
        }
    }

    #[test]
    fn validation_failure_has_output_category() {
        let evt: WorkflowEvent = EngineEvent::ValidationFailure {
            task_name: "t".into(),
            attempt: 1,
            model_response: "".into(),
            missing_fields: vec![],
            extra_fields: vec![],
            type_errors: vec![],
            stop_reason: None,
            truncated: false,
            total_length: 0,
        }
        .into();
        assert_eq!(evt.category(), EventCategory::Output);
    }

    // ── Runtime* variants ───────────────────────────────────────────────────
    //
    // Field-level decode roundtrips live in `tests/runtime_events.rs` (via the
    // JSON envelope decoder). These lib tests cover what that crate can't:
    // the direct `RuntimeEvent → WorkflowEvent` projection (`From` impl) and
    // the `pub(crate)` `is_terminal()` invariant.

    use crate::runtime::{
        RuntimeEndPayload, RuntimeErrorPayload, RuntimeEvent, RuntimeStartPayload,
        RuntimeStderrPayload, RuntimeStdoutPayload,
    };

    #[test]
    fn runtime_event_projects_every_variant() {
        let cases: [(RuntimeEvent, fn(&WorkflowEvent) -> bool); 5] = [
            (
                RuntimeEvent::RuntimeStart(RuntimeStartPayload {
                    task_name: "t".into(),
                    runtime_name: "r".into(),
                    language: "python".into(),
                }),
                |e| matches!(e, WorkflowEvent::RuntimeStart { language, .. } if language == "python"),
            ),
            (
                RuntimeEvent::RuntimeStdout(RuntimeStdoutPayload {
                    task_name: "t".into(),
                    chunk: "x".into(),
                }),
                |e| matches!(e, WorkflowEvent::RuntimeStdout { chunk, .. } if chunk == "x"),
            ),
            (
                RuntimeEvent::RuntimeStderr(RuntimeStderrPayload {
                    task_name: "t".into(),
                    chunk: "x".into(),
                }),
                |e| matches!(e, WorkflowEvent::RuntimeStderr { .. }),
            ),
            (
                RuntimeEvent::RuntimeEnd(RuntimeEndPayload {
                    task_name: "t".into(),
                    exit_code: 0,
                    duration_ms: 4242,
                }),
                |e| matches!(e, WorkflowEvent::RuntimeEnd { duration_ms: 4242, .. }),
            ),
            (
                RuntimeEvent::RuntimeError(RuntimeErrorPayload {
                    task_name: "t".into(),
                    kind: "Timeout".into(),
                    message: "x".into(),
                }),
                |e| matches!(e, WorkflowEvent::RuntimeError { kind, .. } if kind == "Timeout"),
            ),
        ];
        for (input, check) in cases {
            let evt: WorkflowEvent = input.into();
            assert!(check(&evt), "projection failed: {evt:?}");
        }
    }

    #[test]
    fn runtime_events_are_not_terminal() {
        // Runtime* events sit inside a task; the workflow terminates only
        // on the outer End/Error. RunStream must not stop on them.
        let events = [
            WorkflowEvent::RuntimeStart {
                task_name: "t".into(),
                runtime_name: "r".into(),
                language: "python".into(),
            },
            WorkflowEvent::RuntimeStdout {
                task_name: "t".into(),
                chunk: "x".into(),
            },
            WorkflowEvent::RuntimeStderr {
                task_name: "t".into(),
                chunk: "x".into(),
            },
            WorkflowEvent::RuntimeEnd {
                task_name: "t".into(),
                exit_code: 0,
                duration_ms: 0,
            },
            WorkflowEvent::RuntimeError {
                task_name: "t".into(),
                kind: "Timeout".into(),
                message: "x".into(),
            },
        ];
        for evt in events {
            assert!(!evt.is_terminal(), "{evt:?} should not be terminal");
        }
    }
}
