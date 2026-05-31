/** Base error class for all Akribes SDK errors. */
export class AkribesError extends Error {
  override readonly name: string = 'AkribesError';
}

/** Structured HTTP error with status code and server message.
 *
 *  `errorType` and `reason` are populated when the server returns a structured
 *  error body of the shape `{ error, error_type, reason, ... }` (currently
 *  used by the document conversion path). Callers that care about typed
 *  failures should branch on these instead of regex-matching `message`.
 *
 *  As of #987, this is the abstract parent for typed HTTP-status subclasses
 *  (`AkribesAuthError`, `AkribesNotFoundError`, `AkribesRateLimitError`,
 *  `AkribesTransientHttpError`, `AkribesAlreadyExistsError`,
 *  `CaseTypeMismatchError`, `JudgeContractError`). `instanceof AkribesHttpError`
 *  still works as a catch-all for any non-2xx. */
export class AkribesHttpError extends AkribesError {
  override readonly name: string = 'AkribesHttpError';

  constructor(
    readonly status: number,
    readonly body: string,
    readonly serverMessage?: string,
    readonly errorType?: string,
    readonly reason?: string,
  ) {
    super(`HTTP ${status}: ${serverMessage ?? body}`);
  }
}

/** Thrown on HTTP 401/403. Auth / authorization failure — do not retry; mint
 *  a fresh scoped token (or rotate the service-token secret) and try again. */
export class AkribesAuthError extends AkribesHttpError {
  override readonly name: string = 'AkribesAuthError';

  constructor(status: 401 | 403, body: string, serverMessage: string | undefined) {
    super(status, body, serverMessage);
  }
}

/** Thrown on HTTP 429. Carries `retryAfter` (parsed from the `Retry-After`
 *  header) in **seconds** when the server sent numeric-seconds form; `null`
 *  when the header is absent or in HTTP-date form (matching Python). */
export class AkribesRateLimitError extends AkribesHttpError {
  override readonly name: string = 'AkribesRateLimitError';

  constructor(
    body: string,
    serverMessage: string | undefined,
    public readonly retryAfter: number | null,
  ) {
    super(429, body, serverMessage);
  }
}

/** Recommended base backoff (in milliseconds) for a retriable HTTP status
 *  after #1296 split the umbrella `AkribesTransientHttpError` into one
 *  subclass per 5xx variant. Mirrors `ErrorKind::base_backoff_ms` on the
 *  Rust side so server + SDK retry cadences agree:
 *
 *  - 500: maybe-transient origin error — start short (1s).
 *  - 502: edge fronted a failing origin — start short (1s).
 *  - 503: rate-limit-adjacent — honour `Retry-After`; default 2s.
 *  - 504: slow upstream — start longer (4s) before retrying.
 *  - 429: rate-limit — 2s default; honour `Retry-After` when present.
 *
 *  Returned in milliseconds; callers convert to seconds with `/ 1000`.
 *  Returns `null` for non-transient statuses. */
export function recommendedBackoffMs(status: number): number | null {
  switch (status) {
    case 429: return 2_000;
    case 500: return 1_000;
    case 502: return 1_000;
    case 503: return 2_000;
    case 504: return 4_000;
    default: return null;
  }
}

/** Thrown on HTTP 500/502/503/504. Retriable server-side failure (#1296
 *  splits this into status-specific subclasses below). `retryAfter` (in
 *  seconds) is populated when the server sent a numeric `Retry-After`.
 *  Use `instanceof AkribesTransientHttpError` for the umbrella check, or
 *  branch on the specific subclass (`AkribesServerError500`,
 *  `AkribesBadGatewayError502`, `AkribesServiceUnavailableError503`,
 *  `AkribesGatewayTimeoutError504`) for per-status retry cadence. */
export class AkribesTransientHttpError extends AkribesHttpError {
  override readonly name: string = 'AkribesTransientHttpError';

  constructor(
    status: 500 | 502 | 503 | 504,
    body: string,
    serverMessage: string | undefined,
    public readonly retryAfter: number | null,
  ) {
    super(status, body, serverMessage);
  }

