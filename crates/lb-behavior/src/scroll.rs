// scroll.rs: wheel and trackpad scrolling.
//
// Rewritten in LightBrowser (M5); the vendored generator spaced wheel
// notches N(130, 30) ms apart with 100-120 px random deltas and ran one
// exponential decay for trackpads. The model here:
//
// - Notched wheel (a Windows mouse): each detent is one `wheel` event with
//   a fixed delta (`notch_px`; Chrome on Windows reports 100 px per notch
//   at 100 % scaling). People roll the wheel in flicks: one finger stroke
//   turns it a few detents (`notches_per_flick`, normal around the mean,
//   1-8), with detent intervals log-normal around `notch_interval_ms`
//   (slower at the start and end of a flick, as the finger accelerates and
//   stops), then lift and re-grip for the next stroke (log-normal around
//   `regrip_ms`).
// - Trackpad (macOS): each swipe has a finger phase (5-12 display frames,
//   speed rising to a peak, log-normal around `speed_mean_px_per_s`) and
//   then an inertial phase where the speed decays geometrically by
//   `momentum_deceleration` per 16.7 ms frame until it is below 1 px per
//   frame. A new swipe (after a re-grip pause) cancels the remaining
//   momentum. Swipes are a few degrees off-axis, but macOS locks a
//   gesture to its main axis ("rails") once its direction is clear, so a
//   cross-axis delta shows up only in the first frames of a swipe.
//   Deltas are whole pixels with the remainder carried, so the totals stay
//   exact.
// - With `direction_change_probability` the user scrolls back a little at
//   the end (overshot what they were looking for).

use rand::rngs::SmallRng;
use serde::{Deserialize, Serialize};

use crate::rng::{chance, clamp, log_normal, normal_sample, uniform};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Discriminates between the two scroll device classes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrollType {
    /// Notched mouse wheel: fixed-size steps in flicks.
    Wheel,
    /// Trackpad: pixel deltas per frame with momentum.
    Trackpad,
}

/// Scroll direction from the user's perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrollDirection {
    Down,
    Up,
    Left,
    Right,
}

impl ScrollDirection {
    /// Browser convention: positive deltaY scrolls down.
    fn delta_sign(self) -> f64 {
        match self {
            ScrollDirection::Down | ScrollDirection::Right => 1.0,
            ScrollDirection::Up | ScrollDirection::Left => -1.0,
        }
    }

    fn is_horizontal(self) -> bool {
        matches!(self, ScrollDirection::Left | ScrollDirection::Right)
    }
}

/// A single synthesised wheel event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrollEvent {
    /// Horizontal delta in pixels (positive = right).
    pub delta_x: f64,
    /// Vertical delta in pixels (positive = down).
    pub delta_y: f64,
    /// Milliseconds since the start of the generated sequence.
    pub timestamp_ms: u64,
    pub scroll_type: ScrollType,
}

/// Per-session scroll behaviour parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScrollProfile {
    pub scroll_type: ScrollType,
    /// Wheel: pixels per detent.
    pub notch_px: f64,
    /// Wheel: mean detents per finger stroke.
    pub notches_per_flick: f64,
    /// Wheel: median time between detents within a stroke (ms).
    pub notch_interval_ms: f64,
    /// Median pause between strokes or swipes (ms).
    pub regrip_ms: f64,
    /// Trackpad: median peak finger speed in px/s.
    pub speed_mean_px_per_s: f64,
    /// Trackpad: log-space spread of the peak speed, as a fraction.
    pub speed_std: f64,
    /// Trackpad: speed kept per 16.7 ms frame in the inertial phase.
    pub momentum_deceleration: f64,
    /// Probability that the user scrolls back a little at the end.
    pub direction_change_probability: f64,
}

impl Default for ScrollProfile {
    fn default() -> Self {
        crate::BehaviorProfile::casual_user().scroll
    }
}

/// Whole-pixel deltas with the fractional remainder carried forward.
#[derive(Default)]
struct Carry(f64);

