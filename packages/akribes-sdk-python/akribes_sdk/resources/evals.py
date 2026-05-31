from __future__ import annotations

from typing import Any

from akribes_sdk._pagination import AsyncPage
from akribes_sdk.errors import NotFoundError
from akribes_sdk.models import EvalResult, EvalRun, EvalSuite, EvalSuiteSummary
from akribes_sdk._parsers import (
    parse_eval_suite,
    parse_eval_run,
    parse_eval_result,
    parse_eval_suite_summary,
)
from akribes_sdk.resources._base import Resource, ProjectResource
from akribes_sdk.resources._sentinel import _MISSING


class EvalsByID(Resource):
    """Global by-ID ops on eval runs. Mounted on AkribesClient.evals."""

    async def cancel_run(self, run_id: int) -> EvalRun:
        """Cancel an eval run. Raises :class:`NotFoundError` if not found."""
        res = await self._request("DELETE", f"{self._base_url}/eval-runs/{run_id}")
        return parse_eval_run(res.json())

    async def get_run(self, run_id: int, *, default=_MISSING) -> EvalRun:
        """Return the eval run or raise :class:`NotFoundError`.

        Pass ``default=None`` (or any other value) to suppress the error and
        return that value instead — mirroring ``dict.get(key, default)``.
        """
        try:
            res = await self._request("GET", f"{self._base_url}/eval-runs/{run_id}")
        except NotFoundError:
            if default is _MISSING:
                raise
            return default  # type: ignore[return-value]
        return parse_eval_run(res.json())

    async def get_results(self, run_id: int) -> list[EvalResult]:
        res = await self._request("GET", f"{self._base_url}/eval-runs/{run_id}/results")
        return [parse_eval_result(r) for r in res.json()]


