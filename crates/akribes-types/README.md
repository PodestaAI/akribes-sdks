# akribes-types

Wire-level types shared by the [Akribes](https://akribes.ai) SDK and server: the `EngineEvent` stream variants, runtime `Value`s, AST shapes (`Span`, `TypeRef`, `TypeField`, `ActorHint`), and the typed error envelope (`ErrorKind`, `ErrorCode`, `ErrorSource`, `ErrorDetail`).

## When to use this directly

Most callers want [`akribes-sdk`](https://crates.io/crates/akribes-sdk) instead:

```bash
cargo add akribes-sdk
```

Reach for `akribes-types` when you're:

- Building an **alternate transport** (a custom HTTP/WebSocket client, an MQTT bridge, a Kafka consumer) and need to deserialize the same wire format.
- Implementing a **telemetry consumer** (log forwarder, replay store, audit pipeline) that only needs the typed events, not the SDK surface.
- Writing a **language tool** (linter, formatter, codegen) that needs the AST shape without pulling in the full parser/analyzer.

## Dependencies

Tiny on purpose: `serde`, `serde_json`, `thiserror`, `httpdate`, `tracing`. No tokio, no reqwest, no platform-specific code.

## Stability

Versions track the [Akribes monorepo](https://github.com/PodestaAI/akribes-sdks). Pre-1.0: minor bumps can be breaking; patch bumps are additive. Pin a tilde-range (`~0.21`) until 1.0.

## License

MIT. See [`LICENSE`](./LICENSE).
