use crate::ref_parser::RefSegment;

/// Width of each gradient bucket in percentage points.
const BUCKET_WIDTH: f64 = 1.0;

/// The terrain type of the target race.
///
/// When the reference activity was recorded on roads but the target race is on
/// trail, steep downhill paces will be unrealistically fast because the model
/// extrapolates from flatter road data. The `Trail` variant applies a
/// progressive penalty for gradients steeper than −5 % so that on-ground
/// reality is better approximated.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum TerrainType {
    /// Road race — no downhill penalty (default).
    #[default]
    Road,
    /// Trail race — applies a progressive slow-down for steep descents.
    ///
    /// Penalty is linear: +5 % pace per percentage point steeper than −5 %;
    /// i.e. at −10 % → ×1.25, at −15 % → ×1.50, at −20 % → ×1.75.
    Trail,
}

impl TerrainType {
    /// Multiplier to apply to pace (seconds/metre) for a given gradient.
    ///
    /// Returns 1.0 for road terrain or gradients above the threshold.
    pub fn downhill_penalty(self, gradient_pct: f64) -> f64 {
        const THRESHOLD: f64 = -5.0;
        const RATE: f64 = 0.05; // 5 % extra per 1 % steeper than the threshold
        if matches!(self, Self::Road) || gradient_pct >= THRESHOLD {
            1.0
        } else {
            1.0 + (-gradient_pct - THRESHOLD.abs()) * RATE
        }
    }
}
/// Minimum total distance (m) needed in a bucket to be considered reliable.
const MIN_BUCKET_DISTANCE_M: f64 = 100.0;

/// How the runner's performance evolves over the race.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DegradationProfile {
    /// No change in performance throughout the race.
    Stable,
    /// Custom factor (e.g. 0.10 for 10% slowdown).
    Custom(f64),
}

impl DegradationProfile {
    pub fn total_factor(self) -> f64 {
        match self {
            Self::Stable => 0.0,
            Self::Custom(f) => f,
        }
    }

    pub fn multiplier_at(self, progress: f64, exponent: f64) -> f64 {
        1.0 + self.total_factor() * progress.powf(exponent)
    }

    /// Like `multiplier_at`, but with a "wall": fatigue follows the ordinary
    /// power-law shape up to `wall_onset` (a fraction of race progress), then
    /// the *remaining* severity budget is compressed into the rest of the
    /// race using the same exponent — producing a visible plateau-then-crash
    /// around the onset point instead of a smooth curve from the start.
    ///
    /// This mirrors the "hitting the wall" glycogen-depletion phenomenon
    /// described mathematically in Rapoport BI (2010), "Metabolic Factors
    /// Limiting Performance in Marathon Runners" (PLoS Comput Biol 6(10)),
    /// where performance holds up reasonably well until a depletion point,
    /// then falls off sharply. `wall_onset = 0` reduces exactly to
    /// `multiplier_at` (no wall, gradual fatigue from the start).
    pub fn multiplier_at_wall(self, progress: f64, exponent: f64, wall_onset: f64) -> f64 {
        let factor = self.total_factor();
        if factor <= 0.0 {
            return 1.0;
        }
        let p = progress.clamp(0.0, 1.0);
        let o = wall_onset.clamp(0.0, 0.95);
        let shape = if o <= 1e-9 || p <= o {
            p.powf(exponent)
        } else {
            let pre = o.powf(exponent);
            let t = (p - o) / (1.0 - o);
            pre + (1.0 - pre) * t.powf(exponent)
        };
        1.0 + factor * shape
    }
}

