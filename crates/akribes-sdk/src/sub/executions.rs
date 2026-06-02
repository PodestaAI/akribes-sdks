use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use crate::client::{AkribesClient, Inner};
use crate::error::{AkribesError, Result};
use crate::models::*;

/// Pre-dispatch contract validation.
fn validate_contract(
    inner: &Inner,
    script_name: &str,
    document_keys: Option<&[&str]>,
) -> Result<()> {
    if inner.broken_scripts.lock().unwrap().contains(script_name) {
        return Err(AkribesError::ScriptSchemaChanged {
            script_name: script_name.to_string(),
        });
    }

    if let Some(doc_keys) = document_keys {
        let schemas = inner.schema_cache.lock().unwrap();
        if let Some(schema) = schemas.get(script_name) {
            let expected_docs: Vec<&str> = schema
                .iter()
                .filter(|(_, ty)| ty == "document")
                .map(|(name, _)| name.as_str())
                .collect();
            let provided: HashSet<&str> = doc_keys.iter().copied().collect();
            let missing: Vec<String> = expected_docs
                .iter()
                .filter(|n| !provided.contains(**n))
                .map(|n| n.to_string())
                .collect();
            let expected: HashSet<&str> = expected_docs.into_iter().collect();
            let extra: Vec<String> = doc_keys
                .iter()
                .filter(|k| !expected.contains(**k))
                .map(|k| k.to_string())
                .collect();
            if !missing.is_empty() || !extra.is_empty() {
                return Err(AkribesError::ScriptInputMismatch {
                    script_name: script_name.to_string(),
                    missing,
                    extra,
                });
            }
        }
    }

    Ok(())
}

// ── ExecutionsClient (global) ───────────────────────────────────────────────

/// Sub-client for execution operations that are not project-scoped
/// (lookup, resume, cancel, document helpers, await). Obtained via
/// [`AkribesClient::executions()`].
///
/// For project-scoped operations (run, list, etc.), see
/// [`ScopedExecutionsClient`] via [`crate::client::ProjectScope::executions`].
#[derive(Clone, Debug)]
pub struct ExecutionsClient {
    pub(crate) inner: Arc<Inner>,
}

impl ExecutionsClient {
    pub(crate) fn new(inner: Arc<Inner>) -> Self {
        Self { inner }
    }

    fn c(&self) -> AkribesClient {
        AkribesClient {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Resume a suspended checkpoint within a running execution.
    pub async fn resume(
        &self,
        execution_id: &str,
        token: &str,
        data: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let url = format!("{}/executions/{}/resume", self.inner.base_url, execution_id);
        self.c()
            .post(
                &url,
                &ResumeRequest {
                    token: token.to_string(),
                    data,
                },
            )
            .await
    }

    /// Cancel a specific execution by ID; returns `true` if it was cancelled.
    pub async fn cancel(&self, execution_id: &str) -> Result<bool> {
        let url = format!("{}/executions/{}", self.inner.base_url, execution_id);
        self.c().delete(&url).await
    }

    /// List child executions spawned by `execution_id` via the engine's
    /// `spawn_child_execution` callback (#1054). Returns an empty `Vec` when
    /// no children exist (the common case for v1 where parent-linkage
    /// columns are typically NULL). Mirrors TS `executions.children` and
    /// Python `executions.children`.
    pub async fn children(&self, execution_id: &str) -> Result<Vec<ExecutionChildSummary>> {
        let url = format!(
            "{}/executions/{}/children",
            self.inner.base_url, execution_id
        );
        self.c().get_list(&url).await
    }

    /// Fetch the full status record for an execution.
    pub async fn get(&self, execution_id: &str) -> Result<Option<ExecutionStatus>> {
        let url = format!("{}/executions/{}", self.inner.base_url, execution_id);
        self.c().get_opt(&url).await
    }

    /// Per-task cost / token / duration breakdown for an execution
    /// (`GET /executions/{id}/tasks`). Reads from the `execution_tasks`
    /// table populated as `TaskEnd` events arrive, so callers don't have to
    /// parse the event JSON to recover this data. Useful for monolith
    /// workflows where there are no spawned children — every agent
    /// invocation lives in `execution_tasks` keyed by `task_name`.
    ///
    /// 404 → `Ok(None)` (the execution doesn't exist or isn't accessible to
    /// this token), matching `get` / `get_output`. Mirrors the TS SDK's
    /// `executions.tasks`.
    pub async fn tasks(&self, execution_id: &str) -> Result<Option<ExecutionTasksResponse>> {
        let url = format!(
            "{}/executions/{}/tasks",
            self.inner.base_url,
            urlencoding::encode(execution_id)
        );
        self.c().get_opt(&url).await
    }

    /// Fetch only the output (status, error, result) for an execution.
    pub async fn get_output(&self, execution_id: &str) -> Result<Option<ExecutionOutput>> {
        let url = format!("{}/executions/{}/output", self.inner.base_url, execution_id);
        self.c().get_opt(&url).await
    }

    /// Return the persisted event stream for an execution. `types` is an
    /// optional comma-separated allowlist (`"TaskEnd,Error,WorkflowEnd"`)
    /// that lets callers slim down responses on long workflows.
    pub async fn get_events(
        &self,
        execution_id: &str,
        after_id: Option<i64>,
        limit: Option<i64>,
        types: Option<&str>,
    ) -> Result<Option<ExecutionEvents>> {
        #[derive(serde::Serialize)]
        struct Q<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            after_id: Option<i64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            limit: Option<i64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            types: Option<&'a str>,
        }
        let base = format!("{}/executions/{}/events", self.inner.base_url, execution_id);
        let url = AkribesClient::url_with_query(
            &base,
            &Q {
                after_id,
                limit,
                types,
            },
        );
        let res = self.c().send(self.c().inner.http.get(&url)).await?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(crate::client::decode_json(res).await?))
    }

