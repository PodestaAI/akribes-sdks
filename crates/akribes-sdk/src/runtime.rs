//! SDK-facing typed mirror of the engine's `Runtime*` events.
//!
//! These five variants describe the lifecycle of a `runtime` block — Akribes's
//! first-class construct for running AI-generated code inside a sandboxed
//! container (Python, Bash, Node, Rust, Java). They flow alongside the
//! wrapping task's `TaskStart` / `TaskEnd` so reducers that group by task
//! continue to work.
//!
//! Wire shape uses the engine's standard tagged envelope
//! `{"type": "<Variant>", "payload": {...}}` — same as `TaskStart`, `TaskEnd`,
//! `ToolCallStart`. See `crates/akribes-core/src/event.rs` for the
//! source-of-truth `EngineEvent` enum.
//!
//! ```json
//! {"type": "RuntimeStart",  "payload": {"task_name": "t", "runtime_name": "run_py", "language": "python"}}
//! {"type": "RuntimeStdout", "payload": {"task_name": "t", "chunk": "hello\n"}}
//! {"type": "RuntimeStderr", "payload": {"task_name": "t", "chunk": "warn\n"}}
//! {"type": "RuntimeEnd",    "payload": {"task_name": "t", "exit_code": 0, "duration_ms": 1234}}
//! {"type": "RuntimeError",  "payload": {"task_name": "t", "kind": "Timeout", "message": "..."}}
//! ```
//!
//! `RuntimeError.kind` is a free-form string mirroring the engine's
//! `RuntimeError` enum names (`NotConfigured`, `Timeout`, `SandboxUnavailable`,
//! `OomKilled`, `Cancelled`, `Internal`). Consumers that want a typed match
//! should use [`RuntimeErrorKind::from_wire`].

use serde::{Deserialize, Serialize};

/// `RuntimeStart` — emitted once when the engine dispatches a `runtime`
/// block to the executor. Carries the wrapping task's name, the
/// runtime block's declared name, and the language tag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStartPayload {
    /// Name of the task that wraps this runtime call (matches the
    /// surrounding `TaskStart` / `TaskEnd` events).
    pub task_name: String,
    /// Name of the `runtime` block as declared in the source.
    pub runtime_name: String,
    /// Language tag — `"python" | "bash" | "node" | "rust" | "java"`.
    /// Free-form string on the wire so a future language gets a new value
    /// without an SDK release.
    pub language: String,
}

/// `RuntimeStdout` — one chunk of stdout from the running container.
/// Many of these may fire per invocation; consumers should accumulate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStdoutPayload {
    pub task_name: String,
    /// Raw stdout bytes decoded as UTF-8 (invalid bytes replaced lossily).
    pub chunk: String,
}

/// `RuntimeStderr` — one chunk of stderr from the running container.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStderrPayload {
    pub task_name: String,
    pub chunk: String,
}

/// `RuntimeEnd` — emitted exactly once when the runtime invocation
/// finished successfully (the executor returned an `ExecResult`).
/// `exit_code == 0` is the conventional success signal but the engine
/// surfaces non-zero codes here too — only true infrastructure failures
/// (timeout, OOM, sandbox unreachable) emit [`RuntimeErrorPayload`] instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeEndPayload {
    pub task_name: String,
    pub exit_code: i32,
    pub duration_ms: u64,
}

/// `RuntimeError` — emitted instead of `RuntimeEnd` when the runtime
/// invocation could not complete (timeout, OOM, sandbox unavailable,
/// configuration missing, …). `kind` is a stable string tag mirroring
/// the engine's `RuntimeError` enum; `message` is human-readable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeErrorPayload {
    pub task_name: String,
    /// One of `"NotConfigured" | "Timeout" | "SandboxUnavailable" |
    /// "OomKilled" | "Cancelled" | "Internal"`. Other strings are
    /// forward-compatible future kinds; see [`RuntimeErrorKind`].
    pub kind: String,
    pub message: String,
}

