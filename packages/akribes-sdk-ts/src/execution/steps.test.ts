import { test, expect } from "bun:test";
import { reduceExecutionEvent } from "./steps";
import type { ActiveLineRef, ActiveNodeRef } from "./steps";
import type { HubEvent } from "../types";

// ── helpers ───────────────────────────────────────────────────────────────────

function makeActiveLineRef(line: number | null = null): ActiveLineRef {
  return { current: line };
}

function makeActiveNodeRef(nodeId: number | null = null): ActiveNodeRef {
  return { current: nodeId };
}

function makeTaskEndEvent(overrides: Record<string, unknown> = {}): HubEvent {
  return {
    type: "Execution",
    payload: {
      project_id: 1,
      script_name: "test.akr",
      event: {
        type: "TaskEnd",
        payload: {
          task: "summarize",
          on_error_label: null,
          value: "hello world",
          value_type: null,
          duration: { secs: 1, nanos: 0 },
          attempt: 1,
          usage: null,
          ...overrides,
        },
      },
    },
  };
}

// ── TaskEnd reducer tests ────────────────────────────────────────────────────

test("TaskEnd with value_type and attempt > 1 produces step with valueType and attempt", () => {
  const lineRef = makeActiveLineRef(5);
  const nodeRef = makeActiveNodeRef(2);

  const evt = makeTaskEndEvent({
    value: "some result",
    value_type: { name: "markdown", inner: null, choices: null },
    attempt: 3,
  });

  const { steps } = reduceExecutionEvent([], evt, lineRef, nodeRef);

  expect(steps).toHaveLength(1);
  const step = steps[0];
  expect(step.valueType).toEqual({ name: "markdown", inner: null, choices: null });
  expect(step.attempt).toBe(3);
  expect(step.status).toBe("success");
  expect(step.content).toBe("Finished task: summarize");
});

test("TaskEnd with choice value_type (choices array) produces step with valueType.choices", () => {
  const lineRef = makeActiveLineRef(3);
  const nodeRef = makeActiveNodeRef(1);

  const evt = makeTaskEndEvent({
    value: "low",
    value_type: { name: "choice", inner: null, choices: ["low", "med", "high"] },
    attempt: 1,
  });

  const { steps } = reduceExecutionEvent([], evt, lineRef, nodeRef);

  expect(steps).toHaveLength(1);
  const step = steps[0];
  expect(step.valueType?.choices).toEqual(["low", "med", "high"]);
  expect(step.attempt).toBe(1);
});

test("TaskEnd without value_type produces step with valueType null", () => {
  const lineRef = makeActiveLineRef(1);
  const nodeRef = makeActiveNodeRef(0);

  const evt = makeTaskEndEvent({ value_type: null, attempt: 1 });

  const { steps } = reduceExecutionEvent([], evt, lineRef, nodeRef);

  expect(steps).toHaveLength(1);
  expect(steps[0].valueType).toBeNull();
  expect(steps[0].attempt).toBe(1);
});

test("TaskEnd preserves variables.result from value", () => {
  const lineRef = makeActiveLineRef(2);
  const nodeRef = makeActiveNodeRef(1);

  const evt = makeTaskEndEvent({ value: { foo: "bar" }, attempt: 1 });

  const { steps } = reduceExecutionEvent([], evt, lineRef, nodeRef);

  expect(steps[0].variables?.result).toEqual({ foo: "bar" });
});

test("TaskEnd merges into the streaming chat step instead of duplicating it", () => {
  // Regression: the studio panel was rendering the streamed agent output AND
  // the parsed structured result as two separate rows with the same
  // timestamp. AgentOutput chunks arrive first; TaskEnd should fold its
  // value/value_type into the existing chat step.
  const lineRef = makeActiveLineRef(860);
  const nodeRef = makeActiveNodeRef(42);

  const agentOutput: HubEvent = {
    type: "Execution",
    payload: {
      project_id: 1,
      script_name: "test.akr",
      event: {
        type: "AgentOutput",
        payload: {
          task_name: "Feature_Mapper",
          agent_name: "Feature_Mapper",
          task_id: "tid-feature-1",
          schema_type: "FeatureList_Draft_1",
          chunk: "| # | feature_description | feature_number |\n",
        },
      },
    },
  };
  const taskEnd = makeTaskEndEvent({
    task: "Feature_Mapper",
    value: { features: [{ feature_description: "x", feature_number: "C1_F1" }] },
    value_type: { name: "FeatureList_Draft_1", inner: null, choices: null },
    duration: { secs: 6, nanos: 610_000_000 },
  });

  const after1 = reduceExecutionEvent([], agentOutput, lineRef, nodeRef);
  const after2 = reduceExecutionEvent(after1.steps, taskEnd, lineRef, nodeRef);

  expect(after2.steps).toHaveLength(1);
  const merged = after2.steps[0]!;
  expect(merged.type).toBe("chat");
  expect(merged.taskName).toBe("Feature_Mapper");
  expect(merged.valueType?.name).toBe("FeatureList_Draft_1");
  expect(merged.variables?.result).toEqual({
    features: [{ feature_description: "x", feature_number: "C1_F1" }],
  });
  expect(merged.status).toBe("success");
  expect(merged.duration).toBe(6610);
});

// ── TaskCacheHit reducer tests (P3) ───────────────────────────────────────────

test("TaskCacheHit after TaskPrompt marks the streaming chat step as cached", () => {
  // P3 wire flow on a cache hit:
  //   TaskPrompt → TaskCacheHit → AgentOutput("Result served from cache.") → TaskEnd
  // The reducer must flip the chat row's `cached` flag the moment the
  // TaskCacheHit arrives so the UI can render the badge before TaskEnd.
  const lineRef = makeActiveLineRef(12);
  const nodeRef = makeActiveNodeRef(3);

  const taskPrompt: HubEvent = {
    type: "Execution",
    payload: {
      project_id: 1,
      script_name: "test.akr",
      event: {
        type: "TaskPrompt",
        payload: ["speak", "Say hi"],
      },
    },
  };
  const cacheHit: HubEvent = {
    type: "Execution",
    payload: {
      project_id: 1,
      script_name: "test.akr",
      event: {
        type: "TaskCacheHit",
        payload: { agent: "speak", key_prefix: "abc123" },
      },
    },
  };

  const after1 = reduceExecutionEvent([], taskPrompt, lineRef, nodeRef);
  expect(after1.steps).toHaveLength(1);
  expect(after1.steps[0]!.cached).toBeFalsy();

  const after2 = reduceExecutionEvent(after1.steps, cacheHit, lineRef, nodeRef);
  expect(after2.steps).toHaveLength(1);
  const step = after2.steps[0]!;
  expect(step.type).toBe("chat");
  expect(step.cached).toBe(true);
});

