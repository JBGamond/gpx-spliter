# gpx-splitter

Convert GPX files to CSV with gradient-based chunking for race planning.

The tool splits a GPX route into sections of consistent gradient — e.g. a 1.5 km chunk at 4% uphill, then 0.8 km flat, etc. — so you can plan paces for each section.

Optionally, provide a **reference GPX activity** (exported from your watch) to automatically estimate paces and finish time based on your real performance at each gradient.

The web UI also renders a **route preview**: the track on a [MapLibre GL](https://maplibre.org/) map and an elevation chart below it, both colored by estimated pace (green = fast, red = slow), or by gradient before pace data is available.

## Docker

To build and run the entire application using Docker, use the provided build script:

### Build & Run

```bash
./build.sh
docker compose up -d
```

Then open [http://localhost:8080](http://localhost:8080).

The build script compiles the Rust WASM module on your host machine and then builds a lightweight Docker image containing only the Caddy web server and the pre-built static assets.

## Development Environment

This repo ships a [Nix flake](flake.nix) with a complete toolchain (Rust, `wasm-pack`, `wasm-bindgen-cli`, a C linker). If you have [Nix](https://nixos.org/download) and [direnv](https://direnv.net/) installed:

```bash
direnv allow
```

`cargo`, `wasm-pack`, etc. will then be available automatically whenever you `cd` into this directory. Without direnv, `nix develop` drops you into the same shell manually.

## Web UI (Local Development)

```bash
./build.sh          # builds the WASM module into static/pkg
cargo run --bin web  # serves ./static on http://localhost:3000
```

## CLI Build

```bash
cargo build --release --bin gpx-splitter
```

The binary is at `target/release/gpx-splitter`.

## Usage

```bash
gpx-splitter <INPUT> [OPTIONS]
```

### Options

| Flag | Description | Default |
|---|---|---|
| `-r, --reference <PATH>` | Reference GPX activity file from your watch (Garmin, COROS, Suunto, Polar…). Must have `<time>` and `<ele>` on each trackpoint. Adds pace/time columns to the output. | — |
| `-o, --output <PATH>` | Output CSV path (use `-` for stdout) | `output.csv` |
| `-t, --tolerance <VALUE>` | Gradient tolerance in percentage points. **Lower = more splits**, higher = fewer splits | `2.0` |
| `-m, --min-distance <VALUE>` | Minimum chunk distance in metres before a split can occur. **Higher = fewer, longer chunks** | `200` |
| `--terrain <TYPE>` | Terrain type of the race. Use `trail` when reference was recorded on roads. *(only with --reference)* | `road` |

### Examples

```bash
# Route only — chunks with gradient and elevation, no pace estimate
gpx-splitter my_race.gpx

# With a reference activity — adds estimated pace and time per chunk
gpx-splitter my_race.gpx --reference my_training_run.gpx

# Reference recorded on roads, target race is on trail
gpx-splitter my_race.gpx --reference my_training_run.gpx --terrain trail

# Fine-grained splits
gpx-splitter my_race.gpx --reference my_training_run.gpx -t 1.0 -m 100

# Coarse splits
gpx-splitter my_race.gpx --reference my_training_run.gpx -t 4.0 -m 500
```

### Fatigue is auto-detected

Rather than asking you to guess a fatigue profile, the tool derives one directly from your reference activity: it compares your pace at each gradient during the first quarter of the run against the last quarter, per gradient band, and fits a power-law fatigue curve (`multiplier = 1 + factor × progress^exponent`) from that. The detected factor and exponent are printed to stderr (and shown in the web UI) so you can sanity-check them; there is nothing to configure via CLI flags.

### Terrain penalty (`--terrain trail`)

When the reference activity was recorded on roads (or groomed paths), the model may extrapolate unrealistically fast paces for steep downhill sections that don't exist in the reference data.

The `--terrain trail` flag adds a progressive penalty to compensate:

| Chunk gradient | Penalty multiplier | Note |
|---|---|---|
| ≥ −5 % | ×1.00 | No penalty |
| −10 % | ×1.25 | Moderate descent |
| −15 % | ×1.50 | Steep descent |
| −20 % | ×1.75 | Very steep descent |
| −25 % | ×2.00 | Extreme descent |

This brings steep downhill estimates in line with realistic trail descent paces (typically 5–7 min/km for technical terrain), vs. the 2–3 min/km that road extrapolation would suggest.

### How the pace model works

When a reference activity is provided, the tool:
1. Splits the activity into 500 m segments to smooth elevation noise
2. Computes the average pace at each 1%-wide gradient band (e.g. all segments at 3–4% uphill)
3. Prints the full gradient → pace table so you can review it
4. Applies linear interpolation/extrapolation to estimate your pace at every chunk's gradient

The reference activity does **not** need to be on the same course — any run with representative terrain works.

### Elevation smoothing

Before the route's gradients are computed, the parser runs a 5-point moving average over raw elevation. Point-to-point GPX elevation is noisy enough that a single 1-2 m blip between nearby points can register as a triple-digit gradient; smoothing it out means the chunker splits on genuine terrain changes rather than sensor noise, and D+/D- totals aren't inflated by jitter. (The reference activity uses its own noise-reduction strategy — the 500 m accumulation windows above.)

### Controlling the number of splits

Two parameters work together to control granularity:

- **`--tolerance` (`-t`)**: How much the gradient can vary within a chunk. A tolerance of `1.0` is very precise; `4.0` merges gentle uphill and flat together.
- **`--min-distance` (`-m`)**: The minimum length (metres) before a split is allowed. Use `100` for fine detail, `500`+ to avoid short noisy chunks.

| Style | Tolerance | Min distance | Typical result |
|---|---|---|---|
| Very detailed | `1.0` | `100` | Many short chunks |
| Default | `2.0` | `200` | Balanced |
| Broad overview | `4.0` | `500` | Few long chunks |

## CSV Output

Each row is a chunk with consistent gradient. Fields are `;`-delimited with `,` as the decimal separator (European convention, opens cleanly in Excel):

| Column | Description |
|---|---|
| `chunk_number` | Sequential chunk index |
| `distance_km` | Length of this chunk in km |
| `cumulative_km` | Distance from start in km |
| `elevation_gain_m` | D+ in metres |
| `elevation_loss_m` | D- in metres |
| `cumulative_gain_m` | Cumulative D+ from start in metres |
| `cumulative_loss_m` | Cumulative D- from start in metres |
| `gradient_pct` | Average gradient in % (positive = uphill) |
| `classification` | flat, gentle/moderate/steep/very steep uphill/downhill |
| `fatigue_pct` | Applied fatigue multiplier at this point in the race, as % *(only with --reference)* |
| `estimated_pace` | Estimated pace as `MM:SS /km` *(only with --reference)* |
| `estimated_time` | Estimated time for this chunk as `HH:MM:SS` *(only with --reference)* |
| `cumulative_time` | Estimated elapsed time from start as `HH:MM:SS` *(only with --reference)* |

### Sample output (with reference activity)

```
chunk_number;distance_km;cumulative_km;elevation_gain_m;elevation_loss_m;cumulative_gain_m;cumulative_loss_m;gradient_pct;classification;fatigue_pct;estimated_pace;estimated_time;cumulative_time
1;0,276;0,276;6;0;6;0;2,18;gentle uphill;0,0;06:26;00:01:47;00:01:47
2;0,211;0,487;4;4;10;4;0,00;flat;0,2;05:54;00:01:15;00:03:01
3;0,224;0,711;12;1;22;5;4,91;moderate uphill;0,3;06:55;00:01:33;00:04:34
```

Open the CSV in a spreadsheet and you have your race plan.

