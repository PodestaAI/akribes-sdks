//! Content-addressable document ingest sub-client.
//!
//! See the companion spec in akribes's `docs/superpowers/specs/`.

use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::client::{AkribesClient, Inner};
use crate::error::{AkribesError, Result};
use crate::models::*;

/// Sub-client for `POST /projects/{pid}/documents{,/claim}`. Obtained via
/// [`crate::client::ProjectScope::documents`].
#[derive(Clone, Debug)]
pub struct DocumentsClient {
    inner: Arc<Inner>,
    project_id: i64,
}

impl DocumentsClient {
    pub(crate) fn new(inner: Arc<Inner>, project_id: i64) -> Self {
        Self { inner, project_id }
    }

    fn c(&self) -> AkribesClient {
        AkribesClient {
            inner: Arc::clone(&self.inner),
        }
    }

    fn base_url(&self) -> String {
        format!(
            "{}/projects/{}/documents",
            self.inner.base_url, self.project_id
        )
    }

    /// Snapshot the server-side conversion progress for a content hash (#1151).
    /// Returns `None` if no in-flight conversion is registered (terminal
    /// already, or never uploaded). Cheap — a few-byte JSON response off an
    /// in-memory map. Mirrors TS `documents.progress` and Python
    /// `documents.progress`.
    pub async fn progress(&self, content_hash: &str) -> Result<Option<IngestProgress>> {
        let url = format!(
            "{}/by-hash/{}/progress",
            self.base_url(),
            urlencoding::encode(content_hash),
        );
        let res = self.c().send(self.c().inner.http.get(&url)).await?;
        let wire: ProgressResponseWire = crate::client::decode_json(res).await?;
        Ok(match wire {
            ProgressResponseWire::Converting {
                done_pages,
                total_pages,
            } => Some(IngestProgress {
                done: done_pages,
                total: total_pages,
            }),
            ProgressResponseWire::Idle => None,
        })
    }

    /// Check whether the server has these bytes cached (by content_hash).
    /// On hit, the server creates-or-finds a per-project ref and the SDK
    /// returns [`ClaimOutcome::Hit`] with the `doc_id` and current conversion
    /// status. On miss, the caller must [`upload`](Self::upload) the bytes.
    ///
    /// The `content_hash` returned in [`UploadResult`] comes from the server's
    /// response, not the caller's argument — so a caller that passes a wrong
    /// hash gets the server's authoritative value back.
    pub async fn claim(&self, content_hash: &str, filename: &str) -> Result<ClaimOutcome> {
        let url = format!("{}/claim", self.base_url());
        let wire: ClaimResponseWire = self
            .c()
            .post(
                &url,
                &ClaimRequest {
                    content_hash,
                    filename,
                },
            )
            .await?;
        Ok(match wire {
            ClaimResponseWire::Hit {
                document_id,
                filename,
                content_hash,
                conversion_status,
            } => ClaimOutcome::Hit(UploadResult {
                document_id,
                filename,
                content_hash,
                conversion_status,
            }),
            ClaimResponseWire::Miss => ClaimOutcome::Miss,
        })
    }

    /// Upload bytes. Server hashes, dedups against the `blobs` table, creates
    /// a per-project ref, returns a `doc_<uuid>` scoped to this project.
    pub async fn upload(&self, filename: &str, bytes: Vec<u8>) -> Result<UploadResult> {
        let url = self.base_url();
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename.to_string())
            .mime_str("application/octet-stream")
            .expect("valid MIME string");
        let form = reqwest::multipart::Form::new().part("file", part);
        self.c().post_multipart::<UploadResult>(&url, form).await
    }

    /// Convenience: hash locally, call [`claim`](Self::claim), fall back to
    /// [`upload`](Self::upload) on miss. On hit where the blob is still
    /// `Converting`, polls the claim endpoint until the status is terminal or
    /// the configured ingest poll timeout elapses (default 300 s, see
    /// [`crate::AkribesClientBuilder::ingest_poll_timeout`]). Returns
    /// `AkribesError::Transient` on timeout so the caller can retry. If the
    /// server reports `Failed` (after its own inline auto-reconvert has given
    /// up), returns `AkribesError::Other`.
    pub async fn ingest(&self, filename: &str, bytes: Vec<u8>) -> Result<UploadResult> {
        let content_hash = hex::encode(Sha256::digest(&bytes));
        let poll_timeout = self.inner.ingest_poll_timeout;

        let result = match self.claim(&content_hash, filename).await? {
            ClaimOutcome::Hit(mut r) => {
                // If still converting, poll until terminal.
                let deadline = std::time::Instant::now() + poll_timeout;
                let mut backoff = std::time::Duration::from_millis(250);
                while matches!(
                    r.conversion_status,
                    ConversionStatus::Converting | ConversionStatus::Pending
                ) {
                    if std::time::Instant::now() >= deadline {
                        return Err(AkribesError::Transient {
                            message: format!(
                                "document {} still converting after {}s",
                                r.document_id,
                                poll_timeout.as_secs(),
                            ),
                            execution_id: None,
                            retry_after: None,
                            status: None,
                        });
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = std::cmp::min(backoff * 2, std::time::Duration::from_secs(2));
                    match self.claim(&content_hash, filename).await? {
                        ClaimOutcome::Hit(new_r) => r = new_r,
                        ClaimOutcome::Miss => {
                            // Blob vanished mid-poll (likely GC or reconvert
                            // claimed it). Fall through to upload to repopulate.
                            return self.upload(filename, bytes).await;
                        }
                    }
                }
                r
            }
            ClaimOutcome::Miss => self.upload(filename, bytes).await?,
        };

        if result.conversion_status == ConversionStatus::Failed {
            return Err(AkribesError::Other(format!(
                "document {} conversion failed on the server — re-upload or call reconvert",
                result.document_id
            )));
        }
        Ok(result)
    }
}
