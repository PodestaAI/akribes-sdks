/// Error types for the Akribes SDK.

#[derive(Debug, thiserror::Error)]
pub enum AkribesError {
    /// Auth or permission failure (401/403) — retrying will not help.
    #[error("Fatal error: {message}")]
    Fatal {
        message: String,
        execution_id: Option<String>,
    },
    /// Rate-limit or server unavailability (429/500/502/503/504) — safe
    /// to retry. `retry_after` carries the `Retry-After` header when the
    /// server sent one as numeric seconds (HTTP-date form ignored,
    /// matching Python). `status` carries the HTTP status code so callers
    /// can branch on the specific upstream signal after #1296 split the
    /// 5xx variants by retry semantics (500/502 short, 503 rate-limit-adjacent,
    /// 504 long). `None` when the underlying failure has no HTTP status
    /// (e.g. polling timeout, SSE disconnect).
    #[error("Transient error: {message}")]
    Transient {
        message: String,
        execution_id: Option<String>,
        /// Parsed `Retry-After` header in seconds (#1009). `None` when the
        /// server omitted the header or sent HTTP-date form.
        retry_after: Option<std::time::Duration>,
        /// HTTP status code that produced this error. `None` for non-HTTP
        /// transients (e.g. SSE disconnect, polling deadline). Use
        /// [`Self::recommended_backoff_ms`] to pick the per-status base
        /// backoff (#1296).
        status: Option<u16>,
    },
    /// The Akribes script itself failed or was cancelled.
    #[error("Script error: {message}")]
    Script {
        message: String,
        execution_id: Option<String>,
    },
    /// Polling for an execution result exceeded the caller-supplied timeout.
    #[error("Execution timed out")]
    Timeout { execution_id: Option<String> },
    /// Underlying HTTP transport error.
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    /// JSON (de)serialisation error.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Script schema changed since init() — re-register to continue.
    #[error("Script \"{script_name}\" schema has changed since init(). Re-register to continue.")]
    ScriptSchemaChanged { script_name: String },
    /// Document keys don't match the cached script input schema.
    #[error("Script \"{script_name}\" input mismatch: missing={missing:?}, extra={extra:?}")]
    ScriptInputMismatch {
        script_name: String,
        missing: Vec<String>,
        extra: Vec<String>,
    },
    /// A project-scoped operation was called on a client built without
    /// `project_id`. Use `AkribesClient::new(...)` or set `.project_id(...)`
    /// on the builder.
    #[error("project_id is required for this operation but was not set on the client")]
    MissingProjectId,
    /// HTTP error with a non-success status code.
    #[error("HTTP {status}: {message}")]
    HttpStatus { status: u16, message: String },
    /// HTTP 409 with `error_type=suite_already_exists`. Carries the
    /// existing row id so callers can redirect the operator.
    #[error("Already exists: {message} (existing id {existing_id})")]
    AlreadyExists { message: String, existing_id: i64 },
    /// Any other client-side error.
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, AkribesError>;

/// One entry in the server's `input_validation_failed` 400 body (#1017).
/// Mirrors TS `InputValidationErrorEntry` and Python `InputValidationEntry`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct InputValidationEntry {
    /// Dotted / bracketed path to the offending field, e.g. `"payload.b"`,
    /// `"items[2].qty"`.
    pub input: String,
    /// One of: `"missing" | "wrong_type" | "unknown_field" |
    /// "unknown_input" | "disallowed_type"`.
    pub code: String,
    #[serde(default)]
    pub expected: Option<String>,
    #[serde(default)]
    pub got: Option<String>,
}

#[derive(serde::Deserialize)]
struct InputValidationBody {
    error: String,
    errors: Vec<InputValidationEntry>,
}

