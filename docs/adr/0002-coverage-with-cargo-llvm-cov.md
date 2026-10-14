# ADR-0002: Measure coverage with cargo-llvm-cov

[← Back to index](../README.md)

- **Status:** Accepted
- **Date:** 2026-10-14
- **Decision:** Use `cargo-llvm-cov` (chosen over tarpaulin), installed via `flake.nix`. Run `cargo llvm-cov --summary-only` in the nix shell.
- **Consequences:** Baseline was 79.39% lines. `wasm.rs` is not measured (feature `wasm`).