test("TaskCacheHit's cached flag survives the TaskEnd merge", () => {
  // Once the chat step is marked cached, the subsequent TaskEnd merge
  // must preserve the flag (rather than spread it away).
  const lineRef = makeActiveLineRef(20);
  const nodeRef = makeActiveNodeRef(7);

  const taskPrompt: HubEvent = {
    type: "Execution",
    payload: {
      project_id: 1,
      script_name: "test.akr",
      event: { type: "TaskPrompt", payload: ["classify", "Classify input"] },
    },
  };
  const cacheHit: HubEvent = {
    type: "Execution",
    payload: {
      project_id: 1,
      script_name: "test.akr",
      event: {
        type: "TaskCacheHit",
        payload: { agent: "classify", key_prefix: "deadbe" },
      },
    },
  };
  const taskEnd = makeTaskEndEvent({
    task: "classify",
    value: { category: "spam" },
    value_type: { name: "Verdict", inner: null, choices: null },
  });

  const s1 = reduceExecutionEvent([], taskPrompt, lineRef, nodeRef);
  const s2 = reduceExecutionEvent(s1.steps, cacheHit, lineRef, nodeRef);
  const s3 = reduceExecutionEvent(s2.steps, taskEnd, lineRef, nodeRef);

  expect(s3.steps).toHaveLength(1);
  const merged = s3.steps[0]!;
  expect(merged.cached).toBe(true);
  expect(merged.status).toBe("success");
  expect(merged.taskName).toBe("classify");
});

test("TaskCacheHit with no matching chat step is a no-op (defensive)", () => {
  // Defensive: if the reducer sees a TaskCacheHit before any TaskPrompt
  // (or for a different agent), it must not throw and must not append a
  // bogus step. Real wire ordering always pairs the two, but the reducer
  // is a pure function and must tolerate replay edge cases.
  const lineRef = makeActiveLineRef(1);
  const nodeRef = makeActiveNodeRef(0);

  const cacheHit: HubEvent = {
    type: "Execution",
    payload: {
      project_id: 1,
      script_name: "test.akr",
      event: {
        type: "TaskCacheHit",
        payload: { agent: "ghost", key_prefix: "000000" },
      },
    },
  };

  const { steps } = reduceExecutionEvent([], cacheHit, lineRef, nodeRef);
  expect(steps).toHaveLength(0);
});

test("TaskEnd appends a new step when no streaming chat step exists (stdlib path)", () => {
  // Stdlib calls don't emit AgentOutput, so TaskEnd should still produce a
  // standalone execution step.
  const lineRef = makeActiveLineRef(10);
  const nodeRef = makeActiveNodeRef(3);

  const evt = makeTaskEndEvent({ task: "stdlib_call" });
  const { steps } = reduceExecutionEvent([], evt, lineRef, nodeRef);

  expect(steps).toHaveLength(1);
  expect(steps[0].type).toBe("execution");
  expect(steps[0].content).toBe("Finished task: stdlib_call");
});

// ── Suspended (ValidationExhausted) reducer tests ────────────────────────────

function makeSuspendedEvent(
  trigger: unknown,
  checkpointName = "await_review",
  extra: Record<string, unknown> = {},
): HubEvent {
  return {
    type: "Execution",
    payload: {
      project_id: 1,
      script_name: "test.akr",
      event: {
        type: "Suspended",
        payload: { checkpoint_name: checkpointName, trigger, ...extra },
      },
    },
  };
}

test("Suspended with ValidationExhausted populates retryCount, lastAttempt, validationErrors", () => {
  const lineRef = makeActiveLineRef(12);
  const nodeRef = makeActiveNodeRef(4);

  const evt = makeSuspendedEvent({
    kind: "ValidationExhausted",
    task_name: "extract_claim",
    retry_count: 3,
    last_attempt: "{ \"claim\": \"...\" }",
    validation_errors: [
      { stage: "schema", message: "missing required field 'speaker'", path: "/speaker" },
      { stage: "custom:non_empty", message: "claim must not be empty", path: null },
    ],
  });

  const { steps } = reduceExecutionEvent([], evt, lineRef, nodeRef);

  expect(steps).toHaveLength(1);
  const step = steps[0]!;
  expect(step.status).toBe("pending");
  expect(step.retryCount).toBe(3);
  expect(step.lastAttempt).toBe("{ \"claim\": \"...\" }");
  expect(step.validationErrors).toEqual([
    { stage: "schema", message: "missing required field 'speaker'", path: "/speaker" },
    { stage: "custom:non_empty", message: "claim must not be empty", path: null },
  ]);
  expect(step.content).toContain("Validation exhausted after 3 attempts on 'extract_claim'");
  expect(step.content).toContain("await_review");
  expect(step.checkpointName).toBe("await_review");
  expect(step.suspendTrigger?.kind).toBe("ValidationExhausted");
});

test("Suspended with DagPosition trigger leaves validation fields undefined", () => {
  const lineRef = makeActiveLineRef(8);
  const nodeRef = makeActiveNodeRef(2);

  const evt = makeSuspendedEvent({ kind: "DagPosition" }, "manual_review");

  const { steps } = reduceExecutionEvent([], evt, lineRef, nodeRef);

  expect(steps).toHaveLength(1);
  const step = steps[0]!;
  expect(step.retryCount).toBeUndefined();
  expect(step.lastAttempt).toBeUndefined();
  expect(step.validationErrors).toBeUndefined();
  expect(step.content).toBe("Suspended at checkpoint: manual_review");
  expect(step.checkpointName).toBe("manual_review");
  expect(step.suspendTrigger?.kind).toBe("DagPosition");
});

test("Suspended captures token, prompt, and schema for resume", () => {
  const lineRef = makeActiveLineRef(15);
  const nodeRef = makeActiveNodeRef(5);

  const evt = makeSuspendedEvent(
    { kind: "DagPosition" },
    "await_review",
    {
      token: "tok-abc-123",
      prompt: "Please review the extracted claim",
      schema: { type: "object", properties: { approve: { type: "boolean" } } },
    },
  );

  const { steps } = reduceExecutionEvent([], evt, lineRef, nodeRef);

  const step = steps[0]!;
  expect(step.suspendToken).toBe("tok-abc-123");
  expect(step.suspendPrompt).toBe("Please review the extracted claim");
  expect(step.suspendSchema).toEqual({
    type: "object",
    properties: { approve: { type: "boolean" } },
  });
});

