from __future__ import annotations

from akribes_sdk._pagination import AsyncPage
from akribes_sdk._parsers import parse_script
from akribes_sdk.errors import NotFoundError
from akribes_sdk.models import Script
from akribes_sdk.resources._base import ProjectResource
from akribes_sdk.resources._sentinel import _MISSING


class Scripts(ProjectResource):

    def list(self) -> AsyncPage[Script]:
        async def fetch(offset: int, limit: int) -> tuple[list[Script], bool]:
            res = await self._request(
                "GET",
                self._project_url("scripts"),
                params={"limit": limit, "offset": offset},
            )
            items = [parse_script(s) for s in res.json()]
            return items, len(items) == limit
        return AsyncPage(fetch)

    async def get(self, script_name: str, *, default=_MISSING) -> Script:
        """Return the script or raise :class:`NotFoundError`.

        Pass ``default=None`` (or any other value) to suppress the error and
        return that value instead — mirroring ``dict.get(key, default)``.
        """
        try:
            res = await self._request("GET", self._project_url("scripts", script_name))
        except NotFoundError:
            if default is _MISSING:
                raise
            return default  # type: ignore[return-value]
        return parse_script(res.json())

    async def create(self, name: str, source: str) -> Script:
        res = await self._request(
            "POST",
            self._project_url("scripts"),
            params={"name": name},
            json={"source": source},
        )
        return parse_script(res.json())

    async def rename(self, old_name: str, new_name: str) -> None:
        await self._request(
            "PATCH",
            self._project_url("scripts", old_name),
            json={"new_name": new_name},
        )

    async def delete(self, script_name: str) -> None:
        await self._request("DELETE", self._project_url("scripts", script_name))

    async def duplicate(self, script_name: str) -> Script:
        """Duplicate a script within the bound project. The server picks a
        copy name (e.g. ``foo copy``) and returns the new script. Per-project
        sugar over :meth:`Projects.duplicate_script`."""
        res = await self._request(
            "POST",
            self._project_url("scripts", script_name, "duplicate"),
            json={},
        )
        return parse_script(res.json())

    async def move_to(self, script_name: str, target_project_id: int) -> Script:
        """Move a script to another project. Returns the moved script (now
        scoped to the target project). Per-project sugar over
        :meth:`Projects.move_script`."""
        res = await self._request(
            "POST",
            self._project_url("scripts", script_name, "move"),
            json={"target_project_id": target_project_id},
        )
        return parse_script(res.json())

    async def reorder(self, order: list[int]) -> None:
        """Set the sort order of scripts in the bound project. ``order`` is
        the list of script IDs in the desired order. Per-project sugar over
        :meth:`Projects.reorder_scripts` (Rust/TS only — Python exposes only
        this per-project form)."""
        await self._request(
            "PUT",
            self._project_url("scripts", "reorder"),
            json={"order": order},
        )
