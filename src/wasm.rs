#![cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;
use crate::{chunker, output, parser};
use crate::pace_model::ManualPaces;
use crate::trackpoint::Segment;
use geo::{Distance, Haversine, Point};
use std::io::Cursor;

/// A single point along the route, for rendering a map preview and elevation chart.
#[derive(serde::Serialize)]
pub struct RoutePoint {
    pub lat: f64,
    pub lon: f64,
    pub elevation: f64,
    /// Cumulative distance from the start, in km.
    pub distance_km: f64,
}

/// A named point of interest along the route (e.g. an aid station), snapped
/// onto the route's cumulative distance so it can be placed on both the map
/// and the elevation chart's x-axis.
#[derive(serde::Serialize)]
pub struct AidStation {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub elevation: f64,
    pub distance_km: f64,
}

/// Suggest a full set of manual zone paces from a flat-ground pace, using
/// Minetti's metabolic cost model. Doesn't require a loaded GPX file, so
/// it's a free function rather than a `GpxAnalyzer` method.
#[wasm_bindgen]
pub fn suggest_manual_paces(flat_pace_spm: f64, hill_sensitivity: f64) -> Result<JsValue, JsError> {
    let paces = ManualPaces::from_flat_pace(flat_pace_spm, hill_sensitivity);
    serde_wasm_bindgen::to_value(&paces)
        .map_err(|e| JsError::new(&format!("Failed to serialize suggested paces: {}", e)))
}

#[wasm_bindgen]
pub struct GpxAnalyzer {
    input_segments: Vec<Segment>,
    waypoints: Vec<parser::GpxWaypoint>,
}

#[wasm_bindgen]
impl GpxAnalyzer {
    #[wasm_bindgen(constructor)]
    pub fn new(input_gpx: &[u8]) -> Result<GpxAnalyzer, JsError> {
        let input_segments = parser::parse_gpx_reader(Cursor::new(input_gpx))
            .map_err(|e| JsError::new(&format!("Failed to parse input GPX: {}", e)))?;

        // Best-effort: a GPX file with no `<wpt>` elements (or a malformed
        // one) just means no aid stations, not a fatal error.
        let waypoints = parser::parse_gpx_waypoints_reader(Cursor::new(input_gpx))
            .unwrap_or_default();

        Ok(GpxAnalyzer {
            input_segments,
            waypoints,
        })
    }

    /// Full-resolution route geometry (lat/lon/elevation + cumulative distance),
    /// independent of chunking/pace settings.
    fn route_points(&self) -> Vec<RoutePoint> {
        let mut points = Vec::with_capacity(self.input_segments.len() + 1);
        if let Some(first) = self.input_segments.first() {
            points.push(RoutePoint {
                lat: first.start.lat,
                lon: first.start.lon,
                elevation: first.start.elevation,
                distance_km: 0.0,
            });
        }

        let mut cumulative_m = 0.0;
        for seg in &self.input_segments {
            cumulative_m += seg.distance_m;
            points.push(RoutePoint {
                lat: seg.end.lat,
                lon: seg.end.lon,
                elevation: seg.end.elevation,
                distance_km: cumulative_m / 1000.0,
            });
        }

        points
    }

    /// Full-resolution route geometry, for drawing the map preview and
    /// elevation chart.
    pub fn get_route_points(&self) -> Result<JsValue, JsError> {
        serde_wasm_bindgen::to_value(&self.route_points())
            .map_err(|e| JsError::new(&format!("Failed to serialize route points: {}", e)))
    }

    /// Aid stations from the input GPX's `<wpt>` elements, snapped onto the
    /// route's cumulative distance (the nearest route point by straight-line
    /// distance) so they can be placed on the elevation chart's x-axis as
    /// well as on the map.
    pub fn get_aid_stations(&self) -> Result<JsValue, JsError> {
        let route_points = self.route_points();
        let stations: Vec<AidStation> = self
            .waypoints
            .iter()
            .filter_map(|wp| {
                let nearest = route_points.iter().min_by(|a, b| {
                    let da = Haversine.distance(Point::new(wp.lon, wp.lat), Point::new(a.lon, a.lat));
                    let db = Haversine.distance(Point::new(wp.lon, wp.lat), Point::new(b.lon, b.lat));
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                })?;
                Some(AidStation {
                    name: wp.name.clone(),
                    lat: wp.lat,
                    lon: wp.lon,
                    elevation: wp.elevation.unwrap_or(nearest.elevation),
                    distance_km: nearest.distance_km,
                })
            })
            .collect();

        serde_wasm_bindgen::to_value(&stations)
            .map_err(|e| JsError::new(&format!("Failed to serialize aid stations: {}", e)))
    }

    pub fn analyze(
        &self,
        tolerance: f64,
        min_distance: f64,
        manual_paces_val: JsValue,
        degradation_factor: f64,
        fatigue_exponent: f64,
        wall_onset: f64,
        downhill_degradation: f64,
    ) -> Result<JsValue, JsError> {
        let mut chunks = chunker::chunkify(&self.input_segments, tolerance, min_distance);

        let manual_paces: ManualPaces = serde_wasm_bindgen::from_value(manual_paces_val)
            .map_err(|e| JsError::new(&format!("Invalid manual paces: {}", e)))?;
        chunker::apply_manual_paces(
            &mut chunks,
            &manual_paces,
            degradation_factor,
            fatigue_exponent,
            wall_onset,
            downhill_degradation,
        );

        serde_wasm_bindgen::to_value(&chunks)
            .map_err(|e| JsError::new(&format!("Failed to serialize chunks: {}", e)))
    }

    pub fn generate_csv(
        &self,
        tolerance: f64,
        min_distance: f64,
        manual_paces_val: JsValue,
        degradation_factor: f64,
        fatigue_exponent: f64,
        wall_onset: f64,
        downhill_degradation: f64,
    ) -> Result<String, JsError> {
        let mut chunks = chunker::chunkify(&self.input_segments, tolerance, min_distance);

        let manual_paces: ManualPaces = serde_wasm_bindgen::from_value(manual_paces_val)
            .map_err(|e| JsError::new(&format!("Invalid manual paces: {}", e)))?;
        chunker::apply_manual_paces(
            &mut chunks,
            &manual_paces,
            degradation_factor,
            fatigue_exponent,
            wall_onset,
            downhill_degradation,
        );

        let mut buffer = Vec::new();
        output::write_csv_to_writer(&chunks, &mut buffer)
            .map_err(|e| JsError::new(&format!("Failed to generate CSV: {}", e)))?;

        String::from_utf8(buffer)
            .map_err(|e| JsError::new(&format!("Invalid UTF-8 in CSV: {}", e)))
    }
}
