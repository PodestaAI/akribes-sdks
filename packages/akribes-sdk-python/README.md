# akribes

Pythonic async client for the [Akribes](https://akribes.ai) workflow server.

Requires Python 3.10+.

## Install

```bash
pip install akribes
# or with optional OpenTelemetry support:
pip install 'akribes[otel]'
```

## Quick start

```python
import asyncio
from akribes_sdk import AkribesClient

async def main():
    async with AkribesClient("https://akribes.example.com", token="akribes_tk_...") as client:
        proj = client.project(2)
        output = await proj.script("summarize").run_and_await(brief="explain quantum")
        print(output.execution_id, output.result)

asyncio.run(main())
```

## Construction

```python
import os
from datetime import timedelta
from akribes_sdk import AkribesClient, RetryPolicy

client = AkribesClient(
    "https://akribes.example.com",
    token=os.environ["AKRIBES_TOKEN"],
    timeout=timedelta(seconds=30),
    retry=RetryPolicy(max_attempts=4),      # retries transients + 429 by default
    otel=True,                               # opt-in OTel auto-instrumentation
)
```

## Project handles

```python
proj = client.project(2)                              # sync, lazy
proj = await client.get_project("podesta-staging")    # async, resolves name → id
sandbox = await client.sandbox()                       # per-user sandbox project
```

## Typed inputs (codegen)

Generate typed `ScriptType[I, O]` stubs from a live server:

```bash
akribes types pull --project podesta --lang python --out src/akribes_types/
```

Then:

```python
from akribes_types.podesta import summarize   # ScriptType[I, O] with typed input/output

out = await proj.run(summarize, brief="hi", tone="formal")
# IDE knows brief: str, tone: Literal["formal","casual"]
print(out.execution_id, out.result)
```

## Streaming

```python
run = await proj.executions.run_stream("summarize", brief="hi")
async for evt in run:
    match evt.kind:
        case "agent_chunk": print(evt.chunk, end="")
        case "task_end":    print(f"\n[task {evt.task!r} done]")
        case "error":       print(f"\n[error {evt.message}]")
output = await run.output()
```

## Document ingest

```python
from pathlib import Path

# One-liner
result = await proj.documents.ingest_and_wait(Path("invoice.pdf"))

# With progress
handle = proj.documents.ingest(Path("invoice.pdf"))
async for evt in handle:
    print(evt)
result = await handle.result()
```

## Subscribe (long-lived events)

```python
async with proj.events.subscribe(interests=[{"script_name": "summarize"}]) as sub:
    async for evt in sub:
        print(evt)
```

Heartbeat runs for the lifetime of the subscription only — not automatically on client construction.

## Pagination

```python
async for script in proj.scripts.list():
    print(script.name)

# Or materialise
scripts = await proj.scripts.list().to_list()
first   = await proj.scripts.list().first()
top10   = await proj.scripts.list().take(10)
```

## Error handling

```python
from akribes_sdk import (
    AkribesError, AuthError, NotFoundError, TransientError,
    RateLimitError, ScriptError, AkribesTimeoutError,
)

try:
    output = await proj.run_and_await("summarize", brief="hi")
except AuthError:        # 401/403
    ...
except NotFoundError:    # 404
    ...
except TransientError:   # 502/503 (retried by default)
    ...
except RateLimitError:   # 429 (retried by default, respects Retry-After)
    ...
except ScriptError as e: # workflow failed; e.error_kind, e.execution_id
    ...
```

## Tokens

```python
from datetime import timedelta
minted = await client.tokens.mint(
    scopes={"projects": "*", "role": "admin"},
    expires_in=timedelta(hours=8),
    label="web-session",
    user_email="alice@acme.com",
)
print(minted.token)   # ship to the browser
```

## Authentication

`AkribesClient` accepts either a **service token** (long-lived, set via
`AKRIBES_SERVICE_TOKEN_<NAME>=<scope>:<secret>` on the server) or a **scoped
token** (`akribes_tk_...` — legacy `aura_tk_...` still accepted — minted
via `client.tokens.mint(...)`).

### Prefer `Authorization: Bearer` over `?token=…` (#789)

`AkribesClient` always sends the token in the `Authorization` header
for HTTP requests and WebSocket upgrades. The `?token=…` query-string
form exists only because browser `EventSource` / `WebSocket`
constructors cannot set arbitrary headers — treat it as a browser-only
escape hatch.

For CLIs, scripts, and backend services, avoid `?token=…` because:

- Reverse proxies, ingress controllers, and CDNs log the full URL
  (including the token) in access logs by default.
- CI runners (Forgejo Actions, GitHub Actions) echo `curl` commands
  into job logs.
- Browsers leak `?token=` in the `Referer` header on cross-origin
  sub-resource requests.

The server stamps `X-Token-Source: query-param` on responses to any
request that used the query fallback so operators can chart adoption.

---

See `examples/` for runnable end-to-end demos.

Upgrading from v0.20.x? See [MIGRATION-0.21.md](./MIGRATION-0.21.md).

- SDK guide: <https://akribes.ai/sdks/python/>
- Language guide: <https://akribes.ai/language/overview/>
- Source mirror: <https://github.com/PodestaAI/akribes-sdks>
- Issues: <https://github.com/PodestaAI/akribes-sdks/issues>

## License

MIT
