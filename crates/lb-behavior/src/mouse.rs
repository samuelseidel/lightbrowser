// mouse.rs: pointer movement and clicks.
//
// Rewritten in LightBrowser (M5); the vendored Bezier generator used a
// smooth-step speed profile with 8 px arc-length samples and N(0, 3 px)
// click scatter regardless of target size. The model here, and where each
// piece comes from:
//
// - Movement time: Fitts' law in Shannon form, MT = a + b * log2(D/W + 1)
//   (Fitts 1954; MacKenzie 1992). `a` and `b` are per profile (mouse
//   throughput in ISO 9241-9 studies is about 3.5-5 bits/s, so b is
//   roughly 200-280 ms/bit); each movement gets log-normal noise (sigma
//   0.15) on top.
// - Submovements: the optimised-submovement model (Meyer et al. 1988). A
//   primary ballistic submovement covers most of the distance and ends with
//   an error proportional to its length (here sigma 3 % along the path and
//   2.5 % across). Undershoot is the common case (primary amplitude about
//   95 % of D); overshoot happens with `overshoot_rate`. When the primary
//   lands off the aim point by more than a small tolerance, one or two
//   short corrective submovements follow after a 50-150 ms pause.
// - Speed profile: minimum jerk (Flash & Hogan 1985),
//   s(tau) = 10 tau^3 - 15 tau^4 + 6 tau^5, time-warped with tau^g
//   (g in 0.8..1) so peak speed comes at 40-50 % of the movement: aimed
//   movements have a longer deceleration phase than acceleration phase.
// - Path shape: a cubic Bezier bowed sideways by N(0, curvature * D)
//   (arm and wrist rotation; typical maximum deviation 5-10 % of D), plus
//   lateral motor noise: a smooth random curve (independent normal knots
//   every 110-190 ms, SD `path_noise` * D + 0.4 px, Catmull-Rom between
//   them) faded in and out with the movement, so the path wobbles around
//   its arc. Motor noise grows with the size of the command (Harris &
//   Wolpert 1998); mouse paths in ISO 9241-9 tasks show a movement
//   variability of several pixels and a few movement-direction changes
//   per trial (MacKenzie, Kauppinen & Silfverberg 2001), which a smooth
//   arc alone does not have. The hand is a low-pass filter: the wobble
//   lives at submovement time scales (a few Hz), not frame to frame. (Until
//   2026-11-03 this was a first-order Ornstein-Uhlenbeck process with a
//   ~60 ms correlation time: rough at every scale, it gave 10-35 direction
//   changes per movement at 60 Hz and a zigzag of +-10-20 px per frame on
//   long moves; see tests/mouse_rules.rs.)
// - Endpoint scatter: aim points are normal around the target centre with
//   sigma = W / 4.133 per axis (the "effective width" of ISO 9241-9: 96 %
//   of hits inside W), truncated to the target, with a small bias toward
//   the side the pointer comes from (undershoot).
// - Sampling: the device reports at `device_hz` (USB mice poll at 125 Hz
//   by default) with sub-millisecond jitter; Chrome aligns continuous
//   input to display frames (`display_hz`), so the page sees at most one
//   mousemove per frame, carrying the latest device sample, and none when
//   the integer position did not change. Positions are whole CSS pixels,
//   as in Chrome's `MouseEvent.clientX`.
// - Tremor: physiological tremor at 8-12 Hz with about 0.3 px amplitude at
//   the cursor, plus small sensor noise.
// - Pressing: a verification pause between the cursor stopping and the
//   press (log-normal, median 60 ms), a button hold time log-normal around
//   `press_hold_ms` (sigma 0.25; measured click durations are typically
//   80-150 ms), and for double clicks a release-to-press gap with median
//   110 ms (well inside the 500 ms OS double-click time).

use rand::rngs::SmallRng;
use serde::{Deserialize, Serialize};

use crate::rng::{chance, clamp, log_normal, normal_sample, uniform};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A 2-D viewport coordinate in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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

    fn rounded(self) -> Point {
        Point::new(self.x.round(), self.y.round())
    }
}

