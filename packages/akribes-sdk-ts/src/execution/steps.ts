/**
 * Shared execution step model + event reducer.
 *
 * Folds a stream of `EngineEvent`s (wrapped as `HubEvent`) into a structured
 * list of `ExecutionStep`s that can be rendered by any frontend (React,
 * vanilla DOM, etc.). This is the single source of truth for how raw engine
 * events become user-visible steps — both Studio and the docs `.akr` runner
 * consume it.
 */
import type { EngineEvent, HubEvent, SuspendTrigger, TypeRef, ValidationErrorWire } from '../types';
import { normalizeSuspendTrigger } from '../workflowEvents';

/** Controls where a step is displayed. */
export type StepVisibility = 'inline' | 'panel-only' | 'hidden';

/** Token counts attached to a task step (when the engine reports usage). */
export type StepTokens = {
  input: number;
  output: number;
  cachedInput: number;
  model: string;
  provider: string;
};

/**
 * Aggregated token totals across every `TaskEnd` inside a single sub-script
 * call. We deliberately don't compute USD client-side: pricing lives in
 * `akribes-server/src/pricing.rs` and would drift if mirrored here. The
 * execution-level USD shown at the top of the panel still covers the run as
 * a whole; per-call USD is a follow-up that needs either a server endpoint
 * or a synced pricing table in the SDK.
 */
export type SubScriptTokens = {
  input: number;
  output: number;
  cachedInput: number;
  /** Distinct models seen across the sub-script's `TaskEnd` events.
   *  Usually one — multi-task sub-scripts that use different agents will
   *  show all of them so the operator can spot model mix at a glance. */
  models: string[];
};

/** Structured payload for a `validation_failure` step. Mirrors the wire shape
 *  of `EngineEvent::ValidationFailure` (camel-cased on the SDK side). */
export type ValidationFailurePayload = {
  taskName: string;
  attempt: number;
  modelResponse: string;
  missingFields: string[];
  extraFields: string[];
  typeErrors: string[];
  stopReason: string | null;
};

/**
 * Per-task summary surfaced inside a sub-script step's "Timeline" UI section.
 *
 * Each entry corresponds to one wrapped `TaskEnd` (`kind: 'task_end'`) or one
 * wrapped `ValidationFailure` (`kind: 'validation_failure'`) observed on the
 * sub-script's stream. The Studio `SubScriptCard` renders these as a
 * collapsible timeline so operators can see what tasks ran inside a `call(...)`
 * without drilling into the full engine-event log.
 *
 * Intentionally lightweight: we keep per-task rollups (name, duration, tokens,
 * attempt) rather than the streaming `AgentOutput` chunks. Callers that need
 * the full chunk-by-chunk text should request a deeper drill-in (see the
 * sub-script-card brief, "out of scope" section).
 */
export type SubScriptTaskSummary =
  | {
      kind: 'task_end';
      /** Task name from the wrapped `TaskEnd.task` field. */
      taskName: string;
      /** Duration in milliseconds. */
      durationMs: number;
      /** 1-indexed attempt number; `> 1` means this task retried. */
      attempt: number;
      /** Token usage when reported by the engine; absent for stdlib calls. */
      tokens?: StepTokens;
      /** Per-task USD cost when reported by the engine (#871). Today the
       *  engine doesn't emit cost on `TaskEnd` — pricing is computed
       *  server-side at run rollup time — so this field is a placeholder
       *  slot. When the server starts attaching `cost_usd` on `TaskEnd`,
       *  the reducer will populate this and the UI surfaces it as a
       *  per-row badge without further changes. */
      costUsd?: number;
      /** Concatenated `AgentOutput` chunks observed for this task inside its
       *  hosting sub-script. Drives the per-task drill-in: clicking the
       *  timeline row in `SubScriptCard` expands the streaming text so the
       *  operator can see what the LLM actually said inside a nested call.
       *  Absent for stdlib tasks (no streaming output) and for any task
       *  whose AgentOutput never arrived (e.g. cached / replayed runs that
       *  only carry the terminal TaskEnd). */
      streamingOutput?: string;
    }
  | {
      kind: 'validation_failure';
      /** The task whose output failed validation. */
      taskName: string;
      /** 1-indexed attempt number that failed. */
      attempt: number;
    };

/** Per-turn aggregate inside a loop step. `toolCalls` mirrors
 *  `EngineEvent::LoopTurn.tool_calls` — names of the tools the agent invoked
 *  this turn (in dispatch order, includes synthetic `state_get`,
 *  `state_update`, `return`, plus user `skills:` entries). */
export type LoopTurnSummary = {
  turn: number;
  toolCalls: string[];
};