test("Suspended with AgentUnable trigger populates checkpointName + trigger.unable", () => {
  const lineRef = makeActiveLineRef(20);
  const nodeRef = makeActiveNodeRef(7);

  const evt = makeSuspendedEvent({
    kind: "AgentUnable",
    task_name: "classify",
    unable: {
      reason: "input too ambiguous to classify",
      missing: ["context"],
      category: "InputAmbiguous",
    },
  }, "needs_human");

  const { steps } = reduceExecutionEvent([], evt, lineRef, nodeRef);

  expect(steps).toHaveLength(1);
  const step = steps[0]!;
  expect(step.checkpointName).toBe("needs_human");
  expect(step.suspendTrigger?.kind).toBe("AgentUnable");
  if (step.suspendTrigger?.kind === "AgentUnable") {
    expect(step.suspendTrigger.taskName).toBe("classify");
    expect(step.suspendTrigger.unable.category).toBe("InputAmbiguous");
  }
});

// ── SubScript reducer (cross-script `call(...)` envelopes) ──────────────────

/**
 * Build a HubEvent wrapping a single `EngineEvent::SubScript` envelope
 * around an inner event. The shape mirrors what the engine emits — see
 * `crates/akribes-core/src/event.rs` and `engine.rs::execute_subscript`.
 */
function makeSubScriptEvent(scriptName: string, parentTask: string, child: { type: string; payload: unknown }): HubEvent {
  return {
    type: "Execution",
    payload: {
      project_id: 1,
      script_name: "outer.akr",
      execution_id: "exec-1",
      event: {
        type: "SubScript",
        payload: { script_name: scriptName, parent_task: parentTask, child },
      },
    },
  };
}

test("SubScript stream opens a sub_script step on first envelope and folds inputs/output/usage", () => {
  // Sub-engine emits, in order, for `out = call("greeter", who="ada")`:
  //   1. WorkflowStart(1)
  //   2. StateUpdate("who", "ada")          ← input hydration
  //   3. TaskStart("hello")
  //   4. TaskEnd { task: "hello", value: "hi ada", usage: {...}, ... }
  //   5. WorkflowEnd("hi ada")
  // Each is wrapped by the engine in a SubScript envelope and forwarded.
  const lineRef = makeActiveLineRef(7);
  const nodeRef = makeActiveNodeRef(3);

  const wfStart = makeSubScriptEvent("greeter", "out", { type: "WorkflowStart", payload: 1 });
  const stateUpd = makeSubScriptEvent("greeter", "out", { type: "StateUpdate", payload: ["who", "ada"] });
  const taskStart = makeSubScriptEvent("greeter", "out", { type: "TaskStart", payload: ["hello", null] });
  const taskEnd = makeSubScriptEvent("greeter", "out", {
    type: "TaskEnd",
    payload: {
      task: "hello",
      on_error_label: null,
      value: "hi ada",
      value_type: null,
      duration: { secs: 0, nanos: 500_000_000 },
      attempt: 1,
      usage: {
        input_tokens: 120,
        output_tokens: 8,
        cached_input_tokens: 0,
        model: "gpt-5",
        provider: "openai",
      },
    },
  });
  const wfEnd = makeSubScriptEvent("greeter", "out", { type: "WorkflowEnd", payload: "hi ada" });

  let { steps } = reduceExecutionEvent([], wfStart, lineRef, nodeRef);
  ({ steps } = reduceExecutionEvent(steps, stateUpd, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, taskStart, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, taskEnd, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, wfEnd, lineRef, nodeRef));

  // Exactly one sub_script step folded the whole stream.
  const subSteps = steps.filter((s) => s.type === "sub_script");
  expect(subSteps).toHaveLength(1);
  const ss = subSteps[0]!;
  expect(ss.content).toBe('call("greeter")');
  expect(ss.status).toBe("success");
  expect(ss.subScript?.scriptName).toBe("greeter");
  expect(ss.subScript?.parentTask).toBe("out");
  expect(ss.subScript?.inputs).toEqual([{ name: "who", value: "ada" }]);
  expect(ss.subScript?.output).toBe("hi ada");
  expect(ss.subScript?.subScriptTokens).toEqual({
    input: 120,
    output: 8,
    cachedInput: 0,
    models: ["gpt-5"],
  });
  expect(ss.subScript?.nestedTaskCount).toBe(1);
  expect(ss.subScript?.closed).toBe(true);
});

test("Two sequential calls with the same parent_task produce two distinct sub_script steps", () => {
  // After the first call closes (WorkflowEnd), the second call's first
  // envelope must open a fresh step rather than re-opening the closed one.
  const lineRef = makeActiveLineRef(1);
  const nodeRef = makeActiveNodeRef(1);

  const callA_start = makeSubScriptEvent("greeter", "out", { type: "WorkflowStart", payload: 1 });
  const callA_end = makeSubScriptEvent("greeter", "out", { type: "WorkflowEnd", payload: "first" });
  const callB_start = makeSubScriptEvent("greeter", "out", { type: "WorkflowStart", payload: 1 });
  const callB_end = makeSubScriptEvent("greeter", "out", { type: "WorkflowEnd", payload: "second" });

  let { steps } = reduceExecutionEvent([], callA_start, lineRef, nodeRef);
  ({ steps } = reduceExecutionEvent(steps, callA_end, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, callB_start, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, callB_end, lineRef, nodeRef));

  const subSteps = steps.filter((s) => s.type === "sub_script");
  expect(subSteps).toHaveLength(2);
  expect(subSteps[0]!.subScript?.output).toBe("first");
  expect(subSteps[0]!.subScript?.closed).toBe(true);
  expect(subSteps[1]!.subScript?.output).toBe("second");
  expect(subSteps[1]!.subScript?.closed).toBe(true);
});

test("SubScript Error envelope marks the step status='error' and closes it", () => {
  const lineRef = makeActiveLineRef(1);
  const nodeRef = makeActiveNodeRef(1);

  const wfStart = makeSubScriptEvent("flaky", "x", { type: "WorkflowStart", payload: 1 });
  const errEvt = makeSubScriptEvent("flaky", "x", {
    type: "Error",
    payload: { message: "rate-limited", kind: "RateLimit" },
  });

  let { steps } = reduceExecutionEvent([], wfStart, lineRef, nodeRef);
  ({ steps } = reduceExecutionEvent(steps, errEvt, lineRef, nodeRef));

  const ss = steps.find((s) => s.type === "sub_script");
  expect(ss?.status).toBe("error");
  expect(ss?.subScript?.closed).toBe(true);
});

