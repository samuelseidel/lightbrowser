//! CSSStyleDeclaration (inline `style` attribute and `getComputedStyle`),
//! geometry (`getBoundingClientRect`, `offset*`, `client*`, `scroll*`,
//! `elementFromPoint`), `matchMedia`, `ResizeObserver` and
//! `IntersectionObserver`, backed by lb-css / lb-layout.

use std::rc::Rc;

use lb_dom::NodeId;
use lb_js::{Arg, CallCtx, Handle, HostError, HostRef, ValueKind};

use super::{
    Dict, K_OBJ, Obj, Runtime, TaskSource, Window, ctor, getter, illegal, method, need_args,
    setter, this_node, this_obj,
};
use crate::registry::Registry;

/// Observed box for a `ResizeObserver` target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoBox {
    Content,
    Border,
    DevicePixelContent,
}

/// One observed element of a `ResizeObserver`.
#[derive(Clone, Debug)]
pub(crate) struct RoTarget {
    slot: u32,
    node: NodeId,
    box_: RoBox,
    /// Last delivered (inline, block); `None` until the first observation.
    last: Option<(f64, f64)>,
}

/// `ResizeObserver` host state.
pub(crate) struct RoData {
    callback: Handle,
    targets: Vec<RoTarget>,
}

/// One `ResizeObserverEntry` (sizes captured when delivered).
#[derive(Clone, Debug)]
pub(crate) struct RoEntry {
    slot: u32,
    target: NodeId,
    /// Content box as `DOMRectReadOnly` `[x, y, w, h]`.
    content_rect: [f64; 4],
    content: (f64, f64),
    border: (f64, f64),
    device_pixel: (f64, f64),
}

/// `IntersectionObserver` host state (viewport root; enough for Turnstile).
pub(crate) struct IoData {
    callback: Handle,
    targets: Vec<(u32, NodeId)>,
    /// Last delivered intersecting flag per target index.
    last: Vec<Option<bool>>,
    root_margin: String,
    thresholds: Vec<f64>,
}

/// One `IntersectionObserverEntry`.
#[derive(Clone, Debug)]
pub(crate) struct IoEntry {
    slot: u32,
    target: NodeId,
    bounding: [f64; 4],
    intersection: [f64; 4],
    root_bounds: [f64; 4],
    ratio: f64,
    is_intersecting: bool,
    time: f64,
}

/// CSS property names recognised by the named getter/setter (Chrome's
/// common set; unknown names stay ordinary JS properties).
const PROPERTIES: &[&str] = &[
    "-webkit-appearance",
    "-webkit-box-orient",
    "-webkit-box-shadow",
    "-webkit-filter",
    "-webkit-font-smoothing",
    "-webkit-line-clamp",
    "-webkit-mask",
    "-webkit-tap-highlight-color",
    "-webkit-text-fill-color",
    "-webkit-text-size-adjust",
    "-webkit-transform",
    "-webkit-transition",
    "-webkit-user-select",
    "align-content",
    "align-items",
    "align-self",
    "all",
    "animation",
    "animation-delay",
    "animation-direction",
    "animation-duration",
    "animation-fill-mode",
    "animation-iteration-count",
    "animation-name",
    "animation-play-state",
    "animation-timing-function",
    "appearance",
    "aspect-ratio",
    "backdrop-filter",
    "backface-visibility",
    "background",
    "background-attachment",
    "background-blend-mode",
    "background-clip",
    "background-color",
    "background-image",
    "background-origin",
    "background-position",
    "background-position-x",
    "background-position-y",
    "background-repeat",
    "background-size",
    "block-size",
    "border",
    "border-block",
    "border-block-end",
    "border-block-start",
    "border-bottom",
    "border-bottom-color",
    "border-bottom-left-radius",
    "border-bottom-right-radius",
    "border-bottom-style",
    "border-bottom-width",
    "border-collapse",
    "border-color",
    "border-image",
    "border-inline",
    "border-inline-end",
    "border-inline-start",
    "border-left",
    "border-left-color",
    "border-left-style",
    "border-left-width",
    "border-radius",
    "border-right",
    "border-right-color",
    "border-right-style",
    "border-right-width",
    "border-spacing",
    "border-style",
    "border-top",
    "border-top-color",
    "border-top-left-radius",
    "border-top-right-radius",
    "border-top-style",
    "border-top-width",
    "border-width",
    "bottom",
    "box-shadow",
    "box-sizing",
    "break-after",
    "break-before",
    "break-inside",
    "caption-side",
    "caret-color",
    "clear",
    "clip",
    "clip-path",
    "color",
    "color-scheme",
    "column-count",
    "column-gap",
    "column-rule",
    "column-span",
    "column-width",
    "columns",
    "contain",
    "container",
    "content",
    "content-visibility",
    "counter-increment",
    "counter-reset",
    "cursor",
    "direction",
    "display",
    "empty-cells",
    "fill",
    "filter",
    "flex",
    "flex-basis",
    "flex-direction",
    "flex-flow",
    "flex-grow",
    "flex-shrink",
    "flex-wrap",
    "float",
    "font",
    "font-family",
    "font-feature-settings",
    "font-kerning",
    "font-size",
    "font-stretch",
    "font-style",
    "font-variant",
    "font-weight",
    "gap",
    "grid",
    "grid-area",
    "grid-auto-columns",
    "grid-auto-flow",
    "grid-auto-rows",
    "grid-column",
    "grid-column-end",
    "grid-column-start",
    "grid-row",
    "grid-row-end",
    "grid-row-start",
    "grid-template",
    "grid-template-areas",
    "grid-template-columns",
    "grid-template-rows",
    "height",
    "hyphens",
    "image-rendering",
    "inline-size",
    "inset",
    "isolation",
    "justify-content",
    "justify-items",
    "justify-self",
    "left",
    "letter-spacing",
    "line-break",
    "line-height",
    "list-style",
    "list-style-image",
    "list-style-position",
    "list-style-type",
    "margin",
    "margin-block",
    "margin-block-end",
    "margin-block-start",
    "margin-bottom",
    "margin-inline",
    "margin-inline-end",
    "margin-inline-start",
    "margin-left",
    "margin-right",
    "margin-top",
    "mask",
    "max-block-size",
    "max-height",
    "max-inline-size",
    "max-width",
    "min-block-size",
    "min-height",
    "min-inline-size",
    "min-width",
    "mix-blend-mode",
    "object-fit",
    "object-position",
    "opacity",
    "order",
    "orphans",
    "outline",
    "outline-color",
    "outline-offset",
    "outline-style",
    "outline-width",
    "overflow",
    "overflow-anchor",
    "overflow-wrap",
    "overflow-x",
    "overflow-y",
    "overscroll-behavior",
    "padding",
    "padding-block",
    "padding-block-end",
    "padding-block-start",
    "padding-bottom",
    "padding-inline",
    "padding-inline-end",
    "padding-inline-start",
    "padding-left",
    "padding-right",
    "padding-top",
    "page-break-after",
    "page-break-before",
    "page-break-inside",
    "perspective",
    "perspective-origin",
    "place-content",
    "place-items",
    "place-self",
    "pointer-events",
    "position",
    "quotes",
    "resize",
    "right",
    "rotate",
    "row-gap",
    "scale",
    "scroll-behavior",
    "scroll-margin",
    "scroll-padding",
    "scroll-snap-align",
    "scroll-snap-type",
    "scrollbar-color",
    "scrollbar-gutter",
    "scrollbar-width",
    "shape-outside",
    "speak",
    "stroke",
    "stroke-width",
    "tab-size",
    "table-layout",
    "text-align",
    "text-align-last",
    "text-decoration",
    "text-decoration-color",
    "text-decoration-line",
    "text-decoration-style",
    "text-indent",
    "text-overflow",
    "text-rendering",
    "text-shadow",
    "text-transform",
    "text-underline-offset",
    "top",
    "touch-action",
    "transform",
    "transform-origin",
    "transform-style",
    "transition",
    "transition-delay",
    "transition-duration",
    "transition-property",
    "transition-timing-function",
    "translate",
    "unicode-bidi",
    "user-select",
    "vertical-align",
    "visibility",
    "white-space",
    "white-space-collapse",
    "widows",
    "width",
    "will-change",
    "word-break",
    "word-spacing",
    "word-wrap",
    "writing-mode",
    "z-index",
    "zoom",
];