export type ExecutionStep = {
  id: string;
  line: number;
  type: 'execution' | 'chat' | 'variable' | 'tool_call' | 'sub_script' | 'validation_failure' | 'loop';
  content: string;
  status?: 'pending' | 'running' | 'success' | 'error';
  variables?: Record<string, any>;
  timestamp: number;
  /** Server-attached monotonic sequence number. Undefined when the server
   *  predates the seq plumbing or when the reducer ran on an event from a
   *  legacy source (e.g. replay from an older log). */
  seq?: number;
  /** Server-attached epoch-ms timestamp parsed from the `at` field. Distinct
   *  from `timestamp` (which is `Date.now()` at reduce time on the client). */
  serverTs?: number;
  /** Set on `validation_failure` steps only — the structured payload from
   *  `EngineEvent::ValidationFailure`. */
  validationFailure?: ValidationFailurePayload;
  agent?: string;
  taskId?: string;
  /** Task name from the engine. Distinct from `agent` (which prefers
   *  the explicit agent name when one is set). Used by the TaskEnd
   *  reducer to merge the structured result back into the streaming
   *  chat step instead of producing a duplicate row. */
  taskName?: string;
  schemaType?: string;
  duration?: number;
  prompt?: string;
  nodeId?: number;
  cached?: boolean;
  visibility?: StepVisibility;
  /** Token usage reported on TaskEnd (not set for other step types). */
  tokens?: StepTokens;
  /** Error kind when status === 'error' (from the engine's Error event). */
  errorKind?: string;
  /** Stable AKRIBES-E-XXX code attached to the error event (when present). */
  errorCode?: string;
  /** User-facing message from the error envelope. Render this verbatim
   *  in the UI; the developer-oriented `content` already contains the
   *  raw `message`. */
  errorUserMessage?: string;
  /** Provider's retry-after hint in milliseconds (when present). UI can
   *  surface "retry in N seconds" affordances without re-deriving. */
  errorRetryAfterMs?: number;
  /** Where the error originated. Optional fields — render whichever are
   *  present. */
  errorSource?: {
    task?: string;
    agent?: string;
    provider?: string;
    model?: string;
    toolRef?: string;
    script?: string;
    line?: number;
  };
  /** Tool name for tool_call steps (from ToolCallStart/ToolCallEnd events). */
  toolName?: string;
  /** MCP server name for tool_call steps. */
  serverName?: string;
  /** Input passed to the tool call. */
  toolInput?: Record<string, any>;
  /** Output returned by the tool call. */
  toolOutput?: Record<string, any>;
  /** Structured type of the task result value, as reported by the engine on TaskEnd. */
  valueType?: TypeRef | null;
  /** 1-indexed attempt number from TaskEnd (> 1 means a retry occurred). */
  attempt?: number;
  /** Number of validation retries the engine made before suspending. Set
   *  when a Suspended event arrives with a `ValidationExhausted` trigger. */
  retryCount?: number;
  /** Raw last-attempt output that failed validation. Set alongside
   *  `validationErrors` on `ValidationExhausted` suspensions so the UI can
   *  show the author what the model actually produced. */
  lastAttempt?: string;
  /** Per-stage validation errors collected from a `ValidationExhausted`
   *  trigger. Each entry carries the stage (`parse`/`schema`/`custom:<rule>`),
   *  a human-readable message, and an optional JSON-pointer path. */
  validationErrors?: ValidationErrorWire[];
  /** Name of the `checkpoint` block the engine suspended at. Set on the
   *  step produced by a `Suspended` engine event so the UI can scope the
   *  resume action to a specific checkpoint. */
  checkpointName?: string;
  /** Why the engine suspended — one of `DagPosition`, `ValidationExhausted`,
   *  `AgentUnable`, or a forward-compat `UnknownSuspendTrigger`. Carries the
   *  typed payload (task name, unable record, etc.) so the UI can render a
   *  trigger-aware checkpoint card or pre-fill the resume modal. */
  suspendTrigger?: SuspendTrigger;
  /** Opaque token the server expects back on `resume_execution`. Set only on
   *  steps produced by a `Suspended` event. */
  suspendToken?: string;
  /** Checkpoint prompt text (the `prompt:` property on the `checkpoint`
   *  block). Useful for showing the human operator what they're approving. */
  suspendPrompt?: string;
  /** JSON schema describing the expected resume payload. The UI can use
   *  this to render a form or validate edits before calling `resume`. */
  suspendSchema?: unknown;
  /** Cross-script `call(...)` metadata for `type === 'sub_script'` steps.
   *  Set incrementally as the sub-script's wrapped event stream lands:
   *  the parent variable name + script name come from the first envelope,
   *  inputs come from the sub-engine's leading `StateUpdate` events,
   *  output comes from the wrapped terminal `WorkflowEnd`, and
   *  `subScriptTokens` totals every wrapped `TaskEnd`'s usage. */
  subScript?: {
    /** Called script's name as it appears in the `call("…")` site. No
     *  channel/version surfaced today — the engine doesn't expose it on the
     *  envelope. Follow-up if/when needed. */
    scriptName: string;
    /** Variable on the parent side that received the call result, e.g. `out`
     *  in `out = call("foo", x=1)`. `<anonymous>` when the call result was
     *  not bound to a name (engine fallback). */
    parentTask: string;
    /** Resolved inputs collected from the sub-engine's input-hydration
     *  `StateUpdate` events. Order is engine-emitted (input-block order). */
    inputs: { name: string; value: unknown }[];
    /** Sub-script's terminal `WorkflowEnd` value. `undefined` while the
     *  sub-script is still running; set once we see WorkflowEnd. */
    output?: unknown;
    /** Aggregated token usage across every `TaskEnd` reachable from this
     *  sub-script — INCLUDING descendants. Equals `selfTokens` plus the
     *  rolled-up `subScriptTokens` of every entry in `children`. The UI
     *  shows this as the headline cost figure for the call. */
    subScriptTokens?: SubScriptTokens;
    /** Token usage from `TaskEnd` events that the sub-script ran DIRECTLY,
     *  excluding any descendants. The UI uses `subScriptTokens - selfTokens`
     *  to render "of which X% from nested calls" so operators can attribute
     *  spend to a specific level instead of having to read the call chain. */
    selfTokens?: SubScriptTokens;
    /** Number of nested `TaskEnd` events folded into the totals. Useful for
     *  the UI to show "3 tasks" without re-counting child events. */
    nestedTaskCount?: number;
    /** Set true once we observe the sub-script's terminal `WorkflowEnd` (or
     *  an `Error`). False while the sub-script is still streaming events.
     *  The reducer uses this to decide whether the next SubScript event for
     *  the same `parentTask` should accumulate or open a fresh call. */
    closed?: boolean;
    /** Per-task summaries collected from this sub-script's wrapped `TaskEnd`
     *  and `ValidationFailure` events. Drives the Studio "Timeline" drill-in
     *  in `SubScriptCard`. Only populated for the level that directly hosts
     *  the wrapped task event — nested sub-scripts maintain their own
     *  summary list under `children[].subScript.taskSummaries`. */
    taskSummaries?: SubScriptTaskSummary[];
    /** Internal buffer: streaming `AgentOutput` chunks per task name, kept
     *  in-flight until the matching `TaskEnd` arrives and attaches the
     *  concatenated text to the summary as `streamingOutput`. Not part of
     *  the wire format; reducer-private state. Underscore-prefixed so
     *  consumers know it is implementation detail. */
    _streamingByTask?: Record<string, string>;
    /** Nested sub-script calls (depth > 1). When script A calls B which calls
     *  C, the C call lives here on the B step (which itself lives in A's
     *  `children`). Each entry is a fully-formed `ExecutionStep` with
     *  `type === 'sub_script'`, so the renderer can recurse with the same
     *  `SubScriptCard` component. Token totals on the parent already include
     *  the rollup from every descendant. */
    children?: ExecutionStep[];
  };
  /** Loop block name (`loop NAME(...) -> Ret`), set on `type === 'loop'`
   *  steps from the `EngineEvent::LoopStart.name` field. The reducer keys
   *  in-flight loop steps by this name so `LoopTurn`/`LoopEnd` events fold
   *  back into the right step. */
  loopName?: string;
  /** Resolved upper-bound turn budget — declared `max_turns:` if set,
   *  otherwise the engine's `LOOP_MAX_TURNS_DEFAULT`. Set from
   *  `EngineEvent::LoopStart.max_turns`. */
  maxTurns?: number;
  /** One entry per `EngineEvent::LoopTurn` observed, in arrival order. */
  turns?: LoopTurnSummary[];
  /** The agent's submitted return value (from `return(...)`), the final
   *  state on a natural `stop_when:` exit, or a `Value::FatalError`-shaped
   *  envelope when the loop exhausted its `max_turns` budget. Carried
   *  verbatim from `EngineEvent::LoopEnd.value` so the UI can render it
   *  through `AkribesValueViewerWithRawToggle` the same way it renders any
   *  other task result. */
  loopResult?: unknown;
};

/**
 * Mutable ref-like object for tracking active line synchronously.
 * Using a ref avoids the stale-closure problem where React state hasn't
 * re-rendered yet when the next event arrives in the same batch.
 */
export type ActiveLineRef = { current: number | null };

/** Mutable ref-like object for tracking the current node id. */
export type ActiveNodeRef = { current: number | null };

/**
 * Side effects the reducer signals to the caller. The reducer is pure w.r.t.
 * step state but needs to communicate updates to activeLine, globalEnv, and
 * execution lifecycle.
 */
export type ReducerSideEffects = {
  setActiveLine?: number | null;
  globalEnvUpdates?: Record<string, any>;
  executionFinished?: boolean;
  refreshHistory?: boolean;
  breakpoint?: { nodeId: number; token: string; envSnapshot: Record<string, any>; line: number };
  breakpointResumed?: boolean;
};

/**
 * Per-envelope side effects extracted from a `SubScript`'s wrapped child.
 * Only the level that directly contains the leaf event (i.e. the innermost
 * sub-script in a nested chain) consumes these.
 */
type SubScriptChildEffects = {
  /** Single input binding observed in the wrapped child, if any. */
  maybeInput?: { name: string; value: unknown };
  /** Output value when the wrapped child is `WorkflowEnd`. */
  maybeOutput?: unknown;
  /** Token usage when the wrapped child is `TaskEnd`. */
  maybeUsage?: Record<string, unknown>;
  /** Per-task summary entry produced by `TaskEnd` / `ValidationFailure`. */
  maybeTaskSummary?: SubScriptTaskSummary;
  /** Streaming chunk observed for an `AgentOutput` child. We buffer these
   *  on the sub-script step keyed by `taskName` and attach the concatenated
   *  text to the corresponding `task_end` summary when its `TaskEnd` lands.
   *  Sub-script drill-in: this is what surfaces the LLM's verbatim output
   *  inside a nested call. */
  maybeAgentOutput?: { taskName: string; chunk: string };
  /** True when the wrapped child is `WorkflowEnd` or `Error`. */
  childIsTerminal: boolean;
  /** True when the wrapped child is `Error` (terminal AND failed). */
  childIsError: boolean;
};