impl Carry {
    fn take(&mut self, v: f64) -> f64 {
        let want = self.0 + v;
        let r = want.round();
        self.0 = want - r;
        r
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Generate wheel events that scroll about `total_px` in `direction`.
///
/// The wheel model scrolls a whole number of detents (at least one); the
/// trackpad model covers `total_px` within a few pixels. A trailing
/// scroll-back (see `direction_change_probability`) is not counted.
pub fn generate_scroll(
    direction: ScrollDirection,
    total_px: f64,
    profile: &ScrollProfile,
    rng: &mut SmallRng,
) -> Vec<ScrollEvent> {
    let total_px = total_px.abs();
    if total_px < 0.5 {
        return Vec::new();
    }
    match profile.scroll_type {
        ScrollType::Wheel => wheel(direction, total_px, profile, rng),
        ScrollType::Trackpad => trackpad(direction, total_px, profile, rng),
    }
}

fn event(dir: ScrollDirection, along: f64, across: f64, t: f64, ty: ScrollType) -> ScrollEvent {
    let s = dir.delta_sign();
    let (dx, dy) = if dir.is_horizontal() {
        (s * along, across)
    } else {
        (across, s * along)
    };
    ScrollEvent {
        delta_x: dx,
        delta_y: dy,
        timestamp_ms: t.max(0.0).round() as u64,
        scroll_type: ty,
    }
}

fn reverse(d: ScrollDirection) -> ScrollDirection {
    match d {
        ScrollDirection::Down => ScrollDirection::Up,
        ScrollDirection::Up => ScrollDirection::Down,
        ScrollDirection::Left => ScrollDirection::Right,
        ScrollDirection::Right => ScrollDirection::Left,
    }
}

fn wheel(
    dir: ScrollDirection,
    total_px: f64,
    p: &ScrollProfile,
    rng: &mut SmallRng,
) -> Vec<ScrollEvent> {
    let notch = p.notch_px.max(1.0);
    let mut remaining = ((total_px / notch).round() as i64).max(1);
    let mut events = Vec::new();
    let mut t = uniform(rng, 0.0, 20.0);
    let flick = |rng: &mut SmallRng| {
        clamp(
            normal_sample(rng, p.notches_per_flick, 1.2).round(),
            1.0,
            8.0,
        ) as i64
    };
    while remaining > 0 {
        let k = flick(rng).min(remaining);
        for j in 0..k {
            events.push(event(dir, notch, 0.0, t, ScrollType::Wheel));
            if j + 1 < k {
                let edge = if j == 0 || j + 2 == k { 1.3 } else { 1.0 };
                t += clamp(log_normal(rng, p.notch_interval_ms * edge, 0.3), 8.0, 250.0);
            }
        }
        remaining -= k;
        if remaining > 0 {
            t += clamp(log_normal(rng, p.regrip_ms, 0.35), 60.0, 1500.0);
        }
    }
    if events.len() >= 3 && chance(rng, p.direction_change_probability) {
        t += clamp(log_normal(rng, 450.0, 0.4), 150.0, 2000.0);
        events.push(event(reverse(dir), notch, 0.0, t, ScrollType::Wheel));
    }
    events
}

/// Frames of the finger phase and the speed fraction in each.
fn finger_profile(rng: &mut SmallRng) -> Vec<f64> {
    let n = uniform(rng, 5.0, 12.99) as usize;
    (0..n)
        .map(|k| {
            let x = (k + 1) as f64 / n as f64;
            // Raised cosine up to the peak at about 70 % of the phase.
            if x < 0.7 {
                0.5 * (1.0 - (std::f64::consts::PI * x / 0.7).cos())
            } else {
                1.0
            }
        })
        .collect()
}

fn trackpad(
    dir: ScrollDirection,
    total_px: f64,
    p: &ScrollProfile,
    rng: &mut SmallRng,
) -> Vec<ScrollEvent> {
    const FRAME_MS: f64 = 1000.0 / 60.0;
    const RAIL_FRAMES: usize = 3;
    let dt = FRAME_MS / 1000.0;
    let alpha = clamp(p.momentum_deceleration, 0.5, 0.985);
    let inertia = alpha / (1.0 - alpha);
    let mut events = Vec::new();
    let mut t = uniform(rng, 0.0, 10.0);
    let mut done = 0.0;
    let mut along_carry = Carry::default();
    let mut across_carry = Carry::default();
    let ty = ScrollType::Trackpad;
    for _ in 0..60 {
        let remaining = total_px - done;
        if remaining < 1.0 {
            break;
        }
        let shape = finger_profile(rng);
        let factor: f64 = shape.iter().sum::<f64>() + inertia;
        let mut v = log_normal(rng, p.speed_mean_px_per_s, p.speed_std.max(0.01));
        // Last swipe: aim for what is left.
        if v * dt * factor > remaining * 1.15 {
            v = remaining / (dt * factor);
        }
        let tilt = normal_sample(rng, 0.0, 2.5_f64.to_radians()).tan();
        let mut frame_no = 0usize;
        let mut emit = |d: f64, t: f64, done: &mut f64, events: &mut Vec<ScrollEvent>| {
            let d = d.min(total_px - *done);
            let a = along_carry.take(d);
            // Rails: once a swipe is clearly along one axis, macOS drops
            // the other one; only the first frames carry the tilt.
            let c = if frame_no < RAIL_FRAMES {
                across_carry.take(d * tilt)
            } else {
                0.0
            };
            frame_no += 1;
            *done += d;
            if a != 0.0 || c != 0.0 {
                events.push(event(dir, a, c, t, ScrollType::Trackpad));
            }
        };
        for f in &shape {
            emit(v * f * dt, t, &mut done, &mut events);
            t += FRAME_MS + normal_sample(rng, 0.0, 0.5);
        }
        // Inertial phase, cut short by the next swipe while far from done.
        let mut speed = v * alpha;
        let next_swipe = t + clamp(log_normal(rng, p.regrip_ms, 0.35), 80.0, 1500.0);
        while speed * dt >= 1.0 && done < total_px {
            if total_px - done > v * dt * factor * 0.5 && t >= next_swipe {
                break;
            }
            emit(speed * dt, t, &mut done, &mut events);
            t += FRAME_MS + normal_sample(rng, 0.0, 0.5);
            speed *= alpha;
        }
        t = t.max(next_swipe.min(t + FRAME_MS * 4.0));
    }
    // Whatever rounding left over goes in one last event.
    let sum: f64 = events
        .iter()
        .map(|e| {
            if dir.is_horizontal() {
                e.delta_x
            } else {
                e.delta_y
            }
            .abs()
        })
        .sum();
    let left = (total_px - sum).round();
    if left >= 1.0 {
        events.push(event(dir, left, 0.0, t, ty.clone()));
        t += FRAME_MS;
    }
    if !events.is_empty() && chance(rng, p.direction_change_probability) {
        // A short swipe back.
        t += clamp(log_normal(rng, 450.0, 0.4), 150.0, 2000.0);
        let back = uniform(rng, 30.0, 120.0).min(total_px * 0.3).max(5.0);
        let mut c = Carry::default();
        let n = 6;
        for k in 0..n {
            let f = (std::f64::consts::PI * (k as f64 + 0.5) / n as f64).sin();
            let d = c.take(back * f * std::f64::consts::PI / (2.0 * n as f64));
            if d != 0.0 {
                events.push(event(reverse(dir), d, 0.0, t, ty.clone()));
            }
            t += FRAME_MS;
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::BehaviorProfile;
    use crate::rng::seeded_rng;

    fn wheel_profile() -> ScrollProfile {
        let mut p = BehaviorProfile::casual_user().scroll;
        p.scroll_type = ScrollType::Wheel;
        p.direction_change_probability = 0.0;
        p
    }

    fn pad_profile() -> ScrollProfile {
        let mut p = BehaviorProfile::casual_user().scroll;
        p.scroll_type = ScrollType::Trackpad;
        p.direction_change_probability = 0.0;
        p
    }

    #[test]
    fn wheel_notches_are_fixed_and_in_flicks() {
        let mut rng = seeded_rng(20);
        let ev = generate_scroll(ScrollDirection::Down, 900.0, &wheel_profile(), &mut rng);
        assert_eq!(ev.len(), 9);
        assert!(ev.iter().all(|e| e.delta_y == 100.0 && e.delta_x == 0.0));
        let gaps: Vec<u64> = ev
            .windows(2)
            .map(|w| w[1].timestamp_ms - w[0].timestamp_ms)
            .collect();
        // Short gaps inside flicks, at least one long re-grip pause.
        let min = *gaps.iter().min().unwrap();
        let max = *gaps.iter().max().unwrap();
        assert!(min < 100 && max > 2 * min, "{gaps:?}");
    }

    #[test]
    fn wheel_at_least_one_notch() {
        let mut rng = seeded_rng(2);
        let ev = generate_scroll(ScrollDirection::Up, 20.0, &wheel_profile(), &mut rng);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].delta_y, -100.0);
    }

    #[test]
    fn trackpad_covers_distance_with_momentum() {
        for seed in 0..30 {
            let mut rng = seeded_rng(seed);
            let ev = generate_scroll(ScrollDirection::Down, 1500.0, &pad_profile(), &mut rng);
            let total: f64 = ev.iter().map(|e| e.delta_y).sum();
            assert!((total - 1500.0).abs() <= 1.0, "seed {seed}: {total}");
            assert!(
                ev.iter()
                    .all(|e| e.delta_y.fract() == 0.0 && e.delta_y.abs() < 200.0)
            );
            for w in ev.windows(2) {
                assert!(w[1].timestamp_ms >= w[0].timestamp_ms);
            }
            // Deltas vary (not a constant stream).
            assert!(ev.iter().any(|e| e.delta_y != ev[0].delta_y));
        }
    }

    #[test]
    fn trackpad_decays() {
        let mut rng = seeded_rng(5);
        let ev = generate_scroll(ScrollDirection::Down, 300.0, &pad_profile(), &mut rng);
        let peak = ev
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.delta_y.total_cmp(&b.1.delta_y))
            .unwrap()
            .0;
        let tail = &ev[peak..];
        assert!(tail.len() > 3);
        assert!(tail.last().unwrap().delta_y < tail[0].delta_y);
    }

    #[test]
    fn horizontal_and_reverse() {
        let mut rng = seeded_rng(7);
        let mut p = wheel_profile();
        p.direction_change_probability = 1.0;
        let ev = generate_scroll(ScrollDirection::Right, 500.0, &p, &mut rng);
        assert!(ev.iter().all(|e| e.delta_y == 0.0));
        assert_eq!(ev.last().unwrap().delta_x, -100.0);
        let net: f64 = ev.iter().map(|e| e.delta_x).sum();
        assert_eq!(net, 400.0);
    }

    #[test]
    fn zero_distance_is_empty() {
        let mut rng = seeded_rng(1);
        assert!(generate_scroll(ScrollDirection::Down, 0.0, &pad_profile(), &mut rng).is_empty());
    }
}