/// Typed mirror of the engine's `RuntimeError` enum for the
/// `kind` field on [`RuntimeErrorPayload`]. Use [`RuntimeErrorKind::from_wire`]
/// to dispatch; unknown strings surface as [`RuntimeErrorKind::Unknown`] so
/// the SDK stays forward-compatible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeErrorKind {
    /// `AKRIBES_SANDBOX_URL` / `_TOKEN` not set on the server.
    NotConfigured,
    /// Execution exceeded the runtime block's `timeout_secs`.
    Timeout,
    /// The sandbox service was unreachable / dropped the connection.
    SandboxUnavailable,
    /// Container exceeded its `memory_mb` cap (OOM killer fired).
    OomKilled,
    /// User (or engine cancellation token) terminated the call before
    /// it produced an exit code. **Terminal** — retry policies should
    /// NOT auto-retry on this kind; treat it the same as a `Skip`
    /// (propagate the cancel upward).
    Cancelled,
    /// Catch-all for other failures (sandbox 5xx, unknown wire kind, …).
    Internal,
    /// Forward-compat: the engine emitted a `kind` string this SDK
    /// release does not know about. Read the raw wire `kind` from the
    /// surrounding [`RuntimeErrorPayload::kind`] field.
    Unknown,
}

impl RuntimeErrorKind {
    /// Map the wire string to a typed variant. Recognises the six
    /// canonical engine kinds; anything else returns [`Self::Unknown`].
    pub fn from_wire(s: &str) -> Self {
        match s {
            "NotConfigured" => Self::NotConfigured,
            "Timeout" => Self::Timeout,
            "SandboxUnavailable" => Self::SandboxUnavailable,
            "OomKilled" => Self::OomKilled,
            "Cancelled" => Self::Cancelled,
            "Internal" => Self::Internal,
            _ => Self::Unknown,
        }
    }

    /// Emit the stable wire-form string for this kind, mirroring the
    /// engine's [`as_wire_str`]. Returns `None` for [`Self::Unknown`]
    /// because a forward-compat unknown tag has no canonical wire form
    /// to round-trip through.
    ///
    /// [`as_wire_str`]: ../../akribes_core/code_exec/enum.RuntimeError.html#method.as_wire_str
    pub fn to_wire(self) -> Option<&'static str> {
        match self {
            Self::NotConfigured => Some("NotConfigured"),
            Self::Timeout => Some("Timeout"),
            Self::SandboxUnavailable => Some("SandboxUnavailable"),
            Self::OomKilled => Some("OomKilled"),
            Self::Cancelled => Some("Cancelled"),
            Self::Internal => Some("Internal"),
            Self::Unknown => None,
        }
    }
}

