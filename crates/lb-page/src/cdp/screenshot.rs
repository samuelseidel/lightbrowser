//! `Page.captureScreenshot`: paints the main document's layout with
//! lb-paint (see `lb_paint::page`), plus `Page.getLayoutMetrics`' scroll
//! and content size.
//!
//! - `clip` is in page coordinates (CSS px from the document origin);
//!   without it the visible viewport at its scroll offset is captured.
//! - Output pixels are CSS px × `clip.scale` × the persona's device pixel
//!   ratio, as in Chrome.
//! - `captureBeyondViewport: false` (the default) paints only what lies in
//!   the visible viewport; the rest of the clip shows the page background.
//!   With `true`, everything in the clip is painted.
//! - `fromSurface` and `optimizeForSpeed` are accepted and have no effect.
//! - Same-origin and cross-origin iframes are painted from their own
//!   layout. Nothing runs in any world and nothing is noised.

use std::rc::Rc;

use lb_cdp::{HostError, HostResult, PageMetrics, Rgba};
use lb_dom::{Document, NodeFlags, NodeId, local_name};
use lb_layout::Rect;
use lb_paint::page::{Capture, ImageFormat, PaintError, ReplacedContent, output_size};
use lb_paint::{FontSet, Pixmap};
use lb_webapi::rt::{Window, frames, screenshot};
use serde_json::Value;

/// Chrome's default JPEG quality for screenshots.
const DEFAULT_QUALITY: u8 = 80;
/// Nested frames painted at most this deep.
const MAX_FRAME_DEPTH: u32 = 8;

/// Parsed `Page.captureScreenshot` parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Request {
    pub format: ImageFormat,
    /// x, y, width, height (CSS px) and scale.
    pub clip: Option<(Rect, f32)>,
    pub beyond_viewport: bool,
}

impl Request {
    pub(crate) fn parse(p: &Value) -> HostResult<Request> {
        let format = p.get("format").and_then(Value::as_str).unwrap_or("png");
        let quality = match p.get("quality") {
            None | Some(Value::Null) => DEFAULT_QUALITY,
            Some(q) => {
                let q = q
                    .as_f64()
                    .ok_or_else(|| HostError::new("Invalid parameters: quality"))?;
                q.round().clamp(0.0, 100.0) as u8
            }
        };
        let format = match format {
            "png" => ImageFormat::Png,
            "jpeg" => ImageFormat::Jpeg(quality),
            "webp" => ImageFormat::Webp,
            _ => return Err(HostError::new("Invalid parameters: format")),
        };
        let clip = match p.get("clip") {
            None | Some(Value::Null) => None,
            Some(c) => {
                let num = |k: &str| -> HostResult<f32> {
                    c.get(k)
                        .and_then(Value::as_f64)
                        .map(|v| v as f32)
                        .ok_or_else(|| HostError::new(format!("Invalid parameters: clip.{k}")))
                };
                let (x, y, w, h) = (num("x")?, num("y")?, num("width")?, num("height")?);
                let scale = c.get("scale").and_then(Value::as_f64).unwrap_or(1.0) as f32;
                if w <= 0.0 {
                    return Err(HostError::new("Cannot take screenshot with 0 width."));
                }
                if h <= 0.0 {
                    return Err(HostError::new("Cannot take screenshot with 0 height."));
                }
                if !(scale > 0.0 && scale.is_finite()) {
                    return Err(HostError::new("Cannot take screenshot with 0 scale."));
                }
                Some((Rect::new(x, y, w, h), scale))
            }
        };
        let beyond_viewport = p
            .get("captureBeyondViewport")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(Request {
            format,
            clip,
            beyond_viewport,
        })
    }
}

/// Scroll offset and document size of `w`'s main document.
pub(crate) fn metrics(w: &Window) -> PageMetrics {
    let ((sx, sy), (cw, ch)) =
        w.with_layout_engine(|l, d| (l.window_scroll(d), l.document_size(d)));
    PageMetrics {
        scroll_x: f64::from(sx),
        scroll_y: f64::from(sy),
        content_width: f64::from(cw),
        content_height: f64::from(ch),
    }
}

