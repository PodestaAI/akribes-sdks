"""Upload a document and feed it into a workflow.

Demonstrates:
- ``proj.documents.ingest()`` — returns an :class:`IngestHandle` that
  yields phase + progress events as the conversion progresses.
- ``proj.documents.ingest_and_wait()`` — convenience one-liner.
- Passing the resulting ``document_id`` into a workflow input.

Run with::

    uv run python examples/document_upload.py path/to/file.pdf
"""

from __future__ import annotations

import asyncio
import os
import sys
from pathlib import Path

from akribes_sdk import (
    AkribesClient,
    DocumentConversionError,
    IngestHandle,
    IngestPhase,
    IngestProgress,
    IngestTimeoutError,
)


async def main(path: Path) -> None:
    base_url = os.environ.get("AKRIBES_BASE_URL", "http://localhost:3001")
    token = os.environ.get("AKRIBES_TOKEN") or os.environ.get("AKRIBES_SERVICE_TOKEN")
    project_id = int(os.environ.get("AKRIBES_PROJECT_ID", "1"))
    script_name = os.environ.get("AKRIBES_SCRIPT", "summarize_doc")

    async with AkribesClient(base_url, token=token) as client:
        proj = client.project(project_id)

        handle: IngestHandle = proj.documents.ingest(path)

        try:
            async for evt in handle:
                if isinstance(evt, str):
                    # IngestPhase — a string like "claiming", "uploading", "converting", "ready"
                    print(f"  → {evt}")
                elif isinstance(evt, IngestProgress):
                    if evt.total > 0:
                        print(f"    pages {evt.done}/{evt.total}")
                    else:
                        print("    rasterizing…")

            result = await handle.result()
        except IngestTimeoutError as e:
            # Server is still working — bytes are persisted, retry later.
            print(f"timed out after {e.elapsed_ms}ms (document_id={e.document_id!r})")
            return
        except DocumentConversionError as e:
            # Terminal failure — VLM/Docling rejected the file.
            print(f"conversion failed: {e} (reason={e.reason!r})")
            return

        print(f"\nuploaded as {result.document_id} (status={result.conversion_status})")

        # Feed it into a workflow that takes a `doc` input.
        output = await proj.script(script_name).run_and_await(doc=result.document_id)
        print(f"workflow {output.execution_id} → {output.status}")
        print(output.result)


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("usage: python examples/document_upload.py <file>")
        sys.exit(2)
    asyncio.run(main(Path(sys.argv[1])))
