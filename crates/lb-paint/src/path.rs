//! Canvas paths (`CanvasPath` mixin and `Path2D`).
//!
//! Points are stored already transformed: the context's current path is
//! built in device space (each call maps its points through the transform
//! current at that call, as the spec requires); a `Path2D` is built with
//! the identity and mapped at fill/stroke time. Arcs and ellipses become
//! cubic Béziers (at most 90° each).

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use tiny_skia::{FillRule, Mask, Path, PathBuilder, Transform};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pt {
    pub x: f64,
    pub y: f64,
}

impl Pt {
    fn new(x: f64, y: f64) -> Pt {
        Pt { x, y }
    }
}

/// `ts` applied to a point.
pub(crate) fn map(ts: &Transform, x: f64, y: f64) -> Pt {
    let (sx, ky, kx, sy, tx, ty) = (
        f64::from(ts.sx),
        f64::from(ts.ky),
        f64::from(ts.kx),
        f64::from(ts.sy),
        f64::from(ts.tx),
        f64::from(ts.ty),
    );
    Pt::new(sx * x + kx * y + tx, ky * x + sy * y + ty)
}

#[derive(Clone, Debug, PartialEq)]
enum Seg {
    Move(Pt),
    Line(Pt),
    Quad(Pt, Pt),
    Cubic(Pt, Pt, Pt),
    Close,
}

/// A canvas path. See the module docs for the coordinate space.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PathData {
    segs: Vec<Seg>,
    cur: Option<Pt>,
    start: Option<Pt>,
}

fn finite(v: &[f64]) -> bool {
    v.iter().all(|x| x.is_finite())
}

impl PathData {
    pub fn new() -> PathData {
        PathData::default()
    }

