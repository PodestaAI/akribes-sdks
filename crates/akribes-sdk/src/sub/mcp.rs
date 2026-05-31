use std::sync::Arc;

use crate::client::{AkribesClient, Inner};
use crate::error::Result;
use crate::models::{
    McpDriftResult, McpHealth, McpRefreshResult, McpServerSummary, McpToolSummary,
};

/// Sub-client for MCP server/tool discovery. Obtained via
/// [`crate::client::ProjectScope::mcp()`].
#[derive(Clone, Debug)]
pub struct McpClient {
    pub(crate) inner: Arc<Inner>,
    pub(crate) project_id: i64,
}

impl McpClient {
    pub(crate) fn new(inner: Arc<Inner>, project_id: i64) -> Self {
        Self { inner, project_id }
    }

    fn c(&self) -> AkribesClient {
        AkribesClient {
            inner: Arc::clone(&self.inner),
        }
    }

    fn base(&self) -> String {
        format!("{}/projects/{}/mcp", self.inner.base_url, self.project_id)
    }

    pub async fn list_servers(&self) -> Result<Vec<McpServerSummary>> {
        let url = format!("{}/servers", self.base());
        self.c().get_list(&url).await
    }

    pub async fn list_tools(&self) -> Result<Vec<McpToolSummary>> {
        let url = format!("{}/tools", self.base());
        self.c().get_list(&url).await
    }

    pub async fn health(&self, alias: &str) -> Result<McpHealth> {
        let url = format!(
            "{}/servers/{}/health",
            self.base(),
            urlencoding::encode(alias)
        );
        let res = self.c().get_opt::<McpHealth>(&url).await?;
        res.ok_or_else(|| crate::error::AkribesError::HttpStatus {
            status: 404,
            message: format!("mcp server '{}' not found", alias),
        })
    }

    /// Force a fresh `tools/list` against the remote MCP server and update
    /// the pinned schema in the DB.
    pub async fn refresh(&self, alias: &str) -> Result<McpRefreshResult> {
        let url = format!(
            "{}/servers/{}/refresh",
            self.base(),
            urlencoding::encode(alias)
        );
        self.c().post(&url, &serde_json::json!({})).await
    }

    /// Compare the pinned schema against the remote server's live `tools/list`
    /// and report added/removed tool names.
    pub async fn drift(&self, alias: &str) -> Result<McpDriftResult> {
        let url = format!(
            "{}/servers/{}/drift",
            self.base(),
            urlencoding::encode(alias)
        );
        let res = self.c().get_opt::<McpDriftResult>(&url).await?;
        res.ok_or_else(|| crate::error::AkribesError::HttpStatus {
            status: 404,
            message: format!("mcp server '{}' not found", alias),
        })
    }
}
