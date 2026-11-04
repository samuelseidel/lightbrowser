//! CSSStyleDeclaration (inline `style` attribute and `getComputedStyle`),
//! geometry (`getBoundingClientRect`, `offset*`, `client*`, `scroll*`,
//! `elementFromPoint`) and `matchMedia`, backed by lb-css / lb-layout.

use std::rc::Rc;

use lb_dom::NodeId;
use lb_js::{Arg, CallCtx, HostError, HostRef};

use super::{
    K_OBJ, Obj, Runtime, Window, getter, illegal, method, need_args, setter, this_node, this_obj,
};
use crate::registry::Registry;

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
        Ok(())
    });
    method(reg, rt, "Element", "scrollIntoViewIfNeeded", |cx, w| {
        let (slot, n) = elem(cx, w)?;
        w.with_layout(slot, |l, d| {
            l.scroll_into_view(d, n, lb_layout::ScrollAlign::Center, true)
        });
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
            Ok(())
        });
    }
    method(reg, rt, "Document", "elementFromPoint", |cx, w| {
        need_args(cx, 2, "elementFromPoint", "Document")?;
        let (slot, _) = this_node(cx, w)?;
        let (x, y) = (cx.arg_f64(0)? as f32, cx.arg_f64(1)? as f32);
        let r = w.with_layout(slot, |l, d| l.element_from_point(d, x, y));
        cx.ret(w.opt_node_arg(slot, r))
    });
    method(reg, rt, "Document", "elementsFromPoint", |cx, w| {
        need_args(cx, 2, "elementsFromPoint", "Document")?;
        let (slot, _) = this_node(cx, w)?;
        let (x, y) = (cx.arg_f64(0)? as f32, cx.arg_f64(1)? as f32);
        let r = w.with_layout(slot, |l, d| l.elements_from_point(d, x, y));
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
        let media = w.with_layout(0, |l, _| l.styles().match_media(&q).media);
        let id = w.new_obj(Obj::MediaQueryList { media });
        cx.ret(w.obj_arg(id))
    });
    fn mql(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<String, HostError> {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| match o {
            Obj::MediaQueryList { media } => Some(media.clone()),
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)
    }
    getter(reg, rt, "MediaQueryList", "media", |cx, w| {
        let m = mql(cx, w)?;
        cx.ret(m)
    });
    getter(reg, rt, "MediaQueryList", "matches", |cx, w| {
        let m = mql(cx, w)?;
        if super::frames::is_detached(w) {
            return cx.ret(false);
        }
        cx.ret(w.with_layout(0, |l, _| l.styles().match_media(&m).matches))
    });
    method(reg, rt, "MediaQueryList", "addListener", |cx, w| {
        mql(cx, w)?;
        Ok(())
    });
    method(reg, rt, "MediaQueryList", "removeListener", |cx, w| {
        mql(cx, w)?;
        Ok(())
    });
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
