"""Bench (akribes-native eval substrate) client.

Two surfaces, mirroring the Rust SDK's `BenchClient` / `BenchRunsClient`
(`crates/akribes-sdk/src/sub/bench.rs`) and the TS SDK's single `BenchClient`
(`packages/akribes-sdk-ts/src/sub/bench.ts`):

- :class:`Bench` — project-scoped config + case + run-trigger operations rooted
  at ``/projects/{id}/scripts/{name}/bench/...``. Mounted on
  ``ProjectHandle.bench``.
- :class:`BenchRuns` — run-id-keyed (and case-id-keyed) operations under
  ``/bench-runs/{id}/...``, ``/cases/{id}``, ``/benches/{id}``,
  ``/executions/{id}/...``, ``/mcp-sessions/{id}/cost``. These are global —
  the server resolves the owning project from the row — so they live on
  ``AkribesClient.bench_runs``.

Server contract source of truth: ``crates/akribes-server/src/handlers/bench.rs``
(+ ``models.rs`` for request/response shapes, ``lib.rs`` for routes).

Typed error classification (the global ``_raise_for_status`` only raises a
generic :class:`AkribesHTTPError` for 400; we re-classify the bench-specific
400 envelopes here):

- 400 ``{"error": "case_type_mismatch", "field_errors": [...]}`` →
  :class:`CaseTypeMismatchError`.
- 400 ``"Judge contract mismatch: …"`` → :class:`JudgeContractError`.
"""

from __future__ import annotations

import json
from typing import Any, AsyncGenerator

from httpx_sse import aconnect_sse

from akribes_sdk._pagination import AsyncPage
from akribes_sdk.errors import (
    AkribesHTTPError,
    CaseFieldError,
    CaseTypeMismatchError,
    JudgeContractError,
    NotFoundError,
)
from akribes_sdk._parsers import (
    parse_bench,
    parse_bench_case,
    parse_bench_result,
    parse_bench_run,
    parse_bench_run_tag_session,
    parse_compare_report,
    parse_drift_report,
    parse_project_bench_summary,
)
from akribes_sdk.models import (
    Bench as BenchConfig,
    BenchCase,
    BenchResult,
    BenchRun,
    BenchRunTagSessionResponse,
    CompareReport,
    DriftReport,
    ProjectBenchSummary,
)
from akribes_sdk.resources._base import Resource, ProjectResource
from akribes_sdk.resources._sentinel import _MISSING


# ── 400 error classification ─────────────────────────────────────────────────


def _classify_bench_400(err: AkribesHTTPError) -> Exception:
    """Re-classify a generic 400 :class:`AkribesHTTPError` into the bench-typed
    taxonomy when the body matches. Returns the original error otherwise.

    Mirrors the TS SDK's ``classifyBenchError`` decode rules."""
    if err.status != 400 or not err.body_snippet:
        return err
    try:
        body = json.loads(err.body_snippet)
    except (ValueError, TypeError):
        return err
    if not isinstance(body, dict):
        return err
    server_msg = body.get("error")
    if server_msg == "case_type_mismatch":
        raw = body.get("field_errors", [])
        field_errors = [
            CaseFieldError(path=fe["path"], message=fe["message"])
            for fe in raw
            if isinstance(fe, dict) and "path" in fe and "message" in fe
        ]
        return CaseTypeMismatchError(
            body.get("message", "case_type_mismatch"),
            field_errors=field_errors,
            body_snippet=err.body_snippet,
        )
    if isinstance(server_msg, str) and server_msg.startswith("Judge contract mismatch"):
        return JudgeContractError(
            server_msg,
            breaks=_parse_judge_breaks(server_msg),
            body_snippet=err.body_snippet,
        )
    return err


def _parse_judge_breaks(message: str) -> list[str]:
    """Extract the trailing ``field(s) incompatible: …`` list from a Judge
    contract mismatch message. Returns ``[]`` when the format doesn't match."""
    marker = "field(s) incompatible: "
    idx = message.find(marker)
    if idx == -1:
        return []
    tail = message[idx + len(marker):].strip()
    if not tail:
        return []
    return [s.strip() for s in tail.split("; ") if s.strip()]


