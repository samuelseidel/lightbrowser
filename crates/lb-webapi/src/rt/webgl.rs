//! WebGL 1 and 2 as a stub renderer whose answers come from the persona.
//!
//! - `getContext('webgl' | 'experimental-webgl' | 'webgl2')` goes through
//!   the canvas context factory hook ([`Runtime::set_canvas_context_factory`]).
//!   Without a full persona there is no WebGL (null), as on a machine with
//!   WebGL disabled.
//! - `getParameter`: strings from the persona (`hardware.gpu`,
//!   `webgl.version`, `webgl.shading_language_version`; WebGL 1 gets
//!   Chrome's 1.0 strings), limits from `webgl.parameters` by constant
//!   name, then defaults for the persona's backend (D3D11, Metal or GL,
//!   from the unmasked renderer). Derived limits (`MAX_VARYING_COMPONENTS`,
//!   combined uniform components) follow ANGLE's formulas. State queries
//!   return the context's state with Chrome's defaults.
//! - Extensions: `webgl.extensions` is the WebGL 2 list (Chrome's order);
//!   the WebGL 1 list is derived (WebGL 2-only ones dropped, the ones that
//!   are core in WebGL 2 added). Extension objects are instances of their
//!   own hidden interfaces with the IDL constants; `WEBGL_debug_renderer_info`
//!   unlocks `UNMASKED_VENDOR_WEBGL` / `UNMASKED_RENDERER_WEBGL` (persona
//!   `hardware.gpu.*_unmasked`, ANGLE strings).
//! - `getShaderPrecisionFormat` from `webgl.shader_precision`.
//! - Shaders always compile when they have a `main`, programs link when a
//!   vertex and a fragment shader are attached. Active attributes and
//!   uniforms are read from the GLSL declarations (a uniform counts as
//!   active when its name is used again after the declaration).
//! - Pixels: `clear` fills the drawing buffer (scissor and color mask
//!   honoured); `drawArrays` / `drawElements` rasterize triangles, lines
//!   and points from the first enabled float vertex attribute read as
//!   clip-space x/y (no shaders run), filled with a `vec4` uniform if one
//!   was set, else a constant `vec4(...)` written to the fragment output,
//!   else a color hashed from the fragment shader source. `readPixels`,
//!   `toDataURL` and `drawImage` read the same pixels with the canvas
//!   noise rule keyed by `webgl_noise_seed` (edges only; solid fills read
//!   back exactly).
//!
//! Known gaps: no shader execution, textures, framebuffer objects as render
//! targets, blending, depth or stencil; `preserveDrawingBuffer: false`
//! does not clear after compositing; extension prototypes keep a
//! `constructor` property (Chrome's have none).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use lb_js::{Arg, CallCtx, HostError, JsValue, Member, ValueKind, ViewKind};
use lb_paint::tiny_skia;
use lb_persona::PersonaProfile;

use super::canvas::{CanvasTarget, ContextFactory, ExternalContext, canvas_size};
use super::webgl_ext::{EXTENSIONS, ExtDesc};
use super::{Dict, K_OBJ, Obj, Runtime, Window, getter, illegal, method, setter, this_obj};
use crate::registry::{ImplKind, Registry, host_fn};

const IFACES: [&str; 2] = ["WebGLRenderingContext", "WebGL2RenderingContext"];

// GL enums used here.
const DEPTH_BUFFER_BIT: u32 = 0x0100;
const COLOR_BUFFER_BIT: u32 = 0x4000;
const POINTS: u32 = 0;
const LINES: u32 = 1;
const LINE_LOOP: u32 = 2;
const LINE_STRIP: u32 = 3;
const TRIANGLES: u32 = 4;
const TRIANGLE_STRIP: u32 = 5;
const TRIANGLE_FAN: u32 = 6;
const NO_ERROR: u32 = 0;
const INVALID_ENUM: u32 = 0x0500;
const INVALID_VALUE: u32 = 0x0501;
const INVALID_OPERATION: u32 = 0x0502;
const CONTEXT_LOST_WEBGL: u32 = 0x9242;
const ARRAY_BUFFER: u32 = 0x8892;
const ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
const BUFFER_SIZE: u32 = 0x8764;
const BUFFER_USAGE: u32 = 0x8765;
const STATIC_DRAW: u32 = 0x88E4;
const FRAGMENT_SHADER: u32 = 0x8B30;
const VERTEX_SHADER: u32 = 0x8B31;
const COMPILE_STATUS: u32 = 0x8B81;
const DELETE_STATUS: u32 = 0x8B80;
const LINK_STATUS: u32 = 0x8B82;
const VALIDATE_STATUS: u32 = 0x8B83;
const ATTACHED_SHADERS: u32 = 0x8B85;
const ACTIVE_UNIFORMS: u32 = 0x8B86;
const ACTIVE_ATTRIBUTES: u32 = 0x8B89;
const SHADER_TYPE: u32 = 0x8B4F;
const TRANSFORM_FEEDBACK_BUFFER_MODE: u32 = 0x8C7F;
const TRANSFORM_FEEDBACK_VARYINGS: u32 = 0x8C83;
const ACTIVE_UNIFORM_BLOCKS: u32 = 0x8A36;
const SEPARATE_ATTRIBS: u32 = 0x8C8D;
const FLOAT: u32 = 0x1406;
const UNSIGNED_BYTE: u32 = 0x1401;
const UNSIGNED_SHORT: u32 = 0x1403;
const UNSIGNED_INT: u32 = 0x1405;
const RGBA: u32 = 0x1908;
const FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
const TEXTURE0: u32 = 0x84C0;
const UNMASKED_VENDOR_WEBGL: u32 = 0x9245;
const UNMASKED_RENDERER_WEBGL: u32 = 0x9246;
const MAX_TEXTURE_MAX_ANISOTROPY_EXT: u32 = 0x84FF;
const SCISSOR_TEST: u32 = 0x0C11;
const DITHER: u32 = 0x0BD0;
const ALREADY_SIGNALED: u32 = 0x911A;
const SIGNALED: u32 = 0x9119;
const SYNC_STATUS: u32 = 0x9114;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Context creation attributes.
#[derive(Clone, Copy, Debug)]
struct Attrs {
    alpha: bool,
    antialias: bool,
    depth: bool,
    desynchronized: bool,
    fail_if_major_performance_caveat: bool,
    power_preference: &'static str,
    premultiplied_alpha: bool,
    preserve_drawing_buffer: bool,
    stencil: bool,
    xr_compatible: bool,
}

impl Default for Attrs {
    fn default() -> Self {
        Attrs {
            alpha: true,
            antialias: true,
            depth: true,
            desynchronized: false,
            fail_if_major_performance_caveat: false,
            power_preference: "default",
            premultiplied_alpha: true,
            preserve_drawing_buffer: false,
            stencil: false,
            xr_compatible: false,
        }
    }
}

/// What a WebGL object handle is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResKind {
    Buffer,
    Texture,
    Program,
    Shader,
    Framebuffer,
    Renderbuffer,
    VertexArray,
    Query,
    Sampler,
    Sync,
    TransformFeedback,
}

impl ResKind {
    fn iface(self, oes_vao: bool) -> &'static str {
        match self {
            ResKind::Buffer => "WebGLBuffer",
            ResKind::Texture => "WebGLTexture",
            ResKind::Program => "WebGLProgram",
            ResKind::Shader => "WebGLShader",
            ResKind::Framebuffer => "WebGLFramebuffer",
            ResKind::Renderbuffer => "WebGLRenderbuffer",
            ResKind::VertexArray if oes_vao => "WebGLVertexArrayObjectOES",
            ResKind::VertexArray => "WebGLVertexArrayObject",
            ResKind::Query => "WebGLQuery",
            ResKind::Sampler => "WebGLSampler",
            ResKind::Sync => "WebGLSync",
            ResKind::TransformFeedback => "WebGLTransformFeedback",
        }
    }
}

/// One GLSL variable: (name, GL type, array size).
type Var = (String, u32, i32);

#[derive(Default)]
struct Res {
    deleted: bool,
    // Buffer
    bytes: Vec<u8>,
    usage: u32,
    // Shader
    shader_type: u32,
    source: String,
    compiled: bool,
    // Program
    shaders: Vec<u32>,
    linked: bool,
    attribs: Vec<(Var, i32)>,
    uniforms: Vec<Var>,
    bound_locations: HashMap<String, i32>,
    frag_color: Option<[f32; 4]>,
    frag_hash: u32,
}

#[derive(Clone, Copy, Default)]
struct Attrib {
    enabled: bool,
    buffer: Option<u32>,
    size: u32,
    ty: u32,
    normalized: bool,
    stride: u32,
    offset: u32,
}

/// A WebGL context.
pub(crate) struct Gl {
    webgl2: bool,
    target: CanvasTarget,
    obj: u32,
    attrs: Attrs,
    width: u32,
    height: u32,
    /// Drawing buffer, RGBA rows top-down, as stored (premultiplied when
    /// `premultipliedAlpha`).
    fb: Option<Vec<u8>>,
    clear_color: [f32; 4],
    color_mask: [bool; 4],
    viewport: [i32; 4],
    scissor: [i32; 4],
    enabled: HashSet<u32>,
    error: u32,
    lost: bool,
    profile: Arc<PersonaProfile>,
    params: HashMap<u32, serde_json::Value>,
    extensions: Vec<&'static str>,
    ext_objs: HashMap<&'static str, u32>,
    state: HashMap<u32, JsValue>,
    bound: HashMap<u32, u32>,
    res: HashMap<u32, Res>,
    attribs: Vec<Attrib>,
    uniforms: HashMap<(u32, String), Vec<f32>>,
    active_texture: u32,
    seed: Option<[u8; 16]>,
}

/// WebGL host objects (one `Obj` variant).
pub(crate) enum GlObj {
    Ctx {
        gl: Rc<RefCell<Gl>>,
        webgl2: bool,
    },
    Res {
        ctx: Rc<RefCell<Gl>>,
        kind: ResKind,
        oes: bool,
    },
    Location {
        program: u32,
        name: String,
    },
    ActiveInfo {
        name: String,
        size: i32,
        ty: u32,
    },
    Precision([i32; 3]),
    Ext {
        iface: &'static str,
        ctx: Rc<RefCell<Gl>>,
    },
}

impl GlObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            GlObj::Ctx { webgl2: true, .. } => "WebGL2RenderingContext",
            GlObj::Ctx { .. } => "WebGLRenderingContext",
            GlObj::Res { kind, oes, .. } => kind.iface(*oes),
            GlObj::Location { .. } => "WebGLUniformLocation",
            GlObj::ActiveInfo { .. } => "WebGLActiveInfo",
            GlObj::Precision(_) => "WebGLShaderPrecisionFormat",
            GlObj::Ext { iface, .. } => iface,
        }
    }
}

fn with_gl<R>(w: &Window, id: u32, f: impl FnOnce(&GlObj) -> Option<R>) -> Option<R> {
    w.with_obj(id, |o| match o {
        Obj::Gl(g) => f(g),
        _ => None,
    })
    .flatten()
}

fn new_gl(w: &Window, g: GlObj) -> u32 {
    w.new_obj(Obj::Gl(Box::new(g)))
}

fn this_gl(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<Rc<RefCell<Gl>>, HostError> {
    let id = this_obj(cx)?;
    with_gl(w, id, |g| match g {
        GlObj::Ctx { gl, .. } => Some(gl.clone()),
        _ => None,
    })
    .ok_or_else(illegal)
}

/// Object id of a WebGL resource argument (`None` for null/undefined or
/// a foreign object).
fn arg_res(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    i: usize,
    gl: &Rc<RefCell<Gl>>,
) -> Option<u32> {
    let h = cx.arg_host(i).filter(|h| h.kind == K_OBJ)?;
    with_gl(w, h.id, |g| match g {
        GlObj::Res { ctx, .. } if Rc::ptr_eq(ctx, gl) => Some(h.id),
        _ => None,
    })
}

fn opt_obj(w: &Window, id: Option<u32>) -> Arg {
    id.map_or(Arg::Null, |i| w.obj_arg(i))
}

// ---------------------------------------------------------------------------
// Persona-derived answers
// ---------------------------------------------------------------------------

/// Name → value of every WebGL 2 constant (and extension constant).
fn constant_values() -> HashMap<&'static str, u32> {
    let mut m = HashMap::new();
    for iface in IFACES {
        if let Some(d) = crate::generated::chromium::TABLE.interface(iface) {
            for (n, v) in d.constants {
                if let crate::desc::ConstValue::Number(x) = v {
                    m.insert(*n, *x as u32);
                }
            }
        }
    }
    for e in EXTENSIONS {
        for (n, v) in e.consts {
            m.entry(*n).or_insert(*v);
        }
    }
    m
}

