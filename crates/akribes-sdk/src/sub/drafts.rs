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

/// Opt-in flags for [`DraftsClient::save_with_options`]. Both default to
/// `false`, which is exactly the historical [`DraftsClient::save`] behaviour
/// (the flags are omitted from the wire body entirely).
#[derive(Default, Clone, Copy, Debug)]
pub struct SaveDraftOptions {
    /// Reject the save with an `invalid_source` HTTP 400 when the source fails
    /// to parse, persisting nothing, instead of storing it with diagnostics.
    pub require_parse: bool,
    /// Server-side canonical-format the source before persisting it.
    pub format: bool,
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
        self.save_with_options(script_name, source, SaveDraftOptions::default())
            .await
    }

    /// Save a draft with explicit [`SaveDraftOptions`] (parse-gating /
    /// server-side formatting). [`save`](Self::save) delegates here with the
    /// default (both-false) options, so callers that don't need the flags pay
    /// nothing.
    pub async fn save_with_options(
        &self,
        script_name: &str,
        source: &str,
        opts: SaveDraftOptions,
    ) -> Result<PutDraftResponse> {
        let url = format!("{}/draft", self.script_url(script_name));
        self.c()
            .put_json(
                &url,
                &PutDraftRequest {
                    source,
                    require_parse: opts.require_parse,
                    format: opts.format,
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use crate::models::PutDraftRequest;

    /// The plain `save` path builds a request with both flags false; the
    /// serde-skip keeps them off the wire entirely, so old servers and every
    /// existing caller send a byte-for-byte identical `{ "source": ... }` body.
    #[test]
    fn save_body_omits_flags_by_default() {
        let body = serde_json::to_value(PutDraftRequest {
            source: "x",
            require_parse: false,
            format: false,
        })
        .unwrap();
        assert_eq!(body.get("source").and_then(|v| v.as_str()), Some("x"));
        assert!(body.get("require_parse").is_none());
        assert!(body.get("format").is_none());
    }

    /// Opting into parse-gating (or server-side formatting) puts the flag on
    /// the wire as `true`; a server that predates the field ignores the key.
    #[test]
    fn save_body_includes_flags_when_set() {
        let body = serde_json::to_value(PutDraftRequest {
            source: "x",
            require_parse: true,
            format: true,
        })
        .unwrap();
        assert_eq!(
            body.get("require_parse").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(body.get("format").and_then(|v| v.as_bool()), Some(true));
    }
}
