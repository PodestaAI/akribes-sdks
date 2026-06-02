# Changelog

All notable changes to `akribes-sdk` (Rust SDK) will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- `BenchClient::list_project_summaries` → `GET /projects/{id}/benches`,
  returning one `ProjectBenchSummary` per configured bench (script + judge
  names, case count, and the most-recent run's identity + mean score).
  Closes a parity gap with the TS SDK's `listProjectSummaries`.
- `BenchRunsClient::subscribe_run_events` → live SSE stream of a bench
  run's results at `GET /bench-runs/{id}/events` (`Accept:
  text/event-stream`). Yields typed `BenchRunEvent`s (`Result` / `Lagged`
  / `Terminal`) over an `mpsc` receiver plus a drop-to-cancel
  `EventSubscription`, reusing the crate's shared SSE byte-deframer and
  field parser. Mirrors the TS SDK's `subscribeRunEvents`; the server's
  synthetic `terminal` frame is surfaced as a typed variant so callers can
  detect end-of-stream without a side channel.
- `BenchResult.error` — the failed-case error message (`status` is
  `workflow_failed`/`judge_failed`). Mirrors the server column; previously
  the SDK model dropped it, so it was unreadable on both the
  `/bench-runs/{id}/results` read path and the live SSE `result` frame.
- `ExecutionsClient::tasks` → `GET /executions/{id}/tasks`, returning the
  per-task cost / token / duration breakdown (`ExecutionTasksResponse` with
  a `Vec<ExecutionTaskSummary>`) from the `execution_tasks` table populated
  as `TaskEnd` events arrive. 404 → `Ok(None)`, matching `get` /
  `get_output`. Closes a parity gap with the TS SDK's `executions.tasks`.

### Changed
- `ExecutionsClient::get_document_markdown` now returns an error when the
  server response is missing the `markdown` field or it isn't a string,
  instead of silently returning an empty string. A malformed response is a
  server-contract violation, not an "empty document"; hiding it as `""`
  masked the failure from callers.

### Removed
- `BenchRunsClient::events` (the JSON-poll against `GET /bench-runs/{id}/events`)
  and its `BenchRunEventsPage` type. The server serves that path SSE-only, so
  the poll returned a perpetually-empty page. Use `subscribe_run_events` for
  live events and `list_results` for the durable record.

### Internal
- Added behavioral integration tests (no public API change) covering the
  previously-untested `bench`, `evals`, and `convert` sub-clients, the
  project-scoped contract-lock operations (`list_locks`/`revoke_lock`/
  `rebind_lock`), `tokens.revoke_by_email`, the `PublishBuilder`
  (`execute`/`execute_dry_run`/`execute_version_only`), `resolve`
  (id-or-name) on projects and scripts, error/timeout/network-failure
  classification, and the SSE pipeline end to end (byte deframer,
  `event_stream`, and the full `run_stream` subscribe→POST→terminal path
  including cross-execution event filtering). Each test pins the request
  shape against the corresponding akribes-server route.

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
