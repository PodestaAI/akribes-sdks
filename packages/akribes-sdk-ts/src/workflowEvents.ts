/**
 * Typed, normalized workflow events.
 *
 * Layer 2 on top of the raw {@link EngineEvent} wire shape. Where the raw
 * `{ type, payload }` exists for full fidelity, `WorkflowEvent` is a
 * discriminated union that covers the high-traffic variants and funnels
 * everything else through an `other` catch-all.
 *
 * Use {@link toWorkflowEvent} to convert raw engine events to the typed shape,
 * and {@link categoryOf} to bucket events for callback-style registries like
 * {@link RunStream}'s `.on.<category>()` surface.
 */

import type {
  EngineEvent,
  SuspendTrigger,
  TaskEndVariant,
  UnableRecord,
  ValidationErrorWire,
} from './types';

/** Per-LLM-call token usage as emitted on `TaskEnd`. */
export type TokenUsage = {
  inputTokens: number;
  outputTokens: number;
  cachedInputTokens: number;
  /** Cache-creation (write) tokens. Anthropic-only today; billed by the
   *  server at 1.25x base input (5-minute TTL). OpenAI/Gemini emit 0. */
  cacheWriteInputTokens: number;
  model: string;
  provider: string;
};

/** Normalized error classification mirrored from the server's `ErrorKind`. */
export type ErrorKind =
  | 'RateLimit'
  | 'AuthError'
  | 'TokenLimit'
  // #1296: legacy umbrella retained for back-compat; new producers emit
  // one of the four status-specific kinds below.
  | 'ServerError'
  | 'ServerError500'
  | 'BadGateway502'
  | 'ServiceUnavailable503'
  | 'GatewayTimeout504'
  | 'NetworkError'
  | 'ParseError'
  | 'Cancelled'
  | 'Timeout'
  | 'ScriptError'
  | 'AuthorRaise'
  | 'ScriptDepthExceeded'
  | 'Panic'
  | 'Internal';

/**
 * Stable, fine-grained error identifier mirrored from the server's
 * `ErrorCode`. Wire form is `AKRIBES-E-<UPPER-KEBAB>`. Branch on this for
 * retry/UI logic — it's the contract every server release upholds.
 *
 * Unknown codes from a newer server are tolerated as `'AKRIBES-E-OTHER'`
 * by the normalizer so SDK consumers never crash on a fresh release.
 */
export type ErrorCode = string;

/**
 * What the caller should do in response to an error. Derived server-side
 * from {@link ErrorKind}; SDK consumers can branch on the string without
 * walking a switch over every kind.
 */
export type SuggestedAction =
  | 'retry'
  | 'fix-config'
  | 'fix-script'
  | 'fix-input'
  | 'handle-author-failure'
  | 'none'
  | 'report';

/** Where in the workflow an error originated. All fields optional. */
export type ErrorSource = {
  task?: string;
  agent?: string;
  provider?: string;
  model?: string;
  toolRef?: string;
  script?: string;
  line?: number;
};

/**
 * High-level event category. Mirrors the callback registry surface exposed by
 * {@link RunStream}'s `.on.<category>()` methods.
 */
export type EventCategory = 'progress' | 'output' | 'tool' | 'suspend' | 'error' | 'runtime' | 'other';

/**
 * One ancestor frame on a flattened `SubScript` envelope (issue #993).
 *
 * Before #993 the engine wrapped each call-stack level in its own
 * `SubScript` envelope, producing a recursive `Box<EngineEvent>` chain
 * whose serialized size grew O(depth). The post-#993 wire shape carries
 * the ancestor chain via `parentPath: SubScriptFrame[]` (ordered
 * outermost → immediate parent) and reserves the envelope's own `child`
 * for the innermost leaf event.
 *
 * Field semantics mirror the top-level `SubScript` fields — see the
 * `WorkflowEvent` `subScript` arm.
 */
export type SubScriptFrame = {
  scriptName: string;
  parentTask: string;
  parentNodeId: number | null;
  attempt: number | null;
};

/**
 * Walk a (possibly legacy-nested) `SubScript` payload into its flat
 * components: the innermost `script_name` / `parent_task`, an ancestor
 * chain ordered outermost → immediate parent, and the recovered leaf
 * event. Idempotent on already-flat payloads (the engine emits the flat
 * shape natively post-#993).
 */