  /** Recommended base backoff in milliseconds for this transient status
   *  (#1296). Prefer `retryAfter * 1000` when the server sent the header;
   *  fall back to this when it didn't. */
  recommendedBackoffMs(): number {
    return recommendedBackoffMs(this.status) ?? 1_000;
  }
}

/** Thrown on HTTP 500 (internal server error). The origin reported a
 *  generic failure; retry with a short exponential backoff. (#1296) */
export class AkribesServerError500 extends AkribesTransientHttpError {
  override readonly name: string = 'AkribesServerError500';

  constructor(body: string, serverMessage: string | undefined, retryAfter: number | null) {
    super(500, body, serverMessage, retryAfter);
  }
}

/** Thrown on HTTP 502 (bad gateway). The provider's edge fronted a
 *  failing origin; retry with a short backoff. (#1296) */
export class AkribesBadGatewayError502 extends AkribesTransientHttpError {
  override readonly name: string = 'AkribesBadGatewayError502';

  constructor(body: string, serverMessage: string | undefined, retryAfter: number | null) {
    super(502, body, serverMessage, retryAfter);
  }
}

/** Thrown on HTTP 503 (service unavailable). Rate-limit-adjacent; honour
 *  `Retry-After` aggressively, otherwise back off at the rate-limit
 *  cadence. (#1296) */
export class AkribesServiceUnavailableError503 extends AkribesTransientHttpError {
  override readonly name: string = 'AkribesServiceUnavailableError503';

  constructor(body: string, serverMessage: string | undefined, retryAfter: number | null) {
    super(503, body, serverMessage, retryAfter);
  }
}

/** Thrown on HTTP 504 (gateway timeout). The upstream is slow or stuck;
 *  start with a longer base backoff than for 500/502. (#1296) */
export class AkribesGatewayTimeoutError504 extends AkribesTransientHttpError {
  override readonly name: string = 'AkribesGatewayTimeoutError504';

  constructor(body: string, serverMessage: string | undefined, retryAfter: number | null) {
    super(504, body, serverMessage, retryAfter);
  }
}

/** Parse a `Retry-After` header value into a numeric **seconds** value.
 *  Returns `null` for missing, empty, or HTTP-date values (matching the
 *  Python SDK's `_parse_retry_after`). Exported for tests + reuse. */
export function parseRetryAfter(headerValue: string | null | undefined): number | null {
  if (headerValue == null) return null;
  const trimmed = headerValue.trim();
  if (!trimmed) return null;
  const n = Number(trimmed);
  if (Number.isFinite(n) && n >= 0) return n;
  return null;
}

/** Thrown when the server returns 409 with `error_type: "suite_already_exists"`
 *  (or any other future "this resource already exists" condition). The
 *  `existingId` field carries the conflicting row's id so callers can
 *  redirect the operator to it. */
export class AkribesAlreadyExistsError extends AkribesHttpError {
  override readonly name: string = 'AkribesAlreadyExistsError';

  constructor(
    status: number,
    body: string,
    serverMessage: string | undefined,
    public readonly existingId: number,
  ) {
    super(status, body, serverMessage);
  }
}

export class AkribesTransientError extends AkribesError {
  override readonly name = 'AkribesTransientError';

  constructor(message: string, public executionId?: string) {
    super(message);
  }
}

export class AkribesFatalError extends AkribesError {
  override readonly name = 'AkribesFatalError';

  constructor(message: string, public executionId?: string) {
    super(message);
  }
}

export class AkribesScriptError extends AkribesError {
  override readonly name = 'AkribesScriptError';

  constructor(message: string, public executionId?: string) {
    super(message);
  }
}

/** Thrown by polling helpers (`waitFor` / `await`) when an execution does
 *  not reach a terminal state within the supplied timeout. Carries the
 *  execution id + elapsed budget so callers can route timeouts distinctly
 *  from script / transient failures. Mirrors Python's `AkribesTimeoutError`.
 *
 *  The message still contains "timed out" verbatim for back-compat with
 *  pre-#109 callers that grep on it. */
export class AkribesTimeoutError extends AkribesError {
  override readonly name = 'AkribesTimeoutError';