/// Extra pace penalty for downhill running that accumulates through the
/// race, modelling eccentric quadriceps damage rather than aerobic fatigue —
/// distinct from (and stacked on top of) `DegradationProfile`, which applies
/// uniformly to every gradient.
///
/// Grounded in Millet GY et al. (2011), "Neuromuscular Consequences of an
/// Extreme Mountain Ultra-Marathon" (PLoS ONE 6(2):e17059), a UTMB field
/// study showing peripheral (quadriceps) fatigue tracks the race's downhill
/// sections, and Vernillo G et al. (2017), "Biomechanics and Physiology of
/// Uphill and Downhill Running" (Sports Medicine 47:615-629), which reviews
/// the same eccentric-loading mechanism. Only applies to descents; scales
/// with both how far into the race the runner is and how steep the descent
/// is, saturating at -15% (steeper descents don't multiply the damage
/// further — control/technique becomes the limiter, not extra eccentric
/// load).
pub fn downhill_damage_multiplier(progress: f64, gradient_pct: f64, downhill_factor: f64) -> f64 {
    if downhill_factor <= 0.0 || gradient_pct >= 0.0 {
        return 1.0;
    }
    let steepness = (-gradient_pct / 15.0).min(1.0);
    1.0 + downhill_factor * steepness * progress.clamp(0.0, 1.0)
}

/// Metabolic cost of walking/running (J/(kg·m)) as a function of gradient,
/// from Minetti et al. (2002), "Energy cost of walking and running at
/// extreme uphill and downhill slopes" (J Appl Physiol 93:1039-1046). This
/// is the same cost curve behind most "Grade Adjusted Pace" calculators.
/// `gradient` is the slope as a fraction (0.10 = 10%); the polynomial is
/// only valid over the paper's tested range (±45%), so it's clamped there.
pub fn minetti_cost(gradient: f64) -> f64 {
    let i = gradient.clamp(-0.45, 0.45);
    155.4 * i.powi(5) - 30.4 * i.powi(4) - 43.3 * i.powi(3) + 46.3 * i.powi(2) + 19.5 * i + 3.6
}

/// Pace multiplier relative to flat ground at a given gradient, derived from
/// the ratio of Minetti's metabolic cost at that gradient vs. flat cost.
/// >1.0 = slower than flat, <1.0 = faster (e.g. a gentle downhill).
pub fn minetti_pace_multiplier(gradient_pct: f64) -> f64 {
    minetti_cost(gradient_pct / 100.0) / minetti_cost(0.0)
}

/// Manual pace configuration for different gradient zones.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ManualPaces {
    pub flat: f64,           // sec/m
    pub gentle_uphill: f64,
    pub moderate_uphill: f64,
    pub steep_uphill: f64,
    pub very_steep_uphill: f64,
    pub gentle_downhill: f64,
    pub moderate_downhill: f64,
    pub steep_downhill: f64,
    pub very_steep_downhill: f64,
}

impl ManualPaces {
    /// Representative gradient (%) for each pace zone. These anchor points
    /// are used both to derive Minetti-model defaults (`from_flat_pace`) and
    /// to interpolate a smooth pace-vs-gradient curve (`pace_at`), so editing
    /// one zone affects a continuous range around it instead of a hard-edged
    /// bucket.
    const VERY_STEEP_DOWNHILL_GRAD: f64 = -13.0;
    const STEEP_DOWNHILL_GRAD: f64 = -8.0;
    const MODERATE_DOWNHILL_GRAD: f64 = -4.5;
    const GENTLE_DOWNHILL_GRAD: f64 = -2.0;
    const GENTLE_UPHILL_GRAD: f64 = 2.0;
    const MODERATE_UPHILL_GRAD: f64 = 4.5;
    const STEEP_UPHILL_GRAD: f64 = 8.0;
    const VERY_STEEP_UPHILL_GRAD: f64 = 13.0;

    /// The 9 (gradient, pace) anchor points, sorted ascending by gradient.
    fn anchors(&self) -> [(f64, f64); 9] {
        [
            (Self::VERY_STEEP_DOWNHILL_GRAD, self.very_steep_downhill),
            (Self::STEEP_DOWNHILL_GRAD, self.steep_downhill),
            (Self::MODERATE_DOWNHILL_GRAD, self.moderate_downhill),
            (Self::GENTLE_DOWNHILL_GRAD, self.gentle_downhill),
            (0.0, self.flat),
            (Self::GENTLE_UPHILL_GRAD, self.gentle_uphill),
            (Self::MODERATE_UPHILL_GRAD, self.moderate_uphill),
            (Self::STEEP_UPHILL_GRAD, self.steep_uphill),
            (Self::VERY_STEEP_UPHILL_GRAD, self.very_steep_uphill),
        ]
    }

