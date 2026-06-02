//! Structured error envelope shared by core and the SDK.
//!
//! This is the wire-level slice of the `akribes-core::error` module: the
//! [`ErrorKind`] / [`ErrorCode`] enums plus their pure-data impls
//! (`as_wire`, `from_wire`, `kind`, `default_user_message`,
//! `suggested_action`, `is_transient`, `is_server_error`,
//! `is_user_actionable`, `base_backoff_ms`), the [`ErrorSource`] /
//! [`ErrorDetail`] envelopes, the [`SuggestedAction`] tag, and the
//! [`ErrorCode::parse_retry_after_ms`] retry-after hint parser.
//!
//! Functions that bring in heavier deps (regex-backed `sanitize_error` and
//! `ErrorKind::classify`, the tokio-backed `CancelTracker` / `CancelReason`,
//! the regex-backed `ErrorCode::classify_provider_error`) stay in
//! `akribes_core::error` so the types crate keeps its dependency surface
//! to `serde`, `serde_json`, `thiserror`, `httpdate`, and `tracing`.

use serde::{Deserialize, Serialize};

/// Coarse error category. Use [`ErrorCode`] for the finer-grained, stable
/// identifier that consumers should branch on; `ErrorKind` is the rollup
/// every code belongs to (so a UI can show one bucket, an SDK can decide
/// "is this retryable" without enumerating every code).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorKind {
    RateLimit,
    AuthError,
    TokenLimit,
    /// Upstream HTTP 500 — generic provider-side failure. Maybe-transient;
    /// retry with a short exponential backoff (issue #1296 split). Replaces
    /// the legacy umbrella `ServerError` for the 500 case specifically so
    /// retry policies and metrics can distinguish "internal server error"
    /// from "bad gateway" / "service unavailable" / "gateway timeout".
    ServerError500,
    /// Upstream HTTP 502 — bad gateway, the provider's edge fronted a
    /// failing origin. Retry with a short backoff (issue #1296 split).
    BadGateway502,
    /// Upstream HTTP 503 — service unavailable, rate-limit-adjacent.
    /// Honour `Retry-After` aggressively when the provider sent one
    /// (issue #1296 split). Default backoff matches `RateLimit` since the
    /// remediation pattern (wait for capacity) is the same.
    ServiceUnavailable503,
    /// Upstream HTTP 504 — gateway timeout. The provider's gateway didn't
    /// get an answer from the origin in time. Use a longer base backoff
    /// since the slow side is unlikely to recover faster than the request
    /// shape itself (issue #1296 split).
    GatewayTimeout504,
    NetworkError,
    ParseError,
    Cancelled,
    /// Server-side execution-budget timeout (`AKRIBES_EXECUTION_TIMEOUT`),
    /// or a checkpoint that elapsed its declared `on_timeout` window.
    /// Distinct from `Cancelled` (explicit user/client cancel) so consumers
    /// can tell "the workflow was stopped on purpose" from "the workflow
    /// ran past its budget" — the latter is a service-level error, not a
    /// user action. Distinct from `NetworkError`'s "timeout" classification,
    /// which covers per-provider network timeouts inside a still-running
    /// execution.
    Timeout,
    ScriptError,
    /// Workflow-author-initiated failure — the LLM returned a non-success
    /// variant (Unable / a custom failure arm) and the author mapped it to
    /// `fail` (explicit `on <V> fail` or implicit no-trailer default).
    /// Distinguished from `ScriptError` so the workflow runner can retry
    /// the failing task up to `workflow_retries` times before surfacing
    /// the failure to the caller (issue #312). Retry exhaustion converts
    /// this to a `ScriptError` to preserve existing handler behavior.
    AuthorRaise,
    /// Cross-script `call(...)` chain exceeded the engine's `SUBSCRIPT_MAX_DEPTH`
    /// (issue #429, `AKRIBES-E-SCRIPT-DEPTH`).
    ScriptDepthExceeded,
    /// A spawned tokio task in the engine panicked (typically `unwrap()`
    /// on `None`, divide-by-zero in stdlib, or an `expect()` blowing).
    /// Distinct from `ScriptError` because the workflow author didn't
    /// cause it — it indicates an engine bug that should be filed.
    /// Surfaces as `AKRIBES-E-INTERNAL-PANIC`.
    Panic,
    /// An invariant inside the engine/server was violated — a `oneshot`
    /// sender was dropped without sending, a deadlock was detected, an
    /// MCP protocol violation, etc. Always indicates a bug in Akribes
    /// itself, not in user code or in a third-party provider.
    Internal,
}

/// What the client/user/runner should do in response. Derived from
/// [`ErrorKind`] (see [`ErrorKind::suggested_action`]) so consumers don't
/// have to maintain their own switch statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SuggestedAction {
    /// Retry the operation as-is (no input change required). Pair with
    /// [`ErrorDetail::retry_after_ms`] when known.
    Retry,
    /// The error is the operator's responsibility — fix configuration
    /// (API keys, model setup, env vars).
    FixConfig,
    /// The error is the workflow author's responsibility — the script,
    /// prompts, or types need editing.
    FixScript,
    /// The input was too large or wrong-shape for the current run. The
    /// caller should reduce or correct it before retrying.
    FixInput,
    /// The workflow's `on <variant> fail` (or default failure handling)
    /// fired — the caller should treat the failed result as authored
    /// flow rather than bug.
    HandleAuthorFailure,
    /// User cancelled — no remediation needed.
    None,
    /// Looks like an Akribes bug. The caller should report (with the error
    /// code + execution id) rather than retry blindly.
    Report,
}

