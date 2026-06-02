"""Internal wire-format parsers. Pydantic stays here; public models are dataclasses.

PRIVATE — public users MUST NOT import from this module. The API surface is
``akribes_sdk.models`` (dataclasses) and ``akribes_sdk`` (re-exports).

Each ``parse_<model>`` function accepts a raw JSON dict from the wire and
returns the corresponding frozen dataclass from ``akribes_sdk.models``.

Design notes
------------
- Models with datetime fields or nested Pydantic objects (Project, Script,
  TokenInfo) route through a ``_Wire`` Pydantic model so we get RFC 3339
  parsing and coercion for free.
- Simple flat models with no validators (Draft, ConvertResult, SandboxInfo,
  S3PresignedRef, S3CredentialsRef, etc.) are constructed directly from the
  wire dict — leaner and still safe because the field set is fully controlled
  by the dataclass definition.
"""
from __future__ import annotations

from typing import Any

from pydantic import BaseModel, ConfigDict, field_validator

from akribes_sdk import models
from akribes_sdk.models import _parse_dt, TokenScopes


# ── Wire models (Pydantic, private) ─────────────────────────────────────────


class _ProjectWire(BaseModel):
    model_config = ConfigDict(extra="ignore", frozen=True, populate_by_name=True)

    id: int
    name: str
    created_at: Any

    _v_created = field_validator("created_at", mode="before")(_parse_dt)


class _ScriptWire(BaseModel):
    model_config = ConfigDict(extra="ignore", frozen=True, populate_by_name=True)

    id: int
    project_id: int
    name: str
    created_at: Any

    _v_created = field_validator("created_at", mode="before")(_parse_dt)


class _TokenInfoWire(BaseModel):
    model_config = ConfigDict(extra="ignore", frozen=True, populate_by_name=True)

    id: str
    label: str
    user_email: str | None
    scopes: Any
    minted_by: str
    expires_at: Any
    revoked: bool
    created_at: Any
    last_used_at: Any = None

    _v_created = field_validator("created_at", mode="before")(_parse_dt)
    _v_expires = field_validator("expires_at", mode="before")(_parse_dt)
    _v_last_used = field_validator("last_used_at", mode="before")(_parse_dt)

    @field_validator("scopes", mode="before")
    @classmethod
    def _coerce_scopes(cls, v: Any) -> Any:
        if isinstance(v, dict):
            return TokenScopes.from_dict(v)
        return v


class _ScriptChannelWire(BaseModel):
    model_config = ConfigDict(extra="ignore", frozen=True, populate_by_name=True)

    id: int
    script_id: int
    name: str
    version_id: int | None
    updated_at: Any = None

    _v_updated = field_validator("updated_at", mode="before")(_parse_dt)


class _LatestVersionWire(BaseModel):
    model_config = ConfigDict(extra="ignore", frozen=True, populate_by_name=True)

    id: int
    script_id: int
    source: str
    label: str | None = None
    published_by: str | None = None
    created_at: Any
    inputs: list[Any] = []

    _v_created = field_validator("created_at", mode="before")(_parse_dt)


class _TokenMintedWire(BaseModel):
    model_config = ConfigDict(extra="ignore", frozen=True, populate_by_name=True)

    token: str
    token_id: str
    expires_at: Any

    _v_expires = field_validator("expires_at", mode="before")(_parse_dt)


# ── Public parse functions ───────────────────────────────────────────────────


def parse_project(data: dict[str, Any]) -> models.Project:
    """Parse a wire project dict into a frozen :class:`~akribes_sdk.models.Project`."""
    w = _ProjectWire.model_validate(data)
    return models.Project(id=w.id, name=w.name, created_at=w.created_at)


def parse_script(data: dict[str, Any]) -> models.Script:
    """Parse a wire script dict into a frozen :class:`~akribes_sdk.models.Script`."""
    w = _ScriptWire.model_validate(data)
    return models.Script(
        id=w.id,
        project_id=w.project_id,
        name=w.name,
        created_at=w.created_at,
    )


def parse_draft(data: dict[str, Any]) -> models.Draft:
    """Parse a wire draft dict into a frozen :class:`~akribes_sdk.models.Draft`.

    No Pydantic wire layer needed — ``Draft`` has no validators, no aliases,
    and no coercion logic. ``inputs`` normalises list-of-list to list-of-tuple.
    """
    raw_inputs = data.get("inputs", [])
    inputs = [tuple(pair) for pair in raw_inputs]  # type: ignore[misc]
    return models.Draft(source=data["source"], inputs=inputs)  # type: ignore[arg-type]