/// Captures `w`'s main document; returns the encoded image.
pub(crate) fn capture(w: &Rc<Window>, req: &Request, base: Option<Rgba>) -> HostResult<Vec<u8>> {
    let dpr = w
        .profile
        .as_ref()
        .map_or(1.0, |p| p.screen.device_pixel_ratio as f32);
    let fonts = FontSet::new(w.profile.as_deref());
    let base = base.map(|c| lb_css::Color {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a as f32,
    });
    let list = w.with_layout_engine(|l, d| l.display_list(d));
    let viewport = Rect::new(
        list.scroll.0,
        list.scroll.1,
        list.viewport.0,
        list.viewport.1,
    );
    let (clip, scale) = req.clip.unwrap_or((viewport, 1.0));
    let cap = Capture {
        clip,
        scale: scale * dpr,
        base,
        visible: (!req.beyond_viewport).then_some(viewport),
    };
    let pixmap = paint(w, &list, &cap, &fonts, 0).map_err(|e| HostError::new(e.to_string()))?;
    Ok(lb_paint::encode_screenshot(&pixmap, req.format))
}

fn paint(
    w: &Rc<Window>,
    list: &lb_layout::DisplayList,
    cap: &Capture,
    fonts: &FontSet,
    depth: u32,
) -> Result<Pixmap, PaintError> {
    struct Source<'a> {
        w: &'a Rc<Window>,
        scale: f32,
        fonts: &'a FontSet,
        depth: u32,
    }
    impl lb_paint::page::ReplacedSource for Source<'_> {
        fn content(&self, n: NodeId) -> ReplacedContent {
            content(self.w, n, self.scale, self.fonts, self.depth)
        }
        fn background(&self, url: &str) -> Option<Rc<Pixmap>> {
            self.w.background_image(url)
        }
    }
    let source = Source {
        w,
        scale: cap.scale,
        fonts,
        depth,
    };
    lb_paint::paint_page(list, cap, fonts, &source)
}

/// Starts the background images a capture needs; true while some load.
pub(crate) fn images_pending(w: &Rc<Window>) -> bool {
    w.load_background_images()
}

/// Replaced content of element `n` of `w`'s main document.
fn content(w: &Rc<Window>, n: NodeId, scale: f32, fonts: &FontSet, depth: u32) -> ReplacedContent {
    enum Kind {
        Pixels,
        Frame,
        Input(String),
        TextArea,
        Select,
        Other,
    }
    let kind = w.with_doc(|d| {
        let is = |t| d.is_html_element_named(n, &t);
        if is(local_name!("img")) || is(local_name!("canvas")) {
            Kind::Pixels
        } else if is(local_name!("iframe")) {
            Kind::Frame
        } else if is(local_name!("input")) {
            Kind::Input(
                d.get_attribute(n, "type")
                    .unwrap_or("text")
                    .to_ascii_lowercase(),
            )
        } else if is(local_name!("textarea")) {
            Kind::TextArea
        } else if is(local_name!("select")) {
            Kind::Select
        } else {
            Kind::Other
        }
    });
    match kind {
        Kind::Pixels => {
            screenshot::element_pixels(w, n).map_or(ReplacedContent::None, ReplacedContent::Pixels)
        }
        Kind::Frame => frame(w, n, scale, fonts, depth),
        Kind::Input(ty) => w.with_doc(|d| input(w, d, n, &ty)),
        Kind::TextArea => {
            let v = screenshot::control_value(w, n);
            let first = v.lines().next().unwrap_or("").to_owned();
            text_or_placeholder(w, n, first)
        }
        Kind::Select => w.with_doc(|d| {
            let opts: Vec<NodeId> = d
                .descendants(n)
                .filter(|&o| d.is_html_element_named(o, &local_name!("option")))
                .collect();
            let chosen = opts
                .iter()
                .copied()
                .find(|&o| checked(d, o, "selected"))
                .or_else(|| opts.first().copied());
            let text = chosen
                .and_then(|o| d.text_content(o))
                .unwrap_or_default()
                .split_ascii_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            ReplacedContent::Text {
                text,
                placeholder: false,
                center: false,
            }
        }),
        Kind::Other => ReplacedContent::None,
    }
}

/// Checkedness as lb-webapi keeps it: the dirty flag, else the attribute.
fn checked(d: &Document, n: NodeId, attr: &str) -> bool {
    let f = d.flags(n);
    if f.contains(NodeFlags::CHECKED_DIRTY) {
        f.contains(NodeFlags::CHECKED)
    } else {
        d.has_attribute(n, attr)
    }
}