impl ErrorKind {
    /// Whether the underlying condition is expected to clear on its own —
    /// i.e. the same request retried later may succeed without any input
    /// change. Pairs with [`SuggestedAction::Retry`].
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ErrorKind::RateLimit
                | ErrorKind::ServerError500
                | ErrorKind::BadGateway502
                | ErrorKind::ServiceUnavailable503
                | ErrorKind::GatewayTimeout504
                | ErrorKind::NetworkError
        )
    }

    /// True for any of the four upstream 5xx variants (#1296). Use this in
    /// places that need the umbrella "the provider returned a 5xx" check
    /// without enumerating every status. Pair with [`is_transient`] when
    /// the rate-limit / network-error siblings should also count.
    pub fn is_server_error(&self) -> bool {
        matches!(
            self,
            ErrorKind::ServerError500
                | ErrorKind::BadGateway502
                | ErrorKind::ServiceUnavailable503
                | ErrorKind::GatewayTimeout504
        )
    }

    /// Base backoff for the per-error retry loop in milliseconds. Drives
    /// the per-variant retry semantics introduced by issue #1296:
    ///
    /// | Kind                         | Base | Rationale                                            |
    /// |------------------------------|------|------------------------------------------------------|
    /// | `RateLimit`                  | 2000 | Honour `Retry-After`; otherwise a 2s start.          |
    /// | `ServerError500`             | 1000 | Maybe-transient origin failure — short doubling.     |
    /// | `BadGateway502`              | 1000 | Edge fronted a failing origin — short doubling.      |
    /// | `ServiceUnavailable503`      | 2000 | Capacity-adjacent — start at the rate-limit cadence. |
    /// | `GatewayTimeout504`          | 4000 | Slow upstream — longer base before retrying.         |
    /// | `NetworkError`               | 1000 | Connection-level recoverable.                        |
    ///
    /// All other variants return `None` (non-transient).
    pub fn base_backoff_ms(&self) -> Option<u64> {
        Some(match self {
            ErrorKind::RateLimit => 2_000,
            ErrorKind::ServerError500 => 1_000,
            ErrorKind::BadGateway502 => 1_000,
            ErrorKind::ServiceUnavailable503 => 2_000,
            ErrorKind::GatewayTimeout504 => 4_000,
            ErrorKind::NetworkError => 1_000,
            _ => return None,
        })
    }

    /// Whether the user (operator or workflow author) can fix this by
    /// changing something — config, script, or input. Used to gate
    /// "show actionable diagnostic UI" vs "just report it."
    pub fn is_user_actionable(&self) -> bool {
        matches!(
            self,
            ErrorKind::AuthError
                | ErrorKind::TokenLimit
                | ErrorKind::Timeout
                | ErrorKind::ScriptError
                | ErrorKind::ScriptDepthExceeded
                | ErrorKind::AuthorRaise
        )
    }

    /// Stable, machine-parseable identifier for the kind. Use this for
    /// wire payloads, log fields, and the `error_kind` DB column.
    /// Distinct from [`std::fmt::Display`] (which returns a human-readable
    /// phrase like `"rate limit"`) and from `Debug` (which is intentional
    /// here but not load-bearing).
    pub fn as_wire(&self) -> &'static str {
        match self {
            ErrorKind::RateLimit => "RateLimit",
            ErrorKind::AuthError => "AuthError",
            ErrorKind::TokenLimit => "TokenLimit",
            ErrorKind::ServerError500 => "ServerError500",
            ErrorKind::BadGateway502 => "BadGateway502",
            ErrorKind::ServiceUnavailable503 => "ServiceUnavailable503",
            ErrorKind::GatewayTimeout504 => "GatewayTimeout504",
            ErrorKind::NetworkError => "NetworkError",
            ErrorKind::ParseError => "ParseError",
            ErrorKind::Cancelled => "Cancelled",
            ErrorKind::Timeout => "Timeout",
            ErrorKind::ScriptError => "ScriptError",
            ErrorKind::AuthorRaise => "AuthorRaise",
            ErrorKind::ScriptDepthExceeded => "ScriptDepthExceeded",
            ErrorKind::Panic => "Panic",
            ErrorKind::Internal => "Internal",
        }
    }

    /// What the caller should do — see [`SuggestedAction`].
    pub fn suggested_action(&self) -> SuggestedAction {
        match self {
            ErrorKind::RateLimit
            | ErrorKind::ServerError500
            | ErrorKind::BadGateway502
            | ErrorKind::ServiceUnavailable503
            | ErrorKind::GatewayTimeout504
            | ErrorKind::NetworkError => SuggestedAction::Retry,
            ErrorKind::AuthError => SuggestedAction::FixConfig,
            ErrorKind::TokenLimit => SuggestedAction::FixInput,
            ErrorKind::Timeout => SuggestedAction::FixInput,
            ErrorKind::ScriptError | ErrorKind::ScriptDepthExceeded | ErrorKind::ParseError => {
                SuggestedAction::FixScript
            }
            ErrorKind::AuthorRaise => SuggestedAction::HandleAuthorFailure,
            ErrorKind::Cancelled => SuggestedAction::None,
            ErrorKind::Panic | ErrorKind::Internal => SuggestedAction::Report,
        }
    }
}