def parse_token_info(data: dict[str, Any]) -> models.TokenInfo:
    """Parse a wire token-info dict into a frozen :class:`~akribes_sdk.models.TokenInfo`."""
    w = _TokenInfoWire.model_validate(data)
    scopes = w.scopes if isinstance(w.scopes, TokenScopes) else TokenScopes.from_dict(w.scopes)
    return models.TokenInfo(
        id=w.id,
        label=w.label,
        user_email=w.user_email,
        scopes=scopes,
        minted_by=w.minted_by,
        expires_at=w.expires_at,
        revoked=w.revoked,
        created_at=w.created_at,
        last_used_at=w.last_used_at,
    )


def parse_convert_result(data: dict[str, Any]) -> models.ConvertResult:
    """Parse a wire convert-result dict into a frozen :class:`~akribes_sdk.models.ConvertResult`.

    No Pydantic wire layer needed — single ``markdown: str`` field, no coercion.
    """
    return models.ConvertResult(markdown=data["markdown"])


def parse_script_channel(data: dict[str, Any]) -> models.ScriptChannel:
    """Parse a wire script-channel dict into a frozen :class:`~akribes_sdk.models.ScriptChannel`."""
    w = _ScriptChannelWire.model_validate(data)
    return models.ScriptChannel(
        id=w.id,
        script_id=w.script_id,
        name=w.name,
        version_id=w.version_id,
        updated_at=w.updated_at,
    )


def parse_latest_version(data: dict[str, Any]) -> models.LatestVersion:
    """Parse a wire latest-version dict into a frozen :class:`~akribes_sdk.models.LatestVersion`."""
    w = _LatestVersionWire.model_validate(data)
    inputs = [tuple(pair) for pair in w.inputs]  # type: ignore[misc]
    return models.LatestVersion(
        id=w.id,
        script_id=w.script_id,
        source=w.source,
        label=w.label,
        published_by=w.published_by,
        created_at=w.created_at,
        inputs=inputs,  # type: ignore[arg-type]
    )


def parse_token_minted(data: dict[str, Any]) -> models.TokenMinted:
    """Parse a wire token-minted dict into a frozen :class:`~akribes_sdk.models.TokenMinted`."""
    w = _TokenMintedWire.model_validate(data)
    return models.TokenMinted(token=w.token, token_id=w.token_id, expires_at=w.expires_at)


def parse_token_scopes(data: dict[str, Any]) -> models.TokenScopes:
    """Parse a wire token-scopes dict into a frozen :class:`~akribes_sdk.models.TokenScopes`.

    Direct construction — no coercion needed beyond what ``from_dict`` does.
    """
    return models.TokenScopes.from_dict(data)


def parse_token_usage(data: dict[str, Any]) -> models.TokenUsage:
    """Parse a wire token-usage dict into a frozen :class:`~akribes_sdk.models.TokenUsage`."""
    return models.TokenUsage(
        input_tokens=data["input_tokens"],
        output_tokens=data["output_tokens"],
        model=data["model"],
        provider=data["provider"],
        cached_input_tokens=data["cached_input_tokens"],
        cache_write_input_tokens=data.get("cache_write_input_tokens", 0),
    )


def parse_type_ref(data: dict[str, Any]) -> models.TypeRef:
    """Parse a wire type-ref dict into a frozen :class:`~akribes_sdk.models.TypeRef`.

    Recursively constructs nested ``inner`` type refs.
    """
    inner_data = data.get("inner")
    inner = parse_type_ref(inner_data) if isinstance(inner_data, dict) else None
    return models.TypeRef(
        name=data["name"],
        inner=inner,
        choices=data.get("choices"),
    )


def parse_breaking_interest(data: dict[str, Any]) -> models.BreakingInterest:
    """Parse a wire breaking-interest dict into a frozen :class:`~akribes_sdk.models.BreakingInterest`."""
    return models.BreakingInterest(
        client_id=data["client_id"],
        client_name=data["client_name"],
        channel=data["channel"],
        lifetime=data["lifetime"],
        mismatch=data.get("mismatch", {}),
    )


