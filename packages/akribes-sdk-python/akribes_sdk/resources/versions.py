from __future__ import annotations

from akribes_sdk._pagination import AsyncPage
from akribes_sdk._parsers import parse_breaking_interest, parse_latest_version
from akribes_sdk._timing import as_list
from akribes_sdk.errors import NotFoundError
from akribes_sdk.models import LatestVersion, PublishDryRunResult, ScriptVersion
from akribes_sdk.resources._base import ProjectResource
from akribes_sdk.resources._sentinel import _MISSING


class Versions(ProjectResource):

    def list(self, script_name: str) -> AsyncPage[ScriptVersion]:
        async def fetch(offset: int, limit: int) -> tuple[list[ScriptVersion], bool]:
            res = await self._request(
                "GET",
                self._project_url("scripts", script_name, "versions"),
                params={"limit": limit, "offset": offset},
            )
            items = [ScriptVersion(**v) for v in res.json()]
            return items, len(items) == limit
        return AsyncPage(fetch)

    async def get(self, script_name: str, version_id: int, *, default=_MISSING) -> ScriptVersion:
        """Return the script version or raise :class:`NotFoundError`.

        Pass ``default=None`` (or any other value) to suppress the error and
        return that value instead — mirroring ``dict.get(key, default)``.
        """
        try:
            res = await self._request(
                "GET",
                self._project_url("scripts", script_name, "versions", str(version_id)),
            )
        except NotFoundError:
            if default is _MISSING:
                raise
            return default  # type: ignore[return-value]
        return ScriptVersion(**res.json())

    async def get_latest(self, script_name: str, *, default=_MISSING) -> LatestVersion:
        """Return the latest published version or raise :class:`NotFoundError`.

        Pass ``default=None`` (or any other value) to suppress the error and
        return that value instead — mirroring ``dict.get(key, default)``.
        """
        try:
            res = await self._request("GET", self._project_url("scripts", script_name, "latest"))
        except NotFoundError:
            if default is _MISSING:
                raise
            return default  # type: ignore[return-value]
        data = res.json()
        return parse_latest_version(data)

    async def publish(
        self,
        script_name: str,
        channels: str | list[str],
        *,
        label: str | None = None,
        published_by: str | None = None,
        force: bool = False,
        dry_run: bool = False,
    ) -> ScriptVersion | PublishDryRunResult:
        channels = as_list(channels)  # type: ignore[assignment]
        res = await self._request(
            "POST",
            self._project_url("scripts", script_name, "publish"),
            json={
                "label": label,
                "channels": channels,
                "published_by": published_by or self._api.name,
                "force": force,
                "dry_run": dry_run,
            },
        )
        data = res.json()
        if data.get("dry_run"):
            return PublishDryRunResult(
                dry_run=True,
                would_break=data["would_break"],
                breaking_interests=[
                    parse_breaking_interest(b) for b in data.get("breaking_interests", [])
                ],
            )
        return ScriptVersion(**data["version"])
