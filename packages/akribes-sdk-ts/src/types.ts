export type EngineEvent = {
  type: string;
  payload: unknown;
};

export type RegistryEvent =
  | { type: 'ProjectCreated'; payload: { project_id: number; project: Project } }
  | { type: 'ProjectUpdated'; payload: { project_id: number; project: Project } }
  | { type: 'ProjectDeleted'; payload: number }
  | { type: 'ScriptCreated'; payload: { project_id: number; script: Script } }
  | { type: 'ScriptUpdated'; payload: { project_id: number; script_name: string; version_id: number; channel: string | null } }
  | { type: 'ScriptDeleted'; payload: { project_id: number; script_name: string } };

/** Live bench-run lifecycle events, broadcast on the project `/events`
 *  stream as `HubEvent.Bench`. Adjacently tagged to match the server's
 *  `BenchEvent` (`crates/akribes-server/src/models.rs`); all three variants
 *  reuse the existing {@link BenchRun} / {@link BenchResult} row types. */
export type BenchEvent =
  | {
      type: 'RunStarted';
      payload: { project_id: number; script_name: string; run: BenchRun };
    }
  | {
      type: 'ResultRecorded';
      payload: { project_id: number; script_name: string; run_id: number; result: BenchResult };
    }
  | {
      type: 'RunFinished';
      payload: { project_id: number; script_name: string; run: BenchRun };
    };

export type HubEvent =
  | {
      type: 'Execution';
      payload: {
        project_id: number;
        script_name: string;
        execution_id: string;
        event: EngineEvent;
        /** Monotonic per-execution sequence number. Starts at 1; matches the
         *  `execution_events.id` ordering for the same execution. Optional
         *  for forward-compat with older servers that don't yet stamp it. */
        seq?: number;
        /** Server-side RFC3339 timestamp with ms precision (e.g.
         *  `"2026-05-07T10:23:42.187Z"`). Optional for forward-compat. */
        at?: string;
      };
    }
  | { type: 'Registry'; payload: RegistryEvent }
  | { type: 'Bench'; payload: BenchEvent };

export type Project = {
  id: number;
  name: string;
  sort_order: number;
  created_at: string;
};

export type Script = {
  id: number;
  project_id: number;
  name: string;
  sort_order: number;
  created_at: string;
};

export type ClientInterest = {
  script_name: string;
  inputs: Record<string, string>;
  channel?: string;
  lifetime?: 'session' | 'perma';
  strict?: boolean;
};

export type RegisteredInterest = {
  script_name: string;
  channel: string;
  bound_version_id: number | null;
  input_schema: [string, string][];
};

export type RegisterClientResponse = {
  interests: RegisteredInterest[];
};

export type SchemaMismatch = {
  missing: [string, string][];
  wrong_type: [string, string, string][];
  extra: string[];
};

export type ContractLockInfo = {
  id: number;
  client_id: string;
  client_name: string;
  script_name: string;
  channel: string;
  bound_version_id: number | null;
  lifetime: string;
  drifted: boolean;
  created_by: string | null;
  created_at: string;
  input_schema: string;
};

export type ContractWarning = {
  client_id: string;
  client_name: string;
  channel: string;
  mismatch: SchemaMismatch;
};

/** Structural representation of a declared Akribes type. Nested `inner` handles
 *  parameterized types like `list[str]` or `list[Profile]`. `choices` is
 *  populated for `choice[...]` types. */
export type TypeRef = {
  name: string;
  inner?: TypeRef | null;
  choices?: string[] | null;
};

/** Token consumption breakdown for a single task execution, mirroring the
 *  Rust `TokenUsage` struct in `akribes-core`. */
export type TokenUsage = {
  input_tokens: number;
  output_tokens: number;
  model: string;
  provider: string;
  cached_input_tokens: number;
  /** Cache-creation (write) tokens. Anthropic-only today: `cache_creation_input_tokens`
   *  from the Messages API usage block, billed by the server at 1.25x base
   *  input (5-minute TTL). OpenAI and Gemini always emit 0. Older servers
   *  that predate this field emit it as 0 via serde default. */
  cache_write_input_tokens: number;
};

/** Known discriminants of `TaskEndPayload.variant` (issue #206). A future
 *  engine may add more (e.g. `"partial"` for #205); consumers MUST tolerate
 *  other string values via a catch-all.
 */
export type KnownTaskEndVariant = 'success' | 'unable' | 'failed';

/** Wire shape of `TaskEndPayload.variant` — one of the {@link KnownTaskEndVariant}s
 *  today, or any other `snake_case` string from a newer server. Consumers
 *  should narrow on the known set and fall through for unknowns so the stream
 *  keeps flowing across SDK upgrades. Mirrors `akribes_core::event::TaskEndVariant`
 *  which uses `#[serde(other)]` for the same forward-compat contract. */
export type TaskEndVariant = KnownTaskEndVariant | (string & {});

/** Payload of a `TaskEnd` engine event. */
export type TaskEndPayload = {
  task: string;
  on_error_label: string | null;
  value: unknown;
  value_type: TypeRef | null;
  /** serde-serialized `std::time::Duration` */
  duration: { secs: number; nanos: number };
  /** 1-indexed attempt number (u8) */
  attempt: number;
  usage: TokenUsage | null;
  /** How the task finished (issue #206). `"success"` is the wire default
   *  emitted by the server; absence on the wire indicates a pre-#206
   *  server and should be treated as `"success"`. */
  variant?: TaskEndVariant;
};

/**
 * Studio-facing projection of an `input ... by <ref>(args)` clause (Track C).
 *
 * `display` is the source-form workflow ref (e.g. `epopy/fetch@production`).
 * `explicit_args` lists the names of arguments the resolver clause binds
 * literally — Studio uses this to know which slots are deterministic on
 * the parent side vs. propagated implicitly from same-named parent inputs.
 */
export type InputResolver = {
  display: string;
  explicit_args: string[];
};

/** A single `input <name>: <ty>` declaration as emitted by the draft endpoint. */
export type InputDecl = {
  name: string;
  ty: TypeRef;
  /** Optional `##` doc-comment attached to the input. */
  docs?: string | null;
  /** Track C: optional fallback resolver attached via `input X: T by ref(args)`. */
  resolver?: InputResolver | null;
};

