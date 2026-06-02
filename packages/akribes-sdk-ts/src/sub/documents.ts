// Document ingest sub-client. Mirrors the Rust akribes-sdk
// `DocumentsClient` surface (claim / upload / ingest), plus an `onPhase`
// progress callback for browser UI.
//
// See `docs/superpowers/specs/2026-04-25-studio-documents-ingest-design.md`.

import type { HttpClient } from '../http';
import { AkribesHttpError } from '../errors';

/** Translate `AkribesHttpError`s carrying the server's `conversion_failed`
 *  envelope into a typed `DocumentConversionError`. Other HTTP errors are
 *  re-thrown verbatim so callers can still inspect `status` / `body`. */
function rethrowConversionFailure(e: unknown): never {
  if (e instanceof AkribesHttpError && e.errorType === 'conversion_failed') {
    // serverMessage is the user-facing copy assembled by the server's
    // `client_facing(reason)` helper. Pass it through unchanged.
    throw new DocumentConversionError('', e.serverMessage ?? e.message, e.reason);
  }
  throw e;
}

export type ConversionStatus =
  | 'text'
  | 'ready'
  | 'converting'
  | 'pending'
  | 'failed'
  | 'unknown';

export type UploadResult = {
  document_id: string;
  filename: string;
  content_hash: string;
  conversion_status: ConversionStatus;
};

export type ClaimOutcome =
  | { status: 'hit'; result: UploadResult }
  | { status: 'miss' };

export type IngestPhase = 'claiming' | 'uploading' | 'converting' | 'ready';

/** Per-page progress while a conversion is in flight on the server.
 *  `done` and `total` are page counts (not chunks). `total = 0` means the
 *  server hasn't yet rasterized far enough to know — render an indeterminate
 *  bar in that case. */
export type IngestProgress = { done: number; total: number };

export type IngestOptions = {
  signal?: AbortSignal;
  onPhase?: (phase: IngestPhase) => void;
  /** Fires periodically while the server is converting. Polls a separate
   *  metadata endpoint — never carries markdown, just page counts.
   *  Frequency: every ~750 ms during the converting phase. */
  onProgress?: (p: IngestProgress) => void;
  /** Maximum time to keep polling a still-converting blob, in milliseconds.
   *  When omitted, falls back to the client-level
   *  `AkribesClientOptions.ingestPollTimeoutMs` (which itself defaults to
   *  {@link DEFAULT_INGEST_POLL_TIMEOUT_MS} = 300 s). Use this per-call
   *  option to override for a specific ingest, e.g. unusually large PDFs. */
  pollTimeoutMs?: number;
};

/** Default poll budget for {@link DocumentsClient.ingest}, in milliseconds.
 *
 *  20 minutes. The previous 5 min default still surfaced
 *  `IngestTimeoutError`s on perfectly healthy big-PDF conversions while the
 *  server quietly kept working — that's a UX bug, not a hang signal. Real
 *  hangs are caught by the server's own watchdogs (see
 *  `wait_for_blob_ready`); the SDK timeout exists only as a final hard cap
 *  so callers don't poll forever on a process leak. UIs are expected to
 *  surface live progress via `onProgress` and offer a Cancel button rather
 *  than waiting for this deadline.
 *
 *  Override via:
 *  - per-ingest: {@link IngestOptions.pollTimeoutMs}
 *  - per-client: `AkribesClientOptions.ingestPollTimeoutMs`
 *  - process-wide: env var `AKRIBES_SDK_INGEST_TIMEOUT_SECS` (Node/Bun only).
 */
export const DEFAULT_INGEST_POLL_TIMEOUT_MS = 20 * 60 * 1000;

/** Read `AKRIBES_SDK_INGEST_TIMEOUT_SECS` from the host's process env (Node/Bun
 *  expose `process.env`; browser bundles don't and will skip this).
 *  Returns `undefined` if unset, zero, or unparseable — the caller falls back
 *  to the next-lower-precedence value. */
export function ingestPollTimeoutMsFromEnv(): number | undefined {
  // Browsers don't have `process`; guard so this file is tree-shake-safe.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const proc: any = (globalThis as any).process;
  const raw = proc?.env?.AKRIBES_SDK_INGEST_TIMEOUT_SECS;
  if (typeof raw !== 'string' || raw.trim() === '') return undefined;
  const n = Number(raw);
  if (!Number.isFinite(n) || n <= 0) return undefined;
  return Math.floor(n) * 1000;
}