fn input(w: &Window, d: &Document, n: NodeId, ty: &str) -> ReplacedContent {
    match ty {
        "checkbox" | "radio" => ReplacedContent::Check {
            checked: checked(d, n, "checked"),
            radio: ty == "radio",
        },
        "submit" | "reset" | "button" => {
            let label = d
                .get_attribute(n, "value")
                .map(str::to_owned)
                .unwrap_or_else(|| match ty {
                    "submit" => "Submit".into(),
                    "reset" => "Reset".into(),
                    _ => String::new(),
                });
            ReplacedContent::Text {
                text: label,
                placeholder: false,
                center: true,
            }
        }
        "hidden" | "image" | "file" | "color" | "range" => ReplacedContent::None,
        _ => {
            let value = screenshot::control_value(w, n);
            let shown = if ty == "password" {
                "\u{2022}".repeat(value.chars().count())
            } else {
                value
            };
            placeholder_for(d, n, shown)
        }
    }
}

fn placeholder_for(d: &Document, n: NodeId, value: String) -> ReplacedContent {
    if value.is_empty()
        && let Some(p) = d.get_attribute(n, "placeholder")
    {
        return ReplacedContent::Text {
            text: p.replace(['\r', '\n'], ""),
            placeholder: true,
            center: false,
        };
    }
    ReplacedContent::Text {
        text: value,
        placeholder: false,
        center: false,
    }
}

fn text_or_placeholder(w: &Window, n: NodeId, value: String) -> ReplacedContent {
    w.with_doc(|d| placeholder_for(d, n, value))
}

/// An iframe's document, painted at the parent's device scale.
fn frame(w: &Rc<Window>, n: NodeId, scale: f32, fonts: &FontSet, depth: u32) -> ReplacedContent {
    if depth >= MAX_FRAME_DEPTH {
        return ReplacedContent::None;
    }
    let Some(child) = frames::child_for(w, n) else {
        return ReplacedContent::None;
    };
    let cw = child.window;
    let list = cw.with_layout_engine(|l, d| l.display_list(d));
    let view = Rect::new(
        list.scroll.0,
        list.scroll.1,
        list.viewport.0,
        list.viewport.1,
    );
    let cap = Capture {
        clip: view,
        scale,
        // Transparent: the iframe element's own background shows through.
        base: Some(lb_css::Color::TRANSPARENT),
        visible: None,
    };
    if output_size(&cap).is_err() {
        return ReplacedContent::None;
    }
    paint(&cw, &list, &cap, fonts, depth + 1).map_or(ReplacedContent::None, |p| {
        ReplacedContent::Pixels(Rc::new(p))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_params() {
        let r = Request::parse(&json!({})).unwrap();
        assert_eq!(r.format, ImageFormat::Png);
        assert!(r.clip.is_none() && !r.beyond_viewport);
        let r = Request::parse(&json!({"format": "jpeg", "quality": 55,
            "clip": {"x": 1, "y": 2, "width": 3, "height": 4, "scale": 2},
            "captureBeyondViewport": true, "fromSurface": true}))
        .unwrap();
        assert_eq!(r.format, ImageFormat::Jpeg(55));
        assert_eq!(r.clip, Some((Rect::new(1.0, 2.0, 3.0, 4.0), 2.0)));
        assert!(r.beyond_viewport);
        assert_eq!(
            Request::parse(&json!({"format": "jpeg"})).unwrap().format,
            ImageFormat::Jpeg(80)
        );
        assert!(Request::parse(&json!({"format": "gif"})).is_err());
        let e = Request::parse(&json!({"clip": {"x": 0, "y": 0, "width": 0, "height": 4}}))
            .unwrap_err();
        assert_eq!(e.0, "Cannot take screenshot with 0 width.");
    }
}

// ---------------------------------------------------------------------------
// Page.printToPDF
// ---------------------------------------------------------------------------

/// Parsed `Page.printToPDF` parameters (inches, as in CDP).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PrintRequest {
    pub paper: (f64, f64),
    /// Top, right, bottom, left.
    pub margins: [f64; 4],
    pub scale: f64,
    pub ranges: Option<String>,
}

impl PrintRequest {
    pub(crate) fn parse(p: &Value) -> HostResult<PrintRequest> {
        let num = |k: &str, default: f64| p.get(k).and_then(Value::as_f64).unwrap_or(default);
        // Letter and Chrome's default margins (1 cm).
        let (mut w, mut h) = (num("paperWidth", 8.5), num("paperHeight", 11.0));
        if p.get("landscape").and_then(Value::as_bool) == Some(true) {
            std::mem::swap(&mut w, &mut h);
        }
        let m = |k: &str| num(k, 0.393_700_787_4);
        let margins = [
            m("marginTop"),
            m("marginRight"),
            m("marginBottom"),
            m("marginLeft"),
        ];
        let scale = num("scale", 1.0);
        if !(0.1..=2.0).contains(&scale) {
            return Err(HostError::new("scale is outside of range [0.1, 2]"));
        }
        if w <= 0.0 || h <= 0.0 {
            return Err(HostError::new(
                "invalid print parameters: content area is empty",
            ));
        }
        if margins[1] + margins[3] >= w || margins[0] + margins[2] >= h {
            return Err(HostError::new(
                "invalid print parameters: content area is empty",
            ));
        }
        let ranges = p
            .get("pageRanges")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned);
        Ok(PrintRequest {
            paper: (w, h),
            margins,
            scale,
            ranges,
        })
    }
}