/** A single field on a `type` declaration. */
export type TypeField = {
  name: string;
  ty: TypeRef;
  /** Optional `##` doc-comment attached to the field. */
  docs?: string | null;
};

export type PutDraftResponse = {
  schema_warnings: ContractWarning[];
  inputs: InputDecl[];
  type_defs: Record<string, TypeField[]>;
};

export type BreakingInterest = {
  client_id: string;
  client_name: string;
  channel: string;
  lifetime: string;
  mismatch: SchemaMismatch;
};

/** The dependent class a {@link BreakingChange} affects. Mirrors the Rust
 *  `akribes_core::contracts::DependentKind` enum, which serializes with its
 *  variant name. */
export type DependentKind = 'SdkClient' | 'UseImport' | 'Judge' | 'BenchCase';

/** One broken dependent from the unified contract check. Mirrors
 *  `akribes_core::contracts::BreakingChange` — returned by the publish
 *  dry-run (`DryRunResult.unified_breaks`), the live publish 409
 *  (`{ error: "contract_break", breaks }`), and the channel-rollback 409. */
export type BreakingChange = {
  dependent_kind: DependentKind;
  /** Stable identifier (e.g. row id) for the dependent. */
  dependent_id: string;
  /** Human-readable label rendered in error messages. */
  dependent_label: string;
  /** Concrete locator, e.g. `"outputs.score (int) removed"`. */
  what_broke: string;
  /** One-line suggested fix, or `null` if none applies. */
  suggested_fix?: string | null;
  /** App route to the dependent. */
  link_path: string;
};

export type DryRunResult = {
  dry_run: true;
  /** Legacy `client_interests` break count (back-compat). The UI gates its
   *  red/green state on {@link total_break_count} instead. */
  would_break: number;
  breaking_interests: BreakingInterest[];
  /** Unified contract breaks (SDK clients, judges, bench cases, `use`
   *  imports). Present on WS1+ servers; older servers omit it — treat
   *  `undefined` as `[]`. */
  unified_breaks?: BreakingChange[];
  /** Union total: legacy interests + unified dependents. The truthful
   *  "would this publish break anything" count. Older servers omit it;
   *  fall back to `would_break` when absent. */
  total_break_count?: number;
};

export type ScriptVersion = {
  id: number;
  script_id: number;
  source: string;
  label: string | null;
  published_by: string | null;
  created_at: string;
};

export type ScriptVersionResponse = ScriptVersion & {
  inputs: [string, string][];
};

export type ScriptChannel = {
  id: number;
  script_id: number;
  name: string;
  version_id: number | null;
  updated_at: string | null;
};

export type RunResult = {
  execution_id: string;
  /** Event-log watermark a subscriber should pass as `last_event_id` on
   *  the FIRST `events.subscribe(...)` call after this run. Always `0`
   *  on a fresh spawn — the server's catchup path then replays every
   *  buffered event with `id > 0` so no event is dropped between the
   *  spawn response and the SSE/WS attach (#807). Optional for
   *  back-compat with pre-0.21.13 servers; treat `undefined` as `0`. */
  since_id?: number;
};

/** Result of {@link ExecutionsClient.rerun}: the freshly-spawned execution
 *  plus the id of the execution it was re-run from. */
export type RerunResult = RunResult & {
  /** The execution id this re-run reproduced. */
  rerun_of: string;
};

// #1296: legacy umbrella `ServerError` retained for back-compat; new producers emit one of the four status-specific kinds.
export type ErrorKind = 'RateLimit' | 'AuthError' | 'TokenLimit' | 'ServerError' | 'ServerError500' | 'BadGateway502' | 'ServiceUnavailable503' | 'GatewayTimeout504' | 'NetworkError' | 'ParseError' | 'Cancelled' | 'ScriptError';

/** A document reference returned when S3 persistence is active. */
export type DocumentRef = {
  document_id: string;
  filename: string;
};

export type ExecutionStatus = {
  id: string;
  project_id: number;
  script_name: string;
  status: 'running' | 'completed' | 'failed' | 'cancelled';
  started_at: string | null;
  finished_at: string | null;
  version_id: number | null;
  channel: string | null;
  error: string | null;
  error_kind: ErrorKind | null;
  /** Failure-mode discriminator (migration 20260515000000). `null` on success
   *  or for older servers; one of the documented values on a classified
   *  failure (e.g. `provider_rate_limited`, `workflow_timeout`). Returned on
   *  the run-history list + single-execution read. */
  failure_mode?: string | null;
  result: unknown;
  /** When S3 is enabled: `{ inputName: DocumentRef }`. Without S3: `{ inputName: markdownString }`. */
  documents: Record<string, string | DocumentRef> | null;
  triggered_by: string | null;
  input_tokens: number;
  output_tokens: number;
  /** Tokens consumed by tool-response payloads (task 39b). */
  tool_tokens?: number;
  cost_usd: number | null;
  /** Declared record types from the source the execution ran against,
   *  keyed by `type Name:` identifier. Lets clients render results back to
   *  their declared shape (named records, typed columns) instead of
   *  falling through to JSON shape inference. Empty object when the
   *  source couldn't be parsed; `undefined` from older servers. */
  type_defs?: Record<string, TypeField[]>;
  /** Workflow's declared return `TypeRef`, when statically resolvable from
   *  the source. Populated when the workflow ends in
   *  `return <task>(...)` or `return <flow>(...)` and the callee's
   *  signature is local. Lets the renderer dispatch straight into the
   *  typed path (e.g. `list[Patent]` → typed `RecordTable`) instead of
   *  inferring from `value`. `null` / `undefined` for older servers,
   *  unparseable source, or workflows whose final expression isn't a
   *  resolvable call. */
  result_type?: TypeRef | null;
  /** ID of the parent execution that spawned this one via `spawn_child_execution`.
   *  Null for top-level executions. Forward-looking for v1 (typically null until
   *  a host wires the spawn callback). */
  parent_execution_id?: string | null;
  /** The node ID within the parent execution at which this child was spawned.
   *  Null when `parent_execution_id` is null or when the node id is unavailable. */
  parent_node_id?: string | null;
};

