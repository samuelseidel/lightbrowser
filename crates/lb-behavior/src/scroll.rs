// scroll.rs — Momentum/discrete scroll simulation (vendored from stealth-browser)
//
// ## Why Two Scroll Models?
//
// Browser `wheel` events carry a `deltaMode` field that indicates the source:
//
//   • `DOM_DELTA_LINE` (mode=1): traditional mouse wheel, one notch = ~3 lines
//     (~100 px).  Events arrive in discrete bursts spaced 80–200 ms apart.
//
//   • `DOM_DELTA_PIXEL` (mode=0): trackpad or high-resolution wheel.  Events
//     arrive at ~60 fps with continuously varying deltas that follow a
//     momentum curve: they start large and decay exponentially until the
//     gesture ends.  MacOS inertial scrolling is the canonical example.
//
// Bot detectors distinguish these modes and flag implausible combinations
// (e.g. pixel-mode deltas that jump discontinuously, or line-mode events
// spaced exactly 100 ms apart).  We must reproduce the statistical fingerprint
// of each device class.
//
// ## Momentum Scroll Physics
//
// When a user flicks a trackpad, the OS models the remaining velocity as
// decaying exponentially:
//
//   v(t) = v₀ · α^(t/T)
//
// where α ∈ (0, 1) is the deceleration factor and T is the frame period
// (16.67 ms at 60 fps).  The scroll stops when |v| drops below a threshold.
// We sample v₀ from the profile's speed distribution and use the profile's
// `momentum_deceleration` for α.
//
// ## Direction Changes
//
// Real users occasionally scroll back a little (re-reading a line, hovering
// over a link they passed).  We model this as brief reversed-direction
// segments that occur with `direction_change_probability` per generated
// sequence.

use rand::rngs::SmallRng;
use serde::{Deserialize, Serialize};

use crate::rng::{clamp, normal_sample, uniform};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Discriminates between the two scroll device classes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrollType {
    /// Discrete mouse wheel — fixed-size steps, line mode.
    Wheel,
    /// High-resolution trackpad — pixel mode with momentum.
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
    /// Returns the sign of the scroll delta (+1 or -1 in device coordinates).
    ///
    /// Browser convention: positive deltaY = scroll down (content moves up).
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

/// A single synthesised scroll (wheel) event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrollEvent {
    /// Horizontal scroll delta in pixels (positive = right).
    pub delta_x: f64,
    /// Vertical scroll delta in pixels (positive = down).
    pub delta_y: f64,
    /// Milliseconds since the start of the generated sequence.
    pub timestamp_ms: u64,
    pub scroll_type: ScrollType,
}

