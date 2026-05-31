import type { HttpClient } from '../http';
import type { ScriptChannel } from '../types';

export class ChannelsClient {
  constructor(
    private http: HttpClient,
    private projectId: number,
  ) {}

  private path(scriptName: string, ...segments: string[]) {
    return this.http.scriptPath(this.projectId, scriptName, ...segments);
  }

  async list(scriptName: string, opts?: { signal?: AbortSignal }): Promise<ScriptChannel[]> {
    return (await this.http.fetchOk(this.path(scriptName, 'channels'), opts)).json();
  }

  async create(scriptName: string, channelName: string, opts?: { signal?: AbortSignal }): Promise<ScriptChannel> {
    return (await this.http.fetchOk(this.path(scriptName, 'channels'), {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name: channelName }), signal: opts?.signal,
    })).json();
  }

  async delete(scriptName: string, channelName: string, opts?: { signal?: AbortSignal }): Promise<void> {
    await this.http.fetchOk(this.path(scriptName, 'channels', channelName), {
      method: 'DELETE', signal: opts?.signal,
    });
  }

  async move(scriptName: string, channelName: string, versionId: number, opts?: { force?: boolean; signal?: AbortSignal }): Promise<void> {
    await this.http.fetchOk(this.path(scriptName, 'channels', channelName), {
      method: 'PATCH', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ version_id: versionId, ...(opts?.force != null && { force: opts.force }) }), signal: opts?.signal,
    });
  }
}