/// The GPU backend the persona's unmasked renderer names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    D3d11,
    Metal,
    Gl,
}

fn backend(p: &PersonaProfile) -> Backend {
    let r = &p.hardware.gpu.renderer_unmasked;
    if r.contains("Direct3D") || r.contains("D3D11") {
        Backend::D3d11
    } else if r.contains("Metal") {
        Backend::Metal
    } else {
        Backend::Gl
    }
}

/// Extensions that are core in WebGL 2 and exist only in WebGL 1.
const WEBGL1_ONLY: &[&str] = &[
    "ANGLE_instanced_arrays",
    "EXT_blend_minmax",
    "EXT_frag_depth",
    "EXT_shader_texture_lod",
    "EXT_sRGB",
    "OES_element_index_uint",
    "OES_fbo_render_mipmap",
    "OES_standard_derivatives",
    "OES_texture_float",
    "OES_texture_half_float",
    "OES_texture_half_float_linear",
    "OES_vertex_array_object",
    "WEBGL_depth_texture",
    "WEBGL_draw_buffers",
];

/// Extensions only WebGL 2 contexts expose.
const WEBGL2_ONLY: &[&str] = &[
    "EXT_color_buffer_float",
    "EXT_conservative_depth",
    "EXT_disjoint_timer_query_webgl2",
    "EXT_render_snorm",
    "EXT_texture_norm16",
    "NV_shader_noperspective_interpolation",
    "OES_draw_buffers_indexed",
    "OES_sample_variables",
    "OES_shader_multisample_interpolation",
    "OVR_multiview2",
    "WEBGL_clip_cull_distance",
    "WEBGL_provoking_vertex",
    "WEBGL_render_shared_exponent",
    "WEBGL_shader_pixel_local_storage",
    "WEBGL_stencil_texturing",
    "WEBGL_draw_instanced_base_vertex_base_instance",
    "WEBGL_multi_draw_instanced_base_vertex_base_instance",
];

/// Chrome's WebGL 1 registration order.
const WEBGL1_ORDER: &[&str] = &[
    "ANGLE_instanced_arrays",
    "EXT_blend_minmax",
    "EXT_clip_control",
    "EXT_color_buffer_half_float",
    "EXT_depth_clamp",
    "EXT_disjoint_timer_query",
    "EXT_float_blend",
    "EXT_frag_depth",
    "EXT_polygon_offset_clamp",
    "EXT_shader_texture_lod",
    "EXT_texture_compression_bptc",
    "EXT_texture_compression_rgtc",
    "EXT_texture_filter_anisotropic",
    "EXT_texture_mirror_clamp_to_edge",
    "EXT_sRGB",
    "KHR_parallel_shader_compile",
    "OES_element_index_uint",
    "OES_fbo_render_mipmap",
    "OES_standard_derivatives",
    "OES_texture_float",
    "OES_texture_float_linear",
    "OES_texture_half_float",
    "OES_texture_half_float_linear",
    "OES_vertex_array_object",
    "WEBGL_blend_func_extended",
    "WEBGL_color_buffer_float",
    "WEBGL_compressed_texture_astc",
    "WEBGL_compressed_texture_etc",
    "WEBGL_compressed_texture_etc1",
    "WEBGL_compressed_texture_pvrtc",
    "WEBGL_compressed_texture_s3tc",
    "WEBGL_compressed_texture_s3tc_srgb",
    "WEBGL_debug_renderer_info",
    "WEBGL_debug_shaders",
    "WEBGL_depth_texture",
    "WEBGL_draw_buffers",
    "WEBGL_lose_context",
    "WEBGL_multi_draw",
    "WEBGL_polygon_mode",
];

fn static_name(name: &str) -> Option<&'static str> {
    WEBGL1_ORDER
        .iter()
        .chain(WEBGL2_ONLY)
        .chain(WEBGL1_ONLY)
        .find(|n| **n == name)
        .copied()
}

/// The supported extension names of a context.
fn extension_list(p: &PersonaProfile, webgl2: bool) -> Vec<&'static str> {
    let listed: Vec<&str> = p.webgl.extensions.iter().map(String::as_str).collect();
    if webgl2 {
        return listed
            .iter()
            .filter(|n| !WEBGL1_ONLY.contains(n))
            .filter_map(|n| static_name(n))
            .collect();
    }
    let has = |n: &str| listed.contains(&n);
    WEBGL1_ORDER
        .iter()
        .copied()
        .filter(|n| {
            has(n)
                || match *n {
                    "WEBGL_color_buffer_float" => has("EXT_color_buffer_float"),
                    "EXT_disjoint_timer_query" => has("EXT_disjoint_timer_query_webgl2"),
                    other => WEBGL1_ONLY.contains(&other),
                }
        })
        .collect()
}

/// The hidden interface of an extension name (`WEBGL_lose_context` →
/// `WebGLLoseContext`).
fn ext_desc(name: &str) -> Option<&'static ExtDesc> {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| *c != '_')
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let n = norm(name);
    EXTENSIONS.iter().find(|e| norm(e.iface) == n)
}

/// Interface specs of the extension objects, for the installer.
pub(crate) fn extension_specs(registry: &Registry) -> Vec<(&'static str, lb_js::InterfaceSpec)> {
    EXTENSIONS
        .iter()
        .map(|e| {
            let mut proto: Vec<Member> = e
                .consts
                .iter()
                .map(|(n, v)| Member::constant(*n, JsValue::Number(f64::from(*v))))
                .collect();
            for (op, len) in e.ops {
                let f = registry
                    .get(e.iface, op, ImplKind::Method)
                    .cloned()
                    .unwrap_or_else(|| host_fn(|_| Ok(())));
                proto.push(Member::method(*op, u32::from(*len), f));
            }
            (e.iface, lb_js::InterfaceSpec::new(e.iface).prototype(proto))
        })
        .collect()
}

fn persona_num(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
}

impl Gl {
    fn p(&self, name: &str, def: f64) -> f64 {
        self.profile
            .webgl
            .parameters
            .get(name)
            .and_then(persona_num)
            .unwrap_or(def)
    }

    fn pair(&self, name: &str, def: [f64; 2]) -> [f64; 2] {
        match self.profile.webgl.parameters.get(name) {
            Some(serde_json::Value::Array(a)) if a.len() == 2 => [
                a[0].as_f64().unwrap_or(def[0]),
                a[1].as_f64().unwrap_or(def[1]),
            ],
            _ => def,
        }
    }

    fn version(&self) -> String {
        let v = &self.profile.webgl.version;
        if self.webgl2 {
            v.clone()
        } else if v.starts_with("WebGL 2.0") {
            "WebGL 1.0 (OpenGL ES 2.0 Chromium)".into()
        } else {
            v.clone()
        }
    }

    fn glsl_version(&self) -> String {
        let v = &self.profile.webgl.shading_language_version;
        if self.webgl2 {
            v.clone()
        } else if v.contains("3.00") {
            "WebGL GLSL ES 1.0 (OpenGL ES GLSL ES 1.0 Chromium)".into()
        } else {
            v.clone()
        }
    }

    fn ext_enabled(&self, name: &str) -> bool {
        self.ext_objs.contains_key(name)
    }

    fn set_error(&mut self, e: u32) {
        if self.error == NO_ERROR {
            self.error = e;
        }
    }

    /// A limit: persona first, then the backend default.
    fn limit(&self, pname: u32) -> Option<Arg> {
        let be = backend(&self.profile);
        let d3d = be == Backend::D3d11;
        let num = |v: f64| Some(Arg::Number(v));
        const WEBGL1_LIMITS: [u32; 13] = [
            0x0D33, 0x851C, 0x84E8, 0x0D3A, 0x8869, 0x8DFB, 0x8DFD, 0x8DFC, 0x8B4D, 0x8B4C, 0x8872,
            0x846E, 0x846D,
        ];
        if !self.webgl2 && !WEBGL1_LIMITS.contains(&pname) {
            return None;
        }
        if let Some(v) = self.params.get(&pname) {
            return match v {
                serde_json::Value::Array(a) => {
                    let xs: Vec<f64> = a.iter().filter_map(serde_json::Value::as_f64).collect();
                    if matches!(pname, 0x0D3A) {
                        Some(Arg::Int32Array(xs.iter().map(|x| *x as i32).collect()))
                    } else {
                        Some(Arg::Float32Array(xs.iter().map(|x| *x as f32).collect()))
                    }
                }
                serde_json::Value::Number(n) => num(n.as_f64().unwrap_or(0.0)),
                serde_json::Value::Bool(b) => Some(Arg::Bool(*b)),
                serde_json::Value::String(s) => Some(Arg::String(s.clone())),
                _ => None,
            };
        }
        let varying = self.p("MAX_VARYING_VECTORS", if d3d { 30.0 } else { 31.0 });
        let block = self.p("MAX_UNIFORM_BLOCK_SIZE", 65536.0);
        let blocks = if d3d { 12.0 } else { 14.0 };
        let v1 = match pname {
            0x0D33 | 0x851C | 0x84E8 => num(16384.0),
            0x0D3A => {
                let s = if d3d { 32767 } else { 16384 };
                Some(Arg::Int32Array(vec![s, s]))
            }
            0x8869 => num(16.0),
            0x8DFB => num(if d3d { 4096.0 } else { 1024.0 }),
            0x8DFD => num(1024.0),
            0x8DFC => num(varying),
            0x8B4D => num(32.0),
            0x8B4C | 0x8872 => num(16.0),
            0x846E => Some(Arg::Float32Array(vec![1.0, 1.0])),
            0x846D => {
                let r = self.pair("ALIASED_POINT_SIZE_RANGE", [1.0, 1024.0]);
                Some(Arg::Float32Array(vec![r[0] as f32, r[1] as f32]))
            }
            _ => None,
        };
        if v1.is_some() || !self.webgl2 {
            return v1;
        }
        let vuv = self.p("MAX_VERTEX_UNIFORM_VECTORS", 4096.0);
        let fuv = self.p("MAX_FRAGMENT_UNIFORM_VECTORS", 1024.0);
        let vuc = self.p("MAX_VERTEX_UNIFORM_COMPONENTS", vuv * 4.0);
        let fuc = self.p("MAX_FRAGMENT_UNIFORM_COMPONENTS", fuv * 4.0);
        let vub = self.p("MAX_VERTEX_UNIFORM_BLOCKS", blocks);
        let fub = self.p("MAX_FRAGMENT_UNIFORM_BLOCKS", blocks);
        match pname {
            0x8073 | 0x88FF => num(2048.0),
            0x8CDF | 0x8824 => num(8.0),
            0x8A33 => num(fuc + fub * block / 4.0),
            0x8A31 => num(vuv * 4.0 + vub * block / 4.0),
            0x8A2E => num(vub + fub),
            0x8D6B => num(if be == Backend::Gl {
                1_073_741_823.0
            } else {
                4_294_967_295.0
            }),
            0x80E9 | 0x80E8 => num(2_147_483_647.0),
            0x9125 | 0x9122 => num(if d3d {
                varying * 4.0
            } else {
                ((varying + 1.0) * 4.0).min(128.0)
            }),
            0x8A2D => num(fub),
            0x8A2B => num(vub),
            0x8B49 => num(fuc),
            0x8B4A => num(vuc),
            0x8905 => num(7.0),
            0x8904 => num(-8.0),
            0x8D57 => num(4.0),
            0x9111 => num(0.0),
            0x84FD => num(if d3d { 15.0 } else { 16.0 }),
            0x8C8A => num(if d3d { 120.0 } else { 128.0 }),
            0x8C8B | 0x8C80 => num(4.0),
            0x8A30 => num(block),
            0x8A2F => num(if d3d { 24.0 } else { 72.0 }),
            0x8B4B => num(varying * 4.0),
            0x8A34 => num(256.0),
            _ => None,
        }
    }