/// `backgroundColor` → `background-color`, `webkitTransform` →
/// `-webkit-transform`; `None` for names that are not CSS properties.
pub(crate) fn css_name(js: &str) -> Option<String> {
    let mut out = String::with_capacity(js.len() + 4);
    let body = if let Some(r) = js.strip_prefix("webkit") {
        out.push_str("-webkit");
        r
    } else {
        js
    };
    for c in body.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    if js.contains('-') {
        out = js.to_owned();
    }
    PROPERTIES.binary_search(&out.as_str()).ok().map(|_| out)
}

/// Declarations of a style attribute: `(name, value, important)`.
pub(crate) fn parse_decls(text: &str) -> Vec<(String, String, bool)> {
    let mut out: Vec<(String, String, bool)> = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    let bytes: Vec<(usize, char)> = text.char_indices().collect();
    let mut parts = Vec::new();
    for &(i, c) in &bytes {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, ';') if depth <= 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    for p in parts {
        let Some((name, value)) = p.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let mut value = value.trim().to_owned();
        if name.is_empty() || value.is_empty() {
            continue;
        }
        let mut important = false;
        if let Some(pos) = value.to_ascii_lowercase().rfind("!important") {
            important = true;
            value = value[..pos].trim().to_owned();
        }
        out.retain(|(n, _, _)| *n != name);
        out.push((name, value, important));
    }
    out
}

pub(crate) fn serialize_decls(decls: &[(String, String, bool)]) -> String {
    decls
        .iter()
        .map(|(n, v, i)| format!("{n}: {v}{};", if *i { " !important" } else { "" }))
        .collect::<Vec<_>>()
        .join(" ")
}

/// What a `CSSStyleDeclaration` reads: an element's inline style or
/// computed style, or a CSS rule's declarations.
#[derive(Copy, Clone)]
enum Src {
    Node {
        slot: u32,
        node: NodeId,
        computed: bool,
    },
    Rule {
        sheet: u32,
        uid: u32,
    },
}

impl Src {
    fn computed(self) -> bool {
        matches!(self, Src::Node { computed: true, .. })
    }
}

