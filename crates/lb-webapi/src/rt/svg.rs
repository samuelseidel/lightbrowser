//! SVG geometry: `SVGGraphicsElement` (`getBBox`, `getCTM`,
//! `getScreenCTM`), `SVGGeometryElement` (`getTotalLength`,
//! `getPointAtLength`), `SVGTextContentElement` (character counts,
//! advances, positions, extents, rotations, hit testing) and the value
//! types they return (`SVGRect`, `SVGPoint`, `SVGMatrix`, plus
//! `SVGSVGElement.createSVG{Rect,Point,Matrix}`).
//!
//! Geometry follows Blink: path data is normalized to moves, lines and
//! cubics (arcs decomposed as in `SVGPathNormalizer`), circles, ellipses
//! and rounded rects are Skia conics, bounds are Skia's tight bounds and
//! shape lengths come from `SkContourMeasure`'s subdivision, all in
//! `f32`. Path elements keep quadratics and use `SkPath::arcTo` conics for
//! their box; their lengths and points use `SVGPathQuery`'s
//! `PathTraversalState` over normalized cubics, which lands within about
//! 1e-5 (relative) of Chrome 156 but not bit-exact.
//! Container boxes are the union of rendered children through their
//! transforms (`SVGLayoutSupport::ComputeContainerBoundingBoxes`).
//!
//! Text uses the same metric tables as layout and canvas (`font_for`,
//! the persona's `FontSet` for ink bounds): advances summed per
//! character, ascent and descent rounded at the screen font size
//! (`CalculateScreenFontSizeScalingFactor`), whitespace collapsed as
//! `white-space: normal`, `x`/`y`/`dx`/`dy`/`rotate` lists and
//! `text-anchor` per chunk. Not modelled: `textPath` layout, bidi,
//! vertical text, `textLength`, shaping, CSS geometry properties from
//! style sheets (inline `style` only).

use lb_dom::{Document, NodeId, NodeType, ns};
use lb_js::{CallCtx, HostError, ValueKind};
use lb_layout::LayoutEngine;
use lb_paint::{FontSet, TextAlign, TextBaseline, TextStyle};

use super::{Dict, Obj, Runtime, Window, getter, illegal, method, need_args, setter, this_obj};
use crate::registry::Registry;

type R = Result<(), HostError>;

// ----- value objects -----

/// The values behind `SVGRect`, `SVGPoint` and `SVGMatrix` wrappers.
#[derive(Clone, Debug)]
pub(crate) enum SvgObj {
    Rect([f32; 4]),
    Point([f32; 2]),
    Matrix([f64; 6]),
}

impl SvgObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            SvgObj::Rect(_) => "SVGRect",
            SvgObj::Point(_) => "SVGPoint",
            SvgObj::Matrix(_) => "SVGMatrix",
        }
    }
}

fn ret_obj(cx: &mut CallCtx<'_, '_, '_>, w: &Window, o: SvgObj) -> R {
    let id = w.new_obj(Obj::Svg(Box::new(o)));
    cx.ret(w.obj_arg(id))
}

fn svg_obj(w: &Window, id: u32) -> Option<SvgObj> {
    w.with_obj(id, |o| match o {
        Obj::Svg(s) => Some((**s).clone()),
        _ => None,
    })
    .flatten()
}

fn this_svg(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, SvgObj), HostError> {
    let id = this_obj(cx)?;
    svg_obj(w, id).map(|o| (id, o)).ok_or_else(illegal)
}

fn this_rect(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, [f32; 4]), HostError> {
    match this_svg(cx, w)? {
        (id, SvgObj::Rect(r)) => Ok((id, r)),
        _ => Err(illegal()),
    }
}

fn this_point(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, [f32; 2]), HostError> {
    match this_svg(cx, w)? {
        (id, SvgObj::Point(p)) => Ok((id, p)),
        _ => Err(illegal()),
    }
}

fn this_matrix(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, Tf), HostError> {
    match this_svg(cx, w)? {
        (id, SvgObj::Matrix(m)) => Ok((id, Tf(m))),
        _ => Err(illegal()),
    }
}

fn store(w: &Window, id: u32, o: SvgObj) {
    w.with_obj(id, |slot| {
        if let Obj::Svg(s) = slot {
            **s = o;
        }
    });
}

/// WebIDL `float` (restricted): non-finite values throw.
fn arg_float(cx: &mut CallCtx<'_, '_, '_>, i: usize, what: &str) -> Result<f32, HostError> {
    let v = cx.arg_f64(i)?;
    if !v.is_finite() {
        return Err(HostError::type_error(format!(
            "{what}: The provided float value is non-finite."
        )));
    }
    Ok(v as f32)
}

fn exec(op: &str, iface: &str) -> String {
    format!("Failed to execute '{op}' on '{iface}'")
}

// ----- transforms -----

/// A 2D affine transform `[a, b, c, d, e, f]` (Blink's `AffineTransform`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Tf([f64; 6]);

const ID: Tf = Tf([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

impl Tf {
    fn translate(x: f64, y: f64) -> Tf {
        Tf([1.0, 0.0, 0.0, 1.0, x, y])
    }

    fn scale(x: f64, y: f64) -> Tf {
        Tf([x, 0.0, 0.0, y, 0.0, 0.0])
    }

    fn rotate(deg: f64) -> Tf {
        let r = deg.to_radians();
        let (s, c) = (r.sin(), r.cos());
        Tf([c, s, -s, c, 0.0, 0.0])
    }

    /// `self · o` (`o` applied first).
    fn mul(&self, o: &Tf) -> Tf {
        let [a, b, c, d, e, f] = self.0;
        let [oa, ob, oc, od, oe, of] = o.0;
        Tf([
            a * oa + c * ob,
            b * oa + d * ob,
            a * oc + c * od,
            b * oc + d * od,
            a * oe + c * of + e,
            b * oe + d * of + f,
        ])
    }

    fn inverse(&self) -> Option<Tf> {
        let [a, b, c, d, e, f] = self.0;
        let det = a * d - b * c;
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        if b == 0.0 && c == 0.0 {
            return Some(Tf([1.0 / a, 0.0, 0.0, 1.0 / d, -e / a, -f / d]));
        }
        Some(Tf([
            d / det,
            -b / det,
            -c / det,
            a / det,
            (c * f - d * e) / det,
            (b * e - a * f) / det,
        ]))
    }

    fn map(&self, x: f64, y: f64) -> (f64, f64) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }

    fn is_translation(&self) -> bool {
        let [a, b, c, d, ..] = self.0;
        a == 1.0 && b == 0.0 && c == 0.0 && d == 1.0
    }

    /// `AffineTransform::MapRect`: bounding box of the mapped corners.
    fn map_rect(&self, r: Rf) -> Rf {
        if self.is_translation() {
            return Rf {
                x: r.x + self.0[4] as f32,
                y: r.y + self.0[5] as f32,
                ..r
            };
        }
        let pts = [
            (r.x, r.y),
            (r.x + r.w, r.y),
            (r.x + r.w, r.y + r.h),
            (r.x, r.y + r.h),
        ]
        .map(|(x, y)| {
            let (x, y) = self.map(f64::from(x), f64::from(y));
            (x as f32, y as f32)
        });
        let (mut x0, mut y0, mut x1, mut y1) = (pts[0].0, pts[0].1, pts[0].0, pts[0].1);
        for (x, y) in &pts[1..] {
            x0 = x0.min(*x);
            y0 = y0.min(*y);
            x1 = x1.max(*x);
            y1 = y1.max(*y);
        }
        Rf {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }
}

/// A float rect (`gfx::RectF`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Rf {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rf {
    fn new(x: f32, y: f32, w: f32, h: f32) -> Rf {
        Rf { x, y, w, h }
    }

    fn from_ltrb(l: f32, t: f32, r: f32, b: f32) -> Rf {
        Rf::new(l, t, r - l, b - t)
    }

    /// `UnionEvenIfEmpty`.
    fn union(&self, o: &Rf) -> Rf {
        Rf::from_ltrb(
            self.x.min(o.x),
            self.y.min(o.y),
            (self.x + self.w).max(o.x + o.w),
            (self.y + self.h).max(o.y + o.h),
        )
    }

    fn arr(&self) -> [f32; 4] {
        [self.x, self.y, self.w, self.h]
    }
}

// ----- parsing helpers -----

struct Scan<'a> {
    b: &'a [u8],
    i: usize,
}

impl Scan<'_> {
    fn new(s: &str) -> Scan<'_> {
        Scan {
            b: s.as_bytes(),
            i: 0,
        }
    }

    fn ws(&mut self) {
        while self
            .b
            .get(self.i)
            .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c'))
        {
            self.i += 1;
        }
    }

    /// Whitespace with at most one comma.
    fn sep(&mut self) {
        self.ws();
        if self.peek() == Some(b',') {
            self.i += 1;
            self.ws();
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn done(&self) -> bool {
        self.i >= self.b.len()
    }

    /// An SVG number (sign, digits, fraction, exponent).
    fn number(&mut self) -> Option<f32> {
        let start = self.i;
        let mut j = self.i;
        if matches!(self.b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let d0 = j;
        while self.b.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
        }
        let mut digits = j > d0;
        if self.b.get(j) == Some(&b'.') {
            let f0 = j + 1;
            let mut k = f0;
            while self.b.get(k).is_some_and(u8::is_ascii_digit) {
                k += 1;
            }
            if k > f0 {
                digits = true;
                j = k;
            } else if digits {
                j = k;
            }
        }
        if !digits {
            return None;
        }
        if matches!(self.b.get(j), Some(b'e' | b'E')) {
            let mut k = j + 1;
            if matches!(self.b.get(k), Some(b'+' | b'-')) {
                k += 1;
            }
            let e0 = k;
            while self.b.get(k).is_some_and(u8::is_ascii_digit) {
                k += 1;
            }
            if k > e0 {
                j = k;
            }
        }
        let s = std::str::from_utf8(&self.b[start..j]).ok()?;
        let v: f64 = s.parse().ok()?;
        let v = v as f32;
        if !v.is_finite() {
            return None;
        }
        self.i = j;
        Some(v)
    }

    /// Number followed by an optional separator.
    fn num_sep(&mut self) -> Option<f32> {
        let v = self.number()?;
        self.sep();
        Some(v)
    }

    fn flag(&mut self) -> Option<bool> {
        let v = match self.peek()? {
            b'0' => false,
            b'1' => true,
            _ => return None,
        };
        self.i += 1;
        self.sep();
        Some(v)
    }
}

/// A list of numbers (`points`, `rotate`, `viewBox`); stops at an error.
fn number_list(s: &str) -> (Vec<f32>, bool) {
    let mut sc = Scan::new(s);
    sc.ws();
    let mut out = Vec::new();
    while !sc.done() {
        match sc.num_sep() {
            Some(v) => out.push(v),
            None => return (out, false),
        }
    }
    (out, true)
}

/// The `transform` attribute (an invalid list is no transform, as in
/// Blink).
fn parse_transform_list(s: &str) -> Tf {
    let mut sc = Scan::new(s);
    let mut m = ID;
    sc.ws();
    while !sc.done() {
        let start = sc.i;
        while sc.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            sc.i += 1;
        }
        let name = std::str::from_utf8(&sc.b[start..sc.i]).unwrap_or("");
        sc.ws();
        if sc.peek() != Some(b'(') {
            return ID;
        }
        sc.i += 1;
        sc.ws();
        let mut args = Vec::new();
        while sc.peek() != Some(b')') {
            match sc.num_sep() {
                Some(v) => args.push(f64::from(v)),
                None => return ID,
            }
        }
        sc.i += 1;
        let t = match (name, args.as_slice()) {
            ("matrix", &[a, b, c, d, e, f]) => Tf([a, b, c, d, e, f]),
            ("translate", &[x]) => Tf::translate(x, 0.0),
            ("translate", &[x, y]) => Tf::translate(x, y),
            ("scale", &[s]) => Tf::scale(s, s),
            ("scale", &[x, y]) => Tf::scale(x, y),
            ("rotate", &[a]) => Tf::rotate(a),
            ("rotate", &[a, x, y]) => Tf::translate(x, y)
                .mul(&Tf::rotate(a))
                .mul(&Tf::translate(-x, -y)),
            ("skewX", &[a]) => Tf([1.0, 0.0, a.to_radians().tan(), 1.0, 0.0, 0.0]),
            ("skewY", &[a]) => Tf([1.0, a.to_radians().tan(), 0.0, 1.0, 0.0, 0.0]),
            _ => return ID,
        };
        m = m.mul(&t);
        sc.sep();
    }
    m
}