test("SubScript with multiple TaskEnd usages aggregates token totals across distinct models", () => {
  const lineRef = makeActiveLineRef(1);
  const nodeRef = makeActiveNodeRef(1);

  const start = makeSubScriptEvent("multi", "r", { type: "WorkflowStart", payload: 2 });
  const task1 = makeSubScriptEvent("multi", "r", {
    type: "TaskEnd",
    payload: {
      task: "extract",
      on_error_label: null,
      value: { x: 1 },
      value_type: null,
      duration: { secs: 0, nanos: 0 },
      attempt: 1,
      usage: { input_tokens: 100, output_tokens: 20, cached_input_tokens: 50, model: "gpt-5", provider: "openai" },
    },
  });
  const task2 = makeSubScriptEvent("multi", "r", {
    type: "TaskEnd",
    payload: {
      task: "judge",
      on_error_label: null,
      value: 9,
      value_type: null,
      duration: { secs: 0, nanos: 0 },
      attempt: 1,
      usage: { input_tokens: 50, output_tokens: 5, cached_input_tokens: 0, model: "claude-opus-4-7", provider: "anthropic" },
    },
  });
  const end = makeSubScriptEvent("multi", "r", { type: "WorkflowEnd", payload: 9 });

  let { steps } = reduceExecutionEvent([], start, lineRef, nodeRef);
  ({ steps } = reduceExecutionEvent(steps, task1, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, task2, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, end, lineRef, nodeRef));

  const ss = steps.find((s) => s.type === "sub_script");
  expect(ss?.subScript?.subScriptTokens).toEqual({
    input: 150,
    output: 25,
    cachedInput: 50,
    models: ["gpt-5", "claude-opus-4-7"],
  });
  expect(ss?.subScript?.nestedTaskCount).toBe(2);
});

test("SubScript reducer is a no-op for malformed payload (missing child)", () => {
  // Defensive guard: an envelope with `child: null` (or missing) must not
  // crash the reducer or pollute the step list.
  const lineRef = makeActiveLineRef(1);
  const nodeRef = makeActiveNodeRef(1);

  const malformed: HubEvent = {
    type: "Execution",
    payload: {
      project_id: 1,
      script_name: "outer.akr",
      execution_id: "exec-1",
      event: {
        type: "SubScript",
        payload: { script_name: "x", parent_task: "y", child: null },
      },
    },
  };

  // First envelope creates a step (we still record the call's existence)…
  let { steps } = reduceExecutionEvent([], malformed, lineRef, nodeRef);
  expect(steps).toHaveLength(1);
  expect(steps[0]!.type).toBe("sub_script");
  // …but inputs/output stay empty.
  expect(steps[0]!.subScript?.inputs).toEqual([]);
  expect(steps[0]!.subScript?.output).toBeUndefined();
});

test("Nested SubScript chain (A → B → C) produces a children tree on the parent step", () => {
  // Sub-script B is opened by A's call, and B itself opens C. The wire
  // shape for a depth-2 grandchild event is
  //   SubScript { script: A, child: SubScript { script: B, child: <leaf> } }
  // for events on the B level, and three layers deep for events on C.
  //
  // We send: open A → open B inside A → open C inside B → leaf TaskEnd on C
  // → terminal WorkflowEnds bubbling up.
  const lineRef = makeActiveLineRef(1);
  const nodeRef = makeActiveNodeRef(1);

  const wrap = (frames: Array<{ script: string; parent: string }>, leaf: { type: string; payload: unknown }): HubEvent => {
    let inner: { type: string; payload: unknown } = leaf;
    for (let i = frames.length - 1; i >= 0; i -= 1) {
      const f = frames[i]!;
      inner = {
        type: "SubScript",
        payload: { script_name: f.script, parent_task: f.parent, child: inner },
      };
    }
    return {
      type: "Execution",
      payload: {
        project_id: 1,
        script_name: "outer.akr",
        execution_id: "exec-1",
        event: inner as unknown as never,
      },
    };
  };

  // 1. A opens (its WorkflowStart).
  const aStart = wrap([{ script: "A", parent: "out" }], { type: "WorkflowStart", payload: 1 });
  // 2. Inside A, B opens.
  const bStart = wrap(
    [{ script: "A", parent: "out" }, { script: "B", parent: "midA" }],
    { type: "WorkflowStart", payload: 1 },
  );
  // 3. Inside B (inside A), C opens.
  const cStart = wrap(
    [{ script: "A", parent: "out" }, { script: "B", parent: "midA" }, { script: "C", parent: "midB" }],
    { type: "WorkflowStart", payload: 1 },
  );
  // 4. C runs a single task (TaskEnd) with usage.
  const cTaskEnd = wrap(
    [{ script: "A", parent: "out" }, { script: "B", parent: "midA" }, { script: "C", parent: "midB" }],
    {
      type: "TaskEnd",
      payload: {
        task: "leaf_task",
        on_error_label: null,
        value: "deep",
        value_type: null,
        duration: { secs: 0, nanos: 250_000_000 },
        attempt: 1,
        usage: {
          input_tokens: 30,
          output_tokens: 5,
          cached_input_tokens: 0,
          model: "gpt-5",
          provider: "openai",
        },
      },
    },
  );
  // 5. C ends.
  const cEnd = wrap(
    [{ script: "A", parent: "out" }, { script: "B", parent: "midA" }, { script: "C", parent: "midB" }],
    { type: "WorkflowEnd", payload: "deep" },
  );
  // 6. B ends (inside A).
  const bEnd = wrap(
    [{ script: "A", parent: "out" }, { script: "B", parent: "midA" }],
    { type: "WorkflowEnd", payload: "deep" },
  );
  // 7. A ends.
  const aEnd = wrap([{ script: "A", parent: "out" }], { type: "WorkflowEnd", payload: "deep" });

  let { steps } = reduceExecutionEvent([], aStart, lineRef, nodeRef);
  ({ steps } = reduceExecutionEvent(steps, bStart, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, cStart, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, cTaskEnd, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, cEnd, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, bEnd, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, aEnd, lineRef, nodeRef));

  // Exactly one top-level sub_script step (A); B + C are nested under
  // children, not at the top level.
  const top = steps.filter((s) => s.type === "sub_script");
  expect(top).toHaveLength(1);
  const a = top[0]!;
  expect(a.subScript?.scriptName).toBe("A");
  expect(a.subScript?.children).toBeDefined();
  expect(a.subScript?.children).toHaveLength(1);

  const b = a.subScript!.children![0]!;
  expect(b.type).toBe("sub_script");
  expect(b.subScript?.scriptName).toBe("B");
  expect(b.subScript?.children).toHaveLength(1);

  const c = b.subScript!.children![0]!;
  expect(c.subScript?.scriptName).toBe("C");
  expect(c.subScript?.parentTask).toBe("midB");
  // Leaf-level taskSummaries: C ran one task.
  expect(c.subScript?.taskSummaries).toHaveLength(1);
  const leafSummary = c.subScript!.taskSummaries![0]!;
  expect(leafSummary.kind).toBe("task_end");
  if (leafSummary.kind === "task_end") {
    expect(leafSummary.taskName).toBe("leaf_task");
    expect(leafSummary.durationMs).toBe(250);
    expect(leafSummary.tokens?.input).toBe(30);
  }
  // Token rollup: A's totals should reflect C's usage even though the
  // TaskEnd lived two levels deep.
  expect(a.subScript?.subScriptTokens).toEqual({
    input: 30,
    output: 5,
    cachedInput: 0,
    models: ["gpt-5"],
  });
  // nestedTaskCount on A reflects the rolled-up descendant tasks.
  expect(a.subScript?.nestedTaskCount).toBe(1);
  // All three levels should be closed after the WorkflowEnd cascade.
  expect(c.subScript?.closed).toBe(true);
  expect(b.subScript?.closed).toBe(true);
  expect(a.subScript?.closed).toBe(true);
});

