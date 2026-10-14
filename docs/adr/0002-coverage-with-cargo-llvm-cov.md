# ADR-0002: Measure coverage with cargo-llvm-cov

[← Back to index](../README.md)

- **Status:** Accepted
- **Date:** 2026-10-14
- **Decider:** jbgamond

## Context
No coverage measurement existed. A quality step needs a baseline before improving tests.

## Options considered
1. `cargo-llvm-cov`: LLVM source-based, accurate, fast; needs the `llvm-tools` component.
2. `cargo-tarpaulin`: simple, ptrace-based, Linux-centric, less accurate.
Install: add to `flake.nix` devShell (reproducible) vs local `cargo install`.

## Decision
`cargo-llvm-cov`, installed through `flake.nix` (`pkgs.cargo-llvm-cov` plus the `llvm-tools-preview` rust extension). Run: `nix develop --command cargo llvm-cov --summary-only`. The coverage target is deferred until the baseline is reviewed (see Consequences).

## Consequences
- Baseline (2026-10-14): 79.39% lines, 79.01% regions, 83.12% functions. 0%: `main.rs`, `bin/web.rs`, `ref_parser.rs`. Low: `parser.rs` 54.6%, `chunker.rs` 71.1%.
- Threshold to be set by jbgamond in a follow-up ADR.
- `wasm.rs` is excluded from the default run (feature `wasm`).
