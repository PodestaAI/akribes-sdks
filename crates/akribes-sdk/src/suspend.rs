//! SDK-facing mirror of `akribes_types::event::SuspendTrigger` and friends.
//!
//! `akribes-core` is the source of truth for the `Suspended` event wire shape.
//! It already defines [`akribes_types::event::SuspendTrigger`] with the three
//! variants the server emits (`DagPosition`, `ValidationExhausted`,
//! `AgentUnable`). We re-mirror the shape at the SDK layer for two reasons:
//!
//! 1. **Forward-compat.** The SDK mirror carries an [`Unknown`] catch-all
//!    via `#[serde(other)]` so a newer server emitting a future variant
//!    never crashes the SDK — the suspension surfaces as `Unknown` and the
//!    raw payload is still available on the wire for consumers to inspect
//!    via [`crate::WorkflowEvent::Other`] / raw [`akribes_types::event::EngineEvent`]
//!    access. The core enum is deliberately not marked `#[serde(other)]`
//!    because akribes-core tests exhaustiveness of its own variants; the
//!    forward-compat contract lives at the SDK boundary per the Wave-4
//!    tracker decisions.
//! 2. **Stable public surface.** SDK consumers don't have to reach into
//!    `akribes_types::*` for common wire types — [`SuspendTrigger`],
//!    [`UnableRecord`], and [`ValidationErrorWire`] are re-exported at the
//!    SDK crate root.
//!
//! Conversions from the core shape are provided so the SDK's
//! [`crate::WorkflowEvent::Checkpoint`] can carry a typed trigger without
//! leaking `akribes_types::event::*` imports to consumers.
//!
//! [`Unknown`]: SuspendTrigger::Unknown

use akribes_types::event as core_event;
use serde::{Deserialize, Serialize};

/// Wire-format twin of [`akribes_types::validation::ValidationError`].
///
/// Owned + serializable; the `stage` discriminator is a string (`"parse"`,
/// `"schema"`, `"custom:<rule>"`) so SDK consumers don't need to round-trip
/// through the internal enum.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationErrorWire {
    pub stage: String,
    pub message: String,
    pub path: Option<String>,
}

impl From<core_event::ValidationErrorWire> for ValidationErrorWire {
    fn from(v: core_event::ValidationErrorWire) -> Self {
        Self {
            stage: v.stage,
            message: v.message,
            path: v.path,
        }
    }
}

/// Structured "I can't" payload from an agent with a `T | Unable` return
/// type. Canonical wire envelope is `{ "unable": { reason, missing, category } }`;
/// this record is the payload after the envelope key is stripped.
///
/// `category` is kept as a free-form `String` at the SDK layer so forward
/// compat on category names doesn't require an SDK release. The core enum
/// [`akribes_types::value::UnableCategory`] lists the current five canonical
/// buckets (`input_missing`, `input_ambiguous`, `input_conflicts`,
/// `capability`, `other`); consumers can parse into that enum if they want
/// strict typing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnableRecord {
    pub reason: String,
    #[serde(default)]
    pub missing: Vec<String>,
    pub category: String,
}

/// Why the engine suspended execution at a checkpoint.
///
/// Serde-tagged with an internal `"kind"` discriminator matching
/// [`akribes_types::event::SuspendTrigger`]. Unknown discriminants deserialize
/// to [`SuspendTrigger::Unknown`] (via `#[serde(other)]`) so the SDK is
/// forward-compatible with future akribes-core / server additions without a
/// new SDK release.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum SuspendTrigger {
    /// The DAG reached an explicit `checkpoint cp(...)` call site.
    DagPosition,
    /// `on_validation_exhausted:` fired — retries consumed without
    /// producing a payload that passes parse → schema → custom validation.
    ValidationExhausted {
        task_name: String,
        retry_count: u32,
        last_attempt: String,
        validation_errors: Vec<ValidationErrorWire>,
    },
    /// A task with a `T | Unable` return type produced an `Unable` value
    /// and the flow routed it to a checkpoint via `on unable <cp>`.
    AgentUnable {
        task_name: String,
        unable: UnableRecord,
    },
    /// A task with a discriminated-union return type
    /// (`A | B | ... | Unable`) produced a non-Unable variant and the flow
    /// routed it to a checkpoint via `on <Variant> <cp>`. `variant` is
    /// the record name as declared in source (PascalCase); `payload` is
    /// the parsed record (with `kind` stripped).
    AgentVariant {
        task_name: String,
        variant: String,
        payload: serde_json::Value,
    },
    /// Catch-all for discriminants the SDK doesn't recognize (e.g. a
    /// variant added by a newer akribes-core). The raw `Suspended.trigger`
    /// payload is not preserved here — consumers that need full-fidelity
    /// unknown handling can read it off the raw
    /// [`akribes_types::event::EngineEvent::Suspended`] instead of the
    /// normalized [`crate::WorkflowEvent`].
    #[serde(other)]
    Unknown,
}

