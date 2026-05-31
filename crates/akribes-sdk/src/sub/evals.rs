use std::sync::Arc;

use crate::client::{AkribesClient, Inner};
use crate::error::Result;
use crate::models::*;

/// Sub-client for eval suites and runs. Obtained via [`AkribesClient::evals()`].
#[derive(Clone, Debug)]
pub struct EvalsClient {
    pub(crate) inner: Arc<Inner>,
    pub(crate) project_id: i64,
}

impl EvalsClient {
    pub(crate) fn new(inner: Arc<Inner>, project_id: i64) -> Self {
        Self { inner, project_id }
    }

    fn c(&self) -> AkribesClient {
        AkribesClient {
            inner: Arc::clone(&self.inner),
        }
    }

    fn project_url(&self) -> String {
        format!("{}/projects/{}", self.inner.base_url, self.project_id)
    }

    fn script_url(&self, name: &str) -> String {
        format!(
            "{}/scripts/{}",
            self.project_url(),
            urlencoding::encode(name)
        )
    }

    // ── Suites ──────────────────────────────────────────────────────────────

    /// List all eval suites for a script.
    pub async fn list_suites(&self, script_name: &str) -> Result<Vec<EvalSuite>> {
        let url = format!("{}/eval-suites", self.script_url(script_name));
        self.c().get_list(&url).await
    }

    /// Create a new eval suite.
    pub async fn create_suite(
        &self,
        script_name: &str,
        name: &str,
        runner_url: &str,
        config: Option<serde_json::Value>,
        auto_run_channels: Option<Vec<String>>,
    ) -> Result<EvalSuite> {
        let url = format!("{}/eval-suites", self.script_url(script_name));
        self.c()
            .post(
                &url,
                &CreateEvalSuiteRequest {
                    name,
                    runner_url,
                    config,
                    auto_run_channels,
                },
            )
            .await
    }

    /// Get an eval suite by ID.
    pub async fn get_suite(&self, script_name: &str, suite_id: i64) -> Result<Option<EvalSuite>> {
        let url = format!("{}/eval-suites/{}", self.script_url(script_name), suite_id);
        self.c().get_opt(&url).await
    }

    /// Update an eval suite (PATCH — only provided fields are changed).
    pub async fn update_suite(
        &self,
        script_name: &str,
        suite_id: i64,
        runner_url: Option<String>,
        config: Option<serde_json::Value>,
        auto_run_channels: Option<Vec<String>>,
    ) -> Result<EvalSuite> {
        let url = format!("{}/eval-suites/{}", self.script_url(script_name), suite_id);
        self.c()
            .patch(
                &url,
                &UpdateEvalSuiteRequest {
                    runner_url,
                    config,
                    auto_run_channels,
                },
            )
            .await
    }

    /// Delete an eval suite.
    pub async fn delete_suite(&self, script_name: &str, suite_id: i64) -> Result<bool> {
        let url = format!("{}/eval-suites/{}", self.script_url(script_name), suite_id);
        self.c().delete(&url).await
    }

    /// Check the health of a suite's eval runner.
    pub async fn check_runner_health(
        &self,
        script_name: &str,
        suite_id: i64,
    ) -> Result<serde_json::Value> {
        let url = format!(
            "{}/eval-suites/{}/health",
            self.script_url(script_name),
            suite_id
        );
        Ok(self
            .c()
            .get_opt::<serde_json::Value>(&url)
            .await?
            .unwrap_or(serde_json::json!({})))
    }

    // ── Trigger ─────────────────────────────────────────────────────────────

    /// Trigger an eval run for a suite.
    pub async fn trigger(
        &self,
        script_name: &str,
        suite_id: i64,
        source: Option<String>,
        channel: Option<String>,
        auto_publish: Option<bool>,
        triggered_by: Option<String>,
    ) -> Result<EvalRun> {
        let url = format!(
            "{}/eval-suites/{}/trigger",
            self.script_url(script_name),
            suite_id
        );
        self.c()
            .post(
                &url,
                &TriggerEvalRequest {
                    source,
                    channel,
                    auto_publish,
                    triggered_by,
                },
            )
            .await
    }

    // ── Runs & Results ──────────────────────────────────────────────────────

    /// List eval runs for a script, optionally filtered by suite.
    pub async fn list_runs(
        &self,
        script_name: &str,
        suite_id: Option<i64>,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> Result<Vec<EvalRun>> {
        #[derive(serde::Serialize)]
        struct Q {
            #[serde(skip_serializing_if = "Option::is_none")]
            suite_id: Option<i64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            limit: Option<i64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            offset: Option<i64>,
        }
        let base = format!("{}/eval-runs", self.script_url(script_name));
        let url = AkribesClient::url_with_query(
            &base,
            &Q {
                suite_id,
                limit,
                offset,
            },
        );
        self.c().get_list(&url).await
    }

    /// Get an eval run by ID (global endpoint, no project needed).
    pub async fn get_run(&self, run_id: i64) -> Result<Option<EvalRun>> {
        let url = format!("{}/eval-runs/{}", self.inner.base_url, run_id);
        self.c().get_opt(&url).await
    }

    /// Cancel a pending or running eval run. Returns the updated run.
    pub async fn cancel(&self, run_id: i64) -> Result<EvalRun> {
        let url = format!("{}/eval-runs/{}", self.inner.base_url, run_id);
        self.c().delete_json(&url).await
    }

    /// Get results for an eval run.
    pub async fn get_results(&self, run_id: i64) -> Result<Vec<EvalResult>> {
        let url = format!("{}/eval-runs/{}/results", self.inner.base_url, run_id);
        self.c().get_list(&url).await
    }

    // ── Project-level cross-script dashboard (sub-spec 1a) ──────────────────

    /// One summary row per eval suite in this project, with the latest +
    /// prior-completed average score (drives the cross-script dashboard).
    pub async fn list_project_summaries(&self) -> Result<Vec<EvalSuiteSummary>> {
        let url = format!("{}/eval-suite-summaries", self.project_url());
        self.c().get_list(&url).await
    }
}
