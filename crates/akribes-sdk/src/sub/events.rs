use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::client::Inner;
use crate::error::{AkribesError, Result};
use crate::models::*;

/// Handle to a background SSE stream. Dropping it cancels all associated
/// tasks (listener and any filter/translator tasks spawned alongside it).
pub struct EventSubscription {
    handles: Vec<JoinHandle<()>>,
}

impl EventSubscription {
    /// Explicitly cancel the subscription.
    pub fn cancel(self) {
        for h in &self.handles {
            h.abort();
        }
    }

    pub(crate) fn from_handle(handle: JoinHandle<()>) -> Self {
        Self {
            handles: vec![handle],
        }
    }

    pub(crate) fn from_handles(handles: Vec<JoinHandle<()>>) -> Self {
        Self { handles }
    }
}

impl Drop for EventSubscription {
    fn drop(&mut self) {
        for h in &self.handles {
            h.abort();
        }
    }
}

/// Sub-client for SSE event streams. Obtained via [`AkribesClient::events()`].
#[derive(Clone, Debug)]
pub struct EventsClient {
    pub(crate) inner: Arc<Inner>,
    pub(crate) project_id: i64,
}

impl EventsClient {
    pub(crate) fn new(inner: Arc<Inner>, project_id: i64) -> Self {
        Self { inner, project_id }
    }

    /// Open an SSE event stream and return a receiver + subscription handle.
    ///
    /// Events are sent to the returned `mpsc::UnboundedReceiver`. Dropping the
    /// `EventSubscription` cancels the background task automatically.
    ///
    /// **Note:** The channel is unbounded — a slow consumer on a busy execution
    /// stream can cause unbounded memory growth. Callers should process events
    /// promptly, use [`tokio::sync::mpsc::Receiver::try_recv`] to drain, or
    /// prefer [`event_stream_bounded`](Self::event_stream_bounded) (#1117)
    /// when consumer back-pressure is required.
    pub async fn event_stream(
        &self,
        script_name: Option<&str>,
    ) -> Result<(mpsc::UnboundedReceiver<HubEvent>, EventSubscription)> {
        let base_url = self.inner.base_url.clone();
        let project_id = self.project_id;
        let script_name = script_name.map(|s| s.to_string());
        let (tx, rx) = mpsc::unbounded_channel();
        let http = self.inner.http.clone();
        let token = self.inner.token.clone();

        let handle = tokio::spawn(async move {
            let _ = stream_sse_with_retry(http, token, base_url, project_id, script_name, tx, None)
                .await;
        });

        Ok((
            rx,
            EventSubscription {
                handles: vec![handle],
            },
        ))
    }

    /// Open an SSE event stream on a **bounded** channel (#1117).
    ///
    /// `buffer` is the channel's max in-flight event count. When the
    /// consumer can't keep up, the background SSE listener applies
    /// back-pressure: it parks until the consumer drains a slot.
    /// This is the safer default for long-lived subscriptions on busy
    /// executions — the unbounded variant can grow unboundedly when
    /// the consumer stalls. The trade-off is that prolonged stalls can
    /// stall the SSE listener too, which counts against the
    /// server-side keepalive window; pick `buffer` generously
    /// (e.g. 1024) when in doubt.
    ///
    /// Returns a standard bounded `mpsc::Receiver`; otherwise identical
    /// to [`event_stream`](Self::event_stream).
    pub async fn event_stream_bounded(
        &self,
        script_name: Option<&str>,
        buffer: usize,
    ) -> Result<(mpsc::Receiver<HubEvent>, EventSubscription)> {
        let base_url = self.inner.base_url.clone();
        let project_id = self.project_id;
        let script_name = script_name.map(|s| s.to_string());
        let (tx_bounded, rx_bounded) = mpsc::channel::<HubEvent>(buffer.max(1));
        // The internal SSE pipeline uses UnboundedSender; we forward
        // events from it to the bounded sender, applying back-pressure
        // there. A small inner buffer keeps the SSE parser unblocked
        // during transient sender-side awaits.
        let (tx_inner, mut rx_inner) = mpsc::unbounded_channel::<HubEvent>();
        let http = self.inner.http.clone();
        let token = self.inner.token.clone();

        let sse_handle = tokio::spawn(async move {
            let _ = stream_sse_with_retry(
                http,
                token,
                base_url,
                project_id,
                script_name,
                tx_inner,
                None,
            )
            .await;
        });
        let forward_handle = tokio::spawn(async move {
            while let Some(evt) = rx_inner.recv().await {
                if tx_bounded.send(evt).await.is_err() {
                    break;
                }
            }
        });

        Ok((
            rx_bounded,
            EventSubscription {
                handles: vec![sse_handle, forward_handle],
            },
        ))
    }

