//! Sub-client for the akribes-server bench substrate.
//!
//! Wraps the per-script bench config CRUD, case CRUD + promote-from-execution,
//! and the bench-run lifecycle (trigger / list / get / results / events / cancel
//! / delete / compare / tag-session). Two surfaces:
//!
//! - Project-scoped operations live on [`BenchClient`] (obtained via
//!   `client.project(id).bench()`). They take a script name + project_id
//!   together: `/projects/{id}/scripts/{name}/bench/...`.
//! - Run-scoped operations (anything keyed on the global `bench_runs.id`) live
//!   on [`BenchRunsClient`] (obtained via `client.bench_runs()`). The same
//!   endpoints — `/bench-runs/{id}/...` — are reachable cross-project so
//!   they don't need a `project_id`.
//!
//! Strings on the wire (RFC3339 timestamps) — see [`crate::models`].

use std::sync::Arc;

use serde::Serialize;

use crate::client::{AkribesClient, Inner};
use crate::error::Result;
use crate::models::*;

// ── Project-scoped bench client ──────────────────────────────────────────────

/// Bench operations rooted at a project + script. Obtained via
/// `client.project(id).bench()`.
#[derive(Clone, Debug)]
pub struct BenchClient {
    pub(crate) inner: Arc<Inner>,
    pub(crate) project_id: i64,
}

impl BenchClient {
    pub(crate) fn new(inner: Arc<Inner>, project_id: i64) -> Self {
        Self { inner, project_id }
    }

    fn c(&self) -> AkribesClient {
        AkribesClient {
            inner: Arc::clone(&self.inner),
        }
    }

    fn bench_url(&self, script_name: &str) -> String {
        format!(
            "{}/projects/{}/scripts/{}/bench",
            self.inner.base_url,
            self.project_id,
            urlencoding::encode(script_name),
        )
    }

    // ── Bench config CRUD ───────────────────────────────────────────────────

    /// `GET /projects/{id}/scripts/{name}/bench` — 404 → `Ok(None)`.
    pub async fn get(&self, script_name: &str) -> Result<Option<Bench>> {
        self.c().get_opt(&self.bench_url(script_name)).await
    }

    /// `POST /projects/{id}/scripts/{name}/bench` — create or update.
    pub async fn create_or_update(
        &self,
        script_name: &str,
        req: &CreateOrUpdateBenchRequest,
    ) -> Result<Bench> {
        self.c().post(&self.bench_url(script_name), req).await
    }

    /// `DELETE /projects/{id}/scripts/{name}/bench`. Returns `true` if a row
    /// was deleted, `false` if absent.
    pub async fn delete(&self, script_name: &str) -> Result<bool> {
        self.c().delete(&self.bench_url(script_name)).await
    }

    /// `GET /projects/{id}/scripts/{name}/signature` — the parsed script
    /// signature (inputs + outputs) plus named type defs. Returned as
    /// `serde_json::Value` because the server emits an ad-hoc tagged shape
    /// that doesn't have a stable Rust mirror; the studio + MCP both treat it
    /// as a blob.
    pub async fn get_signature(&self, script_name: &str) -> Result<serde_json::Value> {
        let url = format!(
            "{}/projects/{}/scripts/{}/signature",
            self.inner.base_url,
            self.project_id,
            urlencoding::encode(script_name),
        );
        Ok(self
            .c()
            .get_opt::<serde_json::Value>(&url)
            .await?
            .unwrap_or(serde_json::json!({})))
    }

