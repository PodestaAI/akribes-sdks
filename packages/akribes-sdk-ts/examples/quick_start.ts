/**
 * Quick-start: run a script and wait for its output.
 *
 * Set `AKRIBES_BASE_URL` and `AKRIBES_TOKEN` (or `AKRIBES_SERVICE_TOKEN`)
 * in your shell before running:
 *
 *     bun run examples/quick_start.ts
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
    // `runAndAwait` POSTs /run + polls /output until the execution is
    // terminal, returning [id, output]. For event streaming see
    // `run_stream.ts`.
    const [executionId, output] = await client.executions.runAndAwait(scriptName, {
      inputs: { brief: "hello" },
    });
    console.log("execution_id:", executionId);
    console.log("status:      ", output.status);
    console.log("result:      ", output.result);
  } finally {
    client.destroy();
  }
}

await main();
