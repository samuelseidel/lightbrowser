//! `<img>` / `Image` loading: requests go through the page's network
//! host as `Destination::Image` (data: URLs are decoded in place), the
//! response is probed for its size with lb-paint (PNG, JPEG, GIF, WebP,
//! ICO/BMP; SVG sized only) and decoded on first `drawImage`.
//!
//! - Triggers: parser or script insertion of an `img` with `src`/`srcset`,
//!   and changes of `src`, `srcset`, `sizes` or `crossorigin` (via the
//!   mutation queue, so `img.src = x` starts loading synchronously).
//! - `srcset`: density (`x`) and width (`w`, against the viewport width)
//!   descriptors, picked for the persona's device pixel ratio. No
//!   `<picture>`.
//! - Events: `load` / `error` in a networking task; the loads of the main
//!   document delay its `load` event. `decode()` settles with them.
//! - `loading=lazy`: the image loads once it is within 1250px of the
//!   viewport when a check runs (after insertion, and on
//!   [`load_visible_lazy_images`]); it never delays `load`.
//! - Layout gets the natural size of loaded images.
//! - Canvas taint: an image is origin-clean when same-origin, a data: URL,
//!   or requested with `crossorigin` and answered with a matching
//!   `Access-Control-Allow-Origin`.

use std::collections::HashSet;
use std::rc::Rc;

use lb_dom::{Mutation, NodeId, local_name, ns};
use lb_js::{Arg, Resolver};
use lb_paint::Pixmap;
use lb_paint::decode::{self, ImageInfo};
use url::Url;

use super::canvas::Source;
use super::{
    Js, Runtime, TaskSource, Window, fire_event, getter, method, node_ref, setter, this_node,
};
use crate::host::{Destination, NetRequest, NetResponse};
use crate::registry::Registry;

/// Load state of one image element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// No source.
    Empty,
    Loading,
    Loaded,
    Broken,
}

pub(crate) struct ImgState {
    url: Option<Url>,
    seq: u32,
    status: Status,
    info: Option<ImageInfo>,
    bytes: Option<Rc<Vec<u8>>>,
    content_type: Option<String>,
    decoded: Option<Rc<Pixmap>>,
    cors_ok: bool,
    waiters: Vec<Resolver>,
    /// Waiting for the viewport (`loading=lazy`).
    lazy: bool,
}

impl ImgState {
    fn new() -> ImgState {
        ImgState {
            url: None,
            seq: 0,
            status: Status::Empty,
            info: None,
            bytes: None,
            content_type: None,
            decoded: None,
            cors_ok: true,
            waiters: Vec::new(),
            lazy: false,
        }
    }
}

fn with_state<R>(w: &Window, n: NodeId, f: impl FnOnce(&mut ImgState) -> R) -> R {
    let mut m = w.paint.images.borrow_mut();
    f(m.entry((0, n)).or_insert_with(ImgState::new))
}

fn is_img(w: &Window, n: NodeId) -> bool {
    w.with_doc(|d| d.is_html_element_named(n, &local_name!("img")))
}

/// Image elements a mutation may need to (re)load. `true` = an attribute
/// changed (always restart).
pub(crate) fn candidates(w: &Window, m: &Mutation, out: &mut Vec<(NodeId, bool)>) {
    match m {
        Mutation::ChildInserted { node, .. } => w.with_doc(|d| {
            for n in d.inclusive_descendants(*node) {
                if d.is_html_element_named(n, &local_name!("img"))
                    && (d.get_attribute(n, "src").is_some()
                        || d.get_attribute(n, "srcset").is_some())
                {
                    out.push((n, false));
                }
            }
        }),
        Mutation::Attribute { element, name, .. }
            if name.ns == ns!()
                && matches!(&*name.local, "src" | "srcset" | "sizes" | "crossorigin")
                && is_img(w, *element) =>
        {
            out.push((*element, true));
        }
        _ => {}
    }
}

/// Starts or restarts loads for the [`candidates`] of a mutation batch.
pub(crate) fn process(js: &mut dyn Js, w: &Rc<Window>, nodes: Vec<(NodeId, bool)>) {
    let mut seen = HashSet::new();
    let mut lazy_check = false;
    for (n, forced) in nodes.iter().rev() {
        if !seen.insert(*n) {
            continue;
        }
        lazy_check |= update(
            js,
            w,
            *n,
            *forced || !w.paint.images.borrow().contains_key(&(0, *n)),
        );
    }
    if lazy_check {
        let w2 = w.clone();
        w.queue_task(
            TaskSource::DomManipulation,
            Box::new(move |_js, _w| load_visible_lazy_images(&w2)),
        );
    }
}

