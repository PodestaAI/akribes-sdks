//! Upload a document and run a workflow against it.
//!
//! `documents.ingest` is hash-deduped server-side: re-uploading the same
//! file on a retry returns the same `document_id` without re-converting.
//!
//! Build with:
//!
//! ```bash
//! cargo run --example document_upload -p akribes-sdk -- ./contract.pdf
//! ```
//!
//! Configuration:
//!
//! - `AKRIBES_BASE_URL` — server URL (default `http://localhost:3001`)
//! - `AKRIBES_PROJECT_ID` — project ID (default `1`)
//! - `AKRIBES_TOKEN` — service or scoped token (optional)
//! - `AKRIBES_SCRIPT_NAME` — script to drive (default `extract_clauses`)

use std::env;
use std::path::Path;

use akribes_sdk::{AkribesClient, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let path = args.get(1).map(|s| s.as_str()).unwrap_or_else(|| {
        eprintln!("Usage: cargo run --example document_upload -p akribes-sdk -- <file>");
        std::process::exit(2);
    });

    let base_url = env::var("AKRIBES_BASE_URL").unwrap_or_else(|_| "http://localhost:3001".into());
    let project_id: i64 = env::var("AKRIBES_PROJECT_ID")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let script_name = env::var("AKRIBES_SCRIPT_NAME").unwrap_or_else(|_| "extract_clauses".into());
    let token = env::var("AKRIBES_TOKEN").ok();

    let filename = Path::new(path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("doc")
        .to_string();
    let bytes = std::fs::read(path)
        .map_err(|e| akribes_sdk::AkribesError::Other(format!("read {path}: {e}")))?;

    let mut builder = AkribesClient::builder(&base_url)
        .project_id(project_id)
        .name("rust-sdk-document-upload");
    if let Some(t) = token {
        builder = builder.token(t);
    }
    let client = builder.build();

    // 1. Ingest the document — server hashes, dedups, and converts.
    let ingest = client
        .project(project_id)
        .documents()
        .ingest(&filename, bytes)
        .await?;
    println!("[document_upload] ingested as {}", ingest.document_id);

    // 2. Run the workflow with the document reference.
    let (execution_id, output) = client
        .project(project_id)
        .executions()
        .run(&script_name)
        .document("doc", &ingest.document_id)
        .execute_and_await(None)
        .await?;
    println!("[document_upload] execution_id = {execution_id}");
    println!("[document_upload] status       = {}", output.status);
    println!("[document_upload] result       = {:?}", output.result);
    Ok(())
}
