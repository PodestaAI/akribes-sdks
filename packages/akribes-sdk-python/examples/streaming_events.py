"""Stream events from a running workflow.

Three options are shown — pick whichever fits:

1. ``async for event in run`` — Layer 3 typed events.
2. ``run.on.output(cb)`` — register a callback without awaiting iteration.
3. ``await run.output()`` — block until the run is done and grab the final
   :class:`ExecutionOutput`.

Run with::

    uv run python examples/streaming_events.py
"""

from __future__ import annotations

import asyncio
import os

from akribes_sdk import AkribesClient


async def main() -> None:
    base_url = os.environ.get("AKRIBES_BASE_URL", "http://localhost:3001")
    token = os.environ.get("AKRIBES_TOKEN") or os.environ.get("AKRIBES_SERVICE_TOKEN")
    project_id = int(os.environ.get("AKRIBES_PROJECT_ID", "1"))
    script_name = os.environ.get("AKRIBES_SCRIPT", "summarize")

    async with AkribesClient(base_url, token=token) as client:
        proj = client.project(project_id)
        run = await proj.executions.run_stream(
            script_name, inputs={"brief": "hello"}
        )

        # Callback style: print each agent chunk as it arrives.
        run.on.output(lambda chunk: print(chunk.chunk, end="", flush=True))

        # Iterator style: react to terminal events.
        async for evt in run:
            # Layer 2 events are dataclass variants — match on .kind discriminator.
            match evt.kind:
                case "task_end": print(f"\n[task done: {evt.task!r}]")
                case "error":    print(f"\n[error: {evt.message}]")

        # The handle still exposes the resolved output if you want it.
        output = await run.output()
        print(f"\nFinal status: {output.status}")


if __name__ == "__main__":
    asyncio.run(main())