    // ── Document helpers ──────────────────────────────────────────────────

    /// Get document metadata by ID.
    pub async fn get_document(&self, document_id: &str) -> Result<Option<DocumentMeta>> {
        let url = format!(
            "{}/documents/{}",
            self.inner.base_url,
            urlencoding::encode(document_id)
        );
        self.c().get_opt(&url).await
    }

    /// Get converted markdown for a document.
    pub async fn get_document_markdown(&self, document_id: &str) -> Result<String> {
        let url = format!(
            "{}/documents/{}/markdown",
            self.inner.base_url,
            urlencoding::encode(document_id)
        );
        let res = self.c().send(self.c().inner.http.get(&url)).await?;
        let body: serde_json::Value = crate::client::decode_json(res).await?;
        // The server contract is `{"markdown": "<string>"}`. A missing or
        // non-string `markdown` field is a server-contract violation, not an
        // "empty document" — surface it rather than silently returning "".
        match body.get("markdown") {
            Some(serde_json::Value::String(s)) => Ok(s.clone()),
            other => Err(AkribesError::Other(format!(
                "GET /documents/{}/markdown returned a malformed response: \
                 expected a string `markdown` field, got {}",
                document_id,
                match other {
                    None => "no `markdown` field".to_string(),
                    Some(v) => format!("{v}"),
                },
            ))),
        }
    }

    /// Get a presigned download URL for the original document file.
    pub async fn get_document_url(&self, document_id: &str) -> Result<String> {
        let url = format!(
            "{}/documents/{}/content",
            self.inner.base_url,
            urlencoding::encode(document_id)
        );
        let resp = self
            .c()
            .send(self.c().inner.http.get(&url).header("Accept", "*/*"))
            .await?;
        Ok(resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .unwrap_or_else(|| resp.url().to_string()))
    }

    /// Retry conversion on a failed document.
    pub async fn reconvert_document(&self, document_id: &str) -> Result<serde_json::Value> {
        let url = format!(
            "{}/documents/{}/convert",
            self.inner.base_url,
            urlencoding::encode(document_id)
        );
        self.c().post(&url, &serde_json::json!({})).await
    }

