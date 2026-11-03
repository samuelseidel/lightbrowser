//! Page screenshots: paints an lb-layout [`DisplayList`] with tiny-skia
//! (CDP `Page.captureScreenshot`).
//!
//! What is painted, per box in paint order:
//! - background colors; the root element's background (or the body's,
//!   when the root has none) covers the whole canvas, over the base color
//!   (white, or `Emulation.setDefaultBackgroundColorOverride`);
//! - borders: solid, double, dashed and dotted; inset, outset, groove and
//!   ridge are drawn solid with Chrome's darker top-left or bottom-right;
//! - text lines from layout, with the bundled glyph outlines (the same
//!   advances as layout and canvas text), in the element's `color`;
//! - replaced content the host supplies ([`ReplacedSource`]): image and
//!   canvas pixels scaled to the content box, form control text, check
//!   boxes and radio buttons;
//! - `opacity` groups, `overflow` clips and `visibility`.
//!
//! Not painted: CSS background images and gradients, border radius,
//! shadows, outlines, text decoration, transforms, filters, scrollbars.
//!
//! The result is never noised: a screenshot is not a page-visible readback.

use std::collections::HashMap;
use std::rc::Rc;

use lb_css::{BorderStyle, Color, Visibility};
use lb_dom::NodeId;
use lb_layout::{DisplayItem, DisplayKind, DisplayList, DisplayStep, Rect, TextFont};
use tiny_skia::{
    FillRule, FilterQuality, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Stroke, StrokeDash,
    Transform,
};

use crate::encode::{Bitmap, encode};
use crate::font::{FontSet, TextAlign, TextBaseline, TextStyle};

/// Largest screenshot side in device pixels (Chrome's limit).
pub const MAX_SIDE: u32 = 16384;
/// Largest screenshot area in device pixels (200 MB of RGBA).
pub const MAX_AREA: u64 = 50_000_000;

/// Content of a replaced element, supplied by the host.
#[derive(Clone, Debug, Default)]
pub enum ReplacedContent {
    /// Nothing beyond background and borders.
    #[default]
    None,
    /// Premultiplied pixels drawn over the content box (images, canvas).
    Pixels(Rc<Pixmap>),
    /// One line of text in the content box (text inputs, select, buttons).
    Text {
        text: String,
        /// A placeholder: drawn in Chrome's placeholder gray.
        placeholder: bool,
        /// Centered horizontally (button-like inputs).
        center: bool,
    },
    /// A check box or radio button.
    Check { checked: bool, radio: bool },
}

/// Supplies replaced element content to the painter.
pub trait ReplacedSource {
    fn content(&self, node: NodeId) -> ReplacedContent;
}

impl<F: Fn(NodeId) -> ReplacedContent> ReplacedSource for F {
    fn content(&self, node: NodeId) -> ReplacedContent {
        self(node)
    }
}

/// What to capture.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Capture {
    /// Area in page coordinates (CSS px, see [`DisplayList`]).
    pub clip: Rect,
    /// Device pixels per CSS px (CDP clip scale times device pixel ratio).
    pub scale: f32,
    /// Base color under the root background (default white).
    pub base: Option<Color>,
    /// Only boxes inside this page rect are painted (the visible viewport
    /// for CDP `captureBeyondViewport: false`); `None` paints everything.
    pub visible: Option<Rect>,
}

/// Why a capture failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaintError {
    /// The clip is empty or not finite.
    EmptyClip,
    /// The image would exceed [`MAX_SIDE`] or [`MAX_AREA`].
    TooLarge { width: u64, height: u64 },
}

impl std::fmt::Display for PaintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PaintError::EmptyClip => {
                f.write_str("Clipped area is either empty or outside the page")
            }
            PaintError::TooLarge { width, height } => {
                write!(f, "Screenshot of {width}x{height} pixels is too large")
            }
        }
    }
}

impl std::error::Error for PaintError {}

/// Screenshot encodings.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    /// Quality 0..=100.
    Jpeg(u8),
    /// Lossless WebP.
    Webp,
}

