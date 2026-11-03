//! Canvas 2D on lb-paint: `HTMLCanvasElement` (`getContext`, `toDataURL`,
//! `toBlob`, size semantics), `CanvasRenderingContext2D`,
//! `OffscreenCanvas` + its 2D context, `CanvasGradient`, `CanvasPattern`,
//! `Path2D`, `ImageData` and `TextMetrics`.
//!
//! - Every canvas element with a context has a [`CanvasCtx`] in the
//!   window's [`PaintState`]; the bitmap is an `lb_paint::Canvas2d`
//!   (allocated on first draw). Setting `width`/`height` (or the content
//!   attributes) clears the bitmap and resets the context state.
//! - Readback (`toDataURL`, `toBlob`, `convertToBlob`, `getImageData`)
//!   carries the persona's seeded noise (`lb_paint::noise`).
//! - `getContext('webgl' | 'experimental-webgl' | 'webgl2')` asks a
//!   factory registered with [`Runtime::set_canvas_context_factory`]; with
//!   none it returns null. Other unknown kinds return null.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use lb_dom::NodeId;
use lb_js::{Arg, CallCtx, Handle, HostError, HostRef, ValueKind};
use lb_paint::{
    Bitmap, Canvas2d, CanvasPattern, FillRule, Gradient, PaintEnv, PathData, Pixmap, Repetition,
    Style, TextAlign, TextBaseline, TextMetrics,
};

use super::{
    Dict, K_OBJ, Obj, Runtime, TaskSource, Window, decode_node, getter, illegal, method, need_args,
    setter, this_obj,
};
use crate::registry::Registry;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// What a canvas is: an element or an `OffscreenCanvas` object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanvasTarget {
    Element { slot: u32, node: NodeId },
    Offscreen { obj: u32 },
}

/// A context made by a plugged-in factory (WebGL).
pub struct ExternalContext {
    /// The object `getContext` returns (same object every call).
    pub value: Handle,
    /// Current pixels for `toDataURL`, `toBlob` and `drawImage`
    /// (unpremultiplied RGBA). The factory applies its own noise.
    pub snapshot: Option<Rc<dyn Fn() -> Option<Bitmap>>>,
    /// The canvas was resized (bitmap must be cleared).
    pub resize: Option<Rc<dyn Fn(u32, u32)>>,
    /// Current pixels without readback noise (unpremultiplied RGBA), for
    /// page screenshots, which the page cannot read.
    pub raw: Option<Rc<dyn Fn() -> Option<Bitmap>>>,
}

/// Makes the context for `getContext(kind, attributes)`. `Ok(None)` makes
/// `getContext` return null.
pub type ContextFactory = Rc<
    dyn Fn(
        &mut CallCtx<'_, '_, '_>,
        &Rc<Window>,
        CanvasTarget,
        &str,
        Option<Handle>,
    ) -> Result<Option<ExternalContext>, HostError>,
>;

/// The context a canvas has.
#[derive(Clone)]
pub(crate) enum CanvasCtx {
    TwoD {
        ctx: Rc<RefCell<Canvas2d>>,
        obj: u32,
    },
    External {
        kind: String,
        ctx: Rc<ExternalContext>,
    },
}

/// Per-window paint state.
#[derive(Default)]
pub(crate) struct PaintState {
    env: RefCell<Option<Rc<PaintEnv>>>,
    canvases: RefCell<HashMap<(u32, NodeId), CanvasCtx>>,
    /// Gradient/pattern `Rc` address → object id (`fillStyle` getter).
    ids: RefCell<HashMap<usize, u32>>,
    pub(crate) images: RefCell<HashMap<(u32, NodeId), super::image::ImgState>>,
}

/// Context factories by kind, per runtime.
#[derive(Default)]
pub(crate) struct Factories(pub(crate) RefCell<HashMap<String, ContextFactory>>);

impl Runtime {
    /// Plugs in the context factory for `getContext(kind)` (`webgl`,
    /// `experimental-webgl`, `webgl2`).
    pub fn set_canvas_context_factory(&self, kind: &str, f: ContextFactory) {
        self.0
            .canvas_factories
            .0
            .borrow_mut()
            .insert(kind.to_owned(), f);
    }
}

/// Size of a canvas: content attributes of an element (default 300x150),
/// or the `OffscreenCanvas` size.
pub fn canvas_size(w: &Window, t: CanvasTarget) -> (u32, u32) {
    match t {
        CanvasTarget::Element { slot, node } => w.doc_slot(slot, |d| {
            let get = |a: &str, def: u32| {
                d.get_attribute(node, a)
                    .and_then(super::dom::parse_int)
                    .filter(|v| *v >= 0)
                    .map_or(def, |v| v as u32)
            };
            (get("width", 300), get("height", 150))
        }),
        CanvasTarget::Offscreen { obj } => paint(w, obj, |p| match p {
            PaintObj::Offscreen(o) => Some((o.width, o.height)),
            _ => None,
        })
        .unwrap_or((0, 0)),
    }
}

pub(crate) fn env(w: &Window) -> Rc<PaintEnv> {
    w.paint
        .env
        .borrow_mut()
        .get_or_insert_with(|| Rc::new(PaintEnv::new(w.profile.as_deref())))
        .clone()
}

// ---------------------------------------------------------------------------
// Host objects
// ---------------------------------------------------------------------------

pub(crate) struct Offscreen {
    width: u32,
    height: u32,
    ctx: Option<CanvasCtx>,
}

/// Paint host objects (one `Obj` variant).
pub(crate) enum PaintObj {
    Context {
        target: CanvasTarget,
        ctx: Rc<RefCell<Canvas2d>>,
    },
    Gradient(Rc<Gradient>),
    Pattern(Rc<CanvasPattern>),
    Path(Rc<RefCell<PathData>>),
    ImageData {
        width: u32,
        height: u32,
        data: Handle,
    },
    Metrics(Box<TextMetrics>),
    Offscreen(Box<Offscreen>),
    Matrix(Box<super::matrix::Matrix>),
}

impl PaintObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            PaintObj::Context {
                target: CanvasTarget::Offscreen { .. },
                ..
            } => "OffscreenCanvasRenderingContext2D",
            PaintObj::Context { .. } => "CanvasRenderingContext2D",
            PaintObj::Gradient(_) => "CanvasGradient",
            PaintObj::Pattern(_) => "CanvasPattern",
            PaintObj::Path(_) => "Path2D",
            PaintObj::ImageData { .. } => "ImageData",
            PaintObj::Metrics(_) => "TextMetrics",
            PaintObj::Offscreen(_) => "OffscreenCanvas",
            PaintObj::Matrix(m) => m.iface_name(),
        }
    }
}

/// Runs `f` on paint object `id`.
pub(crate) fn paint<R>(
    w: &Window,
    id: u32,
    f: impl FnOnce(&mut PaintObj) -> Option<R>,
) -> Option<R> {
    w.with_obj(id, |o| match o {
        Obj::Paint(p) => f(p),
        _ => None,
    })
    .flatten()
}

pub(crate) fn new_paint(w: &Window, p: PaintObj) -> u32 {
    w.new_obj(Obj::Paint(Box::new(p)))
}

fn host_paint_id(w: &Window, h: Option<HostRef>) -> Option<u32> {
    let h = h.filter(|h| h.kind == K_OBJ)?;
    paint(w, h.id, |_| Some(h.id))
}

