import type { HttpClient } from '../http';
import type { McpServerSummary, McpToolSummary } from '../types';

export type McpHealth = {
  status: 'connected' | 'degraded' | 'offline' | 'pinned_offline';
  last_error?: string;
  last_check_at?: string;
};

/** Response from `POST /projects/{id}/mcp/servers/{alias}/refresh`. */
export type McpRefreshResult = {
  refreshed: boolean;
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

export class McpClient {
  constructor(
    private http: HttpClient,
    private projectId: number,
  ) {}

  private base() {
    return `${this.http.getBaseUrl()}/projects/${this.projectId}/mcp`;
  }

  async listServers(opts?: { signal?: AbortSignal }): Promise<McpServerSummary[]> {
    return (await this.http.fetchOk(`${this.base()}/servers`, opts)).json();
  }

  async listTools(opts?: { signal?: AbortSignal }): Promise<McpToolSummary[]> {
    return (await this.http.fetchOk(`${this.base()}/tools`, opts)).json();
  }

  async health(alias: string, opts?: { signal?: AbortSignal }): Promise<McpHealth> {
    return (await this.http.fetchOk(`${this.base()}/servers/${encodeURIComponent(alias)}/health`, opts)).json();
  }

  /**
   * Force a fresh `tools/list` against the remote MCP server and update the
   * pinned schema in the DB. Returns the new tool count.
   */
  async refresh(alias: string, opts?: { signal?: AbortSignal }): Promise<McpRefreshResult> {
    return (await this.http.fetchOk(
      `${this.base()}/servers/${encodeURIComponent(alias)}/refresh`,
      { method: 'POST', signal: opts?.signal },
    )).json();
  }

  /**
   * Compare the pinned schema against the remote server's live `tools/list`
   * and report added/removed tool names. The server populates `added` and
   * `removed` arrays even when nothing has drifted (both empty in that case).
   */
  async drift(alias: string, opts?: { signal?: AbortSignal }): Promise<McpDriftResult> {
    const res = await (await this.http.fetchOk(
      `${this.base()}/servers/${encodeURIComponent(alias)}/drift`,
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
