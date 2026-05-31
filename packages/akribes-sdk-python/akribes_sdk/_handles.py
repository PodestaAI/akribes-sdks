"""Project- and script-scoped facade handles for the Akribes SDK."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, TypeVar, overload

from typing_extensions import Unpack

from akribes_sdk._parsers import parse_adhoc_run_result
from akribes_sdk.models import (
    AdhocRunResult,
    ExecutionOutput,
    ExecutionStatus,
    ExecutionStatusValue,
    RunResult,
    S3DocumentRef,
)
from akribes_sdk.script_type import ScriptType

if TYPE_CHECKING:
    from akribes_sdk.client import AkribesClient

I = TypeVar("I")
O = TypeVar("O")


class ProjectHandle:
    """Project-scoped namespace. Returned from AkribesClient.project / get_project."""

    __slots__ = (
        "_client", "id",
        "scripts", "drafts", "versions", "channels", "executions", "evals",
        "mcp", "documents", "clients", "events",
    )

    def __init__(self, client: "AkribesClient", project_id: int) -> None:
        self._client = client
        self.id = project_id
        from akribes_sdk.resources._base import _ProjectApiClient
        from akribes_sdk.resources import (
            Scripts, Drafts, Versions, Channels, Executions, Evals, Mcp,
            DocumentsClient, ClientsProjectScoped, EventsProjectScoped,
        )
        api = _ProjectApiClient(client, project_id)
        self.scripts    = Scripts(api)
        self.drafts     = Drafts(api)
        self.versions   = Versions(api)
        self.channels   = Channels(api)
        self.executions = Executions(api)
        self.evals      = Evals(api)
        self.mcp        = Mcp(api)
        self.documents  = DocumentsClient(api, client._ingest_poll_timeout_ms)
        self.clients    = ClientsProjectScoped(api)
        self.events     = EventsProjectScoped(api)

    def script(self, name: str) -> "ScriptHandle":
        """Return a :class:`ScriptHandle` for ``name``."""
        return ScriptHandle(self, name)

    async def _verify_schema(self, script_type: "ScriptType[Any, Any]") -> None:
        """Verify a typed ScriptType's schema_hash matches the live server signature.

        Cached per (project_id, script_name, schema_hash) on the client so a
        long-running session only pays the verification cost once per script.
        No-op when schema_hash is empty (untyped ScriptType, codegen with no hash).
        """
        if not script_type.schema_hash:
            return
        cache_key = (self.id, script_type.name, script_type.schema_hash)
        if cache_key in self._client._verified_schemas:
            return
        res = await self._client._request(
            "GET",
            f"{self._client.base_url}/projects/{self.id}/scripts/{script_type.name}/signature",
            params={"channel": "production"},
        )
        body = res.json()
        server_hash = body.get("schema_hash", "")
        if server_hash and server_hash != script_type.schema_hash:
            from akribes_sdk.errors import ScriptSchemaChangedError
            raise ScriptSchemaChangedError(script_type.name)
        self._client._verified_schemas.add(cache_key)

    # PEP 692 Unpack[TypedDict] on an unbound TypeVar — pyright can't statically
    # prove I is a TypedDict at the overload definition (Python's typing has no
    # `bound=TypedDict` until PEP 696 stabilises), but it correctly narrows
    # **inputs to the field types at concrete call sites where I = some
    # TypedDict (the codegen-emitted ones). The misc ignore silences the
    # def-site warning without affecting call-site narrowing.
    @overload
    async def run(self, script: str, *, channel: str = ..., **inputs: Any) -> RunResult: ...
    @overload
    async def run(self, script: "ScriptType[I, O]", *, channel: str = ..., **inputs: "Unpack[I]") -> RunResult: ...  # type: ignore[misc]

    async def run(self, script: "str | ScriptType[Any, Any]", *, channel: str = "production", **inputs: Any) -> RunResult:
        """Run a script. Accepts a script name string or a typed ScriptType[I, O].

        When called with a ScriptType, the IDE narrows ``**inputs`` to the input
        TypedDict's fields via PEP 692 ``Unpack[I]`` (second overload). The
        runtime is identical for both forms — the ScriptType is unwrapped to its
        ``.name``. Schema drift is checked on first use when schema_hash is set.
        """
        if isinstance(script, ScriptType):
            await self._verify_schema(script)
            return await self.executions.run(script.name, channel=channel, **inputs)
        return await self.executions.run(script, channel=channel, **inputs)

    @overload
    async def run_and_await(self, script: str, *, channel: str = ..., timeout: "float | None" = ..., **inputs: Any) -> ExecutionOutput: ...
    @overload
    async def run_and_await(self, script: "ScriptType[I, O]", *, channel: str = ..., timeout: "float | None" = ..., **inputs: "Unpack[I]") -> ExecutionOutput: ...  # type: ignore[misc]

    async def run_and_await(
        self,
        script: "str | ScriptType[Any, Any]",
        *,
        channel: str = "production",
        timeout: float | None = None,
        **inputs: Any,
    ) -> ExecutionOutput:
        """Run a script and block until it finishes.

        Accepts a script name string or a typed ScriptType[I, O]. When called
        with a ScriptType, the IDE narrows ``**inputs`` to the input TypedDict's
        fields via PEP 692 ``Unpack[I]`` (second overload). The runtime is
        identical for both forms — the ScriptType is unwrapped to its ``.name``.
        Schema drift is checked on first use when schema_hash is set.

        Returns an :class:`~akribes_sdk.models.ExecutionOutput` with
        ``execution_id`` populated.
        """
        if isinstance(script, ScriptType):
            await self._verify_schema(script)
            return await self.executions.run_and_await(
                script.name, channel=channel, timeout=timeout, **inputs,
            )
        return await self.executions.run_and_await(
            script, channel=channel, timeout=timeout, **inputs,
        )

    async def cost(self, *, since: str | None = None, until: str | None = None) -> dict[str, Any]:
        """Get cost aggregation for this project."""
        return await self.executions.get_project_cost(since=since, until=until)

    async def run_source(
        self,
        source: str,
        *,
        channel: str | None = None,
        triggered_by: str | None = None,
        inputs: dict[str, Any] | None = None,
        **input_kwargs: Any,
    ) -> AdhocRunResult:
        """Run raw ``.akr`` source in this project (typically the sandbox).

        Inputs may be passed as a dict via ``inputs=`` or as keyword arguments.
        The two forms are merged; keyword arguments take precedence::

            result = await sandbox.run_source(SCRIPT, brief="hello")
            result = await sandbox.run_source(SCRIPT, inputs={"brief": "hello"})

        The ``/execute`` endpoint is global, so this method reaches back to the
        underlying client's ``_request``.
        """
        body: dict[str, Any] = {"source": source}
        body_inputs = (inputs or {}) | input_kwargs
        if body_inputs:
            body["inputs"] = body_inputs
        if channel is not None:
            body["channel"] = channel
        if triggered_by is not None:
            body["triggered_by"] = triggered_by
        res = await self._client._request(
            "POST", f"{self._client.base_url}/execute", json=body
        )
        return parse_adhoc_run_result(res.json())

    def __repr__(self) -> str:  # pragma: no cover - trivial
        return f"ProjectHandle(id={self.id})"


class ScriptHandle:
    """Script-scoped convenience wrapper. Returned from ``ProjectHandle.script``."""

    __slots__ = ("_project", "name")

    def __init__(self, project: ProjectHandle, name: str) -> None:
        self._project = project
        self.name = name

    # ── Execution ───────────────────────────────────────────────────────

    async def run(
        self,
        *,
        channel: str = "production",
        triggered_by: str | None = None,
        breakpoint_lines: int | list[int] | None = None,
        idempotency_key: str | None = None,
        inputs: dict[str, Any] | None = None,
        **input_kwargs: Any,
    ) -> RunResult:
        """Start an execution. Returns immediately with a :class:`RunResult`.

        Inputs may be passed as keyword arguments (``brief="hi"``) or via the
        ``inputs`` dict. Path/bytes values dispatch to the upload endpoint;
        S3 ref values dispatch to the S3 endpoint.
        """
        return await self._project.executions.run(
            self.name,
            channel=channel,
            triggered_by=triggered_by,
            breakpoint_lines=breakpoint_lines,
            idempotency_key=idempotency_key,
            inputs=inputs,
            **input_kwargs,
        )

    async def run_and_await(
        self,
        *,
        channel: str = "production",
        timeout: float | None = None,
        inputs: dict[str, Any] | None = None,
        **input_kwargs: Any,
    ) -> ExecutionOutput:
        """Start an execution and block until it finishes.

        Returns an :class:`~akribes_sdk.models.ExecutionOutput` with
        ``execution_id`` populated.
        """
        return await self._project.executions.run_and_await(
            self.name, channel=channel, timeout=timeout, inputs=inputs, **input_kwargs
        )

    async def list_executions(
        self,
        *,
        status: ExecutionStatusValue | None = None,
        channel: str | None = None,
        limit: int = 50,
        offset: int = 0,
    ) -> list[ExecutionStatus]:
        return await self._project.executions.list(
            self.name, status=status, channel=channel, limit=limit, offset=offset
        )

    async def cancel_all(self) -> None:
        await self._project.executions.cancel_all(self.name)

    # ── Scripts / Drafts / Versions ─────────────────────────────────────

    async def get_draft(self) -> Any:
        return await self._project.drafts.get(self.name)

    async def save_draft(self, source: str) -> Any:
        return await self._project.drafts.save(self.name, source)

    async def list_versions(self) -> Any:
        return await self._project.versions.list(self.name)

    async def get_latest(self) -> Any:
        return await self._project.versions.get_latest(self.name)

    async def publish(self, channels: list[str], *, label: str | None = None, force: bool = False, dry_run: bool = False) -> Any:
        return await self._project.versions.publish(self.name, channels, label=label, force=force, dry_run=dry_run)

    # ── Streaming ───────────────────────────────────────────────────────

    def on_execution(self, callback: Any, *, on_error: Any = None) -> Any:
        """Subscribe to engine events for this script."""
        return self._project.events.on_execution(self.name, callback, on_error=on_error)

    def on_change(self, callback: Any, *, on_error: Any = None) -> Any:
        """Subscribe to version/channel changes for this script."""
        return self._project.events.on_change(self.name, callback, on_error=on_error)

    def __repr__(self) -> str:  # pragma: no cover - trivial
        return f"ScriptHandle(project_id={self._project.id}, name={self.name!r})"