def parse_publish_dry_run_result(data: dict[str, Any]) -> models.PublishDryRunResult:
    """Parse a wire publish-dry-run-result dict into a frozen :class:`~akribes_sdk.models.PublishDryRunResult`."""
    return models.PublishDryRunResult(
        dry_run=data["dry_run"],
        would_break=data["would_break"],
        breaking_interests=[
            parse_breaking_interest(b) for b in data.get("breaking_interests", [])
        ],
    )


def parse_sandbox_info(data: dict[str, Any]) -> models.SandboxInfo:
    """Parse a wire sandbox-info dict into a frozen :class:`~akribes_sdk.models.SandboxInfo`."""
    return models.SandboxInfo(project_id=data["project_id"])


def parse_s3_presigned_ref(data: dict[str, Any]) -> models.S3PresignedRef:
    """Parse a wire S3 presigned-ref dict into a frozen :class:`~akribes_sdk.models.S3PresignedRef`."""
    return models.S3PresignedRef(presigned_url=data["presigned_url"])


def parse_s3_credentials_ref(data: dict[str, Any]) -> models.S3CredentialsRef:
    """Parse a wire S3 credentials-ref dict into a frozen :class:`~akribes_sdk.models.S3CredentialsRef`."""
    return models.S3CredentialsRef(
        bucket=data["bucket"],
        key=data["key"],
        access_key_id=data["access_key_id"],
        secret_access_key=data["secret_access_key"],
        region=data.get("region"),
        session_token=data.get("session_token"),
    )


def _parse_contract_warning(data: dict[str, Any]) -> models.ContractWarning:
    """Internal helper: parse a single ContractWarning from a wire dict."""
    from akribes_sdk.models import SchemaMismatch
    mismatch_raw = data.get("mismatch", {})
    mismatch = SchemaMismatch(
        missing=[tuple(p) for p in mismatch_raw.get("missing", [])],
        wrong_type=[tuple(p) for p in mismatch_raw.get("wrong_type", [])],
        extra=mismatch_raw.get("extra", []),
    )
    return models.ContractWarning(
        client_id=data["client_id"],
        client_name=data["client_name"],
        channel=data["channel"],
        mismatch=mismatch,
    )


def parse_put_draft_response(data: dict[str, Any]) -> models.PutDraftResponse:
    """Parse a wire put-draft-response dict into a frozen :class:`~akribes_sdk.models.PutDraftResponse`."""
    return models.PutDraftResponse(
        schema_warnings=[
            _parse_contract_warning(w) for w in data.get("schema_warnings", [])
        ],
    )


def parse_upload_result(data: dict[str, Any]) -> models.UploadResult:
    """Parse a wire upload-result dict into a frozen :class:`~akribes_sdk.models.UploadResult`."""
    return models.UploadResult(
        document_id=data["document_id"],
        filename=data["filename"],
        content_hash=data["content_hash"],
        conversion_status=data["conversion_status"],
    )


def parse_claim_hit(data: dict[str, Any]) -> models.ClaimHit:
    """Parse a wire claim-hit dict into a frozen :class:`~akribes_sdk.models.ClaimHit`."""
    result = parse_upload_result(data)
    return models.ClaimHit(result=result)


def parse_ingest_progress(data: dict[str, Any]) -> models.IngestProgress:
    """Parse a wire ingest-progress dict into a frozen :class:`~akribes_sdk.models.IngestProgress`."""
    return models.IngestProgress(
        done=data.get("done_pages", data.get("done", 0)),
        total=data.get("total_pages", data.get("total", 0)),
    )


def parse_run_result(data: dict[str, Any]) -> models.RunResult:
    """Parse a wire run-result dict into a frozen :class:`~akribes_sdk.models.RunResult`."""
    return models.RunResult(
        execution_id=data["execution_id"],
        # `since_id` was added with #807 for the catchup-on-attach SSE
        # flow. Pre-0.21.13 servers don't return the field; default to 0
        # so the SDK still produces a valid model and consumers using the
        # new field on those servers behave as if no event was missed.
        since_id=int(data.get("since_id", 0)),
    )


def parse_adhoc_run_result(data: dict[str, Any]) -> models.AdhocRunResult:
    """Parse a wire adhoc-run-result dict into a frozen :class:`~akribes_sdk.models.AdhocRunResult`."""
    return models.AdhocRunResult(
        execution_id=data["execution_id"],
        project_id=data["project_id"],
        since_id=int(data.get("since_id", 0)),
    )