    /// Stream execution engine events for a specific script.
    pub async fn execution_stream(
        &self,
        script_name: &str,
    ) -> Result<(mpsc::UnboundedReceiver<EngineEvent>, EventSubscription)> {
        let (mut hub_rx, sub) = self.event_stream(Some(script_name)).await?;
        let (tx, rx) = mpsc::unbounded_channel();

        let outer_handle = tokio::spawn(async move {
            while let Some(evt) = hub_rx.recv().await {
                if let HubEvent::Execution { event, .. } = evt {
                    if tx.send(event).is_err() {
                        break;
                    }
                }
            }
        });

        let combined = EventSubscription {
            handles: vec![tokio::spawn(async move {
                let _sub = sub;
                outer_handle.await.ok();
            })],
        };

        Ok((rx, combined))
    }

    /// Stream execution events translated to typed [`WorkflowEvent`]s for a
    /// specific script (#1239 — mirrors Python `events.typed_engine_events`).
    ///
    /// Functionally identical to [`execution_stream`](Self::execution_stream),
    /// but each event is passed through `WorkflowEvent::from(EngineEvent)`
    /// before being yielded so consumers can pattern-match on typed
    /// variants instead of inspecting raw `EngineEvent` payloads. Use
    /// this when you want the same ergonomics as `RunStream`'s typed
    /// iterator on a free-standing execution subscription (e.g. attaching
    /// to a run started by someone else).
    pub async fn typed_execution_stream(
        &self,
        script_name: &str,
    ) -> Result<(
        mpsc::UnboundedReceiver<crate::events::WorkflowEvent>,
        EventSubscription,
    )> {
        let (mut raw_rx, sub) = self.execution_stream(script_name).await?;
        let (tx, rx) = mpsc::unbounded_channel();
        let outer_handle = tokio::spawn(async move {
            while let Some(evt) = raw_rx.recv().await {
                let typed: crate::events::WorkflowEvent = evt.into();
                if tx.send(typed).is_err() {
                    break;
                }
            }
        });
        let combined = EventSubscription {
            handles: vec![tokio::spawn(async move {
                let _sub = sub;
                outer_handle.await.ok();
            })],
        };
        Ok((rx, combined))
    }

    /// Convenience: call `callback` for every hub event.
    pub async fn on_events<F>(
        &self,
        script_name: Option<&str>,
        mut callback: F,
    ) -> Result<EventSubscription>
    where
        F: FnMut(HubEvent) + Send + 'static,
    {
        let (mut rx, sub) = self.event_stream(script_name).await?;
        let handle = tokio::spawn(async move {
            let _sub = sub;
            while let Some(evt) = rx.recv().await {
                callback(evt);
            }
        });
        Ok(EventSubscription {
            handles: vec![handle],
        })
    }

    /// Convenience: call `callback` for every execution event on a script.
    pub async fn on_script_execution<F>(
        &self,
        script_name: &str,
        mut callback: F,
    ) -> Result<EventSubscription>
    where
        F: FnMut(EngineEvent) + Send + 'static,
    {
        let (mut rx, sub) = self.execution_stream(script_name).await?;
        let handle = tokio::spawn(async move {
            let _sub = sub;
            while let Some(evt) = rx.recv().await {
                callback(evt);
            }
        });
        Ok(EventSubscription {
            handles: vec![handle],
        })
    }

