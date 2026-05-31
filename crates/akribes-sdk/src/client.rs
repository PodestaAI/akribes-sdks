/// The main Akribes SDK client.
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::RwLock;

use crate::error::{AkribesError, Result};

// ── Shared state ────────────────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct Inner {
    pub(crate) base_url: String,
    pub(crate) project_id: Option<i64>,
    pub(crate) name: String,
    pub(crate) id: String,
    pub(crate) http: reqwest::Client,
    pub(crate) token: Arc<RwLock<Option<String>>>,
    /// Optional `X-Akribes-User` header value for metrics attribution. Set
    /// when a backend with a service token wants per-end-user accounting.
    /// Advisory only — does not grant any permission.
    pub(crate) on_behalf_of: Arc<RwLock<Option<String>>>,
    pub(crate) heartbeat_handle: Mutex<Option<tokio::task::AbortHandle>>,
    /// Set to `true` when the client is being dropped / destroyed so the
    /// heartbeat task can stop cleanly without a TOCTOU race.
    pub(crate) shutdown: Arc<AtomicBool>,
    /// Cached input schemas per script (populated by init()).
    pub(crate) schema_cache: Mutex<HashMap<String, Vec<(String, String)>>>,
    /// Scripts whose schema has changed since init (marked by SSE events).
    pub(crate) broken_scripts: Mutex<HashSet<String>>,
    /// Maximum time `documents().ingest()` will keep polling a still-converting
    /// blob before surfacing `AkribesError::Transient`. See
    /// [`AkribesClientBuilder::ingest_poll_timeout`].
    pub(crate) ingest_poll_timeout: Duration,
}

/// Default request timeout for the internal HTTP client. Individual requests
/// can still exceed this via their own `reqwest::RequestBuilder::timeout`.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Default TCP/TLS connect timeout for the internal HTTP client.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default poll budget for [`crate::sub::documents::DocumentsClient::ingest`].
///
/// Multi-page real-world PDFs going through the VLM conversion path routinely
/// take 1–5 minutes server-side; the previous 30 s ceiling forced every SDK
/// consumer to handle `AkribesError::Transient` themselves with their own retry
/// loop. 300 s (5 min) covers the long tail comfortably without making a
/// server-side hang invisible.
///
/// Override per-client with [`AkribesClientBuilder::ingest_poll_timeout`], or
/// set `AKRIBES_SDK_INGEST_TIMEOUT_SECS` in the process environment for a global
/// override that takes effect at client construction. The builder param wins
/// over the env var.
pub(crate) const DEFAULT_INGEST_POLL_TIMEOUT_SECS: u64 = 300;

/// Read `AKRIBES_SDK_INGEST_TIMEOUT_SECS` from the environment.
///
/// Returns `None` if unset or unparseable (in which case the caller falls back
/// to [`DEFAULT_INGEST_POLL_TIMEOUT_SECS`]). A value of `0` is rejected too —
/// "ingest with zero deadline" is never the user's intent and would surface as
/// an immediate `Transient` on the first poll iteration; misconfiguration
/// should fall back to the default rather than silently break ingest.
pub(crate) fn ingest_poll_timeout_from_env() -> Option<Duration> {
    let raw = std::env::var("AKRIBES_SDK_INGEST_TIMEOUT_SECS").ok()?;
    match raw.trim().parse::<u64>() {
        Ok(0) => None,
        Ok(n) => Some(Duration::from_secs(n)),
        Err(_) => None,
    }
}

/// Build the SDK's default `reqwest::Client` with sensible timeouts so a hung
/// server can never deadlock the caller. Falls back to `Client::new()` if the
/// builder somehow fails (shouldn't happen with these static settings).
fn default_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(DEFAULT_REQUEST_TIMEOUT)
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Cap on bytes read from an error response body (64 KiB). A misbehaving
/// or malicious server can return arbitrarily large error bodies, and
/// buffering them all with `.text()` would let a single bad response
/// OOM the SDK consumer.
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Cap on bytes quoted back in a decode-failure error. Short enough to fit in
/// a single MCP tool response; long enough to eyeball the mismatch.
const DECODE_ERROR_SNIPPET_BYTES: usize = 512;

/// Read a response body lossily, capped at [`MAX_ERROR_BODY_BYTES`].
/// If the cap is hit, a trailing `… (truncated)` marker is appended.
async fn read_body_capped(res: reqwest::Response) -> String {
    use futures::StreamExt;
    let mut buf: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut stream = res.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else { break };
        let remaining = MAX_ERROR_BODY_BYTES.saturating_sub(buf.len());
        if remaining == 0 {
            truncated = true;
            break;
        }
        let take = chunk.len().min(remaining);
        buf.extend_from_slice(&chunk[..take]);
        if take < chunk.len() {
            truncated = true;
            break;
        }
    }
    let mut s = String::from_utf8_lossy(&buf).into_owned();
    if truncated {
        s.push_str("… (truncated)");
    }
    s
}

