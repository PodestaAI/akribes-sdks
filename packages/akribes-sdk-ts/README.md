# akribes

TypeScript / Bun client SDK for the [Akribes](https://akribes.ai) workflow server.

Akribes is a domain-specific language and execution platform for AI workflows — multi-agent, multi-step processes with type-checked inputs, structured outputs, and a real-time event stream. This package is the typed client; you author `.akr` workflows and the server runs them.

Browser-safe: zero Node-only deps, only the global `fetch` and `EventSource`.

## Install

```bash
npm install akribes
# or
bun add akribes
```

## Quickstart

```ts
import { AkribesClient } from "akribes";

const client = new AkribesClient({
  baseUrl: "https://akribes.example.com",
  projectId: 2,
  token: process.env.AKRIBES_SERVICE_TOKEN!,
});

// List projects
const projects = await client.projects.list();
for (const p of projects) console.log(`${p.id}: ${p.name}`);

// Run a workflow and await the result
const { execution_id } = await client.executions.run("my_script");
const result = await client.executions.awaitExecution(execution_id);
console.log(result);
```

## Authentication

`AkribesClient` accepts either a **service token** (long-lived, from
`AKRIBES_SERVICE_TOKEN_<NAME>=<scope>:<secret>`) or a **scoped token**
(`akribes_tk_...` — legacy `aura_tk_...` still accepted — minted via
`client.tokens.mint(...)`).

- **Backend / CLI**: pass the service-token secret directly.
- **Browser**: never ship a service token. Mint a short-lived scoped
  token server-side and hand it to the browser:

```ts
// Server side, with a service token.
const minted = await backendClient.tokens.mint({
  user_email: "alice@acme.com",
  scopes: { projects: 2, role: "editor" },
  expires_in: 8 * 3600,
  label: "web-session",
});
// `minted.token` is the `akribes_tk_…` string to ship to the browser.

// Browser side.
const client = new AkribesClient({
  baseUrl: "https://akribes.example.com",
  projectId: 2,
  token: mintedTokenFromBackend,
  onHeartbeatStatus: (status) => {
    // Prompt for re-auth when the token is revoked / expires.
    if (status === "auth_failed") promptRelogin();
  },
});
```

See the [auth docs](https://akribes.ai/deployment/authentication/) for the full two-tier token model.

### Prefer `Authorization: Bearer` over `?token=…` (#789)

The SDK ships the bearer token in the `Authorization` header on every
HTTP call and on its WebSocket upgrade — the recommended path for any
non-browser caller. The `?token=…` query-string fallback exists only
because browser `EventSource` / `WebSocket` constructors cannot set
arbitrary headers; treat it as a browser-only escape hatch.

Reasons to avoid query-string tokens for CLIs, backends, and scripts:

- Reverse proxies (nginx, Ingress, CDN) log the full URL, including
  the token, in access logs by default.
- Browsers leak `?token=` in the `Referer` header on cross-origin
  sub-resource requests originating from the same page.
- CI runners (Forgejo / GitHub Actions) echo `curl` command-lines into
  job logs.
- HTTP error responses and OTel spans sometimes capture `url.full`.

The server stamps `X-Token-Source: query-param` on responses to any
request that used the query fallback, so operators can chart adoption
away from the query form without having to log the token.

## Streaming execution

`runStream` yields typed events as the workflow executes:

```ts
import { createRunStream } from "akribes";

const run = await createRunStream({
  scriptName: "summarize",
  inputs: { brief: "Distill the attached doc into 3 bullets." },
  starter: client.executions,
  events: client.events,
});

run.on.output((chunk) => process.stdout.write(chunk.chunk));
run.on.error((e) => console.error("Workflow failed:", e.message));

const result = await run.output();
console.log("Final result:", result);
```

## Document ingest

Upload a document and run a workflow against it without re-uploading on
each retry — `documents.ingest` is hash-deduped server-side:

```ts
import { readFile } from "node:fs/promises";

const data = await readFile("./contract.pdf");
const { documentId } = await client.documents.ingest("contract.pdf", data);

const { execution_id } = await client.executions.run("extract_clauses", {
  inputs: { doc: documentId },
});
```

## Browser usage

Scoped tokens only — never expose a service token client-side. Rotate
via `setToken()` after re-auth, and call `clients.resumeHeartbeat()` if
you wired `onHeartbeatStatus` and paused on `auth_failed`:

```ts
client.setToken(newToken);
client.clients.resumeHeartbeat();
```

## Examples

See [`examples/`](./examples) for runnable scripts:

- `quick_start.ts` — minimal projects/run/await flow.
- `run_stream.ts` — streaming with the subscribe-before-POST race avoidance.
- `document_upload.ts` — `client.documents.ingest` + run.
- `with_scoped_token.ts` — mint + browser-style usage.
- `with_otel.ts` — OpenTelemetry W3C trace propagation.

## Documentation

- SDK guide: <https://akribes.ai/sdks/typescript/>
- Language guide: <https://akribes.ai/language/overview/>
- Source mirror: <https://github.com/PodestaAI/akribes-sdks>

## License

MIT. See [`LICENSE`](./LICENSE).
