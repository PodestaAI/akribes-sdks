//! SDK-facing mirror of `akribes_types::event::TaskEndVariant`.
//!
//! `akribes-core` is the source of truth for the `TaskEnd` event wire shape. It
//! defines [`akribes_types::event::TaskEndVariant`] with two concrete variants
//! today (`Success`, `Unable`) — #205 is slated to add `Partial`. We re-mirror
//! the shape at the SDK layer so the SDK stays forward-compatible without a
//! new release when a future akribes-core adds a variant:
//!
//! * The SDK mirror carries an [`Unknown`] catch-all via `#[serde(other)]`.
//!   A future discriminant deserializes to `Unknown` rather than erroring,
//!   so the `RunStream` keeps yielding instead of dying on the new event.
//! * `#[serde(default)]`-friendly [`Default`] returns `Success`, matching
//!   the akribes-core default. This handles old server streams that emit
//!   `TaskEnd` without the `variant` field at all (pre-#206 shape).
//!
//! The conversion from [`akribes_types::event::TaskEndVariant`] is a total
//! mapping today (every core variant has an SDK variant). As core adds new
//! arms, new SDK arms should be added in lock-step, with the `Unknown`
//! catch-all absorbing anything the SDK release hasn't caught up to on
//! streams from a newer server.
//!
//! [`Unknown`]: TaskEndVariant::Unknown

use akribes_types::event as core_event;
use serde::{Deserialize, Serialize};

/// How a task finished. Wire-compatible with [`akribes_types::event::TaskEndVariant`]
/// — a plain `snake_case` string on the wire (`"success"`, `"unable"`, ...).
/// The `#[serde(other)]` arm is the forward-compat contract at the SDK
/// boundary: future akribes-core arms (e.g. `Partial` from #205) surface as
/// [`Unknown`](Self::Unknown) until the SDK is updated, and the stream
/// keeps flowing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TaskEndVariant {
    /// Task produced a well-typed value that passed the full validation
    /// pipeline. Wire default when `variant` is absent (pre-#206).
    #[default]
    Success,
    /// Task had a `T | Unable` return type and the agent emitted a canonical
    /// `{"unable": ...}` envelope. The owning `TaskEnd.value` carries the
    /// full `Value::Unable` payload.
    Unable,
    /// Task ended with a dispatch-level failure (provider error, sandbox
    /// timeout, OOM kill, exhausted validation budget). The owning
    /// `TaskEnd.value` is a `Value::FatalError`. Surfaced from the
    /// runtime dispatch path. (PR #672.)
    Failed,
    /// Catch-all for discriminants the SDK doesn't recognize (a variant
    /// added by a newer akribes-core). The raw `TaskEnd.value` is still
    /// available — consumers that need strict handling should read the raw
    /// [`akribes_types::event::EngineEvent::TaskEnd`] directly.
    #[serde(other)]
    Unknown,
}

impl From<core_event::TaskEndVariant> for TaskEndVariant {
    fn from(v: core_event::TaskEndVariant) -> Self {
        match v {
            core_event::TaskEndVariant::Success => TaskEndVariant::Success,
            core_event::TaskEndVariant::Unable => TaskEndVariant::Unable,
            core_event::TaskEndVariant::Failed => TaskEndVariant::Failed,
            // Mirror the Unknown-passthrough: a caller constructing a core
            // `Unknown` (e.g. from upstream deserialisation) should surface
            // as SDK-Unknown too.
            core_event::TaskEndVariant::Unknown => TaskEndVariant::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn success_roundtrips_byte_identical() {
        let wire = r#""success""#;
        let parsed: TaskEndVariant = serde_json::from_str(wire).unwrap();
        assert_eq!(parsed, TaskEndVariant::Success);
        let reserialized = serde_json::to_string(&parsed).unwrap();
        assert_eq!(reserialized, wire);
    }

    #[test]
    fn unable_roundtrips_byte_identical() {
        let wire = r#""unable""#;
        let parsed: TaskEndVariant = serde_json::from_str(wire).unwrap();
        assert_eq!(parsed, TaskEndVariant::Unable);
        let reserialized = serde_json::to_string(&parsed).unwrap();
        assert_eq!(reserialized, wire);
    }

    #[test]
    fn unknown_discriminant_deserializes_to_unknown() {
        // Forward-compat: a newer akribes-core adds `"partial"` and the SDK
        // must not crash — it forwards as `Unknown`.
        let wire = json!("partial");
        let parsed: TaskEndVariant = serde_json::from_value(wire).unwrap();
        assert_eq!(parsed, TaskEndVariant::Unknown);
    }

    #[test]
    fn default_is_success() {
        assert_eq!(TaskEndVariant::default(), TaskEndVariant::Success);
    }

    #[test]
    fn converts_from_core_success() {
        let core = core_event::TaskEndVariant::Success;
        let sdk: TaskEndVariant = core.into();
        assert_eq!(sdk, TaskEndVariant::Success);
    }

    #[test]
    fn converts_from_core_unable() {
        let core = core_event::TaskEndVariant::Unable;
        let sdk: TaskEndVariant = core.into();
        assert_eq!(sdk, TaskEndVariant::Unable);
    }

    #[test]
    fn converts_from_core_unknown() {
        let core = core_event::TaskEndVariant::Unknown;
        let sdk: TaskEndVariant = core.into();
        assert_eq!(sdk, TaskEndVariant::Unknown);
    }

    #[test]
    fn variants_are_exhaustive_known_set() {
        // A safety net: every snake_case tag we advertise as "known" must
        // parse to a non-Unknown arm. If a new variant is added to
        // akribes-core and the SDK is updated (adding a new arm), extend this
        // list and the `From<core>` match above in the same commit — this
        // test is the mechanical reminder.
        for known in ["success", "unable"] {
            let wire = json!(known);
            let parsed: TaskEndVariant = serde_json::from_value(wire).unwrap();
            assert_ne!(
                parsed,
                TaskEndVariant::Unknown,
                "known tag {known} surfaced as Unknown"
            );
        }
    }
}