/// Produce a short UTF-8-lossy snippet of a response body for error messages.
fn body_snippet(bytes: &[u8]) -> String {
    let total = bytes.len();
    let cut = total.min(DECODE_ERROR_SNIPPET_BYTES);
    let s: String = String::from_utf8_lossy(&bytes[..cut]).into_owned();
    if total > cut {
        format!("{s}… (truncated, {total} bytes total)")
    } else {
        s
    }
}

/// Buffer a response body and deserialize it as JSON. On decode failure,
/// returns an [`AkribesError::Other`] that names the target type, the URL,
/// and a short snippet of the actual body — so callers don't just see
/// reqwest's opaque "error decoding response body".
///
/// 404 responses that reach this helper are converted to [`AkribesError::HttpStatus`]
/// rather than attempting to decode them as the happy-path body. Callers that
/// want to treat 404 as absent (GET-by-id, list) should check status *before*
/// calling this helper — see [`AkribesClient::get_opt`] for the pattern.
pub(crate) async fn decode_json<T: serde::de::DeserializeOwned>(
    res: reqwest::Response,
) -> Result<T> {
    let status = res.status();
    let url = res.url().to_string();
    if status == reqwest::StatusCode::NOT_FOUND {
        let body = read_body_capped(res).await;
        let message = if body.trim().is_empty() {
            format!("HTTP 404 Not Found (url: {url})")
        } else {
            format!("HTTP 404 (url: {url}): {body}")
        };
        return Err(AkribesError::HttpStatus {
            status: 404,
            message,
        });
    }
    let bytes = res.bytes().await?;
    serde_json::from_slice::<T>(&bytes).map_err(|e| {
        AkribesError::Other(format!(
            "failed to decode response from {url} as {ty}: {e}; body: {snippet}",
            ty = std::any::type_name::<T>(),
            snippet = body_snippet(&bytes),
        ))
    })
}

// ── AkribesClient ──────────────────────────────────────────────────────────────

/// Typed client for the Akribes workflow platform.
///
/// Cheaply cloneable — all clones share the same HTTP client, auth token, and
/// heartbeat task.
///
/// # Authentication
///
/// akribes-server uses two token types. **Pick one** for the `token` field:
///
/// **1. Service token** (long-lived, env var, full-Admin within scope) — for
/// trusted backends. The secret is the part after `:` in
/// `AKRIBES_SERVICE_TOKEN_<NAME>=<scope>:<secret>` from the server's env.
///
/// ```no_run
/// use akribes_sdk::AkribesClient;
/// let client = AkribesClient::builder("https://akribes.example.com")
///     .project_id(2)
///     .token(std::env::var("AKRIBES_SERVICE_TOKEN").unwrap())
///     .build();
/// ```
///
/// **2. Scoped token** (`akribes_tk_...` — legacy `aura_tk_...` still
/// accepted — expires, revokable) — for browsers,
/// CLIs, or anything you don't want to give a long-lived secret. Mint one via
/// [`AkribesClient::tokens`]'s [`mint`](crate::sub::tokens::TokensClient::mint):
///
/// ```no_run
/// # use akribes_sdk::{AkribesClient, models::{MintTokenRequest, TokenScopes, ProjectScope, TokenRole, WildcardMarker}};
/// # async fn ex(backend: AkribesClient) -> Result<(), Box<dyn std::error::Error>> {
/// // From a backend holding a service token, mint a scoped token for a user:
/// let minted = backend.tokens().mint(&MintTokenRequest {
///     user_email: Some("alice@acme.com".to_string()),
///     scopes: TokenScopes {
///         projects: ProjectScope::Wildcard(WildcardMarker),
///         role: TokenRole::Admin,
///         scripts: None,
///         executions: None,
///         can_mint: false,
///         features: vec![],
///         org_id: None,
///     },
///     expires_in: 8 * 3600, // 8h browser session
///     label: "web-session".to_string(),
/// }).await?;
/// // Ship `minted.token` to the browser.
/// # Ok(()) }
/// ```
///
/// Set `X-Akribes-User` for metrics attribution when a backend acts on behalf
/// of a user (advisory header — does not grant permission) via
/// [`AkribesClientBuilder::on_behalf_of`] at construction or
/// [`AkribesClient::set_on_behalf_of`] at runtime. Servers also accept the
/// legacy `X-Aura-User` form for backwards compat with pre-rebrand clients.
#[derive(Clone, Debug)]
pub struct AkribesClient {
    pub(crate) inner: Arc<Inner>,
}

