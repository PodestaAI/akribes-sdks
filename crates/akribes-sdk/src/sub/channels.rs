use std::sync::Arc;

use crate::client::{AkribesClient, Inner};
use crate::error::Result;
use crate::models::*;

/// Sub-client for script channels. Obtained via [`AkribesClient::channels()`].
#[derive(Clone, Debug)]
pub struct ChannelsClient {
    pub(crate) inner: Arc<Inner>,
    pub(crate) project_id: i64,
}

impl ChannelsClient {
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

    pub async fn list(&self, script_name: &str) -> Result<Vec<ScriptChannel>> {
        let url = format!("{}/channels", self.script_url(script_name));
        self.c().get_list(&url).await
    }

    pub async fn create(&self, script_name: &str, channel_name: &str) -> Result<ScriptChannel> {
        let url = format!("{}/channels", self.script_url(script_name));
        self.c()
            .post(&url, &CreateChannelRequest { name: channel_name })
            .await
    }

    pub async fn delete(&self, script_name: &str, channel_name: &str) -> Result<()> {
        let url = format!(
            "{}/channels/{}",
            self.script_url(script_name),
            urlencoding::encode(channel_name)
        );
        self.c().delete(&url).await?;
        Ok(())
    }

    pub async fn move_to(
        &self,
        script_name: &str,
        channel_name: &str,
        version_id: i64,
        force: bool,
    ) -> Result<()> {
        let url = format!(
            "{}/channels/{}",
            self.script_url(script_name),
            urlencoding::encode(channel_name)
        );
        self.c()
            .patch_empty(
                &url,
                &MoveChannelRequest {
                    version_id,
                    force: if force { Some(true) } else { None },
                },
            )
            .await
    }
}