/// Stable, fine-grained error identifier. Each code maps to exactly one
/// [`ErrorKind`] and carries a default user-facing message. Wire form:
/// `AKRIBES-E-<UPPER-KEBAB>` (e.g. `AKRIBES-E-PROVIDER-RATE-LIMIT`).
///
/// Codes are intentionally durable: once published, the wire string and
/// `kind()` mapping should not change. Add new variants for new
/// conditions rather than repurposing old ones; SDKs match on these
/// strings to drive retry/UI/triage logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorCode {
    /// Explicit user/client cancellation via `POST /executions/:id/cancel`.
    UserCancelled,
    /// Server-side execution-budget timeout (`AKRIBES_EXECUTION_TIMEOUT`).
    ExecutionTimeout,
    /// `on_timeout` window on a checkpoint elapsed without a resume.
    CheckpointTimeout,
    /// Provider returned a 429 / rate-limit / quota-exhausted response.
    ProviderRateLimit,
    /// Provider returned 401/403, or an API key was missing / not configured.
    ProviderAuth,
    /// Provider's reported context window or `max_tokens` was exceeded.
    ProviderTokenLimit,
    /// Legacy umbrella for any 5xx — kept for wire backward-compat after
    /// the issue #1296 split. New code should construct one of
    /// [`ProviderServer500`], [`ProviderBadGateway502`],
    /// [`ProviderServiceUnavailable503`], or [`ProviderGatewayTimeout504`].
    /// Decoding the old `AKRIBES-E-PROVIDER-SERVER` wire string yields this
    /// variant so SDKs that match on it stay green.
    ProviderServer,
    /// Provider returned HTTP 500. Maybe-transient; short retry-with-backoff.
    ProviderServer500,
    /// Provider returned HTTP 502 (bad gateway). Retry with short backoff.
    ProviderBadGateway502,
    /// Provider returned HTTP 503 (service unavailable). Rate-limit-adjacent
    /// — honour `Retry-After` aggressively.
    ProviderServiceUnavailable503,
    /// Provider returned HTTP 504 (gateway timeout). Longer base backoff
    /// since the slow side is unlikely to recover faster than the request.
    ProviderGatewayTimeout504,
    /// Network-level failure reaching the provider (DNS, TLS, reset,
    /// per-provider request timeout).
    ProviderNetwork,
    /// Provider response did not parse as the expected schema.
    ProviderParse,
    /// Generic provider/runtime failure that didn't fit a more specific bucket.
    ProviderOther,
    /// A spawned engine task panicked (host-side bug).
    InternalPanic,
    /// A `oneshot::Receiver` returned `Err` because its sender was dropped
    /// before sending — covers breakpoint resume, checkpoint resume,
    /// tool-approval resume. Indicates a server-side cleanup race or a
    /// host bug, never a user action.
    InternalDroppedChannel,
    /// Engine reached a state with pending nodes but none ready to run —
    /// dependency cycle or compiler bug.
    InternalDeadlock,
    /// JoinError that wasn't a panic and wasn't a recognized cancel —
    /// tokio runtime aborted the task externally. Treated as a host
    /// invariant violation.
    InternalTaskAborted,
    /// Generic "this should not happen" host failure that doesn't fit
    /// the more specific Internal* codes.
    InternalOther,
    /// Generic workflow-author error not categorised more specifically.
    ScriptError,
    /// Cross-script `call(...)` chain exceeded the depth cap.
    ScriptDepthExceeded,
    /// Validation retries exhausted with `allow_partial: true` (issue #202)
    /// — partial-retry sentinel routed to `on unable` / handler.
    PartialRetryExhausted,
    /// Workflow author's `fail` arm fired — the LLM returned an Unable
    /// or other non-success variant the author mapped to failure.
    AuthorRaise,
    /// Per-agent tool budget (`tool_budget`) exceeded.
    ToolBudgetExceeded,
    /// Tool approval resume returned a payload that wasn't the expected
    /// `{ approve: bool, args?: Value }` shape — host protocol violation.
    ToolApprovalProtocol,
    /// Tool call attempted but no MCP registry was attached (script-only
    /// engine, missing host wiring).
    ToolNoRegistry,
    /// MCP tool call returned a tool-side failure (the registry exists
    /// and dispatched, but the tool itself errored).
    ToolError,
    /// Agent dispatched a second `tool_use` after the engine had already
    /// folded one round-trip's tool results back into the conversation.
    /// Agents are single-round-trip by design — multi-turn agentic
    /// behaviour belongs on a `loop` block. Surfaces when an LLM ignores
    /// the synthesized "produce your final answer now" follow-up turn
    /// and tries to invoke tools again. The fix is either to use a
    /// `loop` block or to tighten the agent's system prompt.
    AgentToolsDoubleDispatch,
    /// Required configuration missing (API key, env var, etc.).
    ConfigMissing,
    /// Loop block exceeded its `max_total_output_tokens` budget. The
    /// loop driver accumulates each turn's `output_tokens` from the
    /// provider and stamps this code on the resulting `LoopEnd { value:
    /// FatalError }` once the running total exceeds the per-loop or
    /// project-default cap.
    LoopOutputBudgetExceeded,
    /// A second checkpoint fired in the same `loop` turn. The supported
    /// envelope is at most one checkpoint per turn — the driver tracks a
    /// per-turn counter and fails fast when the increment goes past 1.
    /// Surfacing this as a distinct code (rather than `Other`) lets SDKs
    /// and the Studio render a targeted explanation: split the
    /// checkpoints across turns, or move one onto a non-loop sibling.
    LoopMultiCheckpoint,
    /// Mode 1 (`compaction: none` / omitted) only — the assembled
    /// request would exceed the model's context window. Pre-call
    /// diagnostic emitted by `Engine::run_compaction_chain`; replaces
    /// the cryptic provider 400 from upstream. Carries the conversation
    /// length, the model cap, and the agent in the message body.
    ContextOverflow,
    /// `compaction: native()` (or `at <T>: native()`) used with a model
    /// whose `ModelEntry::native_compaction_capable` is `false`. The
    /// related-info span points at the model declaration.
    ContextNativeUnsupported,
    /// All custom-chain steps ran and the conversation still exceeds
    /// the configured cap. Surfaces with the chain of attempted
    /// strategies in the message body. Emitted instead of a provider
    /// 400 — fail-fast at the akribes seam.
    ContextCompactionExhausted,
    /// `compaction: at <invalid>` — value <= 0, percent > 100, or
    /// duplicate threshold in a custom chain. Compile-time only.
    CompactionThresholdInvalid,
    /// User-defined compactor task referenced from a compaction step
    /// doesn't match one of the four supported signatures
    /// (`str|list[message] -> str|list[message]`). Compile-time only.
    CompactorSignature,
    /// `compact_to_state(field=...)` used outside a loop's `compaction:`
    /// block. The primitive is loop-only because it writes into the
    /// loop's state record. Compile-time only.
    CompactionLoopOnly,
    /// `std.format` placeholder `{name}` not present in args.
    /// Stable string form: `AKRIBES-E-STD-FORMAT-MISS-001` (#1224).
    StdFormatMissing,
    /// `std.format` malformed template (unclosed `{`, empty `{}`,
    /// stray `}`). Stable string form: `AKRIBES-E-STD-FORMAT-SYNTAX-001`.
    StdFormatSyntax,
    /// `std.json_parse` could not parse the input string as JSON.
    /// Stable string form: `AKRIBES-E-STD-JSON-PARSE-001`.
    StdJsonParse,
    /// `std.json_stringify` could not serialise the input value (e.g.
    /// `FatalError` payload; non-serializable). Stable string form:
    /// `AKRIBES-E-STD-JSON-STRINGIFY-001`.
    StdJsonStringify,
    /// `std.regex_extract` was given an invalid regex pattern.
    /// Stable string form: `AKRIBES-E-STD-REGEX-001`.
    StdRegexInvalid,
    /// Catch-all for sites that haven't been migrated to a richer code.
    /// Prefer adding a specific variant — this is for transition only.
    Other,
}

