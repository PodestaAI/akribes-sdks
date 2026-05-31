"""``/me/*`` resource — caller-introspection endpoints.

Today this only exposes ``GET /me/sandbox``, mirroring the TS SDK's
``client.me.sandbox()`` for parity. The bound :class:`AkribesClient` already has
a ``get_sandbox_project_id()`` helper that returns just the int — this
resource returns the structured :class:`SandboxInfo` so future fields can be
added without breaking the existing call site."""

from __future__ import annotations

from akribes_sdk._parsers import parse_sandbox_info
from akribes_sdk.models import SandboxInfo
from akribes_sdk.resources._base import Resource


class Me(Resource):

    async def sandbox(self) -> SandboxInfo:
        """Return the caller's per-user sandbox project info.

        The server creates the sandbox project lazily on first call. Use this
        to subscribe to ad-hoc events *before* calling
        :meth:`AkribesClient.run_adhoc` so the first engine events aren't missed.
        """
        res = await self._request("GET", f"{self._base_url}/me/sandbox")
        return parse_sandbox_info(res.json())
