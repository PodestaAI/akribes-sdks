# akribes-sdk

Async Rust client for the [Akribes](https://akribes.ai) workflow server.

Akribes is a domain-specific language and execution platform for AI workflows — multi-agent, multi-step processes with type-checked inputs, structured outputs, and a real-time event stream. This crate is the typed client SDK; you author `.akr` workflows and the server runs them.

## Install

```toml
[dependencies]
akribes-sdk = "0.21"
tokio = { version = "1", features = ["full"] }
```

Or with `cargo add`:

```bash
cargo add akribes-sdk
cargo add tokio --features full
```

## Quickstart

```rust,no_run
use akribes_sdk::AkribesClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = AkribesClient::builder("https://akribes.example.com")
        .token(std::env::var("AKRIBES_TOKEN")?)
        .build();

    // Run a workflow and await its result.
    let project = client.project(2);
    let output = project
        .script("summarize")
        .run_and_await([("brief", "explain quantum computing")])
        .await?;

    println!("{}: {:?}", output.execution_id, output.result);
    Ok(())
}
```

## Streaming events

Workflows emit a typed `EngineEvent` stream — task starts/ends, agent token chunks, validation failures, tool calls, MCP server lifecycle, suspensions, checkpoints. Subscribe with `run_stream`:

```rust,no_run
use akribes_sdk::{AkribesClient, EngineEvent};
use futures::StreamExt;

# async fn ex(client: AkribesClient) -> akribes_sdk::Result<()> {
let mut stream = client.project(2)
    .script("summarize")
    .run_stream([("brief", "explain quantum")])
    .await?;

while let Some(event) = stream.next().await {
    match event? {
        EngineEvent::AgentOutput { chunk, .. } => print!("{chunk}"),
        EngineEvent::TaskEnd { name, .. }      => println!("\n[{name} done]"),
        EngineEvent::Error { message, .. }     => eprintln!("error: {message}"),
        _ => {}
    }
}
# Ok(())
# }
```

## Document ingest

```rust,no_run
# use akribes_sdk::AkribesClient;
# async fn ex(client: AkribesClient) -> akribes_sdk::Result<()> {
let result = client.project(2)
    .documents()
    .ingest_path("invoice.pdf")
    .and_await()
    .await?;
println!("ingested as {}", result.document_id);
# Ok(())
# }
```

## Error handling

The SDK surfaces typed errors via [`AkribesError`]:

- `Auth` — 401 / 403
- `NotFound` — 404
- `RateLimit` — 429 (respects `Retry-After`)
- `Transient` — 502 / 503 / 504 (retried by the SDK's default policy)
- `Script` — workflow failed; carries `error_kind` + `execution_id`
- `Timeout` — request deadline exceeded
- `Other` — anything else (with the wire payload attached)

## Authentication

Two token shapes are accepted:

1. **Service tokens** — long-lived, configured server-side as
   `AKRIBES_SERVICE_TOKEN_<NAME>=<scope>:<secret>`. Suitable for trusted
   backends.
2. **Scoped tokens** — `akribes_tk_...` (legacy `aura_tk_...` still
   accepted) minted by a service token via `client.tokens().mint(...)`.
   Suitable for browsers, CLIs, and read-only shares.

### Prefer `Authorization: Bearer` over `?token=…`

The Rust SDK always ships the bearer token in the `Authorization`
header on every HTTP call and on its WebSocket upgrade — the only
recommended path for non-browser callers. The server's `?token=…`
query-string fallback exists exclusively for browser `EventSource`
/ `WebSocket` clients that cannot set arbitrary headers. Avoid the
query form from CLIs, agents, and backends: reverse-proxy access
logs, CI job traces, and `Referer` headers all routinely capture
query strings and would leak the token. The server stamps
`X-Token-Source: query-param` on responses to query-string requests
so operators can chart adoption away from the query fallback.

See <https://akribes.ai/deployment/authentication/> for the full model.

## Crate layout

| Crate         | Role                                                    |
|---------------|---------------------------------------------------------|
| `akribes-sdk` | The async HTTP client. Most users want this.            |
| `akribes-types` | Wire-level types (events, values, errors). Re-exported by the SDK; use it directly if you're building an alternate transport or telemetry consumer. |

## Links

- SDK guide: <https://akribes.ai/sdks/rust/>
- Language guide: <https://akribes.ai/language/overview/>
- Source mirror: <https://github.com/PodestaAI/akribes-sdks>

## License

MIT. See [`LICENSE`](./LICENSE).