// Internal wire type — mirrors the server's `ClaimResponse` discriminated union
// (see crates/akribes-server/src/handlers/ingest.rs).
type ClaimResponseWire =
  | {
      status: 'hit';
      document_id: string;
      filename: string;
      content_hash: string;
      conversion_status: ConversionStatus;
    }
  | { status: 'miss' };

/** Thrown when the conversion pipeline fails. Two trigger paths:
 *
 *  1. The server returned a 4xx/5xx with `error_type: "conversion_failed"`
 *     during claim/upload (e.g. the VLM rejected the file, pdfium couldn't
 *     read it, the conversion service was down). `document_id` is empty
 *     because no document was created.
 *  2. The server returned 200 with `conversion_status: "failed"` from claim
 *     (the bytes have a poisoned blob row that hasn't been reclaimed yet).
 *
 *  The `message` is server-supplied and user-facing — surface it directly in
 *  the UI instead of swapping in a generic fallback. `reason` is a stable
 *  machine-readable string for branching:
 *  `service_unavailable`, `service_rejected`, `file_too_large`,
 *  `invalid_file`, `extraction_failed`. */
export class DocumentConversionError extends Error {
  readonly document_id: string;
  readonly reason?: string;
  constructor(document_id: string, message: string, reason?: string) {
    super(message);
    this.name = 'DocumentConversionError';
    this.document_id = document_id;
    this.reason = reason;
  }
}

/** Thrown when `ingest()` polls past its `pollTimeoutMs` deadline. */
export class IngestTimeoutError extends Error {
  readonly document_id: string;
  readonly elapsed_ms: number;
  constructor(message: string, document_id: string, elapsed_ms: number) {
    super(message);
    this.name = 'IngestTimeoutError';
    this.document_id = document_id;
    this.elapsed_ms = elapsed_ms;
  }
}

/** Thrown for protocol-level problems: unknown `conversion_status` strings,
 *  `crypto.subtle` unavailable, etc. Signals schema drift or missing browser
 *  features — surfacing loudly is the point. */
export class IngestProtocolError extends Error {
  readonly received_status?: string;
  constructor(message: string, received_status?: string) {
    super(message);
    this.name = 'IngestProtocolError';
    this.received_status = received_status;
  }
}

/** Compute a hex-encoded SHA-256 digest of the given bytes. Throws
 *  `IngestProtocolError` if `crypto.subtle` isn't available (insecure
 *  context — e.g. plain HTTP deploy). */
async function sha256Hex(bytes: Uint8Array): Promise<string> {
  if (typeof crypto === 'undefined' || !crypto.subtle) {
    throw new IngestProtocolError(
      'SHA-256 unavailable: ingest requires a secure context (HTTPS or localhost)',
    );
  }
  // Cast: TypeScript 5.7+ types `bytes` as `Uint8Array<ArrayBufferLike>`,
  // whose `ArrayBufferLike` admits `SharedArrayBuffer` and so isn't assignable
  // to the DOM/webcrypto `BufferSource` (which wants a regular `ArrayBuffer`).
  // Our bytes are always regular (non-shared) buffers, so narrowing the buffer
  // param to `ArrayBuffer` is safe. Casting to the concrete `Uint8Array<
  // ArrayBuffer>` (rather than the global `BufferSource`, which is absent under
  // this package's no-DOM check config) typechecks in both that config and
  // Studio's DOM-enabled build, which compiles this source directly.
  const buf = await crypto.subtle.digest('SHA-256', bytes as Uint8Array<ArrayBuffer>);
  return Array.from(new Uint8Array(buf))
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('');
}

/** Validate a `conversion_status` string from the wire. Throws
 *  `IngestProtocolError` on `'unknown'` — schema drift signal. */
function assertKnownStatus(s: ConversionStatus): void {
  if (s === 'unknown') {
    throw new IngestProtocolError(
      'received unknown conversion_status from server (schema drift)',
      'unknown',
    );
  }
}

/** Throw `DocumentConversionError` if the server reports a `failed` status. */
function ensureNotFailed(r: UploadResult): void {
  if (r.conversion_status === 'failed') {
    throw new DocumentConversionError(
      r.document_id,
      `document ${r.document_id} conversion failed on the server — re-upload or call reconvert`,
    );
  }
}

/** Wire shape of `GET /projects/{pid}/documents/by-hash/{hash}/progress`. */
type ProgressResponseWire =
  | { state: 'converting'; done_pages: number; total_pages: number }
  | { state: 'idle' };