/// A click target: an element's border box in viewport CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TargetBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl TargetBox {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// A box of `size` x `size` centred on `p` (targets known only by a
    /// point).
    pub fn around(p: Point, size: f64) -> Self {
        Self::new(p.x - size / 2.0, p.y - size / 2.0, size, size)
    }

    pub fn center(&self) -> Point {
        Point::new(self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x <= self.x + self.width && p.y >= self.y && p.y <= self.y + self.height
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

/// A mouse button.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    #[default]
    Left,
    Middle,
    Right,
}

impl Button {
    /// Bit in `MouseEvent.buttons`.
    pub fn bit(self) -> u8 {
        match self {
            Button::Left => 1,
            Button::Right => 2,
            Button::Middle => 4,
        }
    }
}

fn one() -> u32 {
    1
}

/// A single synthesised mouse event ready to be injected into the browser.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseEvent {
    pub x: f64,
    pub y: f64,
    /// Milliseconds since the start of the generated sequence.
    pub timestamp_ms: u64,
    /// Bitmask of buttons held after this event (left 1, right 2, middle 4).
    pub buttons: u8,
    pub event_type: MouseEventType,
    /// Button of a Down/Up/Click.
    #[serde(default)]
    pub button: Button,
    /// Click count of a Down/Up/Click (2 for the second press of a double
    /// click).
    #[serde(default = "one")]
    pub click_count: u32,
}

impl MouseEvent {
    fn moved(p: Point, t: f64) -> Self {
        Self {
            x: p.x,
            y: p.y,
            timestamp_ms: t.max(0.0).round() as u64,
            buttons: 0,
            event_type: MouseEventType::Move,
            button: Button::Left,
            click_count: 1,
        }
    }
}

/// Per-session mouse behaviour parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MouseProfile {
    /// Fitts' law intercept in ms.
    pub fitts_a_ms: f64,
    /// Fitts' law slope in ms per bit.
    pub fitts_b_ms: f64,
    /// Sideways bow of a path, as the standard deviation of the Bezier
    /// control-point offset relative to the distance.
    pub curvature: f64,
    /// Lateral motor noise: SD of the path's wobble around its arc, as a
    /// fraction of the distance (plus 0.4 px).
    pub path_noise: f64,
    /// Probability [0, 1] that the primary submovement overshoots the aim
    /// point (otherwise it undershoots).
    pub overshoot_rate: f64,
    /// Mean overshoot distance in pixels for a long (>= 400 px) movement;
    /// shorter movements overshoot proportionally less.
    pub overshoot_distance_px: f64,
    /// Device report rate (Hz).
    pub device_hz: f64,
    /// Display refresh rate (Hz): Chrome dispatches at most one mousemove
    /// per frame.
    pub display_hz: f64,
    /// Median button hold time in ms.
    pub press_hold_ms: f64,
    /// Scale of endpoint scatter: 1.0 is sigma = W / 4.133.
    pub endpoint_spread: f64,
    /// Whether to add micro-movement drift while the cursor is stationary.
    pub micro_movement_enabled: bool,
    /// Std-dev of per-sample micro-movement jitter in pixels.
    pub micro_movement_std_px: f64,
}

impl Default for MouseProfile {
    fn default() -> Self {
        crate::BehaviorProfile::casual_user().mouse
    }
}

// ---------------------------------------------------------------------------
// Trajectory model
// ---------------------------------------------------------------------------

/// Default target width (px) for movements with no known target.
pub const DEFAULT_TARGET_PX: f64 = 24.0;

/// Fitts' law movement time (ms) for distance `d` and target width `w`.
pub fn fitts_ms(profile: &MouseProfile, d: f64, w: f64) -> f64 {
    let id = (d / w.max(1.0) + 1.0).log2();
    profile.fitts_a_ms + profile.fitts_b_ms * id
}

/// One ballistic submovement.
#[derive(Debug, Clone)]
struct Submovement {
    start_ms: f64,
    duration_ms: f64,
    p0: Point,
    c1: Point,
    c2: Point,
    p3: Point,
    /// Time warp exponent of the minimum-jerk profile.
    warp: f64,
    /// Arc-length lookup table: (t, cumulative length).
    lut: Vec<(f64, f64)>,
    /// Unit vector across the movement.
    perp: (f64, f64),
    /// Lateral noise knots over the movement (px), `wobble_step_ms` apart.
    wobble: Vec<f64>,
    wobble_step_ms: f64,
}

/// Catmull-Rom interpolation between `p1` and `p2` (smooth, through the
/// knots).
fn catmull_rom(p0: f64, p1: f64, p2: f64, p3: f64, f: f64) -> f64 {
    let (f2, f3) = (f * f, f * f * f);
    0.5 * (2.0 * p1
        + (p2 - p0) * f
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * f2
        + (3.0 * p1 - p0 - 3.0 * p2 + p3) * f3)
}