    /// `GET /projects/{id}/scripts/{name}/bench/contract-preview` — workflow +
    /// judge signatures with structured `breaks` list. Returned as a `Value`
    /// because the wire shape contains the (unstable, JSON-only) signature
    /// representation.
    pub async fn contract_preview(
        &self,
        script_name: &str,
        judge_script_id: i64,
        channel: Option<&str>,
    ) -> Result<serde_json::Value> {
        #[derive(Serialize)]
        struct Q<'a> {
            judge: i64,
            #[serde(skip_serializing_if = "Option::is_none")]
            channel: Option<&'a str>,
        }
        let base = format!("{}/contract-preview", self.bench_url(script_name));
        let url = AkribesClient::url_with_query(
            &base,
            &Q {
                judge: judge_script_id,
                channel,
            },
        );
        Ok(self
            .c()
            .get_opt::<serde_json::Value>(&url)
            .await?
            .unwrap_or(serde_json::json!({})))
    }

    // ── Cases ───────────────────────────────────────────────────────────────

    /// `GET /projects/{id}/scripts/{name}/bench/cases`. 404 → empty list.
    pub async fn list_cases(&self, script_name: &str) -> Result<Vec<BenchCase>> {
        let url = format!("{}/cases", self.bench_url(script_name));
        self.c().get_list(&url).await
    }

    /// `POST /projects/{id}/scripts/{name}/bench/cases` — form-builder create.
    pub async fn create_case(
        &self,
        script_name: &str,
        req: &CreateBenchCaseRequest,
    ) -> Result<BenchCase> {
        let url = format!("{}/cases", self.bench_url(script_name));
        self.c().post(&url, req).await
    }

    /// `GET /projects/{id}/scripts/{name}/bench/cases/contract-drift`.
    pub async fn case_contract_drift(&self, script_name: &str) -> Result<DriftReport> {
        let url = format!("{}/cases/contract-drift", self.bench_url(script_name));
        Ok(self
            .c()
            .get_opt::<DriftReport>(&url)
            .await?
            .unwrap_or(DriftReport {
                drifted: Vec::new(),
                script_version_id: None,
                published_at: None,
                published_by: None,
                summary: String::new(),
            }))
    }

    // ── Runs (project-scoped surface) ───────────────────────────────────────

    /// `GET /projects/{id}/scripts/{name}/bench/runs` — paginated via
    /// `limit`/`offset`.
    pub async fn list_runs(
        &self,
        script_name: &str,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> Result<Vec<BenchRun>> {
        #[derive(Serialize)]
        struct Q {
            #[serde(skip_serializing_if = "Option::is_none")]
            limit: Option<i64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            offset: Option<i64>,
        }
        let base = format!("{}/runs", self.bench_url(script_name));
        let url = AkribesClient::url_with_query(&base, &Q { limit, offset });
        self.c().get_list(&url).await
    }

    /// `POST /projects/{id}/scripts/{name}/bench/runs` — trigger a run.
    /// `case_ids` constrains the fan-out to a subset (partial run).
    pub async fn trigger_run(
        &self,
        script_name: &str,
        req: &TriggerBenchRunRequest,
    ) -> Result<BenchRun> {
        let url = format!("{}/runs", self.bench_url(script_name));
        self.c().post(&url, req).await
    }
}

// ── Run-scoped (cross-project) client ────────────────────────────────────────

/// Operations keyed on a global `bench_runs.id`. These endpoints live under
/// `/bench-runs/{id}/...` and don't need a project scope (the server resolves
/// the owning project from the run row). Obtained via `client.bench_runs()`.
#[derive(Clone, Debug)]
pub struct BenchRunsClient {
    pub(crate) inner: Arc<Inner>,
}

impl BenchRunsClient {
    pub(crate) fn new(inner: Arc<Inner>) -> Self {
        Self { inner }
    }

    fn c(&self) -> AkribesClient {
        AkribesClient {
            inner: Arc::clone(&self.inner),
        }
    }

    fn run_url(&self, run_id: i64) -> String {
        format!("{}/bench-runs/{}", self.inner.base_url, run_id)
    }

    /// `GET /bench-runs/{id}` — 404 → `Ok(None)`.
    pub async fn get(&self, run_id: i64) -> Result<Option<BenchRun>> {
        self.c().get_opt(&self.run_url(run_id)).await
    }

    /// `DELETE /bench-runs/{id}` — returns `()` (server emits 204 No Content).
    /// Cancels the run first (best-effort) before dropping the row.
    pub async fn delete(&self, run_id: i64) -> Result<()> {
        // `AkribesClient::delete` swallows the body and reports a bool
        // (deleted vs already-absent). The bench delete endpoint emits 204,
        // which is "deleted" — we discard the bool to give consumers a clean
        // `()` return.
        self.c().delete(&self.run_url(run_id)).await?;
        Ok(())
    }

    /// `GET /bench-runs/{id}/results`. 404 → empty list.
    pub async fn list_results(&self, run_id: i64) -> Result<Vec<BenchResult>> {
        let url = format!("{}/results", self.run_url(run_id));
        self.c().get_list(&url).await
    }

    /// `GET /bench-runs/{id}/events?after_id=N` — poll-style page of bench
    /// events. The server's primary surface for run events is SSE
    /// (`/bench-runs/{id}/events`), but the MCP family pulls a JSON page
    /// shape via the same path; we expose the JSON form here.
    pub async fn events(&self, run_id: i64, after_id: Option<i64>) -> Result<BenchRunEventsPage> {
        #[derive(Serialize)]
        struct Q {
            #[serde(skip_serializing_if = "Option::is_none")]
            after_id: Option<i64>,
        }
        let base = format!("{}/events", self.run_url(run_id));
        let url = AkribesClient::url_with_query(&base, &Q { after_id });
        Ok(self
            .c()
            .get_opt::<BenchRunEventsPage>(&url)
            .await?
            .unwrap_or_default())
    }

    /// `POST /bench-runs/{id}/cancel`. Flips the cancel token; in-flight cases
    /// complete naturally. Returns the run row as it stands.
    pub async fn cancel(&self, run_id: i64) -> Result<BenchRun> {
        let url = format!("{}/cancel", self.run_url(run_id));
        let empty: serde_json::Value = serde_json::json!({});
        self.c().post(&url, &empty).await
    }

