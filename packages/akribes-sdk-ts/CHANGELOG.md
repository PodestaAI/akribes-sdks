# Changelog

All notable changes to `akribes` (TypeScript SDK) will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the package follows [Semantic Versioning](https://semver.org/).

## [0.21.16] — 2026-05-30

First public release of the TypeScript SDK.

### Added
- Pure-ESM `AkribesClient`, browser-safe (only the global `fetch` and
  `EventSource`).
- Resource sub-clients: `projects`, `scripts`, `versions`, `executions`,
  `events`, `documents`, `tokens`, `evals`, `mcp`, `drafts`, `clients`.
- Typed streaming via `createRunStream` with discriminated-union events
  and a `subscribe-before-POST` flow that avoids missed events on the
  start edge.
- Document ingest with hash-deduped server-side storage.
- Long-lived event subscription with heartbeat and `onHeartbeatStatus`
  callback for browser re-auth.
- Typed errors (`AkribesError`, `AkribesAuthError`, `AkribesNotFoundError`,
  `AkribesRateLimitError`, `AkribesTransientError`, `AkribesScriptError`,
  `AkribesTimeoutError`).
- Full `.d.ts` types ship; the package is `"type": "module"` only.

### Server compatibility

Targets akribes-server v0.21.16. Accepts both the new `akribes_tk_…`
scoped-token prefix and the legacy `aura_tk_…` prefix.
