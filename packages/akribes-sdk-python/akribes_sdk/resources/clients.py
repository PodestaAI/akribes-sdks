from __future__ import annotations

from urllib.parse import quote

from akribes_sdk._pagination import AsyncPage
from akribes_sdk.models import ClientInfo, ContractLockInfo
from akribes_sdk.resources._base import Resource, ProjectResource


class ClientsByID(Resource):
    """Global by-ID ops on clients. Mounted on AkribesClient.clients."""

    async def delete(self, client_id: str) -> None:
        await self._request("DELETE", f"{self._base_url}/clients/{client_id}")


class ClientsProjectScoped(ProjectResource):
    """Project-scoped client ops. Mounted on ProjectHandle.clients."""

    def list(self) -> AsyncPage[ClientInfo]:
        async def fetch(offset: int, limit: int) -> tuple[list[ClientInfo], bool]:
            res = await self._request(
                "GET",
                self._project_url("clients"),
                params={"limit": limit, "offset": offset},
            )
            items = [ClientInfo(**c) for c in res.json()]
            return items, len(items) == limit
        return AsyncPage(fetch)

    # ── Lock management ─────────────────────────────────────────────────

    def list_locks(self, script_name: str) -> AsyncPage[ContractLockInfo]:
        url = f"{self._project_url('scripts')}/{quote(script_name, safe='')}/locks"

        async def fetch(offset: int, limit: int) -> tuple[list[ContractLockInfo], bool]:
            res = await self._request(
                "GET",
                url,
                params={"limit": limit, "offset": offset},
            )
            items = [ContractLockInfo(**row) for row in res.json()]
            return items, len(items) == limit

        return AsyncPage(fetch)

    async def revoke_lock(self, script_name: str, lock_id: int) -> None:
        url = f"{self._project_url('scripts')}/{quote(script_name, safe='')}/locks/{lock_id}"
        await self._request("DELETE", url)

    async def rebind_lock(
        self, script_name: str, lock_id: int, version_id: int | None = None
    ) -> ContractLockInfo:
        url = f"{self._project_url('scripts')}/{quote(script_name, safe='')}/locks/{lock_id}/rebind"
        res = await self._request("PATCH", url, json={"version_id": version_id})
        return ContractLockInfo(**res.json())

    # Parity aliases — match the names used in the parity matrix (#342). The
    # TS SDK calls these ``revokeLock``/``rebindLock``; Python keeps both
    # naming conventions so callers can pick whichever reads better.

    async def delete_lock(self, script_name: str, lock_id: int) -> None:
        """Alias for :meth:`revoke_lock` — matches the parity-matrix naming."""
        await self.revoke_lock(script_name, lock_id)

    async def update_lock(
        self, script_name: str, lock_id: int, *, version_id: int | None = None
    ) -> ContractLockInfo:
        """Alias for :meth:`rebind_lock` — matches the parity-matrix naming.

        Pass ``version_id=None`` to clear the lock's pinned version (lets it
        track the latest published version)."""
        return await self.rebind_lock(script_name, lock_id, version_id)


# Backward-compat alias: keep the old name pointing at the project-scoped class.
# Tests that construct Clients(project_api) directly (if any) will still work.
Clients = ClientsProjectScoped
