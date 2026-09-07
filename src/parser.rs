use anyhow::{Context, Result};
use geo::{Distance, Haversine};
use geo::Point;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::trackpoint::{Segment, TrackPoint};

/// Width of the moving-average window used to smooth elevation before computing
/// gradients. GPX elevation (whether GPS or barometric) is noisy at the
/// point-to-point level: a single spurious 1-2 m blip between two points a few
/// metres apart can produce a triple-digit gradient percentage. Averaging each
/// point with its neighbours removes that micro jitter so the chunker splits on
/// genuine terrain changes instead of sensor noise, and D+/D- totals stop being
/// inflated by noise that cancels out over any real distance.
const ELEVATION_SMOOTHING_WINDOW: usize = 5;

/// Smooth elevation in place using a centred moving average.
///
/// Lat/lon are left untouched — only elevation (and therefore gradient) is
/// affected. Endpoints use a shrinking window rather than padding, so the
/// first and last points aren't pulled towards a value they never had.
fn smooth_elevations(points: &mut [TrackPoint]) {
    if points.len() < 3 {
        return;
    }
    let half = ELEVATION_SMOOTHING_WINDOW / 2;
    let raw: Vec<f64> = points.iter().map(|p| p.elevation).collect();
    for (i, point) in points.iter_mut().enumerate() {
        let lo = i.saturating_sub(half);
        let hi = (i + half).min(raw.len() - 1);
        let window = &raw[lo..=hi];
        point.elevation = window.iter().sum::<f64>() / window.len() as f64;
    }
}

/// Parse a GPX file and return a list of consecutive segments with distance and elevation data.
pub fn parse_gpx(path: &Path) -> Result<Vec<Segment>> {
    let file = File::open(path).with_context(|| format!("Failed to open GPX file: {}", path.display()))?;
    let reader = BufReader::new(file);
    parse_gpx_reader(reader)
}

/// Parse GPX data from a reader and return a list of consecutive segments.
pub fn parse_gpx_reader<R: Read>(reader: R) -> Result<Vec<Segment>> {
    let gpx = gpx::read(reader).with_context(|| "Failed to parse GPX data")?;

    let mut points: Vec<TrackPoint> = gpx
        .tracks
        .iter()
        .flat_map(|track| &track.segments)
        .flat_map(|segment| &segment.points)
        .filter_map(|wp| {
            let ele = wp.elevation?;
            Some(TrackPoint {
                lat: wp.point().y(),
                lon: wp.point().x(),
                elevation: ele,
            })
        })
        .collect();

    if points.len() < 2 {
        anyhow::bail!("GPX data must contain at least 2 points with elevation data");
    }

    smooth_elevations(&mut points);

    let segments = points
        .windows(2)
        .map(|w| {
            let start = &w[0];
            let end = &w[1];
            let p1 = Point::new(start.lon, start.lat);
            let p2 = Point::new(end.lon, end.lat);
            let distance_m = Haversine.distance(p1, p2);
            let ele_diff = end.elevation - start.elevation;
            let gradient_pct = if distance_m > 0.0 {
                (ele_diff / distance_m) * 100.0
            } else {
                0.0
            };

            Segment {
                start: start.clone(),
                end: end.clone(),
                distance_m,
                elevation_gain: ele_diff.max(0.0),
                elevation_loss: (-ele_diff).max(0.0),
                gradient_pct,
            }
        })
        .collect();

    Ok(segments)
}

/// A named point of interest from a GPX file's standalone `<wpt>` elements,
/// as opposed to the `<trk>` points making up the route itself. Many
/// trail-race GPX exports use these for aid stations / checkpoints.
#[derive(Debug, Clone)]
pub struct GpxWaypoint {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub elevation: Option<f64>,
}

/// Parse the standalone `<wpt>` waypoints embedded in a GPX file.
pub fn parse_gpx_waypoints_reader<R: Read>(reader: R) -> Result<Vec<GpxWaypoint>> {
    let gpx = gpx::read(reader).with_context(|| "Failed to parse GPX data")?;
    Ok(gpx
        .waypoints
        .iter()
        .map(|wp| GpxWaypoint {
            name: wp.name.clone().unwrap_or_else(|| "Aid station".to_string()),
            lat: wp.point().y(),
            lon: wp.point().x(),
            elevation: wp.elevation,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(elevation: f64) -> TrackPoint {
        TrackPoint { lat: 0.0, lon: 0.0, elevation }
    }

    #[test]
    fn smoothing_removes_a_single_point_spike() {
        // A lone 5 m spike between otherwise flat points should be pulled
        // most of the way back down by its neighbours.
        let mut points = vec![pt(100.0), pt(100.0), pt(105.0), pt(100.0), pt(100.0)];
        smooth_elevations(&mut points);
        assert!(
            points[2].elevation < 102.0,
            "expected spike to be smoothed below 102.0, got {}",
            points[2].elevation
        );
    }

    #[test]
    fn smoothing_preserves_a_steady_climb() {
        // A genuine, consistent climb should survive smoothing almost
        // unchanged in its interior (endpoints use a smaller window).
        let mut points: Vec<TrackPoint> = (0..9).map(|i| pt(i as f64 * 10.0)).collect();
        smooth_elevations(&mut points);
        let diff = (points[4].elevation - 40.0).abs();
        assert!(diff < 0.001, "expected ~40.0 at midpoint, got {}", points[4].elevation);
    }

    #[test]
    fn smoothing_is_noop_for_short_input() {
        let mut points = vec![pt(100.0), pt(105.0)];
        smooth_elevations(&mut points);
        assert_eq!(points[0].elevation, 100.0);
        assert_eq!(points[1].elevation, 105.0);
    }

    #[test]
    fn parses_waypoints_from_gpx() {
        let gpx_data = r#"<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.1" creator="test" xmlns="http://www.topografix.com/GPX/1/1">
  <wpt lat="45.0" lon="6.0">
    <ele>1200</ele>
    <name>Ravito 1</name>
  </wpt>
  <trk><trkseg>
    <trkpt lat="45.0" lon="6.0"><ele>1200</ele></trkpt>
    <trkpt lat="45.001" lon="6.001"><ele>1210</ele></trkpt>
  </trkseg></trk>
</gpx>"#;
        let waypoints = parse_gpx_waypoints_reader(gpx_data.as_bytes()).unwrap();
        assert_eq!(waypoints.len(), 1);
        assert_eq!(waypoints[0].name, "Ravito 1");
        assert_eq!(waypoints[0].elevation, Some(1200.0));
    }
}