    /// Estimate pace (sec/m) at a gradient by linearly interpolating between
    /// the zone anchor points, instead of snapping to the nearest zone. This
    /// removes the hard pace jumps that used to occur right at zone
    /// boundaries (e.g. 2.9% vs 3.1%).
    pub fn pace_at(&self, gradient_pct: f64) -> f64 {
        let anchors = self.anchors();
        let pos = anchors.as_slice().partition_point(|(g, _)| *g < gradient_pct);
        if pos == 0 {
            return anchors[0].1;
        }
        if pos >= anchors.len() {
            return anchors[anchors.len() - 1].1;
        }
        let (g0, p0) = anchors[pos - 1];
        let (g1, p1) = anchors[pos];
        let t = if g1 > g0 { (gradient_pct - g0) / (g1 - g0) } else { 0.0 };
        p0 + t * (p1 - p0)
    }

    /// Derive a full set of zone paces from a flat-ground pace, using
    /// Minetti's metabolic cost model to predict how much slower/faster each
    /// gradient should be. `hill_sensitivity` scales how much gradient
    /// affects pace *in the slowing direction* for both uphill and downhill:
    /// 1.0 follows Minetti's curve as-is (uphill slower, moderate downhill
    /// faster); above 1.0 uphill gets slower still *and* the downhill speed
    /// bonus shrinks (a runner who suffers more on hills overall, e.g.
    /// cautious on technical descents); below 1.0 uphill is less punishing
    /// and the downhill bonus grows (a runner hills barely touch either way).
    ///
    /// Naively scaling the signed Minetti deviation by `hill_sensitivity`
    /// would amplify the *downhill speed-up* as sensitivity increases past
    /// 1.0 — the opposite of what "more affected by hills" should mean. So
    /// uphill (deviation ≥ 0) is scaled by `hill_sensitivity` directly, while
    /// downhill (deviation < 0) is scaled by `2.0 - hill_sensitivity`: equal
    /// to 1.0 (unchanged) at sensitivity 1.0, shrinking the benefit as
    /// sensitivity rises, growing it as sensitivity falls.
    ///
    /// Minimum slowdown factor applied to `very_steep_downhill` relative to
    /// `steep_downhill`. Minetti's pure metabolic-cost curve keeps predicting
    /// a speed *benefit* well past -10% (it only turns back upward around
    /// -20%), because it models flat, controlled treadmill terrain. Real
    /// trail descents don't let you cash that benefit in indefinitely: past
    /// a point, control, confidence, and limiting eccentric muscle damage on
    /// the quads force a real slowdown. Without this floor, "very steep"
    /// would end up *faster* than "steep" at every hill-sensitivity setting.
    const VERY_STEEP_DOWNHILL_BRAKING_FACTOR: f64 = 1.15;

    /// The result is meant as a starting point — every field stays
    /// individually editable afterwards.
    pub fn from_flat_pace(flat_spm: f64, hill_sensitivity: f64) -> Self {
        let pace_at_grad = |grad_pct: f64| -> f64 {
            let deviation = minetti_pace_multiplier(grad_pct) - 1.0;
            let scale = if deviation >= 0.0 { hill_sensitivity } else { 2.0 - hill_sensitivity };
            flat_spm * (1.0 + deviation * scale)
        };

        let steep_downhill = pace_at_grad(Self::STEEP_DOWNHILL_GRAD);
        let very_steep_downhill = pace_at_grad(Self::VERY_STEEP_DOWNHILL_GRAD)
            .max(steep_downhill * Self::VERY_STEEP_DOWNHILL_BRAKING_FACTOR);

        ManualPaces {
            flat: flat_spm,
            gentle_uphill: pace_at_grad(Self::GENTLE_UPHILL_GRAD),
            moderate_uphill: pace_at_grad(Self::MODERATE_UPHILL_GRAD),
            steep_uphill: pace_at_grad(Self::STEEP_UPHILL_GRAD),
            very_steep_uphill: pace_at_grad(Self::VERY_STEEP_UPHILL_GRAD),
            gentle_downhill: pace_at_grad(Self::GENTLE_DOWNHILL_GRAD),
            moderate_downhill: pace_at_grad(Self::MODERATE_DOWNHILL_GRAD),
            steep_downhill,
            very_steep_downhill,
        }
    }
}

