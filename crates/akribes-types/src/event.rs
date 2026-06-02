use crate::ast::{ActorHint, Span, TypeRef};
use crate::error::{ErrorCode, ErrorKind, ErrorSource};
use crate::value::Value;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;

/// Node-id type used by `EngineEvent::NodeStart` / `NodeEnd` /
/// `Breakpoint*`. Mirror of `akribes_core::compiler::NodeId` — same
/// underlying `usize` representation; the alias lives here so the
/// SDK-facing `EngineEvent` doesn't need to depend on the compiler
/// module.
pub type NodeId = usize;

/// Caches the LLM-emitted tool-use blocks so a replay rebuilds the same
/// `tool_use_id`s and the downstream `ToolCallEnd` lookups stay stable.
/// Mirror of `akribes_core::replay_cache::CachedToolCall` — same wire
/// shape; the type lives here so it can be embedded in
/// `EngineEvent::LLMResponse` without an akribes-core dependency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedToolCall {
    pub tool_use_id: String,
    pub name: String,
    pub args: serde_json::Value,
}

/// Outcome of a child execution observed by its parent at the
/// `call(...)` boundary. Mirror of
/// `akribes_core::replay_cache::ChildOutcome` — same wire shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail")]
pub enum ChildOutcome {
    Ok {
        value: serde_json::Value,
    },
    Err {
        kind: String,
        message: String,
        code: Option<String>,
    },
}

/// Default for `EngineEvent::Error::code` on payloads from older SDK
/// versions that didn't include the field.
fn default_error_code_other() -> ErrorCode {
    ErrorCode::Other
}

/// Token usage from a single LLM call.
///
/// # Normalized superset convention
///
/// `input_tokens` is the **total** number of input tokens processed — it
/// is a superset of `cached_input_tokens` and `cache_write_input_tokens`.
/// Downstream cost logic derives the "fresh" (non-cached) portion by
/// subtracting the two cache counts. This matches OpenAI and Gemini's
/// native reporting; Anthropic's API reports the three groups as disjoint
/// so the Anthropic parser normalizes by summing before assigning.
///
/// # Prompt-caching semantics per provider
/// - **OpenAI** — `cached_input_tokens` counts cache READS (billed at a
///   discount, typically 0.1x base input). Cache writes are free per
///   OpenAI's caching docs. `cache_write_input_tokens` is always 0.
/// - **Anthropic** — the API returns three token groups:
///   `input_tokens` (fresh, 1x), `cache_read_input_tokens` (0.1x), and
///   `cache_creation_input_tokens` (1.25x at the default 5-minute TTL,
///   2.0x at 1-hour TTL). The parser remaps these to the superset
///   convention above. The per-TTL split is surfaced as
///   `cache_write_5m_input_tokens` and `cache_write_1h_input_tokens`
///   (parsed from `usage.cache_creation.ephemeral_5m_input_tokens` /
///   `ephemeral_1h_input_tokens`); these two fields sum to
///   `cache_write_input_tokens`. Akribes workflows opt into the 1h TTL
///   via the `extended-cache-ttl-2025-04-11` beta header, so this split
///   matters for cost accounting (#1091).
/// - **Gemini** — only cache reads are reported; writes are not
///   separately billed. `cache_write_input_tokens` is always 0.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    /// Total input tokens processed (superset of the two cache counts).
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model: String,
    pub provider: String,
    /// Cache-READ tokens (billed at `CACHE_READ_RATE`, ~0.1x input).
    pub cached_input_tokens: u64,
    /// Cache-WRITE / creation tokens (Anthropic only today; billed at
    /// `CACHE_WRITE_RATE`, 1.25x input at 5m TTL or 2.0x at 1h TTL).
    /// This is the **total** across both TTL buckets; the breakdown
    /// lives on [`Self::cache_write_5m_input_tokens`] and
    /// [`Self::cache_write_1h_input_tokens`] (#1091). Serialized
    /// default for backward-compatibility with events predating this
    /// field.
    #[serde(default)]
    pub cache_write_input_tokens: u64,
    /// Anthropic cache-WRITE tokens at the default 5-minute TTL,
    /// parsed from `usage.cache_creation.ephemeral_5m_input_tokens`.
    /// Subset of [`Self::cache_write_input_tokens`] — sums with
    /// [`Self::cache_write_1h_input_tokens`] to the total. `0` on
    /// providers that don't report the per-TTL breakdown (OpenAI,
    /// Gemini, mock) and for pre-#1091 events that omit the field.
    #[serde(default)]
    pub cache_write_5m_input_tokens: u64,
    /// Anthropic cache-WRITE tokens at the 1-hour TTL, parsed from
    /// `usage.cache_creation.ephemeral_1h_input_tokens`. Subset of
    /// [`Self::cache_write_input_tokens`] — sums with
    /// [`Self::cache_write_5m_input_tokens`] to the total. `0` on
    /// providers without per-TTL reporting (OpenAI, Gemini, mock) and
    /// for pre-#1091 events that omit the field. The 1h-TTL bucket
    /// bills at 2.0x base input vs. 1.25x for 5m — `pricing::compute_cost`
    /// uses this split for accurate cost attribution (#1091).
    #[serde(default)]
    pub cache_write_1h_input_tokens: u64,
    /// The provider-reported stop reason for the underlying call, when
    /// known. Anthropic surfaces values like `"end_turn"`, `"max_tokens"`,
    /// `"tool_use"`, `"stop_sequence"`. OpenAI: `"stop"`, `"length"`,
    /// `"tool_calls"`. Gemini: `"STOP"`, `"MAX_TOKENS"`, etc.
    ///
    /// Carried alongside usage so the engine's validation-failure path can
    /// distinguish "model truncated mid-output" (`max_tokens` / `length` /
    /// `MAX_TOKENS`) from "model finished cleanly but produced an
    /// invalid shape" — see issue #320 / #321. `None` for providers that
    /// don't surface a stop reason or for paths that haven't been threaded
    /// (e.g. the mock provider). Serialized with `#[serde(default)]` so old
    /// wire payloads that omit the field still deserialize.
    ///
    /// Today this field carries the RAW provider value when the
    /// `parse_*_usage` path produced the `TokenUsage` (the common case
    /// for non-streamed calls). The `usage_from_outcome` rebuild path
    /// (streaming + some retry paths) writes the OTel-canonical form
    /// (`"stop"` / `"max_tokens"` / `"tool_use"` / `"content_filter"` /
    /// `"other"`) because `LlmCallOutcome` only carries the canonical
    /// form. Consumers that need a deterministic-by-provider raw value
    /// should prefer [`Self::raw_stop_reason`] (#1077).
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// Raw provider stop reason, never lossy-mapped to OTel canonical
    /// form. Set to the same value as [`Self::stop_reason`] when the
    /// `parse_*_usage` path produced the usage; `None` otherwise
    /// (mock, streaming rebuilds via `usage_from_outcome`).
    ///
    /// Bench / observability code that needs to distinguish Gemini's
    /// `"STOP"` from `"RECITATION"` (both collapse to `"stop"` under
    /// the canonical mapping) or Anthropic's `"stop_sequence"` from
    /// `"end_turn"` should read this field. #1077.
    #[serde(default)]
    pub raw_stop_reason: Option<String>,
    /// Reasoning / thinking tokens — a SUBSET of [`Self::output_tokens`],
    /// not in addition. Captured from:
    /// * OpenAI o-series + GPT-5: `usage.completion_tokens_details.reasoning_tokens`
    /// * Anthropic extended-thinking: `usage.thinking_tokens` (when present)
    /// * Gemini with `thinkingBudget` set: `usageMetadata.thoughtsTokenCount`
    ///
    /// `0` when the model didn't engage reasoning or the provider didn't
    /// surface the breakdown. `#[serde(default)]` keeps wire-compat with
    /// pre-#322 events that omit the field entirely.
    #[serde(default)]
    pub reasoning_tokens: u64,
}

/// One ancestor frame on a flattened [`EngineEvent::SubScript`] payload.
///
/// Issue #993: `EngineEvent::SubScript` used to wrap a nested
/// `Box<EngineEvent>` per call-stack level, so a depth-N chain produced an
/// event whose serialized payload was `O(N)` deep — a 10-level art_123_2
/// fanout could blow a single SSE frame past a megabyte and choke
/// reconnect logic. The new wire shape carries the leaf event flat and
/// names ancestors via an ordered `parent_path` of these frames so SDKs
/// rebuild the tree off the side without paying the recursion cost on
/// every envelope.
///
/// Frame ordering: `parent_path[0]` is the outermost ancestor (direct
/// child of the top-level workflow); `parent_path[len-1]` is the
/// immediate parent of the currently-emitting sub-script (whose own
/// `script_name` lives on the [`EngineEvent::SubScript`] fields, NOT
/// inside `parent_path`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubScriptFrame {
    /// Called script's name as it lives in the parent's project. Matches
    /// the `script_name` an outer SubScript envelope would have carried
    /// in the legacy recursive shape.
    pub script_name: String,
    /// Variable name on the parent side that received the call result —
    /// same semantics as the top-level [`EngineEvent::SubScript::parent_task`].
    pub parent_task: String,
    /// Compiler-stable id of the parent's call(...) node, when known.
    /// Lets consumers correlate retries of the same call site (issue
    /// #845) across the ancestor chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_node_id: Option<u64>,
    /// 1-indexed attempt counter for author-raise retries at the same
    /// call site. Matches [`EngineEvent::SubScript::attempt`] semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u8>,
}

/// Aggregate token + cost rollup emitted on [`EngineEvent::WorkflowEnd`].
///
/// Issue #1173: today's `WorkflowEnd` carries just the workflow's return
/// value, so dashboards and the CLI re-walk every `TaskEnd` to compute
/// per-execution totals. This struct surfaces the same numbers once on
/// the terminating event so consumers can size the run without parsing
/// the full event log.
///
/// The engine populates this by summing [`TokenUsage`] fields across
/// every `TaskEnd` emitted in the workflow scope. Sub-script TaskEnds
/// (events wrapped inside [`EngineEvent::SubScript`]) DO contribute —
/// the engine relay forwards them to the parent counter so a chain's
/// outer `WorkflowEnd` reflects the entire chain's spend.
///
/// `total_cost_usd` is the only field the engine can't always populate
/// on its own — pricing tables live in `akribes-server` to keep
/// `akribes-core` free of provider rate metadata. The field defaults to
/// `0.0` and stays `0.0` when the engine has no cost data; downstream
/// enrichers may overwrite it on the server side before persistence.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct WorkflowTotals {
    /// Sum of [`TokenUsage::input_tokens`] across every `TaskEnd` in the
    /// workflow scope (including sub-script `TaskEnd`s). Superset of
    /// cached + cache-write — see [`TokenUsage`] for the convention.
    #[serde(default)]
    pub total_input_tokens: u64,
    /// Sum of [`TokenUsage::output_tokens`].
    #[serde(default)]
    pub total_output_tokens: u64,
    /// Sum of [`TokenUsage::cached_input_tokens`] (cache READS).
    #[serde(default)]
    pub total_cached_input_tokens: u64,
    /// Sum of [`TokenUsage::reasoning_tokens`] (extended thinking /
    /// reasoning tokens — a SUBSET of `total_output_tokens`).
    #[serde(default)]
    pub total_thinking_tokens: u64,
    /// Tokens spent on tool-call traffic. Reserved for future per-tool
    /// breakdown — the engine doesn't track this separately today, so
    /// it stays `0`. Present on the wire so SDKs can render the slot
    /// without a schema bump when the engine starts populating it.
    #[serde(default)]
    pub total_tool_tokens: u64,
    /// Sum of per-task USD cost. Always `0.0` when emitted by the
    /// engine — pricing lives in `akribes-server`. Server-side
    /// enrichment may overwrite before persistence.
    #[serde(default)]
    pub total_cost_usd: f64,
    /// Number of `TaskEnd` events folded into the totals above.
    #[serde(default)]
    pub task_count: u32,
}

impl WorkflowTotals {
    /// Fold a single [`TokenUsage`] into the running totals. No-op for
    /// `None` so the engine can call it on every `TaskEnd` without
    /// branching on the optional `usage` field.
    pub fn accumulate(&mut self, usage: Option<&TokenUsage>) {
        self.task_count = self.task_count.saturating_add(1);
        if let Some(u) = usage {
            self.total_input_tokens = self.total_input_tokens.saturating_add(u.input_tokens);
            self.total_output_tokens = self.total_output_tokens.saturating_add(u.output_tokens);
            self.total_cached_input_tokens = self
                .total_cached_input_tokens
                .saturating_add(u.cached_input_tokens);
            self.total_thinking_tokens = self
                .total_thinking_tokens
                .saturating_add(u.reasoning_tokens);
        }
    }

    /// Merge another `WorkflowTotals` (e.g. from a sub-script) into self.
    /// Used by the engine when the relay surfaces a child workflow's
    /// rollup — the outer chain's totals subsume every sub-script's.
    pub fn merge(&mut self, other: &WorkflowTotals) {
        self.total_input_tokens = self
            .total_input_tokens
            .saturating_add(other.total_input_tokens);
        self.total_output_tokens = self
            .total_output_tokens
            .saturating_add(other.total_output_tokens);
        self.total_cached_input_tokens = self
            .total_cached_input_tokens
            .saturating_add(other.total_cached_input_tokens);
        self.total_thinking_tokens = self
            .total_thinking_tokens
            .saturating_add(other.total_thinking_tokens);
        self.total_tool_tokens = self
            .total_tool_tokens
            .saturating_add(other.total_tool_tokens);
        self.total_cost_usd += other.total_cost_usd;
        self.task_count = self.task_count.saturating_add(other.task_count);
    }
}