/// The 1-based pages of `ranges` ("1-3, 5, 8-") out of `count`, in order.
fn page_ranges(ranges: &str, count: usize) -> HostResult<Vec<usize>> {
    let invalid = || HostError::new("Page range syntax error");
    let mut out = Vec::new();
    for part in ranges.split(',') {
        let part = part.trim();
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (part, part),
        };
        let parse = |s: &str, d: usize| {
            if s.is_empty() {
                Ok(d)
            } else {
                s.parse::<usize>().map_err(|_| invalid())
            }
        };
        let (from, to) = (parse(a, 1)?, parse(b, count)?);
        if from == 0 || from > to {
            return Err(invalid());
        }
        if from > count {
            return Err(HostError::new("Page range exceeds page count"));
        }
        out.extend(from..=to.min(count));
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Prints `w`'s main document. The page is laid out again at the paper's
/// content width (divided by `scale`), as Chrome does, then restored; the
/// page sees no `resize` and runs nothing meanwhile. Media queries still
/// answer `screen`. Content wider than the paper is scaled down to fit.
/// The layout is cut into page-high slices, painted beyond the viewport
/// and embedded as one JPEG per page, so the PDF looks like the page but
/// its text is not selectable. Backgrounds are kept.
pub(crate) fn print_pdf(w: &Rc<Window>, req: &PrintRequest) -> HostResult<Vec<u8>> {
    const CSS_PER_IN: f64 = 96.0;
    let fonts = FontSet::new(w.profile.as_deref());
    let [mt, mr, mb, ml] = req.margins;
    let content_in = (req.paper.0 - ml - mr, req.paper.1 - mt - mb);
    let saved = w.with_layout_engine(|l, _| l.viewport());
    let print_vp = (
        (content_in.0 * CSS_PER_IN / req.scale) as f32,
        (content_in.1 * CSS_PER_IN / req.scale) as f32,
    );
    w.with_layout_engine(|l, _| l.set_viewport(print_vp.0, print_vp.1));
    let result = print_laid_out(w, req, &fonts, content_in, (mt, ml));
    w.with_layout_engine(|l, _| l.set_viewport(saved.0, saved.1));
    result
}

fn print_laid_out(
    w: &Rc<Window>,
    req: &PrintRequest,
    fonts: &FontSet,
    content_in: (f64, f64),
    (mt, ml): (f64, f64),
) -> HostResult<Vec<u8>> {
    const PT_PER_IN: f64 = 72.0;
    const CSS_PER_IN: f64 = 96.0;
    let list = w.with_layout_engine(|l, d| l.display_list(d));
    let (doc_w, doc_h) = w.with_layout_engine(|l, d| l.document_size(d));
    let (doc_w, doc_h) = (f64::from(doc_w.max(1.0)), f64::from(doc_h.max(1.0)));
    // CSS px of the page across the content width.
    let fit = (content_in.0 * CSS_PER_IN / req.scale / doc_w).min(1.0);
    let slice_h = content_in.1 * CSS_PER_IN / (fit * req.scale);
    let count = ((doc_h / slice_h).ceil() as usize).max(1);
    let pages = match &req.ranges {
        Some(r) => page_ranges(r, count)?,
        None => (1..=count).collect(),
    };
    // One CSS px covers `fit * scale / 96` in of paper: device pixels per
    // CSS px for about 150 dpi.
    let px_scale = (150.0 * fit * req.scale / CSS_PER_IN) as f32;
    let mut images = Vec::with_capacity(pages.len());
    for i in pages {
        let y = (i - 1) as f64 * slice_h;
        let h = slice_h.min(doc_h - y).max(1.0);
        let cap = Capture {
            clip: Rect::new(0.0, y as f32, doc_w as f32, h as f32),
            scale: px_scale.max(0.25),
            base: None,
            visible: None,
        };
        let pixmap = paint(w, &list, &cap, fonts, 0).map_err(|e| HostError::new(e.to_string()))?;
        let jpeg = lb_paint::encode_screenshot(&pixmap, ImageFormat::Jpeg(90));
        // Placement of the slice on the page, in points from the bottom left.
        let draw_w = doc_w * fit * req.scale / CSS_PER_IN * PT_PER_IN;
        let draw_h = h * fit * req.scale / CSS_PER_IN * PT_PER_IN;
        let x0 = ml * PT_PER_IN;
        let y0 = (req.paper.1 - mt) * PT_PER_IN - draw_h;
        images.push((
            jpeg,
            pixmap.width(),
            pixmap.height(),
            [x0, y0, draw_w, draw_h],
        ));
    }
    Ok(pdf(
        &images,
        req.paper.0 * PT_PER_IN,
        req.paper.1 * PT_PER_IN,
    ))
}

/// A PDF with one page per JPEG, each drawn at `[x, y, w, h]` (points).
fn pdf(images: &[(Vec<u8>, u32, u32, [f64; 4])], page_w: f64, page_h: f64) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    let n = images.len();
    // Objects: 1 catalog, 2 pages, then per page: page, content, image.
    let page_obj = |i: usize| 3 + i * 3;
    let obj = |out: &mut Vec<u8>, offsets: &mut Vec<usize>, body: &[u8]| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", offsets.len()).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    obj(&mut out, &mut offsets, b"<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", page_obj(i))).collect();
    obj(
        &mut out,
        &mut offsets,
        format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")).as_bytes(),
    );
    for (i, (jpeg, pw, ph, [x, y, w, h])) in images.iter().enumerate() {
        let (content, image) = (page_obj(i) + 1, page_obj(i) + 2);
        obj(
            &mut out,
            &mut offsets,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {page_w:.2} {page_h:.2}] \
                 /Resources << /XObject << /Im0 {image} 0 R >> >> /Contents {content} 0 R >>"
            )
            .as_bytes(),
        );
        let ops = format!("q {w:.3} 0 0 {h:.3} {x:.3} {y:.3} cm /Im0 Do Q");
        obj(
            &mut out,
            &mut offsets,
            format!("<< /Length {} >>\nstream\n{ops}\nendstream", ops.len()).as_bytes(),
        );
        let mut body = format!(
            "<< /Type /XObject /Subtype /Image /Width {pw} /Height {ph} /ColorSpace /DeviceRGB \
             /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
            jpeg.len()
        )
        .into_bytes();
        body.extend_from_slice(jpeg);
        body.extend_from_slice(b"\nendstream");
        obj(&mut out, &mut offsets, &body);
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes(),
    );
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    out
}