/// A gradient-to-pace model derived from a reference activity.
///
/// The model stores the average pace (seconds per metre) for each gradient
/// bucket of `BUCKET_WIDTH`% width.  Missing buckets are filled by linear
/// interpolation from the nearest neighbours.
pub struct PaceModel {
    /// Sorted list of (gradient_pct, fresh_pace, total_factor, exponent)
    buckets: Vec<(f64, f64, f64, f64)>,
    pub suggested_factor: f64,
    pub suggested_exponent: f64,
}

impl PaceModel {
    pub fn from_segments(segments: &[RefSegment]) -> Self {
        use std::collections::HashMap;
        let total_dist: f64 = segments.iter().map(|s| s.distance_m).sum();
        
        // 1. Group segments by bucket and quarter
        let mut quarter_acc: [HashMap<i64, (f64, f64)>; 4] = [
            HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new()
        ];
        
        let mut current_cumul = 0.0;
        for seg in segments {
            let progress = current_cumul / total_dist;
            let q_idx = (progress * 4.0).floor().min(3.0) as usize;
            let key = (seg.gradient_pct / BUCKET_WIDTH).round() as i64;
            let entry = quarter_acc[q_idx].entry(key).or_insert((0.0, 0.0));
            entry.0 += seg.distance_m;
            entry.1 += seg.elapsed_secs;
            current_cumul += seg.distance_m;
        }

        // 2. Calculate Global Stats (as fallback)
        let q_paces: Vec<f64> = quarter_acc.iter().map(|q| {
            let d: f64 = q.values().map(|v| v.0).sum();
            let s: f64 = q.values().map(|v| v.1).sum();
            if d > 100.0 { s / d } else { 0.0 }
        }).collect();

        let mut global_f = 0.0;
        let mut global_k = 1.0;
        if q_paces[0] > 0.0 && q_paces[3] > 0.0 {
            global_f = (q_paces[3] / q_paces[0]) - 1.0;
            let mid_pace = (q_paces[1] + q_paces[2]) / 2.0;
            let mid_drop = (mid_pace / q_paces[0]) - 1.0;
            if mid_drop > 0.001 && global_f > 0.001 {
                global_k = (global_f / mid_drop).log2().clamp(1.0, 4.0);
            }
        }

        // 3. Calculate Per-Bucket Stats
        let mut buckets: Vec<(f64, f64, f64, f64)> = Vec::new();
        let mut all_keys: Vec<i64> = Vec::new();
        for q in &quarter_acc {
            for k in q.keys() { if !all_keys.contains(k) { all_keys.push(*k); } }
        }

        for key in all_keys {
            let p: Vec<f64> = quarter_acc.iter().map(|q| {
                if let Some(v) = q.get(&key) {
                    if v.0 > 20.0 { v.1 / v.0 } else { 0.0 }
                } else { 0.0 }
            }).collect();

            let total_d: f64 = quarter_acc.iter().filter_map(|q| q.get(&key)).map(|v| v.0).sum();
            if total_d < MIN_BUCKET_DISTANCE_M { continue; }

            // Find first available pace as "fresh"
            let fresh_pace = p.iter().copied().find(|&v| v > 0.0).unwrap_or(0.0);
            let mut local_f = global_f;
            let mut local_k = global_k;

            // If we have data at the end, calculate local factor
            if fresh_pace > 0.0 && p[3] > 0.0 {
                local_f = (p[3] / fresh_pace) - 1.0;
                
                // If we also have data in the middle, calculate local shape
                let mid_pace = if p[1] > 0.0 && p[2] > 0.0 { (p[1]+p[2])/2.0 } 
                              else if p[1] > 0.0 { p[1] } 
                              else { p[2] };
                
                if mid_pace > 0.0 {
                    let mid_drop = (mid_pace / fresh_pace) - 1.0;
                    if mid_drop > 0.001 && local_f > 0.001 {
                        local_k = (local_f / mid_drop).log2().clamp(1.0, 4.0);
                    }
                }
            }

            buckets.push((key as f64 * BUCKET_WIDTH, fresh_pace, local_f.max(0.0), local_k));
        }
        
        buckets.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        PaceModel { buckets, suggested_factor: global_f, suggested_exponent: global_k }
    }

