import type { HttpClient } from '../http';
import type { Project, Script, ScriptChannel, ProjectCost } from '../types';

export class ProjectsClient {
  constructor(private http: HttpClient) {}

  private get base() { return `${this.http.getBaseUrl()}/projects`; }

  private scriptPath(projectId: number, scriptName: string, ...segments: string[]) {
    return this.http.scriptPath(projectId, scriptName, ...segments);
  }

  async list(opts?: { signal?: AbortSignal }): Promise<Project[]> {
    return this.http.fetchJson<Project[]>(this.base, opts);
  }

  async create(name: string, opts?: { signal?: AbortSignal }): Promise<Project> {
    return this.http.fetchJson<Project>(this.base, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name }), signal: opts?.signal,
    });
  }

  /** Fetch a project by numeric id or name. The server resolves either,
   *  so callers holding only a URL slug don't need a name→id round-trip. */
  async get(id: number | string, opts?: { signal?: AbortSignal }): Promise<Project> {
    return this.http.fetchJson<Project>(`${this.base}/${encodeURIComponent(String(id))}`, opts);
  }

  async update(id: number, name: string, opts?: { signal?: AbortSignal }): Promise<Project> {
    return this.http.fetchJson<Project>(`${this.base}/${id}`, {
      method: 'PATCH', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name }), signal: opts?.signal,
    });
  }

  async delete(id: number, opts?: { signal?: AbortSignal }): Promise<void> {
    await this.http.fetchOk(`${this.base}/${id}`, { method: 'DELETE', signal: opts?.signal });
  }

  /** List scripts for a specific project (cross-project, no bound projectId required).
   *  Accepts a numeric id or a name — the server resolves either. */
  async listScripts(projectId: number | string, opts?: { signal?: AbortSignal }): Promise<Script[]> {
    return this.http.fetchJson<Script[]>(`${this.base}/${encodeURIComponent(String(projectId))}/scripts`, opts);
  }

  /** List channels for a script in a specific project (cross-project). */
  async listChannels(projectId: number, scriptName: string, opts?: { signal?: AbortSignal }): Promise<ScriptChannel[]> {
    return this.http.fetchJson<ScriptChannel[]>(`${this.scriptPath(projectId, scriptName, 'channels')}`, opts);
  }

  /** Delete a script in a specific project (cross-project). */
  async deleteScript(projectId: number, scriptName: string, opts?: { signal?: AbortSignal }): Promise<void> {
    await this.http.fetchOk(`${this.scriptPath(projectId, scriptName)}`, { method: 'DELETE', signal: opts?.signal });
  }

  /** Rename a script in a specific project (cross-project). */
  async renameScript(projectId: number, scriptName: string, newName: string, opts?: { signal?: AbortSignal }): Promise<void> {
    await this.http.fetchOk(`${this.scriptPath(projectId, scriptName)}`, {
      method: 'PATCH', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ new_name: newName }), signal: opts?.signal,
    });
  }

  /** Duplicate a script within the same project. Copies versions, channels, and draft — not executions. */
  async duplicateScript(projectId: number, scriptName: string, opts?: { signal?: AbortSignal }): Promise<Script> {
    return this.http.fetchJson<Script>(`${this.scriptPath(projectId, scriptName, 'duplicate')}`, {
      method: 'POST', signal: opts?.signal,
    });
  }

  /** Duplicate an entire project with all its scripts. */
  async duplicate(id: number, opts?: { signal?: AbortSignal }): Promise<Project> {
    return this.http.fetchJson<Project>(`${this.base}/${id}/duplicate`, {
      method: 'POST', signal: opts?.signal,
    });
  }

  /** Reorder projects. Pass an array of project IDs in the desired order. */
  async reorder(order: number[], opts?: { signal?: AbortSignal }): Promise<void> {
    await this.http.fetchOk(`${this.base}/reorder`, {
      method: 'PUT', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ order }), signal: opts?.signal,
    });
  }

  /** Reorder scripts within a project. Pass an array of script IDs in the desired order. */
  async reorderScripts(projectId: number, order: number[], opts?: { signal?: AbortSignal }): Promise<void> {
    await this.http.fetchOk(`${this.base}/${projectId}/scripts/reorder`, {
      method: 'PUT', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ order }), signal: opts?.signal,
    });
  }

  /** Move a script from one project to another. Returns the updated script. */
  async moveScript(projectId: number, scriptName: string, targetProjectId: number, opts?: { signal?: AbortSignal }): Promise<Script> {
    return this.http.fetchJson<Script>(`${this.scriptPath(projectId, scriptName, 'move')}`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ target_project_id: targetProjectId }), signal: opts?.signal,
    });
  }

  /** Get cost aggregation for an arbitrary project (the project the client was
   *  constructed against is NOT used). Mirrors `executions.getProjectCost()`
   *  but takes an explicit `projectId`, so callers like Studio's Sidebar
   *  can roll up costs for every expanded project — not just the active one
   *  (#837). */
  async getCost(projectId: number, opts?: { since?: string; until?: string; signal?: AbortSignal }): Promise<ProjectCost> {
    const params = new URLSearchParams();
    if (opts?.since) params.set('since', opts.since);
    if (opts?.until) params.set('until', opts.until);
    const qs = params.toString();
    return this.http.fetchJson<ProjectCost>(
      `${this.base}/${projectId}/cost${qs ? `?${qs}` : ''}`,
      { signal: opts?.signal },
    );
  }
}