/// Tagged-envelope decoder for the five `Runtime*` events. Matches the
/// engine's `#[serde(tag = "type", content = "payload")]` shape so a raw
/// JSON envelope decodes cleanly. The SDK uses this for the JSON-bypass
/// path in [`crate::events::WorkflowEvent::from_envelope_json`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "payload")]
pub enum RuntimeEvent {
    RuntimeStart(RuntimeStartPayload),
    RuntimeStdout(RuntimeStdoutPayload),
    RuntimeStderr(RuntimeStderrPayload),
    RuntimeEnd(RuntimeEndPayload),
    RuntimeError(RuntimeErrorPayload),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn runtime_start_roundtrips() {
        let evt = RuntimeEvent::RuntimeStart(RuntimeStartPayload {
            task_name: "analyse".into(),
            runtime_name: "run_python".into(),
            language: "python".into(),
        });
        let wire = serde_json::to_value(&evt).unwrap();
        assert_eq!(wire["type"], "RuntimeStart");
        assert_eq!(wire["payload"]["task_name"], "analyse");
        assert_eq!(wire["payload"]["runtime_name"], "run_python");
        assert_eq!(wire["payload"]["language"], "python");
        let back: RuntimeEvent = serde_json::from_value(wire).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn runtime_stdout_roundtrips() {
        let wire = json!({
            "type": "RuntimeStdout",
            "payload": {"task_name": "t", "chunk": "hello\n"},
        });
        let evt: RuntimeEvent = serde_json::from_value(wire.clone()).unwrap();
        match &evt {
            RuntimeEvent::RuntimeStdout(p) => {
                assert_eq!(p.task_name, "t");
                assert_eq!(p.chunk, "hello\n");
            }
            other => panic!("expected RuntimeStdout, got {other:?}"),
        }
        assert_eq!(serde_json::to_value(&evt).unwrap(), wire);
    }

    #[test]
    fn runtime_stderr_roundtrips() {
        let evt = RuntimeEvent::RuntimeStderr(RuntimeStderrPayload {
            task_name: "t".into(),
            chunk: "warn: deprecated\n".into(),
        });
        let wire = serde_json::to_value(&evt).unwrap();
        assert_eq!(wire["type"], "RuntimeStderr");
        let back: RuntimeEvent = serde_json::from_value(wire).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn runtime_end_roundtrips() {
        let evt = RuntimeEvent::RuntimeEnd(RuntimeEndPayload {
            task_name: "t".into(),
            exit_code: 0,
            duration_ms: 1234,
        });
        let wire = serde_json::to_value(&evt).unwrap();
        assert_eq!(wire["type"], "RuntimeEnd");
        assert_eq!(wire["payload"]["exit_code"], 0);
        assert_eq!(wire["payload"]["duration_ms"], 1234);
        let back: RuntimeEvent = serde_json::from_value(wire).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn runtime_end_negative_exit_code() {
        // `exit_code` is `i32` so signals (negative codes on Unix when
        // reported as such) round-trip cleanly.
        let wire = json!({
            "type": "RuntimeEnd",
            "payload": {"task_name": "t", "exit_code": -9, "duration_ms": 50},
        });
        let evt: RuntimeEvent = serde_json::from_value(wire).unwrap();
        match evt {
            RuntimeEvent::RuntimeEnd(p) => assert_eq!(p.exit_code, -9),
            _ => panic!("expected RuntimeEnd"),
        }
    }

    #[test]
    fn runtime_error_roundtrips() {
        let evt = RuntimeEvent::RuntimeError(RuntimeErrorPayload {
            task_name: "t".into(),
            kind: "Timeout".into(),
            message: "exceeded 30s budget".into(),
        });
        let wire = serde_json::to_value(&evt).unwrap();
        assert_eq!(wire["type"], "RuntimeError");
        let back: RuntimeEvent = serde_json::from_value(wire).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn runtime_error_kind_maps_known_variants() {
        assert_eq!(
            RuntimeErrorKind::from_wire("NotConfigured"),
            RuntimeErrorKind::NotConfigured
        );
        assert_eq!(
            RuntimeErrorKind::from_wire("Timeout"),
            RuntimeErrorKind::Timeout
        );
        assert_eq!(
            RuntimeErrorKind::from_wire("SandboxUnavailable"),
            RuntimeErrorKind::SandboxUnavailable
        );
        assert_eq!(
            RuntimeErrorKind::from_wire("OomKilled"),
            RuntimeErrorKind::OomKilled
        );
        assert_eq!(
            RuntimeErrorKind::from_wire("Cancelled"),
            RuntimeErrorKind::Cancelled
        );
        assert_eq!(
            RuntimeErrorKind::from_wire("Internal"),
            RuntimeErrorKind::Internal
        );
    }

    #[test]
    fn runtime_error_kind_to_wire_round_trips() {
        // Every concrete kind round-trips through wire → typed → wire so
        // an SDK consumer that decodes-then-re-encodes (e.g. a UI proxy)
        // can rely on byte-stability.
        for s in [
            "NotConfigured",
            "Timeout",
            "SandboxUnavailable",
            "OomKilled",
            "Cancelled",
            "Internal",
        ] {
            let kind = RuntimeErrorKind::from_wire(s);
            assert_eq!(kind.to_wire(), Some(s), "wire round-trip for {s}");
        }
        // Unknown has no canonical wire form; encoding back returns None
        // so the caller can decide what to do (e.g. carry the raw string
        // from the surrounding RuntimeErrorPayload).
        assert_eq!(RuntimeErrorKind::Unknown.to_wire(), None);
    }

    #[test]
    fn runtime_error_kind_unknown_falls_through() {
        assert_eq!(
            RuntimeErrorKind::from_wire("FutureKindFromNewerEngine"),
            RuntimeErrorKind::Unknown
        );
        assert_eq!(RuntimeErrorKind::from_wire(""), RuntimeErrorKind::Unknown);
    }

    #[test]
    fn unknown_runtime_type_fails_decode() {
        // The runtime decoder only knows the 5 canonical types. A future
        // RuntimeFoo event (or a non-runtime envelope) must FAIL to
        // decode here so callers fall back to the EngineEvent path.
        let wire = json!({
            "type": "RuntimeFoo",
            "payload": {"task_name": "t"},
        });
        assert!(serde_json::from_value::<RuntimeEvent>(wire).is_err());
        let other = json!({
            "type": "TaskStart",
            "payload": ["t", null],
        });
        assert!(serde_json::from_value::<RuntimeEvent>(other).is_err());
    }
}
