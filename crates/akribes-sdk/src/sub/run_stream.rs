//! [`RunStream`] — a handle that wraps a script run together with its SSE
//! event stream, translates the wire events into [`WorkflowEvent`]s and
//! detects terminal events so callers can `await` a final output without
//! hand-rolling a 30-line receiver loop.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::Stream;
use tokio::sync::{mpsc, oneshot};

use crate::client::Inner;
use crate::error::{AkribesError, Result};
use crate::events::WorkflowEvent;
use crate::models::HubEvent;
use crate::sub::events::{EventSubscription, stream_sse_with_retry};
use crate::sub::executions::RunBuilder;
use crate::suspend::SuspendTrigger;

// ── Callback payloads ────────────────────────────────────────────────────────
//
// Owned snapshots passed to category callbacks. Decoupling these from
// `WorkflowEvent` variants lets us add fields to the variants without
// breaking callback signatures.

/// Payload passed to `on_task_end` callbacks.
#[derive(Debug, Clone)]
pub struct TaskEndPayload {
    pub task: String,
    pub output: serde_json::Value,
    pub duration: Duration,
    pub usage: Option<akribes_types::event::TokenUsage>,
    pub variant: crate::task_end::TaskEndVariant,
}

/// Payload passed to `on_suspend` callbacks (mirrors a `Checkpoint` event).
#[derive(Debug, Clone)]
pub struct SuspendPayload {
    pub name: String,
    pub token: String,
    pub prompt: String,
    pub schema: serde_json::Value,
    pub timeout_secs: Option<u64>,
    pub trigger: SuspendTrigger,
}

/// Payload passed to `on_error` callbacks.
#[derive(Debug, Clone)]
pub struct EngineErrorPayload {
    pub message: String,
    pub kind: akribes_types::error::ErrorKind,
}

// Boxed callback aliases. `Send` so callbacks can be registered from one
// task and the stream polled on another (common in async runtimes).
type OutputCb = Box<dyn Fn(&serde_json::Value) + Send + 'static>;
type TaskEndCb = Box<dyn Fn(&TaskEndPayload) + Send + 'static>;
type SuspendCb = Box<dyn Fn(&SuspendPayload) + Send + 'static>;
type ErrorCb = Box<dyn Fn(&EngineErrorPayload) + Send + 'static>;
type AnyCb = Box<dyn Fn(&WorkflowEvent) + Send + 'static>;

/// A live handle to a running workflow execution.
///
/// Obtain one from [`crate::sub::executions::ScopedExecutionsClient::run_stream`].
/// The stream yields [`WorkflowEvent`] items until the workflow reaches a
/// terminal event (`End` or `Error`), at which point it ends. Call
/// [`output`](Self::output) to consume the stream to completion and get the
/// final workflow output (or an error).
///
/// Dropping the `RunStream` cancels the underlying SSE subscription.
pub struct RunStream {
    pub execution_id: String,
    rx: mpsc::UnboundedReceiver<Result<WorkflowEvent>>,
    // Held for cancel-on-drop semantics; the background SSE listener AND
    // the filter/translator task are both aborted when this field is dropped.
    _subscription: EventSubscription,
    // Set to true once the stream has terminated (End or Error observed
    // or the channel closed).
    terminated: bool,
    // Populated when a `WorkflowEvent::End` is yielded, so `output()` can
    // resolve to the final output without re-reading the stream.
    final_output: Option<serde_json::Value>,
    // Populated when a `WorkflowEvent::Error` is yielded.
    final_error: Option<(String, akribes_types::error::ErrorKind)>,
    // ── Callback hooks ──────────────────────────────────────────────────
    //
    // Each list is invoked in registration order while the polling
    // thread holds &mut self. Callbacks must be `Send` so the
    // `RunStream` itself stays `Send`, but they execute *synchronously*
    // on the polling thread — long-running work belongs in a spawned
    // task. See the per-method docs for the contract.
    on_output_cbs: Vec<OutputCb>,
    on_task_end_cbs: Vec<TaskEndCb>,
    on_suspend_cbs: Vec<SuspendCb>,
    on_error_cbs: Vec<ErrorCb>,
    on_any_cbs: Vec<AnyCb>,
}

