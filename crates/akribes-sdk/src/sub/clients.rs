use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::client::{AkribesClient, Inner};
use crate::error::Result;
use crate::models::*;

/// Canonical SDK-wide heartbeat backoff curve (#1182):
/// exponential with full jitter, base 1s, cap 30s. The first failure
/// waits ~1s before retrying, the second ~2s, ..., capped at ~30s.
/// Mirrors `heartbeatBackoffMs` in the TS SDK and `_heartbeat_backoff_s`
/// in the Python SDK.
fn heartbeat_backoff(consecutive_failures: u32) -> Duration {
    if consecutive_failures == 0 {
        return Duration::ZERO;
    }
    // Saturating shift so a stuck client doesn't overflow.
    let base_ms: u64 = 1_000;
    let cap_ms: u64 = 30_000;
    let exponent = consecutive_failures.saturating_sub(1).min(20);
    let exp_ms = base_ms.saturating_mul(1u64 << exponent).min(cap_ms);
    // Full jitter: uniform random in [0, exp_ms].
    // Use a cheap thread-local RNG via `rand`-free trick: jitter on
    // nanoseconds from a monotonic clock. Not cryptographic; we just
    // need de-synchronisation across clients.
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let jitter_ms = if exp_ms == 0 { 0 } else { now_nanos % exp_ms };
    Duration::from_millis(jitter_ms)
}

/// Sub-client for client registration and management.
/// Obtained via [`AkribesClient::clients()`].
#[derive(Clone, Debug)]
pub struct RegisteredClientsClient {
    pub(crate) inner: Arc<Inner>,
    pub(crate) project_id: i64,
}

impl RegisteredClientsClient {
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

    fn script_url(&self, script_name: &str) -> String {
        format!(
            "{}/scripts/{}",
            self.project_url(),
            urlencoding::encode(script_name)
        )
    }

