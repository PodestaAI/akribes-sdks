import type { HttpClient } from '../http';
import type { TokenInfo, MintTokenResponse } from '../types';

export type TokenScopes = {
  /** `'*'` for all projects, or an array of project IDs. */
  projects: '*' | number[];
  /** Permission level. `admin` can mint/revoke tokens; `editor` can modify
   *  scripts and executions; `viewer` is read-only. */
  role: 'admin' | 'editor' | 'viewer';
  /** Optional: restrict the token to specific script names. */
  scripts?: string[];
  /** Optional: restrict the token to specific execution IDs (one-off
   *  read-only sharing). */
  executions?: string[];
  /** Whether the new token may itself mint child tokens. Defaults to
   *  `false`. Service tokens always pass; scoped minters must already have
   *  `can_mint` set on their own scopes for this to be honored. Use `true`
   *  for long-lived Personal API Keys; leave `false` for browser sessions. */
  can_mint?: boolean;
  /** Optional org binding. When set, the minted token deserializes with
   *  `org_id: <value>`; when omitted, the server stamps it `NULL`. Org-aware
   *  callers (e.g. Studio's multi-tenant flow) must pass this through or
   *  newly-created projects become invisible to the same user's org-wide
   *  tokens. */
  org_id?: number;
};

export type MintTokenRequest = {
  /** Email used for metrics attribution and offboarding. Optional but
   *  strongly recommended for end-user tokens so you can later revoke all
   *  tokens for a removed user via {@link TokensClient.revokeByEmail}. */
  user_email?: string;
  scopes: TokenScopes;
  /** Token lifetime in seconds. Server-enforced max is 90 days
   *  (`90 * 24 * 3600 = 7_776_000`). Use ~28800 (8 h) for browser sessions
   *  and 7_776_000 (90 days) for CLI tokens. */
  expires_in: number;
  /** Human-readable label shown in the token list UI. Max 128 chars. */
  label: string;
};

/**
 * Token management API.
 *
 * **The auth model in one paragraph:** akribes-server has two token types.
 * Service tokens live in env vars (`AKRIBES_SERVICE_TOKEN_<NAME>=<scope>:<secret>`)
 * and never expire — your backend uses one to talk to akribes-server. Scoped
 * tokens are minted at runtime via this client and stored in the DB. They
 * expire (max 90 days), can be revoked, and are what you hand out to
 * browsers / end-users / CLIs.
 *
 * @example Backend → mint a per-user token for a browser session
 * ```ts
 * // your backend has AKRIBES_SERVICE_TOKEN=*:secret in its env
 * const akribes = new AkribesClient({
 *   baseUrl: 'https://akribes.example.com',
 *   token: process.env.AKRIBES_SERVICE_TOKEN!,
 * });
 *
 * const minted = await akribes.tokens.mint({
 *   user_email: session.user.email,
 *   scopes: { projects: '*', role: 'admin' },
 *   expires_in: 8 * 3600,            // 8 hour browser session
 *   label: `web-session:${session.id}`,
 * });
 * // → ship `minted.token` to the browser; the browser uses it directly
 * //   against akribes-server.
 * ```
 *
 * @example One-off read-only share for a single execution
 * ```ts
 * const minted = await akribes.tokens.mint({
 *   user_email: 'guest@acme.com',
 *   scopes: {
 *     projects: [2],
 *     role: 'viewer',
 *     executions: [executionId],     // read-only on this single exec
 *   },
 *   expires_in: 3600,
 *   label: `share:${executionId}`,
 * });
 * ```
 *
 * @example Offboarding — revoke every token for a removed user
 * ```ts
 * await akribes.tokens.revokeByEmail('removed@example.com');
 * ```
 */
export class TokensClient {
  constructor(private http: HttpClient) {}

  private get base() { return `${this.http.getBaseUrl()}/tokens`; }

  /** Mint a new scoped token. Only service tokens can mint. */
  async mint(req: MintTokenRequest, opts?: { signal?: AbortSignal }): Promise<MintTokenResponse> {
    return this.http.fetchJson<MintTokenResponse>(this.base, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(req),
      signal: opts?.signal,
    });
  }

  /** List tokens. Service tokens see all; scoped tokens see only their own.
   *
   *  Pagination + filters mirror akribes-server's `GET /tokens` query params:
   *
   *  - `limit` / `offset` — page size (server default 50, max 500) and offset
   *    into the result set. Scoped-token callers ignore these (their list
   *    is already capped to their own row).
   *  - `userEmail` — service-token only; narrows the list to one user's
   *    tokens. Use this on per-user surfaces (e.g. a Personal API Keys page)
   *    so users with older tokens never drop off the first page.
   *  - `includeRevoked` / `includeExpired` — surface tokens hidden by
   *    default. Useful for offboarding tooling.
   */
  async list(
    opts?: {
      signal?: AbortSignal;
      limit?: number;
      offset?: number;
      userEmail?: string;
      includeRevoked?: boolean;
      includeExpired?: boolean;
    },
  ): Promise<TokenInfo[]> {
    const params = new URLSearchParams();
    if (opts?.limit != null) params.set('limit', String(opts.limit));
    if (opts?.offset != null) params.set('offset', String(opts.offset));
    if (opts?.userEmail) params.set('user_email', opts.userEmail);
    if (opts?.includeRevoked) params.set('include_revoked', 'true');
    if (opts?.includeExpired) params.set('include_expired', 'true');
    const qs = params.toString();
    const url = qs ? `${this.base}?${qs}` : this.base;
    return this.http.fetchJson<TokenInfo[]>(url, { signal: opts?.signal });
  }

  /** Revoke a single token by ID. */
  async revoke(tokenId: string, opts?: { signal?: AbortSignal }): Promise<void> {
    await this.http.fetchOk(`${this.base}/${encodeURIComponent(tokenId)}`, {
      method: 'DELETE', signal: opts?.signal,
    });
  }

  /** Revoke all tokens for a user email (offboarding). Only service tokens can do this. */
  async revokeByEmail(email: string, opts?: { signal?: AbortSignal }): Promise<{ revoked: number }> {
    return this.http.fetchJson<{ revoked: number }>(`${this.base}?user_email=${encodeURIComponent(email)}`, {
      method: 'DELETE', signal: opts?.signal,
    });
  }
}
