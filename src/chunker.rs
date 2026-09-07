use serde::Serialize;

use crate::pace_model::{
    downhill_damage_multiplier, format_duration, format_pace, DegradationProfile, ManualPaces,
    PaceModel, TerrainType,
};
use crate::trackpoint::Segment;

/// A chunk represents a section of the route with a roughly consistent gradient.
///
/// The chunking algorithm merges consecutive segments as long as the overall
/// gradient of the merged chunk stays within `tolerance` percentage points of
/// the gradient that was established when the chunk started. When a new segment
/// would push the chunk's gradient outside that tolerance, a new chunk begins.
#[derive(Debug, Clone, Serialize)]
pub struct Chunk {
    pub chunk_number: usize,
    pub distance_km: f64,
    pub cumulative_km: f64,
    pub elevation_gain_m: f64,
    pub elevation_loss_m: f64,
    pub cumulative_gain_m: f64,
    pub cumulative_loss_m: f64,
    pub gradient_pct: f64,
    pub classification: String,
    /// Applied fatigue at this point in the race (percentage multiplier).
    pub fatigue_pct: f64,
    /// Estimated pace in "MM:SS /km" format (only present when a reference activity is provided).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_pace: Option<String>,
    /// Estimated time for this chunk as "HH:MM:SS" (only present when a reference activity is provided).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_time: Option<String>,
    /// Cumulative estimated time from start as "HH:MM:SS".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cumulative_time: Option<String>,
}

impl Chunk {
    fn classify_gradient(gradient: f64) -> String {
        let abs = gradient.abs();
        let direction = if gradient >= 0.0 { "uphill" } else { "downhill" };
        if abs < 1.0 {
            "flat".to_string()
        } else if abs < 3.0 {
            format!("gentle {direction}")
        } else if abs < 6.0 {
            format!("moderate {direction}")
        } else if abs < 10.0 {
            format!("steep {direction}")
        } else {
            format!("very steep {direction}")
        }
    }
}

/// Enrich a list of chunks with pace and time estimates from a pace model.
/// This version is for AUTOMATIC mode (using reference activity).
pub fn apply_pace_model(chunks: &mut Vec<Chunk>, model: &PaceModel, terrain: TerrainType) {
    let total_km: f64 = chunks.iter().map(|c| c.distance_km).sum();
    let mut cumulative_secs = 0.0_f64;

    for chunk in chunks.iter_mut() {
        if let Some((spm, factor, exponent)) = model.pace_at(chunk.gradient_pct) {
            let mid_km = chunk.cumulative_km - chunk.distance_km / 2.0;
            let progress = if total_km > 0.0 { (mid_km / total_km).clamp(0.0, 1.0) } else { 0.0 };
            
            let profile = DegradationProfile::Custom(factor);
            let multiplier = profile.multiplier_at(progress, exponent);
            let degraded_spm = spm * multiplier * terrain.downhill_penalty(chunk.gradient_pct);
            
            let degraded_spm = degraded_spm.clamp(2.0 * 60.0 / 1000.0, 25.0 * 60.0 / 1000.0);
            let chunk_secs = degraded_spm * chunk.distance_km * 1000.0;
            cumulative_secs += chunk_secs;
            
            chunk.fatigue_pct = (multiplier - 1.0) * 100.0;
            chunk.estimated_pace = Some(format_pace(degraded_spm));
            chunk.estimated_time = Some(format_duration(chunk_secs));
            chunk.cumulative_time = Some(format_duration(cumulative_secs));
        }
    }
}

/// Enrich a list of chunks with manual paces and a custom degradation model.
///
/// `degradation_factor`/`fatigue_exponent`/`wall_onset` control the overall
/// aerobic fatigue curve (see `DegradationProfile::multiplier_at_wall`);
/// `downhill_degradation` adds an independent, descent-only penalty that
/// grows through the race (see `downhill_damage_multiplier`).
pub fn apply_manual_paces(
    chunks: &mut Vec<Chunk>,
    manual_paces: &ManualPaces,
    degradation_factor: f64,
    fatigue_exponent: f64,
    wall_onset: f64,
    downhill_degradation: f64,
) {
    let total_km: f64 = chunks.iter().map(|c| c.distance_km).sum();
    let mut cumulative_secs = 0.0_f64;
    let profile = DegradationProfile::Custom(degradation_factor);

    for chunk in chunks.iter_mut() {
        let spm = manual_paces.pace_at(chunk.gradient_pct);
        let mid_km = chunk.cumulative_km - chunk.distance_km / 2.0;
        let progress = if total_km > 0.0 { (mid_km / total_km).clamp(0.0, 1.0) } else { 0.0 };

        let multiplier = profile.multiplier_at_wall(progress, fatigue_exponent, wall_onset)
            * downhill_damage_multiplier(progress, chunk.gradient_pct, downhill_degradation);
        let degraded_spm = spm * multiplier;

        let chunk_secs = degraded_spm * chunk.distance_km * 1000.0;
        cumulative_secs += chunk_secs;

        chunk.fatigue_pct = (multiplier - 1.0) * 100.0;
        chunk.estimated_pace = Some(format_pace(degraded_spm));
        chunk.estimated_time = Some(format_duration(chunk_secs));
        chunk.cumulative_time = Some(format_duration(cumulative_secs));
    }
}