export class DocumentsClient {
  /** Resolved client-level default poll timeout. Per-call
   *  {@link IngestOptions.pollTimeoutMs} still wins. */
  private readonly defaultPollTimeoutMs: number;

  constructor(
    private http: HttpClient,
    private projectId: number,
    defaultPollTimeoutMs?: number,
  ) {
    this.defaultPollTimeoutMs = defaultPollTimeoutMs ?? DEFAULT_INGEST_POLL_TIMEOUT_MS;
  }

  /** The poll timeout this client applies when an `ingest()` caller doesn't
   *  pass `pollTimeoutMs`. Mostly useful for tests / diagnostics. */
  getDefaultPollTimeoutMs(): number {
    return this.defaultPollTimeoutMs;
  }

  /** Fetch a snapshot of the server-side conversion progress for a content
   *  hash. Returns `null` if the server has no in-flight conversion (either
   *  it's terminal already, or there's nothing to show). Cheap (a few-byte
   *  JSON response off an in-memory map). */
  async progress(content_hash: string): Promise<IngestProgress | null> {
    const url = `${this.http.getBaseUrl()}/projects/${this.projectId}/documents/by-hash/${content_hash}/progress`;
    const res = await this.http.fetchOk(url);
    const wire = (await res.json()) as ProgressResponseWire;
    if (wire.state === 'idle') return null;
    return { done: wire.done_pages, total: wire.total_pages };
  }