fn this_style(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<Src, HostError> {
    let id = this_obj(cx)?;
    style_of(w, id).ok_or_else(illegal)
}

fn style_of(w: &Window, id: u32) -> Option<Src> {
    w.with_obj(id, |o| match o {
        Obj::Style {
            slot,
            node,
            computed,
        } => Some(Src::Node {
            slot: *slot,
            node: *node,
            computed: *computed,
        }),
        Obj::RuleStyle { sheet, uid } => Some(Src::Rule {
            sheet: *sheet,
            uid: *uid,
        }),
        _ => None,
    })
    .flatten()
}

fn inline_decls(w: &Window, slot: u32, n: NodeId) -> Vec<(String, String, bool)> {
    w.doc_slot(slot, |d| {
        d.get_attribute(n, "style")
            .map(parse_decls)
            .unwrap_or_default()
    })
}

/// The declarations of a non-computed source.
fn decls(w: &Window, src: Src) -> Vec<(String, String, bool)> {
    match src {
        Src::Node { slot, node, .. } => inline_decls(w, slot, node),
        Src::Rule { sheet, uid } => super::sheets::rule_decls(w, sheet, uid),
    }
}

/// Indexed property names: a rule's longhands, an inline style's names.
fn names(w: &Window, src: Src) -> Vec<String> {
    match src {
        Src::Rule { sheet, uid } => super::sheets::rule_longhands(w, sheet, uid),
        _ => decls(w, src).into_iter().map(|d| d.0).collect(),
    }
}

fn write_decls(w: &Rc<Window>, src: Src, decls: &[(String, String, bool)]) {
    let text = serialize_decls(decls);
    match src {
        Src::Node { slot, node, .. } => {
            let _ = w.doc_slot_mut(slot, |d| d.set_attribute(node, "style", &text));
        }
        Src::Rule { sheet, uid } => super::sheets::set_rule_decls(w, sheet, uid, text),
    }
}

fn get_value(w: &Window, src: Src, name: &str) -> String {
    match src {
        Src::Node {
            slot,
            node,
            computed: true,
        } => w.with_layout(slot, |l, d| l.resolved_style_value(d, node, name)),
        _ => decls(w, src)
            .into_iter()
            .find(|(k, _, _)| k == name)
            .map(|(_, v, _)| v)
            .unwrap_or_default(),
    }
}

fn set_value(w: &Rc<Window>, src: Src, name: &str, value: &str, important: bool) {
    let mut decls = decls(w, src);
    if value.is_empty() {
        decls.retain(|(k, _, _)| k != name);
    } else if let Some(e) = decls.iter_mut().find(|(k, _, _)| k == name) {
        e.1 = value.to_owned();
        e.2 = important;
    } else {
        decls.push((name.to_owned(), value.to_owned(), important));
    }
    write_decls(w, src, &decls);
}

fn readonly_err(w: &Window, op: &str, name: &str) -> HostError {
    w.rt.err(
        "NoModificationAllowedError",
        format!("Failed to execute '{op}' on 'CSSStyleDeclaration': These styles are computed, and therefore the '{name}' property is read-only."),
    )
}

fn rect_obj(w: &Window, r: lb_layout::Rect) -> u32 {
    w.new_obj(Obj::Rect([
        r.x as f64,
        r.y as f64,
        r.width as f64,
        r.height as f64,
    ]))
}

fn elem(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, NodeId), HostError> {
    let (slot, n) = this_node(cx, w)?;
    if !w.doc_slot(slot, |d| d.is_element(n)) {
        return Err(illegal());
    }
    Ok((slot, n))
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    debug_assert!(PROPERTIES.windows(2).all(|p| p[0] < p[1]));
    for iface in ["HTMLElement", "SVGElement", "MathMLElement"] {
        getter(reg, rt, iface, "style", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let id = w.cached_obj((slot, n.0, "style", String::new()), || Obj::Style {
                slot,
                node: n,
                computed: false,
            });
            cx.ret(w.obj_arg(id))
        });
        setter(reg, rt, iface, "style", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let v = cx.arg_string(0)?;
            let _ = w.doc_slot_mut(slot, |d| d.set_attribute(n, "style", &v));
            w.after_mutation(cx);
            Ok(())
        });
    }
    method(reg, rt, "Window", "getComputedStyle", |cx, w| {
        need_args(cx, 1, "getComputedStyle", "Window")?;
        let Some((slot, n)) = cx.arg_host(0).and_then(|h| w.decode_node(h)) else {
            return Err(HostError::type_error(
                "Failed to execute 'getComputedStyle' on 'Window': parameter 1 is not of type 'Element'.",
            ));
        };
        // A removed frame computes no style: an empty declaration (Chrome).
        let id = if super::frames::is_detached(w) {
            let n = w.doc_slot_mut(slot, |d| {
                d.create_element(
                    lb_dom::QualName::new(None, lb_dom::ns!(html), lb_dom::local_name!("div")),
                    Vec::new(),
                )
            });
            w.new_obj(Obj::Style {
                slot,
                node: n,
                computed: false,
            })
        } else {
            w.new_obj(Obj::Style {
                slot,
                node: n,
                computed: true,
            })
        };
        cx.ret(w.obj_arg(id))
    });
    getter(reg, rt, "CSSStyleDeclaration", "cssText", |cx, w| {
        let src = this_style(cx, w)?;
        if src.computed() {
            return cx.ret("");
        }
        cx.ret(serialize_decls(&decls(w, src)))
    });
    setter(reg, rt, "CSSStyleDeclaration", "cssText", |cx, w| {
        let src = this_style(cx, w)?;
        if src.computed() {
            return Err(readonly_err(w, "cssText", "cssText"));
        }
        let v = cx.arg_string(0)?;
        let decls = parse_decls(&v);
        write_decls(w, src, &decls);
        w.after_mutation(cx);
        Ok(())
    });
    getter(reg, rt, "CSSStyleDeclaration", "length", |cx, w| {
        let src = this_style(cx, w)?;
        if src.computed() {
            return cx.ret(lb_css::computed_property_names().len() as u32);
        }
        cx.ret(names(w, src).len() as u32)
    });
    getter(reg, rt, "CSSStyleDeclaration", "parentRule", |cx, _w| {
        cx.ret(Arg::Null)
    });
    getter(reg, rt, "CSSStyleDeclaration", "cssFloat", |cx, w| {
        let src = this_style(cx, w)?;
        cx.ret(get_value(w, src, "float"))
    });
    setter(reg, rt, "CSSStyleDeclaration", "cssFloat", |cx, w| {
        let src = this_style(cx, w)?;
        if src.computed() {
            return Err(readonly_err(w, "cssFloat", "float"));
        }
        let v = cx.arg_string(0)?;
        set_value(w, src, "float", &v, false);
        w.after_mutation(cx);
        Ok(())
    });
    method(
        reg,
        rt,
        "CSSStyleDeclaration",
        "getPropertyValue",
        |cx, w| {
            need_args(cx, 1, "getPropertyValue", "CSSStyleDeclaration")?;
            let src = this_style(cx, w)?;
            let name = cx.arg_string(0)?.to_ascii_lowercase();
            cx.ret(get_value(w, src, &name))
        },
    );
    method(
        reg,
        rt,
        "CSSStyleDeclaration",
        "getPropertyPriority",
        |cx, w| {
            let src = this_style(cx, w)?;
            let name = cx.arg_string(0)?.to_ascii_lowercase();
            if src.computed() {
                return cx.ret("");
            }
            let imp = decls(w, src).into_iter().any(|(k, _, i)| k == name && i);
            cx.ret(if imp { "important" } else { "" })
        },
    );
    method(reg, rt, "CSSStyleDeclaration", "setProperty", |cx, w| {
        need_args(cx, 2, "setProperty", "CSSStyleDeclaration")?;
        let src = this_style(cx, w)?;
        let name = cx.arg_string(0)?;
        if src.computed() {
            return Err(readonly_err(w, "setProperty", &name));
        }
        let value = if cx.arg_kind(1) == lb_js::ValueKind::Null {
            String::new()
        } else {
            cx.arg_string(1)?
        };
        let prio = super::opt_string(cx, 2)?.unwrap_or_default();
        let name = if name.starts_with("--") {
            name
        } else {
            name.to_ascii_lowercase()
        };
        set_value(
            w,
            src,
            &name,
            value.trim(),
            prio.eq_ignore_ascii_case("important"),
        );
        w.after_mutation(cx);
        Ok(())
    });
    method(reg, rt, "CSSStyleDeclaration", "removeProperty", |cx, w| {
        need_args(cx, 1, "removeProperty", "CSSStyleDeclaration")?;
        let src = this_style(cx, w)?;
        let name = cx.arg_string(0)?.to_ascii_lowercase();
        if src.computed() {
            return Err(readonly_err(w, "removeProperty", &name));
        }
        let old = get_value(w, src, &name);
        if !old.is_empty() {
            set_value(w, src, &name, "", false);
            w.after_mutation(cx);
        }
        cx.ret(old)
    });
    method(reg, rt, "CSSStyleDeclaration", "item", |cx, w| {
        let src = this_style(cx, w)?;
        let i = cx.arg_u32(0)? as usize;
        let v = if src.computed() {
            lb_css::computed_property_names()
                .get(i)
                .map(|s| s.to_string())
        } else {
            names(w, src).get(i).cloned()
        };
        cx.ret(v.unwrap_or_default())
    });
    let rt1 = rt.clone();
    reg.indexed_getter(
        "CSSStyleDeclaration",
        Rc::new(move |cx, i| {
            let Some(w) = rt1.window(cx.holder_realm()) else {
                return Ok(None);
            };
            let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
                return Ok(None);
            };
            let Some(src) = style_of(&w, h.id) else {
                return Ok(None);
            };
            Ok(if src.computed() {
                lb_css::computed_property_names()
                    .get(i as usize)
                    .map(|s| Arg::String(s.to_string()))
            } else {
                names(&w, src)
                    .get(i as usize)
                    .map(|d| Arg::String(d.clone()))
            })
        }),
    );
    let rt2 = rt.clone();
    reg.named_getter(
        "CSSStyleDeclaration",
        Rc::new(move |cx, name| {
            let Some(css) = css_name(name) else {
                return Ok(None);
            };
            let Some(w) = rt2.window(cx.holder_realm()) else {
                return Ok(None);
            };
            let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
                return Ok(None);
            };
            let Some(src) = style_of(&w, h.id) else {
                return Ok(None);
            };
            Ok(Some(Arg::String(get_value(&w, src, &css))))
        }),
    );
    let rt3 = rt.clone();
    reg.named_setter(
        "CSSStyleDeclaration",
        Rc::new(move |cx, name, value| {
            let Some(css) = css_name(name) else {
                return Ok(false);
            };
            let Some(w) = rt3.window(cx.holder_realm()) else {
                return Ok(false);
            };
            let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
                return Ok(false);
            };
            let Some(src) = style_of(&w, h.id) else {
                return Ok(false);
            };
            if src.computed() {
                return Err(readonly_err(&w, "setProperty", &css));
            }
            set_value(&w, src, &css, value.trim(), false);
            Ok(true)
        }),
    );

    // Geometry
    method(reg, rt, "Element", "getBoundingClientRect", |cx, w| {
        let (slot, n) = elem(cx, w)?;
        let r = w.with_layout(slot, |l, d| l.bounding_client_rect(d, n));
        cx.ret(w.obj_arg(rect_obj(w, r)))
    });
    method(reg, rt, "Element", "getClientRects", |cx, w| {
        let (slot, n) = elem(cx, w)?;
        let rs = w.with_layout(slot, |l, d| l.client_rects(d, n));
        let id = w.new_obj(Obj::RectList(
            rs.iter()
                .map(|r| [r.x as f64, r.y as f64, r.width as f64, r.height as f64])
                .collect(),
        ));
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, "Element", "scrollIntoView", |cx, w| {
        let (slot, n) = elem(cx, w)?;
        let align = match cx.arg_kind(0) {
            lb_js::ValueKind::Bool if !cx.arg_bool(0) => lb_layout::ScrollAlign::End,
            _ => lb_layout::ScrollAlign::Start,
        };
        w.with_layout(slot, |l, d| l.scroll_into_view(d, n, align, false));
        super::image::schedule_lazy_image_check(w);
        Ok(())
    });
    method(reg, rt, "Element", "scrollIntoViewIfNeeded", |cx, w| {
        let (slot, n) = elem(cx, w)?;
        w.with_layout(slot, |l, d| {
            l.scroll_into_view(d, n, lb_layout::ScrollAlign::Center, true)
        });
        super::image::schedule_lazy_image_check(w);
        Ok(())
    });
    macro_rules! geo {
        ($iface:literal, $name:literal, |$l:ident, $d:ident, $n:ident| $e:expr) => {
            getter(reg, rt, $iface, $name, |cx, w| {
                let (slot, $n) = elem(cx, w)?;
                let v: f32 = w.with_layout(slot, |$l, $d| $e);
                cx.ret(f64::from(v))
            });
        };
    }
    geo!("HTMLElement", "offsetWidth", |l, d, n| l
        .offset_size(d, n)
        .0
        .round());
    geo!("HTMLElement", "offsetHeight", |l, d, n| l
        .offset_size(d, n)
        .1
        .round());
    geo!("HTMLElement", "offsetLeft", |l, d, n| l
        .offset_position(d, n)
        .0
        .round());
    geo!("HTMLElement", "offsetTop", |l, d, n| l
        .offset_position(d, n)
        .1
        .round());
    geo!("Element", "clientWidth", |l, d, n| l
        .client_size(d, n)
        .0
        .round());
    geo!("Element", "clientHeight", |l, d, n| l
        .client_size(d, n)
        .1
        .round());
    geo!("Element", "clientLeft", |l, d, n| l
        .client_offset(d, n)
        .0
        .round());
    geo!("Element", "clientTop", |l, d, n| l
        .client_offset(d, n)
        .1
        .round());
    geo!("Element", "scrollWidth", |l, d, n| l
        .scroll_size(d, n)
        .0
        .round());
    geo!("Element", "scrollHeight", |l, d, n| l
        .scroll_size(d, n)
        .1
        .round());
    geo!("Element", "scrollLeft", |l, d, n| l.scroll_position(d, n).0);
    geo!("Element", "scrollTop", |l, d, n| l.scroll_position(d, n).1);
    getter(reg, rt, "HTMLElement", "offsetParent", |cx, w| {
        let (slot, n) = elem(cx, w)?;
        let p = w.with_layout(slot, |l, d| l.offset_parent(d, n));
        cx.ret(w.opt_node_arg(slot, p))
    });
    for (name, axis) in [("scrollLeft", 0), ("scrollTop", 1)] {
        setter(reg, rt, "Element", name, move |cx, w| {
            let (slot, n) = elem(cx, w)?;
            let v = cx.arg_f64(0)? as f32;
            w.with_layout(slot, |l, d| {
                let (x, y) = l.scroll_position(d, n);
                if axis == 0 {
                    l.set_scroll_position(d, n, v, y)
                } else {
                    l.set_scroll_position(d, n, x, v)
                }
            });
            super::image::schedule_lazy_image_check(w);
            Ok(())
        });
    }
    method(reg, rt, "Document", "elementFromPoint", |cx, w| {
        need_args(cx, 2, "elementFromPoint", "Document")?;
        let (slot, _) = this_node(cx, w)?;
        let (x, y) = (cx.arg_f64(0)? as f32, cx.arg_f64(1)? as f32);
        let r = w.with_layout(slot, |l, d| {
            l.element_from_point(d, x, y)
                .map(|n| super::shadow::retarget_to_document(d, n))
        });
        cx.ret(w.opt_node_arg(slot, r))
    });
    method(reg, rt, "Document", "elementsFromPoint", |cx, w| {
        need_args(cx, 2, "elementsFromPoint", "Document")?;
        let (slot, _) = this_node(cx, w)?;
        let (x, y) = (cx.arg_f64(0)? as f32, cx.arg_f64(1)? as f32);
        let r = w.with_layout(slot, |l, d| {
            let mut out: Vec<NodeId> = Vec::new();
            for n in l.elements_from_point(d, x, y) {
                let t = super::shadow::retarget_to_document(d, n);
                if !out.contains(&t) {
                    out.push(t);
                }
            }
            out
        });
        cx.ret(Arg::Array(
            r.into_iter().map(|n| w.node_arg(slot, n)).collect(),
        ))
    });

    // DOMRect / DOMRectReadOnly / DOMRectList
    fn rect(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<[f64; 4], HostError> {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| match o {
            Obj::Rect(r) => Some(*r),
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)
    }
    for (name, f) in [
        ("x", (|r: [f64; 4]| r[0]) as fn([f64; 4]) -> f64),
        ("y", |r| r[1]),
        ("width", |r| r[2]),
        ("height", |r| r[3]),
        ("top", |r| r[1].min(r[1] + r[3])),
        ("left", |r| r[0].min(r[0] + r[2])),
        ("right", |r| r[0].max(r[0] + r[2])),
        ("bottom", |r| r[1].max(r[1] + r[3])),
    ] {
        getter(reg, rt, "DOMRectReadOnly", name, move |cx, w| {
            let r = rect(cx, w)?;
            cx.ret(f(r))
        });
    }
    for (name, i) in [("x", 0usize), ("y", 1), ("width", 2), ("height", 3)] {
        getter(reg, rt, "DOMRect", name, move |cx, w| {
            let r = rect(cx, w)?;
            cx.ret(r[i])
        });
        setter(reg, rt, "DOMRect", name, move |cx, w| {
            let id = this_obj(cx)?;
            let v = cx.arg_f64(0)?;
            w.with_obj(id, |o| {
                if let Obj::Rect(r) = o {
                    r[i] = v;
                }
            });
            Ok(())
        });
    }
    method(reg, rt, "DOMRectReadOnly", "toJSON", |cx, w| {
        let r = rect(cx, w)?;
        let json = serde_json::json!({
            "x": r[0], "y": r[1], "width": r[2], "height": r[3],
            "top": r[1].min(r[1] + r[3]), "right": r[0].max(r[0] + r[2]),
            "bottom": r[1].max(r[1] + r[3]), "left": r[0].min(r[0] + r[2]),
        });
        cx.ret(Arg::Json(json.to_string()))
    });
    for iface in ["DOMRect", "DOMRectReadOnly"] {
        super::ctor(reg, rt, iface, |cx, w| {
            let mut r = [0.0; 4];
            for (i, v) in r.iter_mut().enumerate() {
                if i < cx.len() && cx.arg_kind(i) != lb_js::ValueKind::Undefined {
                    *v = cx.arg_f64(i)?;
                }
            }
            let id = w.new_obj(Obj::Rect(r));
            cx.init_this(HostRef::new(K_OBJ, id))
        });
    }
    fn rect_list(w: &Window, id: u32) -> Option<Vec<[f64; 4]>> {
        w.with_obj(id, |o| match o {
            Obj::RectList(v) => Some(v.clone()),
            _ => None,
        })
        .flatten()
    }
    getter(reg, rt, "DOMRectList", "length", |cx, w| {
        let id = this_obj(cx)?;
        cx.ret(rect_list(w, id).ok_or_else(illegal)?.len() as u32)
    });
    method(reg, rt, "DOMRectList", "item", |cx, w| {
        let id = this_obj(cx)?;
        let i = cx.arg_u32(0)? as usize;
        match rect_list(w, id).ok_or_else(illegal)?.get(i) {
            Some(r) => {
                let rid = w.new_obj(Obj::Rect(*r));
                cx.ret(w.obj_arg(rid))
            }
            None => cx.ret(Arg::Null),
        }
    });
    let rt4 = rt.clone();
    reg.indexed_getter(
        "DOMRectList",
        Rc::new(move |cx, i| {
            let Some(w) = rt4.window(cx.holder_realm()) else {
                return Ok(None);
            };
            let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
                return Ok(None);
            };
            Ok(rect_list(&w, h.id)
                .and_then(|v| v.get(i as usize).copied())
                .map(|r| w.obj_arg(w.new_obj(Obj::Rect(r)))))
        }),
    );

    // matchMedia
    method(reg, rt, "Window", "matchMedia", |cx, w| {
        need_args(cx, 1, "matchMedia", "Window")?;
        let q = cx.arg_string(0)?;
        let (media, matches) = if super::frames::is_detached(w) {
            let media = lb_css::MediaQueryList::parse(&q).to_css();
            (media, false)
        } else {
            let r = w.with_layout(0, |l, _| l.styles().match_media(&q));
            (r.media, r.matches)
        };
        let id = w.new_obj(Obj::MediaQueryList { media, matches });
        cx.ret(w.obj_arg(id))
    });
    fn mql_id(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u32, HostError> {
        let id = this_obj(cx)?;
        match w.with_obj(id, |o| matches!(o, Obj::MediaQueryList { .. })) {
            Some(true) => Ok(id),
            _ => Err(illegal()),
        }
    }
    fn mql_media(w: &Window, id: u32) -> String {
        w.with_obj(id, |o| match o {
            Obj::MediaQueryList { media, .. } => media.clone(),
            _ => String::new(),
        })
        .unwrap_or_default()
    }
    getter(reg, rt, "MediaQueryList", "media", |cx, w| {
        let id = mql_id(cx, w)?;
        cx.ret(mql_media(w, id))
    });
    getter(reg, rt, "MediaQueryList", "matches", |cx, w| {
        let id = mql_id(cx, w)?;
        if super::frames::is_detached(w) {
            return cx.ret(false);
        }
        let m = mql_media(w, id);
        let matches = w.with_layout(0, |l, _| l.styles().match_media(&m).matches);
        w.with_obj(id, |o| {
            if let Obj::MediaQueryList {
                matches: cached, ..
            } = o
            {
                *cached = matches;
            }
        });
        cx.ret(matches)
    });
    // Legacy aliases for addEventListener/removeEventListener('change', ...).
    method(reg, rt, "MediaQueryList", "addListener", |cx, w| {
        need_args(cx, 1, "addListener", "MediaQueryList")?;
        let id = mql_id(cx, w)?;
        if matches!(
            cx.arg_kind(0),
            lb_js::ValueKind::Undefined | lb_js::ValueKind::Null
        ) {
            return Ok(());
        }
        if !matches!(
            cx.arg_kind(0),
            lb_js::ValueKind::Object | lb_js::ValueKind::Function
        ) {
            return Err(HostError::type_error(
                "Failed to execute 'addListener' on 'MediaQueryList': parameter 1 is not of type 'Object'.",
            ));
        }
        let t = HostRef::new(K_OBJ, id);
        let cb = cx.arg_handle(0);
        let existing: Vec<_> = w
            .listeners
            .borrow()
            .get(&t)
            .map(|l| {
                l.iter()
                    .filter(|l| l.type_ == "change" && !l.capture && !l.removed.get())
                    .filter_map(|l| match &l.kind {
                        super::events::ListenerKind::Callback(h) => Some(*h),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        if existing.into_iter().any(|h| cx.strict_equals(h, cb)) {
            cx.release(cb);
            return Ok(());
        }
        w.listeners
            .borrow_mut()
            .entry(t)
            .or_default()
            .push(std::rc::Rc::new(super::events::Listener {
                type_: "change".into(),
                kind: super::events::ListenerKind::Callback(cb),
                capture: false,
                once: false,
                passive: false,
                removed: std::cell::Cell::new(false),
                realm: cx.realm(),
            }));
        Ok(())
    });
    method(reg, rt, "MediaQueryList", "removeListener", |cx, w| {
        need_args(cx, 1, "removeListener", "MediaQueryList")?;
        let id = mql_id(cx, w)?;
        if !matches!(
            cx.arg_kind(0),
            lb_js::ValueKind::Object | lb_js::ValueKind::Function
        ) {
            return Ok(());
        }
        let t = HostRef::new(K_OBJ, id);
        let cb = cx.arg_handle(0);
        let candidates: Vec<_> = w
            .listeners
            .borrow()
            .get(&t)
            .map(|l| {
                l.iter()
                    .filter(|l| l.type_ == "change" && !l.capture)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        for l in candidates {
            if let super::events::ListenerKind::Callback(h) = &l.kind
                && cx.strict_equals(*h, cb)
            {
                super::events::remove_listener(w, t, &l);
                break;
            }
        }
        cx.release(cb);
        Ok(())
    });

    // ResizeObserver -------------------------------------------------------
    ctor(reg, rt, "ResizeObserver", |cx, w| {
        if cx.arg_kind(0) != ValueKind::Function {
            return Err(HostError::type_error(
                "Failed to construct 'ResizeObserver': parameter 1 is not of type 'Function'.",
            ));
        }
        let cb = cx.arg_handle(0);
        let id = w.new_obj(Obj::ResizeObserver(Box::new(RoData {
            callback: cb,
            targets: Vec::new(),
        })));
        w.resize_observers.borrow_mut().push(id);
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    method(reg, rt, "ResizeObserver", "observe", |cx, w| {
        need_args(cx, 1, "observe", "ResizeObserver")?;
        let id = this_obj(cx)?;
        let Some((slot, node)) = cx.arg_host(0).and_then(|h| w.decode_node(h)) else {
            return Err(HostError::type_error(
                "Failed to execute 'observe' on 'ResizeObserver': parameter 1 is not of type 'Element'.",
            ));
        };
        if !w.doc_slot(slot, |d| d.is_element(node)) {
            return Err(HostError::type_error(
                "Failed to execute 'observe' on 'ResizeObserver': parameter 1 is not of type 'Element'.",
            ));
        }
        let d = Dict::arg(cx, 1);
        let box_ = match d.string(cx, "box")?.as_deref() {
            Some("border-box") => RoBox::Border,
            Some("device-pixel-content-box") => RoBox::DevicePixelContent,
            _ => RoBox::Content,
        };
        d.release(cx);
        let ok = w
            .with_obj(id, |o| {
                if let Obj::ResizeObserver(m) = o {
                    if let Some(t) = m
                        .targets
                        .iter_mut()
                        .find(|t| t.slot == slot && t.node == node)
                    {
                        t.box_ = box_;
                        t.last = None;
                    } else {
                        m.targets.push(RoTarget {
                            slot,
                            node,
                            box_,
                            last: None,
                        });
                    }
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        if !ok {
            return Err(illegal());
        }
        schedule_ro(w);
        Ok(())
    });
    method(reg, rt, "ResizeObserver", "unobserve", |cx, w| {
        need_args(cx, 1, "unobserve", "ResizeObserver")?;
        let id = this_obj(cx)?;
        let Some((slot, node)) = cx.arg_host(0).and_then(|h| w.decode_node(h)) else {
            return Ok(());
        };
        w.with_obj(id, |o| {
            if let Obj::ResizeObserver(m) = o {
                m.targets.retain(|t| !(t.slot == slot && t.node == node));
            }
        });
        Ok(())
    });
    method(reg, rt, "ResizeObserver", "disconnect", |cx, w| {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| {
            if let Obj::ResizeObserver(m) = o {
                m.targets.clear();
            }
        });
        Ok(())
    });

    getter(reg, rt, "ResizeObserverEntry", "target", |cx, w| {
        let (slot, n) = ro_entry(cx, w, |e| (e.slot, e.target))?;
        cx.ret(w.node_arg(slot, n))
    });
    getter(reg, rt, "ResizeObserverEntry", "contentRect", |cx, w| {
        let r = ro_entry(cx, w, |e| e.content_rect)?;
        cx.ret(w.obj_arg(w.new_obj(Obj::Rect(r))))
    });
    for (name, which) in [
        ("contentBoxSize", 0u8),
        ("borderBoxSize", 1),
        ("devicePixelContentBoxSize", 2),
    ] {
        getter(reg, rt, "ResizeObserverEntry", name, move |cx, w| {
            let (inline, block) = ro_entry(cx, w, |e| match which {
                0 => e.content,
                1 => e.border,
                _ => e.device_pixel,
            })?;
            let size = w.obj_arg(w.new_obj(Obj::ResizeObserverSize { inline, block }));
            let arr = Arg::Array(vec![size]);
            if let Some(freeze) = w.helpers.borrow().freeze {
                let h = cx.make_handle(arr)?;
                let _ = cx.try_call(freeze, Arg::Undefined, &[Arg::Handle(h)]);
                cx.ret(Arg::Handle(h))
            } else {
                cx.ret(arr)
            }
        });
    }
    getter(reg, rt, "ResizeObserverSize", "inlineSize", |cx, w| {
        let v = ro_size(cx, w, |i, _| i)?;
        cx.ret(v)
    });
    getter(reg, rt, "ResizeObserverSize", "blockSize", |cx, w| {
        let v = ro_size(cx, w, |_, b| b)?;
        cx.ret(v)
    });

    // IntersectionObserver -------------------------------------------------
    ctor(reg, rt, "IntersectionObserver", |cx, w| {
        if cx.arg_kind(0) != ValueKind::Function {
            return Err(HostError::type_error(
                "Failed to construct 'IntersectionObserver': parameter 1 is not of type 'Function'.",
            ));
        }
        let cb = cx.arg_handle(0);
        let d = Dict::arg(cx, 1);
        let root_margin = d.string(cx, "rootMargin")?.unwrap_or_else(|| "0px".into());
        let thresholds = match d.get(cx, "threshold")? {
            Some(h) => {
                let v = cx.handle_value(h);
                cx.release(h);
                match v {
                    lb_js::JsValue::Number(n) => vec![n.clamp(0.0, 1.0)],
                    lb_js::JsValue::Json(s) => serde_json::from_str::<Vec<f64>>(&s)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|n| n.clamp(0.0, 1.0))
                        .collect(),
                    _ => vec![0.0],
                }
            }
            None => vec![0.0],
        };
        let thresholds = if thresholds.is_empty() {
            vec![0.0]
        } else {
            thresholds
        };
        d.release(cx);
        let id = w.new_obj(Obj::IntersectionObserver(Box::new(IoData {
            callback: cb,
            targets: Vec::new(),
            last: Vec::new(),
            root_margin,
            thresholds,
        })));
        w.intersection_observers.borrow_mut().push(id);
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    method(reg, rt, "IntersectionObserver", "observe", |cx, w| {
        need_args(cx, 1, "observe", "IntersectionObserver")?;
        let id = this_obj(cx)?;
        let Some((slot, node)) = cx.arg_host(0).and_then(|h| w.decode_node(h)) else {
            return Err(HostError::type_error(
                "Failed to execute 'observe' on 'IntersectionObserver': parameter 1 is not of type 'Element'.",
            ));
        };
        if !w.doc_slot(slot, |d| d.is_element(node)) {
            return Err(HostError::type_error(
                "Failed to execute 'observe' on 'IntersectionObserver': parameter 1 is not of type 'Element'.",
            ));
        }
        let ok = w
            .with_obj(id, |o| {
                if let Obj::IntersectionObserver(m) = o {
                    if !m.targets.iter().any(|(s, n)| *s == slot && *n == node) {
                        m.targets.push((slot, node));
                        m.last.push(None);
                    }
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        if !ok {
            return Err(illegal());
        }
        schedule_io(w);
        Ok(())
    });
    method(reg, rt, "IntersectionObserver", "unobserve", |cx, w| {
        need_args(cx, 1, "unobserve", "IntersectionObserver")?;
        let id = this_obj(cx)?;
        let Some((slot, node)) = cx.arg_host(0).and_then(|h| w.decode_node(h)) else {
            return Ok(());
        };
        w.with_obj(id, |o| {
            if let Obj::IntersectionObserver(m) = o
                && let Some(i) = m.targets.iter().position(|(s, n)| *s == slot && *n == node)
            {
                m.targets.remove(i);
                if i < m.last.len() {
                    m.last.remove(i);
                }
            }
        });
        Ok(())
    });
    method(reg, rt, "IntersectionObserver", "disconnect", |cx, w| {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| {
            if let Obj::IntersectionObserver(m) = o {
                m.targets.clear();
                m.last.clear();
            }
        });
        Ok(())
    });
    method(reg, rt, "IntersectionObserver", "takeRecords", |cx, _w| {
        cx.ret(Arg::Array(Vec::new()))
    });
    getter(reg, rt, "IntersectionObserver", "root", |cx, _w| {
        this_obj(cx)?;
        cx.ret(Arg::Null)
    });
    getter(reg, rt, "IntersectionObserver", "rootMargin", |cx, w| {
        let id = this_obj(cx)?;
        let m = w
            .with_obj(id, |o| match o {
                Obj::IntersectionObserver(m) => Some(m.root_margin.clone()),
                _ => None,
            })
            .flatten()
            .ok_or_else(illegal)?;
        cx.ret(m)
    });
    getter(reg, rt, "IntersectionObserver", "thresholds", |cx, w| {
        let id = this_obj(cx)?;
        let t = w
            .with_obj(id, |o| match o {
                Obj::IntersectionObserver(m) => Some(m.thresholds.clone()),
                _ => None,
            })
            .flatten()
            .ok_or_else(illegal)?;
        cx.ret(Arg::Array(t.into_iter().map(Arg::Number).collect()))
    });
    getter(reg, rt, "IntersectionObserverEntry", "target", |cx, w| {
        let (slot, n) = io_entry(cx, w, |e| (e.slot, e.target))?;
        cx.ret(w.node_arg(slot, n))
    });
    getter(
        reg,
        rt,
        "IntersectionObserverEntry",
        "isIntersecting",
        |cx, w| {
            let v = io_entry(cx, w, |e| e.is_intersecting)?;
            cx.ret(v)
        },
    );
    getter(
        reg,
        rt,
        "IntersectionObserverEntry",
        "intersectionRatio",
        |cx, w| {
            let v = io_entry(cx, w, |e| e.ratio)?;
            cx.ret(v)
        },
    );
    getter(reg, rt, "IntersectionObserverEntry", "time", |cx, w| {
        let v = io_entry(cx, w, |e| e.time)?;
        cx.ret(v)
    });
    for (name, which) in [
        ("boundingClientRect", 0u8),
        ("intersectionRect", 1),
        ("rootBounds", 2),
    ] {
        getter(reg, rt, "IntersectionObserverEntry", name, move |cx, w| {
            let r = io_entry(cx, w, |e| match which {
                0 => e.bounding,
                1 => e.intersection,
                _ => e.root_bounds,
            })?;
            cx.ret(w.obj_arg(w.new_obj(Obj::Rect(r))))
        });
    }
}

fn ro_entry<R>(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    f: impl FnOnce(&RoEntry) -> R,
) -> Result<R, HostError> {
    let id = this_obj(cx)?;
    w.with_obj(id, |o| match o {
        Obj::ResizeObserverEntry(e) => Some(f(e)),
        _ => None,
    })
    .flatten()
    .ok_or_else(illegal)
}

fn ro_size<R>(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    f: impl FnOnce(f64, f64) -> R,
) -> Result<R, HostError> {
    let id = this_obj(cx)?;
    w.with_obj(id, |o| match o {
        Obj::ResizeObserverSize { inline, block } => Some(f(*inline, *block)),
        _ => None,
    })
    .flatten()
    .ok_or_else(illegal)
}

fn schedule_ro(w: &Rc<Window>) {
    if w.ro_queued.get() {
        return;
    }
    w.ro_queued.set(true);
    w.queue_task(
        TaskSource::DomManipulation,
        Box::new(|js, w| deliver_resize_observations(js, w)),
    );
}

fn io_entry<R>(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    f: impl FnOnce(&IoEntry) -> R,
) -> Result<R, HostError> {
    let id = this_obj(cx)?;
    w.with_obj(id, |o| match o {
        Obj::IntersectionObserverEntry(e) => Some(f(e)),
        _ => None,
    })
    .flatten()
    .ok_or_else(illegal)
}

fn schedule_io(w: &Rc<Window>) {
    if w.io_queued.get() {
        return;
    }
    w.io_queued.set(true);
    w.queue_task(
        TaskSource::DomManipulation,
        Box::new(|js, w| deliver_intersection_observations(js, w)),
    );
}

/// Deliver `IntersectionObserver` notifications (viewport root). First
/// observe reports whether the target's box intersects the viewport.
pub(crate) fn deliver_intersection_observations(js: &mut dyn super::Js, w: &Rc<Window>) {
    w.io_queued.set(false);
    let vp = super::window::screen_dims(w);
    let root = [
        0.0,
        0.0,
        f64::from(vp.inner_width),
        f64::from(vp.inner_height),
    ];
    let observers = w.intersection_observers.borrow().clone();
    let now = w.clock.now();
    for ob in observers {
        let targets = w
            .with_obj(ob, |o| match o {
                Obj::IntersectionObserver(m) => m.targets.clone(),
                _ => Vec::new(),
            })
            .unwrap_or_default();
        if targets.is_empty() {
            continue;
        }
        let mut entries: Vec<u32> = Vec::new();
        let mut updates: Vec<(usize, bool)> = Vec::new();
        for (i, (slot, node)) in targets.iter().enumerate() {
            let r = w.with_layout(*slot, |l, d| l.bounding_client_rect(d, *node));
            let bounding = [
                f64::from(r.x),
                f64::from(r.y),
                f64::from(r.width),
                f64::from(r.height),
            ];
            let left = bounding[0].max(root[0]);
            let top = bounding[1].max(root[1]);
            let right = (bounding[0] + bounding[2]).min(root[0] + root[2]);
            let bottom = (bounding[1] + bounding[3]).min(root[1] + root[3]);
            let iw = (right - left).max(0.0);
            let ih = (bottom - top).max(0.0);
            let area = bounding[2] * bounding[3];
            let ratio = if area > 0.0 {
                (iw * ih / area).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let is_intersecting = ratio > 0.0 || (bounding[2] == 0.0 && bounding[3] == 0.0);
            // Zero-size targets still count as intersecting when in view
            // for first paint of hidden Turnstile hosts.
            let is_intersecting = if bounding[2] == 0.0 && bounding[3] == 0.0 {
                bounding[0] < root[2] && bounding[1] < root[3]
            } else {
                is_intersecting
            };
            let prev = w
                .with_obj(ob, |o| match o {
                    Obj::IntersectionObserver(m) => m.last.get(i).copied().flatten(),
                    _ => None,
                })
                .flatten();
            if prev == Some(is_intersecting) {
                continue;
            }
            updates.push((i, is_intersecting));
            let eid = w.new_obj(Obj::IntersectionObserverEntry(Box::new(IoEntry {
                slot: *slot,
                target: *node,
                bounding,
                intersection: [left, top, iw, ih],
                root_bounds: root,
                ratio: if is_intersecting && ratio == 0.0 {
                    1.0
                } else {
                    ratio
                },
                is_intersecting,
                time: now,
            })));
            entries.push(eid);
        }
        if entries.is_empty() {
            continue;
        }
        w.with_obj(ob, |o| {
            if let Obj::IntersectionObserver(m) = o {
                for (i, v) in updates {
                    if let Some(slot) = m.last.get_mut(i) {
                        *slot = Some(v);
                    }
                }
            }
        });
        let cb = w
            .with_obj(ob, |o| match o {
                Obj::IntersectionObserver(m) => Some(m.callback),
                _ => None,
            })
            .flatten();
        let Some(cb) = cb else { continue };
        let arr = Arg::Array(entries.into_iter().map(|e| w.obj_arg(e)).collect());
        let me = w.obj_arg(ob);
        if let Err(e) = js.call(cb, me.clone(), &[arr, me]) {
            super::events::report_exception(js, w, &e);
        }
    }
}

/// Deliver pending `ResizeObserver` observations (first observe and size
/// changes). Queued as a DOM task so Turnstile (and similar) see a callback
/// after `observe()` returns.
pub(crate) fn deliver_resize_observations(js: &mut dyn super::Js, w: &Rc<Window>) {
    w.ro_queued.set(false);
    let dpr = super::window::screen_dims(w).device_pixel_ratio;
    let observers = w.resize_observers.borrow().clone();
    for ob in observers {
        let targets = w
            .with_obj(ob, |o| match o {
                Obj::ResizeObserver(m) => m.targets.clone(),
                _ => Vec::new(),
            })
            .unwrap_or_default();
        if targets.is_empty() {
            continue;
        }
        let mut entries: Vec<u32> = Vec::new();
        let mut updates: Vec<(usize, (f64, f64))> = Vec::new();
        for (i, t) in targets.iter().enumerate() {
            let (cw, ch) = w.with_layout(t.slot, |l, d| {
                let (w, h) = l.client_size(d, t.node);
                (f64::from(w), f64::from(h))
            });
            let (bw, bh) = w.with_layout(t.slot, |l, d| {
                let r = l.bounding_client_rect(d, t.node);
                (f64::from(r.width), f64::from(r.height))
            });
            let content = (cw, ch);
            let border = (bw, bh);
            let device_pixel = (cw * dpr, ch * dpr);
            let reported = match t.box_ {
                RoBox::Content => content,
                RoBox::Border => border,
                RoBox::DevicePixelContent => device_pixel,
            };
            if t.last == Some(reported) {
                continue;
            }
            updates.push((i, reported));
            let eid = w.new_obj(Obj::ResizeObserverEntry(Box::new(RoEntry {
                slot: t.slot,
                target: t.node,
                content_rect: [0.0, 0.0, content.0, content.1],
                content,
                border,
                device_pixel,
            })));
            entries.push(eid);
        }
        if entries.is_empty() {
            continue;
        }
        w.with_obj(ob, |o| {
            if let Obj::ResizeObserver(m) = o {
                for (i, reported) in updates {
                    if let Some(t) = m.targets.get_mut(i) {
                        t.last = Some(reported);
                    }
                }
            }
        });
        let cb = w
            .with_obj(ob, |o| match o {
                Obj::ResizeObserver(m) => Some(m.callback),
                _ => None,
            })
            .flatten();
        let Some(cb) = cb else { continue };
        let arr = Arg::Array(entries.into_iter().map(|e| w.obj_arg(e)).collect());
        let me = w.obj_arg(ob);
        if let Err(e) = js.call(cb, me.clone(), &[arr, me]) {
            super::events::report_exception(js, w, &e);
        }
    }
}

/// Re-evaluate every `MediaQueryList` of `w` and fire `change` when
/// `matches` flips (CDP media emulation and viewport resize).
pub fn notify_media_lists(js: &mut dyn super::Js, w: &Rc<Window>) {
    if super::frames::is_detached(w) {
        return;
    }
    let ids: Vec<u32> = w
        .objs
        .borrow()
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match o {
            Some(Obj::MediaQueryList { .. }) => Some(i as u32),
            _ => None,
        })
        .collect();
    for id in ids {
        let media = w
            .with_obj(id, |o| match o {
                Obj::MediaQueryList { media, .. } => Some(media.clone()),
                _ => None,
            })
            .flatten();
        let Some(media) = media else {
            continue;
        };
        let matches = w.with_layout(0, |l, _| l.styles().match_media(&media).matches);
        let changed = w
            .with_obj(id, |o| match o {
                Obj::MediaQueryList {
                    matches: cached, ..
                } => {
                    if *cached == matches {
                        false
                    } else {
                        *cached = matches;
                        true
                    }
                }
                _ => false,
            })
            .unwrap_or(false);
        if !changed {
            continue;
        }
        super::events::fire_event_with(
            js,
            w,
            HostRef::new(K_OBJ, id),
            "MediaQueryListEvent",
            "change",
            false,
            false,
            super::events::EventExtra::MediaQuery {
                media: media.clone(),
                matches,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_table_sorted() {
        assert!(
            PROPERTIES.windows(2).all(|p| p[0] < p[1]),
            "PROPERTIES must be sorted"
        );
        assert_eq!(
            css_name("backgroundColor").as_deref(),
            Some("background-color")
        );
        assert_eq!(
            css_name("webkitTransform").as_deref(),
            Some("-webkit-transform")
        );
        assert_eq!(css_name("color").as_deref(), Some("color"));
        assert_eq!(css_name("cssText"), None);
        assert_eq!(css_name("foo"), None);
    }

    #[test]
    fn declarations() {
        let d = parse_decls("color: red; background: url(a;b) ;width:1px !important; color: blue");
        assert_eq!(
            d,
            vec![
                ("background".into(), "url(a;b)".into(), false),
                ("width".into(), "1px".into(), true),
                ("color".into(), "blue".into(), false),
            ]
        );
        assert_eq!(
            serialize_decls(&d),
            "background: url(a;b); width: 1px !important; color: blue;"
        );
    }
}
