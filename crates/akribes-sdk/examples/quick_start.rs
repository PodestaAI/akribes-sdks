//! Quick-start example for the Rust SDK.
//!
//! Mirrors `packages/akribes-sdk-ts/example_client.ts`: connects to a local
//! akribes-server, ensures a `demo_script` exists, registers a client, then
//! either runs the script with the new callback API or just streams events
//! until interrupted. Build with:
//!
//! ```bash
//! cargo run --example quick_start -p akribes-sdk
//! ```
//!
//! Configuration via env vars (all optional):
//!
//! - `AKRIBES_BASE_URL` — server URL (default `http://localhost:3001`)
//! - `AKRIBES_PROJECT_ID` — project ID (default `1`)
//! - `AKRIBES_TOKEN` — service or scoped token; required if your server has
//!   `AKRIBES_SERVICE_TOKEN_*` set, optional otherwise
//! - `AKRIBES_SCRIPT_NAME` — script to drive (default `demo_script`)

use std::env;

use akribes_sdk::{AkribesClient, Result, WorkflowEvent, models::ClientInterest};

#[tokio::main]
async fn main() -> Result<()> {
    let base_url = env::var("AKRIBES_BASE_URL").unwrap_or_else(|_| "http://localhost:3001".into());
    let project_id: i64 = env::var("AKRIBES_PROJECT_ID")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let script_name = env::var("AKRIBES_SCRIPT_NAME").unwrap_or_else(|_| "demo_script".into());
    let token = env::var("AKRIBES_TOKEN").ok();

    println!("[quick_start] connecting to {base_url} (project {project_id})");

    // 1. Build the client. `.token(...)` is optional — omit it on a server
    //    without service tokens configured.
    let mut builder = AkribesClient::builder(&base_url)
        .project_id(project_id)
        .name("rust-sdk-quick-start");
    if let Some(t) = token {
        builder = builder.token(t);
    }
    let client = builder.build();

    // 2. Ensure the demo script exists. Creates it with a tiny "echo" workflow
    //    if missing — running this against a real LLM provider needs a model
    //    block; here we keep it minimal so the example is offline-runnable.
    let scripts = client.project(project_id).scripts().list().await?;
    let exists = scripts.iter().any(|s| s.name == script_name);
    if !exists {
        println!("[quick_start] creating '{script_name}'");
        let source = r#"
input
  message: string

workflow
  return message
"#;
        client
            .project(project_id)
            .scripts()
            .create(&script_name, source)
            .await?;
    }

    // 3. Register the client so the hub knows we're listening. The heartbeat
    //    runs in a background task; `.destroy()` (or dropping `client`) stops
    //    it cleanly.
    client
        .project(project_id)
        .registered_clients()
        .init(vec![ClientInterest {
            script_name: script_name.clone(),
            inputs: [("message".to_string(), "string".to_string())].into(),
            channel: None,
            lifetime: None,
            strict: None,
        }])
        .await?;
    println!("[quick_start] registered client (heartbeat active)");

    // 4. Run the script and stream events. `run_stream(...)` subscribes to
    //    SSE *before* POSTing /run so we can never miss the opening events.
    let executions = client.project(project_id).executions();
    let req = executions
        .run(&script_name)
        .input("message", "hello from rust");
    let mut stream = executions.run_stream(req).await?;

    // 5. Wire up callbacks. The iterator is still the canonical surface —
    //    callbacks are convenience sinks. They run synchronously on the
    //    polling thread, so keep them quick.
    stream.on_output(|chunk| {
        if let Some(s) = chunk.as_str() {
            print!("{s}");
        }
    });
    stream.on_task_end(|p| {
        println!("\n[quick_start] task '{}' done in {:?}", p.task, p.duration);
    });
    stream.on_error(|p| {
        eprintln!("[quick_start] engine error: {} ({:?})", p.message, p.kind);
    });

    // 6. Drive the stream to completion. The iterator yields every event —
    //    callbacks fire alongside.
    while let Some(evt) = stream.next().await {
        match evt {
            Ok(WorkflowEvent::End { output, .. }) => {
                println!("\n[quick_start] workflow finished: {output}");
                break;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("[quick_start] stream error: {e}");
                return Err(e);
            }
        }
    }

    // 7. Clean up. `destroy()` aborts the heartbeat task; without it the
    //    task lives until `client` is dropped (which happens on `main` exit
    //    too, but doing it explicitly is the well-behaved pattern).
    client.project(project_id).registered_clients().destroy();
    Ok(())
}
