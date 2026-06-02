/**
 * Sub-client for the akribes-server bench substrate.
 *
 * Mirrors `crates/akribes-sdk/src/sub/bench.rs`. Two surfaces glued onto
 * one TypeScript class for ergonomic Studio consumption:
 *
 *  - Project-scoped operations live under
 *    `/projects/{id}/scripts/{name}/bench/...`. The `projectId` is captured
 *    at construction (`new AkribesClient({ projectId })`) and the script
 *    name is the first argument to each method.
 *  - Run-scoped + case-id-keyed operations live under `/bench-runs/{id}/...`
 *    and `/cases/{id}` and aren't tied to a project (the server resolves
 *    the owning project from the row). These are exposed on the same class
 *    so studios don't juggle two client instances.
 *
 * Typed errors:
 *  - 404 with a `{"error": ...}` body → `AkribesNotFoundError`.
 *  - 400 `{"error": "case_type_mismatch", "field_errors": [...]}` →
 *    `CaseTypeMismatchError`.
 *  - 400 with a `Judge contract mismatch: ...` prefix → `JudgeContractError`
 *    (with the trailing `breaks` list parsed when present).
 *  - 409 with `error_type: "..._already_exists"` → `AkribesAlreadyExistsError`
 *    (handled by the shared `fetchOk`).
 *  - All other 4xx/5xx → `AkribesHttpError`.
 */

import type { HttpClient } from '../http';
import { connectSse } from '../sse';
import {
  AkribesHttpError,
  AkribesNotFoundError,
  CaseTypeMismatchError,
  JudgeContractError,
  type CaseFieldError,
} from '../errors';
import type {
  Bench,
  BenchById,
  BenchCase,
  BenchResult,
  BenchRun,
  BenchRunTagSessionResponse,
  CompareReport,
  ContractPreview,
  CreateBenchCaseRequest,
  CreateOrUpdateBenchRequest,
  DriftReport,
  ExecutionStatus,
  McpSessionCost,
  PatchBenchCaseRequest,
  ProjectBenchSummary,
  PromoteExecutionRequest,
  ScriptSignature,
  TriggerBenchRunRequest,
} from '../types';

/** Translate a server response into the typed bench-specific error
 *  taxonomy. Returns the error to throw, or `null` to let the caller
 *  fall through to the default `fetchOk` handling.
 *
 *  Decoding rules — same shape as the Rust SDK's `error::AkribesError`
 *  classification:
 *   - 404 + `{ "error": "..." }` body → `AkribesNotFoundError`. We attach
 *     the body string so consumers can recover the original message.
 *   - 400 + `{ "error": "case_type_mismatch", "field_errors": [...] }`
 *     → `CaseTypeMismatchError`.
 *   - 400 with a `"Judge contract mismatch: ..."` message → `JudgeContractError`.
 *     The server emits the breaks as a `; `-separated trailer; we split
 *     on `; ` after the `"N field(s) incompatible: "` prefix when present
 *     and pass the empty array otherwise (it's still useful to typed-catch
 *     this case for routing).
 */
async function classifyBenchError(res: Response): Promise<Error | null> {
  if (res.ok) return null;
  const body = await res.text();
  let serverMessage: string | undefined;
  let fieldErrors: CaseFieldError[] | undefined;
  try {
    const json: unknown = JSON.parse(body);
    if (json && typeof json === 'object') {
      const obj = json as Record<string, unknown>;
      if (typeof obj.error === 'string') serverMessage = obj.error;
      if (Array.isArray(obj.field_errors)) {
        fieldErrors = obj.field_errors.filter(
          (e): e is CaseFieldError =>
            !!e && typeof e === 'object'
            && typeof (e as { path?: unknown }).path === 'string'
            && typeof (e as { message?: unknown }).message === 'string',
        );
      }
    }
  } catch {
    /* body wasn't JSON; serverMessage stays undefined */
  }

  if (res.status === 404) {
    return new AkribesNotFoundError(body, serverMessage);
  }
  if (res.status === 400) {
    if (serverMessage === 'case_type_mismatch' && fieldErrors) {
      return new CaseTypeMismatchError(body, serverMessage, fieldErrors);
    }
    if (serverMessage && serverMessage.startsWith('Judge contract mismatch')) {
      return new JudgeContractError(body, serverMessage, parseJudgeBreaks(serverMessage));
    }
  }
  return new AkribesHttpError(res.status, body, serverMessage ?? (body || res.statusText));
}