fn bezier(p0: Point, p1: Point, p2: Point, p3: Point, t: f64) -> Point {
    let u = 1.0 - t;
    let (uu, tt) = (u * u, t * t);
    Point {
        x: uu * u * p0.x + 3.0 * uu * t * p1.x + 3.0 * u * tt * p2.x + tt * t * p3.x,
        y: uu * u * p0.y + 3.0 * uu * t * p1.y + 3.0 * u * tt * p2.y + tt * t * p3.y,
    }
}

/// Minimum-jerk position profile on [0, 1].
fn min_jerk(tau: f64) -> f64 {
    let t = tau.clamp(0.0, 1.0);
    let t3 = t * t * t;
    t3 * (10.0 - 15.0 * t + 6.0 * t * t)
}

impl Submovement {
    fn new(
        from: Point,
        to: Point,
        start_ms: f64,
        duration_ms: f64,
        curvature: f64,
        noise: f64,
        rng: &mut SmallRng,
    ) -> Self {
        let d = from.dist(&to).max(1e-6);
        let (ux, uy) = ((to.x - from.x) / d, (to.y - from.y) / d);
        let (px, py) = (-uy, ux);
        // One main bow, sometimes slightly S-shaped.
        let b1 = normal_sample(rng, 0.0, curvature * d);
        let b2 = b1 * uniform(rng, 0.2, 1.0) + normal_sample(rng, 0.0, 0.3 * curvature * d);
        let c1 = Point::new(
            from.x + ux * d / 3.0 + px * b1,
            from.y + uy * d / 3.0 + py * b1,
        );
        let c2 = Point::new(
            from.x + ux * 2.0 * d / 3.0 + px * b2,
            from.y + uy * 2.0 * d / 3.0 + py * b2,
        );
        const STEPS: usize = 200;
        let mut lut = Vec::with_capacity(STEPS + 1);
        let mut len = 0.0;
        let mut prev = from;
        lut.push((0.0, 0.0));
        for i in 1..=STEPS {
            let t = i as f64 / STEPS as f64;
            let p = bezier(from, c1, c2, to, t);
            len += prev.dist(&p);
            lut.push((t, len));
            prev = p;
        }
        // Lateral noise: smooth knots at submovement time scales.
        let duration_ms = duration_ms.max(1.0);
        let sd = noise * d + 0.4;
        let wobble_step_ms = uniform(rng, 110.0, 190.0);
        let n = (duration_ms / wobble_step_ms).ceil() as usize + 3;
        let wobble = (0..n).map(|_| normal_sample(rng, 0.0, sd)).collect();
        Self {
            start_ms,
            duration_ms,
            p0: from,
            c1,
            c2,
            p3: to,
            warp: uniform(rng, 0.8, 1.0),
            lut,
            perp: (px, py),
            wobble,
            wobble_step_ms,
        }
    }

    fn end_ms(&self) -> f64 {
        self.start_ms + self.duration_ms
    }

    /// Position at absolute time `t`.
    fn at(&self, t: f64) -> Point {
        let tau = ((t - self.start_ms) / self.duration_ms).clamp(0.0, 1.0);
        let s = min_jerk(tau.powf(self.warp));
        let total = self.lut.last().map_or(0.0, |l| l.1);
        let want = s * total;
        let i = self.lut.partition_point(|&(_, l)| l < want);
        let t_param = if i == 0 {
            0.0
        } else if i >= self.lut.len() {
            1.0
        } else {
            let (t0, l0) = self.lut[i - 1];
            let (t1, l1) = self.lut[i];
            if l1 - l0 < 1e-9 {
                t1
            } else {
                t0 + (want - l0) / (l1 - l0) * (t1 - t0)
            }
        };
        let p = bezier(self.p0, self.c1, self.c2, self.p3, t_param);
        // Lateral noise, zero at both ends so the endpoints stay exact.
        let last = self.wobble.len() - 1;
        let k = (1.0 + (t - self.start_ms) / self.wobble_step_ms).clamp(1.0, (last - 1) as f64);
        let (i, f) = (k.floor() as usize, k.fract());
        let w = |j: usize| self.wobble[j.min(last)];
        let off =
            catmull_rom(w(i - 1), w(i), w(i + 1), w(i + 2), f) * (std::f64::consts::PI * tau).sin();
        Point::new(p.x + self.perp.0 * off, p.y + self.perp.1 * off)
    }
}

