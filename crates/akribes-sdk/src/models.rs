/// Data types returned by and sent to the Akribes API.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Re-export AST types referenced by SDK wire models so consumers can pattern-
// match on the response without depending on `akribes-core` directly.
pub use akribes_types::ast::{TypeField, TypeRef};

// ── Core resources ───────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Script {
    pub id: i64,
    pub project_id: i64,
    pub name: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ScriptVersion {
    pub id: i64,
    pub script_id: i64,
    pub source: String,
    pub label: Option<String>,
    pub published_by: Option<String>,
    pub created_at: String,
}

/// Internal wrapper for the publish endpoint's response shape.
#[derive(Deserialize, Clone, Debug)]
pub(crate) struct PublishResponse {
    pub version: ScriptVersion,
    /// Per-kind counts of dependents that got implicitly contract-rebased
    /// on first publish (the server skips the unified contract check when
    /// no channel is pinned yet — see handlers/versions.rs). `None` for
    /// subsequent publishes, where the check ran for real and any breaks
    /// would have surfaced as a 409 ContractBreak instead. Surfaced
    /// upward through `PublishOutcome` so MCP and SDK consumers can show
    /// "your first publish implicitly rebased N bench cases + M judges".
    #[serde(default)]
    pub rebased: Option<Vec<RebaseEntry>>,
}

/// One row of the `rebased` array on a first-publish response.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RebaseEntry {
    pub kind: String,
    pub count: usize,
}

/// User-facing publish outcome — version plus the optional rebase summary.
/// Returned by `publish().execute()`; the historical `ScriptVersion`-only
/// return shape is preserved by `execute_version_only()` for callers that
/// don't care about the rebase signal.
#[derive(Clone, Debug)]
pub struct PublishOutcome {
    pub version: ScriptVersion,
    pub rebased: Option<Vec<RebaseEntry>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ScriptChannel {
    pub id: i64,
    pub script_id: i64,
    pub name: String,
    pub version_id: Option<i64>,
    pub updated_at: Option<String>,
}

/// A script draft with its parsed input definitions.
///
/// `inputs` is a list of `(name, type_display)` pairs. The server sends
/// these as structured `{name, ty, docs}` objects — the SDK normalizes
/// that plus the legacy `[[name, type], …]` tuple form into simple
/// `(name, display_string)` pairs. `type_defs` keeps the server's raw
/// custom-type block so new fields don't require an SDK bump.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Draft {
    pub source: String,
    #[serde(deserialize_with = "draft_de::deserialize_inputs")]
    pub inputs: Vec<(String, String)>,
    #[serde(default)]
    pub type_defs: serde_json::Value,
}

mod draft_de {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer};

    /// Accept either `[[name, ty_string], ...]` (legacy / tests) or
    /// `[{name, ty, docs}, ...]` (current server). `ty` may itself be
    /// either a string or a `TypeRef`-shaped object — render both to a
    /// source-level display string so downstream code can keep treating
    /// the type as a plain name.
    pub(super) fn deserialize_inputs<'de, D>(d: D) -> Result<Vec<(String, String)>, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum InputEntry {
            Tuple(String, String),
            Object(InputObject),
        }

        #[derive(Deserialize)]
        struct InputObject {
            name: String,
            #[serde(alias = "type")]
            ty: serde_json::Value,
            #[serde(default)]
            #[allow(dead_code)]
            docs: Option<String>,
        }

        let raw: Vec<InputEntry> = Vec::deserialize(d)?;
        raw.into_iter()
            .map(|e| match e {
                InputEntry::Tuple(name, ty) => Ok((name, ty)),
                InputEntry::Object(o) => {
                    let display = type_display(&o.ty).ok_or_else(|| {
                        D::Error::custom(format!(
                            "input '{}' has unexpected `ty` shape: {}",
                            o.name, o.ty
                        ))
                    })?;
                    Ok((o.name, display))
                }
            })
            .collect()
    }

    /// Render a `TypeRef`-shaped JSON payload (or a plain string) as a
    /// source-level type fragment. Mirrors `akribes_types::ast::TypeRef::display`
    /// but operates on raw JSON so we don't have to track every AST rename.
    fn type_display(v: &serde_json::Value) -> Option<String> {
        if let Some(s) = v.as_str() {
            return Some(s.to_string());
        }
        let obj = v.as_object()?;
        if let Some(arr) = obj.get("variants").and_then(|v| v.as_array()) {
            let arms: Vec<String> = arr.iter().filter_map(type_display).collect();
            if arms.len() == arr.len() {
                return Some(arms.join(" | "));
            }
        }
        if let Some(arr) = obj.get("choices").and_then(|v| v.as_array()) {
            let arms: Vec<String> = arr
                .iter()
                .filter_map(|c| c.as_str().map(|s| format!("\"{s}\"")))
                .collect();
            if arms.len() == arr.len() {
                return Some(arms.join(" | "));
            }
        }
        let name = obj.get("name").and_then(|n| n.as_str())?;
        if let Some(inner) = obj
            .get("inner")
            .and_then(|i| if i.is_null() { None } else { Some(i) })
        {
            if let Some(inner_display) = type_display(inner) {
                return Some(format!("{name}[{inner_display}]"));
            }
        }
        Some(name.to_string())
    }
}