export type ExecutionOutput = {
  status: 'running' | 'completed' | 'failed' | 'cancelled';
  error: string | null;
  error_kind: ErrorKind | null;
  result: unknown;
};

export type CostByVersion = {
  version_id: number | null;
  executions: number;
  total_cost_usd: number;
  avg_cost_usd: number;
  unknown_cost_executions: number;
};

export type CostByChannel = {
  /** `"unknown"` when the execution row's channel column was NULL. */
  channel: string;
  executions: number;
  total_cost_usd: number;
  avg_cost_usd: number;
  unknown_cost_executions: number;
};

export type CostByScript = {
  script_name: string;
  executions: number;
  total_cost_usd: number;
  avg_cost_usd: number;
  unknown_cost_executions: number;
};

/** Per-model cost rollup. Joins through `execution_tasks` (the only place the
 *  model name is normalized to a column), so `task_calls` counts agent
 *  invocations, not whole executions. `model` is `null` for rows the server
 *  couldn't attribute to a named model. */
export type CostByModel = {
  model: string | null;
  task_calls: number;
  total_cost_usd: number;
  total_input_tokens: number;
  total_output_tokens: number;
  total_cached_input_tokens: number;
};

/** Per-task cost rollup (script-scoped only). Joins through `execution_tasks`,
 *  grouped by `task_name`, so `task_calls` counts per-task agent invocations
 *  across every execution of the script in the window. */
export type CostByTask = {
  task_name: string;
  task_calls: number;
  total_cost_usd: number;
  avg_cost_usd: number;
  total_input_tokens: number;
  total_output_tokens: number;
};

export type ScriptCost = {
  total_executions: number;
  total_cost_usd: number;
  avg_cost_usd: number;
  total_input_tokens: number;
  total_output_tokens: number;
  total_cached_input_tokens?: number;
  /** Executions whose model wasn't in the server's pricing table — their tokens
   *  are still counted but they contribute `0` to cost totals. */
  unknown_cost_executions: number;
  by_version: CostByVersion[];
  by_channel: CostByChannel[];
  /** Per-model breakdown (joins through `execution_tasks`). Optional for
   *  back-compat with pre-WS4 servers that didn't emit it. */
  by_model?: CostByModel[];
  /** Per-task breakdown. Script-scoped only. Optional for back-compat. */
  by_task?: CostByTask[];
};

export type ProjectCost = {
  project_id: number;
  total_executions: number;
  total_cost_usd: number;
  avg_cost_usd: number;
  total_input_tokens: number;
  total_output_tokens: number;
  total_cached_input_tokens?: number;
  unknown_cost_executions: number;
  by_script: CostByScript[];
  by_channel: CostByChannel[];
  /** Per-model breakdown (joins through `execution_tasks`). Optional for
   *  back-compat with pre-WS4 servers that didn't emit it. */
  by_model?: CostByModel[];
};

export type ExecutionEvents = {
  execution_id: string;
  status: string;
  complete: boolean;
  events: EngineEvent[];
  next_after_id: number | null;
  has_more: boolean;
};

export type TokenInfo = {
  id: string;
  label: string;
  user_email: string | null;
  scopes: {
    projects: '*' | number[];
    role: 'admin' | 'editor' | 'viewer';
    scripts?: string[];
    executions?: string[];
    can_mint: boolean;
  };
  minted_by: string;
  expires_at: string;
  revoked: boolean;
  created_at: string;
  last_used_at: string | null;
};

export type MintTokenResponse = {
  token: string;
  token_id: string;
  expires_at: string;
};

export type ClientInfo = {
  id: string;
  name: string;
  last_seen: string;
  scripts: string[];
};

export type DraftResponse = {
  source: string;
  inputs: InputDecl[];
  type_defs: Record<string, TypeField[]>;
};

/** Execution DAG graph returned by the /graph endpoint. */
export type ScriptGraph = {
  nodes: ScriptGraphNode[];
  edges: ScriptGraphEdge[];
};

export type ScriptGraphNode = {
  id: number;
  op_type: string;
  op_name: string | null;
  target_var: string | null;
  reads: string[];
  line: number;
  col: number;
};

export type ScriptGraphEdge = {
  from: number;
  to: number;
};

/** S3 document reference via pre-signed URL. */
export type S3PresignedRef = {
  presigned_url: string;
};

/** S3 document reference via temporary credentials. */
export type S3CredentialsRef = {
  bucket: string;
  key: string;
  region?: string;
  access_key_id: string;
  secret_access_key: string;
  session_token?: string;
};

/** S3 document reference — either a pre-signed URL or bucket/key with temp credentials. */
export type S3DocumentRef = S3PresignedRef | S3CredentialsRef;

/** Response from the /convert endpoint. */
export type ConvertResult = {
  markdown: string;
  /** Present when the server has S3 persistence enabled. Pass this back as a
   * document input on subsequent runs to skip re-upload + reconversion. */
  document_id?: string;
  filename?: string;
};

// ── Bench types ──────────────────────────────────────────────────────────────
//
// Mirrors the Rust SDK's `Bench`, `BenchRun`, `BenchResult`, `BenchCase`,
// `CompareReport`, `DriftReport`, etc. from
// `crates/akribes-sdk/src/models.rs`. Timestamps are RFC3339 strings.

/** Wire status of a bench run. */
export type BenchStatus = 'pending' | 'running' | 'completed' | 'failed' | 'canceled';

/** Wire status of a single per-case result row. */
export type BenchResultStatus = 'ok' | 'workflow_failed' | 'judge_failed' | 'skipped' | 'cached';

/** Per-case compare flag emitted by `GET /bench-runs/{a}/compare/{b}`. */
export type CompareFlag = 'improved' | 'regressed' | 'unchanged' | 'missing_a' | 'missing_b';

/** A single typed value flowing through a bench case (input value, expected
 *  output, ground truth, judge score, workflow output). Shape is determined
 *  dynamically by the corresponding `TypeRef` from the script signature;
 *  consumers narrow via the schema. Matches Studio's `AkribesValue`. */
export type AkribesValue = unknown;

/** Per-input typed value bag — keys match the script's declared inputs (or
 *  outputs), values follow each field's `TypeRef`. Opaque at the SDK level
 *  because keys are dynamic per script; the server validates the payload
 *  against the script's signature on every write path. */
