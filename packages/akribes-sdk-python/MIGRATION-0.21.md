# Migrating to v0.21

v0.21 is a **hard break**: no compat shims, no deprecation warnings. The Python SDK rewrites
its public surface to be more Pythonic. Below is every breaking change with a before/after.

For non-breaking new features (codegen, OTel, retry policy), see the [README](./README.md).

## Construction

| Before (v0.20) | After (v0.21) |
|---|---|
| `AkribesClient(url, project_id=2, token=tok)` | `AkribesClient(url, token=tok)` — no `project_id` |
| `client.scripts.list()` | `client.project(2).scripts.list()` |

## Project handles

Project-scoped namespaces moved off `AkribesClient` to a `ProjectHandle`:

| Before | After |
|---|---|
| `client.scripts.list()` | `client.project(2).scripts.list()` |
| `client.executions.run("x")` | `client.project(2).executions.run("x")` |
| `client.documents.ingest(...)` | `client.project(2).documents.ingest(...)` |

By-id ops stay on the client:

| Stays on `client` | Lives on `proj` |
|---|---|
| `client.executions.get(execution_id)` | `proj.executions.run("script")` |
| `client.executions.cancel(execution_id)` | `proj.executions.list("script")` |
| `client.evals.get_run(run_id)` | `proj.evals.list_runs("script")` |
| `client.clients.delete(client_id)` | `proj.clients.list()` |

To resolve a project by name (async, validates it exists):

```python
proj = await client.get_project("podesta-staging")
```

## Execution outputs

`run_and_await` returns `ExecutionOutput` directly (not a tuple):

| Before | After |
|---|---|
| `execution_id, output = await client.script("x").run_and_await(inputs={"brief": "hi"})` | `output = await proj.script("x").run_and_await(brief="hi")` |
| (no `execution_id` on `output`) | `output.execution_id` |

## Inputs as keyword arguments

| Before | After |
|---|---|
| `await proj.run("summarize", inputs={"brief": "hi", "tone": "formal"})` | `await proj.run("summarize", brief="hi", tone="formal")` |

The `inputs={...}` dict form still works for callers spreading dynamic dictionaries.

## Unified `run()` — polymorphic on input value types

The separate `run_with_upload` and `run_with_s3` methods are gone. Dispatch is automatic:

| Before | After |
|---|---|
| `await proj.executions.run_with_upload("ocr", files={"doc": ("x.pdf", data)})` | `await proj.executions.run("ocr", doc=Path("x.pdf"))` |
| `await proj.executions.run_with_s3("ocr", inputs={"doc": S3PresignedRef(...)})` | `await proj.executions.run("ocr", doc=S3PresignedRef(...))` |

## `.get()` raise-by-default

`.get()` raises `NotFoundError` on 404 (mirrors `dict[k]`):

| Before | After |
|---|---|
| `result = await client.projects.get(999); if result is None: ...` | `try: await client.projects.get(999) / except NotFoundError: ...` |
| | OR `result = await client.projects.get(999, default=None)` |
| `ok = await client.tokens.revoke("missing")` (returned `bool`) | `await client.tokens.revoke("missing")` — raises `NotFoundError` on miss |
| `ok = await client.executions.cancel("missing")` (returned `bool`) | `await client.executions.cancel("missing")` — raises `NotFoundError` on miss |

## Pagination

`.list()` returns `AsyncPage[T]` — it does not return a plain list:

| Before | After |
|---|---|
| `scripts = await proj.scripts.list()` | `scripts = await proj.scripts.list().to_list()` |
| Manual offset loop | `async for s in proj.scripts.list(): ...` (auto-paginates) |

Other helpers: `.first()`, `.take(n)`.

## `timedelta` for time values

| Before | After |
|---|---|
| `tokens.mint(expires_in=28_800)` | `tokens.mint(expires_in=timedelta(hours=8))` (int seconds still accepted) |
| `AkribesClient(timeout=30.0)` | `AkribesClient(timeout=timedelta(seconds=30))` (float still accepted) |

## Mutable attributes (no setters)

| Before | After |
|---|---|
| `client.set_token(new_token)` | `client.token = new_token` |
| `client.set_on_behalf_of(email)` | `client.on_behalf_of = email` |
| `client.resume_heartbeat()` | Removed — heartbeat is now opt-in via `subscribe()` |
| `await client.init()` | Removed — no background tasks start on construction |