/// A script version with its parsed input definitions.
/// Returned by the `/latest` endpoint.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LatestVersion {
    pub id: i64,
    pub script_id: i64,
    pub source: String,
    pub label: Option<String>,
    pub published_by: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub inputs: Vec<(String, String)>,
}

// ── Document conversion ─────────────────────────────────────────────────────

/// Result from the `/convert` endpoint.
///
/// `document_id` is populated when akribes-server has S3 persistence
/// configured (the default in prod). Callers that want to re-use the
/// uploaded file as a `document`-typed input on a subsequent `/run`
/// call should pass this id instead of the converted markdown.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ConvertResult {
    pub markdown: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
}

/// S3 document reference — either a pre-signed URL or bucket/key with temp credentials.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(untagged)]
pub enum S3DocumentRef {
    /// Pre-signed URL — server just fetches it.
    Presigned { presigned_url: String },
    /// Bucket + key with temporary credentials.
    Credentials {
        bucket: String,
        key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        region: Option<String>,
        access_key_id: String,
        secret_access_key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_token: Option<String>,
    },
}

// ── Document references ─────────────────────────────────────────────────────

/// A document reference returned when S3 persistence is active.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DocumentRef {
    pub document_id: String,
    pub filename: String,
}

/// Full document metadata returned by `GET /documents/{id}`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DocumentMeta {
    pub id: String,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub content_hash: String,
    pub conversion_status: String,
    pub conversion_error: Option<String>,
    pub created_at: String,
}

// ── Execution ────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RunResult {
    pub execution_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ExecutionStatus {
    pub id: String,
    pub project_id: i64,
    pub script_name: String,
    pub status: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub version_id: Option<i64>,
    pub channel: Option<String>,
    pub error: Option<String>,
    pub error_kind: Option<String>,
    pub result: Option<serde_json::Value>,
    pub documents: Option<serde_json::Value>,
    pub triggered_by: Option<String>,
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    /// Tokens consumed by tool-response payloads (task 39b).
    #[serde(default)]
    pub tool_tokens: i64,
    pub cost_usd: Option<f64>,
    /// Workflow's declared return [`TypeRef`], when statically resolvable
    /// from the source the execution ran against. Lets clients dispatch
    /// directly into a typed renderer (e.g. `list[Patent]` → typed table)
    /// instead of inferring shape from the raw value. `None` when the
    /// server couldn't determine the type (older servers, unparseable
    /// source, workflows without an explicit terminal `return <call>(...)`).
    #[serde(default)]
    pub result_type: Option<TypeRef>,
    /// Declared record types from the source the execution ran against,
    /// keyed by `type Name:` identifier (#1172). Lets clients render
    /// results back to their declared shape (named records, typed columns)
    /// instead of falling through to JSON shape inference. `None` from
    /// older servers; an empty map when the source couldn't be parsed.
    #[serde(default)]
    pub type_defs: Option<serde_json::Value>,
    /// ID of the parent execution that spawned this one via
    /// `spawn_child_execution`. `None` for top-level executions.
    #[serde(default)]
    pub parent_execution_id: Option<String>,
    /// The node ID within the parent execution at which this child was
    /// spawned. `None` when `parent_execution_id` is `None`.
    #[serde(default)]
    pub parent_node_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ExecutionOutput {
    pub status: String,
    pub error: Option<String>,
    pub error_kind: Option<String>,
    pub result: Option<serde_json::Value>,
}

/// Summary of a child execution spawned via `spawn_child_execution` (#1054).
/// Returned by `GET /executions/{id}/children`. For v1 the parent-linkage
/// columns are typically NULL; this type is forward-looking.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ExecutionChildSummary {
    pub id: String,
    pub parent_node_id: Option<String>,
    pub status: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub script_name: String,
}

// ── Engine events ────────────────────────────────────────────────────────────
//
// The raw wire-format event is now re-exported from `akribes-core`. The SDK's
// partial 15-variant enum has been removed in favour of that re-export plus
// the normalized [`crate::events::WorkflowEvent`] for client-friendly
// consumption.