impl AkribesClient {
    /// Create a new project-scoped client.
    ///
    /// Deprecated: prefer [`AkribesClient::builder`], which is now the blessed
    /// constructor and supports a wider range of configurations (no project,
    /// custom http client, etc.).
    #[deprecated(since = "0.4.0", note = "use AkribesClient::builder(base_url) instead")]
    pub fn new(base_url: &str, project_id: i64, name: &str, id: &str) -> Self {
        Self {
            inner: Arc::new(Inner {
                base_url: base_url.trim_end_matches('/').to_string(),
                project_id: Some(project_id),
                name: name.to_string(),
                id: id.to_string(),
                http: default_http_client(),
                token: Arc::new(RwLock::new(None)),
                on_behalf_of: Arc::new(RwLock::new(None)),
                heartbeat_handle: Mutex::new(None),
                shutdown: Arc::new(AtomicBool::new(false)),
                schema_cache: Mutex::new(HashMap::new()),
                broken_scripts: Mutex::new(HashSet::new()),
                ingest_poll_timeout: ingest_poll_timeout_from_env()
                    .unwrap_or(Duration::from_secs(DEFAULT_INGEST_POLL_TIMEOUT_SECS)),
            }),
        }
    }

    /// Create a builder for more control over client construction.
    ///
    /// ```no_run
    /// # use akribes_sdk::AkribesClient;
    /// let client = AkribesClient::builder("http://localhost:3001")
    ///     .project_id(1)
    ///     .name("my-service")
    ///     .token("aura_abc123")
    ///     .build();
    /// ```
    pub fn builder(base_url: impl Into<String>) -> AkribesClientBuilder {
        AkribesClientBuilder {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            project_id: None,
            name: None,
            id: None,
            token: None,
            on_behalf_of: None,
            http_client: None,
            ingest_poll_timeout: None,
        }
    }

    /// Update the authentication token at runtime.
    ///
    /// Pass `None` to clear the token (requests will be unauthenticated).
    pub async fn set_token(&self, token: Option<String>) {
        *self.inner.token.write().await = token;
    }

    /// Set or clear the `X-Akribes-User` header sent on every outbound
    /// request, used by the server for metrics attribution when a backend
    /// (typically holding a service token) acts on behalf of an end user.
    ///
    /// **This header is advisory — it does not grant any permission.**
    /// Authorization remains based on the bearer token's scope. Servers also
    /// honor the legacy `X-Aura-User` form for backwards compat with
    /// pre-rebrand clients, but new code should not rely on that.
    ///
    /// Mirrors [`AkribesClientBuilder::on_behalf_of`] for runtime updates
    /// (e.g. when the same long-lived client services many users).
    pub async fn set_on_behalf_of(&self, email: Option<String>) {
        *self.inner.on_behalf_of.write().await = email;
    }

    /// Return the configured project ID, or `None` if this is a global client.
    pub fn project_id(&self) -> Option<i64> {
        self.inner.project_id
    }

    /// Return the base URL this client points at (no trailing slash). Useful
    /// for callers that need to compose a URL outside the typed sub-clients —
    /// e.g. the MCP bench tools poll an SSE endpoint as a plain GET to keep
    /// their wire-shape contract intact.
    pub fn base_url(&self) -> &str {
        &self.inner.base_url
    }

