import type { HttpClient } from '../http';
import { nullOn404 } from '../http';
import type { ScriptVersion, ScriptVersionResponse, DryRunResult } from '../types';

export class VersionsClient {
  constructor(
    private http: HttpClient,
    private projectId: number,
    private defaultPublishedBy: string | undefined,
  ) {}

  private path(scriptName: string, ...segments: string[]) {
    return this.http.scriptPath(this.projectId, scriptName, ...segments);
  }

  /** List published versions, newest first.
   *
   * The server caps each page (default 50, max 500) and carries the full
   * source blob per row, so long-lived scripts must paginate. Pass `limit` /
   * `offset` to walk past the first page. */
  async list(
    scriptName: string,
    opts?: { limit?: number; offset?: number; signal?: AbortSignal },
  ): Promise<ScriptVersion[]> {
    const params = new URLSearchParams();
    if (opts?.limit != null) params.set('limit', String(opts.limit));
    if (opts?.offset != null) params.set('offset', String(opts.offset));
    const qs = params.toString();
    const url = `${this.path(scriptName, 'versions')}${qs ? `?${qs}` : ''}`;
    return this.http.fetchJson<ScriptVersion[]>(url, { signal: opts?.signal });
  }

  async get(scriptName: string, versionId: number, opts?: { signal?: AbortSignal }): Promise<ScriptVersion | null> {
    return nullOn404(async () =>
      this.http.fetchJson<ScriptVersion>(this.path(scriptName, 'versions', String(versionId)), opts)
    );
  }

  async getLatest(scriptName: string, opts?: { signal?: AbortSignal }): Promise<ScriptVersionResponse | null> {
    return nullOn404(async () =>
      this.http.fetchJson<ScriptVersionResponse>(this.path(scriptName, 'latest'), opts)
    );
  }

  /** @deprecated Use scripts.saveDraft + versions.publish instead */
  async save(scriptName: string, source: string, opts?: { signal?: AbortSignal }): Promise<void> {
    await this.http.fetchOk(this.path(scriptName, 'versions'), {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ source }), signal: opts?.signal,
    });
  }

  async publish(
    scriptName: string,
    label: string | null,
    channels: string[],
    publishedBy?: string,
    opts?: { force?: boolean; reason?: string; dryRun?: boolean; signal?: AbortSignal },
  ): Promise<ScriptVersion | DryRunResult> {
    const body = await (await this.http.fetchOk(this.path(scriptName, 'publish'), {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        label, channels,
        published_by: publishedBy ?? this.defaultPublishedBy,
        force: opts?.force,
        // The server persists `reason` on the versions row's
        // force_published_reason column when force is true and the
        // unified contract check produced breaks. ≥ 20 chars enforced
        // server-side; the Studio enforces it client-side too for fast
        // feedback but the source of truth is the server.
        reason: opts?.reason,
        dry_run: opts?.dryRun,
      }),
      signal: opts?.signal,
    })).json() as { dry_run?: boolean; version?: ScriptVersion } & DryRunResult;
    if (body.dry_run) return body as DryRunResult;
    return body.version as ScriptVersion;
  }
}