impl std::fmt::Debug for RunStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunStream")
            .field("execution_id", &self.execution_id)
            .field("terminated", &self.terminated)
            .finish()
    }
}

impl RunStream {
    /// Wire up a run stream from its pieces. Usually you don't call this
    /// directly — see [`ScopedExecutionsClient::run_stream`].
    ///
    /// [`ScopedExecutionsClient::run_stream`]:
    ///     crate::sub::executions::ScopedExecutionsClient::run_stream
    pub(crate) fn new(
        execution_id: String,
        rx: mpsc::UnboundedReceiver<Result<WorkflowEvent>>,
        subscription: EventSubscription,
    ) -> Self {
        Self {
            execution_id,
            rx,
            _subscription: subscription,
            terminated: false,
            final_output: None,
            final_error: None,
            on_output_cbs: Vec::new(),
            on_task_end_cbs: Vec::new(),
            on_suspend_cbs: Vec::new(),
            on_error_cbs: Vec::new(),
            on_any_cbs: Vec::new(),
        }
    }

    // ── Callback registration ───────────────────────────────────────────
    //
    // The callback API is convenience sugar layered over the iterator —
    // every event still flows through `next()` / `poll_next()`. Use it when
    // you want fire-and-forget sinks (logging, metrics, UI updates) without
    // hand-rolling a match-arm loop.
    //
    // **Threading.** Callbacks must be `Send + 'static` because `RunStream`
    // itself is `Send` and may be polled across thread boundaries by the
    // async runtime. They run synchronously on the polling thread between
    // the time an event arrives and the time it's yielded to the caller —
    // **don't block, sleep, or `.await` inside them**. If you need to do
    // I/O, spawn a task or push onto a channel.
    //
    // Callbacks fire in registration order; multiple callbacks per category
    // are supported. Calls are additive: there is no `clear` or `replace`
    // helper today (the iterator is the canonical surface; callbacks are
    // best registered once at stream construction).

    /// Register a callback for streaming agent output chunks.
    ///
    /// Fires once per [`WorkflowEvent::AgentChunk`]. The callback receives
    /// the chunk text wrapped in a `serde_json::Value::String` so the API
    /// stays uniform across SDKs (TS/Python use `Value`-like shapes too).
    pub fn on_output<F>(&mut self, cb: F)
    where
        F: Fn(&serde_json::Value) + Send + 'static,
    {
        self.on_output_cbs.push(Box::new(cb));
    }

    /// Register a callback for task completion events ([`WorkflowEvent::TaskEnd`]).
    pub fn on_task_end<F>(&mut self, cb: F)
    where
        F: Fn(&TaskEndPayload) + Send + 'static,
    {
        self.on_task_end_cbs.push(Box::new(cb));
    }

    /// Register a callback for workflow suspensions ([`WorkflowEvent::Checkpoint`]).
    ///
    /// `WorkflowEvent::ToolApproval` and `Breakpoint` are not routed here —
    /// register `on_any` if you need to observe every suspend-category event.
    pub fn on_suspend<F>(&mut self, cb: F)
    where
        F: Fn(&SuspendPayload) + Send + 'static,
    {
        self.on_suspend_cbs.push(Box::new(cb));
    }

    /// Register a callback for terminal error events ([`WorkflowEvent::Error`]).
    ///
    /// The stream still terminates on the next poll after an error; the
    /// callback fires once, before termination is observed.
    pub fn on_error<F>(&mut self, cb: F)
    where
        F: Fn(&EngineErrorPayload) + Send + 'static,
    {
        self.on_error_cbs.push(Box::new(cb));
    }