test("SubScript wrapped TaskEnd surfaces a task_end taskSummary on the level that hosts it", () => {
  // Single-level call with one task — the parent step should expose the
  // per-task timeline entry the SubScriptCard renders in its Timeline section.
  const lineRef = makeActiveLineRef(3);
  const nodeRef = makeActiveNodeRef(2);
  const start = makeSubScriptEvent("greeter", "out", { type: "WorkflowStart", payload: 1 });
  const taskEnd = makeSubScriptEvent("greeter", "out", {
    type: "TaskEnd",
    payload: {
      task: "hello",
      on_error_label: null,
      value: "hi",
      value_type: null,
      duration: { secs: 1, nanos: 200_000_000 },
      attempt: 2,
      usage: {
        input_tokens: 50,
        output_tokens: 10,
        cached_input_tokens: 0,
        model: "gpt-5",
        provider: "openai",
      },
    },
  });
  const end = makeSubScriptEvent("greeter", "out", { type: "WorkflowEnd", payload: "hi" });
  let { steps } = reduceExecutionEvent([], start, lineRef, nodeRef);
  ({ steps } = reduceExecutionEvent(steps, taskEnd, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, end, lineRef, nodeRef));

  const ss = steps.find((s) => s.type === "sub_script");
  expect(ss?.subScript?.taskSummaries).toHaveLength(1);
  const summary = ss!.subScript!.taskSummaries![0]!;
  expect(summary.kind).toBe("task_end");
  if (summary.kind === "task_end") {
    expect(summary.taskName).toBe("hello");
    expect(summary.durationMs).toBe(1200);
    expect(summary.attempt).toBe(2);
    expect(summary.tokens?.input).toBe(50);
    expect(summary.tokens?.output).toBe(10);
    expect(summary.tokens?.model).toBe("gpt-5");
  }
});

test("SubScript wrapped ValidationFailure surfaces a validation_failure summary alongside task_end summaries", () => {
  // Engine emits ValidationFailure between attempts; the timeline UI
  // shows them inline so the operator sees what retried.
  const lineRef = makeActiveLineRef(1);
  const nodeRef = makeActiveNodeRef(1);
  const start = makeSubScriptEvent("flaky", "r", { type: "WorkflowStart", payload: 1 });
  const failedAttempt = makeSubScriptEvent("flaky", "r", {
    type: "ValidationFailure",
    payload: {
      task_name: "extract",
      attempt: 1,
      model_response: "{}",
      missing_fields: ["x"],
      extra_fields: [],
      type_errors: [],
      stop_reason: null,
    },
  });
  const retryEnd = makeSubScriptEvent("flaky", "r", {
    type: "TaskEnd",
    payload: {
      task: "extract",
      on_error_label: null,
      value: { x: 1 },
      value_type: null,
      duration: { secs: 0, nanos: 600_000_000 },
      attempt: 2,
      usage: { input_tokens: 80, output_tokens: 12, cached_input_tokens: 0, model: "gpt-5", provider: "openai" },
    },
  });
  const end = makeSubScriptEvent("flaky", "r", { type: "WorkflowEnd", payload: { x: 1 } });

  let { steps } = reduceExecutionEvent([], start, lineRef, nodeRef);
  ({ steps } = reduceExecutionEvent(steps, failedAttempt, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, retryEnd, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, end, lineRef, nodeRef));

  const ss = steps.find((s) => s.type === "sub_script");
  expect(ss?.subScript?.taskSummaries).toHaveLength(2);
  const [first, second] = ss!.subScript!.taskSummaries!;
  expect(first!.kind).toBe("validation_failure");
  if (first!.kind === "validation_failure") {
    expect(first!.taskName).toBe("extract");
    expect(first!.attempt).toBe(1);
  }
  expect(second!.kind).toBe("task_end");
  if (second!.kind === "task_end") {
    expect(second!.attempt).toBe(2);
  }
});

