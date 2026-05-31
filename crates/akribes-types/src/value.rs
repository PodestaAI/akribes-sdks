//! Value type carried by every workflow input/output and engine event.

use crate::error::{ErrorCode, ErrorDetail, ErrorKind, ErrorSource};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};

/// JSON envelope key for an `Unable` payload — `{ "unable": { ... } }`.
///
/// Re-exported from `akribes_core::unable::UNABLE_ENVELOPE_KEY` for
/// backwards compatibility; the canonical definition lives here so the
/// SDK can produce / consume the envelope without depending on core.
pub const UNABLE_ENVELOPE_KEY: &str = "unable";

/// Default `ErrorCode` for `Value::FatalError` payloads that came in
/// without one — older wire formats / hand-built fatals.
fn default_error_code_other() -> ErrorCode {
    ErrorCode::Other
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AgentData {
    pub name: String,
    pub provider: String,
    pub model_name: String,
    pub system_prompt: Option<String>,
    /// Whether extended reasoning / thinking is enabled for this agent.
    /// Resolved by the engine from the agent's `thinking` property and the
    /// backing model's capability. See `models::is_thinking_capable`.
    #[serde(default)]
    pub thinking: bool,
    /// Sampling temperature, when the user pinned one via `temperature:
    /// <float>` on the agent block. `None` means "use the provider
    /// default" — the engine will not set the field on outgoing request
    /// bodies in that case. Per-task overrides are computed at the call
    /// site from `Stmt::TaskDef::temperature` (issue #330).
    #[serde(default)]
    pub temperature: Option<f64>,
}

/// Categorical tag for an `Unable` response. Mirrors the choice variants
/// on the built-in `Unable` record — keep in lock-step with
/// `akribes_core::unable::unable_typedef_stmt`. Serializes to the lower-case
/// snake form on the wire.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum UnableCategory {
    InputMissing,
    InputAmbiguous,
    InputConflicts,
    Capability,
    Other,
    /// Synthetic category produced by the engine (not an agent) when a task
    /// with `allow_partial: true` exhausts its validation retry budget. The
    /// engine folds the exhaustion into a canonical `UnableRecord` so the
    /// existing `on unable <target>` / `on_validation_exhausted` routing
    /// pipes can carry it without a bespoke `SuspendTrigger` variant. See
    /// `Engine::build_partial_retry_unable` and issue #202.
    PartialRetry,
}

impl UnableCategory {
    /// Wire string: `"input_missing"`, `"input_ambiguous"`,
    /// `"input_conflicts"`, `"capability"`, `"other"`, `"partial_retry"`.
    pub fn as_wire_str(&self) -> &'static str {
        match self {
            UnableCategory::InputMissing => "input_missing",
            UnableCategory::InputAmbiguous => "input_ambiguous",
            UnableCategory::InputConflicts => "input_conflicts",
            UnableCategory::Capability => "capability",
            UnableCategory::Other => "other",
            UnableCategory::PartialRetry => "partial_retry",
        }
    }

    /// Parse a wire string back to a category. Returns `None` for any
    /// unrecognized category — callers may choose to fall back to
    /// [`UnableCategory::Other`] or surface a validation error depending
    /// on how strict they need to be.
    pub fn from_wire_str(s: &str) -> Option<Self> {
        match s {
            "input_missing" => Some(UnableCategory::InputMissing),
            "input_ambiguous" => Some(UnableCategory::InputAmbiguous),
            "input_conflicts" => Some(UnableCategory::InputConflicts),
            "capability" => Some(UnableCategory::Capability),
            "other" => Some(UnableCategory::Other),
            "partial_retry" => Some(UnableCategory::PartialRetry),
            _ => None,
        }
    }
}