export type AkribesValueBag = Record<string, unknown>;

/** Free-form bench-runtime knobs. Not script IO — this is for the
 *  coordinator's own configuration (e.g. `concurrency`, `retry_policy`).
 *  The server tolerates extra keys for forward compat. */
export type BenchConfig = {
  /** Max parallel case executions; defaults to 10 server-side. */
  concurrency?: number;
  [extra: string]: unknown;
};

/** Per-script bench configuration. One row per `scripts.id`.
 *  `judge_script_id` is nullable while the bench is still being authored. */
export type Bench = {
  id: number;
  script_id: number;
  judge_script_id: number | null;
  judge_channel: string;
  config: BenchConfig;
  created_at: string;
  updated_at: string;
};

/** Aggregated per-bench summary backing the project-level evals landing
 *  page. Returned by `GET /projects/{id}/benches`. */
export type ProjectBenchSummary = {
  bench_id: number;
  script_id: number;
  script_name: string;
  judge_script_id: number | null;
  judge_script_name: string | null;
  judge_channel: string;
  case_count: number;
  latest_run_id: number | null;
  latest_run_status: BenchStatus | null;
  latest_run_channel: string | null;
  latest_run_workflow_version_id: number | null;
  latest_run_at: string | null;
  latest_run_mean_score: number | null;
  latest_run_cost_usd: number | null;
  updated_at: string;
};

/** A single bench-run row. `workflow_version_id` / `judge_version_id` are
 *  resolved at trigger time so a later channel publish doesn't change what
 *  this run represents. */
export type BenchRun = {
  id: number;
  bench_id: number;
  channel: string;
  workflow_version_id: number;
  judge_version_id: number;
  status: BenchStatus;
  triggered_by: string | null;
  triggered_at: string;
  completed_at: string | null;
  total_cost_usd: number;
  total_cases: number;
  cache_hit_cases: number;
  notes: string | null;
  mcp_session_id?: string | null;
  /** Subset of case IDs this run targets. `null` / absent = every case in
   *  the bench. */
  case_filter?: string[] | null;
  /** Mean headline_score across cases with `status='ok'|'cached'`. Populated
   *  by the list-runs aggregate query; bare GET-run leaves it absent. */
  mean_headline_score?: number | null;
  /** Count of results with `status='ok'|'cached'`. Paired with
   *  `mean_headline_score`. */
  ok_cases?: number | null;
  /** Per-`BenchResultStatus` row count for this run. Populated alongside
   *  `mean_headline_score` / `ok_cases` by the list-runs and get-run
   *  aggregate queries (#753). Statuses with zero rows may be absent
   *  rather than serialised as `0`. Use the headline `ok_cases` for the
   *  ok+cached total — the breakdown lets the rail split the rest into
   *  `workflow_failed` / `judge_failed` / `skipped`. */
  status_breakdown?: Partial<Record<BenchResultStatus, number>>;
  /** Pre-flight warnings populated by the trigger endpoint only — e.g.
   *  "OPENAI_API_KEY missing; N cases will likely fail". Empty / absent on
   *  every other read path. */
  warnings?: string[];
  /** Name of the judge script whose version produced this run, joined in by
   *  `get_run` and `list_runs` so consumers can deep-link to the judge's
   *  source at `judge_version_id` without an N+1 lookup. Absent on
   *  coordinator-inserted rows and on benches with no judge wired up. */
  judge_script_name?: string | null;
};

/** One per-case score row for a bench run. Carries the workflow execution's
 *  typed output alongside the judge's score blob so the studio's typed
 *  renderers don't need a second fetch. */
export type BenchResult = {
  id: number;
  bench_run_id: number;
  case_id: string;
  workflow_execution_id: string | null;
  judge_execution_id: string | null;
  /** Full judge output — shape is dictated by the judge's declared output
   *  `TypeRef`. */
  score: AkribesValue | null;
  /** Workflow execution's actual output value, joined in on the read path
   *  from `executions.result`. `null` when the workflow failed, was
   *  canceled, or this row is a pure cache-hit. */
  workflow_output: AkribesValue | null;
  headline_score: number | null;
  status: BenchResultStatus;
  cost_usd: number;
  duration_ms: number | null;
  cache_hit: boolean;
  input_hash?: string | null;
  /** Human-readable error captured on `workflow_failed` / `judge_failed`
   *  rows. `null` on `ok` / `cached`. */
  error?: string | null;
  created_at: string;
};

/** Server-side projection of an `executions` row with `kind='case'`. */
export type BenchCase = {
  /** `executions.id` for the underlying frozen execution row. */
  id: string;
  project_id: number;
  script_name: string;
  bench_id: number | null;
  kind: string;
  frozen: boolean;
  case_name: string | null;
  inputs: AkribesValueBag | null;
  expected_output: AkribesValue | null;
  ground_truth: AkribesValue | null;
  /** SHA-256 hex of `canonical_json(inputs)`. Nullable on legacy rows. */
  input_hash?: string | null;
  created_at: string;
};

export type CompareCase = {
  case_id: string;
  case_label: string;
  score_a: number | null;
  score_b: number | null;
  delta: number | null;
  /** One of {@link CompareFlag} or any future server-emitted string. */
  flag: CompareFlag | string;
};

export type CompareAggregate = {
  mean_score_delta: number;
  cost_delta_usd: number;
  n_regressed: number;
  n_improved: number;
  n_unchanged: number;
};

export type CompareReport = {
  run_a_id: number;
  run_b_id: number;
  aggregate: CompareAggregate;
  per_case: CompareCase[];
};

export type DriftedCase = {
  case_id: string;
  label: string;
  what_broke: string;
};

export type DriftReport = {
  drifted: DriftedCase[];
  /** `null` when the script has never been published. */
  script_version_id: number | null;
  published_at: string | null;
  published_by: string | null;
  /** Single-line summary suitable for inline display. Empty when no drift. */
  summary: string;
};

/** Receipt returned by `PATCH /bench-runs/{id}/tag-session`. */
export type BenchRunTagSessionResponse = {
  tagged: boolean;
  run_id: number;
  mcp_session_id: string;
};

