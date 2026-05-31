/**
 * Stream events from a running workflow.
 *
 * Demonstrates three styles:
 *
 *   1. `run.on.output(cb)` — callback for every streaming agent chunk.
 *   2. `for await (const evt of run)` — typed event iteration.
 *   3. `await run.output()` — block until the final result.
 *
 * Run with:
 *
 *     bun run examples/run_stream.ts
 *
 * The SDK subscribes to the SSE stream BEFORE issuing the POST so no
 * opening events are missed — see `AkribesClient.runStream`.
 */
import { AkribesClient } from "../src";

async function main() {
  const baseUrl = process.env.AKRIBES_BASE_URL ?? "http://localhost:3001";
  const token =
    process.env.AKRIBES_TOKEN ?? process.env.AKRIBES_SERVICE_TOKEN;
  const projectId = Number(process.env.AKRIBES_PROJECT_ID ?? "1");
  const scriptName = process.env.AKRIBES_SCRIPT ?? "summarize";

  const client = new AkribesClient({ baseUrl, projectId, token });
  try {
    const run = client.executions.runStream(scriptName, {
      inputs: { brief: "hello" },
    });

    // Print every agent chunk as it arrives.
    run.on.output((chunk) => process.stdout.write(chunk.chunk));
    run.on.error((err) => console.error("\n[error]", err.message));

    // Iterate the typed event stream too.
    for await (const evt of run) {
      if (evt.kind === "task_end") {
        console.log(`\n[task done: ${evt.task}]`);
      }
    }

    const result = await run.output();
    console.log("\nFinal:", result);
  } finally {
    client.destroy();
  }
}

await main();