    /// Register this client with the server and start a background heartbeat.
    /// Returns the server's response with bound version info per interest.
    pub async fn init(&self, interests: Vec<ClientInterest>) -> Result<RegisterClientResponse> {
        let url = format!("{}/clients", self.project_url());
        let response: RegisterClientResponse = self
            .c()
            .post(
                &url,
                &RegisterRequest {
                    id: self.inner.id.clone(),
                    name: self.inner.name.clone(),
                    interests,
                },
            )
            .await?;

        // Populate contract state from response
        {
            let mut schemas = self.inner.schema_cache.lock().unwrap();
            schemas.clear();
            for interest in &response.interests {
                schemas.insert(interest.script_name.clone(), interest.input_schema.clone());
            }
        }
        self.inner.broken_scripts.lock().unwrap().clear();

        // Start background heartbeat.
        let base_url = self.inner.base_url.clone();
        let client_id = self.inner.id.clone();
        let http = self.inner.http.clone();
        let token = self.inner.token.clone();
        let shutdown = Arc::clone(&self.inner.shutdown);
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut consecutive_failures: u32 = 0;
            loop {
                interval.tick().await;
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                let mut req =
                    http.post(format!("{}/heartbeat", base_url))
                        .json(&HeartbeatRequest {
                            client_id: client_id.clone(),
                        });
                if let Some(ref t) = *token.read().await {
                    req = req.bearer_auth(t);
                }
                let failed = match req.send().await {
                    Ok(res) if res.status().is_success() => {
                        consecutive_failures = 0;
                        false
                    }
                    Ok(res) => {
                        tracing::warn!(status = res.status().as_u16(), "heartbeat rejected");
                        true
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "heartbeat failed");
                        true
                    }
                };
                if failed {
                    consecutive_failures += 1;
                    let backoff = heartbeat_backoff(consecutive_failures);
                    if !backoff.is_zero() {
                        tokio::time::sleep(backoff).await;
                    }
                }
            }
        })
        .abort_handle();

        if let Ok(mut h) = self.inner.heartbeat_handle.lock() {
            if let Some(old) = h.take() {
                old.abort();
            }
            *h = Some(handle);
        }
        Ok(response)
    }

    /// Stop the background heartbeat.
    pub fn destroy(&self) {
        self.inner.shutdown.store(true, Ordering::Release);
        if let Ok(mut h) = self.inner.heartbeat_handle.lock() {
            if let Some(handle) = h.take() {
                handle.abort();
            }
        }
    }

    pub async fn list(&self) -> Result<Vec<ClientInfo>> {
        let url = format!("{}/clients", self.project_url());
        self.c().get_list(&url).await
    }

    pub async fn delete(&self, client_id: &str) -> Result<()> {
        let url = format!("{}/clients/{}", self.inner.base_url, client_id);
        self.c().delete(&url).await?;
        Ok(())
    }

    // ── Lock management ─────────────────────────────────────────────────

    pub async fn list_locks(&self, script_name: &str) -> Result<Vec<ContractLockInfo>> {
        let url = format!("{}/locks", self.script_url(script_name));
        self.c().get_list(&url).await
    }

    pub async fn revoke_lock(&self, script_name: &str, lock_id: i64) -> Result<()> {
        let url = format!("{}/locks/{}", self.script_url(script_name), lock_id);
        self.c().delete(&url).await?;
        Ok(())
    }

    pub async fn rebind_lock(
        &self,
        script_name: &str,
        lock_id: i64,
        version_id: Option<i64>,
    ) -> Result<ContractLockInfo> {
        let url = format!("{}/locks/{}/rebind", self.script_url(script_name), lock_id);
        self.c()
            .patch(&url, &RebindLockRequest { version_id })
            .await
    }

    // ── Flat cross-project lock helpers ─────────────────────────────────
    //
    // The methods above operate on `self.project_id`. The flat helpers
    // below take an explicit `project_id` so callers that hold a single
    // top-level `AkribesClient` (e.g. an admin tool spanning multiple
    // projects) can manage locks without re-entering project scope. This
    // mirrors the parity matrix in PR #342 and the equivalent flat ops
    // on [`crate::sub::projects::ProjectsClient`].

    fn flat_script_url(&self, project_id: i64, script_name: &str) -> String {
        format!(
            "{}/projects/{}/scripts/{}",
            self.inner.base_url,
            project_id,
            urlencoding::encode(script_name)
        )
    }

    /// List contract locks for `script_name` in `project_id`. Cross-project
    /// variant of [`Self::list_locks`]; the implicit project_id on this
    /// client is ignored.
    pub async fn list_locks_for(
        &self,
        project_id: i64,
        script_name: &str,
    ) -> Result<Vec<ContractLockInfo>> {
        let url = format!("{}/locks", self.flat_script_url(project_id, script_name));
        self.c().get_list(&url).await
    }

    /// Delete (revoke) a single lock in `project_id`. Cross-project variant
    /// of [`Self::revoke_lock`].
    pub async fn delete_lock(
        &self,
        project_id: i64,
        script_name: &str,
        lock_id: i64,
    ) -> Result<()> {
        let url = format!(
            "{}/locks/{}",
            self.flat_script_url(project_id, script_name),
            lock_id
        );
        self.c().delete(&url).await?;
        Ok(())
    }

    /// Update (rebind) a single lock to a new version in `project_id`.
    /// Cross-project variant of [`Self::rebind_lock`]. Pass `version_id =
    /// None` to rebind the lock to the channel's current version.
    pub async fn update_lock(
        &self,
        project_id: i64,
        script_name: &str,
        lock_id: i64,
        version_id: Option<i64>,
    ) -> Result<ContractLockInfo> {
        let url = format!(
            "{}/locks/{}/rebind",
            self.flat_script_url(project_id, script_name),
            lock_id
        );
        self.c()
            .patch(&url, &RebindLockRequest { version_id })
            .await
    }
}