/** Bench row looked up by numeric id via `GET /benches/{id}`, joined with
 *  the owning project id + script name so callers can chain into
 *  list_cases / list_runs without an N+1 project walk. Wider than
 *  {@link Bench} — it carries `project_id` + `script_name` the bare
 *  `(project, script)`-scoped read doesn't need to echo back. */
export type BenchById = {
  id: number;
  project_id: number;
  script_id: number;
  script_name: string;
  judge_script_id: number | null;
  judge_channel: string;
  config: BenchConfig;
  created_at: string;
  updated_at: string;
};

/** Aggregated cost for one MCP session, returned by
 *  `GET /mcp-sessions/{id}/cost`. Sits on the same `mcp_session_cost` table
 *  the bench coordinator's finalize step writes to. A session with no
 *  recorded cost rows still resolves (if a bench run tagged it) with
 *  `total_cost_usd: 0` and an empty `breakdown`. */
export type McpSessionCost = {
  session_id: string;
  total_cost_usd: number;
  /** Free-form per-source cost breakdown blob. Shape is set by the
   *  coordinator's finalize step; `{}` when no rows were recorded. */
  breakdown: Record<string, unknown>;
};

// ── Bench request payloads ──────────────────────────────────────────────────

export type CreateOrUpdateBenchRequest = {
  judge_script_id?: number | null;
  judge_channel?: string;
  config?: BenchConfig;
};

export type CreateBenchCaseRequest = {
  /** Per-input typed value bag. Keys match the script's declared inputs;
   *  each value's shape follows the input's `TypeRef`. */
  inputs: AkribesValueBag;
  /** Optional expected output. Shape follows the script's declared output
   *  `TypeRef`. When omitted, the judge must work off `ground_truth`. */
  expected_output?: AkribesValue;
  /** Free-form judge ground-truth payload (no contract). */
  ground_truth?: AkribesValue;
  name?: string;
};

export type PatchBenchCaseRequest = {
  inputs?: AkribesValueBag;
  expected_output?: AkribesValue;
  ground_truth?: AkribesValue;
  name?: string;
};

export type PromoteCaseEdits = {
  inputs?: AkribesValueBag;
  expected_output?: AkribesValue;
  ground_truth?: AkribesValue;
};

export type PromoteExecutionRequest = {
  /** Override any of the source execution's inputs / outputs before
   *  freezing into a case. All shape-typed against the script's signature
   *  on the server side. */
  edits?: PromoteCaseEdits;
  name?: string;
};

export type TriggerBenchRunRequest = {
  channel: string;
  notes?: string;
  /** Subset of case IDs. Empty / omitted = run every case. */
  case_ids?: string[];
};

// ── Signature + contract-preview wire shapes ────────────────────────────────

/** A single field on a script's declared signature. `ty` is the SDK's
 *  structural `TypeRef`, matching what live `EngineEvent::TaskEnd` values
 *  carry on `value_type`. */
export type BenchSignatureField = {
  path: string;
  ty: TypeRef;
  required: boolean;
  annotations: string[];
};

/** Parsed script signature — used to render type-aware form fields.
 *  Returned by `GET /projects/{id}/scripts/{name}/signature`. */
export type ScriptSignature = {
  inputs: BenchSignatureField[];
  outputs: BenchSignatureField[];
  /** Named record types declared in the script source, keyed by
   *  `type Name:` identifier. */
  type_defs: Record<string, TypeField[]>;
};

/** Workflow + judge signature pair plus the structured `breaks` list.
 *  Returned by `GET /projects/{id}/scripts/{name}/bench/contract-preview`. */
export type ContractPreview = {
  workflow: { fields: BenchSignatureField[] };
  judge: { fields: BenchSignatureField[] };
  breaks: string[];
};

// ── MCP tool events and summaries ────────────────────────────────────────────

export type ToolCallStartEvent = { task_name: string; tool_name: string; server_name: string; input: unknown; tool_use_id?: string };
export type ToolCallEndEvent = { task_name: string; tool_name: string; output: unknown; duration_ms: number; error?: string; tool_use_id?: string };
/** A destructive MCP tool invocation is awaiting operator approval. The engine
 *  suspends the run on a resume channel keyed by `token`; resume via
 *  `executions.resume(id, token, null, { approve })` (optionally with
 *  `args_override`). `tool_ref` is the qualified `server.tool` name. */
export type ToolApprovalPendingEvent = {
  execution_id?: string | null;
  node_id?: number | null;
  token: string;
  tool_ref: string;
  args: unknown;
};
export type McpServerDegradedEvent = { alias: string; reason: string };
export type McpServerRecoveredEvent = { alias: string };

export type McpServerSummary = {
  alias: string;
  url: string;
  origin: 'env' | 'script' | 'db';
  is_registry: boolean;
  status: 'connected' | 'degraded' | 'offline' | 'pinned_offline';
  tool_count: number;
  /** True when a DB config row exists for this alias (a DB server, or a
   *  knob-override layer for a script-declared one). */
  has_config?: boolean;
  /** True when an auth secret is configured (write-only — never returned). */
  auth_configured?: boolean;
  /** Effective timeout in seconds (override > script default). */
  timeout_secs?: number;
  /** Effective server-level approval gate. */
  approval_required?: boolean;
};

export type McpToolSummary = {
  qualified_name: string;      // "github.list_issues"
  server_alias: string;
  description?: string;
  input_schema: unknown;
};

/** Typed narrower: returns true if an `EngineEvent` is a `ToolCallStart` event. */
export function isToolCallStart(event: EngineEvent): event is { type: 'ToolCallStart'; payload: ToolCallStartEvent } {
  return event.type === 'ToolCallStart'
    && typeof event.payload === 'object' && event.payload !== null
    && 'tool_name' in (event.payload as Record<string, unknown>)
    && 'server_name' in (event.payload as Record<string, unknown>);
}

/** Typed narrower: returns true if an `EngineEvent` is a `ToolCallEnd` event. */
export function isToolCallEnd(event: EngineEvent): event is { type: 'ToolCallEnd'; payload: ToolCallEndEvent } {
  return event.type === 'ToolCallEnd'
    && typeof event.payload === 'object' && event.payload !== null
    && 'duration_ms' in (event.payload as Record<string, unknown>);
}

