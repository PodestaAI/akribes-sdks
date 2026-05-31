"""Document ingest resource.

Mirrors the TypeScript SDK's ``DocumentsClient`` (claim / upload / ingest).
Use :meth:`DocumentsClient.ingest` for the full hash-first flow with progress
callbacks; use :meth:`DocumentsClient.upload` directly when you already know
the bytes are uncached.

See ``packages/akribes-sdk-ts/src/sub/documents.ts`` for the canonical shape.
"""

from __future__ import annotations

import asyncio
import hashlib
import os
import time
from datetime import timedelta
from pathlib import Path
from typing import Any, Awaitable, Callable, Coroutine, Union

from akribes_sdk._parsers import parse_claim_hit, parse_upload_result
from akribes_sdk.errors import (
    AkribesConversionError,
    DocumentConversionError,
    IngestProtocolError,
    IngestTimeoutError,
)
from akribes_sdk.models import (
    ClaimHit,
    ClaimMiss,
    ClaimResult,
    IngestPhase,
    IngestProgress,
    UploadResult,
)
from akribes_sdk.resources._base import ProjectResource


# Default poll budget for ``DocumentsClient.ingest``. Multi-page real-world
# PDFs through the VLM conversion path routinely take 1–5 minutes server-side.
# 300 s is the cross-SDK consensus that covers the long tail comfortably while
# still surfacing a real hang inside a typical request budget. Override per
# client (``ingest_poll_timeout_ms`` ctor arg), per call (``poll_timeout_ms``)
# or via env (``AKRIBES_SDK_INGEST_TIMEOUT_SECS``).
DEFAULT_INGEST_POLL_TIMEOUT_MS = 300_000


# Optional callbacks. Sync functions are fine — async ones are awaited. Errors
# inside callbacks are propagated.
PhaseCallback = Callable[[IngestPhase], Union[None, Awaitable[None]]]
ProgressCallback = Callable[[IngestProgress], Union[None, Awaitable[None]]]

# Internal async-only callback types used by _ingest_with_events (IngestHandle adapts these).
AsyncPhaseCallback = Callable[[IngestPhase], Coroutine[Any, Any, None]]
AsyncProgressCallback = Callable[[IngestProgress], Coroutine[Any, Any, None]]


def ingest_poll_timeout_ms_from_env() -> int | None:
    """Read ``AKRIBES_SDK_INGEST_TIMEOUT_SECS`` from the process env.

    Returns ``None`` if unset, zero, or unparseable — caller falls back to the
    next-lower-precedence default."""
    raw = os.environ.get("AKRIBES_SDK_INGEST_TIMEOUT_SECS")
    if not raw or not raw.strip():
        return None
    try:
        n = float(raw)
    except ValueError:
        return None
    if n <= 0:
        return None
    return int(n) * 1000


def _sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _read_payload(file: Path | bytes | str) -> tuple[str, bytes]:
    """Resolve the payload arg into (filename, bytes).

    - ``Path`` / ``str`` paths read from disk; the basename is the filename.
    - Raw ``bytes`` requires the caller to supply a filename via the
      ``filename`` kwarg in the calling method (see :meth:`upload`/``ingest``).

    Returns the filename hint (empty for raw bytes) so callers can fill in.
    """
    if isinstance(file, bytes):
        return ("", file)
    if isinstance(file, (str, Path)):
        path = Path(file)
        return (path.name, path.read_bytes())
    raise TypeError(
        f"file must be a Path, str, or bytes — got {type(file).__name__}"
    )


async def _emit_phase(cb: PhaseCallback | None, phase: IngestPhase) -> None:
    """Fire the ``on_phase`` callback (if any), awaiting if it's async."""
    if cb is None:
        return
    result = cb(phase)
    if asyncio.iscoroutine(result):
        await result


async def _emit_progress(cb: ProgressCallback | None, p: IngestProgress) -> None:
    """Fire the ``on_progress`` callback (if any), awaiting if it's async.

    Errors inside callbacks are swallowed — progress reporting must never
    break the upload. A failing callback is a UI issue, not a data issue."""
    if cb is None:
        return
    try:
        result = cb(p)
        if asyncio.iscoroutine(result):
            await result
    except Exception:
        pass