impl ErrorCode {
    /// Extract a `retry_after_ms` hint from a provider error message
    /// when the wire response carried one (provider implementations
    /// usually echo it as `retry-after: <secs>` or similar). None when
    /// no such hint is present.
    ///
    /// Honours both [RFC 9110 §10.2.3] forms:
    ///
    /// 1. **delta-seconds** — `Retry-After: 30` (returned as `30_000`).
    /// 2. **HTTP-date** — `Retry-After: Wed, 21 Oct 2026 07:28:00 GMT`
    ///    parsed via `httpdate::parse_http_date` and returned as the
    ///    delta from `SystemTime::now()`, clamped to `>= 0`.
    ///
    /// [RFC 9110 §10.2.3]: https://www.rfc-editor.org/rfc/rfc9110#section-10.2.3
    pub fn parse_retry_after_ms(msg: &str) -> Option<u64> {
        // `retry-after: 30` (seconds) — common HTTP convention.
        // Match decimals; we only ever emit milliseconds.
        let needle = "retry-after";
        // ASCII-case-insensitive search directly in `msg` (issue #1058
        // — using `to_lowercase().find()` shifts indices on
        // length-changing chars like `İ`).
        let bytes = msg.as_bytes();
        let n_len = needle.len();
        let start = if bytes.len() < n_len {
            return None;
        } else {
            (0..=bytes.len() - n_len)
                .find(|&i| bytes[i..i + n_len].eq_ignore_ascii_case(needle.as_bytes()))?
        };
        let after = &msg[start + needle.len()..];
        // Walk past separators (`:`, `=`, whitespace).
        let after = after.trim_start_matches(|c: char| c == ':' || c == '=' || c.is_whitespace());
        let end = after
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(after.len());
        let head = &after[..end];
        if !head.is_empty() {
            if let Ok(secs) = head.parse::<u64>() {
                return Some(secs.saturating_mul(1000));
            }
            if let Ok(secs_f) = head.parse::<f64>() {
                if secs_f.is_finite() && secs_f >= 0.0 {
                    return Some((secs_f * 1000.0) as u64);
                }
            }
        }
        // HTTP-date branch (#1058).
        let date_slice = after
            .split(['\n', '\r'])
            .next()
            .unwrap_or(after)
            .trim()
            .trim_end_matches([',', ';', '.']);
        if date_slice.is_empty() {
            return None;
        }
        if let Ok(then) = httpdate::parse_http_date(date_slice) {
            let now = std::time::SystemTime::now();
            match then.duration_since(now) {
                Ok(d) => return Some(d.as_millis().min(u64::MAX as u128) as u64),
                Err(_) => return Some(0),
            }
        }
        None
    }
}

impl ErrorCode {
    /// The [`ErrorKind`] bucket this code belongs to. Computed once,
    /// statically — used by consumers that want the rollup behaviour
    /// (`is_transient`, `suggested_action`) without hand-mapping codes.
    pub fn kind(&self) -> ErrorKind {
        match self {
            ErrorCode::UserCancelled => ErrorKind::Cancelled,
            ErrorCode::ExecutionTimeout | ErrorCode::CheckpointTimeout => ErrorKind::Timeout,
            ErrorCode::ProviderRateLimit => ErrorKind::RateLimit,
            ErrorCode::ProviderAuth | ErrorCode::ConfigMissing => ErrorKind::AuthError,
            ErrorCode::ProviderTokenLimit => ErrorKind::TokenLimit,
            ErrorCode::ProviderServer => ErrorKind::ServerError500,
            ErrorCode::ProviderServer500 => ErrorKind::ServerError500,
            ErrorCode::ProviderBadGateway502 => ErrorKind::BadGateway502,
            ErrorCode::ProviderServiceUnavailable503 => ErrorKind::ServiceUnavailable503,
            ErrorCode::ProviderGatewayTimeout504 => ErrorKind::GatewayTimeout504,
            ErrorCode::ProviderNetwork => ErrorKind::NetworkError,
            ErrorCode::ProviderParse => ErrorKind::ParseError,
            ErrorCode::ProviderOther => ErrorKind::ServerError500,
            ErrorCode::InternalPanic => ErrorKind::Panic,
            ErrorCode::InternalDroppedChannel
            | ErrorCode::InternalDeadlock
            | ErrorCode::InternalTaskAborted
            | ErrorCode::InternalOther => ErrorKind::Internal,
            ErrorCode::ScriptError
            | ErrorCode::ToolBudgetExceeded
            | ErrorCode::ToolApprovalProtocol
            | ErrorCode::ToolNoRegistry
            | ErrorCode::ToolError
            | ErrorCode::AgentToolsDoubleDispatch
            | ErrorCode::LoopOutputBudgetExceeded
            | ErrorCode::LoopMultiCheckpoint
            | ErrorCode::ContextOverflow
            | ErrorCode::ContextNativeUnsupported
            | ErrorCode::ContextCompactionExhausted
            | ErrorCode::CompactionThresholdInvalid
            | ErrorCode::CompactorSignature
            | ErrorCode::CompactionLoopOnly
            | ErrorCode::PartialRetryExhausted
            | ErrorCode::StdFormatMissing
            | ErrorCode::StdFormatSyntax
            | ErrorCode::StdJsonParse
            | ErrorCode::StdJsonStringify
            | ErrorCode::StdRegexInvalid
            | ErrorCode::Other => ErrorKind::ScriptError,
            ErrorCode::ScriptDepthExceeded => ErrorKind::ScriptDepthExceeded,
            ErrorCode::AuthorRaise => ErrorKind::AuthorRaise,
        }
    }