/** Typed narrower: returns true if an `EngineEvent` is an `McpServerDegraded` event. */
export function isMcpServerDegraded(event: EngineEvent): event is { type: 'McpServerDegraded'; payload: McpServerDegradedEvent } {
  return event.type === 'McpServerDegraded';
}

/** Typed narrower: returns true if an `EngineEvent` is an `McpServerRecovered` event. */
export function isMcpServerRecovered(event: EngineEvent): event is { type: 'McpServerRecovered'; payload: McpServerRecoveredEvent } {
  return event.type === 'McpServerRecovered';
}

// ── Loop block events (open-ended agent driver) ──────────────────────────────

/** Emitted exactly once when a `loop NAME(...) -> Ret` call begins.
 *  `max_turns` is the resolved upper-bound turn budget (declared
 *  `max_turns:` if present, else the engine's default). */
export type LoopStartEvent = { name: string; max_turns: number };

/** Emitted after every turn of a `loop` settles. `turn` is 1-indexed.
 *  `tool_calls` is the names of the tools the model invoked this turn,
 *  in dispatch order — including the synthetic `state_get`,
 *  `state_update`, `return`, and any user `tools:` entries. */
export type LoopTurnEvent = { name: string; turn: number; tool_calls: string[] };

/** Emitted exactly once when a `loop` exits. `value` is the agent's
 *  submitted return value (from `return(...)`), the final state on a
 *  natural `stop_when:` exit without a return, or a `FatalError`
 *  envelope when the loop exhausted its `max_turns` budget. */
export type LoopEndEvent = { name: string; turn_count: number; value: unknown };

/** Typed narrower: returns true if an `EngineEvent` is a `LoopStart`. */
export function isLoopStart(event: EngineEvent): event is { type: 'LoopStart'; payload: LoopStartEvent } {
  return event.type === 'LoopStart';
}

/** Typed narrower: returns true if an `EngineEvent` is a `LoopTurn`. */
export function isLoopTurn(event: EngineEvent): event is { type: 'LoopTurn'; payload: LoopTurnEvent } {
  return event.type === 'LoopTurn';
}

/** Typed narrower: returns true if an `EngineEvent` is a `LoopEnd`. */
export function isLoopEnd(event: EngineEvent): event is { type: 'LoopEnd'; payload: LoopEndEvent } {
  return event.type === 'LoopEnd';
}

// ── Compaction events (three-mode context management, RFC 2026-05-12) ────────

/** Emitted once per primitive activation of the compaction chain. Mirrors
 *  `akribes_core::event::EngineEvent::ContextCompacted` — fired by the
 *  engine before/after a compaction step succeeds in shrinking the
 *  conversation under the configured cap. `provider_native: true` means
 *  Anthropic / OpenAI performed the compaction server-side; the engine
 *  surfaces the before/after counts from the response. `strategy` is the
 *  primitive name (`drop_thinking_blocks`, `drop_oldest_tool_results`,
 *  `summarize_to_state`, `provider_native`) or the user task name for a
 *  custom compactor task.
 *
 *  See `docs/superpowers/specs/2026-05-12-compaction-design.md`
 *  ("Observability + cost") for the contract.
 */
export type ContextCompactedEvent = {
  agent: string;
  /** UUID of the surrounding `loop` block when compaction fires mid-loop;
   *  `null` for compaction outside a loop. */
  loop_id: string | null;
  /** 1-indexed loop turn the compaction fired before, when applicable. */
  turn: number | null;
  /** Configured percent-of-window threshold (0-100), when the
   *  triggering rule was `at_pct`. */
  threshold_pct: number | null;
  /** Configured absolute-token threshold, when the triggering rule was
   *  `at_tokens`. */
  threshold_abs: number | null;
  /** Primitive name or user task name. */
  strategy: string;
  before_tokens: number;
  after_tokens: number;
  provider_native: boolean;
  /** Cache TTL applied on the request that produced this compaction.
   *  `"1h"` on the Anthropic `provider_native` path (akribes-core pins
   *  `ttl: "1h"` via the `extended-cache-ttl-2025-04-11` beta header),
   *  `null` for OpenAI native compaction and every non-native primitive.
   *  Cost dashboards multiply cache-write tokens by the correct provider
   *  rate via this field — the 5m and 1h tiers price 60% apart
   *  (issue #1130). */
  cache_ttl?: string | null;
};

/** Emitted when the compaction chain runs to exhaustion (or when
 *  `compaction: none` and the request would still exceed the model's
 *  context window). Mirrors
 *  `akribes_core::event::EngineEvent::ContextOverflow`. Carries the chain
 *  log so users can diagnose which primitives ran before the engine gave
 *  up. A `ContextCompactionExhausted` `Error` event follows.
 */
export type ContextOverflowEvent = {
  agent: string;
  attempted_strategies: string[];
  configured_cap_tokens: number;
  model_context_window: number;
};

/** Typed narrower: returns true if an `EngineEvent` is a `ContextCompacted`. */
export function isContextCompacted(
  event: EngineEvent,
): event is { type: 'ContextCompacted'; payload: ContextCompactedEvent } {
  return event.type === 'ContextCompacted'
    && typeof event.payload === 'object' && event.payload !== null
    && 'agent' in (event.payload as Record<string, unknown>)
    && 'strategy' in (event.payload as Record<string, unknown>)
    && 'before_tokens' in (event.payload as Record<string, unknown>)
    && 'after_tokens' in (event.payload as Record<string, unknown>);
}

/** Typed narrower: returns true if an `EngineEvent` is a `ContextOverflow`. */
export function isContextOverflow(
  event: EngineEvent,
): event is { type: 'ContextOverflow'; payload: ContextOverflowEvent } {
  return event.type === 'ContextOverflow'
    && typeof event.payload === 'object' && event.payload !== null
    && 'agent' in (event.payload as Record<string, unknown>)
    && 'attempted_strategies' in (event.payload as Record<string, unknown>)
    && 'configured_cap_tokens' in (event.payload as Record<string, unknown>);
}

// ── Durable execution replay events ─────────────────────────────────────────

