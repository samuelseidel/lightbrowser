// mouse.rs — Bézier-curve mouse movement simulation (vendored from stealth-browser)
//
// ## Why Bézier curves?
//
// Human cursor trajectories are not straight lines.  Fitts' Law experiments
// and large-scale telemetry studies (e.g. Huang et al. 2011, "Improving the
// Usability of Online Map Interfaces") show that:
//
//   1. Paths curve naturally because the hand swings from the shoulder/elbow
//      pivot rather than translating linearly.
//   2. The speed profile follows a roughly bell-shaped curve: slow start
//      (movement initiation), fast transit, slow finish (homing onto target).
//      This is captured by Minimum Jerk Theory (Flash & Hogan 1985).
//   3. Approximately 20–40 % of movements include an overshoot followed by a
//      corrective sub-movement.
//   4. During hovering (e.g. reading a tooltip) the cursor drifts slightly —
//      "micro-movements" caused by involuntary hand tremor.
//
// A cubic Bézier with randomly perturbed control points reproduces the
// curvature characteristic while being cheap to evaluate.  The ease-in-out
// speed profile is a smooth approximation to the minimum-jerk trajectory.

use rand::rngs::SmallRng;
use serde::{Deserialize, Serialize};

use crate::rng::{clamp, normal_sample, uniform};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A 2-D screen coordinate in pixels.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Euclidean distance to another point.
    pub fn dist(&self, other: &Point) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}

/// The type of mouse event, matching browser MouseEvent.type values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MouseEventType {
    Move,
    Down,
    Up,
    /// Synthetic click (follows a Down+Up pair on the same element).
    Click,
}

/// A single synthesised mouse event ready to be injected into the browser.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseEvent {
    pub x: f64,
    pub y: f64,
    /// Milliseconds since the start of the generated sequence.
    pub timestamp_ms: u64,
    /// Bitmask of currently-held buttons (0 = none, 1 = primary/left).
    pub buttons: u8,
    pub event_type: MouseEventType,
}

/// Per-session mouse behaviour parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseProfile {
    /// Mean cursor speed in pixels per second.
    pub speed_mean_px_per_s: f64,
    /// Standard deviation of cursor speed (session-to-session variation).
    pub speed_std: f64,
    /// Probability [0, 1] that any given movement overshoots the target.
    pub overshoot_rate: f64,
    /// Mean overshoot distance in pixels (Gaussian, always positive via abs).
    pub overshoot_distance_px: f64,
    /// Whether to add micro-movement drift while the cursor is stationary.
    pub micro_movement_enabled: bool,
    /// Std-dev of per-sample micro-movement jitter in pixels.
    pub micro_movement_std_px: f64,
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Evaluate a cubic Bézier at parameter t ∈ [0, 1].
///
/// B(t) = (1-t)³·P0 + 3(1-t)²t·P1 + 3(1-t)t²·P2 + t³·P3
fn bezier(p0: Point, p1: Point, p2: Point, p3: Point, t: f64) -> Point {
    let u = 1.0 - t;
    let uu = u * u;
    let uuu = uu * u;
    let tt = t * t;
    let ttt = tt * t;

    Point {
        x: uuu * p0.x + 3.0 * uu * t * p1.x + 3.0 * u * tt * p2.x + ttt * p3.x,
        y: uuu * p0.y + 3.0 * uu * t * p1.y + 3.0 * u * tt * p2.y + ttt * p3.y,
    }
}

/// Ease-in-out speed multiplier at normalised path position `s` ∈ [0, 1].
///
/// This is a smooth-step (Hermite) function that produces:
///   • 0 at s=0 (cursor is still at the start)
///   • 1 at s=0.5 (peak speed at midpoint)
///   • 0 at s=1 (cursor stops at target)
///
/// The resulting speed profile is a rough approximation of the minimum-jerk
/// trajectory from computational motor-control theory, which is known to fit
/// human movement data well.
fn ease_in_out(s: f64) -> f64 {
    // Smooth-step: 3s² - 2s³
    s * s * (3.0 - 2.0 * s)
}

