use std::sync::Arc;

use crate::client::{AkribesClient, Inner};
use crate::error::Result;
use crate::models::ConvertResult;

/// Sub-client for document conversion via Docling. Obtained via [`AkribesClient::convert()`].
#[derive(Clone, Debug)]
pub struct ConvertClient {
    pub(crate) inner: Arc<Inner>,
}

impl ConvertClient {
    pub(crate) fn new(inner: Arc<Inner>) -> Self {
        Self { inner }
    }

    fn c(&self) -> AkribesClient {
        AkribesClient {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Convert a document file to Markdown via the server's Docling integration.
    ///
    /// The server supports: PDF, DOCX, DOC, PPTX, XLSX, HTML, PNG, JPG, TIFF.
    pub async fn convert_file(&self, filename: &str, data: Vec<u8>) -> Result<ConvertResult> {
        let url = format!("{}/convert", self.inner.base_url);
        self.post_convert(&url, filename, data).await
    }

    /// Project-scoped convert. Prefer this over [`convert_file`](Self::convert_file)
    /// when uploading for a specific project so the server can enforce scope.
    pub async fn convert_file_for_project(
        &self,
        project_id: i64,
        filename: &str,
        data: Vec<u8>,
    ) -> Result<ConvertResult> {
        let url = format!("{}/projects/{}/convert", self.inner.base_url, project_id);
        self.post_convert(&url, filename, data).await
    }

    async fn post_convert(
        &self,
        url: &str,
        filename: &str,
        data: Vec<u8>,
    ) -> Result<ConvertResult> {
        let part = reqwest::multipart::Part::bytes(data)
            .file_name(filename.to_string())
            .mime_str(mime_for(filename))?;
        let form = reqwest::multipart::Form::new().part("file", part);
        self.c().post_multipart(url, form).await
    }
}

fn mime_for(filename: &str) -> &'static str {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "doc" => "application/msword",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "html" | "htm" => "text/html",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "tiff" | "tif" => "image/tiff",
        _ => "application/octet-stream",
    }
}