/// Payload of [`EngineEvent::WorkflowEnd`]. Pairs the workflow's
/// terminal output value with an aggregate [`WorkflowTotals`] rollup.
///
/// Serialized with a hand-written `Serialize` / `Deserialize` so the
/// wire stays bridge-compatible between the legacy (pre-#1173) shape
/// (`payload = <bare-value>`) and the new shape
/// (`payload = {"value": <bare-value>, "total_input_tokens": N, ...}`).
/// See the [`EngineEvent::WorkflowEnd`] doc for the disambiguation
/// rule.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowEndPayload {
    /// The workflow's final return value (the historical payload).
    pub value: Value,
    /// Aggregate rollup across every `TaskEnd` in the workflow scope.
    pub totals: WorkflowTotals,
}

impl Default for WorkflowEndPayload {
    fn default() -> Self {
        Self {
            value: Value::Null,
            totals: WorkflowTotals::default(),
        }
    }
}

impl WorkflowEndPayload {
    /// Construct a payload from just a value (totals default to zero).
    /// Used by call sites that don't have totals yet — they get the
    /// historical behaviour automatically.
    pub fn new(value: Value) -> Self {
        Self {
            value,
            totals: WorkflowTotals::default(),
        }
    }

    /// Construct a payload with both value and totals.
    pub fn with_totals(value: Value, totals: WorkflowTotals) -> Self {
        Self { value, totals }
    }
}

// Allow call sites that have historically passed a bare `Value` to keep
// doing so via `EngineEvent::WorkflowEnd(value.into())`. Tests and the
// CLI fixture use this; the engine emit path uses the explicit
// `WorkflowEndPayload::with_totals` constructor.
impl From<Value> for WorkflowEndPayload {
    fn from(value: Value) -> Self {
        Self::new(value)
    }
}

impl Serialize for WorkflowEndPayload {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(8))?;
        // Emit the workflow output under "value" using the clean wire
        // form (Value::to_wire_json) so downstream consumers see the
        // same shape they always did, just one level nested.
        map.serialize_entry("value", &self.value.to_wire_json())?;
        // Aggregate totals — flat siblings of `value`. Matches issue
        // #1173's wire shape exactly.
        map.serialize_entry("total_input_tokens", &self.totals.total_input_tokens)?;
        map.serialize_entry("total_output_tokens", &self.totals.total_output_tokens)?;
        map.serialize_entry(
            "total_cached_input_tokens",
            &self.totals.total_cached_input_tokens,
        )?;
        map.serialize_entry("total_thinking_tokens", &self.totals.total_thinking_tokens)?;
        map.serialize_entry("total_tool_tokens", &self.totals.total_tool_tokens)?;
        map.serialize_entry("total_cost_usd", &self.totals.total_cost_usd)?;
        map.serialize_entry("task_count", &self.totals.task_count)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for WorkflowEndPayload {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Read into a raw JSON value first so we can dispatch on shape.
        let raw = serde_json::Value::deserialize(d)?;
        // New shape disambiguator: an object that carries BOTH a
        // `value` key AND at least one `total_*` aggregate key (or
        // `task_count`). Anything else is the legacy bare-value form.
        const AGG_KEYS: &[&str] = &[
            "total_input_tokens",
            "total_output_tokens",
            "total_cached_input_tokens",
            "total_thinking_tokens",
            "total_tool_tokens",
            "total_cost_usd",
            "task_count",
        ];
        if let serde_json::Value::Object(map) = &raw {
            let has_value = map.contains_key("value");
            let has_any_agg = AGG_KEYS.iter().any(|k| map.contains_key(*k));
            if has_value && has_any_agg {
                let value = Value::from_json(map.get("value").unwrap());
                let totals = WorkflowTotals {
                    total_input_tokens: map
                        .get("total_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    total_output_tokens: map
                        .get("total_output_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    total_cached_input_tokens: map
                        .get("total_cached_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    total_thinking_tokens: map
                        .get("total_thinking_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    total_tool_tokens: map
                        .get("total_tool_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    total_cost_usd: map
                        .get("total_cost_usd")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0),
                    task_count: map
                        .get("task_count")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as u32)
                        .unwrap_or(0),
                };
                return Ok(WorkflowEndPayload { value, totals });
            }
        }
        // Legacy bare-value form: the payload IS the workflow output.
        Ok(WorkflowEndPayload {
            value: Value::from_json(&raw),
            totals: WorkflowTotals::default(),
        })
    }
}

/// Wire-format twin of [`crate::validation::ValidationError`]. Owned +
/// serializable; the `stage` discriminator is a string (`"parse"`,
/// `"schema"`, `"custom:<rule>"`) so SDK consumers don't need to round-trip
/// through the internal enum.
///
/// Produced by [`crate::validation::ValidationError::to_wire`]. The internal
/// `SchemaCompile` stage is intentionally not representable here — those
/// errors short-circuit to [`crate::value::Value::FatalError`] before any
/// [`EngineEvent::Suspended`] would be emitted (they're engine bugs, not
/// model bugs; author can't fix them by reviewing a payload). See the
/// authoritative decision in
/// `docs/superpowers/plans/2026-04-18-epa-feature-tracker.md`
/// ("Wave-1 M1 ship notes + cross-cutting decisions (2026-04-18, round 3)").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationErrorWire {
    pub stage: String,
    pub message: String,
    pub path: Option<String>,
}

/// Why the engine suspended execution at a checkpoint. This is the canonical
/// wire-shape for the `trigger` discriminator on [`EngineEvent::Suspended`];
/// Stream 4 (#149 + #156 approach C) populates the `AgentUnable` variant,
/// and Stream 6 (this stream, #156 approach B) populates
/// `ValidationExhausted`.
///
/// Serde-tagged with an internal `"kind"` discriminator so deserializers can
/// match on a single field without peeking at shape. Each variant carries
/// its payload inline — there is no sidecar `unable_payload` / `exhaustion`
/// field on `Suspended` itself.
///
/// Spec: `docs/superpowers/specs/2026-04-18-epa-checkpoint-validation-design.md`.
/// The wire shape is locked per the Wave-1 round-3 tracker decision
/// ("Suspended wire shape (S4 ↔ S6) = S6's embedded-payload shape").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
#[derive(Default)]
pub enum SuspendTrigger {
    /// The DAG reached an explicit `checkpoint cp(...)` call site. This is
    /// today's only behaviour; the variant carries no payload because the
    /// checkpoint's own `expects:` schema fully describes what comes back
    /// on resume.
    #[default]
    DagPosition,
    /// The task's `on_validation_exhausted:` property fired: all
    /// validation retries consumed without producing a payload that passes
    /// the parse→schema→custom pipeline. Studio / SDKs render the last
    /// failing attempt + its errors so the human can correct in place.
    ValidationExhausted {
        task_name: String,
        retry_count: u32,
        last_attempt: String,
        validation_errors: Vec<ValidationErrorWire>,
    },
    /// Reserved for Stream 4 (#149 + #156 approach C) — emitted when a
    /// task with a `T | Unable` return type produces an `Unable` value and
    /// the flow routes it to a checkpoint via `on unable <cp>`. Shape is
    /// invariant across Stream 4's four `on unable` forms: the payload is
    /// always the `Unable` record (reason/missing/category). Not produced
    /// by the engine in Stream 6 — defined here because Stream 6 owns the
    /// canonical `SuspendTrigger` type so Stream 4's engine-emit site is
    /// pure code addition, no wire-shape change.
    AgentUnable {
        task_name: String,
        unable: crate::value::UnableRecord,
    },
    /// Emitted when a task whose declared return type is a discriminated
    /// union `A | B | ... | Unable` produces a non-Unable variant and the
    /// flow routes that variant to a checkpoint via `on <Variant> <cp>`.
    /// `variant` is the canonical record name (PascalCase, matches the
    /// source identifier); `payload` is the parsed record (with the
    /// `kind` discriminator stripped). Studio renders a generic
    /// "agent returned variant <X>" badge on this trigger.
    ///
    /// `AgentUnable` remains a specialization for the `Unable` arm —
    /// rather than `AgentVariant { variant: "Unable", ... }` — to
    /// preserve the existing Studio rendering path from #157 (zero-day
    /// compat).
    AgentVariant {
        task_name: String,
        variant: String,
        payload: serde_json::Value,
    },
}

/// Mid-loop checkpoint context attached to [`EngineEvent::Suspended`] when a
/// suspension fires inside a `loop` block's per-turn dispatch (a skill task
/// invoked as a loop tool whose `on_validation_exhausted` / `on unable`
/// handler routes to a checkpoint).
///
/// This metadata travels alongside the regular `SuspendTrigger`: the trigger
/// describes *why* the engine paused (DagPosition / ValidationExhausted /
/// AgentUnable / AgentVariant), and `LoopSuspendContext` tells consumers
/// *which loop turn* the suspension belongs to so resumption can be
/// rendered in the loop's UI lane and so the spawn handler's persisted
/// envelope carries enough context to reconstruct the loop's identity if the
/// execution is later restarted (driver state versioning is a follow-up; the
/// current cycle relies on the in-process await-point to hold the loop's
/// stack).
///
/// Serialized as a flat object (`{loop_id, loop_name, turn}`) on the wire.
/// `#[serde(default)]` on the field on [`EngineEvent::Suspended`] keeps
/// wire-compat with older servers / SDKs that don't emit it (they
/// deserialize as `None` — a non-loop suspension).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopSuspendContext {
    /// UUID v4 generated when the loop driver started; stable for the
    /// lifetime of one `loop NAME(...)` invocation. Used by the spawn
    /// handler / Studio to correlate `LoopStart` → `Suspended` → `Resumed`
    /// → subsequent `LoopTurn` events into the same per-loop UI lane.
    pub loop_id: String,
    /// The declared `loop NAME(...)` identifier. Same value as
    /// [`EngineEvent::LoopStart::name`] / [`EngineEvent::LoopTurn::name`].
    pub loop_name: String,
    /// 1-indexed turn the suspension occurred during. Matches the next
    /// [`EngineEvent::LoopTurn::turn`] the engine will emit when the
    /// suspension resumes and the turn settles.
    pub turn: u32,
}

/// Discriminator for [`EngineEvent::TaskEnd`] that tells consumers *how* a
/// task finished without having to introspect the `value` payload. Extracted
/// in #206 (Stream 4 follow-up): before this, a caller had to inspect the
/// `value` for a `Value::Unable` envelope to distinguish "the agent said I
/// can't" from a well-typed successful return. Serde-tagged on a `"variant"`
/// field so new arms ship without wire-shape churn.
///
/// `#[serde(other)]` on [`TaskEndVariant::Unknown`] preserves forward-compat:
/// a future engine that adds e.g. `Partial` (#205) still deserializes on
/// older SDKs — the unknown variant surfaces as `Unknown` and the stream
/// keeps flowing. Do *not* match without a wildcard on this enum.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TaskEndVariant {
    /// The task produced a well-typed value that passed every stage of the
    /// parse → schema → custom validation pipeline. This is the pre-#206
    /// default and the variant carried when `#[serde(default)]` fires on
    /// payloads that omit the field entirely (older servers).
    #[default]
    Success,
    /// The task's declared return type was `T | Unable` and the agent
    /// emitted a canonical `{"unable": {...}}` envelope. The `value` field
    /// on [`EngineEvent::TaskEnd`] carries the full [`Value::Unable`] record
    /// so consumers can render reason/missing/category without re-parsing.
    Unable,
    /// The task ended with a dispatch-level failure — provider error,
    /// sandbox timeout, OOM kill, schema-validation budget exhausted,
    /// or any other path where the `value` on [`EngineEvent::TaskEnd`]
    /// is a [`Value::FatalError`]. Consumers grouping by task can use
    /// this to render a failure UI without inspecting `value`. Emitted
    /// from the `runtime` dispatch path; LLM tasks may also adopt it in
    /// a follow-up. Older SDKs that don't know this variant will see it
    /// as `Unknown` via `#[serde(other)]` and behave as today.
    Failed,
    /// Catch-all for future variants the SDK doesn't know yet. `#[serde(other)]`
    /// routes unknown discriminants here so consumers never crash on a
    /// newer engine — e.g. `Partial` lands in #205 and an older SDK will
    /// see it as `Unknown` until its own upgrade.
    #[serde(other)]
    Unknown,
}

/// `serde` adapters that project a [`Value`] (or a container of it) to and
/// from the canonical wire form documented in
/// `docs/src/content/docs/reference/engine-events.mdx`.
///
/// The default `Serialize` / `Deserialize` derive on [`Value`] emits the
/// internal tagged-enum shape (`{"String":"hi"}`, `{"Object":{...}}`, …),
/// which is fine for caching / hashing but leaks the engine's internal
/// representation onto the wire. Every [`EngineEvent`] field that carries
/// a workflow-visible value uses these adapters so SDK consumers see the
/// clean form spec'd in the docs.
///
/// On the read path we reconstruct a [`Value`] from clean JSON via
/// [`Value::from_json`] — this is shape-preserving (e.g. an `Object` round-
/// trips, a number becomes `Value::Int` / `Value::Decimal`). Variants that
/// the engine emits with semantic meaning beyond shape — `Unable`, `Union`,
/// `FatalError` — are NOT reconstructed back from their wire envelopes on
/// the deserialize path because no in-process consumer relies on it: the
/// engine never reads its own emitted JSON back as a `Value`, the Rust SDK
/// converts via `to_wire_json` again before exposing it, and the durable
/// replay path branches on a different set of events (`LLMResponse`,
/// `ToolCall*`, `SubScriptResult`, `CheckpointResolution`) that already
/// store their payloads as `serde_json::Value`.
mod value_wire {
    use super::*;

    pub(super) fn serialize<S: Serializer>(v: &Value, s: S) -> Result<S::Ok, S::Error> {
        v.to_wire_json().serialize(s)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Value, D::Error> {
        let j = serde_json::Value::deserialize(d)?;
        Ok(Value::from_json(&j))
    }
}

mod opt_value_wire {
    use super::*;

