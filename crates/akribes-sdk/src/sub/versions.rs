use std::sync::Arc;

use crate::client::{AkribesClient, Inner};
use crate::error::Result;
use crate::models::*;

/// Sub-client for script versions. Obtained via `AkribesClient::project(id).versions()`.
#[derive(Clone, Debug)]
pub struct VersionsClient {
    pub(crate) inner: Arc<Inner>,
    pub(crate) project_id: i64,
}

impl VersionsClient {
    pub(crate) fn new(inner: Arc<Inner>, project_id: i64) -> Self {
        Self { inner, project_id }
    }

    fn c(&self) -> AkribesClient {
        AkribesClient {
            inner: Arc::clone(&self.inner),
        }
    }

    fn script_url(&self, name: &str) -> String {
        format!(
            "{}/projects/{}/scripts/{}",
            self.inner.base_url,
            self.project_id,
            urlencoding::encode(name)
        )
    }

    pub async fn list(&self, script_name: &str) -> Result<Vec<ScriptVersion>> {
        let url = format!("{}/versions", self.script_url(script_name));
        self.c().get_list(&url).await
    }

    pub async fn get(&self, script_name: &str, version_id: i64) -> Result<Option<ScriptVersion>> {
        let url = format!("{}/versions/{}", self.script_url(script_name), version_id);
        self.c().get_opt(&url).await
    }

    pub async fn get_latest(&self, script_name: &str) -> Result<Option<LatestVersion>> {
        let url = format!("{}/latest", self.script_url(script_name));
        self.c().get_opt(&url).await
    }

    /// Start building a publish operation.
    pub fn publish(&self, script_name: &str) -> PublishBuilder {
        PublishBuilder {
            client: self.c(),
            project_id: self.project_id,
            script_name: script_name.to_string(),
            channels: vec![],
            label: None,
            published_by: None,
            force: None,
            dry_run: None,
        }
    }

    /// Publish directly without a builder.
    pub async fn publish_version(
        &self,
        script_name: &str,
        channels: Vec<String>,
        label: Option<&str>,
        published_by: Option<&str>,
    ) -> Result<ScriptVersion> {
        let url = format!("{}/publish", self.script_url(script_name));
        let resp: PublishResponse = self
            .c()
            .post(
                &url,
                &PublishRequest {
                    channels,
                    label: label.map(|s| s.to_string()),
                    published_by: published_by.map(|s| s.to_string()),
                    force: None,
                    dry_run: None,
                },
            )
            .await?;
        Ok(resp.version)
    }
}

/// Builder for publishing a script version.
#[derive(Debug, Clone)]
#[must_use = "a builder does nothing until .execute() is called"]
pub struct PublishBuilder {
    client: AkribesClient,
    project_id: i64,
    script_name: String,
    channels: Vec<String>,
    label: Option<String>,
    published_by: Option<String>,
    force: Option<bool>,
    dry_run: Option<bool>,
}

impl PublishBuilder {
    /// Set the channels to publish to.
    pub fn channels(mut self, channels: Vec<String>) -> Self {
        self.channels = channels;
        self
    }

    /// Set a label for this version.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Set who published this version.
    pub fn published_by(mut self, published_by: impl Into<String>) -> Self {
        self.published_by = Some(published_by.into());
        self
    }

    /// Force publish even if it would break existing contracts.
    pub fn force(mut self, force: bool) -> Self {
        self.force = Some(force);
        self
    }

    /// Perform a dry run — check what would break without actually publishing.
    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = Some(dry_run);
        self
    }

    fn publish_url(&self) -> String {
        format!(
            "{}/projects/{}/scripts/{}/publish",
            self.client.inner.base_url,
            self.project_id,
            urlencoding::encode(&self.script_name)
        )
    }

    /// Execute the publish and return the new version plus an optional
    /// rebase summary (present only on first publish — see
    /// [`PublishOutcome`]). Callers that only need the version can use
    /// the `.version` field or [`Self::execute_version_only`].
    pub async fn execute(self) -> Result<crate::models::PublishOutcome> {
        let url = self.publish_url();
        let resp: PublishResponse = self
            .client
            .post(
                &url,
                &PublishRequest {
                    channels: self.channels,
                    label: self.label,
                    published_by: self.published_by,
                    force: self.force,
                    dry_run: None,
                },
            )
            .await?;
        Ok(crate::models::PublishOutcome {
            version: resp.version,
            rebased: resp.rebased,
        })
    }

    /// Backwards-compat: returns only the new [`ScriptVersion`] without
    /// the rebase summary. Equivalent to `execute().await?.version` —
    /// kept so existing callers don't have to thread `PublishOutcome`.
    pub async fn execute_version_only(self) -> Result<ScriptVersion> {
        Ok(self.execute().await?.version)
    }

    /// Execute a dry-run publish — check what would break without publishing.
    pub async fn execute_dry_run(mut self) -> Result<DryRunResult> {
        self.dry_run = Some(true);
        let url = self.publish_url();
        self.client
            .post(
                &url,
                &PublishRequest {
                    channels: self.channels,
                    label: self.label,
                    published_by: self.published_by,
                    force: self.force,
                    dry_run: Some(true),
                },
            )
            .await
    }
}