    /// Convenience: call `callback` on script version updates.
    pub async fn on_script_change<F>(
        &self,
        script_name: &str,
        mut callback: F,
    ) -> Result<EventSubscription>
    where
        F: FnMut(i64, Option<String>) + Send + 'static,
    {
        let name = script_name.to_string();
        self.on_events(Some(script_name), move |hub_evt| {
            if let HubEvent::Registry(RegistryEvent::ScriptUpdated {
                script_name: ref evt_name,
                version_id,
                ref channel,
                ..
            }) = hub_evt
            {
                if *evt_name == name {
                    callback(version_id, channel.clone());
                }
            }
        })
        .await
    }

    /// Like [`on_script_change`](Self::on_script_change), but also marks the
    /// script as broken in the contract state so that subsequent `run()` calls
    /// raise before POSTing (matching the TS and Python SDK behaviour).
    pub async fn on_script_schema_change<F>(
        &self,
        script_name: &str,
        mut callback: F,
    ) -> Result<EventSubscription>
    where
        F: FnMut(i64, Option<String>) + Send + 'static,
    {
        let name = script_name.to_string();
        let inner = Arc::clone(&self.inner);
        self.on_events(Some(script_name), move |hub_evt| {
            if let HubEvent::Registry(RegistryEvent::ScriptUpdated {
                script_name: ref evt_name,
                version_id,
                ref channel,
                ..
            }) = hub_evt
            {
                if *evt_name == name {
                    inner.broken_scripts.lock().unwrap().insert(name.clone());
                    callback(version_id, channel.clone());
                }
            }
        })
        .await
    }
}

/// Build the SSE URL. The bearer token is delivered exclusively via the
/// `Authorization` header on the inner request (see `stream_sse`); older
/// revisions also appended `&token=...` here so any future caller that
/// invoked this URL directly (e.g. an EventSource shim) would still
/// authenticate, but that put long-lived service-token secrets into
/// reverse-proxy access logs and OTel `http.url` span attributes.
async fn build_events_url(base_url: &str, project_id: i64, script_name: Option<&str>) -> String {
    let mut url = format!("{}/events?project_id={}", base_url, project_id);
    if let Some(name) = script_name {
        url.push_str(&format!("&script_name={}", urlencoding::encode(name)));
    }
    url
}

/// Retry wrapper around stream_sse with exponential-with-jitter backoff
/// (base 1s, cap 30s — same curve as heartbeat per #1182).
///
/// If `ready_tx` is `Some`, it fires exactly once:
/// - `Ok(())` the moment the first `stream_sse` call returns a 2xx HTTP
///   response (the subscription is live on the server).
/// - `Err(e)` if all retries are exhausted without ever connecting.
/// This lets callers block on "SSE subscribed" before issuing a dependent
/// `POST /run` to avoid the subscribe-after-POST race where opening events
/// are lost.
///
/// Tracks the `Last-Event-ID` cursor across reconnects (#1101): every
/// successful event updates `last_event_id`, and each retry replays it
/// as the SSE-spec `Last-Event-ID` request header so the server can
/// resume from the gap when DB-backed replay lands.
pub(crate) async fn stream_sse_with_retry(
    http: reqwest::Client,
    token: Arc<tokio::sync::RwLock<Option<String>>>,
    base_url: String,
    project_id: i64,
    script_name: Option<String>,
    tx: mpsc::UnboundedSender<HubEvent>,
    mut ready_tx: Option<oneshot::Sender<Result<()>>>,
) -> Result<()> {
    let max_retries = 5u32;
    let mut attempt = 0;
    // Wrapped in an Arc<Mutex<...>> so `stream_sse` can update it in-place
    // as it consumes events. The retry wrapper reads its current value on
    // each (re)connect and threads it through the request headers.
    let last_event_id: Arc<std::sync::Mutex<Option<i64>>> = Arc::new(std::sync::Mutex::new(None));
    loop {
        let url = build_events_url(&base_url, project_id, script_name.as_deref()).await;
        let cursor = *last_event_id.lock().unwrap();
        match stream_sse(
            http.clone(),
            token.clone(),
            &url,
            tx.clone(),
            &mut ready_tx,
            cursor,
            Arc::clone(&last_event_id),
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(e) => {
                attempt += 1;
                if attempt > max_retries || tx.is_closed() {
                    if let Some(rt) = ready_tx.take() {
                        let _ = rt.send(Err(AkribesError::Other(format!(
                            "SSE subscribe failed after {} attempts: {}",
                            attempt, e
                        ))));
                    }
                    return Err(e);
                }
                let delay = retry_backoff(attempt);
                tracing::warn!(attempt, max_retries, ?delay, "SSE disconnected, retrying");
                tokio::time::sleep(delay).await;
            }
        }
    }
}