/// Structured `I can't` response from an agent. The wire envelope is
/// `{ "unable": { "reason": str, "missing": [str], "category": str } }`;
/// this type is the payload after the envelope key is stripped.
/// `missing` defaults to `[]` on both wire and runtime so callers never
/// have to branch on `Option<Vec<_>>`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct UnableRecord {
    pub reason: String,
    #[serde(default)]
    pub missing: Vec<String>,
    pub category: UnableCategory,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Value {
    String(String),
    Int(i64),
    /// Fractional numeric value. Backed by `f64` for now (master plan §D1
    /// — "we do not need more digits right now"). The variant name decouples
    /// the value-layer rename (`Float` → `Decimal`) from a future swap to
    /// `rust_decimal::Decimal`, which is a non-breaking value-layer-only
    /// change. JSON serialisation always emits a JSON number — there is
    /// no envelope.
    Decimal(f64),
    List(Vec<Value>),
    Document(String),
    AgentRef(AgentData),
    Object(HashMap<String, Value>),
    Bool(bool),
    /// Structured `Unable` payload from a task declared `T | Unable`.
    /// On-wire shape is the envelope `{ "unable": { ... } }` — see
    /// `to_json` / `from_json` below for the exact round-trip.
    Unable(UnableRecord),
    /// Discriminated-union payload from a task whose declared return type
    /// is `A | B | ... | Unable` (#226). `variant` is the canonical record
    /// name (e.g. `"Feature"`, `"ClaimErr"`) and `payload` is the parsed
    /// record with the `kind` discriminator stripped. The `Unable` arm is
    /// still represented as [`Value::Unable`] — this variant only carries
    /// non-Unable arms. The wire shape is `{"kind": "<variant>", ...}`.
    Union {
        variant: String,
        payload: Box<Value>,
    },
    /// Failure value carrying full structured detail. Construct via
    /// [`Value::fatal`], [`Value::fatal_kind`], or [`Value::fatal_code`] —
    /// they fill `code`, `user_message`, etc. consistently. Existing
    /// pattern matches like `Value::FatalError { message, kind, .. }`
    /// continue to work; reach for the extra fields when surfacing
    /// errors externally (engine events, OTel, DB).
    FatalError {
        message: String,
        kind: ErrorKind,
        /// Stable [`ErrorCode`] (e.g. `ProviderRateLimit`,
        /// `ScriptDepthExceeded`). Defaults to [`ErrorCode::Other`] for
        /// legacy paths that haven't been migrated, and serializes via
        /// the canonical AKRIBES-E-XXX wire form.
        #[serde(default = "default_error_code_other")]
        code: ErrorCode,
        /// User-facing single-paragraph summary + suggested action.
        /// Defaults to [`ErrorCode::default_user_message`] when not
        /// explicitly overridden.
        #[serde(default)]
        user_message: String,
        /// When the upstream provider supplied a `Retry-After` (or
        /// equivalent), the suggested wait in milliseconds. Skipped on
        /// the wire when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_ms: Option<u64>,
        /// Where in the workflow the error originated (task/agent/
        /// provider/model/tool_ref/script/line). Skipped on the wire
        /// when no fields are set.
        #[serde(default, skip_serializing_if = "ErrorSource::is_empty")]
        source: ErrorSource,
    },
    /// Opaque JSON payload. Emitted by stdlib builtins that accept or
    /// return loosely-typed JSON (e.g. a future `std.json_parse`). The
    /// engine does not introspect it; consumers (Studio, SDKs) render
    /// it as collapsible pretty-printed JSON. Not produced by any M1
    /// builtin — the variant ships now so every `match` site downstream
    /// is exhaustive before M7/M8 land.
    Json(serde_json::Value),
    Null,
}

impl Value {
    /// Project this Value to its canonical wire-format JSON shape.
    ///
    /// Alias for [`Value::to_json`] — kept as a named entry point so
    /// `EngineEvent` serialization sites that carry workflow output
    /// values across the wire can call a method whose name says exactly
    /// what it does. Both methods produce the same JSON; new code that
    /// is specifically about wire-format projection should prefer this
    /// one for readability at the call site.
    ///
    /// Spec: `docs/src/content/docs/reference/engine-events.mdx` — the
    /// wire format never exposes the internal tagged-`Value` form to
    /// consumers. See [`Value::to_json`] for the per-variant projection
    /// rules.
    pub fn to_wire_json(&self) -> serde_json::Value {
        self.to_json()
    }