/** LLM provider response captured for durable replay. Carries the full
 *  response (text + tool-use blocks + usage) keyed by `(node_id, call_index)`.
 *  Consumed by the engine's replay cache; UX clients typically ignore it.
 *  Mirrors `akribes_core::event::EngineEvent::LLMResponse`. */
export type LLMResponseEvent = {
  node_id: string;
  call_index: number;
  text: string;
  tool_calls: { tool_use_id: string; name: string; args: unknown }[];
  usage?: unknown;
};

/** A child execution row was just inserted at the parent's `call(...)` node.
 *  Mirrors `akribes_core::event::EngineEvent::SubScriptSpawned`. */
export type SubScriptSpawnedEvent = {
  child_execution_id: string;
  parent_node_id: string;
  args: unknown;
};

/** Child execution finished; the parent observed its terminal state.
 *  `outcome.kind` is `"Ok"` or `"Err"`. Mirrors
 *  `akribes_core::event::EngineEvent::SubScriptResult`. */
export type SubScriptResultEvent = {
  parent_node_id: string;
  child_execution_id: string;
  outcome:
    | { kind: 'Ok'; detail: { value: unknown } }
    | { kind: 'Err'; detail: { kind: string; message: string; code?: string } };
};

/** A `Suspended` checkpoint resolved — the durable record of a /resume payload.
 *  Mirrors `akribes_core::event::EngineEvent::CheckpointResolution`. */
export type CheckpointResolutionEvent = {
  checkpoint_id: string;
  payload: unknown;
};

/** Typed narrower: returns true if an `EngineEvent` is an `LLMResponse`. */
export function isLLMResponse(event: EngineEvent): event is { type: 'LLMResponse'; payload: LLMResponseEvent } {
  return event.type === 'LLMResponse';
}

/** Typed narrower: returns true if an `EngineEvent` is a `SubScriptSpawned`. */
export function isSubScriptSpawned(event: EngineEvent): event is { type: 'SubScriptSpawned'; payload: SubScriptSpawnedEvent } {
  return event.type === 'SubScriptSpawned';
}

/** Typed narrower: returns true if an `EngineEvent` is a `SubScriptResult`. */
export function isSubScriptResult(event: EngineEvent): event is { type: 'SubScriptResult'; payload: SubScriptResultEvent } {
  return event.type === 'SubScriptResult';
}

/** Typed narrower: returns true if an `EngineEvent` is a `CheckpointResolution`. */
export function isCheckpointResolution(event: EngineEvent): event is { type: 'CheckpointResolution'; payload: CheckpointResolutionEvent } {
  return event.type === 'CheckpointResolution';
}

// ── EPA wave types: SuspendTrigger + Unable envelope ────────────────────────

/**
 * Structured "I can't" response from an agent. Mirrors the Rust `Unable`
 * record (`crates/akribes-core/src/unable.rs`) — see `UNABLE_TYPE_NAME`.
 *
 * The canonical wire envelope is `{ "unable": UnableRecord }`. Detect with
 * {@link isUnableEnvelope}.
 */
export type UnableRecord = {
  reason: string;
  missing: string[];
  category: string;
};

/** Wire-format twin of the Rust `ValidationErrorWire`. The `stage` string is
 *  `"parse"` | `"schema"` | `"custom:<rule>"` — kept opaque here so SDK
 *  consumers don't need to round-trip through the internal enum. */
export type ValidationErrorWire = {
  stage: string;
  message: string;
  /** JSON-pointer-like path for schema errors. `null`/absent for parse. */
  path?: string | null;
};

/** Names of the `SuspendTrigger` variants the SDK knows how to normalize. */
export type KnownSuspendTriggerKind = 'DagPosition' | 'ValidationExhausted' | 'AgentUnable';

/** Forward-compat catch-all for unknown `SuspendTrigger` kinds. Emitted
 *  verbatim from the wire (snake_case fields preserved) so a newer server
 *  never crashes an older SDK. Callers that want to opt in inspect `raw`
 *  directly — the SDK makes no typed guarantees about its contents. */
export type UnknownSuspendTrigger = {
  kind: string;
  /** Opaque wire payload with all fields from the server preserved. */
  raw: Record<string, unknown>;
};

/**
 * Why the engine suspended at a checkpoint. Mirrors the Rust `SuspendTrigger`
 * (serde-tagged on `"kind"`, `crates/akribes-core/src/event.rs`).
 *
 * Callers should narrow on `kind` for the known variants and fall through to
 * {@link UnknownSuspendTrigger} for future server versions.
 */
export type SuspendTrigger =
  | { kind: 'DagPosition' }
  | {
      kind: 'ValidationExhausted';
      taskName: string;
      retryCount: number;
      lastAttempt: string;
      validationErrors: ValidationErrorWire[];
    }
  | {
      kind: 'AgentUnable';
      taskName: string;
      unable: UnableRecord;
    }
  | UnknownSuspendTrigger;

/** Return true iff `v` is a `{ "unable": <object> }` envelope and nothing
 *  else. Mirrors `is_unable_envelope` in `akribes-core/src/unable.rs`. */
export function isUnableEnvelope(v: unknown): v is { unable: UnableRecord } {
  if (typeof v !== 'object' || v === null || Array.isArray(v)) return false;
  const obj = v as Record<string, unknown>;
  const keys = Object.keys(obj);
  if (keys.length !== 1 || keys[0] !== 'unable') return false;
  const inner = obj.unable;
  return typeof inner === 'object' && inner !== null && !Array.isArray(inner);
}

/** Typed narrower: returns true if an `EngineEvent` is a `TaskEnd` event. */
export function isTaskEnd(event: EngineEvent): event is { type: 'TaskEnd'; payload: TaskEndPayload } {
  return event.type === 'TaskEnd'
    && typeof event.payload === 'object' && event.payload !== null
    && 'task' in (event.payload as Record<string, unknown>)
    && 'attempt' in (event.payload as Record<string, unknown>);
}

/** Payload of a `ValidationFailure` engine event (issue #320). Emitted in
 *  addition to the existing `Log` line on every structured-output validation
 *  retry — the typed shape lets consumers render the model's actual
 *  response, the schema-validator's structured error breakdown, and the
 *  provider's `stop_reason` (so a `max_tokens` truncation isn't
 *  misdiagnosed as a schema overflow). Mirrors
 *  `akribes_core::event::EngineEvent::ValidationFailure`. */
