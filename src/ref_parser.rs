use anyhow::{Context, Result};
use geo::{Distance, Haversine, Point};
use gpx::Time;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use time::OffsetDateTime;

/// Minimum distance in metres to accumulate before emitting a reference segment.
/// 500 m ensures enough elevation change to compute a meaningful gradient even
/// with integer-precision altitude data (1 m steps).
const ACCUM_DIST_M: f64 = 500.0;

/// A short segment derived from a reference GPX activity.
#[derive(Debug, Clone)]
pub struct RefSegment {
    pub distance_m: f64,
    pub elapsed_secs: f64,
    pub gradient_pct: f64,
}

/// Parse a GPX activity file (recorded by a GPS watch) and return a list of
/// reference segments with distance, elapsed time, and gradient.
pub fn parse_ref_gpx(path: &Path) -> Result<Vec<RefSegment>> {
    let file = File::open(path)
        .with_context(|| format!("Failed to open reference GPX file: {}", path.display()))?;
    let reader = BufReader::new(file);
    parse_ref_gpx_reader(reader)
}

/// Parse reference GPX data from a reader and return a list of reference segments.
pub fn parse_ref_gpx_reader<R: Read>(reader: R) -> Result<Vec<RefSegment>> {
    let gpx = gpx::read(reader).with_context(|| "Failed to parse reference GPX data")?;

    struct Pt {
        ts: OffsetDateTime,
        lat: f64,
        lon: f64,
        ele: f64,
    }

    let points: Vec<Pt> = gpx
        .tracks
        .iter()
        .flat_map(|t| &t.segments)
        .flat_map(|s| &s.points)
        .filter_map(|wp| {
            let t: Time = wp.time?;
            let ts: OffsetDateTime = t.into();
            let ele = wp.elevation?;
            Some(Pt {
                ts,
                lat: wp.point().y(),
                lon: wp.point().x(),
                ele,
            })
        })
        .collect();

    if points.len() < 2 {
        anyhow::bail!(
            "Reference GPX must contain at least 2 trackpoints with <time> and <ele>"
        );
    }

    let mut segments: Vec<RefSegment> = Vec::new();

    // Accumulate consecutive point-to-point steps into larger segments so the
    // integer-precision elevation changes produce meaningful gradients.
    let mut accum_dist = 0.0_f64;
    let mut accum_secs = 0.0_f64;
    let mut start_ele = points[0].ele;
    let mut end_ele = points[0].ele;
    let mut prev = &points[0];

    for pt in points.iter().skip(1) {
        let elapsed = (pt.ts - prev.ts).whole_milliseconds() as f64 / 1000.0;

        // Reset accumulator on pauses or time jumps
        if elapsed <= 0.0 || elapsed > 60.0 {
            accum_dist = 0.0;
            accum_secs = 0.0;
            start_ele = pt.ele;
            prev = pt;
            continue;
        }

        let step_dist =
            Haversine.distance(Point::new(prev.lon, prev.lat), Point::new(pt.lon, pt.lat));

        if step_dist < 0.1 {
            // Stationary — keep accumulating time but reset on next non-stationary
            prev = pt;
            continue;
        }

        accum_dist += step_dist;
        accum_secs += elapsed;
        end_ele = pt.ele;

        if accum_dist >= ACCUM_DIST_M {
            let gradient = ((end_ele - start_ele) / accum_dist * 100.0).clamp(-40.0, 40.0);
            segments.push(RefSegment {
                distance_m: accum_dist,
                elapsed_secs: accum_secs,
                gradient_pct: gradient,
            });
            // Start next accumulation window from current point
            accum_dist = 0.0;
            accum_secs = 0.0;
            start_ele = pt.ele;
        }

        prev = pt;
    }

    // Flush any remaining accumulation if substantial enough
    if accum_dist >= ACCUM_DIST_M / 2.0 && accum_secs > 0.0 {
        let gradient = ((end_ele - start_ele) / accum_dist * 100.0).clamp(-40.0, 40.0);
        segments.push(RefSegment {
            distance_m: accum_dist,
            elapsed_secs: accum_secs,
            gradient_pct: gradient,
        });
    }

    if segments.is_empty() {
        anyhow::bail!("No valid segments could be extracted from the reference GPX file");
    }

    Ok(segments)
}
