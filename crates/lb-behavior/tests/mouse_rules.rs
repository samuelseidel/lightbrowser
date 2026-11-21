//! The detect-bench mouse rules (`beh_mouse_velocity`, `beh_mouse_linear`
//! in `tests/detect-bench/sites/collector.js`) applied to the model's
//! clicks, one movement per page (the worst case: the rules judge every
//! stroke of a page together), and the lateral roughness of its paths.

use lb_behavior::BehaviorProfile;
use lb_behavior::mouse::{
    ClickOptions, MouseEventType, MouseProfile, Point, TargetBox, generate_click_in,
};
use lb_behavior::rng::{mix_seed, seeded_rng};

#[derive(Clone, Copy)]
struct M {
    t: f64,
    x: f64,
    y: f64,
}

fn mean(a: &[f64]) -> f64 {
    a.iter().sum::<f64>() / a.len().max(1) as f64
}

/// collector.js stroke cuts: a gap over 250 ms, a press, or a turn of more
/// than 25 degrees between steps of at least 2 px.
fn strokes(moves: &[M], downs: &[f64]) -> Vec<Vec<M>> {
    let mut out = Vec::new();
    let mut cur: Vec<M> = Vec::new();
    let mut last_dir: Option<f64> = None;
    for &m in moves {
        if let Some(&last) = cur.last() {
            let pressed = downs.iter().any(|&d| d > last.t && d <= m.t);
            if m.t - last.t > 250.0 || pressed {
                out.push(std::mem::take(&mut cur));
                last_dir = None;
            } else if (m.x - last.x).hypot(m.y - last.y) >= 2.0 {
                let dir = (m.y - last.y).atan2(m.x - last.x);
                if let Some(ld) = last_dir {
                    let mut d = (dir - ld).abs();
                    if d > std::f64::consts::PI {
                        d = 2.0 * std::f64::consts::PI - d;
                    }
                    if d > 25f64.to_radians() {
                        out.push(std::mem::take(&mut cur));
                        cur.push(last);
                    }
                }
                last_dir = Some(dir);
            }
        }
        cur.push(m);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// (velocity rule fired, linear rule fired, strokes judged by the velocity rule)
fn page_rules(moves: &[M], downs: &[f64]) -> (bool, bool, usize) {
    let mut shaped = Vec::new();
    for r in strokes(moves, downs) {
        if r.len() < 5 {
            continue;
        }
        let (a, b) = (r[0], r[r.len() - 1]);
        if (b.x - a.x).hypot(b.y - a.y) < 50.0 {
            continue;
        }
        let v: Vec<f64> = r
            .windows(2)
            .filter(|w| w[1].t - w[0].t > 0.0)
            .map(|w| (w[1].x - w[0].x).hypot(w[1].y - w[0].y) / (w[1].t - w[0].t))
            .collect();
        if v.len() < 8 {
            continue;
        }
        let q = v.len() / 4;
        let ends = mean(&[&v[..q], &v[v.len() - q..]].concat());
        let mid = mean(&v[q..v.len() - q]);
        if ends > 0.0 {
            shaped.push(((mid / ends) * 100.0).round() / 100.0);
        }
    }
    let velocity = !shaped.is_empty() && shaped.iter().all(|&b| b < 1.15);

    // Straightness (cut at pauses and presses only).
    let dist = |p: M, q: M| (q.x - p.x).hypot(q.y - p.y);
    let fits = |r: &[M], i: usize, j: usize| {
        let (a, b) = (r[i], r[j]);
        let c = dist(a, b);
        if c < 1.0 {
            return true;
        }
        r[i + 1..j].iter().all(|p| {
            let perp = ((b.x - a.x) * (a.y - p.y) - (a.x - p.x) * (b.y - a.y)).abs() / c;
            let along = ((p.x - a.x) * (b.x - a.x) + (p.y - a.y) * (b.y - a.y)) / c;
            perp <= 1.0 && along >= -1.0 && along <= c + 1.0
        })
    };
    let mut runs: Vec<Vec<M>> = Vec::new();
    let mut run: Vec<M> = Vec::new();
    for &m in moves {
        if let Some(&last) = run.last()
            && (m.t - last.t > 250.0 || downs.iter().any(|&d| d > last.t && d <= m.t))
        {
            runs.push(std::mem::take(&mut run));
        }
        run.push(m);
    }
    if !run.is_empty() {
        runs.push(run);
    }
    let (mut path, mut straight) = (0.0, 0.0);
    for r in &runs {
        for k in 1..r.len() {
            path += dist(r[k - 1], r[k]);
        }
        let mut i = 0;
        while i + 1 < r.len() {
            let mut j = i + 1;
            while j + 1 < r.len() && fits(r, i, j + 1) {
                j += 1;
            }
            if j - i + 1 >= 6 && dist(r[i], r[j]) >= 50.0 {
                straight += dist(r[i], r[j]);
            }
            i = j;
        }
    }
    let linear = path >= 200.0 && straight / path >= 0.85;
    (velocity, linear, shaped.len())
}

/// Movement direction changes (MacKenzie, Kauppinen & Silfverberg 2001):
/// sign changes of the motion across the task axis, here per page event.
fn mdc(moves: &[M], from: Point, to: Point) -> usize {
    let d = from.dist(&to).max(1e-9);
    let (px, py) = (-(to.y - from.y) / d, (to.x - from.x) / d);
    let mut last = 0.0f64;
    let mut n = 0;
    for w in moves.windows(2) {
        let lat = (w[1].x - w[0].x) * px + (w[1].y - w[0].y) * py;
        if lat.abs() >= 1.0 {
            if last != 0.0 && lat.signum() != last.signum() {
                n += 1;
            }
            last = lat;
        }
    }
    n
}

/// Bench-like single movements: (from, target box).
fn cases() -> Vec<(&'static str, Point, TargetBox)> {
    vec![
        (
            "long flick to a nav link",
            Point::new(1855.0, 20.0),
            TargetBox::new(370.0, 20.0, 50.0, 20.0),
        ),
        (
            "button to field",
            Point::new(900.0, 500.0),
            TargetBox::new(600.0, 120.0, 260.0, 36.0),
        ),
        (
            "diagonal 700 px",
            Point::new(200.0, 200.0),
            TargetBox::new(700.0, 680.0, 120.0, 40.0),
        ),
        (
            "short hop",
            Point::new(600.0, 300.0),
            TargetBox::new(700.0, 380.0, 120.0, 40.0),
        ),
    ]
}

fn simulate(profile: &MouseProfile, n: u64) -> Vec<(&'static str, f64, f64, f64)> {
    let mut out = Vec::new();
    for (name, from, target) in cases() {
        let (mut vel, mut lin, mut changes) = (0, 0, 0usize);
        for s in 0..n {
            let mut rng = seeded_rng(mix_seed(0x6d6f7573, s));
            let (ev, _) =
                generate_click_in(from, &target, ClickOptions::default(), profile, &mut rng);
            let moves: Vec<M> = ev
                .iter()
                .filter(|e| e.event_type == MouseEventType::Move)
                .map(|e| M {
                    t: e.timestamp_ms as f64,
                    x: e.x,
                    y: e.y,
                })
                .collect();
            let downs: Vec<f64> = ev
                .iter()
                .filter(|e| e.event_type == MouseEventType::Down)
                .map(|e| e.timestamp_ms as f64)
                .collect();
            let (v, l, _) = page_rules(&moves, &downs);
            vel += v as u32;
            lin += l as u32;
            changes += mdc(&moves, from, target.center());
        }
        let n = n as f64;
        out.push((name, vel as f64 / n, lin as f64 / n, changes as f64 / n));
    }
    out
}

/// A login-like page: three clicks about 200 px apart, the pointer staying
/// where the last click left it. Returns (velocity, linear) chance rates.
fn three_click_page(profile: &MouseProfile, n: u64) -> (f64, f64) {
    let targets = [
        TargetBox::new(170.0, 188.0, 360.0, 38.0),
        TargetBox::new(170.0, 272.0, 360.0, 38.0),
        TargetBox::new(170.0, 356.0, 360.0, 38.0),
    ];
    let (mut vel, mut lin) = (0, 0);
    for s in 0..n {
        let mut rng = seeded_rng(mix_seed(0x7061_6765, s));
        let mut from = Point::new(413.0, 36.0);
        let (mut moves, mut downs, mut t0) = (Vec::new(), Vec::new(), 0.0);
        for t in &targets {
            let (ev, at) = generate_click_in(from, t, ClickOptions::default(), profile, &mut rng);
            for e in &ev {
                let t = t0 + e.timestamp_ms as f64;
                match e.event_type {
                    MouseEventType::Move => moves.push(M { t, x: e.x, y: e.y }),
                    MouseEventType::Down => downs.push(t),
                    _ => {}
                }
            }
            t0 += ev.last().map_or(0.0, |e| e.timestamp_ms as f64) + 3000.0;
            from = at;
        }
        let (v, l, _) = page_rules(&moves, &downs);
        vel += v as u32;
        lin += l as u32;
    }
    (vel as f64 / n as f64, lin as f64 / n as f64)
}

#[test]
fn mouse_rules_on_single_movements() {
    for (preset, p) in [
        ("casual_user", BehaviorProfile::casual_user()),
        ("power_user", BehaviorProfile::power_user()),
        ("slow_reader", BehaviorProfile::slow_reader()),
    ] {
        for (name, vel, lin, mdc) in simulate(&p.mouse, 1000) {
            println!(
                "{preset:12} {name:26} velocity {:5.1}%  linear {:5.1}%  direction changes {mdc:.1}",
                vel * 100.0,
                lin * 100.0
            );
            // A few direction changes per movement, as in MacKenzie et al.
            // 2001, not a frame-to-frame zigzag (the first-order OU wobble
            // used before gave 10-35 here).
            assert!(mdc <= 8.0, "{preset} {name}: {mdc} direction changes");
            // Minimum-jerk strokes are faster in the middle; a movement
            // only looks flat to the rule when it is cut into fragments.
            assert!(vel < 0.05, "{preset} {name}: velocity rule {vel}");
        }
        // Not asserted: with a zero-mean bow some short moves are nearly
        // straight, and a page of only such moves trips beh_mouse_linear.
        let (vel, lin) = three_click_page(&p.mouse, 1000);
        println!(
            "{preset:12} three-click page            velocity {:5.1}%  linear {:5.1}%",
            vel * 100.0,
            lin * 100.0
        );
    }
}