    /// Authenticated raw GET that returns the response body as a generic
    /// `serde_json::Value`. Goes through the same auth + telemetry pipeline
    /// as every typed call; only the deserialisation step is generic.
    ///
    /// 404 surfaces as [`AkribesError::HttpStatus`] (the typed callers convert
    /// it to `Ok(None)` via their own wrappers — for the raw form we keep the
    /// status visible so callers can branch on absence vs. unrelated failures).
    pub async fn get_json_value(&self, url: &str) -> Result<serde_json::Value> {
        let res = self.send(self.inner.http.get(url)).await?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(AkribesError::HttpStatus {
                status: 404,
                message: format!("GET {url} returned 404"),
            });
        }
        decode_json(res).await
    }

    /// 404-tolerant variant of [`AkribesClient::get_json_value`]. Returns
    /// `Ok(None)` instead of surfacing the 404 as an error — useful when
    /// the caller's notion of "absence" is a legitimate response.
    pub async fn get_json_value_opt(&self, url: &str) -> Result<Option<serde_json::Value>> {
        let res = self.send(self.inner.http.get(url)).await?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(decode_json(res).await?))
    }

    /// Authenticated raw POST with a JSON body, returning the response body
    /// as a generic `serde_json::Value`. Companion to
    /// [`AkribesClient::get_json_value`] for endpoints whose response shape
    /// isn't (yet) typed in [`crate::models`].
    pub async fn post_json_value(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let res = self.send(self.inner.http.post(url).json(body)).await?;
        if res.status().as_u16() == 204 || res.content_length() == Some(0) {
            return Ok(serde_json::Value::Null);
        }
        decode_json(res).await
    }

    /// Return the configured `documents().ingest()` poll timeout. Useful for
    /// inspecting the resolved value (builder override → env var → default)
    /// from tests or diagnostics.
    pub fn ingest_poll_timeout(&self) -> Duration {
        self.inner.ingest_poll_timeout
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    /// Build a request with the current auth token + on-behalf-of header
    /// injected.
    pub(crate) async fn authed(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let token_guard = self.inner.token.read().await;
        let mut builder = match token_guard.as_deref() {
            Some(t) => builder.bearer_auth(t),
            None => builder,
        };
        drop(token_guard);
        let obo_guard = self.inner.on_behalf_of.read().await;
        if let Some(email) = obo_guard.as_deref() {
            // Servers also honor the legacy `X-Aura-User` form for backwards
            // compat with pre-rebrand clients, but new clients emit the
            // Akribes form. Header is advisory only — authz comes from the
            // bearer token's scope.
            builder = builder.header("X-Akribes-User", email);
        }
        builder
    }

    /// Send a request, classifying 4xx/5xx status codes into typed errors.
    pub(crate) async fn send(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let req = self.authed(req).await;
        let res = req.send().await?;
        let status = res.status();

        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            let body = read_body_capped(res).await;
            let message = if body.trim().is_empty() {
                format!(
                    "HTTP {} {}",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or("")
                )
            } else {
                format!("HTTP {}: {}", status.as_u16(), body)
            };
            return Err(AkribesError::Fatal {
                message,
                execution_id: None,
            });
        }

        // #1296: all 4 transient 5xx statuses get the same dispatch path —
        // the per-status retry semantics are exposed by the new
        // `status` field on `AkribesError::Transient` so callers can
        // pick a base backoff via `AkribesError::recommended_backoff_ms`.
        if status.as_u16() == 429
            || status == reqwest::StatusCode::INTERNAL_SERVER_ERROR
            || status == reqwest::StatusCode::BAD_GATEWAY
            || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
            || status == reqwest::StatusCode::GATEWAY_TIMEOUT
        {
            // Parse Retry-After before consuming the response (#1009).
            // Numeric seconds only; HTTP-date form is ignored to match the
            // Python SDK's behavior.
            let retry_after = res
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(std::time::Duration::from_secs);
            let body = read_body_capped(res).await;
            let message = if body.trim().is_empty() {
                format!(
                    "HTTP {} {}",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or("")
                )
            } else {
                format!("HTTP {}: {}", status.as_u16(), body)
            };
            return Err(AkribesError::Transient {
                message,
                execution_id: None,
                retry_after,
                status: Some(status.as_u16()),
            });
        }

        if !status.is_success() && status != reqwest::StatusCode::NOT_FOUND {
            let msg = read_body_capped(res).await;
            if status.as_u16() == 409 {
                if let Ok(body) = serde_json::from_str::<serde_json::Value>(&msg) {
                    if body.get("error_type").and_then(|v| v.as_str())
                        == Some("suite_already_exists")
                    {
                        if let Some(id) = body.get("existing_suite_id").and_then(|v| v.as_i64()) {
                            return Err(AkribesError::AlreadyExists {
                                message: body
                                    .get("error")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("already exists")
                                    .to_string(),
                                existing_id: id,
                            });
                        }
                    }
                }
            }
            return Err(AkribesError::HttpStatus {
                status: status.as_u16(),
                message: msg,
            });
        }

        Ok(res)
    }

    /// `GET` — returns `None` on 404, deserialises body otherwise.
    pub(crate) async fn get_opt<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<Option<T>> {
        let res = self.send(self.inner.http.get(url)).await?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(decode_json(res).await?))
    }

    /// `GET` — deserialises body as a list, treats 404 as empty.
    pub(crate) async fn get_list<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<Vec<T>> {
        Ok(self.get_opt::<Vec<T>>(url).await?.unwrap_or_default())
    }

    /// Append a serde-serializable query struct to a URL. Fields set to
    /// `None` are skipped (via `#[serde(skip_serializing_if = "Option::is_none")]`).
    pub(crate) fn url_with_query<Q: Serialize>(base: &str, q: &Q) -> String {
        match serde_urlencoded::to_string(q) {
            Ok(qs) if !qs.is_empty() => format!("{base}?{qs}"),
            _ => base.to_string(),
        }
    }

    /// `POST` with a JSON body — returns deserialised response.
    pub(crate) async fn post<B: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T> {
        let res = self.send(self.inner.http.post(url).json(body)).await?;
        decode_json(res).await
    }

    /// `PATCH` with a JSON body — returns deserialised response.
    pub(crate) async fn patch<B: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T> {
        let res = self.send(self.inner.http.patch(url).json(body)).await?;
        decode_json(res).await
    }

    /// `PATCH` with a JSON body — expects no body in the response.
    pub(crate) async fn patch_empty<B: Serialize>(&self, url: &str, body: &B) -> Result<()> {
        self.send(self.inner.http.patch(url).json(body)).await?;
        Ok(())
    }

    /// `PUT` with a JSON body — expects no body in the response.
    pub(crate) async fn put_empty<B: Serialize>(&self, url: &str, body: &B) -> Result<()> {
        self.send(self.inner.http.put(url).json(body)).await?;
        Ok(())
    }

    /// `PUT` with a JSON body — returns deserialised response.
    pub(crate) async fn put_json<B: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T> {
        let res = self.send(self.inner.http.put(url).json(body)).await?;
        decode_json(res).await
    }

    /// `DELETE` — returns `true` if deleted, `false` if already absent (404).
    pub(crate) async fn delete(&self, url: &str) -> Result<bool> {
        let res = self.send(self.inner.http.delete(url)).await?;
        Ok(res.status() != reqwest::StatusCode::NOT_FOUND)
    }

    /// `DELETE` — returns deserialised response body.
    pub(crate) async fn delete_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let res = self.send(self.inner.http.delete(url)).await?;
        decode_json(res).await
    }

    /// `POST` with a multipart form body — returns deserialised response.
    pub(crate) async fn post_multipart<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        form: reqwest::multipart::Form,
    ) -> Result<T> {
        let res = self.send(self.inner.http.post(url).multipart(form)).await?;
        decode_json(res).await
    }
}