    /// Convert to plain JSON without Rust enum tags.
    ///
    /// The default serde serialization wraps every variant in its tag:
    /// `Value::String("hi")` → `{"String":"hi"}`. This method produces
    /// clean JSON: `"hi"`, `42`, `[...]`, `{...}`, etc.
    ///
    /// For [`Value::Unable`], the output is the canonical envelope
    /// `{ "unable": { "reason": ..., "missing": [...], "category": ... } }`
    /// — the same shape the schema advertises and that providers return.
    ///
    /// **Object key order is NOT a guarantee of this method.**
    /// [`Value::Object`] is `HashMap`-backed, so iteration order is
    /// indeterminate, and the resulting `serde_json::Map`'s order
    /// further depends on whether `serde_json` was compiled with
    /// `preserve_order`. Callers that need stable key ordering for
    /// hashing / golden output / wire-compat should route through
    /// `akribes_core::stdlib::lookup`(`"json_stringify"`) — issue #866 —
    /// which canonically alpha-sorts every nested object via its
    /// internal `canonicalize` pass.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::String(s) | Value::Document(s) => serde_json::Value::String(s.clone()),
            Value::Int(i) => serde_json::json!(i),
            Value::Decimal(f) => serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            Value::Bool(b) => serde_json::json!(b),
            Value::Null => serde_json::Value::Null,
            Value::List(items) => {
                serde_json::Value::Array(items.iter().map(|v| v.to_json()).collect())
            }
            Value::Object(map) => {
                let obj: serde_json::Map<String, serde_json::Value> =
                    map.iter().map(|(k, v)| (k.clone(), v.to_json())).collect();
                serde_json::Value::Object(obj)
            }
            Value::Unable(rec) => {
                serde_json::json!({
                    UNABLE_ENVELOPE_KEY: {
                        "reason": rec.reason,
                        "missing": rec.missing,
                        "category": rec.category.as_wire_str(),
                    }
                })
            }
            Value::Union { variant, payload } => {
                // Re-emit as `{"kind": "<variant>", ...<payload>}` — the
                // mirror image of the engine's lift step. Non-record
                // payloads (shouldn't happen given analyzer rules) get
                // emitted under a single `payload` key so callers never
                // see a malformed value.
                let mut inner = match payload.to_json() {
                    serde_json::Value::Object(m) => m,
                    other => {
                        let mut m = serde_json::Map::new();
                        m.insert("payload".to_string(), other);
                        m
                    }
                };
                inner.insert(
                    "kind".to_string(),
                    serde_json::Value::String(variant.clone()),
                );
                serde_json::Value::Object(inner)
            }
            Value::FatalError { message, kind, code, user_message, retry_after_ms, source } => {
                // Wire shape: legacy keys (`FatalError`, `error_kind`) for
                // SDK back-compat plus the richer envelope under
                // `error_detail` so consumers can opt into code /
                // user_message / retry_after_ms / source. The standalone
                // `code` key is also kept for back-compat with the v0.16
                // string-code format.
                serde_json::json!({
                    "FatalError": message,
                    "error_kind": kind,
                    "code": code.as_wire(),
                    "error_detail": {
                        "kind": kind,
                        "code": code.as_wire(),
                        "message": message,
                        "user_message": user_message,
                        "retry_after_ms": retry_after_ms,
                        "source": source,
                    },
                })
            }
            Value::AgentRef(data) => serde_json::json!({ "agent": data.name }),
            Value::Json(j) => j.clone(),
        }
    }

    /// Convert from plain JSON into a Value.
    ///
    /// Does *not* auto-detect the `Unable` envelope — callers that want
    /// to discriminate a `T | Unable` result should first consult
    /// `akribes_core::unable::is_unable_envelope` and then construct
    /// [`Value::Unable`] explicitly. This keeps `from_json` a pure
    /// shape-preserving decoder and avoids surprising callers who
    /// legitimately want an `Object` with an `"unable"` key.
    ///
    /// Symmetrically, [`Value::Union`] is not auto-detected from the
    /// `{"kind": "<variant>", ...}` wire shape because user records may
    /// legitimately carry a `kind` field. A `Value::Union { variant,
    /// payload }` produced by [`Value::to_json`] round-trips back as a
    /// `Value::Object` whose `kind` key survives in the data — the type
    /// tag is lost. Callers that have the static return type available
    /// should use [`Value::from_json_with_union_arms`] to reconstruct
    /// the `Value::Union` tag deterministically (#1289).
    pub fn from_json(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::String(s) => Value::String(s.clone()),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else if let Some(f) = n.as_f64() {
                    Value::Decimal(f)
                } else {
                    // JSON number that doesn't fit i64 or f64 (e.g. u64::MAX via
                    // arbitrary_precision). Route through `Value::Json` so the
                    // original numeric shape is preserved for downstream
                    // consumers (issue #1031).
                    Value::Json(serde_json::Value::Number(n.clone()))
                }
            }
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Array(arr) => {
                Value::List(arr.iter().map(Value::from_json).collect())
            }
            serde_json::Value::Object(map) => {
                Value::Object(map.iter().map(|(k, v)| (k.clone(), Value::from_json(v))).collect())
            }
        }
    }

    /// Decode plain JSON into a Value, lifting variant-union payloads
    /// when the wire shape `{"kind": "<variant>", ...}` matches one of
    /// the declared `arm_names`. Symmetric inverse of [`Value::to_json`]
    /// for `Value::Union { variant, payload }`.
    ///
    /// Falls back to [`Value::from_json`] when:
    /// - the value is not an object;
    /// - the object has no `"kind"` string field;
    /// - `kind` does not name any of `arm_names`.
    pub fn from_json_with_union_arms(v: &serde_json::Value, arm_names: &[&str]) -> Self {
        if let serde_json::Value::Object(map) = v {
            if let Some(serde_json::Value::String(kind)) = map.get("kind") {
                if arm_names.iter().any(|n| *n == kind.as_str()) {
                    let mut stripped = map.clone();
                    stripped.remove("kind");
                    let payload =
                        Value::from_json(&serde_json::Value::Object(stripped));
                    return Value::Union {
                        variant: kind.clone(),
                        payload: Box::new(payload),
                    };
                }
            }
        }
        Value::from_json(v)
    }

    /// Build a [`Value::FatalError`] from a fully-formed [`ErrorDetail`].
    /// Prefer [`Value::fatal_kind`] / [`Value::fatal_code`] for the common
    /// quick-construction cases.
    pub fn fatal(detail: ErrorDetail) -> Self {
        Value::FatalError {
            message: detail.message,
            kind: detail.kind,
            code: detail.code,
            user_message: detail.user_message,
            retry_after_ms: detail.retry_after_ms,
            source: detail.source,
        }
    }

    /// Quick-construct a fatal error from a kind + developer message.
    /// Code and user_message are derived via [`ErrorDetail::from_kind`].
    /// Use this at sites that don't yet have a specific [`ErrorCode`];
    /// reach for [`Value::fatal_code`] as soon as you do.
    pub fn fatal_kind(kind: ErrorKind, message: impl Into<String>) -> Self {
        Value::fatal(ErrorDetail::from_kind(kind, message))
    }

    /// Quick-construct a fatal error from a specific [`ErrorCode`].
    /// Kind and user_message are derived from the code.
    pub fn fatal_code(code: ErrorCode, message: impl Into<String>) -> Self {
        Value::fatal(ErrorDetail::new(code, message))
    }

    /// Build an [`ErrorDetail`] from a `FatalError` value, or `None` for
    /// any other variant. Clones — use for cross-boundary handoff (engine
    /// event emission, DB serialization).
    pub fn as_fatal_detail(&self) -> Option<ErrorDetail> {
        if let Value::FatalError { message, kind, code, user_message, retry_after_ms, source } = self {
            Some(ErrorDetail {
                kind: *kind,
                code: *code,
                message: message.clone(),
                user_message: user_message.clone(),
                retry_after_ms: *retry_after_ms,
                source: source.clone(),
            })
        } else {
            None
        }
    }

    /// Back-compat shim for the legacy string-coded `fatal_with_code`
    /// helper (#429). Internally normalises the string to an
    /// [`ErrorCode`] via [`ErrorCode::from_wire`]; unrecognised codes
    /// fall through to [`ErrorCode::Other`].
    #[deprecated(note = "use Value::fatal_code(ErrorCode::X, msg) instead")]
    pub fn fatal_with_code(
        message: impl Into<String>,
        kind: ErrorKind,
        code: impl AsRef<str>,
    ) -> Self {
        let code = ErrorCode::from_wire(code.as_ref()).unwrap_or(ErrorCode::Other);
        let detail = ErrorDetail::new(code, message);
        // Override the kind so legacy callers that paired the wrong
        // kind+code keep their original kind. Prefer `fatal_code` at
        // the new sites.
        Value::FatalError {
            message: detail.message,
            kind,
            code: detail.code,
            user_message: detail.user_message,
            retry_after_ms: detail.retry_after_ms,
            source: detail.source,
        }
    }
}

