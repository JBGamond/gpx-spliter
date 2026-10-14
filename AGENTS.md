# Repository Guidelines

## Decision Protocol (MANDATORY)
- **Consult the user (jbgamond) before any significant decision** with real alternatives: dependencies/features, public API or CSV format, module layout, build/deploy/tooling, model/algorithm changes. Use the ask tool; present options and tradeoffs. Routine choices (test names, local refactors, thresholds within an agreed policy) need no ADR.
- **Log only significant decisions as ADRs** in `docs/`:
  - Index: `docs/README.md` (table linking every ADR).
  - One short file per ADR: `docs/adr/NNNN-kebab-title.md` (sequential; never renumber).
  - Each ADR: Status, Date, Decision, Consequences (a few lines each). Add Context/Options only if non-obvious. Link back to `../README.md`.
  - Add file + index row in the same change. Superseding: mark old `Superseded by [NNNN](...)`, never delete.
  - Rationale: [ADR-0001](docs/adr/0001-record-decisions-as-adrs.md).

## Project Overview
`gpx-splitter`: Rust (edition 2024) tool that splits a race-route GPX into gradient-consistent chunks, optionally estimates pace/time/fatigue, plans nutrition, and emits a `;`-delimited CSV. Three front doors over one library: CLI, WASM browser UI, and a static-file web server.

## Architecture & Data Flow
```
GPX bytes/file -> parser (drop no-<ele> pts, 5-pt smoothing, Haversine) -> Vec<Segment>
  -> chunker::chunkify(tolerance %, min distance m) -> Vec<Chunk>
  -> pacing (optional)
       CLI : ref_parser -> PaceModel -> chunker::apply_pace_model (TerrainType road|trail)
       WASM: ManualPaces (Minetti-seeded) -> chunker::apply_manual_paces (fatigue/wall/downhill params)
  -> nutrition::plan (needs Chunk.estimated_secs; empty plan otherwise)
  -> output: CSV (output.rs) or JSON via serde_wasm_bindgen
```
- Core lib (`src/lib.rs`) is wasm-agnostic; only `src/wasm.rs` (`#![cfg(feature="wasm")]`) touches wasm_bindgen. Reference-activity pacing is **CLI-only**; the web UI is manual-pace only.
- `src/bin/web.rs` is just an axum `ServeDir("static")` on `0.0.0.0:3000`; no API routes. Docker/Caddy serves the same `static/`.
- Frontend: `static/app.js` (~1600-line vanilla ES module, module-level mutable state, MapLibre 4.7.1 from CDN, canvas chart) imports `GpxAnalyzer`, `suggest_manual_paces` from `static/pkg/` (generated).

## Key Directories
- `src/` library modules: `parser`, `chunker`, `trackpoint` (plain data types), `pace_model`, `ref_parser`, `nutrition`, `output`, `wasm`.
- `src/main.rs` CLI (clap derive); `src/bin/web.rs` server.
- `static/` browser UI; `static/pkg/` wasm-pack output (self-gitignored, never commit).
- `.github/instructions/` Rust + Docker conventions (follow them).
- `docs/` ADRs.

## Development Commands
```bash
cargo run -- route.gpx -o out.csv          # CLI (-r ref.gpx, -t tolerance=2.0, -m min-distance=200, --terrain road|trail, -o - = stdout)
cargo run --bin web                        # http://localhost:3000 (run from repo root; relative "static")
wasm-pack build --target web --out-dir static/pkg -- --no-default-features --features wasm
./build.sh                                 # wasm-pack + docker build (--no-cache)
docker compose up -d                       # Caddy on :8080 -> static/
cargo fmt && cargo clippy && cargo test
cargo llvm-cov --summary-only             # coverage (nix shell); see docs/adr/0002
```
No CI exists. WASM build **requires** `--no-default-features` (tokio/axum don't target wasm). Docker image does not build Rust; `static/pkg` must exist on host first.

## Code Conventions & Common Patterns
- Follow `.github/instructions/rust.instructions.md`; `cargo fmt` + `clippy` clean.
- Errors: `anyhow::Result` in CLI/parsers; wasm returns `JsError("Failed to ...: {e}")`. Waypoint parse failure intentionally swallowed (`unwrap_or_default`) in `wasm.rs`.
- IO functions come in pairs: path version + generic `*_reader<R: Read>` / `*_to_writer<W: Write>` (wasm and tests use the reader/writer forms).
- Paces stored in seconds/metre; displayed via `format_pace` / `format_duration`.
- `Chunk` is serde `Serialize`; optional pace fields use `skip_serializing_if`. Adding a field means updating test helpers in `nutrition.rs` tests and `output.rs::make_chunk`, and the CSV header + row together.
- CSV: delimiter `;`, decimal comma, 13 fixed columns; `estimated_secs` is not written. Header/row field counts must match (regression test).
- Naming: snake_case modules/functions, descriptive test names (`bars_eaten_before_gels`).
- State: no DI/global state in Rust; functions take inputs and return values. `GpxAnalyzer` holds parsed segments + waypoints; JS state is module-level in `app.js`.
- wasm methods take positional `f64` tuning params (`clippy::too_many_arguments` allowed).

## Important Files
`src/main.rs`, `src/wasm.rs`, `src/chunker.rs`, `src/pace_model.rs`, `src/nutrition.rs`, `src/output.rs`, `Cargo.toml` (features: `default=[server]`, `wasm`; bin `web` requires `server`), `build.sh`, `flake.nix`, `README.md`, `ARCHITECTURE.md`.

## Runtime/Tooling Preferences
- Rust stable via Nix flake (`flake.nix`, `.envrc` = `use flake`; includes wasm32 target, wasm-pack, wasm-bindgen-cli, cargo-watch). Cargo is the package manager; no Node toolchain.
- `.gitignore` excludes `*.gpx`, `output.csv`, `/target`: do not commit samples.
- Known docs/UI divergence: CLI supports `--reference`/`--terrain`; web UI does not.

## Testing & QA
- Built-in `#[test]` in inline `#[cfg(test)] mod tests` per module (~55 tests: nutrition, pace_model, parser, chunker, output). Run `cargo test [name]`.
- Fixtures are inline (`r#"<gpx>..."#`) and local helpers (`chunks()`, `pt()`, `make_chunk()`); floats compared with epsilon.
- Coverage ~94% lines (see docs/adr/0003). Untested: `wasm.rs`, `main.rs`, `web.rs`, JS. Add tests when touching these.