/**
 * Inspect a single `SubScript` envelope (`{ script_name, parent_task, child }`)
 * and either:
 *   * return the leaf-level effects when the wrapped child is a non-SubScript
 *     event (TaskEnd / WorkflowEnd / StateUpdate / ValidationFailure / Error)
 *   * return a nested-frame descriptor when the wrapped child is itself a
 *     `SubScript` envelope, so the caller can recurse one level deeper
 *
 * `child` is one wrapped inner event. We only care about a small subset of
 * leaf children:
 *   * `StateUpdate` at the start of the sub-stream → an input binding
 *   * `TaskEnd` → token usage + a per-task timeline summary
 *   * `ValidationFailure` → a per-task timeline summary (kind=failure)
 *   * `WorkflowEnd` → the call's output value + closes the call
 *   * `Error`     → also closes the call (with error status)
 *
 * Anything else (NodeStart/NodeEnd/AgentOutput/TaskStart/etc.) is forwarded
 * as-is — the call stays open but no fields update. This is intentional: we
 * don't try to reconstruct the full nested event log here, because the
 * sub-script's full streaming chunks are out-of-scope for the card's
 * Timeline section (per-task summaries are enough).
 *
 * Inputs vs internal `StateUpdate`s: the engine emits a `StateUpdate` for
 * every input on workflow-start AND for every assignment inside the
 * workflow body. We use a heuristic: only the StateUpdates that arrive
 * BEFORE the first `TaskStart` in this sub-stream are inputs. The reducer
 * doesn't carry per-call ordering state; instead we accept any StateUpdate
 * as an "input" but de-dupe by name (so an assignment that re-binds the
 * input variable later overwrites with the latest value, which is a
 * reasonable display semantics — the operator sees the most recent state).
 *
 * Returns `null` for malformed payloads so the reducer can safely no-op.
 */
type SubScriptFrame = {
  scriptName: string;
  parentTask: string;
};

type ParsedSubScriptEnvelope = {
  /** The current frame (this envelope's `script_name` + `parent_task`). */
  frame: SubScriptFrame;
} & (
  | {
      kind: 'leaf';
      effects: SubScriptChildEffects;
    }
  | {
      kind: 'nested';
      /** The wrapped inner `SubScript` payload, ready for recursion. */
      innerPayload: unknown;
    }
);

function parseSubScriptEnvelope(payload: unknown): ParsedSubScriptEnvelope | null {
  if (!payload || typeof payload !== 'object') return null;
  const p = payload as Record<string, unknown>;
  const scriptName = typeof p.script_name === 'string' ? p.script_name : '';
  const parentTask = typeof p.parent_task === 'string' ? p.parent_task : '';
  const frame: SubScriptFrame = { scriptName, parentTask };
  const child = p.child;
  if (!child || typeof child !== 'object') {
    return {
      frame,
      kind: 'leaf',
      effects: { childIsTerminal: false, childIsError: false },
    };
  }
  const c = child as { type?: unknown; payload?: unknown };
  const cType = typeof c.type === 'string' ? c.type : '';
  const cPayload = c.payload;

  // Nested sub-script: defer to the caller for one more level of unwrapping.
  if (cType === 'SubScript') {
    return { frame, kind: 'nested', innerPayload: cPayload };
  }

  switch (cType) {
    case 'StateUpdate': {
      // Wire shape: payload is `[name, value]`.
      if (Array.isArray(cPayload) && typeof cPayload[0] === 'string') {
        return {
          frame,
          kind: 'leaf',
          effects: {
            maybeInput: { name: cPayload[0], value: cPayload[1] },
            childIsTerminal: false,
            childIsError: false,
          },
        };
      }
      return { frame, kind: 'leaf', effects: { childIsTerminal: false, childIsError: false } };
    }
    case 'TaskEnd': {
      const obj = cPayload && typeof cPayload === 'object' && !Array.isArray(cPayload)
        ? (cPayload as Record<string, unknown>)
        : null;
      const usage = obj && typeof obj.usage === 'object' && obj.usage !== null
        ? (obj.usage as Record<string, unknown>)
        : undefined;
      const taskName = obj && typeof obj.task === 'string' ? obj.task : '';
      const attempt = obj && typeof obj.attempt === 'number' ? obj.attempt : 1;
      const duration = obj && typeof obj.duration === 'object' && obj.duration !== null
        ? obj.duration as { secs?: unknown; nanos?: unknown }
        : null;
      const durationMs = duration
        ? Number(duration.secs ?? 0) * 1000 + Number(duration.nanos ?? 0) / 1_000_000
        : 0;
      const tokens = parseTokens(usage);
      // #871: per-task USD if the engine attached it. The current server
      // doesn't include `cost_usd` on TaskEnd, but the reducer reads it
      // opportunistically so the field surfaces in the UI as soon as the
      // server side starts emitting it.
      const costUsdRaw = obj && typeof obj.cost_usd === 'number' ? obj.cost_usd : undefined;
      const summary: SubScriptTaskSummary = {
        kind: 'task_end',
        taskName,
        durationMs,
        attempt,
        tokens,
        ...(costUsdRaw !== undefined ? { costUsd: costUsdRaw } : {}),
      };
      return {
        frame,
        kind: 'leaf',
        effects: {
          maybeUsage: usage,
          maybeTaskSummary: summary,
          childIsTerminal: false,
          childIsError: false,
        },
      };
    }
    case 'AgentOutput': {
      // Per-task streaming text inside a sub-script. Wire shape mirrors the
      // top-level `AgentOutput` arm: `{ task_name, agent_name, task_id,
      // schema_type, chunk }`. We carry just `task_name` + `chunk` — the
      // task summary keys by name, and the chunk is what the operator wants
      // to read in the drill-in.
      const obj = cPayload && typeof cPayload === 'object' && !Array.isArray(cPayload)
        ? (cPayload as Record<string, unknown>)
        : null;
      const taskName = obj && typeof obj.task_name === 'string' ? obj.task_name : '';
      const chunk = obj && typeof obj.chunk === 'string' ? obj.chunk : '';
      if (!taskName || !chunk) {
        return { frame, kind: 'leaf', effects: { childIsTerminal: false, childIsError: false } };
      }
      return {
        frame,
        kind: 'leaf',
        effects: {
          maybeAgentOutput: { taskName, chunk },
          childIsTerminal: false,
          childIsError: false,
        },
      };
    }
    case 'ValidationFailure': {
      const obj = cPayload && typeof cPayload === 'object' && !Array.isArray(cPayload)
        ? (cPayload as Record<string, unknown>)
        : null;
      const taskName = obj && typeof obj.task_name === 'string' ? obj.task_name : '';
      const attempt = obj && typeof obj.attempt === 'number' ? obj.attempt : 1;
      return {
        frame,
        kind: 'leaf',
        effects: {
          maybeTaskSummary: { kind: 'validation_failure', taskName, attempt },
          childIsTerminal: false,
          childIsError: false,
        },
      };
    }
    case 'WorkflowEnd': {
      // Issue #1173: WorkflowEnd payload may be either the new
      // `{ value, total_input_tokens, ... }` struct or the legacy bare
      // output value. Recover the output via the same disambiguator
      // used in `workflowEvents.ts`'s parseWorkflowEndPayload (presence
      // of `value` + any `total_*` key signals new shape).
      let output: unknown = cPayload;
      if (cPayload && typeof cPayload === 'object' && !Array.isArray(cPayload)) {
        const o = cPayload as Record<string, unknown>;
        const aggKeys = [
          'total_input_tokens',
          'total_output_tokens',
          'total_cached_input_tokens',
          'total_thinking_tokens',
          'total_tool_tokens',
          'total_cost_usd',
          'task_count',
        ];
        if ('value' in o && aggKeys.some((k) => k in o)) {
          output = o.value;
        }
      }
      return {
        frame,
        kind: 'leaf',
        effects: {
          maybeOutput: output,
          childIsTerminal: true,
          childIsError: false,
        },
      };
    }
    case 'Error': {
      return {
        frame,
        kind: 'leaf',
        effects: { childIsTerminal: true, childIsError: true },
      };
    }
    default:
      return { frame, kind: 'leaf', effects: { childIsTerminal: false, childIsError: false } };
  }
}

/**
 * Unwrap a chain of nested `SubScript` envelopes into:
 *   * the ordered stack of frames (`[outermost, …, innermost]`)
 *   * the leaf-level effects to apply at the innermost frame
 *
 * Returns `null` only when the outermost payload itself is malformed (which
 * the reducer treats as a no-op, mirroring the v1 behavior).
 */