    /// Stable wire identifier (`AKRIBES-E-<UPPER-KEBAB>`). This is the
    /// string consumers should match on for retry/UI logic.
    pub fn as_wire(&self) -> &'static str {
        match self {
            ErrorCode::UserCancelled => "AKRIBES-E-USER-CANCELLED",
            ErrorCode::ExecutionTimeout => "AKRIBES-E-EXECUTION-TIMEOUT",
            ErrorCode::CheckpointTimeout => "AKRIBES-E-CHECKPOINT-TIMEOUT",
            ErrorCode::ProviderRateLimit => "AKRIBES-E-PROVIDER-RATE-LIMIT",
            ErrorCode::ProviderAuth => "AKRIBES-E-PROVIDER-AUTH",
            ErrorCode::ProviderTokenLimit => "AKRIBES-E-PROVIDER-TOKEN-LIMIT",
            ErrorCode::ProviderServer => "AKRIBES-E-PROVIDER-SERVER",
            ErrorCode::ProviderServer500 => "AKRIBES-E-PROVIDER-SERVER-500",
            ErrorCode::ProviderBadGateway502 => "AKRIBES-E-PROVIDER-BAD-GATEWAY-502",
            ErrorCode::ProviderServiceUnavailable503 => {
                "AKRIBES-E-PROVIDER-SERVICE-UNAVAILABLE-503"
            }
            ErrorCode::ProviderGatewayTimeout504 => "AKRIBES-E-PROVIDER-GATEWAY-TIMEOUT-504",
            ErrorCode::ProviderNetwork => "AKRIBES-E-PROVIDER-NETWORK",
            ErrorCode::ProviderParse => "AKRIBES-E-PROVIDER-PARSE",
            ErrorCode::ProviderOther => "AKRIBES-E-PROVIDER-OTHER",
            ErrorCode::InternalPanic => "AKRIBES-E-INTERNAL-PANIC",
            ErrorCode::InternalDroppedChannel => "AKRIBES-E-INTERNAL-DROPPED-CHANNEL",
            ErrorCode::InternalDeadlock => "AKRIBES-E-INTERNAL-DEADLOCK",
            ErrorCode::InternalTaskAborted => "AKRIBES-E-INTERNAL-TASK-ABORTED",
            ErrorCode::InternalOther => "AKRIBES-E-INTERNAL-OTHER",
            ErrorCode::ScriptError => "AKRIBES-E-SCRIPT-ERROR",
            ErrorCode::ScriptDepthExceeded => "AKRIBES-E-SCRIPT-DEPTH",
            ErrorCode::PartialRetryExhausted => "AKRIBES-E-RETRY-PARTIAL-EXHAUSTED",
            ErrorCode::AuthorRaise => "AKRIBES-E-AUTHOR-RAISE",
            ErrorCode::ToolBudgetExceeded => "AKRIBES-E-TOOL-BUDGET",
            ErrorCode::ToolApprovalProtocol => "AKRIBES-E-TOOL-APPROVAL-PROTOCOL",
            ErrorCode::ToolNoRegistry => "AKRIBES-E-TOOL-NO-REGISTRY",
            ErrorCode::ToolError => "AKRIBES-E-TOOL-ERROR",
            ErrorCode::AgentToolsDoubleDispatch => "AKRIBES-E-AGENT-TOOLS-DOUBLE-DISPATCH",
            ErrorCode::ConfigMissing => "AKRIBES-E-CONFIG-MISSING",
            ErrorCode::LoopOutputBudgetExceeded => "AKRIBES-E-LOOP-OUTPUT-BUDGET-EXCEEDED",
            ErrorCode::LoopMultiCheckpoint => "AKRIBES-E-LOOP-MULTI-CHECKPOINT",
            ErrorCode::ContextOverflow => "AKRIBES-E-CONTEXT-OVERFLOW",
            ErrorCode::ContextNativeUnsupported => "AKRIBES-E-CONTEXT-NATIVE-UNSUPPORTED",
            ErrorCode::ContextCompactionExhausted => "AKRIBES-E-CONTEXT-COMPACTION-EXHAUSTED",
            ErrorCode::CompactionThresholdInvalid => "AKRIBES-E-COMPACTION-THRESHOLD-INVALID",
            ErrorCode::CompactorSignature => "AKRIBES-E-COMPACTOR-SIGNATURE",
            ErrorCode::CompactionLoopOnly => "AKRIBES-E-COMPACTION-LOOP-ONLY",
            ErrorCode::StdFormatMissing => "AKRIBES-E-STD-FORMAT-MISS-001",
            ErrorCode::StdFormatSyntax => "AKRIBES-E-STD-FORMAT-SYNTAX-001",
            ErrorCode::StdJsonParse => "AKRIBES-E-STD-JSON-PARSE-001",
            ErrorCode::StdJsonStringify => "AKRIBES-E-STD-JSON-STRINGIFY-001",
            ErrorCode::StdRegexInvalid => "AKRIBES-E-STD-REGEX-001",
            ErrorCode::Other => "AKRIBES-E-OTHER",
        }
    }

    /// Parse the canonical wire form (`AKRIBES-E-<UPPER-KEBAB>`) back to a
    /// code. Returns `None` for any string we don't recognise so the
    /// caller can decide whether to fall back to [`ErrorCode::Other`]
    /// or surface the unknown code as-is. Used by the legacy
    /// `Value::fatal_with_code` shim and SDK normalisers.
    pub fn from_wire(s: &str) -> Option<Self> {
        let code = match s {
            "AKRIBES-E-USER-CANCELLED" => ErrorCode::UserCancelled,
            "AKRIBES-E-EXECUTION-TIMEOUT" => ErrorCode::ExecutionTimeout,
            "AKRIBES-E-CHECKPOINT-TIMEOUT" => ErrorCode::CheckpointTimeout,
            "AKRIBES-E-PROVIDER-RATE-LIMIT" => ErrorCode::ProviderRateLimit,
            "AKRIBES-E-PROVIDER-AUTH" => ErrorCode::ProviderAuth,
            "AKRIBES-E-PROVIDER-TOKEN-LIMIT" => ErrorCode::ProviderTokenLimit,
            "AKRIBES-E-PROVIDER-SERVER" => ErrorCode::ProviderServer,
            "AKRIBES-E-PROVIDER-SERVER-500" => ErrorCode::ProviderServer500,
            "AKRIBES-E-PROVIDER-BAD-GATEWAY-502" => ErrorCode::ProviderBadGateway502,
            "AKRIBES-E-PROVIDER-SERVICE-UNAVAILABLE-503" => {
                ErrorCode::ProviderServiceUnavailable503
            }
            "AKRIBES-E-PROVIDER-GATEWAY-TIMEOUT-504" => ErrorCode::ProviderGatewayTimeout504,
            "AKRIBES-E-PROVIDER-NETWORK" => ErrorCode::ProviderNetwork,
            "AKRIBES-E-PROVIDER-PARSE" => ErrorCode::ProviderParse,
            "AKRIBES-E-PROVIDER-OTHER" => ErrorCode::ProviderOther,
            "AKRIBES-E-INTERNAL-PANIC" => ErrorCode::InternalPanic,
            "AKRIBES-E-INTERNAL-DROPPED-CHANNEL" => ErrorCode::InternalDroppedChannel,
            "AKRIBES-E-INTERNAL-DEADLOCK" => ErrorCode::InternalDeadlock,
            "AKRIBES-E-INTERNAL-TASK-ABORTED" => ErrorCode::InternalTaskAborted,
            "AKRIBES-E-INTERNAL-OTHER" => ErrorCode::InternalOther,
            "AKRIBES-E-SCRIPT-ERROR" => ErrorCode::ScriptError,
            "AKRIBES-E-SCRIPT-DEPTH" => ErrorCode::ScriptDepthExceeded,
            "AKRIBES-E-RETRY-PARTIAL-EXHAUSTED" => ErrorCode::PartialRetryExhausted,
            "AKRIBES-E-AUTHOR-RAISE" => ErrorCode::AuthorRaise,
            "AKRIBES-E-TOOL-BUDGET" => ErrorCode::ToolBudgetExceeded,
            "AKRIBES-E-TOOL-APPROVAL-PROTOCOL" => ErrorCode::ToolApprovalProtocol,
            "AKRIBES-E-TOOL-NO-REGISTRY" => ErrorCode::ToolNoRegistry,
            "AKRIBES-E-TOOL-ERROR" => ErrorCode::ToolError,
            "AKRIBES-E-AGENT-TOOLS-DOUBLE-DISPATCH" => ErrorCode::AgentToolsDoubleDispatch,
            "AKRIBES-E-CONFIG-MISSING" => ErrorCode::ConfigMissing,
            "AKRIBES-E-LOOP-OUTPUT-BUDGET-EXCEEDED" => ErrorCode::LoopOutputBudgetExceeded,
            "AKRIBES-E-LOOP-MULTI-CHECKPOINT" => ErrorCode::LoopMultiCheckpoint,
            "AKRIBES-E-CONTEXT-OVERFLOW" => ErrorCode::ContextOverflow,
            "AKRIBES-E-CONTEXT-NATIVE-UNSUPPORTED" => ErrorCode::ContextNativeUnsupported,
            "AKRIBES-E-CONTEXT-COMPACTION-EXHAUSTED" => ErrorCode::ContextCompactionExhausted,
            "AKRIBES-E-COMPACTION-THRESHOLD-INVALID" => ErrorCode::CompactionThresholdInvalid,
            "AKRIBES-E-COMPACTOR-SIGNATURE" => ErrorCode::CompactorSignature,
            "AKRIBES-E-COMPACTION-LOOP-ONLY" => ErrorCode::CompactionLoopOnly,
            "AKRIBES-E-STD-FORMAT-MISS-001" => ErrorCode::StdFormatMissing,
            "AKRIBES-E-STD-FORMAT-SYNTAX-001" => ErrorCode::StdFormatSyntax,
            "AKRIBES-E-STD-JSON-PARSE-001" => ErrorCode::StdJsonParse,
            "AKRIBES-E-STD-JSON-STRINGIFY-001" => ErrorCode::StdJsonStringify,
            "AKRIBES-E-STD-REGEX-001" => ErrorCode::StdRegexInvalid,
            "AKRIBES-E-OTHER" => {
                // Issue #1039: `AKRIBES-E-OTHER` is the explicit
                // unclassified-fallback bucket. Decoding a wire payload
                // tagged with it is legal, but every occurrence is a hint
                // that an upstream producer skipped a more specific code.
                // Surface that via a warn-log so prod can attribute the
                // drift to the producing component (server / SDK / runner)
                // rather than silently flattening to `ScriptError`.
                tracing::warn!(
                    target: "akribes_types::error",
                    wire_code = "AKRIBES-E-OTHER",
                    "decoded fallback ErrorCode::Other from wire payload —                      the producing component skipped a more specific AKRIBES-E-* code"
                );
                ErrorCode::Other
            }
            _ => return None,
        };
        Some(code)
    }

    /// Default user-facing message for this code. Constructors should
    /// override only when there is meaningfully more to say to the user
    /// (e.g. embedding the offending value), not just to restate the
    /// developer message.
    pub fn default_user_message(&self) -> &'static str {
        match self {
            ErrorCode::UserCancelled => "The execution was cancelled.",
            ErrorCode::ExecutionTimeout => {
                "The workflow ran past its time budget. Try a smaller input, simplify the workflow, or raise AKRIBES_EXECUTION_TIMEOUT."
            }
            ErrorCode::CheckpointTimeout => {
                "A checkpoint waited longer than its on_timeout window without a resume."
            }
            ErrorCode::ProviderRateLimit => {
                "The model provider rate-limited the request. Wait a moment and retry; consider lowering concurrency."
            }
            ErrorCode::ProviderAuth => {
                "The model provider rejected our credentials. Check the provider's API key and that the configured model is enabled."
            }
            ErrorCode::ProviderTokenLimit => {
                "The prompt exceeds the model's context window. Reduce input length, use a larger-context model, or split the work."
            }
            ErrorCode::ProviderServer => {
                "The model provider returned a server-side error. Retrying is usually appropriate."
            }
            ErrorCode::ProviderServer500 => {
                "The model provider returned HTTP 500. The origin reported an internal error; a retry with a short backoff is usually appropriate."
            }
            ErrorCode::ProviderBadGateway502 => {
                "The model provider returned HTTP 502 (bad gateway). The edge fronted a failing origin; retry with a short backoff."
            }
            ErrorCode::ProviderServiceUnavailable503 => {
                "The model provider returned HTTP 503 (service unavailable). This is rate-limit-adjacent — honour Retry-After if the provider sent one, otherwise back off."
            }
            ErrorCode::ProviderGatewayTimeout504 => {
                "The model provider returned HTTP 504 (gateway timeout). The upstream is slow or stuck; retry with a longer backoff before alerting."
            }
            ErrorCode::ProviderNetwork => {
                "Could not reach the model provider (network/DNS/TLS/timeout). Retry; check connectivity if it persists."
            }
            ErrorCode::ProviderParse => {
                "The model produced output that didn't fit the declared schema. Check the prompt and the type definition."
            }
            ErrorCode::ProviderOther => "The model provider failed with an unclassified error.",
            ErrorCode::InternalPanic => {
                "An internal Akribes task crashed (AKRIBES-E-INTERNAL-PANIC). \
                 This is a bug. Report with the execution id at \
                 https://github.com/PodestaAI/akribes-sdks/issues."
            }
            ErrorCode::InternalDroppedChannel => {
                "An internal Akribes channel was closed unexpectedly (AKRIBES-E-INTERNAL-DROPPED-CHANNEL). \
                 This is usually a bug. Report with the execution id at \
                 https://github.com/PodestaAI/akribes-sdks/issues."
            }
            ErrorCode::InternalDeadlock => {
                "Akribes detected a stuck workflow graph (AKRIBES-E-INTERNAL-DEADLOCK). \
                 This is a compiler/engine bug. Report at \
                 https://github.com/PodestaAI/akribes-sdks/issues."
            }
            ErrorCode::InternalTaskAborted => {
                "An internal task was aborted unexpectedly (AKRIBES-E-INTERNAL-TASK-ABORTED). \
                 This is usually a bug. Report at \
                 https://github.com/PodestaAI/akribes-sdks/issues."
            }
            ErrorCode::InternalOther => {
                "An unspecified internal error occurred (AKRIBES-E-INTERNAL-OTHER). \
                 Report with the execution id at \
                 https://github.com/PodestaAI/akribes-sdks/issues."
            }
            ErrorCode::ScriptError => {
                "The workflow encountered a runtime error. Check task logic, types, and inputs."
            }
            ErrorCode::ScriptDepthExceeded => {
                "Workflow call(...) chain exceeded the recursion cap. Refactor to reduce nesting."
            }
            ErrorCode::PartialRetryExhausted => {
                "All validation retries on a partial-retry task were exhausted."
            }
            ErrorCode::AuthorRaise => {
                "The workflow's failure path fired (the LLM returned an Unable or non-success variant the script mapped to fail)."
            }
            ErrorCode::ToolBudgetExceeded => {
                "An agent exceeded its tool_budget cap. Increase the cap or reduce tool use."
            }
            ErrorCode::ToolApprovalProtocol => {
                "Tool approval received an unexpected payload. This is a host-integration bug."
            }
            ErrorCode::ToolNoRegistry => {
                "A tool call was attempted but no MCP registry is attached. Configure mcp_server / mcp_registry, or run via a host that wires the registry."
            }
            ErrorCode::ToolError => {
                "An MCP tool returned an error. Check tool configuration and the upstream service."
            }
            ErrorCode::AgentToolsDoubleDispatch => {
                "An agent invoked tools more than once in a single dispatch. Agents are single-round-trip — use a `loop` block for multi-turn tool use."
            }
            ErrorCode::ConfigMissing => {
                "Required configuration is missing (API key, env var, or provider setup)."
            }
            ErrorCode::LoopOutputBudgetExceeded => {
                "A `loop` block exceeded its `max_total_output_tokens` cap. Raise the cap or shorten per-turn output."
            }
            ErrorCode::LoopMultiCheckpoint => {
                "A loop turn fired more than one checkpoint. One checkpoint per turn is the supported envelope — split them across turns or move one outside the loop."
            }
            ErrorCode::ContextOverflow => {
                "The conversation exceeds the model's context window. Configure `compaction:` on the agent (e.g. `compaction: at 80%`) or pick a model with a larger window."
            }
            ErrorCode::ContextNativeUnsupported => {
                "This model doesn't support server-side native compaction. Pick a capable model (opus_4_7, opus_4_6, sonnet_4_6, gpt_5_3_codex, gpt_5_5) or switch to a custom compaction chain."
            }
            ErrorCode::ContextCompactionExhausted => {
                "The compaction chain ran every configured step and the conversation still exceeds the configured cap. Add a terminal step (truncate or native) or raise the cap."
            }
            ErrorCode::CompactionThresholdInvalid => {
                "A compaction threshold is invalid. Use 1..=100 with `%`, or a positive absolute token count."
            }
            ErrorCode::CompactorSignature => {
                "User-defined compactor must have signature `(history: str | list[message]) -> str | list[message]`."
            }
            ErrorCode::CompactionLoopOnly => {
                "`compact_to_state(...)` may only appear inside a loop's `compaction:` block — move it under the loop, or use a different primitive on the agent."
            }
            ErrorCode::StdFormatMissing => {
                "`std.format` is missing a placeholder key. Pass every `{name}` in the template via the `args` map."
            }
            ErrorCode::StdFormatSyntax => {
                "`std.format` template has malformed brace syntax. Use `{name}` for placeholders, `{{` / `}}` for literal braces."
            }
            ErrorCode::StdJsonParse => "`std.json_parse` could not parse the input as JSON.",
            ErrorCode::StdJsonStringify => {
                "`std.json_stringify` could not serialise the value. Check for control-plane values (FatalError) and non-JSON shapes."
            }
            ErrorCode::StdRegexInvalid => {
                "`std.regex_extract` was given an invalid regex pattern. Check the syntax against the Rust `regex` crate's rules."
            }
            ErrorCode::Other => "An error occurred. See the developer message for detail.",
        }
    }
}

