/**
 * Lifecycle handle for a running workflow.
 *
 * Layer 3 on top of the raw {@link EngineEvent} stream. A `RunStream` is
 * async-iterable AND exposes:
 *
 *   - `.executionId` — resolves when the run POST returns.
 *   - `.output`      — resolves with the final `WorkflowEnd.output`, or rejects
 *                      on `Error`.
 *   - `.on.<cat>()`  — category-based callback registration (sugar over
 *                      iteration); each returns an unsubscribe function.
 *   - `.cancel()`    — stop iterating and tear down the underlying SSE.
 *
 * Internally the handle subscribes to `onScriptExecution` BEFORE calling
 * `run()` so no events are missed.
 *
 * Concurrent iteration is supported: every `for await` creates its own cursor
 * into an append-only event log, so multiple consumers observe the same
 * sequence independently.
 */

import type { EngineEvent, RunResult } from './types';
import { toWorkflowEvent, type WorkflowEvent, type RuntimeStepStatus } from './workflowEvents';

/**
 * Cumulative snapshot of a `runtime` block invocation, suitable for direct
 * rendering. Built inside {@link RunStream} by folding the per-event
 * {@link WorkflowEvent} runtime-step deltas into a per-task accumulator and
 * delivered through `.on.runtime(cb)` on every transition (Start, each
 * stdout/stderr chunk, End/Error).
 *
 * Distinct from the raw `kind: 'runtimeStep'` `WorkflowEvent` in two ways:
 *   * `stdout` and `stderr` here are the FULL accumulated text since
 *     RuntimeStart, not the single-event delta.
 *   * `runtimeName` and `language` are sticky — set once on Start and
 *     preserved across all later snapshots (the underlying delta events
 *     don't carry them).
 */
export type RuntimeStep = {
  taskName: string;
  runtimeName: string;
  language: string;
  status: RuntimeStepStatus;
  /** Cumulative stdout from RuntimeStart to "now". */
  stdout: string;
  /** Cumulative stderr from RuntimeStart to "now". */
  stderr: string;
  /** Set on the End transition; null on Start / chunks / Error. */
  exitCode: number | null;
  /** Set on the End transition; null on Start / chunks / Error. */
  durationMs: number | null;
  /** Set on the Error transition; null on Start / chunks / End. */
  errorKind: string | null;
  /** Set on the Error transition; null on Start / chunks / End. */
  errorMessage: string | null;
};

// ── Public types ────────────────────────────────────────────────────────────

export interface RunStreamCallbacks {
  /**
   * Fires per `agentChunk` event. Receives both the chunk string and the full event.
   *
   * NOTE: Callbacks fire only for events received AFTER registration. If you
   * register late, earlier chunks are not replayed. Register before awaiting
   * `executionId` or calling any method that would yield to the event loop.
   */
  output(cb: (chunk: string, evt: Extract<WorkflowEvent, { kind: 'agentChunk' }>) => void): () => void;
  /** Fires per `taskEnd`. Same late-registration caveat as `output`. */
  taskEnd(cb: (evt: Extract<WorkflowEvent, { kind: 'taskEnd' }>) => void): () => void;
  /** Fires on checkpoint/toolApproval/breakpoint. Same late-registration caveat. */
  suspend(cb: (evt: Extract<WorkflowEvent, { kind: 'checkpoint' | 'toolApproval' | 'breakpoint' }>) => void): () => void;
  /** Fires on `error`. Same late-registration caveat. */
  error(cb: (evt: Extract<WorkflowEvent, { kind: 'error' }>) => void): () => void;
  /**
   * Fires on every `runtime` block transition — Start, each stdout/stderr
   * chunk, and the terminal End or Error. Receives the cumulative
   * {@link RuntimeStep} snapshot (stdout/stderr include all chunks seen so
   * far), so consumers can render the current state of a runtime call
   * directly without re-accumulating. Same late-registration caveat: only
   * fires for events received AFTER registration.
   */
  runtime(cb: (step: RuntimeStep) => void): () => void;
  /** Fires for every event, including `other`. Same late-registration caveat. */
  any(cb: (evt: WorkflowEvent) => void): () => void;
}

