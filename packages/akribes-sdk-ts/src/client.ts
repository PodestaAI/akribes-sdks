import { HttpClient, type TracePropagator } from './http';
import { ProjectsClient } from './sub/projects';
import { ScriptsClient } from './sub/scripts';
import { VersionsClient } from './sub/versions';
import { ChannelsClient } from './sub/channels';
import { ExecutionsClient } from './sub/executions';
import {
  DEFAULT_INGEST_POLL_TIMEOUT_MS,
  DocumentsClient,
  ingestPollTimeoutMsFromEnv,
} from './sub/documents';
import { ClientsClient, type HeartbeatStatus } from './sub/clients';
import { TokensClient } from './sub/tokens';
import { EventsClient } from './sub/events';
import { EvalsClient } from './sub/evals';
import { BenchClient } from './sub/bench';
import { McpClient } from './sub/mcp';
import { AkribesError } from './errors';
import { connectSse } from './sse';
import type { ConvertResult, HubEvent, EngineEvent } from './types';

export type AkribesClientOptions = {
  baseUrl: string;
  /**
   * Bearer token sent on every request. There are two valid kinds:
   *
   * 1. **Service token** — the secret part of `AKRIBES_SERVICE_TOKEN_<NAME>=<scope>:<secret>`
   *    from the server's env. Long-lived, never expires, full Admin within
   *    its project scope. Use this from a trusted backend (e.g. puto's
   *    server-side code).
   * 2. **Scoped token** — `akribes_tk_...` (legacy `aura_tk_...` still
   *    accepted) minted at runtime via
   *    `akribes.tokens.mint()`. Short-lived, revokable, has a `user_email`
   *    attached. Use this in browsers, CLIs, or any context where you don't
   *    want to expose a long-lived secret.
   *
   * Update this at runtime via `setToken()` after refreshing.
   */
  token?: string;
  projectId?: number;
  name?: string;
  id?: string;
  /**
   * Email sent as `X-Akribes-User` header for metrics attribution. Only
   * honored for service tokens — when a backend with a service token makes
   * calls on behalf of an end user, set this so usage shows up against that
   * user. (Servers also accept the legacy `X-Aura-User` form for backwards
   * compat with pre-rebrand clients, but new code should not rely on that.)
   *
   * **This header does not grant any permissions.** Authorization is purely
   * based on the bearer token's scope.
   */
  onBehalfOf?: string;
  /**
   * Optional W3C trace-context propagator. When set, the SDK invokes it on
   * every outbound request so the carrier is populated with `traceparent`
   * (and optionally `tracestate`) headers. The SDK has zero OpenTelemetry
   * runtime dependencies; callers pass in an `@opentelemetry/api`-based
   * injector from their own setup:
   *
   * ```ts
   * import { propagation, context } from '@opentelemetry/api';
   * new AkribesClient({
   *   ...,
   *   propagator: (carrier) => propagation.inject(context.active(), carrier),
   * });
   * ```
   *
   * Leaving this unset is fine — the server doesn't require a traceparent.
   */
  propagator?: TracePropagator;
  /**
   * Default poll budget for `documents.ingest()` when the per-call
   * `pollTimeoutMs` is omitted. Resolution order at construction:
   *
   *   1. This option, if set.
   *   2. `process.env.AKRIBES_SDK_INGEST_TIMEOUT_SECS` × 1000 (Node/Bun only;
   *      ignored in browsers and when zero/unparseable).
   *   3. `DEFAULT_INGEST_POLL_TIMEOUT_MS` (1 200 000 ms = 20 min).
   *
   * Per-call `IngestOptions.pollTimeoutMs` always wins over this default.
   */
  ingestPollTimeoutMs?: number;
  /**
   * Invoked when the background heartbeat transitions between states (`ok`,
   * `unreachable`, `auth_failed`). Fired once per transition — not once per
   * 30s tick — so a UI can prompt for re-auth on `auth_failed` without an
   * endless flood of warnings. After receiving `auth_failed`, the heartbeat
   * stops ticking; call {@link setToken} with a fresh token and then
   * {@link AkribesClient.clients}`.resumeHeartbeat()` to revive it.
   *
   * Resolves #1220 (TS heartbeat silently logged 401/403 forever) and
   * #1182 (heartbeat backoff parity across SDKs).
   */
  onHeartbeatStatus?: (status: HeartbeatStatus) => void;
};

