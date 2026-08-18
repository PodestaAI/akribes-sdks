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

    /// Run the canonical server-side check against `source` (or, when
    /// `source` is `None`, the script's stored draft). Read-only: never
    /// writes the draft and never spawns an execution. Returns the server
    /// analyzer's verdict — the single source of truth that draft saves,
    /// publish gating, and the MCP tools all consume.
    ///
    /// `POST /projects/{id}/scripts/{name}/check`.
    pub async fn check(&self, name: &str, source: Option<&str>) -> Result<CheckResponse> {
        let url = format!("{}/check", self.script_url(name));
        // The endpoint's Json extractor rejects a truly empty body, so an
        // omitted `source` is sent as `{}` (server then checks the draft).
        self.c().post(&url, &CheckRequest { source }).await
    }
}

#[cfg(test)]
mod tests {
    use crate::models::{CheckResponse, PutDraftResponse};

    /// Back-compat: an old server (pre-diagnostics) omits `diagnostics` and
    /// `analyzer_version` from its draft-save response. The SDK's
    /// serde-defaulted fields must still decode such a payload, yielding an
    /// empty verdict rather than a decode error.
    #[test]
    fn put_draft_response_backcompat() {
        let old: PutDraftResponse = serde_json::from_str(r#"{"schema_warnings":[],"inputs":[],"type_defs":[],"updated_at":"2026-07-11T00:00:00Z"}"#).unwrap();
        assert!(old.diagnostics.is_empty());
    }

    /// A new server carries the verdict inline; both new fields decode.
    #[test]
    fn put_draft_response_new_server_fields() {
        let new: PutDraftResponse = serde_json::from_str(
            r#"{"schema_warnings":[],"diagnostics":[{"code":"AKRIBES-E-PARSE","severity":"error","message":"boom","line":1,"col":1}],"analyzer_version":"0.25.2"}"#,
        )
        .unwrap();
        assert_eq!(new.diagnostics.len(), 1);
        assert_eq!(new.diagnostics[0].code, "AKRIBES-E-PARSE");
        assert_eq!(new.analyzer_version.as_deref(), Some("0.25.2"));
    }

    /// `CheckResponse` / `ApiDiagnostic` mirror the server wire shape
    /// field-for-field. The literal below is copied from the serialization
    /// of `akribes_server::models::CheckResponse` + `ApiDiagnostic`.
    /// `end_line`/`end_col` are serde-defaulted, so a diagnostic that omits
    /// them decodes to `None`.
    #[test]
    fn check_response_roundtrip() {
        let wire = r#"{"ok":false,"diagnostics":[{"code":"AKRIBES-E-TYPE-MISMATCH","severity":"error","message":"type mismatch","line":3,"col":5,"end_line":3,"end_col":12},{"code":"","severity":"warning","message":"legacy diagnostic","line":1,"col":1}],"analyzer_version":"0.25.2"}"#;
        let resp: CheckResponse = serde_json::from_str(wire).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.analyzer_version, "0.25.2");
        assert_eq!(resp.diagnostics.len(), 2);

        let first = &resp.diagnostics[0];
        assert_eq!(first.code, "AKRIBES-E-TYPE-MISMATCH");
        assert_eq!(first.severity, "error");
        assert_eq!(first.line, 3);
        assert_eq!(first.col, 5);
        assert_eq!(first.end_line, Some(3));
        assert_eq!(first.end_col, Some(12));

        // A legacy diagnostic without an end span decodes to None.
        let second = &resp.diagnostics[1];
        assert_eq!(second.end_line, None);
        assert_eq!(second.end_col, None);

        // Full round-trip: serialize back out and re-parse to an equal value.
        let reserialized = serde_json::to_string(&resp).unwrap();
        let reparsed: CheckResponse = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(reparsed.diagnostics.len(), 2);
        assert_eq!(reparsed.diagnostics[0].end_col, Some(12));
        assert_eq!(reparsed.diagnostics[1].end_line, None);
    }
}