class Evals(ProjectResource):
    """Project-scoped eval suite ops. Mounted on ProjectHandle.evals."""

    def list_suites(self, script_name: str) -> AsyncPage[EvalSuite]:
        async def fetch(offset: int, limit: int) -> tuple[list[EvalSuite], bool]:
            res = await self._request(
                "GET",
                self._project_url("scripts", script_name, "eval-suites"),
                params={"limit": limit, "offset": offset},
            )
            items = [parse_eval_suite(s) for s in res.json()]
            return items, len(items) == limit
        return AsyncPage(fetch)

    async def create_suite(
        self,
        script_name: str,
        name: str,
        runner_url: str,
        config: dict[str, Any] | None = None,
        auto_run_channels: list[str] | None = None,
    ) -> EvalSuite:
        body: dict[str, Any] = {"name": name, "runner_url": runner_url}
        if config is not None:
            body["config"] = config
        if auto_run_channels is not None:
            body["auto_run_channels"] = auto_run_channels
        res = await self._request(
            "POST",
            self._project_url("scripts", script_name, "eval-suites"),
            json=body,
        )
        return parse_eval_suite(res.json())

    async def get_suite(self, script_name: str, suite_id: int, *, default=_MISSING) -> EvalSuite:
        """Return the eval suite or raise :class:`NotFoundError`.

        Pass ``default=None`` (or any other value) to suppress the error and
        return that value instead — mirroring ``dict.get(key, default)``.
        """
        try:
            res = await self._request(
                "GET",
                self._project_url("scripts", script_name, "eval-suites", str(suite_id)),
            )
        except NotFoundError:
            if default is _MISSING:
                raise
            return default  # type: ignore[return-value]
        return parse_eval_suite(res.json())

    async def update_suite(
        self,
        script_name: str,
        suite_id: int,
        *,
        runner_url: str | None = None,
        config: dict[str, Any] | None = None,
        auto_run_channels: list[str] | None = None,
    ) -> EvalSuite:
        body: dict[str, Any] = {}
        if runner_url is not None:
            body["runner_url"] = runner_url
        if config is not None:
            body["config"] = config
        if auto_run_channels is not None:
            body["auto_run_channels"] = auto_run_channels
        res = await self._request(
            "PATCH",
            self._project_url("scripts", script_name, "eval-suites", str(suite_id)),
            json=body,
        )
        return parse_eval_suite(res.json())

    async def delete_suite(self, script_name: str, suite_id: int) -> None:
        await self._request(
            "DELETE",
            self._project_url("scripts", script_name, "eval-suites", str(suite_id)),
        )

    async def check_runner_health(
        self,
        script_name: str,
        suite_id: int,
    ) -> dict[str, Any]:
        """Check the health of a suite's eval runner."""
        res = await self._request(
            "GET",
            self._project_url("scripts", script_name, "eval-suites", str(suite_id), "health"),
        )
        return res.json()

    async def trigger(
        self,
        script_name: str,
        suite_id: int,
        *,
        source: str | None = None,
        channel: str | None = None,
        auto_publish: bool = False,
        triggered_by: str | None = None,
    ) -> EvalRun:
        body: dict[str, Any] = {}
        if source is not None:
            body["source"] = source
        if channel is not None:
            body["channel"] = channel
        if auto_publish:
            body["auto_publish"] = True
        if triggered_by is not None:
            body["triggered_by"] = triggered_by
        res = await self._request(
            "POST",
            self._project_url("scripts", script_name, "eval-suites", str(suite_id), "trigger"),
            json=body,
        )
        return parse_eval_run(res.json())

    # Keep old alias for backward compat within this module (project half uses 'cancel' for run cancel
    # in the old mixed class; now it's on EvalsByID). Keep it here forwarding to EvalsByID
    # via the global _base_url for any code that still calls it through project evals.
    async def cancel(self, run_id: int) -> EvalRun:
        res = await self._request("DELETE", f"{self._base_url}/eval-runs/{run_id}")
        return parse_eval_run(res.json())

    def list_runs(
        self,
        script_name: str,
        *,
        suite_id: int | None = None,
    ) -> AsyncPage[EvalRun]:
        filter_params: dict[str, Any] = {}
        if suite_id is not None:
            filter_params["suite_id"] = suite_id

        async def fetch(offset: int, limit: int) -> tuple[list[EvalRun], bool]:
            params: dict[str, Any] = {"limit": limit, "offset": offset}
            params.update(filter_params)
            res = await self._request(
                "GET",
                self._project_url("scripts", script_name, "eval-runs"),
                params=params,
            )
            items = [parse_eval_run(r) for r in res.json()]
            return items, len(items) == limit

        return AsyncPage(fetch)

    async def get_run(self, run_id: int, *, default=_MISSING) -> EvalRun:
        """Return the eval run or raise :class:`NotFoundError`.

        Pass ``default=None`` (or any other value) to suppress the error and
        return that value instead — mirroring ``dict.get(key, default)``.
        """
        try:
            res = await self._request("GET", f"{self._base_url}/eval-runs/{run_id}")
        except NotFoundError:
            if default is _MISSING:
                raise
            return default  # type: ignore[return-value]
        return parse_eval_run(res.json())

    async def get_results(self, run_id: int) -> list[EvalResult]:
        res = await self._request("GET", f"{self._base_url}/eval-runs/{run_id}/results")
        return [parse_eval_result(r) for r in res.json()]

    # ── Project-level cross-script dashboard (sub-spec 1a) ──────────────────

    def list_project_summaries(self) -> AsyncPage[EvalSuiteSummary]:
        """Return one summary row per suite in the configured project.

        See sub-spec 1a — drives the Studio cross-script dashboard.
        """
        async def fetch(offset: int, limit: int) -> tuple[list[EvalSuiteSummary], bool]:
            res = await self._request(
                "GET",
                self._project_url("eval-suite-summaries"),
                params={"limit": limit, "offset": offset},
            )
            items = [parse_eval_suite_summary(s) for s in res.json()]
            return items, len(items) == limit
        return AsyncPage(fetch)
