/** Minimal shape of the browser `MessageEvent` fields this module reads.
 *  The base tsconfig targets `lib: ["ESNext"]` (no DOM), so the global
 *  `MessageEvent` interface isn't visible here; the EventSource path below
 *  only runs in a real browser where these fields are present. */
type SseEvent = { data: string; lastEventId?: string };

/** Minimal browser `EventSource` surface used by the EventSource path. Same
 *  rationale as {@link SseEvent}: without the DOM lib, bun-types exposes
 *  `EventSource` as an empty interface, so we describe the members we touch
 *  and cast the constructed instance to this shape. */
interface BrowserEventSource {
  onopen: (() => void) | null;
  onmessage: ((e: SseEvent) => void) | null;
  onerror: (() => void) | null;
  addEventListener(type: string, listener: (e: SseEvent) => void): void;
  close(): void;
}

export type SseMessage = {
  event: string;
  data: string;
  /** SSE `id:` field (server's monotonic per-execution `seq`). Tracked by
   *  callers so a reconnect can send `Last-Event-ID: <id>` per the SSE spec
   *  and avoid silently dropping events emitted during the gap. */
  id?: string;
};

/** Parse a complete SSE message block (lines between blank-line delimiters). */
export function parseSseMessage(block: string): SseMessage | null {
  let event = '';
  let data = '';
  let id: string | undefined;
  for (const line of block.split('\n')) {
    if (line.startsWith('data: ')) {
      if (data) data += '\n';
      data += line.slice(6);
    } else if (line.startsWith('data:')) {
      if (data) data += '\n';
      data += line.slice(5);
    } else if (line.startsWith('event: ')) {
      event = line.slice(7);
    } else if (line.startsWith('event:')) {
      event = line.slice(6);
    } else if (line.startsWith('id: ')) {
      id = line.slice(4);
    } else if (line.startsWith('id:')) {
      id = line.slice(3);
    }
    // retry: field is still ignored
  }
  if (!data) return null;
  return id !== undefined ? { event, data, id } : { event, data };
}

export type EventStreamOptions = {
  /** Either a static URL, or a function that returns a fresh URL on each
   * (re)connect — useful when the URL carries a token that may have been
   * refreshed since the last connection. */
  url: string | (() => Promise<string> | string);
  headers?: Record<string, string>;
  signal?: AbortSignal;
  onMessage: (msg: SseMessage) => void;
  onError?: (error: Error) => void;
  /** Fires immediately after the SSE response is open — i.e. EventSource
   * emits `onopen`, or the fetch-fallback receives a 2xx response with a
   * body. May fire on each successful reconnect. Use this from callers
   * that need to know the server-side broadcast subscriber is attached
   * before issuing a dependent POST (subscribe-before-POST race-avoidance). */
  onOpen?: () => void;
  reconnect?: boolean;
  /** Initial reconnect delay in ms. Backoff is exponential with jitter,
   *  capped at `reconnectMaxDelayMs`. Mirrors the heartbeat backoff curve
   *  so SDK behaviour is consistent across Rust/TS/Python (#1182). */
  reconnectDelayMs?: number;
  /** Maximum reconnect delay in ms (default 30000). */
  reconnectMaxDelayMs?: number;
};

/**
 * Open an SSE connection with proper message parsing and auto-reconnection.
 * Uses EventSource when available, fetch-based ReadableStream otherwise.
 *
 * On reconnect the connector sends `Last-Event-ID: <lastSeenId>` per the
 * SSE spec — the server-side replay layer can use it to resume from the
 * gap. EventSource handles this natively; the fetch fallback wires it
 * through the request headers.
 *
 * Returns a dispose function.
 */