class _ExecutionStatusWire(BaseModel):
    model_config = ConfigDict(extra="ignore", frozen=True, populate_by_name=True)

    id: str
    project_id: int
    script_name: str
    status: Any
    started_at: Any = None
    finished_at: Any = None
    version_id: int | None = None
    channel: str | None = None
    error: str | None = None
    error_kind: str | None = None
    result: Any = None
    documents: Any = None
    triggered_by: str | None = None
    input_tokens: int = 0
    output_tokens: int = 0
    tool_tokens: int = 0
    cost_usd: float | None = None
    result_type: Any = None

    _v_started = field_validator("started_at", mode="before")(_parse_dt)
    _v_finished = field_validator("finished_at", mode="before")(_parse_dt)


def parse_execution_status(data: dict[str, Any]) -> models.ExecutionStatus:
    """Parse a wire execution-status dict into a frozen :class:`~akribes_sdk.models.ExecutionStatus`."""
    w = _ExecutionStatusWire.model_validate(data)
    result_type = parse_type_ref(w.result_type) if isinstance(w.result_type, dict) else None
    return models.ExecutionStatus(
        id=w.id,
        project_id=w.project_id,
        script_name=w.script_name,
        status=w.status,
        started_at=w.started_at,
        finished_at=w.finished_at,
        version_id=w.version_id,
        channel=w.channel,
        error=w.error,
        error_kind=w.error_kind,
        result=w.result,
        documents=w.documents,
        triggered_by=w.triggered_by,
        input_tokens=w.input_tokens,
        output_tokens=w.output_tokens,
        tool_tokens=w.tool_tokens,
        cost_usd=w.cost_usd,
        result_type=result_type,
    )


def parse_execution_output(data: dict[str, Any], *, execution_id: str = "") -> models.ExecutionOutput:
    """Parse a wire execution-output dict into a frozen :class:`~akribes_sdk.models.ExecutionOutput`.

    ``execution_id`` is passed by the caller (e.g. from the preceding ``/run``
    response) because the ``/output`` endpoint does not repeat it in the body.
    """
    return models.ExecutionOutput(
        execution_id=execution_id,
        status=data["status"],
        error=data.get("error"),
        error_kind=data.get("error_kind"),
        result=data.get("result"),
    )


def parse_execution_events(data: dict[str, Any]) -> models.ExecutionEvents:
    """Parse a wire execution-events dict into a frozen :class:`~akribes_sdk.models.ExecutionEvents`."""
    from akribes_sdk.models import EngineEvent
    events = [
        EngineEvent(type=e["type"], payload=e.get("payload"))
        for e in data.get("events", [])
    ]
    return models.ExecutionEvents(
        execution_id=data["execution_id"],
        status=data["status"],
        complete=data["complete"],
        events=events,
        next_after_id=data.get("next_after_id"),
        has_more=data.get("has_more", False),
    )


def parse_execution_tasks(data: dict[str, Any]) -> models.ExecutionTasksResponse:
    """Parse a wire ``GET /executions/{id}/tasks`` envelope into
    :class:`~akribes_sdk.models.ExecutionTasksResponse`."""
    return models.ExecutionTasksResponse(
        execution_id=data["execution_id"],
        tasks=[
            models.ExecutionTaskSummary(**t) for t in data.get("tasks", [])
        ],
    )


def parse_cost_aggregation(data: dict[str, Any]) -> models.CostAggregation:
    """Parse a wire cost-aggregation dict into a frozen :class:`~akribes_sdk.models.CostAggregation`."""
    return models.CostAggregation(
        total_executions=data["total_executions"],
        total_cost_usd=data["total_cost_usd"],
        avg_cost_usd=data["avg_cost_usd"],
        total_input_tokens=data["total_input_tokens"],
        total_output_tokens=data["total_output_tokens"],
        total_tool_tokens=data.get("total_tool_tokens", 0),
        unknown_cost_executions=data.get("unknown_cost_executions", 0),
        by_version=data.get("by_version", []),
        by_channel=data.get("by_channel", []),
    )


def parse_project_cost(data: dict[str, Any]) -> models.ProjectCost:
    """Parse a wire project-cost dict into a frozen :class:`~akribes_sdk.models.ProjectCost`."""
    return models.ProjectCost(
        project_id=data["project_id"],
        total_executions=data["total_executions"],
        total_cost_usd=data["total_cost_usd"],
        avg_cost_usd=data["avg_cost_usd"],
        total_input_tokens=data["total_input_tokens"],
        total_output_tokens=data["total_output_tokens"],
        unknown_cost_executions=data.get("unknown_cost_executions", 0),
        by_script=[
            models.CostByScript(**s) for s in data.get("by_script", [])
        ],
        by_channel=[
            models.CostByChannel(**c) for c in data.get("by_channel", [])
        ],
    )


