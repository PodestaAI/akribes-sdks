from __future__ import annotations

import asyncio
import dataclasses
import json
import time
from pathlib import Path
from typing import Any, Literal, TYPE_CHECKING

from akribes_sdk._pagination import AsyncPage
from akribes_sdk._timing import as_list
from akribes_sdk.errors import (
    AuthError,
    NotFoundError,
    ScriptError,
    AkribesTimeoutError,
    TransientError,
)
from akribes_sdk.resources._sentinel import _MISSING
from akribes_sdk._parsers import (
    parse_cost_aggregation,
    parse_execution_events,
    parse_execution_output,
    parse_execution_status,
    parse_execution_tasks,
    parse_graph_edge,
    parse_graph_node,
    parse_project_cost,
    parse_run_result,
)
from akribes_sdk.models import (
    DocumentMeta,
    ErrorKind,
    ExecutionChildSummary,
    ExecutionEvents,
    ExecutionOutput,
    ExecutionStatus,
    ExecutionStatusValue,
    ExecutionTasksResponse,
    GraphResponse,
    ProjectCost,
    ReconvertResult,
    RunResult,
    S3PresignedRef,
    S3CredentialsRef,
    ScriptCost,
)
from akribes_sdk.resources._base import Resource, ProjectResource

if TYPE_CHECKING:
    from akribes_sdk.run_stream import RunStream


