import type { HttpClient } from '../http';
import type { McpAuthKind, McpConfigOrigin, McpConfigRow } from './mcp';

/** Body for creating / updating an ORG-scoped MCP config row.
 *
 *  Like the project-scoped {@link McpConfigInput}, the inline secret is
 *  write-only. In addition, an org row may reference a named vault secret via
 *  `auth_secret_ref: '{NAME}'` instead of an inline secret. */
export type OrgMcpConfigInput = {
  alias: string;
  origin?: McpConfigOrigin;
  /** Required for `origin: 'db'`; ignored for overrides. */
  url?: string;
  timeout_secs?: number | null;
  approval_required?: boolean | null;
  auth_kind?: McpAuthKind;
  auth_header_name?: string | null;
  /** Write-only inline secret. Omit to keep the stored value (on update). */
  auth_secret?: string;
  /** Reference an org vault secret by name (`{NAME}`) instead of an inline
   *  secret. The resolver dereferences it at run time. */
  auth_secret_ref?: string;
  /** Explicitly clear the stored secret. */
  clear_secret?: boolean;
};

/** A vault entry as returned by the API — name + metadata, never the value. */
export type OrgSecretMeta = {
  name: string;
  description: string | null;
  created_at: string;
  updated_at: string;
};

/** Org model policy: a default model applied when a script omits one, plus an
 *  allowlist enforced at compile/run. */
export type OrgModelPolicy = {
  /** Provider-namespaced default alias (e.g. `anthropic.claude-opus-4-7`). */
  default_model: string | null;
  /** Allowed provider-namespaced aliases; empty = no restriction. */
  model_allowlist: string[];
};

/** Org spend cap + current-period usage. */
export type OrgSpendCap = {
  /** USD cap per period, or null when unmetered. */
  spend_cap_usd: number | null;
  spend_period_start: string | null;
  /** Accumulated USD spend in the current period. */
  spent_usd: number;
};

/** One ranked breakdown row of the org-usage dashboard (project / workflow /
 *  user). `key` is the grouping key the UI links on; `label` is the display
 *  string (often equal to `key`). */
export type OrgUsageBreakdownRow = {
  key: string;
  label: string;
  executions: number;
  total_cost_usd: number;
  /** Runs whose cost was unknown (unpriced model). */
  unknown_cost_executions: number;
};

/** One day of the spend time series (UTC calendar day, `YYYY-MM-DD`). */
export type OrgUsageDailyPoint = {
  date: string;
  executions: number;
  total_cost_usd: number;
};

/** The org's limits + headroom: where it stands against its three caps. */
export type OrgUsageLimits = {
  /** USD spend cap per period, or null when unmetered. */
  spend_cap_usd: number | null;
  /** Accumulated USD spend in the current spend-cap period. */
  period_spend_usd: number;
  spend_period_start: string | null;
  /** Remaining metered executions this period, or null when unmetered. */
  quota_remaining: number | null;
  /** Per-org simultaneous-execution cap (`0` = disabled). */
  concurrency_limit: number;
  /** Currently in-flight executions for the org. */
  in_flight_executions: number;
};

/** Org-wide usage dashboard payload for a `[from, to)` window. */
export type OrgUsage = {
  from: string | null;
  to: string | null;
  total_executions: number;
  total_cost_usd: number;
  unknown_cost_executions: number;
  by_project: OrgUsageBreakdownRow[];
  by_script: OrgUsageBreakdownRow[];
  /** Per-user breakdown. Admin-only — the endpoint is `org.budget.edit`-gated. */
  by_user: OrgUsageBreakdownRow[];
  daily: OrgUsageDailyPoint[];
  limits: OrgUsageLimits;
};

/** One user's spend + run count within an org. */
export type OrgUserUsage = {
  executions: number;
  total_cost_usd: number;
  unknown_cost_executions: number;
  last_run_at: string | null;
};

/**
 * Organization-level configuration scope.
 *
 * The org is a real configuration scope — set once, inherited into every
 * project, overridable per project/script. This sub-client wraps:
 *
 *  - **MCP servers** — org-scoped servers/overrides inherited by all projects
 *    (precedence `script > project > org > env`).
 *  - **Secrets vault** — named, write-only secrets referenced as `{NAME}`.
 *  - **Model policy** — default model + allowlist.
 *  - **Spend cap** — per-period USD cap, with current usage.
 *
 * Org-scoped, not project-scoped, so it lives on the base client and takes an
 * `orgId` per call.
 */
export class OrgConfigClient {
  constructor(private http: HttpClient) {}

  private base(orgId: number) {
    return `${this.http.getBaseUrl()}/organizations/${orgId}`;
  }

  // ── MCP config ────────────────────────────────────────────────────────────

  async listMcpConfigs(
    orgId: number,
    opts?: { signal?: AbortSignal },
  ): Promise<McpConfigRow[]> {
    return this.http.fetchJson<McpConfigRow[]>(`${this.base(orgId)}/mcp/configs`, opts);
  }

