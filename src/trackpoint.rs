/// A single point along a GPS track with position, elevation, and computed metrics.
#[derive(Debug, Clone)]
pub struct TrackPoint {
    pub lat: f64,
    pub lon: f64,
    pub elevation: f64,
}

/// A processed segment between two consecutive track points.
#[derive(Debug, Clone)]
pub struct Segment {
    pub start: TrackPoint,
    pub end: TrackPoint,
    pub distance_m: f64,
    pub elevation_gain: f64,
    pub elevation_loss: f64,
    pub gradient_pct: f64,
}