pub use akribes_types::event::{EngineEvent, TokenUsage};

/// Helper: the variant name of an [`EngineEvent`] as emitted on the wire.
/// Used by [`crate::events::WorkflowEvent`] to tag catch-all variants.
pub(crate) fn engine_event_type_name(evt: &EngineEvent) -> &'static str {
    match evt {
        EngineEvent::Log(_) => "Log",
        EngineEvent::LogLevel { .. } => "LogLevel",
        EngineEvent::StateUpdate(..) => "StateUpdate",
        EngineEvent::WorkflowStart(_) => "WorkflowStart",
        EngineEvent::TaskStart(..) => "TaskStart",
        EngineEvent::TaskPrompt(..) => "TaskPrompt",
        EngineEvent::TaskEnd { .. } => "TaskEnd",
        EngineEvent::AgentOutput { .. } => "AgentOutput",
        EngineEvent::AgentReasoning { .. } => "AgentReasoning",
        EngineEvent::Suspended { .. } => "Suspended",
        EngineEvent::Resumed { .. } => "Resumed",
        EngineEvent::WorkflowEnd(akribes_types::event::WorkflowEndPayload { value: _, .. }) => "WorkflowEnd",
        EngineEvent::Error { .. } => "Error",
        EngineEvent::NodeStart(..) => "NodeStart",
        EngineEvent::NodeEnd { .. } => "NodeEnd",
        EngineEvent::Breakpoint { .. } => "Breakpoint",
        EngineEvent::BreakpointResumed { .. } => "BreakpointResumed",
        EngineEvent::ToolCallStart { .. } => "ToolCallStart",
        EngineEvent::ToolCallEnd { .. } => "ToolCallEnd",
        EngineEvent::McpServerDegraded { .. } => "McpServerDegraded",
        EngineEvent::McpServerRecovered { .. } => "McpServerRecovered",
        EngineEvent::ToolApprovalPending { .. } => "ToolApprovalPending",
        EngineEvent::ToolApprovalResolved { .. } => "ToolApprovalResolved",
        EngineEvent::ToolApprovalSkipped { .. } => "ToolApprovalSkipped",
        EngineEvent::ToolReplayUncertain { .. } => "ToolReplayUncertain",
        EngineEvent::LLMReplayCacheHit { .. } => "LLMReplayCacheHit",
        EngineEvent::VerificationStart { .. } => "VerificationStart",
        EngineEvent::VerificationResult { .. } => "VerificationResult",
        EngineEvent::ValidationFailure { .. } => "ValidationFailure",
        EngineEvent::SubScript { .. } => "SubScript",
        EngineEvent::CachePlanned { .. } => "CachePlanned",
        EngineEvent::LoopStart { .. } => "LoopStart",
        EngineEvent::LoopTurn { .. } => "LoopTurn",
        EngineEvent::LoopEnd { .. } => "LoopEnd",
        EngineEvent::ContextCompacted { .. } => "ContextCompacted",
        EngineEvent::ContextOverflow { .. } => "ContextOverflow",
        // P3 telemetry: persistent task-cache hit (per-task `cache_control`
        // hit served from `task_cache_entries`). Carried on the wire so
        // the Studio + bench can attribute "this task was free this run".
        EngineEvent::TaskCacheHit { .. } => "TaskCacheHit",
        EngineEvent::LLMResponse { .. } => "LLMResponse",
        EngineEvent::SubScriptSpawned { .. } => "SubScriptSpawned",
        EngineEvent::SubScriptResult { .. } => "SubScriptResult",
        EngineEvent::CheckpointResolution { .. } => "CheckpointResolution",
        // FIXME(unit-6): The Rust SDK's typed `WorkflowEvent` arms for
        // container code-execution events land in unit 6 of the
        // "AI-driven container code execution" feature. Unit 3 (engine
        // wiring) lands the engine-side variants; unit 6 will replace
        // these stubs with typed `Runtime*` arms on `WorkflowEvent` and
        // matching reducer logic so SDK consumers don't see them as
        // `Other` first. Today they round-trip through the catch-all
        // path in `events.rs` (`other => Self::Other { ... }`).
        EngineEvent::RuntimeStart { .. } => "RuntimeStart",
        EngineEvent::RuntimeStdout { .. } => "RuntimeStdout",
        EngineEvent::RuntimeStderr { .. } => "RuntimeStderr",
        EngineEvent::RuntimeEnd { .. } => "RuntimeEnd",
        EngineEvent::RuntimeError { .. } => "RuntimeError",
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ExecutionEvents {
    pub execution_id: String,
    pub status: String,
    /// `false` while the execution is still running (snapshot of events so far).
    /// `true` once the execution has reached a terminal state.
    pub complete: bool,
    pub events: Vec<EngineEvent>,
    /// Cursor for fetching the next page of events.
    #[serde(default)]
    pub next_after_id: Option<i64>,
    /// Whether more events are available beyond this page.
    #[serde(default)]
    pub has_more: bool,
}

// ── Cost aggregation ────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VersionCost {
    pub version_id: Option<i64>,
    pub executions: i64,
    pub avg_cost_usd: f64,
    pub total_cost_usd: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProjectCost {
    pub project_id: i64,
    pub total_executions: i64,
    pub total_cost_usd: f64,
    pub avg_cost_usd: f64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CostAggregation {
    pub total_executions: i64,
    pub total_cost_usd: f64,
    pub avg_cost_usd: f64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    #[serde(default)]
    pub total_tool_tokens: i64,
    #[serde(default)]
    pub by_version: Vec<VersionCost>,
}

/// Canonical, cross-SDK name for [`CostAggregation`] (#1193). TS exposes the
/// same shape as `ScriptCost`; Python re-exports both. New Rust code should
/// prefer this alias so the type name matches the other SDKs. The legacy
/// `CostAggregation` name is kept for back-compat — both refer to the same
/// type.
pub type ScriptCost = CostAggregation;

// ── Graph ───────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GraphNode {
    pub id: usize,
    pub op_type: String,
    pub op_name: Option<String>,
    pub target_var: Option<String>,
    pub reads: Vec<String>,
    pub line: usize,
    pub col: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GraphEdge {
    pub from: usize,
    pub to: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GraphResponse {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// Cross-SDK alias for [`GraphResponse`] (#1189). TS calls the same shape
/// `ScriptGraph`; Python re-exports both. New Rust code should prefer this
/// name when interop matters. The legacy `GraphResponse` is kept for
/// back-compat — both refer to the same type.
pub type ScriptGraph = GraphResponse;

// ── Hub events (SSE) ─────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", content = "payload")]
pub enum RegistryEvent {
    ProjectCreated(Project),
    ProjectUpdated(Project),
    ProjectDeleted(i64),
    ScriptCreated {
        project_id: i64,
        script: Script,
    },
    ScriptUpdated {
        project_id: i64,
        script_name: String,
        version_id: i64,
        #[serde(default)]
        channel: Option<String>,
    },
    ScriptDeleted {
        project_id: i64,
        script_name: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", content = "payload")]
pub enum EvalEvent {
    RunStarted {
        project_id: i64,
        script_name: String,
        run: EvalRun,
    },
    RunProgress {
        project_id: i64,
        script_name: String,
        run_id: i64,
        completed_cases: i32,
        total_cases: Option<i32>,
        average_score: Option<f64>,
        latest_result: Option<EvalCaseReport>,
    },
    RunFinished {
        project_id: i64,
        script_name: String,
        run: EvalRun,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", content = "payload")]
pub enum HubEvent {
    Execution {
        project_id: i64,
        script_name: String,
        /// The execution row's id. Lets subscribers filter out events
        /// from a different concurrent run of the same script — without
        /// it, two callers running the same script around the same time
        /// see each other's events. Optional on the wire for back-compat
        /// with older servers that predate the field (#1042 / TS SDK
        /// `HubEvent.payload.execution_id`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
        event: EngineEvent,
        /// Monotonic per-execution sequence number from the
        /// `execution_events.id` row that this broadcast accompanies.
        /// Optional on the wire — older subscribers ignore it; missing
        /// on reconnect-replay frames where we don't have a row yet.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<i64>,
        /// Server-side RFC3339 timestamp with ms precision. Same value
        /// the REST `get_execution_events` endpoint stamps on each event.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<String>,
    },
    Registry(RegistryEvent),
    Eval(EvalEvent),
}

// ── Draft response ──────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PutDraftResponse {
    #[serde(default)]
    pub schema_warnings: Vec<ContractWarning>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ContractWarning {
    pub client_id: String,
    pub client_name: String,
    pub channel: String,
    pub mismatch: SchemaMismatch,
}

// ── Publish dry-run ─────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DryRunResult {
    pub dry_run: bool,
    pub would_break: i64,
    pub breaking_interests: Vec<BreakingInterest>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BreakingInterest {
    pub client_id: String,
    pub client_name: String,
    pub channel: String,
    pub lifetime: String,
    pub mismatch: SchemaMismatch,
}

// ── Client registration ──────────────────────────────────────────────────────

/// Info about a registered client, returned by `GET /projects/{id}/clients`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ClientInfo {
    pub id: String,
    pub name: String,
    pub last_seen: String,
    #[serde(default)]
    pub scripts: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ClientInterest {
    pub script_name: String,
    pub inputs: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifetime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RegisteredInterest {
    pub script_name: String,
    pub channel: String,
    pub bound_version_id: Option<i64>,
    #[serde(default)]
    pub input_schema: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RegisterClientResponse {
    #[serde(default)]
    pub interests: Vec<RegisteredInterest>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SchemaMismatch {
    #[serde(default)]
    pub missing: Vec<(String, String)>,
    #[serde(default)]
    pub wrong_type: Vec<(String, String, String)>,
    #[serde(default)]
    pub extra: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ContractLockInfo {
    pub id: i64,
    pub client_id: String,
    pub client_name: String,
    pub script_name: String,
    pub channel: String,
    pub bound_version_id: Option<i64>,
    pub lifetime: String,
    pub drifted: bool,
    pub created_by: Option<String>,
    pub created_at: String,
    pub input_schema: String,
}

// ── Scoped tokens ───────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TokenScopes {
    pub projects: ProjectScope,
    pub role: TokenRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scripts: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executions: Option<Vec<String>>,
    /// Whether the new token may itself mint child tokens. Defaults to
    /// `false`. Service tokens always pass; scoped minters must already have
    /// `can_mint` set on their own scopes for this to be honored.
    #[serde(default)]
    pub can_mint: bool,
    /// Feature flags granted to this token (e.g. `["lumen"]`). Empty by
    /// default. Service tokens have all features unless explicitly restricted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
    /// Optional org binding. Studio populates this on every per-user mint so
    /// akribes-server can stamp `projects.organization_id` and enforce
    /// `OrgWide` scope checks. Legacy CLI mints leave it `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(untagged)]
pub enum ProjectScope {
    Wildcard(WildcardMarker),
    Specific(Vec<i64>),
}

/// Represents the `"*"` wildcard for project scope.
#[derive(Clone, Debug)]
pub struct WildcardMarker;

impl Serialize for WildcardMarker {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str("*")
    }
}

impl<'de> Deserialize<'de> for WildcardMarker {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        if s == "*" {
            Ok(WildcardMarker)
        } else {
            Err(serde::de::Error::custom("expected \"*\""))
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "lowercase")]
pub enum TokenRole {
    Admin,
    Editor,
    Viewer,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TokenInfo {
    pub id: String,
    pub label: String,
    pub user_email: Option<String>,
    pub scopes: TokenScopes,
    pub minted_by: String,
    pub expires_at: String,
    pub revoked: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

/// Returned only on creation — the raw token is shown once and never again.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MintTokenResponse {
    pub token: String,
    pub token_id: String,
    pub expires_at: String,
}

/// Request body for minting a new scoped token.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MintTokenRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_email: Option<String>,
    pub scopes: TokenScopes,
    pub expires_in: i64,
    pub label: String,
}

/// Response from revoking tokens by email.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RevokeByEmailResponse {
    pub revoked: i64,
}

// ── Ad-hoc execution ────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AdhocRunResult {
    pub execution_id: String,
    pub project_id: i64,
}

// ── MCP ─────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "lowercase")]
pub enum McpOrigin {
    Env,
    Script,
    Db,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct McpServerSummary {
    pub alias: String,
    pub url: String,
    pub origin: McpOrigin,
    pub is_registry: bool,
    pub status: String,
    pub tool_count: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct McpToolSummary {
    pub qualified_name: String,
    pub server_alias: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct McpHealth {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check_at: Option<String>,
}

/// Response from `GET /me/sandbox`.
#[derive(Deserialize, Clone, Debug)]
pub(crate) struct SandboxProjectIdResponse {
    pub project_id: i64,
}

// ── Internal request bodies ──────────────────────────────────────────────────

#[derive(Serialize)]
pub(crate) struct RegisterRequest {
    pub id: String,
    pub name: String,
    pub interests: Vec<ClientInterest>,
}

#[derive(Serialize)]
pub(crate) struct HeartbeatRequest {
    pub client_id: String,
}

#[derive(Serialize, Default)]
pub(crate) struct RunRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs: Option<HashMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breakpoint_lines: Option<Vec<usize>>,
}

#[derive(Serialize)]
pub(crate) struct CreateProjectRequest<'a> {
    pub name: &'a str,
}

#[derive(Serialize)]
pub(crate) struct UpdateProjectRequest<'a> {
    pub name: &'a str,
}

#[derive(Serialize)]
pub(crate) struct CreateScriptBody<'a> {
    pub source: &'a str,
}

#[derive(Serialize)]
pub(crate) struct RenameScriptRequest<'a> {
    pub new_name: &'a str,
}

#[derive(Serialize)]
pub(crate) struct MoveScriptRequest {
    pub target_project_id: i64,
}

#[derive(Serialize)]
pub(crate) struct ReorderRequest {
    pub order: Vec<i64>,
}

/// Response from `POST /projects/{id}/mcp/servers/{alias}/refresh`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct McpRefreshResult {
    pub refreshed: bool,
    pub alias: String,
    pub tool_count: usize,
}

/// Response from `GET /projects/{id}/mcp/servers/{alias}/drift`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct McpDriftResult {
    pub drifted: bool,
    #[serde(default)]
    pub added: Vec<String>,
    #[serde(default)]
    pub removed: Vec<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct PutDraftRequest<'a> {
    pub source: &'a str,
}

#[derive(Serialize, Default)]
pub(crate) struct PublishRequest {
    pub channels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
}

#[derive(Serialize)]
pub(crate) struct CreateChannelRequest<'a> {
    pub name: &'a str,
}

#[derive(Serialize)]
pub(crate) struct MoveChannelRequest {
    pub version_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
}

#[derive(Serialize)]
pub(crate) struct RebindLockRequest {
    pub version_id: Option<i64>,
}

#[derive(Serialize)]
pub(crate) struct AdhocRunRequest<'a> {
    pub source: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs: Option<HashMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breakpoint_lines: Option<Vec<usize>>,
    /// Release channel for resolving `use foo` references (#1120). When
    /// `None`, the server applies its default (typically `production`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<&'a str>,
    /// Opaque identifier recorded with the execution for audit (#1120).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<&'a str>,
}

// ── Evals ────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EvalSuite {
    pub id: i64,
    pub script_id: i64,
    pub name: String,
    pub runner_url: String,
    pub config: serde_json::Value,
    #[serde(default)]
    pub auto_run_channels: Vec<String>,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EvalRun {
    pub id: i64,
    pub suite_id: i64,
    pub script_id: i64,
    pub version_id: Option<i64>,
    pub channel: Option<String>,
    pub source_hash: String,
    pub status: String,
    pub total_cases: Option<i32>,
    pub completed_cases: i32,
    pub average_score: Option<f64>,
    pub runner_run_id: Option<String>,
    pub detail_url: Option<String>,
    pub triggered_by: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub error: Option<String>,
}

/// A per-case report pushed by an eval runner during a run.
/// Matches the server's `EvalCaseReport`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EvalCaseReport {
    pub case_id: String,
    pub score: Option<f64>,
    pub status: String,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    pub execution_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EvalResult {
    pub id: i64,
    pub run_id: i64,
    pub case_id: String,
    pub score: Option<f64>,
    pub status: String,
    pub metadata: Option<serde_json::Value>,
    pub execution_id: Option<String>,
    pub created_at: String,
}

/// One row per eval suite for the project-level cross-script dashboard.
/// Returned by `GET /projects/{id}/eval-suite-summaries` (sub-spec 1a).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EvalSuiteSummary {
    pub suite_id: i64,
    pub script_id: i64,
    pub script_name: String,
    pub suite_name: String,
    pub latest_run_id: Option<i64>,
    pub latest_run_at: Option<String>,
    pub latest_avg_score: Option<f64>,
    pub prior_avg_score: Option<f64>,
}

#[derive(Serialize)]
pub(crate) struct ResumeRequest {
    pub token: String,
    pub data: serde_json::Value,
}

#[derive(Serialize)]
pub(crate) struct RunWithS3Request {
    pub inputs: HashMap<String, S3DocumentRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct CreateEvalSuiteRequest<'a> {
    pub name: &'a str,
    pub runner_url: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_run_channels: Option<Vec<String>>,
}

#[derive(Serialize, Default)]
pub(crate) struct UpdateEvalSuiteRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_run_channels: Option<Vec<String>>,
}

#[derive(Serialize, Default)]
pub(crate) struct TriggerEvalRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_publish: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<String>,
}