// ── Sub-client accessors ─────────────────────────────────────────────────

impl AkribesClient {
    /// Project management (list, create, update, delete). Global resource.
    pub fn projects(&self) -> crate::sub::projects::ProjectsClient {
        crate::sub::projects::ProjectsClient::new(Arc::clone(&self.inner))
    }

    /// Script execution. Global methods only (get, get_output, get_events,
    /// cancel, resume, document helpers, await_execution).
    ///
    /// For project-scoped methods (run, list, cancel_run, run_from,
    /// run_with_upload, run_with_s3, get_graph, get_cost), use
    /// [`AkribesClient::project`] first.
    pub fn executions(&self) -> crate::sub::executions::ExecutionsClient {
        crate::sub::executions::ExecutionsClient::new(Arc::clone(&self.inner))
    }

    /// Scoped token management (mint, list, revoke). Not project-scoped.
    pub fn tokens(&self) -> crate::sub::tokens::TokensClient {
        crate::sub::tokens::TokensClient::new(Arc::clone(&self.inner))
    }

    /// Document conversion via the server's Docling integration.
    pub fn convert(&self) -> crate::sub::convert::ConvertClient {
        crate::sub::convert::ConvertClient::new(Arc::clone(&self.inner))
    }

    /// Global bench-run operations (anything keyed on the cross-project
    /// `bench_runs.id`): get/delete a run, list results, page events, cancel,
    /// tag-session, compare two runs, promote an execution to a case. These
    /// endpoints live at `/bench-runs/{id}/...` and don't need a project
    /// scope — the server resolves the owning project from the run row.
    ///
    /// Project-scoped bench operations (config CRUD, case CRUD, list/trigger
    /// runs for a script) live on [`ProjectScope::bench`] instead.
    pub fn bench_runs(&self) -> crate::sub::bench::BenchRunsClient {
        crate::sub::bench::BenchRunsClient::new(Arc::clone(&self.inner))
    }

    /// Enter a project scope. The returned [`ProjectScope`] gives infallible
    /// access to all project-scoped sub-clients (`scripts`, `drafts`,
    /// `versions`, `channels`, `evals`, `events`, `registered_clients`).
    ///
    /// ```no_run
    /// # use akribes_sdk::AkribesClient;
    /// # async fn example(client: AkribesClient) -> akribes_sdk::Result<()> {
    /// let scripts = client.project(1).scripts().list().await?;
    /// # Ok(()) }
    /// ```
    pub fn project(&self, project_id: i64) -> ProjectScope {
        ProjectScope {
            inner: Arc::clone(&self.inner),
            project_id,
        }
    }

    /// Shortcut for clients constructed with [`AkribesClientBuilder::project_id`]:
    /// returns a [`ProjectScope`] using the embedded project id, or
    /// `Err(MissingProjectId)` if none was set.
    pub fn scoped(&self) -> Result<ProjectScope> {
        let pid = self
            .inner
            .project_id
            .ok_or(AkribesError::MissingProjectId)?;
        Ok(ProjectScope {
            inner: Arc::clone(&self.inner),
            project_id: pid,
        })
    }

    /// Fetch the global server state (provider env, etc.).
    /// Does **not** require `project_id`.
    pub async fn get_state(&self) -> crate::error::Result<serde_json::Value> {
        let url = format!("{}/state", self.inner.base_url);
        Ok(self
            .get_opt::<serde_json::Value>(&url)
            .await?
            .unwrap_or(serde_json::json!({})))
    }

    /// Fetch the caller's per-user sandbox project id (creates one if missing).
    /// Use this to subscribe to ad-hoc events *before* calling [`run_adhoc`](Self::run_adhoc)
    /// so the first engine events aren't missed.
    pub async fn get_sandbox_project_id(&self) -> crate::error::Result<i64> {
        let url = format!("{}/me/sandbox", self.inner.base_url);
        let body: crate::models::SandboxProjectIdResponse =
            self.send(self.inner.http.get(&url)).await?.json().await?;
        Ok(body.project_id)
    }

    /// Execute raw `.akr` source ad-hoc. The server runs it in the caller's
    /// per-user sandbox project and returns `{execution_id, project_id}`.
    ///
    /// Equivalent to [`run_adhoc_with`](Self::run_adhoc_with) with `channel`
    /// and `triggered_by` both `None`.
    pub async fn run_adhoc(
        &self,
        source: &str,
        inputs: Option<std::collections::HashMap<String, serde_json::Value>>,
        breakpoint_lines: Option<Vec<usize>>,
    ) -> crate::error::Result<crate::models::AdhocRunResult> {
        self.run_adhoc_with(source, inputs, breakpoint_lines, None, None)
            .await
    }