/// Generate random control points that produce a naturally curving path.
///
/// The two interior control points P1 and P2 are placed at 1/3 and 2/3 of
/// the straight-line distance, then displaced laterally by a random amount.
/// The lateral displacement is proportional to the total distance so that
/// short movements curve less than long ones — matching human motor data.
fn random_control_points(from: Point, to: Point, rng: &mut SmallRng) -> (Point, Point) {
    let dist = from.dist(&to);

    // Unit vector along the direct path.
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let len = dist.max(1.0);
    let ux = dx / len;
    let uy = dy / len;

    // Perpendicular unit vector (rotate 90°).
    let px = -uy;
    let py = ux;

    // Lateral deviation: humans rarely deviate more than ~30 % of path length.
    // The sign is randomised so curves can go either way.
    let max_deviation = dist * 0.25;
    let d1 = uniform(rng, -max_deviation, max_deviation);
    let d2 = uniform(rng, -max_deviation, max_deviation);

    // Place control points at 1/3 and 2/3 along the straight line.
    let p1 = Point {
        x: from.x + dx / 3.0 + px * d1,
        y: from.y + dy / 3.0 + py * d1,
    };
    let p2 = Point {
        x: from.x + 2.0 * dx / 3.0 + px * d2,
        y: from.y + 2.0 * dy / 3.0 + py * d2,
    };

    (p1, p2)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Generate a realistic human mouse-movement sequence from `from` to `to`.
///
/// # Algorithm
///
/// 1. Compute total Euclidean distance.
/// 2. Sample an effective speed for this movement from N(mean, std).
/// 3. Generate two random interior Bézier control points with lateral deviation.
/// 4. Optionally add an overshoot: extend the path past the target by a small
///    Gaussian distance, then add a corrective straight-line return path.
/// 5. Arc-length–parameterise the Bézier so that we can apply the ease-in-out
///    speed profile uniformly along the curve (naïve t-parameterisation gives
///    non-uniform speed because Béziers are not arc-length parameterised by
///    default).
/// 6. For each sample point, add ±0.5 px Gaussian jitter to simulate hand
///    tremor and quantisation noise from the HID layer.
/// 7. Derive timestamps from the cumulative distance and the speed profile.
///
/// Returns a `Vec<MouseEvent>` containing only `Move` events; click events
/// are appended separately by `generate_click`.
pub fn generate_mouse_path(
    from: Point,
    to: Point,
    profile: &MouseProfile,
    rng: &mut SmallRng,
) -> Vec<MouseEvent> {
    let dist = from.dist(&to);

    // Very short movements (< 3 px) — just emit one event at the destination.
    if dist < 3.0 {
        return vec![MouseEvent {
            x: to.x,
            y: to.y,
            timestamp_ms: 0,
            buttons: 0,
            event_type: MouseEventType::Move,
        }];
    }

    // Sample an effective speed for this individual movement.
    // We clamp to a sensible human range (50 – 5000 px/s).
    let speed = clamp(
        normal_sample(rng, profile.speed_mean_px_per_s, profile.speed_std),
        50.0,
        5000.0,
    );

    // --- Bézier control points -------------------------------------------
    let (p1, p2) = random_control_points(from, to, rng);

    // --- Overshoot --------------------------------------------------------
    // Some movements over-run the target (Fitts' Law predicts this is more
    // common for fast movements toward small targets).  We model overshoot as
    // a second, shorter Bézier segment that continues past `to` by a small
    // amount and then returns.
    let do_overshoot = uniform(rng, 0.0, 1.0) < profile.overshoot_rate;

    // Build path segment(s): list of (from, p1, p2, to) tuples.
    let mut segments: Vec<(Point, Point, Point, Point)> = Vec::new();

    if do_overshoot {
        // Compute a point slightly past the target along the final tangent.
        let tangent_dx = to.x - p2.x;
        let tangent_dy = to.y - p2.y;
        let tangent_len = (tangent_dx * tangent_dx + tangent_dy * tangent_dy)
            .sqrt()
            .max(1.0);
        let overshoot_dist = clamp(
            normal_sample(
                rng,
                profile.overshoot_distance_px,
                profile.overshoot_distance_px * 0.4,
            ),
            2.0,
            profile.overshoot_distance_px * 2.5,
        );
        let overshoot_pt = Point {
            x: to.x + (tangent_dx / tangent_len) * overshoot_dist,
            y: to.y + (tangent_dy / tangent_len) * overshoot_dist,
        };

        // Primary segment: from → overshoot_pt
        let (op1, op2) = random_control_points(from, overshoot_pt, rng);
        segments.push((from, op1, op2, overshoot_pt));

        // Corrective segment: overshoot_pt → to (short, nearly straight)
        let (cp1, cp2) = random_control_points(overshoot_pt, to, rng);
        segments.push((overshoot_pt, cp1, cp2, to));
    } else {
        segments.push((from, p1, p2, to));
    }

    // --- Arc-length parameterisation -------------------------------------
    // We sample the Bézier at a large number of t values to build an
    // approximate arc-length lookup table, then use it to place samples at
    // equal arc-length intervals.
    //
    // This is necessary because raw Bézier parameterisation produces
    // non-uniform speed: samples bunch up near inflection points and spread
    // out near the endpoints, which would look robotic.

    // Target sampling interval: ~8 px of arc-length gives ~100 events per
    // 800 px path, which is realistic for 60 Hz mouse reports at ~800 px/s.
    const SAMPLE_INTERVAL_PX: f64 = 8.0;
    const LUT_STEPS: usize = 500; // granularity of the arc-length LUT

    let mut all_points: Vec<Point> = Vec::new();

    for (sp0, sp1, sp2, sp3) in &segments {
        // Build LUT for this segment.
        let mut lut: Vec<(f64, f64)> = Vec::with_capacity(LUT_STEPS + 1); // (t, arc_length)
        let mut arc_len = 0.0_f64;
        let mut prev = *sp0;
        lut.push((0.0, 0.0));
        for i in 1..=LUT_STEPS {
            let t = i as f64 / LUT_STEPS as f64;
            let pt = bezier(*sp0, *sp1, *sp2, *sp3, t);
            arc_len += prev.dist(&pt);
            lut.push((t, arc_len));
            prev = pt;
        }

        let seg_len = arc_len;
        if seg_len < 1.0 {
            continue;
        }

        // Walk along the arc at SAMPLE_INTERVAL_PX steps.
        let n_samples = (seg_len / SAMPLE_INTERVAL_PX).ceil() as usize;
        let mut lut_idx = 0usize;
        for s in 0..=n_samples {
            let target_len = (s as f64 / n_samples as f64) * seg_len;
            // Advance LUT index until we bracket the target arc length.
            while lut_idx + 1 < lut.len() && lut[lut_idx + 1].1 < target_len {
                lut_idx += 1;
            }
            // Linear interpolation between adjacent LUT entries.
            let t = if lut_idx + 1 < lut.len() {
                let (t0, l0) = lut[lut_idx];
                let (t1, l1) = lut[lut_idx + 1];
                let frac = if (l1 - l0).abs() < 1e-9 {
                    0.0
                } else {
                    (target_len - l0) / (l1 - l0)
                };
                t0 + frac * (t1 - t0)
            } else {
                1.0
            };

            let pt = bezier(*sp0, *sp1, *sp2, *sp3, t);
            all_points.push(pt);
        }
    }

    if all_points.is_empty() {
        all_points.push(to);
    }

    // --- Speed profile & timestamp calculation ---------------------------
    // Apply ease-in-out speed modulation.  The speed at each point is:
    //   v(s) = v_peak * ease_in_out(s)
    // where s is the normalised position along the whole path.
    //
    // Integrating v(s) over the path gives the total travel time.  We use
    // the sampled points to compute cumulative distances and from those derive
    // timestamps.
    //
    // The constant `v_peak` is chosen so that the total time equals
    // dist / (speed * integral_of_ease_in_out).
    //
    // ∫₀¹ ease_in_out(s) ds = ∫₀¹ (3s²-2s³) ds = [s³ - s⁴/2]₀¹ = 0.5
    // So v_peak = speed / 0.5 = 2 * speed.
    let v_peak = 2.0 * speed;

    let n = all_points.len();
    let mut events: Vec<MouseEvent> = Vec::with_capacity(n);
    let mut timestamp_ms: f64 = 0.0;

    for (i, pt) in all_points.iter().enumerate() {
        // Normalised position along the path [0, 1].
        let s = i as f64 / (n - 1).max(1) as f64;

        // Instantaneous speed at this position.
        let v_inst = (v_peak * ease_in_out(s)).max(1.0); // avoid division by zero

        // Time to advance one arc-length sample at this speed.
        // Each sample is approximately SAMPLE_INTERVAL_PX apart.
        if i > 0 {
            let dt_s = SAMPLE_INTERVAL_PX / v_inst;
            timestamp_ms += dt_s * 1000.0;
        }

        // Apply micro-jitter: small Gaussian noise on every point.
        // This simulates hand tremor and sub-pixel HID quantisation.
        // Real mice report at 125–1000 Hz with ±0.5 px noise at the low end.
        let jitter_x = normal_sample(rng, 0.0, 0.5);
        let jitter_y = normal_sample(rng, 0.0, 0.5);

        events.push(MouseEvent {
            x: pt.x + jitter_x,
            y: pt.y + jitter_y,
            timestamp_ms: timestamp_ms as u64,
            buttons: 0,
            event_type: MouseEventType::Move,
        });
    }

    events
}

/// Generate events for a human-like click at `position`.
///
/// # Sequence
///
/// 1. Move to the target (via `generate_mouse_path`) with a Gaussian offset
///    from the exact centre — humans do not click pixel-perfectly.
/// 2. `MouseDown` with a small random delay after arrival (reaction time +
///    button-press latency ≈ 10–150 ms).
/// 3. `MouseUp` after a random button-hold duration (10–150 ms).  The hold
///    time is log-normally distributed because it has a hard lower bound
///    (physical button travel time ~10 ms) but a long tail (users sometimes
///    hold briefly while deciding).
/// 4. Synthetic `Click` event at the same position.
///
/// # Gaussian click offset
///
/// Real click positions cluster around the visual centre of a target but are
/// offset by N(0, 3 px) in each axis.  This is known as "endpoint scatter" in
/// motor-control literature and is caused by neuromotor noise, not
/// inattention.  Bot detectors look for pixel-perfect click coordinates as a
/// strong signal.
pub fn generate_click(
    from: Point,
    position: Point,
    profile: &MouseProfile,
    rng: &mut SmallRng,
) -> Vec<MouseEvent> {
    // Move events to the target.
    let mut events = generate_mouse_path(from, position, profile, rng);

    // Determine the timestamp of arrival.
    let arrive_ts = events.last().map(|e| e.timestamp_ms).unwrap_or(0);

    // Apply Gaussian endpoint scatter: N(0, 3 px) per axis.
    // This is the key anti-bot signal — do not reduce this.
    let click_x = position.x + normal_sample(rng, 0.0, 3.0);
    let click_y = position.y + normal_sample(rng, 0.0, 3.0);

    // Pre-click pause: time between cursor stop and button press.
    // Uniform 10–80 ms (reaction lag + button actuation latency).
    let pre_click_pause = uniform(rng, 10.0, 80.0) as u64;

    // Button hold duration: time from MouseDown to MouseUp.
    // Log-normal with μ=60, σ=30 ms, clamped to [10, 150].
    let hold_raw = normal_sample(rng, 60.0, 30.0);
    let hold_ms = clamp(hold_raw, 10.0, 150.0) as u64;

    let down_ts = arrive_ts + pre_click_pause;
    let up_ts = down_ts + hold_ms;

    events.push(MouseEvent {
        x: click_x,
        y: click_y,
        timestamp_ms: down_ts,
        buttons: 1,
        event_type: MouseEventType::Down,
    });

    events.push(MouseEvent {
        x: click_x,
        y: click_y,
        timestamp_ms: up_ts,
        buttons: 0,
        event_type: MouseEventType::Up,
    });

    // Synthetic Click immediately follows MouseUp (browser fires it at the
    // same tick in most implementations).
    events.push(MouseEvent {
        x: click_x,
        y: click_y,
        timestamp_ms: up_ts,
        buttons: 0,
        event_type: MouseEventType::Click,
    });

    events
}

/// Generate micro-movement events for hovering over a fixed point.
///
/// While a user reads a tooltip or a modal, the cursor is not perfectly still
/// — involuntary hand tremor produces drifts of 1–3 px over seconds.  Bot
/// detectors measure cursor variance during pauses; zero variance is a strong
/// bot signal.
///
/// # Arguments
/// * `position`    — nominal hover position
/// * `duration_ms` — how long to generate micro-movements for
/// * `profile`     — provides `micro_movement_std_px`
pub fn generate_micro_movements(
    position: Point,
    duration_ms: u64,
    profile: &MouseProfile,
    rng: &mut SmallRng,
) -> Vec<MouseEvent> {
    if !profile.micro_movement_enabled || profile.micro_movement_std_px <= 0.0 {
        return Vec::new();
    }

    // Emit one micro-movement event roughly every 200 ms.
    // Real hover variance studies sample at this cadence.
    let interval_ms = 200u64;
    let n = (duration_ms / interval_ms).max(1) as usize;

    // We use a random walk so consecutive positions are correlated (more
    // realistic than independent Gaussian samples).
    let mut x = position.x;
    let mut y = position.y;
    let mut events = Vec::with_capacity(n);

    for i in 0..n {
        // Each step is a small Gaussian increment.
        x += normal_sample(rng, 0.0, profile.micro_movement_std_px);
        y += normal_sample(rng, 0.0, profile.micro_movement_std_px);

        // Soft pull back toward the nominal position to prevent unbounded drift.
        let pull = 0.1;
        x = x * (1.0 - pull) + position.x * pull;
        y = y * (1.0 - pull) + position.y * pull;

        events.push(MouseEvent {
            x,
            y,
            timestamp_ms: (i as u64 + 1) * interval_ms,
            buttons: 0,
            event_type: MouseEventType::Move,
        });
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::BehaviorProfile;
    use crate::rng::seeded_rng;

    fn default_profile() -> MouseProfile {
        BehaviorProfile::casual_user().mouse
    }

    #[test]
    fn path_ends_near_target() {
        let profile = default_profile();
        let mut rng = seeded_rng(1);
        let from = Point::new(100.0, 100.0);
        let to = Point::new(800.0, 400.0);
        let events = generate_mouse_path(from, to, &profile, &mut rng);
        assert!(!events.is_empty());
        let last = events.last().unwrap();
        // With overshoot the last move event can be the corrective return;
        // allow up to 60 px tolerance.
        let d = ((last.x - to.x).powi(2) + (last.y - to.y).powi(2)).sqrt();
        assert!(d < 60.0, "path ended too far from target: {:.1} px", d);
    }

    #[test]
    fn click_events_have_correct_order() {
        let profile = default_profile();
        let mut rng = seeded_rng(2);
        let from = Point::new(0.0, 0.0);
        let target = Point::new(300.0, 200.0);
        let events = generate_click(from, target, &profile, &mut rng);

        // Last three events must be Down, Up, Click in that order.
        let n = events.len();
        assert!(n >= 3);
        assert_eq!(events[n - 3].event_type, MouseEventType::Down);
        assert_eq!(events[n - 2].event_type, MouseEventType::Up);
        assert_eq!(events[n - 1].event_type, MouseEventType::Click);

        // Timestamps must be non-decreasing.
        for w in events.windows(2) {
            assert!(
                w[1].timestamp_ms >= w[0].timestamp_ms,
                "timestamps not monotonic: {} > {}",
                w[0].timestamp_ms,
                w[1].timestamp_ms
            );
        }
    }

    #[test]
    fn timestamps_are_monotonic() {
        let profile = default_profile();
        let mut rng = seeded_rng(3);
        let events = generate_mouse_path(
            Point::new(0.0, 0.0),
            Point::new(1000.0, 500.0),
            &profile,
            &mut rng,
        );
        for w in events.windows(2) {
            assert!(w[1].timestamp_ms >= w[0].timestamp_ms);
        }
    }
}