    /// Run a script from a specific point with pre-seeded environment values.
    ///
    /// Convenience wrapper that doesn't require building a [`ScopedExecutionsClient`]
    /// first — useful for callers (e.g. akribes-mcp) that already know the
    /// `project_id` numerically. Forwards to
    /// [`ScopedExecutionsClient::run_from`].
    ///
    /// `seed_env` carries pre-computed variable values for nodes already
    /// completed by a prior execution; `skip_node_ids` lists those node IDs
    /// so the engine skips re-running them. `channel` defaults to `draft`.
    pub async fn run_from_node(
        &self,
        project_id: i64,
        script_name: &str,
        seed_env: HashMap<String, serde_json::Value>,
        skip_node_ids: Vec<usize>,
        channel: Option<&str>,
        inputs: Option<HashMap<String, serde_json::Value>>,
    ) -> Result<RunResult> {
        let scoped = ScopedExecutionsClient::new(Arc::clone(&self.inner), project_id);
        scoped
            .run_from(script_name, seed_env, skip_node_ids, channel, inputs, None)
            .await
    }

    /// Poll `get_output` until the execution reaches a terminal state.
    pub async fn await_execution(
        &self,
        execution_id: &str,
        timeout_ms: Option<u64>,
        poll_interval_ms: Option<u64>,
    ) -> Result<ExecutionOutput> {
        let interval = Duration::from_millis(poll_interval_ms.unwrap_or(500));
        let deadline = timeout_ms.map(|ms| std::time::Instant::now() + Duration::from_millis(ms));

        loop {
            if let Some(deadline) = deadline {
                if std::time::Instant::now() >= deadline {
                    return Err(AkribesError::Timeout {
                        execution_id: Some(execution_id.to_string()),
                    });
                }
            }

            let output = self.get_output(execution_id).await?;
            if let Some(output) = output {
                match output.status.as_str() {
                    "completed" => return Ok(output),
                    "failed" | "cancelled" => {
                        let msg = output.error.clone().unwrap_or_default();
                        let eid = Some(execution_id.to_string());
                        return Err(match output.error_kind.as_deref() {
                            // #1296: accept the legacy "ServerError" wire
                            // form for back-compat plus the four new
                            // status-specific variants.
                            Some("RateLimit")
                            | Some("ServerError")
                            | Some("ServerError500")
                            | Some("BadGateway502")
                            | Some("ServiceUnavailable503")
                            | Some("GatewayTimeout504")
                            | Some("NetworkError") => {
                                let status = match output.error_kind.as_deref() {
                                    Some("ServerError500") => Some(500u16),
                                    Some("BadGateway502") => Some(502u16),
                                    Some("ServiceUnavailable503") => Some(503u16),
                                    Some("GatewayTimeout504") => Some(504u16),
                                    Some("RateLimit") => Some(429u16),
                                    _ => None,
                                };
                                AkribesError::Transient {
                                    message: msg,
                                    execution_id: eid,
                                    retry_after: None,
                                    status,
                                }
                            }
                            Some("AuthError") | Some("TokenLimit") => AkribesError::Fatal {
                                message: msg,
                                execution_id: eid,
                            },
                            _ => AkribesError::Script {
                                message: msg,
                                execution_id: eid,
                            },
                        });
                    }
                    _ => {}
                }
            }

            tokio::time::sleep(interval).await;
        }
    }

    /// Cross-SDK naming alias for [`await_execution`]. Mirrors the Python
    /// SDK's `await_result`; forwarded verbatim so callers porting examples
    /// across SDKs don't trip on the rename. Refs #109 (item 3:
    /// method-naming consistency).
    pub async fn await_result(
        &self,
        execution_id: &str,
        timeout_ms: Option<u64>,
        poll_interval_ms: Option<u64>,
    ) -> Result<ExecutionOutput> {
        self.await_execution(execution_id, timeout_ms, poll_interval_ms)
            .await
    }
}

// ── ScopedExecutionsClient (project-scoped) ─────────────────────────────────

/// Project-scoped execution operations. Obtained via
/// [`crate::client::ProjectScope::executions`].
///
/// Derefs to [`ExecutionsClient`], so global methods (`get`, `cancel`,
/// `resume`, `await_execution`, document helpers) are available too.
#[derive(Clone, Debug)]
pub struct ScopedExecutionsClient {
    base: ExecutionsClient,
    project_id: i64,
}