# ── Project-scoped surface ───────────────────────────────────────────────────


class Bench(ProjectResource):
    """Project + script scoped bench operations. Mounted on ``ProjectHandle.bench``.

    Each method takes the ``script_name`` as its first argument, mirroring the
    Rust ``BenchClient`` (``client.project(id).bench()``)."""

    # ── Config CRUD ──────────────────────────────────────────────────────────

    async def get(self, script_name: str, *, default=_MISSING) -> BenchConfig:
        """``GET /projects/{id}/scripts/{name}/bench``.

        Raises :class:`NotFoundError` when no bench is configured. Pass
        ``default=None`` (or any value) to suppress and return that instead."""
        try:
            res = await self._request("GET", self._bench_url(script_name))
        except NotFoundError:
            if default is _MISSING:
                raise
            return default  # type: ignore[return-value]
        return parse_bench(res.json())

    async def upsert(
        self,
        script_name: str,
        *,
        judge_script_id: int | None = None,
        judge_channel: str | None = None,
        config: dict[str, Any] | None = None,
    ) -> BenchConfig:
        """``POST /projects/{id}/scripts/{name}/bench`` — create or update.

        The server upserts on ``(script_id)``, so this is idempotent. An
        omitted ``judge_channel`` defaults to ``"draft"`` server-side."""
        body: dict[str, Any] = {}
        if judge_script_id is not None:
            body["judge_script_id"] = judge_script_id
        if judge_channel is not None:
            body["judge_channel"] = judge_channel
        if config is not None:
            body["config"] = config
        res = await self._request("POST", self._bench_url(script_name), json=body)
        return parse_bench(res.json())

    async def delete(self, script_name: str) -> None:
        """``DELETE /projects/{id}/scripts/{name}/bench``. Idempotent — the
        server emits ``{"deleted": true}`` whether or not a row existed."""
        await self._request("DELETE", self._bench_url(script_name))

    async def list_project_summaries(self) -> list[ProjectBenchSummary]:
        """``GET /projects/{id}/benches`` — one summary row per script that has
        a bench configured, joined with its latest run."""
        res = await self._request("GET", self._project_url("benches"))
        return [parse_project_bench_summary(s) for s in res.json()]

    # ── Signature + contract preview ─────────────────────────────────────────

    async def get_signature(
        self, script_name: str, *, channel: str | None = None
    ) -> dict[str, Any]:
        """``GET /projects/{id}/scripts/{name}/signature`` — the parsed script
        signature (inputs + outputs + named type defs + SDK codegen fields).

        Returned as a raw dict because the server emits an ad-hoc tagged shape
        without a stable typed mirror (matches the Rust SDK's
        ``serde_json::Value`` return)."""
        params = {"channel": channel} if channel is not None else None
        res = await self._request(
            "GET", self._project_url("scripts", script_name, "signature"), params=params
        )
        return res.json()

    async def contract_preview(
        self,
        script_name: str,
        judge_script_id: int,
        *,
        channel: str | None = None,
    ) -> dict[str, Any]:
        """``GET /projects/{id}/scripts/{name}/bench/contract-preview`` —
        workflow + judge signatures plus the structured ``breaks`` list.

        Returned as a raw dict (the wire shape embeds the unstable, JSON-only
        signature representation)."""
        params: dict[str, Any] = {"judge": judge_script_id}
        if channel is not None:
            params["channel"] = channel
        res = await self._request(
            "GET", self._bench_url(script_name, "contract-preview"), params=params
        )
        return res.json()

    # ── Cases ────────────────────────────────────────────────────────────────

    async def list_cases(self, script_name: str) -> list[BenchCase]:
        """``GET /projects/{id}/scripts/{name}/bench/cases``. 404 (no bench
        configured) → empty list, matching the Rust/TS SDKs."""
        try:
            res = await self._request("GET", self._bench_url(script_name, "cases"))
        except NotFoundError:
            return []
        return [parse_bench_case(c) for c in res.json()]

    async def create_case(
        self,
        script_name: str,
        *,
        inputs: dict[str, Any],
        expected_output: Any | None = None,
        ground_truth: Any | None = None,
        name: str | None = None,
    ) -> BenchCase:
        """``POST /projects/{id}/scripts/{name}/bench/cases`` — form-builder
        create. Raises :class:`CaseTypeMismatchError` on a 400 contract
        violation so form layers can surface per-field errors."""
        body: dict[str, Any] = {"inputs": inputs}
        if expected_output is not None:
            body["expected_output"] = expected_output
        if ground_truth is not None:
            body["ground_truth"] = ground_truth
        if name is not None:
            body["name"] = name
        try:
            res = await self._request(
                "POST", self._bench_url(script_name, "cases"), json=body
            )
        except AkribesHTTPError as e:
            raise _classify_bench_400(e) from e
        return parse_bench_case(res.json())

    async def case_contract_drift(self, script_name: str) -> DriftReport:
        """``GET /projects/{id}/scripts/{name}/bench/cases/contract-drift``.

        404 (script never published) → empty drift report."""
        try:
            res = await self._request(
                "GET", self._bench_url(script_name, "cases", "contract-drift")
            )
        except NotFoundError:
            return DriftReport()
        return parse_drift_report(res.json())

    # ── Runs (project-scoped surface) ────────────────────────────────────────

    def list_runs(self, script_name: str) -> AsyncPage[BenchRun]:
        """``GET /projects/{id}/scripts/{name}/bench/runs`` — paginated via
        ``limit`` / ``offset`` (server clamps limit to 1..=500, default 50),
        newest first. Returns an :class:`AsyncPage`."""

        async def fetch(offset: int, limit: int) -> tuple[list[BenchRun], bool]:
            try:
                res = await self._request(
                    "GET",
                    self._bench_url(script_name, "runs"),
                    params={"limit": str(limit), "offset": str(offset)},
                )
            except NotFoundError:
                return [], False
            items = [parse_bench_run(r) for r in res.json()]
            return items, len(items) == limit

        return AsyncPage(fetch)

    async def trigger_run(
        self,
        script_name: str,
        *,
        channel: str,
        case_ids: list[str] | None = None,
        notes: str | None = None,
    ) -> BenchRun:
        """``POST /projects/{id}/scripts/{name}/bench/runs`` — trigger a run.

        ``case_ids`` constrains the fan-out to a subset (partial run); omit or
        pass an empty list to run every case. Raises :class:`JudgeContractError`
        on the contract pre-flight 400."""
        body: dict[str, Any] = {"channel": channel}
        if case_ids is not None:
            body["case_ids"] = case_ids
        if notes is not None:
            body["notes"] = notes
        try:
            res = await self._request(
                "POST", self._bench_url(script_name, "runs"), json=body
            )
        except AkribesHTTPError as e:
            raise _classify_bench_400(e) from e
        return parse_bench_run(res.json())

    # ── URL helpers ──────────────────────────────────────────────────────────

    def _bench_url(self, script_name: str, *segments: str) -> str:
        return self._project_url("scripts", script_name, "bench", *segments)