function flattenSubScriptPayload(payload: Record<string, unknown>): {
  scriptName: string;
  parentTask: string;
  parentPath: SubScriptFrame[];
  leaf: unknown;
} {
  let scriptName = typeof payload.script_name === 'string' ? payload.script_name : '';
  let parentTask = typeof payload.parent_task === 'string' ? payload.parent_task : '';
  let parentNodeId = typeof payload.parent_node_id === 'number' ? payload.parent_node_id : null;
  let attempt = typeof payload.attempt === 'number' ? payload.attempt : null;
  // Start with whatever the payload already carries in parent_path.
  const parentPath: SubScriptFrame[] = Array.isArray(payload.parent_path)
    ? payload.parent_path
        .filter((f): f is Record<string, unknown> => !!f && typeof f === 'object')
        .map((f) => ({
          scriptName: typeof f.script_name === 'string' ? f.script_name : '',
          parentTask: typeof f.parent_task === 'string' ? f.parent_task : '',
          parentNodeId: typeof f.parent_node_id === 'number' ? f.parent_node_id : null,
          attempt: typeof f.attempt === 'number' ? f.attempt : null,
        }))
    : [];

  let cur: unknown = payload.child;
  // Walk through nested SubScripts (legacy emissions). Hard cap so a
  // malformed cycle can't hang the reducer — chains beyond 64 levels
  // would already have been pathological under the recursive shape.
  for (let depth = 0; depth < 64; depth += 1) {
    if (!cur || typeof cur !== 'object') break;
    const evt = cur as { type?: unknown; payload?: unknown };
    if (evt.type !== 'SubScript') break;
    if (!evt.payload || typeof evt.payload !== 'object') break;
    const inner = evt.payload as Record<string, unknown>;
    // Promote the current frame onto the path — it becomes an ancestor
    // of whatever lives inside this nested envelope. Any frames the
    // nested envelope already carried slot in between.
    if (Array.isArray(inner.parent_path)) {
      for (const f of inner.parent_path) {
        if (!f || typeof f !== 'object') continue;
        const ff = f as Record<string, unknown>;
        parentPath.push({
          scriptName: typeof ff.script_name === 'string' ? ff.script_name : '',
          parentTask: typeof ff.parent_task === 'string' ? ff.parent_task : '',
          parentNodeId: typeof ff.parent_node_id === 'number' ? ff.parent_node_id : null,
          attempt: typeof ff.attempt === 'number' ? ff.attempt : null,
        });
      }
    }
    parentPath.push({ scriptName, parentTask, parentNodeId, attempt });
    scriptName = typeof inner.script_name === 'string' ? inner.script_name : '';
    parentTask = typeof inner.parent_task === 'string' ? inner.parent_task : '';
    parentNodeId = typeof inner.parent_node_id === 'number' ? inner.parent_node_id : null;
    attempt = typeof inner.attempt === 'number' ? inner.attempt : null;
    cur = inner.child;
  }
  return { scriptName, parentTask, parentPath, leaf: cur };
}

/**
 * Runtime-step status mirrored from the spec. `running` covers Start +
 * every streaming chunk; `completed` is the terminal RuntimeEnd
 * (regardless of exit_code — non-zero is still a clean exit); `error`
 * is the terminal RuntimeError envelope.
 */
export type RuntimeStepStatus = 'running' | 'completed' | 'error';

/**
 * Aggregate token / cost rollup on `WorkflowEnd` (issue #1173). Mirrors
 * the Rust `WorkflowTotals` struct. All fields default to zero on
 * legacy bare-value wire shape and on workflows that ran no `TaskEnd`s.
 */
export type WorkflowTotals = {
  totalInputTokens: number;
  totalOutputTokens: number;
  totalCachedInputTokens: number;
  totalThinkingTokens: number;
  totalToolTokens: number;
  totalCostUsd: number;
  taskCount: number;
};

/** All-zero totals — used as the default for legacy wire payloads. */
const EMPTY_WORKFLOW_TOTALS: WorkflowTotals = {
  totalInputTokens: 0,
  totalOutputTokens: 0,
  totalCachedInputTokens: 0,
  totalThinkingTokens: 0,
  totalToolTokens: 0,
  totalCostUsd: 0,
  taskCount: 0,
};

/**
 * Parse a `WorkflowEnd` payload, accepting both the new (#1173) shape
 * (`{ value, total_input_tokens, ... }`) and the legacy bare-value shape
 * (`<output>`). Disambiguation: an object with both a `value` key and
 * any `total_*`/`task_count` key is interpreted as the new shape.
 */
function parseWorkflowEndPayload(payload: unknown): { output: unknown; totals: WorkflowTotals } {
  const AGG_KEYS = [
    'total_input_tokens',
    'total_output_tokens',
    'total_cached_input_tokens',
    'total_thinking_tokens',
    'total_tool_tokens',
    'total_cost_usd',
    'task_count',
  ];
  if (isRecord(payload)) {
    const hasValue = 'value' in payload;
    const hasAnyAgg = AGG_KEYS.some((k) => k in payload);
    if (hasValue && hasAnyAgg) {
      const totals: WorkflowTotals = {
        totalInputTokens: typeof payload.total_input_tokens === 'number' ? payload.total_input_tokens : 0,
        totalOutputTokens: typeof payload.total_output_tokens === 'number' ? payload.total_output_tokens : 0,
        totalCachedInputTokens: typeof payload.total_cached_input_tokens === 'number' ? payload.total_cached_input_tokens : 0,
        totalThinkingTokens: typeof payload.total_thinking_tokens === 'number' ? payload.total_thinking_tokens : 0,
        totalToolTokens: typeof payload.total_tool_tokens === 'number' ? payload.total_tool_tokens : 0,
        totalCostUsd: typeof payload.total_cost_usd === 'number' ? payload.total_cost_usd : 0,
        taskCount: typeof payload.task_count === 'number' ? payload.task_count : 0,
      };
      return { output: payload.value, totals };
    }
  }
  // Legacy bare-value form: the payload IS the workflow output.
  return { output: payload, totals: { ...EMPTY_WORKFLOW_TOTALS } };
}