test("3-level chain (A → B → C) populates per-level selfTokens + recursive subScriptTokens with the sum invariant", () => {
  // One TaskEnd at each level. selfTokens at each level reflects ONLY the
  // tokens that level produced directly; subScriptTokens (recursive total)
  // includes descendants. Sum of selfTokens across the chain must equal
  // the outermost step's recursive subScriptTokens.
  const lineRef = makeActiveLineRef(1);
  const nodeRef = makeActiveNodeRef(1);

  const wrap = (frames: Array<{ script: string; parent: string }>, leaf: { type: string; payload: unknown }): HubEvent => {
    let inner: { type: string; payload: unknown } = leaf;
    for (let i = frames.length - 1; i >= 0; i -= 1) {
      const f = frames[i]!;
      inner = {
        type: "SubScript",
        payload: { script_name: f.script, parent_task: f.parent, child: inner },
      };
    }
    return {
      type: "Execution",
      payload: {
        project_id: 1,
        script_name: "outer.akr",
        execution_id: "exec-3lvl",
        event: inner as unknown as never,
      },
    };
  };

  const fA = [{ script: "A", parent: "out" }];
  const fAB = [...fA, { script: "B", parent: "midA" }];
  const fABC = [...fAB, { script: "C", parent: "midB" }];

  const usageA = { input_tokens: 100, output_tokens: 20, cached_input_tokens: 0, model: "gpt-5", provider: "openai" };
  const usageB = { input_tokens: 50, output_tokens: 10, cached_input_tokens: 5, model: "claude-opus-4-7", provider: "anthropic" };
  const usageC = { input_tokens: 30, output_tokens: 6, cached_input_tokens: 0, model: "gpt-5", provider: "openai" };

  const aStart = wrap(fA, { type: "WorkflowStart", payload: 1 });
  const bStart = wrap(fAB, { type: "WorkflowStart", payload: 1 });
  const cStart = wrap(fABC, { type: "WorkflowStart", payload: 1 });
  const cTask = wrap(fABC, {
    type: "TaskEnd",
    payload: {
      task: "leaf_c", on_error_label: null, value: "c", value_type: null,
      duration: { secs: 0, nanos: 100_000_000 }, attempt: 1, usage: usageC,
    },
  });
  const cEnd = wrap(fABC, { type: "WorkflowEnd", payload: "c" });
  const bTask = wrap(fAB, {
    type: "TaskEnd",
    payload: {
      task: "leaf_b", on_error_label: null, value: "b", value_type: null,
      duration: { secs: 0, nanos: 200_000_000 }, attempt: 1, usage: usageB,
    },
  });
  const bEnd = wrap(fAB, { type: "WorkflowEnd", payload: "b" });
  const aTask = wrap(fA, {
    type: "TaskEnd",
    payload: {
      task: "leaf_a", on_error_label: null, value: "a", value_type: null,
      duration: { secs: 0, nanos: 300_000_000 }, attempt: 1, usage: usageA,
    },
  });
  const aEnd = wrap(fA, { type: "WorkflowEnd", payload: "a" });

  let { steps } = reduceExecutionEvent([], aStart, lineRef, nodeRef);
  for (const evt of [bStart, cStart, cTask, cEnd, bTask, bEnd, aTask, aEnd]) {
    ({ steps } = reduceExecutionEvent(steps, evt, lineRef, nodeRef));
  }

  const top = steps.filter((s) => s.type === "sub_script");
  expect(top).toHaveLength(1);
  const a = top[0]!;
  const b = a.subScript!.children![0]!;
  const c = b.subScript!.children![0]!;

  // selfTokens per level — exactly that level's TaskEnd usage, no descendants.
  expect(c.subScript?.selfTokens).toEqual({ input: 30, output: 6, cachedInput: 0, models: ["gpt-5"] });
  expect(b.subScript?.selfTokens).toEqual({ input: 50, output: 10, cachedInput: 5, models: ["claude-opus-4-7"] });
  expect(a.subScript?.selfTokens).toEqual({ input: 100, output: 20, cachedInput: 0, models: ["gpt-5"] });

  // subScriptTokens — recursive totals.
  expect(c.subScript?.subScriptTokens).toEqual({ input: 30, output: 6, cachedInput: 0, models: ["gpt-5"] });
  expect(b.subScript?.subScriptTokens).toEqual({ input: 80, output: 16, cachedInput: 5, models: ["claude-opus-4-7", "gpt-5"] });
  // A's recursive total includes its own + B's recursive (which includes C).
  expect(a.subScript?.subScriptTokens?.input).toBe(180);
  expect(a.subScript?.subScriptTokens?.output).toBe(36);
  expect(a.subScript?.subScriptTokens?.cachedInput).toBe(5);
  // Models de-duped across the chain.
  const aModels = a.subScript!.subScriptTokens!.models.slice().sort();
  expect(aModels).toEqual(["claude-opus-4-7", "gpt-5"]);

  // Sum invariant: Σ selfTokens.input == outermost.subScriptTokens.input
  const selfSumIn = (a.subScript!.selfTokens!.input)
    + (b.subScript!.selfTokens!.input)
    + (c.subScript!.selfTokens!.input);
  const selfSumOut = (a.subScript!.selfTokens!.output)
    + (b.subScript!.selfTokens!.output)
    + (c.subScript!.selfTokens!.output);
  const selfSumCached = (a.subScript!.selfTokens!.cachedInput)
    + (b.subScript!.selfTokens!.cachedInput)
    + (c.subScript!.selfTokens!.cachedInput);
  expect(selfSumIn).toBe(a.subScript!.subScriptTokens!.input);
  expect(selfSumOut).toBe(a.subScript!.subScriptTokens!.output);
  expect(selfSumCached).toBe(a.subScript!.subScriptTokens!.cachedInput);
});

test("SubScript with a streaming task surfaces concatenated AgentOutput chunks on the task summary", () => {
  // AgentOutput chunks observed BEFORE the task's TaskEnd are buffered
  // per-task and attached as `streamingOutput` on the resulting summary.
  // The drill-in UI uses this to render the LLM's verbatim output for a
  // nested task without needing a separate event log.
  const lineRef = makeActiveLineRef(1);
  const nodeRef = makeActiveNodeRef(1);

  const start = makeSubScriptEvent("greeter", "out", { type: "WorkflowStart", payload: 1 });
  const chunk1 = makeSubScriptEvent("greeter", "out", {
    type: "AgentOutput",
    payload: {
      task_name: "hello", agent_name: "Bot", task_id: "tid-1",
      schema_type: "str", chunk: "Hello, ",
    },
  });
  const chunk2 = makeSubScriptEvent("greeter", "out", {
    type: "AgentOutput",
    payload: {
      task_name: "hello", agent_name: "Bot", task_id: "tid-1",
      schema_type: "str", chunk: "world!",
    },
  });
  const taskEnd = makeSubScriptEvent("greeter", "out", {
    type: "TaskEnd",
    payload: {
      task: "hello", on_error_label: null, value: "Hello, world!", value_type: null,
      duration: { secs: 0, nanos: 500_000_000 }, attempt: 1,
      usage: { input_tokens: 10, output_tokens: 5, cached_input_tokens: 0, model: "gpt-5", provider: "openai" },
    },
  });
  const end = makeSubScriptEvent("greeter", "out", { type: "WorkflowEnd", payload: "Hello, world!" });

  let { steps } = reduceExecutionEvent([], start, lineRef, nodeRef);
  ({ steps } = reduceExecutionEvent(steps, chunk1, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, chunk2, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, taskEnd, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, end, lineRef, nodeRef));

  const ss = steps.find((s) => s.type === "sub_script");
  expect(ss?.subScript?.taskSummaries).toHaveLength(1);
  const summary = ss!.subScript!.taskSummaries![0]!;
  expect(summary.kind).toBe("task_end");
  if (summary.kind === "task_end") {
    expect(summary.taskName).toBe("hello");
    expect(summary.streamingOutput).toBe("Hello, world!");
  }
  // Buffer is dropped once attached, so a subsequent same-name task wouldn't
  // pick up stale chunks.
  expect(ss?.subScript?._streamingByTask).toBeUndefined();
});

test("Suspended with legacy tuple payload (array) still produces a step", () => {
  const lineRef = makeActiveLineRef(1);
  const nodeRef = makeActiveNodeRef(1);

  const evt: HubEvent = {
    type: "Execution",
    payload: {
      project_id: 1,
      script_name: "test.akr",
      event: {
        type: "Suspended",
        payload: ["legacy_checkpoint"] as unknown as never,
      },
    },
  };

  const { steps } = reduceExecutionEvent([], evt, lineRef, nodeRef);

  expect(steps).toHaveLength(1);
  expect(steps[0]!.content).toBe("Suspended at checkpoint: legacy_checkpoint");
  expect(steps[0]!.validationErrors).toBeUndefined();
});

// ── seq + at + dedup + ValidationFailure tests (Workstream 04) ───────────────

