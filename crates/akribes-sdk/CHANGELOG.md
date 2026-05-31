# Changelog

All notable changes to `akribes-sdk` (Rust SDK) will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows [Semantic Versioning](https://semver.org/).

## [0.21.16] — 2026-05-30

First public release of the Rust SDK on crates.io.

### Added
- Async `AkribesClient` built on `reqwest` + `tokio`.
- Resource sub-clients: `projects`, `scripts`, `versions`, `executions`,
  `events`, `documents`, `tokens`, `evals`, `mcp`, `drafts`, `clients`,
  `bench`.
- Typed `EngineEvent` stream via `run_stream` — `AgentOutput`,
  `TaskStart`/`End`, `Suspended`/`Resumed`, `Error`, `ToolCall*`,
  `McpServerDegraded`/`Recovered`, and 20+ more variants.
- Typed errors via `AkribesError`: `Auth`, `NotFound`, `RateLimit`
  (with `Retry-After`), `Transient`, `Script` (with `error_kind` +
  `execution_id`), `Timeout`, `Other`.
- Hash-deduped document ingest (`documents().ingest_path(...).and_await()`).
- Long-lived event subscription with heartbeat.

### Changed
- The crate is renamed from `akribes-sdk-rust` (internal, git-only) to
  `akribes-sdk` (published on crates.io). The library name is
  `akribes_sdk`.
- Wire-level types now live in the new `akribes-types` crate. They are
  re-exported from `akribes-sdk` for convenience; callers building
  alternative transports can depend on `akribes-types` directly.

### Server compatibility

Targets akribes-server v0.21.16. Accepts both the new `akribes_tk_…`
scoped-token prefix and the legacy `aura_tk_…` prefix.
