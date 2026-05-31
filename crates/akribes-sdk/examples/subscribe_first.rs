//! Subscribe-before-POST race avoidance with Notify.
//!
//! A fast workflow can emit `WorkflowStart`, `NodeStart`, `TaskStart`, etc.
//! before a naive `run_adhoc().then(stream)` has the SSE subscriber
//! attached on the server side. The opening events are then dropped on
//! the broadcast channel.
//!
//! The fix is to subscribe *first*, wait for the SSE GET to return 2xx
//! (i.e. the server has attached the subscriber), and only THEN POST
//! `/execute`. This example wires that with `tokio::sync::Notify` as
//! the ready signal — mirroring the `ready: asyncio.Event` pattern in
//! the Python SDK and the `onReady` callback in the TS SDK.
//!
//! Build with:
//!
//! ```bash
//! cargo run --example subscribe_first -p akribes-sdk
//! ```
//!
//! Configuration:
//!
//! - `AKRIBES_BASE_URL` — server URL (default `http://localhost:3001`)
//! - `AKRIBES_TOKEN` — service or scoped token (optional)

use std::env;
use std::sync::Arc;

use akribes_sdk::{AkribesClient, Result};
use tokio::sync::Notify;

#[tokio::main]
async fn main() -> Result<()> {
    let base_url = env::var("AKRIBES_BASE_URL").unwrap_or_else(|_| "http://localhost:3001".into());
    let token = env::var("AKRIBES_TOKEN").ok();

    let mut builder = AkribesClient::builder(&base_url).name("rust-sdk-subscribe-first");
    if let Some(t) = token {
        builder = builder.token(t);
    }
    let client = builder.build();

    // 1. Resolve the per-user sandbox project for ad-hoc execution.
    let project_id = client.get_sandbox_project_id().await?;
    println!("[subscribe_first] sandbox project_id = {project_id}");

    // 2. Subscribe FIRST, with a Notify as the ready signal.
    let ready = Arc::new(Notify::new());
    let (mut rx, sub) = client
        .adhoc_event_stream_with_ready(project_id, Some(Arc::clone(&ready)))
        .await?;

    // 3. Wait for the server to acknowledge the subscription.
    ready.notified().await;
    println!("[subscribe_first] SSE subscriber attached, safe to POST");

    // 4. NOW it's safe to POST /execute — no opening events can precede us.
    let source = r#"
input
  greeting: string

workflow
  return greeting
"#;
    let mut inputs = std::collections::HashMap::new();
    inputs.insert("greeting".to_string(), serde_json::json!("hi from rust"));
    let result = client.run_adhoc(source, Some(inputs), None).await?;
    println!(
        "[subscribe_first] dispatched execution {}",
        result.execution_id
    );

    // 5. Drain the event stream until the workflow ends.
    while let Some(hub_event) = rx.recv().await {
        // The full hub frame is yielded; filter to the execution we just
        // started. (Other concurrent runs on the same sandbox would also
        // flow through this channel.)
        println!("[subscribe_first] hub event: {hub_event:?}");
    }
    drop(sub);
    Ok(())
}
