# Changelog

`akribes-types` is versioned in lock-step with [`akribes-sdk`](https://crates.io/crates/akribes-sdk). See the [SDK changelog](https://github.com/PodestaAI/akribes-sdks/blob/main/crates/akribes-sdk/CHANGELOG.md) for release notes.

## [0.21.16] — 2026-05-30

First public release. Extracted from `akribes-core` (private) so the SDK can depend on a small, stable wire-types crate instead of pulling the parser, analyzer, and engine into every downstream build.