/// A declaration of the element's inline `style` attribute.
fn inline_style(d: &Document, n: NodeId, prop: &str) -> Option<String> {
    let style = d.element(n)?.attr("style")?;
    let mut found = None;
    for decl in style.split(';') {
        if let Some((k, v)) = decl.split_once(':')
            && k.trim().eq_ignore_ascii_case(prop)
        {
            let v = v.trim();
            let v = v
                .strip_suffix("!important")
                .map_or(v, str::trim_end)
                .to_string();
            found = Some(v);
        }
    }
    found
}

fn attr<'a>(d: &'a Document, n: NodeId, name: &str) -> Option<&'a str> {
    d.element(n)?.attr(name)
}

/// A property that is also a presentation attribute: inline style, then
/// the attribute.
fn prop(d: &Document, n: NodeId, name: &str) -> Option<String> {
    inline_style(d, n, name).or_else(|| attr(d, n, name).map(str::to_string))
}

/// An inherited presentation property (`text-anchor`).
fn inherited_prop(d: &Document, n: NodeId, name: &str) -> Option<String> {
    std::iter::successors(Some(n), |&p| d.parent(p))
        .take_while(|&p| d.node_type(p) == NodeType::Element)
        .find_map(|p| prop(d, p, name).filter(|v| v.trim() != "inherit"))
}

fn svg_local(d: &Document, n: NodeId) -> Option<&str> {
    let q = d.qual_name(n)?;
    (q.ns == ns!(svg)).then_some(&*q.local)
}

fn href(d: &Document, n: NodeId) -> Option<String> {
    if let Some(h) = attr(d, n, "href") {
        return Some(h.to_string());
    }
    d.attributes(n)
        .iter()
        .find(|a| a.name.ns == ns!(xlink) && &*a.name.local == "href")
        .map(|a| a.value.to_string())
}

// ----- element kinds -----

const SHAPES: &[&str] = &[
    "rect", "circle", "ellipse", "line", "polyline", "polygon", "path",
];
const TEXT_CONTENT: &[&str] = &["text", "tspan", "textPath"];
const GRAPHICS: &[&str] = &[
    "a",
    "circle",
    "defs",
    "ellipse",
    "foreignObject",
    "g",
    "image",
    "line",
    "path",
    "polygon",
    "polyline",
    "rect",
    "svg",
    "switch",
    "text",
    "textPath",
    "tspan",
    "use",
];
/// Containers whose children have layout objects but add nothing to an
/// ancestor's box.
const HIDDEN: &[&str] = &[
    "defs",
    "symbol",
    "clipPath",
    "mask",
    "marker",
    "pattern",
    "linearGradient",
    "radialGradient",
    "filter",
];
/// Containers whose SVG children are rendered.
const CONTAINERS: &[&str] = &["svg", "g", "a", "switch"];

/// `this` as an SVG element with one of these local names.
fn this_svg_elem(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    kinds: &[&str],
) -> Result<(u32, NodeId), HostError> {
    let (slot, n) = super::this_node(cx, w)?;
    let ok = w.doc_slot(slot, |d| {
        svg_local(d, n).is_some_and(|l| kinds.contains(&l))
    });
    if ok { Ok((slot, n)) } else { Err(illegal()) }
}

// ----- paths -----

type P = [f32; 2];

#[derive(Clone, Copy, Debug)]
enum Seg {
    Move(P),
    Line(P),
    Quad(P, P),
    Cubic(P, P, P),
    /// Control point, end point, weight.
    Conic(P, P, f32),
    Close,
}

#[derive(Default, Debug)]
struct Path {
    segs: Vec<Seg>,
    start: P,
    cur: P,
}

const ROOT2_OVER_2: f32 = std::f32::consts::FRAC_1_SQRT_2;

impl Path {
    fn inject_move(&mut self) {
        if matches!(self.segs.last(), Some(Seg::Close)) {
            self.segs.push(Seg::Move(self.start));
        }
    }

    fn move_to(&mut self, p: P) {
        self.segs.push(Seg::Move(p));
        self.start = p;
        self.cur = p;
    }

    fn line_to(&mut self, p: P) {
        self.inject_move();
        self.segs.push(Seg::Line(p));
        self.cur = p;
    }

    fn cubic_to(&mut self, a: P, b: P, p: P) {
        self.inject_move();
        self.segs.push(Seg::Cubic(a, b, p));
        self.cur = p;
    }

    fn quad_to(&mut self, c: P, p: P) {
        self.inject_move();
        self.segs.push(Seg::Quad(c, p));
        self.cur = p;
    }

    fn conic_to(&mut self, c: P, p: P) {
        self.conic_w(c, p, ROOT2_OVER_2);
    }

    fn conic_w(&mut self, c: P, p: P, w: f32) {
        self.inject_move();
        self.segs.push(Seg::Conic(c, p, w));
        self.cur = p;
    }

    fn close(&mut self) {
        if !self.segs.is_empty() && !matches!(self.segs.last(), Some(Seg::Close)) {
            self.segs.push(Seg::Close);
        }
        self.cur = self.start;
    }

    /// `SkPath::addRect` (clockwise from the top left).
    fn rect(x: f32, y: f32, w: f32, h: f32) -> Path {
        let mut p = Path::default();
        p.move_to([x, y]);
        p.line_to([x + w, y]);
        p.line_to([x + w, y + h]);
        p.line_to([x, y + h]);
        p.close();
        p
    }

    /// `SkPath::addRRect` with start index 0, clockwise.
    fn rrect(x: f32, y: f32, w: f32, h: f32, rx: f32, ry: f32) -> Path {
        let (l, t, r, b) = (x, y, x + w, y + h);
        let mut p = Path::default();
        p.move_to([l + rx, t]);
        p.line_to([r - rx, t]);
        p.conic_to([r, t], [r, t + ry]);
        p.line_to([r, b - ry]);
        p.conic_to([r, b], [r - rx, b]);
        p.line_to([l + rx, b]);
        p.conic_to([l, b], [l, b - ry]);
        p.line_to([l, t + ry]);
        p.conic_to([l, t], [l + rx, t]);
        p.close();
        p
    }

    /// `SkPath::addOval` starting at 3 o'clock, clockwise.
    fn oval(cx: f32, cy: f32, rx: f32, ry: f32) -> Path {
        let (l, t, r, b) = (cx - rx, cy - ry, cx + rx, cy + ry);
        let mut p = Path::default();
        p.move_to([r, cy]);
        p.conic_to([r, b], [cx, b]);
        p.conic_to([l, b], [l, cy]);
        p.conic_to([l, t], [cx, t]);
        p.conic_to([r, t], [r, cy]);
        p.close();
        p
    }

    /// Path data; parsing stops at the first error, keeping what came
    /// before. As `SVGPathBuilder` builds the rendered path (quadratics
    /// stay quadratics, arcs become Skia conics), or `normalized` as
    /// `SVGPathNormalizer` feeds `SVGPathQuery` (cubics only, arcs
    /// decomposed).
    fn parse(d: &str, normalized: bool) -> Path {
        let mut p = Path::default();
        let mut sc = Scan::new(d);
        sc.ws();
        let mut cmd = 0u8;
        // Last control point of a C/S (true) or Q/T (false) segment.
        let mut last_ctrl: Option<(P, bool)> = None;
        let mut first = true;
        loop {
            sc.ws();
            if sc.done() {
                break;
            }
            match sc.peek() {
                Some(c) if c.is_ascii_alphabetic() => {
                    sc.i += 1;
                    cmd = c;
                    sc.ws();
                }
                _ => {
                    if cmd == 0 || matches!(cmd, b'z' | b'Z') {
                        break;
                    }
                    if cmd == b'M' {
                        cmd = b'L';
                    } else if cmd == b'm' {
                        cmd = b'l';
                    }
                }
            }
            if first && !matches!(cmd, b'M' | b'm') {
                break;
            }
            first = false;
            let rel = cmd.is_ascii_lowercase();
            let [ox, oy] = if rel { p.cur } else { [0.0, 0.0] };
            let cur = p.cur;
            let ok = (|| -> Option<Option<(P, bool)>> {
                Some(match cmd.to_ascii_uppercase() {
                    b'M' => {
                        let pt = [sc.num_sep()? + ox, sc.num_sep()? + oy];
                        p.move_to(pt);
                        None
                    }
                    b'L' => {
                        let pt = [sc.num_sep()? + ox, sc.num_sep()? + oy];
                        p.line_to(pt);
                        None
                    }
                    b'H' => {
                        let x = sc.num_sep()? + ox;
                        p.line_to([x, cur[1]]);
                        None
                    }
                    b'V' => {
                        let y = sc.num_sep()? + oy;
                        p.line_to([cur[0], y]);
                        None
                    }
                    b'C' => {
                        let a = [sc.num_sep()? + ox, sc.num_sep()? + oy];
                        let b = [sc.num_sep()? + ox, sc.num_sep()? + oy];
                        let e = [sc.num_sep()? + ox, sc.num_sep()? + oy];
                        p.cubic_to(a, b, e);
                        Some((b, true))
                    }
                    b'S' => {
                        let b = [sc.num_sep()? + ox, sc.num_sep()? + oy];
                        let e = [sc.num_sep()? + ox, sc.num_sep()? + oy];
                        let a = match last_ctrl {
                            Some((c, true)) => [2.0 * cur[0] - c[0], 2.0 * cur[1] - c[1]],
                            _ => cur,
                        };
                        p.cubic_to(a, b, e);
                        Some((b, true))
                    }
                    b'Q' => {
                        let q = [sc.num_sep()? + ox, sc.num_sep()? + oy];
                        let e = [sc.num_sep()? + ox, sc.num_sep()? + oy];
                        if normalized {
                            p.cubic_to(blend(cur, q), blend(e, q), e);
                        } else {
                            p.quad_to(q, e);
                        }
                        Some((q, false))
                    }
                    b'T' => {
                        let e = [sc.num_sep()? + ox, sc.num_sep()? + oy];
                        let q = match last_ctrl {
                            Some((c, false)) => [2.0 * cur[0] - c[0], 2.0 * cur[1] - c[1]],
                            _ => cur,
                        };
                        if normalized {
                            p.cubic_to(blend(cur, q), blend(e, q), e);
                        } else {
                            p.quad_to(q, e);
                        }
                        Some((q, false))
                    }
                    b'A' => {
                        let rx = sc.num_sep()?;
                        let ry = sc.num_sep()?;
                        let angle = sc.num_sep()?;
                        let large = sc.flag()?;
                        let sweep = sc.flag()?;
                        let e = [sc.num_sep()? + ox, sc.num_sep()? + oy];
                        if normalized {
                            if !arc_to_cubics(&mut p, cur, e, rx, ry, angle, large, sweep) {
                                p.line_to(e);
                            }
                        } else {
                            sk_arc_to(&mut p, cur, e, rx, ry, angle, large, sweep);
                        }
                        None
                    }
                    b'Z' => {
                        p.close();
                        None
                    }
                    _ => return None,
                })
            })();
            match ok {
                Some(c) => last_ctrl = c,
                None => break,
            }
        }
        p
    }

    fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }

    /// `SkPath::computeTightBounds`.
    fn tight_bounds(&self) -> Rf {
        let mut pts: Vec<P> = Vec::new();
        let mut cur = [0.0, 0.0];
        let mut start = [0.0, 0.0];
        for s in &self.segs {
            match *s {
                Seg::Move(p) => {
                    pts.push(p);
                    cur = p;
                    start = p;
                }
                Seg::Line(p) => {
                    pts.push(p);
                    cur = p;
                }
                Seg::Quad(c, e) => {
                    let q = [cur, c, e];
                    for axis in 0..2 {
                        let (a, b, c) = (cur[axis], c[axis], e[axis]);
                        if let Some(t) = unit_divide(a - b, a - b - b + c) {
                            pts.push(eval_quad(&q, t));
                        }
                    }
                    pts.push(e);
                    cur = e;
                }
                Seg::Cubic(a, b, e) => {
                    let c = [cur, a, b, e];
                    let mut ts = cubic_extrema(cur[0], a[0], b[0], e[0]);
                    ts.extend(cubic_extrema(cur[1], a[1], b[1], e[1]));
                    pts.extend(ts.into_iter().map(|t| eval_cubic(&c, t)));
                    pts.push(e);
                    cur = e;
                }
                Seg::Conic(c, e, wt) => {
                    let k = [cur, c, e];
                    for axis in 0..2 {
                        if let Some(t) = conic_extremum(cur[axis], c[axis], e[axis], wt) {
                            pts.push(eval_conic(&k, wt, t));
                        }
                    }
                    pts.push(e);
                    cur = e;
                }
                Seg::Close => cur = start,
            }
        }
        let Some(&[x, y]) = pts.first() else {
            return Rf::default();
        };
        let (mut l, mut t, mut r, mut b) = (x, y, x, y);
        for &[x, y] in &pts[1..] {
            l = l.min(x);
            t = t.min(y);
            r = r.max(x);
            b = b.max(y);
        }
        Rf::from_ltrb(l, t, r, b)
    }
}

/// `BlendPoints` in `SVGPathNormalizer` (quadratic to cubic control).
fn blend(p1: P, p2: P) -> P {
    const THIRD: f32 = 1.0 / 3.0;
    [(p1[0] + 2.0 * p2[0]) * THIRD, (p1[1] + 2.0 * p2[1]) * THIRD]
}

/// `SVGPathNormalizer::DecomposeArcToCubic`; false when the arc is a
/// line.
#[allow(clippy::too_many_arguments)]
fn arc_to_cubics(
    p: &mut Path,
    cur: P,
    target: P,
    rx: f32,
    ry: f32,
    angle: f32,
    large: bool,
    sweep: bool,
) -> bool {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx == 0.0 || ry == 0.0 || cur == target {
        return false;
    }
    let angle = f64::from(angle);
    let mid = [(cur[0] - target[0]) * 0.5, (cur[1] - target[1]) * 0.5];
    let (tx, ty) = Tf::rotate(-angle).map(f64::from(mid[0]), f64::from(mid[1]));
    let (tx, ty) = (tx as f32, ty as f32);
    let radii_scale = (tx * tx) / (rx * rx) + (ty * ty) / (ry * ry);
    if radii_scale > 1.0 {
        rx *= radii_scale.sqrt();
        ry *= radii_scale.sqrt();
    }
    let to_unit = Tf::scale(f64::from(1.0 / rx), f64::from(1.0 / ry)).mul(&Tf::rotate(-angle));
    let mp = |t: &Tf, q: P| {
        let (x, y) = t.map(f64::from(q[0]), f64::from(q[1]));
        [x as f32, y as f32]
    };
    let p1 = mp(&to_unit, cur);
    let p2 = mp(&to_unit, target);
    let delta = [p2[0] - p1[0], p2[1] - p1[1]];
    let dd = delta[0] * delta[0] + delta[1] * delta[1];
    let mut sf = (1.0 / dd - 0.25).max(0.0).sqrt();
    if sweep == large {
        sf = -sf;
    }
    let delta = [delta[0] * sf, delta[1] * sf];
    let center = [
        (p1[0] + p2[0]) * 0.5 - delta[1],
        (p1[1] + p2[1]) * 0.5 + delta[0],
    ];
    let theta1 = (p1[1] - center[1]).atan2(p1[0] - center[0]);
    let theta2 = (p2[1] - center[1]).atan2(p2[0] - center[0]);
    let two_pi = std::f32::consts::PI * 2.0;
    let mut arc = theta2 - theta1;
    if arc < 0.0 && sweep {
        arc += two_pi;
    } else if arc > 0.0 && !sweep {
        arc -= two_pi;
    }
    let from_unit = Tf::rotate(angle).mul(&Tf::scale(f64::from(rx), f64::from(ry)));
    let segments = (arc / (std::f32::consts::FRAC_PI_2 + 0.001)).abs().ceil() as i32;
    for i in 0..segments {
        let st = theta1 + i as f32 * arc / segments as f32;
        let et = theta1 + (i + 1) as f32 * arc / segments as f32;
        let t = (8.0 / 6.0) * (0.25 * (et - st)).tan();
        if !t.is_finite() {
            return false;
        }
        let (ss, cs) = (st.sin(), st.cos());
        let (se, ce) = (et.sin(), et.cos());
        let a = [cs - t * ss + center[0], ss + t * cs + center[1]];
        let e = [ce + center[0], se + center[1]];
        let b = [e[0] + t * se, e[1] - t * ce];
        p.cubic_to(mp(&from_unit, a), mp(&from_unit, b), mp(&from_unit, e));
    }
    true
}

/// An `f32` affine matrix as `SkMatrix` (`[sx, kx, tx, ky, sy, ty]`).
#[derive(Clone, Copy)]
struct SkM([f32; 6]);

impl SkM {
    fn rotate(deg: f32) -> SkM {
        let rad = deg * (std::f32::consts::PI / 180.0);
        let snap = |v: f32| if v.abs() <= 1.0 / 4096.0 { 0.0 } else { v };
        let (s, c) = (snap(rad.sin()), snap(rad.cos()));
        SkM([c, -s, 0.0, s, c, 0.0])
    }

    fn scale(x: f32, y: f32) -> SkM {
        SkM([x, 0.0, 0.0, 0.0, y, 0.0])
    }

    /// `self · o` (`o` applied first), as `SkMatrix::setConcat`.
    fn concat(&self, o: &SkM) -> SkM {
        let [a0, a1, a2, a3, a4, a5] = self.0.map(f64::from);
        let [b0, b1, b2, b3, b4, b5] = o.0.map(f64::from);
        SkM([
            (a0 * b0 + a1 * b3) as f32,
            (a0 * b1 + a1 * b4) as f32,
            (a0 * b2 + a1 * b5 + a2) as f32,
            (a3 * b0 + a4 * b3) as f32,
            (a3 * b1 + a4 * b4) as f32,
            (a3 * b2 + a4 * b5 + a5) as f32,
        ])
    }

    fn map(&self, p: P) -> P {
        let [sx, kx, tx, ky, sy, ty] = self.0;
        [sx * p[0] + kx * p[1] + tx, ky * p[0] + sy * p[1] + ty]
    }
}

/// `SkPath::arcTo` for SVG arcs (Blink's `SVGPathBuilder`): up to four
/// conics, the last ending exactly at `target`.
#[allow(clippy::too_many_arguments)]
fn sk_arc_to(
    p: &mut Path,
    cur: P,
    target: P,
    rx: f32,
    ry: f32,
    angle: f32,
    large: bool,
    sweep: bool,
) {
    use std::f32::consts::PI;
    if rx == 0.0 || ry == 0.0 || cur == target {
        p.line_to(target);
        return;
    }
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    let mid = [(cur[0] - target[0]) * 0.5, (cur[1] - target[1]) * 0.5];
    let tm = SkM::rotate(-angle).map(mid);
    let radii_scale = (tm[0] * tm[0]) / (rx * rx) + (tm[1] * tm[1]) / (ry * ry);
    if radii_scale > 1.0 {
        let s = radii_scale.sqrt();
        rx *= s;
        ry *= s;
    }
    let to_unit = SkM::scale(1.0 / rx, 1.0 / ry).concat(&SkM::rotate(-angle));
    let mut u0 = to_unit.map(cur);
    let mut u1 = to_unit.map(target);
    let mut delta = [u1[0] - u0[0], u1[1] - u0[1]];
    let d = delta[0] * delta[0] + delta[1] * delta[1];
    let mut sf = (1.0 / d - 0.25).max(0.0).sqrt();
    if !sweep != large {
        sf = -sf;
    }
    delta = [delta[0] * sf, delta[1] * sf];
    let mut center = [(u0[0] + u1[0]) * 0.5, (u0[1] + u1[1]) * 0.5];
    center = [center[0] - delta[1], center[1] + delta[0]];
    u0 = [u0[0] - center[0], u0[1] - center[1]];
    u1 = [u1[0] - center[0], u1[1] - center[1]];
    let theta1 = u0[1].atan2(u0[0]);
    let theta2 = u1[1].atan2(u1[0]);
    let mut arc = theta2 - theta1;
    if arc < 0.0 && sweep {
        arc += PI * 2.0;
    } else if arc > 0.0 && !sweep {
        arc -= PI * 2.0;
    }
    if arc.abs() < PI / (1000.0 * 1000.0) {
        p.line_to(target);
        return;
    }
    let from_unit = SkM::rotate(angle).concat(&SkM::scale(rx, ry));
    let segments = (arc / (2.0 * PI / 3.0)).abs().ceil() as i32;
    let width = arc / segments as f32;
    let t = (0.5 * width).tan();
    if !t.is_finite() {
        return;
    }
    let w = (0.5 + width.cos() * 0.5).sqrt();
    let int = |v: f32| v == v.floor();
    let expect_int = (PI / 2.0 - width.abs()).abs() <= 1.0 / 4096.0
        && int(rx)
        && int(ry)
        && int(target[0])
        && int(target[1]);
    let snap = |v: f32| if v.abs() <= 1.0 / 4096.0 { 0.0 } else { v };
    let mut start = theta1;
    for _ in 0..segments {
        let end = start + width;
        let (se, ce) = (snap(end.sin()), snap(end.cos()));
        let e = [ce + center[0], se + center[1]];
        let c = [e[0] + t * se, e[1] - t * ce];
        let (mut mc, mut me) = (from_unit.map(c), from_unit.map(e));
        if expect_int {
            mc = mc.map(|v| (v + 0.5).floor());
            me = me.map(|v| (v + 0.5).floor());
        }
        p.conic_w(mc, me, w);
        start = end;
    }
    if let Some(Seg::Conic(_, e, _)) = p.segs.last_mut() {
        *e = target;
    }
    p.cur = target;
}

/// `valid_unit_divide`: `n / d` when it is in (0, 1).
fn unit_divide(n: f32, d: f32) -> Option<f32> {
    let (n, d) = if n < 0.0 { (-n, -d) } else { (n, d) };
    if d == 0.0 || n == 0.0 || n >= d {
        return None;
    }
    let r = n / d;
    (!r.is_nan() && r != 0.0).then_some(r)
}

