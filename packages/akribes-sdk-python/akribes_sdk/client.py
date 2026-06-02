from __future__ import annotations

import json
import logging
import threading
import uuid
from datetime import timedelta
from typing import TYPE_CHECKING, Any, Callable

if TYPE_CHECKING:
    from akribes_sdk._handles import ProjectHandle

import httpx

from akribes_sdk._otel import OtelArg, get_tracer, inject_into_headers
from akribes_sdk._retry import ExponentialBackoff, RetryPolicy, with_retry
from akribes_sdk._timing import to_seconds
from akribes_sdk.errors import (
    AlreadyExistsError,
    AkribesConnectionError,
    AkribesConversionError,
    AkribesHTTPError,
    AuthError,
    BadGateway502,
    GatewayTimeout504,
    NotFoundError,
    RateLimitError,
    ScriptSchemaChangedError,
    ServerError500,
    ServiceUnavailable503,
)
from akribes_sdk._parsers import parse_convert_result
from akribes_sdk.models import ConvertResult

logger = logging.getLogger("akribes_sdk")


# Heartbeat / SSE backoff curve (#1182): canonical SDK-wide curve is
# exponential with full jitter, base 1s, cap 30s. Modelled by ExponentialBackoff
# (from _retry.py). The thin wrapper below keeps `_backoff_s` importable for
# tests and external code that referenced it before the consolidation, and for
# the long-lived subscription heartbeat in resources/events.py.

_HEARTBEAT_BACKOFF = ExponentialBackoff(base=1.0, cap=30.0, jitter="full")


def _backoff_s(consecutive_failures: int) -> float:
    """Compute one backoff sleep duration (in seconds, with jitter).

    Thin compatibility shim over :class:`ExponentialBackoff`.  New code
    should use ``_HEARTBEAT_BACKOFF.delay(attempt)`` directly.
    """
    return _HEARTBEAT_BACKOFF.delay(consecutive_failures)