/** Extract the trailing `field(s) incompatible: ...` list from a
 *  "Judge contract mismatch" message. Returns an empty array when the
 *  format doesn't match — the typed error is still useful as a routing
 *  signal even without the list. */
function parseJudgeBreaks(message: string): string[] {
  const marker = 'field(s) incompatible: ';
  const idx = message.indexOf(marker);
  if (idx === -1) return [];
  const tail = message.slice(idx + marker.length).trim();
  if (!tail) return [];
  return tail.split('; ').map((s) => s.trim()).filter(Boolean);
}

/** Run the request through the bench-specific error classifier. On 2xx
 *  returns the response; otherwise throws the appropriate typed error. */
async function benchFetch(
  http: HttpClient,
  url: string,
  init?: RequestInit & { signal?: AbortSignal },
): Promise<Response> {
  const res = await http.authFetch(url, init);
  const err = await classifyBenchError(res);
  if (err) throw err;
  return res;
}

/** Shorthand for `benchFetch(...).then(r => r.json() as Promise<T>)`. */
async function benchFetchJson<T>(
  http: HttpClient,
  url: string,
  init?: RequestInit & { signal?: AbortSignal },
): Promise<T> {
  const res = await benchFetch(http, url, init);
  return res.json() as Promise<T>;
}

/** Append `?k=v&...` query params, skipping `undefined` / `null` entries. */
function withQuery(url: string, params: Record<string, string | number | undefined | null>): string {
  const qs = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v === undefined || v === null) continue;
    qs.set(k, String(v));
  }
  const tail = qs.toString();
  return tail ? `${url}?${tail}` : url;
}

export class BenchClient {
  constructor(
    private http: HttpClient,
    private projectId: number,
  ) {}

  // ── URL helpers ─────────────────────────────────────────────────────────

  private benchPath(scriptName: string, ...segments: string[]): string {
    return this.http.scriptPath(this.projectId, scriptName, 'bench', ...segments);
  }

  private scriptOnlyPath(scriptName: string, ...segments: string[]): string {
    return this.http.scriptPath(this.projectId, scriptName, ...segments);
  }

  private runUrl(runId: number, ...segments: string[]): string {
    const tail = segments.length ? `/${segments.map(encodeURIComponent).join('/')}` : '';
    return `${this.http.getBaseUrl()}/bench-runs/${runId}${tail}`;
  }

  private caseUrl(caseId: string): string {
    return `${this.http.getBaseUrl()}/cases/${encodeURIComponent(caseId)}`;
  }

  // ── Bench config CRUD ───────────────────────────────────────────────────

  /** `GET /projects/{id}/scripts/{name}/bench` — 404 → `null`. Other 4xx/5xx
   *  surface via {@link AkribesHttpError}. */
  async get(scriptName: string, opts?: { signal?: AbortSignal }): Promise<Bench | null> {
    try {
      return await benchFetchJson<Bench>(this.http, this.benchPath(scriptName), opts);
    } catch (e) {
      if (e instanceof AkribesNotFoundError) return null;
      throw e;
    }
  }