/// SDK-wide canonical SSE/heartbeat backoff curve (#1182):
/// exponential with full jitter, base 1s, cap 30s.
fn retry_backoff(attempt: u32) -> std::time::Duration {
    if attempt == 0 {
        return std::time::Duration::ZERO;
    }
    let base_ms: u64 = 1_000;
    let cap_ms: u64 = 30_000;
    let exponent = attempt.saturating_sub(1).min(20);
    let exp_ms = base_ms.saturating_mul(1u64 << exponent).min(cap_ms);
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let jitter_ms = if exp_ms == 0 { 0 } else { now_nanos % exp_ms };
    std::time::Duration::from_millis(jitter_ms)
}

/// Parse an SSE byte stream and send deserialized events to the channel.
///
/// `ready_tx` (if present) fires once the server returns a 2xx response,
/// indicating the subscription is active. Non-2xx and transport errors
/// return `Err` without firing; the retry wrapper is responsible for
/// deciding whether to fire the signal after retries are exhausted.
///
/// `cursor` (when `Some`) is sent as the `Last-Event-ID` header on the
/// request — the SSE-spec mechanism for resuming after a transport drop.
/// `last_event_id_out` is updated in-place as `id:` lines arrive, so the
/// retry wrapper has the latest cursor for the *next* attempt.
async fn stream_sse(
    http: reqwest::Client,
    token: Arc<tokio::sync::RwLock<Option<String>>>,
    url: &str,
    tx: mpsc::UnboundedSender<HubEvent>,
    ready_tx: &mut Option<oneshot::Sender<Result<()>>>,
    cursor: Option<i64>,
    last_event_id_out: Arc<std::sync::Mutex<Option<i64>>>,
) -> Result<()> {
    let mut req = http.get(url).header("Accept", "text/event-stream");
    if let Some(ref t) = *token.read().await {
        req = req.bearer_auth(t);
    }
    if let Some(seq) = cursor {
        req = req.header("Last-Event-ID", seq.to_string());
    }
    let res = req.send().await.map_err(AkribesError::Http)?;
    if !res.status().is_success() {
        return Err(AkribesError::HttpStatus {
            status: res.status().as_u16(),
            message: format!("SSE subscribe failed: {}", res.status()),
        });
    }
    if let Some(rt) = ready_tx.take() {
        let _ = rt.send(Ok(()));
    }
    let mut stream = res.bytes_stream();
    // Buffer raw bytes — reqwest's bytes_stream() yields arbitrary chunks
    // that do NOT respect UTF-8 character boundaries, so we can only decode
    // a complete SSE message once we have its delimiter.
    let mut buf: Vec<u8> = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(AkribesError::Http)?;
        buf.extend_from_slice(&chunk);

        // Process complete SSE messages. Support \n\n, \r\n\r\n, \r\r per spec.
        while let Some((msg_bytes, delim_len)) = split_sse_message_bytes(&buf) {
            // Decode just the completed message as UTF-8. If the server sent
            // invalid bytes inside a message, replace them lossily rather
            // than tearing down the stream.
            let message = String::from_utf8_lossy(&buf[..msg_bytes]).into_owned();
            buf.drain(..msg_bytes + delim_len);

            let mut data_parts: Vec<&str> = Vec::new();
            let mut event_type = String::new();
            let mut event_id: Option<i64> = None;
            for line in message.lines() {
                if let Some(rest) = line.strip_prefix("data: ") {
                    data_parts.push(rest);
                } else if let Some(rest) = line.strip_prefix("data:") {
                    data_parts.push(rest);
                } else if let Some(rest) = line.strip_prefix("event: ") {
                    event_type = rest.to_string();
                } else if let Some(rest) = line.strip_prefix("event:") {
                    event_type = rest.to_string();
                } else if let Some(rest) = line.strip_prefix("id: ") {
                    event_id = rest.parse::<i64>().ok();
                } else if let Some(rest) = line.strip_prefix("id:") {
                    event_id = rest.parse::<i64>().ok();
                }
            }
            // Persist the cursor so the retry wrapper sees the latest
            // `seq` we received before any subsequent disconnect.
            if let Some(seq) = event_id {
                *last_event_id_out.lock().unwrap() = Some(seq);
            }

            if data_parts.is_empty() {
                continue;
            }

            // Per SSE spec, multiple data: lines are joined with \n.
            let data = data_parts.join("\n");

            if event_type == "batch" || event_type.is_empty() {
                match serde_json::from_str::<Vec<HubEvent>>(&data) {
                    Ok(batch) => {
                        for evt in batch {
                            if tx.send(evt).is_err() {
                                return Ok(());
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "SSE JSON parse error");
                    }
                }
            } else {
                tracing::warn!(event_type, "ignoring unknown SSE event type");
            }
        }
    }

    Ok(())
}