/**
 * Akribes client.
 *
 * **Auth quickstart**:
 *
 * ```ts
 * // Backend → talk to akribes-server with your service token
 * const akribes = new AkribesClient({
 *   baseUrl: 'https://akribes.example.com',
 *   token: process.env.AKRIBES_SERVICE_TOKEN!,         // <scope>:<secret>'s secret part
 *   onBehalfOf: 'customer@acme.com',                // optional, for metrics
 *   projectId: 2,
 * });
 *
 * // Browser → use a scoped token your backend minted via akribes.tokens.mint()
 * const akribes = new AkribesClient({
 *   baseUrl: 'https://akribes.example.com',
 *   token: 'akribes_tk_xxxx',                       // expires, revokable
 *   projectId: 2,
 * });
 * ```
 *
 * See {@link TokensClient} for minting/listing/revoking scoped tokens.
 */
export class AkribesClient {
  private http: HttpClient;
  private token: string | undefined;
  private _projectId: number | undefined;

  private _projects: ProjectsClient;
  private _scripts?: ScriptsClient;
  private _versions?: VersionsClient;
  private _channels?: ChannelsClient;
  private _executions?: ExecutionsClient;
  private _documents?: DocumentsClient;
  private _clients?: ClientsClient;
  private _tokens: TokensClient;
  private _events: EventsClient;
  private _evals?: EvalsClient;
  private _bench?: BenchClient;
  private _mcp?: McpClient;
  private _state: StateClient;
  private _adHocDisposers: Set<() => void> = new Set();

  constructor(private options: AkribesClientOptions) {
    this.token = options.token;
    this._projectId = options.projectId;
    const baseUrl = options.baseUrl.replace(/\/$/, '');
    this.http = new HttpClient(baseUrl, () => this.token, options.onBehalfOf, options.propagator);
    this._projects = new ProjectsClient(this.http);
    this._state = new StateClient(this.http);
    this._tokens = new TokensClient(this.http);
    // Hub events live above the project scope — a client without a
    // `projectId` still subscribes to the global stream (used by Studio's
    // top-level editor, which surfaces events across the user's projects).
    this._events = new EventsClient(this.http, options.projectId, () => this.token);

    if (options.projectId != null) {
      this.initProjectScoped(options.projectId);
    }
  }

  private initProjectScoped(projectId: number) {
    this._scripts = new ScriptsClient(this.http, projectId);
    this._versions = new VersionsClient(this.http, projectId, this.options.name);
    this._channels = new ChannelsClient(this.http, projectId);
    this._clients = new ClientsClient(
      this.http,
      projectId,
      this.options.id,
      this.options.name,
      { onHeartbeatStatus: this.options.onHeartbeatStatus },
    );
    // Wire the project-scoped contract state into the already-constructed
    // events client so `onScriptSchemaChange` flips the broken flag for
    // `validateContract`. Safe to mutate post-init — `_events` only reads
    // `contractState` from inside subscription callbacks fired later.
    this._events.setContractState(this._clients.contractState);
    this._executions = new ExecutionsClient(
      this.http,
      projectId,
      this.options.name,
      this._clients.contractState,
      () => this._events,
    );
    this._documents = new DocumentsClient(
      this.http,
      projectId,
      this.options.ingestPollTimeoutMs
        ?? ingestPollTimeoutMsFromEnv()
        ?? DEFAULT_INGEST_POLL_TIMEOUT_MS,
    );
    this._evals = new EvalsClient(this.http, projectId);
    this._bench = new BenchClient(this.http, projectId);
    this._mcp = new McpClient(this.http, projectId);
  }

  private requireProjectScoped<T>(client: T | undefined, name: string): T {
    if (!client) throw new AkribesError(`projectId is required for ${name} operations. Pass projectId to AkribesClient constructor.`);
    return client;
  }

