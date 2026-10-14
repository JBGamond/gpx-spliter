//! Nutrition planner: schedules gel/bar stops over a paced race timeline.
//!
//! Drinks are sipped continuously (back-to-back, in list order, from the
//! start unless a start time is set) and count toward the hourly carb goal. Gels, bars and purées are eaten once
//! the hourly target has "earned" their carbs, then nudged to the nearest
//! moment on terrain gentle enough to eat on.
//!
//! When aid stations are supplied (and `aid_window_minutes > 0`), bars and
//! purées are eaten at an aid station whenever one lies within the window of
//! their ideal time (slope limits don't apply: you're standing still). Bars and
//! purées still come before gels either way.
//!
//! Manual stops are applied on top of the automatic plan without rippling into
//! it: they either move one planned stop to a chosen km, or place a spare unit
//! (one the plan left unused) there.

use serde::{Deserialize, Serialize};

use crate::chunker::Chunk;
use crate::pace_model::format_duration;

const EPS: f64 = 1e-6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProductKind {
    Gel,
    Bar,
    Puree,
    Drink,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Product {
    pub name: String,
    pub kind: ProductKind,
    pub carbs_g: f64,
    pub quantity: u32,
    /// Drinks only: minutes to finish one unit (bottle). Ignored for solids.
    #[serde(default)]
    pub minutes_per_unit: f64,
    /// Drinks only: minutes after the start at which this drink begins. When
    /// absent, it starts as soon as the previous drink in the list ends.
    #[serde(default)]
    pub start_minutes: Option<f64>,
}

impl Product {
    fn is_usable(&self) -> bool {
        self.carbs_g > 0.0
            && self.quantity > 0
            && (self.kind != ProductKind::Drink || self.minutes_per_unit > 0.0)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AidStationInput {
    pub name: String,
    pub distance_km: f64,
}

/// A hand-placed stop at a route distance.
#[derive(Debug, Clone, Deserialize)]
pub struct ManualStopInput {
    /// Caller-chosen id, echoed back as `NutritionStop::manual_id`.
    pub id: String,
    pub product_index: usize,
    /// `Some(n)`: move the n-th planned stop of this product (see
    /// `NutritionStop::occurrence`). `None`: place a spare unit the plan left unused.
    #[serde(default)]
    pub occurrence: Option<usize>,
    pub distance_km: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NutritionSettings {
    pub carbs_per_hour: f64,
    /// Steepest uphill gradient (%) you'll still eat on, e.g. 8.0.
    pub max_uphill_pct: f64,
    /// Steepest downhill gradient magnitude (%) you'll still eat on, e.g. 5.0 blocks < -5%.
    pub max_downhill_pct: f64,
    pub products: Vec<Product>,
    /// Aid stations along the route, used to eat bars and purées.
    #[serde(default)]
    pub aid_stations: Vec<AidStationInput>,
    /// How far (minutes, either side of the ideal moment) a bar or purée may
    /// be moved to reach an aid station. 0 disables aid-station eating.
    #[serde(default)]
    pub aid_window_minutes: f64,
    /// Hand-placed stops, applied after the automatic plan.
    #[serde(default)]
    pub manual_stops: Vec<ManualStopInput>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NutritionStop {
    /// Index into `settings.products`.
    pub product_index: usize,
    pub name: String,
    pub kind: ProductKind,
    pub carbs_g: f64,
    pub ideal_time_secs: f64,
    pub time_secs: f64,
    pub time: String,
    pub distance_km: f64,
    /// Gradient of the chunk the stop sits in.
    pub gradient_pct: f64,
    pub on_hard_slope: bool,
    /// Name of the aid station the stop was snapped to, if any.
    pub aid_station: Option<String>,
    /// For stops of the automatic plan: rank among that product's planned
    /// stops (so a manual move can refer to it). `None` for spare units placed by hand.
    pub occurrence: Option<usize>,
    /// Id of the manual stop that placed or moved this one.
    pub manual_id: Option<String>,
    /// Solids eaten so far (incl. this one) + drink carbs by `time_secs`.
    pub cumulative_carbs_g: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProductUsage {
    pub name: String,
    pub kind: ProductKind,
    pub carried: u32,
    pub used_units: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NutritionPlan {
    pub stops: Vec<NutritionStop>,
    pub total_time_secs: f64,
    pub total_time: String,
    /// `carbs_per_hour * total_time_secs / 3600`.
    pub target_carbs_g: f64,
    pub drink_carbs_g: f64,
    pub solid_carbs_g: f64,
    /// Drink + solid.
    pub planned_carbs_g: f64,
    pub shortfall_g: f64,
    /// One entry per input product, same order.
    pub usage: Vec<ProductUsage>,
    /// Cumulative planned intake (drinks + solids) over time, as a polyline:
    /// solids appear as vertical steps (two points at the same time).
    pub curve: Vec<CurvePoint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CurvePoint {
    pub time_secs: f64,
    pub carbs_g: f64,
}

struct TimedChunk {
    start_s: f64,
    end_s: f64,
    start_km: f64,
    end_km: f64,
    gradient_pct: f64,
}

struct DrinkWindow {
    product_index: usize,
    start: f64,
    end: f64,
    /// Carbs per second while drinking.
    rate: f64,
}

fn build_timeline(chunks: &[Chunk]) -> Option<Vec<TimedChunk>> {
    if chunks.is_empty() {
        return None;
    }
    let mut t = 0.0;
    let mut out = Vec::with_capacity(chunks.len());
    for c in chunks {
        let secs = c.estimated_secs?;
        out.push(TimedChunk {
            start_s: t,
            end_s: t + secs,
            start_km: c.cumulative_km - c.distance_km,
            end_km: c.cumulative_km,
            gradient_pct: c.gradient_pct,
        });
        t += secs;
    }
    Some(out)
}

fn build_drink_windows(products: &[Product]) -> Vec<DrinkWindow> {
    let mut windows = Vec::new();
    // Where the next drink without an explicit start begins: right after the
    // previous drink window ends.
    let mut cursor = 0.0;
    for (i, p) in products.iter().enumerate() {
        if p.kind != ProductKind::Drink || !p.is_usable() {
            continue;
        }
        let unit_secs = p.minutes_per_unit * 60.0;
        let start = match p.start_minutes {
            Some(m) if m.is_finite() && m >= 0.0 => m * 60.0,
            _ => cursor,
        };
        let end = start + f64::from(p.quantity) * unit_secs;
        windows.push(DrinkWindow {
            product_index: i,
            start,
            end,
            rate: p.carbs_g / unit_secs,
        });
        cursor = end;
    }
    windows
}

fn drink_carbs_at(windows: &[DrinkWindow], t: f64) -> f64 {
    windows
        .iter()
        .map(|w| w.rate * (t - w.start).clamp(0.0, w.end - w.start))
        .sum()
}

/// First time at which the solid target `S(t)` reaches `need` grams.
/// `S` is piecewise linear between drink window boundaries.
fn earliest_time_reaching(
    need: f64,
    goal_per_sec: f64,
    windows: &[DrinkWindow],
    total_secs: f64,
) -> Option<f64> {
    let mut breaks = vec![0.0, total_secs];
    for w in windows {
        for b in [w.start, w.end] {
            if b > 0.0 && b < total_secs {
                breaks.push(b);
            }
        }
    }
    breaks.sort_by(f64::total_cmp);

    let solid = |t: f64| goal_per_sec * t - drink_carbs_at(windows, t);
    for pair in breaks.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (sa, sb) = (solid(a), solid(b));
        if sa >= need - EPS {
            return Some(a);
        }
        if sb >= need - EPS {
            return Some(a + (need - sa) / (sb - sa) * (b - a));
        }
    }
    None
}

fn build_curve(windows: &[DrinkWindow], stops: &[NutritionStop], total_secs: f64) -> Vec<CurvePoint> {
    // (time, carbs of a solid eaten at that time; 0 for plain breakpoints)
    let mut events: Vec<(f64, f64)> = vec![(0.0, 0.0), (total_secs, 0.0)];
    for w in windows {
        for b in [w.start, w.end] {
            if b > 0.0 && b < total_secs {
                events.push((b, 0.0));
            }
        }
    }
    // Stops come after breakpoints at equal times; the sort is stable.
    events.extend(stops.iter().map(|s| (s.time_secs, s.carbs_g)));
    events.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut curve = Vec::with_capacity(events.len() * 2);
    let mut solids = 0.0;
    for (t, carbs) in events {
        let drinks = drink_carbs_at(windows, t);
        curve.push(CurvePoint { time_secs: t, carbs_g: drinks + solids });
        if carbs > 0.0 {
            solids += carbs;
            curve.push(CurvePoint { time_secs: t, carbs_g: drinks + solids });
        }
    }
    curve
}

/// Solids that need a stop to eat (hard to handle on the move): eaten first,
/// and the only kinds that go to aid stations.
const BULKY: &[ProductKind] = &[ProductKind::Bar, ProductKind::Puree];
const GELS: &[ProductKind] = &[ProductKind::Gel];

/// Within `kinds`, the product with the highest remaining/quantity ratio wins,
/// ties to the lowest index, so flavors rotate rather than being finished one
/// by one.
fn pick_in(products: &[Product], remaining: &[u32], kinds: &[ProductKind]) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, p) in products.iter().enumerate() {
        if !kinds.contains(&p.kind) || remaining[i] == 0 {
            continue;
        }
        let ratio = f64::from(remaining[i]) / f64::from(p.quantity);
        if best.is_none_or(|(_, r)| ratio > r) {
            best = Some((i, ratio));
        }
    }
    best.map(|(i, _)| i)
}

/// An aid station located on the paced timeline.
struct AidSlot<'a> {
    name: &'a str,
    time: f64,
    chunk: usize,
}

/// Time and chunk index at which the runner reaches `km`, if it's on the route.
fn locate_km(timeline: &[TimedChunk], km: f64) -> Option<(f64, usize)> {
    let chunk = timeline
        .iter()
        .position(|c| km >= c.start_km - EPS && km <= c.end_km + EPS)?;
    let c = &timeline[chunk];
    let span = c.end_km - c.start_km;
    let time = if span > 0.0 {
        c.start_s + ((km - c.start_km) / span).clamp(0.0, 1.0) * (c.end_s - c.start_s)
    } else {
        c.start_s
    };
    Some((time, chunk))
}

/// Maps each aid station onto the timeline; stations off the route are dropped.
fn build_aid_slots<'a>(timeline: &[TimedChunk], stations: &'a [AidStationInput]) -> Vec<AidSlot<'a>> {
    stations
        .iter()
        .filter_map(|s| {
            let (time, chunk) = locate_km(timeline, s.distance_km)?;
            Some(AidSlot { name: &s.name, time, chunk })
        })
        .collect()
}

/// Applies `settings.manual_stops`: moves planned stops and places spare
/// units. Entries that no longer match anything (stale occurrence, no spare
/// unit left, km off the route) are ignored.
fn apply_manual_stops(
    timeline: &[TimedChunk],
    settings: &NutritionSettings,
    stops: &mut Vec<NutritionStop>,
    remaining: &mut [u32],
    usage: &mut [ProductUsage],
) {
    for m in &settings.manual_stops {
        let Some(p) = settings.products.get(m.product_index) else { continue };
        let Some((t, chunk_idx)) = locate_km(timeline, m.distance_km) else { continue };
        let c = &timeline[chunk_idx];

        let stop = match m.occurrence {
            Some(n) => {
                let Some(s) = stops
                    .iter_mut()
                    .find(|s| s.product_index == m.product_index && s.occurrence == Some(n))
                else {
                    continue;
                };
                s
            }
            None => {
                if remaining[m.product_index] == 0 {
                    continue;
                }
                remaining[m.product_index] -= 1;
                usage[m.product_index].used_units += 1.0;
                stops.push(NutritionStop {
                    product_index: m.product_index,
                    name: p.name.clone(),
                    kind: p.kind,
                    carbs_g: p.carbs_g,
                    ideal_time_secs: t,
                    time_secs: t,
                    time: String::new(),
                    distance_km: 0.0,
                    gradient_pct: 0.0,
                    on_hard_slope: false,
                    aid_station: None,
                    occurrence: None,
                    manual_id: None,
                    cumulative_carbs_g: 0.0,
                });
                stops.last_mut().expect("just pushed")
            }
        };
        stop.time_secs = t;
        stop.time = format_duration(t);
        stop.distance_km = km_at(c, t);
        stop.gradient_pct = c.gradient_pct;
        stop.on_hard_slope = false;
        stop.aid_station = None;
        stop.manual_id = Some(m.id.clone());
    }
}

/// Index of the unused station closest to `ideal` (within `window_secs`, and
/// not before `t_prev`).
fn pick_aid_slot(slots: &[AidSlot], used: &[bool], ideal: f64, t_prev: f64, window_secs: f64) -> Option<usize> {
    slots
        .iter()
        .enumerate()
        .filter(|&(i, s)| !used[i] && s.time >= t_prev && (s.time - ideal).abs() <= window_secs)
        .min_by(|(_, a), (_, b)| (a.time - ideal).abs().total_cmp(&(b.time - ideal).abs()))
        .map(|(i, _)| i)
}

fn km_at(c: &TimedChunk, t: f64) -> f64 {
    let dur = c.end_s - c.start_s;
    if dur > 0.0 {
        c.start_km + (t - c.start_s) / dur * (c.end_km - c.start_km)
    } else {
        c.start_km
    }
}

/// Returns `(time, chunk_index, on_hard_slope)`.
fn place_stop(
    timeline: &[TimedChunk],
    settings: &NutritionSettings,
    ideal: f64,
    t_prev: f64,
    total_secs: f64,
) -> (f64, usize, bool) {
    let mut best: Option<(f64, usize, f64)> = None; // (time, chunk, score)
    for (i, c) in timeline.iter().enumerate() {
        let eligible =
            c.gradient_pct >= -settings.max_downhill_pct && c.gradient_pct <= settings.max_uphill_pct;
        if !eligible {
            continue;
        }
        let lo = c.start_s.max(t_prev);
        let hi = c.end_s.min(total_secs);
        if lo > hi {
            continue;
        }
        let candidate = ideal.clamp(lo, hi);
        let score = (candidate - ideal).abs();
        if best.is_none_or(|(_, _, s)| score < s) {
            best = Some((candidate, i, score));
        }
    }
    if let Some((t, i, _)) = best {
        return (t, i, false);
    }

    let t = ideal.max(t_prev).clamp(0.0, total_secs);
    let idx = timeline
        .iter()
        .position(|c| c.start_s <= t && t <= c.end_s)
        .unwrap_or(timeline.len() - 1);
    (t, idx, true)
}

pub fn plan(chunks: &[Chunk], settings: &NutritionSettings) -> NutritionPlan {
    let products = &settings.products;
    let mut usage: Vec<ProductUsage> = products
        .iter()
        .map(|p| ProductUsage {
            name: p.name.clone(),
            kind: p.kind,
            carried: p.quantity,
            used_units: 0.0,
        })
        .collect();

    let Some(timeline) = build_timeline(chunks) else {
        return NutritionPlan {
            stops: Vec::new(),
            total_time_secs: 0.0,
            total_time: format_duration(0.0),
            target_carbs_g: 0.0,
            drink_carbs_g: 0.0,
            solid_carbs_g: 0.0,
            planned_carbs_g: 0.0,
            shortfall_g: 0.0,
            usage,
            curve: Vec::new(),
        };
    };
    let total_secs = timeline.last().map_or(0.0, |c| c.end_s);

    let windows = build_drink_windows(products);
    for w in &windows {
        let unit_secs = products[w.product_index].minutes_per_unit * 60.0;
        let in_window = (total_secs - w.start).clamp(0.0, w.end - w.start);
        usage[w.product_index].used_units = in_window / unit_secs;
    }

    let goal_per_sec = settings.carbs_per_hour / 3600.0;
    let mut remaining: Vec<u32> = products
        .iter()
        .map(|p| {
            if p.kind != ProductKind::Drink && p.is_usable() {
                p.quantity
            } else {
                0
            }
        })
        .collect();

    let aid_slots = build_aid_slots(&timeline, &settings.aid_stations);
    let window_secs = settings.aid_window_minutes * 60.0;
    let aid_active = window_secs > 0.0 && !aid_slots.is_empty();
    let mut aid_used = vec![false; aid_slots.len()];

    let mut stops: Vec<NutritionStop> = Vec::new();
    let mut occurrences = vec![0usize; products.len()];
    let mut consumed_solids = 0.0;
    let mut t_prev = 0.0;

    loop {
        let ideal_for = |i: usize| {
            earliest_time_reaching(consumed_solids + products[i].carbs_g, goal_per_sec, &windows, total_secs)
        };
        let bulky = pick_in(products, &remaining, BULKY);
        let gel = pick_in(products, &remaining, GELS);

        // A bar/purée goes to an aid station if one is in reach of its ideal time.
        let at_aid = if aid_active {
            bulky.and_then(|i| {
                let ideal = ideal_for(i)?;
                let slot = pick_aid_slot(&aid_slots, &aid_used, ideal, t_prev, window_secs)?;
                Some((i, ideal, slot))
            })
        } else {
            None
        };

        let (i, ideal, aid_slot) = match at_aid {
            Some((i, ideal, slot)) => (i, ideal, Some(slot)),
            None => {
                // Bars and purées first, then gels, with or without stations.
                let pick = bulky.or(gel);
                let Some(i) = pick else { break };
                let Some(ideal) = ideal_for(i) else { break };
                (i, ideal, None)
            }
        };
        let p = &products[i];

        let (t, chunk_idx, on_hard_slope, aid_station) = match aid_slot {
            Some(s) => {
                aid_used[s] = true;
                (aid_slots[s].time, aid_slots[s].chunk, false, Some(aid_slots[s].name.to_string()))
            }
            None => {
                let (t, chunk, hard) = place_stop(&timeline, settings, ideal, t_prev, total_secs);
                (t, chunk, hard, None)
            }
        };
        let c = &timeline[chunk_idx];

        consumed_solids += p.carbs_g;
        remaining[i] -= 1;
        usage[i].used_units += 1.0;
        t_prev = t;

        stops.push(NutritionStop {
            product_index: i,
            name: p.name.clone(),
            kind: p.kind,
            carbs_g: p.carbs_g,
            ideal_time_secs: ideal,
            time_secs: t,
            time: format_duration(t),
            distance_km: km_at(c, t),
            gradient_pct: c.gradient_pct,
            on_hard_slope,
            aid_station,
            occurrence: Some(occurrences[i]),
            manual_id: None,
            cumulative_carbs_g: 0.0,
        });
        occurrences[i] += 1;
    }

    apply_manual_stops(&timeline, settings, &mut stops, &mut remaining, &mut usage);

    // Stops may have moved: order them and recompute the running total.
    stops.sort_by(|a, b| a.time_secs.total_cmp(&b.time_secs));
    let mut solids = 0.0;
    for s in &mut stops {
        solids += s.carbs_g;
        s.cumulative_carbs_g = solids + drink_carbs_at(&windows, s.time_secs);
    }

    let curve = build_curve(&windows, &stops, total_secs);
    let target_carbs_g = settings.carbs_per_hour * total_secs / 3600.0;
    let drink_carbs_g = drink_carbs_at(&windows, total_secs);
    let solid_carbs_g = solids;
    let planned_carbs_g = drink_carbs_g + solid_carbs_g;
    let out_of_solids = remaining.iter().all(|&r| r == 0);
    let shortfall_g = if out_of_solids {
        (target_carbs_g - planned_carbs_g).max(0.0)
    } else {
        0.0
    };

    NutritionPlan {
        curve,
        stops,
        total_time_secs: total_secs,
        total_time: format_duration(total_secs),
        target_carbs_g,
        drink_carbs_g,
        solid_carbs_g,
        planned_carbs_g,
        shortfall_g,
        usage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build chunks from `(len_km, secs, gradient_pct)` tuples.
    fn chunks(spec: &[(f64, f64, f64)]) -> Vec<Chunk> {
        let mut cum_km = 0.0;
        spec.iter()
            .enumerate()
            .map(|(i, &(km, secs, grad))| {
                cum_km += km;
                Chunk {
                    chunk_number: i + 1,
                    distance_km: km,
                    cumulative_km: cum_km,
                    elevation_gain_m: 0.0,
                    elevation_loss_m: 0.0,
                    cumulative_gain_m: 0.0,
                    cumulative_loss_m: 0.0,
                    gradient_pct: grad,
                    classification: String::new(),
                    fatigue_pct: 0.0,
                    estimated_pace: None,
                    estimated_time: None,
                    cumulative_time: None,
                    estimated_secs: Some(secs),
                }
            })
            .collect()
    }

    fn product(name: &str, kind: ProductKind, carbs_g: f64, quantity: u32, minutes: f64) -> Product {
        Product {
            name: name.to_string(),
            kind,
            carbs_g,
            quantity,
            minutes_per_unit: minutes,
            start_minutes: None,
        }
    }

    fn gel(carbs_g: f64, quantity: u32) -> Product {
        product("gel", ProductKind::Gel, carbs_g, quantity, 0.0)
    }

    fn settings(goal: f64, products: Vec<Product>) -> NutritionSettings {
        NutritionSettings {
            carbs_per_hour: goal,
            max_uphill_pct: 8.0,
            max_downhill_pct: 5.0,
            products,
            aid_stations: Vec::new(),
            aid_window_minutes: 0.0,
            manual_stops: Vec::new(),
        }
    }

    #[test]
    fn delayed_drink_leaves_early_race_to_solids() {
        let c = chunks(&[(20.0, 7200.0, 0.0)]);
        let mut drink = product("iso", ProductKind::Drink, 40.0, 1, 60.0);
        drink.start_minutes = Some(60.0);
        let plan = plan(&c, &settings(60.0, vec![drink, gel(30.0, 10)]));
        // Full goal from solids until the drink starts at 3600 s, then it takes over.
        assert_times(&times(&plan), &[1800.0, 3600.0]);
        assert!((plan.drink_carbs_g - 40.0).abs() < 1e-9);
        assert!((plan.usage[0].used_units - 1.0).abs() < 1e-9);
    }

    fn times(plan: &NutritionPlan) -> Vec<f64> {
        plan.stops.iter().map(|s| s.time_secs).collect()
    }

    fn assert_times(actual: &[f64], expected: &[f64]) {
        assert_eq!(actual.len(), expected.len(), "got {actual:?}, want {expected:?}");
        for (a, e) in actual.iter().zip(expected) {
            assert!((a - e).abs() < 1e-6, "got {actual:?}, want {expected:?}");
        }
    }

    #[test]
    fn flat_course_spaces_gels_by_carbs_goal() {
        let c = chunks(&[(20.0, 7200.0, 0.0)]);
        let plan = plan(&c, &settings(60.0, vec![gel(30.0, 10)]));
        assert_times(&times(&plan), &[1800.0, 3600.0, 5400.0, 7200.0]);
        assert_eq!(plan.shortfall_g, 0.0);
        assert_eq!(plan.usage[0].used_units, 4.0);
    }

    #[test]
    fn stop_moves_off_steep_climb_to_nearest_time() {
        let c = chunks(&[(5.0, 1500.0, 0.0), (1.0, 900.0, 12.0), (5.0, 1500.0, 0.0)]);
        let plan = plan(&c, &settings(60.0, vec![gel(30.0, 1)]));
        assert_eq!(plan.stops.len(), 1);
        let s = &plan.stops[0];
        assert!((s.time_secs - 1500.0).abs() < 1e-6);
        assert!((s.distance_km - 5.0).abs() < 1e-9);
        assert!(!s.on_hard_slope);
    }

    #[test]
    fn descent_limit_respected() {
        let steep = chunks(&[(5.0, 1500.0, 0.0), (1.0, 900.0, -8.0), (5.0, 1500.0, 0.0)]);
        let plan_steep = plan(&steep, &settings(60.0, vec![gel(30.0, 1)]));
        assert_times(&times(&plan_steep), &[1500.0]);

        let mild = chunks(&[(5.0, 1500.0, 0.0), (1.0, 900.0, -4.0), (5.0, 1500.0, 0.0)]);
        let plan_mild = plan(&mild, &settings(60.0, vec![gel(30.0, 1)]));
        assert_times(&times(&plan_mild), &[1800.0]);
    }

    #[test]
    fn drinks_reduce_solid_needs() {
        let c = chunks(&[(20.0, 7200.0, 0.0)]);
        let products = vec![
            product("drink", ProductKind::Drink, 40.0, 1, 60.0),
            gel(20.0, 10),
        ];
        let plan = plan(&c, &settings(60.0, products));
        assert_times(&times(&plan), &[3600.0, 4800.0, 6000.0, 7200.0]);
        assert!((plan.drink_carbs_g - 40.0).abs() < 1e-9);
        assert!((plan.solid_carbs_g - 80.0).abs() < 1e-9);
        assert!((plan.usage[0].used_units - 1.0).abs() < 1e-9);
    }

    #[test]
    fn running_out_reports_shortfall() {
        let c = chunks(&[(20.0, 7200.0, 0.0)]);
        let plan = plan(&c, &settings(60.0, vec![gel(30.0, 2)]));
        assert_eq!(plan.stops.len(), 2);
        assert!((plan.shortfall_g - 60.0).abs() < 1e-9);
    }

    #[test]
    fn bars_eaten_before_gels() {
        let c = chunks(&[(30.0, 10000.0, 0.0)]);
        let products = vec![
            product("gel A", ProductKind::Gel, 25.0, 2, 0.0),
            product("bar", ProductKind::Bar, 25.0, 2, 0.0),
            product("gel B", ProductKind::Gel, 25.0, 2, 0.0),
        ];
        let plan = plan(&c, &settings(60.0, products));
        let names: Vec<&str> = plan.stops.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["bar", "bar", "gel A", "gel B", "gel A", "gel B"]);
    }

    #[test]
    fn purees_rotate_with_bars_before_gels() {
        let c = chunks(&[(30.0, 10000.0, 0.0)]);
        let products = vec![
            product("bar", ProductKind::Bar, 25.0, 2, 0.0),
            product("puree", ProductKind::Puree, 25.0, 2, 0.0),
            product("gel", ProductKind::Gel, 25.0, 1, 0.0),
        ];
        let plan = plan(&c, &settings(60.0, products));
        let names: Vec<&str> = plan.stops.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["bar", "puree", "bar", "puree", "gel"]);
    }

    #[test]
    fn curve_tracks_cumulative_intake() {
        let c = chunks(&[(20.0, 7200.0, 0.0)]);
        let products = vec![
            product("drink", ProductKind::Drink, 40.0, 1, 60.0),
            gel(20.0, 10),
        ];
        let plan = plan(&c, &settings(60.0, products));
        let first = plan.curve.first().unwrap();
        let last = plan.curve.last().unwrap();
        assert_eq!((first.time_secs, first.carbs_g), (0.0, 0.0));
        assert!((last.time_secs - 7200.0).abs() < 1e-6);
        assert!((last.carbs_g - plan.planned_carbs_g).abs() < 1e-6);
        assert!(plan.curve.windows(2).all(|w| w[1].time_secs >= w[0].time_secs && w[1].carbs_g >= w[0].carbs_g - 1e-9));
        // First gel at 3600 s: drink done (40 g) then a 20 g step.
        assert!(plan.curve.iter().any(|p| (p.time_secs - 3600.0).abs() < 1e-6 && (p.carbs_g - 60.0).abs() < 1e-6));
    }

    #[test]
    fn all_steep_course_flags_hard_slope() {
        let c = chunks(&[(10.0, 3600.0, 12.0)]);
        let plan = plan(&c, &settings(60.0, vec![gel(30.0, 5)]));
        assert_times(&times(&plan), &[1800.0, 3600.0]);
        assert!(plan.stops.iter().all(|s| s.on_hard_slope));
    }

    fn bar(carbs_g: f64, quantity: u32) -> Product {
        product("bar", ProductKind::Bar, carbs_g, quantity, 0.0)
    }

    fn with_aid(mut s: NutritionSettings, window_min: f64, stations: &[(&str, f64)]) -> NutritionSettings {
        s.aid_window_minutes = window_min;
        s.aid_stations = stations
            .iter()
            .map(|&(name, distance_km)| AidStationInput { name: name.to_string(), distance_km })
            .collect();
        s
    }

    // 20 km in 7200 s flat: 1 km = 360 s. At 60 g/h a 30 g solid is due every 1800 s.

    #[test]
    fn bar_snaps_to_aid_station_within_window_ignoring_slope() {
        // Station at km 6 (2160 s) sits on a 12% climb, too steep to eat on otherwise.
        let c = chunks(&[(5.0, 1800.0, 0.0), (2.0, 720.0, 12.0), (13.0, 4680.0, 0.0)]);
        let s = with_aid(settings(60.0, vec![bar(30.0, 1)]), 10.0, &[("Ravito 1", 6.0)]);
        let plan = plan(&c, &s);
        assert_eq!(plan.stops.len(), 1);
        let stop = &plan.stops[0];
        assert!((stop.time_secs - 2160.0).abs() < 1e-6);
        assert!((stop.distance_km - 6.0).abs() < 1e-6);
        assert_eq!(stop.aid_station.as_deref(), Some("Ravito 1"));
        assert!(!stop.on_hard_slope);
    }

    #[test]
    fn station_outside_window_or_disabled_is_ignored() {
        let c = chunks(&[(20.0, 7200.0, 0.0)]);
        // Ideal 1800 s, station at 2160 s: 360 s away.
        let too_far = with_aid(settings(60.0, vec![bar(30.0, 1)]), 5.0, &[("R", 6.0)]);
        let plan_far = plan(&c, &too_far);
        assert_times(&times(&plan_far), &[1800.0]);
        assert!(plan_far.stops[0].aid_station.is_none());

        let off = with_aid(settings(60.0, vec![bar(30.0, 1)]), 0.0, &[("R", 6.0)]);
        assert_times(&times(&plan(&c, &off)), &[1800.0]);
    }

    #[test]
    fn gels_stay_last_even_with_stations() {
        let c = chunks(&[(20.0, 7200.0, 0.0)]);
        // Station at 2160 s is in reach of the first bar only (due 1800 s); the second
        // bar (due 3600 s) has none and must still come before every gel.
        let s = with_aid(
            settings(60.0, vec![gel(30.0, 10), bar(30.0, 2)]),
            10.0,
            &[("R", 6.0)],
        );
        let plan = plan(&c, &s);
        let names: Vec<&str> = plan.stops.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["bar", "bar", "gel", "gel"]);
        assert_times(&times(&plan), &[2160.0, 3600.0, 5400.0, 7200.0]);
        assert_eq!(plan.stops[0].aid_station.as_deref(), Some("R"));
        assert!(plan.stops[1].aid_station.is_none());
    }

    #[test]
    fn one_solid_per_station() {
        let c = chunks(&[(20.0, 7200.0, 0.0)]);
        // Station at 2700 s is within 20 min of both due times (1800 s and 3600 s).
        let s = with_aid(settings(60.0, vec![bar(30.0, 2)]), 20.0, &[("R", 7.5)]);
        let plan = plan(&c, &s);
        assert_times(&times(&plan), &[2700.0, 3600.0]);
        assert!(plan.stops[0].aid_station.is_some());
        assert!(plan.stops[1].aid_station.is_none());
    }

    fn manual(id: &str, product_index: usize, occurrence: Option<usize>, km: f64) -> ManualStopInput {
        ManualStopInput { id: id.to_string(), product_index, occurrence, distance_km: km }
    }

    #[test]
    fn moving_a_planned_stop_leaves_the_others_alone() {
        let c = chunks(&[(20.0, 7200.0, 0.0)]);
        let mut s = settings(60.0, vec![gel(30.0, 4)]);
        // Baseline 1800/3600/5400/7200; move the 2nd gel to km 4 (1440 s).
        s.manual_stops = vec![manual("m1", 0, Some(1), 4.0)];
        let plan = plan(&c, &s);
        assert_times(&times(&plan), &[1440.0, 1800.0, 5400.0, 7200.0]);
        let moved = &plan.stops[0];
        assert_eq!(moved.manual_id.as_deref(), Some("m1"));
        assert_eq!(moved.occurrence, Some(1));
        assert!((moved.ideal_time_secs - 3600.0).abs() < 1e-6);
        assert!(plan.stops[1..].iter().all(|s| s.manual_id.is_none()));
        let totals: Vec<f64> = plan.stops.iter().map(|s| s.cumulative_carbs_g).collect();
        assert_eq!(totals, [30.0, 60.0, 90.0, 120.0]);
        assert!((plan.solid_carbs_g - 120.0).abs() < 1e-9);
    }

    #[test]
    fn spare_unit_can_be_placed_but_only_once_per_unit() {
        let c = chunks(&[(20.0, 7200.0, 0.0)]);
        let mut s = settings(60.0, vec![gel(30.0, 5)]);
        // The plan uses 4 of 5 gels; only one spare exists.
        s.manual_stops = vec![manual("a", 0, None, 2.0), manual("b", 0, None, 12.0)];
        let plan = plan(&c, &s);
        assert_times(&times(&plan), &[720.0, 1800.0, 3600.0, 5400.0, 7200.0]);
        assert_eq!(plan.stops[0].manual_id.as_deref(), Some("a"));
        assert_eq!(plan.stops[0].occurrence, None);
        assert_eq!(plan.usage[0].used_units, 5.0);
        assert!((plan.solid_carbs_g - 150.0).abs() < 1e-9);
    }

    #[test]
    fn stale_manual_stops_are_ignored() {
        let c = chunks(&[(20.0, 7200.0, 0.0)]);
        let mut s = settings(60.0, vec![gel(30.0, 4)]);
        // No 10th gel in the plan, no spare unit, and km 99 is off the route.
        s.manual_stops = vec![
            manual("a", 0, Some(9), 3.0),
            manual("b", 0, None, 3.0),
            manual("c", 0, Some(0), 99.0),
        ];
        let plan = plan(&c, &s);
        assert_times(&times(&plan), &[1800.0, 3600.0, 5400.0, 7200.0]);
        assert!(plan.stops.iter().all(|s| s.manual_id.is_none()));
    }

    #[test]
    fn missing_timing_yields_empty_plan() {
        let mut c = chunks(&[(5.0, 1000.0, 0.0)]);
        c[0].estimated_secs = None;
        let plan = plan(&c, &settings(60.0, vec![gel(30.0, 3)]));
        assert!(plan.stops.is_empty());
        assert_eq!(plan.total_time_secs, 0.0);
        assert_eq!(plan.usage.len(), 1);
    }
}