function unwrapSubScriptChain(payload: unknown): {
  frames: SubScriptFrame[];
  effects: SubScriptChildEffects;
} | null {
  // Issue #993: the new flat wire shape carries the ancestor chain via
  // `parent_path` on the OUTER envelope (frames ordered outermost →
  // immediate parent). Read those first so the chain reads correctly
  // even when the engine emitted a depth-1 (`parent_path` empty) +
  // flat-child envelope. Pre-#993 emissions had no `parent_path` and
  // nested every level via `child`; we still walk that case for
  // back-compat against archived event logs.
  const frames: SubScriptFrame[] = [];
  if (payload && typeof payload === 'object') {
    const outer = payload as Record<string, unknown>;
    if (Array.isArray(outer.parent_path)) {
      for (const f of outer.parent_path) {
        if (!f || typeof f !== 'object') continue;
        const ff = f as Record<string, unknown>;
        frames.push({
          scriptName: typeof ff.script_name === 'string' ? ff.script_name : '',
          parentTask: typeof ff.parent_task === 'string' ? ff.parent_task : '',
        });
      }
    }
  }
  let current: unknown = payload;
  // Hard upper bound to keep the loop honest if a future engine ever produces
  // a cycle (it can't today — `Box<EngineEvent>` is a tree, not a graph).
  for (let depth = 0; depth < 64; depth += 1) {
    const parsed = parseSubScriptEnvelope(current);
    if (!parsed) return null;
    frames.push(parsed.frame);
    if (parsed.kind === 'leaf') {
      return { frames, effects: parsed.effects };
    }
    current = parsed.innerPayload;
  }
  // Defensive fallback for an absurdly deep chain — render as a single leaf
  // with no effects rather than crashing the reducer.
  return { frames, effects: { childIsTerminal: false, childIsError: false } };
}

function aggregateSubScriptTokens(
  prev: SubScriptTokens | undefined,
  raw: Record<string, unknown>,
): SubScriptTokens | undefined {
  const input = Number(raw.input_tokens ?? 0);
  const output = Number(raw.output_tokens ?? 0);
  const cachedInput = Number(raw.cached_input_tokens ?? 0);
  const model = typeof raw.model === 'string' ? raw.model : '';
  // Skip empty-usage events (mock provider emits `{}` sometimes).
  if (!input && !output && !cachedInput && !model) return prev;
  const base: SubScriptTokens = prev ?? { input: 0, output: 0, cachedInput: 0, models: [] };
  const models = model && !base.models.includes(model) ? [...base.models, model] : base.models;
  return {
    input: base.input + input,
    output: base.output + output,
    cachedInput: base.cachedInput + cachedInput,
    models,
  };
}

/**
 * Walk a list of nested sub-script steps and roll their token totals into a
 * single aggregate. Used when a parent sub-script's totals need to be
 * recomputed after a descendant updated.
 */
function rollupTokensFromChildren(children: ExecutionStep[]): SubScriptTokens | undefined {
  let agg: SubScriptTokens | undefined = undefined;
  for (const c of children) {
    const t = c.subScript?.subScriptTokens;
    if (!t) continue;
    const base: SubScriptTokens = agg ?? { input: 0, output: 0, cachedInput: 0, models: [] };
    const models = [...base.models];
    for (const m of t.models) if (!models.includes(m)) models.push(m);
    agg = {
      input: base.input + t.input,
      output: base.output + t.output,
      cachedInput: base.cachedInput + t.cachedInput,
      models,
    };
  }
  return agg;
}

/**
 * Sum the `nestedTaskCount` across a list of children. Used to keep an
 * ancestor's count in sync as descendants accumulate `TaskEnd` envelopes.
 */
function rollupTaskCountFromChildren(children: ExecutionStep[]): number {
  let n = 0;
  for (const c of children) n += c.subScript?.nestedTaskCount ?? 0;
  return n;
}

/**
 * Combine this level's `selfTokens` (tokens from `TaskEnd`s wrapped DIRECTLY
 * at this frame, never from descendants) with the recursive rollup of every
 * child's `subScriptTokens`. The result is the level's recursive
 * `subScriptTokens`. Returns `undefined` only when both inputs are
 * absent/empty so the UI can preserve its "no tokens yet" state.
 *
 * Invariant the SubScript reducer relies on:
 *
 *   subScriptTokens = selfTokens + Σ children[i].subScriptTokens
 *
 * Maintaining this invariant explicitly (rather than the previous
 * `prevTokens - prevChildrenRollup` subtraction) avoids the floating-point
 * + model-list drift the old recompute path was vulnerable to when a level
 * accumulated multiple direct `TaskEnd`s in between child updates.
 */
function combineSelfAndChildrenTokens(
  self: SubScriptTokens | undefined,
  children: ExecutionStep[] | undefined,
): SubScriptTokens | undefined {
  const childRoll = rollupTokensFromChildren(children ?? []);
  if (!self && !childRoll) return undefined;
  const merged: SubScriptTokens = {
    input: (self?.input ?? 0) + (childRoll?.input ?? 0),
    output: (self?.output ?? 0) + (childRoll?.output ?? 0),
    cachedInput: (self?.cachedInput ?? 0) + (childRoll?.cachedInput ?? 0),
    models: [],
  };
  for (const m of self?.models ?? []) if (!merged.models.includes(m)) merged.models.push(m);
  for (const m of childRoll?.models ?? []) if (!merged.models.includes(m)) merged.models.push(m);
  if (!merged.input && !merged.output && !merged.cachedInput && merged.models.length === 0) {
    return undefined;
  }
  return merged;
}

/**
 * If the summary is a `task_end` and the streaming buffer holds matching
 * text, attach it as `streamingOutput`. The reducer drops the buffer entry
 * after this call so it doesn't keep accumulating across the sub-script's
 * lifetime. Returns `undefined` when there is no summary (caller no-ops).
 */
function attachStreamingToSummary(
  summary: SubScriptTaskSummary | undefined,
  streamingByTask: Record<string, string> | undefined,
): SubScriptTaskSummary | undefined {
  if (!summary) return undefined;
  if (summary.kind !== 'task_end') return summary;
  const buffered = streamingByTask?.[summary.taskName];
  if (!buffered) return summary;
  return { ...summary, streamingOutput: buffered };
}

function parseTokens(raw: unknown): StepTokens | undefined {
  if (!raw || typeof raw !== 'object') return undefined;
  const u = raw as Record<string, unknown>;
  const input = Number(u.input_tokens ?? 0);
  const output = Number(u.output_tokens ?? 0);
  const cachedInput = Number(u.cached_input_tokens ?? 0);
  if (!input && !output && !cachedInput) return undefined;
  return {
    input,
    output,
    cachedInput,
    model: typeof u.model === 'string' ? u.model : '',
    provider: typeof u.provider === 'string' ? u.provider : '',
  };
}

/**
 * Pure reducer: takes current steps + a hub event, returns new steps + side
 * effects. This is the core event-handling logic shared between Studio's
 * live panel and the docs runner.
 */