#[derive(Serialize, Default)]
pub(crate) struct RunFromRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs: Option<HashMap<String, serde_json::Value>>,
    pub seed_env: HashMap<String, serde_json::Value>,
    pub skip_node_ids: Vec<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<String>,
}

// ── Document ingest (new API, puto-first) ────────────────────────────────

/// Conversion status reported by the ingest endpoints. `Text` means the file
/// was ingested via the pure-text fast-path (no VLM/Docling call). `Ready`
/// means conversion completed. `Converting` means another caller is currently
/// converting these bytes; use [`DocumentsClient::ingest`] to wait.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversionStatus {
    Text,
    Ready,
    Converting,
    Pending,
    Failed,
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UploadResult {
    pub document_id: String,
    pub filename: String,
    pub content_hash: String,
    pub conversion_status: ConversionStatus,
}

/// Snapshot of server-side conversion progress for a content hash (#1151).
/// Returned by [`crate::sub::documents::DocumentsClient::progress`].
/// Mirrors the TS `IngestProgress` and Python `IngestProgress` types.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct IngestProgress {
    /// Pages already converted.
    pub done: u32,
    /// Total pages in the document.
    pub total: u32,
}

/// Wire-level shape of `GET /projects/{pid}/documents/by-hash/{hash}/progress`.
#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum ProgressResponseWire {
    Converting { done_pages: u32, total_pages: u32 },
    Idle,
}