/// Where in the workflow an error originated. Every field is optional —
/// fill what you know, leave the rest. SDKs render whichever fields are
/// present; downstream tools (logs, OTel) read them as structured
/// attributes for filtering/aggregation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ErrorSource {
    /// Workflow-author-declared task name (matches `task <name>` in source).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Agent name from the matching `agent <name>` declaration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Provider id when the error came from an LLM/provider call
    /// (`anthropic`, `google`, `openai`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model alias (`opus_4_7`, `gpt_4o_mini`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// MCP `<alias>.<tool>` reference when the error came from a tool call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_ref: Option<String>,
    /// Script name when the error came from a sub-script (`call(...)`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// 1-indexed source line in the originating `.akr` file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

impl ErrorSource {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// Builder helpers — chainable, infallible.
    pub fn with_task(mut self, task: impl Into<String>) -> Self {
        self.task = Some(task.into());
        self
    }
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
    pub fn with_tool_ref(mut self, tool_ref: impl Into<String>) -> Self {
        self.tool_ref = Some(tool_ref.into());
        self
    }
    pub fn with_script(mut self, script: impl Into<String>) -> Self {
        self.script = Some(script.into());
        self
    }
    pub fn with_line(mut self, line: u32) -> Self {
        self.line = Some(line);
        self
    }
}