    /// No segments at all.
    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }

    /// `beginPath()`.
    pub fn clear(&mut self) {
        *self = PathData::default();
    }

    fn push_move(&mut self, p: Pt) {
        if let Some(Seg::Move(last)) = self.segs.last_mut() {
            *last = p;
        } else {
            self.segs.push(Seg::Move(p));
        }
        self.cur = Some(p);
        self.start = Some(p);
    }

    fn ensure(&mut self, p: Pt) {
        if self.cur.is_none() {
            self.push_move(p);
        }
    }

    pub fn move_to(&mut self, ts: &Transform, x: f64, y: f64) {
        if finite(&[x, y]) {
            self.push_move(map(ts, x, y));
        }
    }

    pub fn line_to(&mut self, ts: &Transform, x: f64, y: f64) {
        if !finite(&[x, y]) {
            return;
        }
        let p = map(ts, x, y);
        if self.cur.is_none() {
            self.push_move(p);
            return;
        }
        self.line_dev(p);
    }

    fn line_dev(&mut self, p: Pt) {
        self.reopen();
        self.segs.push(Seg::Line(p));
        self.cur = Some(p);
    }

    fn reopen(&mut self) {
        if matches!(self.segs.last(), Some(Seg::Close))
            && let Some(s) = self.start
        {
            self.segs.push(Seg::Move(s));
        }
    }

    pub fn quad_to(&mut self, ts: &Transform, cx: f64, cy: f64, x: f64, y: f64) {
        if !finite(&[cx, cy, x, y]) {
            return;
        }
        let c = map(ts, cx, cy);
        self.ensure(c);
        self.reopen();
        let p = map(ts, x, y);
        self.segs.push(Seg::Quad(c, p));
        self.cur = Some(p);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn bezier_to(
        &mut self,
        ts: &Transform,
        c1x: f64,
        c1y: f64,
        c2x: f64,
        c2y: f64,
        x: f64,
        y: f64,
    ) {
        if !finite(&[c1x, c1y, c2x, c2y, x, y]) {
            return;
        }
        let c1 = map(ts, c1x, c1y);
        self.ensure(c1);
        self.reopen();
        let (c2, p) = (map(ts, c2x, c2y), map(ts, x, y));
        self.segs.push(Seg::Cubic(c1, c2, p));
        self.cur = Some(p);
    }

    /// `closePath()`.
    pub fn close(&mut self) {
        if self.cur.is_some() && !matches!(self.segs.last(), Some(Seg::Close) | None) {
            self.segs.push(Seg::Close);
            self.cur = self.start;
        }
    }

    /// `rect()`.
    pub fn rect(&mut self, ts: &Transform, x: f64, y: f64, w: f64, h: f64) {
        if !finite(&[x, y, w, h]) {
            return;
        }
        self.push_move(map(ts, x, y));
        self.line_dev(map(ts, x + w, y));
        self.line_dev(map(ts, x + w, y + h));
        self.line_dev(map(ts, x, y + h));
        self.close();
        self.push_move(map(ts, x, y));
    }

    /// `roundRect()` with radii already validated and expanded to
    /// (upper-left, upper-right, lower-right, lower-left) `(rx, ry)`.
    pub fn round_rect(
        &mut self,
        ts: &Transform,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        radii: [(f64, f64); 4],
    ) {
        if !finite(&[x, y, w, h]) || radii.iter().any(|r| !finite(&[r.0, r.1])) {
            return;
        }
        let [mut ul, mut ur, mut lr, mut ll] = radii;
        let (mut x, mut y, mut w, mut h) = (x, y, w, h);
        if w < 0.0 {
            x += w;
            w = -w;
            std::mem::swap(&mut ul, &mut ur);
            std::mem::swap(&mut ll, &mut lr);
        }
        if h < 0.0 {
            y += h;
            h = -h;
            std::mem::swap(&mut ul, &mut ll);
            std::mem::swap(&mut ur, &mut lr);
        }
        let mut scale: f64 = 1.0;
        for (sum, side) in [
            (ul.0 + ur.0, w),
            (ur.1 + lr.1, h),
            (lr.0 + ll.0, w),
            (ul.1 + ll.1, h),
        ] {
            if sum > side && sum > 0.0 {
                scale = scale.min(side / sum);
            }
        }
        for r in [&mut ul, &mut ur, &mut lr, &mut ll] {
            r.0 *= scale;
            r.1 *= scale;
        }
        const K: f64 = 0.552_284_749_830_793_6;
        self.push_move(map(ts, x + ul.0, y));
        self.line_dev(map(ts, x + w - ur.0, y));
        self.corner(
            ts,
            (x + w - ur.0, y),
            (x + w, y + ur.1),
            (x + w - ur.0 * (1.0 - K), y),
            (x + w, y + ur.1 * (1.0 - K)),
        );
        self.line_dev(map(ts, x + w, y + h - lr.1));
        self.corner(
            ts,
            (x + w, y + h - lr.1),
            (x + w - lr.0, y + h),
            (x + w, y + h - lr.1 * (1.0 - K)),
            (x + w - lr.0 * (1.0 - K), y + h),
        );
        self.line_dev(map(ts, x + ll.0, y + h));
        self.corner(
            ts,
            (x + ll.0, y + h),
            (x, y + h - ll.1),
            (x + ll.0 * (1.0 - K), y + h),
            (x, y + h - ll.1 * (1.0 - K)),
        );
        self.line_dev(map(ts, x, y + ul.1));
        self.corner(
            ts,
            (x, y + ul.1),
            (x + ul.0, y),
            (x, y + ul.1 * (1.0 - K)),
            (x + ul.0 * (1.0 - K), y),
        );
        self.close();
        self.push_move(map(ts, x, y));
    }

    fn corner(
        &mut self,
        ts: &Transform,
        from: (f64, f64),
        to: (f64, f64),
        c1: (f64, f64),
        c2: (f64, f64),
    ) {
        if from == to {
            return;
        }
        let p = map(ts, to.0, to.1);
        self.segs
            .push(Seg::Cubic(map(ts, c1.0, c1.1), map(ts, c2.0, c2.1), p));
        self.cur = Some(p);
    }

    /// `arc()`; the caller rejects a negative radius.
    #[allow(clippy::too_many_arguments)]
    pub fn arc(&mut self, ts: &Transform, x: f64, y: f64, r: f64, start: f64, end: f64, ccw: bool) {
        self.ellipse(ts, x, y, r, r, 0.0, start, end, ccw);
    }

    /// `ellipse()`; the caller rejects negative radii.
    #[allow(clippy::too_many_arguments)]
    pub fn ellipse(
        &mut self,
        ts: &Transform,
        x: f64,
        y: f64,
        rx: f64,
        ry: f64,
        rotation: f64,
        start: f64,
        end: f64,
        ccw: bool,
    ) {
        if !finite(&[x, y, rx, ry, rotation, start, end]) {
            return;
        }
        let sweep = arc_sweep(start, end, ccw);
        self.arc_sweep(ts, x, y, rx, ry, rotation, start, sweep);
    }

    #[allow(clippy::too_many_arguments)]
    fn arc_sweep(
        &mut self,
        ts: &Transform,
        x: f64,
        y: f64,
        rx: f64,
        ry: f64,
        rotation: f64,
        start: f64,
        sweep: f64,
    ) {
        let (sr, cr) = rotation.sin_cos();
        let pt = |ux: f64, uy: f64| {
            let (ex, ey) = (rx * ux, ry * uy);
            map(ts, x + ex * cr - ey * sr, y + ex * sr + ey * cr)
        };
        let p0 = pt(start.cos(), start.sin());
        if self.cur.is_some() {
            self.line_dev(p0);
        } else {
            self.push_move(p0);
        }
        if sweep == 0.0 {
            return;
        }
        let n = (sweep.abs() / FRAC_PI_2 - 1e-9).ceil().max(1.0) as usize;
        let step = sweep / n as f64;
        let k = 4.0 / 3.0 * (step / 4.0).tan();
        let mut a0 = start;
        for i in 0..n {
            let a1 = if i + 1 == n { start + sweep } else { a0 + step };
            let (s0, c0) = a0.sin_cos();
            let (s1, c1) = a1.sin_cos();
            let p = pt(c1, s1);
            self.segs.push(Seg::Cubic(
                pt(c0 - k * s0, s0 + k * c0),
                pt(c1 + k * s1, s1 - k * c1),
                p,
            ));
            self.cur = Some(p);
            a0 = a1;
        }
    }

    /// `arcTo()`; the caller rejects a negative radius.
    #[allow(clippy::too_many_arguments)]
    pub fn arc_to(&mut self, ts: &Transform, x1: f64, y1: f64, x2: f64, y2: f64, r: f64) {
        if !finite(&[x1, y1, x2, y2, r]) {
            return;
        }
        let p1d = map(ts, x1, y1);
        self.ensure(p1d);
        let Some(cur) = self.cur else { return };
        let Some(inv) = ts.invert() else {
            self.line_dev(p1d);
            return;
        };
        let p0 = map(&inv, cur.x, cur.y);
        let (p1, p2) = (Pt::new(x1, y1), Pt::new(x2, y2));
        let (v1x, v1y) = (p0.x - p1.x, p0.y - p1.y);
        let (v2x, v2y) = (p2.x - p1.x, p2.y - p1.y);
        let (l1, l2) = (v1x.hypot(v1y), v2x.hypot(v2y));
        let cross = v1x * v2y - v1y * v2x;
        if p0 == p1
            || p1 == p2
            || r == 0.0
            || l1 == 0.0
            || l2 == 0.0
            || cross.abs() < 1e-12 * l1 * l2
        {
            self.line_dev(p1d);
            return;
        }
        let (u1x, u1y, u2x, u2y) = (v1x / l1, v1y / l1, v2x / l2, v2y / l2);
        let cos = (u1x * u2x + u1y * u2y).clamp(-1.0, 1.0);
        let theta = cos.acos();
        let dist = r / (theta / 2.0).tan();
        let t1 = Pt::new(p1.x + u1x * dist, p1.y + u1y * dist);
        let t2 = Pt::new(p1.x + u2x * dist, p1.y + u2y * dist);
        let (bx, by) = (u1x + u2x, u1y + u2y);
        let bl = bx.hypot(by);
        let d = r / (theta / 2.0).sin();
        let c = Pt::new(p1.x + bx / bl * d, p1.y + by / bl * d);
        let a1 = (t1.y - c.y).atan2(t1.x - c.x);
        let a2 = (t2.y - c.y).atan2(t2.x - c.x);
        let mut sweep = a2 - a1;
        while sweep > PI {
            sweep -= TAU;
        }
        while sweep < -PI {
            sweep += TAU;
        }
        self.line_dev(map(ts, t1.x, t1.y));
        self.arc_sweep(ts, c.x, c.y, r, r, 0.0, a1, sweep);
    }

    /// `addPath(path, transform)`.
    pub fn add_path(&mut self, other: &PathData, ts: &Transform) {
        let m = |p: &Pt| map(ts, p.x, p.y);
        for s in &other.segs {
            match s {
                Seg::Move(p) => self.push_move(m(p)),
                Seg::Line(p) => {
                    self.reopen();
                    self.segs.push(Seg::Line(m(p)));
                    self.cur = Some(m(p));
                }
                Seg::Quad(c, p) => {
                    self.reopen();
                    self.segs.push(Seg::Quad(m(c), m(p)));
                    self.cur = Some(m(p));
                }
                Seg::Cubic(a, b, p) => {
                    self.reopen();
                    self.segs.push(Seg::Cubic(m(a), m(b), m(p)));
                    self.cur = Some(m(p));
                }
                Seg::Close => self.close(),
            }
        }
    }

    /// The path mapped through `ts`.
    pub fn transformed(&self, ts: &Transform) -> PathData {
        let mut p = PathData::new();
        p.add_path(self, ts);
        p
    }

    /// A tiny-skia path, `None` when there is nothing to draw.
    pub fn to_skia(&self) -> Option<Path> {
        let mut pb = PathBuilder::with_capacity(self.segs.len(), self.segs.len() * 3);
        let f = |p: &Pt| (p.x as f32, p.y as f32);
        for s in &self.segs {
            match s {
                Seg::Move(p) => {
                    let (x, y) = f(p);
                    pb.move_to(x, y);
                }
                Seg::Line(p) => {
                    let (x, y) = f(p);
                    pb.line_to(x, y);
                }
                Seg::Quad(c, p) => {
                    let ((cx, cy), (x, y)) = (f(c), f(p));
                    pb.quad_to(cx, cy, x, y);
                }
                Seg::Cubic(a, b, p) => {
                    let ((ax, ay), (bx, by), (x, y)) = (f(a), f(b), f(p));
                    pb.cubic_to(ax, ay, bx, by, x, y);
                }
                Seg::Close => pb.close(),
            }
        }
        pb.finish()
    }

    /// Whether (`x`, `y`) (same space as the path) is inside.
    pub fn contains(&self, x: f64, y: f64, rule: FillRule) -> bool {
        self.to_skia().is_some_and(|p| point_in(&p, x, y, rule))
    }
}

