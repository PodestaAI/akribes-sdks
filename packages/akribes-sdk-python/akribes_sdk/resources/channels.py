from __future__ import annotations

from typing import Any

from akribes_sdk._pagination import AsyncPage
from akribes_sdk._parsers import parse_script_channel
from akribes_sdk.models import ScriptChannel
from akribes_sdk.resources._base import ProjectResource


class Channels(ProjectResource):

    def list(self, script_name: str) -> AsyncPage[ScriptChannel]:
        async def fetch(offset: int, limit: int) -> tuple[list[ScriptChannel], bool]:
            res = await self._request(
                "GET",
                self._project_url("scripts", script_name, "channels"),
                params={"limit": limit, "offset": offset},
            )
            items = [parse_script_channel(c) for c in res.json()]
            return items, len(items) == limit
        return AsyncPage(fetch)

    async def create(self, script_name: str, channel_name: str) -> ScriptChannel:
        res = await self._request(
            "POST",
            self._project_url("scripts", script_name, "channels"),
            json={"name": channel_name},
        )
        return parse_script_channel(res.json())

    async def delete(self, script_name: str, channel_name: str) -> None:
        await self._request(
            "DELETE",
            self._project_url("scripts", script_name, "channels", channel_name),
        )

    async def move(self, script_name: str, channel_name: str, version_id: int, *, force: bool = False) -> None:
        body: dict[str, Any] = {"version_id": version_id}
        if force:
            body["force"] = True
        await self._request(
            "PATCH",
            self._project_url("scripts", script_name, "channels", channel_name),
            json=body,
        )