## Heartbeat opt-in via `subscribe()`

The 30s heartbeat loop is no longer auto-started by `__aenter__`. It runs only during an
active `events.subscribe()` context:

```python
async with AkribesClient(url, token=tok) as client:   # no heartbeat starts here
    await client.projects.list().to_list()             # plain REST, no background tasks

    proj = client.project(2)
    async with proj.events.subscribe(interests=[...]) as sub:
        # heartbeat runs for the lifetime of this block only
        async for evt in sub:
            ...
    # subscription closed → heartbeat stops
```

## Documents — `ingest()` returns `IngestHandle`

| Before | After |
|---|---|
| `result = await client.documents.ingest(path, on_phase=cb, on_progress=cb)` | `handle = proj.documents.ingest(path)` |
| | `async for evt in handle: ...` |
| | `result = await handle.result()` |
| | OR one-liner: `result = await proj.documents.ingest_and_wait(path)` |

## Sandbox

The sandbox project is now a regular `ProjectHandle`:

| Before | After |
|---|---|
| `pid = await client.get_sandbox_project_id()` | `sandbox = await client.sandbox()` — returns `ProjectHandle` |
| `await client.run_adhoc(source, inputs={...})` | `await sandbox.run_source(source, **inputs)` |
| `async for evt in client.adhoc_event_stream(pid): ...` | `async with sandbox.events.subscribe(...) as sub: async for evt in sub: ...` |

## Models are stdlib dataclasses

Public models (`Project`, `Script`, `ExecutionOutput`, all `WorkflowEvent` variants, etc.)
are now `@dataclass(frozen=True, slots=True)`. Pydantic is internal-only.

| Before | After |
|---|---|
| `model.model_dump()` | `dataclasses.asdict(model)` |
| `Model.model_validate(raw)` | Use the corresponding `parse_*` helper from `akribes_sdk._parsers` (escape hatch) |

## Error hierarchy cleanup

Back-compat alias names are deleted:

| Before | After |
|---|---|
| `except AkribesFatalError:` | `except AuthError:` |
| `except AkribesNotFoundError:` | `except NotFoundError:` |
| `except AkribesScriptError:` | `except ScriptError:` |
| `except AkribesTransientError:` | `except TransientError:` |

Surviving (non-alias) classes are unchanged: `AkribesError`, `AkribesHTTPError`,
`AkribesConnectionError`, `AkribesTimeoutError`, `AkribesConversionError`,
`AlreadyExistsError`.

## OpenTelemetry — opt-in instead of manual hook

| Before | After |
|---|---|
| `propagator=lambda c: propagate.inject(c)` | `otel=True` — auto-wires HTTP spans + W3C propagation |
| | OR `otel=tracer` — pass a `Tracer` instance |
| | Install with `pip install 'akribes[otel]'` |

The manual `propagator=` hook still works for callers with custom carriers.

## Retry — built-in policy

The SDK retries transient errors and 429s by default. No caller-side retry loops needed:

```python
client = AkribesClient(url, token=tok, retry=RetryPolicy(max_attempts=4))   # default
client = AkribesClient(url, token=tok, retry=RetryPolicy.none())            # disable
```

POSTs that need retry support pass an idempotency key:

```python
await proj.run("ocr", idempotency_key="batch-42-x", doc=Path("x.pdf"))
```

## What did NOT change

- `RunStream` (Layer 3 event handle) — same `async for` / `run.on.output(cb)` / `await run.output()`.
- `WorkflowEvent` variant names + their `kind` discriminator strings (still snake_case).
- Service token format + scoped-token mint flow.
- Server endpoints, URL paths, authentication.

## Known follow-ups for a later release

- Sync facade (`from akribes_sdk.sync import AkribesClient`) for callers
  that can't or don't want to use `asyncio`.

Out of scope for v0.21. File an issue at
<https://github.com/PodestaAI/akribes-sdks/issues> if it blocks you.

## Landed after v0.21 (no migration required)

- WebSocket transport for execution streaming. `RunStream` and
  `events.subscribe()` now prefer `GET /events/ws` and fall back to SSE
  on handshake failure. Public API is unchanged; tune with
  `AKRIBES_TRANSPORT=ws|sse` if you need to pin a transport.