    /// Register a catch-all callback that sees every yielded event.
    ///
    /// Fires *after* category-specific callbacks for the same event, in
    /// registration order. Use for logging or generic event sinks; prefer
    /// the category callbacks for typed access.
    pub fn on_any<F>(&mut self, cb: F)
    where
        F: Fn(&WorkflowEvent) + Send + 'static,
    {
        self.on_any_cbs.push(Box::new(cb));
    }

    /// Dispatch the configured callbacks for one event. Called from both
    /// `next()` and `poll_next()` after a successful receive.
    fn dispatch_callbacks(&self, evt: &WorkflowEvent) {
        match evt {
            WorkflowEvent::AgentChunk { chunk, .. } => {
                if !self.on_output_cbs.is_empty() {
                    let v = serde_json::Value::String(chunk.clone());
                    for cb in &self.on_output_cbs {
                        cb(&v);
                    }
                }
            }
            WorkflowEvent::TaskEnd {
                task,
                output,
                duration,
                usage,
                variant,
            } => {
                if !self.on_task_end_cbs.is_empty() {
                    let payload = TaskEndPayload {
                        task: task.clone(),
                        output: output.clone(),
                        duration: *duration,
                        usage: usage.clone(),
                        variant: *variant,
                    };
                    for cb in &self.on_task_end_cbs {
                        cb(&payload);
                    }
                }
            }
            WorkflowEvent::Checkpoint {
                name,
                token,
                prompt,
                schema,
                timeout_secs,
                trigger,
            } => {
                if !self.on_suspend_cbs.is_empty() {
                    let payload = SuspendPayload {
                        name: name.clone(),
                        token: token.clone(),
                        prompt: prompt.clone(),
                        schema: schema.clone(),
                        timeout_secs: *timeout_secs,
                        trigger: trigger.clone(),
                    };
                    for cb in &self.on_suspend_cbs {
                        cb(&payload);
                    }
                }
            }
            WorkflowEvent::Error { message, kind, .. } => {
                if !self.on_error_cbs.is_empty() {
                    let payload = EngineErrorPayload {
                        message: message.clone(),
                        kind: kind.clone(),
                    };
                    for cb in &self.on_error_cbs {
                        cb(&payload);
                    }
                }
            }
            _ => {}
        }
        for cb in &self.on_any_cbs {
            cb(evt);
        }
    }

    /// Pull the next typed event. Returns `None` once the stream terminates.
    ///
    /// `Error` events are yielded as `Some(Ok(WorkflowEvent::Error{..}))` so
    /// the caller can observe them, and they also cause the stream to end
    /// immediately after.
    pub async fn next(&mut self) -> Option<Result<WorkflowEvent>> {
        if self.terminated {
            return None;
        }
        match self.rx.recv().await {
            Some(Ok(evt)) => {
                // Capture terminal state before yielding so `output()` can
                // resolve cheaply afterwards.
                match &evt {
                    WorkflowEvent::End { output, .. } => {
                        self.final_output = Some(output.clone());
                        self.terminated = true;
                    }
                    WorkflowEvent::Error { message, kind, .. } => {
                        self.final_error = Some((message.clone(), kind.clone()));
                        self.terminated = true;
                    }
                    _ => {}
                }
                self.dispatch_callbacks(&evt);
                Some(Ok(evt))
            }
            Some(Err(e)) => {
                self.terminated = true;
                Some(Err(e))
            }
            None => {
                self.terminated = true;
                None
            }
        }
    }