    pub(super) fn serialize<S: Serializer>(v: &Option<Value>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(val) => val.to_wire_json().serialize(s),
            None => s.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
        let j = Option::<serde_json::Value>::deserialize(d)?;
        Ok(j.map(|v| Value::from_json(&v)))
    }
}

mod value_map_wire {
    use super::*;

    pub(super) fn serialize<S: Serializer>(
        m: &HashMap<String, Value>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(m.len()))?;
        for (k, v) in m {
            map.serialize_entry(k, &v.to_wire_json())?;
        }
        map.end()
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<HashMap<String, Value>, D::Error> {
        let raw = HashMap::<String, serde_json::Value>::deserialize(d)?;
        Ok(raw
            .into_iter()
            .map(|(k, v)| (k, Value::from_json(&v)))
            .collect())
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", content = "payload")]
pub enum EngineEvent {
    Log(String),
    /// Structured log line. Parallel to [`EngineEvent::Log`] but carries a
    /// severity so consumers (Studio's trace panel, the bench `why_failed`
    /// runner, the future eval-failure dashboard) can highlight WARNs and
    /// ERRORs without string-sniffing.
    ///
    /// Added in the "why-did-it-fail" infra round so that
    /// `tracing::warn!` calls in providers.rs / engine.rs ALSO show up in
    /// the execution event stream — previously they only went to the
    /// akribes-server stdout that nobody actively watches, which is how the
    /// `max_tokens=4096` truncation hid for a week. Pre-existing
    /// [`EngineEvent::Log`] is retained verbatim for wire compat with
    /// older SDKs that match on the bare-string variant.
    ///
    /// `level` is a free-form short string (`"WARN"`, `"ERROR"`,
    /// `"INFO"`, …) — kept as `String` rather than an enum so a newer
    /// engine adding `"DEBUG"` doesn't crash an older SDK.
    LogLevel {
        level: String,
        message: String,
    },
    StateUpdate(String, #[serde(with = "value_wire")] Value),
    WorkflowStart(usize),              // total tasks
    TaskStart(String, Option<String>), // (name, on_error policy label)
    TaskPrompt(String, String),        // (task_name, rendered_prompt)
    TaskEnd {
        task: String,
        on_error_label: Option<String>,
        #[serde(with = "value_wire")]
        value: Value,
        /// The declared return type of the task, if any. `None` when the task
        /// has no `-> Type` annotation (e.g. plain `str` tasks or untyped tasks).
        value_type: Option<TypeRef>,
        duration: std::time::Duration,
        /// 1-indexed attempt count: `1` = first call succeeded, `2` = first
        /// validation retry succeeded, etc. Resets on task-level `on_error`
        /// retries (which are orthogonal to validation retries).
        attempt: u8,
        usage: Option<TokenUsage>,
        /// How the task finished. Explicit discriminator so consumers don't
        /// have to inspect `value` to distinguish Success from Unable.
        /// `#[serde(default)]` keeps pre-#206 wire payloads deserialising
        /// cleanly (they become [`TaskEndVariant::Success`]). Forward-compat
        /// for later expansion (e.g. `Partial` in #205) is provided by
        /// [`TaskEndVariant::Unknown`].
        #[serde(default)]
        variant: TaskEndVariant,
    },
    AgentOutput {
        task_name: String,
        agent_name: Option<String>,
        task_id: String,
        schema_type: Option<String>,
        chunk: String,
    },
    /// Streaming extended-thinking / reasoning fragment from the LLM,
    /// emitted alongside `AgentOutput` for providers that interleave
    /// reasoning blocks into a streamed response (Anthropic extended
    /// thinking, OpenAI o-series + GPT-5 reasoning, Gemini 2.5 thinking).
    ///
    /// Issue #1176. Pre-fix the streaming path collapsed every non-text
    /// rig variant into an empty `AgentOutput` chunk; reasoning content
    /// was simply lost, so Studio's "thinking" inline panel stayed dark
    /// for streamed runs even when `reasoning_tokens` showed up on the
    /// trailing usage. This variant surfaces the same content
    /// non-streaming consumers see via the provider's message-level
    /// reasoning block, but as it arrives.
    ///
    /// Fields mirror [`Self::AgentOutput`] so a consumer that wants to
    /// render reasoning identically to text can switch on the variant
    /// name only. The `chunk` is plain text — encrypted / redacted
    /// reasoning blocks (Anthropic's opaque thinking signatures) are
    /// dropped at the provider boundary rather than surfaced here, as
    /// they have no plain-text equivalent.
    AgentReasoning {
        task_name: String,
        agent_name: Option<String>,
        task_id: String,
        /// Mirrors [`Self::AgentOutput::schema_type`] — the declared
        /// return type, when present. Reasoning is associated with the
        /// task the engine is currently dispatching.
        schema_type: Option<String>,
        chunk: String,
    },
    /// Auto cache-breakpoint engine emitted a placement decision for
    /// the upcoming dispatch. One event per Anthropic structured-output
    /// call; absent for non-Anthropic providers and for any dispatch
    /// where `EngineOptions::auto_cache_enabled` is `false`.
    ///
    /// Fields mirror `engine_cache::CachePlan` plus a human-readable
    /// agent label. Captured by the rig path (and any in-process
    /// observer) to track cache-hit rates across iterations.
    ///
    /// # All four marker slots are now reported
    ///
    /// Anthropic's 4-marker per-request budget covers four canonical
    /// slots: `tools`, `system`, `user-msg #1`, `user-msg #2`. Issue
    /// #472 item 1 brought the tools and system slots under engine
    /// ownership; the two `*_marker_placed` boolean fields below
    /// report the engine's decision for each slot. Pre-#472 payloads
    /// reported only the user-message slots (`markers_placed` /
    /// `markers_placed_at`); the new booleans default to `false` on
    /// older wire payloads via `#[serde(default)]`, which preserves
    /// the historical "engine reports user-message markers only" view
    /// for legacy consumers reading the JSON.
    ///
    /// # Engine intent vs. wire-level cache footprint
    ///
    /// Every `*_marker_placed` field describes the engine's INTENT to
    /// stamp `cache_control` on that block. Anthropic's response is
    /// what determines actual `cache_creation_input_tokens` /
    /// `cache_read_input_tokens`. Anthropic enforces a per-prefix
    /// minimum cacheable size (1024 tokens for sonnet/haiku-class
    /// models). When a `cache_control` marker sits at a prefix BELOW
    /// that minimum (e.g. the system block by itself is ~30 tokens),
    /// Anthropic extends the cache write forward to the next eligible
    /// boundary. So a "system marker only" cold dispatch can still
    /// report `cache_creation_input_tokens` ≈ entire prompt size.
    /// This is Anthropic's documented behavior; the engine's plan is
    /// correct, but the `*_marker_placed` fields describe INTENT, not
    /// the resulting wire-level cache footprint.
    ///
    /// Set `AKRIBES_DEBUG_CACHE_BODY=1` on the process running the
    /// dispatcher to print a per-call summary of which body sections
    /// carry markers.
    CachePlanned {
        /// Agent name as declared in the source (`agent classifier`).
        agent: String,
        /// Number of segments the engine assembled for this dispatch.
        /// Includes the static prefix (docstrings/rules/examples), one
        /// segment per `{placeholder}` boundary in the body template,
        /// and the trailing structured-output instruction.
        n_segments: usize,
        /// How many segments the engine considered "stable" — i.e.
        /// either previously cached for this agent or marked stable by
        /// the DAG-aware peek (referenced by an upcoming dispatch).
        n_stable: usize,
        /// Total character length of the longest stable run at the
        /// HEAD of the segment list. `0` when no leading prefix was
        /// stable.
        longest_stable_prefix_len_chars: usize,
        /// Number of `cache_control` markers actually placed on the
        /// user message. `0`, `1`, or `2` (Anthropic's user-message
        /// budget within the 4-marker per-request cap).
        ///
        /// Engine-level intent only; see the type-level docstring
        /// above for the relationship to wire markers and Anthropic's
        /// observed `cache_creation_input_tokens`.
        markers_placed: usize,
        /// Segment indices the engine stamped with a `cache_control`
        /// marker, sorted ascending. Length always equals
        /// `markers_placed`. Empty on the cold-cache / no-stable-prefix
        /// paths. Captured for the DAG-aware integration tests so they
        /// can assert the boundary picked by the placement algorithm
        /// without scraping the outbound HTTP body.
        ///
        /// `#[serde(default)]` for backwards compat with payloads
        /// emitted before the field existed.
        #[serde(default)]
        markers_placed_at: Vec<usize>,
        /// Whether the engine asked the provider to stamp
        /// `cache_control` on the `tools` block (issue #472 item 1).
        /// `true` on every Anthropic structured-output dispatch
        /// today — the synthetic `return_result` tool is stable per
        /// task and always benefits from caching. Reserved for
        /// future per-call unique-tool flows that may flip it to
        /// `false`.
        ///
        /// Provider name the dispatch is going to (`anthropic`,
        /// `openai`, `gemini`, ...). Lets SDK consumers route each
        /// `CachePlanned` event to a provider-specific renderer
        /// without scraping `agent` / model state. Issue #1019:
        /// before this field every event was an Anthropic event,
        /// invisible for OpenAI / Gemini caching paths. Carries the
        /// lower-cased provider id so renderers match against
        /// `"anthropic"` / `"openai"` / `"gemini"` directly. Empty
        /// string in old wire payloads (the `#[serde(default)]`
        /// default).
        #[serde(default)]
        provider: String,
        /// `#[serde(default)]` for backwards compat with payloads
        /// emitted before the field existed (pre-#472 payloads
        /// silently treat this as `false`, which under-reports the
        /// real wire footprint — the field is the canonical truth
        /// from this PR onward).
        #[serde(default)]
        tools_marker_placed: bool,
        /// Whether the engine asked the provider to stamp
        /// `cache_control` on the `system` block (issue #472
        /// item 1). `true` on any dispatch with a non-empty agent
        /// system prompt. Reserved for future one-shot
        /// system-prompt flows (e.g. agent definitions that include
        /// per-call data) that may flip it to `false`.
        ///
        /// `#[serde(default)]` for backwards compat — see
        /// `tools_marker_placed`.
        #[serde(default)]
        system_marker_placed: bool,
    },
    Suspended {
        checkpoint_name: String,
        token: String,
        prompt: String,
        schema: serde_json::Value,
        actor_hint: ActorHint,
        timeout_secs: Option<u64>,
        /// Why we suspended. Defaults to [`SuspendTrigger::DagPosition`] on
        /// older wire payloads that omit the field (e.g. an older server
        /// serializing against a pre-Stream-6 SDK, or vice versa). The
        /// `#[serde(default)]` here is what guarantees backwards compat.
        #[serde(default)]
        trigger: SuspendTrigger,
        /// `Some(ctx)` when the suspension fired inside a `loop` block's
        /// per-turn dispatch — see [`LoopSuspendContext`] for the carried
        /// fields. `None` for the existing top-level / DAG checkpoint
        /// path, which is the wire shape every pre-loop-checkpoint SDK
        /// emits. `#[serde(default, skip_serializing_if = "Option::is_none")]`
        /// keeps both directions wire-compatible: old servers serializing
        /// against new SDKs simply omit the field, and new servers emit it
        /// only when there's a loop context to carry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        loop_context: Option<LoopSuspendContext>,
    },
    Resumed {
        checkpoint_name: String,
        token: String,
    },
    /// Terminal event of a workflow run.
    ///
    /// # Aggregate rollup (issue #1173)
    ///
    /// Pre-#1173 this was the tuple variant `WorkflowEnd(Value)` whose
    /// payload was the bare workflow output. Consumers re-walked every
    /// `TaskEnd` to compute totals; cluster dashboards and the CLI's
    /// trailer line did this on every execution. The variant still has
    /// a single payload slot — a [`WorkflowEndPayload`] — but the
    /// payload itself now carries both the output value AND a
    /// [`WorkflowTotals`] rollup so consumers can size the run without
    /// touching the per-task stream.
    ///
    /// # Wire compatibility
    ///
    /// New wire shape:
    /// `{"type": "WorkflowEnd", "payload": {"value": <output>, "total_input_tokens": N, ...}}`.
    /// Legacy wire shape (pre-#1173):
    /// `{"type": "WorkflowEnd", "payload": <output>}` — payload IS the
    /// bare output value.
    ///
    /// [`WorkflowEndPayload`] implements `Serialize`/`Deserialize` by
    /// hand so it accepts both shapes: new shape when the payload is a
    /// JSON object with a `value` key plus at least one `total_*`
    /// aggregate key, legacy bare-value otherwise. Aggregate fields
    /// default to `0` on legacy reads; serialization always emits the
    /// new shape.
    WorkflowEnd(WorkflowEndPayload),
    /// Structured failure surfaced to subscribers (SSE, WebSocket, OTel).
    /// `message` and `kind` are kept for back-compat; `code`,
    /// `user_message`, `retry_after_ms`, and `source` carry the richer
    /// detail consumers should branch on. New construction goes through
    /// [`EngineEvent::error_from_detail`] so all fields stay in sync.
    Error {
        message: String,
        kind: ErrorKind,
        #[serde(default = "default_error_code_other")]
        code: ErrorCode,
        #[serde(default)]
        user_message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "ErrorSource::is_empty")]
        source: ErrorSource,
    },
    NodeStart(NodeId, Span),
    NodeEnd {
        node_id: NodeId,
        span: Span,
        target_var: Option<String>,
        #[serde(with = "opt_value_wire")]
        value: Option<Value>,
        duration: std::time::Duration,
    },
    Breakpoint {
        node_id: NodeId,
        span: Span,
        token: String,
        #[serde(with = "value_map_wire")]
        env_snapshot: std::collections::HashMap<String, Value>,
    },
    BreakpointResumed {
        node_id: NodeId,
        token: String,
    },
    ToolCallStart {
        task_name: String,
        tool_name: String,
        server_name: String,
        input: serde_json::Value,
        /// LLM-issued `tool_use_id`. Empty string on pre-durable-execution
        /// payloads (preserved by `#[serde(default)]`). Always present on
        /// events written by v1+ engines; the cache lookup at the
        /// `ToolCallEnd` site keys on this.
        #[serde(default)]
        tool_use_id: String,
    },
    ToolCallEnd {
        task_name: String,
        tool_name: String,
        #[serde(default)]
        tool_use_id: String,
        output: serde_json::Value,
        duration: std::time::Duration,
    },
    /// An MCP server's circuit breaker tripped open.
    McpServerDegraded {
        alias: String,
        reason: String,
    },
    /// An MCP server recovered (circuit breaker closed again).
    McpServerRecovered {
        alias: String,
    },
    /// A destructive MCP tool invocation is awaiting operator approval.
    /// Mirrors the checkpoint suspension protocol — the execution is
    /// resumed by `POST /executions/{id}/resume` with
    /// `{ approve: bool, args_override?: Value }` keyed on `token`.
    ToolApprovalPending {
        execution_id: Option<String>,
        node_id: Option<u64>,
        token: String,
        tool_ref: String,
        args: serde_json::Value,
    },
    /// Audit-trail companion to [`EngineEvent::ToolApprovalPending`]
    /// (issue #857). Emitted on every resume path — both approval and
    /// rejection — so trace replay can reconstruct the decision and
    /// Studio's approval inbox can render a checkmark/X next to the
    /// resolved row. Without this event the only way to distinguish
    /// "approved" from "rejected" after the fact is to observe whether
    /// a subsequent `ToolCallStart` fired against the same token,
    /// which is fragile and loses the rejection reason.
    ToolApprovalResolved {
        /// Matches the `token` on the originating
        /// [`EngineEvent::ToolApprovalPending`].
        token: String,
        /// True on approve, false on reject.
        approved: bool,
        /// Operator-supplied argument override on approval, when the
        /// approver chose to edit the tool args before dispatch. None
        /// on rejection or on plain approve-as-proposed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args_override: Option<serde_json::Value>,
        /// Optional reason string the approver attached to the decision.
        /// Surfaced by Studio's inbox; persisted for compliance audits.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Emitted when a tool that would normally have required approval
    /// was dispatched without prompting because a pre-configured policy
    /// (allowlist / read-only classification / project-level
    /// auto-approval) covered it. Operator-audit gap closer (issue
    /// #1110): without this event the only way to spot auto-approval
    /// is to compare the policy config against the trace stream
    /// out-of-band.
    ///
    /// `reason` is a short policy-driven discriminator (e.g.
    /// `"policy:read_only"`, `"policy:allowlisted"`,
    /// `"policy:internal"`) so SDK consumers can render a badge
    /// without re-classifying the tool themselves.
    ToolApprovalSkipped {
        execution_id: Option<String>,
        node_id: Option<u64>,
        tool_ref: String,
        reason: String,
    },
    /// Emitted during durable replay when the replay cache holds a
    /// `ToolCallStart` for a given `tool_use_id` but no matching
    /// `ToolCallEnd`. The tool MAY have fired once on a previous run before
    /// the server crashed mid-call, and we are about to re-fire it — which
    /// will cause a duplicate side effect for non-idempotent tools (e.g.
    /// `send_email`, `create_pr`).
    ///
    /// This previously emitted only a `tracing::warn!(target =
    /// "tool_replay_uncertain", ...)`, which is invisible to SDK / Studio
    /// consumers. The structured event lets the operator surface a
    /// "replay-uncertain tool" badge inline in the execution timeline and
    /// decide whether to acknowledge before the re-run continues (#872).
    ///
    /// `args` is the raw JSON-encoded tool input from the cached start
    /// event so the operator can compare against the impending re-fire's
    /// args. `#[serde(default)]` on the deserializer keeps wire-compat
    /// with pre-#872 events that omit the variant entirely (they decode
    /// through the SDK's `Other` catch-all).
    ToolReplayUncertain {
        execution_id: Option<String>,
        tool_use_id: String,
        tool_name: String,
        #[serde(default)]
        args: serde_json::Value,
    },
    VerificationStart {
        workflow_name: String,
    },
    VerificationResult {
        workflow_name: String,
        results: serde_json::Value,
        duration: std::time::Duration,
    },
    /// A structured-output task's response failed validation. Emitted in
    /// addition to the existing `Log` line so SDK consumers without the new
    /// event still render the human-readable summary, but tooling that knows
    /// about this variant can render the model's actual response, the
    /// schema-validator's structured error breakdown, and the provider's
    /// `stop_reason` (so e.g. a `max_tokens` truncation isn't misdiagnosed
    /// as "schema overflow" — see issue #320).
    ///
    /// Fields:
    /// * `task_name` — the task whose validation failed.
    /// * `attempt` — 1-indexed attempt number (1 = first call, 2 = first
    ///   retry, …).
    /// * `model_response` — the raw text / JSON-serialized tool input the
    ///   model emitted, exactly as the validator saw it. May be empty (`""`)
    ///   or `"{}"` when the model truncated mid-output.
    ///
    ///   **Issue #1139 — bounded.** Capped at
    ///   [`VALIDATION_FAILURE_RESPONSE_CAP_BYTES`] bytes on emit. If the
    ///   raw response was longer, `truncated` is `true` and `total_length`
    ///   carries the original byte count so consumers can surface "model
    ///   emitted 4 MB; first 64 KB shown" instead of silently logging a
    ///   megabyte to `execution_events` three times in a retry loop.
    /// * `truncated` — `true` when `model_response` was truncated to fit
    ///   the cap. `#[serde(default)]` keeps pre-#1139 wire payloads
    ///   decoding cleanly (they decode as `false`).
    /// * `total_length` — original byte length of the model response
    ///   before truncation. Equal to `model_response.len()` when
    ///   `truncated` is `false`. `#[serde(default)]` for wire-compat.
    /// * `missing_fields` — JSON-pointer paths to required fields the schema
    ///   validator flagged as absent.
    /// * `extra_fields` — paths to fields rejected by `additionalProperties:
    ///   false`.
    /// * `type_errors` — human-readable type / value mismatches (e.g.
    ///   `"expected string, got null at /name"`). Includes any non-missing,
    ///   non-additional-property schema errors plus parse / custom-validator
    ///   messages.
    /// * `stop_reason` — the provider's stop reason, when known. For
    ///   Anthropic this is `"end_turn"` / `"max_tokens"` / `"tool_use"` etc.
    ///   `None` when the upstream call didn't surface one.
    ValidationFailure {
        task_name: String,
        attempt: u32,
        model_response: String,
        #[serde(default)]
        truncated: bool,
        #[serde(default)]
        total_length: u64,
        missing_fields: Vec<String>,
        extra_fields: Vec<String>,
        type_errors: Vec<String>,
        stop_reason: Option<String>,
    },
    /// Envelope that wraps an event emitted by a sub-script invoked from a
    /// parent task via the `call("name", inputs={...})` script-composition
    /// primitive (roadmap item A).
    ///
    /// # Flat wire shape (issue #993)
    ///
    /// Before #993 the envelope nested a `Box<EngineEvent>` per call-stack
    /// level, so a depth-N chain produced a single event whose serialized
    /// size was `O(N)` (a 10-level fanout could push one SSE frame past a
    /// megabyte and choke reconnect logic in cluster cards). The new shape
    /// is flat: `child` is always the LEAF event the innermost sub-engine
    /// emitted (never another `SubScript`), and `parent_path` carries the
    /// outer ancestor chain by id.
    ///
    /// * `script_name` — the immediately-running sub-script (innermost
    ///   frame). Same field semantics as the legacy wire shape.
    /// * `parent_task` — the variable name on the immediate-parent side
    ///   that received the call's result.
    /// * `parent_node_id` / `attempt` — the immediate parent's
    ///   call(...) node id + author-raise attempt counter (#845).
    /// * `parent_path` — ordered `[outermost, ..., immediate_parent]`;
    ///   empty when the current sub-script is a direct child of the
    ///   top-level workflow.
    /// * `child` — the leaf event the innermost sub-engine emitted.
    ///   Boxed to keep the variant cheap on the stack. Field name kept
    ///   for wire-compat with pre-#993 consumers; the meaning narrowed
    ///   from "wrapped event (potentially another SubScript)" to "leaf
    ///   event".
    ///
    /// Consumers that want the full call tree walk `parent_path` once
    /// and consume `child` as a leaf. The top-level reducer in each SDK
    /// is responsible for stitching frames back into a tree.
    ///
    /// # Back-compat read path
    ///
    /// Legacy emissions (pre-#993) put a nested `SubScript` inside
    /// `child`, with `parent_path` absent or empty. [`Self::flatten_subscript_chain`]
    /// is the canonical post-deserialize step that walks any nested
    /// SubScripts into `parent_path`, leaving the resulting envelope
    /// in the new flat form. The Rust SDK runs it on every inbound
    /// event before projecting to `WorkflowEvent`; the TS and Python
    /// SDKs flatten equivalently inside their reducers.
    SubScript {
        /// Script name of the innermost (currently emitting) sub-script.
        script_name: String,
        /// Parent-side variable name that received the immediate parent's
        /// call result.
        parent_task: String,
        /// Compiler-stable id of the immediate parent's call(...) node.
        /// Lets SDK consumers correlate retries of the same call site
        /// (issue #845). `#[serde(default)]` keeps pre-#845 wire payloads
        /// decoding cleanly.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_node_id: Option<u64>,
        /// 1-indexed attempt counter for author-raise retries at the
        /// same call site. See [`SubScriptFrame::attempt`] for the same
        /// semantics on ancestor frames.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt: Option<u8>,
        /// Ancestor chain from outermost to immediate parent. Empty
        /// when the emitting sub-script sits directly under the
        /// top-level workflow. Skipped from the wire when empty so a
        /// depth-1 envelope stays as compact as it was pre-#993, and
        /// `#[serde(default)]` keeps pre-#993 payloads (which omit the
        /// field) decoding cleanly.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        parent_path: Vec<SubScriptFrame>,
        /// The leaf event the innermost sub-engine emitted. New
        /// emissions guarantee this is never another `SubScript`; legacy
        /// payloads may still nest one — call [`EngineEvent::flatten_subscript_chain`]
        /// to normalize.
        child: Box<EngineEvent>,
    },
    /// A `loop` block began executing. Emitted exactly once per loop call,
    /// before any `LoopTurn`. `max_turns` is the resolved upper-bound budget
    /// (declared `max_turns:` if present, else the engine's
    /// `LOOP_MAX_TURNS_DEFAULT`). Additive event — older SDKs treat the
    /// JSON payload as an unknown variant and ignore it.
    LoopStart {
        name: String,
        max_turns: u32,
    },
    /// A single turn of a `loop` block settled (provider call returned and
    /// every `tool_use` block was dispatched). `turn` is 1-indexed.
    /// `tool_calls` is the names of the tools the model invoked this turn,
    /// in dispatch order — including the synthetic `state_get`,
    /// `state_update`, `return` and any user `skills:` entries.
    LoopTurn {
        name: String,
        turn: u32,
        tool_calls: Vec<String>,
        /// Per-turn token usage reported by the provider for the
        /// dispatch this turn produced. `None` on providers that don't
        /// surface per-call usage (mock provider) and on pre-#829
        /// wire payloads. Lets Studio's loop card show "turn 3 spent
        /// 4500 tokens" without walking the wrapped `LLMResponse`
        /// sub-tree of events.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<TokenUsage>,
    },
    /// A `loop` block exited. `value` is the agent's submitted return value
    /// (from `return(...)`), the final state on natural `stop_when` exit
    /// without a return, or a `Value::FatalError` envelope when the loop
    /// exhausted its `max_turns` budget without ever calling `return`.
    /// `turn_count` is the number of turns actually executed (1-indexed).
    LoopEnd {
        name: String,
        turn_count: u32,
        #[serde(with = "value_wire")]
        value: Value,
    },
    /// Emitted when a compaction step runs — once per primitive
    /// activation. `provider_native: true` means Anthropic / OpenAI
    /// performed the compaction server-side; the engine surfaces the
    /// before/after counts from the response. `strategy` is the
    /// primitive name (`drop_thinking_blocks`, `drop_oldest_tool_results`,
    /// `summarize_to_state`, `provider_native`) or the user task name
    /// for a custom compactor task.
    ///
    /// `cache_ttl` is `Some("5m")` or `Some("1h")` on the Anthropic
    /// `provider_native` path (the engine pins `ttl: "1h"` via the
    /// `extended-cache-ttl-2025-04-11` beta header — see
    /// `providers.rs:1772`), `None` for every non-native compaction
    /// primitive (the engine-driven primitives don't write any cache
    /// block themselves). Downstream cost dashboards multiply
    /// cache-write tokens by the 1h-vs-5m rate (issue #1130); without
    /// this field the wire envelope leaves the TTL ambiguous.
    /// `#[serde(default, skip_serializing_if = "Option::is_none")]`
    /// keeps pre-#1130 wire payloads decodable as `cache_ttl = None`.
    ///
    /// See the compaction design at
    /// `docs/superpowers/specs/2026-05-12-compaction-design.md` for the
    /// "Observability + cost" contract; the chain emits one
    /// `ContextCompacted` per primitive activation and `ContextOverflow`
    /// at chain exhaustion.
    ContextCompacted {
        agent: String,
        loop_id: Option<String>,
        turn: Option<u32>,
        threshold_pct: Option<u8>,
        threshold_abs: Option<u32>,
        strategy: String,
        before_tokens: u32,
        after_tokens: u32,
        provider_native: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_ttl: Option<String>,
    },
    /// Emitted when the compaction chain runs to exhaustion (or when
    /// `compaction: none` and the request would exceed the model context).
    /// Carries the chain log so users can diagnose which primitives ran
    /// before the engine gave up.
    ///
    /// `terminated_by_hard_error` distinguishes the user-authored
    /// `hard_error()` early-exit (issue #1056) from the implicit
    /// chain-exhaustion exit. SDKs can render a stronger "author
    /// intentionally bailed out at threshold X" message instead of the
    /// generic "all arms exhausted" copy.
    ///
    /// `#[serde(default)]` on the new field keeps pre-#1056 wire
    /// payloads decodable (they decode as `terminated_by_hard_error =
    /// false`, which matches their original semantics — every old
    /// emission was a chain-exhaustion exit).
    ContextOverflow {
        agent: String,
        attempted_strategies: Vec<String>,
        configured_cap_tokens: u32,
        model_context_window: u32,
        /// `true` when the chain hit a user-authored `hard_error()`
        /// step; `false` when the chain ran past every step without
        /// freeing enough budget. See the type-level docs for the
        /// distinction and the back-compat default.
        #[serde(default)]
        terminated_by_hard_error: bool,
    },
    /// Emitted by the engine when a task's result is served from
    /// [`crate::engine_persistent_cache::PersistentTaskCache`] instead of
    /// being dispatched to a provider. Lets trace inspectors, the bench
    /// UI, and the MCP show "stages 1-3 were free, stage 4 ran" without
    /// having to parse prompt-segment internals.
    ///
    /// `agent` is the agent name the task ran under (matches
    /// [`EngineEvent::AgentOutput::agent_name`] for the same task).
    /// `key_prefix` is the first 6 hex chars of the (u64) cache key so
    /// consumers can cluster repeated hits without persisting the full
    /// key. The prefix is informational only — collisions are harmless,
    /// the key itself is the source of truth.
    ///
    /// Emitted on the cache-hit branch of the engine's task dispatch
    /// loop, before the corresponding `TaskEnd` event for the same task.
    TaskCacheHit {
        agent: String,
        key_prefix: String,
    },
    /// LLM provider response captured for durable replay. Carries the full
    /// response (text + tool-use blocks + usage) keyed by `(node_id,
    /// call_index)`. See `crates/akribes-core/src/replay_cache.rs`.
    LLMResponse {
        node_id: String,
        call_index: u32,
        text: String,
        tool_calls: Vec<CachedToolCall>,
        usage: Option<TokenUsage>,
    },
    /// Emitted on the cache-hit branch of `call_provider_*` —
    /// distinguishes "this task's underlying LLM call was served from
    /// the replay cache" from "the task's full result was served from
    /// the persistent task cache" (the latter already has
    /// [`EngineEvent::TaskCacheHit`]). Issue #815: a reconnecting SSE
    /// client otherwise can't tell a cache-served task from a slow
    /// first-token, because the engine deliberately does not re-emit
    /// streaming AgentOutput chunks on replay.
    ///
    /// `node_id` + `call_index` mirror the keying of the underlying
    /// `LLMResponse` variant so consumers can correlate the hit with
    /// the cached response.
    LLMReplayCacheHit {
        node_id: String,
        call_index: u32,
    },
    /// A child execution row was just inserted at the parent's `call(...)`
    /// node. The parent's event log carries this *intent* event; the
    /// child's own log records its lifecycle independently.
    SubScriptSpawned {
        child_execution_id: String,
        parent_node_id: String,
        args: serde_json::Value,
    },
    /// Child execution finished and the parent observed its terminal
    /// state. Synthesised by the parent reading the child's terminal
    /// event; never written by the child itself.
    SubScriptResult {
        parent_node_id: String,
        child_execution_id: String,
        outcome: ChildOutcome,
    },
    /// A `Suspended` checkpoint resolved. Written when `POST
    /// /executions/:id/resume` lands a payload; replay re-derives the
    /// engine state from this event instead of re-suspending.
    CheckpointResolution {
        checkpoint_id: String,
        payload: serde_json::Value,
    },
    /// Emitted when the engine starts dispatching a `runtime` block (the
    /// container code-execution construct — see the
    /// "AI-driven container code execution" feature). One event per
    /// runtime call site, before any `RuntimeStdout`/`RuntimeStderr`
    /// chunks. `task_name` is the call-site identifier the engine uses
    /// to attribute the call (matches the wrapping `TaskStart`/`TaskEnd`
    /// pair's `task` field so SDK reducers that group by task keep
    /// working). `runtime_name` is the declared `runtime NAME(...)`
    /// identifier. `language` is the source-form keyword
    /// (`"python"` / `"bash"` / `"node"` / `"rust"` / `"java"`).
    RuntimeStart {
        task_name: String,
        runtime_name: String,
        language: String,
    },
    /// A chunk of stdout produced by the running sandbox. `chunk` is the
    /// raw byte slice decoded as UTF-8 (lossy). Multiple events fire in
    /// arrival order; SDK reducers concatenate to reconstruct the full
    /// stream.
    RuntimeStdout {
        task_name: String,
        chunk: String,
    },
    /// A chunk of stderr produced by the running sandbox. Mirrors
    /// [`EngineEvent::RuntimeStdout`] for the error stream.
    RuntimeStderr {
        task_name: String,
        chunk: String,
    },
    /// The runtime call finished successfully. `exit_code` is the
    /// container's process exit code (0 for clean exit; non-zero on
    /// crash / panic / explicit non-zero exit). `duration_ms` is the
    /// wall-clock time the sandbox reported between dispatch and exit
    /// — does not include the time the engine spent waiting on its own
    /// semaphore.
    RuntimeEnd {
        task_name: String,
        exit_code: i32,
        duration_ms: u64,
    },
    /// The runtime call failed before producing an exit code.
    /// `kind` is a stable wire-form discriminator:
    /// `"NotConfigured"` (no sandbox URL set),
    /// `"Timeout"` (execution exceeded the declared timeout),
    /// `"SandboxUnavailable"` (network / connect error to the sandbox),
    /// `"OomKilled"` (the container hit its memory cap),
    /// `"Internal"` (any other sandbox-side failure).
    /// `message` is human-readable detail forwarded from the sandbox's
    /// `error` SSE event (or synthesised by the engine for client-side
    /// errors like `NotConfigured`).
    RuntimeError {
        task_name: String,
        kind: String,
        message: String,
    },
}

/// Cap applied to [`EngineEvent::ValidationFailure::model_response`]
/// on emit (issue #1139). Set to 64 KiB — enough to capture the
/// validator's view of any reasonable model output while keeping
/// `execution_events` rows bounded. A 4 MB tool input on a three-retry
/// validation loop used to bloat the persisted log by 12 MB per task;
/// post-cap the same loop emits at most 192 KB.
pub const VALIDATION_FAILURE_RESPONSE_CAP_BYTES: usize = 64 * 1024;

impl EngineEvent {
    /// Flatten any legacy nested [`EngineEvent::SubScript`] chain into the
    /// new `parent_path + child(leaf)` shape (issue #993).
    ///
    /// Pre-#993 emissions wrapped each call-stack level in its own
    /// `SubScript` envelope (`SubScript { child: Box<SubScript{ child: ... }> }`).
    /// The new shape carries the chain via `parent_path` and reserves
    /// `child` for the innermost leaf event. This method walks down
    /// `child` for as long as it is itself a `SubScript`, accumulating
    /// each frame into a growing `parent_path` (so the resulting list
    /// reads `[outermost_ancestor, …, immediate_parent_of_leaf]`), and
    /// finally sets `child` to the recovered leaf.
    ///
    /// Events that are not `SubScript` envelopes are returned unchanged.
    /// SDK consumers (Rust / TS / Python) call this on every inbound
    /// event so reducers see a uniform flat shape regardless of which
    /// engine version emitted the wire log.
    pub fn flatten_subscript_chain(self) -> Self {
        // We treat the current SubScript as a stack of `(script_name,
        // parent_task, parent_node_id, attempt)` frames anchored above
        // a leaf. Walk inward, accumulating frames; the OUTER frame at
        // each step is the immediate parent of the next inward step.
        let (script_name, parent_task, parent_node_id, attempt, outer_path, child) = match self {
            EngineEvent::SubScript {
                script_name,
                parent_task,
                parent_node_id,
                attempt,
                parent_path,
                child,
            } => (
                script_name,
                parent_task,
                parent_node_id,
                attempt,
                parent_path,
                child,
            ),
            other => return other,
        };

        // Path order is `[outermost_ancestor, ..., immediate_parent_of_innermost]`.
        // `outer_path` is already in that order (legacy emissions have it empty).
        // The current top-level frame (cur_*) sits AFTER everything in outer_path
        // and is the immediate parent of whatever lives in `child`. As we walk
        // inward, cur_* moves down into `outer_path`'s suffix.
        let mut frames: Vec<SubScriptFrame> = outer_path;
        let mut cur_script = script_name;
        let mut cur_task = parent_task;
        let mut cur_node = parent_node_id;
        let mut cur_attempt = attempt;
        let mut cur_child: Box<EngineEvent> = child;

        loop {
            match *cur_child {
                EngineEvent::SubScript {
                    script_name: inner_script,
                    parent_task: inner_task,
                    parent_node_id: inner_node,
                    attempt: inner_attempt,
                    parent_path: inner_path,
                    child: inner_child,
                } => {
                    // Promote cur_* into the frame list — it now sits
                    // ABOVE the inner frame in the ancestor chain.
                    // Order: existing frames, then any frames the inner
                    // envelope already carried in its own parent_path
                    // (legacy emissions have this empty), then cur_*.
                    frames.extend(inner_path);
                    frames.push(SubScriptFrame {
                        script_name: cur_script,
                        parent_task: cur_task,
                        parent_node_id: cur_node,
                        attempt: cur_attempt,
                    });
                    cur_script = inner_script;
                    cur_task = inner_task;
                    cur_node = inner_node;
                    cur_attempt = inner_attempt;
                    cur_child = inner_child;
                }
                leaf => {
                    return EngineEvent::SubScript {
                        script_name: cur_script,
                        parent_task: cur_task,
                        parent_node_id: cur_node,
                        attempt: cur_attempt,
                        parent_path: frames,
                        child: Box::new(leaf),
                    };
                }
            }
        }
    }

    /// Build a [`EngineEvent::ValidationFailure`] with `model_response`
    /// capped to [`VALIDATION_FAILURE_RESPONSE_CAP_BYTES`] (issue
    /// #1139). The original byte length is preserved on `total_length`
    /// and `truncated` is set when the cap fired so consumers can
    /// surface "first 64 KB shown; original was N bytes" without
    /// having to re-derive it. UTF-8 boundary is respected — the
    /// truncation slices at the closest char boundary below the cap.
    pub fn validation_failure(
        task_name: impl Into<String>,
        attempt: u32,
        model_response: String,
        missing_fields: Vec<String>,
        extra_fields: Vec<String>,
        type_errors: Vec<String>,
        stop_reason: Option<String>,
    ) -> Self {
        let total_length = model_response.len() as u64;
        let (response, truncated) = if model_response.len() > VALIDATION_FAILURE_RESPONSE_CAP_BYTES
        {
            // Walk down to the nearest char boundary so we don't
            // produce a fragment of a multi-byte UTF-8 sequence.
            let mut end = VALIDATION_FAILURE_RESPONSE_CAP_BYTES;
            while end > 0 && !model_response.is_char_boundary(end) {
                end -= 1;
            }
            (model_response[..end].to_string(), true)
        } else {
            (model_response, false)
        };
        EngineEvent::ValidationFailure {
            task_name: task_name.into(),
            attempt,
            model_response: response,
            truncated,
            total_length,
            missing_fields,
            extra_fields,
            type_errors,
            stop_reason,
        }
    }

    /// Build an [`EngineEvent::Error`] from a fully-formed
    /// [`crate::error::ErrorDetail`]. Use this at every error-emission
    /// site so SDK / OTel / DB consumers all see the same structured
    /// fields.
    pub fn error(detail: crate::error::ErrorDetail) -> Self {
        EngineEvent::Error {
            message: detail.message,
            kind: detail.kind,
            code: detail.code,
            user_message: detail.user_message,
            retry_after_ms: detail.retry_after_ms,
            source: detail.source,
        }
    }

    /// Quick constructor for sites that don't yet have a specific
    /// [`ErrorCode`]. Picks the closest "Other" code for the kind via
    /// [`crate::error::ErrorDetail::from_kind`].
    pub fn error_kind(kind: ErrorKind, message: impl Into<String>) -> Self {
        EngineEvent::error(crate::error::ErrorDetail::from_kind(kind, message))
    }

    /// Quick constructor from a specific [`ErrorCode`].
    pub fn error_code(code: ErrorCode, message: impl Into<String>) -> Self {
        EngineEvent::error(crate::error::ErrorDetail::new(code, message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::TypeRef;
    use crate::value::Value;

    #[test]
    fn task_end_event_serializes_with_value_and_value_type_and_attempt() {
        let e = EngineEvent::TaskEnd {
            task: "t".into(),
            on_error_label: None,
            value: Value::String("x".into()),
            value_type: Some(TypeRef::primitive("int")),
            duration: std::time::Duration::from_millis(10),
            attempt: 2,
            usage: None,
            variant: TaskEndVariant::Success,
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(
            s.contains("\"value\""),
            "serialized event should contain 'value' key: {}",
            s
        );
        assert!(
            s.contains("\"value_type\""),
            "serialized event should contain 'value_type' key: {}",
            s
        );
        assert!(
            s.contains("\"attempt\":2"),
            "serialized event should contain 'attempt':2: {}",
            s
        );
        assert!(
            s.contains("\"variant\":\"success\""),
            "variant should round-trip as snake_case 'success': {}",
            s
        );
    }

    #[test]
    fn task_end_value_emits_clean_wire_form_not_tagged_value() {
        // Spec: `docs/src/content/docs/reference/engine-events.mdx` — every
        // `Value`-carrying field on an `EngineEvent` serializes as clean
        // JSON (the form `Value::to_wire_json` produces), not the internal
        // tagged-enum form (`{"String": ...}`, `{"Object": {...}}`).
        let mut obj = std::collections::HashMap::new();
        obj.insert("exit_code".to_string(), Value::Int(0));
        obj.insert("stdout".to_string(), Value::String("hi\n".into()));
        let e = EngineEvent::TaskEnd {
            task: "run".into(),
            on_error_label: None,
            value: Value::Object(obj),
            value_type: None,
            duration: std::time::Duration::from_millis(10),
            attempt: 1,
            usage: None,
            variant: TaskEndVariant::Success,
        };
        let j: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        let value = &j["payload"]["value"];
        assert_eq!(value["exit_code"], serde_json::json!(0));
        assert_eq!(value["stdout"], serde_json::json!("hi\n"));
        assert!(
            value.get("Object").is_none(),
            "wire form must not carry the tagged-enum 'Object' key: {value}",
        );
        assert!(
            value["exit_code"].get("Int").is_none(),
            "wire form must not carry the tagged-enum 'Int' key for scalars: {value}",
        );
    }

    #[test]
    fn workflow_end_payload_emits_clean_wire_form_not_tagged_value() {
        // Issue #1173 reshape: `WorkflowEnd.payload` is now an object
        // with a `value` sub-field carrying the workflow's clean output
        // alongside the aggregate `total_*` rollup. The output under
        // `value` is still in the clean wire form (no `"Object"` /
        // `"Int"` tagged-enum keys) per the engine-events reference.
        let mut obj = std::collections::HashMap::new();
        obj.insert("exit_code".to_string(), Value::Int(0));
        obj.insert("duration_ms".to_string(), Value::Int(12));
        let e = EngineEvent::WorkflowEnd(WorkflowEndPayload::new(Value::Object(obj)));
        let j: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        let payload = &j["payload"];
        // The workflow output is now nested under `payload.value`.
        let value = &payload["value"];
        assert_eq!(value["exit_code"], serde_json::json!(0));
        assert_eq!(value["duration_ms"], serde_json::json!(12));
        assert!(
            value.get("Object").is_none(),
            "wire form must not carry the tagged-enum 'Object' key: {value}",
        );
        // Aggregate rollup is present and defaults to zero on a
        // payload built without explicit totals.
        assert_eq!(payload["total_input_tokens"], serde_json::json!(0));
        assert_eq!(payload["total_output_tokens"], serde_json::json!(0));
        assert_eq!(payload["task_count"], serde_json::json!(0));
    }

    #[test]
    fn workflow_end_value_deserialise_round_trip_keeps_clean_shape() {
        // A consumer that round-trips `EngineEvent` via serde (e.g. the
        // durable event log on the server) should see the same clean JSON
        // shape both directions. The inner `Value` is reconstructed via
        // `Value::from_json`, which preserves shape but not semantic
        // variants (`Object`/`Int`/`String`/`List`/`Null`/`Decimal`/`Bool`).
        let mut obj = std::collections::HashMap::new();
        obj.insert("exit_code".to_string(), Value::Int(0));
        obj.insert("stdout".to_string(), Value::String("hi".into()));
        let e = EngineEvent::WorkflowEnd(WorkflowEndPayload::new(Value::Object(obj)));
        let s = serde_json::to_string(&e).unwrap();
        let back: EngineEvent = serde_json::from_str(&s).unwrap();
        let s_again = serde_json::to_string(&back).unwrap();
        // Re-serialising the round-tripped event must produce identical JSON,
        // i.e. the wire form is its own fixed point.
        assert_eq!(s, s_again, "wire form should round-trip without drift");
    }

    #[test]
    fn workflow_end_back_compat_legacy_bare_value_payload_parses() {
        // Issue #1173 back-compat: an event log captured against a
        // pre-#1173 server emits `payload` as the bare workflow value.
        // The custom `Deserialize` impl must accept that shape and
        // default the aggregate rollup to all-zero.
        let legacy = r#"{"type":"WorkflowEnd","payload":{"exit_code":0,"stdout":"hi"}}"#;
        let evt: EngineEvent = serde_json::from_str(legacy).unwrap();
        match evt {
            EngineEvent::WorkflowEnd(payload) => {
                // Output recovered from bare payload.
                assert!(matches!(payload.value, Value::Object(_)));
                // Totals default to zero on legacy reads.
                assert_eq!(payload.totals.total_input_tokens, 0);
                assert_eq!(payload.totals.total_output_tokens, 0);
                assert_eq!(payload.totals.task_count, 0);
            }
            other => panic!("expected WorkflowEnd, got {other:?}"),
        }
    }

    #[test]
    fn workflow_end_scalar_legacy_payload_parses_as_bare_value() {
        // Edge case: a workflow that returns a scalar (e.g.
        // `return "hi"`) emits `payload: "hi"` on the legacy wire.
        // Make sure the deserializer doesn't try to interpret that
        // as the new struct shape.
        let legacy = r#"{"type":"WorkflowEnd","payload":"hi"}"#;
        let evt: EngineEvent = serde_json::from_str(legacy).unwrap();
        match evt {
            EngineEvent::WorkflowEnd(payload) => {
                assert!(matches!(payload.value, Value::String(ref s) if s == "hi"));
            }
            other => panic!("expected WorkflowEnd, got {other:?}"),
        }
    }

    #[test]
    fn workflow_end_new_wire_shape_round_trips_with_totals() {
        // The new wire shape carries both `value` and `total_*` keys.
        // Round-trip serialize → deserialize → serialize must preserve
        // every field exactly.
        let totals = WorkflowTotals {
            total_input_tokens: 1234,
            total_output_tokens: 567,
            total_cached_input_tokens: 100,
            total_thinking_tokens: 25,
            total_tool_tokens: 0,
            total_cost_usd: 0.42,
            task_count: 3,
        };
        let mut obj = std::collections::HashMap::new();
        obj.insert("answer".to_string(), Value::Int(42));
        let payload = WorkflowEndPayload::with_totals(Value::Object(obj), totals.clone());
        let evt = EngineEvent::WorkflowEnd(payload);
        let s = serde_json::to_string(&evt).unwrap();
        let back: EngineEvent = serde_json::from_str(&s).unwrap();
        match back {
            EngineEvent::WorkflowEnd(p) => {
                assert_eq!(p.totals.total_input_tokens, 1234);
                assert_eq!(p.totals.total_output_tokens, 567);
                assert_eq!(p.totals.total_cached_input_tokens, 100);
                assert_eq!(p.totals.total_thinking_tokens, 25);
                assert_eq!(p.totals.task_count, 3);
                assert!((p.totals.total_cost_usd - 0.42).abs() < 1e-9);
            }
            other => panic!("expected WorkflowEnd, got {other:?}"),
        }
    }

    #[test]
    fn workflow_totals_accumulate_folds_token_usage() {
        // Engine's `emit(TaskEnd)` path delegates to `accumulate`;
        // each call adds usage tokens and bumps `task_count` even
        // when usage is `None`.
        let mut tot = WorkflowTotals::default();
        tot.accumulate(Some(&TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            cached_input_tokens: 10,
            reasoning_tokens: 7,
            ..Default::default()
        }));
        tot.accumulate(Some(&TokenUsage {
            input_tokens: 200,
            output_tokens: 80,
            cached_input_tokens: 30,
            reasoning_tokens: 3,
            ..Default::default()
        }));
        tot.accumulate(None);
        assert_eq!(tot.total_input_tokens, 300);
        assert_eq!(tot.total_output_tokens, 130);
        assert_eq!(tot.total_cached_input_tokens, 40);
        assert_eq!(tot.total_thinking_tokens, 10);
        assert_eq!(tot.task_count, 3);
    }

    #[test]
    fn sub_script_flatten_chain_collapses_legacy_nested_envelopes() {
        // Build the legacy 3-deep shape:
        //   SubScript{ name=A, child= SubScript{ name=B, child= SubScript{ name=C, child=Log("hi") } } }
        // Expected after `flatten_subscript_chain`:
        //   SubScript{
        //     name=C, parent_path=[A, B], child=Log("hi")
        //   }
        let leaf = EngineEvent::Log("hi".into());
        let depth3 = EngineEvent::SubScript {
            script_name: "C".into(),
            parent_task: "c_task".into(),
            parent_node_id: Some(3),
            attempt: None,
            parent_path: Vec::new(),
            child: Box::new(leaf),
        };
        let depth2 = EngineEvent::SubScript {
            script_name: "B".into(),
            parent_task: "b_task".into(),
            parent_node_id: Some(2),
            attempt: None,
            parent_path: Vec::new(),
            child: Box::new(depth3),
        };
        let legacy = EngineEvent::SubScript {
            script_name: "A".into(),
            parent_task: "a_task".into(),
            parent_node_id: Some(1),
            attempt: None,
            parent_path: Vec::new(),
            child: Box::new(depth2),
        };

        let flat = legacy.flatten_subscript_chain();
        match flat {
            EngineEvent::SubScript {
                script_name,
                parent_task,
                parent_node_id,
                parent_path,
                child,
                ..
            } => {
                // Innermost frame surfaces on the envelope itself.
                assert_eq!(script_name, "C");
                assert_eq!(parent_task, "c_task");
                assert_eq!(parent_node_id, Some(3));
                // Ancestor chain: [outermost A, then B].
                assert_eq!(parent_path.len(), 2);
                assert_eq!(parent_path[0].script_name, "A");
                assert_eq!(parent_path[0].parent_task, "a_task");
                assert_eq!(parent_path[0].parent_node_id, Some(1));
                assert_eq!(parent_path[1].script_name, "B");
                assert_eq!(parent_path[1].parent_task, "b_task");
                assert_eq!(parent_path[1].parent_node_id, Some(2));
                // Leaf is the original Log event.
                assert!(matches!(*child, EngineEvent::Log(ref s) if s == "hi"));
            }
            other => panic!("expected SubScript, got {other:?}"),
        }
    }

    #[test]
    fn sub_script_flatten_leaves_already_flat_envelopes_alone() {
        // A SubScript whose `child` is already a leaf event is the
        // canonical shape — no transformation needed.
        let evt = EngineEvent::SubScript {
            script_name: "child".into(),
            parent_task: "result".into(),
            parent_node_id: Some(7),
            attempt: Some(1),
            parent_path: Vec::new(),
            child: Box::new(EngineEvent::Log("hi".into())),
        };
        let flat = evt.flatten_subscript_chain();
        match flat {
            EngineEvent::SubScript {
                parent_path, child, ..
            } => {
                assert!(parent_path.is_empty());
                assert!(matches!(*child, EngineEvent::Log(_)));
            }
            other => panic!("expected SubScript, got {other:?}"),
        }
    }

    #[test]
    fn sub_script_flatten_no_op_for_non_subscript_events() {
        // Non-SubScript events pass through `flatten_subscript_chain`
        // unchanged.
        let evt = EngineEvent::Log("hi".into());
        let flat = evt.flatten_subscript_chain();
        assert!(matches!(flat, EngineEvent::Log(ref s) if s == "hi"));
    }

    #[test]
    fn task_end_event_value_type_none_serializes_null() {
        let e = EngineEvent::TaskEnd {
            task: "t".into(),
            on_error_label: None,
            value: Value::Null,
            value_type: None,
            duration: std::time::Duration::from_millis(5),
            attempt: 1,
            usage: None,
            variant: TaskEndVariant::Success,
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"attempt\":1"), "attempt should be 1: {}", s);
        // value_type: None serializes as null
        assert!(
            s.contains("\"value_type\":null"),
            "value_type should be null: {}",
            s
        );
    }

    #[test]
    fn task_end_event_without_variant_deserializes_as_success() {
        // Wire-compat guard: pre-#206 servers emit `TaskEnd` with no
        // `variant` field. `#[serde(default)]` must carry it to `Success`.
        //
        // The `value` field uses the clean wire form spec'd in
        // `docs/src/content/docs/reference/engine-events.mdx` — a bare JSON
        // value, not the engine's internal tagged `{"String": "x"}` shape.
        let json = r#"{
            "type": "TaskEnd",
            "payload": {
                "task": "t",
                "on_error_label": null,
                "value": "x",
                "value_type": null,
                "duration": {"secs": 0, "nanos": 10000000},
                "attempt": 1,
                "usage": null
            }
        }"#;
        let e: EngineEvent = serde_json::from_str(json).unwrap();
        match e {
            EngineEvent::TaskEnd { variant, .. } => {
                assert_eq!(variant, TaskEndVariant::Success);
            }
            _ => panic!("expected TaskEnd"),
        }
    }

    #[test]
    fn task_end_event_with_unable_variant_roundtrips() {
        let e = EngineEvent::TaskEnd {
            task: "decompose".into(),
            on_error_label: None,
            value: Value::Unable(crate::value::UnableRecord {
                reason: "image too blurry".into(),
                missing: vec!["claim_text".into()],
                category: crate::value::UnableCategory::InputAmbiguous,
            }),
            value_type: None,
            duration: std::time::Duration::from_millis(10),
            attempt: 1,
            usage: None,
            variant: TaskEndVariant::Unable,
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"variant\":\"unable\""), "{s}");
        let back: EngineEvent = serde_json::from_str(&s).unwrap();
        match back {
            EngineEvent::TaskEnd { variant, .. } => {
                assert_eq!(variant, TaskEndVariant::Unable);
            }
            _ => panic!("expected TaskEnd"),
        }
    }

    #[test]
    fn task_end_variant_unknown_discriminator_deserializes_as_unknown() {
        // Forward-compat: a newer engine adds (say) `partial` for #205 and
        // an older SDK must not crash. `#[serde(other)]` routes the unknown
        // tag to `TaskEndVariant::Unknown`.
        let json = r#"{
            "type": "TaskEnd",
            "payload": {
                "task": "t",
                "on_error_label": null,
                "value": null,
                "value_type": null,
                "duration": {"secs": 0, "nanos": 0},
                "attempt": 1,
                "usage": null,
                "variant": "partial"
            }
        }"#;
        let e: EngineEvent = serde_json::from_str(json).unwrap();
        match e {
            EngineEvent::TaskEnd { variant, .. } => {
                assert_eq!(variant, TaskEndVariant::Unknown);
            }
            _ => panic!("expected TaskEnd"),
        }
    }

    #[test]
    fn suspended_event_with_validation_exhausted_trigger_serializes() {
        let e = EngineEvent::Suspended {
            checkpoint_name: "human_review".into(),
            token: "tok".into(),
            prompt: "Please review".into(),
            schema: serde_json::json!({"type": "integer"}),
            actor_hint: ActorHint::Human,
            timeout_secs: Some(3600),
            trigger: SuspendTrigger::ValidationExhausted {
                task_name: "decompose_claims".into(),
                retry_count: 3,
                last_attempt: "{\"bad\": true}".into(),
                validation_errors: vec![ValidationErrorWire {
                    stage: "schema".into(),
                    message: "required property \"number\" missing".into(),
                    path: Some("/0".into()),
                }],
            },
            loop_context: None,
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"kind\":\"ValidationExhausted\""), "{s}");
        assert!(s.contains("\"retry_count\":3"), "{s}");
        assert!(s.contains("\"task_name\":\"decompose_claims\""), "{s}");
        assert!(s.contains("\"stage\":\"schema\""), "{s}");
        // `loop_context` is None on a top-level checkpoint; the field
        // skips serialization so older SDKs see the same wire shape.
        assert!(!s.contains("\"loop_context\""), "{s}");
    }

