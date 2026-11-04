//! Box decorations for page screenshots: background layers (images and
//! gradients with size, position, repeat, origin, clip and attachment),
//! border radius (backgrounds, borders and replaced content clipped to
//! rounded corners) and `box-shadow` (outer and inset, Gaussian blur).
//!
//! Geometry follows CSS Backgrounds 3: radii are scaled down together when
//! adjacent ones overlap; inner radii shrink by the border (and padding)
//! widths; gradients follow CSS Images 3 (gradient line length, corner
//! angles, ending shape sizes, stop fix-up, transition hints). Everything
//! is drawn in device pixels.

use std::rc::Rc;

use lb_css::bg::{Attachment, BgBox, BgImage, BgSize, Image, LinearDir, RadialSize, Repeat, Stop};
use lb_css::{Color, ComputedStyle, Lp};
use lb_layout::{DisplayItem, Rect};
use tiny_skia::{
    BlendMode, FillRule, FilterQuality, GradientStop, LinearGradient, Mask, Paint, Path,
    PathBuilder, Pattern, Pixmap, PixmapPaint, Point, RadialGradient, Shader, SpreadMode,
    Transform,
};

/// Largest tile or shadow buffer, in device pixels.
const MAX_BUF: u64 = 32_000_000;

/// Corner radii (top-left, top-right, bottom-right, bottom-left), each
/// (horizontal, vertical), in the units of the rect they belong to.
pub(crate) type Radii = [(f32, f32); 4];

pub(crate) fn sk_color(c: Color) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.r, c.g, c.b, (c.a.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn paint_of(c: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(sk_color(c));
    p.anti_alias = true;
    p
}

/// A page rect in device pixels.
fn dev(r: Rect, ts: Transform) -> Rect {
    Rect::new(
        r.x * ts.sx + ts.tx,
        r.y * ts.sy + ts.ty,
        r.width * ts.sx,
        r.height * ts.sy,
    )
}

fn scale_radii(r: &Radii, s: f32) -> Radii {
    r.map(|(a, b)| (a * s, b * s))
}

pub(crate) fn is_zero(r: &Radii) -> bool {
    r.iter().all(|(a, b)| *a <= 0.0 || *b <= 0.0)
}

/// Used outer radii of a box in CSS px (overlapping radii scaled down).
pub(crate) fn used_radii(style: &ComputedStyle, b: Rect) -> Radii {
    let mut r: Radii = [0, 1, 2, 3].map(|i| {
        let (h, v) = &style.radius[i];
        (h.resolve(b.width).max(0.0), v.resolve(b.height).max(0.0))
    });
    if r.iter().all(|(a, b)| *a == 0.0 && *b == 0.0) {
        return r;
    }
    let ratio = |len: f32, a: f32, b: f32| {
        if a + b > 0.0 {
            len / (a + b)
        } else {
            f32::INFINITY
        }
    };
    let f = ratio(b.width, r[0].0, r[1].0)
        .min(ratio(b.width, r[3].0, r[2].0))
        .min(ratio(b.height, r[0].1, r[3].1))
        .min(ratio(b.height, r[1].1, r[2].1));
    if f < 1.0 {
        for c in &mut r {
            c.0 *= f;
            c.1 *= f;
        }
    }
    r
}

/// Radii of an inner box: shrunk by the insets (top, right, bottom, left).
pub(crate) fn inner_radii(r: &Radii, ins: [f32; 4]) -> Radii {
    let [t, rt, b, l] = ins;
    [
        ((r[0].0 - l).max(0.0), (r[0].1 - t).max(0.0)),
        ((r[1].0 - rt).max(0.0), (r[1].1 - t).max(0.0)),
        ((r[2].0 - rt).max(0.0), (r[2].1 - b).max(0.0)),
        ((r[3].0 - l).max(0.0), (r[3].1 - b).max(0.0)),
    ]
}

/// Radii grown by `d` (box-shadow spread): zero radii stay zero.
fn grow_radii(r: &Radii, d: f32) -> Radii {
    r.map(|(a, b)| {
        let g = |v: f32| if v > 0.0 { (v + d).max(0.0) } else { 0.0 };
        (g(a), g(b))
    })
}

/// A rounded rect path (device coordinates).
pub(crate) fn rrect(r: Rect, radii: &Radii) -> Option<Path> {
    if r.width <= 0.0 || r.height <= 0.0 {
        return None;
    }
    if is_zero(radii) {
        return tiny_skia::Rect::from_xywh(r.x, r.y, r.width, r.height).map(PathBuilder::from_rect);
    }
    const K: f32 = 0.552_284_8;
    let (x0, y0, x1, y1) = (r.x, r.y, r.right(), r.bottom());
    let [tl, tr, br, bl] = *radii;
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + tl.0, y0);
    pb.line_to(x1 - tr.0, y0);
    if tr.0 > 0.0 && tr.1 > 0.0 {
        pb.cubic_to(
            x1 - tr.0 * (1.0 - K),
            y0,
            x1,
            y0 + tr.1 * (1.0 - K),
            x1,
            y0 + tr.1,
        );
    }
    pb.line_to(x1, y1 - br.1);
    if br.0 > 0.0 && br.1 > 0.0 {
        pb.cubic_to(
            x1,
            y1 - br.1 * (1.0 - K),
            x1 - br.0 * (1.0 - K),
            y1,
            x1 - br.0,
            y1,
        );
    }
    pb.line_to(x0 + bl.0, y1);
    if bl.0 > 0.0 && bl.1 > 0.0 {
        pb.cubic_to(
            x0 + bl.0 * (1.0 - K),
            y1,
            x0,
            y1 - bl.1 * (1.0 - K),
            x0,
            y1 - bl.1,
        );
    }
    pb.line_to(x0, y0 + tl.1);
    if tl.0 > 0.0 && tl.1 > 0.0 {
        pb.cubic_to(
            x0,
            y0 + tl.1 * (1.0 - K),
            x0 + tl.0 * (1.0 - K),
            y0,
            x0 + tl.0,
            y0,
        );
    }
    pb.close();
    pb.finish()
}