class AkribesClient:
    """Async client for the Akribes workflow server.

    Global resource namespaces::

        client.projects       # CRUD for projects
        client.tokens         # Scoped token management (mint/list/revoke)
        client.me             # GET /me, sandbox
        client.events         # Hub-level SSE streaming (global)
        client.executions     # By-ID ops: get, cancel, resume, await_result, documents
        client.clients        # By-ID ops: delete(client_id)

    Project-scoped namespaces — obtain via :meth:`project` or :meth:`get_project`::

        proj = client.project(2)
        proj.scripts / proj.drafts / proj.versions / proj.channels
        proj.executions / proj.bench / proj.mcp / proj.documents
        proj.clients / proj.events

    Authentication
    --------------
    Pass either kind of token via the ``token=`` argument:

    1. **Service token** — long-lived, env var, full Admin within its project
       scope. The secret is the part after ``:`` in
       ``AKRIBES_SERVICE_TOKEN_<NAME>=<scope>:<secret>`` from the server's env::

           import os
           client = AkribesClient(
               base_url="https://akribes.example.com",
               token=os.environ["AKRIBES_SERVICE_TOKEN"],
               on_behalf_of="customer@acme.com",  # optional, metrics only
           )

    2. **Scoped token** (``akribes_tk_...`` — legacy ``aura_tk_...`` still
       accepted) — minted at runtime via
       :meth:`Tokens.mint`. Short-lived, revokable. Hand these out to
       browsers / users::

           minted = await backend.tokens.mint(
               user_email="alice@acme.com",
               scopes={"projects": "*", "role": "admin"},
               expires_in=8 * 3600,  # 8h browser session
               label="web-session",
           )
           # ship minted.token to the browser

    The ``on_behalf_of`` parameter sets the ``X-Akribes-User`` header on
    outgoing requests for metrics attribution. **It is advisory only** — it
    does not grant any permission. Authorization is purely based on the
    bearer token's scope. (Servers continue to honor the legacy
    ``X-Aura-User`` form for backwards compat with pre-rebrand clients, but
    new code should not rely on that.)

    Can be used as an async context manager::

        async with AkribesClient(url, token=tok) as client:
            proj = client.project(2)
            scripts = await proj.scripts.list()

    Environment variables
    ---------------------
    ``AKRIBES_TRANSPORT``
        Event-stream transport for :meth:`Events.stream` / :class:`RunStream`.

        * unset (default) — try WebSocket (``GET /events/ws``), fall back to
          SSE (``GET /events``) on handshake failure.
        * ``ws``   — force WebSocket, propagate handshake errors instead of
          falling back. Useful in production where you want to fail loud if
          a proxy strips the WS upgrade.
        * ``sse``  — force Server-Sent Events. Useful when the WS path is
          known-broken (proxy, browser policy, debugging).
    """

    def __init__(
        self,
        base_url: str,
        *,
        name: str = "python-sdk",
        client_id: str | None = None,
        token: str | None = None,
        on_behalf_of: str | None = None,
        timeout: timedelta | float = 30.0,
        propagator: Callable[[dict[str, str]], None] | None = None,
        otel: OtelArg = None,
        ingest_poll_timeout_ms: int | None = None,
        retry: RetryPolicy | None = None,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.name = name
        self.client_id = client_id or str(uuid.uuid4())
        self.token = token
        self.on_behalf_of = on_behalf_of
        self.timeout = timeout
        self._tracer = get_tracer(otel)
        # If otel=True/Tracer is set AND the user didn't pass an explicit
        # propagator, default to OTel's own propagate.inject so trace context
        # is forwarded automatically. Manual propagator= still wins.
        if self._tracer is not None and propagator is None:
            self._propagator: Callable[[dict[str, str]], None] | None = inject_into_headers
        else:
            self._propagator = propagator
        self._http_client: httpx.AsyncClient | None = None
        self._sse_client: httpx.AsyncClient | None = None
        self._http_lock = threading.Lock()
        self._closing = False
        # Retry policy — defaults to sensible transient-error retries.
        self._retry: RetryPolicy = retry if retry is not None else RetryPolicy()

        # Resolve the ingest poll timeout once at construction time. Order:
        # explicit ctor arg > AKRIBES_SDK_INGEST_TIMEOUT_SECS env > default.
        from akribes_sdk.resources.documents import (
            DEFAULT_INGEST_POLL_TIMEOUT_MS,
            ingest_poll_timeout_ms_from_env,
        )
        if ingest_poll_timeout_ms is not None:
            self._ingest_poll_timeout_ms = ingest_poll_timeout_ms
        else:
            env_val = ingest_poll_timeout_ms_from_env()
            self._ingest_poll_timeout_ms = (
                env_val if env_val is not None else DEFAULT_INGEST_POLL_TIMEOUT_MS
            )

        # Contract state: shared across resource namespaces
        self._schema_cache: dict[str, list[tuple[str, str]]] = {}
        self._broken_scripts: set[str] = set()
        # Verified schema set: (project_id, script_name, schema_hash) tuples.
        # Entries are added by ProjectHandle._verify_schema on first successful
        # match so subsequent runs skip the /signature fetch.
        self._verified_schemas: set[tuple[int, str, str]] = set()

        # Wire global resource namespaces (imported here to avoid circular imports)
        from akribes_sdk.resources import (
            Events,
            ExecutionsByID,
            BenchRuns,
            ClientsByID,
            Me,
            Projects,
            Tokens,
        )
        from akribes_sdk.resources._base import _ApiClient

        global_api = _ApiClient(self)

        # Global resources — no project context required
        self.projects   = Projects(global_api)
        self.tokens     = Tokens(global_api)
        self.me         = Me(global_api)
        self.events     = Events(global_api)
        self.executions = ExecutionsByID(global_api)
        self.bench_runs = BenchRuns(global_api)
        self.clients    = ClientsByID(global_api)

    # ── Project handles ──────────────────────────────────────────────────

    def project(self, project_id: int) -> "ProjectHandle":
        """Return a :class:`ProjectHandle` for *project_id*.

        Synchronous and lazy — does not validate that the project exists.
        Use :meth:`get_project` for async validation::

            proj = client.project(2)
            await proj.scripts.list()
        """
        from akribes_sdk._handles import ProjectHandle
        return ProjectHandle(self, project_id)

    async def get_project(self, name_or_id: str | int) -> "ProjectHandle":
        """Resolve a project by name or numeric ID, validate it exists, and return a handle.

        Raises :class:`NotFoundError` if the project does not exist.

        By name (resolves via projects.list())::

            proj = await client.get_project("podesta-staging")

        By ID (validates exists via projects.get())::

            proj = await client.get_project(2)
        """
        if isinstance(name_or_id, int):
            p = await self.projects.get(name_or_id)
            if p is None:
                raise NotFoundError(f"project {name_or_id!r}")
            return self.project(name_or_id)
        # name resolution — list all and match. ``projects.list()`` returns an
        # ``AsyncPage`` (async-iterable, not awaitable), so iterate it directly.
        async for p in self.projects.list():
            if p.name == name_or_id:
                return self.project(p.id)
        raise NotFoundError(f"project {name_or_id!r}")

    # ── Auth ────────────────────────────────────────────────────────────

    def _auth_headers(self) -> dict[str, str]:
        headers: dict[str, str] = {}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        if self.on_behalf_of:
            # Servers also honor the legacy `X-Aura-User` for backwards compat,
            # but new clients emit the Akribes form.
            headers["X-Akribes-User"] = self.on_behalf_of
        return headers

    def _propagator_headers(self) -> dict[str, str]:
        """Invoke the configured ``propagator`` (if any) and return its keys.

        The propagator is called with a fresh empty dict on every outbound
        request; it should mutate the dict in-place to inject headers
        (typically W3C ``traceparent`` / ``tracestate``). This mirrors the TS
        SDK's pattern: zero OTel runtime deps in the SDK; the caller wires
        ``opentelemetry.propagate.inject`` (or any other carrier injector)
        through this hook.

        Example::

            from opentelemetry import propagate
            client = AkribesClient(
                base_url=..., token=...,
                propagator=lambda carrier: propagate.inject(carrier),
            )

        Errors raised by the propagator are swallowed — tracing must never
        break the request."""
        if self._propagator is None:
            return {}
        carrier: dict[str, str] = {}
        try:
            self._propagator(carrier)
        except Exception:
            logger.debug("propagator raised; skipping trace headers", exc_info=True)
            return {}
        return carrier

    # ── HTTP plumbing ───────────────────────────────────────────────────

    @property
    def _http(self) -> httpx.AsyncClient:
        with self._http_lock:
            if self._http_client is None or self._http_client.is_closed:
                self._http_client = httpx.AsyncClient(timeout=to_seconds(self.timeout))
            return self._http_client

    @property
    def _sse_http(self) -> httpx.AsyncClient:
        """Shared HTTP client for SSE streams (no timeout)."""
        with self._http_lock:
            if self._sse_client is None or self._sse_client.is_closed:
                self._sse_client = httpx.AsyncClient(timeout=None)
            return self._sse_client

    async def _request_inner(self, method: str, url: str, **kwargs: Any) -> httpx.Response:
        """Execute an HTTP request with auth headers, propagation, and retry.

        This is the real HTTP-level workhorse. :meth:`_request` wraps this
        with an optional OTel span so the span covers the full retry lifecycle.
        """
        # Build headers once; they are constant across retry attempts.
        base_headers: dict[str, str] = {
            **self._auth_headers(),
            **self._propagator_headers(),
            **kwargs.pop("headers", {}),
        }
        # Detect idempotency key from the pre-merged header dict so that
        # callers (e.g. executions._run_json) can keep passing it via
        # headers= without any change to their call sites.
        has_idempotency_key = "Idempotency-Key" in base_headers

        async def _send() -> httpx.Response:
            try:
                res = await self._http.request(method, url, headers=base_headers, **kwargs)
            except httpx.ConnectError as e:
                raise AkribesConnectionError(str(e)) from e
            except httpx.TimeoutException as e:
                raise AkribesConnectionError(f"Request timed out: {e}") from e
            self._raise_for_status(res)
            return res

        return await with_retry(
            _send,
            method=method,
            policy=self._retry,
            has_idempotency_key=has_idempotency_key,
        )

    async def _request(self, method: str, url: str, **kwargs: Any) -> httpx.Response:
        """Execute an HTTP request, optionally wrapped in an OTel span.

        The span covers the full retry lifecycle (one span per logical request,
        even if multiple HTTP attempts are made internally). When OTel is off
        (default), this is a zero-overhead pass-through to :meth:`_request_inner`.
        """
        if self._tracer is None:
            return await self._request_inner(method, url, **kwargs)

        span_name = f"akribes.http.{method.lower()}"
        with self._tracer.start_as_current_span(span_name) as span:
            span.set_attribute("http.method", method)
            span.set_attribute("http.url", url)
            try:
                res = await self._request_inner(method, url, **kwargs)
                span.set_attribute("http.status_code", res.status_code)
                return res
            except Exception as exc:
                # Record the exception on the span, then re-raise so the retry
                # middleware / caller still sees the typed error.
                span.record_exception(exc)
                from opentelemetry.trace import Status, StatusCode
                span.set_status(Status(StatusCode.ERROR, str(exc)[:200]))
                raise

    def _raise_for_status(self, res: httpx.Response) -> None:
        """Translate a non-2xx response into a typed SDK exception."""
        if res.status_code < 400:
            return

        status = res.status_code
        msg = f"HTTP {status}: {_extract_error_message(res)}"
        snippet = _body_snippet(res)
        retry_after = _parse_retry_after(res)

        if status in (401, 403):
            raise AuthError(msg, status=status, body_snippet=snippet)
        if status == 404:
            raise NotFoundError(_extract_error_message(res), body_snippet=snippet)
        if status == 409:
            body = _try_parse_json(res)
            if body is not None and body.get("error_type") == "suite_already_exists":
                raise AlreadyExistsError(
                    body.get("error", "already exists"),
                    status=409,
                    body_snippet=snippet,
                    existing_id=body.get("existing_suite_id"),
                )
        if status == 429:
            raise RateLimitError(msg, status=status, body_snippet=snippet, retry_after=retry_after)
        # #1296: dispatch to the specific 5xx subclass so callers can branch
        # on `isinstance(e, GatewayTimeout504)` etc. without enumerating
        # every status. `TransientError` is still the umbrella for any 5xx.
        if status == 500:
            raise ServerError500(msg, status=status, body_snippet=snippet, retry_after=retry_after)
        if status == 502:
            body = _try_parse_json(res)
            if body is not None and body.get("error_type") == "conversion_failed":
                raise AkribesConversionError(
                    message=body.get("error", "Document conversion failed"),
                    reason=body.get("reason", "unknown"),
                    attempts=body.get("attempts", 0),
                    body_snippet=snippet,
                )
            raise BadGateway502(msg, status=status, body_snippet=snippet, retry_after=retry_after)
        if status == 503:
            raise ServiceUnavailable503(msg, status=status, body_snippet=snippet, retry_after=retry_after)
        if status == 504:
            raise GatewayTimeout504(msg, status=status, body_snippet=snippet, retry_after=retry_after)
        # Any other 4xx (incl. 400) — carry the status + body so callers can
        # re-classify structured error envelopes (e.g. the bench client's
        # `case_type_mismatch` / `Judge contract mismatch` 400 bodies). Still
        # an AkribesError subclass, so existing `except AkribesError` keeps
        # working.
        raise AkribesHTTPError(msg, status=status, body_snippet=snippet)

    # ── Document Conversion ──────────────────────────────────────────────

    async def convert(self, filename: str, data: bytes) -> ConvertResult:
        """Convert a document to Markdown via Docling.

        Uses ``POST /convert`` (global). For project-scoped conversion where
        the resulting ``document_id`` must be accessible with a scoped token,
        use ``proj.documents.convert(...)`` from a :class:`~akribes_sdk.ProjectHandle`.
        """
        url = f"{self.base_url}/convert"
        res = await self._request(
            "POST",
            url,
            files={"file": (filename, data)},
        )
        return parse_convert_result(res.json())

    # ── State ───────────────────────────────────────────────────────────

    async def get_state(self) -> dict[str, Any]:
        res = await self._request("GET", f"{self.base_url}/state")
        return res.json()

    # ── Sandbox ─────────────────────────────────────────────────────────

    async def sandbox(self) -> "ProjectHandle":
        """Return a :class:`ProjectHandle` for the caller's per-user sandbox project.

        The sandbox is just a regular project — same scripts, executions, events
        surface. Use ``proj.run_source(...)`` to execute raw .akr source against it::

            async with AkribesClient(url, token=tok) as client:
                sandbox = await client.sandbox()
                result = await sandbox.run_source(SCRIPT, brief="hello")
                print(result.execution_id, result.result)
        """
        info = await self.me.sandbox()   # GET /me/sandbox → SandboxInfo with project_id
        return self.project(info.project_id)

    # ── Lifecycle ───────────────────────────────────────────────────────

    def validate_contract(self, script_name: str) -> None:
        """Pre-dispatch validation: check contract state before sending request.

        Server validates input types/shape authoritatively; we only flag scripts
        whose schema changed via subscription events so callers re-register.
        """
        if script_name in self._broken_scripts:
            raise ScriptSchemaChangedError(script_name)

    async def close(self) -> None:
        """Close the underlying HTTP connections."""
        self._closing = True
        with self._http_lock:
            if self._http_client and not self._http_client.is_closed:
                await self._http_client.aclose()
                self._http_client = None
            if self._sse_client and not self._sse_client.is_closed:
                await self._sse_client.aclose()
                self._sse_client = None

    async def __aenter__(self) -> "AkribesClient":
        return self

    async def __aexit__(self, *exc: Any) -> None:
        await self.close()


def _body_snippet(res: httpx.Response, limit: int = 500) -> str | None:
    """Return the first *limit* chars of the response body for logs/errors."""
    try:
        text = res.text
    except Exception:
        return None
    if not text:
        return None
    return text[:limit]


def _parse_retry_after(res: httpx.Response) -> float | None:
    """Parse the ``Retry-After`` header. Returns seconds as float, or None.

    Mirrors the TS SDK's ``parseRetryAfter`` — accepts only finite,
    non-negative numeric-seconds form. HTTP-date form, NaN, infinity,
    and negative values all return ``None`` so callers fall back to
    their default backoff instead of sleeping forever (``inf``) or
    raising on ``asyncio.sleep(NaN)``.
    """
    import math

    v = res.headers.get("Retry-After") or res.headers.get("retry-after")
    if not v:
        return None
    try:
        n = float(v)
    except ValueError:
        # HTTP-date form is not handled — callers can fall back to default.
        return None
    if not math.isfinite(n) or n < 0:
        return None
    return n


def _try_parse_json(res: httpx.Response) -> dict[str, Any] | None:
    """Try to parse the response body as a JSON object."""
    try:
        body = res.json()
        if isinstance(body, dict):
            return body
    except (json.JSONDecodeError, ValueError):
        pass
    return None


def _extract_error_message(res: httpx.Response) -> str:
    """Try to parse a JSON ``error`` field, fall back to raw text."""
    try:
        body = res.json()
        if isinstance(body, dict) and "error" in body:
            return body["error"]
    except (json.JSONDecodeError, ValueError):
        pass
    return res.text or f"HTTP {res.status_code}"
