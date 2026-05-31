from __future__ import annotations

from urllib.parse import quote

from akribes_sdk._pagination import AsyncPage
from akribes_sdk._parsers import parse_project, parse_script
from akribes_sdk.errors import NotFoundError
from akribes_sdk.models import Project, Script, ScriptChannel
from akribes_sdk.resources._base import Resource
from akribes_sdk.resources._sentinel import _MISSING


class Projects(Resource):

    def list(self) -> AsyncPage[Project]:
        async def fetch(offset: int, limit: int) -> tuple[list[Project], bool]:
            res = await self._request(
                "GET",
                f"{self._base_url}/projects",
                params={"limit": limit, "offset": offset},
            )
            items = [parse_project(p) for p in res.json()]
            return items, len(items) == limit
        return AsyncPage(fetch)

    async def get(self, project_id: int, *, default=_MISSING) -> Project:
        """Return the project or raise :class:`NotFoundError`.

        Pass ``default=None`` (or any other value) to suppress the error and
        return that value instead — mirroring ``dict.get(key, default)``.
        """
        try:
            res = await self._request("GET", f"{self._base_url}/projects/{project_id}")
        except NotFoundError:
            if default is _MISSING:
                raise
            return default  # type: ignore[return-value]
        return parse_project(res.json())

    async def create(self, name: str) -> Project:
        res = await self._request("POST", f"{self._base_url}/projects", json={"name": name})
        return parse_project(res.json())

    async def update(self, project_id: int, name: str) -> Project:
        res = await self._request("PATCH", f"{self._base_url}/projects/{project_id}", json={"name": name})
        return parse_project(res.json())

    async def delete(self, project_id: int) -> None:
        await self._request("DELETE", f"{self._base_url}/projects/{project_id}")

    async def duplicate(self, project_id: int) -> Project:
        """Duplicate a project (including all scripts). Server picks the copy
        name. Requires a wildcard-scoped identity."""
        res = await self._request(
            "POST", f"{self._base_url}/projects/{project_id}/duplicate", json={}
        )
        return parse_project(res.json())

    async def reorder(self, order: list[int]) -> None:
        """Set the global project ordering. ``order`` is the list of project
        IDs in the desired order. Requires a wildcard-scoped identity."""
        await self._request(
            "PUT", f"{self._base_url}/projects/reorder", json={"order": order}
        )

    # ── Flat cross-project script ops ───────────────────────────────────
    #
    # The project-scoped chain (``client.scripts.X``) is the primary surface
    # for script management, but admin-style code that touches several
    # projects in a row reads more naturally with flat, cross-project ops on
    # ``projects`` itself — same as Rust ``projects().X_script(...)`` and
    # TypeScript ``projects.XScript(...)``.

    def _script_url(self, project_id: int, script_name: str, *segments: str) -> str:
        encoded = quote(script_name, safe="")
        url = f"{self._base_url}/projects/{project_id}/scripts/{encoded}"
        for s in segments:
            url = f"{url}/{s}"
        return url

    async def list_scripts(self, project_id: int) -> list[Script]:
        """List scripts in a specific project. Flat alternative to
        ``client.scripts.list()`` (which uses the client's bound project)."""
        res = await self._request(
            "GET", f"{self._base_url}/projects/{project_id}/scripts"
        )
        return [parse_script(s) for s in res.json()]

    async def move_script(
        self,
        project_id: int,
        script_name: str,
        target_project_id: int,
    ) -> Script:
        """Move a script from ``project_id`` to ``target_project_id``."""
        res = await self._request(
            "POST",
            self._script_url(project_id, script_name, "move"),
            json={"target_project_id": target_project_id},
        )
        return parse_script(res.json())

    async def rename_script(
        self,
        project_id: int,
        current_name: str,
        new_name: str,
    ) -> None:
        """Rename a script in ``project_id``."""
        await self._request(
            "PATCH",
            self._script_url(project_id, current_name),
            json={"new_name": new_name},
        )

    async def delete_script(self, project_id: int, script_name: str) -> None:
        """Delete a script in ``project_id``."""
        await self._request("DELETE", self._script_url(project_id, script_name))

    async def duplicate_script(
        self,
        project_id: int,
        script_name: str,
    ) -> Script:
        """Duplicate a script within ``project_id``. The server picks the
        copy name."""
        res = await self._request(
            "POST",
            self._script_url(project_id, script_name, "duplicate"),
            json={},
        )
        return parse_script(res.json())

    async def list_channels(
        self,
        project_id: int,
        script_name: str,
    ) -> list[ScriptChannel]:
        """List channels for ``script_name`` in ``project_id`` (#1141).

        Flat cross-project alternative to
        ``client.channels.list(script_name)`` — mirrors Rust
        ``projects.list_channels`` and TS ``projects.listChannels``."""
        from akribes_sdk._parsers import parse_script_channel
        res = await self._request(
            "GET",
            self._script_url(project_id, script_name, "channels"),
        )
        return [parse_script_channel(c) for c in res.json()]

    async def reorder_scripts(
        self,
        project_id: int,
        order: list[int],
    ) -> None:
        """Reorder scripts within ``project_id`` (#1141).

        ``order`` is the list of script IDs in the desired order. Flat
        cross-project alternative to ``client.scripts.reorder(order)``."""
        await self._request(
            "PUT",
            f"{self._base_url}/projects/{project_id}/scripts/reorder",
            json={"order": order},
        )