/// Plan the submovements that bring the pointer from `from` to `aim` for a
/// target of width `w` (px, measured along the movement).
fn plan(
    from: Point,
    aim: Point,
    w: f64,
    profile: &MouseProfile,
    rng: &mut SmallRng,
) -> Vec<Submovement> {
    let d = from.dist(&aim);
    if d < 1.0 {
        return Vec::new();
    }
    let mt = fitts_ms(profile, d, w) * log_normal(rng, 1.0, 0.15);
    let (ux, uy) = ((aim.x - from.x) / d, (aim.y - from.y) / d);
    let (px, py) = (-uy, ux);
    // Primary submovement endpoint.
    let along = if d > 40.0 && chance(rng, profile.overshoot_rate) {
        let mean = profile.overshoot_distance_px * (d / 400.0).min(1.0);
        d + normal_sample(rng, mean, 0.4 * mean).abs().max(2.0)
    } else {
        d * clamp(normal_sample(rng, 0.95, 0.03), 0.85, 1.0)
    };
    let across = normal_sample(rng, 0.0, 0.025 * d);
    let p1 = Point::new(
        from.x + ux * along + px * across,
        from.y + uy * along + py * across,
    );
    let tolerance = (0.15 * w).max(1.5);
    if p1.dist(&aim) <= tolerance {
        // The primary submovement lands on the aim point.
        return vec![Submovement::new(
            from,
            aim,
            0.0,
            mt,
            profile.curvature,
            profile.path_noise,
            rng,
        )];
    }
    let primary_ms = mt * uniform(rng, 0.75, 0.9);
    let mut subs = vec![Submovement::new(
        from,
        p1,
        0.0,
        primary_ms,
        profile.curvature,
        profile.path_noise,
        rng,
    )];
    // Corrective submovements: the first lands near the aim, a second (if
    // needed) on it.
    let mut cur = p1;
    for k in 0..2 {
        let err = cur.dist(&aim);
        if err < 1.0 {
            break;
        }
        let pause = clamp(log_normal(rng, 70.0, 0.4), 30.0, 250.0);
        let dur = (fitts_ms(profile, err, w) * 0.6).max(70.0) * log_normal(rng, 1.0, 0.15);
        let next = if k == 0 && err > 2.0 * tolerance {
            Point::new(
                aim.x + normal_sample(rng, 0.0, 0.1 * err),
                aim.y + normal_sample(rng, 0.0, 0.1 * err),
            )
        } else {
            aim
        };
        let start = subs.last().map_or(0.0, Submovement::end_ms) + pause;
        subs.push(Submovement::new(
            cur,
            next,
            start,
            dur,
            profile.curvature * 0.5,
            profile.path_noise,
            rng,
        ));
        cur = next;
    }
    if cur.dist(&aim) >= 1.0 {
        let start = subs.last().map_or(0.0, Submovement::end_ms) + 40.0;
        subs.push(Submovement::new(cur, aim, start, 60.0, 0.0, 0.0, rng));
    }
    subs
}

/// Position of the planned trajectory at `t` (at rest between and after
/// submovements).
fn position(subs: &[Submovement], from: Point, t: f64) -> Point {
    let mut p = from;
    for s in subs {
        if t < s.start_ms {
            return p;
        }
        p = s.at(t);
        if t <= s.end_ms() {
            return p;
        }
    }
    p
}

