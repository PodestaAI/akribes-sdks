import {
  AkribesHttpError,
  AkribesAlreadyExistsError,
  AkribesAuthError,
  AkribesNotFoundError,
  AkribesRateLimitError,
  AkribesServerError500,
  AkribesBadGatewayError502,
  AkribesServiceUnavailableError503,
  AkribesGatewayTimeoutError504,
  AkribesTransientHttpError,
  parseRetryAfter,
} from './errors';

export type RequestOptions = RequestInit & {
  signal?: AbortSignal;
};

/**
 * Callback that writes outbound-request tracing headers into `carrier`
 * (typically W3C `traceparent`/`tracestate`). Returning no headers is a
 * valid no-op; the SDK itself has zero OpenTelemetry dependencies, so
 * callers that want distributed tracing wire this from their own OTel
 * setup — e.g. Studio does:
 *
 * ```ts
 * import { propagation, context } from '@opentelemetry/api';
 * new AkribesClient({ propagator: (carrier) => propagation.inject(context.active(), carrier) });
 * ```
 */
export type TracePropagator = (carrier: Record<string, string>) => void;

export class HttpClient {
  constructor(
    private baseUrl: string,
    private getToken: () => string | undefined,
    private onBehalfOf?: string,
    private propagator?: TracePropagator,
  ) {}

  /** Build auth headers if a token is set. */
  authHeaders(): Record<string, string> {
    const token = this.getToken();
    const headers: Record<string, string> = {};
    if (token) headers['Authorization'] = `Bearer ${token}`;
    // Servers also accept the legacy `X-Aura-User` for backwards compat,
    // but new clients emit the Akribes form.
    if (this.onBehalfOf) headers['X-Akribes-User'] = this.onBehalfOf;
    return headers;
  }

  /** Expose the configured token for callers that need to embed it in
   *  a URL (currently: browser SSE `?token=` fallback, which can't use
   *  headers). Always returns the raw token — call sites must run it
   *  through {@link assertTokenSafeInUrl} before appending. */
  rawToken(): string | undefined {
    return this.getToken();
  }

  /** Inject W3C trace-context headers if a propagator is configured. */
  traceHeaders(): Record<string, string> {
    if (!this.propagator) return {};
    const carrier: Record<string, string> = {};
    this.propagator(carrier);
    return carrier;
  }

  /** Fetch with auth + trace headers injected automatically. */
  async authFetch(url: string, options?: RequestOptions): Promise<Response> {
    const headers = {
      ...this.authHeaders(),
      ...this.traceHeaders(),
      ...options?.headers as Record<string, string>,
    };
    return fetch(url, { ...options, headers });
  }

  /** Throws a typed AkribesHttpError subclass (or AkribesHttpError directly)
   *  when the server returns a non-2xx response.
   *
   *  Per #987 the SDK now maps common HTTP statuses to dedicated subclasses
   *  so callers can branch on `instanceof AkribesRateLimitError` instead of
   *  `err instanceof AkribesHttpError && err.status === 429`. The base class
   *  `AkribesHttpError` is still thrown for unknown statuses, and
   *  `instanceof AkribesHttpError` still catches everything. */
  async fetchOk(url: string, options?: RequestOptions): Promise<Response> {
    const res = await this.authFetch(url, options);
    if (!res.ok) {
      const body = await res.text();
      let serverMessage: string | undefined;
      let errorType: string | undefined;
      let reason: string | undefined;
      let existingId: number | undefined;
      try {
        const json = JSON.parse(body);
        if (typeof json.error === 'string') serverMessage = json.error;
        if (typeof json.error_type === 'string') errorType = json.error_type;
        if (typeof json.reason === 'string') reason = json.reason;
        if (typeof json.existing_suite_id === 'number') existingId = json.existing_suite_id;
      } catch { /* not JSON */ }
      const retryAfter = parseRetryAfter(res.headers.get('Retry-After'));
      if (res.status === 401 || res.status === 403) {
        throw new AkribesAuthError(res.status as 401 | 403, body, serverMessage);
      }
      if (res.status === 404) {
        throw new AkribesNotFoundError(body, serverMessage);
      }
      if (res.status === 409 && errorType === 'suite_already_exists' && existingId !== undefined) {
        throw new AkribesAlreadyExistsError(res.status, body, serverMessage, existingId);
      }
      if (res.status === 429) {
        throw new AkribesRateLimitError(body, serverMessage, retryAfter);
      }
      // #1296: dispatch to the specific 5xx subclass so callers can branch on
      // `instanceof AkribesGatewayTimeoutError504` etc. without enumerating
      // every status. `AkribesTransientHttpError` is still the umbrella for
      // any of them.
      if (res.status === 500) {
        throw new AkribesServerError500(body, serverMessage, retryAfter);
      }
      if (res.status === 502) {
        throw new AkribesBadGatewayError502(body, serverMessage, retryAfter);
      }
      if (res.status === 503) {
        throw new AkribesServiceUnavailableError503(body, serverMessage, retryAfter);
      }
      if (res.status === 504) {
        throw new AkribesGatewayTimeoutError504(body, serverMessage, retryAfter);
      }
      throw new AkribesHttpError(
        res.status,
        body,
        serverMessage ?? (body || res.statusText),
        errorType,
        reason,
      );
    }
    return res;
  }

  /** Fetch + decode the JSON body, typed as `T`. `Response.json()` is
   *  declared to return `Promise<any>` (effectively `unknown` under our
   *  `strict` config), so the cast lives here once instead of at every
   *  call site. Runtime behaviour is identical to
   *  `(await this.fetchOk(url, options)).json()`; the assertion is the
   *  standard JSON→T boundary cast — callers keep their declared return
   *  type accurate to the server shape. */
  async fetchJson<T>(url: string, options?: RequestOptions): Promise<T> {
    const res = await this.fetchOk(url, options);
    return res.json() as Promise<T>;
  }

  /** Build a project-scoped script path with proper encoding. */
  scriptPath(projectId: number, scriptName: string, ...segments: string[]): string {
    const parts = [
      `${this.baseUrl}/projects/${projectId}/scripts/${encodeURIComponent(scriptName)}`,
      ...segments.map(s => encodeURIComponent(s)),
    ];
    return parts.join('/');
  }

  getBaseUrl(): string {
    return this.baseUrl;
  }

  /** Set the X-Akribes-User header for metrics attribution. */
  setOnBehalfOf(email: string | undefined) {
    this.onBehalfOf = email;
  }

  /** Replace the trace propagator after construction. */
  setPropagator(propagator: TracePropagator | undefined) {
    this.propagator = propagator;
  }
}

/** Catch 404 errors and return null, rethrow everything else. */
export async function nullOn404<T>(fn: () => Promise<T>): Promise<T | null> {
  try {
    return await fn();
  } catch (e) {
    if (e instanceof AkribesHttpError && e.status === 404) return null;
    throw e;
  }
}