  get projects(): ProjectsClient { return this._projects; }
  get scripts(): ScriptsClient { return this.requireProjectScoped(this._scripts, 'scripts'); }
  get versions(): VersionsClient { return this.requireProjectScoped(this._versions, 'versions'); }
  get channels(): ChannelsClient { return this.requireProjectScoped(this._channels, 'channels'); }
  get executions(): ExecutionsClient { return this.requireProjectScoped(this._executions, 'executions'); }
  get documents(): DocumentsClient { return this.requireProjectScoped(this._documents, 'documents'); }
  get clients(): ClientsClient { return this.requireProjectScoped(this._clients, 'clients'); }
  get tokens(): TokensClient { return this._tokens; }
  /** Hub event subscriptions. Always available — non-project-scoped clients
   *  receive the global stream (filtered to what the token can see). */
  get events(): EventsClient { return this._events; }
  get evals(): EvalsClient { return this.requireProjectScoped(this._evals, 'evals'); }
  get bench(): BenchClient { return this.requireProjectScoped(this._bench, 'bench'); }
  get mcp(): McpClient { return this.requireProjectScoped(this._mcp, 'mcp'); }
  get state(): StateClient { return this._state; }

  /**
   * @deprecated Prefer `client.documents.ingest(filename, bytes, opts)`, which
   * hash-first-dedups against the server's blob cache and supports progress
   * callbacks. The legacy `/convert` endpoint remains functional for now.
   *
   * Convert a document file to Markdown via Docling.
   *
   * When the client is constructed with a `projectId`, the persisted document
   * is owned by that project and the returned `document_id` can be passed back
   * as a document input on subsequent runs to skip re-upload + reconversion.
   * Without a `projectId`, the document (if S3-persisted) has no project owner
   * and can only be accessed by service tokens. */
  async convert(
    file: Blob | File,
    opts?: { signal?: AbortSignal },
  ): Promise<ConvertResult> {
    const form = new FormData();
    form.append('file', file);
    const path = this._projectId != null
      ? `/projects/${this._projectId}/convert`
      : '/convert';
    return (await this.http.fetchOk(`${this.http.getBaseUrl()}${path}`, { method: 'POST', body: form, signal: opts?.signal })).json();
  }

  /** Fetch the caller's per-user sandbox project id (creates one on the
   * server if missing). Use this to subscribe to ad-hoc events *before*
   * calling `runAdHoc()` so the first engine events aren't missed. */
  async getSandboxProjectId(opts?: { signal?: AbortSignal }): Promise<number> {
    const res = await this.http.fetchOk(`${this.http.getBaseUrl()}/me/sandbox`, opts);
    const body = await res.json() as { project_id: number };
    return body.project_id;
  }