impl Deref for ScopedExecutionsClient {
    type Target = ExecutionsClient;
    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl ScopedExecutionsClient {
    pub(crate) fn new(inner: Arc<Inner>, project_id: i64) -> Self {
        Self {
            base: ExecutionsClient { inner },
            project_id,
        }
    }

    fn c(&self) -> AkribesClient {
        AkribesClient {
            inner: Arc::clone(&self.base.inner),
        }
    }

    fn project_url(&self) -> String {
        format!("{}/projects/{}", self.base.inner.base_url, self.project_id)
    }

    fn script_url(&self, name: &str) -> String {
        format!(
            "{}/scripts/{}",
            self.project_url(),
            urlencoding::encode(name)
        )
    }

    /// Start building a script run.
    pub fn run(&self, script_name: &str) -> RunBuilder {
        RunBuilder {
            client: self.c(),
            inner: Arc::clone(&self.base.inner),
            project_id: self.project_id,
            script_name: script_name.to_string(),
            channel: "production".to_string(),
            inputs: None,
            triggered_by: None,
            breakpoint_lines: None,
        }
    }

    /// Subscribe to the SSE event stream *first*, then kick off a run, and
    /// return a [`RunStream`] that yields typed [`WorkflowEvent`]s until the
    /// workflow terminates.
    ///
    /// `req` is a [`RunBuilder`] (from [`Self::run`]) carrying the script
    /// name, channel, inputs and other run parameters.
    ///
    /// [`RunStream`]: crate::sub::run_stream::RunStream
    /// [`WorkflowEvent`]: crate::events::WorkflowEvent
    pub async fn run_stream(&self, req: RunBuilder) -> Result<crate::sub::run_stream::RunStream> {
        crate::sub::run_stream::start_run_stream(Arc::clone(&self.base.inner), self.project_id, req)
            .await
    }

    /// Start building an execution list query.
    pub fn list(&self, script_name: &str) -> ListExecutionsBuilder {
        ListExecutionsBuilder {
            client: self.c(),
            script_url: self.script_url(script_name),
            status: None,
            channel: None,
            limit: None,
            offset: None,
        }
    }

    /// Cancel all running executions for a script; returns `true` if any were cancelled.
    pub async fn cancel_run(&self, script_name: &str) -> Result<bool> {
        let url = format!("{}/run", self.script_url(script_name));
        self.c().delete(&url).await
    }

    /// Cross-SDK naming alias for [`cancel_run`]. Mirrors the Python SDK's
    /// `cancel_all`; forwarded verbatim so callers porting examples across
    /// SDKs don't trip on the rename. Refs #109 (item 3: method-naming
    /// consistency).
    pub async fn cancel_all(&self, script_name: &str) -> Result<bool> {
        self.cancel_run(script_name).await
    }

    /// Run a script from a specific point with pre-seeded environment values.
    pub async fn run_from(
        &self,
        script_name: &str,
        seed_env: HashMap<String, serde_json::Value>,
        skip_node_ids: Vec<usize>,
        channel: Option<&str>,
        inputs: Option<HashMap<String, serde_json::Value>>,
        triggered_by: Option<&str>,
    ) -> Result<RunResult> {
        let channel = channel.unwrap_or("draft");
        let url = format!(
            "{}/run/from?channel={}",
            self.script_url(script_name),
            urlencoding::encode(channel)
        );
        let tb = triggered_by
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.base.inner.name.clone());
        self.c()
            .post(
                &url,
                &RunFromRequest {
                    inputs,
                    seed_env,
                    skip_node_ids,
                    triggered_by: Some(tb),
                },
            )
            .await
    }

    /// Get the compiled execution DAG for a script.
    pub async fn get_graph(
        &self,
        script_name: &str,
        version_id: Option<i64>,
    ) -> Result<GraphResponse> {
        #[derive(serde::Serialize)]
        struct Q {
            #[serde(skip_serializing_if = "Option::is_none")]
            version: Option<i64>,
        }
        let base = format!("{}/graph", self.script_url(script_name));
        let url = AkribesClient::url_with_query(
            &base,
            &Q {
                version: version_id,
            },
        );
        let res = self.c().send(self.c().inner.http.get(&url)).await?;
        crate::client::decode_json(res).await
    }

    /// Get cost aggregation for the entire project.
    pub async fn get_project_cost(
        &self,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<crate::models::ProjectCost> {
        #[derive(serde::Serialize)]
        struct Q<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            since: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            until: Option<&'a str>,
        }
        let base = format!("{}/cost", self.project_url());
        let url = AkribesClient::url_with_query(&base, &Q { since, until });
        let res = self.c().send(self.c().inner.http.get(&url)).await?;
        crate::client::decode_json(res).await
    }