class DocumentsClient(ProjectResource):
    """Resource for hash-first document upload + conversion.

    Three layers of API depending on how much you want to manage yourself:

    - :meth:`claim` — ask the server "do you already have these bytes?"
    - :meth:`upload` — send a single multipart upload and block on conversion.
    - :meth:`ingest` — full claim → upload → poll-until-converted with phase
      and per-page progress callbacks. Use this for UI flows.

    The poll timeout for :meth:`ingest` is resolved at construction time from,
    in order: explicit ``ingest_poll_timeout_ms`` ctor arg, the
    ``AKRIBES_SDK_INGEST_TIMEOUT_SECS`` env var, then
    :data:`DEFAULT_INGEST_POLL_TIMEOUT_MS`. A per-call ``poll_timeout_ms``
    overrides everything for that call.
    """

    def __init__(self, client: Any, default_poll_timeout_ms: int | None = None) -> None:
        super().__init__(client)
        # Resolution order: explicit > env > default. The client's __init__
        # has already done this resolution; we just store what it gives us.
        self._default_poll_timeout_ms = (
            default_poll_timeout_ms
            if default_poll_timeout_ms is not None
            else DEFAULT_INGEST_POLL_TIMEOUT_MS
        )

    @property
    def default_poll_timeout_ms(self) -> int:
        """Resolved poll-timeout default for ``ingest()``. Useful for tests."""
        return self._default_poll_timeout_ms

    async def progress(self, content_hash: str) -> IngestProgress | None:
        """Snapshot the server-side conversion progress for a content hash.

        Returns ``None`` if no in-flight conversion is registered (terminal
        already, or never uploaded). Cheap — a few-byte JSON response off an
        in-memory map."""
        url = self._project_url("documents", "by-hash", content_hash, "progress")
        res = await self._request("GET", url)
        wire = res.json()
        if wire.get("state") == "idle":
            return None
        return IngestProgress(
            done=wire.get("done_pages", wire.get("done", 0)),
            total=wire.get("total_pages", wire.get("total", 0)),
        )

    async def claim(self, content_hash: str, filename: str) -> ClaimResult:
        """Probe the server for cached bytes by SHA-256 hash.

        Returns :class:`ClaimHit` if the server already has the blob in any
        usable state (creating a per-project ref on the way), or
        :class:`ClaimMiss` if the bytes are missing or the existing blob row
        was previously poisoned (caller should follow up with :meth:`upload`)."""
        url = self._project_url("documents", "claim")
        try:
            res = await self._request(
                "POST",
                url,
                json={"content_hash": content_hash, "filename": filename},
            )
        except AkribesConversionError as e:
            raise DocumentConversionError("", str(e), e.reason) from e
        wire = res.json()
        if wire.get("status") == "hit":
            return parse_claim_hit(wire)
        return ClaimMiss()

    async def upload(
        self,
        file: Path | bytes | str,
        *,
        filename: str | None = None,
    ) -> UploadResult:
        """Multipart-upload a single file. Blocks for server-side conversion.

        Pass either a :class:`Path`/``str`` path (the basename becomes the
        filename) or raw ``bytes`` together with an explicit ``filename=``."""
        resolved_name, data = _read_payload(file)
        final_name = filename or resolved_name
        if not final_name:
            raise ValueError(
                "filename is required when 'file' is raw bytes"
            )
        url = self._project_url("documents")
        try:
            res = await self._request(
                "POST",
                url,
                files={"file": (final_name, data)},
            )
        except AkribesConversionError as e:
            raise DocumentConversionError("", str(e), e.reason) from e
        return parse_upload_result(res.json())

    def ingest(
        self,
        file: Path | bytes | str,
        *,
        filename: str | None = None,
        poll_timeout_ms: int | None = None,
    ) -> "IngestHandle":
        """Full claim → upload → poll-until-converted flow with streaming progress.

        Returns an :class:`IngestHandle` — async-iterable over
        :class:`IngestPhase` / :class:`IngestProgress` events with
        ``.result(timeout=)`` for the final :class:`UploadResult`. Mirrors
        :class:`RunStream`.

        Phases emitted: ``claiming`` → ``uploading`` (when miss) →
        ``converting`` → ``ready``. :class:`IngestProgress` objects carry
        ``done`` / ``total`` page counts while the server converts.

        Example::

            handle = proj.documents.ingest(Path("report.pdf"))
            async for evt in handle:
                if isinstance(evt, IngestProgress):
                    print(f"pages {evt.done}/{evt.total}")
            result = await handle.result()

        Raises :class:`IngestTimeoutError` on poll deadline (via
        ``.result(timeout=...)``), or :class:`DocumentConversionError` if
        the server reports a terminal ``failed`` status.

        For one-liner callers use :meth:`ingest_and_wait`."""
        from akribes_sdk.ingest_handle import IngestHandle

        # Stash poll_timeout_ms override onto a thin wrapper so _ingest_with_events
        # can pick it up. We resolve once on the handle's DocumentsClient.
        docs = self
        if poll_timeout_ms is not None:
            # Temporarily override the default for this handle only.
            import copy
            docs = copy.copy(self)
            docs._default_poll_timeout_ms = poll_timeout_ms

        if filename is not None:
            # Wrap bytes/str with filename hint by materialising as a named source.
            if isinstance(file, bytes):
                # IngestHandle passes source to _ingest_with_events which calls
                # _read_payload; we need filename to propagate. Package it as a
                # (file, filename) tuple that _ingest_with_events understands.
                return IngestHandle(docs, (file, filename))  # type: ignore[arg-type]
        return IngestHandle(docs, file)

    async def ingest_and_wait(
        self,
        file: Path | bytes | str,
        *,
        filename: str | None = None,
        poll_timeout_ms: int | None = None,
        timeout: "timedelta | float | None" = None,
    ) -> UploadResult:
        """One-liner: ingest a document and return the final :class:`UploadResult`.

        Equivalent to ``await proj.documents.ingest(file).result(timeout=timeout)``.

        Example::

            result = await proj.documents.ingest_and_wait(Path("doc.pdf"))
        """
        handle = self.ingest(file, filename=filename, poll_timeout_ms=poll_timeout_ms)
        return await handle.result(timeout=timeout)

    async def _ingest_with_events(
        self,
        source: "Path | bytes | str | tuple[bytes, str]",
        on_phase: "AsyncPhaseCallback",
        on_progress: "AsyncProgressCallback",
        *,
        set_document_id: "Callable[[str], None]",
    ) -> UploadResult:
        """Internal: full claim → upload → poll flow driving async callbacks.

        Used by :class:`IngestHandle` to decouple the wire protocol from the
        queue-based consumer interface. Callback args are always async functions
        (IngestHandle wraps them with queue.put).
        """
        # Resolve (filename, data) from the various input forms.
        if isinstance(source, tuple):
            # (bytes, filename) packaged by ingest() when filename was explicit.
            data, final_name = source[0], source[1]
        else:
            resolved_name, data = _read_payload(source)  # type: ignore[arg-type]
            final_name = resolved_name

        if not final_name:
            raise ValueError("filename is required when 'file' is raw bytes")

        deadline_ms = self._default_poll_timeout_ms

        await on_phase("claiming")

        content_hash = _sha256_hex(data)
        initial = await self.claim(content_hash, final_name)

        # Populate document_id as early as possible.
        if isinstance(initial, ClaimHit):
            set_document_id(initial.result.document_id)

        # Side-channel progress poller.
        progress_task: asyncio.Task[None] | None = None
        stop_event = asyncio.Event()

        async def _poll_progress() -> None:
            seen_once = False
            while not stop_event.is_set():
                try:
                    snap = await self.progress(content_hash)
                except Exception:
                    snap = None
                if snap is not None:
                    seen_once = True
                    await on_progress(snap)
                elif seen_once:
                    return
                try:
                    await asyncio.wait_for(stop_event.wait(), timeout=0.75)
                except asyncio.TimeoutError:
                    pass

        def _start_poller() -> None:
            nonlocal progress_task
            if progress_task is None:
                progress_task = asyncio.create_task(_poll_progress())

        try:
            if isinstance(initial, ClaimMiss):
                await on_phase("uploading")
                _start_poller()
                result = await self.upload(data, filename=final_name)
                set_document_id(result.document_id)
            else:
                _assert_known_status(initial.result.conversion_status)
                _ensure_not_failed(initial.result)
                result = initial.result

            # Poll until terminal if the server is still working.
            if result.conversion_status in ("converting", "pending"):
                await on_phase("converting")
                _start_poller()
                started_at = time.monotonic()
                deadline_at = started_at + deadline_ms / 1000.0
                backoff = 0.25
                while result.conversion_status in ("converting", "pending"):
                    if time.monotonic() >= deadline_at:
                        elapsed_ms = int((time.monotonic() - started_at) * 1000)
                        raise IngestTimeoutError(
                            f"Conversion is taking longer than expected "
                            f"({elapsed_ms // 1000}s). The server is still "
                            f"working — try again in a moment.",
                            result.document_id,
                            elapsed_ms,
                        )
                    await asyncio.sleep(backoff)
                    backoff = min(backoff * 2, 2.0)

                    poll_outcome = await self.claim(content_hash, final_name)
                    if isinstance(poll_outcome, ClaimMiss):
                        await on_phase("uploading")
                        uploaded = await self.upload(data, filename=final_name)
                        set_document_id(uploaded.document_id)
                        _assert_known_status(uploaded.conversion_status)
                        _ensure_not_failed(uploaded)
                        await on_phase("ready")
                        return uploaded
                    _assert_known_status(poll_outcome.result.conversion_status)
                    _ensure_not_failed(poll_outcome.result)
                    result = poll_outcome.result

            _assert_known_status(result.conversion_status)
            _ensure_not_failed(result)
            await on_phase("ready")
            return result
        finally:
            stop_event.set()
            if progress_task is not None:
                try:
                    await progress_task
                except Exception:
                    pass


def _assert_known_status(s: str) -> None:
    """Raise :class:`IngestProtocolError` on schema drift (``unknown``)."""
    if s == "unknown":
        raise IngestProtocolError(
            "received unknown conversion_status from server (schema drift)",
            received_status="unknown",
        )


def _ensure_not_failed(r: UploadResult) -> None:
    """Raise :class:`DocumentConversionError` if the server reports ``failed``."""
    if r.conversion_status == "failed":
        raise DocumentConversionError(
            r.document_id,
            f"document {r.document_id} conversion failed on the server — "
            f"re-upload or call reconvert",
        )
