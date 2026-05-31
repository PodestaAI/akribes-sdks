"""Run a workflow with OpenTelemetry tracing.

Install with:
    pip install 'akribes-sdk[otel]'

Or pass a Tracer instance directly if you've already configured one.
"""
from __future__ import annotations

import asyncio
import os

from akribes_sdk import AkribesClient

# Application-side setup: configure a tracer provider + exporter exactly once.
# (This is application boilerplate, not SDK code; shown here for completeness.)
from opentelemetry import trace
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import ConsoleSpanExporter, BatchSpanProcessor

provider = TracerProvider()
provider.add_span_processor(BatchSpanProcessor(ConsoleSpanExporter()))
trace.set_tracer_provider(provider)


async def main() -> None:
    base_url = os.environ.get("AKRIBES_BASE_URL", "http://localhost:3001")
    token = os.environ.get("AKRIBES_TOKEN") or os.environ.get("AKRIBES_SERVICE_TOKEN")
    project_id = int(os.environ.get("AKRIBES_PROJECT_ID", "1"))
    script_name = os.environ.get("AKRIBES_SCRIPT", "summarize")

    # otel=True — the SDK auto-instruments every HTTP request and execution
    # with spans, and propagates trace context to the server via traceparent.
    async with AkribesClient(base_url, token=token, otel=True) as client:
        proj = client.project(project_id)
        output = await proj.script(script_name).run_and_await(brief="hi")
        print(f"execution_id: {output.execution_id}")
        print(f"result:       {output.result!r}")


if __name__ == "__main__":
    asyncio.run(main())
