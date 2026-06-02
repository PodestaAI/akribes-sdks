import type { HttpClient } from '../http';
import type { ClientInterest, ClientInfo, RegisterClientResponse, ContractLockInfo } from '../types';

export type ContractState = {
  /** Cached input schemas per script (from init response). */
  schemas: Map<string, [string, string][]>;
  /** Scripts whose schema has changed since init (from SSE events). */
  brokenScripts: Set<string>;
};

/** Reason for a heartbeat status change. */
export type HeartbeatStatus = 'ok' | 'auth_failed' | 'unreachable';

export type ClientsClientOptions = {
  /** Invoked whenever the heartbeat result changes category. Useful for
   *  surfacing a revoked-token state to a UI that needs to prompt for
   *  re-auth. Fired once per state transition (no 30s-stutter). */
  onHeartbeatStatus?: (status: HeartbeatStatus) => void;
};

/** Heartbeat backoff curve — SDK-wide canonical (#1182).
 *  Exponential with full jitter, base 1s, cap 30s. The first failure waits
 *  ~1s before retrying, the second ~2s, ..., capped at ~30s. Jitter spreads
 *  reconnect attempts when many clients lose their token at once.
 *
 *  Exported for reuse in the SSE reconnect path and for tests. */
export function heartbeatBackoffMs(consecutiveFailures: number): number {
  if (consecutiveFailures <= 0) return 0;
  const base = 1_000;
  const cap = 30_000;
  const exp = Math.min(base * 2 ** (consecutiveFailures - 1), cap);
  return Math.floor(Math.random() * exp);
}

export class ClientsClient {
  private heartbeatTimer: ReturnType<typeof setTimeout> | undefined;
  private inflightHeartbeat: Promise<void> | undefined;
  private consecutiveFailures = 0;
  private lastStatus: HeartbeatStatus | undefined;
  private paused = false;
  readonly contractState: ContractState = {
    schemas: new Map(),
    brokenScripts: new Set(),
  };

  constructor(
    private http: HttpClient,
    private projectId: number,
    private clientId: string | undefined,
    private clientName: string | undefined,
    private options: ClientsClientOptions = {},
  ) {}