    #[test]
    fn suspended_event_with_loop_context_roundtrips() {
        // Mid-loop suspension carries a `loop_context` envelope that
        // SDK consumers use to render the suspension in the loop's UI
        // lane and that the spawn handler persists alongside the event.
        let e = EngineEvent::Suspended {
            checkpoint_name: "review".into(),
            token: "tok".into(),
            prompt: "Triage skill failure".into(),
            schema: serde_json::json!({}),
            actor_hint: ActorHint::Human,
            timeout_secs: None,
            trigger: SuspendTrigger::AgentUnable {
                task_name: "summarize".into(),
                unable: crate::value::UnableRecord {
                    reason: "input ambiguous".into(),
                    missing: vec![],
                    category: crate::value::UnableCategory::InputAmbiguous,
                },
            },
            loop_context: Some(LoopSuspendContext {
                loop_id: "11111111-2222-3333-4444-555555555555".into(),
                loop_name: "research".into(),
                turn: 2,
            }),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"loop_context\""), "{s}");
        assert!(s.contains("\"loop_name\":\"research\""), "{s}");
        assert!(s.contains("\"turn\":2"), "{s}");
        let back: EngineEvent = serde_json::from_str(&s).unwrap();
        match back {
            EngineEvent::Suspended {
                loop_context: Some(ctx),
                ..
            } => {
                assert_eq!(ctx.loop_name, "research");
                assert_eq!(ctx.turn, 2);
                assert_eq!(ctx.loop_id, "11111111-2222-3333-4444-555555555555");
            }
            _ => panic!("expected Suspended with loop_context"),
        }
    }