  async createMcpConfig(
    orgId: number,
    input: OrgMcpConfigInput,
    opts?: { signal?: AbortSignal },
  ): Promise<McpConfigRow> {
    return this.http.fetchJson<McpConfigRow>(`${this.base(orgId)}/mcp/configs`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(input),
      signal: opts?.signal,
    });
  }

  async updateMcpConfig(
    orgId: number,
    alias: string,
    input: Omit<OrgMcpConfigInput, 'alias'>,
    opts?: { signal?: AbortSignal },
  ): Promise<McpConfigRow> {
    return this.http.fetchJson<McpConfigRow>(
      `${this.base(orgId)}/mcp/configs/${encodeURIComponent(alias)}`,
      {
        method: 'PATCH',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(input),
        signal: opts?.signal,
      },
    );
  }

  async deleteMcpConfig(
    orgId: number,
    alias: string,
    opts?: { signal?: AbortSignal },
  ): Promise<void> {
    await this.http.fetchOk(
      `${this.base(orgId)}/mcp/configs/${encodeURIComponent(alias)}`,
      { method: 'DELETE', signal: opts?.signal },
    );
  }

  // ── Secrets vault ───────────────────────────────────────────────────────────

  async listSecrets(
    orgId: number,
    opts?: { signal?: AbortSignal },
  ): Promise<OrgSecretMeta[]> {
    return this.http.fetchJson<OrgSecretMeta[]>(`${this.base(orgId)}/secrets`, opts);
  }

  /** Add a new named secret. The value is write-only — never returned. */
  async createSecret(
    orgId: number,
    input: { name: string; value: string; description?: string },
    opts?: { signal?: AbortSignal },
  ): Promise<OrgSecretMeta> {
    return this.http.fetchJson<OrgSecretMeta>(`${this.base(orgId)}/secrets`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(input),
      signal: opts?.signal,
    });
  }

  /** Rotate an existing secret's value (and optionally its description). */
  async rotateSecret(
    orgId: number,
    name: string,
    input: { value: string; description?: string },
    opts?: { signal?: AbortSignal },
  ): Promise<OrgSecretMeta> {
    return this.http.fetchJson<OrgSecretMeta>(
      `${this.base(orgId)}/secrets/${encodeURIComponent(name)}`,
      {
        method: 'PUT',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(input),
        signal: opts?.signal,
      },
    );
  }

  async deleteSecret(
    orgId: number,
    name: string,
    opts?: { signal?: AbortSignal },
  ): Promise<void> {
    await this.http.fetchOk(
      `${this.base(orgId)}/secrets/${encodeURIComponent(name)}`,
      { method: 'DELETE', signal: opts?.signal },
    );
  }

  // ── Model policy ────────────────────────────────────────────────────────────

  async getModelPolicy(
    orgId: number,
    opts?: { signal?: AbortSignal },
  ): Promise<OrgModelPolicy> {
    return this.http.fetchJson<OrgModelPolicy>(`${this.base(orgId)}/model-policy`, opts);
  }

  async setModelPolicy(
    orgId: number,
    input: OrgModelPolicy,
    opts?: { signal?: AbortSignal },
  ): Promise<OrgModelPolicy> {
    return this.http.fetchJson<OrgModelPolicy>(`${this.base(orgId)}/model-policy`, {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(input),
      signal: opts?.signal,
    });
  }

  // ── Spend cap ─────────────────────────────────────────────────────────────

  async getSpendCap(
    orgId: number,
    opts?: { signal?: AbortSignal },
  ): Promise<OrgSpendCap> {
    return this.http.fetchJson<OrgSpendCap>(`${this.base(orgId)}/spend-cap`, opts);
  }

  async setSpendCap(
    orgId: number,
    input: { spend_cap_usd: number | null; spend_period_start?: string | null },
    opts?: { signal?: AbortSignal },
  ): Promise<OrgSpendCap> {
    return this.http.fetchJson<OrgSpendCap>(`${this.base(orgId)}/spend-cap`, {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(input),
      signal: opts?.signal,
    });
  }

  // ── Usage dashboard ─────────────────────────────────────────────────────────

  /** Org-wide spend + usage over a `[from, to)` window: totals, breakdowns by
   *  project / workflow / user, a daily time series, and the current limits +
   *  headroom. Admin-only on the server (`org.budget.edit`). Omit `from`/`to`
   *  for all-time. */
  async getUsage(
    orgId: number,
    opts?: { from?: string; to?: string; signal?: AbortSignal },
  ): Promise<OrgUsage> {
    const qs = new URLSearchParams();
    if (opts?.from) qs.set('from', opts.from);
    if (opts?.to) qs.set('to', opts.to);
    const query = qs.toString();
    return this.http.fetchJson<OrgUsage>(
      `${this.base(orgId)}/usage${query ? `?${query}` : ''}`,
      { signal: opts?.signal },
    );
  }

  /** One user's spend + run count within the org over a `[from, to)` window.
   *  Admin-only (`org.budget.edit`). */
  async getUserUsage(
    orgId: number,
    email: string,
    opts?: { from?: string; to?: string; signal?: AbortSignal },
  ): Promise<OrgUserUsage> {
    const qs = new URLSearchParams();
    if (opts?.from) qs.set('from', opts.from);
    if (opts?.to) qs.set('to', opts.to);
    const query = qs.toString();
    return this.http.fetchJson<OrgUserUsage>(
      `${this.base(orgId)}/users/${encodeURIComponent(email)}/usage${query ? `?${query}` : ''}`,
      { signal: opts?.signal },
    );
  }
}
