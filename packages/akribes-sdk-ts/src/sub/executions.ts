import type { HttpClient } from '../http';
import { nullOn404 } from '../http';
import { AkribesTransientError, AkribesFatalError, AkribesScriptError, AkribesTimeoutError, ScriptSchemaChangedError } from '../errors';
import type { ContractState } from './clients';
import type { RunResult, RerunResult, ExecutionStatus, ExecutionOutput, ExecutionEvents, S3DocumentRef, ScriptGraph, ScriptCost, ProjectCost } from '../types';
import { createRunStream, type RunStream, type RunStreamEventsSource, type RunStreamOptions } from '../runStream';

/** Summary of a child execution spawned via the `spawn_child_execution`
 *  callback. Returned by `GET /executions/:id/children`. For v1 the parent
 *  linkage columns are typically NULL; this type is forward-looking. */
export type ExecutionChildSummary = {
  id: string;
  parent_node_id: string | null;
  status: string;
  started_at: string | null;
  finished_at: string | null;
  script_name: string;
};

/** Per-task cost / token breakdown row returned by
 *  `GET /executions/:id/tasks`. Mirrors the shape produced by
 *  `get_execution_tasks` in akribes-server: one row per `execution_tasks`
 *  entry, populated as `TaskEnd` events arrive. */
export type ExecutionTaskSummary = {
  task_name: string;
  model: string | null;
  provider: string | null;
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens: number;
  cache_write_input_tokens: number;
  cost_usd: number | null;
  duration_ms: number | null;
  attempt: number;
  finished_at: string;
};

/** Envelope returned by `GET /executions/:id/tasks`. */
export type ExecutionTasksResponse = {
  execution_id: string;
  tasks: ExecutionTaskSummary[];
};

/** Outcome of a cancel request. Distinguishes "stopped now" from "stop
 *  requested, winding down elsewhere". See {@link ExecutionsClient.cancel}. */
export type CancelResult = {
  /** True when the server signalled an in-process run to stop immediately. */
  cancelled: boolean;
  /** The execution's status after the request: `'cancelling'` when a durable
   *  stop was requested but the owning replica hasn't exited yet, a terminal
   *  status (`'cancelled'`/`'completed'`/`'failed'`) on a post-finish race, or
   *  `undefined` for the script-scoped `cancelRun` shape. */
  status?: string;
};

/** Parse the JSON envelope returned by the cancel endpoints. Tolerates the
 *  `{ cancelled: number }` shape from `cancelRun` (count of stopped runs) and
 *  the `{ cancelled: bool, status }` shape from `cancel`. */
async function parseCancelResult(res: Response): Promise<CancelResult> {
  let body: unknown;
  try {
    body = await res.json();
  } catch {
    // No/empty body (older server) — treat as a best-effort success.
    return { cancelled: true };
  }
  if (!body || typeof body !== 'object') return { cancelled: false };
  const b = body as Record<string, unknown>;
  const cancelledRaw = b.cancelled;
  const cancelled = typeof cancelledRaw === 'number'
    ? cancelledRaw > 0
    : cancelledRaw === true;
  const status = typeof b.status === 'string' ? b.status : undefined;
  return { cancelled, status };
}

/** Filters for the run-history list endpoints. All optional and
 *  AND-combined; `since`/`until` are ISO 8601 timestamps matched against
 *  `started_at`. */
export type ListExecutionsOptions = {
  status?: string;
  channel?: string;
  /** `failure_mode` discriminator (see migration 20260515000000). */
  failureMode?: string;
  /** Inclusive lower bound on `started_at`. */
  since?: string;
  /** Inclusive upper bound on `started_at`. */
  until?: string;
  limit?: number;
  offset?: number;
  signal?: AbortSignal;
};

/** Build the query string shared by the script- and project-level run-history
 *  list endpoints. `scriptName` is only meaningful for the project-level call. */