/// Find the first complete SSE message in the byte buffer.
/// Returns `Some((message_len, delimiter_len))` or `None` if no complete
/// message yet. The caller should take the first `message_len` bytes and
/// then drain `message_len + delimiter_len` bytes from the buffer.
///
/// Per the SSE spec, `\n\n`, `\r\n\r\n`, and `\r\r` are all valid blank-line
/// delimiters; mixed conventions can appear in the same stream when an
/// intermediary rewrites line endings. We must pick the EARLIEST delimiter
/// in the buffer — not the first one a fixed-order scan happens to find —
/// or two interleaved events would be merged into one and parsed as a
/// single (malformed) message. Mirrors the TS SDK's `findSseDelimiter`.
fn split_sse_message_bytes(buf: &[u8]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for delimiter in &[
        b"\r\n\r\n".as_slice(),
        b"\n\n".as_slice(),
        b"\r\r".as_slice(),
    ] {
        if let Some(pos) = find_bytes(buf, delimiter) {
            match best {
                Some((best_pos, _)) if pos >= best_pos => {}
                _ => best = Some((pos, delimiter.len())),
            }
        }
    }
    best
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod sse_split_tests {
    use super::split_sse_message_bytes;

    #[test]
    fn picks_lf_lf_when_alone() {
        let buf = b"event: ping\ndata: 1\n\nrest";
        let (msg_len, delim_len) = split_sse_message_bytes(buf).expect("delim found");
        assert_eq!(&buf[..msg_len], b"event: ping\ndata: 1");
        assert_eq!(delim_len, 2);
    }

    #[test]
    fn picks_crlf_crlf_when_alone() {
        let buf = b"event: ping\r\ndata: 1\r\n\r\nrest";
        let (msg_len, delim_len) = split_sse_message_bytes(buf).expect("delim found");
        assert_eq!(&buf[..msg_len], b"event: ping\r\ndata: 1");
        assert_eq!(delim_len, 4);
    }

    #[test]
    fn picks_earliest_delimiter_when_mixed() {
        // Earlier event uses LF/LF; later one uses CRLF/CRLF. The split
        // must land on the EARLIER `\n\n` or the two events get merged
        // into one message and the second falls into the "data:" parse
        // path of the first. Pre-fix this returned the CRLF position.
        let buf = b"data: a\n\ndata: b\r\n\r\n";
        let (msg_len, delim_len) = split_sse_message_bytes(buf).expect("delim found");
        assert_eq!(&buf[..msg_len], b"data: a");
        assert_eq!(delim_len, 2);
    }

    #[test]
    fn picks_earliest_delimiter_crlf_first() {
        // Reverse case: CRLF-terminated event first, LF-terminated after.
        let buf = b"data: a\r\n\r\ndata: b\n\n";
        let (msg_len, delim_len) = split_sse_message_bytes(buf).expect("delim found");
        assert_eq!(&buf[..msg_len], b"data: a");
        assert_eq!(delim_len, 4);
    }

    #[test]
    fn returns_none_without_delimiter() {
        let buf = b"data: incomplete";
        assert!(split_sse_message_bytes(buf).is_none());
    }
}
