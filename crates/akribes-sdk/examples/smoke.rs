//! Canonical SDK smoke runner. Reads `AKRIBES_INTEGRATION_SERVER_URL` +
//! `AKRIBES_INTEGRATION_SERVICE_TOKEN`, runs `e2e/fixtures/canonical_smoke.akr`,
//! prints each event as JSON. Used by humans for ad-hoc inspection and by
//! the e2e workflow as `cargo run --example smoke -p akribes-sdk`.
//!
//! Demonstrates the documented subscribe-before-POST pattern: pre-subscribe
//! to the sandbox project's SSE stream, wait for the 2xx ready notify, and
//! only then issue `run_adhoc` so opening events from a fast workflow can't
//! be lost to the broadcast-receiver attach race.

use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use akribes_sdk::AkribesClient;
use tokio::sync::Notify;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let url = std::env::var("AKRIBES_INTEGRATION_SERVER_URL")
        .map_err(|_| "AKRIBES_INTEGRATION_SERVER_URL not set")?;
    let token = std::env::var("AKRIBES_INTEGRATION_SERVICE_TOKEN")
        .map_err(|_| "AKRIBES_INTEGRATION_SERVICE_TOKEN not set")?;

    let mut workflow = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    workflow.push("../../e2e/fixtures/canonical_smoke.akr");
    let source = std::fs::read_to_string(&workflow)
        .map_err(|e| format!("read {}: {e}", workflow.display()))?;

    let client = AkribesClient::builder(&url).token(&token).build();

    let project_id = client.get_sandbox_project_id().await?;
    let ready = Arc::new(Notify::new());
    let (mut rx, _sub) = client
        .adhoc_event_stream_with_ready(project_id, Some(Arc::clone(&ready)))
        .await?;

    tokio::time::timeout(Duration::from_secs(10), ready.notified())
        .await
        .map_err(|_| "SSE subscription did not become ready within 10s")?;

    let run = client.run_adhoc(&source, None, None).await?;
    eprintln!(
        "[smoke] run started: execution_id={} project_id={}",
        run.execution_id, run.project_id
    );

    let mut count = 0usize;
    let mut saw_workflow_end = false;
    while let Some(ev) = rx.recv().await {
        count += 1;
        let line = serde_json::to_string(&ev)?;
        println!("{line}");
        if line.contains("WorkflowEnd") {
            saw_workflow_end = true;
            break;
        }
    }
    eprintln!("[smoke] received {count} events; workflow_end={saw_workflow_end}");
    Ok(())
}