#[derive(Clone, Debug)]
pub enum ClaimOutcome {
    Hit(UploadResult),
    Miss,
}

// Internal wire types.

#[derive(Serialize)]
pub(crate) struct ClaimRequest<'a> {
    pub content_hash: &'a str,
    pub filename: &'a str,
}

/// Wire-level discriminated union returned by POST /projects/{pid}/documents/claim.
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum ClaimResponseWire {
    Hit {
        document_id: String,
        filename: String,
        content_hash: String,
        conversion_status: ConversionStatus,
    },
    Miss,
}

// ── Bench ────────────────────────────────────────────────────────────────────
//
// Wire models mirroring `crates/akribes-server/src/models.rs` for the bench
// substrate. Timestamps are surfaced as `String` rather than `chrono::DateTime`
// to keep the SDK independent of `chrono` — the server emits RFC3339 strings
// that round-trip through `String` cleanly.

/// Per-script bench configuration. One row per `scripts.id`.
/// `judge_script_id` is nullable while the bench is still being authored.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Bench {
    pub id: i64,
    pub script_id: i64,
    #[serde(default)]
    pub judge_script_id: Option<i64>,
    pub judge_channel: String,
    pub config: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
}

/// Aggregated per-bench summary used by the project-level evals landing page.
/// Returned by `GET /projects/{id}/benches`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProjectBenchSummary {
    pub bench_id: i64,
    pub script_id: i64,
    pub script_name: String,
    #[serde(default)]
    pub judge_script_id: Option<i64>,
    #[serde(default)]
    pub judge_script_name: Option<String>,
    pub judge_channel: String,
    pub case_count: i64,
    #[serde(default)]
    pub latest_run_id: Option<i64>,
    #[serde(default)]
    pub latest_run_status: Option<String>,
    #[serde(default)]
    pub latest_run_channel: Option<String>,
    #[serde(default)]
    pub latest_run_workflow_version_id: Option<i64>,
    #[serde(default)]
    pub latest_run_at: Option<String>,
    #[serde(default)]
    pub latest_run_mean_score: Option<f64>,
    #[serde(default)]
    pub latest_run_cost_usd: Option<f64>,
    pub updated_at: String,
}