/**
 * Discriminated union covering the high-traffic engine events with an `other`
 * catch-all for variants the SDK doesn't normalize (StateUpdate, Log, Node*,
 * Resumed, BreakpointResumed, McpServer*, TaskPrompt, Verification*).
 *
 * `durationMs` fields are normalized from the core `{ secs, nanos }` wire
 * format to milliseconds via `secs * 1000 + round(nanos / 1_000_000)`.
 */
export type WorkflowEvent =
  | { kind: 'start'; totalTasks: number }
  | {
      kind: 'end';
      output: unknown;
      durationMs: number;
      /**
       * Aggregate token + cost rollup across every `TaskEnd` in the
       * workflow scope (issue #1173). Always populated; zero on legacy
       * bare-value wire shape (`payload` is the raw output value, no
       * aggregates were emitted) and on workflows that ran no
       * `TaskEnd`s.
       */
      totals: WorkflowTotals;
    }
  | { kind: 'taskStart'; task: string; onError: string | null }
  | { kind: 'taskEnd'; task: string; output: unknown; durationMs: number; usage: TokenUsage | null; variant: TaskEndVariant }
  | { kind: 'agentChunk'; task: string; agent: string | null; taskId: string; chunk: string }
  | { kind: 'toolCallStart'; task: string; tool: string; server: string; input: unknown }
  | { kind: 'toolCallEnd'; task: string; tool: string; output: unknown; durationMs: number }
  | { kind: 'checkpoint'; name: string; token: string; prompt: string; schema: unknown; timeoutSecs: number | null; trigger: SuspendTrigger }
  | { kind: 'toolApproval'; token: string; toolRef: string; args: unknown; executionId: string | null; nodeId: number | null }
  | { kind: 'breakpoint'; token: string; nodeId: number; env: Record<string, unknown> }
  | {
      kind: 'error';
      message: string;
      errorKind: ErrorKind;
      /** Stable AKRIBES-E-XXX identifier; branch on this for retry/UI logic. */
      code: ErrorCode | null;
      /** User-facing single-paragraph summary; render verbatim. */
      userMessage: string;
      /** Provider's suggested wait (ms) before retry, when known. */
      retryAfterMs: number | null;
      /** Suggested action keyword derived from `errorKind`. */
      suggestedAction: SuggestedAction;
      /** True when retrying as-is may succeed (transient kinds + retry hint). */
      retryable: boolean;
      /** Where in the workflow the error originated. */
      source: ErrorSource;
    }
  /**
   * Cross-script `call(...)` envelope (akribes-core PR #360 / #377).
   *
   * Wraps a single inner event from a child script invoked via the
   * script-composition primitive. `scriptName` is the called script's name
   * (no version pinning surfaced here today — the engine doesn't expose
   * `channel_or_version` on the envelope), `parentTask` is the variable name
   * on the parent side that received the call result. `child` is the typed
   * inner event already normalized through {@link toWorkflowEvent}, so
   * consumers can recursively inspect / render the sub-stream without
   * re-implementing wire parsing.
   *
   * The envelope itself carries no inputs/output/cost — those have to be
   * reconstructed by accumulating the wrapped child stream:
   *   * inputs → child `StateUpdate` events at the start of the sub-stream
   *   * output → the sub-stream's terminal `WorkflowEnd`
   *   * tokens / usage → child `TaskEnd` events' `usage` field
   * See `reduceExecutionEvent` for the canonical accumulator.
   */
  | {
      kind: 'validationFailure';
      taskName: string;
      /** 1-indexed attempt number. */
      attempt: number;
      modelResponse: string;
      missingFields: string[];
      extraFields: string[];
      typeErrors: string[];
      stopReason: string | null;
    }
  | {
      kind: 'subScript';
      scriptName: string;
      parentTask: string;
      /**
       * Ancestor chain from outermost to immediate parent (issue #993).
       * Empty when this sub-script is a direct child of the top-level
       * workflow. `child` is always a leaf event in the new flat wire
       * shape; legacy nested emissions are flattened on the read path.
       */
      parentPath: SubScriptFrame[];
      child: WorkflowEvent;
    }
  /**
   * Per-event snapshot of a `runtime` block invocation. Emitted once per raw
   * `RuntimeStart`/`RuntimeStdout`/`RuntimeStderr`/`RuntimeEnd`/`RuntimeError`
   * engine event, so a single runtime call typically maps to one Start +
   * many Stdout/Stderr deltas + one terminal event. Each snapshot carries
   * ONLY the data tied to its triggering event: the `stdout`/`stderr`
   * strings hold the delta from a single chunk event, not the running
   * total. Consumers that want the cumulative accumulated text should
   * subscribe via {@link RunStream}'s `.on.runtime((step) => ...)`, which
   * folds successive deltas into a running snapshot before invoking the
   * callback.
   *
   * Status transitions: `RuntimeStart` → `'running'`; `RuntimeStdout` /
   * `RuntimeStderr` → `'running'`; `RuntimeEnd` → `'completed'`;
   * `RuntimeError` → `'error'`.
   *
   * Fields populated on Start: `runtimeName`, `language` (both empty on
   * later events). Populated on End: `exitCode`, `durationMs`. Populated
   * on Error: `errorKind`, `errorMessage`.
   */
  | {
      kind: 'runtimeStep';
      taskName: string;
      /** Declared runtime block name (e.g. `run_python`). Populated on the
       *  Start event; empty string on subsequent events. */
      runtimeName: string;
      /** Lowercase language token (`python` / `bash` / etc.). Populated on
       *  the Start event; empty string on subsequent events. */
      language: string;
      status: RuntimeStepStatus;
      /** stdout delta from a single chunk event; empty on Start / End /
       *  Error. Use `.on.runtime(cb)` to receive the cumulative snapshot. */
      stdout: string;
      /** stderr delta from a single chunk event; empty on Start / End /
       *  Error. Use `.on.runtime(cb)` to receive the cumulative snapshot. */
      stderr: string;
      /** Set ONLY on the End event; null on Start / chunks / Error. */
      exitCode: number | null;
      /** Set ONLY on the End event; null on Start / chunks / Error. */
      durationMs: number | null;
      /** Set ONLY on the Error event; null on Start / chunks / End. */
      errorKind: string | null;
      /** Set ONLY on the Error event; null on Start / chunks / End. */
      errorMessage: string | null;
    }
  /** Any variant the SDK doesn't normalize — the original wire name is preserved in `typeName`. */
  | { kind: 'other'; typeName: string; payload: unknown };

