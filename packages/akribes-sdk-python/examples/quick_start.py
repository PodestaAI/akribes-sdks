"""Run a script and await its output.

Set ``AKRIBES_BASE_URL`` and ``AKRIBES_TOKEN`` (or ``AKRIBES_SERVICE_TOKEN``) in your
shell before running:

    uv run python examples/quick_start.py
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
        # Fluent ScriptHandle: proj.script(name) is shorthand for
        # proj.executions on a particular script. ``run_and_await`` runs +
        # polls until terminal, raising the appropriate typed error on failure.
        output = await proj.script(script_name).run_and_await(brief="hello")
        print(f"execution_id: {output.execution_id}")
        print(f"status:       {output.status}")
        print(f"result:       {output.result!r}")


if __name__ == "__main__":
    asyncio.run(main())