/// Output size in device pixels for a capture.
pub fn output_size(c: &Capture) -> Result<(u32, u32), PaintError> {
    let (w, h) = (
        (f64::from(c.clip.width) * f64::from(c.scale)).round(),
        (f64::from(c.clip.height) * f64::from(c.scale)).round(),
    );
    if !(w.is_finite() && h.is_finite())
        || w < 1.0
        || h < 1.0
        || !c.clip.x.is_finite()
        || !c.clip.y.is_finite()
    {
        return Err(PaintError::EmptyClip);
    }
    let (wu, hu) = (w as u64, h as u64);
    if wu > u64::from(MAX_SIDE) || hu > u64::from(MAX_SIDE) || wu * hu > MAX_AREA {
        return Err(PaintError::TooLarge {
            width: wu,
            height: hu,
        });
    }
    Ok((wu as u32, hu as u32))
}

/// Paints `list` for `capture`.
pub fn paint_page(
    list: &DisplayList,
    capture: &Capture,
    fonts: &FontSet,
    source: &dyn ReplacedSource,
) -> Result<Pixmap, PaintError> {
    let (w, h) = output_size(capture)?;
    let mut base = Pixmap::new(w, h).ok_or(PaintError::TooLarge {
        width: u64::from(w),
        height: u64::from(h),
    })?;
    let s = capture.scale;
    let ts = Transform::from_row(s, 0.0, 0.0, s, -capture.clip.x * s, -capture.clip.y * s);
    let mut p = Painter {
        ts,
        fonts,
        source,
        masks: HashMap::new(),
        width: w,
        height: h,
        canvas_owner: None,
        visible: capture.visible,
    };
    // Canvas background: base color, then the root's (or body's).
    let base_color = capture.base.unwrap_or(Color::WHITE);
    if base_color.a > 0.0 {
        base.fill(skia_color(base_color));
    }
    let bg_of = |n: Option<NodeId>| {
        n.and_then(|n| list.items().find(|i| i.node == n))
            .map(|i| (i.node, i.style.background_color))
    };
    let canvas_bg = match bg_of(list.root) {
        Some((n, c)) if c.a > 0.0 => Some((n, c)),
        _ => bg_of(list.body).filter(|(_, c)| c.a > 0.0),
    };
    if let Some((n, c)) = canvas_bg {
        p.canvas_owner = Some(n);
        let mut paint = Paint::default();
        paint.set_color(skia_color(c));
        if let Some(r) = tiny_skia::Rect::from_xywh(0.0, 0.0, w as f32, h as f32) {
            base.fill_rect(r, &paint, Transform::identity(), None);
        }
    }
    // Opacity groups paint into layers composited on EndGroup.
    let mut layers: Vec<(Pixmap, f32)> = Vec::new();
    for step in &list.steps {
        match step {
            DisplayStep::BeginGroup { opacity } => {
                if let Some(l) = Pixmap::new(w, h) {
                    layers.push((l, *opacity));
                }
            }
            DisplayStep::EndGroup => {
                if let Some((layer, opacity)) = layers.pop() {
                    let target = layers.last_mut().map_or(&mut base, |(l, _)| l);
                    target.draw_pixmap(
                        0,
                        0,
                        layer.as_ref(),
                        &PixmapPaint {
                            opacity,
                            ..PixmapPaint::default()
                        },
                        Transform::identity(),
                        None,
                    );
                }
            }
            DisplayStep::Item(item) => {
                let target = layers.last_mut().map_or(&mut base, |(l, _)| l);
                p.item(target, item);
            }
        }
    }
    Ok(base)
}

/// Encodes a screenshot (unpremultiplied first). JPEG is opaque: the
/// page is composited onto white.
pub fn encode_screenshot(p: &Pixmap, format: ImageFormat) -> Vec<u8> {
    let mut bmp = Bitmap::new(p.width(), p.height());
    let opaque = matches!(format, ImageFormat::Jpeg(_));
    for (dst, px) in bmp.data.chunks_exact_mut(4).zip(p.pixels()) {
        let c = px.demultiply();
        if opaque {
            // Over white.
            let a = u16::from(c.alpha());
            let over = |v: u8| ((u16::from(v) * a + 255 * (255 - a) + 127) / 255) as u8;
            dst.copy_from_slice(&[over(c.red()), over(c.green()), over(c.blue()), 255]);
        } else {
            dst.copy_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
        }
    }
    match format {
        ImageFormat::Png => encode(&bmp, "image/png", None),
        ImageFormat::Jpeg(q) => encode(&bmp, "image/jpeg", Some(f64::from(q.min(100)) / 100.0)),
        ImageFormat::Webp => encode(&bmp, "image/webp", None),
    }
}

fn skia_color(c: Color) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.r, c.g, c.b, (c.a.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn solid(c: Color, aa: bool) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(skia_color(c));
    p.anti_alias = aa;
    p
}