function buildListExecutionsQuery(
  options?: ListExecutionsOptions & { scriptName?: string },
): string {
  const params = new URLSearchParams();
  if (options?.status) params.set('status', options.status);
  if (options?.channel) params.set('channel', options.channel);
  if (options?.failureMode) params.set('failure_mode', options.failureMode);
  if (options?.since) params.set('since', options.since);
  if (options?.until) params.set('until', options.until);
  if (options?.scriptName) params.set('script_name', options.scriptName);
  if (options?.limit != null) params.set('limit', String(options.limit));
  if (options?.offset != null) params.set('offset', String(options.offset));
  return params.toString();
}

/** Default timeout for {@link ExecutionsClient.await} — 5 minutes. */
const DEFAULT_AWAIT_TIMEOUT_MS = 5 * 60 * 1000;

/** Sleep for `ms`, returning early if `signal` aborts. */
function sleepUntilAborted(ms: number, signal?: AbortSignal): Promise<void> {
  if (signal?.aborted) return Promise.resolve();
  return new Promise<void>((resolve) => {
    const t = setTimeout(() => {
      signal?.removeEventListener('abort', onAbort);
      resolve();
    }, ms);
    const onAbort = () => { clearTimeout(t); resolve(); };
    signal?.addEventListener('abort', onAbort, { once: true });
  });
}

export class ExecutionsClient {
  constructor(
    private http: HttpClient,
    private projectId: number,
    private defaultTriggeredBy: string | undefined,
    private contractState?: ContractState,
    /** Lazy getter for the SSE source. Called on each `runStream()` call so
     *  `AkribesClient` can instantiate both sub-clients without a circular init. */
    private eventsSourceFactory?: () => RunStreamEventsSource,
  ) {}

  private path(scriptName: string, ...segments: string[]) {
    return this.http.scriptPath(this.projectId, scriptName, ...segments);
  }

  /** Pre-dispatch validation: check contract state before sending the request.
   *  Server is authoritative for per-input type checking; the client only
   *  short-circuits on a known schema break. */
  private validateContract(scriptName: string) {
    if (!this.contractState) return;
    if (this.contractState.brokenScripts.has(scriptName)) {
      throw new ScriptSchemaChangedError(scriptName);
    }
  }