    /// Drain the stream and resolve to the final workflow output.
    ///
    /// Resolves to `Ok(output)` when a `WorkflowEvent::End` was observed
    /// (either already, or while draining). If the workflow ended with an
    /// `Error` event, resolves to an [`AkribesError::Script`] / `::Transient` /
    /// `::Fatal` depending on the `ErrorKind` — same classification as
    /// [`crate::sub::executions::ExecutionsClient::await_execution`]. If the
    /// stream closes without a terminal event, resolves to
    /// [`AkribesError::Other`].
    pub async fn output(mut self) -> Result<serde_json::Value> {
        while !self.terminated {
            if self.next().await.is_none() {
                break;
            }
        }

        if let Some(out) = self.final_output.take() {
            return Ok(out);
        }
        if let Some((message, kind)) = self.final_error.take() {
            return Err(classify_error(message, kind, self.execution_id.clone()));
        }
        Err(AkribesError::Other(format!(
            "run stream for execution {} ended without a terminal event",
            self.execution_id
        )))
    }

    /// Drain the stream to terminal and return a [`RunSummary`] aggregated
    /// from observed events (#1033 — mirrors TS `RunStream.summary()`).
    ///
    /// Resolves the same way as [`output`](Self::output): rejects when the
    /// workflow ended with an `Error` event, or when the stream closed
    /// without a terminal event. On success, the returned `RunSummary`
    /// rolls up workflow duration, per-task durations, task pass/fail
    /// counts, and per-model token totals collected from `TaskEnd` usage
    /// blocks.
    pub async fn summary(mut self) -> Result<RunSummary> {
        let mut total: Duration = Duration::ZERO;
        let mut per_task_ms: std::collections::HashMap<String, u128> =
            std::collections::HashMap::new();
        // `passed` / `failed` is determined by the last variant we see for
        // each task — `unable` overrides a prior success on retry (matches
        // TS).
        let mut tasks_status: std::collections::HashMap<String, bool> =
            std::collections::HashMap::new();
        let mut by_model_tokens: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        let mut usage_observed = false;
        let mut mock_observed = false;
        let mut final_output: Option<serde_json::Value> = None;

        while !self.terminated {
            match self.next().await {
                Some(Ok(evt)) => match &evt {
                    WorkflowEvent::End {
                        output, duration, ..
                    } => {
                        total = *duration;
                        final_output = Some(output.clone());
                    }
                    WorkflowEvent::TaskEnd {
                        task,
                        duration,
                        usage,
                        variant,
                        ..
                    } => {
                        *per_task_ms.entry(task.clone()).or_insert(0) += duration.as_millis();
                        // Latest variant wins.
                        let passed = matches!(variant, crate::task_end::TaskEndVariant::Success);
                        tasks_status.insert(task.clone(), passed);
                        if let Some(u) = usage {
                            usage_observed = true;
                            if u.provider == "mock" {
                                mock_observed = true;
                            }
                            let tokens = u.input_tokens.saturating_add(u.output_tokens);
                            let model = if u.model.is_empty() {
                                "unknown".to_string()
                            } else {
                                u.model.clone()
                            };
                            *by_model_tokens.entry(model).or_insert(0) += tokens;
                        }
                    }
                    _ => {}
                },
                Some(Err(e)) => return Err(e),
                None => break,
            }
        }

        if let Some((message, kind)) = self.final_error.take() {
            return Err(classify_error(message, kind, self.execution_id.clone()));
        }
        let Some(out) = final_output.or(self.final_output.take()) else {
            return Err(AkribesError::Other(format!(
                "run stream for execution {} ended without a terminal event",
                self.execution_id
            )));
        };

        let total_tasks = tasks_status.len();
        let passed = tasks_status.values().filter(|p| **p).count();
        let failed = total_tasks - passed;

        // Mirrors TS: when we have no real usage signal (no usage block, or
        // the engine reported the `mock` provider) we report `cost = None`.
        // When usage is real, `by_model` carries the total (input + output)
        // token count per model; `total_usd` stays 0 until a pricing table
        // is wired in.
        let cost = if !usage_observed || mock_observed {
            None
        } else {
            Some(RunSummaryCost {
                total_usd: 0.0,
                by_model: by_model_tokens,
            })
        };

        Ok(RunSummary {
            execution_id: self.execution_id.clone(),
            output: out,
            cost,
            duration: RunSummaryDuration {
                total_ms: total.as_millis(),
                per_task_ms,
            },
            tasks: RunSummaryTasks {
                passed,
                failed,
                total: total_tasks,
            },
        })
    }
}