  /** Execute raw .akr source ad-hoc. Server runs it in the caller's
   * per-user sandbox project and returns the execution_id + project_id.
   *
   * `channel` and `triggeredBy` (#1120) match the Python SDK's `run_adhoc`
   * parameters: `channel` selects the published-version channel to resolve
   * `use foo` references against (default: server's default channel);
   * `triggeredBy` is an opaque identifier recorded with the execution. */
  async runAdHoc(
    source: string,
    opts?: {
      inputs?: Record<string, unknown>;
      breakpointLines?: number[];
      channel?: string;
      triggeredBy?: string;
      signal?: AbortSignal;
    },
  ): Promise<{ execution_id: string; project_id: number }> {
    const body: Record<string, unknown> = { source };
    if (opts?.inputs !== undefined) body.inputs = opts.inputs;
    if (opts?.breakpointLines !== undefined) body.breakpoint_lines = opts.breakpointLines;
    if (opts?.channel !== undefined) body.channel = opts.channel;
    if (opts?.triggeredBy !== undefined) body.triggered_by = opts.triggeredBy;
    return (await this.http.fetchOk(`${this.http.getBaseUrl()}/execute`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
      signal: opts?.signal,
    })).json();
  }

  /** Stream events from ad-hoc executions in the given sandbox project.
   *
   * Pass the `project_id` returned from `runAdHoc()` (or
   * `getSandboxProjectId()`). Returns an unsubscribe function.
   *
   * **Avoiding the subscribe-after-POST race.** A fast workflow
   * (single-digit-millisecond mock providers) can emit `NodeStart`,
   * `TaskStart`, … before a naive `runAdHoc().then(onAdHocExecution)` has
   * the SSE subscriber attached on the server side, dropping those opening
   * events on the broadcast channel. To eliminate the race, pass an
   * `opts.onReady` callback — it fires once the SSE `GET /events` response
   * is open (status 2xx) — and only POST `/execute` after it resolves:
   *
   * ```ts
   * const projectId = await client.getSandboxProjectId();
   * const ready = Promise.withResolvers<void>();
   * const unsubscribe = client.onAdHocExecution(
   *   projectId,
   *   (ev) => { ... },
   *   { onReady: () => ready.resolve() },
   * );
   * await ready.promise;             // SSE attached, safe to POST
   * await client.runAdHoc(source);
   * ```
   *
   * `onReady` is invoked at most once per call (transparent reconnects do
   * not re-fire it). If the connection never establishes — bad token, wrong
   * URL, server down — `onReady` is never called; pair the `await` with a
   * timeout so a misconfigured callsite fails fast instead of hanging.
   */
  onAdHocExecution(
    projectId: number,
    callback: (event: EngineEvent) => void,
    opts?: { onReady?: () => void },
  ): () => void {
    // Token rides on the Authorization header (see connectSse below) so
    // service-token secrets don't end up in reverse-proxy access logs or
    // OTel `http.url` span attributes. sse.ts skips EventSource when
    // an Authorization header is set and uses the fetch-fallback path.
    const buildUrl = () => {
      const url = new URL(`${this.http.getBaseUrl()}/events`);
      url.searchParams.set('project_id', String(projectId));
      url.searchParams.set('script_name', 'adhoc');
      return url.toString();
    };

    let readyFired = false;
    const fireReady = () => {
      if (readyFired) return;
      readyFired = true;
      try { opts?.onReady?.(); } catch { /* swallow caller errors */ }
    };

    const dispose = connectSse({
      url: buildUrl,
      headers: { ...this.http.authHeaders(), ...this.http.traceHeaders() },
      onOpen: fireReady,
      onMessage: (msg) => {
        // Belt-and-braces: EventSource doesn't expose an onopen on every
        // platform, but the first inbound message necessarily means the
        // GET response is open. Fire ready here too so callers always
        // receive it before the first `callback(...)` invocation.
        fireReady();
        if (msg.event !== 'batch' && msg.event !== '') return;
        try {
          const batch: HubEvent[] = JSON.parse(msg.data);
          for (const evt of batch) {
            if (evt.type === 'Execution') callback(evt.payload.event);
          }
        } catch { /* malformed JSON, skip */ }
      },
    });
    this._adHocDisposers.add(dispose);
    return () => {
      dispose();
      this._adHocDisposers.delete(dispose);
    };
  }

  /** Update the auth token at runtime (e.g. after refresh). */
  setToken(token: string | undefined) {
    this.token = token;
  }

  /** Update the X-Akribes-User header for metrics attribution. */
  setOnBehalfOf(email: string | undefined) {
    this.http.setOnBehalfOf(email);
  }

  /** Clean up heartbeat and SSE connections. */
  destroy() {
    this._clients?.destroy();
    this._events.destroy();
    for (const dispose of this._adHocDisposers) dispose();
    this._adHocDisposers.clear();
  }
}

class StateClient {
  constructor(private http: HttpClient) {}

  async get(opts?: { signal?: AbortSignal }): Promise<{ env: Record<string, string> }> {
    return (await this.http.fetchOk(`${this.http.getBaseUrl()}/state`, opts)).json() as Promise<{ env: Record<string, string> }>;
  }
}

export { StateClient };
export type { HeartbeatStatus } from './sub/clients';