fn shade(c: Color, k: f32) -> Color {
    let f = |v: u8| (f32::from(v) * k).round().clamp(0.0, 255.0) as u8;
    Color {
        r: f(c.r),
        g: f(c.g),
        b: f(c.b),
        a: c.a,
    }
}

struct Painter<'a> {
    ts: Transform,
    fonts: &'a FontSet,
    source: &'a dyn ReplacedSource,
    /// Clip masks by device rect.
    masks: HashMap<[i32; 4], Mask>,
    width: u32,
    height: u32,
    /// The element whose background was painted on the whole canvas.
    canvas_owner: Option<NodeId>,
    /// See [`Capture::visible`].
    visible: Option<Rect>,
}

impl Painter<'_> {
    /// The mask for an item's clip; `Err` when nothing is visible.
    fn mask(&mut self, clip: Option<Rect>, bounds: Rect) -> Result<Option<&Mask>, ()> {
        let Some(c) = clip else { return Ok(None) };
        if c.is_empty() {
            return Err(());
        }
        let inside = bounds.x >= c.x
            && bounds.y >= c.y
            && bounds.right() <= c.right()
            && bounds.bottom() <= c.bottom();
        if inside {
            return Ok(None);
        }
        let s = self.ts.sx;
        let key = [
            ((c.x * s + self.ts.tx).round()) as i32,
            ((c.y * s + self.ts.ty).round()) as i32,
            ((c.right() * s + self.ts.tx).round()) as i32,
            ((c.bottom() * s + self.ts.ty).round()) as i32,
        ];
        if key[2] <= 0 || key[3] <= 0 || key[0] >= self.width as i32 || key[1] >= self.height as i32
        {
            return Err(());
        }
        let (w, h) = (self.width, self.height);
        if let std::collections::hash_map::Entry::Vacant(e) = self.masks.entry(key) {
            let mut m = Mask::new(w, h).ok_or(())?;
            let r = tiny_skia::Rect::from_ltrb(
                key[0] as f32,
                key[1] as f32,
                key[2] as f32,
                key[3] as f32,
            )
            .ok_or(())?;
            m.fill_path(
                &PathBuilder::from_rect(r),
                FillRule::Winding,
                false,
                Transform::identity(),
            );
            e.insert(m);
        }
        Ok(self.masks.get(&key))
    }

    fn item(&mut self, px: &mut Pixmap, item: &DisplayItem) {
        if item.style.visibility != Visibility::Visible {
            return;
        }
        let bounds = match &item.kind {
            // Glyphs may overhang the line box a little.
            DisplayKind::Text { font, .. } => {
                let m = font.size;
                Rect::new(
                    item.rect.x - m,
                    item.rect.y - m,
                    item.rect.width + 2.0 * m,
                    item.rect.height + 2.0 * m,
                )
            }
            _ => item.rect,
        };
        let ts = self.ts;
        let fonts = self.fonts;
        let source = self.source;
        let canvas_owner = self.canvas_owner;
        let clip = match (item.clip, self.visible) {
            (Some(c), Some(v)) => Some(c.intersect(&v)),
            (c, v) => c.or(v),
        };
        let Ok(mask) = self.mask(clip, bounds) else {
            return;
        };
        match &item.kind {
            DisplayKind::Text { font, lines } => {
                let paint = solid(item.style.color, true);
                for line in lines {
                    text(
                        px,
                        fonts,
                        font,
                        &line.text,
                        line.x,
                        line.baseline,
                        &paint,
                        ts,
                        mask,
                    );
                }
            }
            DisplayKind::Element => {
                if canvas_owner != Some(item.node) {
                    background(px, item, ts, mask);
                }
                borders(px, item, ts, mask);
            }
            DisplayKind::Replaced { font } => {
                if canvas_owner != Some(item.node) {
                    background(px, item, ts, mask);
                }
                borders(px, item, ts, mask);
                replaced(px, item, font, source.content(item.node), fonts, ts, mask);
            }
        }
    }
}

fn sk_rect(r: Rect) -> Option<tiny_skia::Rect> {
    tiny_skia::Rect::from_xywh(r.x, r.y, r.width, r.height)
}

fn background(px: &mut Pixmap, item: &DisplayItem, ts: Transform, mask: Option<&Mask>) {
    let c = item.style.background_color;
    if c.a <= 0.0 {
        return;
    }
    if let Some(r) = sk_rect(item.rect) {
        px.fill_rect(r, &solid(c, false), ts, mask);
    }
}