export type ValidationFailurePayload = {
  task_name: string;
  /** 1-indexed attempt number. */
  attempt: number;
  /** Raw text / JSON-serialized tool input the model emitted. */
  model_response: string;
  /** JSON-pointer paths to required fields the validator flagged as absent. */
  missing_fields: string[];
  /** Paths to fields rejected by `additionalProperties: false`. */
  extra_fields: string[];
  /** Human-readable type/value mismatches. */
  type_errors: string[];
  /** Provider stop_reason (`"max_tokens"` / `"end_turn"` / etc.) when known. */
  stop_reason: string | null;
};

/** Typed narrower: returns true if an `EngineEvent` is a `ValidationFailure`. */
export function isValidationFailure(
  event: EngineEvent,
): event is { type: 'ValidationFailure'; payload: ValidationFailurePayload } {
  return event.type === 'ValidationFailure'
    && typeof event.payload === 'object' && event.payload !== null
    && 'task_name' in (event.payload as Record<string, unknown>)
    && 'attempt' in (event.payload as Record<string, unknown>)
    && 'missing_fields' in (event.payload as Record<string, unknown>);
}

// ── Runtime / sandbox code-execution events ─────────────────────────────────
//
// New `EngineEvent` variants for the `runtime` block: an Akribes-language
// construct (parallel to `task`) that ships generated code through the
// `akribes-sandbox` microservice. Each invocation produces a Start envelope,
// 0+ stdout/stderr chunks streamed live, and exactly one terminal End or
// Error event. Mirrors the Rust shapes in `akribes_core::event::EngineEvent`
// — JSON field names are snake_case to match serde defaults.
//
// See `docs/superpowers/specs/2026-05-12-runtime-design.md` (§4) for the
// frozen wire contract.

/** Emitted exactly once when a `runtime` block begins executing. `language`
 *  is the lowercase language token from the `language:` field
 *  (`python` / `bash` / `node` / `rust` / `java`); `runtime_name` is the
 *  declared block name (e.g. `run_python`). */
export type RuntimeStartEvent = {
  task_name: string;
  runtime_name: string;
  language: string;
};

/** Streaming stdout chunk from a `runtime` block. `chunk` is the partial
 *  text exactly as the sandbox saw it on the child process's stdout; the
 *  engine doesn't buffer line-by-line, so consumers may need to coalesce
 *  partial lines themselves. */
export type RuntimeStdoutEvent = { task_name: string; chunk: string };

/** Streaming stderr chunk from a `runtime` block. Same shape as
 *  {@link RuntimeStdoutEvent} — separated so consumers can colour-code
 *  output without re-classifying. */
export type RuntimeStderrEvent = { task_name: string; chunk: string };

/** Emitted exactly once when a `runtime` block exits cleanly (process
 *  finished — exit_code 0 means success, non-zero is still a "clean" exit
 *  from the sandbox's perspective). `duration_ms` is wall-clock time from
 *  RuntimeStart to process exit. */
export type RuntimeEndEvent = {
  task_name: string;
  exit_code: number;
  duration_ms: number;
};

/** Emitted exactly once when a `runtime` block fails to run to completion —
 *  e.g. sandbox unavailable, timeout, OOM kill, or any other internal
 *  error. `kind` mirrors the `RuntimeError` enum on the Rust side
 *  (`Timeout`, `OomKilled`, `SandboxUnavailable`, `Internal`, `NotConfigured`);
 *  callers should branch on the known set and fall through for unknowns. */
export type RuntimeErrorEvent = {
  task_name: string;
  kind: string;
  message: string;
};

/** Typed narrower: returns true if an `EngineEvent` is a `RuntimeStart`. */
export function isRuntimeStart(
  event: EngineEvent,
): event is { type: 'RuntimeStart'; payload: RuntimeStartEvent } {
  return event.type === 'RuntimeStart'
    && typeof event.payload === 'object' && event.payload !== null
    && 'task_name' in (event.payload as Record<string, unknown>)
    && 'runtime_name' in (event.payload as Record<string, unknown>)
    && 'language' in (event.payload as Record<string, unknown>);
}

/** Typed narrower: returns true if an `EngineEvent` is a `RuntimeStdout`. */
export function isRuntimeStdout(
  event: EngineEvent,
): event is { type: 'RuntimeStdout'; payload: RuntimeStdoutEvent } {
  return event.type === 'RuntimeStdout'
    && typeof event.payload === 'object' && event.payload !== null
    && 'task_name' in (event.payload as Record<string, unknown>)
    && 'chunk' in (event.payload as Record<string, unknown>);
}

/** Typed narrower: returns true if an `EngineEvent` is a `RuntimeStderr`. */
export function isRuntimeStderr(
  event: EngineEvent,
): event is { type: 'RuntimeStderr'; payload: RuntimeStderrEvent } {
  return event.type === 'RuntimeStderr'
    && typeof event.payload === 'object' && event.payload !== null
    && 'task_name' in (event.payload as Record<string, unknown>)
    && 'chunk' in (event.payload as Record<string, unknown>);
}

/** Typed narrower: returns true if an `EngineEvent` is a `RuntimeEnd`. */
export function isRuntimeEnd(
  event: EngineEvent,
): event is { type: 'RuntimeEnd'; payload: RuntimeEndEvent } {
  return event.type === 'RuntimeEnd'
    && typeof event.payload === 'object' && event.payload !== null
    && 'task_name' in (event.payload as Record<string, unknown>)
    && 'exit_code' in (event.payload as Record<string, unknown>)
    && 'duration_ms' in (event.payload as Record<string, unknown>);
}

/** Typed narrower: returns true if an `EngineEvent` is a `RuntimeError`. */
export function isRuntimeError(
  event: EngineEvent,
): event is { type: 'RuntimeError'; payload: RuntimeErrorEvent } {
  return event.type === 'RuntimeError'
    && typeof event.payload === 'object' && event.payload !== null
    && 'task_name' in (event.payload as Record<string, unknown>)
    && 'kind' in (event.payload as Record<string, unknown>)
    && 'message' in (event.payload as Record<string, unknown>);
}
