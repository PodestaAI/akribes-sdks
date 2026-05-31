# Changelog

All notable changes to `akribes` (Python SDK) will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the package follows [Semantic Versioning](https://semver.org/).

## [0.21.16] — 2026-05-30

First public release of the Python SDK.

### Added
- Async `AkribesClient` with `httpx` + `pydantic` v2 throughout.
- Resource sub-clients: `projects`, `scripts`, `versions`, `executions`,
  `events`, `documents`, `tokens`, `evals`, `mcp`, `drafts`, `clients`.
- Typed streaming `run_stream` with `match`-friendly event union
  (`agent_chunk`, `task_start`, `task_end`, `error`, `checkpoint`, ...).
- Document ingest (`documents.ingest_and_wait`, plus a progress-yielding
  handle for long uploads).
- Long-lived subscriptions (`events.subscribe`) with heartbeat.
- Typed errors (`AkribesError`, `AuthError`, `NotFoundError`,
  `TransientError`, `RateLimitError`, `ScriptError`, `AkribesTimeoutError`).
- Optional OpenTelemetry W3C trace propagation (`extras = "otel"`).
- Codegen entry-point: `akribes types pull --lang python` generates
  `ScriptType[I, O]` stubs from a running server.

### Server compatibility

Targets akribes-server v0.21.16. The SDK accepts both the new
`akribes_tk_…` scoped-token prefix and the legacy `aura_tk_…` prefix.
