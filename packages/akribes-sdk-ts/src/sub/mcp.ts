import type { HttpClient } from '../http';
import type { McpServerSummary, McpToolSummary } from '../types';

export type McpHealth = {
  status: 'connected' | 'degraded' | 'offline' | 'pinned_offline';
  last_error?: string;
  last_check_at?: string;
};

/** Response from `POST /projects/{id}/mcp/servers/{alias}/refresh` (legacy) and
 *  `POST /projects/{id}/mcp/servers/{alias}/pin`. */
export type McpRefreshResult = {
  refreshed?: boolean;
  pinned?: boolean;
  alias: string;
  tool_count: number;
};

/** Response from `GET /projects/{id}/mcp/servers/{alias}/drift`. */
export type McpDriftResult = {
  drifted: boolean;
  added: string[];
  removed: string[];
  reason?: string;
};

/** Response from `GET /projects/{id}/mcp/servers/{alias}/schema-diff`. Richer
 *  than `drift`: includes `changed` tools (same name, different schema) and a
 *  `reachable` flag so the UI can show "couldn't reach server" rather than a
 *  spinner-forever. */
export type McpSchemaDiff = {
  alias: string;
  pinned: boolean;
  reachable: boolean;
  drifted?: boolean;
  reason?: string;
  added: string[];
  removed: string[];
  changed: string[];
};

/** Auth strategy for a DB-backed / overridden MCP server. The secret value
 *  itself is write-only and never read back. */
export type McpAuthKind = 'none' | 'bearer' | 'header';

/** Origin of a persisted MCP config row. */
export type McpConfigOrigin = 'db' | 'override';

/** A persisted MCP config row (no secret material — only `auth_configured`). */
export type McpConfigRow = {
  alias: string;
  origin: McpConfigOrigin;
  /** `null` for override rows (script owns the URL). */
  url: string | null;
  transport: string;
  timeout_secs: number | null;
  approval_required: boolean | null;
  auth_kind: McpAuthKind;
  auth_header_name: string | null;
  auth_configured: boolean;
};

/** Body for creating / updating an MCP config. The secret is write-only:
 *  omit `auth_secret` on update to keep the stored value; set `clear_secret`
 *  to remove it. */
export type McpConfigInput = {
  alias: string;
  origin?: McpConfigOrigin;
  /** Required for `origin: 'db'`; ignored for overrides. */
  url?: string;
  timeout_secs?: number | null;
  approval_required?: boolean | null;
  auth_kind?: McpAuthKind;
  auth_header_name?: string | null;
  /** Write-only secret. Omit to keep the existing value (on update). */
  auth_secret?: string;
  /** Explicitly clear the stored secret. */
  clear_secret?: boolean;
};

export class McpClient {
  constructor(
    private http: HttpClient,
    private projectId: number,
  ) {}

  private base() {
    return `${this.http.getBaseUrl()}/projects/${this.projectId}/mcp`;
  }

  private serverPath(alias: string) {
    return `${this.base()}/servers/${encodeURIComponent(alias)}`;
  }

  async listServers(opts?: { signal?: AbortSignal }): Promise<McpServerSummary[]> {
    return this.http.fetchJson<McpServerSummary[]>(`${this.base()}/servers`, opts);
  }

  async listTools(opts?: { signal?: AbortSignal }): Promise<McpToolSummary[]> {
    return this.http.fetchJson<McpToolSummary[]>(`${this.base()}/tools`, opts);
  }

  /** Persisted config rows (DB servers + knob overrides). Secrets are never
   *  returned — only `auth_configured`. */
  async listConfigs(opts?: { signal?: AbortSignal }): Promise<McpConfigRow[]> {
    return this.http.fetchJson<McpConfigRow[]>(`${this.base()}/configs`, opts);
  }

  /** Create a DB-backed server or a knob override. 409 if the alias already
   *  has a config row. */
  async createConfig(input: McpConfigInput, opts?: { signal?: AbortSignal }): Promise<McpConfigRow> {
    return this.http.fetchJson<McpConfigRow>(`${this.base()}/servers`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(input),
      signal: opts?.signal,
    });
  }

  /** Update an existing config row. Omit `auth_secret` to keep the stored
   *  secret; pass `clear_secret: true` to remove it. */
  async updateConfig(
    alias: string,
    input: Omit<McpConfigInput, 'alias'>,
    opts?: { signal?: AbortSignal },
  ): Promise<McpConfigRow> {
    return this.http.fetchJson<McpConfigRow>(this.serverPath(alias), {
      method: 'PATCH',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ alias, ...input }),
      signal: opts?.signal,
    });
  }

  /** Delete a config row (a DB server or a knob override). */
  async deleteConfig(alias: string, opts?: { signal?: AbortSignal }): Promise<void> {
    await this.http.fetchOk(this.serverPath(alias), { method: 'DELETE', signal: opts?.signal });
  }

  async health(alias: string, opts?: { signal?: AbortSignal }): Promise<McpHealth> {
    return this.http.fetchJson<McpHealth>(`${this.serverPath(alias)}/health`, opts);
  }

  /**
   * Re-discover the server's tools and store them as the pinned schema. Used
   * after intentionally adopting a schema change (or to pin for the first
   * time). Connection failures surface as an error.
   */
  async pin(alias: string, opts?: { signal?: AbortSignal }): Promise<McpRefreshResult> {
    return this.http.fetchJson<McpRefreshResult>(`${this.serverPath(alias)}/pin`, {
      method: 'POST',
      signal: opts?.signal,
    });
  }

  /**
   * Back-compat alias for {@link pin} — hits the legacy `/refresh` endpoint.
   * @deprecated prefer {@link pin}.
   */
  async refresh(alias: string, opts?: { signal?: AbortSignal }): Promise<McpRefreshResult> {
    return this.http.fetchJson<McpRefreshResult>(`${this.serverPath(alias)}/refresh`, {
      method: 'POST',
      signal: opts?.signal,
    });
  }

  /**
   * Compare the pinned schema against the live `tools/list`, reporting added,
   * removed, and changed tools. `reachable: false` (with a `reason`) means the
   * server couldn't be dialled — the UI should show that state, not a spinner.
   */
  async schemaDiff(alias: string, opts?: { signal?: AbortSignal }): Promise<McpSchemaDiff> {
    const res = (await (await this.http.fetchOk(
      `${this.serverPath(alias)}/schema-diff`,
      opts,
    )).json()) as Partial<McpSchemaDiff>;
    return {
      alias: res.alias ?? alias,
      pinned: !!res.pinned,
      reachable: res.reachable ?? false,
      drifted: res.drifted,
      reason: res.reason,
      added: res.added ?? [],
      removed: res.removed ?? [],
      changed: res.changed ?? [],
    };
  }

  /**
   * Compare the pinned schema against the remote server's live `tools/list`
   * and report added/removed tool names. The server populates `added` and
   * `removed` arrays even when nothing has drifted (both empty in that case).
   */
  async drift(alias: string, opts?: { signal?: AbortSignal }): Promise<McpDriftResult> {
    const res = await (await this.http.fetchOk(
      `${this.serverPath(alias)}/drift`,
      opts,
    )).json() as Partial<McpDriftResult>;
    return {
      drifted: !!res.drifted,
      added: res.added ?? [],
      removed: res.removed ?? [],
      reason: res.reason,
    };
  }
}