/// `base` (or everything) intersected with `path`.
pub(crate) fn clip_mask(base: Option<&Mask>, path: &Path, w: u32, h: u32) -> Option<Mask> {
    let mut m = match base {
        Some(b) => b.clone(),
        None => {
            let mut m = Mask::new(w, h)?;
            m.data_mut().fill(255);
            m
        }
    };
    m.intersect_path(path, FillRule::Winding, true, Transform::identity());
    Some(m)
}

/// A box of an item in page coordinates.
fn box_rect(item: &DisplayItem, b: BgBox) -> Rect {
    match b {
        BgBox::BorderBox | BgBox::Text => item.rect,
        BgBox::PaddingBox => item.padding_box(),
        BgBox::ContentBox => item.content_box(),
    }
}

/// Insets of `b` from the border box (top, right, bottom, left).
fn box_insets(item: &DisplayItem, b: BgBox) -> [f32; 4] {
    let [bt, br, bb, bl] = item.border;
    let [pt, pr, pb, pl] = item.padding;
    match b {
        BgBox::BorderBox | BgBox::Text => [0.0; 4],
        BgBox::PaddingBox => [bt, br, bb, bl],
        BgBox::ContentBox => [bt + pt, br + pr, bb + pb, bl + pl],
    }
}

/// What backgrounds need from the host.
pub(crate) struct BgEnv<'a> {
    pub ts: Transform,
    pub width: u32,
    pub height: u32,
    pub images: &'a dyn Fn(&str) -> Option<Rc<Pixmap>>,
    /// The viewport in page coordinates (`background-attachment: fixed`).
    pub viewport: Rect,
}

/// Paints an item's background color and image layers. `canvas` paints
/// the root background over the whole canvas instead of the box (device
/// rect); `color` false skips the color (already painted).
pub(crate) fn background(
    px: &mut Pixmap,
    item: &DisplayItem,
    env: &BgEnv<'_>,
    mask: Option<&Mask>,
    canvas: Option<Rect>,
    color: bool,
) {
    let st = &item.style;
    let bg = &st.background;
    let c = st.background_color;
    if bg.is_none() && (!color || c.a <= 0.0) {
        return;
    }
    let s = env.ts.sx;
    let radii = used_radii(st, item.rect);
    let clip_path = |b: BgBox| -> Option<Path> {
        if let Some(cv) = canvas {
            return rrect(cv, &[(0.0, 0.0); 4]);
        }
        let r = inner_radii(&radii, box_insets(item, b));
        rrect(dev(box_rect(item, b), env.ts), &scale_radii(&r, s))
    };
    if color
        && c.a > 0.0
        && let Some(path) = clip_path(bg.color_clip())
    {
        px.fill_path(
            &path,
            &paint_of(c),
            FillRule::Winding,
            Transform::identity(),
            mask,
        );
    }
    for layer in bg.layers().iter().rev() {
        if matches!(layer.image, Image::None) {
            continue;
        }
        let Some(clip) = clip_path(layer.clip) else {
            continue;
        };
        let area = if layer.attachment == Attachment::Fixed {
            dev(env.viewport, env.ts)
        } else {
            dev(box_rect(item, layer.origin), env.ts)
        };
        let lmask = if canvas.is_none() && !is_zero(&radii) {
            clip_mask(mask, &clip, env.width, env.height)
        } else {
            None
        };
        let m = lmask.as_ref().or(mask);
        let bounds = clip.bounds();
        let paint_rect = Rect::new(bounds.x(), bounds.y(), bounds.width(), bounds.height());
        paint_layer(px, layer, area, paint_rect, s, env, m);
    }
}

