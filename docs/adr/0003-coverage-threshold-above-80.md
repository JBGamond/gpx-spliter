# ADR-0003: Line coverage target above 80%

[← Back to index](../README.md)

- **Status:** Accepted
- **Date:** 2026-10-14
- **Decision:** Keep line coverage above 80% for the default-feature run ([ADR-0002](0002-coverage-with-cargo-llvm-cov.md)).
- **Consequences:** Now 94.2% (71 tests). `main.rs`, `bin/web.rs`, `wasm.rs` stay uncovered. No CI enforcement.