fn this_ctx(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<Rc<RefCell<Canvas2d>>, HostError> {
    let id = this_obj(cx)?;
    let (target, ctx) = paint(w, id, |p| match p {
        PaintObj::Context { target, ctx } => Some((*target, ctx.clone())),
        _ => None,
    })
    .ok_or_else(illegal)?;
    if let CanvasTarget::Element { .. } = target {
        let (cw, ch) = canvas_size(w, target);
        let stale = {
            let c = ctx.borrow();
            c.width() != cw || c.height() != ch
        };
        if stale {
            ctx.borrow_mut().resize(cw, ch);
        }
    }
    Ok(ctx)
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn num(cx: &mut CallCtx<'_, '_, '_>, i: usize) -> Result<f64, HostError> {
    cx.arg_f64(i)
}

fn nums<const N: usize>(cx: &mut CallCtx<'_, '_, '_>, from: usize) -> Result<[f64; N], HostError> {
    let mut out = [0.0; N];
    for (k, v) in out.iter_mut().enumerate() {
        *v = cx.arg_f64(from + k)?;
    }
    Ok(out)
}

fn present(cx: &CallCtx<'_, '_, '_>, i: usize) -> bool {
    i < cx.len() && cx.arg_kind(i) != ValueKind::Undefined
}

/// `[EnforceRange] long`.
fn enforce_long(
    cx: &mut CallCtx<'_, '_, '_>,
    i: usize,
    op: &str,
    iface: &str,
) -> Result<i64, HostError> {
    let v = cx.arg_f64(i)?;
    if !v.is_finite() || v.abs() > f64::from(i32::MAX) + 1.0 {
        return Err(HostError::type_error(format!(
            "Failed to execute '{op}' on '{iface}': Value is {} outside the range of type 'long'.",
            if v.is_finite() { "" } else { "infinite and" }
        ).replace("is  outside", "is outside")));
    }
    Ok(v.trunc() as i64)
}

fn fill_rule(
    cx: &mut CallCtx<'_, '_, '_>,
    i: usize,
    op: &str,
    iface: &str,
) -> Result<FillRule, HostError> {
    if !present(cx, i) {
        return Ok(FillRule::Winding);
    }
    let s = cx.arg_string(i)?;
    match s.as_str() {
        "nonzero" => Ok(FillRule::Winding),
        "evenodd" => Ok(FillRule::EvenOdd),
        _ => Err(HostError::type_error(format!(
            "Failed to execute '{op}' on '{iface}': The provided value '{s}' is not a valid enum value of type CanvasFillRule."
        ))),
    }
}

/// A `Path2D` argument, if argument `i` is one.
fn arg_path(cx: &mut CallCtx<'_, '_, '_>, w: &Window, i: usize) -> Option<Rc<RefCell<PathData>>> {
    if i >= cx.len() {
        return None;
    }
    let id = host_paint_id(w, cx.arg_host(i))?;
    paint(w, id, |p| match p {
        PaintObj::Path(p) => Some(p.clone()),
        _ => None,
    })
}

/// Numbers of a JS array-like.
fn number_list(cx: &mut CallCtx<'_, '_, '_>, h: Handle) -> Result<Vec<f64>, HostError> {
    let len_h = cx.get_property(h, "length")?;
    let len = match cx.handle_value(len_h) {
        lb_js::JsValue::Number(n) if n.is_finite() && n >= 0.0 => n as usize,
        _ => 0,
    };
    cx.release(len_h);
    let mut out = Vec::with_capacity(len.min(1 << 16));
    for i in 0..len.min(1 << 16) {
        let v = cx.get_property(h, &i.to_string())?;
        let n = match cx.handle_value(v) {
            lb_js::JsValue::Number(n) => n,
            lb_js::JsValue::Bool(b) => f64::from(u8::from(b)),
            lb_js::JsValue::Null => 0.0,
            lb_js::JsValue::String(s) => s.trim().parse().unwrap_or(f64::NAN),
            _ => f64::NAN,
        };
        cx.release(v);
        out.push(n);
    }
    Ok(out)
}

fn type_error_seq(op: &str, iface: &str) -> HostError {
    HostError::type_error(format!(
        "Failed to execute '{op}' on '{iface}': The provided value cannot be converted to a sequence."
    ))
}

/// A `DOMMatrix2DInit` (dictionary or DOMMatrix) as `[a, b, c, d, e, f]`.
pub(crate) fn matrix_2d_init(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    i: usize,
) -> Result<[f64; 6], HostError> {
    if let Some(id) = host_paint_id(w, cx.arg_host(i))
        && let Some(m) = paint(w, id, |p| match p {
            PaintObj::Matrix(m) => Some(m.to_2d()),
            _ => None,
        })
    {
        return Ok(m);
    }
    let d = Dict::arg(cx, i);
    let mut out = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    for (k, (a, m)) in [
        ("a", "m11"),
        ("b", "m12"),
        ("c", "m21"),
        ("d", "m22"),
        ("e", "m41"),
        ("f", "m42"),
    ]
    .into_iter()
    .enumerate()
    {
        let va = d.number(cx, a)?;
        let vm = d.number(cx, m)?;
        if let (Some(x), Some(y)) = (va, vm)
            && x != y
            && !(x.is_nan() && y.is_nan())
        {
            d.release(cx);
            return Err(HostError::type_error(format!(
                "Failed to execute 'setTransform': The '{a}' property should equal the '{m}' property."
            )));
        }
        if let Some(v) = va.or(vm) {
            out[k] = v;
        }
    }
    d.release(cx);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Image sources
// ---------------------------------------------------------------------------

/// A resolved `CanvasImageSource`.
pub(crate) enum Source {
    /// Premultiplied pixels and whether they are origin-clean.
    Pixels(Rc<Pixmap>, bool),
    /// Nothing to draw yet (image loading, video, vector image).
    Nothing,
}

const SOURCE_TYPES: &str = "(CSSImageValue or HTMLCanvasElement or HTMLImageElement or HTMLVideoElement or ImageBitmap or OffscreenCanvas or SVGImageElement or VideoFrame)";

fn snapshot_of(ctx: Option<CanvasCtx>, size: (u32, u32)) -> Option<Rc<Pixmap>> {
    match ctx {
        Some(CanvasCtx::TwoD { ctx, .. }) => ctx.borrow().snapshot().map(Rc::new),
        Some(CanvasCtx::External { ctx, .. }) => {
            let bmp = ctx.snapshot.as_ref().and_then(|f| f())?;
            let mut data = bmp.data;
            lb_paint::decode::premultiply(&mut data);
            Pixmap::from_vec(data, tiny_size(bmp.width, bmp.height)?).map(Rc::new)
        }
        None => Pixmap::new(size.0, size.1).map(Rc::new),
    }
}

fn tiny_size(w: u32, h: u32) -> Option<lb_paint::tiny_skia::IntSize> {
    lb_paint::tiny_skia::IntSize::from_wh(w, h)
}

pub(crate) fn element_ctx(w: &Window, slot: u32, n: NodeId) -> Option<CanvasCtx> {
    w.paint.canvases.borrow().get(&(slot, n)).cloned()
}

pub(crate) fn sync_element(w: &Window, slot: u32, n: NodeId) {
    if let Some(CanvasCtx::TwoD { ctx, .. }) = element_ctx(w, slot, n) {
        let (cw, ch) = canvas_size(w, CanvasTarget::Element { slot, node: n });
        let stale = {
            let c = ctx.borrow();
            c.width() != cw || c.height() != ch
        };
        if stale {
            ctx.borrow_mut().resize(cw, ch);
        }
    }
}

/// Argument `i` as a `CanvasImageSource`.
pub(crate) fn image_source(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    i: usize,
    op: &str,
    iface: &str,
) -> Result<Source, HostError> {
    let bad = || {
        HostError::type_error(format!(
            "Failed to execute '{op}' on '{iface}': The provided value is not of type '{SOURCE_TYPES}'."
        ))
    };
    let h = cx.arg_host(i).ok_or_else(bad)?;
    if let Some((slot, n)) = decode_node(h) {
        let local = w.doc_slot(slot, |d| {
            d.element(n)
                .filter(|e| e.is_html() || &*e.name.local == "image")
                .map(|e| e.name.local.to_string())
        });
        return match local.as_deref() {
            Some("img") => match super::image::pixels(w, slot, n) {
                Ok(s) => Ok(s),
                Err(()) => Err(w.rt.err(
                    "InvalidStateError",
                    format!(
                        "Failed to execute '{op}' on '{iface}': The HTMLImageElement provided is in the 'broken' state."
                    ),
                )),
            },
            Some("canvas") => {
                sync_element(w, slot, n);
                let size = canvas_size(w, CanvasTarget::Element { slot, node: n });
                if size.0 == 0 || size.1 == 0 {
                    return Err(w.rt.err(
                        "InvalidStateError",
                        format!(
                            "Failed to execute '{op}' on '{iface}': The image argument is a canvas element with a width or height of 0."
                        ),
                    ));
                }
                let ctx = element_ctx(w, slot, n);
                let clean = !matches!(&ctx, Some(CanvasCtx::TwoD { ctx, .. }) if ctx.borrow().tainted);
                Ok(snapshot_of(ctx, size).map_or(Source::Nothing, |p| Source::Pixels(p, clean)))
            }
            Some("video") | Some("image") => Ok(Source::Nothing),
            _ => Err(bad()),
        };
    }
    if h.kind == K_OBJ
        && let Some((size, ctx)) = paint(w, h.id, |p| match p {
            PaintObj::Offscreen(o) => Some(((o.width, o.height), o.ctx.clone())),
            _ => None,
        })
    {
        if size.0 == 0 || size.1 == 0 {
            return Err(w.rt.err(
                "InvalidStateError",
                format!(
                    "Failed to execute '{op}' on '{iface}': The image argument is an OffscreenCanvas element with a width or height of 0."
                ),
            ));
        }
        let clean = !matches!(&ctx, Some(CanvasCtx::TwoD { ctx, .. }) if ctx.borrow().tainted);
        return Ok(snapshot_of(ctx, size).map_or(Source::Nothing, |p| Source::Pixels(p, clean)));
    }
    Err(bad())
}

// ---------------------------------------------------------------------------
// Object returns
// ---------------------------------------------------------------------------

fn style_arg(w: &Window, s: &Style) -> Arg {
    match s {
        Style::Color(c) => Arg::String(lb_paint::serialize_color(*c)),
        Style::Gradient(g) => w
            .paint
            .ids
            .borrow()
            .get(&(Rc::as_ptr(g) as usize))
            .map_or(Arg::Null, |id| w.obj_arg(*id)),
        Style::Pattern(p) => w
            .paint
            .ids
            .borrow()
            .get(&(Rc::as_ptr(p) as *const () as usize))
            .map_or(Arg::Null, |id| w.obj_arg(*id)),
    }
}

/// `fillStyle` / `strokeStyle` setter value: a color string, a gradient or
/// a pattern. `None` keeps the current value (invalid color).
fn parse_style(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<Option<Style>, HostError> {
    if let Some(id) = host_paint_id(w, cx.arg_host(0)) {
        let s = paint(w, id, |p| match p {
            PaintObj::Gradient(g) => Some(Style::Gradient(g.clone())),
            PaintObj::Pattern(p) => Some(Style::Pattern(p.clone())),
            _ => None,
        });
        if s.is_some() {
            return Ok(s);
        }
    }
    let s = cx.arg_string(0)?;
    Ok(lb_paint::parse_color(&s).map(Style::Color))
}

fn ret_image_data(cx: &mut CallCtx<'_, '_, '_>, w: &Window, b: Bitmap) -> Result<(), HostError> {
    let data = cx.make_handle(Arg::Uint8ClampedArray(b.data))?;
    let id = new_paint(
        w,
        PaintObj::ImageData {
            width: b.width,
            height: b.height,
            data,
        },
    );
    cx.ret(w.obj_arg(id))
}

/// `ImageData` argument `i` as a bitmap (copied).
fn arg_image_data(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    i: usize,
    op: &str,
    iface: &str,
) -> Result<Bitmap, HostError> {
    let found = host_paint_id(w, cx.arg_host(i)).and_then(|id| {
        paint(w, id, |p| match p {
            PaintObj::ImageData {
                width,
                height,
                data,
            } => Some((*width, *height, *data)),
            _ => None,
        })
    });
    let Some((width, height, data)) = found else {
        return Err(HostError::type_error(format!(
            "Failed to execute '{op}' on '{iface}': parameter {} is not of type 'ImageData'.",
            i + 1
        )));
    };
    let mut bytes = cx.handle_bytes(data).unwrap_or_default();
    bytes.resize(width as usize * height as usize * 4, 0);
    Ok(Bitmap {
        width,
        height,
        data: bytes,
    })
}

fn image_data_size_ok(
    w: &Window,
    sw: i64,
    sh: i64,
    op: &str,
    iface: &str,
) -> Result<(), HostError> {
    if sw == 0 {
        return Err(w.rt.err(
            "IndexSizeError",
            format!("Failed to execute '{op}' on '{iface}': The source width is 0."),
        ));
    }
    if sh == 0 {
        return Err(w.rt.err(
            "IndexSizeError",
            format!("Failed to execute '{op}' on '{iface}': The source height is 0."),
        ));
    }
    if (sw.unsigned_abs() as u128) * (sh.unsigned_abs() as u128) * 4 > (1u128 << 31) {
        return Err(HostError::range_error(
            "Out of memory at ImageData creation",
        ));
    }
    Ok(())
}

fn tainted_error(w: &Window, op: &str, iface: &str) -> HostError {
    w.rt.err(
        "SecurityError",
        format!("Failed to execute '{op}' on '{iface}': The canvas has been tainted by cross-origin data."),
    )
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

const CTX: [&str; 2] = [
    "CanvasRenderingContext2D",
    "OffscreenCanvasRenderingContext2D",
];

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    register_element(reg, rt);
    register_offscreen(reg, rt);
    for iface in CTX {
        register_state(reg, rt, iface);
        register_drawing(reg, rt, iface);
        register_pixels(reg, rt, iface);
    }
    for iface in [
        "CanvasRenderingContext2D",
        "OffscreenCanvasRenderingContext2D",
        "Path2D",
    ] {
        register_path_methods(reg, rt, iface);
    }
    register_objects(reg, rt);
    super::matrix::register(reg, rt);
}

/// Creates (or returns) the 2D context of a canvas.
fn context_2d(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Rc<Window>,
    target: CanvasTarget,
    attrs: usize,
) -> Result<CanvasCtx, HostError> {
    let d = Dict::arg(cx, attrs);
    let alpha = match d.get(cx, "alpha")? {
        Some(h) => {
            let b = super::truthy(&cx.handle_value(h));
            cx.release(h);
            b
        }
        None => true,
    };
    let wrf = d.bool(cx, "willReadFrequently")?;
    d.release(cx);
    let (cw, ch) = canvas_size(w, target);
    let ctx = Rc::new(RefCell::new(Canvas2d::new(env(w), cw, ch, alpha)));
    let _ = wrf;
    let obj = new_paint(
        w,
        PaintObj::Context {
            target,
            ctx: ctx.clone(),
        },
    );
    Ok(CanvasCtx::TwoD { ctx, obj })
}

/// `getContext(kind, attributes)` for either canvas kind.
fn get_context(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Rc<Window>,
    target: CanvasTarget,
    existing: Option<CanvasCtx>,
) -> Result<Option<CanvasCtx>, HostError> {
    let kind = cx.arg_string(0)?;
    if let Some(c) = existing {
        let same = match &c {
            CanvasCtx::TwoD { .. } => kind == "2d",
            CanvasCtx::External { kind: k, .. } => {
                *k == kind
                    || (k.as_str() == "webgl" && kind == "experimental-webgl")
                    || (k.as_str() == "experimental-webgl" && kind == "webgl")
            }
        };
        return Ok(same.then_some(c));
    }
    match kind.as_str() {
        "2d" => context_2d(cx, w, target, 1).map(Some),
        "webgl" | "experimental-webgl" | "webgl2" => {
            let f = w.rt.0.canvas_factories.0.borrow().get(&kind).cloned();
            let Some(f) = f else { return Ok(None) };
            let attrs = (present(cx, 1)
                && matches!(cx.arg_kind(1), ValueKind::Object | ValueKind::Function))
            .then(|| cx.arg_handle(1));
            Ok(
                f(cx, w, target, &kind, attrs)?.map(|ext| CanvasCtx::External {
                    kind,
                    ctx: Rc::new(ext),
                }),
            )
        }
        _ => Ok(None),
    }
}

fn ctx_arg(w: &Window, c: &CanvasCtx) -> Arg {
    match c {
        CanvasCtx::TwoD { obj, .. } => w.obj_arg(*obj),
        CanvasCtx::External { ctx, .. } => Arg::Handle(ctx.value),
    }
}

/// Encoded canvas pixels (noised), or an empty vec for a zero-sized one.
fn encode_canvas(
    w: &Window,
    ctx: Option<CanvasCtx>,
    size: (u32, u32),
    mime: &str,
    quality: Option<f64>,
) -> (Vec<u8>, &'static str) {
    match ctx {
        Some(CanvasCtx::TwoD { ctx, .. }) => ctx.borrow().encode(mime, quality),
        Some(CanvasCtx::External { ctx, .. }) => {
            let ty = lb_paint::encode::output_type(mime);
            match ctx.snapshot.as_ref().and_then(|f| f()) {
                Some(b) if b.width > 0 && b.height > 0 => {
                    (lb_paint::encode::encode(&b, ty, quality), ty)
                }
                _ => (Vec::new(), ty),
            }
        }
        None => Canvas2d::new(env(w), size.0, size.1, true).encode(mime, quality),
    }
}

fn quality_arg(cx: &mut CallCtx<'_, '_, '_>, i: usize) -> Result<Option<f64>, HostError> {
    if i < cx.len() && cx.arg_kind(i) == ValueKind::Number {
        Ok(Some(cx.arg_f64(i)?))
    } else {
        Ok(None)
    }
}

fn is_tainted(ctx: &Option<CanvasCtx>) -> bool {
    matches!(ctx, Some(CanvasCtx::TwoD { ctx, .. }) if ctx.borrow().tainted)
}

fn register_element(reg: &mut Registry, rt: &Runtime) {
    const I: &str = "HTMLCanvasElement";
    method(reg, rt, I, "getContext", |cx, w| {
        need_args(cx, 1, "getContext", I)?;
        let (slot, node) = super::this_node(cx)?;
        let target = CanvasTarget::Element { slot, node };
        sync_element(w, slot, node);
        let existing = element_ctx(w, slot, node);
        let made = existing.is_none();
        match get_context(cx, w, target, existing)? {
            Some(c) => {
                if made {
                    w.paint
                        .canvases
                        .borrow_mut()
                        .insert((slot, node), c.clone());
                }
                cx.ret(ctx_arg(w, &c))
            }
            None => cx.ret(Arg::Null),
        }
    });
    method(reg, rt, I, "toDataURL", |cx, w| {
        let (slot, node) = super::this_node(cx)?;
        sync_element(w, slot, node);
        let ctx = element_ctx(w, slot, node);
        if is_tainted(&ctx) {
            return Err(w.rt.err(
                "SecurityError",
                "Failed to execute 'toDataURL' on 'HTMLCanvasElement': Tainted canvases may not be exported.",
            ));
        }
        let mime = if present(cx, 0) {
            cx.arg_string(0)?
        } else {
            String::new()
        };
        let quality = quality_arg(cx, 1)?;
        let size = canvas_size(w, CanvasTarget::Element { slot, node });
        if size.0 == 0 || size.1 == 0 {
            return cx.ret("data:,");
        }
        let (bytes, ty) = encode_canvas(w, ctx, size, &mime, quality);
        if bytes.is_empty() {
            return cx.ret("data:,");
        }
        let mut s = format!("data:{ty};base64,");
        lb_paint::encode::base64_into(&bytes, &mut s);
        cx.ret(s)
    });
    method(reg, rt, I, "toBlob", |cx, w| {
        need_args(cx, 1, "toBlob", I)?;
        let (slot, node) = super::this_node(cx)?;
        if cx.arg_kind(0) != ValueKind::Function {
            return Err(HostError::type_error(
                "Failed to execute 'toBlob' on 'HTMLCanvasElement': The callback provided as parameter 1 is not a function.",
            ));
        }
        sync_element(w, slot, node);
        let ctx = element_ctx(w, slot, node);
        if is_tainted(&ctx) {
            return Err(w.rt.err(
                "SecurityError",
                "Failed to execute 'toBlob' on 'HTMLCanvasElement': Tainted canvases may not be exported.",
            ));
        }
        let cb = cx.arg_handle(0);
        let mime = if present(cx, 1) {
            cx.arg_string(1)?
        } else {
            String::new()
        };
        let quality = quality_arg(cx, 2)?;
        let size = canvas_size(w, CanvasTarget::Element { slot, node });
        let (bytes, ty) = if size.0 == 0 || size.1 == 0 {
            (Vec::new(), "image/png")
        } else {
            encode_canvas(w, ctx, size, &mime, quality)
        };
        w.queue_task(
            TaskSource::DomManipulation,
            Box::new(move |js, w| {
                let arg = if bytes.is_empty() {
                    Arg::Null
                } else {
                    let id = w.new_obj(Obj::Blob(Box::new(super::misc::BlobData {
                        bytes,
                        type_: ty.to_owned(),
                    })));
                    w.obj_arg(id)
                };
                if let Err(e) = js.call(cb, Arg::Undefined, &[arg]) {
                    w.report_exception(js, &e);
                }
                js.release(cb);
            }),
        );
        Ok(())
    });
    for (attr, def) in [("width", 300u32), ("height", 150u32)] {
        setter(reg, rt, I, attr, move |cx, w| {
            let (slot, node) = super::this_node(cx)?;
            let v = cx.arg_u32(0)?;
            let v = if v > i32::MAX as u32 { def } else { v };
            let _ = w.doc_slot_mut(slot, |d| d.set_attribute(node, attr, &v.to_string()));
            w.after_mutation(cx);
            let size = canvas_size(w, CanvasTarget::Element { slot, node });
            match element_ctx(w, slot, node) {
                Some(CanvasCtx::TwoD { ctx, .. }) => ctx.borrow_mut().resize(size.0, size.1),
                Some(CanvasCtx::External { ctx, .. }) => {
                    if let Some(f) = &ctx.resize {
                        f(size.0, size.1);
                    }
                }
                None => {}
            }
            Ok(())
        });
    }
}

fn register_offscreen(reg: &mut Registry, rt: &Runtime) {
    const I: &str = "OffscreenCanvas";
    super::ctor(reg, rt, I, |cx, w| {
        need_args(cx, 2, "OffscreenCanvas", I)?;
        let mut dims = [0u32; 2];
        for (k, d) in dims.iter_mut().enumerate() {
            let v = cx.arg_f64(k)?;
            if !(0.0..=9_007_199_254_740_991.0).contains(&v) {
                return Err(HostError::type_error(
                    "Failed to construct 'OffscreenCanvas': Value is outside the 'unsigned long long' value range.",
                ));
            }
            *d = v.trunc().min(f64::from(u32::MAX)) as u32;
        }
        let id = new_paint(
            w,
            PaintObj::Offscreen(Box::new(Offscreen {
                width: dims[0],
                height: dims[1],
                ctx: None,
            })),
        );
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    for (k, attr) in ["width", "height"].into_iter().enumerate() {
        getter(reg, rt, I, attr, move |cx, w| {
            let id = this_obj(cx)?;
            let (ww, hh) = canvas_size(w, CanvasTarget::Offscreen { obj: id });
            cx.ret(if k == 0 { ww } else { hh })
        });
        setter(reg, rt, I, attr, move |cx, w| {
            let id = this_obj(cx)?;
            let v = cx.arg_f64(0)?;
            let v = if v.is_finite() && v >= 0.0 {
                v.trunc().min(f64::from(u32::MAX)) as u32
            } else {
                0
            };
            let ctx = paint(w, id, |p| match p {
                PaintObj::Offscreen(o) => {
                    if k == 0 {
                        o.width = v;
                    } else {
                        o.height = v;
                    }
                    Some((o.ctx.clone(), (o.width, o.height)))
                }
                _ => None,
            })
            .ok_or_else(illegal)?;
            match ctx {
                (Some(CanvasCtx::TwoD { ctx, .. }), (ww, hh)) => ctx.borrow_mut().resize(ww, hh),
                (Some(CanvasCtx::External { ctx, .. }), (ww, hh)) => {
                    if let Some(f) = &ctx.resize {
                        f(ww, hh);
                    }
                }
                _ => {}
            }
            Ok(())
        });
    }
    method(reg, rt, I, "getContext", |cx, w| {
        need_args(cx, 1, "getContext", I)?;
        let id = this_obj(cx)?;
        let kind = cx.arg_string(0)?;
        if !["2d", "webgl", "webgl2", "bitmaprenderer", "webgpu"].contains(&kind.as_str()) {
            return Err(HostError::type_error(format!(
                "Failed to execute 'getContext' on 'OffscreenCanvas': The provided value '{kind}' is not a valid enum value of type OffscreenRenderingContextType."
            )));
        }
        let existing = paint(w, id, |p| match p {
            PaintObj::Offscreen(o) => Some(o.ctx.clone()),
            _ => None,
        })
        .ok_or_else(illegal)?;
        let made = existing.is_none();
        match get_context(cx, w, CanvasTarget::Offscreen { obj: id }, existing)? {
            Some(c) => {
                if made {
                    let c2 = c.clone();
                    paint(w, id, move |p| match p {
                        PaintObj::Offscreen(o) => {
                            o.ctx = Some(c2);
                            Some(())
                        }
                        _ => None,
                    });
                }
                cx.ret(ctx_arg(w, &c))
            }
            None => cx.ret(Arg::Null),
        }
    });
    method(reg, rt, I, "convertToBlob", |cx, w| {
        let id = this_obj(cx)?;
        let (size, ctx) = paint(w, id, |p| match p {
            PaintObj::Offscreen(o) => Some(((o.width, o.height), o.ctx.clone())),
            _ => None,
        })
        .ok_or_else(illegal)?;
        let d = Dict::arg(cx, 0);
        let mime = d.string(cx, "type")?.unwrap_or_default();
        let quality = d.number(cx, "quality")?;
        d.release(cx);
        let r = cx.ret_new_promise()?;
        if is_tainted(&ctx) {
            let e = w.rt.err(
                "SecurityError",
                "Failed to execute 'convertToBlob' on 'OffscreenCanvas': Tainted OffscreenCanvas may not be exported.",
            );
            return cx.reject_promise(r, e);
        }
        if size.0 == 0 || size.1 == 0 {
            let e = w.rt.err(
                "IndexSizeError",
                "Failed to execute 'convertToBlob' on 'OffscreenCanvas': The size of the OffscreenCanvas is zero.",
            );
            return cx.reject_promise(r, e);
        }
        let (bytes, ty) = encode_canvas(w, ctx, size, &mime, quality);
        let b = w.new_obj(Obj::Blob(Box::new(super::misc::BlobData {
            bytes,
            type_: ty.to_owned(),
        })));
        let arg = w.obj_arg(b);
        cx.resolve_promise(r, arg)
    });
}

/// Attributes and state methods of a 2D context.
fn register_state(reg: &mut Registry, rt: &Runtime, iface: &'static str) {
    getter(reg, rt, iface, "canvas", |cx, w| {
        let id = this_obj(cx)?;
        let target = paint(w, id, |p| match p {
            PaintObj::Context { target, .. } => Some(*target),
            _ => None,
        })
        .ok_or_else(illegal)?;
        match target {
            CanvasTarget::Element { slot, node } => cx.ret(w.node_arg(slot, node)),
            CanvasTarget::Offscreen { obj } => cx.ret(w.obj_arg(obj)),
        }
    });
    macro_rules! num_attr {
        ($name:literal, $get:expr, $set:expr) => {
            getter(reg, rt, iface, $name, |cx, w| {
                let c = this_ctx(cx, w)?;
                let v: f64 = $get(&*c.borrow());
                cx.ret(v)
            });
            setter(reg, rt, iface, $name, |cx, w| {
                let c = this_ctx(cx, w)?;
                let v = cx.arg_f64(0)?;
                $set(&mut *c.borrow_mut(), v);
                Ok(())
            });
        };
    }
    num_attr!(
        "globalAlpha",
        |c: &Canvas2d| c.global_alpha(),
        |c: &mut Canvas2d, v| c.set_global_alpha(v)
    );
    num_attr!(
        "lineWidth",
        |c: &Canvas2d| c.line_width(),
        |c: &mut Canvas2d, v| c.set_line_width(v)
    );
    num_attr!(
        "miterLimit",
        |c: &Canvas2d| c.miter_limit(),
        |c: &mut Canvas2d, v| c.set_miter_limit(v)
    );
    num_attr!(
        "lineDashOffset",
        |c: &Canvas2d| c.line_dash_offset(),
        |c: &mut Canvas2d, v| c.set_line_dash_offset(v)
    );
    num_attr!(
        "shadowOffsetX",
        |c: &Canvas2d| c.shadow_offset().0,
        |c: &mut Canvas2d, v| c.set_shadow_offset_x(v)
    );
    num_attr!(
        "shadowOffsetY",
        |c: &Canvas2d| c.shadow_offset().1,
        |c: &mut Canvas2d, v| c.set_shadow_offset_y(v)
    );
    num_attr!(
        "shadowBlur",
        |c: &Canvas2d| c.shadow_blur(),
        |c: &mut Canvas2d, v| c.set_shadow_blur(v)
    );

    macro_rules! str_attr {
        ($name:literal, $get:expr, $set:expr) => {
            getter(reg, rt, iface, $name, |cx, w| {
                let c = this_ctx(cx, w)?;
                let v: String = $get(&*c.borrow());
                cx.ret(v)
            });
            setter(reg, rt, iface, $name, |cx, w| {
                let c = this_ctx(cx, w)?;
                let v = cx.arg_string(0)?;
                $set(&mut *c.borrow_mut(), v.as_str());
                Ok(())
            });
        };
    }
    str_attr!(
        "globalCompositeOperation",
        |c: &Canvas2d| c.composite().to_owned(),
        |c: &mut Canvas2d, v: &str| c.set_composite(v)
    );
    str_attr!(
        "lineCap",
        |c: &Canvas2d| c.line_cap().to_owned(),
        |c: &mut Canvas2d, v: &str| c.set_line_cap(v)
    );
    str_attr!(
        "lineJoin",
        |c: &Canvas2d| c.line_join().to_owned(),
        |c: &mut Canvas2d, v: &str| c.set_line_join(v)
    );
    str_attr!(
        "font",
        |c: &Canvas2d| c.font().to_owned(),
        |c: &mut Canvas2d, v: &str| c.set_font(v)
    );
    str_attr!(
        "textAlign",
        |c: &Canvas2d| c.text_align().as_str().to_owned(),
        |c: &mut Canvas2d, v: &str| {
            if let Some(a) = TextAlign::parse(v) {
                c.set_text_align(a)
            }
        }
    );
    str_attr!(
        "textBaseline",
        |c: &Canvas2d| c.text_baseline().as_str().to_owned(),
        |c: &mut Canvas2d, v: &str| {
            if let Some(b) = TextBaseline::parse(v) {
                c.set_text_baseline(b)
            }
        }
    );
    str_attr!(
        "direction",
        |c: &Canvas2d| c.direction().to_owned(),
        |c: &mut Canvas2d, v: &str| c.set_direction(v)
    );
    str_attr!(
        "filter",
        |c: &Canvas2d| c.filter().to_owned(),
        |c: &mut Canvas2d, v: &str| c.set_filter(v)
    );
    str_attr!(
        "imageSmoothingQuality",
        |c: &Canvas2d| c.image_smoothing().1.to_owned(),
        |c: &mut Canvas2d, v: &str| c.set_image_smoothing_quality(v)
    );
    str_attr!(
        "fontKerning",
        |c: &Canvas2d| c.text_flags()[0].to_owned(),
        |c: &mut Canvas2d, v: &str| c.set_text_flag(0, v)
    );
    str_attr!(
        "fontStretch",
        |c: &Canvas2d| c.text_flags()[1].to_owned(),
        |c: &mut Canvas2d, v: &str| c.set_text_flag(1, v)
    );
    str_attr!(
        "fontVariantCaps",
        |c: &Canvas2d| c.text_flags()[2].to_owned(),
        |c: &mut Canvas2d, v: &str| c.set_text_flag(2, v)
    );
    str_attr!(
        "textRendering",
        |c: &Canvas2d| c.text_flags()[3].to_owned(),
        |c: &mut Canvas2d, v: &str| c.set_text_flag(3, v)
    );
    for (k, name) in ["letterSpacing", "wordSpacing"].into_iter().enumerate() {
        getter(reg, rt, iface, name, move |cx, w| {
            let c = this_ctx(cx, w)?;
            let (l, ws) = c.borrow().spacing();
            cx.ret(lb_css::fmt_px(if k == 0 { l } else { ws }))
        });
        setter(reg, rt, iface, name, move |cx, w| {
            let c = this_ctx(cx, w)?;
            let v = cx.arg_string(0)?;
            let size = {
                let cb = c.borrow();
                lb_paint::CanvasFont::parse(cb.font()).map_or(10.0, |f| f.size)
            };
            if let Some(px) = spacing_px(&v, size) {
                let mut cb = c.borrow_mut();
                if k == 0 {
                    cb.set_spacing(Some(px), None);
                } else {
                    cb.set_spacing(None, Some(px));
                }
            }
            Ok(())
        });
    }
    getter(reg, rt, iface, "lang", |cx, w| {
        this_ctx(cx, w)?;
        cx.ret("inherit")
    });
    setter(reg, rt, iface, "lang", |cx, w| {
        this_ctx(cx, w)?;
        Ok(())
    });
    getter(reg, rt, iface, "imageSmoothingEnabled", |cx, w| {
        let c = this_ctx(cx, w)?;
        let v = c.borrow().image_smoothing().0;
        cx.ret(v)
    });
    setter(reg, rt, iface, "imageSmoothingEnabled", |cx, w| {
        let c = this_ctx(cx, w)?;
        let v = cx.arg_bool(0);
        c.borrow_mut().set_image_smoothing_enabled(v);
        Ok(())
    });
    for (k, name) in ["fillStyle", "strokeStyle"].into_iter().enumerate() {
        getter(reg, rt, iface, name, move |cx, w| {
            let c = this_ctx(cx, w)?;
            let s = {
                let cb = c.borrow();
                if k == 0 {
                    cb.fill_style().clone()
                } else {
                    cb.stroke_style().clone()
                }
            };
            cx.ret(style_arg(w, &s))
        });
        setter(reg, rt, iface, name, move |cx, w| {
            let c = this_ctx(cx, w)?;
            if let Some(s) = parse_style(cx, w)? {
                let mut cb = c.borrow_mut();
                if k == 0 {
                    cb.set_fill_style(s);
                } else {
                    cb.set_stroke_style(s);
                }
            }
            Ok(())
        });
    }
    getter(reg, rt, iface, "shadowColor", |cx, w| {
        let c = this_ctx(cx, w)?;
        let v = c.borrow().shadow_color();
        cx.ret(lb_paint::serialize_color(v))
    });
    setter(reg, rt, iface, "shadowColor", |cx, w| {
        let c = this_ctx(cx, w)?;
        let s = cx.arg_string(0)?;
        if let Some(col) = lb_paint::parse_color(&s) {
            c.borrow_mut().set_shadow_color(col);
        }
        Ok(())
    });

    method(reg, rt, iface, "save", |cx, w| {
        this_ctx(cx, w)?.borrow_mut().save();
        Ok(())
    });
    method(reg, rt, iface, "restore", |cx, w| {
        this_ctx(cx, w)?.borrow_mut().restore();
        Ok(())
    });
    method(reg, rt, iface, "reset", |cx, w| {
        this_ctx(cx, w)?.borrow_mut().reset();
        Ok(())
    });
    method(reg, rt, iface, "isContextLost", |cx, w| {
        this_ctx(cx, w)?;
        cx.ret(false)
    });
    method(reg, rt, iface, "getContextAttributes", |cx, w| {
        let c = this_ctx(cx, w)?;
        let alpha = c.borrow().alpha();
        cx.ret(Arg::Json(format!(
            "{{\"alpha\":{alpha},\"colorSpace\":\"srgb\",\"desynchronized\":false,\"toneMapping\":{{\"mode\":\"standard\"}},\"willReadFrequently\":false}}"
        )))
    });
    method(reg, rt, iface, "drawFocusIfNeeded", |cx, w| {
        this_ctx(cx, w)?;
        Ok(())
    });
    method(reg, rt, iface, "setLineDash", move |cx, w| {
        need_args(cx, 1, "setLineDash", iface)?;
        let c = this_ctx(cx, w)?;
        if !matches!(cx.arg_kind(0), ValueKind::Object) {
            return Err(type_error_seq("setLineDash", iface));
        }
        let h = cx.arg_handle(0);
        let list = number_list(cx, h)?;
        c.borrow_mut().set_line_dash(list);
        Ok(())
    });
    method(reg, rt, iface, "getLineDash", |cx, w| {
        let c = this_ctx(cx, w)?;
        let v: Vec<Arg> = c
            .borrow()
            .line_dash()
            .iter()
            .map(|v| Arg::Number(*v))
            .collect();
        cx.ret(Arg::Array(v))
    });

    // Transforms.
    method(reg, rt, iface, "scale", move |cx, w| {
        need_args(cx, 2, "scale", iface)?;
        let c = this_ctx(cx, w)?;
        let [x, y] = nums::<2>(cx, 0)?;
        c.borrow_mut().scale(x, y);
        Ok(())
    });
    method(reg, rt, iface, "rotate", move |cx, w| {
        need_args(cx, 1, "rotate", iface)?;
        let c = this_ctx(cx, w)?;
        let a = num(cx, 0)?;
        c.borrow_mut().rotate(a);
        Ok(())
    });
    method(reg, rt, iface, "translate", move |cx, w| {
        need_args(cx, 2, "translate", iface)?;
        let c = this_ctx(cx, w)?;
        let [x, y] = nums::<2>(cx, 0)?;
        c.borrow_mut().translate(x, y);
        Ok(())
    });
    method(reg, rt, iface, "transform", move |cx, w| {
        need_args(cx, 6, "transform", iface)?;
        let c = this_ctx(cx, w)?;
        let m = nums::<6>(cx, 0)?;
        c.borrow_mut().transform(m);
        Ok(())
    });
    method(reg, rt, iface, "setTransform", move |cx, w| {
        let c = this_ctx(cx, w)?;
        let m = match cx.len() {
            0 => [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            1 => matrix_2d_init(cx, w, 0)?,
            n if n >= 6 => nums::<6>(cx, 0)?,
            n => {
                return Err(HostError::type_error(format!(
                    "Failed to execute 'setTransform' on '{iface}': Valid arities are: [0, 1, 6], but {n} arguments provided."
                )));
            }
        };
        c.borrow_mut().set_transform(m);
        Ok(())
    });
    method(reg, rt, iface, "resetTransform", |cx, w| {
        this_ctx(cx, w)?.borrow_mut().reset_transform();
        Ok(())
    });
    method(reg, rt, iface, "getTransform", |cx, w| {
        let c = this_ctx(cx, w)?;
        let m = c.borrow().get_transform();
        let id = new_paint(
            w,
            PaintObj::Matrix(Box::new(super::matrix::Matrix::from_2d(m, false))),
        );
        cx.ret(w.obj_arg(id))
    });
}

/// `letterSpacing` / `wordSpacing` value in px.
fn spacing_px(v: &str, font_size: f32) -> Option<f32> {
    let v = v.trim();
    if v.contains([';', '{', '}', '!']) {
        return None;
    }
    let block = lb_css::DeclarationBlock::parse(&format!("letter-spacing: {v}"));
    match block.decls.first().map(|d| &d.value) {
        Some(lb_css::Decl::Value(lb_css::Sv::Length(c))) => {
            let px = resolve_len(c, font_size);
            px.is_finite().then_some(px)
        }
        Some(lb_css::Decl::Value(lb_css::Sv::Normal)) => Some(0.0),
        _ => None,
    }
}

fn resolve_len(c: &lb_css::Calc, em: f32) -> f32 {
    use lb_css::{Calc, Unit};
    match c {
        Calc::Dim(v, u) => match u {
            Unit::Px => *v,
            Unit::Pct => v * em / 100.0,
            Unit::Em => v * em,
            Unit::Rem => v * 16.0,
            Unit::Ex | Unit::Ch => v * em * 0.5,
            _ => 0.0,
        },
        Calc::Num(v) => *v,
        Calc::Sum(xs) => xs.iter().map(|x| resolve_len(x, em)).sum(),
        Calc::Mul(x, k) => resolve_len(x, em) * k,
        Calc::Min(xs) => xs
            .iter()
            .map(|x| resolve_len(x, em))
            .fold(f32::INFINITY, f32::min),
        Calc::Max(xs) => xs
            .iter()
            .map(|x| resolve_len(x, em))
            .fold(f32::NEG_INFINITY, f32::max),
        Calc::Clamp(lo, v, hi) => resolve_len(v, em)
            .min(resolve_len(hi, em))
            .max(resolve_len(lo, em)),
    }
}

/// Paths, rectangles, text and images.
fn register_drawing(reg: &mut Registry, rt: &Runtime, iface: &'static str) {
    method(reg, rt, iface, "beginPath", |cx, w| {
        this_ctx(cx, w)?.borrow_mut().begin_path();
        Ok(())
    });
    for (name, k) in [("fillRect", 0), ("strokeRect", 1), ("clearRect", 2)] {
        method(reg, rt, iface, name, move |cx, w| {
            need_args(cx, 4, name, iface)?;
            let c = this_ctx(cx, w)?;
            let [x, y, ww, hh] = nums::<4>(cx, 0)?;
            let mut cb = c.borrow_mut();
            match k {
                0 => cb.fill_rect(x, y, ww, hh),
                1 => cb.stroke_rect(x, y, ww, hh),
                _ => cb.clear_rect(x, y, ww, hh),
            }
            Ok(())
        });
    }
    for (name, k) in [("fill", 0), ("clip", 1)] {
        method(reg, rt, iface, name, move |cx, w| {
            let c = this_ctx(cx, w)?;
            let path = arg_path(cx, w, 0);
            let rule = fill_rule(cx, w_rule_index(&path), name, iface)?;
            let p = path.map(|p| p.borrow().clone());
            let mut cb = c.borrow_mut();
            if k == 0 {
                cb.fill(p.as_ref(), rule);
            } else {
                cb.clip(p.as_ref(), rule);
            }
            Ok(())
        });
    }
    method(reg, rt, iface, "stroke", |cx, w| {
        let c = this_ctx(cx, w)?;
        let p = arg_path(cx, w, 0).map(|p| p.borrow().clone());
        c.borrow_mut().stroke(p.as_ref());
        Ok(())
    });
    method(reg, rt, iface, "isPointInPath", move |cx, w| {
        need_args(cx, 2, "isPointInPath", iface)?;
        let c = this_ctx(cx, w)?;
        let path = arg_path(cx, w, 0);
        let o = usize::from(path.is_some());
        if path.is_some() {
            need_args(cx, 3, "isPointInPath", iface)?;
        }
        let [x, y] = nums::<2>(cx, o)?;
        let rule = fill_rule(cx, o + 2, "isPointInPath", iface)?;
        let p = path.map(|p| p.borrow().clone());
        let r = c.borrow().is_point_in_path(p.as_ref(), x, y, rule);
        cx.ret(r)
    });
    method(reg, rt, iface, "isPointInStroke", move |cx, w| {
        need_args(cx, 2, "isPointInStroke", iface)?;
        let c = this_ctx(cx, w)?;
        let path = arg_path(cx, w, 0);
        let o = usize::from(path.is_some());
        if path.is_some() {
            need_args(cx, 3, "isPointInStroke", iface)?;
        }
        let [x, y] = nums::<2>(cx, o)?;
        let p = path.map(|p| p.borrow().clone());
        let r = c.borrow().is_point_in_stroke(p.as_ref(), x, y);
        cx.ret(r)
    });
    for (name, k) in [("fillText", 0), ("strokeText", 1)] {
        method(reg, rt, iface, name, move |cx, w| {
            need_args(cx, 3, name, iface)?;
            let c = this_ctx(cx, w)?;
            let text = cx.arg_string(0)?;
            let [x, y] = nums::<2>(cx, 1)?;
            let max = if present(cx, 3) {
                Some(num(cx, 3)?)
            } else {
                None
            };
            let mut cb = c.borrow_mut();
            if k == 0 {
                cb.fill_text(&text, x, y, max);
            } else {
                cb.stroke_text(&text, x, y, max);
            }
            Ok(())
        });
    }
    method(reg, rt, iface, "measureText", move |cx, w| {
        need_args(cx, 1, "measureText", iface)?;
        let c = this_ctx(cx, w)?;
        let text = cx.arg_string(0)?;
        let m = c.borrow_mut().measure_text(&text);
        let id = new_paint(w, PaintObj::Metrics(Box::new(m)));
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, iface, "drawImage", move |cx, w| {
        let n = cx.len();
        if !matches!(n, 3 | 5 | 9) {
            if n < 3 {
                need_args(cx, 3, "drawImage", iface)?;
            }
            return Err(HostError::type_error(format!(
                "Failed to execute 'drawImage' on '{iface}': Valid arities are: [3, 5, 9], but {n} arguments provided."
            )));
        }
        let c = this_ctx(cx, w)?;
        let src = image_source(cx, w, 0, "drawImage", iface)?;
        let args: Vec<f64> = (1..n).map(|i| cx.arg_f64(i)).collect::<Result<_, _>>()?;
        let Source::Pixels(pm, clean) = src else {
            return Ok(());
        };
        let (iw, ih) = (f64::from(pm.width()), f64::from(pm.height()));
        let (s, d) = match args.as_slice() {
            [dx, dy] => ([0.0, 0.0, iw, ih], [*dx, *dy, iw, ih]),
            [dx, dy, dw, dh] => ([0.0, 0.0, iw, ih], [*dx, *dy, *dw, *dh]),
            [sx, sy, sw, sh, dx, dy, dw, dh] => ([*sx, *sy, *sw, *sh], [*dx, *dy, *dw, *dh]),
            _ => return Ok(()),
        };
        let mut cb = c.borrow_mut();
        if !clean {
            cb.tainted = true;
        }
        cb.draw_image(&pm, s[0], s[1], s[2], s[3], d[0], d[1], d[2], d[3]);
        Ok(())
    });

    // Paint objects.
    method(reg, rt, iface, "createLinearGradient", move |cx, w| {
        need_args(cx, 4, "createLinearGradient", iface)?;
        this_ctx(cx, w)?;
        let [x0, y0, x1, y1] = nums::<4>(cx, 0)?;
        if ![x0, y0, x1, y1].iter().all(|v| v.is_finite()) {
            return Err(non_finite("createLinearGradient", iface));
        }
        ret_gradient(cx, w, Gradient::linear(x0, y0, x1, y1))
    });
    method(reg, rt, iface, "createRadialGradient", move |cx, w| {
        need_args(cx, 6, "createRadialGradient", iface)?;
        this_ctx(cx, w)?;
        let v = nums::<6>(cx, 0)?;
        if !v.iter().all(|v| v.is_finite()) {
            return Err(non_finite("createRadialGradient", iface));
        }
        for (r, name) in [(v[2], "r0"), (v[5], "r1")] {
            if r < 0.0 {
                return Err(w.rt.err(
                    "IndexSizeError",
                    format!(
                        "Failed to execute 'createRadialGradient' on '{iface}': The {name} provided is less than 0."
                    ),
                ));
            }
        }
        ret_gradient(cx, w, Gradient::radial(v[0], v[1], v[2], v[3], v[4], v[5]))
    });
    method(reg, rt, iface, "createConicGradient", move |cx, w| {
        need_args(cx, 3, "createConicGradient", iface)?;
        this_ctx(cx, w)?;
        let [a, x, y] = nums::<3>(cx, 0)?;
        if ![a, x, y].iter().all(|v| v.is_finite()) {
            return Err(non_finite("createConicGradient", iface));
        }
        ret_gradient(cx, w, Gradient::conic(a, x, y))
    });
    method(reg, rt, iface, "createPattern", move |cx, w| {
        need_args(cx, 2, "createPattern", iface)?;
        let c = this_ctx(cx, w)?;
        let src = image_source(cx, w, 0, "createPattern", iface)?;
        let rep = if cx.arg_kind(1) == ValueKind::Null {
            String::new()
        } else {
            cx.arg_string(1)?
        };
        let Some(r) = Repetition::parse(&rep) else {
            return Err(w.rt.err(
                "SyntaxError",
                format!(
                    "Failed to execute 'createPattern' on '{iface}': The provided type ('{rep}') is not one of 'repeat', 'repeat-x', 'repeat-y', or 'no-repeat'."
                ),
            ));
        };
        let Source::Pixels(pm, clean) = src else {
            return cx.ret(Arg::Null);
        };
        if !clean {
            c.borrow_mut().tainted = true;
        }
        let p = Rc::new(CanvasPattern::new(pm, r));
        let addr = Rc::as_ptr(&p) as *const () as usize;
        let id = new_paint(w, PaintObj::Pattern(p));
        w.paint.ids.borrow_mut().insert(addr, id);
        cx.ret(w.obj_arg(id))
    });
}

/// `fill(rule)` vs `fill(path, rule)`: index of the rule argument.
fn w_rule_index(path: &Option<Rc<RefCell<PathData>>>) -> usize {
    usize::from(path.is_some())
}

fn non_finite(op: &str, iface: &str) -> HostError {
    HostError::type_error(format!(
        "Failed to execute '{op}' on '{iface}': The provided double value is non-finite."
    ))
}

fn ret_gradient(cx: &mut CallCtx<'_, '_, '_>, w: &Window, g: Gradient) -> Result<(), HostError> {
    let g = Rc::new(g);
    let addr = Rc::as_ptr(&g) as usize;
    let id = new_paint(w, PaintObj::Gradient(g));
    w.paint.ids.borrow_mut().insert(addr, id);
    cx.ret(w.obj_arg(id))
}

/// `getImageData`, `putImageData`, `createImageData`.
fn register_pixels(reg: &mut Registry, rt: &Runtime, iface: &'static str) {
    method(reg, rt, iface, "getImageData", move |cx, w| {
        need_args(cx, 4, "getImageData", iface)?;
        let c = this_ctx(cx, w)?;
        let mut v = [0i64; 4];
        for (k, x) in v.iter_mut().enumerate() {
            *x = enforce_long(cx, k, "getImageData", iface)?;
        }
        let [mut sx, mut sy, mut sw, mut sh] = v;
        image_data_size_ok(w, sw, sh, "getImageData", iface)?;
        if sw < 0 {
            sx += sw;
            sw = -sw;
        }
        if sh < 0 {
            sy += sh;
            sh = -sh;
        }
        let b = {
            let cb = c.borrow();
            if cb.tainted {
                return Err(tainted_error(w, "getImageData", iface));
            }
            cb.get_image_data(sx, sy, sw as u32, sh as u32)
        };
        ret_image_data(cx, w, b)
    });
    method(reg, rt, iface, "createImageData", move |cx, w| {
        need_args(cx, 1, "createImageData", iface)?;
        this_ctx(cx, w)?;
        let (sw, sh) = if cx.len() == 1 || cx.arg_kind(0) == ValueKind::Object {
            let b = arg_image_data(cx, w, 0, "createImageData", iface)?;
            (i64::from(b.width), i64::from(b.height))
        } else {
            let sw = enforce_long(cx, 0, "createImageData", iface)?;
            let sh = enforce_long(cx, 1, "createImageData", iface)?;
            image_data_size_ok(w, sw, sh, "createImageData", iface)?;
            (sw.abs(), sh.abs())
        };
        ret_image_data(cx, w, Bitmap::new(sw as u32, sh as u32))
    });
    method(reg, rt, iface, "putImageData", move |cx, w| {
        need_args(cx, 3, "putImageData", iface)?;
        if !matches!(cx.len(), 3 | 7) && cx.len() < 7 {
            return Err(HostError::type_error(format!(
                "Failed to execute 'putImageData' on '{iface}': Valid arities are: [3, 7], but {} arguments provided.",
                cx.len()
            )));
        }
        let c = this_ctx(cx, w)?;
        let img = arg_image_data(cx, w, 0, "putImageData", iface)?;
        let dx = enforce_long(cx, 1, "putImageData", iface)?;
        let dy = enforce_long(cx, 2, "putImageData", iface)?;
        let dirty = if cx.len() >= 7 {
            let mut d = [0i64; 4];
            for (k, x) in d.iter_mut().enumerate() {
                *x = enforce_long(cx, 3 + k, "putImageData", iface)?;
            }
            Some((d[0], d[1], d[2], d[3]))
        } else {
            None
        };
        c.borrow_mut().put_image_data(&img, dx, dy, dirty);
        Ok(())
    });
}

/// Path methods shared by contexts and `Path2D`.
fn register_path_methods(reg: &mut Registry, rt: &Runtime, iface: &'static str) {
    fn with_path(
        cx: &mut CallCtx<'_, '_, '_>,
        w: &Window,
        f: impl FnOnce(&mut PathData, &tiny_skia_ts::Transform),
    ) -> Result<(), HostError> {
        let id = this_obj(cx)?;
        let path = paint(w, id, |p| match p {
            PaintObj::Path(p) => Some(p.clone()),
            _ => None,
        });
        match path {
            Some(p) => {
                f(&mut p.borrow_mut(), &tiny_skia_ts::Transform::identity());
                Ok(())
            }
            None => {
                let c = this_ctx(cx, w)?;
                c.borrow_mut().path_op(f);
                Ok(())
            }
        }
    }
    method(reg, rt, iface, "closePath", |cx, w| {
        with_path(cx, w, |p, _| p.close())
    });
    method(reg, rt, iface, "moveTo", move |cx, w| {
        need_args(cx, 2, "moveTo", iface)?;
        let [x, y] = nums::<2>(cx, 0)?;
        with_path(cx, w, |p, ts| p.move_to(ts, x, y))
    });
    method(reg, rt, iface, "lineTo", move |cx, w| {
        need_args(cx, 2, "lineTo", iface)?;
        let [x, y] = nums::<2>(cx, 0)?;
        with_path(cx, w, |p, ts| p.line_to(ts, x, y))
    });
    method(reg, rt, iface, "quadraticCurveTo", move |cx, w| {
        need_args(cx, 4, "quadraticCurveTo", iface)?;
        let [a, b, x, y] = nums::<4>(cx, 0)?;
        with_path(cx, w, |p, ts| p.quad_to(ts, a, b, x, y))
    });
    method(reg, rt, iface, "bezierCurveTo", move |cx, w| {
        need_args(cx, 6, "bezierCurveTo", iface)?;
        let v = nums::<6>(cx, 0)?;
        with_path(cx, w, |p, ts| {
            p.bezier_to(ts, v[0], v[1], v[2], v[3], v[4], v[5])
        })
    });
    method(reg, rt, iface, "arcTo", move |cx, w| {
        need_args(cx, 5, "arcTo", iface)?;
        let v = nums::<5>(cx, 0)?;
        if v[4] < 0.0 {
            return Err(w.rt.err(
                "IndexSizeError",
                format!(
                    "Failed to execute 'arcTo' on '{iface}': The radius provided ({}) is negative.",
                    super::matrix::js_num(v[4])
                ),
            ));
        }
        with_path(cx, w, |p, ts| p.arc_to(ts, v[0], v[1], v[2], v[3], v[4]))
    });
    method(reg, rt, iface, "rect", move |cx, w| {
        need_args(cx, 4, "rect", iface)?;
        let v = nums::<4>(cx, 0)?;
        with_path(cx, w, |p, ts| p.rect(ts, v[0], v[1], v[2], v[3]))
    });
    method(reg, rt, iface, "roundRect", move |cx, w| {
        need_args(cx, 4, "roundRect", iface)?;
        let v = nums::<4>(cx, 0)?;
        let radii = round_rect_radii(cx, w, 4, iface)?;
        with_path(cx, w, |p, ts| {
            if let Some(r) = radii {
                p.round_rect(ts, v[0], v[1], v[2], v[3], r)
            }
        })
    });
    method(reg, rt, iface, "arc", move |cx, w| {
        need_args(cx, 5, "arc", iface)?;
        let v = nums::<5>(cx, 0)?;
        let ccw = present(cx, 5) && cx.arg_bool(5);
        if v[2] < 0.0 {
            return Err(w.rt.err(
                "IndexSizeError",
                format!(
                    "Failed to execute 'arc' on '{iface}': The radius provided ({}) is negative.",
                    super::matrix::js_num(v[2])
                ),
            ));
        }
        with_path(cx, w, |p, ts| p.arc(ts, v[0], v[1], v[2], v[3], v[4], ccw))
    });
    method(reg, rt, iface, "ellipse", move |cx, w| {
        need_args(cx, 7, "ellipse", iface)?;
        let v = nums::<7>(cx, 0)?;
        let ccw = present(cx, 7) && cx.arg_bool(7);
        for (r, name) in [(v[2], "major-axis"), (v[3], "minor-axis")] {
            if r < 0.0 {
                return Err(w.rt.err(
                    "IndexSizeError",
                    format!(
                        "Failed to execute 'ellipse' on '{iface}': The {name} radius provided ({}) is negative.",
                        super::matrix::js_num(r)
                    ),
                ));
            }
        }
        with_path(cx, w, |p, ts| {
            p.ellipse(ts, v[0], v[1], v[2], v[3], v[4], v[5], v[6], ccw)
        })
    });
}

/// `roundRect` radii argument: a number, a `DOMPointInit`, or a list of
/// 1-4 of those. `None` when a value is non-finite (nothing drawn).
fn round_rect_radii(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    i: usize,
    iface: &str,
) -> Result<Option<[(f64, f64); 4]>, HostError> {
    let range = |n: usize| {
        HostError::range_error(format!(
            "Failed to execute 'roundRect' on '{iface}': {n} radii provided. Between one and four radii are necessary."
        ))
    };
    let mut list: Vec<(f64, f64)> = Vec::new();
    if !present(cx, i) {
        list.push((0.0, 0.0));
    } else if cx.arg_kind(i) == ValueKind::Object {
        let h = cx.arg_handle(i);
        let len = cx.get_property(h, "length")?;
        let is_list = matches!(cx.handle_value(len), lb_js::JsValue::Number(_));
        cx.release(len);
        if is_list {
            let n = number_or_points(cx, h)?;
            list = n;
        } else {
            list.push(point_init(cx, h)?);
        }
    } else {
        let r = cx.arg_f64(i)?;
        list.push((r, r));
    }
    if list.is_empty() || list.len() > 4 {
        return Err(range(list.len()));
    }
    if list.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
        return Ok(None);
    }
    if let Some((x, y)) = list.iter().find(|(x, y)| *x < 0.0 || *y < 0.0) {
        let v = if *x < 0.0 { *x } else { *y };
        return Err(w.rt.err(
            "IndexSizeError",
            format!(
                "Failed to execute 'roundRect' on '{iface}': Radius value {} is negative.",
                super::matrix::js_num(v)
            ),
        ));
    }
    let r = match list.as_slice() {
        [a] => [*a; 4],
        [a, b] => [*a, *b, *a, *b],
        [a, b, c] => [*a, *b, *c, *b],
        [a, b, c, d] => [*a, *b, *c, *d],
        _ => unreachable!(),
    };
    Ok(Some(r))
}

fn point_init(cx: &mut CallCtx<'_, '_, '_>, h: Handle) -> Result<(f64, f64), HostError> {
    let mut out = [0.0; 2];
    for (k, key) in ["x", "y"].into_iter().enumerate() {
        let v = cx.get_property(h, key)?;
        out[k] = match cx.handle_value(v) {
            lb_js::JsValue::Number(n) => n,
            lb_js::JsValue::Undefined => 0.0,
            _ => f64::NAN,
        };
        cx.release(v);
    }
    Ok((out[0], out[1]))
}

fn number_or_points(cx: &mut CallCtx<'_, '_, '_>, h: Handle) -> Result<Vec<(f64, f64)>, HostError> {
    let len_h = cx.get_property(h, "length")?;
    let len = match cx.handle_value(len_h) {
        lb_js::JsValue::Number(n) if n.is_finite() && n >= 0.0 => n as usize,
        _ => 0,
    };
    cx.release(len_h);
    let mut out = Vec::new();
    for i in 0..len.min(8) {
        let v = cx.get_property(h, &i.to_string())?;
        let p = match cx.handle_kind(v) {
            ValueKind::Object => point_init(cx, v)?,
            _ => match cx.handle_value(v) {
                lb_js::JsValue::Number(n) => (n, n),
                _ => (f64::NAN, f64::NAN),
            },
        };
        cx.release(v);
        out.push(p);
    }
    if len > 8 {
        out.resize(len, (0.0, 0.0));
    }
    Ok(out)
}

/// `CanvasGradient`, `CanvasPattern`, `Path2D`, `ImageData`, `TextMetrics`.
fn register_objects(reg: &mut Registry, rt: &Runtime) {
    method(reg, rt, "CanvasGradient", "addColorStop", |cx, w| {
        need_args(cx, 2, "addColorStop", "CanvasGradient")?;
        let id = this_obj(cx)?;
        let g = paint(w, id, |p| match p {
            PaintObj::Gradient(g) => Some(g.clone()),
            _ => None,
        })
        .ok_or_else(illegal)?;
        let off = cx.arg_f64(0)?;
        let color = cx.arg_string(1)?;
        if !off.is_finite() {
            return Err(non_finite("addColorStop", "CanvasGradient"));
        }
        if !(0.0..=1.0).contains(&off) {
            return Err(w.rt.err(
                "IndexSizeError",
                format!(
                    "Failed to execute 'addColorStop' on 'CanvasGradient': The provided value ({}) is outside the range (0.0, 1.0).",
                    super::matrix::js_num(off)
                ),
            ));
        }
        let Some(c) = lb_paint::parse_color(&color) else {
            return Err(w.rt.err(
                "SyntaxError",
                format!(
                    "Failed to execute 'addColorStop' on 'CanvasGradient': The value provided ('{color}') could not be parsed as a color."
                ),
            ));
        };
        g.add_color_stop(off, c);
        Ok(())
    });
    method(reg, rt, "CanvasPattern", "setTransform", |cx, w| {
        let id = this_obj(cx)?;
        let p = paint(w, id, |p| match p {
            PaintObj::Pattern(p) => Some(p.clone()),
            _ => None,
        })
        .ok_or_else(illegal)?;
        let m = if cx.is_empty() {
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
        } else {
            matrix_2d_init(cx, w, 0)?
        };
        p.set_transform(m);
        Ok(())
    });

    // Path2D.
    super::ctor(reg, rt, "Path2D", |cx, w| {
        let path = match arg_path(cx, w, 0) {
            Some(p) => p.borrow().clone(),
            None if present(cx, 0) => {
                let d = cx.arg_string(0)?;
                lb_paint::parse_svg_path(&d)
            }
            None => PathData::new(),
        };
        let id = new_paint(w, PaintObj::Path(Rc::new(RefCell::new(path))));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    method(reg, rt, "Path2D", "addPath", |cx, w| {
        need_args(cx, 1, "addPath", "Path2D")?;
        let id = this_obj(cx)?;
        let me = paint(w, id, |p| match p {
            PaintObj::Path(p) => Some(p.clone()),
            _ => None,
        })
        .ok_or_else(illegal)?;
        let Some(other) = arg_path(cx, w, 0) else {
            return Err(HostError::type_error(
                "Failed to execute 'addPath' on 'Path2D': parameter 1 is not of type 'Path2D'.",
            ));
        };
        let m = if present(cx, 1) {
            matrix_2d_init(cx, w, 1)?
        } else {
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
        };
        if !m.iter().all(|v| v.is_finite()) {
            return Ok(());
        }
        let ts = tiny_skia_ts::Transform::from_row(
            m[0] as f32,
            m[1] as f32,
            m[2] as f32,
            m[3] as f32,
            m[4] as f32,
            m[5] as f32,
        );
        let src = other.borrow().clone();
        me.borrow_mut().add_path(&src, &ts);
        Ok(())
    });

    // ImageData.
    super::ctor(reg, rt, "ImageData", |cx, w| {
        const I: &str = "ImageData";
        need_args(cx, 2, I, I)?;
        let err = |w: &Window, name: &str, msg: &str| {
            w.rt.err(name, format!("Failed to construct 'ImageData': {msg}"))
        };
        let (width, height, data) = if matches!(cx.arg_kind(0), ValueKind::Object) {
            let h = cx.arg_handle(0);
            let Some(bytes) = cx.handle_bytes(h) else {
                return Err(HostError::type_error(
                    "Failed to construct 'ImageData': parameter 1 is not of type 'Uint8ClampedArray'.",
                ));
            };
            let sw = cx.arg_u32(1)?;
            let len = bytes.len();
            if len == 0 || len % 4 != 0 {
                return Err(err(
                    w,
                    "InvalidStateError",
                    "The input data length is not a multiple of 4.",
                ));
            }
            if sw == 0 {
                return Err(err(
                    w,
                    "IndexSizeError",
                    "The source width is zero or not a number.",
                ));
            }
            if (len / 4) % sw as usize != 0 {
                return Err(err(
                    w,
                    "IndexSizeError",
                    "The input data length is not a multiple of (4 * width).",
                ));
            }
            let rows = (len / 4 / sw as usize) as u32;
            if present(cx, 2) {
                let sh = cx.arg_u32(2)?;
                if sh != rows {
                    return Err(err(
                        w,
                        "IndexSizeError",
                        "The input data length is not equal to (4 * width * height).",
                    ));
                }
            }
            (sw, rows, h)
        } else {
            let sw = cx.arg_u32(0)?;
            let sh = cx.arg_u32(1)?;
            if sw == 0 {
                return Err(err(
                    w,
                    "IndexSizeError",
                    "The source width is zero or not a number.",
                ));
            }
            if sh == 0 {
                return Err(err(
                    w,
                    "IndexSizeError",
                    "The source height is zero or not a number.",
                ));
            }
            if u64::from(sw) * u64::from(sh) * 4 > (1u64 << 31) {
                return Err(HostError::range_error(
                    "Out of memory at ImageData creation",
                ));
            }
            let h = cx.make_handle(Arg::Uint8ClampedArray(vec![
                0;
                sw as usize * sh as usize * 4
            ]))?;
            (sw, sh, h)
        };
        let id = new_paint(
            w,
            PaintObj::ImageData {
                width,
                height,
                data,
            },
        );
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    for (name, k) in [("width", 0), ("height", 1)] {
        getter(reg, rt, "ImageData", name, move |cx, w| {
            let id = this_obj(cx)?;
            let v = paint(w, id, |p| match p {
                PaintObj::ImageData { width, height, .. } => {
                    Some(if k == 0 { *width } else { *height })
                }
                _ => None,
            })
            .ok_or_else(illegal)?;
            cx.ret(v)
        });
    }
    getter(reg, rt, "ImageData", "data", |cx, w| {
        let id = this_obj(cx)?;
        let h = paint(w, id, |p| match p {
            PaintObj::ImageData { data, .. } => Some(*data),
            _ => None,
        })
        .ok_or_else(illegal)?;
        cx.ret(Arg::Handle(h))
    });
    getter(reg, rt, "ImageData", "colorSpace", |cx, _w| {
        this_obj(cx)?;
        cx.ret("srgb")
    });
    getter(reg, rt, "ImageData", "pixelFormat", |cx, _w| {
        this_obj(cx)?;
        cx.ret("rgba-unorm8")
    });

    // TextMetrics.
    type Field = fn(&TextMetrics) -> f64;
    let fields: [(&'static str, Field); 12] = [
        ("width", |m| m.width),
        ("actualBoundingBoxLeft", |m| m.actual_bounding_box_left),
        ("actualBoundingBoxRight", |m| m.actual_bounding_box_right),
        ("fontBoundingBoxAscent", |m| m.font_bounding_box_ascent),
        ("fontBoundingBoxDescent", |m| m.font_bounding_box_descent),
        ("actualBoundingBoxAscent", |m| m.actual_bounding_box_ascent),
        ("actualBoundingBoxDescent", |m| {
            m.actual_bounding_box_descent
        }),
        ("emHeightAscent", |m| m.em_height_ascent),
        ("emHeightDescent", |m| m.em_height_descent),
        ("hangingBaseline", |m| m.hanging_baseline),
        ("alphabeticBaseline", |m| m.alphabetic_baseline),
        ("ideographicBaseline", |m| m.ideographic_baseline),
    ];
    for (name, f) in fields {
        getter(reg, rt, "TextMetrics", name, move |cx, w| {
            let id = this_obj(cx)?;
            let v = paint(w, id, |p| match p {
                PaintObj::Metrics(m) => Some(f(m)),
                _ => None,
            })
            .ok_or_else(illegal)?;
            cx.ret(v)
        });
    }
}

// tiny-skia's transform type, through lb-paint's re-export.
use lb_paint::tiny_skia as tiny_skia_ts;
