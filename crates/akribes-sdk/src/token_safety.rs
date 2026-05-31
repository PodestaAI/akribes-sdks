//! Refuse to embed a non-scoped token in a URL query string.
//!
//! Scoped tokens (`akribes_tk_…` / `aura_tk_…`) are short-lived and
//! revokable, so appearing in access logs is acceptable. The raw
//! `<secret>` half of `AKRIBES_SERVICE_TOKEN_<NAME>=*:<secret>` is
//! long-lived and wildcard-Admin in production deployments — a single
//! leak equals full platform compromise (PENTEST CRITICAL-02). Backends
//! using a service token MUST authenticate via the `Authorization`
//! header, never via `?token=` in a URL that reverse-proxy access logs
//! and OTel `http.url` span attributes will capture.

use crate::error::AkribesError;

/// Returns `Ok(())` if *token* is a recognised scoped-token form and
/// therefore safe to embed in a URL query string; returns
/// `Err(AkribesError::Other)` otherwise so the caller is forced onto
/// the header-bearer auth path.
pub fn assert_token_safe_in_url(token: &str) -> Result<(), AkribesError> {
    if token.starts_with("akribes_tk_") || token.starts_with("aura_tk_") {
        return Ok(());
    }
    Err(AkribesError::Other(
        "Refusing to put a non-scoped token in the URL query string. \
         Scoped tokens (akribes_tk_…) may be passed in ?token= because \
         they are short-lived and revokable; service tokens (the secret \
         half of AKRIBES_SERVICE_TOKEN_<NAME>=*:secret) MUST use header \
         bearer auth and never appear in URLs that hit access logs."
            .to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_token_accepted() {
        assert!(assert_token_safe_in_url("akribes_tk_abc123").is_ok());
        assert!(assert_token_safe_in_url("aura_tk_legacyhex").is_ok());
    }

    #[test]
    fn service_token_secret_rejected() {
        let err = assert_token_safe_in_url("puto-secret-padded-to-thirtytwo-bytes-aaaa")
            .expect_err("service-token secret must be rejected");
        assert!(format!("{err:?}").contains("Refusing"));
    }

    #[test]
    fn opaque_bearer_rejected() {
        assert!(assert_token_safe_in_url("eyJhbGciOi...JWT-shape").is_err());
        assert!(assert_token_safe_in_url("").is_err());
    }
}