/// Coverage test of one point: the pixel centre of a 1x1 mask.
pub(crate) fn point_in(p: &Path, x: f64, y: f64, rule: FillRule) -> bool {
    if !x.is_finite() || !y.is_finite() {
        return false;
    }
    let Some(mut m) = Mask::new(1, 1) else {
        return false;
    };
    let ts = Transform::from_translate((0.5 - x) as f32, (0.5 - y) as f32);
    m.fill_path(p, rule, false, ts);
    m.data()[0] > 0
}

/// Sweep of an arc per the canvas spec (full turn when asked for one).
pub(crate) fn arc_sweep(start: f64, end: f64, ccw: bool) -> f64 {
    if !ccw && end - start >= TAU {
        return TAU;
    }
    if ccw && start - end >= TAU {
        return -TAU;
    }
    let mut d = (end - start) % TAU;
    if !ccw && d < 0.0 {
        d += TAU;
    } else if ccw && d > 0.0 {
        d -= TAU;
    }
    d
}

/// Parses SVG path data (`new Path2D("M0 0 L10 10")`). Parsing stops at
/// the first error, keeping what came before (as browsers do).
pub fn parse_svg_path(d: &str) -> PathData {
    let mut path = PathData::new();
    let id = Transform::identity();
    let mut s = Scanner {
        b: d.as_bytes(),
        i: 0,
    };
    let (mut cx, mut cy) = (0.0f64, 0.0f64);
    let (mut sx, mut sy) = (0.0f64, 0.0f64);
    // Last control point for S/T reflection, with the command kind.
    let mut last_c: Option<(f64, f64, bool)> = None;
    let mut cmd = 0u8;
    loop {
        s.ws();
        if s.done() {
            break;
        }
        if let Some(c) = s.peek().filter(u8::is_ascii_alphabetic) {
            s.i += 1;
            cmd = c;
        } else if cmd == 0 || matches!(cmd, b'z' | b'Z') {
            break;
        }
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (cx, cy) } else { (0.0, 0.0) };
        let ok = (|| -> Option<()> {
            match cmd.to_ascii_uppercase() {
                b'M' => {
                    let (x, y) = (s.num()? + ox, s.num()? + oy);
                    path.move_to(&id, x, y);
                    (cx, cy, sx, sy) = (x, y, x, y);
                    last_c = None;
                    // Further pairs are implicit line-tos.
                    cmd = if rel { b'l' } else { b'L' };
                }
                b'L' => {
                    let (x, y) = (s.num()? + ox, s.num()? + oy);
                    path.line_to(&id, x, y);
                    (cx, cy) = (x, y);
                    last_c = None;
                }
                b'H' => {
                    let x = s.num()? + ox;
                    path.line_to(&id, x, cy);
                    cx = x;
                    last_c = None;
                }
                b'V' => {
                    let y = s.num()? + oy;
                    path.line_to(&id, cx, y);
                    cy = y;
                    last_c = None;
                }
                b'C' => {
                    let v = [s.num()?, s.num()?, s.num()?, s.num()?, s.num()?, s.num()?];
                    let (x1, y1, x2, y2, x, y) = (
                        v[0] + ox,
                        v[1] + oy,
                        v[2] + ox,
                        v[3] + oy,
                        v[4] + ox,
                        v[5] + oy,
                    );
                    path.bezier_to(&id, x1, y1, x2, y2, x, y);
                    last_c = Some((x2, y2, true));
                    (cx, cy) = (x, y);
                }
                b'S' => {
                    let v = [s.num()?, s.num()?, s.num()?, s.num()?];
                    let (x1, y1) = match last_c {
                        Some((lx, ly, true)) => (2.0 * cx - lx, 2.0 * cy - ly),
                        _ => (cx, cy),
                    };
                    let (x2, y2, x, y) = (v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy);
                    path.bezier_to(&id, x1, y1, x2, y2, x, y);
                    last_c = Some((x2, y2, true));
                    (cx, cy) = (x, y);
                }
                b'Q' => {
                    let v = [s.num()?, s.num()?, s.num()?, s.num()?];
                    let (x1, y1, x, y) = (v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy);
                    path.quad_to(&id, x1, y1, x, y);
                    last_c = Some((x1, y1, false));
                    (cx, cy) = (x, y);
                }
                b'T' => {
                    let (x, y) = (s.num()? + ox, s.num()? + oy);
                    let (x1, y1) = match last_c {
                        Some((lx, ly, false)) => (2.0 * cx - lx, 2.0 * cy - ly),
                        _ => (cx, cy),
                    };
                    path.quad_to(&id, x1, y1, x, y);
                    last_c = Some((x1, y1, false));
                    (cx, cy) = (x, y);
                }
                b'A' => {
                    let (rx, ry, rot) = (s.num()?, s.num()?, s.num()?);
                    let (large, sweep) = (s.flag()?, s.flag()?);
                    let (x, y) = (s.num()? + ox, s.num()? + oy);
                    svg_arc(&mut path, (cx, cy), rx, ry, rot, large, sweep, (x, y));
                    (cx, cy) = (x, y);
                    last_c = None;
                }
                b'Z' => {
                    path.close();
                    (cx, cy) = (sx, sy);
                    last_c = None;
                }
                _ => return None,
            }
            Some(())
        })();
        if ok.is_none() {
            break;
        }
    }
    path
}