# ── Run-id keyed (global) surface ────────────────────────────────────────────


class BenchRuns(Resource):
    """Run-id and case-id keyed bench operations. Mounted on
    ``AkribesClient.bench_runs``.

    These endpoints live under ``/bench-runs/{id}``, ``/cases/{id}``,
    ``/benches/{id}``, ``/executions/{id}/...`` and don't need a project scope —
    the server resolves the owning project from the row."""

    # ── Run lifecycle ────────────────────────────────────────────────────────

    async def get(self, run_id: int, *, default=_MISSING) -> BenchRun:
        """``GET /bench-runs/{id}``. Raises :class:`NotFoundError`; pass
        ``default=None`` to suppress."""
        try:
            res = await self._request("GET", f"{self._base_url}/bench-runs/{run_id}")
        except NotFoundError:
            if default is _MISSING:
                raise
            return default  # type: ignore[return-value]
        return parse_bench_run(res.json())

    async def delete(self, run_id: int) -> None:
        """``DELETE /bench-runs/{id}`` — cancels first (best-effort) then drops
        the run row + every result (FK CASCADE). Server emits 204 No Content."""
        await self._request("DELETE", f"{self._base_url}/bench-runs/{run_id}")

    async def list_results(self, run_id: int) -> list[BenchResult]:
        """``GET /bench-runs/{id}/results``. 404 → empty list. Carries the
        typed ``workflow_output`` + ``error`` per case."""
        try:
            res = await self._request(
                "GET", f"{self._base_url}/bench-runs/{run_id}/results"
            )
        except NotFoundError:
            return []
        return [parse_bench_result(r) for r in res.json()]

    async def cancel(self, run_id: int) -> BenchRun:
        """``POST /bench-runs/{id}/cancel`` — flips the cancel token; in-flight
        cases finish naturally. Returns the run row as it stands."""
        res = await self._request(
            "POST", f"{self._base_url}/bench-runs/{run_id}/cancel", json={}
        )
        return parse_bench_run(res.json())

    async def compare(self, run_a: int, run_b: int, *, default=_MISSING) -> CompareReport:
        """``GET /bench-runs/{a}/compare/{b}`` — diff two runs of the same bench.

        Raises :class:`NotFoundError` (the server collapses the cross-project
        case to 404). Pass ``default=None`` to suppress."""
        try:
            res = await self._request(
                "GET", f"{self._base_url}/bench-runs/{run_a}/compare/{run_b}"
            )
        except NotFoundError:
            if default is _MISSING:
                raise
            return default  # type: ignore[return-value]
        return parse_compare_report(res.json())

    async def tag_session(
        self, run_id: int, mcp_session_id: str
    ) -> BenchRunTagSessionResponse:
        """``PATCH /bench-runs/{id}/tag-session`` — attribute the run to an MCP
        session id so the coordinator's finalize step writes the cost into
        ``mcp_session_cost``."""
        res = await self._request(
            "PATCH",
            f"{self._base_url}/bench-runs/{run_id}/tag-session",
            json={"mcp_session_id": mcp_session_id},
        )
        return parse_bench_run_tag_session(res.json())

    # ── Run events: SSE stream ───────────────────────────────────────────────

    async def stream_events(
        self, run_id: int
    ) -> AsyncGenerator[BenchResult, None]:
        """Subscribe to a bench run's live result stream over SSE
        (``GET /bench-runs/{id}/events``).

        Yields a :class:`BenchResult` per server ``result`` event. The server
        also emits synthetic ``lagged`` (``{"dropped": N}``) and ``terminal``
        (``{"status": "…"}``) events; both end/continue the stream rather than
        yielding a result — ``lagged`` is skipped and ``terminal`` closes the
        generator. Mirrors the TS SDK's ``subscribeRunEvents``.

        Example::

            async for result in client.bench_runs.stream_events(run_id):
                print(result.case_id, result.headline_score)
        """
        token = getattr(self._api, "token", None)
        headers: dict[str, str] = {}
        if token:
            headers["Authorization"] = f"Bearer {token}"
        async with aconnect_sse(
            self._api._client._sse_http,
            "GET",
            f"{self._base_url}/bench-runs/{run_id}/events",
            headers=headers,
        ) as sse:
            async for event in sse.aiter_sse():
                if event.event == "result":
                    try:
                        payload = json.loads(event.data)
                    except (json.JSONDecodeError, ValueError):
                        continue
                    yield parse_bench_result(payload)
                elif event.event == "terminal":
                    # Run reached a terminal state; the broadcaster is done.
                    return
                # `lagged` (and any unknown event) — skip and keep listening.

    # ── Case-id keyed operations ─────────────────────────────────────────────

    async def get_case(self, case_id: str, *, default=_MISSING) -> dict[str, Any]:
        """``GET /executions/{case_id}`` — fetch a single case's raw execution
        row (cases are ``executions`` rows with ``kind='case'``).

        Returned as a raw dict for parity with the Rust SDK / MCP tool, which
        don't trust legacy promoted rows to always type-check as a case.
        Raises :class:`NotFoundError`; pass ``default`` to suppress."""
        try:
            res = await self._request(
                "GET", f"{self._base_url}/executions/{case_id}"
            )
        except NotFoundError:
            if default is _MISSING:
                raise
            return default  # type: ignore[return-value]
        return res.json()

    async def patch_case(
        self,
        case_id: str,
        *,
        inputs: dict[str, Any] | None = None,
        expected_output: Any | None = None,
        ground_truth: Any | None = None,
        name: str | None = None,
    ) -> BenchCase:
        """``PATCH /cases/{id}`` — sparse update. Only the supplied fields are
        sent (and changed). Raises :class:`CaseTypeMismatchError` on a 400
        contract violation."""
        body: dict[str, Any] = {}
        if inputs is not None:
            body["inputs"] = inputs
        if expected_output is not None:
            body["expected_output"] = expected_output
        if ground_truth is not None:
            body["ground_truth"] = ground_truth
        if name is not None:
            body["name"] = name
        try:
            res = await self._request(
                "PATCH", f"{self._base_url}/cases/{case_id}", json=body
            )
        except AkribesHTTPError as e:
            raise _classify_bench_400(e) from e
        return parse_bench_case(res.json())

    async def delete_case(self, case_id: str) -> None:
        """``DELETE /cases/{id}``. Server emits ``{"deleted": true}``; discarded."""
        await self._request("DELETE", f"{self._base_url}/cases/{case_id}")

    async def promote_execution(
        self,
        execution_id: str,
        *,
        name: str | None = None,
        edits: dict[str, Any] | None = None,
    ) -> BenchCase:
        """``POST /executions/{exec_id}/promote-to-case`` — promote a completed
        execution into a bench case.

        ``edits`` is an optional overlay (``{inputs?, expected_output?,
        ground_truth?}``) applied on top of the source execution's values; an
        absent field is inherited as-is. Raises :class:`CaseTypeMismatchError`
        on a 400 contract violation."""
        body: dict[str, Any] = {}
        if name is not None:
            body["name"] = name
        if edits is not None:
            body["edits"] = edits
        try:
            res = await self._request(
                "POST",
                f"{self._base_url}/executions/{execution_id}/promote-to-case",
                json=body,
            )
        except AkribesHTTPError as e:
            raise _classify_bench_400(e) from e
        return parse_bench_case(res.json())

    # ── Bench-by-id + MCP session cost ───────────────────────────────────────

    async def get_bench(self, bench_id: int, *, default=_MISSING) -> dict[str, Any]:
        """``GET /benches/{id}`` — fast bench-by-id lookup. Returns the bench row
        joined with the owning ``project_id`` + ``script_name`` so callers can
        chain into list_cases / list_runs without a project walk.

        Returned as a raw dict (the wire shape adds ``project_id`` +
        ``script_name`` beyond the typed :class:`~akribes_sdk.models.Bench`).
        Raises :class:`NotFoundError`; pass ``default`` to suppress."""
        try:
            res = await self._request("GET", f"{self._base_url}/benches/{bench_id}")
        except NotFoundError:
            if default is _MISSING:
                raise
            return default  # type: ignore[return-value]
        return res.json()

    async def mcp_session_cost(self, session_id: str) -> dict[str, Any]:
        """``GET /mcp-sessions/{id}/cost`` — aggregated cost for an MCP session.

        Returns ``{session_id, total_cost_usd, breakdown}``. Service-token only
        (the server rejects scoped tokens with 403)."""
        res = await self._request(
            "GET", f"{self._base_url}/mcp-sessions/{session_id}/cost"
        )
        return res.json()