/// `SkFindUnitQuadRoots`.
fn unit_quad_roots(a: f32, b: f32, c: f32) -> Vec<f32> {
    let div = unit_divide;
    if a == 0.0 {
        return div(-c, b).into_iter().collect();
    }
    let dr = f64::from(b) * f64::from(b) - 4.0 * f64::from(a) * f64::from(c);
    if dr < 0.0 {
        return Vec::new();
    }
    let r = dr.sqrt() as f32;
    if !r.is_finite() {
        return Vec::new();
    }
    let q = if b < 0.0 {
        -(b - r) / 2.0
    } else {
        -(b + r) / 2.0
    };
    let mut out: Vec<f32> = div(q, a).into_iter().chain(div(c, q)).collect();
    if out.len() == 2 {
        if out[0] > out[1] {
            out.swap(0, 1);
        } else if out[0] == out[1] {
            out.pop();
        }
    }
    out
}

/// `SkFindCubicExtrema`.
fn cubic_extrema(a: f32, b: f32, c: f32, d: f32) -> Vec<f32> {
    let qa = d - a + 3.0 * (b - c);
    let qb = 2.0 * (a - b - b + c);
    let qc = b - a;
    unit_quad_roots(qa, qb, qc)
}

/// `conic_find_extrema` (one coordinate).
fn conic_extremum(p0: f32, p1: f32, p2: f32, w: f32) -> Option<f32> {
    let p20 = p2 - p0;
    let p10 = p1 - p0;
    let wp10 = w * p10;
    let r = unit_quad_roots(w * p20 - p20, p20 - 2.0 * wp10, wp10);
    (r.len() == 1).then(|| r[0])
}

/// `SkCubicCoeff::eval`.
fn eval_cubic(p: &[P; 4], t: f32) -> P {
    let mut out = [0.0; 2];
    for (i, o) in out.iter_mut().enumerate() {
        let (p0, p1, p2, p3) = (p[0][i], p[1][i], p[2][i], p[3][i]);
        let a = p3 + 3.0 * (p1 - p2) - p0;
        let b = 3.0 * (p2 - 2.0 * p1 + p0);
        let c = 3.0 * (p1 - p0);
        *o = ((a * t + b) * t + c) * t + p0;
    }
    out
}

/// `SkQuadCoeff::eval`.
fn eval_quad(p: &[P; 3], t: f32) -> P {
    let mut out = [0.0; 2];
    for (i, o) in out.iter_mut().enumerate() {
        let (p0, p1, p2) = (p[0][i], p[1][i], p[2][i]);
        let a = p2 - 2.0 * p1 + p0;
        let b = 2.0 * (p1 - p0);
        *o = (a * t + b) * t + p0;
    }
    out
}

/// `SkConic::evalAt`.
fn eval_conic(p: &[P; 3], w: f32, t: f32) -> P {
    let da = 2.0 - 2.0 * w;
    let db = 2.0 * (w - 1.0);
    let den = (da * t + db) * t + 1.0;
    let mut out = [0.0; 2];
    for (i, o) in out.iter_mut().enumerate() {
        let (p0, p1, p2) = (p[0][i], p[1][i] * w, p[2][i]);
        let a = p2 - 2.0 * p1 + p0;
        let b = 2.0 * (p1 - p0);
        *o = ((a * t + b) * t + p0) / den;
    }
    out
}

fn dist(a: P, b: P) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    (dx * dx + dy * dy).sqrt()
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

// ----- SkContourMeasure -----

const MAX_T: i32 = 0x3FFF_FFFF;
const TOLERANCE: f32 = 0.5;

fn t_scalar(t: i32) -> f32 {
    t as f32 * (1.0 / MAX_T as f32)
}

fn big_enough(span: i32) -> bool {
    span >> 10 != 0
}

#[derive(Clone, Copy, Debug)]
enum Piece {
    Line(P, P),
    Cubic([P; 4]),
    Conic([P; 3], f32),
}

#[derive(Clone, Copy, Debug)]
struct MSeg {
    dist: f32,
    piece: usize,
    t: i32,
}

#[derive(Debug, Default)]
struct Contour {
    pieces: Vec<Piece>,
    segs: Vec<MSeg>,
    length: f32,
}

impl Contour {
    fn add(&mut self, piece: usize, d: f32, t: i32) {
        let prev = self.length;
        let next = prev + d;
        if next > prev {
            self.length = next;
            self.segs.push(MSeg {
                dist: next,
                piece,
                t,
            });
        }
    }

    fn cubic(&mut self, p: [P; 4], piece: usize, mint: i32, maxt: i32) {
        let third = 1.0f32 / 3.0;
        let two_thirds = 2.0f32 / 3.0;
        let far = |q: P, x: f32, y: f32| (x - q[0]).abs().max((y - q[1]).abs()) > TOLERANCE;
        let curvy = far(
            p[1],
            lerp(p[0][0], p[3][0], third),
            lerp(p[0][1], p[3][1], third),
        ) || far(
            p[2],
            lerp(p[0][0], p[3][0], two_thirds),
            lerp(p[0][1], p[3][1], two_thirds),
        );
        if big_enough(maxt - mint) && curvy {
            let half = (mint + maxt) >> 1;
            let m = |a: P, b: P| [lerp(a[0], b[0], 0.5), lerp(a[1], b[1], 0.5)];
            let ab = m(p[0], p[1]);
            let bc = m(p[1], p[2]);
            let cd = m(p[2], p[3]);
            let abc = m(ab, bc);
            let bcd = m(bc, cd);
            let abcd = m(abc, bcd);
            self.cubic([p[0], ab, abc, abcd], piece, mint, half);
            self.cubic([abcd, bcd, cd, p[3]], piece, half, maxt);
        } else {
            self.add(piece, dist(p[0], p[3]), maxt);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn conic(&mut self, k: &[P; 3], w: f32, piece: usize, mint: i32, minp: P, maxt: i32, maxp: P) {
        let half = (mint + maxt) >> 1;
        let hp = eval_conic(k, w, t_scalar(half));
        if !(hp[0].is_finite() && hp[1].is_finite()) {
            return;
        }
        let mid = [(minp[0] + maxp[0]) * 0.5, (minp[1] + maxp[1]) * 0.5];
        let curvy = (hp[0] - mid[0]).abs().max((hp[1] - mid[1]).abs()) > TOLERANCE;
        if big_enough(maxt - mint) && curvy {
            self.conic(k, w, piece, mint, minp, half, hp);
            self.conic(k, w, piece, half, hp, maxt, maxp);
        } else {
            self.add(piece, dist(minp, maxp), maxt);
        }
    }

    /// `SkContourMeasure::getPosTan` (position only).
    fn pos(&self, distance: f32) -> P {
        let d = distance.clamp(0.0, self.length);
        let idx = self
            .segs
            .partition_point(|s| s.dist < d)
            .min(self.segs.len() - 1);
        let seg = self.segs[idx];
        let (mut start_t, mut start_d) = (0.0, 0.0);
        if idx > 0 {
            let prev = self.segs[idx - 1];
            start_d = prev.dist;
            if prev.piece == seg.piece {
                start_t = t_scalar(prev.t);
            }
        }
        let t = start_t + (t_scalar(seg.t) - start_t) * (d - start_d) / (seg.dist - start_d);
        match self.pieces[seg.piece] {
            Piece::Line(a, b) => [lerp(a[0], b[0], t), lerp(a[1], b[1], t)],
            Piece::Cubic(c) => eval_cubic(&c, t),
            Piece::Conic(k, w) => eval_conic(&k, w, t),
        }
    }
}

/// Contours with a positive length (`SkContourMeasureIter`).
fn contours(path: &Path) -> Vec<Contour> {
    let mut out = Vec::new();
    let mut c = Contour::default();
    let mut start = [0.0, 0.0];
    let mut cur = [0.0, 0.0];
    let mut closed = false;
    let finish = |c: &mut Contour, closed: bool, cur: P, start: P, out: &mut Vec<Contour>| {
        if closed {
            let i = c.pieces.len();
            c.pieces.push(Piece::Line(cur, start));
            c.add(i, dist(cur, start), MAX_T);
        }
        if c.length > 0.0 && c.length.is_finite() {
            out.push(std::mem::take(c));
        } else {
            *c = Contour::default();
        }
    };
    for s in &path.segs {
        match *s {
            Seg::Move(p) => {
                finish(&mut c, closed, cur, start, &mut out);
                closed = false;
                start = p;
                cur = p;
            }
            Seg::Line(p) => {
                let i = c.pieces.len();
                c.pieces.push(Piece::Line(cur, p));
                c.add(i, dist(cur, p), MAX_T);
                cur = p;
            }
            Seg::Cubic(a, b, e) => {
                let i = c.pieces.len();
                let pts = [cur, a, b, e];
                c.pieces.push(Piece::Cubic(pts));
                c.cubic(pts, i, 0, MAX_T);
                cur = e;
            }
            Seg::Quad(k, e) => {
                let i = c.pieces.len();
                let pts = [cur, k, e];
                c.pieces.push(Piece::Conic(pts, 1.0));
                c.conic(&pts, 1.0, i, 0, cur, MAX_T, e);
                cur = e;
            }
            Seg::Conic(k, e, w) => {
                let i = c.pieces.len();
                let pts = [cur, k, e];
                c.pieces.push(Piece::Conic(pts, w));
                c.conic(&pts, w, i, 0, cur, MAX_T, e);
                cur = e;
            }
            Seg::Close => closed = true,
        }
    }
    finish(&mut c, closed, cur, start, &mut out);
    out
}

/// `Path::length`.
fn path_length(path: &Path) -> f32 {
    contours(path).iter().map(|c| c.length).sum()
}

/// `Path::PointAtLength` with the length clamped to the path.
fn point_at_length(path: &Path, len: f32) -> P {
    let cs = contours(path);
    let total: f32 = cs.iter().map(|c| c.length).sum();
    let mut l = len.clamp(0.0, total);
    for c in &cs {
        if l <= c.length {
            return c.pos(l);
        }
        l -= c.length;
    }
    match path.segs.first() {
        Some(Seg::Move(p)) => *p,
        _ => [0.0, 0.0],
    }
}

// ----- PathTraversalState (path elements) -----

/// `gfx::Vector2dF::Length` (double sum, float result).
fn dist_line(a: P, b: P) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    (f64::from(dx) * f64::from(dx) + f64::from(dy) * f64::from(dy)).sqrt() as f32
}

fn mid(a: P, b: P) -> P {
    [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]
}

/// Blink's `SVGPathQuery` over normalized segments (`PathTraversalState`):
/// curves split until their control polygon is within 1e-5 of the chord.
/// With `want`, stops at that length and returns the point there.
fn traverse(path: &Path, want: Option<f32>) -> (f32, P) {
    const TOL: f32 = 0.00001;
    let mut total = 0.0f32;
    let (mut start, mut cur, mut prev) = ([0.0f32; 2], [0.0f32; 2], [0.0f32; 2]);
    for s in &path.segs {
        let d = match *s {
            Seg::Move(p) => {
                start = p;
                cur = p;
                0.0
            }
            Seg::Line(p) => {
                let d = dist_line(cur, p);
                cur = p;
                d
            }
            Seg::Close => {
                let d = dist_line(cur, start);
                cur = start;
                d
            }
            Seg::Cubic(c1, c2, e) => {
                let mut curve = [cur, c1, c2, e];
                let mut stack: Vec<[P; 4]> = Vec::new();
                let mut len = 0.0f32;
                loop {
                    let approx = dist_line(curve[0], curve[1])
                        + dist_line(curve[1], curve[2])
                        + dist_line(curve[2], curve[3]);
                    if approx - dist_line(curve[0], curve[3]) > TOL && stack.len() < 20 {
                        let s12 = mid(curve[1], curve[2]);
                        let l1 = mid(curve[0], curve[1]);
                        let l2 = mid(l1, s12);
                        let r2 = mid(curve[2], curve[3]);
                        let r1 = mid(r2, s12);
                        let m = mid(l2, r1);
                        stack.push([m, r1, r2, curve[3]]);
                        curve = [curve[0], l1, l2, m];
                        continue;
                    }
                    len += approx;
                    if let Some(w) = want {
                        prev = curve[0];
                        cur = curve[3];
                        if total + len > w {
                            break;
                        }
                    }
                    match stack.pop() {
                        Some(c) => curve = c,
                        None => break,
                    }
                }
                if want.is_none() {
                    cur = e;
                }
                len
            }
            Seg::Quad(..) | Seg::Conic(..) => 0.0,
        };
        total += d;
        if let Some(w) = want {
            if total >= w {
                let slope = (cur[1] - prev[1]).atan2(cur[0] - prev[0]);
                let off = w - total;
                return (
                    total,
                    [cur[0] + off * slope.cos(), cur[1] + off * slope.sin()],
                );
            }
            prev = cur;
        }
    }
    (total, cur)
}

/// `SVGPathElement::getPointAtLength`.
fn path_point_at_length(path: &Path, len: f32) -> P {
    let len = if len < 0.0 {
        0.0
    } else {
        len.min(traverse(path, None).0)
    };
    traverse(path, Some(len)).1
}

// ----- text layout -----

#[derive(Clone, Copy, Debug)]
struct Glyph {
    /// First UTF-16 index in the text element.
    unit: u32,
    units: u32,
    owner: NodeId,
    x: f32,
    y: f32,
    adv: f32,
    asc: f32,
    desc: f32,
    /// Ink left of the origin and right extent (px, from the glyph pack).
    ink_l: f32,
    ink_r: f32,
    rot: f32,
}

impl Glyph {
    fn rotation(&self) -> Tf {
        Tf::translate(f64::from(self.x), f64::from(self.y))
            .mul(&Tf::rotate(f64::from(self.rot)))
            .mul(&Tf::translate(-f64::from(self.x), -f64::from(self.y)))
    }