/// A single bench-run row. `workflow_version_id` and `judge_version_id` are
/// resolved at trigger time so a later channel publish doesn't change what
/// this run represents.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BenchRun {
    pub id: i64,
    pub bench_id: i64,
    pub channel: String,
    pub workflow_version_id: i64,
    pub judge_version_id: i64,
    pub status: String,
    #[serde(default)]
    pub triggered_by: Option<String>,
    pub triggered_at: String,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub total_cost_usd: f64,
    #[serde(default)]
    pub total_cases: i32,
    #[serde(default)]
    pub cache_hit_cases: i32,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub mcp_session_id: Option<String>,
    #[serde(default)]
    pub case_filter: Option<Vec<String>>,
    /// Mean headline score across completed (`status='ok' OR 'cached'`) results
    /// in this run. Populated by the list-runs aggregate query; bare
    /// GET-single-run + coordinator inserts leave it `None`.
    #[serde(default)]
    pub mean_headline_score: Option<f64>,
    /// Number of results with `status='ok' OR 'cached'`. Populated alongside
    /// `mean_headline_score` by the list-runs aggregate.
    #[serde(default)]
    pub ok_cases: Option<i64>,
    /// Per-`BenchResultStatus` row count for this run, surfaced by the
    /// list-runs and get-run aggregate queries (#753). Lets consumers
    /// render a failure-mix breakdown (workflow_failed vs judge_failed vs
    /// skipped) without an N+1 `/results` fetch. Statuses with zero rows
    /// may be absent rather than serialised as `0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_breakdown: Option<std::collections::HashMap<String, i64>>,
    /// Name of the judge script whose version produced this run. Joined in by
    /// `get_run` and `list_runs` on the server so a caller can deep-link to
    /// the judge's source at `judge_version_id` without an N+1 lookup. Empty
    /// on coordinator-inserted rows and on benches with no judge wired up.
    #[serde(default)]
    pub judge_script_name: Option<String>,
}

