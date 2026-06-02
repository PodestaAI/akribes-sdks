// Client
export { AkribesClient } from './client';
export type { AkribesClientOptions } from './client';

// Token safety
export { assertTokenSafeInUrl } from './tokenSafety';

// Errors
export { AkribesError, AkribesHttpError, AkribesAlreadyExistsError, AkribesAuthError, AkribesNotFoundError, AkribesRateLimitError, AkribesTransientHttpError, AkribesServerError500, AkribesBadGatewayError502, AkribesServiceUnavailableError503, AkribesGatewayTimeoutError504, recommendedBackoffMs, AkribesTransientError, AkribesFatalError, AkribesScriptError, AkribesTimeoutError, ScriptSchemaChangedError, ScriptInputMismatchError, CaseTypeMismatchError, JudgeContractError, tryParseInputValidationErrors, parseRetryAfter } from './errors';
export type { InputValidationErrorEntry, CaseFieldError } from './errors';

// Types
export type {
  EngineEvent,
  RegistryEvent,
  HubEvent,
  BenchEvent,
  Project,
  Script,
  ClientInterest,
  ScriptVersion,
  ScriptVersionResponse,
  ScriptChannel,
  RunResult,
  ErrorKind,
  ExecutionStatus,
  ExecutionOutput,
  ExecutionEvents,
  TokenInfo,
  MintTokenResponse,
  ClientInfo,
  DraftResponse,
  TypeRef,
  InputDecl,
  TypeField,
  S3PresignedRef,
  S3CredentialsRef,
  S3DocumentRef,
  ConvertResult,
  Bench,
  BenchConfig,
  BenchStatus,
  BenchResultStatus,
  BenchRun,
  BenchResult,
  BenchCase,
  BenchById,
  McpSessionCost,
  BenchRunTagSessionResponse,
  BenchSignatureField,
  ScriptSignature,
  ContractPreview,
  ProjectBenchSummary,
  CompareCase,
  CompareAggregate,
  CompareReport,
  CompareFlag,
  DriftedCase,
  DriftReport,
  CreateOrUpdateBenchRequest,
  CreateBenchCaseRequest,
  PatchBenchCaseRequest,
  PromoteCaseEdits,
  PromoteExecutionRequest,
  TriggerBenchRunRequest,
  AkribesValue,
  AkribesValueBag,
  RegisterClientResponse,
  RegisteredInterest,
  SchemaMismatch,
  ContractLockInfo,
  ContractWarning,
  PutDraftResponse,
  BreakingInterest,
  DryRunResult,
  ScriptGraph,
  ScriptGraphNode,
  ScriptGraphEdge,
  ToolCallStartEvent,
  ToolCallEndEvent,
  McpServerDegradedEvent,
  McpServerRecoveredEvent,
  McpServerSummary,
  McpToolSummary,
  SuspendTrigger,
  UnableRecord,
  ValidationErrorWire,
  UnknownSuspendTrigger,
  KnownSuspendTriggerKind,
  ProjectCost,
  ScriptCost,
  CostByScript,
  CostByVersion,
  CostByChannel,
  LoopStartEvent,
  LoopTurnEvent,
  LoopEndEvent,
  ContextCompactedEvent,
  ContextOverflowEvent,
  RuntimeStartEvent,
  RuntimeStdoutEvent,
  RuntimeStderrEvent,
  RuntimeEndEvent,
  RuntimeErrorEvent,
} from './types';

// MCP event narrowers (runtime helpers)
export { isToolCallStart, isToolCallEnd, isMcpServerDegraded, isMcpServerRecovered, isUnableEnvelope } from './types';

// Loop event narrowers (runtime helpers)
export { isLoopStart, isLoopTurn, isLoopEnd } from './types';

// Compaction event narrowers (runtime helpers)
export { isContextCompacted, isContextOverflow } from './types';

// Runtime (sandbox code-execution) event narrowers
export { isRuntimeStart, isRuntimeStdout, isRuntimeStderr, isRuntimeEnd, isRuntimeError } from './types';

// Sub-clients (for advanced usage / type narrowing)
export { ProjectsClient } from './sub/projects';
export { ScriptsClient } from './sub/scripts';
export { VersionsClient } from './sub/versions';
export { ChannelsClient } from './sub/channels';
export { ExecutionsClient } from './sub/executions';
export type { ExecutionChildSummary, ExecutionTaskSummary, ExecutionTasksResponse } from './sub/executions';
export { ClientsClient, heartbeatBackoffMs } from './sub/clients';
export type { HeartbeatStatus, ClientsClientOptions } from './sub/clients';
export { TokensClient } from './sub/tokens';
export type { TokenScopes, MintTokenRequest } from './sub/tokens';
export { EventsClient } from './sub/events';
export { BenchClient } from './sub/bench';
export { McpClient } from './sub/mcp';
export type { McpHealth, McpDriftResult, McpRefreshResult } from './sub/mcp';

// SSE utilities
export { connectSse, parseSseMessage } from './sse';
export type { SseMessage, EventStreamOptions } from './sse';

// Execution step model + reducer (shared by Studio + docs runner)
export {
  reduceExecutionEvent,
  buildRunFromParams,
  replayEvents,
  createReplayController,
} from './execution';
export type {
  ExecutionStep,
  StepVisibility,
  StepTokens,
  SubScriptTokens,
  SubScriptTaskSummary,
  LoopTurnSummary,
  ActiveLineRef,
  ActiveNodeRef,
  ReducerSideEffects,
  ReplayController,
  ReplayState,
  ValidationFailurePayload,
} from './execution';

// Typed WorkflowEvent (layer 2) + RunStream lifecycle (layer 3).
export { toWorkflowEvent, categoryOf } from './workflowEvents';
export type { WorkflowEvent, TokenUsage, EventCategory, RuntimeStepStatus } from './workflowEvents';
// Re-export `ErrorKind` under a workflow-scoped alias so the existing
// `types.ErrorKind` export stays the canonical one.
export { createRunStream } from './runStream';
export type { RunStream, RunStreamCallbacks, RunStreamOptions, RunStreamEventsSource, RunStarter, RunSummary, RuntimeStep } from './runStream';

// Document ingest sub-client
export {
  DocumentsClient,
  DocumentConversionError,
  IngestTimeoutError,
  IngestProtocolError,
  DEFAULT_INGEST_POLL_TIMEOUT_MS,
  ingestPollTimeoutMsFromEnv,
} from './sub/documents';
export type { ConversionStatus, UploadResult, ClaimOutcome, IngestPhase, IngestOptions } from './sub/documents';
