import type { HttpClient } from '../http';
import { nullOn404 } from '../http';
import type { EvalSuite, EvalSuiteSummary, EvalRun, EvalResult } from '../types';

export class EvalsClient {
  constructor(
    private http: HttpClient,
    private projectId: number,
  ) {}

  private path(scriptName: string, ...segments: string[]) {
    return this.http.scriptPath(this.projectId, scriptName, ...segments);
  }

  // ── Suites ────────────────────────────────────────────────────────────────

  async listSuites(scriptName: string, opts?: { signal?: AbortSignal }): Promise<EvalSuite[]> {
    return (await this.http.fetchOk(this.path(scriptName, 'eval-suites'), opts)).json();
  }

  async createSuite(
    scriptName: string,
    data: { name: string; runner_url: string; config?: Record<string, unknown>; auto_run_channels?: string[] },
    opts?: { signal?: AbortSignal },
  ): Promise<EvalSuite> {
    return (await this.http.fetchOk(this.path(scriptName, 'eval-suites'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(data),
      signal: opts?.signal,
    })).json();
  }

  async getSuite(scriptName: string, suiteId: number, opts?: { signal?: AbortSignal }): Promise<EvalSuite | null> {
    return nullOn404(async () =>
      (await this.http.fetchOk(this.path(scriptName, 'eval-suites', String(suiteId)), opts)).json()
    );
  }

  async updateSuite(
    scriptName: string,
    suiteId: number,
    data: { runner_url?: string; config?: Record<string, unknown>; auto_run_channels?: string[] },
    opts?: { signal?: AbortSignal },
  ): Promise<EvalSuite> {
    return (await this.http.fetchOk(this.path(scriptName, 'eval-suites', String(suiteId)), {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(data),
      signal: opts?.signal,
    })).json();
  }

  async deleteSuite(scriptName: string, suiteId: number, opts?: { signal?: AbortSignal }): Promise<void> {
    await this.http.fetchOk(this.path(scriptName, 'eval-suites', String(suiteId)), {
      method: 'DELETE',
      signal: opts?.signal,
    });
  }

  async checkRunnerHealth(
    scriptName: string,
    suiteId: number,
    opts?: { signal?: AbortSignal },
  ): Promise<{ status: string; http_status?: number; error?: string } | null> {
    return nullOn404(async () =>
      (await this.http.fetchOk(this.path(scriptName, 'eval-suites', String(suiteId), 'health'), opts)).json()
    );
  }

  // ── Trigger & Cancel ──────────────────────────────────────────────────────

  async trigger(
    scriptName: string,
    suiteId: number,
    data?: { source?: string; channel?: string; auto_publish?: boolean; triggered_by?: string },
    opts?: { signal?: AbortSignal },
  ): Promise<EvalRun> {
    return (await this.http.fetchOk(this.path(scriptName, 'eval-suites', String(suiteId), 'trigger'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(data ?? {}),
      signal: opts?.signal,
    })).json();
  }

  async cancel(runId: number, opts?: { signal?: AbortSignal }): Promise<EvalRun> {
    return (await this.http.fetchOk(`${this.http.getBaseUrl()}/eval-runs/${runId}`, {
      method: 'DELETE',
      signal: opts?.signal,
    })).json();
  }

  // ── Runs & Results ────────────────────────────────────────────────────────

  async listRuns(
    scriptName: string,
    params?: { suite_id?: number; limit?: number; offset?: number },
    opts?: { signal?: AbortSignal },
  ): Promise<EvalRun[]> {
    const qs = new URLSearchParams();
    if (params?.suite_id != null) qs.set('suite_id', String(params.suite_id));
    if (params?.limit != null) qs.set('limit', String(params.limit));
    if (params?.offset != null) qs.set('offset', String(params.offset));
    const suffix = qs.toString() ? `?${qs}` : '';
    return (await this.http.fetchOk(this.path(scriptName, 'eval-runs') + suffix, opts)).json();
  }

  async getRun(runId: number, opts?: { signal?: AbortSignal }): Promise<EvalRun | null> {
    return nullOn404(async () =>
      (await this.http.fetchOk(`${this.http.getBaseUrl()}/eval-runs/${runId}`, opts)).json()
    );
  }

  async getResults(runId: number, opts?: { signal?: AbortSignal }): Promise<EvalResult[]> {
    return (await this.http.fetchOk(`${this.http.getBaseUrl()}/eval-runs/${runId}/results`, opts)).json();
  }

  // ── Project-level cross-script dashboard (sub-spec 1a) ────────────────────

  /**
   * One row per eval suite in the configured project, including the latest +
   * prior completed average score (drives the cross-script dashboard).
   */
  async listProjectSummaries(opts?: { signal?: AbortSignal }): Promise<EvalSuiteSummary[]> {
    return (await this.http.fetchOk(
      `${this.http.getBaseUrl()}/projects/${this.projectId}/eval-suite-summaries`,
      opts,
    )).json();
  }
}