    #[test]
    fn suspended_event_without_trigger_deserializes_as_dag_position() {
        // Wire-compat guard: old servers / old SDKs omit `trigger`.
        // `#[serde(default)]` must carry the field to `DagPosition`.
        let json = r#"{
            "type": "Suspended",
            "payload": {
                "checkpoint_name": "cp",
                "token": "t",
                "prompt": "p",
                "schema": {},
                "actor_hint": "Human",
                "timeout_secs": null
            }
        }"#;
        let e: EngineEvent = serde_json::from_str(json).unwrap();
        match e {
            EngineEvent::Suspended { trigger, .. } => {
                assert!(matches!(trigger, SuspendTrigger::DagPosition));
            }
            _ => panic!("expected Suspended"),
        }
    }

    #[test]
    fn suspended_event_with_dag_position_trigger_roundtrips() {
        let e = EngineEvent::Suspended {
            checkpoint_name: "cp".into(),
            token: "t".into(),
            prompt: "p".into(),
            schema: serde_json::json!({}),
            actor_hint: ActorHint::Human,
            timeout_secs: None,
            trigger: SuspendTrigger::DagPosition,
            loop_context: None,
        };
        let s = serde_json::to_string(&e).unwrap();
        let back: EngineEvent = serde_json::from_str(&s).unwrap();
        match back {
            EngineEvent::Suspended { trigger, .. } => {
                assert!(matches!(trigger, SuspendTrigger::DagPosition));
            }
            _ => panic!("expected Suspended"),
        }
    }

    #[test]
    fn suspend_trigger_agent_variant_roundtrips_with_payload() {
        // #226 M5: non-Unable arm of a discriminated union routed to a
        // checkpoint emits AgentVariant, carrying the variant name and
        // the parsed record as JSON. Studio renders a generic
        // "agent returned variant <X>" badge on this trigger.
        let trigger = SuspendTrigger::AgentVariant {
            task_name: "decompose".into(),
            variant: "ClaimErr".into(),
            payload: serde_json::json!({
                "message": "claim unsupported by evidence",
                "claim_id": "c-7",
            }),
        };
        let s = serde_json::to_string(&trigger).unwrap();
        assert!(s.contains("\"kind\":\"AgentVariant\""), "{s}");
        assert!(s.contains("\"variant\":\"ClaimErr\""), "{s}");
        let back: SuspendTrigger = serde_json::from_str(&s).unwrap();
        match back {
            SuspendTrigger::AgentVariant {
                task_name,
                variant,
                payload,
            } => {
                assert_eq!(task_name, "decompose");
                assert_eq!(variant, "ClaimErr");
                assert_eq!(
                    payload["message"].as_str(),
                    Some("claim unsupported by evidence"),
                );
            }
            other => panic!("expected AgentVariant, got {other:?}"),
        }
    }

    #[test]
    fn validation_failure_event_serializes_full_payload() {
        // #320: Studio + tooling consume the structured fields. Round-trip
        // ensures missing/extra/type breakdowns and the stop_reason carry
        // across the wire without losing shape.
        let e = EngineEvent::ValidationFailure {
            task_name: "classify_features".into(),
            attempt: 2,
            model_response: "{}".into(),
            truncated: false,
            total_length: 2,
            missing_fields: vec!["/classifications".into()],
            extra_fields: vec![],
            type_errors: vec!["expected string, got null at /summary".into()],
            stop_reason: Some("max_tokens".into()),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"type\":\"ValidationFailure\""), "{s}");
        assert!(s.contains("\"task_name\":\"classify_features\""), "{s}");
        assert!(s.contains("\"attempt\":2"), "{s}");
        assert!(s.contains("\"model_response\":\"{}\""), "{s}");
        assert!(s.contains("\"/classifications\""), "{s}");
        assert!(s.contains("\"stop_reason\":\"max_tokens\""), "{s}");

        let back: EngineEvent = serde_json::from_str(&s).unwrap();
        match back {
            EngineEvent::ValidationFailure {
                task_name,
                attempt,
                model_response,
                missing_fields,
                extra_fields,
                type_errors,
                stop_reason,
                ..
            } => {
                assert_eq!(task_name, "classify_features");
                assert_eq!(attempt, 2);
                assert_eq!(model_response, "{}");
                assert_eq!(missing_fields, vec!["/classifications"]);
                assert!(extra_fields.is_empty());
                assert_eq!(type_errors.len(), 1);
                assert_eq!(stop_reason.as_deref(), Some("max_tokens"));
            }
            other => panic!("expected ValidationFailure, got {other:?}"),
        }
    }

    #[test]
    fn suspend_trigger_agent_unable_roundtrips_with_payload() {
        // Reserved variant — Stream 4 populates this in a follow-up. The
        // shape is locked here so Stream 4's engine-emit site is pure code
        // addition, no wire-shape churn.
        let trigger = SuspendTrigger::AgentUnable {
            task_name: "escalate".into(),
            unable: crate::value::UnableRecord {
                reason: "image too blurry to OCR".into(),
                missing: vec!["claim_text".into()],
                category: crate::value::UnableCategory::InputAmbiguous,
            },
        };
        let s = serde_json::to_string(&trigger).unwrap();
        assert!(s.contains("\"kind\":\"AgentUnable\""), "{s}");
        let back: SuspendTrigger = serde_json::from_str(&s).unwrap();
        match back {
            SuspendTrigger::AgentUnable { task_name, unable } => {
                assert_eq!(task_name, "escalate");
                assert_eq!(
                    unable.category,
                    crate::value::UnableCategory::InputAmbiguous
                );
            }
            other => panic!("expected AgentUnable, got {other:?}"),
        }
    }

    #[test]
    fn error_event_with_code_serializes_with_code_field() {
        let e = EngineEvent::error(crate::error::ErrorDetail::new(
            crate::error::ErrorCode::ScriptDepthExceeded,
            "boom",
        ));
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"code\":\"ScriptDepthExceeded\""), "{s}");
    }

    #[test]
    fn error_event_default_code_is_other_for_kind_only_construction() {
        // `error_kind` derives the code from the kind via
        // `ErrorDetail::from_kind`. ScriptError → ScriptError code.
        let e = EngineEvent::error_kind(crate::error::ErrorKind::ScriptError, "plain");
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"code\":\"ScriptError\""), "{s}");
    }

    #[test]
    fn context_compacted_event_roundtrips_full_payload() {
        let e = EngineEvent::ContextCompacted {
            agent: "researcher".into(),
            loop_id: Some("11111111-2222-3333-4444-555555555555".into()),
            turn: Some(3),
            threshold_pct: Some(70),
            threshold_abs: Some(140_000),
            strategy: "drop_thinking_blocks".into(),
            before_tokens: 142_000,
            after_tokens: 96_000,
            provider_native: false,
            cache_ttl: None,
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"type\":\"ContextCompacted\""), "{s}");
        assert!(s.contains("\"agent\":\"researcher\""), "{s}");
        assert!(s.contains("\"strategy\":\"drop_thinking_blocks\""), "{s}");
        assert!(s.contains("\"before_tokens\":142000"), "{s}");
        assert!(s.contains("\"after_tokens\":96000"), "{s}");
        assert!(s.contains("\"provider_native\":false"), "{s}");
        assert!(s.contains("\"turn\":3"), "{s}");

        let back: EngineEvent = serde_json::from_str(&s).unwrap();
        match back {
            EngineEvent::ContextCompacted {
                agent,
                loop_id,
                turn,
                threshold_pct,
                threshold_abs,
                strategy,
                before_tokens,
                after_tokens,
                provider_native,
                cache_ttl: _,
            } => {
                assert_eq!(agent, "researcher");
                assert_eq!(
                    loop_id.as_deref(),
                    Some("11111111-2222-3333-4444-555555555555")
                );
                assert_eq!(turn, Some(3));
                assert_eq!(threshold_pct, Some(70));
                assert_eq!(threshold_abs, Some(140_000));
                assert_eq!(strategy, "drop_thinking_blocks");
                assert_eq!(before_tokens, 142_000);
                assert_eq!(after_tokens, 96_000);
                assert!(!provider_native);
            }
            other => panic!("expected ContextCompacted, got {other:?}"),
        }
    }

    #[test]
    fn context_compacted_event_provider_native_roundtrips() {
        // Anthropic / OpenAI server-side compaction surfaces with
        // provider_native = true and the loop/turn context is absent
        // (compaction happens inside a provider call, not at a loop
        // boundary).
        let e = EngineEvent::ContextCompacted {
            agent: "summarizer".into(),
            loop_id: None,
            turn: None,
            threshold_pct: None,
            threshold_abs: None,
            strategy: "provider_native".into(),
            before_tokens: 180_000,
            after_tokens: 42_000,
            provider_native: true,
            cache_ttl: Some("1h".to_string()),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"provider_native\":true"), "{s}");
        assert!(s.contains("\"strategy\":\"provider_native\""), "{s}");
        let back: EngineEvent = serde_json::from_str(&s).unwrap();
        match back {
            EngineEvent::ContextCompacted {
                provider_native,
                loop_id,
                turn,
                ..
            } => {
                assert!(provider_native);
                assert!(loop_id.is_none());
                assert!(turn.is_none());
            }
            other => panic!("expected ContextCompacted, got {other:?}"),
        }
    }

    #[test]
    fn context_overflow_event_roundtrips_full_payload() {
        let e = EngineEvent::ContextOverflow {
            agent: "researcher".into(),
            attempted_strategies: vec![
                "drop_thinking_blocks".into(),
                "drop_oldest_tool_results".into(),
                "summarize_to_state".into(),
            ],
            configured_cap_tokens: 200_000,
            model_context_window: 200_000,
            terminated_by_hard_error: false,
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"type\":\"ContextOverflow\""), "{s}");
        assert!(s.contains("\"agent\":\"researcher\""), "{s}");
        assert!(s.contains("\"configured_cap_tokens\":200000"), "{s}");
        assert!(s.contains("\"model_context_window\":200000"), "{s}");
        assert!(s.contains("\"drop_thinking_blocks\""), "{s}");

        let back: EngineEvent = serde_json::from_str(&s).unwrap();
        match back {
            EngineEvent::ContextOverflow {
                agent,
                attempted_strategies,
                configured_cap_tokens,
                model_context_window,
                terminated_by_hard_error,
            } => {
                assert_eq!(agent, "researcher");
                assert_eq!(attempted_strategies.len(), 3);
                assert_eq!(attempted_strategies[0], "drop_thinking_blocks");
                assert_eq!(configured_cap_tokens, 200_000);
                assert_eq!(model_context_window, 200_000);
                assert!(!terminated_by_hard_error);
            }
            other => panic!("expected ContextOverflow, got {other:?}"),
        }
    }

    #[test]
    fn runtime_start_serializes_with_expected_fields() {
        let ev = EngineEvent::RuntimeStart {
            task_name: "analyze".into(),
            runtime_name: "run_python".into(),
            language: "python".into(),
        };
        let j = serde_json::to_value(&ev).unwrap();
        assert_eq!(j["type"], "RuntimeStart");
        assert_eq!(j["payload"]["task_name"], "analyze");
        assert_eq!(j["payload"]["runtime_name"], "run_python");
        assert_eq!(j["payload"]["language"], "python");

        let s = serde_json::to_string(&ev).unwrap();
        let back: EngineEvent = serde_json::from_str(&s).unwrap();
        match back {
            EngineEvent::RuntimeStart {
                task_name,
                runtime_name,
                language,
            } => {
                assert_eq!(task_name, "analyze");
                assert_eq!(runtime_name, "run_python");
                assert_eq!(language, "python");
            }
            other => panic!("expected RuntimeStart, got {other:?}"),
        }
    }

    #[test]
    fn runtime_stdout_stderr_roundtrip() {
        let stdout = EngineEvent::RuntimeStdout {
            task_name: "t".into(),
            chunk: "hello\n".into(),
        };
        let stderr = EngineEvent::RuntimeStderr {
            task_name: "t".into(),
            chunk: "warn\n".into(),
        };
        let j_out = serde_json::to_value(&stdout).unwrap();
        let j_err = serde_json::to_value(&stderr).unwrap();
        assert_eq!(j_out["type"], "RuntimeStdout");
        assert_eq!(j_err["type"], "RuntimeStderr");
        assert_eq!(j_out["payload"]["chunk"], "hello\n");
        assert_eq!(j_err["payload"]["chunk"], "warn\n");
    }

    #[test]
    fn runtime_end_serializes_with_exit_code_and_duration() {
        let ev = EngineEvent::RuntimeEnd {
            task_name: "analyze".into(),
            exit_code: 0,
            duration_ms: 1234,
        };
        let j = serde_json::to_value(&ev).unwrap();
        assert_eq!(j["type"], "RuntimeEnd");
        assert_eq!(j["payload"]["exit_code"], 0);
        assert_eq!(j["payload"]["duration_ms"], 1234);

        let back: EngineEvent = serde_json::from_value(j).unwrap();
        match back {
            EngineEvent::RuntimeEnd {
                exit_code,
                duration_ms,
                ..
            } => {
                assert_eq!(exit_code, 0);
                assert_eq!(duration_ms, 1234);
            }
            other => panic!("expected RuntimeEnd, got {other:?}"),
        }
    }

    #[test]
    fn runtime_error_serializes_with_kind_and_message() {
        let ev = EngineEvent::RuntimeError {
            task_name: "t".into(),
            kind: "Timeout".into(),
            message: "exceeded 30s".into(),
        };
        let j = serde_json::to_value(&ev).unwrap();
        assert_eq!(j["type"], "RuntimeError");
        assert_eq!(j["payload"]["kind"], "Timeout");
        assert_eq!(j["payload"]["message"], "exceeded 30s");
    }

    #[test]
    fn task_cache_hit_serializes_with_agent_and_key_prefix() {
        // P3: when `PersistentTaskCache::get` returns a hit, the engine
        // emits this event so trace inspectors / Studio / MCP can show
        // "stage N was cached" without parsing prompt-segment internals.
        let ev = EngineEvent::TaskCacheHit {
            agent: "Researcher".into(),
            key_prefix: "f7d3a9".into(),
        };
        let j = serde_json::to_value(&ev).unwrap();
        assert_eq!(j["type"], "TaskCacheHit");
        assert_eq!(j["payload"]["agent"], "Researcher");
        assert_eq!(j["payload"]["key_prefix"], "f7d3a9");

        // Round-trip
        let s = serde_json::to_string(&ev).unwrap();
        let back: EngineEvent = serde_json::from_str(&s).unwrap();
        match back {
            EngineEvent::TaskCacheHit { agent, key_prefix } => {
                assert_eq!(agent, "Researcher");
                assert_eq!(key_prefix, "f7d3a9");
            }
            other => panic!("expected TaskCacheHit, got {other:?}"),
        }
    }

    #[test]
    fn validation_failure_caps_oversized_response() {
        // Issue #1139: emit-time cap prevents megabyte tool outputs
        // from bloating execution_events. A response larger than the
        // cap is truncated, `truncated=true`, and `total_length`
        // preserves the original byte count.
        let huge = "x".repeat(VALIDATION_FAILURE_RESPONSE_CAP_BYTES + 1024);
        let ev = EngineEvent::validation_failure(
            "t".to_string(),
            1,
            huge.clone(),
            vec![],
            vec![],
            vec![],
            None,
        );
        match ev {
            EngineEvent::ValidationFailure {
                model_response,
                truncated,
                total_length,
                ..
            } => {
                assert!(truncated, "should be truncated");
                assert_eq!(total_length, huge.len() as u64);
                assert!(model_response.len() <= VALIDATION_FAILURE_RESPONSE_CAP_BYTES);
            }
            other => panic!("expected ValidationFailure, got {other:?}"),
        }
    }

    #[test]
    fn validation_failure_under_cap_is_unchanged() {
        let body = "{\"k\": \"v\"}".to_string();
        let ev = EngineEvent::validation_failure(
            "t".to_string(),
            1,
            body.clone(),
            vec![],
            vec![],
            vec![],
            None,
        );
        match ev {
            EngineEvent::ValidationFailure {
                model_response,
                truncated,
                total_length,
                ..
            } => {
                assert!(!truncated);
                assert_eq!(total_length, body.len() as u64);
                assert_eq!(model_response, body);
            }
            other => panic!("expected ValidationFailure, got {other:?}"),
        }
    }

    #[test]
    fn tool_approval_resolved_round_trips() {
        // Issue #857: audit trail companion to ToolApprovalPending.
        let ev = EngineEvent::ToolApprovalResolved {
            token: "tok_abc".into(),
            approved: true,
            args_override: Some(serde_json::json!({"safe": true})),
            reason: Some("operator approved".into()),
        };
        let s = serde_json::to_string(&ev).unwrap();
        let back: EngineEvent = serde_json::from_str(&s).unwrap();
        match back {
            EngineEvent::ToolApprovalResolved {
                token,
                approved,
                reason,
                ..
            } => {
                assert_eq!(token, "tok_abc");
                assert!(approved);
                assert_eq!(reason.as_deref(), Some("operator approved"));
            }
            other => panic!("expected ToolApprovalResolved, got {other:?}"),
        }
    }

    #[test]
    fn tool_approval_skipped_round_trips() {
        // Issue #1110: auto-approval audit gap closer.
        let ev = EngineEvent::ToolApprovalSkipped {
            execution_id: Some("exec_1".into()),
            node_id: Some(7),
            tool_ref: "gh.list_issues".into(),
            reason: "policy:read_only".into(),
        };
        let s = serde_json::to_string(&ev).unwrap();
        let back: EngineEvent = serde_json::from_str(&s).unwrap();
        match back {
            EngineEvent::ToolApprovalSkipped {
                tool_ref, reason, ..
            } => {
                assert_eq!(tool_ref, "gh.list_issues");
                assert_eq!(reason, "policy:read_only");
            }
            other => panic!("expected ToolApprovalSkipped, got {other:?}"),
        }
    }

    #[test]
    fn tool_replay_uncertain_round_trips() {
        // Issue #872: structured event for durable-replay tool ambiguity.
        let args = serde_json::json!({"channel": "general", "text": "hi"});
        let ev = EngineEvent::ToolReplayUncertain {
            execution_id: Some("exec_42".into()),
            tool_use_id: "tu_abc".into(),
            tool_name: "send_message".into(),
            args: args.clone(),
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"type\":\"ToolReplayUncertain\""), "{s}");
        let back: EngineEvent = serde_json::from_str(&s).unwrap();
        match back {
            EngineEvent::ToolReplayUncertain {
                execution_id,
                tool_use_id,
                tool_name,
                args: a,
            } => {
                assert_eq!(execution_id.as_deref(), Some("exec_42"));
                assert_eq!(tool_use_id, "tu_abc");
                assert_eq!(tool_name, "send_message");
                assert_eq!(a, args);
            }
            other => panic!("expected ToolReplayUncertain, got {other:?}"),
        }
    }

    #[test]
    fn llm_replay_cache_hit_round_trips() {
        // Issue #815: explicit replay-cache-hit signal for SDK consumers.
        let ev = EngineEvent::LLMReplayCacheHit {
            node_id: "n42".into(),
            call_index: 3,
        };
        let s = serde_json::to_string(&ev).unwrap();
        let back: EngineEvent = serde_json::from_str(&s).unwrap();
        match back {
            EngineEvent::LLMReplayCacheHit {
                node_id,
                call_index,
            } => {
                assert_eq!(node_id, "n42");
                assert_eq!(call_index, 3);
            }
            other => panic!("expected LLMReplayCacheHit, got {other:?}"),
        }
    }

    #[test]
    fn loop_turn_carries_usage_when_present_and_omits_when_none() {
        // Issue #829: per-turn token usage on LoopTurn lets consumers
        // skip walking the LLMResponse sub-tree for per-turn cost.
        let ev = EngineEvent::LoopTurn {
            name: "review".into(),
            turn: 4,
            tool_calls: vec!["fetch".into()],
            usage: Some(TokenUsage {
                input_tokens: 100,
                output_tokens: 25,
                model: "claude".into(),
                provider: "anthropic".into(),
                ..Default::default()
            }),
        };
        let s = serde_json::to_string(&ev).unwrap();
        let back: EngineEvent = serde_json::from_str(&s).unwrap();
        match back {
            EngineEvent::LoopTurn { usage, .. } => {
                let u = usage.expect("usage present");
                assert_eq!(u.input_tokens, 100);
                assert_eq!(u.output_tokens, 25);
            }
            other => panic!("expected LoopTurn, got {other:?}"),
        }
        // `None` usage omits the field from the wire shape.
        let ev2 = EngineEvent::LoopTurn {
            name: "review".into(),
            turn: 1,
            tool_calls: vec![],
            usage: None,
        };
        let s2 = serde_json::to_string(&ev2).unwrap();
        assert!(
            !s2.contains("\"usage\""),
            "usage should be skipped when None: {s2}"
        );
    }

    #[test]
    fn sub_script_carries_parent_node_id_and_attempt() {
        // Issue #845: parent retry attribution on SubScript envelopes.
        let inner = EngineEvent::Log("hi".into());
        let ev = EngineEvent::SubScript {
            script_name: "child".into(),
            parent_task: "result".into(),
            parent_node_id: Some(13),
            attempt: Some(2),
            parent_path: Vec::new(),
            child: Box::new(inner),
        };
        let s = serde_json::to_string(&ev).unwrap();
        let back: EngineEvent = serde_json::from_str(&s).unwrap();
        match back {
            EngineEvent::SubScript {
                parent_node_id,
                attempt,
                ..
            } => {
                assert_eq!(parent_node_id, Some(13));
                assert_eq!(attempt, Some(2));
            }
            other => panic!("expected SubScript, got {other:?}"),
        }
    }

    #[test]
    fn sub_script_back_compat_omits_new_fields_when_none() {
        // The new fields default to None and are skipped from the
        // wire shape when unset — preserving wire compat for pre-#845
        // payloads.
        let inner = EngineEvent::Log("hi".into());
        let ev = EngineEvent::SubScript {
            script_name: "child".into(),
            parent_task: "result".into(),
            parent_node_id: None,
            attempt: None,
            parent_path: Vec::new(),
            child: Box::new(inner),
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(
            !s.contains("\"parent_node_id\""),
            "should be omitted when None: {s}"
        );
        assert!(
            !s.contains("\"attempt\""),
            "should be omitted when None: {s}"
        );
    }

    #[test]
    fn token_usage_raw_stop_reason_round_trips() {
        // Issue #1077: `raw_stop_reason` is a new field that mirrors
        // `stop_reason` when the parse path produced the usage. It must
        // serialize alongside the canonical field and round-trip with
        // serde-default for back-compat.
        let u = TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            model: "claude-sonnet-4-6".into(),
            provider: "anthropic".into(),
            stop_reason: Some("end_turn".into()),
            raw_stop_reason: Some("end_turn".into()),
            ..Default::default()
        };
        let s = serde_json::to_string(&u).unwrap();
        assert!(s.contains("\"raw_stop_reason\":\"end_turn\""), "{s}");
        let back: TokenUsage = serde_json::from_str(&s).unwrap();
        assert_eq!(back.raw_stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(back.stop_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn token_usage_raw_stop_reason_back_compat_pre_field() {
        // A wire payload predating #1077 omits `raw_stop_reason` entirely.
        // It must decode as `None` rather than failing.
        let json = r#"{
            "input_tokens": 10,
            "output_tokens": 5,
            "model": "m",
            "provider": "p",
            "cached_input_tokens": 0,
            "stop_reason": "stop"
        }"#;
        let u: TokenUsage = serde_json::from_str(json).unwrap();
        assert_eq!(u.raw_stop_reason, None);
        assert_eq!(u.stop_reason.as_deref(), Some("stop"));
    }
}