    /// Estimate (pace, factor, exponent) for a given gradient.
    pub fn pace_at(&self, gradient_pct: f64) -> Option<(f64, f64, f64)> {
        if self.buckets.is_empty() { return None; }
        let pos = self.buckets.partition_point(|(g, _, _, _)| *g < gradient_pct);

        let (g0, p0, f0, k0);
        let (g1, p1, f1, k1);
        let t;

        if pos == 0 {
            return Some((self.buckets[0].1, self.buckets[0].2, self.buckets[0].3));
        } else if pos >= self.buckets.len() {
            let last = self.buckets.last().unwrap();
            return Some((last.1, last.2, last.3));
        } else {
            let b0 = self.buckets[pos - 1];
            let b1 = self.buckets[pos];
            g0 = b0.0; p0 = b0.1; f0 = b0.2; k0 = b0.3;
            g1 = b1.0; p1 = b1.1; f1 = b1.2; k1 = b1.3;
            t = (gradient_pct - g0) / (g1 - g0);
        }

        Some((
            p0 + t * (p1 - p0),
            f0 + t * (f1 - f0),
            k0 + t * (k1 - k0)
        ))
    }
    
    pub fn entries(&self) -> impl Iterator<Item = (f64, f64)> + '_ {
        self.buckets.iter().map(|(g, p, _, _)| (*g, p * 1000.0 / 60.0))
    }
}