/// Parse a 400 `input_validation_failed` body off an [`AkribesError::HttpStatus`].
/// Returns `None` when the error is something else or the body doesn't match.
///
/// Mirrors TS `tryParseInputValidationErrors` (#1017). Form-style UIs use
/// this to map per-field errors back to inputs without regex-matching the
/// text message.
pub fn parse_input_validation_errors(err: &AkribesError) -> Option<Vec<InputValidationEntry>> {
    let body = match err {
        AkribesError::HttpStatus {
            status: 400,
            message,
        } => message,
        _ => return None,
    };
    // The `message` field on HttpStatus is the raw body when it's not empty
    // (see `client::send`). If it's an `HTTP 400 Bad Request`-style prefix,
    // it won't parse as JSON anyway, so the from_str call returns None.
    // Try the body as JSON first; then strip any `HTTP 400: ` prefix.
    let json = serde_json::from_str::<InputValidationBody>(body)
        .ok()
        .or_else(|| {
            body.strip_prefix("HTTP 400: ")
                .and_then(|rest| serde_json::from_str::<InputValidationBody>(rest).ok())
        })?;
    if json.error != "input_validation_failed" {
        return None;
    }
    Some(json.errors)
}

impl AkribesError {
    /// Method form of [`parse_input_validation_errors`].
    pub fn parse_input_validation_errors(&self) -> Option<Vec<InputValidationEntry>> {
        parse_input_validation_errors(self)
    }

    /// Recommended base backoff (in milliseconds) for a transient HTTP
    /// status (#1296). Mirrors `ErrorKind::base_backoff_ms` on the core
    /// side and `recommendedBackoffMs` in the TS SDK, so retry cadences
    /// agree across the stack:
    ///
    /// | Status | Base (ms) | Rationale                            |
    /// |--------|-----------|--------------------------------------|
    /// | 429    | 2000      | Rate-limit; honour Retry-After       |
    /// | 500    | 1000      | Maybe-transient origin error         |
    /// | 502    | 1000      | Edge fronted failing origin          |
    /// | 503    | 2000      | Rate-limit-adjacent capacity issue   |
    /// | 504    | 4000      | Slow upstream — longer base          |
    ///
    /// Returns `None` for any status the SDK doesn't classify as
    /// retriable (or for non-HTTP transients).
    pub fn recommended_backoff_ms(status: u16) -> Option<u64> {
        Some(match status {
            429 => 2_000,
            500 => 1_000,
            502 => 1_000,
            503 => 2_000,
            504 => 4_000,
            _ => return None,
        })
    }

    /// Return the HTTP status if this error is a transient with a known
    /// status code (#1296). Returns `None` for non-Transient variants or
    /// for transients without a status (e.g. SSE disconnect, polling
    /// timeout).
    pub fn transient_status(&self) -> Option<u16> {
        match self {
            AkribesError::Transient { status, .. } => *status,
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AkribesError, parse_input_validation_errors};

    #[test]
    fn already_exists_renders_message_with_id() {
        let err = AkribesError::AlreadyExists {
            message: "Script 'foo' already has a suite".to_string(),
            existing_id: 42,
        };
        let s = format!("{err}");
        assert!(s.contains("foo"));
        assert!(s.contains("42"));
    }

    #[test]
    fn parse_input_validation_errors_decodes_per_field_codes() {
        let body = r#"{"error":"input_validation_failed","errors":[{"input":"x","code":"missing","expected":"int"},{"input":"y","code":"wrong_type","got":"str"}]}"#;
        let err = AkribesError::HttpStatus {
            status: 400,
            message: body.to_string(),
        };
        let parsed = parse_input_validation_errors(&err).expect("parses");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].input, "x");
        assert_eq!(parsed[0].code, "missing");
        assert_eq!(parsed[0].expected.as_deref(), Some("int"));
        assert_eq!(parsed[1].input, "y");
        assert_eq!(parsed[1].code, "wrong_type");
        assert_eq!(parsed[1].got.as_deref(), Some("str"));
    }

    #[test]
    fn parse_input_validation_errors_returns_none_on_unrelated_400() {
        let err = AkribesError::HttpStatus {
            status: 400,
            message: r#"{"error":"something_else"}"#.to_string(),
        };
        assert!(parse_input_validation_errors(&err).is_none());
    }

    #[test]
    fn parse_input_validation_errors_method_form() {
        let err = AkribesError::HttpStatus {
            status: 400,
            message:
                r#"{"error":"input_validation_failed","errors":[{"input":"a","code":"missing"}]}"#
                    .to_string(),
        };
        assert!(err.parse_input_validation_errors().is_some());
    }
}