/// Normalize an `f64` to a canonical bit pattern for hashing purposes
/// (issue #1012). Two correctness hazards in the naive `f.to_bits()`
/// approach:
///
/// 1. `-0.0` and `+0.0` compare equal under `PartialEq` but have
///    distinct bit patterns; using `to_bits()` directly violates the
///    `Hash` contract ("equal values MUST hash equal").
/// 2. Any NaN — quiet, signalling, with different payloads — has many
///    distinct bit patterns; we canonicalise to one quiet-NaN repr so
///    cache lookups behave deterministically across runs.
///
/// `Value::Decimal(NaN)` still hashes (unlike `Hash` on a bare `f64`,
/// which has no impl). Two NaNs are not `PartialEq::eq` to each other,
/// so the contract "equal values must hash equal" is vacuously
/// satisfied for NaN — collapsing every NaN bit-pattern to a single
/// hash slot is permitted ("unequal values MAY hash equal").
pub fn normalized_f64_bits(f: f64) -> u64 {
    if f.is_nan() {
        // Canonical quiet-NaN bit pattern. Payload-stripped,
        // sign-stripped so every NaN flavor hashes identically.
        f64::NAN.to_bits()
    } else if f == 0.0 {
        // Collapse -0.0 and +0.0 to +0.0's bit pattern. `f == 0.0`
        // matches both signs; `0.0_f64.to_bits()` is the all-zero
        // pattern by IEEE-754.
        0u64
    } else {
        f.to_bits()
    }
}