test("reducer copies HubEvent seq + at onto the produced step", () => {
  const lineRef = makeActiveLineRef(10);
  const nodeRef = makeActiveNodeRef(3);
  const evt: HubEvent = {
    type: "Execution",
    payload: {
      project_id: 1,
      script_name: "t.akr",
      execution_id: "exec-x",
      seq: 7,
      at: "2026-05-07T10:23:42.187Z",
      event: { type: "Log", payload: "hello" },
    },
  };
  const { steps } = reduceExecutionEvent([], evt, lineRef, nodeRef);
  expect(steps).toHaveLength(1);
  expect(steps[0]!.seq).toBe(7);
  expect(steps[0]!.serverTs).toBe(Date.parse("2026-05-07T10:23:42.187Z"));
});

test("merges TaskPrompt + AgentOutput + TaskEnd from the same task into one inline step", () => {
  const lineRef = makeActiveLineRef(10);
  const nodeRef = makeActiveNodeRef(5);
  const mk = (event: { type: string; payload: unknown }, seq: number): HubEvent => ({
    type: "Execution",
    payload: {
      project_id: 1,
      script_name: "t.akr",
      execution_id: "x",
      seq,
      event: event as unknown as never,
    },
  });

  let { steps } = reduceExecutionEvent(
    [],
    mk({ type: "NodeStart", payload: [5, { line: 10, col: 0 }] }, 1),
    lineRef,
    nodeRef,
  );
  ({ steps } = reduceExecutionEvent(
    steps,
    mk({ type: "TaskPrompt", payload: ["t1", "p"] }, 2),
    lineRef,
    nodeRef,
  ));
  ({ steps } = reduceExecutionEvent(
    steps,
    mk(
      {
        type: "AgentOutput",
        payload: {
          task_name: "t1",
          agent_name: "Bot",
          task_id: "abc",
          schema_type: "str",
          chunk: "hello",
        },
      },
      3,
    ),
    lineRef,
    nodeRef,
  ));
  ({ steps } = reduceExecutionEvent(
    steps,
    mk(
      {
        type: "TaskEnd",
        payload: {
          task: "t1",
          on_error_label: null,
          value: "hello",
          value_type: null,
          duration: { secs: 0, nanos: 5e8 },
          attempt: 1,
          usage: null,
        },
      },
      4,
    ),
    lineRef,
    nodeRef,
  ));

  // Inline steps only — UI applies the same filter via `visibility !== 'inline'`.
  const inlineSteps = steps.filter((s) => s.visibility === "inline");
  expect(inlineSteps).toHaveLength(1);
  expect(inlineSteps[0]!.type).toBe("chat");
  expect(inlineSteps[0]!.content).toContain("hello");
  expect(inlineSteps[0]!.status).toBe("success");
});

test("StateUpdate produces a panel-only step (not visible in inline timeline)", () => {
  const lineRef = makeActiveLineRef(10);
  const nodeRef = makeActiveNodeRef(2);
  const evt: HubEvent = {
    type: "Execution",
    payload: {
      project_id: 1,
      script_name: "t.akr",
      execution_id: "x",
      event: { type: "StateUpdate", payload: ["x", 42] as unknown as never },
    },
  };
  const { steps } = reduceExecutionEvent([], evt, lineRef, nodeRef);
  expect(steps).toHaveLength(1);
  expect(steps[0]!.visibility).toBe("panel-only");
});

// ── Loop block reducer (LoopStart / LoopTurn / LoopEnd) ────────────────────

/**
 * Build a HubEvent wrapping an arbitrary `EngineEvent` payload — used by the
 * loop tests below to author the canonical event stream that mirrors what the
 * engine integration test in `crates/akribes-core/tests/loop_block_engine.rs`
 * emits end-to-end.
 */
function makeEngineEvent(child: { type: string; payload: unknown }): HubEvent {
  return {
    type: "Execution",
    payload: {
      project_id: 1,
      script_name: "loop_research.akr",
      execution_id: "exec-loop",
      event: child,
    },
  };
}

test("LoopStart → LoopTurn × 3 → LoopEnd folds into one loop step with 3 turns + success status", () => {
  // Mirrors the canonical event stream `loop_block_emits_*` produces against
  // the mock provider — see the integration test in
  // `crates/akribes-core/tests/loop_block_engine.rs`. Three turns: a skill
  // call + state update on turn 1, a state update on turn 2, then a return
  // on turn 3. The terminal `LoopEnd` carries the agent's `return(...)`
  // value, serialized as `Value::String("…")` (default serde tagging).
  const lineRef = makeActiveLineRef(30);
  const nodeRef = makeActiveNodeRef(0);

  const wfStart = makeEngineEvent({ type: "WorkflowStart", payload: 1 });
  const nodeStart = makeEngineEvent({
    type: "NodeStart",
    payload: [0, { line: 30, col: 10, end_line: 31, end_col: 1 }],
  });
  const loopStart = makeEngineEvent({
    type: "LoopStart",
    payload: { name: "research", max_turns: 32 },
  });
  const turn1 = makeEngineEvent({
    type: "LoopTurn",
    payload: { name: "research", turn: 1, tool_calls: ["summarize", "state_update"] },
  });
  const turn2 = makeEngineEvent({
    type: "LoopTurn",
    payload: { name: "research", turn: 2, tool_calls: ["state_update"] },
  });
  const turn3 = makeEngineEvent({
    type: "LoopTurn",
    payload: { name: "research", turn: 3, tool_calls: ["return"] },
  });
  const loopEnd = makeEngineEvent({
    type: "LoopEnd",
    payload: {
      name: "research",
      turn_count: 3,
      // Wire shape of `Value::String(...)`: default serde tag wraps the
      // payload under the variant name. The reducer keeps it verbatim as
      // `loopResult` so the UI can hand it to AkribesValueViewer unchanged.
      value: { String: "mock loop completed" },
    },
  });
  const wfEnd = makeEngineEvent({
    type: "WorkflowEnd",
    payload: { String: "mock loop completed" },
  });

  let { steps } = reduceExecutionEvent([], wfStart, lineRef, nodeRef);
  ({ steps } = reduceExecutionEvent(steps, nodeStart, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, loopStart, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, turn1, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, turn2, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, turn3, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, loopEnd, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, wfEnd, lineRef, nodeRef));

  // Exactly one `loop` step survived, with all three turns folded into it.
  const loopSteps = steps.filter((s) => s.type === "loop");
  expect(loopSteps).toHaveLength(1);
  const ls = loopSteps[0]!;
  expect(ls.loopName).toBe("research");
  expect(ls.maxTurns).toBe(32);
  expect(ls.status).toBe("success");
  expect(ls.turns).toEqual([
    { turn: 1, toolCalls: ["summarize", "state_update"] },
    { turn: 2, toolCalls: ["state_update"] },
    { turn: 3, toolCalls: ["return"] },
  ]);
  expect(ls.loopResult).toEqual({ String: "mock loop completed" });
  expect(ls.visibility).toBe("inline");
  expect(ls.content).toBe("loop research");
});