    /// Get cost aggregation for a script.
    pub async fn get_cost(&self, script_name: &str) -> Result<CostAggregation> {
        let url = format!("{}/cost", self.script_url(script_name));
        self.c().get_opt::<CostAggregation>(&url).await.map(|o| {
            o.unwrap_or_else(|| CostAggregation {
                total_executions: 0,
                total_cost_usd: 0.0,
                avg_cost_usd: 0.0,
                total_input_tokens: 0,
                total_output_tokens: 0,
                total_tool_tokens: 0,
                by_version: vec![],
            })
        })
    }

    /// Run a script with file uploads that get converted to Markdown via Docling.
    pub async fn run_with_upload(
        &self,
        script_name: &str,
        files: HashMap<String, (String, Vec<u8>)>,
        channel: Option<&str>,
        triggered_by: Option<&str>,
    ) -> Result<RunResult> {
        let channel = channel.unwrap_or("production");
        let url = format!(
            "{}/run/upload?channel={}",
            self.script_url(script_name),
            urlencoding::encode(channel)
        );

        let mut form = reqwest::multipart::Form::new();
        for (input_name, (filename, data)) in files {
            let part = reqwest::multipart::Part::bytes(data)
                .file_name(filename)
                .mime_str("application/octet-stream")
                .expect("valid static MIME type");
            form = form.part(input_name, part);
        }

        let tb = triggered_by
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.base.inner.name.clone());
        let meta = serde_json::json!({ "triggered_by": tb });
        form = form.text("_meta", meta.to_string());

        self.c().post_multipart(&url, form).await
    }

    /// Run a script with documents sourced from S3.
    pub async fn run_with_s3(
        &self,
        script_name: &str,
        inputs: HashMap<String, S3DocumentRef>,
        channel: Option<&str>,
        triggered_by: Option<&str>,
    ) -> Result<RunResult> {
        let url = format!("{}/run/s3", self.script_url(script_name));
        let tb = triggered_by
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.base.inner.name.clone());
        self.c()
            .post(
                &url,
                &RunWithS3Request {
                    inputs,
                    channel: channel.map(|s| s.to_string()),
                    triggered_by: Some(tb),
                },
            )
            .await
    }
}

// ── RunBuilder ──────────────────────────────────────────────────────────────

/// Builder for a script run.
///
/// Inputs may be supplied one-at-a-time with [`input`](Self::input) (and the
/// [`document`](Self::document) / [`documents`](Self::documents) ergonomic
/// shortcuts for doc-id references), or in bulk with [`inputs`](Self::inputs).
/// All merge into the same map; later writes to the same key overwrite earlier
/// ones.
///
/// ```no_run
/// # use akribes_sdk::AkribesClient;
/// # use serde_json::json;
/// # async fn demo() -> akribes_sdk::Result<()> {
/// let client = AkribesClient::builder("http://localhost:8080").token("tok").build();
/// let _run = client.project(1).executions().run("my_script")
///     .input("age", 25)
///     .input("tags", json!(["a", "b"]))
///     .document("resume", "doc_00000000-0000-0000-0000-000000000001")
///     .execute()
///     .await?;
/// # Ok(()) }
/// ```
#[derive(Debug, Clone)]
#[must_use = "a builder does nothing until .execute() is called"]
pub struct RunBuilder {
    client: AkribesClient,
    inner: Arc<Inner>,
    project_id: i64,
    script_name: String,
    channel: String,
    inputs: Option<HashMap<String, serde_json::Value>>,
    triggered_by: Option<String>,
    breakpoint_lines: Option<Vec<usize>>,
}

impl RunBuilder {
    /// The script this builder will run.
    pub fn script_name(&self) -> &str {
        &self.script_name
    }

    fn script_url(&self) -> String {
        format!(
            "{}/projects/{}/scripts/{}",
            self.inner.base_url,
            self.project_id,
            urlencoding::encode(&self.script_name)
        )
    }

    pub fn channel(mut self, channel: impl Into<String>) -> Self {
        self.channel = channel.into();
        self
    }