    fn extent(&self) -> Rf {
        let r = Rf::new(self.x, self.y - self.asc, self.adv, self.asc + self.desc);
        if self.rot == 0.0 {
            r
        } else {
            self.rotation().map_rect(r)
        }
    }

    fn ink_box(&self) -> Rf {
        let l = self.x + (-self.ink_l).min(0.0);
        let r = self.x + self.adv.max(self.ink_r);
        let b = Rf::from_ltrb(l, self.y - self.asc, r, self.y + self.desc);
        if self.rot == 0.0 {
            b
        } else {
            self.rotation().map_rect(b)
        }
    }
}

// ----- the geometry context -----

struct Geo<'a> {
    l: &'a mut LayoutEngine,
    d: &'a Document,
    fonts: &'a FontSet,
    dpr: f32,
}

#[derive(Clone, Copy)]
enum Axis {
    X,
    Y,
    Other,
}

impl Geo<'_> {
    fn style(&mut self, n: NodeId) -> Option<std::rc::Rc<lb_css::ComputedStyle>> {
        self.l.computed_style(self.d, n)
    }

    fn display_none(&mut self, n: NodeId) -> bool {
        self.style(n).is_none_or(|s| s.is_display_none())
    }

    fn font_size(&mut self, n: NodeId) -> f32 {
        self.style(n).map_or(16.0, |s| s.font_size)
    }

    /// Nearest `svg` ancestor (not `n`).
    fn viewport_elem(&self, n: NodeId) -> Option<NodeId> {
        self.d
            .ancestors(n)
            .take_while(|&p| svg_local(self.d, p).is_some())
            .find(|&p| svg_local(self.d, p) == Some("svg"))
    }

    fn is_outer_svg(&self, n: NodeId) -> bool {
        svg_local(self.d, n) == Some("svg")
            && self
                .d
                .parent(n)
                .is_none_or(|p| svg_local(self.d, p).is_none())
    }

    fn view_box(&self, n: NodeId) -> Option<[f32; 4]> {
        let (v, ok) = number_list(attr(self.d, n, "viewBox")?);
        (ok && v.len() == 4 && v[2] >= 0.0 && v[3] >= 0.0).then(|| [v[0], v[1], v[2], v[3]])
    }

    /// Size of an `svg` element's viewport (before its `viewBox`).
    fn svg_size(&mut self, n: NodeId) -> (f32, f32) {
        if self.is_outer_svg(n) {
            let mut size = [300.0f32, 150.0];
            let mut need_box = false;
            for (i, name) in ["width", "height"].into_iter().enumerate() {
                match prop(self.d, n, name).as_deref().and_then(parse_length) {
                    Some((v, u)) if u != "%" => {
                        size[i] = abs_length(v, u, 16.0).unwrap_or(size[i]);
                    }
                    Some(_) => need_box = true,
                    None => {}
                }
            }
            if need_box {
                let r = self.l.bounding_client_rect(self.d, n);
                if r.width > 0.0 && r.height > 0.0 {
                    return (r.width, r.height);
                }
            }
            (size[0], size[1])
        } else {
            let w = self
                .length(n, "width", Axis::X)
                .unwrap_or_else(|| self.percent_of(n, 100.0, Axis::X));
            let h = self
                .length(n, "height", Axis::Y)
                .unwrap_or_else(|| self.percent_of(n, 100.0, Axis::Y));
            (w, h)
        }
    }

    /// The coordinate space percentages of `n`'s lengths resolve against.
    fn viewport_of(&mut self, n: NodeId) -> (f32, f32) {
        match self.viewport_elem(n) {
            Some(v) => match self.view_box(v) {
                Some(vb) => (vb[2], vb[3]),
                None => self.svg_size(v),
            },
            None => (0.0, 0.0),
        }
    }

    fn percent_of(&mut self, n: NodeId, pct: f32, axis: Axis) -> f32 {
        let (w, h) = self.viewport_of(n);
        let base = match axis {
            Axis::X => w,
            Axis::Y => h,
            Axis::Other => ((w * w + h * h) / 2.0).sqrt(),
        };
        pct / 100.0 * base
    }

    /// A length attribute (or inline geometry property); `None` when
    /// missing or invalid.
    fn length(&mut self, n: NodeId, name: &str, axis: Axis) -> Option<f32> {
        let v = prop(self.d, n, name)?;
        self.resolve(n, &v, axis)
    }

    fn resolve(&mut self, n: NodeId, v: &str, axis: Axis) -> Option<f32> {
        let (num, unit) = parse_length(v)?;
        if unit == "%" {
            return Some(self.percent_of(n, num, axis));
        }
        let em = self.font_size(n);
        abs_length(num, unit, em)
    }

    fn len_or0(&mut self, n: NodeId, name: &str, axis: Axis) -> f32 {
        self.length(n, name, axis).unwrap_or(0.0)
    }

    /// The `transform` of an element: CSS `transform` when set, else the
    /// attribute.
    fn transform(&mut self, n: NodeId) -> Tf {
        if let Some(t) = self.style(n).and_then(|s| s.transform.clone())
            && &*t.css != "none"
            && let Some(m) = t.matrix
        {
            return Tf(m.map(f64::from));
        }
        attr(self.d, n, "transform").map_or(ID, parse_transform_list)
    }

    /// `viewBox` to viewport (`SVGPreserveAspectRatio::ComputeTransform`).
    fn view_box_transform(&self, n: NodeId, vw: f32, vh: f32) -> Tf {
        let Some([lx, ly, lw, lh]) = self.view_box(n) else {
            return ID;
        };
        if lw == 0.0 || lh == 0.0 || vw <= 0.0 || vh <= 0.0 {
            return ID;
        }
        let (lx, ly, lw, lh) = (f64::from(lx), f64::from(ly), f64::from(lw), f64::from(lh));
        let (pw, ph) = (f64::from(vw), f64::from(vh));
        let par = attr(self.d, n, "preserveAspectRatio").unwrap_or("xMidYMid meet");
        let mut parts = par.split_ascii_whitespace();
        let align = parts.next().unwrap_or("xMidYMid");
        let slice = parts.next() == Some("slice");
        if align == "none" {
            return Tf::scale(pw / lw, ph / lh).mul(&Tf::translate(-lx, -ly));
        }
        let (ax, ay) = match align {
            "xMinYMin" => (0, 0),
            "xMidYMin" => (1, 0),
            "xMaxYMin" => (2, 0),
            "xMinYMid" => (0, 1),
            "xMinYMax" => (0, 2),
            "xMidYMax" => (1, 2),
            "xMaxYMid" => (2, 1),
            "xMaxYMax" => (2, 2),
            _ => (1, 1),
        };
        let lr = lw / lh;
        let pr = pw / ph;
        if (lr < pr && !slice) || (lr >= pr && slice) {
            let s = ph / lh;
            let extra = lw - pw * lh / ph;
            let tx = -lx - extra * f64::from(ax) / 2.0;
            Tf::scale(s, s).mul(&Tf::translate(tx, -ly))
        } else {
            let s = pw / lw;
            let extra = lh - ph * lw / pw;
            let ty = -ly - extra * f64::from(ay) / 2.0;
            Tf::scale(s, s).mul(&Tf::translate(-lx, ty))
        }
    }

    /// Transform from `n`'s user space to its parent's (for boxes and the
    /// CTM).
    fn local(&mut self, n: NodeId) -> Tf {
        match svg_local(self.d, n) {
            Some("svg") => {
                let (w, h) = self.svg_size(n);
                let vb = self.view_box_transform(n, w, h);
                if self.is_outer_svg(n) {
                    vb
                } else {
                    let x = self.len_or0(n, "x", Axis::X);
                    let y = self.len_or0(n, "y", Axis::Y);
                    Tf::translate(f64::from(x), f64::from(y)).mul(&vb)
                }
            }
            _ => self.transform(n),
        }
    }

    // ----- rendering -----

    /// Whether `n` has a layout object in Blink: connected, inside an
    /// outermost `svg` through containers that render their children,
    /// not `display: none` (a `g` with it becomes a hidden container).
    fn rendered(&mut self, n: NodeId) -> bool {
        if !self.d.is_connected(n) {
            return false;
        }
        let Some(local) = svg_local(self.d, n) else {
            return false;
        };
        if self.display_none(n) && local != "g" {
            return false;
        }
        let mut child = n;
        loop {
            let Some(p) = self.d.parent(child) else {
                return false;
            };
            let cl = svg_local(self.d, child).unwrap_or("");
            let Some(pl) = svg_local(self.d, p) else {
                // Left the SVG subtree: `child` must be an outermost svg
                // in a rendered HTML tree.
                if cl != "svg" {
                    return false;
                }
                let up: Vec<NodeId> = self
                    .d
                    .ancestors(child)
                    .filter(|&a| self.d.node_type(a) == NodeType::Element)
                    .collect();
                return !up.into_iter().any(|a| self.display_none(a));
            };
            let in_text = TEXT_CONTENT.contains(&pl) || (pl == "a" && self.inside_text(p));
            let allowed = if in_text {
                matches!(cl, "tspan" | "textPath" | "a")
            } else if CONTAINERS.contains(&pl) || HIDDEN.contains(&pl) {
                !matches!(cl, "tspan" | "textPath")
            } else {
                false
            };
            if !allowed || (pl != "g" && self.display_none(p)) {
                return false;
            }
            child = p;
        }
    }

    /// Whether `n` has a text content ancestor in its SVG chain.
    fn inside_text(&self, n: NodeId) -> bool {
        self.d
            .ancestors(n)
            .take_while(|&a| svg_local(self.d, a).is_some())
            .any(|a| svg_local(self.d, a).is_some_and(|l| TEXT_CONTENT.contains(&l)))
    }

    /// Whether a child takes part in its container's box: rendered kind,
    /// not hidden.
    fn child_counts(&mut self, c: NodeId) -> bool {
        let Some(local) = svg_local(self.d, c) else {
            return false;
        };
        if !GRAPHICS.contains(&local) || matches!(local, "defs" | "tspan" | "textPath") {
            return false;
        }
        !self.display_none(c)
    }

    /// Object bounding box and whether it is valid for a container's
    /// union (`HasValidBoundingBoxForContainer`).
    fn object_box(&mut self, n: NodeId, depth: u32) -> (Rf, bool) {
        if depth > 32 {
            return (Rf::default(), false);
        }
        let Some(local) = svg_local(self.d, n).map(str::to_string) else {
            return (Rf::default(), false);
        };
        match local.as_str() {
            "rect" => {
                let x = self.len_or0(n, "x", Axis::X);
                let y = self.len_or0(n, "y", Axis::Y);
                let w = self.len_or0(n, "width", Axis::X).max(0.0);
                let h = self.len_or0(n, "height", Axis::Y).max(0.0);
                (Rf::new(x, y, w, h), w > 0.0 && h > 0.0)
            }
            "circle" => {
                let cx = self.len_or0(n, "cx", Axis::X);
                let cy = self.len_or0(n, "cy", Axis::Y);
                let r = self.len_or0(n, "r", Axis::Other).max(0.0);
                (Rf::new(cx - r, cy - r, 2.0 * r, 2.0 * r), r > 0.0)
            }
            "ellipse" => {
                let cx = self.len_or0(n, "cx", Axis::X);
                let cy = self.len_or0(n, "cy", Axis::Y);
                let (rx, ry) = self.ellipse_radii(n);
                let (rx, ry) = (rx.max(0.0), ry.max(0.0));
                (
                    Rf::new(cx - rx, cy - ry, 2.0 * rx, 2.0 * ry),
                    rx > 0.0 && ry > 0.0,
                )
            }
            "line" | "polyline" | "polygon" | "path" => {
                let p = self.shape_path(n, false);
                (p.tight_bounds(), !p.is_empty())
            }
            "image" | "foreignObject" => {
                let x = self.len_or0(n, "x", Axis::X);
                let y = self.len_or0(n, "y", Axis::Y);
                let w = self.len_or0(n, "width", Axis::X).max(0.0);
                let h = self.len_or0(n, "height", Axis::Y).max(0.0);
                (Rf::new(x, y, w, h), true)
            }
            "text" | "tspan" | "textPath" => {
                let glyphs = self.text_glyphs(n);
                let mut acc: Option<Rf> = None;
                for g in &glyphs {
                    let b = g.ink_box();
                    acc = Some(acc.map_or(b, |a| a.union(&b)));
                }
                (acc.unwrap_or_default(), acc.is_some())
            }
            "use" => {
                let x = self.len_or0(n, "x", Axis::X);
                let y = self.len_or0(n, "y", Axis::Y);
                let shift = Tf::translate(f64::from(x), f64::from(y));
                let Some(target) = href(self.d, n)
                    .and_then(|h| h.strip_prefix('#').map(str::to_string))
                    .and_then(|id| self.d.get_element_by_id(&id))
                    .filter(|&t| t != n && !self.d.contains(t, n))
                else {
                    return (Rf::default(), false);
                };
                if self.display_none(target) {
                    return (Rf::default(), false);
                }
                if svg_local(self.d, target) == Some("symbol") {
                    let w = self
                        .length(n, "width", Axis::X)
                        .unwrap_or_else(|| self.percent_of(n, 100.0, Axis::X));
                    let h = self
                        .length(n, "height", Axis::Y)
                        .unwrap_or_else(|| self.percent_of(n, 100.0, Axis::Y));
                    let vb = self.view_box_transform(target, w, h);
                    let (r, ok) = self.children_box(target, depth + 1);
                    return (shift.mul(&vb).map_rect(r), ok);
                }
                if !self.child_counts(target) {
                    return (Rf::default(), false);
                }
                let (r, ok) = self.object_box(target, depth + 1);
                if !ok {
                    return (Rf::default(), false);
                }
                let t = shift.mul(&self.local(target));
                (t.map_rect(r), true)
            }
            "switch" => {
                let kids: Vec<NodeId> = self.d.element_children(n).collect();
                for c in kids {
                    if self.child_counts(c) {
                        let (r, ok) = self.object_box(c, depth + 1);
                        if !ok {
                            return (Rf::default(), false);
                        }
                        return (self.local(c).map_rect(r), true);
                    }
                }
                (Rf::default(), false)
            }
            _ => self.children_box(n, depth),
        }
    }

    /// Union of the children's boxes in `n`'s user space.
    fn children_box(&mut self, n: NodeId, depth: u32) -> (Rf, bool) {
        let kids: Vec<NodeId> = self.d.element_children(n).collect();
        let mut acc: Option<Rf> = None;
        for c in kids {
            if !self.child_counts(c) {
                continue;
            }
            let (r, ok) = self.object_box(c, depth + 1);
            if !ok {
                continue;
            }
            let r = self.local(c).map_rect(r);
            acc = Some(acc.map_or(r, |a| a.union(&r)));
        }
        (acc.unwrap_or_default(), acc.is_some())
    }

    fn ellipse_radii(&mut self, n: NodeId) -> (f32, f32) {
        let rx = self.length(n, "rx", Axis::X);
        let ry = self.length(n, "ry", Axis::Y);
        match (rx, ry) {
            (Some(x), Some(y)) => (x, y),
            (Some(x), None) => (x, x),
            (None, Some(y)) => (y, y),
            (None, None) => (0.0, 0.0),
        }
    }

    /// The element's geometry as Blink's `AsPath` (`normalized`: a path
    /// element's data as `SVGPathQuery` sees it).
    fn shape_path(&mut self, n: NodeId, normalized: bool) -> Path {
        match svg_local(self.d, n) {
            Some("rect") => {
                let x = self.len_or0(n, "x", Axis::X);
                let y = self.len_or0(n, "y", Axis::Y);
                let w = self.len_or0(n, "width", Axis::X);
                let h = self.len_or0(n, "height", Axis::Y);
                if w <= 0.0 || h <= 0.0 {
                    return Path::default();
                }
                let (rx, ry) = self.ellipse_radii(n);
                let (rx, ry) = (rx.clamp(0.0, w / 2.0), ry.clamp(0.0, h / 2.0));
                if rx > 0.0 && ry > 0.0 {
                    Path::rrect(x, y, w, h, rx, ry)
                } else {
                    Path::rect(x, y, w, h)
                }
            }
            Some("circle") => {
                let cx = self.len_or0(n, "cx", Axis::X);
                let cy = self.len_or0(n, "cy", Axis::Y);
                let r = self.len_or0(n, "r", Axis::Other);
                if r > 0.0 {
                    Path::oval(cx, cy, r, r)
                } else {
                    Path::default()
                }
            }
            Some("ellipse") => {
                let cx = self.len_or0(n, "cx", Axis::X);
                let cy = self.len_or0(n, "cy", Axis::Y);
                let (rx, ry) = self.ellipse_radii(n);
                if rx > 0.0 && ry > 0.0 {
                    Path::oval(cx, cy, rx, ry)
                } else {
                    Path::default()
                }
            }
            Some("line") => {
                let mut p = Path::default();
                let x1 = self.len_or0(n, "x1", Axis::X);
                let y1 = self.len_or0(n, "y1", Axis::Y);
                let x2 = self.len_or0(n, "x2", Axis::X);
                let y2 = self.len_or0(n, "y2", Axis::Y);
                p.move_to([x1, y1]);
                p.line_to([x2, y2]);
                p
            }
            Some(k @ ("polyline" | "polygon")) => {
                let close = k == "polygon";
                let (nums, _) = number_list(attr(self.d, n, "points").unwrap_or(""));
                let mut p = Path::default();
                for (i, xy) in nums.chunks_exact(2).enumerate() {
                    if i == 0 {
                        p.move_to([xy[0], xy[1]]);
                    } else {
                        p.line_to([xy[0], xy[1]]);
                    }
                }
                if close && !p.is_empty() {
                    p.close();
                }
                p
            }
            Some("path") => {
                let d = inline_style(self.d, n, "d")
                    .and_then(|v| {
                        let v = v.trim();
                        v.strip_prefix("path(")
                            .and_then(|r| r.strip_suffix(')'))
                            .map(|r| r.trim().trim_matches(['"', '\'']).to_string())
                    })
                    .or_else(|| attr(self.d, n, "d").map(str::to_string))
                    .unwrap_or_default();
                Path::parse(&d, normalized)
            }
            _ => Path::default(),
        }
    }

    // ----- CTM -----

    /// `ComputeCTM`: the nearest viewport scope, or the screen.
    fn ctm(&mut self, n: NodeId, screen: bool) -> Tf {
        let mut m = ID;
        let mut cur = Some(n);
        while let Some(e) = cur {
            if svg_local(self.d, e).is_none() {
                break;
            }
            m = self.local(e).mul(&m);
            if screen && self.is_outer_svg(e) {
                let r = self.l.bounding_client_rect(self.d, e);
                let (bx, by) = self.l.client_offset(self.d, e);
                let (sx, sy) = self.l.window_scroll(self.d);
                m = Tf::translate(f64::from(r.x + bx + sx), f64::from(r.y + by + sy)).mul(&m);
                break;
            }
            if !screen && e != n && svg_local(self.d, e) == Some("svg") {
                break;
            }
            cur = self.d.parent(e);
        }
        m
    }

    /// `CalculateScreenFontSizeScalingFactor` for a text element.
    fn font_scale(&mut self, n: NodeId) -> f32 {
        let [a, b, c, d, ..] = self.ctm(n, true).0;
        let k = f64::from(self.dpr);
        let (xs, ys) = ((a * a + b * b).sqrt() * k, (c * c + d * d).sqrt() * k);
        let s = ((xs * xs + ys * ys) / 2.0).sqrt() as f32;
        if s.is_finite() && s > 0.0 { s } else { 1.0 }
    }

    // ----- text -----

    /// Lays out the `text` element that `n` belongs to; returns the
    /// glyphs of `n` (all glyphs for the `text` itself), with UTF-16
    /// indices relative to `n`.
    fn text_glyphs(&mut self, n: NodeId) -> Vec<Glyph> {
        if !self.rendered(n) {
            return Vec::new();
        }
        let root = std::iter::successors(Some(n), |&p| self.d.parent(p))
            .find(|&p| svg_local(self.d, p) == Some("text"));
        let Some(root) = root else {
            return Vec::new();
        };
        let all = self.layout_text(root);
        let mine: Vec<Glyph> = all
            .into_iter()
            .filter(|g| self.d.contains(n, g.owner))
            .collect();
        let base = mine.first().map_or(0, |g| g.unit);
        mine.into_iter()
            .map(|mut g| {
                g.unit -= base;
                g
            })
            .collect()
    }

    fn collect_chars(&mut self, n: NodeId, out: &mut Vec<(char, NodeId)>) {
        let kids: Vec<NodeId> = self.d.children(n).collect();
        for c in kids {
            match self.d.node_type(c) {
                NodeType::Text => {
                    if let Some(t) = self.d.character_data(c) {
                        out.extend(t.chars().map(|ch| (ch, n)));
                    }
                }
                NodeType::Element
                    if matches!(svg_local(self.d, c), Some("tspan" | "textPath" | "a"))
                        && !self.display_none(c) =>
                {
                    self.collect_chars(c, out);
                }
                _ => {}
            }
        }
    }

    fn layout_text(&mut self, root: NodeId) -> Vec<Glyph> {
        let mut raw = Vec::new();
        self.collect_chars(root, &mut raw);
        // white-space: normal collapsing across the whole text.
        let mut chars: Vec<(char, NodeId)> = Vec::with_capacity(raw.len());
        for (c, o) in raw {
            let space = matches!(c, ' ' | '\t' | '\n' | '\r');
            if space {
                if chars.last().is_none_or(|l| l.0 == ' ') {
                    continue;
                }
                chars.push((' ', o));
            } else {
                chars.push((c, o));
            }
        }
        if chars.last().is_some_and(|l| l.0 == ' ') {
            chars.pop();
        }
        let count = chars.len();
        if count == 0 {
            return Vec::new();
        }
        // Positioning attributes, outer elements first.
        let mut xs: Vec<Option<f32>> = vec![None; count];
        let mut ys = xs.clone();
        let mut dxs = xs.clone();
        let mut dys = xs.clone();
        let mut rots = xs.clone();
        let elems: Vec<NodeId> = std::iter::once(root)
            .chain(
                self.d
                    .descendants(root)
                    .filter(|&e| matches!(svg_local(self.d, e), Some("tspan"))),
            )
            .collect();
        for e in elems {
            let Some(start) = chars.iter().position(|c| self.d.contains(e, c.1)) else {
                continue;
            };
            let len = chars[start..]
                .iter()
                .take_while(|c| self.d.contains(e, c.1))
                .count();
            for (name, axis, arr) in [
                ("x", Axis::X, &mut xs),
                ("y", Axis::Y, &mut ys),
                ("dx", Axis::X, &mut dxs),
                ("dy", Axis::Y, &mut dys),
            ] {
                let Some(list) = attr(self.d, e, name).map(str::to_string) else {
                    continue;
                };
                let vals = self.length_list(e, &list, axis);
                for (i, v) in vals.into_iter().take(len).enumerate() {
                    arr[start + i] = Some(v);
                }
            }
            if let Some(list) = attr(self.d, e, "rotate") {
                let (vals, _) = number_list(list);
                if let Some(&last) = vals.last() {
                    for i in 0..len {
                        rots[start + i] = Some(vals.get(i).copied().unwrap_or(last));
                    }
                }
            }
        }
        // Fonts per owner element.
        let mut fonts: Vec<(NodeId, lb_layout::TextFont, f32)> = Vec::new();
        let mut glyphs = Vec::with_capacity(count);
        let (mut px, mut py) = (0.0f32, 0.0f32);
        let mut unit = 0u32;
        let mut chunks: Vec<usize> = vec![0];
        for (i, &(c, owner)) in chars.iter().enumerate() {
            if !fonts.iter().any(|f| f.0 == owner) {
                let st = self.style(owner);
                let tf = match st {
                    Some(st) => self.l.font_for(&st),
                    None => continue,
                };
                let scale = self.font_scale(owner);
                fonts.push((owner, tf, scale));
            }
            let (_, tf, scale) = fonts.iter().find(|f| f.0 == owner).expect("font").clone();
            if i > 0 && (xs[i].is_some() || ys[i].is_some()) {
                chunks.push(i);
            }
            if let Some(x) = xs[i] {
                px = x;
            }
            if let Some(y) = ys[i] {
                py = y;
            }
            px += dxs[i].unwrap_or(0.0);
            py += dys[i].unwrap_or(0.0);
            let mut scaled = tf.clone();
            scaled.size = tf.size * scale;
            let face = scaled.family.face(scaled.weight, scaled.italic);
            let mut adv = scaled.advance(face, c) / scale + tf.letter_spacing;
            if c == ' ' || c == '\u{a0}' {
                adv += tf.word_spacing;
            }
            let lm = scaled.line_metrics();
            let st = TextStyle {
                font: tf.clone(),
                align: TextAlign::Left,
                baseline: TextBaseline::Alphabetic,
                rtl: false,
            };
            let m = self.fonts.measure(&st, &c.to_string());
            let units = c.len_utf16() as u32;
            glyphs.push(Glyph {
                unit,
                units,
                owner,
                x: px,
                y: py,
                adv,
                asc: lm.ascent / scale,
                desc: lm.descent / scale,
                ink_l: m.actual_bounding_box_left as f32,
                ink_r: m.actual_bounding_box_right as f32,
                rot: rots[i].unwrap_or(0.0),
            });
            unit += units;
            px += adv;
        }
        // text-anchor per chunk.
        chunks.push(glyphs.len());
        for w in chunks.windows(2) {
            let (s, e) = (w[0], w[1]);
            if s >= e {
                continue;
            }
            let anchor = inherited_prop(self.d, glyphs[s].owner, "text-anchor");
            let k = match anchor.as_deref().map(str::trim) {
                Some("middle") => 0.5,
                Some("end") => 1.0,
                _ => continue,
            };
            let width = glyphs[e - 1].x + glyphs[e - 1].adv - glyphs[s].x;
            for g in &mut glyphs[s..e] {
                g.x -= width * k;
            }
        }
        glyphs
    }

    fn length_list(&mut self, n: NodeId, list: &str, axis: Axis) -> Vec<f32> {
        let mut out = Vec::new();
        for item in list.split(|c: char| c == ',' || c.is_ascii_whitespace()) {
            if item.is_empty() {
                continue;
            }
            match self.resolve(n, item, axis) {
                Some(v) => out.push(v),
                None => return Vec::new(),
            }
        }
        out
    }
}

