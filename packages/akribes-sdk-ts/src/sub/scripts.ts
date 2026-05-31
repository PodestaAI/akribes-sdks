import type { HttpClient } from '../http';
import { nullOn404 } from '../http';
import type { Script, DraftResponse, PutDraftResponse, ScriptGraph } from '../types';

export class ScriptsClient {
  constructor(
    private http: HttpClient,
    private projectId: number,
  ) {}

  private path(name: string, ...segments: string[]) {
    return this.http.scriptPath(this.projectId, name, ...segments);
  }

  async list(opts?: { signal?: AbortSignal }): Promise<Script[]> {
    return (await this.http.fetchOk(
      `${this.http.getBaseUrl()}/projects/${this.projectId}/scripts`, opts,
    )).json();
  }

  async create(name: string, source: string, opts?: { signal?: AbortSignal }): Promise<Script> {
    return (await this.http.fetchOk(
      `${this.http.getBaseUrl()}/projects/${this.projectId}/scripts?name=${encodeURIComponent(name)}`,
      { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ source }), signal: opts?.signal },
    )).json();
  }

  async get(name: string, opts?: { signal?: AbortSignal }): Promise<Script | null> {
    return nullOn404(async () =>
      (await this.http.fetchOk(this.path(name), opts)).json()
    );
  }

  async rename(oldName: string, newName: string, opts?: { signal?: AbortSignal }): Promise<void> {
    await this.http.fetchOk(this.path(oldName), {
      method: 'PATCH', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ new_name: newName }), signal: opts?.signal,
    });
  }

  async delete(name: string, opts?: { signal?: AbortSignal }): Promise<void> {
    await this.http.fetchOk(this.path(name), { method: 'DELETE', signal: opts?.signal });
  }

  async getDraft(name: string, opts?: { signal?: AbortSignal }): Promise<DraftResponse | null> {
    return nullOn404(async () =>
      (await this.http.fetchOk(this.path(name, 'draft'), opts)).json()
    );
  }

  async saveDraft(name: string, source: string, opts?: { signal?: AbortSignal }): Promise<PutDraftResponse> {
    const res = await this.http.fetchOk(this.path(name, 'draft'), {
      method: 'PUT', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ source }), signal: opts?.signal,
    });
    const text = await res.text();
    if (!text) return { schema_warnings: [], inputs: [], type_defs: {} };
    return JSON.parse(text);
  }

  async getGraph(name: string, opts?: { version?: number; signal?: AbortSignal }): Promise<ScriptGraph | null> {
    const url = new URL(this.path(name, 'graph'));
    if (opts?.version !== undefined) url.searchParams.set('version', String(opts.version));
    return nullOn404(async () =>
      (await this.http.fetchOk(url.toString(), { signal: opts?.signal })).json()
    );
  }

  /**
   * Duplicate a script within this project. The server picks a copy name
   * (e.g. `foo copy`) and returns the new script. Per-project sugar over
   * `projects.duplicateScript(projectId, name)`.
   */
  async duplicate(name: string, opts?: { signal?: AbortSignal }): Promise<Script> {
    return (await this.http.fetchOk(this.path(name, 'duplicate'), {
      method: 'POST', signal: opts?.signal,
    })).json();
  }

  /**
   * Move a script to another project. Returns the moved script (now scoped
   * to the target project). Per-project sugar over
   * `projects.moveScript(projectId, name, targetProjectId)`.
   */
  async moveTo(name: string, targetProjectId: number, opts?: { signal?: AbortSignal }): Promise<Script> {
    return (await this.http.fetchOk(this.path(name, 'move'), {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ target_project_id: targetProjectId }), signal: opts?.signal,
    })).json();
  }

  /**
   * Set the sort order of scripts in this project. `order` is the list of
   * script IDs in the desired order. Per-project sugar over
   * `projects.reorderScripts(projectId, order)`.
   */
  async reorder(order: number[], opts?: { signal?: AbortSignal }): Promise<void> {
    await this.http.fetchOk(
      `${this.http.getBaseUrl()}/projects/${this.projectId}/scripts/reorder`,
      {
        method: 'PUT', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ order }), signal: opts?.signal,
      },
    );
  }
}