/// The URL `srcset` / `src` select (spec "select an image source", no
/// `<picture>`). `Some("")` for an empty `src`.
fn selected_source(w: &Window, n: NodeId) -> Option<String> {
    let (src, srcset) = w.with_doc(|d| {
        (
            d.get_attribute(n, "src").map(str::to_owned),
            d.get_attribute(n, "srcset").map(str::to_owned),
        )
    });
    let dpr = w
        .profile
        .as_ref()
        .map_or(1.0, |p| p.screen.device_pixel_ratio);
    let vw = w
        .profile
        .as_ref()
        .map_or(1280.0, |p| f64::from(p.screen.inner_width.max(1)));
    let mut cands: Vec<(String, f64)> = srcset
        .as_deref()
        .map(|s| parse_srcset(s, vw))
        .unwrap_or_default();
    if let Some(s) = &src
        && !s.is_empty()
        && !cands.iter().any(|(_, d)| *d == 1.0)
    {
        cands.push((s.clone(), 1.0));
    }
    if cands.is_empty() {
        return src;
    }
    cands.sort_by(|a, b| a.1.total_cmp(&b.1));
    cands
        .iter()
        .find(|(_, d)| *d >= dpr)
        .or(cands.last())
        .map(|(u, _)| u.clone())
}

/// `srcset` candidates as (URL, density).
fn parse_srcset(s: &str, viewport: f64) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b',') {
            i += 1;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut url = &s[start..i];
        let mut descs = String::new();
        if url.ends_with(',') {
            url = url.trim_end_matches(',');
        } else {
            let mut depth = 0;
            while i < b.len() {
                match b[i] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    b',' if depth <= 0 => break,
                    _ => {}
                }
                descs.push(b[i] as char);
                i += 1;
            }
        }
        if url.is_empty() {
            continue;
        }
        let mut density = 1.0;
        let mut ok = true;
        for d in descs.split_ascii_whitespace() {
            let (num, unit) = d.split_at(d.len().saturating_sub(1));
            match (unit, num.parse::<f64>()) {
                ("x", Ok(v)) if v > 0.0 => density = v,
                ("w", Ok(v)) if v > 0.0 => density = v / viewport,
                ("h", Ok(_)) => {}
                _ => ok = false,
            }
        }
        if ok {
            out.push((url.to_owned(), density));
        }
    }
    out
}

fn is_lazy(w: &Window, n: NodeId) -> bool {
    w.with_doc(|d| {
        d.get_attribute(n, "loading")
            .is_some_and(|v| v.eq_ignore_ascii_case("lazy"))
    })
}

/// "Update the image data". Returns true when the image waits for a lazy
/// check.
fn update(js: &mut dyn Js, w: &Rc<Window>, n: NodeId, forced: bool) -> bool {
    let src = selected_source(w, n);
    let url = src
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|s| w.resolve_url(s));
    let (same, status) = with_state(w, n, |st| (st.url == url && url.is_some(), st.status));
    if same && !forced && status != Status::Empty {
        return false;
    }
    let seq = with_state(w, n, |st| {
        st.seq = st.seq.wrapping_add(1);
        st.lazy = false;
        st.seq
    });
    match (src, url) {
        (None, _) => {
            with_state(w, n, |st| {
                st.url = None;
                st.status = Status::Empty;
                st.info = None;
                st.bytes = None;
                st.decoded = None;
            });
            set_layout_size(w, n, None);
            false
        }
        (Some(_), None) => {
            // Empty or unparsable URL: broken, `error` fires.
            with_state(w, n, |st| {
                st.url = None;
                st.status = Status::Broken;
                st.info = None;
                st.bytes = None;
                st.decoded = None;
            });
            set_layout_size(w, n, None);
            let w2 = w.clone();
            w.queue_task(
                TaskSource::DomManipulation,
                Box::new(move |js, _| settle(js, &w2, n, seq, false, false)),
            );
            false
        }
        (Some(_), Some(url)) => {
            if same && status == Status::Loaded {
                // Same URL again: the memory cache answers.
                let w2 = w.clone();
                w.pending_loads.set(w.pending_loads.get() + 1);
                w.queue_task(
                    TaskSource::Networking,
                    Box::new(move |js, _| settle(js, &w2, n, seq, true, true)),
                );
                return false;
            }
            with_state(w, n, |st| {
                st.url = Some(url.clone());
                st.status = Status::Loading;
                st.info = None;
                st.bytes = None;
                st.decoded = None;
            });
            if is_lazy(w, n) && url.scheme() != "data" {
                with_state(w, n, |st| st.lazy = true);
                return true;
            }
            let _ = js;
            fetch(w, n, url, seq, true);
            false
        }
    }
}

