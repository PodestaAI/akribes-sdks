"""IngestHandle — async-iterable handle for a document ingest.

Yields IngestPhase / IngestProgress events as the conversion progresses;
.result() awaits the final UploadResult.
"""
from __future__ import annotations

import asyncio
from datetime import timedelta
from pathlib import Path
from typing import TYPE_CHECKING, AsyncIterator, Union

from akribes_sdk._timing import to_seconds
from akribes_sdk.errors import IngestTimeoutError
from akribes_sdk.models import IngestPhase, IngestProgress, UploadResult

if TYPE_CHECKING:
    from akribes_sdk.resources.documents import DocumentsClient

IngestEvent = Union[IngestPhase, IngestProgress]


class IngestHandle:
    """Handle for a single document ingest run.

    Obtain via DocumentsClient.ingest(path). The SDK kicks off the claim →
    upload → poll flow on construction; consumers iterate to watch progress
    and call .result() to await the final UploadResult.

    Example::

        handle = proj.documents.ingest(Path("report.pdf"))
        async for evt in handle:
            if isinstance(evt, IngestProgress):
                print(f"pages {evt.done}/{evt.total}")
        result = await handle.result()
        print(result.document_id)
    """

    def __init__(self, documents: "DocumentsClient", source: Path | bytes | str) -> None:
        self._documents = documents
        self._source = source
        self._queue: asyncio.Queue[IngestEvent | None] = asyncio.Queue()
        self._final: UploadResult | None = None
        self._error: BaseException | None = None
        self._done = asyncio.Event()
        self.document_id: str = ""   # populated once /claim returns
        self._task = asyncio.create_task(self._run())

    async def _run(self) -> None:
        """Background driver: claim → upload → poll. Emits events to the queue."""
        try:
            self._final = await self._documents._ingest_with_events(
                self._source,
                self._emit_phase,
                self._emit_progress,
                set_document_id=self._set_document_id,
            )
        except BaseException as exc:
            self._error = exc
        finally:
            self._done.set()
            await self._queue.put(None)  # sentinel

    async def _emit_phase(self, phase: IngestPhase) -> None:
        await self._queue.put(phase)

    async def _emit_progress(self, progress: IngestProgress) -> None:
        await self._queue.put(progress)

    def _set_document_id(self, document_id: str) -> None:
        self.document_id = document_id

    def __aiter__(self) -> "IngestHandle":
        return self

    async def __anext__(self) -> IngestEvent:
        evt = await self._queue.get()
        if evt is None:
            raise StopAsyncIteration
        return evt

    async def result(self, *, timeout: timedelta | float | None = None) -> UploadResult:
        """Await the final UploadResult, raising IngestTimeoutError on timeout."""
        try:
            if timeout is None:
                await self._done.wait()
            else:
                await asyncio.wait_for(self._done.wait(), timeout=to_seconds(timeout))
        except asyncio.TimeoutError as exc:
            elapsed_s = to_seconds(timeout) if timeout is not None else 0
            raise IngestTimeoutError(
                f"ingest of {self.document_id or 'unclaimed-document'} timed out",
                document_id=self.document_id,
                elapsed_ms=int(elapsed_s * 1000),
            ) from exc
        if self._error is not None:
            raise self._error
        assert self._final is not None
        return self._final

    async def cancel(self) -> None:
        """Cancel the background task. Pending events drop."""
        self._task.cancel()
        try:
            await self._task
        except (asyncio.CancelledError, Exception):
            pass
        self._done.set()