/// One per-case score row for a bench run. Carries the workflow execution's
/// typed `workflow_output` alongside the judge's `score` blob so the studio's
/// typed renderers don't need a second fetch.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BenchResult {
    pub id: i64,
    pub bench_run_id: i64,
    pub case_id: String,
    #[serde(default)]
    pub workflow_execution_id: Option<String>,
    #[serde(default)]
    pub judge_execution_id: Option<String>,
    #[serde(default)]
    pub score: Option<serde_json::Value>,
    #[serde(default)]
    pub headline_score: Option<f64>,
    pub status: String,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub duration_ms: Option<i32>,
    #[serde(default)]
    pub cache_hit: bool,
    #[serde(default)]
    pub input_hash: Option<String>,
    pub created_at: String,
    /// Parsed `WorkflowEnd` payload from the workflow execution. `None` when
    /// the workflow failed, was canceled, or this is a cache-hit row.
    #[serde(default)]
    pub workflow_output: Option<serde_json::Value>,
}

/// Server-side projection of an `executions` row with `kind='case'`. Cases live
/// in the same table as live executions.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BenchCase {
    pub id: String,
    pub project_id: i64,
    pub script_name: String,
    #[serde(default)]
    pub bench_id: Option<i64>,
    pub kind: String,
    pub frozen: bool,
    #[serde(default)]
    pub case_name: Option<String>,
    #[serde(default)]
    pub inputs: Option<serde_json::Value>,
    #[serde(default)]
    pub expected_output: Option<serde_json::Value>,
    #[serde(default)]
    pub ground_truth: Option<serde_json::Value>,
    /// SHA-256 hex (lowercase) of `canonical_json(inputs)`. Used as one
    /// component of the bench-result cache key. Nullable for legacy rows.
    #[serde(default)]
    pub input_hash: Option<String>,
    pub created_at: String,
}

