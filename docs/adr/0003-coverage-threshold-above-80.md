# ADR-0003: Line coverage target above 80%

[← Back to index](../README.md)

- **Status:** Accepted
- **Date:** 2026-10-14
- **Decider:** jbgamond

## Context
[ADR-0002](0002-coverage-with-cargo-llvm-cov.md) measured a 79.39% line baseline. The owner reviewed it and chose a target of "more than 80%". Options offered were ≥80% on the core library, ≥90% on the core library, or everything including CLI/web integration tests.

## Decision
Keep line coverage above 80% as measured by `cargo llvm-cov --summary-only`. The assistant interpreted the target as applying to the whole default-feature run (including `main.rs` and `bin/web.rs`, which stay at 0%).

## Consequences
- Added unit tests in `parser.rs`, `ref_parser.rs` and `chunker.rs` (`apply_pace_model`, `apply_manual_paces`, gradient classification, error paths, clamping, pause handling).
- Result: 94.20% lines, 93.28% regions, 93.23% functions (71 tests pass).
- Still uncovered: `main.rs`, `bin/web.rs`, `wasm.rs` (not built by default), `output.rs` file-writing path.
- No CI enforces the threshold; checked manually.
