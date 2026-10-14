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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a GPX activity moving north in `steps` steps of 0.0001° latitude
    /// (~11.1 m). `step` gives (seconds since previous point, elevation delta).
    fn activity(steps: &[(u32, f64)]) -> String {
        let mut pts = String::new();
        let (mut t, mut ele, mut lat) = (0u32, 100.0_f64, 45.0_f64);
        let mut push = |t: u32, ele: f64, lat: f64| {
            pts.push_str(&format!(
                r#"<trkpt lat="{lat:.4}" lon="6.0"><ele>{ele}</ele><time>2024-01-01T{:02}:{:02}:{:02}Z</time></trkpt>"#,
                t / 3600,
                (t % 3600) / 60,
                t % 60
            ));
        };
        push(t, ele, lat);
        for &(dt, de) in steps {
            t += dt;
            ele += de;
            lat += 0.0001;
            push(t, ele, lat);
        }
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.1" creator="test" xmlns="http://www.topografix.com/GPX/1/1"><trk><trkseg>{pts}</trkseg></trk></gpx>"#
        )
    }

    #[test]
    fn steady_climb_yields_500m_segments_with_matching_gradient() {
        // 100 steps ≈ 1.1 km: two full 500 m windows; the ~100 m remainder is dropped.
        let gpx = activity(&vec![(3, 1.0); 100]);
        let segs = parse_ref_gpx_reader(gpx.as_bytes()).unwrap();
        assert_eq!(segs.len(), 2);
        for s in &segs {
            assert!(s.distance_m >= ACCUM_DIST_M);
            assert!((s.gradient_pct - 9.0).abs() < 0.5, "gradient {}", s.gradient_pct);
            assert!((s.elapsed_secs / s.distance_m - 3.0 / 11.12).abs() < 0.01);
        }
    }

    #[test]
    fn gradient_is_clamped_to_forty_percent() {
        let gpx = activity(&vec![(3, 10.0); 50]);
        let segs = parse_ref_gpx_reader(gpx.as_bytes()).unwrap();
        assert_eq!(segs[0].gradient_pct, 40.0);
    }

    #[test]
    fn remainder_over_half_a_window_is_flushed() {
        // 80 steps ≈ 890 m: one full window + ~390 m remainder (≥ 250 m).
        let gpx = activity(&vec![(3, 0.0); 80]);
        let segs = parse_ref_gpx_reader(gpx.as_bytes()).unwrap();
        assert_eq!(segs.len(), 2);
        assert!(segs[1].distance_m < ACCUM_DIST_M);
    }

    #[test]
    fn route_shorter_than_half_a_window_has_no_segments() {
        let gpx = activity(&vec![(3, 0.0); 10]);
        let err = parse_ref_gpx_reader(gpx.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("No valid segments"), "{err}");
    }

    #[test]
    fn pause_longer_than_a_minute_resets_the_window() {
        // 30 steps, a 5-minute pause, then 30 more: neither half reaches 250 m after
        // the reset (the pause step itself is discarded), so combined distance is lost.
        let mut steps = vec![(3, 0.0); 30];
        steps.push((300, 0.0));
        steps.extend(vec![(3, 0.0); 10]);
        let gpx = activity(&steps);
        let err = parse_ref_gpx_reader(gpx.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("No valid segments"), "{err}");
    }

    #[test]
    fn stationary_points_do_not_add_distance() {
        // Same coordinates repeated: build manually with identical lat.
        let pts = (0..5)
            .map(|i| {
                format!(
                    r#"<trkpt lat="45.0" lon="6.0"><ele>100</ele><time>2024-01-01T00:00:{:02}Z</time></trkpt>"#,
                    i * 3
                )
            })
            .collect::<String>();
        let gpx = format!(
            r#"<?xml version="1.0"?><gpx version="1.1" creator="t" xmlns="http://www.topografix.com/GPX/1/1"><trk><trkseg>{pts}</trkseg></trk></gpx>"#
        );
        assert!(parse_ref_gpx_reader(gpx.as_bytes()).is_err());
    }

    #[test]
    fn points_missing_time_or_elevation_are_rejected() {
        let gpx = r#"<?xml version="1.0"?><gpx version="1.1" creator="t" xmlns="http://www.topografix.com/GPX/1/1"><trk><trkseg>
<trkpt lat="45.0" lon="6.0"><ele>100</ele></trkpt>
<trkpt lat="45.001" lon="6.0"><time>2024-01-01T00:00:03Z</time></trkpt>
</trkseg></trk></gpx>"#;
        let err = parse_ref_gpx_reader(gpx.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("at least 2 trackpoints"), "{err}");
    }

    #[test]
    fn invalid_xml_and_missing_file_are_errors() {
        assert!(parse_ref_gpx_reader("<gpx>".as_bytes()).is_err());
        let err = parse_ref_gpx(Path::new("/nonexistent/ref.gpx")).unwrap_err();
        assert!(err.to_string().contains("/nonexistent/ref.gpx"), "{err}");
    }

    #[test]
    fn parse_ref_gpx_reads_from_disk() {
        let path = std::env::temp_dir().join("gpx_splitter_ref_parser_test.gpx");
        std::fs::write(&path, activity(&vec![(3, 1.0); 100])).unwrap();
        let segs = parse_ref_gpx(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(segs.len(), 2);
    }
}