/** Return the high-level category of a typed {@link WorkflowEvent}. */
export function categoryOf(evt: WorkflowEvent): EventCategory {
  switch (evt.kind) {
    case 'start':
    case 'end':
    case 'taskStart':
    case 'taskEnd':
      return 'progress';
    case 'agentChunk':
      return 'output';
    case 'toolCallStart':
    case 'toolCallEnd':
      return 'tool';
    case 'checkpoint':
    case 'toolApproval':
    case 'breakpoint':
      return 'suspend';
    case 'error':
      return 'error';
    case 'validationFailure':
      return 'output';
    case 'subScript':
      // A sub-script envelope is a forwarder; categorize by the inner event
      // so existing `.on.<category>()` subscribers see chained-pipeline
      // events under the same bucket they'd already receive direct events.
      return categoryOf(evt.child);
    case 'runtimeStep':
      return 'runtime';
    case 'other':
      return 'other';
  }
}

// ── Internal helpers ────────────────────────────────────────────────────────

type Duration = { secs: number; nanos: number };

function isDuration(v: unknown): v is Duration {
  return (
    typeof v === 'object' && v !== null &&
    typeof (v as { secs?: unknown }).secs === 'number' &&
    typeof (v as { nanos?: unknown }).nanos === 'number'
  );
}

