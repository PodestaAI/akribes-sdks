use std::sync::Arc;

use crate::client::{AkribesClient, Inner};
use crate::error::Result;
use crate::models::*;

/// Sub-client for project management. Obtained via [`AkribesClient::projects()`].
///
/// Does **not** require a `project_id` on the parent client.
#[derive(Clone, Debug)]
pub struct ProjectsClient {
    pub(crate) inner: Arc<Inner>,
}

impl ProjectsClient {
    pub(crate) fn new(inner: Arc<Inner>) -> Self {
        Self { inner }
    }

    /// Wrap as an AkribesClient to reuse HTTP helpers.
    fn c(&self) -> AkribesClient {
        AkribesClient {
            inner: Arc::clone(&self.inner),
        }
    }

    pub async fn list(&self) -> Result<Vec<Project>> {
        let url = format!("{}/projects", self.inner.base_url);
        self.c().get_list(&url).await
    }

    pub async fn get(&self, project_id: i64) -> Result<Option<Project>> {
        let url = format!("{}/projects/{}", self.inner.base_url, project_id);
        self.c().get_opt(&url).await
    }

    pub async fn create(&self, name: &str) -> Result<Project> {
        let url = format!("{}/projects", self.inner.base_url);
        self.c().post(&url, &CreateProjectRequest { name }).await
    }

    pub async fn update(&self, project_id: i64, name: &str) -> Result<Project> {
        let url = format!("{}/projects/{}", self.inner.base_url, project_id);
        self.c().patch(&url, &UpdateProjectRequest { name }).await
    }

    /// Look up a project by numeric id (if `id_or_name` parses as `i64`) or
    /// by exact name. Returns `None` when no project matches.
    ///
    /// Numeric-looking inputs are treated as ids first; a caller that really
    /// wants to look up a project *named* e.g. `"42"` must currently fetch
    /// the full list and filter themselves.
    pub async fn resolve(&self, id_or_name: &str) -> Result<Option<Project>> {
        if let Ok(id) = id_or_name.parse::<i64>() {
            return self.get(id).await;
        }
        let list = self.list().await?;
        Ok(list.into_iter().find(|p| p.name == id_or_name))
    }

    pub async fn delete(&self, project_id: i64) -> Result<()> {
        let url = format!("{}/projects/{}", self.inner.base_url, project_id);
        self.c().delete(&url).await?;
        Ok(())
    }

    /// Duplicate a project (including all scripts). Server picks the copy
    /// name. Requires a wildcard-scoped identity.
    pub async fn duplicate(&self, project_id: i64) -> Result<Project> {
        let url = format!("{}/projects/{}/duplicate", self.inner.base_url, project_id);
        self.c().post(&url, &serde_json::json!({})).await
    }

    /// Set the global project ordering. `order` is the list of project IDs
    /// in the desired order. Requires a wildcard-scoped identity.
    pub async fn reorder(&self, order: Vec<i64>) -> Result<()> {
        let url = format!("{}/projects/reorder", self.inner.base_url);
        self.c().put_empty(&url, &ReorderRequest { order }).await
    }

    // ── Flat cross-project script ops ───────────────────────────────────
    //
    // The project-scoped chain (`client.project(id).scripts().X`) is the
    // primary surface for script management, but admin-style code that
    // touches several projects in a row reads more naturally with flat,
    // cross-project ops on `projects` itself — same as TS `projects.*`.
    // Both surfaces coexist; these delegate to the equivalent server
    // endpoints directly to avoid an extra constructor allocation.

    fn script_url(&self, project_id: i64, script_name: &str) -> String {
        format!(
            "{}/projects/{}/scripts/{}",
            self.inner.base_url,
            project_id,
            urlencoding::encode(script_name)
        )
    }

    /// List scripts in a specific project. Flat alternative to
    /// `client.project(id).scripts().list()`.
    pub async fn list_scripts(&self, project_id: i64) -> Result<Vec<Script>> {
        let url = format!("{}/projects/{}/scripts", self.inner.base_url, project_id);
        self.c().get_list(&url).await
    }

    /// Move a script from `src_project_id` to `dest_project_id`. Flat
    /// alternative to `client.project(src).scripts().move_to(name, dest)`.
    pub async fn move_script(
        &self,
        src_project_id: i64,
        src_script_name: &str,
        dest_project_id: i64,
    ) -> Result<Script> {
        let url = format!("{}/move", self.script_url(src_project_id, src_script_name));
        self.c()
            .post(
                &url,
                &MoveScriptRequest {
                    target_project_id: dest_project_id,
                },
            )
            .await
    }

    /// Rename a script in `project_id`. Flat alternative to
    /// `client.project(id).scripts().rename(current, new)`.
    pub async fn rename_script(
        &self,
        project_id: i64,
        current_name: &str,
        new_name: &str,
    ) -> Result<()> {
        let url = self.script_url(project_id, current_name);
        self.c()
            .patch_empty(&url, &RenameScriptRequest { new_name })
            .await
    }

    /// Delete a script in `project_id`. Flat alternative to
    /// `client.project(id).scripts().delete(name)`.
    pub async fn delete_script(&self, project_id: i64, script_name: &str) -> Result<()> {
        let url = self.script_url(project_id, script_name);
        self.c().delete(&url).await?;
        Ok(())
    }

    /// Duplicate a script within `project_id`. The server picks the copy
    /// name; `new_name` is accepted for parity with the TypeScript SDK and
    /// will be honored once the server supports it. Flat alternative to
    /// `client.project(id).scripts().duplicate(name)`.
    pub async fn duplicate_script(
        &self,
        project_id: i64,
        script_name: &str,
        _new_name: Option<&str>,
    ) -> Result<Script> {
        let url = format!("{}/duplicate", self.script_url(project_id, script_name));
        self.c().post(&url, &serde_json::json!({})).await
    }

    /// List channels for `script_name` in `project_id` (#1141). Flat
    /// cross-project alternative to
    /// `client.project(id).channels().list(name)`. Mirrors TS
    /// `projects.listChannels`.
    pub async fn list_channels(
        &self,
        project_id: i64,
        script_name: &str,
    ) -> Result<Vec<ScriptChannel>> {
        let url = format!("{}/channels", self.script_url(project_id, script_name));
        self.c().get_list(&url).await
    }

    /// Reorder scripts within `project_id` (#1141). `order` is the list of
    /// script IDs in the desired order. Flat cross-project alternative to
    /// `client.project(id).scripts().reorder(order)`. Mirrors TS
    /// `projects.reorderScripts`.
    pub async fn reorder_scripts(&self, project_id: i64, order: Vec<i64>) -> Result<()> {
        let url = format!(
            "{}/projects/{}/scripts/reorder",
            self.inner.base_url, project_id
        );
        self.c().put_empty(&url, &ReorderRequest { order }).await
    }
}