impl Default for SuspendTrigger {
    fn default() -> Self {
        // Mirrors akribes-core's default. Old wire payloads (pre-Stream 6) that
        // omit `trigger` entirely come in as `DagPosition`.
        SuspendTrigger::DagPosition
    }
}

impl From<core_event::SuspendTrigger> for SuspendTrigger {
    fn from(t: core_event::SuspendTrigger) -> Self {
        match t {
            core_event::SuspendTrigger::DagPosition => SuspendTrigger::DagPosition,
            core_event::SuspendTrigger::ValidationExhausted {
                task_name,
                retry_count,
                last_attempt,
                validation_errors,
            } => SuspendTrigger::ValidationExhausted {
                task_name,
                retry_count,
                last_attempt,
                validation_errors: validation_errors.into_iter().map(Into::into).collect(),
            },
            core_event::SuspendTrigger::AgentUnable { task_name, unable } => {
                SuspendTrigger::AgentUnable {
                    task_name,
                    unable: UnableRecord {
                        reason: unable.reason,
                        missing: unable.missing,
                        category: unable.category.as_wire_str().to_string(),
                    },
                }
            }
            core_event::SuspendTrigger::AgentVariant {
                task_name,
                variant,
                payload,
            } => SuspendTrigger::AgentVariant {
                task_name,
                variant,
                payload,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── DagPosition round-trip ───────────────────────────────────────────────

    #[test]
    fn dag_position_roundtrips_byte_identical() {
        let wire = r#"{"kind":"DagPosition"}"#;
        let parsed: SuspendTrigger = serde_json::from_str(wire).unwrap();
        assert!(matches!(parsed, SuspendTrigger::DagPosition));
        let reserialized = serde_json::to_string(&parsed).unwrap();
        assert_eq!(reserialized, wire);
    }

    // ── ValidationExhausted round-trip ───────────────────────────────────────

    #[test]
    fn validation_exhausted_roundtrips_byte_identical() {
        // Field order mirrors the SDK struct declaration order — serde_json
        // serializes in declaration order for derived Serialize, so a wire
        // sample built in the same order should be byte-identical.
        let wire = r#"{"kind":"ValidationExhausted","task_name":"decompose_claims","retry_count":3,"last_attempt":"{\"bad\":true}","validation_errors":[{"stage":"schema","message":"required property \"number\" missing","path":"/0"}]}"#;
        let parsed: SuspendTrigger = serde_json::from_str(wire).unwrap();
        match &parsed {
            SuspendTrigger::ValidationExhausted {
                task_name,
                retry_count,
                last_attempt,
                validation_errors,
            } => {
                assert_eq!(task_name, "decompose_claims");
                assert_eq!(*retry_count, 3);
                assert_eq!(last_attempt, r#"{"bad":true}"#);
                assert_eq!(validation_errors.len(), 1);
                assert_eq!(validation_errors[0].stage, "schema");
                assert_eq!(validation_errors[0].path.as_deref(), Some("/0"));
            }
            other => panic!("expected ValidationExhausted, got {other:?}"),
        }
        let reserialized = serde_json::to_string(&parsed).unwrap();
        assert_eq!(reserialized, wire);
    }

    // ── AgentUnable round-trip ───────────────────────────────────────────────

    #[test]
    fn agent_unable_roundtrips_byte_identical() {
        let wire = r#"{"kind":"AgentUnable","task_name":"escalate","unable":{"reason":"image too blurry to OCR","missing":["claim_text"],"category":"input_ambiguous"}}"#;
        let parsed: SuspendTrigger = serde_json::from_str(wire).unwrap();
        match &parsed {
            SuspendTrigger::AgentUnable { task_name, unable } => {
                assert_eq!(task_name, "escalate");
                assert_eq!(unable.reason, "image too blurry to OCR");
                assert_eq!(unable.missing, vec!["claim_text".to_string()]);
                assert_eq!(unable.category, "input_ambiguous");
            }
            other => panic!("expected AgentUnable, got {other:?}"),
        }
        let reserialized = serde_json::to_string(&parsed).unwrap();
        assert_eq!(reserialized, wire);
    }

    #[test]
    fn agent_unable_accepts_missing_field_default() {
        // `missing` defaults to `[]` so older/minimal payloads still parse.
        let wire = json!({
            "kind": "AgentUnable",
            "task_name": "t",
            "unable": { "reason": "x", "category": "other" },
        });
        let parsed: SuspendTrigger = serde_json::from_value(wire).unwrap();
        match parsed {
            SuspendTrigger::AgentUnable { unable, .. } => {
                assert!(unable.missing.is_empty());
            }
            other => panic!("expected AgentUnable, got {other:?}"),
        }
    }

    // ── Unknown variant passthrough ──────────────────────────────────────────

    #[test]
    fn unknown_kind_deserializes_to_unknown_variant() {
        // A future akribes-core release might add a new discriminant. The SDK
        // must not crash — it forwards as `Unknown`.
        let wire = json!({
            "kind": "SomeFutureVariant",
            "extra_field": 42,
        });
        let parsed: SuspendTrigger = serde_json::from_value(wire).unwrap();
        assert!(matches!(parsed, SuspendTrigger::Unknown));
    }

    #[test]
    fn unknown_kind_with_no_extra_fields_still_parses() {
        let parsed: SuspendTrigger = serde_json::from_str(r#"{"kind":"Nope"}"#).unwrap();
        assert!(matches!(parsed, SuspendTrigger::Unknown));
    }

    // ── Interop with akribes-core ───────────────────────────────────────────────

    #[test]
    fn converts_from_core_dag_position() {
        let core = core_event::SuspendTrigger::DagPosition;
        let sdk: SuspendTrigger = core.into();
        assert!(matches!(sdk, SuspendTrigger::DagPosition));
    }

    #[test]
    fn converts_from_core_validation_exhausted() {
        let core = core_event::SuspendTrigger::ValidationExhausted {
            task_name: "t".into(),
            retry_count: 2,
            last_attempt: "{}".into(),
            validation_errors: vec![core_event::ValidationErrorWire {
                stage: "parse".into(),
                message: "bad json".into(),
                path: None,
            }],
        };
        let sdk: SuspendTrigger = core.into();
        match sdk {
            SuspendTrigger::ValidationExhausted {
                task_name,
                retry_count,
                validation_errors,
                ..
            } => {
                assert_eq!(task_name, "t");
                assert_eq!(retry_count, 2);
                assert_eq!(validation_errors[0].stage, "parse");
            }
            other => panic!("expected ValidationExhausted, got {other:?}"),
        }
    }

    #[test]
    fn converts_from_core_agent_unable() {
        let core = core_event::SuspendTrigger::AgentUnable {
            task_name: "escalate".into(),
            unable: akribes_types::value::UnableRecord {
                reason: "blurry".into(),
                missing: vec!["claim_text".into()],
                category: akribes_types::value::UnableCategory::InputAmbiguous,
            },
        };
        let sdk: SuspendTrigger = core.into();
        match sdk {
            SuspendTrigger::AgentUnable { task_name, unable } => {
                assert_eq!(task_name, "escalate");
                assert_eq!(unable.reason, "blurry");
                assert_eq!(unable.category, "input_ambiguous");
                assert_eq!(unable.missing, vec!["claim_text".to_string()]);
            }
            other => panic!("expected AgentUnable, got {other:?}"),
        }
    }

    #[test]
    fn default_is_dag_position() {
        assert!(matches!(
            SuspendTrigger::default(),
            SuspendTrigger::DagPosition
        ));
    }
}