/// Structured failure detail attached to a [`crate::value::Value::FatalError`] and to
/// every `EngineEvent::Error`. Replaces the previous `(message, kind)`
/// shape with a richer envelope so SDKs can decide what to do, users get
/// actionable text, and developers get structured fields for OTel/logs.
///
/// Construction patterns:
///
/// * Simple: `ErrorDetail::from_kind(ErrorKind::ScriptError, "div by zero")`
///   — pulls a generic code (`AKRIBES-E-SCRIPT-ERROR`) and the kind's default
///   user message.
/// * Specific: `ErrorDetail::new(ErrorCode::ProviderRateLimit, "...")`
///   — code drives kind + default user_message via [`ErrorCode::kind`].
/// * With retry hint: `.with_retry_after_ms(30_000)`.
/// * With source: `.with_source(ErrorSource::default().with_task("foo"))`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub kind: ErrorKind,
    pub code: ErrorCode,
    /// Developer-facing message — full detail, may include sanitized
    /// stack/protocol fragments. Always non-empty.
    pub message: String,
    /// User-facing single-paragraph summary + suggested action. Always
    /// non-empty (defaults to [`ErrorCode::default_user_message`]).
    pub user_message: String,
    /// When the provider supplied a `Retry-After` (or equivalent), the
    /// suggested wait in milliseconds. None when not known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    /// Where the error originated. Empty (`is_empty()`) when no
    /// attribution is available.
    #[serde(skip_serializing_if = "ErrorSource::is_empty", default)]
    pub source: ErrorSource,
}