    /// `getParameter(pname)`; `None` → INVALID_ENUM.
    fn parameter(&self, w: &Window, pname: u32) -> Option<Arg> {
        let gpu = &self.profile.hardware.gpu;
        let b = |v: bool| Some(Arg::Bool(v));
        let n = |v: f64| Some(Arg::Number(v));
        let obj = |p: u32| Some(opt_obj(w, self.bound.get(&p).copied()));
        let wgl2 = self.webgl2;
        match pname {
            0x1F00 => return Some(Arg::String(gpu.vendor.clone())),
            0x1F01 => return Some(Arg::String(gpu.renderer.clone())),
            0x1F02 => return Some(Arg::String(self.version())),
            0x8B8C => return Some(Arg::String(self.glsl_version())),
            UNMASKED_VENDOR_WEBGL if self.ext_enabled("WEBGL_debug_renderer_info") => {
                return Some(Arg::String(gpu.vendor_unmasked.clone()));
            }
            UNMASKED_RENDERER_WEBGL if self.ext_enabled("WEBGL_debug_renderer_info") => {
                return Some(Arg::String(gpu.renderer_unmasked.clone()));
            }
            MAX_TEXTURE_MAX_ANISOTROPY_EXT
                if self.ext_enabled("EXT_texture_filter_anisotropic") =>
            {
                return n(16.0);
            }
            0x0BE2 | 0x0B44 | 0x0B71 | 0x0BD0 | 0x8037 | 0x809E | 0x80A0 | 0x0C11 | 0x0B90 => {
                return b(self.enabled.contains(&pname));
            }
            0x8C89 if wgl2 => return b(self.enabled.contains(&pname)),
            0x8894 | 0x8B8D | 0x8CA6 | 0x8CA7 | 0x8069 | 0x8514 | 0x8895 => return obj(pname),
            0x85B5 if wgl2 || self.ext_enabled("OES_vertex_array_object") => return obj(pname),
            0x8CAA | 0x8F36 | 0x8F37 | 0x88ED | 0x88EF | 0x8919 | 0x8C1D | 0x806A | 0x8E25
            | 0x8C8F | 0x8A28
                if wgl2 =>
            {
                return obj(pname);
            }
            0x84E0 => return n(f64::from(self.active_texture)),
            0x8005 => {
                return Some(self.state_f32s(pname, &[0.0; 4]));
            }
            0x0C22 => return Some(Arg::Float32Array(self.clear_color.to_vec())),
            0x0C23 => {
                return Some(Arg::Array(
                    self.color_mask.iter().map(|v| Arg::Bool(*v)).collect(),
                ));
            }
            0x0B70 => return Some(self.state_f32s(pname, &[0.0, 1.0])),
            0x0BA2 => return Some(Arg::Int32Array(self.viewport.to_vec())),
            0x0C10 => return Some(Arg::Int32Array(self.scissor.to_vec())),
            0x86A3 => return Some(Arg::Uint32Array(Vec::new())),
            0x0D52..=0x0D54 => return n(8.0),
            0x0D55 => return n(if self.attrs.alpha { 8.0 } else { 0.0 }),
            0x0D56 => return n(if self.attrs.depth { 24.0 } else { 0.0 }),
            0x0D57 => return n(if self.attrs.stencil { 8.0 } else { 0.0 }),
            0x0D50 => return n(4.0),
            0x80A8 => return n(if self.attrs.antialias { 1.0 } else { 0.0 }),
            0x80A9 => return n(if self.attrs.antialias { 4.0 } else { 0.0 }),
            0x8B9B => return n(f64::from(RGBA)),
            0x8B9A => return n(f64::from(UNSIGNED_BYTE)),
            0x9247 => return n(0.0),
            _ => {}
        }
        if let Some(v) = self.state.get(&pname) {
            return Some(match v {
                JsValue::Bool(x) => Arg::Bool(*x),
                JsValue::Number(x) => Arg::Number(*x),
                other => Arg::from(other.clone()),
            });
        }
        if let Some(v) = state_default(pname, wgl2) {
            return Some(v);
        }
        if pname == 0x8B8B && (wgl2 || self.ext_enabled("OES_standard_derivatives")) {
            return n(4352.0);
        }
        if !wgl2 && matches!(pname, 0x8824 | 0x8CDF) && self.ext_enabled("WEBGL_draw_buffers") {
            return n(8.0);
        }
        self.limit(pname)
    }

    fn state_f32s(&self, pname: u32, def: &[f32]) -> Arg {
        match self.state.get(&pname) {
            Some(JsValue::Json(s)) => Arg::Float32Array(
                serde_json::from_str::<Vec<f32>>(s).unwrap_or_else(|_| def.to_vec()),
            ),
            _ => Arg::Float32Array(def.to_vec()),
        }
    }
}

/// Default values of plain state queries.
fn state_default(pname: u32, webgl2: bool) -> Option<Arg> {
    let n = |v: f64| Some(Arg::Number(v));
    let all = 4_294_967_295.0;
    let v = match pname {
        0x8009 | 0x883D => n(32774.0),
        0x80C9 | 0x80CB => n(1.0),
        0x80C8 | 0x80CA => n(0.0),
        0x0B45 => n(1029.0),
        0x0B46 => n(2305.0),
        0x0B74 => n(513.0),
        0x0B73 => n(1.0),
        0x0B72 => Some(Arg::Bool(true)),
        0x0B21 => n(1.0),
        0x8038 | 0x2A00 => n(0.0),
        0x80AA => n(1.0),
        0x80AB => Some(Arg::Bool(false)),
        0x0B91 | 0x0B97 | 0x8CA3 => n(0.0),
        0x0B92 | 0x8800 => n(519.0),
        0x0B93 | 0x0B98 | 0x8CA4 | 0x8CA5 => n(all),
        0x0B94 | 0x0B95 | 0x0B96 | 0x8801 | 0x8802 | 0x8803 => n(7680.0),
        0x0CF5 | 0x0D05 => n(4.0),
        0x8192 => n(4352.0),
        0x9240 | 0x9241 => Some(Arg::Bool(false)),
        0x9243 => n(37444.0),
        _ => None,
    };
    if v.is_some() || !webgl2 {
        return v;
    }
    match pname {
        0x0C02 => n(1029.0),
        0x0CF2 | 0x0CF3 | 0x0CF4 | 0x0D02 | 0x0D03 | 0x0D04 | 0x806D | 0x806E => n(0.0),
        0x8825..=0x882C => n(1029.0),
        0x8E23 | 0x8E24 => Some(Arg::Bool(false)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// GLSL declarations
// ---------------------------------------------------------------------------

fn glsl_type(t: &str) -> Option<u32> {
    Some(match t {
        "float" => 0x1406,
        "vec2" => 0x8B50,
        "vec3" => 0x8B51,
        "vec4" => 0x8B52,
        "int" => 0x1404,
        "ivec2" => 0x8B53,
        "ivec3" => 0x8B54,
        "ivec4" => 0x8B55,
        "bool" => 0x8B56,
        "bvec2" => 0x8B57,
        "bvec3" => 0x8B58,
        "bvec4" => 0x8B59,
        "mat2" => 0x8B5A,
        "mat3" => 0x8B5B,
        "mat4" => 0x8B5C,
        "sampler2D" => 0x8B5E,
        "samplerCube" => 0x8B60,
        "sampler3D" => 0x8B5F,
        "sampler2DShadow" => 0x8B62,
        "sampler2DArray" => 0x8DC1,
        "uint" => 0x1405,
        "uvec2" => 0x8DC6,
        "uvec3" => 0x8DC7,
        "uvec4" => 0x8DC8,
        "isampler2D" => 0x8DCA,
        "usampler2D" => 0x8DD2,
        _ => return None,
    })
}

/// Strips comments.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    out
}

/// Global declarations with `qualifier` (`attribute`, `in`, `uniform`).
fn declarations(src: &str, qualifiers: &[&str]) -> Vec<Var> {
    let src = strip_comments(src);
    let mut out = Vec::new();
    let mut depth = 0i32;
    for stmt in src.split(';') {
        let opens = stmt.matches('{').count() as i32;
        let closes = stmt.matches('}').count() as i32;
        let at_top = depth == 0 && opens == 0;
        depth += opens - closes;
        if !at_top {
            continue;
        }
        let stmt = match stmt.rfind('}') {
            Some(p) => &stmt[p + 1..],
            None => stmt,
        };
        let mut toks: Vec<&str> = stmt
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|t| !t.is_empty())
            .collect();
        // Drop layout(...) and precision/interpolation qualifiers.
        if let Some(p) = toks.iter().position(|t| t.starts_with("layout")) {
            let end = toks
                .iter()
                .skip(p)
                .position(|t| t.contains(')'))
                .map_or(p, |e| p + e);
            toks.drain(p..=end);
        }
        let Some(q) = toks.iter().position(|t| qualifiers.contains(t)) else {
            continue;
        };
        let rest: Vec<&str> = toks[q + 1..]
            .iter()
            .copied()
            .filter(|t| {
                !matches!(
                    *t,
                    "highp" | "mediump" | "lowp" | "flat" | "smooth" | "centroid"
                )
            })
            .collect();
        let Some((ty, names)) = rest.split_first() else {
            continue;
        };
        let Some(gt) = glsl_type(ty) else { continue };
        for raw in names {
            let (name, size) = match raw.find('[') {
                Some(b) => {
                    let n = raw[b + 1..].trim_end_matches(']').parse().unwrap_or(1);
                    (&raw[..b], n)
                }
                None => (*raw, 1),
            };
            if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                out.push((name.to_owned(), gt, size));
            }
        }
    }
    out
}

fn is_used(src: &str, name: &str) -> bool {
    let src = strip_comments(src);
    let mut count = 0;
    let bytes = src.as_bytes();
    let mut start = 0;
    while let Some(p) = src[start..].find(name) {
        let i = start + p;
        let before = i.checked_sub(1).map(|j| bytes[j]);
        let after = bytes.get(i + name.len()).copied();
        let ident = |c: Option<u8>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_');
        if !ident(before) && !ident(after) {
            count += 1;
        }
        start = i + name.len();
    }
    count > 1
}

/// A constant `vec4(a, b, c, d)` written to the fragment output.
fn constant_frag_color(src: &str) -> Option<[f32; 4]> {
    let src = strip_comments(src);
    let p = src.find("gl_FragColor").or_else(|| {
        declarations(&src, &["out"])
            .first()
            .and_then(|(n, ..)| src.rfind(n.as_str()))
    })?;
    let rest = &src[p..];
    let eq = rest.find('=')?;
    let stmt = &rest[eq + 1..rest.find(';')?];
    let v = stmt.trim().strip_prefix("vec4")?.trim();
    let inner = v.strip_prefix('(')?.strip_suffix(')')?;
    let nums: Vec<f32> = inner
        .split(',')
        .map(|t| t.trim().trim_end_matches('f').parse::<f32>())
        .collect::<Result<_, _>>()
        .ok()?;
    match nums.len() {
        4 => Some([nums[0], nums[1], nums[2], nums[3]]),
        1 => Some([nums[0]; 4]),
        _ => None,
    }
}