test("Skill TaskStart/TaskEnd inside a loop turn stays as sibling steps; the loop step is unaffected", () => {
  // A skill invocation that fires within a loop turn (TaskStart/TaskEnd
  // between LoopTurn events) renders as its own sibling step rather than
  // being nested inside the turn. Decision: simpler to render coherently
  // — the sibling step shows token usage / structured output via the
  // existing chat-step pipeline, while the loop card focuses on the
  // turn timeline. Drilling into per-turn task events is a follow-up
  // (overlaps with the sub-script drill-in work).
  const lineRef = makeActiveLineRef(30);
  const nodeRef = makeActiveNodeRef(0);

  const loopStart = makeEngineEvent({
    type: "LoopStart",
    payload: { name: "research", max_turns: 32 },
  });
  // Skill task fires DURING turn 1 (between LoopStart and the turn's
  // settle event). The engine emits TaskStart/TaskEnd as siblings — they
  // don't carry any "I'm inside a loop" attribution today.
  const taskStart = makeEngineEvent({
    type: "TaskStart",
    payload: ["summarize", null],
  });
  const taskEnd = makeEngineEvent({
    type: "TaskEnd",
    payload: {
      task: "summarize",
      on_error_label: null,
      value: "summary",
      value_type: null,
      duration: { secs: 0, nanos: 100_000_000 },
      attempt: 1,
      usage: null,
    },
  });
  const turn1 = makeEngineEvent({
    type: "LoopTurn",
    payload: { name: "research", turn: 1, tool_calls: ["summarize", "state_update"] },
  });
  const loopEnd = makeEngineEvent({
    type: "LoopEnd",
    payload: { name: "research", turn_count: 1, value: { String: "ok" } },
  });

  let { steps } = reduceExecutionEvent([], loopStart, lineRef, nodeRef);
  ({ steps } = reduceExecutionEvent(steps, taskStart, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, taskEnd, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, turn1, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, loopEnd, lineRef, nodeRef));

  // The loop step lives alongside the task-derived sibling steps (the panel
  // filters `panel-only` ones out before render, so the inline view shows
  // just the loop card today). The key invariant: the skill's TaskEnd
  // event did NOT escape the reducer as some duplicate top-level chat row
  // that would visually overshadow the loop card.
  const loopSteps = steps.filter((s) => s.type === "loop");
  expect(loopSteps).toHaveLength(1);
  expect(loopSteps[0]!.turns).toEqual([
    { turn: 1, toolCalls: ["summarize", "state_update"] },
  ]);
  expect(loopSteps[0]!.status).toBe("success");

  // The skill TaskEnd produced an `execution`-typed sibling (no streaming
  // chat step existed, so it took the stdlib append path). The Studio panel
  // renders these alongside the loop card — they don't get nested under it,
  // which keeps the SubScriptCard / loop card layouts independent.
  const taskEndSteps = steps.filter(
    (s) => s.type === "execution" && s.content === "Finished task: summarize",
  );
  expect(taskEndSteps).toHaveLength(1);
});

test("LoopEnd with FatalError value (max_turns exhaustion) yields status='error' + carries the FatalError envelope", () => {
  // The engine emits `Value::FatalError {...}` when a loop runs out of
  // turn budget without ever calling `return(...)`. The wire shape uses
  // serde's default tag wrapper:
  //   { FatalError: "...", error_kind: "...", code: "...", error_detail: {...} }
  // `loopResult` carries the whole envelope so the UI can render it via
  // the same value-viewer + raw-JSON toggle that handles other task
  // results.
  const lineRef = makeActiveLineRef(40);
  const nodeRef = makeActiveNodeRef(2);

  const loopStart = makeEngineEvent({
    type: "LoopStart",
    payload: { name: "stuck", max_turns: 4 },
  });
  const turn1 = makeEngineEvent({
    type: "LoopTurn",
    payload: { name: "stuck", turn: 1, tool_calls: ["state_update"] },
  });
  const fatalEnvelope = {
    FatalError:
      "AKRIBES-E-LOOP-BUDGET-EXCEEDED: loop 'stuck' exceeded max_turns=4 without calling return(...)",
    error_kind: "ScriptError",
    code: "AKRIBES-E-OTHER",
    error_detail: {
      kind: "ScriptError",
      code: "AKRIBES-E-OTHER",
      message:
        "AKRIBES-E-LOOP-BUDGET-EXCEEDED: loop 'stuck' exceeded max_turns=4 without calling return(...)",
      user_message: "",
      retry_after_ms: null,
      source: {},
    },
  };
  const loopEnd = makeEngineEvent({
    type: "LoopEnd",
    payload: { name: "stuck", turn_count: 4, value: fatalEnvelope },
  });

  let { steps } = reduceExecutionEvent([], loopStart, lineRef, nodeRef);
  ({ steps } = reduceExecutionEvent(steps, turn1, lineRef, nodeRef));
  ({ steps } = reduceExecutionEvent(steps, loopEnd, lineRef, nodeRef));

  const loopSteps = steps.filter((s) => s.type === "loop");
  expect(loopSteps).toHaveLength(1);
  const ls = loopSteps[0]!;
  expect(ls.status).toBe("error");
  expect(ls.loopResult).toEqual(fatalEnvelope);
  expect(ls.maxTurns).toBe(4);
});

test("ValidationFailure event produces a validation_failure step with structured payload", () => {
  const lineRef = makeActiveLineRef(15);
  const nodeRef = makeActiveNodeRef(7);
  const evt: HubEvent = {
    type: "Execution",
    payload: {
      project_id: 1,
      script_name: "t.akr",
      execution_id: "x",
      event: {
        type: "ValidationFailure",
        payload: {
          task_name: "extract_claim",
          attempt: 2,
          model_response: '{ "claim": "..." }',
          missing_fields: ["/speaker"],
          extra_fields: ["/extra"],
          type_errors: ["expected string at /claim"],
          stop_reason: "max_tokens",
        },
      } as unknown as never,
    },
  };
  const { steps } = reduceExecutionEvent([], evt, lineRef, nodeRef);
  expect(steps).toHaveLength(1);
  expect(steps[0]!.type).toBe("validation_failure");
  expect(steps[0]!.validationFailure).toEqual({
    taskName: "extract_claim",
    attempt: 2,
    modelResponse: '{ "claim": "..." }',
    missingFields: ["/speaker"],
    extraFields: ["/extra"],
    typeErrors: ["expected string at /claim"],
    stopReason: "max_tokens",
  });
  expect(steps[0]!.visibility).toBe("inline");
});