/// Split a list of GPS segments into chunks of consistent gradient.
///
/// `tolerance` is the maximum allowed deviation (in percentage points) between
/// the running gradient of the current chunk and a candidate segment's gradient
/// before a new chunk is started.
///
/// `min_chunk_distance_m` is the minimum distance a chunk should reach before
/// it can be split. This prevents very tiny chunks caused by noisy GPS data.
pub fn chunkify(segments: &[Segment], tolerance: f64, min_chunk_distance_m: f64) -> Vec<Chunk> {
    if segments.is_empty() {
        return Vec::new();
    }

    let mut chunks: Vec<Chunk> = Vec::new();
    let mut chunk_number: usize = 1;

    // Accumulated values for the current chunk.
    let mut chunk_distance = 0.0_f64;
    let mut chunk_gain = 0.0_f64;
    let mut chunk_loss = 0.0_f64;
    let mut cumulative_distance = 0.0_f64;
    let mut cumulative_gain = 0.0_f64;
    let mut cumulative_loss = 0.0_f64;

    for (i, seg) in segments.iter().enumerate() {
        let current_gradient = if chunk_distance > 0.0 {
            ((chunk_gain - chunk_loss) / chunk_distance) * 100.0
        } else {
            seg.gradient_pct
        };

        // Decide whether to continue the current chunk or start a new one.
        // Compare the individual segment's gradient against the chunk's running
        // gradient. This prevents gradual drift that would blur distinct sections.
        let should_split = chunk_distance >= min_chunk_distance_m
            && (seg.gradient_pct - current_gradient).abs() > tolerance
            && i > 0;

        if should_split {
            // Finalise the current chunk.
            let gradient = if chunk_distance > 0.0 {
                ((chunk_gain - chunk_loss) / chunk_distance) * 100.0
            } else {
                0.0
            };
            
            // Internal cumulative distance update BEFORE rounding
            cumulative_distance += chunk_distance;
            cumulative_gain += chunk_gain;
            cumulative_loss += chunk_loss;

            chunks.push(Chunk {
                chunk_number,
                distance_km: chunk_distance / 1000.0, // High precision
                cumulative_km: cumulative_distance / 1000.0,
                elevation_gain_m: chunk_gain,
                elevation_loss_m: chunk_loss,
                cumulative_gain_m: cumulative_gain,
                cumulative_loss_m: cumulative_loss,
                gradient_pct: gradient,
                classification: Chunk::classify_gradient(gradient),
                fatigue_pct: 0.0,
                estimated_pace: None,
                estimated_time: None,
                cumulative_time: None,
            });
            chunk_number += 1;

            // Start a new chunk with this segment.
            chunk_distance = seg.distance_m;
            chunk_gain = seg.elevation_gain;
            chunk_loss = seg.elevation_loss;
        } else {
            // Extend the current chunk.
            chunk_distance += seg.distance_m;
            chunk_gain += seg.elevation_gain;
            chunk_loss += seg.elevation_loss;
        }
    }

    // Flush the last chunk.
    let gradient = if chunk_distance > 0.0 {
        ((chunk_gain - chunk_loss) / chunk_distance) * 100.0
    } else {
        0.0
    };
    cumulative_distance += chunk_distance;
    cumulative_gain += chunk_gain;
    cumulative_loss += chunk_loss;

    chunks.push(Chunk {
        chunk_number,
        distance_km: chunk_distance / 1000.0,
        cumulative_km: cumulative_distance / 1000.0,
        elevation_gain_m: chunk_gain,
        elevation_loss_m: chunk_loss,
        cumulative_gain_m: cumulative_gain,
        cumulative_loss_m: cumulative_loss,
        gradient_pct: gradient,
        classification: Chunk::classify_gradient(gradient),
        fatigue_pct: 0.0,
        estimated_pace: None,
        estimated_time: None,
        cumulative_time: None,
    });

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trackpoint::TrackPoint;

    fn make_segment(distance_m: f64, ele_gain: f64, ele_loss: f64, gradient: f64) -> Segment {
        Segment {
            start: TrackPoint { lat: 0.0, lon: 0.0, elevation: 0.0 },
            end: TrackPoint { lat: 0.0, lon: 0.0, elevation: ele_gain - ele_loss },
            distance_m,
            elevation_gain: ele_gain,
            elevation_loss: ele_loss,
            gradient_pct: gradient,
        }
    }

    #[test]
    fn empty_segments_produces_empty_chunks() {
        let chunks = chunkify(&[], 2.0, 200.0);
        assert!(chunks.is_empty());
    }

    #[test]
    fn single_segment_produces_one_chunk() {
        let segments = vec![make_segment(1000.0, 40.0, 0.0, 4.0)];
        let chunks = chunkify(&segments, 2.0, 200.0);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_number, 1);
    }

    #[test]
    fn consistent_gradient_stays_in_one_chunk() {
        // 5 segments all at ~4% gradient
        let segments: Vec<Segment> = (0..5)
            .map(|_| make_segment(300.0, 12.0, 0.0, 4.0))
            .collect();
        let chunks = chunkify(&segments, 2.0, 200.0);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn gradient_change_causes_split() {
        // 3 segments uphill, then 3 segments flat
        let mut segments: Vec<Segment> = (0..3)
            .map(|_| make_segment(500.0, 30.0, 0.0, 6.0))
            .collect();
        segments.extend((0..3).map(|_| make_segment(500.0, 0.0, 0.0, 0.0)));

        let chunks = chunkify(&segments, 2.0, 200.0);
        assert!(chunks.len() >= 2, "Expected at least 2 chunks, got {}", chunks.len());
    }
}