    /// `PATCH /bench-runs/{id}/tag-session` — attribute the run to an MCP
    /// session id so the coordinator's finalize step posts the cost into
    /// `mcp_session_cost`.
    pub async fn tag_session(
        &self,
        run_id: i64,
        mcp_session_id: &str,
    ) -> Result<BenchRunTagSessionResponse> {
        #[derive(Serialize)]
        struct Body<'a> {
            mcp_session_id: &'a str,
        }
        let url = format!("{}/tag-session", self.run_url(run_id));
        self.c().patch(&url, &Body { mcp_session_id }).await
    }

    /// `POST /executions/{exec_id}/promote-to-case` — promote a completed
    /// execution into a bench case, with optional `edits` overlay.
    ///
    /// This is run-scoped only in the loose sense: it lives on `/executions`
    /// rather than `/bench-runs`, but it's the natural counterpart to the
    /// "promote-from-execution" flow on the bench surface and doesn't need a
    /// project_id (the server resolves the owning project from the source
    /// execution).
    pub async fn promote_execution(
        &self,
        execution_id: &str,
        req: &PromoteExecutionRequest,
    ) -> Result<BenchCase> {
        let url = format!(
            "{}/executions/{}/promote-to-case",
            self.inner.base_url,
            urlencoding::encode(execution_id),
        );
        self.c().post(&url, req).await
    }

    /// `GET /bench-runs/{a}/compare/{b}` — diff two runs of the same bench.
    pub async fn compare(&self, run_a: i64, run_b: i64) -> Result<CompareReport> {
        let url = format!(
            "{}/bench-runs/{}/compare/{}",
            self.inner.base_url, run_a, run_b,
        );
        Ok(self
            .c()
            .get_opt::<CompareReport>(&url)
            .await?
            .ok_or_else(|| crate::error::AkribesError::HttpStatus {
                status: 404,
                message: format!("compare runs {}↔{} returned 404", run_a, run_b),
            })?)
    }

    // ── Case-id keyed operations ────────────────────────────────────────────
    //
    // `PATCH /cases/{id}` and `DELETE /cases/{id}` live under `/cases` (no
    // project scope) — same naming as `/bench-runs/{id}`. Surface them on
    // the same global client.

    /// `GET /executions/{case_id}` — fetch a single case (cases are
    /// `executions` rows with `kind='case'`). Returned as `Value` for
    /// compatibility with the MCP tool, which doesn't trust the row to
    /// always type-check as `BenchCase` (legacy promoted-execution rows can
    /// have null kind on older servers).
    pub async fn get_case(&self, case_id: &str) -> Result<serde_json::Value> {
        let url = format!(
            "{}/executions/{}",
            self.inner.base_url,
            urlencoding::encode(case_id),
        );
        Ok(self
            .c()
            .get_opt::<serde_json::Value>(&url)
            .await?
            .unwrap_or(serde_json::Value::Null))
    }

    /// `PATCH /cases/{id}` — sparse update.
    pub async fn patch_case(
        &self,
        case_id: &str,
        req: &PatchBenchCaseRequest,
    ) -> Result<BenchCase> {
        let url = format!(
            "{}/cases/{}",
            self.inner.base_url,
            urlencoding::encode(case_id),
        );
        self.c().patch(&url, req).await
    }

    /// `DELETE /cases/{id}`. The server emits a `{"deleted": true}` JSON body;
    /// we discard it and return `()`.
    pub async fn delete_case(&self, case_id: &str) -> Result<()> {
        let url = format!(
            "{}/cases/{}",
            self.inner.base_url,
            urlencoding::encode(case_id),
        );
        self.c().delete(&url).await?;
        Ok(())
    }

    /// `GET /benches/{id}` — fast bench-by-id lookup. Returns the bench
    /// row joined with the owning `project_id` + `script_name` so the
    /// caller can chain into list_cases / list_runs without an N+1
    /// project walk. 404 → `Ok(None)`.
    pub async fn bench_by_id(&self, bench_id: i64) -> Result<Option<serde_json::Value>> {
        let url = format!("{}/benches/{}", self.inner.base_url, bench_id);
        self.c().get_json_value_opt(&url).await
    }

    /// `GET /mcp-sessions/{id}/cost` — aggregated cost for an MCP
    /// session. Returns `{session_id, total_cost_usd, breakdown}`.
    /// Lets the MCP server (and any other client) read accumulated
    /// cost via HTTP rather than querying the `mcp_session_cost`
    /// table directly.
    pub async fn mcp_session_cost(&self, session_id: &str) -> Result<serde_json::Value> {
        let url = format!(
            "{}/mcp-sessions/{}/cost",
            self.inner.base_url,
            urlencoding::encode(session_id),
        );
        self.c().get_json_value(&url).await
    }
}
