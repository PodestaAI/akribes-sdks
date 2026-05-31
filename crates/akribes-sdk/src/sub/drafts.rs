use std::sync::Arc;

use crate::client::{AkribesClient, Inner};
use crate::error::Result;
use crate::models::*;

/// Sub-client for script drafts. Obtained via [`AkribesClient::drafts()`].
#[derive(Clone, Debug)]
pub struct DraftsClient {
    pub(crate) inner: Arc<Inner>,
    pub(crate) project_id: i64,
}

impl DraftsClient {
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

    pub async fn get(&self, script_name: &str) -> Result<Option<Draft>> {
        let url = format!("{}/draft", self.script_url(script_name));
        self.c().get_opt(&url).await
    }

    pub async fn save(&self, script_name: &str, source: &str) -> Result<PutDraftResponse> {
        let url = format!("{}/draft", self.script_url(script_name));
        self.c().put_json(&url, &PutDraftRequest { source }).await
    }
}