  constructor(
    message: string,
    public readonly executionId: string,
    public readonly timeoutMs: number,
  ) {
    super(message);
  }
}

/** Thrown when run() is called after a ScriptUpdated event with schema_changed=true. */
export class ScriptSchemaChangedError extends AkribesError {
  override readonly name = 'ScriptSchemaChangedError';

  constructor(public scriptName: string) {
    super(`Script "${scriptName}" schema has changed since init(). Re-register to continue.`);
  }
}

/** One entry in the server's `input_validation_failed` payload. */
export type InputValidationErrorEntry = {
  /** Dotted / bracketed path to the offending field: "payload.b", "items[2].qty". */
  input: string;
  code: 'missing' | 'wrong_type' | 'unknown_field' | 'unknown_input' | 'disallowed_type';
  expected?: string;
  got?: string;
};

/** Parse a 400 `input_validation_failed` body off an AkribesHttpError.
 *  Returns null when the error is something else or the body doesn't match. */
export function tryParseInputValidationErrors(err: unknown): InputValidationErrorEntry[] | null {
  if (!(err instanceof AkribesHttpError) || err.status !== 400) return null;
  try {
    const body = JSON.parse(err.body);
    if (body?.error === 'input_validation_failed' && Array.isArray(body.errors)) {
      return body.errors as InputValidationErrorEntry[];
    }
    return null;
  } catch {
    return null;
  }
}

/** Thrown when run() is called with document keys that don't match the cached schema. */
export class ScriptInputMismatchError extends AkribesError {
  override readonly name = 'ScriptInputMismatchError';

  constructor(
    public scriptName: string,
    public missing: string[],
    public extra: string[],
  ) {
    const parts: string[] = [];
    if (missing.length) parts.push(`missing: ${missing.join(', ')}`);
    if (extra.length) parts.push(`extra: ${extra.join(', ')}`);
    super(`Script "${scriptName}" input mismatch: ${parts.join('; ')}`);
  }
}

/** Thrown when the server returns 404 with a structured `{"error": ...}`
 *  body. Carries the parsed `error` message so callers can distinguish
 *  "no bench configured" from "bench run not found" without regex-matching
 *  `message`. Status is always 404. */
export class AkribesNotFoundError extends AkribesHttpError {
  override readonly name = 'AkribesNotFoundError';

  constructor(body: string, serverMessage: string | undefined) {
    super(404, body, serverMessage, undefined, undefined);
  }
}

/** One entry in a `case_type_mismatch` 400 response's `field_errors` array.
 *  Mirrors `akribes_core::contracts::TypeMismatch`. The `path` uses the
 *  same dotted/bracketed convention as Studio's `ObjectInputForm`
 *  `errorsByPath` so a form layer can highlight the offending leaf inline. */
export type CaseFieldError = {
  path: string;
  message: string;
};

/** Thrown by case-create / promote-execution when the server rejects the
 *  payload with structured per-field violations (HTTP 400 +
 *  `error: "case_type_mismatch"`). Callers in the form layer catch this
 *  and populate `errorsByPath` from {@link CaseTypeMismatchError.fieldErrors}. */
export class CaseTypeMismatchError extends AkribesHttpError {
  override readonly name = 'CaseTypeMismatchError';

  constructor(
    body: string,
    serverMessage: string | undefined,
    public readonly fieldErrors: CaseFieldError[],
  ) {
    super(400, body, serverMessage, 'case_type_mismatch', undefined);
  }
}

/** Thrown by `bench.triggerRun()` when the workflow's outputs are
 *  incompatible with the judge's `inputs.{expected,actual}` slots. The
 *  server returns 400 with a `Judge contract mismatch: ...` message; the
 *  SDK parses the trailing `N field(s) incompatible: ...` fragment into a
 *  `breaks` list when present, leaving it empty otherwise. */
export class JudgeContractError extends AkribesHttpError {
  override readonly name = 'JudgeContractError';

  constructor(
    body: string,
    serverMessage: string | undefined,
    public readonly breaks: string[],
  ) {
    super(400, body, serverMessage, 'judge_contract_mismatch', undefined);
  }
}