export function connectSse(options: EventStreamOptions): () => void {
  const {
    url,
    headers,
    onMessage,
    onError,
    onOpen,
    reconnect = true,
    reconnectDelayMs = 1000,
    reconnectMaxDelayMs = 30000,
  } = options;

  // Merged abort controller: combines external signal + internal dispose
  const internal = new AbortController();
  const disposed = () => internal.signal.aborted || options.signal?.aborted;
  const resolveUrl = async () => typeof url === 'function' ? await url() : url;

  // `Last-Event-ID` tracking: every message with an `id:` line updates this
  // cursor. On reconnect we replay it as the header so the server can backfill
  // events emitted during the gap. The cursor is captured by every parser
  // path (EventSource native + fetch-fallback parseSseMessage).
  let lastEventId: string | undefined;
  const wrappedOnMessage = (msg: SseMessage) => {
    if (msg.id !== undefined) lastEventId = msg.id;
    onMessage(msg);
  };

  // Reconnect backoff state. Exponential with full jitter, base 1s, cap 30s
  // (the SDK-wide canonical curve — see #1182).
  let attempt = 0;
  const nextDelay = () => {
    attempt += 1;
    const exp = Math.min(reconnectDelayMs * 2 ** (attempt - 1), reconnectMaxDelayMs);
    return Math.random() * exp;
  };
  const resetBackoff = () => { attempt = 0; };

  // EventSource cannot set custom request headers, so it is only useful
  // when the caller authenticates via a URL-embedded credential (e.g.
  // `?token=…`). When the caller has supplied an Authorization header,
  // we deliberately skip EventSource and use the fetch-fallback instead:
  // the header path keeps long-lived tokens out of reverse-proxy access
  // logs and OTel `http.url` span attributes.
  const hasAuthHeader = headers && (headers['Authorization'] || headers['authorization']);

  if (typeof globalThis.EventSource !== 'undefined' && !hasAuthHeader) {
    // EventSource has no public API for replacing the URL on reconnect, so
    // when the URL is dynamic we fall back to a manual reconnect loop: tear
    // down the current EventSource on error, build a fresh URL, and open a
    // new one. EventSource's native reconnect also sends `Last-Event-ID`
    // automatically — but only on the same instance, not after we tear it
    // down and re-open. So we track + replay it explicitly.
    let es: BrowserEventSource | null = null;
    let reconnecting = false;
    const open = async () => {
      if (disposed() || reconnecting) return;
      reconnecting = true;
      try {
        let resolved = await resolveUrl();
        if (disposed()) return;
        if (lastEventId !== undefined) {
          // EventSource does not let callers set headers, so we encode the
          // last-event-id in the query string for the fallback path on the
          // server. Browsers that ALSO send the native `Last-Event-ID`
          // header in this scenario are fine — the server reads whichever
          // it finds first. (See server `sse.rs`.)
          const u = new URL(resolved);
          u.searchParams.set('last_event_id', lastEventId);
          resolved = u.toString();
        }
        es = new (EventSource as unknown as new (url: string) => BrowserEventSource)(resolved);
        es.onopen = () => { resetBackoff(); onOpen?.(); };
        es.addEventListener('batch', (e: SseEvent) => {
          wrappedOnMessage({ event: 'batch', data: e.data, id: e.lastEventId });
        });
        es.onmessage = (e: SseEvent) => {
          wrappedOnMessage({ event: '', data: e.data, id: e.lastEventId });
        };
        es.onerror = () => {
          if (disposed()) return;
          onError?.(new Error('EventSource connection error'));
          if (typeof url === 'function' && reconnect) {
            try { es?.close(); } catch { /* ignore */ }
            setTimeout(() => { if (!disposed()) open(); }, nextDelay());
          }
        };
      } finally {
        reconnecting = false;
      }
    };
    open();
    const dispose = () => { internal.abort(); if (es) try { es.close(); } catch { /* ignore */ } };
    options.signal?.addEventListener('abort', dispose, { once: true });
    return dispose;
  }

  // Fetch-based fallback with reconnection
  const run = async () => {
    while (!disposed()) {
      try {
        const resolved = await resolveUrl();
        if (disposed()) return;
        const reqHeaders: Record<string, string> = { 'Accept': 'text/event-stream', ...headers };
        if (lastEventId !== undefined) {
          reqHeaders['Last-Event-ID'] = lastEventId;
        }
        const res = await fetch(resolved, {
          headers: reqHeaders,
          signal: internal.signal,
        });
        if (!res.ok || !res.body) {
          // A non-2xx (or body-less) response is a transport failure, not a
          // reason to give up. Treat it like a thrown fetch: surface it via
          // `onError` and fall through to the reconnect-with-backoff path
          // below. Returning here would silently and permanently kill the
          // stream — e.g. a restarting server or a gateway hiccup returning
          // 503 would never recover even with `reconnect: true`.
          onError?.(new Error(`SSE connection failed: HTTP ${res.status}`));
          if (!reconnect || disposed()) return;
          await new Promise((r) => setTimeout(r, nextDelay()));
          continue;
        }
        // Subscribe-before-POST hand-off: the server has accepted the GET
        // (and thus attached its broadcast subscriber) the moment fetch
        // resolves with a 2xx response and a body. Fire `onOpen` before
        // we start parsing so callers waiting on the ready signal can
        // proceed to issue their dependent POST.
        resetBackoff();
        onOpen?.();
        const reader = res.body.getReader();
        const decoder = new TextDecoder();
        let buf = '';

        while (!disposed()) {
          const { value, done } = await reader.read();
          if (done) break;
          buf += decoder.decode(value, { stream: true });

          // Process complete SSE messages delimited by blank lines.
          // Per the SSE spec, \n\n, \r\n\r\n, and \r\r are all valid
          // separators — servers or intermediaries may emit any of them.
          while (true) {
            const split = findSseDelimiter(buf);
            if (!split) break;
            const block = buf.slice(0, split.pos);
            buf = buf.slice(split.pos + split.len);
            const msg = parseSseMessage(block);
            if (msg) wrappedOnMessage(msg);
          }
        }
      } catch (e: unknown) {
        if (disposed() || (e instanceof Error && e.name === 'AbortError')) return;
        onError?.(e instanceof Error ? e : new Error(String(e)));
      }

      if (!reconnect || disposed()) return;
      await new Promise(r => setTimeout(r, nextDelay()));
    }
  };

  run();
  options.signal?.addEventListener('abort', () => internal.abort(), { once: true });
  return () => internal.abort();
}

/** Find the earliest SSE message delimiter in `buf`. Returns its position
 *  and length, or `null` if no complete delimiter is present yet. */
function findSseDelimiter(buf: string): { pos: number; len: number } | null {
  const delimiters: Array<[string, number]> = [
    ['\r\n\r\n', 4],
    ['\n\n', 2],
    ['\r\r', 2],
  ];
  let best: { pos: number; len: number } | null = null;
  for (const [d, len] of delimiters) {
    const pos = buf.indexOf(d);
    if (pos !== -1 && (best === null || pos < best.pos)) {
      best = { pos, len };
    }
  }
  return best;
}
