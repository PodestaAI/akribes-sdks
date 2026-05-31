use std::sync::Arc;

use crate::client::{AkribesClient, Inner};
use crate::error::Result;
use crate::models::*;

/// Sub-client for scoped token management. Obtained via [`AkribesClient::tokens()`].
/// Not project-scoped — tokens are a global resource.
#[derive(Clone, Debug)]
pub struct TokensClient {
    pub(crate) inner: Arc<Inner>,
}

impl TokensClient {
    pub(crate) fn new(inner: Arc<Inner>) -> Self {
        Self { inner }
    }

    fn c(&self) -> AkribesClient {
        AkribesClient {
            inner: Arc::clone(&self.inner),
        }
    }

    fn base_url(&self) -> String {
        format!("{}/tokens", self.inner.base_url)
    }

    /// Mint a new scoped token. Only service tokens can mint.
    pub async fn mint(&self, req: &MintTokenRequest) -> Result<MintTokenResponse> {
        self.c().post(&self.base_url(), req).await
    }

    /// List tokens. Service tokens see all; scoped tokens see only their own.
    pub async fn list(&self) -> Result<Vec<TokenInfo>> {
        self.c().get_list(&self.base_url()).await
    }

    /// Revoke a single token by ID.
    pub async fn revoke(&self, token_id: &str) -> Result<()> {
        let url = format!("{}/{}", self.base_url(), token_id);
        self.c().delete(&url).await?;
        Ok(())
    }

    /// Revoke all tokens for a user email (offboarding). Only service tokens can do this.
    pub async fn revoke_by_email(&self, email: &str) -> Result<RevokeByEmailResponse> {
        let url = format!(
            "{}?user_email={}",
            self.base_url(),
            urlencoding::encode(email)
        );
        self.c().delete_json(&url).await
    }
}
