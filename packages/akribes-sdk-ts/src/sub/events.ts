import type { HttpClient } from '../http';
import { connectSse } from '../sse';
import type { ContractState } from './clients';
import type { HubEvent, EngineEvent } from '../types';
import { toWorkflowEvent, type WorkflowEvent } from '../workflowEvents';

type HubEventCallback = (event: HubEvent) => void;
type SseErrorCallback = (error: Error) => void;

export class EventsClient {
  private subscribers = new Set<HubEventCallback>();
  private dispose: (() => void) | null = null;
  private _onSseError: SseErrorCallback | undefined;

  constructor(
    private http: HttpClient,
    /** When set, the SSE URL includes `?project_id=<id>` so the server
     *  filters to that project's hub events. When unset (no `projectId`
     *  on the parent {@link AkribesClient}), the SDK subscribes to the
     *  global hub stream and the server scopes events by the token's
     *  reachability — used by editors that need cross-project events. */
    private projectId: number | undefined,
    private getToken: () => string | undefined,
    private contractState?: ContractState,
  ) {}

  /** Register an optional callback to be notified of SSE connection errors.
   * Without this, SSE errors are silently swallowed (the stream auto-reconnects). */
  set onSseError(cb: SseErrorCallback | undefined) {
    this._onSseError = cb;
  }

  /** Attach a {@link ContractState} after construction. The parent client
   *  initialises the events sub-client before the project-scoped
   *  `ClientsClient` (which owns the contract state) exists, so this
   *  binding happens post-init. No-op for non-project-scoped clients. */
  setContractState(state: ContractState) {
    this.contractState = state;
  }

  private ensureConnection() {
    if (this.dispose) return;

    // Token is delivered exclusively via the Authorization header passed
    // through `connectSse({ headers })` below. Older revisions appended a
    // `?token=` query string here so EventSource (which can't set headers)
    // would still authenticate; that put long-lived service-token secrets
    // into reverse-proxy access logs and OTel `http.url` span attributes
    // for any backend SDK consumer like puto. `sse.ts` now skips
    // EventSource whenever an Authorization header is present and uses
    // the fetch-fallback streaming SSE path instead.
    const buildUrl = () => {
      const url = new URL(`${this.http.getBaseUrl()}/events`);
      if (this.projectId !== undefined) {
        url.searchParams.set('project_id', this.projectId.toString());
      }
      return url.toString();
    };

    this.dispose = connectSse({
      url: buildUrl,
      headers: { ...this.http.authHeaders(), ...this.http.traceHeaders() },
      onMessage: (msg) => {
        if (msg.event !== 'batch' && msg.event !== '') return;
        try {
          const batch: HubEvent[] = JSON.parse(msg.data);
          for (const evt of batch) {
            for (const cb of this.subscribers) cb(evt);
          }
        } catch { /* malformed JSON, skip */ }
      },
      onError: (err) => {
        this._onSseError?.(err);
      },
    });
  }

  private removeSubscriber(cb: HubEventCallback) {
    this.subscribers.delete(cb);
    if (this.subscribers.size === 0 && this.dispose) {
      this.dispose();
      this.dispose = null;
    }
  }

  /** Subscribe to all hub events. Returns an unsubscribe function. */
  onAll(callback: HubEventCallback): () => void {
    this.subscribers.add(callback);
    this.ensureConnection();
    return () => this.removeSubscriber(callback);
  }

  /** Subscribe to execution events for a specific script. The callback's
   *  second argument is the execution_id from the wrapping HubEvent so
   *  callers can demultiplex multiple concurrent runs of the same script. */
  onScriptExecution(scriptName: string, callback: (event: EngineEvent, executionId?: string) => void): () => void {
    const wrapper: HubEventCallback = (hubEvt) => {
      if (hubEvt.type === 'Execution' && hubEvt.payload.script_name === scriptName) {
        callback(hubEvt.payload.event, hubEvt.payload.execution_id);
      }
    };
    this.subscribers.add(wrapper);
    this.ensureConnection();
    return () => this.removeSubscriber(wrapper);
  }

  /** Subscribe to execution events for a specific script, normalised through
   *  {@link toWorkflowEvent} so the callback receives the typed
   *  {@link WorkflowEvent} variants (`{ kind: 'taskEnd', task, … }`) instead
   *  of raw `{ type, payload }` shapes (#1239 — mirrors Python
   *  `events.typed_engine_events`).
   *
   *  Use this when you want the same ergonomics as `RunStream`'s typed
   *  iterator on a free-standing subscription (e.g. attaching to a run
   *  started by someone else; multi-tab Studio editors observing the
   *  same execution).
   */
  onScriptExecutionTyped(
    scriptName: string,
    callback: (event: WorkflowEvent, executionId?: string) => void,
  ): () => void {
    return this.onScriptExecution(scriptName, (raw, executionId) => {
      callback(toWorkflowEvent(raw), executionId);
    });
  }

  /** Subscribe to script version/channel changes. */
  onScriptChange(scriptName: string, callback: (versionId: number, channel?: string) => void): () => void {
    const wrapper: HubEventCallback = (hubEvt) => {
      if (
        hubEvt.type === 'Registry' &&
        hubEvt.payload.type === 'ScriptUpdated' &&
        hubEvt.payload.payload.script_name === scriptName
      ) {
        callback(hubEvt.payload.payload.version_id, hubEvt.payload.payload.channel ?? undefined);
      }
    };
    this.subscribers.add(wrapper);
    this.ensureConnection();
    return () => this.removeSubscriber(wrapper);
  }

  /** Subscribe to schema changes for a specific script.
   * Marks the script as "broken" in the contract state so pre-dispatch
   * validation in run() will throw ScriptSchemaChangedError. */
  onScriptSchemaChange(scriptName: string, callback: (versionId: number, channel?: string) => void): () => void {
    const wrapper: HubEventCallback = (hubEvt) => {
      if (
        hubEvt.type === 'Registry' &&
        hubEvt.payload.type === 'ScriptUpdated' &&
        hubEvt.payload.payload.script_name === scriptName
      ) {
        // Mark as broken in contract state (if init() was called)
        this.contractState?.brokenScripts.add(scriptName);
        callback(hubEvt.payload.payload.version_id, hubEvt.payload.payload.channel ?? undefined);
      }
    };
    this.subscribers.add(wrapper);
    this.ensureConnection();
    return () => this.removeSubscriber(wrapper);
  }

  /** Close the SSE connection and remove all subscribers. */
  destroy() {
    this.subscribers.clear();
    if (this.dispose) {
      this.dispose();
      this.dispose = null;
    }
  }
}