/// Format a duration in total seconds as "HH:MM:SS".
pub fn format_duration(total_secs: f64) -> String {
    let secs = total_secs.round() as u64;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

/// Format pace in seconds/metre as "MM:SS /km".
pub fn format_pace(secs_per_metre: f64) -> String {
    let spm_km = secs_per_metre * 1000.0;
    let m = spm_km as u64 / 60;
    let s = spm_km as u64 % 60;
    format!("{:02}:{:02}", m, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(gradient: f64, dist: f64, secs: f64) -> RefSegment {
        RefSegment { distance_m: dist, elapsed_secs: secs, gradient_pct: gradient }
    }

    #[test]
    fn model_returns_known_pace_for_exact_gradient() {
        // 500 m at flat at 6 min/km → 0.36 s/m
        let segments = vec![seg(0.0, 500.0, 180.0)];
        let model = PaceModel::from_segments(&segments);
        let (pace, _, _) = model.pace_at(0.0).unwrap();
        let diff = (pace - 0.36).abs();
        assert!(diff < 0.001, "Expected ~0.36 s/m, got {}", pace);
    }

    #[test]
    fn model_interpolates_between_buckets() {
        // 0% → 0.36 s/m (500 m), 2% → 0.42 s/m (500 m), ask 1% → ~0.39 s/m
        let segments = vec![
            seg(0.0, 500.0, 180.0),
            seg(2.0, 500.0, 210.0),
        ];
        let model = PaceModel::from_segments(&segments);
        let (pace, _, _) = model.pace_at(1.0).unwrap();
        let expected = (0.36 + 0.42) / 2.0;
        let diff = (pace - expected).abs();
        assert!(diff < 0.01, "Expected ~{}, got {}", expected, pace);
    }

    #[test]
    fn minetti_cost_is_symmetric_around_zero_only_at_zero() {
        // Flat cost is the polynomial's constant-dominated baseline (~3.6).
        let flat = minetti_cost(0.0);
        assert!((flat - 3.6).abs() < 0.01, "expected ~3.6, got {flat}");
    }

    #[test]
    fn minetti_multiplier_is_one_at_flat() {
        assert!((minetti_pace_multiplier(0.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn minetti_multiplier_increases_with_uphill_steepness() {
        let gentle = minetti_pace_multiplier(2.0);
        let steep = minetti_pace_multiplier(10.0);
        assert!(gentle > 1.0, "gentle uphill should be slower than flat, got {gentle}");
        assert!(steep > gentle, "steeper uphill should be slower still, got {steep} vs {gentle}");
    }

    #[test]
    fn minetti_multiplier_is_faster_on_gentle_downhill() {
        let mult = minetti_pace_multiplier(-5.0);
        assert!(mult < 1.0, "gentle downhill should be faster than flat, got {mult}");
    }

    fn manual_paces_from(flat: f64) -> ManualPaces {
        ManualPaces::from_flat_pace(flat, 1.0)
    }

    #[test]
    fn manual_paces_from_flat_pace_round_trips_flat() {
        let paces = manual_paces_from(0.36);
        assert!((paces.flat - 0.36).abs() < 1e-9);
    }

    #[test]
    fn manual_paces_from_flat_pace_is_monotonic_uphill() {
        let paces = manual_paces_from(0.36);
        assert!(paces.flat < paces.gentle_uphill);
        assert!(paces.gentle_uphill < paces.moderate_uphill);
        assert!(paces.moderate_uphill < paces.steep_uphill);
        assert!(paces.steep_uphill < paces.very_steep_uphill);
    }

    #[test]
    fn very_steep_downhill_is_never_faster_than_steep_downhill() {
        // Minetti's raw curve alone would make very_steep faster than steep
        // (it doesn't turn back upward until ~-20%) — the braking floor must
        // override that at every hill-sensitivity setting the UI allows.
        for sensitivity in [0.5, 0.8, 1.0, 1.2, 1.6] {
            let paces = ManualPaces::from_flat_pace(0.36, sensitivity);
            assert!(
                paces.very_steep_downhill >= paces.steep_downhill,
                "at sensitivity {sensitivity}: very_steep_downhill ({}) should be >= steep_downhill ({})",
                paces.very_steep_downhill, paces.steep_downhill
            );
        }
    }

    #[test]
    fn hill_sensitivity_above_one_slows_both_uphill_and_downhill() {
        let flat = 0.36;
        let baseline = ManualPaces::from_flat_pace(flat, 1.0);
        let sensitive = ManualPaces::from_flat_pace(flat, 1.5);

        // Uphill: more sensitive to hills => slower (higher sec/m).
        assert!(sensitive.moderate_uphill > baseline.moderate_uphill);

        // Downhill: more sensitive to hills => *less* of a speed benefit
        // (still faster than flat here, just not as much), not faster.
        assert!(
            sensitive.moderate_downhill > baseline.moderate_downhill,
            "higher hill sensitivity should shrink the downhill speed bonus, got {} (sensitive) vs {} (baseline)",
            sensitive.moderate_downhill, baseline.moderate_downhill
        );
        assert!(sensitive.moderate_downhill < flat, "some downhill benefit should remain at sensitivity 1.5");
    }

    #[test]
    fn hill_sensitivity_below_one_gives_easier_uphill_and_bigger_downhill_bonus() {
        let flat = 0.36;
        let baseline = ManualPaces::from_flat_pace(flat, 1.0);
        let relaxed = ManualPaces::from_flat_pace(flat, 0.5);

        assert!(relaxed.moderate_uphill < baseline.moderate_uphill, "lower sensitivity should ease the uphill penalty");
        assert!(relaxed.moderate_downhill < baseline.moderate_downhill, "lower sensitivity should grow the downhill bonus");
    }

    #[test]
    fn manual_paces_pace_at_interpolates_smoothly_across_a_zone_boundary() {
        let paces = manual_paces_from(0.36);
        // Just below and just above the gentle/moderate uphill boundary (4.5%)
        // should differ by a small, continuous amount, not jump by the full
        // gentle-to-moderate gap the way the old bucket lookup did.
        let just_below = paces.pace_at(4.4);
        let just_above = paces.pace_at(4.6);
        let full_zone_gap = (paces.moderate_uphill - paces.gentle_uphill).abs();
        assert!(
            (just_above - just_below).abs() < full_zone_gap * 0.2,
            "expected a small step across the boundary, got {} vs {} (zone gap {})",
            just_below, just_above, full_zone_gap
        );
    }

    #[test]
    fn manual_paces_pace_at_matches_anchor_at_representative_gradient() {
        let paces = manual_paces_from(0.36);
        let diff = (paces.pace_at(8.0) - paces.steep_uphill).abs();
        assert!(diff < 1e-9, "expected pace_at(8.0) to match steep_uphill exactly, got diff {diff}");
    }

    #[test]
    fn manual_paces_pace_at_clamps_beyond_extreme_zones() {
        let paces = manual_paces_from(0.36);
        assert_eq!(paces.pace_at(50.0), paces.very_steep_uphill);
        assert_eq!(paces.pace_at(-50.0), paces.very_steep_downhill);
    }

    #[test]
    fn multiplier_at_wall_matches_plain_curve_when_onset_is_zero() {
        let profile = DegradationProfile::Custom(0.20);
        for p in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let plain = profile.multiplier_at(p, 2.0);
            let walled = profile.multiplier_at_wall(p, 2.0, 0.0);
            assert!((plain - walled).abs() < 1e-9, "at progress {p}: {plain} vs {walled}");
        }
    }

    #[test]
    fn multiplier_at_wall_reaches_full_factor_at_finish() {
        let profile = DegradationProfile::Custom(0.20);
        let m = profile.multiplier_at_wall(1.0, 2.5, 0.6);
        assert!((m - 1.20).abs() < 1e-9, "expected full +20% at the finish, got {m}");
    }

    #[test]
    fn multiplier_at_wall_is_gentler_than_plain_curve_just_after_onset() {
        // The whole point of the wall: right after onset, the walled curve
        // should lag behind where a plain power law would already be,
        // because its budget was held back for the "crash" phase.
        let profile = DegradationProfile::Custom(0.20);
        let onset = 0.6;
        let just_after = onset + 0.05;
        let plain = profile.multiplier_at(just_after, 2.0);
        let walled = profile.multiplier_at_wall(just_after, 2.0, onset);
        assert!(walled < plain, "expected walled ({walled}) < plain ({plain}) just after onset");
    }

    #[test]
    fn downhill_damage_multiplier_only_applies_to_descents() {
        assert_eq!(downhill_damage_multiplier(0.8, 5.0, 0.5), 1.0, "uphill should be unaffected");
        assert_eq!(downhill_damage_multiplier(0.8, 0.0, 0.5), 1.0, "flat should be unaffected");
        assert!(downhill_damage_multiplier(0.8, -10.0, 0.5) > 1.0, "steep descent should be slowed");
    }

    #[test]
    fn downhill_damage_multiplier_grows_with_progress_and_steepness() {
        let early = downhill_damage_multiplier(0.1, -10.0, 0.5);
        let late = downhill_damage_multiplier(0.9, -10.0, 0.5);
        assert!(late > early, "damage should accumulate as the race goes on");

        let gentle = downhill_damage_multiplier(0.9, -2.0, 0.5);
        let steep = downhill_damage_multiplier(0.9, -20.0, 0.5);
        assert!(steep > gentle, "steeper descents should suffer more damage");
    }

    #[test]
    fn downhill_damage_multiplier_is_off_when_factor_is_zero() {
        assert_eq!(downhill_damage_multiplier(1.0, -15.0, 0.0), 1.0);
    }
}