#[allow(clippy::too_many_arguments)]
fn text(
    px: &mut Pixmap,
    fonts: &FontSet,
    font: &TextFont,
    s: &str,
    x: f32,
    baseline: f32,
    paint: &Paint<'_>,
    ts: Transform,
    mask: Option<&Mask>,
) {
    if s.trim().is_empty() {
        return;
    }
    let st = TextStyle {
        font: font.clone(),
        align: TextAlign::Left,
        baseline: TextBaseline::Alphabetic,
        rtl: false,
    };
    let path = fonts.text_path(&st, s, f64::from(x), f64::from(baseline), None, &ts);
    if let Some(p) = path.to_skia() {
        px.fill_path(&p, paint, FillRule::Winding, Transform::identity(), mask);
    }
    let (x, baseline) = (f64::from(x), f64::from(baseline));
    for (path, c) in fonts.emoji_layers(&st, s, x, baseline, None, &ts) {
        if let Some(p) = path.to_skia() {
            let mut paint = paint.clone();
            paint.set_color(tiny_skia::Color::from_rgba8(c.r, c.g, c.b, 255));
            px.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), mask);
        }
    }
}

/// Side colors for 3D border styles: (top, right, bottom, left).
fn side_colors(style: BorderStyle, c: Color) -> [Color; 2] {
    // [top/left, bottom/right]
    let dark = shade(c, 0.66);
    match style {
        BorderStyle::Inset | BorderStyle::Groove => [dark, c],
        BorderStyle::Outset | BorderStyle::Ridge => [c, dark],
        _ => [c, c],
    }
}

fn borders(px: &mut Pixmap, item: &DisplayItem, ts: Transform, mask: Option<&Mask>) {
    let [t, r, b, l] = item.border;
    if t + r + b + l <= 0.0 {
        return;
    }
    let st = &item.style;
    let (x0, y0, x1, y1) = (
        item.rect.x,
        item.rect.y,
        item.rect.right(),
        item.rect.bottom(),
    );
    // Outer and inner quads per side: top, right, bottom, left.
    let sides: [([(f32, f32); 4], f32); 4] = [
        ([(x0, y0), (x1, y0), (x1 - r, y0 + t), (x0 + l, y0 + t)], t),
        ([(x1, y0), (x1, y1), (x1 - r, y1 - b), (x1 - r, y0 + t)], r),
        ([(x1, y1), (x0, y1), (x0 + l, y1 - b), (x1 - r, y1 - b)], b),
        ([(x0, y1), (x0, y0), (x0 + l, y0 + t), (x0 + l, y1 - b)], l),
    ];
    for (i, (quad, width)) in sides.iter().enumerate() {
        let style = st.border_style[i];
        let color = st.border_color[i];
        if *width <= 0.0
            || matches!(style, BorderStyle::None | BorderStyle::Hidden)
            || color.a <= 0.0
        {
            continue;
        }
        let color = side_colors(style, color)[usize::from(i == 1 || i == 2)];
        match style {
            BorderStyle::Dashed | BorderStyle::Dotted => {
                dashed_side(px, i, item.rect, *width, style, color, ts, mask)
            }
            BorderStyle::Double if *width >= 3.0 => {
                // Two stripes, each a third of the width.
                let k = 1.0 / 3.0;
                let lerp = |a: (f32, f32), b: (f32, f32), f: f32| {
                    (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f)
                };
                let [o0, o1, i1, i0] = *quad;
                let outer = [o0, o1, lerp(o1, i1, k), lerp(o0, i0, k)];
                let inner = [lerp(o0, i0, 1.0 - k), lerp(o1, i1, 1.0 - k), i1, i0];
                fill_quad(px, &outer, color, ts, mask);
                fill_quad(px, &inner, color, ts, mask);
            }
            _ => fill_quad(px, quad, color, ts, mask),
        }
    }
}

fn fill_quad(px: &mut Pixmap, q: &[(f32, f32); 4], c: Color, ts: Transform, mask: Option<&Mask>) {
    let mut pb = PathBuilder::new();
    pb.move_to(q[0].0, q[0].1);
    for p in &q[1..] {
        pb.line_to(p.0, p.1);
    }
    pb.close();
    if let Some(path) = pb.finish() {
        px.fill_path(&path, &solid(c, false), FillRule::Winding, ts, mask);
    }
}