/// Sample a planned trajectory the way the page sees it: device reports,
/// coalesced to one event per display frame, whole pixels, no event when
/// the position did not change. The last event is at `aim` (rounded).
fn sample(
    subs: &[Submovement],
    from: Point,
    aim: Point,
    profile: &MouseProfile,
    rng: &mut SmallRng,
) -> (Vec<MouseEvent>, f64) {
    let end = subs.last().map_or(0.0, Submovement::end_ms);
    let device = 1000.0 / profile.device_hz.max(1.0);
    let frame = 1000.0 / profile.display_hz.max(1.0);
    // Tremor: two sinusoids (x, y) at 8-12 Hz plus sensor noise.
    let f = uniform(rng, 8.0, 12.0) / 1000.0 * std::f64::consts::TAU;
    let (phx, phy) = (
        uniform(rng, 0.0, std::f64::consts::TAU),
        uniform(rng, 0.0, std::f64::consts::TAU),
    );
    let amp = uniform(rng, 0.2, 0.4);

    // Device samples.
    let mut samples: Vec<(f64, Point)> = Vec::new();
    let mut t = uniform(rng, 0.0, device);
    while t <= end + device {
        let ts = t + normal_sample(rng, 0.0, 0.3);
        let mut p = position(subs, from, ts);
        if ts < end {
            p.x += amp * (f * ts + phx).sin() + normal_sample(rng, 0.0, 0.15);
            p.y += amp * (f * ts + phy).sin() + normal_sample(rng, 0.0, 0.15);
        }
        samples.push((ts.max(0.0), p));
        t += device;
    }
    // Coalesce to frames.
    let mut events = Vec::new();
    let mut last = from.rounded();
    let mut next = 0usize;
    let mut fr = uniform(rng, 0.0, frame);
    let mut last_t = 0.0f64;
    while next < samples.len() {
        let mut pick = None;
        while next < samples.len() && samples[next].0 <= fr {
            pick = Some(samples[next]);
            next += 1;
        }
        if let Some((ts, p)) = pick {
            let p = p.rounded();
            if p != last {
                events.push(MouseEvent::moved(p, ts.max(last_t)));
                last = p;
                last_t = ts.max(last_t);
            }
        }
        fr += frame + normal_sample(rng, 0.0, 0.4);
    }
    let aim_r = aim.rounded();
    if last != aim_r {
        last_t = last_t.max(end) + frame;
        events.push(MouseEvent::moved(aim_r, last_t));
    }
    (events, last_t.max(end))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Pointer movement from `from` to `to` (no particular target: Fitts width
/// [`DEFAULT_TARGET_PX`]). Returns only `Move` events; the last one is at
/// `to` rounded to whole pixels.
pub fn generate_mouse_path(
    from: Point,
    to: Point,
    profile: &MouseProfile,
    rng: &mut SmallRng,
) -> Vec<MouseEvent> {
    generate_path_to(from, to, DEFAULT_TARGET_PX, profile, rng).0
}

/// Movement to `aim` for a target of width `w`; also returns when the
/// pointer came to rest (ms).
pub fn generate_path_to(
    from: Point,
    aim: Point,
    w: f64,
    profile: &MouseProfile,
    rng: &mut SmallRng,
) -> (Vec<MouseEvent>, f64) {
    let subs = plan(from, aim, w, profile, rng);
    if subs.is_empty() {
        let aim_r = aim.rounded();
        if aim_r == from.rounded() {
            return (Vec::new(), 0.0);
        }
        return (vec![MouseEvent::moved(aim_r, 0.0)], 0.0);
    }
    sample(&subs, from, aim, profile, rng)
}

/// Pick the point inside `target` a person would hit when coming from
/// `from`: normal around the centre with sigma = W / 4.133 per axis
/// (scaled by `endpoint_spread`), a small undershoot bias toward `from`,
/// kept at least 1 px (or 15 %) inside the box. Whole pixels.
pub fn aim_point(
    from: Point,
    target: &TargetBox,
    profile: &MouseProfile,
    rng: &mut SmallRng,
) -> Point {
    let c = target.center();
    let d = from.dist(&c).max(1e-6);
    let (ux, uy) = ((c.x - from.x) / d, (c.y - from.y) / d);
    let k = profile.endpoint_spread / 4.133;
    let bias = 0.04;
    let x = c.x - ux * bias * target.width + normal_sample(rng, 0.0, k * target.width);
    let y = c.y - uy * bias * target.height + normal_sample(rng, 0.0, k * target.height);
    let inset_x = (0.15 * target.width)
        .min(4.0)
        .max(target.width.min(1.0) * 0.5);
    let inset_y = (0.15 * target.height)
        .min(4.0)
        .max(target.height.min(1.0) * 0.5);
    let lo_x = target.x + inset_x;
    let hi_x = (target.x + target.width - inset_x).max(lo_x);
    let lo_y = target.y + inset_y;
    let hi_y = (target.y + target.height - inset_y).max(lo_y);
    let p = Point::new(clamp(x, lo_x, hi_x).round(), clamp(y, lo_y, hi_y).round());
    // Rounding can leave a tiny box; keep the point inside it.
    Point::new(
        clamp(
            p.x,
            target.x.ceil(),
            (target.x + target.width).floor().max(target.x.ceil()),
        ),
        clamp(
            p.y,
            target.y.ceil(),
            (target.y + target.height).floor().max(target.y.ceil()),
        ),
    )
}

/// Click options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClickOptions {
    pub button: Button,
    /// 1 for a click, 2 for a double click, ...
    pub click_count: u32,
}

impl Default for ClickOptions {
    fn default() -> Self {
        Self {
            button: Button::Left,
            click_count: 1,
        }
    }
}

/// Move to a point inside `target` and click it. Returns the events (moves,
/// then Down/Up/Click per click) and the clicked point.
pub fn generate_click_in(
    from: Point,
    target: &TargetBox,
    opts: ClickOptions,
    profile: &MouseProfile,
    rng: &mut SmallRng,
) -> (Vec<MouseEvent>, Point) {
    let aim = aim_point(from, target, profile, rng);
    // Fitts width along the movement direction.
    let d = from.dist(&aim).max(1e-6);
    let (ux, uy) = ((aim.x - from.x) / d, (aim.y - from.y) / d);
    let w = (ux.abs() * target.width + uy.abs() * target.height).max(4.0);
    let (mut events, rest) = generate_path_to(from, aim, w, profile, rng);
    let mut t = rest + clamp(log_normal(rng, 60.0, 0.5), 15.0, 400.0);
    for n in 1..=opts.click_count.max(1) {
        let hold = clamp(log_normal(rng, profile.press_hold_ms, 0.25), 35.0, 350.0);
        let base = MouseEvent {
            x: aim.x,
            y: aim.y,
            timestamp_ms: t.round() as u64,
            buttons: opts.button.bit(),
            event_type: MouseEventType::Down,
            button: opts.button,
            click_count: n,
        };
        let up_t = (t + hold).round() as u64;
        events.push(base.clone());
        events.push(MouseEvent {
            timestamp_ms: up_t,
            buttons: 0,
            event_type: MouseEventType::Up,
            ..base.clone()
        });
        events.push(MouseEvent {
            timestamp_ms: up_t,
            buttons: 0,
            event_type: MouseEventType::Click,
            ..base
        });
        t += hold + clamp(log_normal(rng, 110.0, 0.25), 50.0, 300.0);
    }
    (events, aim)
}

/// Move to `position` (a target known only by its centre, treated as a
/// [`DEFAULT_TARGET_PX`] box) and left-click it.
pub fn generate_click(
    from: Point,
    position: Point,
    profile: &MouseProfile,
    rng: &mut SmallRng,
) -> Vec<MouseEvent> {
    let target = TargetBox::around(position, DEFAULT_TARGET_PX);
    generate_click_in(from, &target, ClickOptions::default(), profile, rng).0
}

/// Generate micro-movement events for hovering over a fixed point.
///
/// While a user reads a tooltip or a modal, the cursor is not perfectly
/// still. A random walk with a soft pull back to `position`, one step every
/// ~200 ms, whole pixels; steps that do not change the pixel are dropped.
pub fn generate_micro_movements(
    position: Point,
    duration_ms: u64,
    profile: &MouseProfile,
    rng: &mut SmallRng,
) -> Vec<MouseEvent> {
    if !profile.micro_movement_enabled || profile.micro_movement_std_px <= 0.0 {
        return Vec::new();
    }
    let mut t = 0.0;
    let (mut x, mut y) = (position.x, position.y);
    let mut last = position.rounded();
    let mut events = Vec::new();
    loop {
        t += clamp(log_normal(rng, 200.0, 0.3), 50.0, 600.0);
        if t > duration_ms as f64 {
            break;
        }
        x += normal_sample(rng, 0.0, profile.micro_movement_std_px);
        y += normal_sample(rng, 0.0, profile.micro_movement_std_px);
        x = x * 0.9 + position.x * 0.1;
        y = y * 0.9 + position.y * 0.1;
        let p = Point::new(x, y).rounded();
        if p != last {
            events.push(MouseEvent::moved(p, t));
            last = p;
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::BehaviorProfile;
    use crate::rng::seeded_rng;

    fn profile() -> MouseProfile {
        BehaviorProfile::casual_user().mouse
    }

    fn moves(events: &[MouseEvent]) -> Vec<&MouseEvent> {
        events
            .iter()
            .filter(|e| e.event_type == MouseEventType::Move)
            .collect()
    }

    #[test]
    fn path_ends_exactly_on_target() {
        for seed in 0..50 {
            let mut rng = seeded_rng(seed);
            let to = Point::new(800.0, 400.0);
            let ev = generate_mouse_path(Point::new(100.0, 100.0), to, &profile(), &mut rng);
            let last = ev.last().unwrap();
            assert_eq!((last.x, last.y), (800.0, 400.0), "seed {seed}");
        }
    }

    #[test]
    fn timestamps_monotonic_and_whole_pixels() {
        for seed in 0..50 {
            let mut rng = seeded_rng(seed);
            let ev = generate_mouse_path(
                Point::new(0.0, 0.0),
                Point::new(1000.0, 500.0),
                &profile(),
                &mut rng,
            );
            for w in ev.windows(2) {
                assert!(w[1].timestamp_ms >= w[0].timestamp_ms);
                assert!((w[0].x, w[0].y) != (w[1].x, w[1].y), "duplicate position");
            }
            assert!(ev.iter().all(|e| e.x.fract() == 0.0 && e.y.fract() == 0.0));
        }
    }

    #[test]
    fn sampling_is_frame_coalesced() {
        // At 60 Hz, no two moves are much closer than a frame apart and the
        // gaps are not all identical.
        let mut rng = seeded_rng(9);
        let ev = generate_mouse_path(
            Point::new(50.0, 50.0),
            Point::new(900.0, 600.0),
            &profile(),
            &mut rng,
        );
        let gaps: Vec<u64> = ev
            .windows(2)
            .map(|w| w[1].timestamp_ms - w[0].timestamp_ms)
            .collect();
        assert!(gaps.len() > 20);
        assert!(gaps.iter().all(|&g| g >= 8), "{gaps:?}");
        assert!(gaps.iter().any(|&g| g != gaps[0]));
    }

    #[test]
    fn fitts_duration_grows_with_difficulty() {
        let p = profile();
        let mean = |d: f64, w: f64| {
            (0..200)
                .map(|s| {
                    let mut rng = seeded_rng(s);
                    generate_path_to(Point::new(0.0, 0.0), Point::new(d, 0.0), w, &p, &mut rng).1
                })
                .sum::<f64>()
                / 200.0
        };
        let easy = mean(100.0, 100.0);
        let hard = mean(800.0, 10.0);
        assert!(hard > easy * 2.0, "easy {easy} hard {hard}");
        // Roughly Fitts: within 40 % of the prediction plus corrections.
        let pred = fitts_ms(&p, 800.0, 10.0);
        assert!(
            hard > pred * 0.8 && hard < pred * 1.8,
            "hard {hard} pred {pred}"
        );
    }

    #[test]
    fn speed_profile_is_bell_shaped() {
        // Middle of the primary movement is faster than its ends.
        let mut rng = seeded_rng(4);
        let mut p = profile();
        p.overshoot_rate = 0.0;
        let ev = generate_mouse_path(Point::new(0.0, 0.0), Point::new(900.0, 0.0), &p, &mut rng);
        let v: Vec<f64> = ev
            .windows(2)
            .filter(|w| w[1].timestamp_ms > w[0].timestamp_ms)
            .map(|w| {
                ((w[1].x - w[0].x).hypot(w[1].y - w[0].y))
                    / (w[1].timestamp_ms - w[0].timestamp_ms) as f64
            })
            .collect();
        let q = v.len() / 4;
        let ends: f64 =
            (v[..q].iter().sum::<f64>() + v[v.len() - q..].iter().sum::<f64>()) / (2 * q) as f64;
        let mid: f64 = v[q..v.len() - q].iter().sum::<f64>() / (v.len() - 2 * q) as f64;
        assert!(mid > ends * 1.3, "mid {mid} ends {ends}");
    }

    #[test]
    fn paths_curve() {
        // Maximum sideways deviation from the chord is usually > 1 px for a
        // long movement, and small relative to its length.
        let mut curved = 0;
        for seed in 0..40 {
            let mut rng = seeded_rng(seed);
            let ev = generate_mouse_path(
                Point::new(0.0, 0.0),
                Point::new(600.0, 0.0),
                &profile(),
                &mut rng,
            );
            let dev = ev.iter().map(|e| e.y.abs()).fold(0.0, f64::max);
            assert!(dev < 200.0, "seed {seed}: {dev}");
            if dev > 3.0 {
                curved += 1;
            }
        }
        assert!(curved >= 30, "{curved}/40 curved");
    }

    #[test]
    fn paths_wobble_around_their_arc() {
        // Movement direction changes (sign changes of the lateral motion,
        // MacKenzie et al. 2001) happen several times on a long movement.
        let mut total = 0;
        for seed in 0..40 {
            let mut rng = seeded_rng(seed);
            let ev = generate_mouse_path(
                Point::new(0.0, 300.0),
                Point::new(700.0, 300.0),
                &profile(),
                &mut rng,
            );
            let mut last = 0.0f64;
            for w in ev.windows(2) {
                let dy = w[1].y - w[0].y;
                if dy != 0.0 {
                    if last != 0.0 && dy.signum() != last.signum() {
                        total += 1;
                    }
                    last = dy;
                }
            }
        }
        let mean = total as f64 / 40.0;
        assert!(mean >= 2.0, "mean direction changes {mean}");
    }

    #[test]
    fn clicks_scatter_inside_the_target() {
        let target = TargetBox::new(300.0, 200.0, 120.0, 40.0);
        let mut off_centre = 0;
        let mut xs = Vec::new();
        for seed in 0..200 {
            let mut rng = seeded_rng(seed);
            let (ev, p) = generate_click_in(
                Point::new(10.0, 10.0),
                &target,
                ClickOptions::default(),
                &profile(),
                &mut rng,
            );
            assert!(target.contains(p), "{p:?}");
            let down = ev
                .iter()
                .find(|e| e.event_type == MouseEventType::Down)
                .unwrap();
            assert_eq!((down.x, down.y), (p.x, p.y));
            let last_move = moves(&ev).last().map(|m| (m.x, m.y));
            assert_eq!(last_move, Some((p.x, p.y)));
            let c = target.center();
            if (p.x - c.x).abs() >= 1.0 || (p.y - c.y).abs() >= 1.0 {
                off_centre += 1;
            }
            xs.push(p.x - c.x);
        }
        assert!(off_centre > 150, "{off_centre}/200 off centre");
        let sd = (xs.iter().map(|x| x * x).sum::<f64>() / xs.len() as f64).sqrt();
        // sigma = W / 4.133 = 29 px, truncated by the box.
        assert!(sd > 15.0 && sd < 35.0, "sd {sd}");
    }

    #[test]
    fn click_order_and_hold() {
        let mut holds = Vec::new();
        for seed in 0..100 {
            let mut rng = seeded_rng(seed);
            let ev = generate_click(
                Point::new(0.0, 0.0),
                Point::new(300.0, 200.0),
                &profile(),
                &mut rng,
            );
            let n = ev.len();
            assert_eq!(ev[n - 3].event_type, MouseEventType::Down);
            assert_eq!(ev[n - 2].event_type, MouseEventType::Up);
            assert_eq!(ev[n - 1].event_type, MouseEventType::Click);
            assert_eq!(ev[n - 3].buttons, 1);
            assert_eq!(ev[n - 2].buttons, 0);
            for w in ev.windows(2) {
                assert!(w[1].timestamp_ms >= w[0].timestamp_ms);
            }
            assert!(ev[n - 3].timestamp_ms > ev[n - 4].timestamp_ms);
            holds.push(ev[n - 2].timestamp_ms - ev[n - 3].timestamp_ms);
        }
        holds.sort_unstable();
        let med = holds[50];
        assert!((70..=130).contains(&med), "median hold {med}");
        assert!(holds[0] >= 35 && holds[99] <= 350);
    }

    #[test]
    fn double_click_counts() {
        let mut rng = seeded_rng(3);
        let (ev, _) = generate_click_in(
            Point::new(0.0, 0.0),
            &TargetBox::new(100.0, 100.0, 50.0, 20.0),
            ClickOptions {
                button: Button::Right,
                click_count: 2,
            },
            &profile(),
            &mut rng,
        );
        let downs: Vec<&MouseEvent> = ev
            .iter()
            .filter(|e| e.event_type == MouseEventType::Down)
            .collect();
        assert_eq!(downs.len(), 2);
        assert_eq!((downs[0].click_count, downs[1].click_count), (1, 2));
        assert!(
            downs
                .iter()
                .all(|d| d.button == Button::Right && d.buttons == 2)
        );
    }

    #[test]
    fn short_or_zero_moves() {
        let mut rng = seeded_rng(1);
        let p = Point::new(10.0, 10.0);
        assert!(generate_mouse_path(p, p, &profile(), &mut rng).is_empty());
        let ev = generate_mouse_path(p, Point::new(11.0, 10.0), &profile(), &mut rng);
        assert_eq!(ev.last().map(|e| (e.x, e.y)), Some((11.0, 10.0)));
    }
}
