"""Auto-paginating async iterable for SDK list endpoints."""
from __future__ import annotations

from typing import AsyncIterator, Awaitable, Callable, Generic, TypeVar

T = TypeVar("T")

_DEFAULT_PAGE_SIZE = 200


class AsyncPage(Generic[T]):
    """Async-iterable wrapper around an offset/limit-paginated endpoint.

    Construct with a ``fetch_page(offset, limit) -> (items, has_more)``
    coroutine.  The wrapper drives the pagination internally; callers iterate
    or materialise as they please.

    Example::

        async for s in proj.scripts.list():
            ...
        scripts = await proj.scripts.list().to_list()
        first = await proj.scripts.list().first()
    """

    __slots__ = ("_fetch", "_page_size")

    def __init__(
        self,
        fetch_page: Callable[[int, int], Awaitable[tuple[list[T], bool]]],
        *,
        page_size: int = _DEFAULT_PAGE_SIZE,
    ) -> None:
        self._fetch = fetch_page
        self._page_size = page_size

    def __aiter__(self) -> AsyncIterator[T]:
        return self._iter()

    async def _iter(self) -> AsyncIterator[T]:
        offset = 0
        while True:
            items, has_more = await self._fetch(offset, self._page_size)
            for item in items:
                yield item
            if not has_more or not items:
                return
            offset += len(items)

    async def to_list(self) -> list[T]:
        """Drain the stream into a list."""
        return [x async for x in self]

    async def first(self) -> T | None:
        """Return the first item, or None if empty.

        Short-circuits — fetches only the first page.
        """
        async for x in self:
            return x
        return None

    async def take(self, n: int) -> list[T]:
        """Return the first *n* items (or fewer if the stream ends earlier)."""
        out: list[T] = []
        async for x in self:
            out.append(x)
            if len(out) >= n:
                return out
        return out
