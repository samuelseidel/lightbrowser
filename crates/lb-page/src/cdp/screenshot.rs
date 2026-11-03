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
    let source = |n: NodeId| content(w, n, cap.scale, fonts, depth);
    lb_paint::paint_page(list, cap, fonts, &source)
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