    /// Execute raw `.akr` source ad-hoc, with optional `channel` and
    /// `triggered_by` (#1120). Mirrors Python's `run_adhoc(channel=...,
    /// triggered_by=...)` and the TS `runAdHoc` opts.
    ///
    /// - `channel`: release channel for resolving `use foo` references. When
    ///   `None`, the server applies its default (typically `production`).
    /// - `triggered_by`: opaque identifier recorded with the execution for
    ///   audit. Common values: `"studio"`, `"bench"`, `"<user_email>"`.
    pub async fn run_adhoc_with(
        &self,
        source: &str,
        inputs: Option<std::collections::HashMap<String, serde_json::Value>>,
        breakpoint_lines: Option<Vec<usize>>,
        channel: Option<&str>,
        triggered_by: Option<&str>,
    ) -> crate::error::Result<crate::models::AdhocRunResult> {
        let url = format!("{}/execute", self.inner.base_url);
        self.post(
            &url,
            &crate::models::AdhocRunRequest {
                source,
                inputs,
                breakpoint_lines,
                channel,
                triggered_by,
            },
        )
        .await
    }

    /// Subscribe to engine events from ad-hoc executions in the given sandbox
    /// project. Pass the `project_id` returned from [`run_adhoc`](Self::run_adhoc)
    /// or [`get_sandbox_project_id`](Self::get_sandbox_project_id).
    ///
    /// Returns a receiver for [`EngineEvent`]s (filtered to `Execution` variants)
    /// plus a subscription handle — dropping the handle cancels the stream.
    ///
    /// Equivalent to [`adhoc_event_stream_with_ready`](Self::adhoc_event_stream_with_ready)
    /// with `ready = None`. Use the `_with_ready` variant when you need to
    /// avoid the subscribe-after-POST race on a fast workflow.
    pub async fn adhoc_event_stream(
        &self,
        project_id: i64,
    ) -> crate::error::Result<(
        tokio::sync::mpsc::UnboundedReceiver<crate::models::EngineEvent>,
        crate::sub::events::EventSubscription,
    )> {
        self.adhoc_event_stream_with_ready(project_id, None).await
    }

    /// Like [`adhoc_event_stream`](Self::adhoc_event_stream), but takes an
    /// optional [`tokio::sync::Notify`] that fires once the SSE `GET /events`
    /// response returns a 2xx status — i.e. the moment the server-side
    /// broadcast subscriber is attached and no events emitted from this point
    /// on can be missed.
    ///
    /// Use this to avoid the subscribe-after-POST race: a fast workflow
    /// (single-digit milliseconds, mock providers) can emit `NodeStart`,
    /// `TaskStart`, … before a naive `run_adhoc().then(adhoc_event_stream)`
    /// has its SSE subscriber registered, and those opening events are then
    /// dropped by the broadcast channel.
    ///
    /// The pattern is **subscribe → await ready → POST**:
    ///
    /// ```no_run
    /// # use akribes_sdk::AkribesClient;
    /// # use std::sync::Arc;
    /// # use tokio::sync::Notify;
    /// # async fn ex(client: AkribesClient, source: &str) -> akribes_sdk::Result<()> {
    /// let project_id = client.get_sandbox_project_id().await?;
    /// let ready = Arc::new(Notify::new());
    /// let (mut rx, _sub) = client
    ///     .adhoc_event_stream_with_ready(project_id, Some(Arc::clone(&ready)))
    ///     .await?;
    /// ready.notified().await;            // SSE attached, safe to POST
    /// client.run_adhoc(source, None, None).await?;
    /// while let Some(_event) = rx.recv().await { /* … */ }
    /// # Ok(()) }
    /// ```
    ///
    /// If the SSE subscription fails (auth error, server down, all retries
    /// exhausted) the notify is **never fired** — wrap the wait in a timeout
    /// or join it with the subscription handle to avoid blocking forever.
    pub async fn adhoc_event_stream_with_ready(
        &self,
        project_id: i64,
        ready: Option<Arc<tokio::sync::Notify>>,
    ) -> crate::error::Result<(
        tokio::sync::mpsc::UnboundedReceiver<crate::models::EngineEvent>,
        crate::sub::events::EventSubscription,
    )> {
        use crate::models::HubEvent;
        use tokio::sync::oneshot;
        let (hub_tx, mut hub_rx) = tokio::sync::mpsc::unbounded_channel();
        let (engine_tx, engine_rx) = tokio::sync::mpsc::unbounded_channel();

        // Bridge the underlying oneshot ready-signal to the caller's Notify.
        // `stream_sse_with_retry` fires the oneshot exactly once with `Ok(())`
        // on first 2xx, or `Err(_)` if all retries are exhausted; we only
        // notify the caller on the success path so a `notified().await` paired
        // with a timeout still surfaces the failure.
        let (ready_tx, ready_rx) = oneshot::channel::<crate::error::Result<()>>();
        if let Some(notify) = ready.clone() {
            tokio::spawn(async move {
                if let Ok(Ok(())) = ready_rx.await {
                    notify.notify_one();
                }
            });
        } else {
            tokio::spawn(async move {
                let _ = ready_rx.await;
            });
        }

        let http = self.inner.http.clone();
        let token = self.inner.token.clone();
        let base_url = self.inner.base_url.clone();
        let sse_handle = tokio::spawn(async move {
            let _ = crate::sub::events::stream_sse_with_retry(
                http,
                token,
                base_url,
                project_id,
                Some("adhoc".to_string()),
                hub_tx,
                Some(ready_tx),
            )
            .await;
        });

        let filter_handle = tokio::spawn(async move {
            while let Some(evt) = hub_rx.recv().await {
                if let HubEvent::Execution { event, .. } = evt {
                    if engine_tx.send(event).is_err() {
                        break;
                    }
                }
            }
            sse_handle.abort();
        });

        Ok((
            engine_rx,
            crate::sub::events::EventSubscription::from_handle(filter_handle),
        ))
    }
}