export function reduceExecutionEvent(
  prev: ExecutionStep[],
  hubEvt: HubEvent,
  activeLineRef: ActiveLineRef,
  activeNodeRef: ActiveNodeRef,
): { steps: ExecutionStep[]; effects: ReducerSideEffects } {
  if (hubEvt.type !== 'Execution') return { steps: prev, effects: {} };

  const evt: EngineEvent = hubEvt.payload.event;
  const evName = evt.type;
  // `EngineEvent.payload` is `unknown` (the union is open — one variant per
  // engine event kind). Each `case` below knows the concrete shape for its
  // `evName`, so we widen to `any` once here rather than re-asserting the
  // payload shape in every arm. The `evName` switch is the de-facto narrower.
  const evPayload = evt.payload as any;
  const timestamp = Date.now();
  const id = `${evName}-${timestamp}-${Math.random()}`;
  const effects: ReducerSideEffects = {};

  // Server-attached fields (Workstream 04 §B). Optional — older servers don't
  // stamp these, so the reducer treats both as undefined-by-default. The UI
  // displays the seq badge only when present.
  const wirePayload = hubEvt.payload as { seq?: number; at?: string };
  const wireSeq = typeof wirePayload.seq === 'number' ? wirePayload.seq : undefined;
  const wireServerTs =
    typeof wirePayload.at === 'string' ? Date.parse(wirePayload.at) : undefined;

  let steps: ExecutionStep[];

  switch (evName) {
    case 'NodeStart': {
      const [nodeId, span] = evPayload;
      activeLineRef.current = span.line;
      activeNodeRef.current = nodeId;
      effects.setActiveLine = span.line;
      steps = [...prev, { id, line: span.line, type: 'execution', content: `Executing Node ${nodeId}`, status: 'running', timestamp, nodeId, visibility: 'hidden', seq: wireSeq, serverTs: wireServerTs }];
      break;
    }
    case 'TaskPrompt': {
      const [name, prompt] = evPayload;
      // Set `taskName` on the chat step (Workstream 04 \u00a7A.2): without it, the
      // later `TaskEnd` merge \u2014 which keys off `s.taskName === name` \u2014 appends
      // a second step instead of folding into the streaming chat row.
      steps = [...prev, { id, line: activeLineRef.current || 0, type: 'chat', agent: name, taskName: name, content: 'Generating response\u2026', prompt, timestamp, nodeId: activeNodeRef.current ?? undefined, visibility: 'hidden', seq: wireSeq, serverTs: wireServerTs }];
      break;
    }
    case 'AgentOutput': {
      const { task_name, agent_name, task_id, schema_type, chunk } = evPayload;
      // First try the per-`task_id` merge (used for repeated chunks once the
      // chat step has captured a task_id).
      const byTaskId = prev.findIndex(s => s.type === 'chat' && s.taskId === task_id);
      // Fall back to the chat step opened by `TaskPrompt` for this task name
      // on the same active node \u2014 that's the row we want to fill in. The
      // TaskPrompt arm sets taskName but no taskId yet.
      const byTaskName = byTaskId === -1
        ? prev.findIndex(s =>
            s.type === 'chat'
            && s.taskName === task_name
            && !s.taskId
            && s.nodeId === activeNodeRef.current,
          )
        : -1;
      const idx = byTaskId !== -1 ? byTaskId : byTaskName;
      const cur = idx !== -1 ? prev[idx] : undefined;
      if (cur) {
        const newSteps = [...prev];
        newSteps[idx] = {
          ...cur,
          // The TaskPrompt arm seeds with the placeholder "Generating response\u2026";
          // overwrite that on the first chunk instead of concatenating to it.
          content: (cur.content === 'Generating response\u2026' ? '' : cur.content) + chunk,
          taskId: task_id, // absorb so future AgentOutput chunks hit byTaskId
          schemaType: schema_type ?? cur.schemaType,
          visibility: 'inline',
          seq: wireSeq ?? cur.seq,
          serverTs: wireServerTs ?? cur.serverTs,
        };
        steps = newSteps;
      } else {
        steps = [...prev, { id, line: activeLineRef.current || 0, type: 'chat', agent: agent_name || task_name, taskId: task_id, taskName: task_name, schemaType: schema_type, content: chunk, timestamp, nodeId: activeNodeRef.current ?? undefined, visibility: 'inline', seq: wireSeq, serverTs: wireServerTs }];
      }
      break;
    }
    case 'StateUpdate': {
      const [name, value] = evPayload;
      effects.globalEnvUpdates = { [name]: value };
      // Workstream 04 \u00a7A.3: `StateUpdate` events are panel-only \u2014 the
      // Variables segment shows them; the inline Output stream filters them
      // out so users don't see "Variable updated: x" rows next to the chat
      // step that already produced `x`.
      steps = [...prev, { id, line: activeLineRef.current || 0, type: 'variable', content: `Variable updated: ${name}`, variables: { [name]: value }, status: 'success', timestamp, nodeId: activeNodeRef.current ?? undefined, visibility: 'panel-only', seq: wireSeq, serverTs: wireServerTs }];
      break;
    }
    case 'TaskEnd': {
      // TaskEnd payload: { task, on_error_label, value, value_type, duration, attempt, usage }
      const { task: name, value: result, value_type, duration, attempt, usage } = evPayload;
      const durationMs = duration.secs * 1000 + duration.nanos / 1000000;
      const tokens = parseTokens(usage);
      // If we already have a chat step from this task's AgentOutput chunks,
      // merge the structured result into it rather than appending a duplicate.
      // Without this the panel renders the streamed text AND the parsed object
      // as two separate rows with the same timestamp.
      const chatIdx = (() => {
        for (let i = prev.length - 1; i >= 0; i -= 1) {
          const s = prev[i];
          if (s && s.type === 'chat' && s.taskName === name && s.valueType == null) return i;
        }
        return -1;
      })();
      const chatStep = chatIdx !== -1 ? prev[chatIdx] : undefined;
      if (chatStep) {
        const merged = [...prev];
        merged[chatIdx] = {
          ...chatStep,
          status: 'success',
          variables: { ...(chatStep.variables ?? {}), result },
          duration: durationMs,
          tokens,
          valueType: value_type ?? null,
          attempt: typeof attempt === 'number' ? attempt : undefined,
          seq: wireSeq ?? chatStep.seq,
          serverTs: wireServerTs ?? chatStep.serverTs,
        };
        steps = merged;
      } else {
        steps = [...prev, { id, line: activeLineRef.current || 0, type: 'execution', content: `Finished task: ${name}`, status: 'success', variables: { result }, duration: durationMs, timestamp, nodeId: activeNodeRef.current ?? undefined, visibility: 'inline', tokens, valueType: value_type ?? null, attempt: typeof attempt === 'number' ? attempt : undefined, seq: wireSeq, serverTs: wireServerTs }];
      }
      break;
    }
    case 'NodeEnd': {
      // Support object form { node_id, span, target_var, value, duration },
      // 2-tuple [nodeId, duration], and enriched array [nodeId, span, ..., duration].
      let nodeId: number;
      let durationMs: number;

      if (typeof evPayload === 'object' && !Array.isArray(evPayload) && 'node_id' in evPayload) {
        nodeId = evPayload.node_id;
        const dur = evPayload.duration;
        durationMs = dur.secs * 1000 + dur.nanos / 1000000;
      } else if (Array.isArray(evPayload)) {
        if (evPayload.length === 2) {
          const [nid, duration] = evPayload;
          nodeId = nid;
          durationMs = duration.secs * 1000 + duration.nanos / 1000000;
        } else {
          nodeId = evPayload[0];
          const duration = evPayload[evPayload.length - 1];
          durationMs = duration.secs * 1000 + duration.nanos / 1000000;
        }
      } else {
        steps = prev;
        break;
      }

      steps = prev.map(s => s.nodeId === nodeId && s.content.startsWith('Executing Node') ? { ...s, status: 'success' as const, duration: durationMs, seq: wireSeq ?? s.seq, serverTs: wireServerTs ?? s.serverTs } : s);
      break;
    }
    case 'WorkflowEnd':
      effects.executionFinished = true;
      effects.refreshHistory = true;
      steps = [...prev, { id, line: activeLineRef.current || 0, type: 'execution', content: 'Workflow completed', status: 'success', variables: { final_result: evPayload }, timestamp, nodeId: activeNodeRef.current ?? undefined, visibility: 'inline', seq: wireSeq, serverTs: wireServerTs }];
      break;
    case 'Error': {
      effects.executionFinished = true;
      effects.refreshHistory = true;
      // Error payload can be a bare string (legacy) or the structured
      // envelope `{ message, kind, code, user_message, retry_after_ms,
      // source }`. We forward every field present so the UI doesn't
      // have to re-derive them.
      let message: string;
      let kind: string | undefined;
      let code: string | undefined;
      let userMessage: string | undefined;
      let retryAfterMs: number | undefined;
      let source: ExecutionStep['errorSource'] | undefined;
      if (typeof evPayload === 'string') {
        message = evPayload;
      } else if (evPayload && typeof evPayload === 'object') {
        message = typeof evPayload.message === 'string' ? evPayload.message : JSON.stringify(evPayload);
        kind = typeof evPayload.kind === 'string' ? evPayload.kind : undefined;
        code = typeof evPayload.code === 'string' ? evPayload.code : undefined;
        userMessage = typeof evPayload.user_message === 'string' ? evPayload.user_message : undefined;
        retryAfterMs = typeof evPayload.retry_after_ms === 'number' ? evPayload.retry_after_ms : undefined;
        if (evPayload.source && typeof evPayload.source === 'object') {
          const s = evPayload.source as Record<string, unknown>;
          source = {
            task: typeof s.task === 'string' ? s.task : undefined,
            agent: typeof s.agent === 'string' ? s.agent : undefined,
            provider: typeof s.provider === 'string' ? s.provider : undefined,
            model: typeof s.model === 'string' ? s.model : undefined,
            toolRef: typeof s.tool_ref === 'string' ? s.tool_ref : undefined,
            script: typeof s.script === 'string' ? s.script : undefined,
            line: typeof s.line === 'number' ? s.line : undefined,
          };
        }
      } else {
        message = String(evPayload);
      }
      steps = [...prev, {
        id,
        line: activeLineRef.current || 0,
        type: 'execution',
        content: `Error: ${message}`,
        status: 'error',
        timestamp,
        nodeId: activeNodeRef.current ?? undefined,
        visibility: 'inline',
        errorKind: kind,
        errorCode: code,
        errorUserMessage: userMessage,
        errorRetryAfterMs: retryAfterMs,
        errorSource: source,
        seq: wireSeq,
        serverTs: wireServerTs,
      }];
      break;
    }
    case 'TaskStart': {
      const [name] = evPayload;
      steps = [...prev, { id, line: activeLineRef.current || 0, type: 'execution', content: `Starting task: ${name}`, status: 'running', timestamp, nodeId: activeNodeRef.current ?? undefined, visibility: 'panel-only', seq: wireSeq, serverTs: wireServerTs }];
      break;
    }
    case 'TaskCacheHit': {
      // P3: the engine emits `TaskCacheHit { agent, key_prefix }` right
      // before the cached `AgentOutput` + `TaskEnd` arrive. Find the
      // most recent open chat step for this agent and flip its
      // `cached` flag so the renderer can show a "cached" pill before
      // the row settles. The subsequent `TaskEnd` merges into the
      // same step and spreads the rest of the fields without touching
      // `cached` — so the flag survives end-to-end.
      const agent = (evPayload && typeof evPayload === 'object' && 'agent' in evPayload)
        ? String((evPayload as { agent: unknown }).agent)
        : '';
      // Walk newest-first: the most recently-opened chat row for this
      // agent on the active node is the one the upcoming TaskEnd will
      // fold into. We match on `taskName === agent` (the TaskPrompt
      // arm seeds the chat step with `taskName = name` where `name`
      // is the task identifier — which is also what the engine emits
      // as `agent` on the cache-hit event for an agent-bound task).
      let hitIdx = -1;
      for (let i = prev.length - 1; i >= 0; i -= 1) {
        const s = prev[i];
        if (s && s.type === 'chat' && (s.taskName === agent || s.agent === agent)) {
          hitIdx = i;
          break;
        }
      }
      const hit = hitIdx !== -1 ? prev[hitIdx] : undefined;
      if (!hit) {
        // Defensive: replay edge cases (e.g. event arrives before its
        // TaskPrompt) shouldn't produce a stray step. Forward-only.
        steps = prev;
      } else {
        const updated = [...prev];
        updated[hitIdx] = { ...hit, cached: true };
        steps = updated;
      }
      break;
    }
    case 'ValidationFailure': {
      // Workstream 04 §A.4: structured payload from `EngineEvent::ValidationFailure`.
      // `ValidationFailureCard` consumes `step.validationFailure` directly; the
      // detail page re-renders the same card expanded.
      const p = evPayload as {
        task_name: string;
        attempt: number;
        model_response: string;
        missing_fields: string[];
        extra_fields: string[];
        type_errors: string[];
        stop_reason: string | null;
      };
      steps = [...prev, {
        id,
        line: activeLineRef.current || 0,
        type: 'validation_failure',
        content: `Validation failed on ${p.task_name} (attempt ${p.attempt})`,
        status: 'error',
        timestamp,
        seq: wireSeq,
        serverTs: wireServerTs,
        nodeId: activeNodeRef.current ?? undefined,
        visibility: 'inline',
        validationFailure: {
          taskName: p.task_name,
          attempt: p.attempt,
          modelResponse: p.model_response,
          missingFields: p.missing_fields,
          extraFields: p.extra_fields,
          typeErrors: p.type_errors,
          stopReason: p.stop_reason ?? null,
        },
      }];
      break;
    }
    case 'Suspended': {
      const payloadObj: Record<string, unknown> | null =
        evPayload && typeof evPayload === 'object' && !Array.isArray(evPayload)
          ? (evPayload as Record<string, unknown>)
          : null;
      const checkpointName: string = payloadObj
        ? (typeof payloadObj.checkpoint_name === 'string' ? payloadObj.checkpoint_name : '')
        : (Array.isArray(evPayload) && typeof evPayload[0] === 'string' ? evPayload[0] : '');
      const suspendToken: string | undefined =
        payloadObj && typeof payloadObj.token === 'string' ? payloadObj.token : undefined;
      const suspendPrompt: string | undefined =
        payloadObj && typeof payloadObj.prompt === 'string' ? payloadObj.prompt : undefined;
      const suspendSchema: unknown = payloadObj ? payloadObj.schema : undefined;
      const trigger = payloadObj
        ? normalizeSuspendTrigger(payloadObj.trigger)
        : { kind: 'DagPosition' as const };
      const exhausted = trigger.kind === 'ValidationExhausted'
        ? (trigger as Extract<typeof trigger, { kind: 'ValidationExhausted' }>)
        : null;
      const step: ExecutionStep = {
        id,
        line: activeLineRef.current || 0,
        type: 'execution',
        content: exhausted
          ? `Validation exhausted after ${exhausted.retryCount} attempts on '${exhausted.taskName}' — suspended at ${checkpointName}`
          : `Suspended at checkpoint: ${checkpointName}`,
        status: 'pending',
        timestamp,
        seq: wireSeq,
        serverTs: wireServerTs,
        nodeId: activeNodeRef.current ?? undefined,
        visibility: 'inline',
        checkpointName,
        suspendTrigger: trigger,
        suspendToken,
        suspendPrompt,
        suspendSchema,
      };
      if (exhausted) {
        step.retryCount = exhausted.retryCount;
        step.lastAttempt = exhausted.lastAttempt;
        step.validationErrors = exhausted.validationErrors;
      }
      steps = [...prev, step];
      break;
    }
    case 'Resumed': {
      const payloadObj: Record<string, unknown> | null =
        evPayload && typeof evPayload === 'object' && !Array.isArray(evPayload)
          ? (evPayload as Record<string, unknown>)
          : null;
      const checkpointName: string = payloadObj
        ? (typeof payloadObj.checkpoint_name === 'string' ? payloadObj.checkpoint_name : '')
        : (Array.isArray(evPayload) && typeof evPayload[0] === 'string' ? evPayload[0] : '');
      steps = [...prev, { id, line: activeLineRef.current || 0, type: 'execution', content: `Resumed from checkpoint: ${checkpointName}`, status: 'running', timestamp, nodeId: activeNodeRef.current ?? undefined, visibility: 'inline', seq: wireSeq, serverTs: wireServerTs }];
      break;
    }
    case 'Breakpoint': {
      const { node_id, span, token, env_snapshot } = evPayload;
      activeLineRef.current = span.line;
      activeNodeRef.current = node_id;
      effects.setActiveLine = span.line;
      effects.breakpoint = { nodeId: node_id, token, envSnapshot: env_snapshot, line: span.line };
      steps = [...prev, { id, line: span.line, type: 'execution', content: `Paused at breakpoint (line ${span.line})`, status: 'running', variables: env_snapshot, timestamp, nodeId: node_id, visibility: 'inline', seq: wireSeq, serverTs: wireServerTs }];
      break;
    }
    case 'BreakpointResumed': {
      const { node_id } = evPayload;
      effects.breakpointResumed = true;
      steps = prev.map(s => s.nodeId === node_id && s.content.startsWith('Paused at breakpoint') ? { ...s, content: `Resumed from breakpoint (line ${s.line})`, status: 'success' as const, seq: wireSeq ?? s.seq, serverTs: wireServerTs ?? s.serverTs } : s);
      break;
    }
    case 'Log': {
      const message = typeof evPayload === 'string' ? evPayload : JSON.stringify(evPayload);
      steps = [...prev, { id, line: activeLineRef.current || 0, type: 'execution', content: message, status: 'success', timestamp, nodeId: activeNodeRef.current ?? undefined, visibility: 'inline', seq: wireSeq, serverTs: wireServerTs }];
      break;
    }
    case 'ToolCallStart': {
      const step: ExecutionStep = {
        id,
        line: activeLineRef.current || 0,
        type: 'tool_call',
        content: `Calling ${evPayload.tool_name}...`,
        status: 'running',
        timestamp,
        seq: wireSeq,
        serverTs: wireServerTs,
        toolName: evPayload.tool_name,
        serverName: evPayload.server_name,
        toolInput: evPayload.input,
        nodeId: activeNodeRef.current ?? undefined,
        visibility: 'panel-only',
      };
      steps = [...prev, step];
      break;
    }
    case 'SubScript': {
      // Cross-script `call(...)` envelope (akribes-core EngineEvent::SubScript,
      // PR #360). Each event wraps ONE inner sub-engine event; one logical
      // call therefore arrives as a stream of SubScript envelopes that share
      // the same `parent_task`.
      //
      // Strategy:
      //   * Maintain ONE `sub_script` step per (parent_task, currently open
      //     call). The first envelope for a parent_task with no open call
      //     creates the step; subsequent envelopes accumulate into that step
      //     until the wrapped child event is `WorkflowEnd` (or `Error`),
      //     which closes the call. A later envelope with the same
      //     `parent_task` — e.g. the same variable being assigned a fresh
      //     `call(...)` later in the workflow — opens a new step.
      //
      //   * Nested calls (depth > 1, i.e. A → B → C): the engine wraps each
      //     level as a `SubScript` envelope, so a depth-2 grandchild event
      //     arrives as `SubScript { child: SubScript { child: <leaf> } }`.
      //     `unwrapSubScriptChain` peels off the wrappers into a frame stack
      //     `[A, B, C]`. We then walk into the open A-step's `children`,
      //     find/open the open B-step, walk into ITS `children`, and apply
      //     the leaf effects on the innermost (C) step. The outermost
      //     `SubScriptCard` recursively renders nested cards from `children`.
      //
      //   * Token rollup: each wrapped `TaskEnd` reports usage at the level
      //     that hosts the task (so C's `TaskEnd` lands on the C step). We
      //     ALSO add it to every ancestor's `subScriptTokens` so the
      //     outermost card keeps showing the run-wide total — preserving the
      //     existing v1 "tokens still aggregate correctly" guarantee even
      //     across nesting. `nestedTaskCount` is rolled up the same way.
      //
      // Pricing: token totals accumulate; USD is intentionally NOT computed
      // client-side. See the note on `SubScriptTokens` for the rationale.
      const chain = unwrapSubScriptChain(evPayload);
      if (!chain) {
        steps = prev;
        break;
      }
      const { frames, effects } = chain;
      const outermostFrame = frames[0]!;

      // Walk the chain top-down: at each level, locate (or create) the open
      // sub-script step matching that level's `parent_task`. The leaf-level
      // effects are applied to the innermost frame. Token usage is folded
      // into every ancestor's totals via the explicit
      // `subScriptTokens = selfTokens + Σ child.subScriptTokens` invariant —
      // see `combineSelfAndChildrenTokens`.
      const updateFrame = (
        steps: ExecutionStep[],
        frameIndex: number,
      ): ExecutionStep[] => {
        const frame = frames[frameIndex]!;
        const isLeaf = frameIndex === frames.length - 1;
        const openIdx = (() => {
          for (let i = steps.length - 1; i >= 0; i -= 1) {
            const s = steps[i];
            if (
              s
              && s.type === 'sub_script'
              && s.subScript?.parentTask === frame.parentTask
              && !s.subScript?.closed
            ) {
              return i;
            }
          }
          return -1;
        })();

        if (openIdx === -1) {
          // First envelope for this call at this level — create the step.
          // For a non-leaf frame (an outer level on a nested chain), the
          // creation happens here on the recursion's way down; the leaf
          // effects are applied below when `isLeaf` is true.
          let childrenSteps: ExecutionStep[] = [];
          if (!isLeaf) {
            childrenSteps = updateFrame([], frameIndex + 1);
          }
          // selfTokens: only set when a leaf TaskEnd lands at this level on
          // creation (rare — usually the level was opened by an upstream
          // event before its first TaskEnd). Children's tokens are NEVER
          // folded into selfTokens, only into the recursive total.
          const selfTokens = isLeaf && effects.maybeUsage
            ? aggregateSubScriptTokens(undefined, effects.maybeUsage)
            : undefined;
          // Apply AgentOutput streaming buffer at the leaf level on creation.
          let streamingBuffer: Record<string, string> | undefined;
          if (isLeaf && effects.maybeAgentOutput) {
            streamingBuffer = {
              [effects.maybeAgentOutput.taskName]: effects.maybeAgentOutput.chunk,
            };
          }
          // Attach the in-flight streaming text to the new task summary if
          // this creation event IS a TaskEnd.
          const newSummary = isLeaf
            ? attachStreamingToSummary(effects.maybeTaskSummary, streamingBuffer)
            : undefined;
          // Drop the streaming entry once consumed by its TaskEnd so the
          // buffer doesn't grow unbounded across a long-running sub-script.
          if (newSummary && newSummary.kind === 'task_end' && streamingBuffer) {
            const { [newSummary.taskName]: _drop, ...rest } = streamingBuffer;
            streamingBuffer = Object.keys(rest).length > 0 ? rest : undefined;
          }
          const taskCountForThisLevel = isLeaf
            ? (selfTokens ? 1 : 0)
            : rollupTaskCountFromChildren(childrenSteps);
          const next: ExecutionStep = {
            id: `${id}-l${frameIndex}`,
            line: activeLineRef.current || 0,
            type: 'sub_script',
            content: `call("${frame.scriptName}")`,
            status: isLeaf
              ? effects.childIsTerminal
                ? effects.childIsError
                  ? 'error'
                  : 'success'
                : 'running'
              : 'running',
            timestamp,
            seq: wireSeq,
            serverTs: wireServerTs,
            nodeId: activeNodeRef.current ?? undefined,
            visibility: 'inline',
            subScript: {
              scriptName: frame.scriptName,
              parentTask: frame.parentTask,
              inputs: isLeaf && effects.maybeInput ? [effects.maybeInput] : [],
              output: isLeaf ? effects.maybeOutput : undefined,
              subScriptTokens: combineSelfAndChildrenTokens(selfTokens, childrenSteps),
              selfTokens,
              nestedTaskCount: taskCountForThisLevel,
              closed: isLeaf ? effects.childIsTerminal : false,
              taskSummaries: newSummary ? [newSummary] : undefined,
              children: isLeaf ? undefined : childrenSteps,
              _streamingByTask: streamingBuffer,
            },
          };
          return [...steps, next];
        }

        const cur = steps[openIdx]!;
        const curSub = cur.subScript!;
        let nextChildren = curSub.children;
        if (!isLeaf) {
          nextChildren = updateFrame(curSub.children ?? [], frameIndex + 1);
        }
        // selfTokens accumulates ONLY at the leaf level. Ancestors keep
        // their existing selfTokens unchanged; their `subScriptTokens` is
        // re-derived from `selfTokens + children` so the descendant update
        // bubbles up cleanly without any subtraction-based bookkeeping.
        const nextSelfTokens = isLeaf && effects.maybeUsage
          ? aggregateSubScriptTokens(curSub.selfTokens, effects.maybeUsage)
          : curSub.selfTokens;
        // Update the streaming buffer for this leaf if AgentOutput arrived.
        let nextStreaming = curSub._streamingByTask;
        if (isLeaf && effects.maybeAgentOutput) {
          const { taskName, chunk } = effects.maybeAgentOutput;
          nextStreaming = {
            ...(nextStreaming ?? {}),
            [taskName]: (nextStreaming?.[taskName] ?? '') + chunk,
          };
        }
        // Build the (possibly enriched) new summary entry.
        const enrichedSummary = isLeaf
          ? attachStreamingToSummary(effects.maybeTaskSummary, nextStreaming)
          : undefined;
        // Drop the streaming entry from the buffer once it has been
        // attached to a finalized `task_end` summary; keeps memory flat.
        if (enrichedSummary && enrichedSummary.kind === 'task_end' && nextStreaming) {
          const { [enrichedSummary.taskName]: _drop, ...rest } = nextStreaming;
          nextStreaming = Object.keys(rest).length > 0 ? rest : undefined;
        }
        const updatedSub: NonNullable<ExecutionStep['subScript']> = {
          ...curSub,
          inputs: isLeaf && effects.maybeInput
            ? [...curSub.inputs.filter((i) => i.name !== effects.maybeInput!.name), effects.maybeInput]
            : curSub.inputs,
          output: isLeaf && effects.maybeOutput !== undefined ? effects.maybeOutput : curSub.output,
          selfTokens: nextSelfTokens,
          subScriptTokens: combineSelfAndChildrenTokens(
            nextSelfTokens,
            isLeaf ? curSub.children : nextChildren,
          ),
          nestedTaskCount: isLeaf
            ? (curSub.nestedTaskCount ?? 0) + (effects.maybeUsage ? 1 : 0)
            : (curSub.nestedTaskCount ?? 0)
              - rollupTaskCountFromChildren(curSub.children ?? [])
              + rollupTaskCountFromChildren(nextChildren ?? []),
          closed: isLeaf ? (curSub.closed || effects.childIsTerminal) : curSub.closed,
          taskSummaries: enrichedSummary
            ? [...(curSub.taskSummaries ?? []), enrichedSummary]
            : curSub.taskSummaries,
          children: isLeaf ? curSub.children : nextChildren,
          _streamingByTask: nextStreaming,
        };
        const updated: ExecutionStep = {
          ...cur,
          status: isLeaf
            ? (effects.childIsTerminal
                ? (effects.childIsError ? 'error' : 'success')
                : cur.status)
            : cur.status,
          seq: wireSeq ?? cur.seq,
          serverTs: wireServerTs ?? cur.serverTs,
          subScript: updatedSub,
        };
        const next = [...steps];
        next[openIdx] = updated;
        return next;
      };

      // Sanity: outermost frame must be at the top level of `steps`.
      void outermostFrame; // silence unused-var linter when no debugger active
      steps = updateFrame(prev, 0);
      break;
    }
    case 'LoopStart': {
      // Open a new loop step. Subsequent `LoopTurn`s and the terminal
      // `LoopEnd` will fold back into this step keyed by `loopName`. The
      // step starts with an empty `turns` array; the panel renders a
      // "running…" pulse until LoopEnd flips status to success/error.
      const p = evPayload as { name?: unknown; max_turns?: unknown };
      const loopName = typeof p.name === 'string' ? p.name : '';
      const maxTurns = typeof p.max_turns === 'number' ? p.max_turns : 0;
      steps = [...prev, {
        id,
        line: activeLineRef.current || 0,
        type: 'loop',
        content: `loop ${loopName}`,
        status: 'running',
        timestamp,
        seq: wireSeq,
        serverTs: wireServerTs,
        nodeId: activeNodeRef.current ?? undefined,
        visibility: 'inline',
        loopName,
        maxTurns,
        turns: [],
      }];
      break;
    }
    case 'LoopTurn': {
      // Append a turn summary to the most-recent open (status === 'running')
      // loop step with the matching `loopName`. We scan from the back so a
      // later loop with a name that happens to collide with an earlier one
      // (sequential loops in the same workflow) hits the right step. If no
      // open loop is found we silently no-op — better than mis-attributing.
      const p = evPayload as { name?: unknown; turn?: unknown; tool_calls?: unknown };
      const loopName = typeof p.name === 'string' ? p.name : '';
      const turn = typeof p.turn === 'number' ? p.turn : 0;
      const toolCalls = Array.isArray(p.tool_calls)
        ? p.tool_calls.filter((t): t is string => typeof t === 'string')
        : [];
      const idx = (() => {
        for (let i = prev.length - 1; i >= 0; i -= 1) {
          const s = prev[i];
          if (s && s.type === 'loop' && s.loopName === loopName && s.status === 'running') return i;
        }
        return -1;
      })();
      const cur = idx !== -1 ? prev[idx] : undefined;
      if (!cur) {
        steps = prev;
        break;
      }
      const next = [...prev];
      next[idx] = {
        ...cur,
        turns: [...(cur.turns ?? []), { turn, toolCalls }],
        seq: wireSeq ?? cur.seq,
        serverTs: wireServerTs ?? cur.serverTs,
      };
      steps = next;
      break;
    }
    case 'LoopEnd': {
      // Finalize the matching loop step. Status is 'error' when the value is
      // a `Value::FatalError` envelope (max_turns exhaustion) and 'success'
      // otherwise. The full `value` is carried as `loopResult` so the UI can
      // render it via `AkribesValueViewerWithRawToggle`.
      //
      // Wire shape: `Value` is serialised via `Value::to_wire_json` per the
      // contract in `docs/src/content/docs/reference/engine-events.mdx` —
      // scalars (`Value::String`, `Value::Int`, `Value::Bool`) emit bare JSON
      // values, `Value::Object`/`Value::List` emit clean JSON containers, and
      // `Value::FatalError` emits a `{ "FatalError": <msg>, "error_kind": ...,
      // "code": ..., "error_detail": { ... } }` envelope. We detect the
      // FatalError arm by structural shape so future additions to the
      // FatalError wire envelope (e.g. extending `error_detail`) don't break
      // the check.
      const p = evPayload as { name?: unknown; turn_count?: unknown; value?: unknown };
      const loopName = typeof p.name === 'string' ? p.name : '';
      const value = p.value;
      const isFatal = !!value
        && typeof value === 'object'
        && !Array.isArray(value)
        && 'FatalError' in (value as Record<string, unknown>);
      const idx = (() => {
        for (let i = prev.length - 1; i >= 0; i -= 1) {
          const s = prev[i];
          if (s && s.type === 'loop' && s.loopName === loopName && s.status === 'running') return i;
        }
        return -1;
      })();
      const cur = idx !== -1 ? prev[idx] : undefined;
      if (!cur) {
        steps = prev;
        break;
      }
      const next = [...prev];
      next[idx] = {
        ...cur,
        status: isFatal ? 'error' : 'success',
        loopResult: value,
        seq: wireSeq ?? cur.seq,
        serverTs: wireServerTs ?? cur.serverTs,
      };
      steps = next;
      break;
    }
    case 'ToolCallEnd': {
      const toolStepIndex = prev.findLastIndex(
        (s: ExecutionStep) => s.type === 'tool_call' && s.toolName === evPayload.tool_name && s.status === 'running'
      );
      const toolStep = toolStepIndex >= 0 ? prev[toolStepIndex] : undefined;
      if (toolStep) {
        const updated = [...prev];
        const durationMs = evPayload.duration
          ? evPayload.duration.secs * 1000 + evPayload.duration.nanos / 1000000
          : undefined;
        updated[toolStepIndex] = {
          ...toolStep,
          status: 'success' as const,
          content: `${evPayload.tool_name} completed`,
          toolOutput: evPayload.output,
          duration: durationMs,
          seq: wireSeq ?? toolStep.seq,
          serverTs: wireServerTs ?? toolStep.serverTs,
        };
        steps = updated;
      } else {
        steps = prev;
      }
      break;
    }
    default:
      steps = prev;
  }

  return { steps, effects };
}

/**
 * Helper to build run-from-line parameters from a previous execution's steps.
 * Returns null if there's no previous execution to build from.
 */
export function buildRunFromParams(
  executionSteps: ExecutionStep[],
  targetLine: number,
): { seedEnv: Record<string, unknown>; skipNodeIds: number[] } | null {
  if (executionSteps.length === 0) return null;

  const upstreamSteps = executionSteps.filter(
    s => s.line < targetLine && s.line > 0 && s.nodeId != null && s.status === 'success'
  );

  if (upstreamSteps.length === 0) return null;

  const seedEnv: Record<string, unknown> = {};
  const skipNodeIds = new Set<number>();

  for (const step of upstreamSteps) {
    if (step.nodeId != null) {
      skipNodeIds.add(step.nodeId);
    }
    if (step.variables) {
      Object.assign(seedEnv, step.variables);
    }
  }

  return {
    seedEnv,
    skipNodeIds: Array.from(skipNodeIds),
  };
}