/**
 * Aggregated post-execution stats. Built by walking the events seen during a
 * stream — see {@link RunStream.summary}.
 *
 * Cost-in-USD is **not** currently computed inside the SDK (no pricing table
 * is bundled). `cost` is `null` when no real usage was observed (i.e. running
 * under `MOCK_LLM=1` or before any task emitted a usage block); otherwise
 * `totalUsd` is `0` and `byModel` maps each observed model name to the total
 * of `input_tokens + output_tokens` across all of its `taskEnd` events. Phase
 * 4 adds a pricing-aware variant on the server.
 */
export interface RunSummary {
  executionId: string;
  output: unknown;
  cost: { totalUsd: number; byModel: Record<string, number> } | null;
  duration: { totalMs: number; perTaskMs: Record<string, number> };
  tasks: { passed: number; failed: number; total: number };
}

export interface RunStream extends AsyncIterable<WorkflowEvent> {
  /** Resolves once the run POST completes. Stays resolved after `cancel()`. */
  readonly executionId: Promise<string>;
  /** Resolves with `WorkflowEnd.output`, rejects on `Error` or `cancel()`. */
  readonly output: Promise<unknown>;
  /** Category-based callback registration (sugar over iteration). */
  readonly on: RunStreamCallbacks;
  /**
   * Stop iterating and cancel the underlying subscription. Rejects `.output`
   * if still pending; leaves `.executionId` alone (if the POST already
   * resolved, the id is still valid — callers may want to issue a server-side
   * cancel using it).
   */
  cancel(): void;
  /**
   * Iterate only `error` events. Wires straight to the underlying log; works
   * concurrently with the main `for await` and other filters.
   */
  errorsOnly(): AsyncIterable<Extract<WorkflowEvent, { kind: 'error' }>>;
  /**
   * Iterate only `agentChunk` events.
   */
  agentChunksOnly(): AsyncIterable<Extract<WorkflowEvent, { kind: 'agentChunk' }>>;
  /**
   * Drain the stream to terminal and return a {@link RunSummary} aggregated
   * from observed events. Resolves the same way as {@link RunStream.output} —
   * rejects on `Error` or `cancel()`.
   */
  summary(): Promise<RunSummary>;
}

// ── Dependencies injected from ExecutionsClient ─────────────────────────────

/** Just the bits of `EventsClient` that `RunStream` needs. The callback's
 *  second argument is the execution_id from the wrapping HubEvent — added so
 *  `RunStream` can drop events from a concurrent run of the same script
 *  started by another caller. Optional so older event sources / tests that
 *  emit single-arg can still wire up. */
export interface RunStreamEventsSource {
  onScriptExecution(scriptName: string, callback: (event: EngineEvent, executionId?: string) => void): () => void;
}

/** Signature matching `ExecutionsClient.run`'s opts. */
export type RunStreamOptions = {
  inputs?: Record<string, unknown>;
  channel?: string;
  triggeredBy?: string;
  signal?: AbortSignal;
  breakpointLines?: number[];
  dryRunTools?: boolean;
};

/** What the executions resource gives us to actually POST the run. */
export type RunStarter = (scriptName: string, opts?: RunStreamOptions) => Promise<RunResult>;

// ── Implementation ──────────────────────────────────────────────────────────

type Resolver<T> = { resolve: (v: T) => void; reject: (e: unknown) => void };