/// Deterministic `Hash` for `Value`, used for task-cache keys.
///
/// Notes:
/// - `Value::Decimal` (f64-backed per §D1) is hashed through
///   [`normalized_f64_bits`] so `-0.0`/`+0.0` collide and every NaN
///   payload collapses to a single canonical key. See issue #1012.
/// - `Object` entries are sorted by key before hashing since `HashMap` iteration
///   order is not stable across runs or insertions.
impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Value::String(s) | Value::Document(s) => s.hash(state),
            Value::Int(i) => i.hash(state),
            Value::Decimal(f) => normalized_f64_bits(*f).hash(state),
            Value::Bool(b) => b.hash(state),
            Value::Null => {}
            Value::List(items) => items.hash(state),
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                for k in keys {
                    k.hash(state);
                    map[k].hash(state);
                }
            }
            Value::AgentRef(a) => a.hash(state),
            Value::Unable(rec) => rec.hash(state),
            Value::Union { variant, payload } => {
                variant.hash(state);
                payload.hash(state);
            }
            Value::FatalError { message, kind, code, .. } => {
                message.hash(state);
                kind.hash(state);
                code.hash(state);
            }
            // Opaque JSON — hash the canonical compact-string repr so
            // semantically-equal payloads (including reordered object
            // keys, which `serde_json::Value` does not normalise) would
            // ideally collide, but serde_json preserves insertion order
            // in its default `Map<String, Value>`. Acceptable for cache
            // keys today; cache-hit rate is a polish concern.
            Value::Json(j) => j.to_string().hash(state),
        }
    }
}

impl Hash for AgentData {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.provider.hash(state);
        self.model_name.hash(state);
        self.system_prompt.hash(state);
        self.thinking.hash(state);
        // f64 has no Hash impl; route through `normalized_f64_bits` so
        // `-0.0` and `+0.0` collide and every NaN payload collapses to
        // one canonical key (issue #1012). Same convention as
        // `Value::Decimal` above.
        self.temperature.map(normalized_f64_bits).hash(state);
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::String(s) | Value::Document(s) => write!(f, "{}", s),
            Value::Int(i) => write!(f, "{}", i),
            Value::Decimal(fl) => write!(f, "{}", fl),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Null => write!(f, "null"),
            Value::List(items) => {
                write!(f, "[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{}", item)?;
                }
                write!(f, "]")
            }
            Value::Object(map) => {
                // Iterate by sorted key — `HashMap` iteration order is
                // not stable across runs or insertion orders, so a bare
                // `map.iter()` produces nondeterministic Display output
                // (issue #1081). Mirrors the Hash impl above, which
                // already sorts keys for the same reason.
                write!(f, "{{")?;
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{}: {}", k, map[*k])?;
                }
                write!(f, "}}")
            }
            Value::Unable(rec) => {
                write!(
                    f,
                    "Unable({}: {})",
                    rec.category.as_wire_str(),
                    rec.reason
                )
            }
            Value::Union { variant, payload } => write!(f, "{}({})", variant, payload),
            Value::FatalError { message, .. } => write!(f, "{}", message),
            Value::AgentRef(data) => write!(f, "<agent:{}>", data.name),
            Value::Json(j) => write!(f, "{}", j),
        }
    }
}