#[cfg(test)]
mod print_tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(page_ranges("1-3, 5", 6).unwrap(), vec![1, 2, 3, 5]);
        assert_eq!(page_ranges("4-", 6).unwrap(), vec![4, 5, 6]);
        assert_eq!(page_ranges("-2", 6).unwrap(), vec![1, 2]);
        assert!(page_ranges("7", 6).is_err());
        assert!(page_ranges("3-1", 6).is_err());
        assert!(page_ranges("x", 6).is_err());
    }

    #[test]
    fn pdf_structure() {
        let doc = pdf(
            &[(vec![0xff, 0xd8, 0xff, 0xd9], 1, 1, [10.0, 20.0, 30.0, 40.0])],
            612.0,
            792.0,
        );
        let s = String::from_utf8_lossy(&doc);
        assert!(s.starts_with("%PDF-1.4"));
        assert!(s.contains("/Count 1"));
        assert!(s.contains("/Filter /DCTDecode"));
        assert!(s.ends_with("%%EOF\n"));
        // The xref offset points at "xref" (in bytes: the header and the
        // JPEG are not UTF-8).
        let at = doc.windows(10).rposition(|w| w == b"startxref\n").unwrap();
        let start: usize = std::str::from_utf8(&doc[at + 10..])
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(doc[start..].starts_with(b"xref"));
    }
}