class ExecutionsByID(Resource):
    """Global by-ID ops on executions and documents. Mounted on AkribesClient.executions."""

    async def get(self, execution_id: str, *, default=_MISSING) -> ExecutionStatus:
        """Return the execution status or raise :class:`NotFoundError`.

        Pass ``default=None`` (or any other value) to suppress the error and
        return that value instead — mirroring ``dict.get(key, default)``.
        """
        try:
            res = await self._request("GET", f"{self._base_url}/executions/{execution_id}")
        except NotFoundError:
            if default is _MISSING:
                raise
            return default  # type: ignore[return-value]
        return parse_execution_status(res.json())

    async def get_output(self, execution_id: str, *, default=_MISSING) -> ExecutionOutput:
        """Return the execution output or raise :class:`NotFoundError`.

        Pass ``default=None`` (or any other value) to suppress the error and
        return that value instead — mirroring ``dict.get(key, default)``.
        """
        try:
            res = await self._request("GET", f"{self._base_url}/executions/{execution_id}/output")
        except NotFoundError:
            if default is _MISSING:
                raise
            return default  # type: ignore[return-value]
        return parse_execution_output(res.json(), execution_id=execution_id)

    async def get_events(self, execution_id: str, *, after_id: int | None = None, limit: int | None = None, default=_MISSING) -> ExecutionEvents:
        """Return the persisted event stream for an execution or raise :class:`NotFoundError`.

        Works for both completed and in-progress executions. When ``complete``
        is ``False`` the caller may poll until it becomes ``True``.

        Use *after_id* and *limit* for pagination. Pass ``default=None`` (or
        any other value) to suppress :class:`NotFoundError` and return that
        value instead.
        """
        params: dict[str, str] = {}
        if after_id is not None:
            params["after_id"] = str(after_id)
        if limit is not None:
            params["limit"] = str(limit)
        try:
            res = await self._request(
                "GET",
                f"{self._base_url}/executions/{execution_id}/events",
                params=params if params else None,
            )
        except NotFoundError:
            if default is _MISSING:
                raise
            return default  # type: ignore[return-value]
        return parse_execution_events(res.json())

    async def cancel(self, execution_id: str) -> None:
        """Cancel a specific execution. Raises :class:`NotFoundError` if not found."""
        await self._request("DELETE", f"{self._base_url}/executions/{execution_id}")

    async def resume(self, execution_id: str, token: str, data: Any) -> None:
        """Resume a suspended checkpoint within a running execution."""
        await self._request(
            "POST",
            f"{self._base_url}/executions/{execution_id}/resume",
            json={"token": token, "data": data},
        )

    async def await_result(
        self,
        execution_id: str,
        *,
        timeout: float | None = None,
        poll_interval: float = 0.5,
        max_poll_interval: float = 5.0,
    ) -> ExecutionOutput:
        """Poll until execution reaches a terminal state, then return output.

        Uses exponential backoff (1.5x) capped at *max_poll_interval*.
        Raises :class:`AkribesTimeoutError` if *timeout* seconds elapse first.
        On failure, raises the appropriate typed error based on ``error_kind``.
        """
        start = time.monotonic()
        interval = poll_interval
        while True:
            output = await self.get_output(execution_id, default=None)
            if output is not None and output.status in ("completed", "failed", "cancelled"):
                if output.status == "failed":
                    _raise_for_failed_execution(output, execution_id)
                return output
            if timeout is not None and (time.monotonic() - start) >= timeout:
                raise AkribesTimeoutError(
                    f"Execution {execution_id} did not complete within {timeout}s",
                    execution_id=execution_id,
                )
            await asyncio.sleep(interval)
            interval = min(interval * 1.5, max_poll_interval)

    # Cross-SDK naming alias. Rust uses ``await_execution`` and TS uses
    # ``await``; Python keeps ``await_result`` as the canonical form
    # (``await`` is a Python keyword) but exposes ``await_execution`` so
    # callers porting examples across SDKs don't trip on the rename.
    # Refs #109 (item 3: method-naming consistency).
    async def await_execution(
        self,
        execution_id: str,
        *,
        timeout: float | None = None,
        poll_interval: float = 0.5,
        max_poll_interval: float = 5.0,
    ) -> ExecutionOutput:
        """Alias for :meth:`await_result`. Matches Rust ``await_execution``."""
        return await self.await_result(
            execution_id,
            timeout=timeout,
            poll_interval=poll_interval,
            max_poll_interval=max_poll_interval,
        )

    async def get_document(self, document_id: str, *, default=_MISSING) -> DocumentMeta:
        """Get document metadata by ID. Raises :class:`NotFoundError` if not found.

        Returns :class:`DocumentMeta` (typed) instead of ``dict[str, Any]`` (#1063).
        Pass ``default=None`` (or any other value) to suppress the error and
        return that value instead — mirroring ``dict.get(key, default)``.
        """
        try:
            res = await self._request("GET", f"{self._base_url}/documents/{document_id}")
        except NotFoundError:
            if default is _MISSING:
                raise
            return default  # type: ignore[return-value]
        return DocumentMeta(**res.json())

    async def get_document_markdown(self, document_id: str) -> str:
        """Get converted markdown for a document."""
        res = await self._request("GET", f"{self._base_url}/documents/{document_id}/markdown")
        return res.json()["markdown"]

    async def get_document_url(self, document_id: str) -> str:
        """Get a presigned download URL for the original document file."""
        res = await self._request(
            "GET",
            f"{self._base_url}/documents/{document_id}/content",
            follow_redirects=False,
        )
        return res.headers.get("location", str(res.url))

    async def reconvert_document(self, document_id: str) -> ReconvertResult:
        """Retry conversion on a failed document (#1063).

        Returns :class:`ReconvertResult` (``{status: str}``)."""
        res = await self._request("POST", f"{self._base_url}/documents/{document_id}/convert")
        return ReconvertResult(**res.json())

    async def children(self, execution_id: str) -> list[ExecutionChildSummary]:
        """List child executions spawned via the engine's ``spawn_child_execution``
        callback. Returns an empty list when no children exist (the common
        case for v1, where parent-linkage columns are typically NULL).

        Mirrors TS ``executions.children(executionId)`` (#1054).
        """
        try:
            res = await self._request(
                "GET",
                f"{self._base_url}/executions/{execution_id}/children",
            )
        except NotFoundError:
            return []
        return [ExecutionChildSummary(**c) for c in res.json()]

    async def tasks(self, execution_id: str) -> ExecutionTasksResponse:
        """Per-task cost / token / duration breakdown for an execution
        (``GET /executions/{id}/tasks``).

        Reads from the ``execution_tasks`` table, populated as ``TaskEnd``
        events arrive — one row per ``task_name``. Useful for monolith
        workflows with no spawned children, where every agent invocation
        lives in ``execution_tasks``. Mirrors TS ``executions.tasks``."""
        res = await self._request(
            "GET",
            f"{self._base_url}/executions/{execution_id}/tasks",
        )
        return parse_execution_tasks(res.json())