impl ErrorDetail {
    /// Construct from a code + developer message. Kind and user_message
    /// are derived from the code.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            kind: code.kind(),
            code,
            message: message.into(),
            user_message: code.default_user_message().to_string(),
            retry_after_ms: None,
            source: ErrorSource::default(),
        }
    }

    /// Construct from an existing kind when no specific code is yet
    /// available. Picks the closest "Other" code for that kind, and for
    /// rate-limit/server messages also extracts a `retry_after_ms` hint
    /// when the upstream response embedded one.
    pub fn from_kind(kind: ErrorKind, message: impl Into<String>) -> Self {
        let message = message.into();
        let code = match kind {
            ErrorKind::RateLimit => ErrorCode::ProviderRateLimit,
            ErrorKind::AuthError => ErrorCode::ProviderAuth,
            ErrorKind::TokenLimit => ErrorCode::ProviderTokenLimit,
            ErrorKind::ServerError500 => ErrorCode::ProviderServer500,
            ErrorKind::BadGateway502 => ErrorCode::ProviderBadGateway502,
            ErrorKind::ServiceUnavailable503 => ErrorCode::ProviderServiceUnavailable503,
            ErrorKind::GatewayTimeout504 => ErrorCode::ProviderGatewayTimeout504,
            ErrorKind::NetworkError => ErrorCode::ProviderNetwork,
            ErrorKind::ParseError => ErrorCode::ProviderParse,
            ErrorKind::Cancelled => ErrorCode::UserCancelled,
            ErrorKind::Timeout => ErrorCode::ExecutionTimeout,
            ErrorKind::ScriptError => ErrorCode::ScriptError,
            ErrorKind::AuthorRaise => ErrorCode::AuthorRaise,
            ErrorKind::ScriptDepthExceeded => ErrorCode::ScriptDepthExceeded,
            ErrorKind::Panic => ErrorCode::InternalPanic,
            ErrorKind::Internal => ErrorCode::InternalOther,
        };
        // Best-effort retry hint extraction for transient kinds. Cheap
        // (single substring scan) and only relevant for kinds that
        // would benefit from the hint.
        let retry_after_ms = if matches!(
            kind,
            ErrorKind::RateLimit
                | ErrorKind::ServerError500
                | ErrorKind::BadGateway502
                | ErrorKind::ServiceUnavailable503
                | ErrorKind::GatewayTimeout504
                | ErrorKind::NetworkError
        ) {
            ErrorCode::parse_retry_after_ms(&message)
        } else {
            None
        };
        Self {
            kind,
            code,
            message,
            user_message: code.default_user_message().to_string(),
            retry_after_ms,
            source: ErrorSource::default(),
        }
    }

    /// Override the user-facing message. Use when the default for the
    /// code isn't specific enough (e.g. embedding the offending value).
    pub fn with_user_message(mut self, msg: impl Into<String>) -> Self {
        self.user_message = msg.into();
        self
    }

    pub fn with_retry_after_ms(mut self, ms: u64) -> Self {
        self.retry_after_ms = Some(ms);
        self
    }

    pub fn with_source(mut self, source: ErrorSource) -> Self {
        self.source = source;
        self
    }

    /// Convenience: builder-style task attribution.
    pub fn with_task(mut self, task: impl Into<String>) -> Self {
        self.source.task = Some(task.into());
        self
    }

    /// Whether retrying as-is may succeed. Pulls from the kind plus
    /// `retry_after_ms` (an explicit hint always implies retryable).
    pub fn is_retryable(&self) -> bool {
        self.retry_after_ms.is_some() || self.kind.is_transient()
    }

    pub fn suggested_action(&self) -> SuggestedAction {
        self.kind.suggested_action()
    }
}

impl std::fmt::Display for ErrorDetail {
    /// Renders as `<wire-code>: <message>` so log lines and the legacy
    /// "string error" shape stay readable. Use the structured fields
    /// directly when constructing JSON wire payloads.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_wire(), self.message)
    }
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErrorKind::RateLimit => write!(f, "rate limit"),
            ErrorKind::AuthError => write!(f, "authentication error"),
            ErrorKind::TokenLimit => write!(f, "token limit"),
            ErrorKind::ServerError500 => write!(f, "server error (HTTP 500)"),
            ErrorKind::BadGateway502 => write!(f, "bad gateway (HTTP 502)"),
            ErrorKind::ServiceUnavailable503 => write!(f, "service unavailable (HTTP 503)"),
            ErrorKind::GatewayTimeout504 => write!(f, "gateway timeout (HTTP 504)"),
            ErrorKind::NetworkError => write!(f, "network error"),
            ErrorKind::ParseError => write!(f, "parse error"),
            ErrorKind::Cancelled => write!(f, "cancelled"),
            ErrorKind::Timeout => write!(f, "timeout"),
            ErrorKind::ScriptError => write!(f, "script error"),
            ErrorKind::AuthorRaise => write!(f, "author raise"),
            ErrorKind::ScriptDepthExceeded => write!(f, "script depth exceeded"),
            ErrorKind::Panic => write!(f, "panic"),
            ErrorKind::Internal => write!(f, "internal error"),
        }
    }
}
