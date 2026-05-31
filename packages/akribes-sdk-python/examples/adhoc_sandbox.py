"""Run raw .akr source against the caller's sandbox project."""
from __future__ import annotations

import asyncio
import os

from akribes_sdk import AkribesClient


SCRIPT = """
input brief: String
agent summarizer = openai:gpt-4o-mini
task summarize -> String {
    agent: summarizer
    prompt: brief
}
workflow {
    result := summarize
}
"""


async def main() -> None:
    base_url = os.environ.get("AKRIBES_BASE_URL", "http://localhost:3001")
    token = os.environ.get("AKRIBES_TOKEN") or os.environ.get("AKRIBES_SERVICE_TOKEN")

    async with AkribesClient(base_url, token=token) as client:
        sandbox = await client.sandbox()
        result = await sandbox.run_source(SCRIPT, brief="explain quantum entanglement to a 7yo")
        print(f"execution_id: {result.execution_id}")
        print(f"result:       {result.result}")


if __name__ == "__main__":
    asyncio.run(main())
