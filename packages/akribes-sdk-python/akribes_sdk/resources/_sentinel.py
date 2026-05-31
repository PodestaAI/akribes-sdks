"""Shared sentinel for the ``default=`` opt-in pattern on ``.get()`` methods.

Usage::

    from akribes_sdk.resources._sentinel import _MISSING

    async def get(self, id: int, *, default=_MISSING) -> T:
        try:
            res = await self._request("GET", ...)
        except NotFoundError:
            if default is _MISSING:
                raise
            return default
        return parse_t(res.json())

Pass ``default=None`` (or any other value) to opt into the nullable form.
The sentinel is intentionally distinct from ``None`` so callers can use
``default=None`` as an explicit fallback.
"""

from __future__ import annotations

_MISSING = object()