/// Starts lazy images that are now near the viewport (within 1250px).
/// Call after scrolling or viewport changes.
pub fn load_visible_lazy_images(w: &Rc<Window>) {
    let waiting: Vec<(NodeId, Url, u32)> = w
        .paint
        .images
        .borrow()
        .iter()
        .filter(|(_, st)| st.lazy && st.status == Status::Loading)
        .filter_map(|((_, n), st)| Some((*n, st.url.clone()?, st.seq)))
        .collect();
    for (n, url, seq) in waiting {
        let near = w.with_doc(|d| d.is_connected(n))
            && w.with_layout(0, |le, d| {
                let r = le.bounding_client_rect(d, n);
                let (vw, vh) = le.viewport();
                const MARGIN: f32 = 1250.0;
                r.bottom() >= -MARGIN
                    && r.y <= vh + MARGIN
                    && r.right() >= -MARGIN
                    && r.x <= vw + MARGIN
            });
        if near {
            with_state(w, n, |st| st.lazy = false);
            fetch(w, n, url, seq, false);
        }
    }
}

fn data_url(url: &Url) -> Option<(Vec<u8>, String)> {
    let rest = url.as_str().strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let base64 = meta.to_ascii_lowercase().ends_with(";base64");
    let mime = meta.split(';').next().unwrap_or("").trim().to_owned();
    let raw = percent_encoding::percent_decode_str(data).collect::<Vec<u8>>();
    let bytes = if base64 {
        lb_paint::encode::base64_decode(&String::from_utf8_lossy(&raw))?
    } else {
        raw
    };
    Some((bytes, mime))
}

fn fetch(w: &Rc<Window>, n: NodeId, url: Url, seq: u32, delay_load: bool) {
    if delay_load {
        w.pending_loads.set(w.pending_loads.get() + 1);
    }
    let done =
        move |js: &mut dyn Js, w: &Rc<Window>, r: Result<(Vec<u8>, Option<String>, bool), ()>| {
            let ok = match r {
                Ok((bytes, ct, cors_ok)) => {
                    let probed = decode::probe(&bytes, ct.as_deref());
                    let current = with_state(w, n, |st| st.seq == seq);
                    if current {
                        with_state(w, n, |st| {
                            st.cors_ok = cors_ok;
                            match probed {
                                Ok(info) => {
                                    st.info = Some(info);
                                    st.bytes = Some(Rc::new(bytes));
                                    st.content_type = ct;
                                    true
                                }
                                Err(_) => false,
                            }
                        })
                    } else {
                        false
                    }
                }
                Err(()) => false,
            };
            settle(js, w, n, seq, ok, delay_load);
        };
    if url.scheme() == "data" {
        let r = data_url(&url).map(|(b, m)| (b, Some(m), true)).ok_or(());
        let w2 = w.clone();
        w.queue_task(
            TaskSource::Networking,
            Box::new(move |js, _| done(js, &w2, r)),
        );
        return;
    }
    let cors = w.with_doc(|d| d.get_attribute(n, "crossorigin").is_some());
    let mut req = NetRequest::get(url.clone(), Destination::Image, Some(w.url()));
    if cors {
        req.mode = "cors";
    }
    let origin = w.url().origin();
    super::net::start_request(
        w,
        req,
        Box::new(move |js, w, r: Result<NetResponse, String>| {
            let r = r
                .map(|resp| {
                    let same = resp.url.origin() == origin;
                    let acao = resp.header("access-control-allow-origin").is_some_and(|v| {
                        v.trim() == "*" || v.trim() == origin.ascii_serialization()
                    });
                    let ct = resp.header("content-type").map(str::to_owned);
                    (resp.body, ct, same || (cors && acao))
                })
                .map_err(|_| ());
            done(js, w, r)
        }),
    );
}