    /// Replace the inputs map in bulk. Merges into any previously-set inputs —
    /// entries with the same key are overwritten by this call.
    pub fn inputs(mut self, inputs: HashMap<String, serde_json::Value>) -> Self {
        match &mut self.inputs {
            Some(existing) => existing.extend(inputs),
            None => self.inputs = Some(inputs),
        }
        self
    }

    /// Set one input. Overwrites any previous value for the same name.
    pub fn input<V: Into<serde_json::Value>>(mut self, name: impl Into<String>, value: V) -> Self {
        self.inputs
            .get_or_insert_with(HashMap::new)
            .insert(name.into(), value.into());
        self
    }

    /// Convenience for setting a `document`-typed input from a `doc_<uuid>`
    /// reference. The server resolves it to markdown before the workflow runs.
    /// Inline content is no longer supported — use `input name: markdown`
    /// for that.
    pub fn document(self, name: impl Into<String>, doc_id: impl Into<String>) -> Self {
        self.input(name, serde_json::Value::String(doc_id.into()))
    }

    /// Convenience for setting a `list[document]`-typed input from an iterable
    /// of `doc_<uuid>` references. Each is resolved independently.
    pub fn documents<I, S>(self, name: impl Into<String>, doc_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let arr: Vec<serde_json::Value> = doc_ids
            .into_iter()
            .map(|d| serde_json::Value::String(d.into()))
            .collect();
        self.input(name, serde_json::Value::Array(arr))
    }

    pub fn triggered_by(mut self, triggered_by: impl Into<String>) -> Self {
        self.triggered_by = Some(triggered_by.into());
        self
    }

    pub fn breakpoint_lines(mut self, lines: Vec<usize>) -> Self {
        self.breakpoint_lines = Some(lines);
        self
    }

    pub async fn execute(self) -> Result<RunResult> {
        let input_keys: Vec<&str> = self
            .inputs
            .as_ref()
            .map(|d| d.keys().map(|k| k.as_str()).collect())
            .unwrap_or_default();
        validate_contract(
            &self.inner,
            &self.script_name,
            if input_keys.is_empty() {
                None
            } else {
                Some(&input_keys)
            },
        )?;
        let url = format!(
            "{}/run?channel={}",
            self.script_url(),
            urlencoding::encode(&self.channel)
        );
        let triggered_by = self.triggered_by.unwrap_or_else(|| self.inner.name.clone());
        self.client
            .post(
                &url,
                &RunRequest {
                    inputs: self.inputs,
                    triggered_by: Some(triggered_by),
                    breakpoint_lines: self.breakpoint_lines,
                },
            )
            .await
    }

    pub async fn execute_and_await(
        self,
        timeout_ms: Option<u64>,
    ) -> Result<(String, ExecutionOutput)> {
        let execs = ExecutionsClient {
            inner: Arc::clone(&self.inner),
        };
        let run = self.execute().await?;
        let eid = run.execution_id.clone();
        let output = execs.await_execution(&eid, timeout_ms, None).await?;
        Ok((eid, output))
    }
}

// ── ListExecutionsBuilder ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
#[must_use = "a builder does nothing until .fetch() is called"]
pub struct ListExecutionsBuilder {
    client: AkribesClient,
    script_url: String,
    status: Option<String>,
    channel: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

impl ListExecutionsBuilder {
    pub fn status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }

    pub fn channel(mut self, channel: impl Into<String>) -> Self {
        self.channel = Some(channel.into());
        self
    }

    pub fn limit(mut self, limit: i64) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn offset(mut self, offset: i64) -> Self {
        self.offset = Some(offset);
        self
    }

    pub async fn fetch(self) -> Result<Vec<ExecutionStatus>> {
        #[derive(serde::Serialize)]
        struct Q<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            status: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            channel: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            limit: Option<i64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            offset: Option<i64>,
        }
        let base = format!("{}/executions", self.script_url);
        let url = AkribesClient::url_with_query(
            &base,
            &Q {
                status: self.status.as_deref(),
                channel: self.channel.as_deref(),
                limit: self.limit,
                offset: self.offset,
            },
        );
        let res = self.client.send(self.client.inner.http.get(&url)).await?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }
        crate::client::decode_json(res).await
    }
}
