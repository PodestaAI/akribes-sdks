/**
 * Upload a document and run a workflow against it.
 *
 * `documents.ingest` is hash-deduped server-side: re-uploading the same
 * file on a retry returns the same `documentId` without re-converting.
 *
 * Run with:
 *
 *     bun run examples/document_upload.ts ./contract.pdf
 */
import { readFile } from "node:fs/promises";

import { AkribesClient } from "../src";

async function main() {
  const path = process.argv[2];
  if (!path) {
    console.error("Usage: bun run examples/document_upload.ts <file>");
    process.exit(2);
  }

  const baseUrl = process.env.AKRIBES_BASE_URL ?? "http://localhost:3001";
  const token =
    process.env.AKRIBES_TOKEN ?? process.env.AKRIBES_SERVICE_TOKEN;
  const projectId = Number(process.env.AKRIBES_PROJECT_ID ?? "1");
  const scriptName = process.env.AKRIBES_SCRIPT ?? "extract_clauses";

  const data = await readFile(path);
  const filename = path.split("/").pop() ?? "doc";

  const client = new AkribesClient({ baseUrl, projectId, token });
  try {
    const { document_id } = await client.documents.ingest(filename, data);
    console.log(`ingested as ${document_id}`);

    const [executionId, output] = await client.executions.runAndAwait(
      scriptName,
      { inputs: { doc: document_id } },
    );
    console.log("execution:", executionId);
    console.log("status:   ", output.status);
    console.log("result:   ", output.result);
  } finally {
    client.destroy();
  }
}

await main();
