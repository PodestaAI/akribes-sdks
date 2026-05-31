from __future__ import annotations

from akribes_sdk._parsers import parse_draft, parse_put_draft_response
from akribes_sdk.errors import NotFoundError
from akribes_sdk.models import Draft, PutDraftResponse
from akribes_sdk.resources._base import ProjectResource
from akribes_sdk.resources._sentinel import _MISSING


class Drafts(ProjectResource):

    async def get(self, script_name: str, *, default=_MISSING) -> Draft:
        """Return the script draft or raise :class:`NotFoundError`.

        Pass ``default=None`` (or any other value) to suppress the error and
        return that value instead — mirroring ``dict.get(key, default)``.
        """
        try:
            res = await self._request("GET", self._project_url("scripts", script_name, "draft"))
        except NotFoundError:
            if default is _MISSING:
                raise
            return default  # type: ignore[return-value]
        return parse_draft(res.json())

    async def save(self, script_name: str, source: str) -> PutDraftResponse:
        res = await self._request(
            "PUT",
            self._project_url("scripts", script_name, "draft"),
            json={"source": source},
        )
        return parse_put_draft_response(res.json())