function deferred<T>(): { promise: Promise<T> } & Resolver<T> {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

type AnyCb = (evt: WorkflowEvent) => void;
type OutputCb = (chunk: string, evt: Extract<WorkflowEvent, { kind: 'agentChunk' }>) => void;
type TaskEndCb = (evt: Extract<WorkflowEvent, { kind: 'taskEnd' }>) => void;
type SuspendCb = (evt: Extract<WorkflowEvent, { kind: 'checkpoint' | 'toolApproval' | 'breakpoint' }>) => void;
type ErrorCb = (evt: Extract<WorkflowEvent, { kind: 'error' }>) => void;
type RuntimeCb = (step: RuntimeStep) => void;

/** Per-iterator cursor into the shared log. */
type IteratorCursor = {
  cursor: number;
  waiting: Resolver<IteratorResult<WorkflowEvent>> | null;
};

class RunStreamImpl implements RunStream {
  readonly executionId: Promise<string>;
  readonly output: Promise<unknown>;
  readonly on: RunStreamCallbacks;

  private execIdDef = deferred<string>();
  private outputDef = deferred<unknown>();

  private unsubscribe: (() => void) | null = null;

  /** Resolved once the run POST returns. Until then, events that carry
   *  an executionId can't be matched, so we hold them here and replay
   *  after the id is known. */
  private knownExecutionId: string | null = null;
  private pending: Array<{ raw: EngineEvent; eid?: string }> = [];

  /** Append-only log of every event delivered. Each iterator walks this via
   *  its own cursor, so concurrent iteration is safe. */
  private log: WorkflowEvent[] = [];
  /** Live iterator cursors — notified when a new event is appended. */
  private iterators = new Set<IteratorCursor>();
  /** Set after terminal event (`end`/`error`) or `cancel()`. */
  private finished = false;

  // Callback registries (keyed by identity so the returned unsubscribe works).
  private anyCbs = new Set<AnyCb>();
  private outputCbs = new Set<OutputCb>();
  private taskEndCbs = new Set<TaskEndCb>();
  private suspendCbs = new Set<SuspendCb>();
  private errorCbs = new Set<ErrorCb>();
  private runtimeCbs = new Set<RuntimeCb>();

  /**
   * Per-taskName running snapshots of in-flight `runtime` block invocations.
   * Mutated on each `kind: 'runtimeStep'` workflow event before invoking
   * `.on.runtime` callbacks, so the callback receives the cumulative state
   * (stdout/stderr include all chunks seen so far) rather than the delta
   * from a single chunk event. Entries are kept after the terminal
   * End/Error so a late `.on.runtime` subscriber that registers between
   * the terminal event and the workflow end still observes the final
   * state — but since callbacks only fire on subsequent transitions,
   * "late" really means "in the next runtime call".
   */
  private runtimeAccumulators = new Map<string, RuntimeStep>();

  constructor(
    scriptName: string,
    opts: RunStreamOptions | undefined,
    events: RunStreamEventsSource,
    starter: RunStarter,
  ) {
    this.executionId = this.execIdDef.promise;
    this.output = this.outputDef.promise;
    this.on = this.buildCallbacks();

    // Swallow rejections on the promises in case no one awaits them — prevents
    // an unhandled-rejection crash when the caller only iterates.
    this.output.catch(() => { /* observed elsewhere */ });
    this.executionId.catch(() => { /* observed elsewhere */ });

    // Step 1: subscribe BEFORE issuing the run POST so we don't miss early events.
    this.unsubscribe = events.onScriptExecution(scriptName, (raw, eid) => this.routeRaw(raw, eid));

    // Step 2: kick off the run. If it fails, reject executionId + output and
    // tear down iteration.
    starter(scriptName, opts).then(
      (res) => {
        this.knownExecutionId = res.execution_id;
        this.execIdDef.resolve(res.execution_id);
        // Drain pre-resolution buffer: deliver only events that match our
        // execution_id (or have no execution_id at all, e.g. legacy emitters).
        const buffered = this.pending;
        this.pending = [];
        for (const { raw, eid } of buffered) {
          if (eid && eid !== res.execution_id) continue;
          this.handleRaw(raw);
        }
      },
      (err) => {
        this.execIdDef.reject(err);
        this.outputDef.reject(err);
        this.pending = [];
        this.finish();
      },
    );
  }

  /** Filter events by execution_id (when known) before forwarding to the
   *  reducer + callbacks. Buffers events that arrive before the run POST
   *  resolves — without this, a concurrent run of the same script started
   *  by another caller could deliver its WorkflowEnd into our handle and
   *  resolve `.output` with the wrong value. */
  private routeRaw(raw: EngineEvent, eid?: string): void {
    if (this.finished) return;
    if (this.knownExecutionId == null) {
      this.pending.push({ raw, eid });
      return;
    }
    if (eid && eid !== this.knownExecutionId) return;
    this.handleRaw(raw);
  }

  private buildCallbacks(): RunStreamCallbacks {
    return {
      output: (cb) => { this.outputCbs.add(cb); return () => { this.outputCbs.delete(cb); }; },
      taskEnd: (cb) => { this.taskEndCbs.add(cb); return () => { this.taskEndCbs.delete(cb); }; },
      suspend: (cb) => { this.suspendCbs.add(cb); return () => { this.suspendCbs.delete(cb); }; },
      error: (cb) => { this.errorCbs.add(cb); return () => { this.errorCbs.delete(cb); }; },
      runtime: (cb) => { this.runtimeCbs.add(cb); return () => { this.runtimeCbs.delete(cb); }; },
      any: (cb) => { this.anyCbs.add(cb); return () => { this.anyCbs.delete(cb); }; },
    };
  }

  /**
   * Fold a per-event runtime-step delta into the per-taskName accumulator
   * and return the resulting cumulative snapshot. Called from
   * `dispatchCallbacks` before invoking `.on.runtime` subscribers.
   *
   * Stickiness rules (the delta doesn't carry runtimeName/language on
   * non-Start events, so we preserve them from the prior snapshot):
   *   * `runtimeName`/`language` are taken from the delta when non-empty
   *     (i.e. only on RuntimeStart), otherwise from the prior snapshot.
   *   * `stdout`/`stderr` append the delta to the prior cumulative text.
   *   * `status` always tracks the delta (each transition reports its
   *     own status).
   *   * `exitCode`/`durationMs` are non-null only on RuntimeEnd; we copy
   *     them through verbatim — they overwrite any prior nulls and are
   *     not "stuck" to past values because End is terminal.
   *   * `errorKind`/`errorMessage` are non-null only on RuntimeError;
   *     same overwrite semantics.
   */
  private foldRuntimeDelta(delta: Extract<WorkflowEvent, { kind: 'runtimeStep' }>): RuntimeStep {
    const prior = this.runtimeAccumulators.get(delta.taskName);
    const next: RuntimeStep = {
      taskName: delta.taskName,
      runtimeName: delta.runtimeName !== '' ? delta.runtimeName : (prior?.runtimeName ?? ''),
      language: delta.language !== '' ? delta.language : (prior?.language ?? ''),
      status: delta.status,
      stdout: (prior?.stdout ?? '') + delta.stdout,
      stderr: (prior?.stderr ?? '') + delta.stderr,
      exitCode: delta.exitCode,
      durationMs: delta.durationMs,
      errorKind: delta.errorKind,
      errorMessage: delta.errorMessage,
    };
    this.runtimeAccumulators.set(delta.taskName, next);
    return next;
  }

  private handleRaw(raw: EngineEvent): void {
    if (this.finished) return;
    const evt = toWorkflowEvent(raw);
    this.dispatchCallbacks(evt);
    this.pushEvent(evt);

    // Terminal detection.
    if (evt.kind === 'end') {
      // NB: `evt.durationMs` comes from `WorkflowEnd.duration_secs` — on some
      // transports (notably streamed events with no aggregate) it may be 0.
      this.outputDef.resolve(evt.output);
      this.finish();
    } else if (evt.kind === 'error') {
      const err = new Error(evt.message);
      (err as Error & { errorKind?: string }).errorKind = evt.errorKind;
      this.outputDef.reject(err);
      this.finish();
    }
  }

  private dispatchCallbacks(evt: WorkflowEvent): void {
    for (const cb of this.anyCbs) { try { cb(evt); } catch { /* swallow listener errors */ } }
    switch (evt.kind) {
      case 'agentChunk':
        for (const cb of this.outputCbs) { try { cb(evt.chunk, evt); } catch { /* swallow */ } }
        break;
      case 'taskEnd':
        for (const cb of this.taskEndCbs) { try { cb(evt); } catch { /* swallow */ } }
        break;
      case 'checkpoint':
      case 'toolApproval':
      case 'breakpoint':
        for (const cb of this.suspendCbs) { try { cb(evt); } catch { /* swallow */ } }
        break;
      case 'error':
        for (const cb of this.errorCbs) { try { cb(evt); } catch { /* swallow */ } }
        break;
      case 'runtimeStep': {
        // Fold the per-event delta into the per-task accumulator first so the
        // callback receives the cumulative snapshot rather than the
        // single-event delta. We always fold (even when no callbacks are
        // registered) so a late `.on.runtime` subscriber that registers on
        // the next runtime call doesn't reset its accumulator's prior
        // state mid-stream.
        const snapshot = this.foldRuntimeDelta(evt);
        for (const cb of this.runtimeCbs) { try { cb(snapshot); } catch { /* swallow */ } }
        break;
      }
      default:
        break;
    }
  }

  private pushEvent(evt: WorkflowEvent): void {
    this.log.push(evt);
    // Wake any iterators that have caught up and are parked.
    for (const it of this.iterators) {
      if (it.waiting && it.cursor < this.log.length) {
        const w = it.waiting;
        it.waiting = null;
        const value = this.log[it.cursor++] as WorkflowEvent;
        w.resolve({ value, done: false });
      }
    }
  }

  /** Mark the stream finished and tear down the SSE subscription. Any
   *  iterators parked past the end of the log get `{ done: true }`. */
  private finish(): void {
    if (this.finished) return;
    this.finished = true;
    if (this.unsubscribe) {
      try { this.unsubscribe(); } catch { /* ignore */ }
      this.unsubscribe = null;
    }
    for (const it of this.iterators) {
      if (it.waiting) {
        const w = it.waiting;
        it.waiting = null;
        w.resolve({ value: undefined, done: true });
      }
    }
  }

  cancel(): void {
    if (this.finished) return;
    // Reject output so awaiters don't hang. Leave executionId alone — if the
    // POST already resolved, the id is still valid for the caller (e.g. to
    // issue a server-side cancel). If it hasn't, the pending promise gets
    // rejected only when the POST itself fails (normal flow).
    this.outputDef.reject(new Error('RunStream cancelled'));
    this.finish();
  }

  errorsOnly(): AsyncIterable<Extract<WorkflowEvent, { kind: 'error' }>> {
    return this.filter((e): e is Extract<WorkflowEvent, { kind: 'error' }> => e.kind === 'error');
  }

  agentChunksOnly(): AsyncIterable<Extract<WorkflowEvent, { kind: 'agentChunk' }>> {
    return this.filter((e): e is Extract<WorkflowEvent, { kind: 'agentChunk' }> => e.kind === 'agentChunk');
  }

  /** Build an async-iterable that yields only events matching `pred`. Each
   *  call gets its own cursor into the shared log, so filtered iteration
   *  composes safely with the main `for await (const e of rs)` loop. */
  private filter<T extends WorkflowEvent>(pred: (e: WorkflowEvent) => e is T): AsyncIterable<T> {
    const self = this;
    return {
      [Symbol.asyncIterator](): AsyncIterator<T> {
        const inner = self[Symbol.asyncIterator]();
        return {
          async next(): Promise<IteratorResult<T>> {
            while (true) {
              const r = await inner.next();
              if (r.done) return { value: undefined, done: true };
              if (pred(r.value)) return { value: r.value, done: false };
            }
          },
          return(): Promise<IteratorResult<T>> {
            return inner.return
              ? inner.return().then(() => ({ value: undefined, done: true }))
              : Promise.resolve({ value: undefined, done: true });
          },
        };
      },
    };
  }

  async summary(): Promise<RunSummary> {
    // Drain to terminal, then aggregate. Re-uses the shared log via a fresh
    // iterator cursor so callers can also `for await` the same stream.
    const output = await this.output;
    const executionId = await this.executionId;

    let totalMs = 0;
    const perTaskMs: Record<string, number> = {};
    const tasksByName: Record<string, 'passed' | 'failed'> = {};
    const byModelTokens: Record<string, number> = {};
    let usageObserved = false;
    let mockObserved = false;

    for (const evt of this.log) {
      switch (evt.kind) {
        case 'end':
          totalMs = evt.durationMs;
          break;
        case 'taskEnd': {
          perTaskMs[evt.task] = (perTaskMs[evt.task] ?? 0) + evt.durationMs;
          // Latest variant wins — `unable` overrides a prior success on retry.
          tasksByName[evt.task] = evt.variant === 'success' ? 'passed' : 'failed';
          if (evt.usage) {
            usageObserved = true;
            if (evt.usage.provider === 'mock') mockObserved = true;
            const tokens = evt.usage.inputTokens + evt.usage.outputTokens;
            const model = evt.usage.model || 'unknown';
            byModelTokens[model] = (byModelTokens[model] ?? 0) + tokens;
          }
          break;
        }
        default:
          break;
      }
    }

    const taskNames = Object.keys(tasksByName);
    const passed = taskNames.filter((t) => tasksByName[t] === 'passed').length;
    const failed = taskNames.length - passed;

    // The SDK doesn't bundle a pricing table — we report null when we have no
    // real usage signal (mock or no taskEnd usage block). When usage is real,
    // `byModel` carries the total (input + output) token count per model so
    // callers can compute their own USD cost; `totalUsd` stays 0 for now.
    // TODO: server-side pricing-aware variant pending.
    const cost = (!usageObserved || mockObserved)
      ? null
      : { totalUsd: 0, byModel: byModelTokens };

    return {
      executionId,
      output,
      cost,
      duration: { totalMs, perTaskMs },
      tasks: { passed, failed, total: taskNames.length },
    };
  }

  [Symbol.asyncIterator](): AsyncIterator<WorkflowEvent> {
    const it: IteratorCursor = { cursor: 0, waiting: null };
    this.iterators.add(it);
    const self = this;
    return {
      next: (): Promise<IteratorResult<WorkflowEvent>> => {
        if (it.cursor < self.log.length) {
          const value = self.log[it.cursor++] as WorkflowEvent;
          return Promise.resolve({ value, done: false });
        }
        if (self.finished) {
          self.iterators.delete(it);
          return Promise.resolve({ value: undefined, done: true });
        }
        return new Promise<IteratorResult<WorkflowEvent>>((resolve, reject) => {
          it.waiting = { resolve, reject };
        });
      },
      return: (): Promise<IteratorResult<WorkflowEvent>> => {
        // Detach this iterator only — do NOT cancel the whole stream, since
        // other iterators / callback-consumers may still want events.
        if (it.waiting) {
          const w = it.waiting;
          it.waiting = null;
          w.resolve({ value: undefined, done: true });
        }
        self.iterators.delete(it);
        return Promise.resolve({ value: undefined, done: true });
      },
    };
  }
}

/**
 * Construct a {@link RunStream}. Exported so tests and advanced callers can
 * build one without going through `ExecutionsClient`.
 */
export function createRunStream(
  scriptName: string,
  opts: RunStreamOptions | undefined,
  events: RunStreamEventsSource,
  starter: RunStarter,
): RunStream {
  return new RunStreamImpl(scriptName, opts, events, starter);
}