// ── Project scope ────────────────────────────────────────────────────────────

/// Project-scoped handle to the server. Obtained from
/// [`AkribesClient::project`]. Cheap to clone (just an `Arc` and an `i64`).
#[derive(Clone, Debug)]
pub struct ProjectScope {
    pub(crate) inner: Arc<Inner>,
    pub(crate) project_id: i64,
}

impl ProjectScope {
    /// The project ID this scope is bound to.
    pub fn project_id(&self) -> i64 {
        self.project_id
    }

    /// Underlying client (e.g. for global operations alongside scoped ones).
    pub fn client(&self) -> AkribesClient {
        AkribesClient {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Script management within this project.
    pub fn scripts(&self) -> crate::sub::scripts::ScriptsClient {
        crate::sub::scripts::ScriptsClient::new(Arc::clone(&self.inner), self.project_id)
    }

    /// Script drafts within this project.
    pub fn drafts(&self) -> crate::sub::drafts::DraftsClient {
        crate::sub::drafts::DraftsClient::new(Arc::clone(&self.inner), self.project_id)
    }

    /// Script versions within this project.
    pub fn versions(&self) -> crate::sub::versions::VersionsClient {
        crate::sub::versions::VersionsClient::new(Arc::clone(&self.inner), self.project_id)
    }

    /// Script channels within this project.
    pub fn channels(&self) -> crate::sub::channels::ChannelsClient {
        crate::sub::channels::ChannelsClient::new(Arc::clone(&self.inner), self.project_id)
    }

    /// Eval suites and runs within this project.
    pub fn evals(&self) -> crate::sub::evals::EvalsClient {
        crate::sub::evals::EvalsClient::new(Arc::clone(&self.inner), self.project_id)
    }

    /// Project-scoped bench operations: config CRUD, cases, list/trigger runs
    /// for a given script. Run-scoped operations (anything keyed on
    /// `bench_runs.id`) live on [`AkribesClient::bench_runs`] instead.
    pub fn bench(&self) -> crate::sub::bench::BenchClient {
        crate::sub::bench::BenchClient::new(Arc::clone(&self.inner), self.project_id)
    }

    /// SSE event streams scoped to this project.
    pub fn events(&self) -> crate::sub::events::EventsClient {
        crate::sub::events::EventsClient::new(Arc::clone(&self.inner), self.project_id)
    }

    /// Client registration scoped to this project.
    pub fn registered_clients(&self) -> crate::sub::clients::RegisteredClientsClient {
        crate::sub::clients::RegisteredClientsClient::new(Arc::clone(&self.inner), self.project_id)
    }

    /// Project-scoped execution operations (run, list, cancel_run, etc.).
    pub fn executions(&self) -> crate::sub::executions::ScopedExecutionsClient {
        crate::sub::executions::ScopedExecutionsClient::new(
            Arc::clone(&self.inner),
            self.project_id,
        )
    }

    /// MCP server/tool discovery for this project.
    pub fn mcp(&self) -> crate::sub::mcp::McpClient {
        crate::sub::mcp::McpClient::new(Arc::clone(&self.inner), self.project_id)
    }

    /// Document ingest (claim + upload) for this project.
    pub fn documents(&self) -> crate::sub::documents::DocumentsClient {
        crate::sub::documents::DocumentsClient::new(Arc::clone(&self.inner), self.project_id)
    }

    /// Project-scoped document conversion. Uploads go to
    /// `POST /projects/{id}/convert` so the server can enforce project access.
    pub async fn convert_file(
        &self,
        filename: &str,
        data: Vec<u8>,
    ) -> Result<crate::models::ConvertResult> {
        crate::sub::convert::ConvertClient::new(Arc::clone(&self.inner))
            .convert_file_for_project(self.project_id, filename, data)
            .await
    }
}

impl Drop for AkribesClient {
    fn drop(&mut self) {
        // Signal the heartbeat task to stop. The AtomicBool avoids the
        // TOCTOU race that `Arc::strong_count` had: a clone could appear
        // between the count check and the abort.
        if Arc::strong_count(&self.inner) == 1 {
            self.inner.shutdown.store(true, Ordering::Release);
            if let Ok(mut h) = self.inner.heartbeat_handle.lock() {
                if let Some(handle) = h.take() {
                    handle.abort();
                }
            }
        }
    }
}

// ── Builder ─────────────────────────────────────────────────────────────────

#[must_use = "a builder does nothing until .build() is called"]
pub struct AkribesClientBuilder {
    base_url: String,
    project_id: Option<i64>,
    name: Option<String>,
    id: Option<String>,
    token: Option<String>,
    on_behalf_of: Option<String>,
    http_client: Option<reqwest::Client>,
    ingest_poll_timeout: Option<Duration>,
}

impl AkribesClientBuilder {
    /// Set the project ID. Required for project-scoped operations (scripts,
    /// executions, etc.). Omit for global-only usage (list_projects, etc.).
    pub fn project_id(mut self, project_id: i64) -> Self {
        self.project_id = Some(project_id);
        self
    }

    /// Client display name (default: `"rust-sdk"`).
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Client ID (default: random UUID v4).
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Initial authentication token. Either:
    /// - a **service token** (the secret part of `AKRIBES_SERVICE_TOKEN_<NAME>=<scope>:<secret>`), or
    /// - a **scoped token** of the form `akribes_tk_<...>` (legacy
    ///   `aura_tk_<...>` still accepted) minted via
    ///   [`crate::sub::tokens::TokensClient::mint`].
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Set the `X-Akribes-User` header sent on every outbound request.
    ///
    /// Used by the server for metrics attribution when a backend (typically
    /// holding a service token) acts on behalf of an end user. **Advisory
    /// only — does not grant any permission.** Authorization remains based
    /// on the bearer token's scope.
    ///
    /// Mirrors the TS `AkribesClientOptions.onBehalfOf` and Python
    /// `AkribesClient(on_behalf_of=...)` knobs. Use
    /// [`AkribesClient::set_on_behalf_of`] to update the value at runtime.
    ///
    /// ```no_run
    /// # use akribes_sdk::AkribesClient;
    /// let client = AkribesClient::builder("http://localhost:3001")
    ///     .project_id(2)
    ///     .token(std::env::var("AKRIBES_SERVICE_TOKEN").unwrap())
    ///     .on_behalf_of("alice@acme.com")
    ///     .build();
    /// ```
    pub fn on_behalf_of(mut self, email: impl Into<String>) -> Self {
        self.on_behalf_of = Some(email.into());
        self
    }

    /// Use a pre-configured [`reqwest::Client`] so multiple `AkribesClient`s
    /// can share a connection pool, proxy settings, or TLS configuration.
    ///
    /// If not called, a default `reqwest::Client` is created with a 60s
    /// request timeout and a 10s connect timeout.
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    /// Override the deadline `documents().ingest()` waits for a still-converting
    /// blob before surfacing [`AkribesError::Transient`].
    ///
    /// Resolution order at [`build`](Self::build) time:
    /// 1. This builder param if set.
    /// 2. `AKRIBES_SDK_INGEST_TIMEOUT_SECS` env var (parsed as `u64` seconds;
    ///    `0` and unparseable values are ignored).
    /// 3. [`DEFAULT_INGEST_POLL_TIMEOUT_SECS`] (300 s).
    ///
    /// Setting this to a very short duration is occasionally useful in tests
    /// that want to assert the timeout path. Setting it absurdly long (hours)
    /// just shifts the failure mode — the server has its own conversion
    /// timeouts.
    pub fn ingest_poll_timeout(mut self, timeout: Duration) -> Self {
        self.ingest_poll_timeout = Some(timeout);
        self
    }

    /// Build the client.
    pub fn build(self) -> AkribesClient {
        let ingest_poll_timeout = self
            .ingest_poll_timeout
            .or_else(ingest_poll_timeout_from_env)
            .unwrap_or(Duration::from_secs(DEFAULT_INGEST_POLL_TIMEOUT_SECS));
        AkribesClient {
            inner: Arc::new(Inner {
                base_url: self.base_url,
                project_id: self.project_id,
                name: self.name.unwrap_or_else(|| "rust-sdk".to_string()),
                id: self.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                http: self.http_client.unwrap_or_else(default_http_client),
                token: Arc::new(RwLock::new(self.token)),
                on_behalf_of: Arc::new(RwLock::new(self.on_behalf_of)),
                heartbeat_handle: Mutex::new(None),
                shutdown: Arc::new(AtomicBool::new(false)),
                schema_cache: Mutex::new(HashMap::new()),
                broken_scripts: Mutex::new(HashSet::new()),
                ingest_poll_timeout,
            }),
        }
    }
}
