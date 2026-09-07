use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use gpx_splitter::pace_model::TerrainType;
use gpx_splitter::{chunker, output, pace_model, parser, ref_parser};

/// Terrain type of the target race.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Terrain {
    /// Road race — no downhill penalty.
    Road,
    /// Trail race — applies a progressive penalty on steep descents
    /// (useful when the reference activity was recorded on roads).
    Trail,
}

impl From<Terrain> for TerrainType {
    fn from(t: Terrain) -> Self {
        match t {
            Terrain::Road => TerrainType::Road,
            Terrain::Trail => TerrainType::Trail,
        }
    }
}

/// Convert GPX files to CSV with gradient-based chunking for race planning.
#[derive(Parser, Debug)]
#[command(name = "gpx-splitter", version, about)]
struct Cli {
    /// Path to the input GPX file (race route).
    input: PathBuf,

    /// Path to an optional reference GPX activity file recorded by a GPS watch.
    /// The file must have <time> and <ele> on each trackpoint (standard for
    /// Garmin, COROS, Suunto, Polar exports).
    /// When provided, the tool builds a gradient→pace model from your real
    /// activity and adds estimated pace and time columns to the output.
    #[arg(short, long)]
    reference: Option<PathBuf>,

    /// Path to the output CSV file. Use "-" for stdout.
    #[arg(short, long, default_value = "output.csv")]
    output: PathBuf,

    /// Gradient tolerance in percentage points.
    /// Lower values = more splits (finer grain), higher values = fewer splits.
    /// e.g. 1.0 gives very detailed splits, 4.0 gives coarse splits.
    #[arg(short, long, default_value_t = 2.0)]
    tolerance: f64,

    /// Minimum chunk distance in metres before a split can occur.
    /// Higher values = fewer, longer chunks. Lower values = more splits.
    /// e.g. 100 for fine detail, 500 for broad sections.
    #[arg(short, long, default_value_t = 200.0)]
    min_distance: f64,

    /// Terrain type of the target race.
    /// Only used when --reference is provided.
    ///   road  — no downhill penalty
    ///   trail — slows down steep descents to compensate for road-reference data
    ///           (+5% pace per 1% gradient steeper than −5%, e.g. ×1.25 at −10%)
    #[arg(long, value_enum, default_value_t = Terrain::Road)]
    terrain: Terrain,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let segments = parser::parse_gpx(&cli.input)?;
    eprintln!(
        "Parsed {} segments from {}",
        segments.len(),
        cli.input.display()
    );

    let mut chunks = chunker::chunkify(&segments, cli.tolerance, cli.min_distance);
    eprintln!("Created {} chunks", chunks.len());

    let total_distance: f64 = chunks.iter().map(|c| c.distance_km).sum();
    let total_gain: f64 = chunks.iter().map(|c| c.elevation_gain_m).sum();
    let total_loss: f64 = chunks.iter().map(|c| c.elevation_loss_m).sum();
    eprintln!(
        "Total: {:.1} km | D+ {:.0} m | D- {:.0} m",
        total_distance,
        total_gain,
        total_loss
    );

    if let Some(ref_path) = cli.reference {
        eprintln!("Loading reference activity from {}…", ref_path.display());
        let ref_segs = ref_parser::parse_ref_gpx(&ref_path)?;
        eprintln!("Parsed {} reference segments", ref_segs.len());

        let model = pace_model::PaceModel::from_segments(&ref_segs);

        // Print the gradient→pace table so the user can inspect it.
        eprintln!("\nGradient → pace model:");
        eprintln!("{:>8}  {:>10}", "Grad %", "Pace /km");
        for (grad, pace_min_km) in model.entries() {
            let pace_secs_total = (pace_min_km * 60.0) as u64;
            let m = pace_secs_total / 60;
            let s = pace_secs_total % 60;
            eprintln!("{:>7.1}%  {:02}:{:02} /km", grad, m, s);
        }
        eprintln!();
        eprintln!(
            "Auto-detected fatigue: {:+.1}% at the finish (shape exponent {:.1}, varies by gradient)",
            model.suggested_factor * 100.0,
            model.suggested_exponent,
        );
        eprintln!("Terrain: {:?}", cli.terrain);

        chunker::apply_pace_model(&mut chunks, &model, cli.terrain.into());

        // Sum estimated times by parsing the HH:MM:SS strings back to seconds.
        let total_secs: f64 = chunks
            .iter()
            .filter_map(|c| {
                let t = c.estimated_time.as_ref()?;
                let parts: Vec<u64> = t.split(':').map(|p| p.parse().unwrap_or(0)).collect();
                if parts.len() == 3 { Some((parts[0] * 3600 + parts[1] * 60 + parts[2]) as f64) } else { None }
            })
            .sum();
        eprintln!(
            "Estimated total time: {}",
            pace_model::format_duration(total_secs)
        );
    }

    output::write_csv(&chunks, &cli.output)?;
    if cli.output.as_os_str() != "-" {
        eprintln!("Written to {}", cli.output.display());
    }

    Ok(())
}