def parse_hub_event(data: dict[str, Any]) -> models.HubEvent:
    """Parse a wire hub-event dict into a frozen :class:`~akribes_sdk.models.HubEvent`."""
    return models.HubEvent(type=data["type"], payload=data["payload"])


def parse_registered_interest(data: dict[str, Any]) -> models.RegisteredInterest:
    """Parse a wire registered-interest dict into a frozen :class:`~akribes_sdk.models.RegisteredInterest`."""
    raw_schema = data.get("input_schema", [])
    input_schema = [tuple(pair) for pair in raw_schema]  # type: ignore[misc]
    return models.RegisteredInterest(
        script_name=data["script_name"],
        channel=data["channel"],
        bound_version_id=data.get("bound_version_id"),
        input_schema=input_schema,  # type: ignore[arg-type]
    )


def parse_register_client_response(data: dict[str, Any]) -> models.RegisterClientResponse:
    """Parse a wire register-client-response dict into a frozen :class:`~akribes_sdk.models.RegisterClientResponse`."""
    return models.RegisterClientResponse(
        interests=[parse_registered_interest(i) for i in data.get("interests", [])],
    )


# ── Batch D: MCP family ──────────────────────────────────────────────────────


def parse_mcp_server_summary(data: dict[str, Any]) -> models.McpServerSummary:
    """Parse a wire MCP server summary dict."""
    return models.McpServerSummary(
        alias=data["alias"],
        url=data["url"],
        origin=data["origin"],
        is_registry=data["is_registry"],
        status=data["status"],
        tool_count=data["tool_count"],
    )


def parse_mcp_tool_summary(data: dict[str, Any]) -> models.McpToolSummary:
    """Parse a wire MCP tool summary dict."""
    return models.McpToolSummary(
        qualified_name=data["qualified_name"],
        server_alias=data["server_alias"],
        input_schema=data["input_schema"],
        description=data.get("description"),
    )


def parse_mcp_health(data: dict[str, Any]) -> models.McpHealth:
    """Parse a wire MCP health dict."""
    return models.McpHealth(
        status=data["status"],
        last_error=data.get("last_error"),
        last_check_at=data.get("last_check_at"),
    )


def parse_mcp_refresh_result(data: dict[str, Any]) -> models.McpRefreshResult:
    """Parse a wire MCP refresh-result dict."""
    return models.McpRefreshResult(
        refreshed=data["refreshed"],
        alias=data["alias"],
        tool_count=data["tool_count"],
    )


def parse_mcp_drift_result(data: dict[str, Any]) -> models.McpDriftResult:
    """Parse a wire MCP drift-result dict."""
    return models.McpDriftResult(
        drifted=data["drifted"],
        added=data.get("added", []),
        removed=data.get("removed", []),
        reason=data.get("reason"),
    )


# ── Batch E: Graph family ────────────────────────────────────────────────────


def parse_graph_node(data: dict[str, Any]) -> models.GraphNode:
    """Parse a wire graph-node dict."""
    return models.GraphNode(
        id=data["id"],
        op_type=data["op_type"],
        op_name=data.get("op_name"),
        target_var=data.get("target_var"),
        reads=data.get("reads", []),
        line=data["line"],
        col=data["col"],
    )


def parse_graph_edge(data: dict[str, Any]) -> models.GraphEdge:
    """Parse a wire graph-edge dict, mapping the ``"from"`` wire key to ``from_node``."""
    return models.GraphEdge(
        from_node=data["from"],
        to=data["to"],
    )


def parse_graph_response(data: dict[str, Any]) -> models.GraphResponse:
    """Parse a wire graph-response dict."""
    return models.GraphResponse(
        nodes=[parse_graph_node(n) for n in data.get("nodes", [])],
        edges=[parse_graph_edge(e) for e in data.get("edges", [])],
    )


# ── Bench family ─────────────────────────────────────────────────────────────


def parse_bench(data: dict[str, Any]) -> models.Bench:
    """Parse a wire bench-config dict into :class:`~akribes_sdk.models.Bench`."""
    return models.Bench(
        id=data["id"],
        script_id=data["script_id"],
        judge_script_id=data.get("judge_script_id"),
        judge_channel=data["judge_channel"],
        config=data.get("config", {}),
        created_at=data.get("created_at", ""),
        updated_at=data.get("updated_at", ""),
    )