/// Finishes a load: state, layout size, `load`/`error`, `decode()`.
fn settle(js: &mut dyn Js, w: &Rc<Window>, n: NodeId, seq: u32, ok: bool, counted: bool) {
    let current = with_state(w, n, |st| st.seq == seq);
    if current {
        let (size, waiters) = with_state(w, n, |st| {
            st.status = if ok { Status::Loaded } else { Status::Broken };
            if !ok {
                st.info = None;
                st.bytes = None;
                st.decoded = None;
            }
            (
                st.info
                    .filter(|_| ok)
                    .map(|i| (i.width as f32, i.height as f32)),
                std::mem::take(&mut st.waiters),
            )
        });
        set_layout_size(w, n, size);
        fire_event(
            js,
            w,
            node_ref(0, n),
            if ok { "load" } else { "error" },
            false,
            false,
        );
        for r in waiters {
            if ok {
                js.resolve(r, Arg::Undefined);
            } else {
                js.reject(r, decode_error(&w.rt));
            }
        }
    }
    if counted {
        w.pending_loads.set(w.pending_loads.get().saturating_sub(1));
        w.wake();
    }
}

fn decode_error(rt: &Runtime) -> lb_js::HostError {
    rt.err("EncodingError", "The source image cannot be decoded.")
}

fn set_layout_size(w: &Window, n: NodeId, size: Option<(f32, f32)>) {
    if let Some(le) = w.layout.borrow_mut().as_mut() {
        le.set_intrinsic_size(n, size);
    }
}

/// Gives a newly created layout engine the natural sizes of loaded images.
pub(crate) fn apply_intrinsic(w: &Window, le: &mut lb_layout::LayoutEngine) {
    for ((slot, n), st) in w.paint.images.borrow().iter() {
        if *slot == 0
            && st.status == Status::Loaded
            && let Some(i) = st.info
        {
            le.set_intrinsic_size(*n, Some((i.width as f32, i.height as f32)));
        }
    }
}

/// Pixels of an image for drawing. `Err` when the image is broken.
pub(crate) fn pixels(w: &Window, slot: u32, n: NodeId) -> Result<Source, ()> {
    let mut m = w.paint.images.borrow_mut();
    let Some(st) = m.get_mut(&(slot, n)) else {
        return Ok(Source::Nothing);
    };
    match st.status {
        Status::Empty | Status::Loading => Ok(Source::Nothing),
        Status::Broken => Err(()),
        Status::Loaded => {
            if st.info.is_some_and(|i| i.vector) {
                return Ok(Source::Nothing);
            }
            if st.decoded.is_none() {
                let bytes = st.bytes.clone().ok_or(())?;
                match decode::decode(&bytes, st.content_type.as_deref()) {
                    Ok(p) => st.decoded = Some(Rc::new(p)),
                    Err(_) => return Err(()),
                }
            }
            Ok(st
                .decoded
                .clone()
                .map_or(Source::Nothing, |p| Source::Pixels(p, st.cors_ok)))
        }
    }
}

