/**
 * OpenTelemetry propagator example for the Akribes TypeScript SDK.
 *
 * The SDK has **zero OpenTelemetry runtime dependencies** — it only exposes a
 * `propagator` hook that lets the caller inject W3C trace-context headers
 * (`traceparent`, `tracestate`) into every outbound HTTP request. This keeps
 * the SDK bundle small for callers that don't need tracing, and lets tracing
 * users wire their own OTel setup (Node SDK, browser web SDK, custom
 * propagators) without dragging it through ours.
 *
 * The two patterns below cover the common environments. Both rely on
 * `@opentelemetry/api`'s `propagation.inject(context.active(), carrier)` —
 * the carrier is the plain `Record<string, string>` the SDK hands you, and
 * the active context is whatever your tracer set up around the call.
 *
 * Run (after `bun add @opentelemetry/api`):
 *   bun run packages/akribes-sdk-ts/examples/with_otel.ts
 */

import { AkribesClient } from '../src/index';

// ── Pattern 1: Node / Bun backend ──────────────────────────────────────────
//
// Typical setup: `@opentelemetry/sdk-node` registers a tracer provider
// + propagator (W3C trace-context by default), then any code path that
// calls `akribes.executions.run(...)` inside an active span will have its
// trace id propagated to akribes-server automatically.
async function nodeExample() {
  // Lazy import — keep the SDK example runnable when @opentelemetry/api
  // isn't installed (the SDK itself doesn't need it). The `// @ts-ignore`
  // is here because the example deliberately ships without `@opentelemetry/api`
  // as a dependency: we want type-checking to pass in this repo's CI even
  // though the runtime import is conditional. End users who install
  // `@opentelemetry/api` will get full types automatically.
  // @ts-ignore -- optional peer; install in your app to use this example.
  const { propagation, context, trace } = await import('@opentelemetry/api')
    .catch(() => ({ propagation: null, context: null, trace: null } as const));

  if (!propagation) {
    console.warn('@opentelemetry/api not installed — skipping Node example.');
    return;
  }

  const akribes = new AkribesClient({
    baseUrl: 'http://localhost:3001',
    token: process.env.AKRIBES_TOKEN,
    projectId: 1,
    // The SDK calls this on every outbound fetch. `context.active()` reads the
    // current span set up by your tracer; `propagation.inject` writes the
    // configured headers (default: `traceparent` + `tracestate`) into the carrier.
    propagator: (carrier) => propagation.inject(context.active(), carrier),
  });

  const tracer = trace.getTracer('my-app');
  const span = tracer.startSpan('akribes.run');
  try {
    await context.with(trace.setSpan(context.active(), span), async () => {
      // Inside this callback `context.active()` returns the span context, so
      // every akribes-server request carries the matching `traceparent`.
      const out = await akribes.executions.runAndAwait('demo_script', { inputs: { message: 'hi' } });
      console.log('execution', out);
    });
  } finally {
    span.end();
  }
}

// ── Pattern 2: Browser ─────────────────────────────────────────────────────
//
// Typical setup: `@opentelemetry/sdk-trace-web` + a fetch instrumentation,
// or a manual tracer provider. The SDK API is identical to Node — the only
// thing that changes is which OTel SDK you load. Don't import
// `@opentelemetry/sdk-node` in the browser (it pulls Node-only modules).
//
// In a browser bundle:
//
//   import { AkribesClient } from 'akribes';
//   import { propagation, context } from '@opentelemetry/api';
//
//   const akribes = new AkribesClient({
//     baseUrl: '/akribes',
//     token: scopedToken,        // 8-hour scoped token minted by your backend
//     projectId: 1,
//     propagator: (carrier) => propagation.inject(context.active(), carrier),
//   });
//
//   // Wrap user actions in a span so the propagated trace-id ties the
//   // browser-side click to the server-side execution.

// ── Pattern 3: Custom carrier shape (advanced) ─────────────────────────────
//
// If you don't want @opentelemetry/api as a runtime dep, you can implement
// the W3C propagator yourself — the carrier is a plain `Record<string, string>`
// and the SDK forwards every header you write into it:
//
//   const akribes = new AkribesClient({
//     // ...
//     propagator: (carrier) => {
//       carrier['traceparent'] = generateTraceparent(); // your impl
//     },
//   });

if (import.meta.main) {
  nodeExample().catch((err) => {
    console.error('example failed:', err);
    process.exit(1);
  });
}