def parse_project_bench_summary(data: dict[str, Any]) -> models.ProjectBenchSummary:
    """Parse a wire project-bench-summary dict (``GET /projects/{id}/benches``)."""
    return models.ProjectBenchSummary(
        bench_id=data["bench_id"],
        script_id=data["script_id"],
        script_name=data["script_name"],
        judge_script_id=data.get("judge_script_id"),
        judge_script_name=data.get("judge_script_name"),
        judge_channel=data["judge_channel"],
        case_count=data.get("case_count", 0),
        latest_run_id=data.get("latest_run_id"),
        latest_run_status=data.get("latest_run_status"),
        latest_run_channel=data.get("latest_run_channel"),
        latest_run_workflow_version_id=data.get("latest_run_workflow_version_id"),
        latest_run_at=data.get("latest_run_at"),
        latest_run_mean_score=data.get("latest_run_mean_score"),
        latest_run_cost_usd=data.get("latest_run_cost_usd"),
        updated_at=data.get("updated_at", ""),
    )


def parse_bench_run(data: dict[str, Any]) -> models.BenchRun:
    """Parse a wire bench-run dict into :class:`~akribes_sdk.models.BenchRun`."""
    return models.BenchRun(
        id=data["id"],
        bench_id=data["bench_id"],
        channel=data["channel"],
        workflow_version_id=data["workflow_version_id"],
        judge_version_id=data["judge_version_id"],
        status=data["status"],
        triggered_by=data.get("triggered_by"),
        triggered_at=data.get("triggered_at", ""),
        completed_at=data.get("completed_at"),
        total_cost_usd=data.get("total_cost_usd", 0.0),
        total_cases=data.get("total_cases", 0),
        cache_hit_cases=data.get("cache_hit_cases", 0),
        notes=data.get("notes"),
        mcp_session_id=data.get("mcp_session_id"),
        case_filter=data.get("case_filter"),
        mean_headline_score=data.get("mean_headline_score"),
        ok_cases=data.get("ok_cases"),
        status_breakdown=data.get("status_breakdown"),
        judge_script_name=data.get("judge_script_name"),
        warnings=list(data.get("warnings", [])),
    )


def parse_bench_result(data: dict[str, Any]) -> models.BenchResult:
    """Parse a wire bench-result dict into :class:`~akribes_sdk.models.BenchResult`.

    Handles both the ``/results`` read shape (with ``workflow_output`` +
    ``error``) and the leaner per-case SSE ``result`` event shape."""
    return models.BenchResult(
        id=data["id"],
        bench_run_id=data["bench_run_id"],
        case_id=data["case_id"],
        status=data["status"],
        created_at=data.get("created_at", ""),
        workflow_execution_id=data.get("workflow_execution_id"),
        judge_execution_id=data.get("judge_execution_id"),
        score=data.get("score"),
        headline_score=data.get("headline_score"),
        cost_usd=data.get("cost_usd", 0.0),
        duration_ms=data.get("duration_ms"),
        cache_hit=data.get("cache_hit", False),
        input_hash=data.get("input_hash"),
        error=data.get("error"),
        workflow_output=data.get("workflow_output"),
    )


def parse_bench_event(data: dict[str, Any]) -> models.BenchEvent:
    """Parse a wire ``BenchEvent`` dict (the ``payload`` of a ``HubEvent`` whose
    ``type == "Bench"``) into :class:`~akribes_sdk.models.BenchEvent`.

    The wire shape is adjacently tagged like the server's ``BenchEvent`` enum::

        {"type": "RunStarted",     "payload": {"project_id", "script_name", "run"}}
        {"type": "ResultRecorded", "payload": {"project_id", "script_name", "run_id", "result"}}
        {"type": "RunFinished",    "payload": {"project_id", "script_name", "run"}}

    Reuses :func:`parse_bench_run` / :func:`parse_bench_result` for the embedded
    rows."""
    ty = data["type"]
    inner = data.get("payload", {})
    run = inner.get("run")
    result = inner.get("result")
    return models.BenchEvent(
        type=ty,
        project_id=inner["project_id"],
        script_name=inner["script_name"],
        run=parse_bench_run(run) if run is not None else None,
        run_id=inner.get("run_id"),
        result=parse_bench_result(result) if result is not None else None,
    )