#[allow(clippy::too_many_arguments)]
fn dashed_side(
    px: &mut Pixmap,
    side: usize,
    r: Rect,
    w: f32,
    style: BorderStyle,
    c: Color,
    ts: Transform,
    mask: Option<&Mask>,
) {
    let h = w / 2.0;
    let (a, b) = match side {
        0 => ((r.x, r.y + h), (r.right(), r.y + h)),
        1 => ((r.right() - h, r.y), (r.right() - h, r.bottom())),
        2 => ((r.right(), r.bottom() - h), (r.x, r.bottom() - h)),
        _ => ((r.x + h, r.bottom()), (r.x + h, r.y)),
    };
    let mut pb = PathBuilder::new();
    pb.move_to(a.0, a.1);
    pb.line_to(b.0, b.1);
    let Some(path) = pb.finish() else { return };
    let dotted = style == BorderStyle::Dotted;
    let dash = if dotted { [w, w] } else { [w * 3.0, w * 3.0] };
    let stroke = Stroke {
        width: w,
        dash: StrokeDash::new(dash.to_vec(), 0.0),
        ..Stroke::default()
    };
    px.stroke_path(&path, &solid(c, dotted), &stroke, ts, mask);
}

fn replaced(
    px: &mut Pixmap,
    item: &DisplayItem,
    font: &TextFont,
    content: ReplacedContent,
    fonts: &FontSet,
    ts: Transform,
    mask: Option<&Mask>,
) {
    let cb = item.content_box();
    match content {
        ReplacedContent::None => {}
        ReplacedContent::Pixels(img) => {
            if cb.is_empty() || img.width() == 0 || img.height() == 0 {
                return;
            }
            let sx = cb.width / img.width() as f32;
            let sy = cb.height / img.height() as f32;
            let t = ts.pre_translate(cb.x, cb.y).pre_scale(sx, sy);
            // Upscaling pixel art stays crisp only with nearest; Chrome
            // smooths by default.
            px.draw_pixmap(
                0,
                0,
                img.as_ref().as_ref(),
                &PixmapPaint {
                    quality: FilterQuality::Bilinear,
                    ..PixmapPaint::default()
                },
                t,
                mask,
            );
        }
        ReplacedContent::Text {
            text: s,
            placeholder,
            center,
        } => {
            let color = if placeholder {
                Color::rgb(117, 117, 117)
            } else {
                item.style.color
            };
            let lm = font.line_metrics();
            let width = font.measure(&s);
            let x = if center {
                cb.x + (cb.width - width) / 2.0
            } else {
                cb.x
            };
            let baseline = cb.y + (cb.height - (lm.ascent + lm.descent)) / 2.0 + lm.ascent;
            text(
                px,
                fonts,
                font,
                &s,
                x,
                baseline,
                &solid(color, true),
                ts,
                mask,
            );
        }
        ReplacedContent::Check { checked, radio } => {
            let r = item.content_box();
            let side = r.width.min(r.height);
            if side <= 0.0 {
                return;
            }
            let (cx, cy) = (r.x + r.width / 2.0, r.y + r.height / 2.0);
            let half = side / 2.0 - 0.5;
            let blue = Color::rgb(0, 117, 255);
            let gray = Color::rgb(118, 118, 118);
            let shape = |inset: f32| -> Option<tiny_skia::Path> {
                let hh = half - inset;
                if hh <= 0.0 {
                    return None;
                }
                if radio {
                    PathBuilder::from_circle(cx, cy, hh)
                } else {
                    tiny_skia::Rect::from_xywh(cx - hh, cy - hh, 2.0 * hh, 2.0 * hh)
                        .map(PathBuilder::from_rect)
                }
            };
            let fill = |px: &mut Pixmap, path: Option<tiny_skia::Path>, c: Color| {
                if let Some(p) = path {
                    px.fill_path(&p, &solid(c, true), FillRule::Winding, ts, mask);
                }
            };
            let outline = |px: &mut Pixmap, path: Option<tiny_skia::Path>, c: Color| {
                if let Some(p) = path {
                    let stroke = Stroke {
                        width: 1.0,
                        ..Stroke::default()
                    };
                    px.stroke_path(&p, &solid(c, true), &stroke, ts, mask);
                }
            };
            if checked && radio {
                outline(px, shape(0.0), blue);
                fill(px, shape(side * 0.2), blue);
            } else if checked {
                fill(px, shape(-0.5), blue);
                let mut pb = PathBuilder::new();
                pb.move_to(cx - side * 0.28, cy);
                pb.line_to(cx - side * 0.08, cy + side * 0.2);
                pb.line_to(cx + side * 0.3, cy - side * 0.22);
                if let Some(p) = pb.finish() {
                    let stroke = Stroke {
                        width: (side * 0.15).max(1.5),
                        ..Stroke::default()
                    };
                    px.stroke_path(&p, &solid(Color::WHITE, true), &stroke, ts, mask);
                }
            } else {
                fill(px, shape(0.0), Color::WHITE);
                outline(px, shape(0.0), gray);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lb_css::MediaEnv;
    use lb_dom::Document;
    use lb_layout::LayoutEngine;

    fn shot(html: &str, clip: Rect, scale: f32) -> Pixmap {
        let doc = Document::parse_html(html);
        let mut e = LayoutEngine::with_env(MediaEnv {
            viewport_width: 200.0,
            viewport_height: 100.0,
            ..MediaEnv::default()
        });
        let list = e.display_list(&doc);
        let fonts = FontSet::new(None);
        let none = |_: NodeId| ReplacedContent::None;
        paint_page(
            &list,
            &Capture {
                clip,
                scale,
                base: None,
                visible: None,
            },
            &fonts,
            &none,
        )
        .unwrap()
    }

    fn rgba(p: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let c = p.pixel(x, y).unwrap().demultiply();
        [c.red(), c.green(), c.blue(), c.alpha()]
    }

    #[test]
    fn backgrounds_borders_and_scale() {
        let html = "<body style='margin:0;background:rgb(0,0,255)'>\
            <div style='width:50px;height:20px;background:rgb(255,0,0);border:5px solid rgb(0,128,0)'></div>";
        let p = shot(html, Rect::new(0.0, 0.0, 200.0, 100.0), 1.0);
        assert_eq!((p.width(), p.height()), (200, 100));
        assert_eq!(rgba(&p, 2, 2), [0, 128, 0, 255]);
        assert_eq!(rgba(&p, 10, 10), [255, 0, 0, 255]);
        assert_eq!(rgba(&p, 150, 80), [0, 0, 255, 255]);
        let p2 = shot(html, Rect::new(5.0, 5.0, 10.0, 10.0), 2.0);
        assert_eq!((p2.width(), p2.height()), (20, 20));
        assert_eq!(rgba(&p2, 1, 1), [255, 0, 0, 255]);
    }

    #[test]
    fn opacity_clip_and_text() {
        let html = "<body style='margin:0'>\
            <div style='opacity:0.5;width:20px;height:20px;background:rgb(0,0,0)'></div>\
            <div style='overflow:hidden;width:20px;height:20px'><div style='width:100px;height:20px;background:rgb(255,0,0)'></div></div>\
            <p style='margin:0;font-size:40px;color:rgb(0,0,0)'>MMMM</p>";
        let p = shot(html, Rect::new(0.0, 0.0, 200.0, 100.0), 1.0);
        let half = rgba(&p, 10, 10);
        assert!((126..=129).contains(&half[0]), "{half:?}");
        assert_eq!(rgba(&p, 10, 30), [255, 0, 0, 255]);
        assert_eq!(rgba(&p, 50, 30), [255, 255, 255, 255]);
        // Some glyph ink below y=40.
        let dark = (40..p.height())
            .flat_map(|y| (0..p.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| rgba(&p, x, y)[0] < 64)
            .count();
        assert!(dark > 100, "{dark}");
    }

    #[test]
    fn limits_and_encoding() {
        let c = Capture {
            clip: Rect::new(0.0, 0.0, 0.0, 10.0),
            scale: 1.0,
            base: None,
            visible: None,
        };
        assert_eq!(output_size(&c), Err(PaintError::EmptyClip));
        let c = Capture {
            clip: Rect::new(0.0, 0.0, 20000.0, 10.0),
            ..c
        };
        assert!(matches!(output_size(&c), Err(PaintError::TooLarge { .. })));
        let p = shot(
            "<body style='background:rgb(1,2,3)'>",
            Rect::new(0.0, 0.0, 4.0, 4.0),
            1.0,
        );
        let png = encode_screenshot(&p, ImageFormat::Png);
        assert_eq!(&png[1..4], b"PNG");
        let jpg = encode_screenshot(&p, ImageFormat::Jpeg(80));
        assert_eq!(&jpg[..2], &[0xff, 0xd8]);
        let webp = encode_screenshot(&p, ImageFormat::Webp);
        assert_eq!(&webp[8..12], b"WEBP");
    }
}
