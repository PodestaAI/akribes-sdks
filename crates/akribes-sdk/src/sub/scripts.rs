use std::sync::Arc;

use crate::client::{AkribesClient, Inner};
use crate::error::Result;
use crate::models::*;

/// Sub-client for script management. Obtained via `AkribesClient::project(id).scripts()`.
#[derive(Clone, Debug)]
pub struct ScriptsClient {
    pub(crate) inner: Arc<Inner>,
    pub(crate) project_id: i64,
}

impl ScriptsClient {
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

    pub async fn list(&self) -> Result<Vec<Script>> {
        let url = format!("{}/scripts", self.project_url());
        self.c().get_list(&url).await
    }

    pub async fn get(&self, name: &str) -> Result<Option<Script>> {
        self.c().get_opt(&self.script_url(name)).await
    }

    pub async fn create(&self, name: &str, source: &str) -> Result<Script> {
        let encoded = urlencoding::encode(name);
        let url = format!("{}/scripts?name={}", self.project_url(), encoded);
        self.c().post(&url, &CreateScriptBody { source }).await
    }

    pub async fn rename(&self, old_name: &str, new_name: &str) -> Result<()> {
        self.c()
            .patch_empty(
                &self.script_url(old_name),
                &RenameScriptRequest { new_name },
            )
            .await
    }

    /// Look up a script by numeric id (if `id_or_name` parses as `i64`) or by
    /// exact name. Returns `None` when nothing matches. Ids are resolved by
    /// listing and filtering — there is no GET-by-id server route.
    pub async fn resolve(&self, id_or_name: &str) -> Result<Option<Script>> {
        if let Ok(id) = id_or_name.parse::<i64>() {
            let list = self.list().await?;
            return Ok(list.into_iter().find(|s| s.id == id));
        }
        self.get(id_or_name).await
    }

    pub async fn delete(&self, name: &str) -> Result<()> {
        self.c().delete(&self.script_url(name)).await?;
        Ok(())
    }

    /// Duplicate a script within this project. The server picks a copy name
    /// (e.g. `foo copy`) and returns the new script.
    pub async fn duplicate(&self, name: &str) -> Result<Script> {
        let url = format!("{}/duplicate", self.script_url(name));
        self.c().post(&url, &serde_json::json!({})).await
    }

    /// Move a script to another project. Returns the moved script (now scoped
    /// to the target project).
    pub async fn move_to(&self, name: &str, target_project_id: i64) -> Result<Script> {
        let url = format!("{}/move", self.script_url(name));
        self.c()
            .post(&url, &MoveScriptRequest { target_project_id })
            .await
    }

    /// Set the sort order of scripts in this project. `order` is the list of
    /// script IDs in the desired order.
    pub async fn reorder(&self, order: Vec<i64>) -> Result<()> {
        let url = format!("{}/scripts/reorder", self.project_url());
        self.c().put_empty(&url, &ReorderRequest { order }).await
    }
}