/// SVG elliptical arc (endpoint parameterization, SVG 1.1 F.6.5).
#[allow(clippy::too_many_arguments)]
fn svg_arc(
    path: &mut PathData,
    from: (f64, f64),
    rx: f64,
    ry: f64,
    rot_deg: f64,
    large: bool,
    sweep: bool,
    to: (f64, f64),
) {
    let id = Transform::identity();
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if from == to {
        return;
    }
    if rx == 0.0 || ry == 0.0 {
        path.line_to(&id, to.0, to.1);
        return;
    }
    let phi = rot_deg.to_radians();
    let (sp, cp) = phi.sin_cos();
    let (dx, dy) = ((from.0 - to.0) / 2.0, (from.1 - to.1) / 2.0);
    let x1p = cp * dx + sp * dy;
    let y1p = -sp * dx + cp * dy;
    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let mut coef = if den == 0.0 {
        0.0
    } else {
        (num / den).max(0.0).sqrt()
    };
    if large == sweep {
        coef = -coef;
    }
    let cxp = coef * rx * y1p / ry;
    let cyp = -coef * ry * x1p / rx;
    let cx = cp * cxp - sp * cyp + (from.0 + to.0) / 2.0;
    let cy = sp * cxp + cp * cyp + (from.1 + to.1) / 2.0;
    let ang = |ux: f64, uy: f64, vx: f64, vy: f64| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let t1 = ang(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut dt = ang(
        (x1p - cxp) / rx,
        (y1p - cyp) / ry,
        (-x1p - cxp) / rx,
        (-y1p - cyp) / ry,
    ) % TAU;
    if !sweep && dt > 0.0 {
        dt -= TAU;
    } else if sweep && dt < 0.0 {
        dt += TAU;
    }
    path.arc_sweep(&id, cx, cy, rx, ry, phi, t1, dt);
}

struct Scanner<'a> {
    b: &'a [u8],
    i: usize,
}