/// Per-session scroll behaviour parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrollProfile {
    pub scroll_type: ScrollType,
    /// Mean scroll speed in pixels per second.
    pub speed_mean_px_per_s: f64,
    pub speed_std: f64,
    /// Exponential deceleration factor per 16 ms frame (trackpad only).
    /// Typical macOS values: 0.75–0.92.
    pub momentum_deceleration: f64,
    /// Probability [0, 1] that the sequence includes a brief direction reversal.
    pub direction_change_probability: f64,
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Build a single scroll event helper.
fn make_event(
    delta_x: f64,
    delta_y: f64,
    timestamp_ms: u64,
    scroll_type: &ScrollType,
) -> ScrollEvent {
    ScrollEvent {
        delta_x,
        delta_y,
        timestamp_ms,
        scroll_type: scroll_type.clone(),
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Generate a realistic sequence of scroll events.
///
/// # Arguments
/// * `direction` — primary scroll direction
/// * `total_px`  — total pixels to scroll (absolute value; direction is set
///   by `direction` argument)
/// * `profile`   — scroll behaviour parameters
/// * `rng`       — seeded RNG for this session
///
/// # Wheel scroll algorithm
///
/// Standard mouse wheels move ~100–120 px per notch.  We:
///   1. Choose a per-notch step size uniformly from [100, 120] px.
///   2. Compute the number of notches needed to cover `total_px`.
///   3. Sample the inter-event interval from N(130, 30) ms, clamped to [80, 300].
///      (Real users roll the wheel at 4–10 notches/second.)
///   4. With probability `direction_change_probability`, insert a 1–2 notch
///      reversed segment at a random position in the sequence.
///
/// # Trackpad scroll algorithm
///
/// Trackpad momentum is modelled as exponential velocity decay:
///   1. Sample initial velocity v₀ from N(speed_mean, speed_std).
///   2. Every 16 ms (one display frame), emit a delta of v(t) * 0.016 px.
///   3. v(t+1) = v(t) * momentum_deceleration.
///   4. Stop when |v| < 5 px/s (imperceptible motion).
///   5. Add tiny Gaussian direction perturbations (±2 px/s per axis) to
///      simulate imprecise finger trajectories.
///   6. With `direction_change_probability`: briefly reverse for 3–8 frames
///      mid-sequence.
pub fn generate_scroll(
    direction: ScrollDirection,
    total_px: f64,
    profile: &ScrollProfile,
    rng: &mut SmallRng,
) -> Vec<ScrollEvent> {
    let total_px = total_px.abs().max(1.0);
    let sign = direction.delta_sign();
    let horizontal = direction.is_horizontal();

    match profile.scroll_type {
        ScrollType::Wheel => {
            generate_wheel_scroll(direction, total_px, sign, horizontal, profile, rng)
        }
        ScrollType::Trackpad => {
            generate_trackpad_scroll(direction, total_px, sign, horizontal, profile, rng)
        }
    }
}

fn generate_wheel_scroll(
    _direction: ScrollDirection,
    total_px: f64,
    sign: f64,
    horizontal: bool,
    profile: &ScrollProfile,
    rng: &mut SmallRng,
) -> Vec<ScrollEvent> {
    // --- Step size per wheel notch ---
    // Standard HID wheel: 120 units = 1 notch.  Browsers typically translate
    // this to ~100 px, but there is device variation.
    let step_px = uniform(rng, 100.0, 120.0);
    let n_notches = (total_px / step_px).ceil() as usize;

    // --- Decide on a direction-change segment ---
    let do_reverse = uniform(rng, 0.0, 1.0) < profile.direction_change_probability;
    // If reversing, choose a position somewhere in the middle of the sequence
    // and a length of 1–2 notches.
    let reverse_at = if do_reverse && n_notches > 4 {
        Some((uniform(rng, 0.3, 0.7) * n_notches as f64) as usize)
    } else {
        None
    };
    let reverse_len = if do_reverse {
        uniform(rng, 1.0, 2.5) as usize + 1
    } else {
        0
    };

    let mut events: Vec<ScrollEvent> = Vec::with_capacity(n_notches + reverse_len + 2);
    let mut cursor_ms: u64 = 0;
    let mut accumulated_px = 0.0_f64;
    let mut reverse_remaining = 0usize;
    let mut in_reverse = false;

    for i in 0..n_notches {
        // Check if we should start a direction reversal.
        if let Some(rev_pos) = reverse_at
            && i == rev_pos
            && !in_reverse
        {
            in_reverse = true;
            reverse_remaining = reverse_len;
        }

        // Determine this notch's direction.
        let this_sign = if in_reverse { -sign } else { sign };

        // Actual step for this notch (slight variation).
        let this_step = uniform(rng, step_px * 0.9, step_px * 1.1);

        // Don't scroll past the total if we're in the forward direction.
        let clamped_step = if !in_reverse {
            (total_px - accumulated_px).min(this_step).max(0.0)
        } else {
            this_step
        };

        let (dx, dy) = if horizontal {
            (this_sign * clamped_step, 0.0)
        } else {
            (0.0, this_sign * clamped_step)
        };

        events.push(make_event(dx, dy, cursor_ms, &ScrollType::Wheel));

        if !in_reverse {
            accumulated_px += clamped_step;
        }

        // Inter-notch interval: N(130, 30) ms, clamped to [80, 300].
        // This corresponds to a scrolling rate of ~7.7 notches/s — typical
        // for comfortable page scrolling.
        let interval = clamp(normal_sample(rng, 130.0, 30.0), 80.0, 300.0) as u64;
        cursor_ms += interval;

        if in_reverse {
            reverse_remaining = reverse_remaining.saturating_sub(1);
            if reverse_remaining == 0 {
                in_reverse = false;
            }
        }

        if accumulated_px >= total_px && !in_reverse {
            break;
        }
    }

    events
}

fn generate_trackpad_scroll(
    _direction: ScrollDirection,
    total_px: f64,
    sign: f64,
    horizontal: bool,
    profile: &ScrollProfile,
    rng: &mut SmallRng,
) -> Vec<ScrollEvent> {
    // --- Initial velocity ---
    // Sample from N(speed_mean, speed_std), clamped to a reasonable range.
    let v0 = clamp(
        normal_sample(rng, profile.speed_mean_px_per_s, profile.speed_std),
        100.0,
        8000.0,
    );

    // Frame period = 16.67 ms (60 Hz display refresh).
    const FRAME_MS: f64 = 16.667;
    const STOP_THRESHOLD_PX_S: f64 = 5.0;

    // α per frame: we use the profile's deceleration factor directly.
    // alpha = 0.85 means velocity retains 85 % each frame, giving a
    // ~180 ms half-life — consistent with macOS inertial scroll data.
    let alpha = clamp(profile.momentum_deceleration, 0.50, 0.99);

    // Pre-compute how many frames the momentum will last.
    // v(n) = v0 * alpha^n < threshold  =>  n > ln(threshold/v0) / ln(alpha)
    let max_frames = if v0 > STOP_THRESHOLD_PX_S {
        (((STOP_THRESHOLD_PX_S / v0).ln()) / alpha.ln()).ceil() as usize + 5
    } else {
        1
    };

    // --- Direction change setup ---
    let do_reverse = uniform(rng, 0.0, 1.0) < profile.direction_change_probability;
    let reverse_start = if do_reverse && max_frames > 10 {
        Some((uniform(rng, 0.25, 0.6) * max_frames as f64) as usize)
    } else {
        None
    };
    let reverse_frames = if do_reverse {
        (uniform(rng, 3.0, 8.0)) as usize + 1
    } else {
        0
    };

    let mut events: Vec<ScrollEvent> = Vec::with_capacity(max_frames + reverse_frames + 4);
    let mut v = v0;
    let mut cursor_ms: f64 = 0.0;
    let mut accumulated_px = 0.0_f64;
    let mut reverse_remaining = 0usize;
    let mut in_reverse = false;

    for frame in 0..max_frames {
        if v < STOP_THRESHOLD_PX_S && !in_reverse {
            break;
        }

        // Start a direction reversal if scheduled.
        if let Some(rs) = reverse_start
            && frame == rs
            && !in_reverse
        {
            in_reverse = true;
            reverse_remaining = reverse_frames;
        }

        let this_sign = if in_reverse { -sign } else { sign };

        // Delta for this frame = velocity × frame_duration_s
        let delta = this_sign * v * (FRAME_MS / 1000.0);

        // Tiny perpendicular perturbation: real fingers wobble slightly
        // during a swipe, producing small cross-axis deltas.  This is a key
        // fingerprint of real trackpad input — pure-axis scrolling is rare.
        let perturb = normal_sample(rng, 0.0, 0.8); // ±0.8 px/frame cross-axis

        let (dx, dy) = if horizontal {
            (delta, perturb)
        } else {
            (perturb, delta)
        };

        events.push(make_event(dx, dy, cursor_ms as u64, &ScrollType::Trackpad));

        if !in_reverse {
            accumulated_px += delta.abs();
            if accumulated_px >= total_px {
                // Reached the target; begin deceleration to stop.
                // We continue emitting frames until velocity drops naturally.
                // (Abrupt stop is a bot signal.)
            }
        }

        // Advance time by one frame (with slight jitter: ±2 ms).
        // Real display refresh is not perfectly 60 Hz due to OS scheduling.
        let frame_jitter = normal_sample(rng, 0.0, 2.0);
        cursor_ms += (FRAME_MS + frame_jitter).max(10.0);

        // Decay velocity.
        v *= alpha;

        if in_reverse {
            reverse_remaining = reverse_remaining.saturating_sub(1);
            if reverse_remaining == 0 {
                in_reverse = false;
                // Resume with slightly reduced velocity after reversal.
                v *= 0.7;
            }
        }
    }

    // If we stopped before covering total_px (e.g. initial velocity was low),
    // add a final event to close the gap — the user would naturally do another
    // swipe, but for API simplicity we emit one extra event here.
    if accumulated_px < total_px && !events.is_empty() {
        let remaining = total_px - accumulated_px;
        let last_ts = events.last().unwrap().timestamp_ms;
        let (dx, dy) = if horizontal {
            (sign * remaining, 0.0)
        } else {
            (0.0, sign * remaining)
        };
        events.push(make_event(
            dx,
            dy,
            last_ts + FRAME_MS as u64,
            &ScrollType::Trackpad,
        ));
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::BehaviorProfile;
    use crate::rng::seeded_rng;

    #[test]
    fn wheel_scroll_timestamps_monotonic() {
        let profile = BehaviorProfile::casual_user().scroll;
        let mut rng = seeded_rng(20);
        let events = generate_scroll(ScrollDirection::Down, 800.0, &profile, &mut rng);
        assert!(!events.is_empty());
        for w in events.windows(2) {
            assert!(w[1].timestamp_ms >= w[0].timestamp_ms);
        }
    }

    #[test]
    fn trackpad_scroll_timestamps_monotonic() {
        let profile = BehaviorProfile::power_user().scroll;
        let mut rng = seeded_rng(21);
        let events = generate_scroll(ScrollDirection::Down, 1200.0, &profile, &mut rng);
        assert!(!events.is_empty());
        for w in events.windows(2) {
            assert!(w[1].timestamp_ms >= w[0].timestamp_ms);
        }
    }

    #[test]
    fn wheel_scroll_covers_distance() {
        let profile = BehaviorProfile::casual_user().scroll;
        let mut rng = seeded_rng(22);
        let total = 500.0;
        let events = generate_scroll(ScrollDirection::Down, total, &profile, &mut rng);
        let net: f64 = events.iter().map(|e| e.delta_y).sum();
        // Net scroll should be close to total (direction changes reduce it slightly).
        assert!(net > 0.0, "expected net positive scroll, got {}", net);
    }

    #[test]
    fn trackpad_has_cross_axis_noise() {
        let profile = BehaviorProfile::power_user().scroll;
        let mut rng = seeded_rng(23);
        let events = generate_scroll(ScrollDirection::Down, 600.0, &profile, &mut rng);
        // At least some events should have non-zero delta_x (cross-axis perturbation).
        let has_cross = events.iter().any(|e| e.delta_x.abs() > 0.001);
        assert!(has_cross, "expected cross-axis noise in trackpad scroll");
    }
}