/// A length: number and unit (`""` for none).
fn parse_length(s: &str) -> Option<(f32, &str)> {
    let s = s.trim();
    let mut sc = Scan::new(s);
    let v = sc.number()?;
    let unit = &s[sc.i..];
    let ok = matches!(
        unit.to_ascii_lowercase().as_str(),
        "" | "px" | "%" | "em" | "ex" | "rem" | "pt" | "pc" | "cm" | "mm" | "in" | "q"
    );
    ok.then_some((v, unit))
}

fn abs_length(v: f32, unit: &str, em: f32) -> Option<f32> {
    Some(match unit.to_ascii_lowercase().as_str() {
        "" | "px" => v,
        "em" => v * em,
        "ex" => v * em * 0.5,
        "rem" => v * 16.0,
        "pt" => v * 4.0 / 3.0,
        "pc" => v * 16.0,
        "in" => v * 96.0,
        "cm" => v * 96.0 / 2.54,
        "mm" => v * 96.0 / 25.4,
        "q" => v * 96.0 / 101.6,
        _ => return None,
    })
}

/// Runs `f` with the geometry context of `slot`'s document.
fn with_geo<T>(w: &Window, slot: u32, f: impl FnOnce(&mut Geo<'_>) -> T) -> T {
    let env = super::canvas::env(w);
    let dpr = super::window::screen_dims(w).device_pixel_ratio as f32;
    w.with_layout(slot, |l, d| {
        let mut g = Geo {
            l,
            d,
            fonts: &env.fonts,
            dpr,
        };
        f(&mut g)
    })
}

/// Blink's `ExceptionMessages::IndexExceedsMaximumBound`.
fn index_error(w: &Window, op: &str, given: u32, bound: u32) -> HostError {
    let eq = if given == bound { "or equal to " } else { "" };
    w.rt.err(
        "IndexSizeError",
        format!(
            "{}: The charnum provided ({given}) is greater than {eq}the maximum bound ({bound}).",
            exec(op, "SVGTextContentElement")
        ),
    )
}

/// The glyph holding UTF-16 index `i`.
fn glyph_at(gs: &[Glyph], i: u32) -> Option<&Glyph> {
    gs.iter().find(|g| i >= g.unit && i < g.unit + g.units)
}

fn num_chars(gs: &[Glyph]) -> u32 {
    gs.last().map_or(0, |g| g.unit + g.units)
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    register_values(reg, rt);
    register_graphics(reg, rt);
    register_text(reg, rt);
}

fn register_graphics(reg: &mut Registry, rt: &Runtime) {
    const G: &str = "SVGGraphicsElement";
    method(reg, rt, G, "getBBox", |cx, w| {
        let (slot, n) = this_svg_elem(cx, w, GRAPHICS)?;
        let r = with_geo(w, slot, |g| {
            if g.rendered(n) {
                g.object_box(n, 0).0
            } else {
                Rf::default()
            }
        });
        ret_obj(cx, w, SvgObj::Rect(r.arr()))
    });
    for (name, screen) in [("getCTM", false), ("getScreenCTM", true)] {
        method(reg, rt, G, name, move |cx, w| {
            let (slot, n) = this_svg_elem(cx, w, GRAPHICS)?;
            let m = with_geo(w, slot, |g| g.ctm(n, screen));
            ret_obj(cx, w, SvgObj::Matrix(m.0))
        });
    }

    const GEO: &str = "SVGGeometryElement";
    method(reg, rt, GEO, "getTotalLength", |cx, w| {
        let (slot, n) = this_svg_elem(cx, w, SHAPES)?;
        let len = with_geo(w, slot, |g| {
            g.d.is_connected(n).then(|| {
                let p = g.shape_path(n, true);
                if svg_local(g.d, n) == Some("path") {
                    traverse(&p, None).0
                } else {
                    path_length(&p)
                }
            })
        });
        match len {
            Some(l) => cx.ret(f64::from(l)),
            None => Err(w.rt.err(
                "InvalidStateError",
                format!(
                    "{}: This element is non-rendered element.",
                    exec("getTotalLength", GEO)
                ),
            )),
        }
    });
    method(reg, rt, GEO, "getPointAtLength", |cx, w| {
        need_args(cx, 1, "getPointAtLength", GEO)?;
        let (slot, n) = this_svg_elem(cx, w, SHAPES)?;
        let len = arg_float(cx, 0, &exec("getPointAtLength", GEO))?;
        let p = with_geo(w, slot, |g| {
            if !g.d.is_connected(n) {
                return Err("This element is non-rendered element.");
            }
            let path = g.shape_path(n, true);
            if path.is_empty() {
                return Err("The element's path is empty.");
            }
            Ok(if svg_local(g.d, n) == Some("path") {
                path_point_at_length(&path, len)
            } else {
                point_at_length(&path, len)
            })
        });
        match p {
            Ok(p) => ret_obj(cx, w, SvgObj::Point(p)),
            Err(m) => Err(w.rt.err(
                "InvalidStateError",
                format!("{}: {m}", exec("getPointAtLength", GEO)),
            )),
        }
    });

    const SVG: &str = "SVGSVGElement";
    for (name, make) in [
        ("createSVGRect", SvgObj::Rect([0.0; 4])),
        ("createSVGPoint", SvgObj::Point([0.0; 2])),
        ("createSVGMatrix", SvgObj::Matrix(ID.0)),
    ] {
        method(reg, rt, SVG, name, move |cx, w| {
            this_svg_elem(cx, w, &["svg"])?;
            ret_obj(cx, w, make.clone())
        });
    }
}

fn register_text(reg: &mut Registry, rt: &Runtime) {
    const T: &str = "SVGTextContentElement";
    fn glyphs(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<Vec<Glyph>, HostError> {
        let (slot, n) = this_svg_elem(cx, w, TEXT_CONTENT)?;
        Ok(with_geo(w, slot, |g| g.text_glyphs(n)))
    }
    /// The checked `charnum` argument and the glyph holding it.
    fn char_arg(
        cx: &mut CallCtx<'_, '_, '_>,
        w: &Window,
        op: &'static str,
        nargs: usize,
    ) -> Result<(Vec<Glyph>, u32), HostError> {
        need_args(cx, nargs, op, T)?;
        let gs = glyphs(cx, w)?;
        let i = cx.arg_u32(0)?;
        let n = num_chars(&gs);
        if i >= n {
            return Err(index_error(w, op, i, n));
        }
        Ok((gs, i))
    }
    method(reg, rt, T, "getNumberOfChars", |cx, w| {
        let gs = glyphs(cx, w)?;
        cx.ret(num_chars(&gs) as i32)
    });
    method(reg, rt, T, "getComputedTextLength", |cx, w| {
        let gs = glyphs(cx, w)?;
        let len: f32 = gs.iter().map(|g| g.adv).sum();
        cx.ret(f64::from(len))
    });
    method(reg, rt, T, "getSubStringLength", |cx, w| {
        let (gs, i) = char_arg(cx, w, "getSubStringLength", 2)?;
        let n = cx.arg_u32(1)?;
        let end = u64::from(i) + u64::from(n);
        let len: f32 = gs
            .iter()
            .filter(|g| u64::from(g.unit + g.units) > u64::from(i) && u64::from(g.unit) < end)
            .map(|g| g.adv)
            .sum();
        cx.ret(f64::from(len))
    });
    method(reg, rt, T, "getStartPositionOfChar", |cx, w| {
        let (gs, i) = char_arg(cx, w, "getStartPositionOfChar", 1)?;
        let g = glyph_at(&gs, i).copied().ok_or_else(illegal)?;
        ret_obj(cx, w, SvgObj::Point([g.x, g.y]))
    });
    method(reg, rt, T, "getEndPositionOfChar", |cx, w| {
        let (gs, i) = char_arg(cx, w, "getEndPositionOfChar", 1)?;
        let g = glyph_at(&gs, i).copied().ok_or_else(illegal)?;
        let (x, y) = g.rotation().map(f64::from(g.x + g.adv), f64::from(g.y));
        ret_obj(cx, w, SvgObj::Point([x as f32, y as f32]))
    });
    method(reg, rt, T, "getExtentOfChar", |cx, w| {
        let (gs, i) = char_arg(cx, w, "getExtentOfChar", 1)?;
        let g = glyph_at(&gs, i).copied().ok_or_else(illegal)?;
        ret_obj(cx, w, SvgObj::Rect(g.extent().arr()))
    });
    method(reg, rt, T, "getRotationOfChar", |cx, w| {
        let (gs, i) = char_arg(cx, w, "getRotationOfChar", 1)?;
        let g = glyph_at(&gs, i).copied().ok_or_else(illegal)?;
        cx.ret(f64::from(g.rot))
    });
    method(reg, rt, T, "getCharNumAtPosition", |cx, w| {
        let (x, y) =
            if cx.is_empty() || matches!(cx.arg_kind(0), ValueKind::Undefined | ValueKind::Null) {
                (0.0, 0.0)
            } else if matches!(cx.arg_kind(0), ValueKind::Object | ValueKind::Function) {
                let d = Dict::arg(cx, 0);
                let x = d.number(cx, "x")?.unwrap_or(0.0);
                let y = d.number(cx, "y")?.unwrap_or(0.0);
                (x, y)
            } else {
                return Err(HostError::type_error(format!(
                    "{}: The provided value is not of type 'DOMPointInit'.",
                    exec("getCharNumAtPosition", T)
                )));
            };
        let gs = glyphs(cx, w)?;
        let hit = gs.iter().find(|g| {
            let r = g.extent();
            let (x, y) = (x as f32, y as f32);
            x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h
        });
        cx.ret(hit.map_or(-1, |g| g.unit as i32))
    });
    method(reg, rt, T, "selectSubString", |cx, w| {
        char_arg(cx, w, "selectSubString", 2)?;
        Ok(())
    });
}

fn register_values(reg: &mut Registry, rt: &Runtime) {
    // SVGRect
    for (i, name) in ["x", "y", "width", "height"].into_iter().enumerate() {
        getter(reg, rt, "SVGRect", name, move |cx, w| {
            let (_, r) = this_rect(cx, w)?;
            cx.ret(f64::from(r[i]))
        });
        setter(reg, rt, "SVGRect", name, move |cx, w| {
            let (id, mut r) = this_rect(cx, w)?;
            r[i] = arg_float(
                cx,
                0,
                &format!("Failed to set the '{name}' property on 'SVGRect'"),
            )?;
            store(w, id, SvgObj::Rect(r));
            Ok(())
        });
    }
    // SVGPoint
    for (i, name) in ["x", "y"].into_iter().enumerate() {
        getter(reg, rt, "SVGPoint", name, move |cx, w| {
            let (_, p) = this_point(cx, w)?;
            cx.ret(f64::from(p[i]))
        });
        setter(reg, rt, "SVGPoint", name, move |cx, w| {
            let (id, mut p) = this_point(cx, w)?;
            p[i] = arg_float(
                cx,
                0,
                &format!("Failed to set the '{name}' property on 'SVGPoint'"),
            )?;
            store(w, id, SvgObj::Point(p));
            Ok(())
        });
    }
    method(reg, rt, "SVGPoint", "matrixTransform", |cx, w| {
        need_args(cx, 1, "matrixTransform", "SVGPoint")?;
        let (_, p) = this_point(cx, w)?;
        let m = matrix_arg(cx, w, "matrixTransform", "SVGPoint")?;
        let (x, y) = m.map(f64::from(p[0]), f64::from(p[1]));
        ret_obj(cx, w, SvgObj::Point([x as f32, y as f32]))
    });
    // SVGMatrix
    const M: &str = "SVGMatrix";
    for (i, name) in ["a", "b", "c", "d", "e", "f"].into_iter().enumerate() {
        getter(reg, rt, M, name, move |cx, w| {
            let (_, m) = this_matrix(cx, w)?;
            cx.ret(m.0[i])
        });
        setter(reg, rt, M, name, move |cx, w| {
            let (id, mut m) = this_matrix(cx, w)?;
            let v = cx.arg_f64(0)?;
            if !v.is_finite() {
                return Err(HostError::type_error(format!(
                    "Failed to set the '{name}' property on 'SVGMatrix': The provided double value is non-finite."
                )));
            }
            m.0[i] = v;
            store(w, id, SvgObj::Matrix(m.0));
            Ok(())
        });
    }
    method(reg, rt, M, "multiply", |cx, w| {
        need_args(cx, 1, "multiply", M)?;
        let (_, m) = this_matrix(cx, w)?;
        let o = matrix_arg(cx, w, "multiply", M)?;
        ret_obj(cx, w, SvgObj::Matrix(m.mul(&o).0))
    });
    method(reg, rt, M, "inverse", |cx, w| {
        let (_, m) = this_matrix(cx, w)?;
        match m.inverse() {
            Some(i) => ret_obj(cx, w, SvgObj::Matrix(i.0)),
            None => Err(w.rt.err(
                "InvalidStateError",
                format!("{}: The matrix is not invertible.", exec("inverse", M)),
            )),
        }
    });
    /// An operation taking `n` float arguments that post-multiplies.
    fn float_op(
        reg: &mut Registry,
        rt: &Runtime,
        name: &'static str,
        n: usize,
        make: fn(&[f64]) -> Tf,
    ) {
        method(reg, rt, M, name, move |cx, w| {
            need_args(cx, n, name, M)?;
            let (_, m) = this_matrix(cx, w)?;
            let mut a = Vec::with_capacity(n);
            for i in 0..n {
                a.push(f64::from(arg_float(cx, i, &exec(name, M))?));
            }
            ret_obj(cx, w, SvgObj::Matrix(m.mul(&make(&a)).0))
        });
    }
    float_op(reg, rt, "translate", 2, |a| Tf::translate(a[0], a[1]));
    float_op(reg, rt, "scale", 1, |a| Tf::scale(a[0], a[0]));
    float_op(reg, rt, "scaleNonUniform", 2, |a| Tf::scale(a[0], a[1]));
    float_op(reg, rt, "rotate", 1, |a| Tf::rotate(a[0]));
    float_op(reg, rt, "flipX", 0, |_| Tf::scale(-1.0, 1.0));
    float_op(reg, rt, "flipY", 0, |_| Tf::scale(1.0, -1.0));
    float_op(reg, rt, "skewX", 1, |a| {
        Tf([1.0, 0.0, a[0].to_radians().tan(), 1.0, 0.0, 0.0])
    });
    float_op(reg, rt, "skewY", 1, |a| {
        Tf([1.0, a[0].to_radians().tan(), 0.0, 1.0, 0.0, 0.0])
    });
    method(reg, rt, M, "rotateFromVector", |cx, w| {
        need_args(cx, 2, "rotateFromVector", M)?;
        let (_, m) = this_matrix(cx, w)?;
        let x = arg_float(cx, 0, &exec("rotateFromVector", M))?;
        let y = arg_float(cx, 1, &exec("rotateFromVector", M))?;
        if x == 0.0 || y == 0.0 {
            return Err(w.rt.err(
                "InvalidAccessError",
                format!("{}: Arguments cannot be zero.", exec("rotateFromVector", M)),
            ));
        }
        let r = f64::from(y).atan2(f64::from(x));
        let (s, c) = (r.sin(), r.cos());
        ret_obj(cx, w, SvgObj::Matrix(m.mul(&Tf([c, s, -s, c, 0.0, 0.0])).0))
    });
}

/// An `SVGMatrix` argument (index 0).
fn matrix_arg(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    op: &str,
    iface: &str,
) -> Result<Tf, HostError> {
    let m = cx
        .arg_host(0)
        .filter(|h| h.kind == super::K_OBJ)
        .and_then(|h| svg_obj(w, h.id));
    match m {
        Some(SvgObj::Matrix(m)) => Ok(Tf(m)),
        _ => Err(HostError::type_error(format!(
            "{}: parameter 1 is not of type 'SVGMatrix'.",
            exec(op, iface)
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_bounds_and_lengths() {
        let p = Path::parse("M10 10 C 20 20, 40 20, 50 10", false);
        assert_eq!(p.tight_bounds(), Rf::new(10.0, 10.0, 40.0, 7.5));
        let l = traverse(&Path::parse("M10 10 C 20 20, 40 20, 50 10", true), None).0;
        assert!((l - 43.808_63).abs() < 1e-3, "{l}");
        let r = Path::rect(10.0, 20.0, 30.0, 40.0);
        assert_eq!(path_length(&r), 140.0);
        let bad = Path::parse("M 10 10 L 20 20 X 50 50", false);
        assert_eq!(bad.tight_bounds(), Rf::new(10.0, 10.0, 10.0, 10.0));
        assert!(Path::parse("L 1 1", false).is_empty());
    }

    #[test]
    fn transform_lists() {
        let t = parse_transform_list("translate(5,7) scale(2)");
        assert_eq!(t, Tf([2.0, 0.0, 0.0, 2.0, 5.0, 7.0]));
        assert_eq!(parse_transform_list("translate(5,7) bogus(1)"), ID);
        assert_eq!(parse_transform_list("rotate(1 2)"), ID);
    }
}