  async claim(content_hash: string, filename: string): Promise<ClaimOutcome> {
    const url = `${this.http.getBaseUrl()}/projects/${this.projectId}/documents/claim`;
    const res = await this.http.fetchOk(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ content_hash, filename }),
    }).catch(rethrowConversionFailure);
    const wire = await res.json() as ClaimResponseWire;
    if (wire.status === 'hit') {
      return {
        status: 'hit',
        result: {
          document_id: wire.document_id,
          filename: wire.filename,
          content_hash: wire.content_hash,
          conversion_status: wire.conversion_status,
        },
      };
    }
    return { status: 'miss' };
  }

  async upload(
    filename: string,
    bytes: Uint8Array,
    opts?: { signal?: AbortSignal },
  ): Promise<UploadResult> {
    const form = new FormData();
    // Same `Uint8Array<ArrayBufferLike>` → non-shared-buffer narrowing as in
    // `sha256Hex`; a `Uint8Array<ArrayBuffer>` is a valid `BlobPart` under both
    // the no-DOM check config and Studio's DOM build.
    form.append('file', new Blob([bytes as Uint8Array<ArrayBuffer>]), filename);
    const url = `${this.http.getBaseUrl()}/projects/${this.projectId}/documents`;
    // POST /documents blocks for the full server-side conversion. Bun
    // enforces an internal 5-minute fetch timeout that AbortSignal cannot
    // override; on dense documents the fetch will throw `TimeoutError` while
    // the server keeps working. `ingest()` catches that and falls back to
    // claim-polling, so we just propagate the error here.
    const res = await this.http.fetchOk(url, {
      method: 'POST',
      body: form,
      signal: opts?.signal,
    }).catch(rethrowConversionFailure);
    return (await res.json()) as UploadResult;
  }

  async ingest(
    filename: string,
    bytes: Uint8Array,
    opts?: IngestOptions,
  ): Promise<UploadResult> {
    const { signal, onPhase, onProgress, pollTimeoutMs = this.defaultPollTimeoutMs } =
      opts ?? {};

    onPhase?.('claiming');
    const hash = await sha256Hex(bytes);

    const initial = await this.claim(hash, filename);

    // Side-channel progress poller. Runs concurrently with the (blocking)
    // upload fetch, hits the in-memory progress map endpoint every ~750 ms
    // and pushes updates through `onProgress`. Stops as soon as `done`
    // becomes 0 again (server cleared the entry → conversion finished) or
    // the abort flag is set.
    let progressPollerActive = false;
    const stopProgressPoller = () => { progressPollerActive = false; };
    const startProgressPoller = () => {
      if (!onProgress || progressPollerActive) return;
      progressPollerActive = true;
      void (async () => {
        // Two distinct null cases:
        //   1) Server hasn't created the entry yet (pdfium still parsing).
        //      Keep polling — entry shows up once the page count is known.
        //   2) Server cleared the entry (conversion finished). Stop.
        // We only treat null as "finished" *after* we've seen a non-null
        // snapshot at least once.
        let everSawProgress = false;
        while (progressPollerActive && !signal?.aborted) {
          try {
            const snap = await this.progress(hash);
            if (snap) {
              everSawProgress = true;
              onProgress(snap);
            } else if (everSawProgress) {
              break;
            }
          } catch {
            // Don't fail the upload because progress polling glitched.
          }
          await new Promise((r) => setTimeout(r, 750));
        }
      })();
    };

    let result: UploadResult;
    if (initial.status === 'miss') {
      onPhase?.('uploading');
      startProgressPoller();
      // The blocking upload connection can die before the server finishes
      // converting for several distinct reasons, all of which mean
      // "fetch is gone, bytes are still on the server, fall back to
      // claim-polling so the conversion result can still be surfaced":
      //
      // 1. Bun's fetch enforces an internal 5-minute timeout that even
      //    AbortSignal.timeout(longer) cannot extend. Surfaces as
      //    `DOMException: TimeoutError`. (Server-side caller only.)
      // 2. Browsers (Firefox in particular) drop the connection mid-
      //    request when an idle keepalive elapses on a hop between
      //    browser and aura-server (ISP NAT reaper, Traefik upstream
      //    pool, OS conntrack). Firefox surfaces this as
      //    `TypeError: NetworkError when attempting to fetch resource`,
      //    Chrome as `TypeError: Failed to fetch`, Safari as
      //    `TypeError: Load failed`. None of these are `TimeoutError`,
      //    so a narrow timeout-only catch lets them propagate to the
      //    UI as a misleading "upload failed" while the server is in
      //    fact still busy converting.
      //
      // Detection: treat any `TypeError` from `fetch()` whose message
      // matches the well-known network-failure phrases as recoverable.
      // This is the same shape the WHATWG spec mandates fetch throw on
      // network errors, so the heuristic is stable across browsers.
      try {
        result = await this.upload(filename, bytes, { signal });
      } catch (e: any) {
        const isFetchTimeout =
          e?.name === 'TimeoutError' ||
          (e instanceof DOMException && /timed? *out/i.test(e.message));
        const isFetchNetworkError =
          e instanceof TypeError &&
          /network ?error|failed to fetch|load failed/i.test(e?.message ?? '');
        if (!isFetchTimeout && !isFetchNetworkError) throw e;

        // Switch to converting phase and let the existing poll loop below take
        // over by simulating a Hit-converting initial state.
        onPhase?.('converting');
        result = {
          document_id: '',
          filename,
          content_hash: hash,
          conversion_status: 'converting',
        };
      }
    } else {
      // Hit path — may be terminal (return immediately) or non-terminal (poll).
      assertKnownStatus(initial.result.conversion_status);
      ensureNotFailed(initial.result);
      result = initial.result;
    }

    // Poll until terminal. Reached when:
    //   - Hit returned a non-terminal status (concurrent uploader still
    //     processing), or
    //   - Miss + upload's fetch died at Bun's 5-min timeout (we converted to
    //     a synthetic converting state above).
    if (result.conversion_status === 'converting' || result.conversion_status === 'pending') {
      onPhase?.('converting');
      // Hit-converting branch hasn't started the poller yet.
      startProgressPoller();
      const startedAt = Date.now();
      const deadline = startedAt + pollTimeoutMs;
      let backoffMs = 250;

      while (result.conversion_status === 'converting' || result.conversion_status === 'pending') {
        if (signal?.aborted) {
          throw new DOMException('Aborted', 'AbortError');
        }
        if (Date.now() >= deadline) {
          const elapsed = Date.now() - startedAt;
          throw new IngestTimeoutError(
            'Document conversion did not finish within 20 minutes. The server is still working in the background. Refresh the page in a few minutes to check, or contact support.',
            result.document_id,
            elapsed,
          );
        }
        await new Promise((resolve) => setTimeout(resolve, backoffMs));
        backoffMs = Math.min(backoffMs * 2, 2000);

        const pollOutcome = await this.claim(hash, filename);
        if (pollOutcome.status === 'miss') {
          // GC reclaimed the blob mid-poll; re-upload to repopulate.
          onPhase?.('uploading');
          const uploaded = await this.upload(filename, bytes, { signal });
          assertKnownStatus(uploaded.conversion_status);
          ensureNotFailed(uploaded);
          onPhase?.('ready');
          return uploaded;
        }
        assertKnownStatus(pollOutcome.result.conversion_status);
        ensureNotFailed(pollOutcome.result);
        result = pollOutcome.result;
      }
    }

    assertKnownStatus(result.conversion_status);
    ensureNotFailed(result);
    stopProgressPoller();
    onPhase?.('ready');
    return result;
  }
}