fn state_of(w: &Window, slot: u32, n: NodeId) -> (Status, Option<ImageInfo>, Option<Url>, bool) {
    w.paint
        .images
        .borrow()
        .get(&(slot, n))
        .map_or((Status::Empty, None, None, false), |st| {
            (st.status, st.info, st.url.clone(), st.lazy)
        })
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    const I: &str = "HTMLImageElement";
    getter(reg, rt, I, "complete", |cx, w| {
        let (slot, n) = this_node(cx)?;
        let (status, _, _, lazy) = state_of(w, slot, n);
        cx.ret(!lazy && status != Status::Loading)
    });
    for (k, name) in ["naturalWidth", "naturalHeight"].into_iter().enumerate() {
        getter(reg, rt, I, name, move |cx, w| {
            let (slot, n) = this_node(cx)?;
            let (status, info, _, _) = state_of(w, slot, n);
            let v = match (status, info) {
                (Status::Loaded, Some(i)) => {
                    if k == 0 {
                        i.width
                    } else {
                        i.height
                    }
                }
                _ => 0,
            };
            cx.ret(v)
        });
    }
    for (k, name) in ["width", "height"].into_iter().enumerate() {
        getter(reg, rt, I, name, move |cx, w| {
            let (slot, n) = this_node(cx)?;
            let rendered = (slot == 0 && w.with_doc(|d| d.is_connected(n)))
                .then(|| {
                    w.with_layout(0, |le, d| {
                        le.computed_style(d, n)
                            .is_some_and(|s| s.display != lb_css::Display::None)
                            .then(|| le.box_model(d, n))
                            .flatten()
                    })
                })
                .flatten();
            if let Some(b) = rendered {
                let v = if k == 0 {
                    b.content.width
                } else {
                    b.content.height
                };
                return cx.ret(v.max(0.0).round() as u32);
            }
            let attr = w.doc_slot(slot, |d| {
                d.get_attribute(n, name)
                    .and_then(super::dom::parse_int)
                    .filter(|v| *v >= 0)
            });
            if let Some(a) = attr {
                return cx.ret(a as u32);
            }
            let (status, info, _, _) = state_of(w, slot, n);
            let v = match (status, info) {
                (Status::Loaded, Some(i)) => {
                    if k == 0 {
                        i.width
                    } else {
                        i.height
                    }
                }
                _ => 0,
            };
            cx.ret(v)
        });
    }
    getter(reg, rt, I, "currentSrc", |cx, w| {
        let (slot, n) = this_node(cx)?;
        let (_, _, url, _) = state_of(w, slot, n);
        cx.ret(url.map(|u| u.to_string()).unwrap_or_default())
    });
    method(reg, rt, I, "decode", |cx, w| {
        let (slot, n) = this_node(cx)?;
        let r = cx.ret_new_promise()?;
        let (status, _, _, _) = state_of(w, slot, n);
        match status {
            Status::Loaded => cx.resolve_promise(r, Arg::Undefined),
            Status::Loading if slot == 0 => {
                with_state(w, n, |st| st.waiters.push(r));
                Ok(())
            }
            _ => cx.reject_promise(r, decode_error(&w.rt)),
        }
    });
    // Enumerated attributes not in the generic reflection table.
    for (name, attr, values, missing) in [
        ("loading", "loading", &["lazy", "eager"][..], "auto"),
        (
            "decoding",
            "decoding",
            &["sync", "async", "auto"][..],
            "auto",
        ),
    ] {
        getter(reg, rt, I, name, move |cx, w| {
            let (slot, n) = this_node(cx)?;
            let v = w.doc_slot(slot, |d| {
                d.get_attribute(n, attr).map(str::to_ascii_lowercase)
            });
            cx.ret(match v {
                Some(v) if values.contains(&v.as_str()) => v,
                _ => missing.to_owned(),
            })
        });
        setter(reg, rt, I, name, move |cx, w| {
            let (slot, n) = this_node(cx)?;
            let v = cx.arg_string(0)?;
            let _ = w.doc_slot_mut(slot, |d| d.set_attribute(n, attr, &v));
            w.after_mutation(cx);
            Ok(())
        });
    }
    getter(reg, rt, I, "crossOrigin", |cx, w| {
        let (slot, n) = this_node(cx)?;
        let v = w.doc_slot(slot, |d| {
            d.get_attribute(n, "crossorigin")
                .map(str::to_ascii_lowercase)
        });
        cx.ret(match v {
            None => Arg::Null,
            Some(v) if v == "use-credentials" => Arg::from("use-credentials"),
            Some(_) => Arg::from("anonymous"),
        })
    });
    setter(reg, rt, I, "crossOrigin", |cx, w| {
        let (slot, n) = this_node(cx)?;
        if cx.arg_kind(0) == lb_js::ValueKind::Null {
            w.doc_slot_mut(slot, |d| d.remove_attribute(n, "crossorigin"));
        } else {
            let v = cx.arg_string(0)?;
            let _ = w.doc_slot_mut(slot, |d| d.set_attribute(n, "crossorigin", &v));
        }
        w.after_mutation(cx);
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srcset_candidates() {
        // Commas inside a URL belong to it (HTML's srcset parser).
        let c = parse_srcset("a.png 1x, b.png 2x,c.png 300w, d,e.png 1.5x, f.png,", 600.0);
        let want = [
            ("a.png", 1.0),
            ("b.png", 2.0),
            ("c.png", 0.5),
            ("d,e.png", 1.5),
            ("f.png", 1.0),
        ];
        assert_eq!(c, want.map(|(u, d)| (u.to_owned(), d)).to_vec());
        assert!(parse_srcset("x.png 2q", 100.0).is_empty());
    }

    #[test]
    fn data_urls() {
        let u = Url::parse("data:image/png;base64,iVBORw0K").unwrap();
        let (b, m) = data_url(&u).unwrap();
        assert_eq!(m, "image/png");
        assert_eq!(&b[..4], b"\x89PNG");
        let u = Url::parse("data:image/svg+xml,%3Csvg%20width='3'%20height='4'/%3E").unwrap();
        let (b, m) = data_url(&u).unwrap();
        assert_eq!(m, "image/svg+xml");
        assert_eq!(decode::probe(&b, Some(&m)).unwrap().width, 3);
    }
}