class Executions(ProjectResource):
    """Project-scoped run/list ops. Mounted on ProjectHandle.executions."""

    async def run(
        self,
        script_name: str,
        *,
        channel: str = "production",
        triggered_by: str | None = None,
        breakpoint_lines: int | list[int] | None = None,
        idempotency_key: str | None = None,
        inputs: dict[str, Any] | None = None,
        **input_kwargs: Any,
    ) -> RunResult:
        body_inputs = (inputs or {}) | input_kwargs
        mode = _classify_inputs(body_inputs) if body_inputs else "json"
        if mode == "upload":
            files = {
                k: (
                    (v.name if isinstance(v, Path) else "blob.bin"),
                    v.read_bytes() if isinstance(v, Path) else v,
                )
                for k, v in body_inputs.items()
            }
            return await self._run_with_upload(
                script_name, files, channel=channel, triggered_by=triggered_by
            )
        if mode == "s3":
            return await self._run_with_s3(
                script_name, body_inputs, channel=channel, triggered_by=triggered_by
            )
        return await self._run_json(
            script_name, body_inputs, channel=channel, triggered_by=triggered_by,
            breakpoint_lines=as_list(breakpoint_lines), idempotency_key=idempotency_key,
        )

    async def _run_json(
        self,
        script_name: str,
        inputs: dict[str, Any],
        *,
        channel: str,
        triggered_by: str | None,
        breakpoint_lines: list[int] | None,
        idempotency_key: str | None,
    ) -> RunResult:
        self._api.validate_contract(script_name)
        body: dict[str, Any] = {
            "inputs": inputs or None,
            "triggered_by": triggered_by or self._api.name,
        }
        if breakpoint_lines is not None:
            body["breakpoint_lines"] = breakpoint_lines
        headers = {"Idempotency-Key": idempotency_key} if idempotency_key else {}
        res = await self._request(
            "POST",
            self._project_url("scripts", script_name, "run"),
            params={"channel": channel},
            json=body,
            headers=headers,
        )
        return parse_run_result(res.json())

    def list(
        self,
        script_name: str,
        *,
        status: ExecutionStatusValue | None = None,
        channel: str | None = None,
    ) -> AsyncPage[ExecutionStatus]:
        """List executions for a script, newest first.

        *status* and *channel* are filter parameters orthogonal to pagination.
        Pagination (offset/limit) is driven internally by :class:`AsyncPage`.
        """
        filter_params: dict[str, str] = {}
        if status:
            filter_params["status"] = status
        if channel:
            filter_params["channel"] = channel

        async def fetch(offset: int, limit: int) -> tuple[list[ExecutionStatus], bool]:
            params: dict[str, str] = {"limit": str(limit), "offset": str(offset)}
            params.update(filter_params)
            res = await self._request(
                "GET",
                self._project_url("scripts", script_name, "executions"),
                params=params,
            )
            items = [parse_execution_status(e) for e in res.json()]
            return items, len(items) == limit

        return AsyncPage(fetch)

    async def get_graph(
        self,
        script_name: str,
        version_id: int | None = None,
    ) -> GraphResponse:
        """Get the compiled execution DAG for a script. If version_id is None, uses the draft."""
        params: dict[str, str] = {}
        if version_id is not None:
            params["version"] = str(version_id)
        res = await self._request(
            "GET",
            self._project_url("scripts", script_name, "graph"),
            params=params if params else None,
        )
        data = res.json()
        return GraphResponse(
            nodes=[parse_graph_node(n) for n in data["nodes"]],
            edges=[parse_graph_edge(e) for e in data["edges"]],
        )

    async def get_project_cost(
        self,
        *,
        since: str | None = None,
        until: str | None = None,
    ) -> ProjectCost:
        """Get typed cost aggregation for the entire project (#1063).

        Optionally filtered by date range. Returns :class:`ProjectCost` —
        callers on ``mypy --strict`` get autocomplete on ``.total_cost_usd``,
        ``.by_script``, ``.by_channel`` etc. (previously ``dict[str, Any]``).
        """
        params: dict[str, str] = {}
        if since is not None:
            params["since"] = since
        if until is not None:
            params["until"] = until
        res = await self._request(
            "GET",
            self._project_url("cost"),
            params=params if params else None,
        )
        return parse_project_cost(res.json())

    async def get_cost(self, script_name: str) -> ScriptCost:
        """Get cost aggregation for a script (total, avg, per-version) (#1193).

        Returns :class:`ScriptCost` (canonical name shared with TS;
        ``CostAggregation`` is kept as a back-compat alias)."""
        res = await self._request(
            "GET",
            self._project_url("scripts", script_name, "cost"),
        )
        return parse_cost_aggregation(res.json())

    async def _run_with_upload(
        self,
        script_name: str,
        files: dict[str, tuple[str, bytes]],
        *,
        channel: str = "production",
        triggered_by: str | None = None,
    ) -> RunResult:
        """Internal: run a script by uploading document files as multipart form data.

        *files* maps input names to ``(filename, content)`` tuples.
        Call via the polymorphic :meth:`run` — pass ``Path`` or ``bytes`` values.
        """
        meta = {"triggered_by": triggered_by or self._api.name}
        res = await self._request(
            "POST",
            self._project_url("scripts", script_name, "run", "upload"),
            params={"channel": channel},
            files=files,
            data={"_meta": json.dumps(meta)},
        )
        return parse_run_result(res.json())

    async def _run_with_s3(
        self,
        script_name: str,
        inputs: dict[str, Any],
        *,
        channel: str = "production",
        triggered_by: str | None = None,
    ) -> RunResult:
        """Internal: run a script referencing documents already stored in S3.

        Call via the polymorphic :meth:`run` — pass
        :class:`~akribes_sdk.models.S3PresignedRef` or
        :class:`~akribes_sdk.models.S3CredentialsRef` values.
        """
        serialized: dict[str, dict[str, Any]] = {}
        for name, ref in inputs.items():
            d = dataclasses.asdict(ref)
            # Strip None values for cleaner JSON
            serialized[name] = {k: v for k, v in d.items() if v is not None}

        res = await self._request(
            "POST",
            self._project_url("scripts", script_name, "run", "s3"),
            json={
                "inputs": serialized,
                "channel": channel,
                "triggered_by": triggered_by or self._api.name,
            },
        )
        return parse_run_result(res.json())

    async def run_from(
        self,
        script_name: str,
        seed_env: dict[str, Any],
        skip_node_ids: int | list[int],
        *,
        channel: str = "draft",
        inputs: dict[str, Any] | None = None,
        triggered_by: str | None = None,
    ) -> RunResult:
        """Partial re-execution from a specific point in the graph.

        *seed_env* provides previously computed node outputs, and
        *skip_node_ids* lists nodes whose execution should be skipped.
        """
        skip_node_ids = as_list(skip_node_ids)  # type: ignore[assignment]
        body: dict[str, Any] = {
            "seed_env": seed_env,
            "skip_node_ids": skip_node_ids,
            "triggered_by": triggered_by or self._api.name,
        }
        if inputs is not None:
            body["inputs"] = inputs
        res = await self._request(
            "POST",
            self._project_url("scripts", script_name, "run", "from"),
            params={"channel": channel},
            json=body,
        )
        return parse_run_result(res.json())

    async def cancel_all(self, script_name: str) -> None:
        """Cancel all running executions for a script."""
        await self._request("DELETE", self._project_url("scripts", script_name, "run"))

    # Cross-SDK naming alias. Rust uses ``cancel_run`` and TS uses
    # ``cancelRun``; Python keeps ``cancel_all`` as the canonical form but
    # exposes ``cancel_run`` so callers porting examples across SDKs don't
    # trip on the name. Refs #109 (item 3: method-naming consistency).
    async def cancel_run(self, script_name: str) -> None:
        """Alias for :meth:`cancel_all`. Matches Rust ``cancel_run`` /
        TypeScript ``cancelRun``."""
        await self.cancel_all(script_name)

    async def run_and_await(
        self,
        script_name: str,
        *,
        channel: str = "production",
        timeout: float | None = None,
        inputs: dict[str, Any] | None = None,
        **input_kwargs: Any,
    ) -> ExecutionOutput:
        """Convenience: run a script and block until it finishes.

        Returns an :class:`~akribes_sdk.models.ExecutionOutput` with
        ``execution_id`` populated. Input kwargs are merged with ``inputs``
        (kwargs take precedence).
        """
        from akribes_sdk.resources._base import _ApiClient
        result = await self.run(
            script_name, channel=channel, inputs=inputs, **input_kwargs
        )
        by_id = ExecutionsByID(_ApiClient(self._api._client))
        return await by_id.await_result(result.execution_id, timeout=timeout)

    async def run_stream(
        self,
        script_name: str,
        *,
        inputs: dict[str, Any] | None = None,
        channel: str = "production",
        triggered_by: str | None = None,
        breakpoint_lines: int | list[int] | None = None,
        **input_kwargs: Any,
    ) -> "RunStream":
        """Run a script and return a live :class:`RunStream` handle.

        The stream subscribes to engine events *before* POSTing ``/run`` so
        early events are never missed. Consume with ``async for``, register
        callbacks via ``stream.on.<category>()``, or ``await stream.output()``
        for the final payload.

        Input kwargs are merged with ``inputs`` (kwargs take precedence).

        Example::

            run = await proj.executions.run_stream("summarize", brief="…")
            run.on.output(lambda c: print(c.chunk, end=""))
            result = await run.output(timeout=300)
        """
        from akribes_sdk.run_stream import RunStream
        body_inputs = (inputs or {}) | input_kwargs
        stream = RunStream(self, script_name)
        await stream._start(
            inputs=body_inputs or None,
            channel=channel,
            triggered_by=triggered_by,
            breakpoint_lines=as_list(breakpoint_lines),  # type: ignore[arg-type]
        )
        return stream


def _classify_inputs(inputs: dict[str, Any]) -> Literal["json", "upload", "s3"]:
    """Dispatch classifier: determine which run endpoint to use based on input value types."""
    has_path_or_bytes = any(isinstance(v, (Path, bytes)) for v in inputs.values())
    has_s3 = any(isinstance(v, (S3PresignedRef, S3CredentialsRef)) for v in inputs.values())
    if has_path_or_bytes and has_s3:
        raise ValueError("cannot mix Path/bytes and S3 doc inputs in one run")
    if has_path_or_bytes:
        return "upload"
    if has_s3:
        return "s3"
    return "json"


def _raise_for_failed_execution(output: ExecutionOutput, execution_id: str) -> None:
    msg = output.error or "Execution failed"
    try:
        kind = ErrorKind(output.error_kind) if output.error_kind else None
    except ValueError:
        kind = None
    if kind is not None and kind.is_transient:
        raise TransientError(msg, execution_id=execution_id)
    if kind is not None and kind.is_fatal:
        raise AuthError(msg, execution_id=execution_id)
    raise ScriptError(msg, execution_id=execution_id)