def parse_bench_case(data: dict[str, Any]) -> models.BenchCase:
    """Parse a wire bench-case dict into :class:`~akribes_sdk.models.BenchCase`."""
    return models.BenchCase(
        id=data["id"],
        project_id=data["project_id"],
        script_name=data["script_name"],
        kind=data.get("kind", "case"),
        frozen=data.get("frozen", True),
        created_at=data.get("created_at", ""),
        bench_id=data.get("bench_id"),
        case_name=data.get("case_name"),
        inputs=data.get("inputs"),
        expected_output=data.get("expected_output"),
        ground_truth=data.get("ground_truth"),
        input_hash=data.get("input_hash"),
    )


def parse_compare_report(data: dict[str, Any]) -> models.CompareReport:
    """Parse a wire compare-report dict (``GET /bench-runs/{a}/compare/{b}``)."""
    agg = data.get("aggregate", {})
    return models.CompareReport(
        run_a_id=data["run_a_id"],
        run_b_id=data["run_b_id"],
        aggregate=models.CompareAggregate(
            mean_score_delta=agg.get("mean_score_delta", 0.0),
            cost_delta_usd=agg.get("cost_delta_usd", 0.0),
            n_regressed=agg.get("n_regressed", 0),
            n_improved=agg.get("n_improved", 0),
            n_unchanged=agg.get("n_unchanged", 0),
        ),
        per_case=[
            models.CompareCase(
                case_id=c["case_id"],
                case_label=c.get("case_label", c["case_id"]),
                flag=c.get("flag", "unchanged"),
                score_a=c.get("score_a"),
                score_b=c.get("score_b"),
                delta=c.get("delta"),
            )
            for c in data.get("per_case", [])
        ],
    )


def parse_drift_report(data: dict[str, Any]) -> models.DriftReport:
    """Parse a wire drift-report dict (cases contract-drift endpoint)."""
    return models.DriftReport(
        drifted=[
            models.DriftedCase(
                case_id=d["case_id"],
                label=d.get("label", d["case_id"]),
                what_broke=d.get("what_broke", ""),
            )
            for d in data.get("drifted", [])
        ],
        script_version_id=data.get("script_version_id"),
        published_at=data.get("published_at"),
        published_by=data.get("published_by"),
        summary=data.get("summary", ""),
    )


def parse_bench_run_tag_session(data: dict[str, Any]) -> models.BenchRunTagSessionResponse:
    """Parse the receipt from ``PATCH /bench-runs/{id}/tag-session``."""
    return models.BenchRunTagSessionResponse(
        tagged=data.get("tagged", False),
        run_id=data["run_id"],
        mcp_session_id=data["mcp_session_id"],
    )


# ── Batch G: Suspend trigger family ─────────────────────────────────────────


def parse_unable_record(data: dict[str, Any]) -> models.UnableRecord:
    """Parse a wire unable-record dict."""
    return models.UnableRecord(
        reason=data["reason"],
        category=data["category"],
        missing=data.get("missing", []),
    )


def parse_validation_error_wire(data: dict[str, Any]) -> models.ValidationErrorWire:
    """Parse a wire validation-error dict."""
    return models.ValidationErrorWire(
        stage=data["stage"],
        message=data["message"],
        path=data.get("path"),
    )


def parse_dag_position_trigger(data: dict[str, Any]) -> models.DagPositionTrigger:
    """Parse a wire DagPosition trigger dict."""
    return models.DagPositionTrigger(kind=data.get("kind", "DagPosition"))


def parse_validation_exhausted_trigger(data: dict[str, Any]) -> models.ValidationExhaustedTrigger:
    """Parse a wire ValidationExhausted trigger dict."""
    return models.ValidationExhaustedTrigger(
        kind=data.get("kind", "ValidationExhausted"),
        task_name=data["task_name"],
        retry_count=data["retry_count"],
        last_attempt=data["last_attempt"],
        validation_errors=[
            parse_validation_error_wire(e) for e in data.get("validation_errors", [])
        ],
    )


def parse_agent_unable_trigger(data: dict[str, Any]) -> models.AgentUnableTrigger:
    """Parse a wire AgentUnable trigger dict."""
    return models.AgentUnableTrigger(
        kind=data.get("kind", "AgentUnable"),
        task_name=data["task_name"],
        unable=parse_unable_record(data["unable"]),
    )