/// Returned by `GET /bench-runs/{a}/compare/{b}`. Per-case score delta.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CompareCase {
    pub case_id: String,
    pub case_label: String,
    #[serde(default)]
    pub score_a: Option<f64>,
    #[serde(default)]
    pub score_b: Option<f64>,
    #[serde(default)]
    pub delta: Option<f64>,
    /// `improved | regressed | unchanged | missing_a | missing_b`.
    pub flag: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CompareAggregate {
    pub mean_score_delta: f64,
    pub cost_delta_usd: f64,
    pub n_regressed: i32,
    pub n_improved: i32,
    pub n_unchanged: i32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CompareReport {
    pub run_a_id: i64,
    pub run_b_id: i64,
    pub aggregate: CompareAggregate,
    pub per_case: Vec<CompareCase>,
}

/// Single drifted case from `GET /projects/{id}/scripts/{name}/bench/cases/contract-drift`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DriftedCase {
    pub case_id: String,
    pub label: String,
    pub what_broke: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DriftReport {
    pub drifted: Vec<DriftedCase>,
    #[serde(default)]
    pub script_version_id: Option<i64>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub published_by: Option<String>,
    pub summary: String,
}

/// Receipt returned by `PATCH /bench-runs/{id}/tag-session`. Used to confirm
/// the coordinator picked up the MCP-session attribution.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BenchRunTagSessionResponse {
    pub tagged: bool,
    pub run_id: i64,
    pub mcp_session_id: String,
}

// ── Bench request wire types ────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct CreateOrUpdateBenchRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judge_script_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judge_channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CreateBenchCaseRequest {
    pub inputs: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_output: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ground_truth: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PatchBenchCaseRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_output: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ground_truth: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PromoteCaseEdits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_output: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ground_truth: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PromoteExecutionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edits: Option<PromoteCaseEdits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct TriggerBenchRunRequest {
    pub channel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Optional subset of case IDs. `None` or empty array → run every case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_ids: Option<Vec<String>>,
}

/// Page of bench-run events emitted by `GET /bench-runs/{id}/events`.
/// The MCP layer polls this endpoint for incremental updates rather than
/// subscribing to the SSE form — same path, JSON wrapper.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct BenchRunEventsPage {
    #[serde(default)]
    pub events: Vec<serde_json::Value>,
    #[serde(default)]
    pub complete: Option<bool>,
}