function toMs(d: unknown): number {
  if (!isDuration(d)) return 0;
  return d.secs * 1000 + Math.round(d.nanos / 1_000_000);
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function normalizeUsage(raw: unknown): TokenUsage | null {
  if (!isRecord(raw)) return null;
  const inputTokens = typeof raw.input_tokens === 'number' ? raw.input_tokens : 0;
  const outputTokens = typeof raw.output_tokens === 'number' ? raw.output_tokens : 0;
  const cachedInputTokens = typeof raw.cached_input_tokens === 'number' ? raw.cached_input_tokens : 0;
  // Older servers may not emit this field — default to 0 to preserve
  // wire-format compatibility. Anthropic on current servers emits non-zero
  // when the call involves cache creation.
  const cacheWriteInputTokens = typeof raw.cache_write_input_tokens === 'number' ? raw.cache_write_input_tokens : 0;
  const model = typeof raw.model === 'string' ? raw.model : '';
  const provider = typeof raw.provider === 'string' ? raw.provider : '';
  return { inputTokens, outputTokens, cachedInputTokens, cacheWriteInputTokens, model, provider };
}

function normalizeValidationErrors(raw: unknown): ValidationErrorWire[] {
  if (!Array.isArray(raw)) return [];
  const out: ValidationErrorWire[] = [];
  for (const item of raw) {
    if (!isRecord(item)) continue;
    const stage = typeof item.stage === 'string' ? item.stage : '';
    const message = typeof item.message === 'string' ? item.message : '';
    const path = typeof item.path === 'string' ? item.path : null;
    out.push({ stage, message, path });
  }
  return out;
}

function normalizeUnableRecord(raw: unknown): UnableRecord {
  if (!isRecord(raw)) return { reason: '', missing: [], category: '' };
  const reason = typeof raw.reason === 'string' ? raw.reason : '';
  const category = typeof raw.category === 'string' ? raw.category : '';
  const missing = Array.isArray(raw.missing)
    ? raw.missing.filter((m): m is string => typeof m === 'string')
    : [];
  return { reason, missing, category };
}

/**
 * Normalize a serde-tagged `SuspendTrigger` wire payload (snake_case) into
 * the SDK's camelCase {@link SuspendTrigger}. Unknown `kind` values are
 * forwarded verbatim under the catch-all arm so future server versions
 * don't crash older SDKs.
 *
 * Missing / malformed input defaults to `DagPosition` — matching the
 * `#[serde(default)]` on the Rust side.
 */
export function normalizeSuspendTrigger(raw: unknown): SuspendTrigger {
  if (!isRecord(raw) || typeof raw.kind !== 'string') {
    return { kind: 'DagPosition' };
  }
  const kind = raw.kind;
  switch (kind) {
    case 'DagPosition':
      return { kind: 'DagPosition' };
    case 'ValidationExhausted': {
      const taskName = typeof raw.task_name === 'string' ? raw.task_name : '';
      const retryCount = typeof raw.retry_count === 'number' ? raw.retry_count : 0;
      const lastAttempt = typeof raw.last_attempt === 'string' ? raw.last_attempt : '';
      const validationErrors = normalizeValidationErrors(raw.validation_errors);
      return { kind: 'ValidationExhausted', taskName, retryCount, lastAttempt, validationErrors };
    }
    case 'AgentUnable': {
      const taskName = typeof raw.task_name === 'string' ? raw.task_name : '';
      const unable = normalizeUnableRecord(raw.unable);
      return { kind: 'AgentUnable', taskName, unable };
    }
    default:
      // Forward-compat: unknown discriminants pass through under the
      // catch-all arm — raw fields (snake_case, verbatim) live under `raw`
      // so callers can opt in without dropping data.
      return { kind, raw: { ...raw } };
  }
}

function normalizeErrorKind(raw: unknown): ErrorKind {
  const known: readonly ErrorKind[] = [
    'RateLimit', 'AuthError', 'TokenLimit',
    // #1296: legacy umbrella + new status-specific kinds.
    'ServerError', 'ServerError500', 'BadGateway502', 'ServiceUnavailable503', 'GatewayTimeout504',
    'NetworkError', 'ParseError', 'Cancelled', 'Timeout', 'ScriptError',
    'AuthorRaise', 'ScriptDepthExceeded', 'Panic', 'Internal',
  ];
  if (typeof raw === 'string' && (known as readonly string[]).includes(raw)) return raw as ErrorKind;
  return 'ServerError';
}

/**
 * `ErrorKind` is the bucket; this maps each bucket to the action a client
 * should take. Mirrors the server's `SuggestedAction` derivation so the
 * SDK never needs to ship a second copy of the policy.
 */
function suggestedActionFor(kind: ErrorKind): SuggestedAction {
  switch (kind) {
    case 'RateLimit':
    case 'ServerError':
    // #1296: status-specific 5xx variants all share the umbrella `retry`
    // suggestion; per-variant backoff comes from `recommendedBackoffMs`.
    case 'ServerError500':
    case 'BadGateway502':
    case 'ServiceUnavailable503':
    case 'GatewayTimeout504':
    case 'NetworkError':
      return 'retry';
    case 'AuthError':
      return 'fix-config';
    case 'TokenLimit':
    case 'Timeout':
      return 'fix-input';
    case 'ScriptError':
    case 'ScriptDepthExceeded':
    case 'ParseError':
      return 'fix-script';
    case 'AuthorRaise':
      return 'handle-author-failure';
    case 'Cancelled':
      return 'none';
    case 'Panic':
    case 'Internal':
      return 'report';
    default: {
      const _exhaustive: never = kind;
      void _exhaustive;
      return 'report';
    }
  }
}

const TRANSIENT_KINDS: readonly ErrorKind[] = [
  'RateLimit', 'ServerError', 'NetworkError',
  // #1296: status-specific 5xx variants — all transient with per-status backoff.
  'ServerError500', 'BadGateway502', 'ServiceUnavailable503', 'GatewayTimeout504',
];

/**
 * Normalize one of the 5 raw `Runtime*` engine events into a `runtimeStep`
 * `WorkflowEvent` snapshot. Each emission is a single-event DELTA — not a
 * running total — matching the contract documented on the `runtimeStep`
 * union arm in {@link WorkflowEvent}. The `RunStream` accumulator turns
 * deltas into cumulative snapshots for `.on.runtime(cb)` subscribers.
 *
 * Field semantics by event type:
 *   * `RuntimeStart`:  populates `runtimeName` + `language`; status='running'.
 *   * `RuntimeStdout`: populates `stdout` with the chunk; status='running'.
 *   * `RuntimeStderr`: populates `stderr` with the chunk; status='running'.
 *   * `RuntimeEnd`:    populates `exitCode` + `durationMs`; status='completed'.
 *   * `RuntimeError`:  populates `errorKind` + `errorMessage`; status='error'.
 *
 * Malformed payloads degrade to an empty-field snapshot at the same status
 * (rather than throwing) so a partial server bug doesn't crash the SDK.
 */
function normalizeRuntimeEvent(
  type: 'RuntimeStart' | 'RuntimeStdout' | 'RuntimeStderr' | 'RuntimeEnd' | 'RuntimeError',
  payload: unknown,
): Extract<WorkflowEvent, { kind: 'runtimeStep' }> {
  const p = isRecord(payload) ? payload : {};
  const taskName = typeof p.task_name === 'string' ? p.task_name : '';

  // Start with a fully-defaulted snapshot, then overlay only the fields the
  // event's type implies. Keeps each arm to two lines of meaningful diff.
  const base: Extract<WorkflowEvent, { kind: 'runtimeStep' }> = {
    kind: 'runtimeStep',
    taskName,
    runtimeName: '',
    language: '',
    status: 'running',
    stdout: '',
    stderr: '',
    exitCode: null,
    durationMs: null,
    errorKind: null,
    errorMessage: null,
  };

  switch (type) {
    case 'RuntimeStart':
      return {
        ...base,
        runtimeName: typeof p.runtime_name === 'string' ? p.runtime_name : '',
        language: typeof p.language === 'string' ? p.language : '',
      };
    case 'RuntimeStdout':
      return { ...base, stdout: typeof p.chunk === 'string' ? p.chunk : '' };
    case 'RuntimeStderr':
      return { ...base, stderr: typeof p.chunk === 'string' ? p.chunk : '' };
    case 'RuntimeEnd':
      // Terminal-success envelope — `completed` even when exit_code != 0
      // (non-zero is still a clean exit from the sandbox's perspective).
      return {
        ...base,
        status: 'completed',
        exitCode: typeof p.exit_code === 'number' ? p.exit_code : 0,
        durationMs: typeof p.duration_ms === 'number' ? p.duration_ms : 0,
      };
    case 'RuntimeError':
      // `kind` mirrors the Rust `RuntimeError` enum
      // (`Timeout`/`OomKilled`/`SandboxUnavailable`/`Internal`/
      // `NotConfigured`) — kept as a plain string so future variants pass
      // through without an SDK release.
      return {
        ...base,
        status: 'error',
        errorKind: typeof p.kind === 'string' ? p.kind : '',
        errorMessage: typeof p.message === 'string' ? p.message : '',
      };
  }
}

function normalizeErrorSource(raw: unknown): ErrorSource {
  if (!isRecord(raw)) return {};
  const out: ErrorSource = {};
  if (typeof raw.task === 'string') out.task = raw.task;
  if (typeof raw.agent === 'string') out.agent = raw.agent;
  if (typeof raw.provider === 'string') out.provider = raw.provider;
  if (typeof raw.model === 'string') out.model = raw.model;
  if (typeof raw.tool_ref === 'string') out.toolRef = raw.tool_ref;
  if (typeof raw.script === 'string') out.script = raw.script;
  if (typeof raw.line === 'number') out.line = raw.line;
  return out;
}

/**
 * Convert a raw {@link EngineEvent} to a typed {@link WorkflowEvent}. Unknown
 * variants are funneled through `{ kind: 'other', typeName, payload }`
 * preserving the original wire name.
 */
export function toWorkflowEvent(raw: EngineEvent): WorkflowEvent {
  const { type, payload } = raw;
  switch (type) {
    case 'WorkflowStart': {
      // payload: usize (total tasks)
      const totalTasks = typeof payload === 'number' ? payload : 0;
      return { kind: 'start', totalTasks };
    }
    case 'WorkflowEnd': {
      // Issue #1173: payload is now `{value, total_input_tokens, ...}`.
      // Legacy emissions (pre-#1173) put the bare output value directly
      // under `payload`. Detect the new shape by the presence of a
      // `value` key plus at least one `total_*` key; otherwise treat
      // `payload` itself as the output value and default totals to
      // zero. Mirrors the Rust `WorkflowEndPayload::Deserialize` rule.
      const { output, totals } = parseWorkflowEndPayload(payload);
      return { kind: 'end', output, durationMs: 0, totals };
    }
    case 'TaskStart': {
      // payload: [name, on_error]
      if (Array.isArray(payload)) {
        const task = typeof payload[0] === 'string' ? payload[0] : '';
        const onError = typeof payload[1] === 'string' ? payload[1] : null;
        return { kind: 'taskStart', task, onError };
      }
      return { kind: 'taskStart', task: '', onError: null };
    }
    case 'TaskEnd': {
      // payload is a struct variant: { task, on_error_label, value, duration,
      // attempt, usage, variant, ... }. Historical tuple shape is also
      // accepted for back-compat with older wire captures.
      if (isRecord(payload)) {
        const task = typeof payload.task === 'string' ? payload.task : '';
        const output = payload.value;
        const durationMs = toMs(payload.duration);
        const usage = normalizeUsage(payload.usage);
        // Pre-#206 servers omit `variant` entirely; default to "success" to
        // mirror the Rust `#[serde(default)]` on `TaskEndVariant`.
        const variant: TaskEndVariant = typeof payload.variant === 'string' ? payload.variant : 'success';
        return { kind: 'taskEnd', task, output, durationMs, usage, variant };
      }
      if (Array.isArray(payload)) {
        const task = typeof payload[0] === 'string' ? payload[0] : '';
        const output = payload[2];
        const durationMs = toMs(payload[3]);
        const usage = payload.length >= 5 ? normalizeUsage(payload[4]) : null;
        return { kind: 'taskEnd', task, output, durationMs, usage, variant: 'success' };
      }
      return { kind: 'taskEnd', task: '', output: undefined, durationMs: 0, usage: null, variant: 'success' };
    }
    case 'AgentOutput': {
      if (isRecord(payload)) {
        const task = typeof payload.task_name === 'string' ? payload.task_name : '';
        const agent = typeof payload.agent_name === 'string' ? payload.agent_name : null;
        const taskId = typeof payload.task_id === 'string' ? payload.task_id : '';
        const chunk = typeof payload.chunk === 'string' ? payload.chunk : '';
        return { kind: 'agentChunk', task, agent, taskId, chunk };
      }
      return { kind: 'agentChunk', task: '', agent: null, taskId: '', chunk: '' };
    }
    case 'ToolCallStart': {
      if (isRecord(payload)) {
        const task = typeof payload.task_name === 'string' ? payload.task_name : '';
        const tool = typeof payload.tool_name === 'string' ? payload.tool_name : '';
        const server = typeof payload.server_name === 'string' ? payload.server_name : '';
        return { kind: 'toolCallStart', task, tool, server, input: payload.input };
      }
      return { kind: 'toolCallStart', task: '', tool: '', server: '', input: undefined };
    }
    case 'ToolCallEnd': {
      if (isRecord(payload)) {
        const task = typeof payload.task_name === 'string' ? payload.task_name : '';
        const tool = typeof payload.tool_name === 'string' ? payload.tool_name : '';
        // Core emits `{ secs, nanos }` under `duration`; older flattened shape
        // used `duration_ms` — accept both to stay resilient.
        const durationMs = isDuration(payload.duration)
          ? toMs(payload.duration)
          : typeof payload.duration_ms === 'number' ? payload.duration_ms : 0;
        return { kind: 'toolCallEnd', task, tool, output: payload.output, durationMs };
      }
      return { kind: 'toolCallEnd', task: '', tool: '', output: undefined, durationMs: 0 };
    }
    case 'Suspended': {
      if (isRecord(payload)) {
        const name = typeof payload.checkpoint_name === 'string' ? payload.checkpoint_name : '';
        const token = typeof payload.token === 'string' ? payload.token : '';
        const prompt = typeof payload.prompt === 'string' ? payload.prompt : '';
        const schema = payload.schema;
        const timeoutSecs = typeof payload.timeout_secs === 'number' ? payload.timeout_secs : null;
        // Older servers omit `trigger` entirely; normalizer defaults to
        // `DagPosition` to match the Rust `#[serde(default)]`.
        const trigger = normalizeSuspendTrigger(payload.trigger);
        return { kind: 'checkpoint', name, token, prompt, schema, timeoutSecs, trigger };
      }
      return { kind: 'checkpoint', name: '', token: '', prompt: '', schema: null, timeoutSecs: null, trigger: { kind: 'DagPosition' } };
    }
    case 'ToolApprovalPending': {
      if (isRecord(payload)) {
        const token = typeof payload.token === 'string' ? payload.token : '';
        const toolRef = typeof payload.tool_ref === 'string' ? payload.tool_ref : '';
        const executionId = typeof payload.execution_id === 'string' ? payload.execution_id : null;
        const nodeId = typeof payload.node_id === 'number' ? payload.node_id : null;
        return { kind: 'toolApproval', token, toolRef, args: payload.args, executionId, nodeId };
      }
      return { kind: 'toolApproval', token: '', toolRef: '', args: undefined, executionId: null, nodeId: null };
    }
    case 'Breakpoint': {
      if (isRecord(payload)) {
        const token = typeof payload.token === 'string' ? payload.token : '';
        const nodeId = typeof payload.node_id === 'number' ? payload.node_id : 0;
        const env = isRecord(payload.env_snapshot) ? payload.env_snapshot : {};
        return { kind: 'breakpoint', token, nodeId, env };
      }
      return { kind: 'breakpoint', token: '', nodeId: 0, env: {} };
    }
    case 'Error': {
      if (isRecord(payload)) {
        const message = typeof payload.message === 'string' ? payload.message : String(payload.message ?? '');
        const errorKind = normalizeErrorKind(payload.kind);
        // Codes are kebab-cased AKRIBES-E-... strings; we forward them as
        // opaque tags rather than enumerating, so a newer server's code
        // surfaces verbatim instead of getting collapsed.
        const code = typeof payload.code === 'string' ? payload.code : null;
        const userMessage = typeof payload.user_message === 'string' ? payload.user_message : message;
        const retryAfterMs = typeof payload.retry_after_ms === 'number'
          ? payload.retry_after_ms
          : null;
        const source = normalizeErrorSource(payload.source);
        const suggestedAction = suggestedActionFor(errorKind);
        const retryable = retryAfterMs !== null
          || (TRANSIENT_KINDS as readonly string[]).includes(errorKind);
        return {
          kind: 'error',
          message,
          errorKind,
          code,
          userMessage,
          retryAfterMs,
          suggestedAction,
          retryable,
          source,
        };
      }
      // Legacy: bare-string error payload from an older server. Fill the
      // structured fields with sensible defaults so consumers don't
      // branch.
      const message = typeof payload === 'string' ? payload : '';
      return {
        kind: 'error',
        message,
        errorKind: 'ServerError',
        code: null,
        userMessage: message,
        retryAfterMs: null,
        suggestedAction: 'retry',
        retryable: true,
        source: {},
      };
    }
    case 'ValidationFailure': {
      if (isRecord(payload)) {
        const taskName = typeof payload.task_name === 'string' ? payload.task_name : '';
        const attempt = typeof payload.attempt === 'number' ? payload.attempt : 0;
        const modelResponse = typeof payload.model_response === 'string' ? payload.model_response : '';
        const missingFields = Array.isArray(payload.missing_fields)
          ? payload.missing_fields.filter((s): s is string => typeof s === 'string')
          : [];
        const extraFields = Array.isArray(payload.extra_fields)
          ? payload.extra_fields.filter((s): s is string => typeof s === 'string')
          : [];
        const typeErrors = Array.isArray(payload.type_errors)
          ? payload.type_errors.filter((s): s is string => typeof s === 'string')
          : [];
        const stopReason = typeof payload.stop_reason === 'string' ? payload.stop_reason : null;
        return {
          kind: 'validationFailure',
          taskName,
          attempt,
          modelResponse,
          missingFields,
          extraFields,
          typeErrors,
          stopReason,
        };
      }
      return {
        kind: 'validationFailure',
        taskName: '',
        attempt: 0,
        modelResponse: '',
        missingFields: [],
        extraFields: [],
        typeErrors: [],
        stopReason: null,
      };
    }
    case 'RuntimeStart':
    case 'RuntimeStdout':
    case 'RuntimeStderr':
    case 'RuntimeEnd':
    case 'RuntimeError':
      return normalizeRuntimeEvent(type, payload);
    case 'SubScript': {
      // Wire shape (akribes-core EngineEvent::SubScript, post-#993):
      //   { script_name, parent_task, parent_node_id?, attempt?,
      //     parent_path: SubScriptFrame[], child: EngineEvent (leaf) }
      // Legacy wire shape (pre-#993) wrapped each call-stack level
      // recursively in its own SubScript envelope. We unwrap the legacy
      // shape on the fly so consumers always see the flat shape:
      // `parentPath` holds the ancestor chain (outermost → immediate
      // parent), and `child` is the normalized leaf event.
      if (isRecord(payload)) {
        const { scriptName, parentTask, parentPath, leaf } = flattenSubScriptPayload(payload);
        const child: WorkflowEvent = leaf && isRecord(leaf) && typeof (leaf as { type?: unknown }).type === 'string'
          ? toWorkflowEvent(leaf as EngineEvent)
          : { kind: 'other', typeName: '', payload: leaf };
        return { kind: 'subScript', scriptName, parentTask, parentPath, child };
      }
      return { kind: 'subScript', scriptName: '', parentTask: '', parentPath: [], child: { kind: 'other', typeName: '', payload } };
    }
    default:
      return { kind: 'other', typeName: type, payload };
  }
}
