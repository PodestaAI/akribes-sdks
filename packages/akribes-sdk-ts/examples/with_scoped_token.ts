/**
 * Mint a short-lived scoped token (backend side) and use it from a
 * browser-style client (no service token client-side).
 *
 * The same pattern is what Podesta Studio's `/api/token` endpoint does
 * under the hood — `setToken()` rotates the bearer, and the
 * `onHeartbeatStatus` callback fires when a token is revoked so the UI
 * can prompt for re-auth instead of silently warn-looping (#1220).
 *
 * Run with:
 *
 *     bun run examples/with_scoped_token.ts
 */
import { AkribesClient } from "../src";

async function main() {
  const baseUrl = process.env.AKRIBES_BASE_URL ?? "http://localhost:3001";
  const serviceToken = process.env.AKRIBES_SERVICE_TOKEN;
  const projectId = Number(process.env.AKRIBES_PROJECT_ID ?? "1");
  if (!serviceToken) {
    console.error("AKRIBES_SERVICE_TOKEN is required for this example.");
    process.exit(2);
  }

  // ── Backend: mint a scoped token for a particular user.
  const backend = new AkribesClient({ baseUrl, projectId, token: serviceToken });
  const minted = await backend.tokens.mint({
    user_email: "alice@acme.com",
    scopes: { projects: [projectId], role: "editor" },
    expires_in: 8 * 3600,
    label: "web-session-example",
  });
  console.log("minted token:", minted.token.slice(0, 16), "…");

  // ── Browser-style: use only the scoped token.
  const browser = new AkribesClient({
    baseUrl,
    projectId,
    token: minted.token,
    onHeartbeatStatus: (status) => {
      console.log(`[heartbeat] ${status}`);
      if (status === "auth_failed") {
        // In a real app: open a re-auth modal, refresh the token, then
        // browser.setToken(newToken); browser.clients.resumeHeartbeat();
        console.warn("token revoked or expired — would prompt re-login here");
      }
    },
  });

  try {
    const projects = await browser.projects.list();
    console.log(`found ${projects.length} project(s) with the scoped token`);
  } finally {
    browser.destroy();
    backend.destroy();
  }
}

await main();