/// Aggregated summary of a run, returned by [`RunStream::summary`] (#1033).
/// Mirrors TS `RunSummary` from `runStream.ts`.
#[derive(Debug, Clone)]
pub struct RunSummary {
    pub execution_id: String,
    pub output: serde_json::Value,
    /// `None` when the stream observed no usage (`TaskEnd.usage` was
    /// absent or the engine reported the `mock` provider). When `Some`,
    /// the SDK currently leaves `total_usd` at 0 — `by_model` carries the
    /// raw (input + output) token total per model so callers can multiply
    /// by their own pricing table.
    pub cost: Option<RunSummaryCost>,
    pub duration: RunSummaryDuration,
    pub tasks: RunSummaryTasks,
}

#[derive(Debug, Clone)]
pub struct RunSummaryCost {
    pub total_usd: f64,
    pub by_model: std::collections::HashMap<String, u64>,
}

#[derive(Debug, Clone)]
pub struct RunSummaryDuration {
    pub total_ms: u128,
    pub per_task_ms: std::collections::HashMap<String, u128>,
}

#[derive(Debug, Clone)]
pub struct RunSummaryTasks {
    pub passed: usize,
    pub failed: usize,
    pub total: usize,
}

impl Stream for RunStream {
    type Item = Result<WorkflowEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.terminated {
            return Poll::Ready(None);
        }
        match this.rx.poll_recv(cx) {
            Poll::Ready(Some(Ok(evt))) => {
                match &evt {
                    WorkflowEvent::End { output, .. } => {
                        this.final_output = Some(output.clone());
                        this.terminated = true;
                    }
                    WorkflowEvent::Error { message, kind, .. } => {
                        this.final_error = Some((message.clone(), kind.clone()));
                        this.terminated = true;
                    }
                    _ => {}
                }
                this.dispatch_callbacks(&evt);
                Poll::Ready(Some(Ok(evt)))
            }
            Poll::Ready(Some(Err(e))) => {
                this.terminated = true;
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(None) => {
                this.terminated = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

fn classify_error(
    message: String,
    kind: akribes_types::error::ErrorKind,
    execution_id: String,
) -> AkribesError {
    use akribes_types::error::ErrorKind;
    let eid = Some(execution_id);
    match kind {
        ErrorKind::RateLimit
        | ErrorKind::ServerError500
        | ErrorKind::BadGateway502
        | ErrorKind::ServiceUnavailable503
        | ErrorKind::GatewayTimeout504
        | ErrorKind::NetworkError => {
            // #1296: surface the status when the kind maps cleanly so
            // callers can prefer the per-status base backoff over the
            // sdk-wide default.
            let status = match kind {
                ErrorKind::RateLimit => Some(429u16),
                ErrorKind::ServerError500 => Some(500u16),
                ErrorKind::BadGateway502 => Some(502u16),
                ErrorKind::ServiceUnavailable503 => Some(503u16),
                ErrorKind::GatewayTimeout504 => Some(504u16),
                _ => None,
            };
            AkribesError::Transient {
                message,
                execution_id: eid,
                retry_after: None,
                status,
            }
        }
        ErrorKind::AuthError | ErrorKind::TokenLimit => AkribesError::Fatal {
            message,
            execution_id: eid,
        },
        _ => AkribesError::Script {
            message,
            execution_id: eid,
        },
    }
}

// ── Assembling a RunStream ──────────────────────────────────────────────────

/// Start an SSE subscription filtered to the given script, then kick off the
/// run and return a [`RunStream`] wired to translate `HubEvent::Execution`
/// payloads into [`WorkflowEvent`]s.
///
/// Subscribes to SSE *first*, waits for the subscription to be live on the
/// server (ready signal), then POSTs `/run`. This avoids the race where
/// opening events broadcast by the hub are lost if the GET /events handshake
/// hasn't completed when the POST response fires.
///
/// Called by [`ScopedExecutionsClient::run_stream`].
pub(crate) async fn start_run_stream(
    inner: Arc<Inner>,
    project_id: i64,
    builder: RunBuilder,
) -> Result<RunStream> {
    let script_name = builder.script_name().to_string();

    // ── 1. Spawn the SSE listener with a ready-signal oneshot. Wait for the
    //        server to confirm the subscription before POSTing `/run`.
    let (hub_tx, mut hub_rx) = mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = oneshot::channel::<Result<()>>();
    let http = inner.http.clone();
    let token = inner.token.clone();
    let base_url = inner.base_url.clone();
    let script_for_sse = script_name.clone();
    let sse_handle = tokio::spawn(async move {
        let _ = stream_sse_with_retry(
            http,
            token,
            base_url,
            project_id,
            Some(script_for_sse),
            hub_tx,
            Some(ready_tx),
        )
        .await;
    });

    // Wait for "subscribed" signal. If the server rejects the subscription
    // or the task dies before firing, surface the error to the caller.
    match ready_rx.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            sse_handle.abort();
            return Err(e);
        }
        Err(_) => {
            sse_handle.abort();
            return Err(AkribesError::Other(
                "SSE listener died before subscription was confirmed".into(),
            ));
        }
    }

    // ── 2. Kick off the run now that we're guaranteed to receive events.
    let run = match builder.execute().await {
        Ok(r) => r,
        Err(e) => {
            sse_handle.abort();
            return Err(e);
        }
    };
    let execution_id = run.execution_id;

    // ── 3. Filter-and-translate task: pull `HubEvent::Execution` entries
    //        whose script_name AND execution_id match this run, convert
    //        them to WorkflowEvent, and forward. Stop as soon as a
    //        terminal event is seen.
    //
    //  Filtering by script name alone would conflate concurrent runs of
    //  the same script started by another caller — their `WorkflowEnd`
    //  would resolve this handle's `output()` with the wrong value.
    //  Matches the TS SDK's `RunStream` execution-id filter (see
    //  `packages/akribes-sdk-ts/src/runStream.ts::routeRaw`).
    //  Pre-#1042 servers that don't stamp `execution_id` on the
    //  broadcast envelope still flow through (back-compat: `None`
    //  matches anything) — but every server in production today does.
    let (out_tx, out_rx) = mpsc::unbounded_channel::<Result<WorkflowEvent>>();
    let script_for_filter = script_name.clone();
    let exec_id_for_filter = execution_id.clone();
    let filter_handle = tokio::spawn(async move {
        while let Some(hub) = hub_rx.recv().await {
            if let HubEvent::Execution {
                script_name: evt_script,
                execution_id: evt_exec_id,
                event,
                ..
            } = hub
            {
                if evt_script != script_for_filter {
                    continue;
                }
                if let Some(eid) = evt_exec_id {
                    if eid != exec_id_for_filter {
                        continue;
                    }
                }
                let wf: WorkflowEvent = event.into();
                let is_terminal = wf.is_terminal();
                if out_tx.send(Ok(wf)).is_err() {
                    break;
                }
                if is_terminal {
                    break;
                }
            }
        }
    });

    // Drop-guard: both the SSE listener AND the filter task abort when the
    // RunStream is dropped. Previously only the filter was tracked, which
    // leaked the SSE task whenever a RunStream was dropped pre-terminal.
    let subscription = EventSubscription::from_handles(vec![sse_handle, filter_handle]);
    Ok(RunStream::new(execution_id, out_rx, subscription))
}