  /** `POST /projects/{id}/scripts/{name}/bench` — create or update. The
   *  server upserts on `(script_id)`, so this is idempotent w.r.t. an
   *  existing bench config. */
  async save(
    scriptName: string,
    req: CreateOrUpdateBenchRequest,
    opts?: { signal?: AbortSignal },
  ): Promise<Bench> {
    return benchFetchJson<Bench>(this.http, this.benchPath(scriptName), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(req),
      signal: opts?.signal,
    });
  }

  /** `DELETE /projects/{id}/scripts/{name}/bench`. The server emits a
   *  `{"deleted": true}` body either way; we discard it. */
  async delete(scriptName: string, opts?: { signal?: AbortSignal }): Promise<void> {
    await benchFetch(this.http, this.benchPath(scriptName), {
      method: 'DELETE',
      signal: opts?.signal,
    });
  }

  /** `GET /projects/{id}/benches` — one row per script with a bench
   *  configured, joined with the latest run's summary. Backs the project-
   *  level evals landing page. */
  async listProjectSummaries(opts?: { signal?: AbortSignal }): Promise<ProjectBenchSummary[]> {
    return benchFetchJson<ProjectBenchSummary[]>(
      this.http,
      `${this.http.getBaseUrl()}/projects/${this.projectId}/benches`,
      opts,
    );
  }

  // ── Signature + contract preview ────────────────────────────────────────

  /** `GET /projects/{id}/scripts/{name}/signature` — parsed script inputs +
   *  outputs + named type defs. Used by the case-builder modal. */
  async getSignature(
    scriptName: string,
    opts?: { signal?: AbortSignal },
  ): Promise<ScriptSignature> {
    return benchFetchJson<ScriptSignature>(this.http, this.scriptOnlyPath(scriptName, 'signature'), opts);
  }

  /** `GET /projects/{id}/scripts/{name}/bench/contract-preview` — workflow
   *  + judge signature pair plus the structured `breaks` list. Used by the
   *  judge-picker UI to surface incompatibilities before a save. */
  async contractPreview(
    scriptName: string,
    args: { judgeScriptId: number; channel?: string },
    opts?: { signal?: AbortSignal },
  ): Promise<ContractPreview> {
    const url = withQuery(this.benchPath(scriptName, 'contract-preview'), {
      judge: args.judgeScriptId,
      channel: args.channel,
    });
    return benchFetchJson<ContractPreview>(this.http, url, opts);
  }

  // ── Cases ───────────────────────────────────────────────────────────────

  /** `GET /projects/{id}/scripts/{name}/bench/cases`. 404 (no bench
   *  configured) → empty list. */
  async listCases(
    scriptName: string,
    opts?: { signal?: AbortSignal },
  ): Promise<BenchCase[]> {
    try {
      return await benchFetchJson<BenchCase[]>(this.http, this.benchPath(scriptName, 'cases'), opts);
    } catch (e) {
      if (e instanceof AkribesNotFoundError) return [];
      throw e;
    }
  }

  /** `POST /projects/{id}/scripts/{name}/bench/cases` — form-builder create.
   *  Throws {@link CaseTypeMismatchError} on a 400 `case_type_mismatch`
   *  envelope so form layers can surface per-field violations. */
  async createCase(
    scriptName: string,
    req: CreateBenchCaseRequest,
    opts?: { signal?: AbortSignal },
  ): Promise<BenchCase> {
    return benchFetchJson<BenchCase>(this.http, this.benchPath(scriptName, 'cases'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(req),
      signal: opts?.signal,
    });
  }

  /** `GET /projects/{id}/scripts/{name}/bench/cases/contract-drift`. Returns
   *  an empty drift report when the endpoint 404s (script never published). */
  async caseContractDrift(
    scriptName: string,
    opts?: { signal?: AbortSignal },
  ): Promise<DriftReport> {
    try {
      return await benchFetchJson<DriftReport>(this.http, this.benchPath(scriptName, 'cases', 'contract-drift'), opts);
    } catch (e) {
      if (e instanceof AkribesNotFoundError) {
        return {
          drifted: [],
          script_version_id: null,
          published_at: null,
          published_by: null,
          summary: '',
        };
      }
      throw e;
    }
  }

  /** `GET /executions/{caseId}` — fetch the raw case execution row. Cases are
   *  `executions` rows with `kind='case'`, so this hits the same handler as
   *  {@link ExecutionsClient.get}; it lives here as the bench-surface
   *  counterpart that resolves a case id to its frozen execution. 404 → `null`
   *  (mirrors the Rust SDK's `get_case`, which returns `Value::Null` for an
   *  absent row). The shape is the standard {@link ExecutionStatus} projection;
   *  legacy promoted-execution rows may carry a null `kind`. */
  async getCase(caseId: string, opts?: { signal?: AbortSignal }): Promise<ExecutionStatus | null> {
    const url = `${this.http.getBaseUrl()}/executions/${encodeURIComponent(caseId)}`;
    try {
      return await benchFetchJson<ExecutionStatus>(this.http, url, opts);
    } catch (e) {
      if (e instanceof AkribesNotFoundError) return null;
      throw e;
    }
  }

  /** `PATCH /cases/{id}` — sparse update. */
  async patchCase(
    caseId: string,
    req: PatchBenchCaseRequest,
    opts?: { signal?: AbortSignal },
  ): Promise<BenchCase> {
    return benchFetchJson<BenchCase>(this.http, this.caseUrl(caseId), {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(req),
      signal: opts?.signal,
    });
  }

  /** `DELETE /cases/{id}`. The server emits `{"deleted": true}`; we discard
   *  it for an idiomatic `void` return. */
  async deleteCase(caseId: string, opts?: { signal?: AbortSignal }): Promise<void> {
    await benchFetch(this.http, this.caseUrl(caseId), {
      method: 'DELETE',
      signal: opts?.signal,
    });
  }

  /** `GET /benches/{id}` — look up a bench by numeric id without going
   *  through `(project, script)` routing. The server joins in the owning
   *  `project_id` + `script_name` so a caller can chain into list_cases /
   *  list_runs without an N+1 project walk. Not tied to this client's
   *  `projectId` (the server resolves the project from the row). 404 →
   *  `null` (mirrors the Rust SDK's `bench_by_id`). */
  async getBenchById(benchId: number, opts?: { signal?: AbortSignal }): Promise<BenchById | null> {
    const url = `${this.http.getBaseUrl()}/benches/${benchId}`;
    try {
      return await benchFetchJson<BenchById>(this.http, url, opts);
    } catch (e) {
      if (e instanceof AkribesNotFoundError) return null;
      throw e;
    }
  }

  /** `GET /mcp-sessions/{id}/cost` — aggregated cost for one MCP session,
   *  read from the same `mcp_session_cost` table the bench coordinator's
   *  finalize step writes to. Returns `{session_id, total_cost_usd,
   *  breakdown}`. Service-token only and narrowed to the session's owning
   *  project server-side; a scoped (non-service) token is rejected with 403.
   *  A session with no associated bench run is a 404, surfaced as
   *  {@link AkribesNotFoundError} (the Rust SDK's `mcp_session_cost` keeps
   *  the 404 visible too rather than coercing it to a zero-cost row). */
  async getMcpSessionCost(sessionId: string, opts?: { signal?: AbortSignal }): Promise<McpSessionCost> {
    const url = `${this.http.getBaseUrl()}/mcp-sessions/${encodeURIComponent(sessionId)}/cost`;
    return benchFetchJson<McpSessionCost>(this.http, url, opts);
  }

  /** `POST /executions/{exec_id}/promote-to-case` — promote a completed
   *  execution into a bench case, with an optional `edits` overlay. Lives
   *  on `/executions` rather than `/bench-runs` but is the natural
   *  counterpart to the case-builder flow, so it lives on this client. */
  async promoteExecution(
    executionId: string,
    req: PromoteExecutionRequest = {},
    opts?: { signal?: AbortSignal },
  ): Promise<BenchCase> {
    const url = `${this.http.getBaseUrl()}/executions/${encodeURIComponent(executionId)}/promote-to-case`;
    return benchFetchJson<BenchCase>(this.http, url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(req),
      signal: opts?.signal,
    });
  }

  // ── Runs (project-scoped surface) ───────────────────────────────────────

  /** `GET /projects/{id}/scripts/{name}/bench/runs` — paginated via
   *  `limit` / `offset`. 404 → empty list. */
  async listRuns(
    scriptName: string,
    params?: { limit?: number; offset?: number },
    opts?: { signal?: AbortSignal },
  ): Promise<BenchRun[]> {
    const url = withQuery(this.benchPath(scriptName, 'runs'), {
      limit: params?.limit,
      offset: params?.offset,
    });
    try {
      return await benchFetchJson<BenchRun[]>(this.http, url, opts);
    } catch (e) {
      if (e instanceof AkribesNotFoundError) return [];
      throw e;
    }
  }

  /** `POST /projects/{id}/scripts/{name}/bench/runs` — trigger a run.
   *  `case_ids` constrains the fan-out to a subset. Throws
   *  {@link JudgeContractError} on the contract pre-flight 400. */
  async triggerRun(
    scriptName: string,
    req: TriggerBenchRunRequest,
    opts?: { signal?: AbortSignal },
  ): Promise<BenchRun> {
    return benchFetchJson<BenchRun>(this.http, this.benchPath(scriptName, 'runs'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(req),
      signal: opts?.signal,
    });
  }

  // ── Runs (run-id keyed, global surface) ─────────────────────────────────

  /** `GET /bench-runs/{id}` — 404 → `null`. */
  async getRun(runId: number, opts?: { signal?: AbortSignal }): Promise<BenchRun | null> {
    try {
      return await benchFetchJson<BenchRun>(this.http, this.runUrl(runId), opts);
    } catch (e) {
      if (e instanceof AkribesNotFoundError) return null;
      throw e;
    }
  }

  /** `DELETE /bench-runs/{id}`. Cancels first (best-effort) before
   *  dropping. The server emits a JSON receipt; we discard it. */
  async deleteRun(runId: number, opts?: { signal?: AbortSignal }): Promise<void> {
    await benchFetch(this.http, this.runUrl(runId), {
      method: 'DELETE',
      signal: opts?.signal,
    });
  }

  /** `GET /bench-runs/{id}/results`. 404 → empty list. */
  async listResults(runId: number, opts?: { signal?: AbortSignal }): Promise<BenchResult[]> {
    try {
      return await benchFetchJson<BenchResult[]>(this.http, this.runUrl(runId, 'results'), opts);
    } catch (e) {
      if (e instanceof AkribesNotFoundError) return [];
      throw e;
    }
  }

  /** `POST /bench-runs/{id}/cancel`. Flips the cancel token; in-flight cases
   *  complete naturally. Returns the run row as it stands. */
  async cancelRun(runId: number, opts?: { signal?: AbortSignal }): Promise<BenchRun> {
    return benchFetchJson<BenchRun>(this.http, this.runUrl(runId, 'cancel'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: '{}',
      signal: opts?.signal,
    });
  }

  /** `GET /bench-runs/{a}/compare/{b}` — diff two runs of the same bench. */
  async compareRuns(
    runA: number,
    runB: number,
    opts?: { signal?: AbortSignal },
  ): Promise<CompareReport> {
    const url = `${this.http.getBaseUrl()}/bench-runs/${runA}/compare/${runB}`;
    return benchFetchJson<CompareReport>(this.http, url, opts);
  }

  /** `PATCH /bench-runs/{id}/tag-session` — attribute the run to an MCP
   *  session id so the coordinator's finalize step writes the cost into
   *  `mcp_session_cost`. */
  async tagSession(
    runId: number,
    mcpSessionId: string,
    opts?: { signal?: AbortSignal },
  ): Promise<BenchRunTagSessionResponse> {
    return benchFetchJson<BenchRunTagSessionResponse>(this.http, this.runUrl(runId, 'tag-session'), {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mcp_session_id: mcpSessionId }),
      signal: opts?.signal,
    });
  }

  // ── Live run-event stream (SSE) ─────────────────────────────────────────

  /**
   * Subscribe to a bench run's live result stream via SSE.
   *
   * The server emits two event types on `/bench-runs/{id}/events`:
   *  - `result` — a new {@link BenchResult} row.
   *  - `lagged` — broadcast-stream lag report (`{ dropped: N }`).
   *
   * Built on the shared {@link connectSse} helper, which uses
   * {@link EventSource} in browsers and a fetch-based reader in Node / Bun.
   * Auth rides on the Authorization header from the SDK's `HttpClient`.
   * Older revisions also appended `opts.token` to the URL as a `?token=`
   * fallback so EventSource (which can't set headers) would authenticate;
   * that leaked long-lived service-token secrets into reverse-proxy
   * access logs. `sse.ts` now skips EventSource whenever an Authorization
   * header is present and uses the fetch-fallback path instead — so the
   * `opts.token` URL fallback is no longer needed. `opts.token` is kept
   * on the API for now (a no-op) to avoid breaking compiled callers.
   * Returns an unsubscribe function.
   */
  subscribeRunEvents(
    runId: number,
    handlers: {
      onResult?: (result: BenchResult) => void;
      onLagged?: (dropped: number) => void;
      onError?: (error: Error) => void;
    },
    opts?: { token?: string; signal?: AbortSignal },
  ): () => void {
    void opts?.token;
    const buildUrl = () => {
      const url = new URL(`${this.http.getBaseUrl()}/bench-runs/${runId}/events`);
      return url.toString();
    };

    return connectSse({
      url: buildUrl,
      headers: { ...this.http.authHeaders(), ...this.http.traceHeaders() },
      signal: opts?.signal,
      onMessage: (msg) => {
        if (msg.event === 'result') {
          try {
            handlers.onResult?.(JSON.parse(msg.data) as BenchResult);
          } catch {
            /* malformed payload, swallow */
          }
        } else if (msg.event === 'lagged') {
          try {
            const parsed = JSON.parse(msg.data) as { dropped: number };
            handlers.onLagged?.(parsed.dropped);
          } catch {
            /* malformed payload, swallow */
          }
        }
      },
      onError: (err) => handlers.onError?.(err),
    });
  }
}