  async init(interests: ClientInterest[], opts?: { signal?: AbortSignal }): Promise<RegisterClientResponse> {
    if (!this.clientId || !this.clientName) {
      throw new Error('clientId and clientName are required for init(). Pass id and name to AkribesClient constructor.');
    }
    const res = await this.http.fetchJson<RegisterClientResponse>(`${this.http.getBaseUrl()}/projects/${this.projectId}/clients`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ id: this.clientId, name: this.clientName, interests }),
      signal: opts?.signal,
    });

    // Populate contract state from response
    this.contractState.brokenScripts.clear();
    if (res.interests) {
      for (const interest of res.interests) {
        this.contractState.schemas.set(interest.script_name, interest.input_schema);
      }
    }

    // Reset paused state from any prior `auth_failed` so a fresh `init()`
    // (e.g. after `setToken()` with a new token) revives the heartbeat.
    this.paused = false;
    this.consecutiveFailures = 0;
    this.startHeartbeat();
    return res;
  }

  /** Resume a heartbeat that was paused after an auth failure. Call this
   *  after rotating the underlying token via {@link AkribesClient.setToken}. */
  resumeHeartbeat() {
    if (!this.heartbeatTimer && !this.inflightHeartbeat) {
      this.paused = false;
      this.consecutiveFailures = 0;
      this.startHeartbeat();
    }
  }

  private fireStatus(status: HeartbeatStatus) {
    if (this.lastStatus === status) return;
    this.lastStatus = status;
    try { this.options.onHeartbeatStatus?.(status); } catch { /* swallow */ }
  }

  private startHeartbeat() {
    // Use a recursive setTimeout chain rather than setInterval so we can vary
    // the next delay based on the previous result — the canonical SDK-wide
    // pattern (#1182). Base interval 30s on success, exponential-with-jitter
    // capped at 30s on failure (added on TOP of the base interval).
    const tick = () => {
      this.inflightHeartbeat = (async () => {
        try {
          const res = await this.http.authFetch(`${this.http.getBaseUrl()}/heartbeat`, {
            method: 'POST', headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ client_id: this.clientId }),
          });
          if (!res.ok) {
            if (res.status === 401 || res.status === 403) {
              // Revoked / expired token. Stop ticking — a 401/403 will not
              // self-heal by waiting, only by `setToken()` + `resumeHeartbeat()`.
              // Surfaces via the typed callback (#1220) instead of an endless
              // console.warn loop.
              this.fireStatus('auth_failed');
              this.paused = true;
              return;
            }
            this.consecutiveFailures += 1;
            this.fireStatus('unreachable');
            console.warn(`[Akribes SDK] heartbeat rejected: HTTP ${res.status}`);
          } else {
            this.consecutiveFailures = 0;
            this.fireStatus('ok');
          }
        } catch (e) {
          this.consecutiveFailures += 1;
          this.fireStatus('unreachable');
          console.warn('[Akribes SDK] heartbeat failed:', e);
        } finally {
          this.inflightHeartbeat = undefined;
          if (!this.paused) {
            const backoff = heartbeatBackoffMs(this.consecutiveFailures);
            this.heartbeatTimer = setTimeout(tick, 30_000 + backoff);
          } else {
            this.heartbeatTimer = undefined;
          }
        }
      })();
    };

    if (this.heartbeatTimer) clearTimeout(this.heartbeatTimer);
    this.heartbeatTimer = setTimeout(tick, 30_000);
  }

  async list(opts?: { signal?: AbortSignal }): Promise<ClientInfo[]> {
    return this.http.fetchJson<ClientInfo[]>(`${this.http.getBaseUrl()}/projects/${this.projectId}/clients`, opts);
  }

  async delete(id: string, opts?: { signal?: AbortSignal }): Promise<void> {
    await this.http.fetchOk(`${this.http.getBaseUrl()}/clients/${encodeURIComponent(id)}`, {
      method: 'DELETE', signal: opts?.signal,
    });
  }

  // ── Lock management ─────────────────────────────────────────────────

  async listLocks(scriptName: string, opts?: { signal?: AbortSignal }): Promise<ContractLockInfo[]> {
    return this.http.fetchJson<ContractLockInfo[]>(
      `${this.http.getBaseUrl()}/projects/${this.projectId}/scripts/${encodeURIComponent(scriptName)}/locks`,
      opts,
    );
  }

  async revokeLock(scriptName: string, lockId: number, opts?: { signal?: AbortSignal }): Promise<void> {
    await this.http.fetchOk(
      `${this.http.getBaseUrl()}/projects/${this.projectId}/scripts/${encodeURIComponent(scriptName)}/locks/${lockId}`,
      { method: 'DELETE', signal: opts?.signal },
    );
  }

  async rebindLock(scriptName: string, lockId: number, versionId?: number, opts?: { signal?: AbortSignal }): Promise<ContractLockInfo> {
    return this.http.fetchJson<ContractLockInfo>(
      `${this.http.getBaseUrl()}/projects/${this.projectId}/scripts/${encodeURIComponent(scriptName)}/locks/${lockId}/rebind`,
      {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ version_id: versionId ?? null }),
        signal: opts?.signal,
      },
    );
  }

  // ── Flat cross-project lock helpers (#1133) ───────────────────────────
  //
  // The methods above operate on `this.projectId`. The flat helpers below
  // take an explicit `projectId` so admin tools spanning multiple projects
  // can manage locks without spinning up a fresh project-scoped client.
  // Mirrors Rust's `list_locks_for` / `delete_lock` / `update_lock`.

  /** List contract locks for `scriptName` in `projectId`. Cross-project
   *  variant of {@link listLocks}; the implicit project_id on this client
   *  is ignored. */
  async listLocksFor(projectId: number, scriptName: string, opts?: { signal?: AbortSignal }): Promise<ContractLockInfo[]> {
    return this.http.fetchJson<ContractLockInfo[]>(
      `${this.http.getBaseUrl()}/projects/${projectId}/scripts/${encodeURIComponent(scriptName)}/locks`,
      opts,
    );
  }

  /** Delete (revoke) a single lock in `projectId`. Cross-project variant
   *  of {@link revokeLock}. */
  async deleteLock(projectId: number, scriptName: string, lockId: number, opts?: { signal?: AbortSignal }): Promise<void> {
    await this.http.fetchOk(
      `${this.http.getBaseUrl()}/projects/${projectId}/scripts/${encodeURIComponent(scriptName)}/locks/${lockId}`,
      { method: 'DELETE', signal: opts?.signal },
    );
  }

  /** Update (rebind) a single lock to a new version in `projectId`.
   *  Cross-project variant of {@link rebindLock}. Pass `versionId =
   *  undefined` to rebind the lock to the channel's current version. */
  async updateLock(
    projectId: number,
    scriptName: string,
    lockId: number,
    versionId?: number,
    opts?: { signal?: AbortSignal },
  ): Promise<ContractLockInfo> {
    return this.http.fetchJson<ContractLockInfo>(
      `${this.http.getBaseUrl()}/projects/${projectId}/scripts/${encodeURIComponent(scriptName)}/locks/${lockId}/rebind`,
      {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ version_id: versionId ?? null }),
        signal: opts?.signal,
      },
    );
  }

  async destroy() {
    if (this.heartbeatTimer) clearTimeout(this.heartbeatTimer);
    this.heartbeatTimer = undefined;
    this.paused = true;
    if (this.inflightHeartbeat) await this.inflightHeartbeat;
  }
}