  async run(
    scriptName: string,
    opts?: {
      inputs?: Record<string, unknown>;
      channel?: string;
      triggeredBy?: string;
      signal?: AbortSignal;
      breakpointLines?: number[];
      /** When true, MCP tool calls execute in dry-run mode (no side effects).
       *  Server-side enforcement pending — flag passes through today. */
      dryRunTools?: boolean;
    },
  ): Promise<RunResult> {
    this.validateContract(scriptName);
    const channel = opts?.channel ?? 'production';
    const url = `${this.path(scriptName, 'run')}?channel=${encodeURIComponent(channel)}`;
    return this.http.fetchJson<RunResult>(url, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        inputs: opts?.inputs,
        triggered_by: opts?.triggeredBy ?? this.defaultTriggeredBy,
        breakpoint_lines: opts?.breakpointLines,
        dry_run_tools: opts?.dryRunTools,
      }),
      signal: opts?.signal,
    });
  }

  /** Run a script with files uploaded via multipart form data. */
  async runWithUpload(
    scriptName: string,
    files: Record<string, { filename: string; data: Blob | File }>,
    opts?: { channel?: string; triggeredBy?: string; signal?: AbortSignal },
  ): Promise<RunResult> {
    const channel = opts?.channel ?? 'production';
    const url = `${this.path(scriptName, 'run', 'upload')}?channel=${encodeURIComponent(channel)}`;
    const form = new FormData();
    for (const [name, file] of Object.entries(files)) {
      form.append(name, file.data, file.filename);
    }
    if (opts?.triggeredBy ?? this.defaultTriggeredBy) {
      form.append('_meta', JSON.stringify({ triggered_by: opts?.triggeredBy ?? this.defaultTriggeredBy }));
    }
    return this.http.fetchJson<RunResult>(url, { method: 'POST', body: form, signal: opts?.signal });
  }

  /** Run a script with documents referenced from S3. */
  async runWithS3(
    scriptName: string,
    inputs: Record<string, S3DocumentRef>,
    opts?: { channel?: string; triggeredBy?: string; signal?: AbortSignal },
  ): Promise<RunResult> {
    const url = this.path(scriptName, 'run', 's3');
    return this.http.fetchJson<RunResult>(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        inputs,
        channel: opts?.channel ?? 'production',
        triggered_by: opts?.triggeredBy ?? this.defaultTriggeredBy,
      }),
      signal: opts?.signal,
    });
  }

  async get(executionId: string, opts?: { signal?: AbortSignal }): Promise<ExecutionStatus | null> {
    return nullOn404(async () =>
      this.http.fetchJson<ExecutionStatus>(`${this.http.getBaseUrl()}/executions/${encodeURIComponent(executionId)}`, opts)
    );
  }

  async getOutput(executionId: string, opts?: { signal?: AbortSignal }): Promise<ExecutionOutput | null> {
    return nullOn404(async () =>
      this.http.fetchJson<ExecutionOutput>(`${this.http.getBaseUrl()}/executions/${encodeURIComponent(executionId)}/output`, opts)
    );
  }

  async getEvents(executionId: string, opts?: { afterId?: number; limit?: number; signal?: AbortSignal }): Promise<ExecutionEvents | null> {
    const params = new URLSearchParams();
    if (opts?.afterId != null) params.set('after_id', String(opts.afterId));
    if (opts?.limit != null) params.set('limit', String(opts.limit));
    const qs = params.toString();
    const url = `${this.http.getBaseUrl()}/executions/${encodeURIComponent(executionId)}/events${qs ? `?${qs}` : ''}`;
    return nullOn404(async () =>
      this.http.fetchJson<ExecutionEvents>(url, { signal: opts?.signal })
    );
  }

  /** List a script's run history, newest first.
   *
   * Filters (all optional, AND-combined): `status`, `channel`, `failureMode`,
   * and a `since`/`until` `started_at` date range (ISO 8601). The server caps
   * the page (default 50, max 200); pass `limit`/`offset` to paginate. */
  async list(
    scriptName: string,
    options?: ListExecutionsOptions,
  ): Promise<ExecutionStatus[]> {
    const qs = buildListExecutionsQuery(options);
    const url = `${this.path(scriptName, 'executions')}${qs ? '?' + qs : ''}`;
    return this.http.fetchJson<ExecutionStatus[]>(url, { signal: options?.signal });
  }

  /** Project-level run history across every script, newest first.
   *
   * Same filters as {@link list}, plus an optional `scriptName` narrowing.
   * Powers the project-wide "Runs" view where the operator hasn't picked a
   * script yet. */
  async listForProject(
    options?: ListExecutionsOptions & { scriptName?: string },
  ): Promise<ExecutionStatus[]> {
    const qs = buildListExecutionsQuery(options);
    const url = `${this.http.getBaseUrl()}/projects/${this.projectId}/executions${qs ? '?' + qs : ''}`;
    return this.http.fetchJson<ExecutionStatus[]>(url, { signal: options?.signal });
  }

  /** List child executions spawned by this execution via the engine's
   *  spawn_child_execution callback. Returns an empty array when no children
   *  exist (the common case for v1 where parent linkage isn't yet wired). */
  async children(executionId: string, opts?: { signal?: AbortSignal }): Promise<ExecutionChildSummary[]> {
    return this.http.fetchJson<ExecutionChildSummary[]>(
      `${this.http.getBaseUrl()}/executions/${encodeURIComponent(executionId)}/children`,
      opts,
    );
  }

  /** Per-task cost / token / duration breakdown for an execution. Reads from
   *  the `execution_tasks` table populated as `TaskEnd` events arrive. Useful
   *  for monolith workflows where there are no spawned children — every
   *  agent invocation lives in `execution_tasks` keyed by `task_name`. */
  async tasks(executionId: string, opts?: { signal?: AbortSignal }): Promise<ExecutionTasksResponse> {
    return this.http.fetchJson<ExecutionTasksResponse>(
      `${this.http.getBaseUrl()}/executions/${encodeURIComponent(executionId)}/tasks`,
      opts,
    );
  }

  /**
   * Cancel a specific execution by ID.
   *
   * Returns the server's real outcome so callers can be honest about whether
   * the run actually stopped. `cancelled` is `true` only when THIS server held
   * the in-memory cancellation token and signalled it; on a multi-replica
   * deployment the owning replica may be another process, in which case the
   * server records a durable `cancel_requested` flag, returns
   * `{ cancelled: false, status: 'cancelling' }`, and the run winds down on the
   * owner's next claim-loop poll. Callers should poll {@link get} until the row
   * reaches a terminal state rather than claiming instant success.
   */
  async cancel(executionId: string, opts?: { signal?: AbortSignal }): Promise<CancelResult> {
    const res = await this.http.fetchOk(`${this.http.getBaseUrl()}/executions/${encodeURIComponent(executionId)}`, {
      method: 'DELETE', signal: opts?.signal,
    });
    return parseCancelResult(res);
  }

  async cancelRun(scriptName: string, opts?: { signal?: AbortSignal }): Promise<CancelResult> {
    const res = await this.http.fetchOk(this.path(scriptName, 'run'), { method: 'DELETE', signal: opts?.signal });
    return parseCancelResult(res);
  }

  /** Cross-SDK naming alias for {@link cancelRun}. Mirrors the Python SDK's
   *  `cancel_all`. Refs #109 (item 3: method-naming consistency). */
  async cancelAll(scriptName: string, opts?: { signal?: AbortSignal }): Promise<CancelResult> {
    return this.cancelRun(scriptName, opts);
  }

  /**
   * Resume a suspended checkpoint.
   *
   * For plain `checkpoint` resumes pass `data` (the payload that satisfies
   * the checkpoint's schema). For tool-approval gates pass
   * `{ approve: true|false, args_override? }`; the server builds the
   * `{approve, args}` payload the engine's approval gate expects. (#729)
   */
  async resume(
    executionId: string,
    token: string,
    data: unknown,
    opts?: { signal?: AbortSignal; approve?: boolean; args_override?: unknown },
  ): Promise<void> {
    const body: Record<string, unknown> = { token, data };
    if (opts?.approve !== undefined) body.approve = opts.approve;
    if (opts?.args_override !== undefined) body.args_override = opts.args_override;
    await this.http.fetchOk(`${this.http.getBaseUrl()}/executions/${encodeURIComponent(executionId)}/resume`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body), signal: opts?.signal,
    });
  }

  /** Poll until execution reaches a terminal state. */
  async waitFor(
    executionId: string,
    options?: { timeoutMs?: number; pollIntervalMs?: number; signal?: AbortSignal },
  ): Promise<ExecutionOutput> {
    const timeout = options?.timeoutMs ?? 0;
    const interval = options?.pollIntervalMs ?? 500;
    const start = Date.now();

    while (true) {
      if (options?.signal?.aborted) {
        throw new Error(`Execution ${executionId} await aborted`);
      }
      const output = await this.getOutput(executionId, { signal: options?.signal });
      const terminalStates: ReadonlyArray<string> = ['completed', 'failed', 'cancelled'];
      if (output && terminalStates.includes(output.status)) {
        if (output.status === 'failed') {
          const msg = output.error || 'Execution failed';
          const kind = output.error_kind;
          if (kind === 'RateLimit' || kind === 'ServerError' || kind === 'NetworkError') {
            throw new AkribesTransientError(msg, executionId);
          } else if (kind === 'AuthError' || kind === 'TokenLimit') {
            throw new AkribesFatalError(msg, executionId);
          } else {
            throw new AkribesScriptError(msg, executionId);
          }
        }
        return output;
      }
      if (timeout > 0 && Date.now() - start > timeout) {
        throw new AkribesTimeoutError(
          `Execution ${executionId} timed out after ${timeout}ms`,
          executionId,
          timeout,
        );
      }
      await sleepUntilAborted(interval, options?.signal);
    }
  }

  /**
   * Poll an execution to completion and return its final output.
   *
   * Mirrors the Rust SDK's `await_execution`. Unlike {@link RunStream.output}
   * (which only resolves for streams the caller initiated), this works for any
   * `executionId` — including ones discovered via {@link list} or surfaced by
   * another process.
   *
   * Defaults: 5-minute timeout. Pass `timeout: 0` to wait indefinitely. The
   * optional `AbortSignal` short-circuits the poll loop and is forwarded to
   * the underlying HTTP request.
   *
   * Throws {@link AkribesTransientError} on retryable failures (`RateLimit`,
   * `ServerError`, `NetworkError`), {@link AkribesFatalError} on `AuthError` /
   * `TokenLimit`, and {@link AkribesScriptError} for everything else (including
   * cancellation).
   */
  async await(
    executionId: string,
    opts?: { timeout?: number; signal?: AbortSignal },
  ): Promise<ExecutionOutput> {
    return this.waitFor(executionId, {
      timeoutMs: opts?.timeout ?? DEFAULT_AWAIT_TIMEOUT_MS,
      signal: opts?.signal,
    });
  }

  /** Cross-SDK naming alias for {@link await}. Mirrors the Rust SDK's
   *  `await_execution`. Refs #109 (item 3: method-naming consistency). */
  async awaitExecution(
    executionId: string,
    opts?: { timeout?: number; signal?: AbortSignal },
  ): Promise<ExecutionOutput> {
    return this.await(executionId, opts);
  }

  /** Cross-SDK naming alias for {@link await}. Mirrors the Python SDK's
   *  `await_result`. Refs #109 (item 3: method-naming consistency). */
  async awaitResult(
    executionId: string,
    opts?: { timeout?: number; signal?: AbortSignal },
  ): Promise<ExecutionOutput> {
    return this.await(executionId, opts);
  }

  /** Run a script from a specific point, with pre-seeded environment values.
   * Skips nodes whose outputs are provided in seedEnv.
   */
  async runFrom(
    scriptName: string,
    opts: {
      seedEnv: Record<string, unknown>;
      skipNodeIds: number[];
      inputs?: Record<string, unknown>;
      channel?: string;
      triggeredBy?: string;
      signal?: AbortSignal;
    },
  ): Promise<RunResult> {
    const channel = opts.channel ?? 'draft';
    const url = `${this.path(scriptName, 'run', 'from')}?channel=${encodeURIComponent(channel)}`;
    return this.http.fetchJson<RunResult>(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        inputs: opts.inputs,
        seed_env: opts.seedEnv,
        skip_node_ids: opts.skipNodeIds,
        triggered_by: opts.triggeredBy ?? this.defaultTriggeredBy,
      }),
      signal: opts.signal,
    });
  }

  /**
   * Re-run a previous execution with its stored inputs.
   *
   * The server re-submits the original workflow with the exact inputs the
   * prior run used. Document/S3-input runs are re-referenced by their retained
   * `doc_<uuid>` handles; if a referenced document has been purged the call
   * rejects with a `422` ("inputs no longer available") rather than failing
   * mid-spawn. Source resolution prefers the run's original `version_id`,
   * falling back to its channel when that version is gone.
   *
   * Returns the new execution id (and `rerun_of`, the id it was re-run from).
   */
  async rerun(executionId: string, opts?: { signal?: AbortSignal }): Promise<RerunResult> {
    return this.http.fetchJson<RerunResult>(
      `${this.http.getBaseUrl()}/executions/${encodeURIComponent(executionId)}/rerun`,
      { method: 'POST', signal: opts?.signal },
    );
  }

  /** Get the compiled execution DAG for a script. If versionId is omitted, uses the draft. */
  async getGraph(scriptName: string, opts?: { versionId?: number; signal?: AbortSignal }): Promise<ScriptGraph> {
    const params = new URLSearchParams();
    if (opts?.versionId != null) params.set('version', String(opts.versionId));
    const qs = params.toString();
    return this.http.fetchJson<ScriptGraph>(`${this.path(scriptName, 'graph')}${qs ? `?${qs}` : ''}`, { signal: opts?.signal });
  }

  /** Get cost aggregation for the entire project, optionally filtered by date range.
   *  Rolls up: per-script totals, per-channel totals, and overall totals.
   *  `unknown_cost_executions` counts runs whose model wasn't in the pricing
   *  table; their tokens are still included, but they contribute `0` to cost.
   */
  async getProjectCost(opts?: { since?: string; until?: string; signal?: AbortSignal }): Promise<ProjectCost> {
    const params = new URLSearchParams();
    if (opts?.since) params.set('since', opts.since);
    if (opts?.until) params.set('until', opts.until);
    const qs = params.toString();
    return this.http.fetchJson<ProjectCost>(
      `${this.http.getBaseUrl()}/projects/${this.projectId}/cost${qs ? `?${qs}` : ''}`,
      { signal: opts?.signal },
    );
  }

  /** Get cost aggregation for a script (total, avg, per-version, per-channel). */
  async getCost(
    scriptName: string,
    opts?: { since?: string; until?: string; signal?: AbortSignal },
  ): Promise<ScriptCost> {
    const params = new URLSearchParams();
    if (opts?.since) params.set('since', opts.since);
    if (opts?.until) params.set('until', opts.until);
    const qs = params.toString();
    return this.http.fetchJson<ScriptCost>(
      `${this.path(scriptName, 'cost')}${qs ? `?${qs}` : ''}`,
      { signal: opts?.signal },
    );
  }

  /** Run a script and wait for the result. Returns [execution_id, output]. */
  async runAndAwait(
    scriptName: string,
    opts?: {
      inputs?: Record<string, unknown>;
      channel?: string;
      triggeredBy?: string;
      timeoutMs?: number;
      pollIntervalMs?: number;
      signal?: AbortSignal;
    },
  ): Promise<[string, ExecutionOutput]> {
    const { execution_id } = await this.run(scriptName, opts);
    const output = await this.waitFor(execution_id, opts);
    return [execution_id, output];
  }

  /**
   * Start a run and return a {@link RunStream} lifecycle handle. The stream
   * subscribes to events BEFORE issuing the run POST so no early events are
   * missed. It's async-iterable, exposes `.output` / `.executionId` promises,
   * and offers category-based `.on.<cat>()` callback registration.
   *
   * Requires the client was constructed with a `projectId` so the events
   * sub-client is available.
   */
  runStream(scriptName: string, opts?: RunStreamOptions): RunStream {
    if (!this.eventsSourceFactory) {
      throw new Error(
        'runStream() requires the EventsClient to be wired. Construct AkribesClient with a projectId.',
      );
    }
    this.validateContract(scriptName);
    return createRunStream(
      scriptName,
      opts,
      this.eventsSourceFactory(),
      (name, o) => this.run(name, o),
    );
  }

  // ── Document helpers ──────────────────────────────────────────────────

  /** Get document metadata by ID. */
  async getDocument(documentId: string, opts?: { signal?: AbortSignal }): Promise<{
    id: string; filename: string; content_type: string; size_bytes: number;
    content_hash: string; conversion_status: string; conversion_error: string | null;
    created_at: string;
  } | null> {
    return nullOn404(async () =>
      this.http.fetchJson<{
        id: string; filename: string; content_type: string; size_bytes: number;
        content_hash: string; conversion_status: string; conversion_error: string | null;
        created_at: string;
      }>(`${this.http.getBaseUrl()}/documents/${encodeURIComponent(documentId)}`, opts)
    );
  }

  /** Get converted markdown for a document. */
  async getDocumentMarkdown(documentId: string, opts?: { signal?: AbortSignal }): Promise<{ markdown: string }> {
    return this.http.fetchJson<{ markdown: string }>(
      `${this.http.getBaseUrl()}/documents/${encodeURIComponent(documentId)}/markdown`, opts,
    );
  }

  /** Get a presigned download URL for the original document file. */
  async getDocumentUrl(documentId: string, opts?: { signal?: AbortSignal }): Promise<string> {
    const resp = await this.http.fetchOk(
      `${this.http.getBaseUrl()}/documents/${encodeURIComponent(documentId)}/content`,
      { ...opts, redirect: 'manual' },
    );
    return resp.headers.get('location') || resp.url;
  }

  /** Retry conversion on a failed document. */
  async reconvertDocument(documentId: string, opts?: { signal?: AbortSignal }): Promise<{ status: string }> {
    return this.http.fetchJson<{ status: string }>(
      `${this.http.getBaseUrl()}/documents/${encodeURIComponent(documentId)}/convert`,
      { method: 'POST', signal: opts?.signal },
    );
  }
}
