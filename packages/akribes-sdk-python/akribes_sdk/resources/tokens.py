from __future__ import annotations

from datetime import timedelta
from typing import Any

from akribes_sdk._pagination import AsyncPage
from akribes_sdk._parsers import parse_token_info, parse_token_minted
from akribes_sdk._timing import to_seconds
from akribes_sdk.models import TokenInfo, TokenMinted, TokenScopes
from akribes_sdk.resources._base import Resource


class Tokens(Resource):
    """Scoped token management.

    The auth model in one paragraph: akribes-server has two token types.
    Service tokens live in env vars (``AKRIBES_SERVICE_TOKEN_<NAME>=<scope>:<secret>``)
    and never expire -- your backend uses one to talk to akribes-server. Scoped
    tokens are minted at runtime via this client and stored in the DB. They
    expire (max 90 days), can be revoked, and are what you hand out to
    browsers / end-users / CLIs.
    """

    def _url(self, *parts: str) -> str:
        base = f"{self._base_url}/tokens"
        if parts:
            return base + "/" + "/".join(parts)
        return base

    async def mint(
        self,
        scopes: TokenScopes | dict[str, Any],
        expires_in: timedelta | int,
        label: str,
        *,
        user_email: str | None = None,
    ) -> TokenMinted:
        """Mint a new scoped token. The raw token is only returned once.

        ``scopes`` is a :class:`TokenScopes` dataclass or an equivalent dict,
        e.g. ``TokenScopes(projects="*", role="admin")``.

        ``expires_in`` is the token lifetime as a :class:`datetime.timedelta`
        or integer seconds. Server-enforced max is 90 days
        (``90 * 24 * 3600 = 7_776_000``). Use ``timedelta(hours=8)`` for
        browser sessions and ``timedelta(days=90)`` for long-lived CLI/PAT tokens.

        ``user_email`` is optional but strongly recommended for end-user
        tokens so you can later bulk-revoke via :meth:`revoke_by_email` for
        offboarding.
        """
        scopes_dict = scopes.to_dict() if isinstance(scopes, TokenScopes) else scopes
        body: dict[str, Any] = {
            "scopes": scopes_dict,
            "expires_in": int(to_seconds(expires_in)),
            "label": label,
        }
        if user_email is not None:
            body["user_email"] = user_email
        res = await self._request("POST", self._url(), json=body)
        return parse_token_minted(res.json())

    def list(self) -> AsyncPage[TokenInfo]:
        """List tokens. Service tokens see all; scoped tokens see only their own."""
        async def fetch(offset: int, limit: int) -> tuple[list[TokenInfo], bool]:
            res = await self._request(
                "GET",
                self._url(),
                params={"limit": limit, "offset": offset},
            )
            items = [parse_token_info(t) for t in res.json()]
            return items, len(items) == limit
        return AsyncPage(fetch)

    async def revoke(self, token_id: str) -> None:
        """Revoke a token by ID. Raises :class:`NotFoundError` if not found."""
        await self._request("DELETE", self._url(token_id))

    async def revoke_by_email(self, email: str) -> int:
        """Revoke all tokens for a given user email (offboarding).

        Only service tokens may call this. Returns the number of tokens
        that were revoked.
        """
        res = await self._request("DELETE", self._url(), params={"user_email": email})
        return int(res.json().get("revoked", 0))