/// Intrinsic size of an image in CSS px (`None` for gradients).
fn intrinsic(img: &BgImage, env: &BgEnv<'_>) -> Option<Option<(f32, f32)>> {
    match img {
        Image::Url(u) => {
            let p = (env.images)(u)?;
            Some(Some((p.width() as f32, p.height() as f32)))
        }
        _ => Some(None),
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_layer(
    px: &mut Pixmap,
    layer: &lb_css::bg::Layer<'_>,
    area: Rect,
    paint: Rect,
    s: f32,
    env: &BgEnv<'_>,
    mask: Option<&Mask>,
) {
    let Some(natural) = intrinsic(layer.image, env) else {
        return;
    };
    // Tile size in device px.
    let (aw, ah) = (area.width, area.height);
    let nat = natural.map(|(w, h)| (w * s, h * s));
    let (mut tw, mut th) = match layer.size {
        BgSize::Cover | BgSize::Contain => match nat {
            Some((w, h)) if w > 0.0 && h > 0.0 => {
                let k = if matches!(layer.size, BgSize::Cover) {
                    (aw / w).max(ah / h)
                } else {
                    (aw / w).min(ah / h)
                };
                (w * k, h * k)
            }
            _ => (aw, ah),
        },
        BgSize::Explicit(a, b) => {
            let a = a.as_ref().map(|l| l.resolve(aw / s) * s);
            let b = b.as_ref().map(|l| l.resolve(ah / s) * s);
            match (a, b, nat) {
                (Some(a), Some(b), _) => (a, b),
                (Some(a), None, Some((w, h))) if w > 0.0 => (a, a * h / w),
                (None, Some(b), Some((w, h))) if h > 0.0 => (b * w / h, b),
                (Some(a), None, None) => (a, ah),
                (None, Some(b), None) => (aw, b),
                (None, None, Some((w, h))) => (w, h),
                (None, None, None) => (aw, ah),
                (Some(a), None, Some(_)) => (a, ah),
                (None, Some(b), Some(_)) => (aw, b),
            }
        }
    };
    let (rx, ry) = layer.repeat;
    if rx == Repeat::Round && tw > 0.0 {
        let n = (aw / tw).round().max(1.0);
        let nw = aw / n;
        if matches!(layer.size, BgSize::Explicit(_, None)) || ry != Repeat::Round {
            th = if matches!(
                layer.size,
                BgSize::Explicit(None, _) | BgSize::Explicit(_, None)
            ) {
                th * nw / tw
            } else {
                th
            };
        }
        tw = nw;
    }
    if ry == Repeat::Round && th > 0.0 {
        let n = (ah / th).round().max(1.0);
        th = ah / n;
    }
    if tw < 0.5 || th < 0.5 {
        return;
    }
    let (itw, ith) = (tw.round().max(1.0) as u32, th.round().max(1.0) as u32);
    if u64::from(itw) * u64::from(ith) > MAX_BUF {
        return;
    }
    // Position: percentages of (area - tile).
    let pos = |l: &Lp, free: f32| l.resolve(free / s) * s;
    let mut x = area.x + pos(layer.pos.0, aw - tw);
    let mut y = area.y + pos(layer.pos.1, ah - th);
    let Some(tile) = make_tile(layer.image, itw, ith, s, env) else {
        return;
    };
    // `space`: as many whole tiles as fit, spread out.
    let space = |len: f32, t: f32, start: &mut f32| -> Option<f32> {
        let n = (len / t).floor();
        if n >= 2.0 {
            *start = 0.0;
            Some((len - n * t) / (n - 1.0))
        } else {
            None
        }
    };
    let mut gap_x = None;
    let mut gap_y = None;
    let (mut rx, mut ry) = (rx, ry);
    if rx == Repeat::Space {
        let mut st = x - area.x;
        gap_x = space(aw, tw, &mut st);
        if gap_x.is_some() {
            x = area.x + st;
        } else {
            rx = Repeat::NoRepeat;
        }
    }
    if ry == Repeat::Space {
        let mut st = y - area.y;
        gap_y = space(ah, th, &mut st);
        if gap_y.is_some() {
            y = area.y + st;
        } else {
            ry = Repeat::NoRepeat;
        }
    }
    if gap_x.is_some() || gap_y.is_some() {
        // Explicit tiles.
        let step_x = tw + gap_x.unwrap_or(0.0);
        let step_y = th + gap_y.unwrap_or(0.0);
        let xs: Vec<f32> = axis_starts(x, step_x, paint.x, paint.right(), rx != Repeat::NoRepeat);
        let ys: Vec<f32> = axis_starts(y, step_y, paint.y, paint.bottom(), ry != Repeat::NoRepeat);
        if xs.len() * ys.len() > 100_000 {
            return;
        }
        let clip = Rect::new(paint.x, paint.y, paint.width, paint.height);
        let cmask = clip_rect_mask(mask, clip, env);
        for &ty in &ys {
            for &tx in &xs {
                px.draw_pixmap(
                    tx.round() as i32,
                    ty.round() as i32,
                    tile.as_ref(),
                    &PixmapPaint::default(),
                    Transform::identity(),
                    cmask.as_ref().or(mask),
                );
            }
        }
        return;
    }
    let mut region = paint;
    if rx == Repeat::NoRepeat {
        region = intersect(region, Rect::new(x, region.y, tw, region.height));
    }
    if ry == Repeat::NoRepeat {
        region = intersect(region, Rect::new(region.x, y, region.width, th));
    }
    if region.width <= 0.0 || region.height <= 0.0 {
        return;
    }
    // The tile is whole pixels; scale it to the exact (fractional) size so
    // repeats do not drift.
    let (kx, ky) = (tw / itw as f32, th / ith as f32);
    let exact = (kx - 1.0).abs() < 1e-4 && (ky - 1.0).abs() < 1e-4;
    let shader = Pattern::new(
        tile.as_ref(),
        SpreadMode::Repeat,
        if exact {
            FilterQuality::Nearest
        } else {
            FilterQuality::Bilinear
        },
        1.0,
        Transform::from_row(kx, 0.0, 0.0, ky, x, y),
    );
    let mut p = Paint {
        shader,
        ..Paint::default()
    };
    p.anti_alias = false;
    if let Some(r) = tiny_skia::Rect::from_xywh(region.x, region.y, region.width, region.height) {
        px.fill_rect(r, &p, Transform::identity(), mask);
    }
}

fn axis_starts(origin: f32, step: f32, lo: f32, hi: f32, repeat: bool) -> Vec<f32> {
    if !repeat || step <= 0.0 {
        return vec![origin];
    }
    let mut start = origin - ((origin - lo) / step).ceil() * step;
    let mut out = Vec::new();
    while start < hi && out.len() < 10_000 {
        out.push(start);
        start += step;
    }
    out
}

fn intersect(a: Rect, b: Rect) -> Rect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let r = a.right().min(b.right());
    let btm = a.bottom().min(b.bottom());
    Rect::new(x, y, (r - x).max(0.0), (btm - y).max(0.0))
}

fn clip_rect_mask(mask: Option<&Mask>, r: Rect, env: &BgEnv<'_>) -> Option<Mask> {
    let path =
        tiny_skia::Rect::from_xywh(r.x, r.y, r.width, r.height).map(PathBuilder::from_rect)?;
    clip_mask(mask, &path, env.width, env.height)
}

/// One tile of an image layer, `w`×`h` device px.
fn make_tile(img: &BgImage, w: u32, h: u32, s: f32, env: &BgEnv<'_>) -> Option<Pixmap> {
    let mut tile = Pixmap::new(w, h)?;
    match img {
        Image::None => return None,
        Image::Url(u) => {
            let src = (env.images)(u)?;
            let t = Transform::from_scale(
                w as f32 / src.width() as f32,
                h as f32 / src.height() as f32,
            );
            tile.draw_pixmap(
                0,
                0,
                src.as_ref().as_ref(),
                &PixmapPaint {
                    quality: FilterQuality::Bilinear,
                    ..PixmapPaint::default()
                },
                t,
                None,
            );
        }
        Image::Linear {
            repeating,
            dir,
            stops,
        } => {
            let (wf, hf) = (w as f32, h as f32);
            let deg = match *dir {
                LinearDir::Default => 180.0,
                LinearDir::Angle(a) => a,
                LinearDir::To(0, v) => {
                    if v < 0 {
                        0.0
                    } else {
                        180.0
                    }
                }
                LinearDir::To(hz, 0) => {
                    if hz > 0 {
                        90.0
                    } else {
                        270.0
                    }
                }
                LinearDir::To(hz, v) => {
                    // Perpendicular to the diagonal between the other two
                    // corners.
                    let a = hf.atan2(wf).to_degrees();
                    match (hz > 0, v < 0) {
                        (true, true) => a,
                        (true, false) => 180.0 - a,
                        (false, false) => 180.0 + a,
                        (false, true) => 360.0 - a,
                    }
                }
            };
            let rad = deg.to_radians();
            let (dx, dy) = (rad.sin(), -rad.cos());
            let len = (wf * rad.sin()).abs() + (hf * rad.cos()).abs();
            let (cx, cy) = (wf / 2.0, hf / 2.0);
            let Some((p0, p1, gs)) = gradient_stops(stops, len, s, *repeating) else {
                return Some(tile);
            };
            let sx = cx - dx * len / 2.0;
            let sy = cy - dy * len / 2.0;
            let start = Point::from_xy(sx + dx * p0, sy + dy * p0);
            let end = Point::from_xy(sx + dx * p1, sy + dy * p1);
            let mode = if *repeating {
                SpreadMode::Repeat
            } else {
                SpreadMode::Pad
            };
            let shader = LinearGradient::new(start, end, gs, mode, Transform::identity());
            fill_shader(&mut tile, shader);
        }
        Image::Radial {
            repeating,
            circle,
            size,
            at,
            stops,
        } => {
            let (wf, hf) = (w as f32, h as f32);
            let (cx, cy) = match at {
                Some((x, y)) => (x.resolve(wf / s) * s, y.resolve(hf / s) * s),
                None => (wf / 2.0, hf / 2.0),
            };
            let (dl, dr, dt, db) = (cx.abs(), (wf - cx).abs(), cy.abs(), (hf - cy).abs());
            let (rx, ry) = match size {
                RadialSize::ClosestSide if *circle => {
                    let r = dl.min(dr).min(dt).min(db);
                    (r, r)
                }
                RadialSize::FarthestSide if *circle => {
                    let r = dl.max(dr).max(dt).max(db);
                    (r, r)
                }
                RadialSize::ClosestSide => (dl.min(dr), dt.min(db)),
                RadialSize::FarthestSide => (dl.max(dr), dt.max(db)),
                RadialSize::ClosestCorner | RadialSize::FarthestCorner => {
                    let far = matches!(size, RadialSize::FarthestCorner);
                    let (sx, sy) = if far {
                        (dl.max(dr), dt.max(db))
                    } else {
                        (dl.min(dr), dt.min(db))
                    };
                    if *circle {
                        let d = sx.hypot(sy);
                        (d, d)
                    } else {
                        (sx * std::f32::consts::SQRT_2, sy * std::f32::consts::SQRT_2)
                    }
                }
                RadialSize::Explicit(a, b) => {
                    let a = a.resolve(wf / s) * s;
                    match b {
                        Some(b) => (a, b.resolve(hf / s) * s),
                        None => (a, a),
                    }
                }
            };
            if rx <= 0.0 || ry <= 0.0 {
                return Some(tile);
            }
            let Some((p0, p1, gs)) = gradient_stops(stops, rx, s, *repeating) else {
                return Some(tile);
            };
            let mode = if *repeating {
                SpreadMode::Repeat
            } else {
                SpreadMode::Pad
            };
            let k = ry / rx;
            let t = Transform::from_row(1.0, 0.0, 0.0, k, 0.0, cy * (1.0 - k));
            let c = Point::from_xy(cx, cy);
            let shader = RadialGradient::new(c, p0.max(0.0), c, p1.max(0.01), gs, mode, t);
            fill_shader(&mut tile, shader);
        }
    }
    Some(tile)
}

fn fill_shader(tile: &mut Pixmap, shader: Option<Shader<'static>>) {
    let Some(shader) = shader else { return };
    let p = Paint {
        shader,
        anti_alias: false,
        ..Paint::default()
    };
    if let Some(r) = tiny_skia::Rect::from_xywh(0.0, 0.0, tile.width() as f32, tile.height() as f32)
    {
        tile.fill_rect(r, &p, Transform::identity(), None);
    }
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    let l = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color {
        r: l(a.r, b.r),
        g: l(a.g, b.g),
        b: l(a.b, b.b),
        a: a.a + (b.a - a.a) * t,
    }
}

/// Premultiplied interpolation, returned unpremultiplied.
fn mix_premul(a: Color, b: Color, t: f32) -> Color {
    let alpha = a.a + (b.a - a.a) * t;
    if alpha <= 0.0 {
        return Color {
            a: 0.0,
            ..mix(a, b, t)
        };
    }
    let ch = |x: u8, y: u8| {
        let v = (f32::from(x) * a.a * (1.0 - t) + f32::from(y) * b.a * t) / alpha;
        v.round().clamp(0.0, 255.0) as u8
    };
    Color {
        r: ch(a.r, b.r),
        g: ch(a.g, b.g),
        b: ch(a.b, b.b),
        a: alpha,
    }
}

/// Resolves stops along a gradient ray of `len` device px: returns the
/// first and last position (device px) and normalized tiny-skia stops.
fn gradient_stops(
    stops: &[Stop<Color, Lp>],
    len: f32,
    s: f32,
    repeating: bool,
) -> Option<(f32, f32, Vec<GradientStop>)> {
    // Positions in device px; None = to fix up.
    let mut pos: Vec<Option<f32>> = stops
        .iter()
        .map(|st| st.pos.as_ref().map(|l| l.resolve(len / s) * s))
        .collect();
    let n = stops.len();
    if n == 0 {
        return None;
    }
    if pos[0].is_none() {
        pos[0] = Some(0.0);
    }
    if pos[n - 1].is_none() {
        pos[n - 1] = Some(len);
    }
    // Never decreasing.
    let mut maxp = f32::MIN;
    for p in pos.iter_mut().flatten() {
        if *p < maxp {
            *p = maxp;
        }
        maxp = *p;
    }
    // Spread missing positions evenly.
    let mut i = 0;
    while i < n {
        if pos[i].is_none() {
            let a = i - 1;
            let mut b = i;
            while pos[b].is_none() {
                b += 1;
            }
            let (pa, pb) = (pos[a].unwrap_or(0.0), pos[b].unwrap_or(len));
            for (k, slot) in pos.iter_mut().enumerate().take(b).skip(a + 1) {
                *slot = Some(pa + (pb - pa) * (k - a) as f32 / (b - a) as f32);
            }
            i = b;
        }
        i += 1;
    }
    // Colors, with transition hints expanded into samples.
    let mut out: Vec<(f32, Color)> = Vec::new();
    for i in 0..n {
        let p = pos[i].unwrap_or(0.0);
        match stops[i].color {
            Some(c) => out.push((p, c)),
            None => {
                let (Some(&(pa, ca)), Some(cb), Some(pb)) = (
                    out.last(),
                    stops.get(i + 1).and_then(|x| x.color),
                    pos.get(i + 1).copied().flatten(),
                ) else {
                    continue;
                };
                if pb <= pa {
                    continue;
                }
                let h = ((p - pa) / (pb - pa)).clamp(0.001, 0.999);
                let e = 0.5f32.ln() / h.ln();
                for k in 1..10 {
                    let t = k as f32 / 10.0;
                    out.push((pa + (pb - pa) * t, mix(ca, cb, t.powf(e))));
                }
            }
        }
    }
    // CSS interpolates in premultiplied space, tiny-skia does not: add
    // samples between stops whose alpha differs.
    let mut pm: Vec<(f32, Color)> = Vec::with_capacity(out.len());
    for (i, &(p, c)) in out.iter().enumerate() {
        if let Some(&(pp, pc)) = i.checked_sub(1).and_then(|j| out.get(j))
            && (pc.a - c.a).abs() > 0.01
            && p > pp
        {
            for k in 1..16 {
                let t = k as f32 / 16.0;
                pm.push((pp + (p - pp) * t, mix_premul(pc, c, t)));
            }
        }
        pm.push((p, c));
    }
    let out = pm;
    let p0 = out.first()?.0;
    let mut p1 = out.last()?.0;
    if p1 - p0 < 0.01 {
        if repeating {
            // A zero-length repeating gradient paints its average; use the
            // last color.
            let c = out.last()?.1;
            return Some((
                0.0,
                1.0,
                vec![
                    GradientStop::new(0.0, sk_color(c)),
                    GradientStop::new(1.0, sk_color(c)),
                ],
            ));
        }
        p1 = p0 + 0.01;
    }
    let span = p1 - p0;
    let gs = out
        .iter()
        .map(|(p, c)| GradientStop::new((p - p0) / span, sk_color(*c)))
        .collect();
    Some((p0, p1, gs))
}

// ---------------------------------------------------------------------------
// Box shadows
// ---------------------------------------------------------------------------

/// Three box blurs approximating a Gaussian with `sigma`, on an alpha
/// buffer.
fn blur_alpha(a: &mut [u8], w: usize, h: usize, sigma: f32) {
    if sigma < 0.5 || w == 0 || h == 0 {
        return;
    }
    // Box sizes for 3 passes (Kovesi).
    let n = 3.0f32;
    let wi = ((12.0 * sigma * sigma / n) + 1.0).sqrt().floor();
    let wl = if (wi as i32) % 2 == 0 { wi - 1.0 } else { wi };
    let wu = wl + 2.0;
    let m =
        ((12.0 * sigma * sigma - n * wl * wl - 4.0 * n * wl - 3.0 * n) / (-4.0 * wl - 4.0)).round();
    let sizes: Vec<usize> = (0..3)
        .map(|i| {
            if (i as f32) < m {
                wl as usize
            } else {
                wu as usize
            }
        })
        .collect();
    let mut tmp = vec![0u8; a.len()];
    for &size in &sizes {
        let r = size / 2;
        box_pass(a, &mut tmp, w, h, r, true);
        box_pass(&tmp, a, w, h, r, false);
    }
}

fn box_pass(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize, horizontal: bool) {
    let (outer, inner) = if horizontal { (h, w) } else { (w, h) };
    let idx = |o: usize, i: usize| if horizontal { o * w + i } else { i * w + o };
    let d = (2 * r + 1) as u32;
    for o in 0..outer {
        let mut sum: u32 = 0;
        for i in 0..=r.min(inner.saturating_sub(1)) {
            sum += u32::from(src[idx(o, i)]);
        }
        for i in 0..inner {
            dst[idx(o, i)] = ((sum + d / 2) / d) as u8;
            let add = i + r + 1;
            if add < inner {
                sum += u32::from(src[idx(o, add)]);
            }
            if i >= r {
                sum -= u32::from(src[idx(o, i - r)]);
            }
        }
    }
}

/// An alpha buffer of `path` (device coords) inside `area`, blurred.
fn shadow_alpha(
    path: &Path,
    area: Rect,
    sigma: f32,
    invert: bool,
) -> Option<(Vec<u8>, usize, usize)> {
    let (w, h) = (
        area.width.ceil().max(1.0) as u32,
        area.height.ceil().max(1.0) as u32,
    );
    if u64::from(w) * u64::from(h) > MAX_BUF {
        return None;
    }
    let mut m = Mask::new(w, h)?;
    m.fill_path(
        path,
        FillRule::Winding,
        true,
        Transform::from_translate(-area.x, -area.y),
    );
    let mut a = m.take();
    if invert {
        for v in &mut a {
            *v = 255 - *v;
        }
    }
    blur_alpha(&mut a, w as usize, h as usize, sigma);
    Some((a, w as usize, h as usize))
}

fn colorize(a: &[u8], w: usize, h: usize, c: Color) -> Option<Pixmap> {
    let mut p = Pixmap::new(w as u32, h as u32)?;
    let ca = c.a.clamp(0.0, 1.0);
    for (px, &al) in p.pixels_mut().iter_mut().zip(a) {
        let alpha = f32::from(al) / 255.0 * ca;
        let pm = |v: u8| (f32::from(v) * alpha).round() as u8;
        let a8 = (alpha * 255.0).round() as u8;
        if let Some(v) = tiny_skia::PremultipliedColorU8::from_rgba(
            pm(c.r).min(a8),
            pm(c.g).min(a8),
            pm(c.b).min(a8),
            a8,
        ) {
            *px = v;
        }
    }
    Some(p)
}

/// Outer `box-shadow`s of an item (below its background).
pub(crate) fn outer_shadows(
    px: &mut Pixmap,
    item: &DisplayItem,
    ts: Transform,
    mask: Option<&Mask>,
) {
    let shadows = &item.style.box_shadow;
    if shadows.iter().all(|s| s.inset) {
        return;
    }
    let s = ts.sx;
    let radii = used_radii(&item.style, item.rect);
    let border = dev(item.rect, ts);
    let Some(own) = rrect(border, &scale_radii(&radii, s)) else {
        return;
    };
    for sh in shadows.iter().rev().filter(|x| !x.inset) {
        if sh.color.a <= 0.0 {
            continue;
        }
        let spread = sh.spread * s;
        let r = Rect::new(
            border.x + sh.x * s - spread,
            border.y + sh.y * s - spread,
            border.width + 2.0 * spread,
            border.height + 2.0 * spread,
        );
        let Some(shape) = rrect(r, &grow_radii(&scale_radii(&radii, s), spread)) else {
            continue;
        };
        let sigma = sh.blur * s / 2.0;
        let m = (sigma * 3.0).ceil() + 1.0;
        let area = Rect::new(r.x - m, r.y - m, r.width + 2.0 * m, r.height + 2.0 * m);
        let Some((a, w, h)) = shadow_alpha(&shape, area, sigma, false) else {
            continue;
        };
        let Some(mut layer) = colorize(&a, w, h, sh.color) else {
            continue;
        };
        // The shadow never shows under the box itself.
        let clear = Paint {
            blend_mode: BlendMode::Clear,
            anti_alias: true,
            ..Paint::default()
        };
        layer.fill_path(
            &own,
            &clear,
            FillRule::Winding,
            Transform::from_translate(-area.x.floor(), -area.y.floor()),
            None,
        );
        px.draw_pixmap(
            area.x.floor() as i32,
            area.y.floor() as i32,
            layer.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            mask,
        );
    }
}

/// Inset `box-shadow`s (above the background, inside the padding box).
pub(crate) fn inset_shadows(
    px: &mut Pixmap,
    item: &DisplayItem,
    ts: Transform,
    mask: Option<&Mask>,
    width: u32,
    height: u32,
) {
    let shadows = &item.style.box_shadow;
    if !shadows.iter().any(|s| s.inset) {
        return;
    }
    let s = ts.sx;
    let radii = used_radii(&item.style, item.rect);
    let inner = scale_radii(&inner_radii(&radii, item.border), s);
    let pad = dev(item.padding_box(), ts);
    let Some(pad_path) = rrect(pad, &inner) else {
        return;
    };
    let Some(pmask) = clip_mask(mask, &pad_path, width, height) else {
        return;
    };
    for sh in shadows.iter().rev().filter(|x| x.inset) {
        if sh.color.a <= 0.0 {
            continue;
        }
        let spread = sh.spread * s;
        let hole = Rect::new(
            pad.x + sh.x * s + spread,
            pad.y + sh.y * s + spread,
            (pad.width - 2.0 * spread).max(0.0),
            (pad.height - 2.0 * spread).max(0.0),
        );
        let sigma = sh.blur * s / 2.0;
        let m = (sigma * 3.0).ceil() + 1.0;
        let area = Rect::new(
            pad.x - m,
            pad.y - m,
            pad.width + 2.0 * m,
            pad.height + 2.0 * m,
        );
        let hole_path = rrect(hole, &grow_radii(&inner, -spread));
        let (a, w, h) = match hole_path {
            Some(hp) => match shadow_alpha(&hp, area, sigma, true) {
                Some(x) => x,
                None => continue,
            },
            None => {
                let (w, h) = (area.width.ceil() as usize, area.height.ceil() as usize);
                (vec![255u8; w * h], w, h)
            }
        };
        let Some(layer) = colorize(&a, w, h, sh.color) else {
            continue;
        };
        px.draw_pixmap(
            area.x.floor() as i32,
            area.y.floor() as i32,
            layer.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            Some(&pmask),
        );
    }
}

/// Borders of a box with rounded corners: the ring between the outer and
/// inner rounded rects, split between the sides along the corner
/// diagonals. Dashed and dotted sides are drawn solid.
pub(crate) fn rounded_borders(
    px: &mut Pixmap,
    item: &DisplayItem,
    ts: Transform,
    mask: Option<&Mask>,
    width: u32,
    height: u32,
    side_color: &dyn Fn(usize) -> Option<Color>,
) {
    let s = ts.sx;
    let radii = used_radii(&item.style, item.rect);
    let outer_r = dev(item.rect, ts);
    let inner_r = dev(item.padding_box(), ts);
    let (Some(outer), inner) = (
        rrect(outer_r, &scale_radii(&radii, s)),
        rrect(inner_r, &scale_radii(&inner_radii(&radii, item.border), s)),
    ) else {
        return;
    };
    let mut pb = PathBuilder::new();
    pb.push_path(&outer);
    if let Some(i) = &inner {
        pb.push_path(i);
    }
    let Some(ring) = pb.finish() else { return };
    let colors: Vec<Option<Color>> = (0..4).map(side_color).collect();
    if colors.iter().all(|c| *c == colors[0]) {
        if let Some(c) = colors[0] {
            px.fill_path(
                &ring,
                &paint_of(c),
                FillRule::EvenOdd,
                Transform::identity(),
                mask,
            );
        }
        return;
    }
    let Some(mut rmask) = (match mask {
        Some(m) => Some(m.clone()),
        None => Mask::new(width, height).map(|mut m| {
            m.data_mut().fill(255);
            m
        }),
    }) else {
        return;
    };
    rmask.intersect_path(&ring, FillRule::EvenOdd, true, Transform::identity());
    let [t, r, b, l] = item.border.map(|v| v * s);
    let (x0, y0, x1, y1) = (outer_r.x, outer_r.y, outer_r.right(), outer_r.bottom());
    let (w, h) = (outer_r.width, outer_r.height);
    let k = |a: f32, b: f32, len: f32| {
        if a + b > 0.0 {
            (len / (a + b)).min(1e4)
        } else {
            1e4
        }
    };
    let kv = k(t, b, h);
    let kh = k(l, r, w);
    let kk = kv.min(kh);
    let quads: [[(f32, f32); 4]; 4] = [
        [
            (x0, y0),
            (x1, y0),
            (x1 - r * kk, y0 + t * kk),
            (x0 + l * kk, y0 + t * kk),
        ],
        [
            (x1, y0),
            (x1, y1),
            (x1 - r * kk, y1 - b * kk),
            (x1 - r * kk, y0 + t * kk),
        ],
        [
            (x1, y1),
            (x0, y1),
            (x0 + l * kk, y1 - b * kk),
            (x1 - r * kk, y1 - b * kk),
        ],
        [
            (x0, y1),
            (x0, y0),
            (x0 + l * kk, y0 + t * kk),
            (x0 + l * kk, y1 - b * kk),
        ],
    ];
    for (i, q) in quads.iter().enumerate() {
        let Some(c) = colors[i] else { continue };
        let mut pb = PathBuilder::new();
        pb.move_to(q[0].0, q[0].1);
        for p in &q[1..] {
            pb.line_to(p.0, p.1);
        }
        pb.close();
        if let Some(path) = pb.finish() {
            px.fill_path(
                &path,
                &paint_of(c),
                FillRule::Winding,
                Transform::identity(),
                Some(&rmask),
            );
        }
    }
}

/// The padding box rounded rect of an item, as a clip for replaced content
/// (`None` when its corners are square).
pub(crate) fn content_clip(
    item: &DisplayItem,
    ts: Transform,
    mask: Option<&Mask>,
    width: u32,
    height: u32,
) -> Option<Mask> {
    let radii = used_radii(&item.style, item.rect);
    if is_zero(&radii) {
        return None;
    }
    let inner = scale_radii(&inner_radii(&radii, item.border), ts.sx);
    let path = rrect(dev(item.padding_box(), ts), &inner)?;
    clip_mask(mask, &path, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_keeps_mass_and_spreads() {
        let (w, h) = (40, 40);
        let mut a = vec![0u8; w * h];
        for y in 15..25 {
            for x in 15..25 {
                a[y * w + x] = 255;
            }
        }
        let before: u32 = a.iter().map(|&v| u32::from(v)).sum();
        blur_alpha(&mut a, w, h, 3.0);
        let after: u32 = a.iter().map(|&v| u32::from(v)).sum();
        assert!((before as f32 - after as f32).abs() / (before as f32) < 0.05);
        assert!(a[20 * w + 10] > 0 && a[20 * w + 20] < 255);
    }

    #[test]
    fn stops_fix_up_like_css() {
        let st = |c: u8, p: Option<f32>| Stop {
            color: Some(Color::rgb(c, 0, 0)),
            pos: p.map(Lp::Px),
        };
        let (p0, p1, gs) = gradient_stops(
            &[
                st(0, None),
                st(100, Some(30.0)),
                st(200, Some(10.0)),
                st(255, None),
            ],
            100.0,
            1.0,
            false,
        )
        .unwrap();
        assert_eq!((p0, p1), (0.0, 100.0));
        assert_eq!(gs.len(), 4);
    }
}