impl Scanner<'_> {
    fn done(&self) -> bool {
        self.i >= self.b.len()
    }
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }
    fn ws(&mut self) {
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_whitespace() || c == b',')
        {
            self.i += 1;
        }
    }
    fn flag(&mut self) -> Option<bool> {
        self.ws();
        let c = self.peek()?;
        self.i += 1;
        match c {
            b'0' => Some(false),
            b'1' => Some(true),
            _ => None,
        }
    }
    fn num(&mut self) -> Option<f64> {
        self.ws();
        let start = self.i;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.i += 1;
        }
        let mut digits = false;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
            digits = true;
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
                digits = true;
            }
        }
        if !digits {
            self.i = start;
            return None;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let save = self.i;
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.i += 1;
                }
            } else {
                self.i = save;
            }
        }
        std::str::from_utf8(&self.b[start..self.i])
            .ok()?
            .parse()
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> Transform {
        Transform::identity()
    }

    #[test]
    fn sweeps() {
        assert_eq!(arc_sweep(0.0, TAU, false), TAU);
        assert_eq!(arc_sweep(0.0, 3.0 * TAU, false), TAU);
        assert!((arc_sweep(PI, 0.0, false) - PI).abs() < 1e-12);
        assert!((arc_sweep(0.0, PI / 2.0, true) + 1.5 * PI).abs() < 1e-12);
        assert_eq!(arc_sweep(1.0, 1.0, false), 0.0);
    }

    #[test]
    fn rect_and_contains() {
        let mut p = PathData::new();
        p.rect(&id(), 10.0, 10.0, 20.0, 10.0);
        assert!(p.contains(15.0, 15.0, FillRule::Winding));
        assert!(!p.contains(5.0, 15.0, FillRule::Winding));
        let mut q = PathData::new();
        q.arc(&id(), 50.0, 50.0, 10.0, 0.0, TAU, false);
        assert!(q.contains(50.0, 50.0, FillRule::Winding));
        assert!(q.contains(58.0, 50.0, FillRule::Winding));
        assert!(!q.contains(61.0, 50.0, FillRule::Winding));
        // Transformed: points are mapped as they are added.
        let ts = Transform::from_row(2.0, 0.0, 0.0, 2.0, 0.0, 0.0);
        let mut t = PathData::new();
        t.rect(&ts, 10.0, 10.0, 5.0, 5.0);
        assert!(t.contains(25.0, 25.0, FillRule::Winding));
    }

    #[test]
    fn svg_path_data() {
        let p = parse_svg_path("M10 10 h 20 v 20 H10 z");
        assert!(p.contains(20.0, 20.0, FillRule::Winding));
        let q = parse_svg_path("M0,0 L10,0 L10,10 L0,10 Z M2 2 L8 2 L8 8 L2 8 Z");
        assert!(!q.contains(5.0, 5.0, FillRule::EvenOdd));
        assert!(q.contains(1.0, 5.0, FillRule::EvenOdd));
        let a = parse_svg_path("M 10 50 A 40 40 0 0 1 90 50 Z");
        assert!(a.contains(50.0, 30.0, FillRule::Winding));
        assert!(!a.contains(50.0, 60.0, FillRule::Winding));
        // Garbage after valid data keeps the valid part.
        let g = parse_svg_path("M0 0 L 10 0 L 10 10 Z L foo");
        assert!(g.contains(8.0, 2.0, FillRule::Winding));
        assert!(parse_svg_path("10 10").is_empty());
    }

    #[test]
    fn arc_to_rounds_a_corner() {
        let mut p = PathData::new();
        p.move_to(&id(), 0.0, 0.0);
        p.arc_to(&id(), 100.0, 0.0, 100.0, 100.0, 20.0);
        p.line_to(&id(), 100.0, 100.0);
        p.line_to(&id(), 0.0, 100.0);
        p.close();
        assert!(p.contains(50.0, 50.0, FillRule::Winding));
        assert!(!p.contains(99.0, 1.0, FillRule::Winding));
    }

    #[test]
    fn round_rect_corners_are_cut() {
        let mut p = PathData::new();
        p.round_rect(&id(), 0.0, 0.0, 100.0, 50.0, [(20.0, 20.0); 4]);
        assert!(p.contains(50.0, 25.0, FillRule::Winding));
        assert!(!p.contains(1.0, 1.0, FillRule::Winding));
        assert!(p.contains(20.0, 1.0, FillRule::Winding));
    }
}