fn fnv(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

// ---------------------------------------------------------------------------
// Pixels
// ---------------------------------------------------------------------------

impl Gl {
    fn ensure_fb(&mut self) -> &mut Vec<u8> {
        let len = self.width as usize * self.height as usize * 4;
        self.fb.get_or_insert_with(|| vec![0; len])
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.width = w;
        self.height = h;
        self.fb = None;
    }

    fn clear(&mut self, mask: u32) {
        if mask & COLOR_BUFFER_BIT == 0 || self.bound.contains_key(&0x8CA6) {
            return;
        }
        let (w, h) = (self.width as i32, self.height as i32);
        let c = self.clear_color;
        let a = c[3].clamp(0.0, 1.0);
        let px: [u8; 4] = std::array::from_fn(|i| (c[i].clamp(0.0, 1.0) * 255.0).round() as u8);
        let px = if self.attrs.alpha {
            px
        } else {
            [px[0], px[1], px[2], 255]
        };
        let _ = a;
        let (x0, y0, x1, y1) = if self.enabled.contains(&SCISSOR_TEST) {
            let s = self.scissor;
            (
                s[0].max(0),
                s[1].max(0),
                (s[0] + s[2]).min(w),
                (s[1] + s[3]).min(h),
            )
        } else {
            (0, 0, w, h)
        };
        let mask_c = self.color_mask;
        let width = self.width as usize;
        let fb = self.ensure_fb();
        for gy in y0..y1 {
            let row = (h - 1 - gy) as usize;
            for x in x0..x1 {
                let i = (row * width + x as usize) * 4;
                for k in 0..4 {
                    if mask_c[k] {
                        fb[i + k] = px[k];
                    }
                }
            }
        }
    }

    /// Clip-space x/y of vertex `i` from the position attribute.
    fn positions(&self) -> Option<(Vec<u8>, Attrib)> {
        let a = self
            .attribs
            .iter()
            .find(|a| a.enabled && a.buffer.is_some() && a.ty == FLOAT && a.size >= 2)?;
        let buf = self.res.get(&a.buffer?)?;
        Some((buf.bytes.clone(), *a))
    }

    fn draw_color(&self) -> [u8; 4] {
        let prog = self.bound.get(&0x8B8D).copied();
        if let Some(p) = prog {
            if let Some((_, v)) = self
                .uniforms
                .iter()
                .filter(|((pp, _), v)| *pp == p && v.len() == 4)
                .min_by(|a, b| a.0.1.cmp(&b.0.1))
            {
                return std::array::from_fn(|i| (v[i].clamp(0.0, 1.0) * 255.0).round() as u8);
            }
            if let Some(r) = self.res.get(&p) {
                if let Some(c) = r.frag_color {
                    return std::array::from_fn(|i| (c[i].clamp(0.0, 1.0) * 255.0).round() as u8);
                }
                let h = r.frag_hash;
                return [(h >> 16) as u8, (h >> 8) as u8, h as u8, 255];
            }
        }
        [255, 255, 255, 255]
    }

    fn draw(&mut self, mode: u32, indices: &[u32]) {
        if self.bound.contains_key(&0x8CA6)
            || self.enabled.contains(&0x8C89)
            || self.width == 0
            || self.height == 0
        {
            return;
        }
        let Some((bytes, a)) = self.positions() else {
            return;
        };
        let stride = if a.stride == 0 { a.size * 4 } else { a.stride } as usize;
        let vp = self.viewport;
        let h = self.height as f32;
        let pt = |i: u32| -> Option<(f32, f32)> {
            let o = a.offset as usize + i as usize * stride;
            let f = |k: usize| -> Option<f32> {
                let b = bytes.get(o + k * 4..o + k * 4 + 4)?;
                Some(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            };
            let (x, y) = (f(0)?, f(1)?);
            let wx = vp[0] as f32 + (x + 1.0) / 2.0 * vp[2] as f32;
            let wy = vp[1] as f32 + (y + 1.0) / 2.0 * vp[3] as f32;
            Some((wx, h - wy))
        };
        let pts: Vec<(f32, f32)> = indices.iter().filter_map(|i| pt(*i)).collect();
        let color = self.draw_color();
        let mut pb = tiny_skia::PathBuilder::new();
        let mut fill = true;
        let tri = |pb: &mut tiny_skia::PathBuilder, a: (f32, f32), b: (f32, f32), c: (f32, f32)| {
            pb.move_to(a.0, a.1);
            pb.line_to(b.0, b.1);
            pb.line_to(c.0, c.1);
            pb.close();
        };
        match mode {
            TRIANGLES => {
                for t in pts.chunks_exact(3) {
                    tri(&mut pb, t[0], t[1], t[2]);
                }
            }
            TRIANGLE_STRIP => {
                for k in 2..pts.len() {
                    tri(&mut pb, pts[k - 2], pts[k - 1], pts[k]);
                }
            }
            TRIANGLE_FAN => {
                for k in 2..pts.len() {
                    tri(&mut pb, pts[0], pts[k - 1], pts[k]);
                }
            }
            LINES | LINE_STRIP | LINE_LOOP => {
                fill = false;
                if mode == LINES {
                    for l in pts.chunks_exact(2) {
                        pb.move_to(l[0].0, l[0].1);
                        pb.line_to(l[1].0, l[1].1);
                    }
                } else if let Some(first) = pts.first() {
                    pb.move_to(first.0, first.1);
                    for p in &pts[1..] {
                        pb.line_to(p.0, p.1);
                    }
                    if mode == LINE_LOOP {
                        pb.close();
                    }
                }
            }
            POINTS => {
                for p in &pts {
                    if let Some(r) = tiny_skia::Rect::from_xywh(p.0 - 0.5, p.1 - 0.5, 1.0, 1.0) {
                        pb.push_rect(r);
                    }
                }
            }
            _ => {
                self.set_error(INVALID_ENUM);
                return;
            }
        }
        let Some(path) = pb.finish() else { return };
        let (w, hh, aa) = (self.width, self.height, self.attrs.antialias);
        let alpha = self.attrs.alpha;
        let fb = self.ensure_fb();
        let Some(mut pm) = tiny_skia::PixmapMut::from_bytes(fb, w, hh) else {
            return;
        };
        let mut paint = tiny_skia::Paint::default();
        let a = if alpha { color[3] } else { 255 };
        paint.set_color_rgba8(color[0], color[1], color[2], a);
        paint.anti_alias = aa;
        if fill {
            pm.fill_path(
                &path,
                &paint,
                tiny_skia::FillRule::Winding,
                tiny_skia::Transform::identity(),
                None,
            );
        } else {
            let stroke = tiny_skia::Stroke {
                width: 1.0,
                ..Default::default()
            };
            pm.stroke_path(
                &path,
                &paint,
                &stroke,
                tiny_skia::Transform::identity(),
                None,
            );
        }
    }

    /// Unpremultiplied, noised RGBA rows top-down (what leaves the canvas).
    fn readback(&self) -> lb_paint::Bitmap {
        let mut bmp = lb_paint::Bitmap::new(self.width, self.height);
        let Some(fb) = &self.fb else { return bmp };
        bmp.data.copy_from_slice(fb);
        if self.attrs.premultiplied_alpha {
            for px in bmp.data.chunks_exact_mut(4) {
                let a = u32::from(px[3]);
                if a != 0 && a != 255 {
                    for c in &mut px[..3] {
                        *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                    }
                }
            }
        }
        if !self.attrs.alpha {
            for px in bmp.data.chunks_exact_mut(4) {
                px[3] = 255;
            }
        }
        if let Some(seed) = self.seed {
            lb_paint::CanvasNoise::new(seed).apply(
                fb,
                self.width,
                self.height,
                0,
                0,
                self.width,
                &mut bmp.data,
            );
        }
        bmp
    }
}

// ---------------------------------------------------------------------------
// Context factory
// ---------------------------------------------------------------------------

fn make_context(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Rc<Window>,
    target: CanvasTarget,
    kind: &str,
    attrs: Option<lb_js::Handle>,
) -> Result<Option<ExternalContext>, HostError> {
    let Some(profile) = w.profile.clone() else {
        return Ok(None);
    };
    let mut a = Attrs::default();
    if let Some(h) = attrs {
        let d = Dict(Some(h));
        for (key, slot) in [
            ("alpha", &mut a.alpha as &mut bool),
            ("antialias", &mut a.antialias),
            ("depth", &mut a.depth),
            ("desynchronized", &mut a.desynchronized),
            (
                "failIfMajorPerformanceCaveat",
                &mut a.fail_if_major_performance_caveat,
            ),
            ("premultipliedAlpha", &mut a.premultiplied_alpha),
            ("preserveDrawingBuffer", &mut a.preserve_drawing_buffer),
            ("stencil", &mut a.stencil),
            ("xrCompatible", &mut a.xr_compatible),
        ] {
            if let Some(v) = d.get(cx, key)? {
                *slot = super::truthy(&cx.handle_value(v));
                cx.release(v);
            }
        }
        if let Some(s) = d.string(cx, "powerPreference")? {
            a.power_preference = match s.as_str() {
                "low-power" => "low-power",
                "high-performance" => "high-performance",
                _ => "default",
            };
        }
        d.release(cx);
    }
    let webgl2 = kind == "webgl2";
    let (width, height) = canvas_size(w, target);
    let consts = constant_values();
    let params = profile
        .webgl
        .parameters
        .iter()
        .filter_map(|(k, v)| consts.get(k.as_str()).map(|c| (*c, v.clone())))
        .collect();
    let mut enabled = HashSet::new();
    enabled.insert(DITHER);
    let gl = Rc::new(RefCell::new(Gl {
        webgl2,
        target,
        obj: 0,
        attrs: a,
        width,
        height,
        fb: None,
        clear_color: [0.0; 4],
        color_mask: [true; 4],
        viewport: [0, 0, width as i32, height as i32],
        scissor: [0, 0, width as i32, height as i32],
        enabled,
        error: NO_ERROR,
        lost: false,
        extensions: extension_list(&profile, webgl2),
        seed: profile.fingerprint_seeds.webgl_bytes(),
        profile,
        params,
        ext_objs: HashMap::new(),
        state: HashMap::new(),
        bound: HashMap::new(),
        res: HashMap::new(),
        attribs: vec![Attrib::default(); 16],
        uniforms: HashMap::new(),
        active_texture: TEXTURE0,
    }));
    let id = new_gl(
        w,
        GlObj::Ctx {
            gl: gl.clone(),
            webgl2,
        },
    );
    gl.borrow_mut().obj = id;
    let value = cx.make_handle(w.obj_arg(id))?;
    let g1 = gl.clone();
    let g2 = gl;
    Ok(Some(ExternalContext {
        value,
        snapshot: Some(Rc::new(move || Some(g1.borrow().readback()))),
        resize: Some(Rc::new(move |ww, hh| g2.borrow_mut().resize(ww, hh))),
    }))
}

impl Runtime {
    fn register_webgl_factories(&self) {
        let f: ContextFactory = Rc::new(make_context);
        for kind in ["webgl", "experimental-webgl", "webgl2"] {
            self.set_canvas_context_factory(kind, f.clone());
        }
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Registers `f` for both context interfaces; the argument count is
/// checked against the IDL first.
fn gl_op(
    reg: &mut Registry,
    rt: &Runtime,
    name: &'static str,
    f: impl Fn(&mut CallCtx<'_, '_, '_>, &Rc<Window>, &Rc<RefCell<Gl>>) -> Result<(), HostError>
    + Clone
    + 'static,
) {
    for iface in IFACES {
        let Some(len) = crate::generated::chromium::TABLE
            .interface(iface)
            .and_then(|d| d.operations.iter().find(|o| o.name == name))
            .map(|o| usize::from(o.length))
        else {
            continue;
        };
        let f = f.clone();
        method(reg, rt, iface, name, move |cx, w| {
            super::need_args(cx, len, name, iface)?;
            let gl = this_gl(cx, w)?;
            f(cx, w, &gl)
        });
    }
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    rt.register_webgl_factories();
    register_defaults(reg, rt);
    register_attributes(reg, rt);
    register_queries(reg, rt);
    register_state(reg, rt);
    register_objects(reg, rt);
    register_drawing(reg, rt);
    register_helpers(reg, rt);
    register_extensions(reg, rt);
}

/// Every operation without its own implementation: argument check, then
/// a value of the right shape (`is*` false, `get*` null, others
/// undefined).
fn register_defaults(reg: &mut Registry, rt: &Runtime) {
    for iface in IFACES {
        let Some(d) = crate::generated::chromium::TABLE.interface(iface) else {
            continue;
        };
        for op in d.operations {
            let (name, len) = (op.name, usize::from(op.length));
            method(reg, rt, iface, name, move |cx, w| {
                super::need_args(cx, len, name, iface)?;
                let _ = this_gl(cx, w)?;
                if name.starts_with("is") {
                    cx.ret(false)
                } else if name.starts_with("get") {
                    cx.ret(Arg::Null)
                } else {
                    Ok(())
                }
            });
        }
    }
}

fn register_attributes(reg: &mut Registry, rt: &Runtime) {
    for iface in IFACES {
        getter(reg, rt, iface, "canvas", |cx, w| {
            let gl = this_gl(cx, w)?;
            let t = gl.borrow().target;
            match t {
                CanvasTarget::Element { slot, node } => cx.ret(w.node_arg(slot, node)),
                CanvasTarget::Offscreen { obj } => cx.ret(w.obj_arg(obj)),
            }
        });
        getter(reg, rt, iface, "drawingBufferWidth", |cx, w| {
            let gl = this_gl(cx, w)?;
            let v = gl.borrow().width;
            cx.ret(v)
        });
        getter(reg, rt, iface, "drawingBufferHeight", |cx, w| {
            let gl = this_gl(cx, w)?;
            let v = gl.borrow().height;
            cx.ret(v)
        });
        getter(reg, rt, iface, "drawingBufferFormat", |cx, w| {
            let _ = this_gl(cx, w)?;
            cx.ret(32856)
        });
        for attr in ["drawingBufferColorSpace", "unpackColorSpace"] {
            getter(reg, rt, iface, attr, move |cx, w| {
                let gl = this_gl(cx, w)?;
                let v = match gl
                    .borrow()
                    .state
                    .get(&if attr == "unpackColorSpace" { 1 } else { 2 })
                {
                    Some(JsValue::String(s)) => s.clone(),
                    _ => "srgb".into(),
                };
                cx.ret(v)
            });
            setter(reg, rt, iface, attr, move |cx, w| {
                let gl = this_gl(cx, w)?;
                let s = cx.arg_string(0)?;
                if s == "srgb" || s == "display-p3" {
                    gl.borrow_mut().state.insert(
                        if attr == "unpackColorSpace" { 1 } else { 2 },
                        JsValue::String(s),
                    );
                }
                Ok(())
            });
        }
    }
}

fn register_queries(reg: &mut Registry, rt: &Runtime) {
    gl_op(reg, rt, "getParameter", |cx, w, gl| {
        let pname = cx.arg_u32(0)?;
        let v = {
            let g = gl.borrow();
            if g.lost {
                None
            } else {
                Some(g.parameter(w, pname))
            }
        };
        match v {
            Some(Some(a)) => cx.ret(a),
            Some(None) => {
                gl.borrow_mut().set_error(INVALID_ENUM);
                cx.ret(Arg::Null)
            }
            None => cx.ret(Arg::Null),
        }
    });
    gl_op(reg, rt, "getError", |cx, _w, gl| {
        let e = std::mem::replace(&mut gl.borrow_mut().error, NO_ERROR);
        cx.ret(e)
    });
    gl_op(reg, rt, "isContextLost", |cx, _w, gl| {
        let v = gl.borrow().lost;
        cx.ret(v)
    });
    gl_op(reg, rt, "getContextAttributes", |cx, _w, gl| {
        let g = gl.borrow();
        if g.lost {
            drop(g);
            return cx.ret(Arg::Null);
        }
        let a = g.attrs;
        drop(g);
        cx.ret(Arg::Json(format!(
            "{{\"alpha\":{},\"antialias\":{},\"depth\":{},\"desynchronized\":{},\"failIfMajorPerformanceCaveat\":{},\"powerPreference\":\"{}\",\"premultipliedAlpha\":{},\"preserveDrawingBuffer\":{},\"stencil\":{},\"xrCompatible\":{}}}",
            a.alpha,
            a.antialias,
            a.depth,
            a.desynchronized,
            a.fail_if_major_performance_caveat,
            a.power_preference,
            a.premultiplied_alpha,
            a.preserve_drawing_buffer,
            a.stencil,
            a.xr_compatible
        )))
    });
    gl_op(reg, rt, "getSupportedExtensions", |cx, _w, gl| {
        let g = gl.borrow();
        if g.lost {
            drop(g);
            return cx.ret(Arg::Null);
        }
        let list: Vec<Arg> = g
            .extensions
            .iter()
            .map(|s| Arg::String((*s).into()))
            .collect();
        drop(g);
        cx.ret(Arg::Array(list))
    });
    gl_op(reg, rt, "getExtension", |cx, w, gl| {
        let name = cx.arg_string(0)?;
        let (found, existing) = {
            let g = gl.borrow();
            if g.lost {
                (None, None)
            } else {
                let found = g
                    .extensions
                    .iter()
                    .find(|e| e.eq_ignore_ascii_case(&name))
                    .copied();
                (found, found.and_then(|f| g.ext_objs.get(f).copied()))
            }
        };
        let Some(found) = found else {
            return cx.ret(Arg::Null);
        };
        if let Some(id) = existing {
            return cx.ret(w.obj_arg(id));
        }
        let Some(desc) = ext_desc(found) else {
            return cx.ret(Arg::Null);
        };
        let id = new_gl(
            w,
            GlObj::Ext {
                iface: desc.iface,
                ctx: gl.clone(),
            },
        );
        gl.borrow_mut().ext_objs.insert(found, id);
        cx.ret(w.obj_arg(id))
    });
    gl_op(reg, rt, "getShaderPrecisionFormat", |cx, w, gl| {
        let st = cx.arg_u32(0)?;
        let pt = cx.arg_u32(1)?;
        let sname = match st {
            VERTEX_SHADER => "VERTEX_SHADER",
            FRAGMENT_SHADER => "FRAGMENT_SHADER",
            _ => {
                gl.borrow_mut().set_error(INVALID_ENUM);
                return cx.ret(Arg::Null);
            }
        };
        let (pname, int) = match pt {
            0x8DF0 => ("LOW_FLOAT", false),
            0x8DF1 => ("MEDIUM_FLOAT", false),
            0x8DF2 => ("HIGH_FLOAT", false),
            0x8DF3 => ("LOW_INT", true),
            0x8DF4 => ("MEDIUM_INT", true),
            0x8DF5 => ("HIGH_INT", true),
            _ => {
                gl.borrow_mut().set_error(INVALID_ENUM);
                return cx.ret(Arg::Null);
            }
        };
        let key = format!("{sname}.{pname}");
        let v = {
            let g = gl.borrow();
            g.profile
                .webgl
                .shader_precision
                .get(&key)
                .map(|p| [p.range_min, p.range_max, p.precision])
        };
        let v = v.unwrap_or(if int { [31, 30, 0] } else { [127, 127, 23] });
        let id = new_gl(w, GlObj::Precision(v));
        cx.ret(w.obj_arg(id))
    });
    for (attr, k) in [("rangeMin", 0), ("rangeMax", 1), ("precision", 2)] {
        getter(reg, rt, "WebGLShaderPrecisionFormat", attr, move |cx, w| {
            let id = this_obj(cx)?;
            let v = with_gl(w, id, |g| match g {
                GlObj::Precision(p) => Some(p[k]),
                _ => None,
            })
            .ok_or_else(illegal)?;
            cx.ret(v)
        });
    }
    for attr in ["name", "size", "type"] {
        getter(reg, rt, "WebGLActiveInfo", attr, move |cx, w| {
            let id = this_obj(cx)?;
            let v = with_gl(w, id, |g| match g {
                GlObj::ActiveInfo { name, size, ty } => Some(match attr {
                    "name" => Arg::String(name.clone()),
                    "size" => Arg::Number(f64::from(*size)),
                    _ => Arg::Number(f64::from(*ty)),
                }),
                _ => None,
            })
            .ok_or_else(illegal)?;
            cx.ret(v)
        });
    }
}

fn set_state(gl: &Rc<RefCell<Gl>>, pname: u32, v: JsValue) {
    gl.borrow_mut().state.insert(pname, v);
}

fn register_state(reg: &mut Registry, rt: &Runtime) {
    gl_op(reg, rt, "enable", |cx, _w, gl| {
        let cap = cx.arg_u32(0)?;
        gl.borrow_mut().enabled.insert(cap);
        Ok(())
    });
    gl_op(reg, rt, "disable", |cx, _w, gl| {
        let cap = cx.arg_u32(0)?;
        gl.borrow_mut().enabled.remove(&cap);
        Ok(())
    });
    gl_op(reg, rt, "isEnabled", |cx, _w, gl| {
        let cap = cx.arg_u32(0)?;
        let v = gl.borrow().enabled.contains(&cap);
        cx.ret(v)
    });
    gl_op(reg, rt, "clearColor", |cx, _w, gl| {
        let mut c = [0f32; 4];
        for (i, v) in c.iter_mut().enumerate() {
            *v = cx.arg_f64(i)? as f32;
        }
        gl.borrow_mut().clear_color = c;
        Ok(())
    });
    gl_op(reg, rt, "colorMask", |cx, _w, gl| {
        let m: [bool; 4] = std::array::from_fn(|i| cx.arg_bool(i));
        gl.borrow_mut().color_mask = m;
        Ok(())
    });
    gl_op(reg, rt, "viewport", |cx, _w, gl| {
        let mut v = [0i32; 4];
        for (i, x) in v.iter_mut().enumerate() {
            *x = cx.arg_i32(i)?;
        }
        if v[2] < 0 || v[3] < 0 {
            gl.borrow_mut().set_error(INVALID_VALUE);
            return Ok(());
        }
        gl.borrow_mut().viewport = v;
        Ok(())
    });
    gl_op(reg, rt, "scissor", |cx, _w, gl| {
        let mut v = [0i32; 4];
        for (i, x) in v.iter_mut().enumerate() {
            *x = cx.arg_i32(i)?;
        }
        if v[2] < 0 || v[3] < 0 {
            gl.borrow_mut().set_error(INVALID_VALUE);
            return Ok(());
        }
        gl.borrow_mut().scissor = v;
        Ok(())
    });
    gl_op(reg, rt, "activeTexture", |cx, _w, gl| {
        let t = cx.arg_u32(0)?;
        if !(TEXTURE0..TEXTURE0 + 32).contains(&t) {
            gl.borrow_mut().set_error(INVALID_ENUM);
            return Ok(());
        }
        gl.borrow_mut().active_texture = t;
        Ok(())
    });
    // Single-value state setters: (method, [pnames per argument]).
    let simple: &[(&'static str, &'static [u32])] = &[
        ("blendEquation", &[0x8009, 0x883D]),
        ("blendEquationSeparate", &[0x8009, 0x883D]),
        ("blendFunc", &[0x80C9, 0x80C8]),
        ("blendFuncSeparate", &[0x80C9, 0x80C8, 0x80CB, 0x80CA]),
        ("cullFace", &[0x0B45]),
        ("frontFace", &[0x0B46]),
        ("depthFunc", &[0x0B74]),
        ("clearDepth", &[0x0B73]),
        ("clearStencil", &[0x0B91]),
        ("lineWidth", &[0x0B21]),
        ("polygonOffset", &[0x8038, 0x2A00]),
        ("stencilMask", &[0x0B98, 0x8CA5]),
        ("stencilFunc", &[0x0B92, 0x0B97, 0x0B93]),
        ("stencilOp", &[0x0B94, 0x0B95, 0x0B96]),
        ("readBuffer", &[0x0C02]),
    ];
    for (name, pnames) in simple {
        let pnames = *pnames;
        let name = *name;
        gl_op(reg, rt, name, move |cx, _w, gl| {
            for (i, p) in pnames.iter().enumerate() {
                let arg = if name == "blendEquation" || name == "stencilMask" {
                    0
                } else {
                    i
                };
                let v = cx.arg_f64(arg)?;
                set_state(gl, *p, JsValue::Number(v));
                if name == "stencilFunc" || name == "stencilOp" {
                    set_state(gl, back_of(*p), JsValue::Number(v));
                }
            }
            Ok(())
        });
    }
    gl_op(reg, rt, "depthMask", |cx, _w, gl| {
        let v = cx.arg_bool(0);
        set_state(gl, 0x0B72, JsValue::Bool(v));
        Ok(())
    });
    gl_op(reg, rt, "sampleCoverage", |cx, _w, gl| {
        let v = cx.arg_f64(0)?;
        let inv = cx.arg_bool(1);
        set_state(gl, 0x80AA, JsValue::Number(v.clamp(0.0, 1.0)));
        set_state(gl, 0x80AB, JsValue::Bool(inv));
        Ok(())
    });
    gl_op(reg, rt, "blendColor", |cx, _w, gl| {
        let mut c = [0f64; 4];
        for (i, v) in c.iter_mut().enumerate() {
            *v = cx.arg_f64(i)?;
        }
        set_state(
            gl,
            0x8005,
            JsValue::Json(format!("[{},{},{},{}]", c[0], c[1], c[2], c[3])),
        );
        Ok(())
    });
    gl_op(reg, rt, "depthRange", |cx, _w, gl| {
        let a = cx.arg_f64(0)?.clamp(0.0, 1.0);
        let b = cx.arg_f64(1)?.clamp(0.0, 1.0);
        set_state(gl, 0x0B70, JsValue::Json(format!("[{a},{b}]")));
        Ok(())
    });
    gl_op(reg, rt, "pixelStorei", |cx, _w, gl| {
        let p = cx.arg_u32(0)?;
        let v = cx.arg_i32(1)?;
        let val = match p {
            0x9240 | 0x9241 => JsValue::Bool(v != 0),
            _ => JsValue::Number(f64::from(v)),
        };
        set_state(gl, p, val);
        Ok(())
    });
    gl_op(reg, rt, "hint", |cx, _w, gl| {
        let t = cx.arg_u32(0)?;
        let m = cx.arg_u32(1)?;
        set_state(gl, t, JsValue::Number(f64::from(m)));
        Ok(())
    });
    gl_op(reg, rt, "finish", |_cx, _w, _gl| Ok(()));
    gl_op(reg, rt, "flush", |_cx, _w, _gl| Ok(()));
}

fn back_of(p: u32) -> u32 {
    match p {
        0x0B92 => 0x8800,
        0x0B97 => 0x8CA3,
        0x0B93 => 0x8CA4,
        0x0B94 => 0x8801,
        0x0B95 => 0x8802,
        0x0B96 => 0x8803,
        other => other,
    }
}

fn new_res(w: &Window, gl: &Rc<RefCell<Gl>>, kind: ResKind, oes: bool) -> u32 {
    let id = new_gl(
        w,
        GlObj::Res {
            ctx: gl.clone(),
            kind,
            oes,
        },
    );
    gl.borrow_mut().res.insert(id, Res::default());
    id
}

fn res_kind(w: &Window, id: u32) -> Option<ResKind> {
    with_gl(w, id, |g| match g {
        GlObj::Res { kind, .. } => Some(*kind),
        _ => None,
    })
}

fn register_objects(reg: &mut Registry, rt: &Runtime) {
    let creators: &[(&'static str, ResKind)] = &[
        ("createBuffer", ResKind::Buffer),
        ("createTexture", ResKind::Texture),
        ("createProgram", ResKind::Program),
        ("createFramebuffer", ResKind::Framebuffer),
        ("createRenderbuffer", ResKind::Renderbuffer),
        ("createVertexArray", ResKind::VertexArray),
        ("createQuery", ResKind::Query),
        ("createSampler", ResKind::Sampler),
        ("createTransformFeedback", ResKind::TransformFeedback),
    ];
    for (name, kind) in creators {
        let kind = *kind;
        gl_op(reg, rt, name, move |cx, w, gl| {
            if gl.borrow().lost {
                return cx.ret(Arg::Null);
            }
            let id = new_res(w, gl, kind, false);
            cx.ret(w.obj_arg(id))
        });
    }
    gl_op(reg, rt, "createShader", |cx, w, gl| {
        let t = cx.arg_u32(0)?;
        if t != VERTEX_SHADER && t != FRAGMENT_SHADER {
            gl.borrow_mut().set_error(INVALID_ENUM);
            return cx.ret(Arg::Null);
        }
        let id = new_res(w, gl, ResKind::Shader, false);
        if let Some(r) = gl.borrow_mut().res.get_mut(&id) {
            r.shader_type = t;
        }
        cx.ret(w.obj_arg(id))
    });
    gl_op(reg, rt, "fenceSync", |cx, w, gl| {
        let id = new_res(w, gl, ResKind::Sync, false);
        cx.ret(w.obj_arg(id))
    });
    gl_op(reg, rt, "clientWaitSync", |cx, _w, _gl| {
        cx.ret(ALREADY_SIGNALED)
    });
    gl_op(reg, rt, "getSyncParameter", |cx, _w, _gl| {
        let p = cx.arg_u32(1)?;
        cx.ret(if p == SYNC_STATUS { SIGNALED } else { 0 })
    });
    let deleters: &[(&'static str, ResKind)] = &[
        ("deleteBuffer", ResKind::Buffer),
        ("deleteTexture", ResKind::Texture),
        ("deleteProgram", ResKind::Program),
        ("deleteShader", ResKind::Shader),
        ("deleteFramebuffer", ResKind::Framebuffer),
        ("deleteRenderbuffer", ResKind::Renderbuffer),
        ("deleteVertexArray", ResKind::VertexArray),
        ("deleteQuery", ResKind::Query),
        ("deleteSampler", ResKind::Sampler),
        ("deleteSync", ResKind::Sync),
        ("deleteTransformFeedback", ResKind::TransformFeedback),
    ];
    for (name, kind) in deleters {
        let kind = *kind;
        gl_op(reg, rt, name, move |cx, w, gl| {
            if let Some(id) = arg_res(cx, w, 0, gl)
                && res_kind(w, id) == Some(kind)
            {
                let mut g = gl.borrow_mut();
                if let Some(r) = g.res.get_mut(&id) {
                    r.deleted = true;
                }
                g.bound.retain(|_, v| *v != id);
            }
            Ok(())
        });
    }
    let checks: &[(&'static str, ResKind)] = &[
        ("isBuffer", ResKind::Buffer),
        ("isTexture", ResKind::Texture),
        ("isProgram", ResKind::Program),
        ("isShader", ResKind::Shader),
        ("isFramebuffer", ResKind::Framebuffer),
        ("isRenderbuffer", ResKind::Renderbuffer),
        ("isVertexArray", ResKind::VertexArray),
        ("isQuery", ResKind::Query),
        ("isSampler", ResKind::Sampler),
        ("isSync", ResKind::Sync),
        ("isTransformFeedback", ResKind::TransformFeedback),
    ];
    for (name, kind) in checks {
        let kind = *kind;
        gl_op(reg, rt, name, move |cx, w, gl| {
            let ok = arg_res(cx, w, 0, gl).is_some_and(|id| {
                res_kind(w, id) == Some(kind)
                    && gl.borrow().res.get(&id).is_some_and(|r| !r.deleted)
            });
            cx.ret(ok)
        });
    }
    // Bindings.
    let binders: &[(&'static str, fn(u32) -> Option<u32>)] = &[
        ("bindBuffer", |t| match t {
            ARRAY_BUFFER => Some(0x8894),
            ELEMENT_ARRAY_BUFFER => Some(0x8895),
            0x8F36 => Some(0x8F36),
            0x8F37 => Some(0x8F37),
            0x88EB => Some(0x88ED),
            0x88EC => Some(0x88EF),
            0x8A11 => Some(0x8A28),
            0x8C8E => Some(0x8C8F),
            _ => None,
        }),
        ("bindTexture", |t| match t {
            0x0DE1 => Some(0x8069),
            0x8513 => Some(0x8514),
            0x8C1A => Some(0x8C1D),
            0x806F => Some(0x806A),
            _ => None,
        }),
        ("bindFramebuffer", |t| match t {
            0x8D40 | 0x8CA9 => Some(0x8CA6),
            0x8CA8 => Some(0x8CAA),
            _ => None,
        }),
        ("bindRenderbuffer", |t| (t == 0x8D41).then_some(0x8CA7)),
        ("bindTransformFeedback", |t| (t == 0x8E22).then_some(0x8E25)),
    ];
    for (name, map) in binders {
        let map = *map;
        let name = *name;
        gl_op(reg, rt, name, move |cx, w, gl| {
            let target = cx.arg_u32(0)?;
            let Some(pname) = map(target) else {
                gl.borrow_mut().set_error(INVALID_ENUM);
                return Ok(());
            };
            let obj = arg_res(cx, w, 1, gl);
            let mut g = gl.borrow_mut();
            match obj {
                Some(id) => {
                    g.bound.insert(pname, id);
                    if name == "bindFramebuffer" && target == 0x8D40 {
                        g.bound.insert(0x8CAA, id);
                    }
                }
                None => {
                    g.bound.remove(&pname);
                    if name == "bindFramebuffer" && target == 0x8D40 {
                        g.bound.remove(&0x8CAA);
                    }
                }
            }
            Ok(())
        });
    }
    gl_op(reg, rt, "bindVertexArray", |cx, w, gl| {
        let obj = arg_res(cx, w, 0, gl);
        let mut g = gl.borrow_mut();
        match obj {
            Some(id) => g.bound.insert(0x85B5, id),
            None => g.bound.remove(&0x85B5),
        };
        Ok(())
    });
    gl_op(reg, rt, "bindSampler", |cx, w, gl| {
        let obj = arg_res(cx, w, 1, gl);
        let mut g = gl.borrow_mut();
        match obj {
            Some(id) => g.bound.insert(0x8919, id),
            None => g.bound.remove(&0x8919),
        };
        Ok(())
    });
    gl_op(reg, rt, "useProgram", |cx, w, gl| {
        let obj = arg_res(cx, w, 0, gl);
        let mut g = gl.borrow_mut();
        match obj {
            Some(id) if g.res.get(&id).is_some_and(|r| r.linked) => {
                g.bound.insert(0x8B8D, id);
            }
            Some(_) => g.set_error(INVALID_OPERATION),
            None => {
                g.bound.remove(&0x8B8D);
            }
        }
        Ok(())
    });
    gl_op(reg, rt, "checkFramebufferStatus", |cx, _w, _gl| {
        cx.ret(FRAMEBUFFER_COMPLETE)
    });
    // Buffers.
    gl_op(reg, rt, "bufferData", |cx, _w, gl| {
        let target = cx.arg_u32(0)?;
        let pname = match target {
            ARRAY_BUFFER => 0x8894,
            ELEMENT_ARRAY_BUFFER => 0x8895,
            _ => target,
        };
        let usage = cx.arg_u32(2)?;
        let bytes = if cx.arg_kind(1) == ValueKind::Number {
            let n = cx.arg_f64(1)?;
            if n < 0.0 {
                gl.borrow_mut().set_error(INVALID_VALUE);
                return Ok(());
            }
            vec![0u8; n as usize]
        } else {
            match cx.arg_bytes(1) {
                Some(b) => b,
                None => {
                    gl.borrow_mut().set_error(INVALID_VALUE);
                    return Ok(());
                }
            }
        };
        let mut g = gl.borrow_mut();
        let Some(id) = g.bound.get(&pname).copied() else {
            g.set_error(INVALID_OPERATION);
            return Ok(());
        };
        if let Some(r) = g.res.get_mut(&id) {
            r.bytes = bytes;
            r.usage = usage;
        }
        Ok(())
    });
    gl_op(reg, rt, "bufferSubData", |cx, _w, gl| {
        let target = cx.arg_u32(0)?;
        let pname = match target {
            ARRAY_BUFFER => 0x8894,
            ELEMENT_ARRAY_BUFFER => 0x8895,
            _ => target,
        };
        let off = cx.arg_f64(1)?.max(0.0) as usize;
        let Some(src) = cx.arg_bytes(2) else {
            return Ok(());
        };
        let mut g = gl.borrow_mut();
        let Some(id) = g.bound.get(&pname).copied() else {
            g.set_error(INVALID_OPERATION);
            return Ok(());
        };
        let bad = match g.res.get_mut(&id) {
            Some(r) if off + src.len() <= r.bytes.len() => {
                r.bytes[off..off + src.len()].copy_from_slice(&src);
                false
            }
            _ => true,
        };
        if bad {
            g.set_error(INVALID_VALUE);
        }
        Ok(())
    });
    gl_op(reg, rt, "getBufferParameter", |cx, _w, gl| {
        let target = cx.arg_u32(0)?;
        let p = cx.arg_u32(1)?;
        let pname = match target {
            ARRAY_BUFFER => 0x8894,
            ELEMENT_ARRAY_BUFFER => 0x8895,
            _ => target,
        };
        let g = gl.borrow();
        let r = g.bound.get(&pname).and_then(|id| g.res.get(id));
        let v = match (r, p) {
            (Some(r), BUFFER_SIZE) => Arg::Number(r.bytes.len() as f64),
            (Some(r), BUFFER_USAGE) => {
                Arg::Number(f64::from(if r.usage == 0 { STATIC_DRAW } else { r.usage }))
            }
            _ => Arg::Null,
        };
        drop(g);
        cx.ret(v)
    });
    // Shaders and programs.
    gl_op(reg, rt, "shaderSource", |cx, w, gl| {
        let src = cx.arg_string(1)?;
        if let Some(id) = arg_res(cx, w, 0, gl)
            && let Some(r) = gl.borrow_mut().res.get_mut(&id)
        {
            r.source = src;
        }
        Ok(())
    });
    gl_op(reg, rt, "getShaderSource", |cx, w, gl| {
        let src =
            arg_res(cx, w, 0, gl).and_then(|id| gl.borrow().res.get(&id).map(|r| r.source.clone()));
        match src {
            Some(s) => cx.ret(s),
            None => cx.ret(Arg::Null),
        }
    });
    gl_op(reg, rt, "compileShader", |cx, w, gl| {
        if let Some(id) = arg_res(cx, w, 0, gl)
            && let Some(r) = gl.borrow_mut().res.get_mut(&id)
        {
            r.compiled = strip_comments(&r.source).contains("main");
        }
        Ok(())
    });
    gl_op(reg, rt, "getShaderParameter", |cx, w, gl| {
        let p = cx.arg_u32(1)?;
        let r = arg_res(cx, w, 0, gl).and_then(|id| {
            gl.borrow()
                .res
                .get(&id)
                .map(|r| (r.compiled, r.shader_type, r.deleted))
        });
        let Some((compiled, ty, deleted)) = r else {
            return cx.ret(Arg::Null);
        };
        match p {
            COMPILE_STATUS => cx.ret(compiled),
            SHADER_TYPE => cx.ret(ty),
            DELETE_STATUS => cx.ret(deleted),
            _ => {
                gl.borrow_mut().set_error(INVALID_ENUM);
                cx.ret(Arg::Null)
            }
        }
    });
    gl_op(reg, rt, "getShaderInfoLog", |cx, w, gl| {
        let r = arg_res(cx, w, 0, gl).and_then(|id| {
            gl.borrow()
                .res
                .get(&id)
                .map(|r| (r.compiled, r.source.is_empty()))
        });
        match r {
            Some((true, _)) => cx.ret(""),
            Some((false, _)) => cx.ret("ERROR: 0:1: '' : No main() function\n"),
            None => cx.ret(Arg::Null),
        }
    });
    gl_op(reg, rt, "attachShader", |cx, w, gl| {
        let (Some(p), Some(s)) = (arg_res(cx, w, 0, gl), arg_res(cx, w, 1, gl)) else {
            gl.borrow_mut().set_error(INVALID_VALUE);
            return Ok(());
        };
        let mut g = gl.borrow_mut();
        let ty = g.res.get(&s).map_or(0, |r| r.shader_type);
        let clash = g.res.get(&p).is_some_and(|r| r.shaders.contains(&s));
        if clash {
            g.set_error(INVALID_OPERATION);
            return Ok(());
        }
        let _ = ty;
        if let Some(r) = g.res.get_mut(&p) {
            r.shaders.push(s);
        }
        Ok(())
    });
    gl_op(reg, rt, "detachShader", |cx, w, gl| {
        if let (Some(p), Some(s)) = (arg_res(cx, w, 0, gl), arg_res(cx, w, 1, gl))
            && let Some(r) = gl.borrow_mut().res.get_mut(&p)
        {
            r.shaders.retain(|x| *x != s);
        }
        Ok(())
    });
    gl_op(reg, rt, "getAttachedShaders", |cx, w, gl| {
        let list =
            arg_res(cx, w, 0, gl).and_then(|p| gl.borrow().res.get(&p).map(|r| r.shaders.clone()));
        match list {
            Some(l) => cx.ret(Arg::Array(l.into_iter().map(|id| w.obj_arg(id)).collect())),
            None => cx.ret(Arg::Null),
        }
    });
    gl_op(reg, rt, "bindAttribLocation", |cx, w, gl| {
        let loc = cx.arg_i32(1)?;
        let name = cx.arg_string(2)?;
        if let Some(p) = arg_res(cx, w, 0, gl)
            && let Some(r) = gl.borrow_mut().res.get_mut(&p)
        {
            r.bound_locations.insert(name, loc);
        }
        Ok(())
    });
    gl_op(reg, rt, "linkProgram", |cx, w, gl| {
        let Some(p) = arg_res(cx, w, 0, gl) else {
            return Ok(());
        };
        let mut g = gl.borrow_mut();
        let shaders: Vec<(u32, String, bool)> = g
            .res
            .get(&p)
            .map(|r| {
                r.shaders
                    .iter()
                    .filter_map(|s| {
                        g.res
                            .get(s)
                            .map(|x| (x.shader_type, x.source.clone(), x.compiled))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let vs = shaders.iter().find(|s| s.0 == VERTEX_SHADER);
        let fs = shaders.iter().find(|s| s.0 == FRAGMENT_SHADER);
        let linked = matches!((vs, fs), (Some(v), Some(f)) if v.2 && f.2);
        let (vsrc, fsrc) = (
            vs.map(|s| s.1.clone()).unwrap_or_default(),
            fs.map(|s| s.1.clone()).unwrap_or_default(),
        );
        let attribs: Vec<Var> = declarations(&vsrc, &["attribute", "in"])
            .into_iter()
            .filter(|(n, ..)| is_used(&vsrc, n))
            .collect();
        let mut uniforms: Vec<Var> = Vec::new();
        for (src, other) in [(&vsrc, &fsrc), (&fsrc, &vsrc)] {
            for u in declarations(src, &["uniform"]) {
                if (is_used(src, &u.0) || is_used(other, &u.0))
                    && !uniforms.iter().any(|x| x.0 == u.0)
                {
                    uniforms.push(u);
                }
            }
        }
        let frag_color = constant_frag_color(&fsrc);
        let frag_hash = fnv(&fsrc);
        if let Some(r) = g.res.get_mut(&p) {
            let mut used: HashSet<i32> = r.bound_locations.values().copied().collect();
            let mut next = 0;
            r.attribs = attribs
                .into_iter()
                .map(|v| {
                    let loc = match r.bound_locations.get(&v.0) {
                        Some(l) => *l,
                        None => {
                            while used.contains(&next) {
                                next += 1;
                            }
                            used.insert(next);
                            next
                        }
                    };
                    (v, loc)
                })
                .collect();
            r.uniforms = uniforms;
            r.linked = linked;
            r.frag_color = frag_color;
            r.frag_hash = frag_hash;
        }
        Ok(())
    });
    gl_op(reg, rt, "validateProgram", |_cx, _w, _gl| Ok(()));
    gl_op(reg, rt, "getProgramParameter", |cx, w, gl| {
        let p = cx.arg_u32(1)?;
        let Some(id) = arg_res(cx, w, 0, gl) else {
            return cx.ret(Arg::Null);
        };
        let webgl2 = gl.borrow().webgl2;
        let v = {
            let g = gl.borrow();
            let Some(r) = g.res.get(&id) else {
                drop(g);
                return cx.ret(Arg::Null);
            };
            match p {
                LINK_STATUS => Some(Arg::Bool(r.linked)),
                DELETE_STATUS => Some(Arg::Bool(r.deleted)),
                VALIDATE_STATUS => Some(Arg::Bool(r.linked)),
                ATTACHED_SHADERS => Some(Arg::Number(r.shaders.len() as f64)),
                ACTIVE_ATTRIBUTES => Some(Arg::Number(r.attribs.len() as f64)),
                ACTIVE_UNIFORMS => Some(Arg::Number(r.uniforms.len() as f64)),
                TRANSFORM_FEEDBACK_BUFFER_MODE if webgl2 => {
                    Some(Arg::Number(f64::from(SEPARATE_ATTRIBS)))
                }
                TRANSFORM_FEEDBACK_VARYINGS | ACTIVE_UNIFORM_BLOCKS if webgl2 => {
                    Some(Arg::Number(0.0))
                }
                _ => None,
            }
        };
        match v {
            Some(a) => cx.ret(a),
            None => {
                gl.borrow_mut().set_error(INVALID_ENUM);
                cx.ret(Arg::Null)
            }
        }
    });
    gl_op(reg, rt, "getProgramInfoLog", |cx, w, gl| {
        let r = arg_res(cx, w, 0, gl).and_then(|id| gl.borrow().res.get(&id).map(|r| r.linked));
        match r {
            Some(_) => cx.ret(""),
            None => cx.ret(Arg::Null),
        }
    });
    gl_op(reg, rt, "getAttribLocation", |cx, w, gl| {
        let name = cx.arg_string(1)?;
        let loc = arg_res(cx, w, 0, gl).and_then(|id| {
            gl.borrow().res.get(&id).and_then(|r| {
                r.linked
                    .then(|| r.attribs.iter().find(|(v, _)| v.0 == name).map(|(_, l)| *l))
                    .flatten()
            })
        });
        cx.ret(loc.unwrap_or(-1))
    });
    gl_op(reg, rt, "getUniformLocation", |cx, w, gl| {
        let name = cx.arg_string(1)?;
        let Some(p) = arg_res(cx, w, 0, gl) else {
            return cx.ret(Arg::Null);
        };
        let base = name.split('[').next().unwrap_or("").to_owned();
        let ok = gl
            .borrow()
            .res
            .get(&p)
            .is_some_and(|r| r.linked && r.uniforms.iter().any(|u| u.0 == base));
        if !ok {
            return cx.ret(Arg::Null);
        }
        let id = new_gl(
            w,
            GlObj::Location {
                program: p,
                name: base,
            },
        );
        cx.ret(w.obj_arg(id))
    });
    for (op, attr) in [("getActiveAttrib", true), ("getActiveUniform", false)] {
        gl_op(reg, rt, op, move |cx, w, gl| {
            let i = cx.arg_u32(1)? as usize;
            let Some(p) = arg_res(cx, w, 0, gl) else {
                return cx.ret(Arg::Null);
            };
            let v = gl.borrow().res.get(&p).and_then(|r| {
                if attr {
                    r.attribs.get(i).map(|(v, _)| v.clone())
                } else {
                    r.uniforms.get(i).cloned()
                }
            });
            let Some((name, ty, size)) = v else {
                gl.borrow_mut().set_error(INVALID_VALUE);
                return cx.ret(Arg::Null);
            };
            let name = if size > 1 && !attr {
                format!("{name}[0]")
            } else {
                name
            };
            let id = new_gl(w, GlObj::ActiveInfo { name, size, ty });
            cx.ret(w.obj_arg(id))
        });
    }
    gl_op(reg, rt, "getUniform", |cx, w, gl| {
        let Some(h) = cx.arg_host(1).filter(|h| h.kind == K_OBJ) else {
            return cx.ret(Arg::Null);
        };
        let key = with_gl(w, h.id, |g| match g {
            GlObj::Location { program, name } => Some((*program, name.clone())),
            _ => None,
        });
        let v = key.and_then(|k| gl.borrow().uniforms.get(&k).cloned());
        match v {
            Some(v) if v.len() == 1 => cx.ret(f64::from(v[0])),
            Some(v) => cx.ret(Arg::Float32Array(v)),
            None => cx.ret(0),
        }
    });
}

/// Uniform setters store values (used for the draw color).
fn register_uniforms(reg: &mut Registry, rt: &Runtime) {
    let names: Vec<&'static str> = IFACES
        .iter()
        .filter_map(|i| crate::generated::chromium::TABLE.interface(i))
        .flat_map(|d| d.operations.iter().map(|o| o.name))
        .filter(|n| n.starts_with("uniform") && !n.starts_with("uniformBlock"))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    for name in names {
        let vector = name.ends_with('v');
        let matrix = name.starts_with("uniformMatrix");
        gl_op(reg, rt, name, move |cx, w, gl| {
            let Some(h) = cx.arg_host(0).filter(|h| h.kind == K_OBJ) else {
                return Ok(());
            };
            let Some(key) = with_gl(w, h.id, |g| match g {
                GlObj::Location { program, name } => Some((*program, name.clone())),
                _ => None,
            }) else {
                return Ok(());
            };
            let first = if matrix { 2 } else { 1 };
            let vals: Vec<f32> = if vector {
                match cx.arg_view_kind(first) {
                    Some(ViewKind::Float32) => cx
                        .arg_bytes(first)
                        .map(|b| {
                            b.chunks_exact(4)
                                .map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                                .collect()
                        })
                        .unwrap_or_default(),
                    _ => match cx.arg_value(first) {
                        JsValue::Json(s) => serde_json::from_str::<Vec<f64>>(&s)
                            .map(|v| v.into_iter().map(|x| x as f32).collect())
                            .unwrap_or_default(),
                        _ => Vec::new(),
                    },
                }
            } else {
                let mut v = Vec::new();
                for i in 1..cx.len() {
                    v.push(cx.arg_f64(i)? as f32);
                }
                v
            };
            gl.borrow_mut().uniforms.insert(key, vals);
            Ok(())
        });
    }
}

fn register_drawing(reg: &mut Registry, rt: &Runtime) {
    register_uniforms(reg, rt);
    gl_op(reg, rt, "enableVertexAttribArray", |cx, _w, gl| {
        let i = cx.arg_u32(0)? as usize;
        let mut g = gl.borrow_mut();
        match g.attribs.get_mut(i) {
            Some(a) => a.enabled = true,
            None => g.set_error(INVALID_VALUE),
        }
        Ok(())
    });
    gl_op(reg, rt, "disableVertexAttribArray", |cx, _w, gl| {
        let i = cx.arg_u32(0)? as usize;
        let mut g = gl.borrow_mut();
        match g.attribs.get_mut(i) {
            Some(a) => a.enabled = false,
            None => g.set_error(INVALID_VALUE),
        }
        Ok(())
    });
    gl_op(reg, rt, "vertexAttribPointer", |cx, _w, gl| {
        let i = cx.arg_u32(0)? as usize;
        let size = cx.arg_u32(1)?;
        let ty = cx.arg_u32(2)?;
        let normalized = cx.arg_bool(3);
        let stride = cx.arg_u32(4)?;
        let offset = cx.arg_f64(5)?.max(0.0) as u32;
        let mut g = gl.borrow_mut();
        let buffer = g.bound.get(&0x8894).copied();
        if buffer.is_none() && offset != 0 {
            g.set_error(INVALID_OPERATION);
            return Ok(());
        }
        match g.attribs.get_mut(i) {
            Some(a) if (1..=4).contains(&size) => {
                a.buffer = buffer;
                a.size = size;
                a.ty = ty;
                a.normalized = normalized;
                a.stride = stride;
                a.offset = offset;
            }
            _ => g.set_error(INVALID_VALUE),
        }
        Ok(())
    });
    gl_op(reg, rt, "getVertexAttrib", |cx, w, gl| {
        let i = cx.arg_u32(0)? as usize;
        let p = cx.arg_u32(1)?;
        let a = gl.borrow().attribs.get(i).copied();
        let Some(a) = a else {
            gl.borrow_mut().set_error(INVALID_VALUE);
            return cx.ret(Arg::Null);
        };
        match p {
            0x8622 => cx.ret(a.enabled),
            0x8623 => cx.ret(if a.size == 0 { 4 } else { a.size }),
            0x8624 => cx.ret(a.stride),
            0x8625 => cx.ret(if a.ty == 0 { FLOAT } else { a.ty }),
            0x886A => cx.ret(a.normalized),
            0x889F => cx.ret(opt_obj(w, a.buffer)),
            0x8626 => cx.ret(Arg::Float32Array(vec![0.0, 0.0, 0.0, 1.0])),
            _ => cx.ret(Arg::Null),
        }
    });
    gl_op(reg, rt, "getVertexAttribOffset", |cx, _w, gl| {
        let i = cx.arg_u32(0)? as usize;
        let v = gl.borrow().attribs.get(i).map_or(0, |a| a.offset);
        cx.ret(v)
    });
    gl_op(reg, rt, "clear", |cx, _w, gl| {
        let mask = cx.arg_u32(0)?;
        if mask & !(COLOR_BUFFER_BIT | DEPTH_BUFFER_BIT | 0x0400) != 0 {
            gl.borrow_mut().set_error(INVALID_VALUE);
            return Ok(());
        }
        gl.borrow_mut().clear(mask);
        Ok(())
    });
    for name in ["drawArrays", "drawArraysInstanced"] {
        gl_op(reg, rt, name, |cx, _w, gl| {
            let mode = cx.arg_u32(0)?;
            let first = cx.arg_i32(1)?;
            let count = cx.arg_i32(2)?;
            if first < 0 || count < 0 {
                gl.borrow_mut().set_error(INVALID_VALUE);
                return Ok(());
            }
            let idx: Vec<u32> = (first as u32..(first + count) as u32).collect();
            gl.borrow_mut().draw(mode, &idx);
            Ok(())
        });
    }
    for name in ["drawElements", "drawElementsInstanced", "drawRangeElements"] {
        gl_op(reg, rt, name, move |cx, _w, gl| {
            let ranged = name == "drawRangeElements";
            let mode = cx.arg_u32(0)?;
            let count = cx.arg_i32(if ranged { 3 } else { 1 })?;
            let ty = cx.arg_u32(if ranged { 4 } else { 2 })?;
            let off = cx.arg_f64(if ranged { 5 } else { 3 })?.max(0.0) as usize;
            if count < 0 {
                gl.borrow_mut().set_error(INVALID_VALUE);
                return Ok(());
            }
            let idx: Vec<u32> = {
                let g = gl.borrow();
                let Some(buf) = g.bound.get(&0x8895).and_then(|id| g.res.get(id)) else {
                    drop(g);
                    gl.borrow_mut().set_error(INVALID_OPERATION);
                    return Ok(());
                };
                let b = &buf.bytes;
                (0..count as usize)
                    .filter_map(|k| match ty {
                        UNSIGNED_BYTE => b.get(off + k).map(|v| u32::from(*v)),
                        UNSIGNED_SHORT => b
                            .get(off + k * 2..off + k * 2 + 2)
                            .map(|s| u32::from(u16::from_le_bytes([s[0], s[1]]))),
                        UNSIGNED_INT => b
                            .get(off + k * 4..off + k * 4 + 4)
                            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])),
                        _ => None,
                    })
                    .collect()
            };
            gl.borrow_mut().draw(mode, &idx);
            Ok(())
        });
    }
    gl_op(reg, rt, "readPixels", |cx, _w, gl| {
        let x = cx.arg_i32(0)?;
        let y = cx.arg_i32(1)?;
        let wd = cx.arg_i32(2)?;
        let ht = cx.arg_i32(3)?;
        let format = cx.arg_u32(4)?;
        let ty = cx.arg_u32(5)?;
        if wd < 0 || ht < 0 {
            gl.borrow_mut().set_error(INVALID_VALUE);
            return Ok(());
        }
        if format != RGBA || ty != UNSIGNED_BYTE {
            gl.borrow_mut().set_error(INVALID_OPERATION);
            return Ok(());
        }
        if cx.arg_kind(6) != ValueKind::Object || cx.arg_view_kind(6).is_none() {
            return Err(HostError::type_error(
                "Failed to execute 'readPixels' on 'WebGLRenderingContext': parameter 7 is not of type 'ArrayBufferView'.",
            ));
        }
        if cx.arg_view_kind(6) != Some(ViewKind::Uint8)
            && cx.arg_view_kind(6) != Some(ViewKind::Uint8Clamped)
        {
            gl.borrow_mut().set_error(INVALID_OPERATION);
            return Ok(());
        }
        let dst_off = if cx.len() > 7 {
            cx.arg_f64(7)?.max(0.0) as usize
        } else {
            0
        };
        let bmp = gl.borrow().readback_raw();
        let (fw, fh) = (bmp.width as i32, bmp.height as i32);
        let mut out = vec![0u8; wd as usize * ht as usize * 4];
        for row in 0..ht {
            let gy = y + row;
            if gy < 0 || gy >= fh {
                continue;
            }
            let src_row = (fh - 1 - gy) as usize;
            for col in 0..wd {
                let gx = x + col;
                if gx < 0 || gx >= fw {
                    continue;
                }
                let s = (src_row * fw as usize + gx as usize) * 4;
                let d = (row as usize * wd as usize + col as usize) * 4;
                out[d..d + 4].copy_from_slice(&bmp.data[s..s + 4]);
            }
        }
        cx.write_arg_bytes(6, dst_off, &out);
        Ok(())
    });
}

impl Gl {
    /// Stored values with the readback noise (`readPixels` returns stored,
    /// not unpremultiplied, values).
    fn readback_raw(&self) -> lb_paint::Bitmap {
        let mut bmp = lb_paint::Bitmap::new(self.width, self.height);
        let Some(fb) = &self.fb else { return bmp };
        bmp.data.copy_from_slice(fb);
        if let Some(seed) = self.seed {
            lb_paint::CanvasNoise::new(seed).apply(
                fb,
                self.width,
                self.height,
                0,
                0,
                self.width,
                &mut bmp.data,
            );
        }
        bmp
    }
}

fn register_helpers(reg: &mut Registry, rt: &Runtime) {
    gl_op(
        reg,
        rt,
        "getFramebufferAttachmentParameter",
        |cx, _w, _gl| cx.ret(Arg::Null),
    );
    gl_op(reg, rt, "getRenderbufferParameter", |cx, _w, _gl| cx.ret(0));
    gl_op(reg, rt, "getTexParameter", |cx, _w, _gl| {
        let p = cx.arg_u32(1)?;
        cx.ret(match p {
            0x2800 => 9729,
            0x2801 => 9986,
            0x2802 | 0x2803 | 0x8072 => 10497,
            _ => 0,
        })
    });
    gl_op(reg, rt, "getInternalformatParameter", |cx, _w, _gl| {
        cx.ret(Arg::Int32Array(vec![4]))
    });
    gl_op(reg, rt, "getIndexedParameter", |cx, _w, _gl| {
        cx.ret(Arg::Null)
    });
    gl_op(reg, rt, "getFragDataLocation", |cx, _w, _gl| cx.ret(-1));
    gl_op(reg, rt, "getUniformBlockIndex", |cx, _w, _gl| {
        cx.ret(4_294_967_295u32)
    });
    gl_op(reg, rt, "makeXRCompatible", |cx, _w, _gl| {
        let r = cx.ret_new_promise()?;
        cx.resolve_promise(r, Arg::Undefined)
    });
}

fn this_ext(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<Rc<RefCell<Gl>>, HostError> {
    let id = this_obj(cx)?;
    with_gl(w, id, |g| match g {
        GlObj::Ext { ctx, .. } => Some(ctx.clone()),
        _ => None,
    })
    .ok_or_else(illegal)
}

fn register_extensions(reg: &mut Registry, rt: &Runtime) {
    // Default no-op for every extension operation.
    for e in EXTENSIONS {
        for (op, len) in e.ops {
            let (iface, op, len) = (e.iface, *op, usize::from(*len));
            method(reg, rt, iface, op, move |cx, w| {
                super::need_args(cx, len, op, iface)?;
                let _ = this_ext(cx, w)?;
                if op.starts_with("is") {
                    cx.ret(false)
                } else if op.starts_with("get") {
                    cx.ret(Arg::Null)
                } else {
                    Ok(())
                }
            });
        }
    }
    method(reg, rt, "WebGLLoseContext", "loseContext", |cx, w| {
        let gl = this_ext(cx, w)?;
        let mut g = gl.borrow_mut();
        g.lost = true;
        g.error = CONTEXT_LOST_WEBGL;
        Ok(())
    });
    method(reg, rt, "WebGLLoseContext", "restoreContext", |cx, w| {
        let gl = this_ext(cx, w)?;
        let mut g = gl.borrow_mut();
        if !g.lost {
            g.set_error(INVALID_OPERATION);
            return Ok(());
        }
        g.lost = false;
        g.error = NO_ERROR;
        Ok(())
    });
    method(
        reg,
        rt,
        "OESVertexArrayObject",
        "createVertexArrayOES",
        |cx, w| {
            let gl = this_ext(cx, w)?;
            let id = new_res(w, &gl, ResKind::VertexArray, true);
            cx.ret(w.obj_arg(id))
        },
    );
    method(
        reg,
        rt,
        "OESVertexArrayObject",
        "bindVertexArrayOES",
        |cx, w| {
            let gl = this_ext(cx, w)?;
            let obj = arg_res(cx, w, 0, &gl);
            let mut g = gl.borrow_mut();
            match obj {
                Some(id) => g.bound.insert(0x85B5, id),
                None => g.bound.remove(&0x85B5),
            };
            Ok(())
        },
    );
    method(
        reg,
        rt,
        "OESVertexArrayObject",
        "isVertexArrayOES",
        |cx, w| {
            let gl = this_ext(cx, w)?;
            let ok = arg_res(cx, w, 0, &gl).is_some();
            cx.ret(ok)
        },
    );
    method(
        reg,
        rt,
        "WebGLDebugShaders",
        "getTranslatedShaderSource",
        |cx, w| {
            super::need_args(cx, 1, "getTranslatedShaderSource", "WebGLDebugShaders")?;
            let gl = this_ext(cx, w)?;
            let ok = arg_res(cx, w, 0, &gl).is_some();
            if ok { cx.ret("") } else { cx.ret(Arg::Null) }
        },
    );
    method(
        reg,
        rt,
        "ANGLEInstancedArrays",
        "drawArraysInstancedANGLE",
        |cx, w| {
            super::need_args(cx, 4, "drawArraysInstancedANGLE", "ANGLEInstancedArrays")?;
            let gl = this_ext(cx, w)?;
            let mode = cx.arg_u32(0)?;
            let first = cx.arg_i32(1)?.max(0) as u32;
            let count = cx.arg_i32(2)?.max(0) as u32;
            let idx: Vec<u32> = (first..first + count).collect();
            gl.borrow_mut().draw(mode, &idx);
            Ok(())
        },
    );
}
