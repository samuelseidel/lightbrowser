//! WebGL rendering through `lb-gl` (ADR 0030): shader compilation and
//! linking, uniforms, vertex arrays, textures, renderbuffers,
//! framebuffers, draws, clears, blits and `readPixels`.

use lb_gl::image::{self as gimg, Data, Format, Image, Kind};
use lb_gl::program::{LinkOptions, Program};
use lb_gl::raster::{self, AttribSrc, Blend, Fixed, Indices, StencilFace, Target, VertexArray};
use lb_gl::texture::{self as gtex, SamplerParams, Texture, Units};

use super::*;

#[derive(Clone, Default)]
pub(crate) struct Vao {
    attribs: Vec<Attrib>,
    element: Option<u32>,
}

/// Texture bindings of one unit: 2D, cube, 3D, 2D array; sampler.
#[derive(Clone, Copy, Default)]
pub(crate) struct Unit {
    tex: [Option<u32>; 4],
    sampler: Option<u32>,
}

#[derive(Clone, Copy, Debug)]
enum AttKind {
    Tex { level: u32, face: u32, layer: u32 },
    Rb,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Att {
    obj: u32,
    kind: AttKind,
}

#[derive(Clone, Default)]
pub(crate) struct Fbo {
    color: [Option<Att>; 8],
    depth: Option<Att>,
    stencil: Option<Att>,
    draw: Vec<u32>,
    read: u32,
}

const COLOR_ATTACHMENT0: u32 = 0x8CE0;
const DEPTH_ATTACHMENT: u32 = 0x8D00;
const STENCIL_ATTACHMENT: u32 = 0x8D20;
const DEPTH_STENCIL_ATTACHMENT: u32 = 0x821A;
const FRAMEBUFFER_INCOMPLETE_ATTACHMENT: u32 = 0x8CD6;
const FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT: u32 = 0x8CD7;
const FRAMEBUFFER_INCOMPLETE_DIMENSIONS: u32 = 0x8CD9;
const FRAMEBUFFER_UNSUPPORTED: u32 = 0x8CDD;
const INVALID_FRAMEBUFFER_OPERATION: u32 = 0x0506;
const STENCIL_BUFFER_BIT: u32 = 0x0400;
const FLOAT_T: u32 = 0x1406;
const INT_T: u32 = 0x1404;
const HALF_FLOAT: u32 = 0x140B;
const HALF_FLOAT_OES: u32 = 0x8D61;

/// Texture target → binding slot.
fn tslot(target: u32) -> Option<usize> {
    match target {
        gtex::TEXTURE_2D => Some(0),
        gtex::TEXTURE_CUBE_MAP => Some(1),
        gtex::TEXTURE_3D => Some(2),
        gtex::TEXTURE_2D_ARRAY => Some(3),
        _ => None,
    }
}

/// Image target (incl. cube faces) → (binding slot, face).
fn image_target(target: u32) -> Option<(usize, usize)> {
    match target {
        gtex::TEXTURE_2D => Some((0, 0)),
        0x8515..=0x851A => Some((1, (target - 0x8515) as usize)),
        gtex::TEXTURE_3D => Some((2, 0)),
        gtex::TEXTURE_2D_ARRAY => Some((3, 0)),
        _ => None,
    }
}

const SLOT_PNAMES: [u32; 4] = [0x8069, 0x8514, 0x806A, 0x8C1D];
const SLOT_TARGETS: [u32; 4] = [
    gtex::TEXTURE_2D,
    gtex::TEXTURE_CUBE_MAP,
    gtex::TEXTURE_3D,
    gtex::TEXTURE_2D_ARRAY,
];

/// Where a taken framebuffer image belongs.
#[derive(Clone, Copy)]
enum Where {
    Color,
    Ds,
    Tex {
        obj: u32,
        face: u32,
        level: u32,
        layer: Option<u32>,
    },
    Rb(u32),
}

struct Bufs {
    colors: Vec<Option<(Image, Where)>>,
    depth: Option<(Image, Where)>,
    stencil: Option<(Image, Where)>,
    width: u32,
    height: u32,
    samples: u32,
    default: bool,
}

/// GLSL extension names enabled through WebGL extensions.
fn glsl_extensions(g: &Gl) -> Vec<&'static str> {
    let mut v = Vec::new();
    for (webgl, glsl) in [
        ("OES_standard_derivatives", "GL_OES_standard_derivatives"),
        ("EXT_shader_texture_lod", "GL_EXT_shader_texture_lod"),
        ("EXT_frag_depth", "GL_EXT_frag_depth"),
        ("WEBGL_draw_buffers", "GL_EXT_draw_buffers"),
    ] {
        if g.ext_enabled(webgl) {
            v.push(glsl);
        }
    }
    v
}

impl Gl {
    fn num(&self, pname: u32) -> f64 {
        match self.state.get(&pname) {
            Some(JsValue::Number(n)) => *n,
            Some(JsValue::Bool(b)) => f64::from(u8::from(*b)),
            _ => match state_default(pname, true) {
                Some(Arg::Number(n)) => n,
                Some(Arg::Bool(b)) => f64::from(u8::from(b)),
                _ => 0.0,
            },
        }
    }

    fn flag(&self, pname: u32) -> bool {
        self.num(pname) != 0.0
    }

    fn f32s(&self, pname: u32, def: &[f32]) -> Vec<f32> {
        match self.state.get(&pname) {
            Some(JsValue::Json(s)) => {
                serde_json::from_str::<Vec<f32>>(s).unwrap_or_else(|_| def.to_vec())
            }
            _ => def.to_vec(),
        }
    }

    fn unit(&self) -> usize {
        (self.active_texture - TEXTURE0) as usize
    }

    fn tex_bound(&self, slot: usize) -> Option<u32> {
        self.units.get(self.unit()).and_then(|u| u.tex[slot])
    }

    fn tex_mut(&mut self, id: u32) -> Option<&mut Texture> {
        self.res.get_mut(&id).and_then(|r| r.tex.as_deref_mut())
    }

    fn default_format(&self) -> Format {
        if self.attrs.alpha {
            gimg::RGBA8
        } else {
            gimg::RGB8
        }
    }

    fn samples(&self) -> u32 {
        if self.attrs.antialias { 4 } else { 1 }
    }

    pub(super) fn ensure_default(&mut self) {
        let (w, h) = (self.width, self.height);
        if self.color.is_none() {
            self.color = Some(Image::new(self.default_format(), w, h, 1, self.samples()));
        }
        if self.ds.is_none() && (self.attrs.depth || self.attrs.stencil) {
            let f = if self.attrs.stencil { 0x88F0 } else { 0x81A6 };
            let mut img = Image::new(gimg::sized(f).expect("ds"), w, h, 1, self.samples());
            img.clear_depth(1.0, None);
            self.ds = Some(img);
        }
    }

    pub(super) fn resize(&mut self, w: u32, h: u32) {
        self.width = w;
        self.height = h;
        self.color = None;
        self.ds = None;
    }

    /// Stored RGBA8 rows, top-down, of the drawing buffer.
    fn default_rgba(&self) -> Vec<u8> {
        let (w, h) = (self.width as usize, self.height as usize);
        let Some(c) = &self.color else {
            let a = if self.attrs.alpha { 0 } else { 255 };
            let mut v = vec![0u8; w * h * 4];
            for p in v.chunks_exact_mut(4) {
                p[3] = a;
            }
            return v;
        };
        let up = c.resolve_rgba8(0);
        let mut out = vec![0u8; w * h * 4];
        for y in 0..h {
            out[y * w * 4..(y + 1) * w * 4]
                .copy_from_slice(&up[(h - 1 - y) * w * 4..(h - y) * w * 4]);
        }
        out
    }

    /// Unpremultiplied, noised RGBA rows top-down (what leaves the canvas).
    pub(super) fn readback(&self) -> lb_paint::Bitmap {
        let fb = self.default_rgba();
        let mut bmp = self.unpremultiplied(&fb);
        if let Some(seed) = self.seed {
            lb_paint::CanvasNoise::new(seed).apply(
                &fb,
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

    /// Readback without noise (page screenshots only).
    pub(super) fn readback_clean(&self) -> lb_paint::Bitmap {
        let fb = self.default_rgba();
        self.unpremultiplied(&fb)
    }

    fn unpremultiplied(&self, fb: &[u8]) -> lb_paint::Bitmap {
        let mut bmp = lb_paint::Bitmap::new(self.width, self.height);
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
        bmp
    }

    // -----------------------------------------------------------------
    // Framebuffers
    // -----------------------------------------------------------------

    fn fbo(&self, id: u32) -> Option<&Fbo> {
        self.res.get(&id).and_then(|r| r.fbo.as_deref())
    }

    fn att_image(&self, a: &Att) -> Option<&Image> {
        let r = self.res.get(&a.obj)?;
        if r.deleted {
            return None;
        }
        match a.kind {
            AttKind::Rb => r.rb.as_ref(),
            AttKind::Tex { level, face, .. } => r.tex.as_ref()?.level(face as usize, level),
        }
    }

    fn fbo_status(&self, id: u32) -> u32 {
        let Some(f) = self.fbo(id) else {
            return FRAMEBUFFER_COMPLETE;
        };
        let atts: Vec<(Att, u8)> = f
            .color
            .iter()
            .flatten()
            .map(|a| (*a, 0))
            .chain(f.depth.map(|a| (a, 1)))
            .chain(f.stencil.map(|a| (a, 2)))
            .collect();
        if atts.is_empty() {
            return FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT;
        }
        let mut size: Option<(u32, u32)> = None;
        let mut samples: Option<u32> = None;
        for (a, role) in &atts {
            let Some(img) = self.att_image(a) else {
                return FRAMEBUFFER_INCOMPLETE_ATTACHMENT;
            };
            if img.width == 0 || img.height == 0 {
                return FRAMEBUFFER_INCOMPLETE_ATTACHMENT;
            }
            let k = img.format.kind;
            let ok = match role {
                0 => {
                    !matches!(k, Kind::Depth | Kind::DepthStencil | Kind::Stencil)
                        && self.color_renderable(&img.format)
                }
                1 => matches!(k, Kind::Depth | Kind::DepthStencil),
                _ => matches!(k, Kind::Stencil | Kind::DepthStencil),
            };
            if !ok {
                return FRAMEBUFFER_INCOMPLETE_ATTACHMENT;
            }
            if let AttKind::Tex { layer, .. } = a.kind
                && layer >= img.layers
            {
                return FRAMEBUFFER_INCOMPLETE_ATTACHMENT;
            }
            match size {
                None => size = Some((img.width, img.height)),
                Some(s) if s != (img.width, img.height) && !self.webgl2 => {
                    return FRAMEBUFFER_INCOMPLETE_DIMENSIONS;
                }
                _ => {}
            }
            match samples {
                None => samples = Some(img.samples),
                Some(s) if s != img.samples => return 0x8D56,
                _ => {}
            }
        }
        if let (Some(d), Some(s)) = (f.depth, f.stencil)
            && d.obj != s.obj
            && self.webgl2
        {
            return FRAMEBUFFER_UNSUPPORTED;
        }
        FRAMEBUFFER_COMPLETE
    }

    fn color_renderable(&self, f: &Format) -> bool {
        match f.kind {
            Kind::Unorm => f.swizzle == gimg::Swizzle::Rgba && !(f.srgb && f.channels == 3),
            Kind::Float => {
                if self.webgl2 {
                    self.ext_enabled("EXT_color_buffer_float")
                        || (f.bits[0] == 16 && self.ext_enabled("EXT_color_buffer_half_float"))
                } else {
                    (f.bits[0] == 32
                        && (self.ext_enabled("WEBGL_color_buffer_float")
                            || self.ext_enabled("OES_texture_float")))
                        || (f.bits[0] == 16
                            && (self.ext_enabled("EXT_color_buffer_half_float")
                                || self.ext_enabled("OES_texture_half_float")))
                }
            }
            Kind::Int | Kind::Uint => self.webgl2,
            Kind::Snorm => false,
            _ => false,
        }
    }

    fn take(&mut self, a: &Att) -> Option<(Image, Where)> {
        let r = self.res.get_mut(&a.obj)?;
        match a.kind {
            AttKind::Rb => r.rb.take().map(|i| (i, Where::Rb(a.obj))),
            AttKind::Tex { level, face, layer } => {
                let t = r.tex.as_deref_mut()?;
                let img = t.level_mut(face as usize, level)?;
                if img.layers > 1 {
                    // Copy the layer out.
                    let mut one = Image::new(img.format, img.width, img.height, 1, img.samples);
                    copy_layer(img, layer, &mut one, 0);
                    Some((
                        one,
                        Where::Tex {
                            obj: a.obj,
                            face,
                            level,
                            layer: Some(layer),
                        },
                    ))
                } else {
                    let img = t
                        .faces
                        .get_mut(face as usize)?
                        .get_mut(level as usize)?
                        .take()?;
                    Some((
                        img,
                        Where::Tex {
                            obj: a.obj,
                            face,
                            level,
                            layer: None,
                        },
                    ))
                }
            }
        }
    }

    fn put(&mut self, img: Image, w: Where) {
        match w {
            Where::Color => self.color = Some(img),
            Where::Ds => self.ds = Some(img),
            Where::Rb(id) => {
                if let Some(r) = self.res.get_mut(&id) {
                    r.rb = Some(img);
                }
            }
            Where::Tex {
                obj,
                face,
                level,
                layer,
            } => {
                if let Some(t) = self.tex_mut(obj) {
                    match layer {
                        Some(l) => {
                            if let Some(dst) = t.level_mut(face as usize, level) {
                                copy_layer(&img, 0, dst, l);
                            }
                        }
                        None => t.set_level(face as usize, level, Some(img)),
                    }
                }
            }
        }
    }

    /// Takes the images of the draw (or read) framebuffer out of the
    /// context; `Err` is the GL error.
    fn take_bufs(&mut self, read: bool) -> Result<Bufs, u32> {
        let fb = self.bound.get(&if read { 0x8CAA } else { 0x8CA6 }).copied();
        let Some(id) = fb else {
            self.ensure_default();
            let color = self.color.take().map(|c| (c, Where::Color));
            let (w, h, s) = (self.width, self.height, self.samples());
            let ds = self.ds.take().map(|d| (d, Where::Ds));
            let has_stencil = self.attrs.stencil;
            let has_depth = self.attrs.depth;
            let (depth, stencil) = match ds {
                Some(d) if has_depth => (Some(d), None),
                Some(d) if has_stencil => (None, Some(d)),
                _ => (None, None),
            };
            let colors = if read {
                vec![color]
            } else {
                let draw = self.num(0x8825) as u32;
                if draw == 0 { vec![None] } else { vec![color] }
            };
            return Ok(Bufs {
                colors,
                depth,
                stencil,
                width: w,
                height: h,
                samples: s,
                default: true,
            });
        };
        let status = self.fbo_status(id);
        if status != FRAMEBUFFER_COMPLETE {
            return Err(INVALID_FRAMEBUFFER_OPERATION);
        }
        let f = self.fbo(id).cloned().unwrap_or_default();
        let mut colors = Vec::new();
        if read {
            let k = if f.read == 0 {
                0
            } else {
                (f.read - COLOR_ATTACHMENT0) as usize
            };
            let a = if f.read == 0 && !f.draw.is_empty() && false {
                None
            } else {
                f.color.get(k).copied().flatten()
            };
            colors.push(a.and_then(|a| self.take(&a)));
        } else {
            let draw: Vec<u32> = if f.draw.is_empty() {
                vec![COLOR_ATTACHMENT0]
            } else {
                f.draw.clone()
            };
            for d in draw {
                let a = if d == 0 {
                    None
                } else {
                    f.color
                        .get((d - COLOR_ATTACHMENT0) as usize)
                        .copied()
                        .flatten()
                };
                colors.push(a.and_then(|a| self.take(&a)));
            }
        }
        let depth = f.depth.and_then(|a| self.take(&a));
        let stencil = match (f.stencil, f.depth) {
            (Some(s), Some(d)) if s.obj == d.obj => None,
            (Some(s), _) => self.take(&s),
            _ => None,
        };
        let mut w = u32::MAX;
        let mut h = u32::MAX;
        let mut samples = 1;
        for img in colors
            .iter()
            .flatten()
            .map(|c| &c.0)
            .chain(depth.iter().map(|d| &d.0))
            .chain(stencil.iter().map(|d| &d.0))
        {
            w = w.min(img.width);
            h = h.min(img.height);
            samples = img.samples;
        }
        if w == u32::MAX {
            // Only attachments that are not drawn: use any attachment size.
            let any = f
                .color
                .iter()
                .flatten()
                .next()
                .and_then(|a| self.att_image(a))
                .map(|i| (i.width, i.height));
            let (aw, ah) = any.unwrap_or((0, 0));
            w = aw;
            h = ah;
        }
        let ds_shared = matches!((f.stencil, f.depth), (Some(s), Some(d)) if s.obj == d.obj);
        let _ = ds_shared;
        Ok(Bufs {
            colors,
            depth,
            stencil,
            width: w,
            height: h,
            samples,
            default: false,
        })
    }

    fn put_bufs(&mut self, b: Bufs) {
        for (img, w) in b.colors.into_iter().flatten() {
            self.put(img, w);
        }
        if let Some((img, w)) = b.depth {
            self.put(img, w);
        }
        if let Some((img, w)) = b.stencil {
            self.put(img, w);
        }
    }

    fn fixed(&self, b: &Bufs) -> Fixed {
        let en = |p: u32| self.enabled.contains(&p);
        let has_depth = b
            .depth
            .as_ref()
            .is_some_and(|d| matches!(d.0.format.kind, Kind::Depth | Kind::DepthStencil));
        let has_stencil = b
            .stencil
            .as_ref()
            .is_some_and(|d| matches!(d.0.format.kind, Kind::Stencil | Kind::DepthStencil))
            || b.depth.as_ref().is_some_and(|d| {
                d.0.format.kind == Kind::DepthStencil && (!b.default || self.attrs.stencil)
            });
        let face = |f: u32, r: u32, m: u32, w: u32, o: [u32; 3]| StencilFace {
            func: self.num(f) as u32,
            reference: self.num(r) as i32,
            value_mask: self.num(m) as u32 & 0xFF,
            write_mask: self.num(w) as u32 & 0xFF,
            fail: self.num(o[0]) as u32,
            zfail: self.num(o[1]) as u32,
            zpass: self.num(o[2]) as u32,
        };
        let dr = self.f32s(0x0B70, &[0.0, 1.0]);
        let bc = self.f32s(0x8005, &[0.0; 4]);
        Fixed {
            viewport: self.viewport,
            depth_range: [dr[0], dr[1]],
            scissor: en(SCISSOR_TEST).then_some(self.scissor),
            cull: en(0x0B44).then(|| self.num(0x0B45) as u32),
            front_ccw: self.num(0x0B46) as u32 == 0x0901,
            polygon_offset: en(0x8037).then(|| (self.num(0x8038) as f32, self.num(0x2A00) as f32)),
            depth_func: (en(0x0B71) && has_depth).then(|| self.num(0x0B74) as u32),
            depth_mask: !matches!(self.state.get(&0x0B72), Some(JsValue::Bool(false))),
            stencil: (en(0x0B90) && has_stencil).then(|| {
                [
                    face(0x0B92, 0x0B97, 0x0B93, 0x0B98, [0x0B94, 0x0B95, 0x0B96]),
                    face(0x8800, 0x8CA3, 0x8CA4, 0x8CA5, [0x8801, 0x8802, 0x8803]),
                ]
            }),
            blend: en(0x0BE2).then(|| Blend {
                eq: [self.num(0x8009) as u32, self.num(0x883D) as u32],
                src: [self.num(0x80C9) as u32, self.num(0x80CB) as u32],
                dst: [self.num(0x80C8) as u32, self.num(0x80CA) as u32],
                color: [bc[0], bc[1], bc[2], bc[3]],
            }),
            color_mask: if b.default && !self.attrs.alpha {
                [
                    self.color_mask[0],
                    self.color_mask[1],
                    self.color_mask[2],
                    false,
                ]
            } else {
                self.color_mask
            },
            rasterizer_discard: en(0x8C89),
            alpha_to_coverage: en(0x809E),
            sample_coverage: en(0x80A0).then(|| (self.num(0x80AA) as f32, self.flag(0x80AB))),
        }
    }

    // -----------------------------------------------------------------
    // Clear
    // -----------------------------------------------------------------

    fn clear_buffers(&mut self, mask: u32) {
        if self.enabled.contains(&0x8C89) {
            return;
        }
        let mut b = match self.take_bufs(false) {
            Ok(b) => b,
            Err(e) => {
                self.set_error(e);
                return;
            }
        };
        let rect = self.enabled.contains(&SCISSOR_TEST).then_some(self.scissor);
        let default = b.default;
        if mask & COLOR_BUFFER_BIT != 0 {
            let mut m = self.color_mask;
            if default && !self.attrs.alpha {
                m[3] = false;
            }
            let c = self.clear_color;
            for (img, _) in b.colors.iter_mut().flatten() {
                if matches!(img.format.kind, Kind::Int | Kind::Uint) {
                    continue;
                }
                let mut v = c;
                if img.format.kind == Kind::Unorm {
                    v = v.map(|x| x.clamp(0.0, 1.0));
                }
                if img.format.srgb {
                    for x in v.iter_mut().take(3) {
                        *x = gimg::linear_to_srgb(*x);
                    }
                }
                img.clear_color(v, m, rect);
            }
        }
        if mask & DEPTH_BUFFER_BIT != 0
            && !matches!(self.state.get(&0x0B72), Some(JsValue::Bool(false)))
        {
            let d = self.num(0x0B73) as f32;
            if let Some((img, _)) = b.depth.as_mut() {
                img.clear_depth(d, rect);
            }
        }
        if mask & STENCIL_BUFFER_BIT != 0 {
            let v = self.num(0x0B91) as i32 as u8;
            let wm = self.num(0x0B98) as u32 as u8;
            if let Some((img, _)) = b.stencil.as_mut().or(b.depth.as_mut()) {
                img.clear_stencil(v, wm, rect);
            }
        }
        self.put_bufs(b);
    }

    // -----------------------------------------------------------------
    // Draw
    // -----------------------------------------------------------------

    fn current_program(&self) -> Option<(u32, Rc<Program>)> {
        let id = *self.bound.get(&0x8B8D)?;
        let r = self.res.get(&id)?;
        Some((id, r.prog.clone()?))
    }

    /// Validates attribute buffers for vertices `0..=max_vertex` and
    /// `instances`.
    fn attribs_ok(&self, p: &Program, max_vertex: Option<u32>, instances: u32) -> bool {
        for a in &p.attribs {
            for c in 0..u32::from(a.ty.cols.max(1)) {
                let loc = (a.location + c) as usize;
                let Some(at) = self.attribs.get(loc) else {
                    continue;
                };
                if !at.enabled {
                    continue;
                }
                let Some(buf) = at.buffer.and_then(|b| self.res.get(&b)) else {
                    return false;
                };
                let esize = attrib_bytes(at);
                let stride = if at.stride == 0 {
                    esize
                } else {
                    at.stride as usize
                };
                let count = if at.divisor > 0 {
                    Some(instances.max(1).div_ceil(at.divisor))
                } else {
                    max_vertex.map(|m| m + 1)
                };
                if let Some(n) = count
                    && n > 0
                {
                    let need = at.offset as usize + (n as usize - 1) * stride + esize;
                    if need > buf.bytes.len() {
                        return false;
                    }
                }
            }
        }
        true
    }

    pub(super) fn draw(&mut self, mode: u32, idx: Indices, instances: u32, restart: Option<u32>) {
        if mode > raster::TRIANGLE_FAN {
            self.set_error(INVALID_ENUM);
            return;
        }
        let Some((pid, prog)) = self.current_program() else {
            self.set_error(INVALID_OPERATION);
            return;
        };
        let max_vertex = match &idx {
            Indices::Seq(first, count) => (*count > 0).then(|| first + count - 1),
            Indices::List(l) => l.iter().filter(|v| Some(**v) != restart).max().copied(),
        };
        if !self.attribs_ok(&prog, max_vertex, instances) {
            self.set_error(INVALID_OPERATION);
            return;
        }
        let empty = match &idx {
            Indices::Seq(_, c) => *c == 0,
            Indices::List(l) => l.is_empty(),
        };
        if empty || instances == 0 {
            return;
        }
        let mut b = match self.take_bufs(false) {
            Ok(b) => b,
            Err(e) => {
                self.set_error(e);
                return;
            }
        };
        let fixed = self.fixed(&b);
        let budget_ok;
        {
            let g = &*self;
            let r = &g.res[&pid];
            let udata = r.udata.clone();
            let blocks: Vec<Option<&[u8]>> = prog
                .blocks
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    let binding = r.block_bind.get(i).copied().unwrap_or(0) as usize;
                    let (buf, off, size) = g.ubo.get(binding).copied().unwrap_or((None, 0, 0));
                    let bytes = &g.res.get(&buf?)?.bytes;
                    let end = if size == 0 {
                        bytes.len()
                    } else {
                        (off + size).min(bytes.len())
                    };
                    bytes.get(off.min(end)..end)
                })
                .collect();
            let attribs: Vec<AttribSrc<'_>> = (0..16)
                .map(|loc| {
                    let a = g.attribs.get(loc).copied().unwrap_or_default();
                    if a.enabled
                        && let Some(buf) = a.buffer.and_then(|b| g.res.get(&b))
                    {
                        AttribSrc::Array(VertexArray {
                            data: &buf.bytes,
                            size: a.size,
                            ty: a.ty,
                            normalized: a.normalized,
                            integer: a.integer,
                            stride: a.stride,
                            offset: a.offset as usize,
                            divisor: a.divisor,
                        })
                    } else {
                        AttribSrc::Value(g.generic.get(loc).copied().unwrap_or([
                            0,
                            0,
                            0,
                            1f32.to_bits(),
                        ]))
                    }
                })
                .collect();
            let units = g.units_view();
            let draw = raster::Draw {
                prog: &prog,
                uniforms: &udata,
                blocks,
                attribs,
                textures: &units,
                mode,
                indices: idx,
                instances,
                fixed: &fixed,
                restart,
            };
            let (w, h, s) = (b.width, b.height, b.samples);
            let mut colors: Vec<Option<&mut Image>> = b
                .colors
                .iter_mut()
                .map(|c| c.as_mut().map(|x| &mut x.0))
                .collect();
            let (depth, stencil): (Option<(&mut [f32], u8)>, Option<&mut [u8]>) = {
                let dimg = b.depth.as_mut().map(|d| &mut d.0);
                let simg = b.stencil.as_mut().map(|d| &mut d.0);
                let mut depth = None;
                let mut stencil = None;
                if let Some(di) = dimg {
                    let bits = di.format.depth_bits;
                    if let Data::Ds(dd, ss) = &mut di.data {
                        if !dd.is_empty() {
                            depth = Some((dd.as_mut_slice(), bits));
                        }
                        if !ss.is_empty() {
                            stencil = Some(ss.as_mut_slice());
                        }
                    }
                }
                if let Some(si) = simg
                    && let Data::Ds(_, ss) = &mut si.data
                    && !ss.is_empty()
                {
                    stencil = Some(ss.as_mut_slice());
                }
                (depth, stencil)
            };
            let mut target = Target {
                width: w,
                height: h,
                samples: s,
                colors: std::mem::take(&mut colors),
                depth,
                stencil,
            };
            let mut budget = 400_000_000u64;
            budget_ok = raster::draw(&draw, &mut target, &mut budget);
        }
        self.put_bufs(b);
        if !budget_ok {
            // A runaway shader: Chrome's GPU watchdog loses the context.
            self.lost = true;
            self.error = CONTEXT_LOST_WEBGL;
        }
    }

    fn units_view(&self) -> Units<'_> {
        let units = self
            .units
            .iter()
            .map(|u| {
                let t = |k: usize| {
                    u.tex[k]
                        .and_then(|id| self.res.get(&id))
                        .filter(|r| !r.deleted)
                        .and_then(|r| r.tex.as_deref())
                };
                gtex::Unit {
                    d2: t(0),
                    cube: t(1),
                    d3: t(2),
                    array: t(3),
                    sampler: u
                        .sampler
                        .and_then(|s| self.res.get(&s))
                        .and_then(|r| r.sampler),
                }
            })
            .collect();
        Units {
            units,
            webgl2: self.webgl2,
            float_linear: self.ext_enabled("OES_texture_float_linear"),
            feedback: Vec::new(),
        }
    }

    // -----------------------------------------------------------------
    // Vertex arrays
    // -----------------------------------------------------------------

    fn bind_vao(&mut self, id: Option<u32>) {
        let cur = self.bound.get(&0x85B5).copied();
        let saved = Vao {
            attribs: std::mem::take(&mut self.attribs),
            element: self.bound.get(&0x8895).copied(),
        };
        match cur.and_then(|c| self.res.get_mut(&c)) {
            Some(r) => r.vao = Some(Box::new(saved)),
            None => self.default_vao = saved,
        }
        let next = match id.and_then(|i| self.res.get_mut(&i)) {
            Some(r) => r.vao.take().map(|b| *b).unwrap_or_default(),
            None => std::mem::take(&mut self.default_vao),
        };
        self.attribs = if next.attribs.is_empty() {
            vec![Attrib::default(); 16]
        } else {
            next.attribs
        };
        match next.element {
            Some(e) => self.bound.insert(0x8895, e),
            None => self.bound.remove(&0x8895),
        };
        match id {
            Some(i) => self.bound.insert(0x85B5, i),
            None => self.bound.remove(&0x85B5),
        };
    }

    // -----------------------------------------------------------------
    // Reading pixels
    // -----------------------------------------------------------------

    /// Pixels of the read framebuffer as (bytes per pixel, rows bottom-up
    /// with `PACK_ALIGNMENT`), only in-bounds rectangle filled.
    fn read_rect(
        &mut self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        format: u32,
        ty: u32,
        dst: &mut [u8],
    ) -> Result<(), u32> {
        let b = self.take_bufs(true)?;
        let res = (|| {
            let Some((img, _)) = b.colors.first().and_then(|c| c.as_ref()) else {
                return Err(INVALID_OPERATION);
            };
            if !b.default && img.samples > 1 {
                return Err(INVALID_OPERATION);
            }
            let kind = img.format.kind;
            let ok = match (format, ty) {
                (RGBA, UNSIGNED_BYTE) => matches!(kind, Kind::Unorm),
                (RGBA, FLOAT_T) => matches!(kind, Kind::Float | Kind::Snorm),
                (0x8D99, INT_T) => kind == Kind::Int,
                (0x8D99, UNSIGNED_INT) => kind == Kind::Uint,
                _ => false,
            };
            if !ok {
                return Err(INVALID_OPERATION);
            }
            let bpp = if ty == UNSIGNED_BYTE { 4 } else { 16 };
            let align = (self.num(0x0D05) as usize).max(1);
            let row = (w as usize * bpp).div_ceil(align) * align;
            let need = if h > 0 {
                row * (h as usize - 1) + w as usize * bpp
            } else {
                0
            };
            if dst.len() < need {
                return Err(INVALID_OPERATION);
            }
            let (fw, fh) = (b.width as i32, b.height as i32);
            if ty == UNSIGNED_BYTE {
                // Stored values with the readback noise (top-down copy).
                let up = img.resolve_rgba8(0);
                let (iw, ih) = (img.width as usize, img.height as usize);
                let mut top = vec![0u8; iw * ih * 4];
                for yy in 0..ih {
                    top[yy * iw * 4..(yy + 1) * iw * 4]
                        .copy_from_slice(&up[(ih - 1 - yy) * iw * 4..(ih - yy) * iw * 4]);
                }
                let mut noised = top.clone();
                if let Some(seed) = self.seed {
                    lb_paint::CanvasNoise::new(seed).apply(
                        &top,
                        iw as u32,
                        ih as u32,
                        0,
                        0,
                        iw as u32,
                        &mut noised,
                    );
                }
                for r in 0..h {
                    let gy = y + r;
                    if gy < 0 || gy >= fh {
                        continue;
                    }
                    let src_row = ih - 1 - gy as usize;
                    for c in 0..w {
                        let gx = x + c;
                        if gx < 0 || gx >= fw {
                            continue;
                        }
                        let s = (src_row * iw + gx as usize) * 4;
                        let d = r as usize * row + c as usize * 4;
                        dst[d..d + 4].copy_from_slice(&noised[s..s + 4]);
                    }
                }
            } else {
                let res = img.resolved();
                for r in 0..h {
                    let gy = y + r;
                    if gy < 0 || gy >= fh {
                        continue;
                    }
                    for c in 0..w {
                        let gx = x + c;
                        if gx < 0 || gx >= fw {
                            continue;
                        }
                        let i = res.index(gx as u32, gy as u32, 0);
                        let v = if ty == FLOAT_T {
                            res.read(i).map(f32::to_bits)
                        } else {
                            res.read_bits(i)
                        };
                        let d = r as usize * row + c as usize * 16;
                        for k in 0..4 {
                            dst[d + k * 4..d + k * 4 + 4].copy_from_slice(&v[k].to_le_bytes());
                        }
                    }
                }
            }
            Ok(())
        })();
        self.put_bufs(b);
        res
    }

    // -----------------------------------------------------------------
    // Textures
    // -----------------------------------------------------------------

    /// Bound texture for an image target, with its slot and face.
    fn tex_for(&mut self, target: u32) -> Result<(u32, usize, usize), u32> {
        let Some((slot, face)) = image_target(target) else {
            return Err(INVALID_ENUM);
        };
        if !self.webgl2 && slot >= 2 {
            return Err(INVALID_ENUM);
        }
        let Some(id) = self.tex_bound(slot) else {
            return Err(INVALID_OPERATION);
        };
        Ok((id, slot, face))
    }

    fn unpack(&self) -> Unpack {
        Unpack {
            align: (self.num(0x0CF5) as usize).max(1),
            flip: self.flag(0x9240),
            premultiply: self.flag(0x9241),
            row_length: self.num(0x0CF2) as usize,
            skip_rows: self.num(0x0CF3) as usize,
            skip_pixels: self.num(0x0CF4) as usize,
            image_height: self.num(0x806E) as usize,
            skip_images: self.num(0x806D) as usize,
        }
    }

    /// Effective internal format of a `texImage*` call.
    fn tex_format(&self, internal: u32, format: u32, ty: u32) -> Result<Format, u32> {
        let ty = if ty == HALF_FLOAT_OES { HALF_FLOAT } else { ty };
        if let Some(f) = gimg::unsized_format(internal, ty) {
            if internal != format {
                return Err(INVALID_OPERATION);
            }
            if !self.webgl2 {
                let ok = match ty {
                    FLOAT_T => self.ext_enabled("OES_texture_float"),
                    HALF_FLOAT => self.ext_enabled("OES_texture_half_float"),
                    0x1403 | 0x1405 | 0x84FA => self.ext_enabled("WEBGL_depth_texture"),
                    _ => true,
                };
                if !ok {
                    return Err(INVALID_ENUM);
                }
            }
            return Ok(f);
        }
        if !self.webgl2 {
            return Err(INVALID_VALUE);
        }
        gimg::sized(internal).ok_or(INVALID_VALUE)
    }
}

/// Bytes of one vertex of an attribute array.
fn attrib_bytes(a: &Attrib) -> usize {
    let t = match a.ty {
        0x1400 | 0x1401 => 1,
        0x1402 | 0x1403 | 0x140B => 2,
        0x8D9F | 0x8368 => return 4,
        _ => 4,
    };
    t * a.size.max(1) as usize
}

fn copy_layer(src: &Image, sl: u32, dst: &mut Image, dl: u32) {
    let n = src.width as usize * src.height as usize * src.samples as usize;
    let (so, d0) = (sl as usize * n, dl as usize * n);
    match (&src.data, &mut dst.data) {
        (Data::U8(a), Data::U8(b)) => {
            b[d0 * 4..(d0 + n) * 4].copy_from_slice(&a[so * 4..(so + n) * 4])
        }
        (Data::F32(a), Data::F32(b)) => {
            b[d0 * 4..(d0 + n) * 4].copy_from_slice(&a[so * 4..(so + n) * 4])
        }
        (Data::I32(a), Data::I32(b)) => {
            b[d0 * 4..(d0 + n) * 4].copy_from_slice(&a[so * 4..(so + n) * 4])
        }
        (Data::U32(a), Data::U32(b)) => {
            b[d0 * 4..(d0 + n) * 4].copy_from_slice(&a[so * 4..(so + n) * 4])
        }
        (Data::Ds(ad, as_), Data::Ds(bd, bs)) => {
            if !ad.is_empty() {
                bd[d0..d0 + n].copy_from_slice(&ad[so..so + n]);
            }
            if !as_.is_empty() {
                bs[d0..d0 + n].copy_from_slice(&as_[so..so + n]);
            }
        }
        _ => {}
    }
}

#[derive(Clone, Copy)]
struct Unpack {
    align: usize,
    flip: bool,
    premultiply: bool,
    row_length: usize,
    skip_rows: usize,
    skip_pixels: usize,
    image_height: usize,
    skip_images: usize,
}

/// Source texel values: normalized floats or raw integers.
enum Texels {
    F(Vec<[f32; 4]>),
    I(Vec<[u32; 4]>),
    /// Depth and stencil.
    D(Vec<(f32, u8)>),
}

/// Components of a client format.
fn format_comps(format: u32) -> Option<usize> {
    Some(match format {
        0x1908 | 0x8D99 => 4,
        0x1907 | 0x8D98 => 3,
        0x8227 | 0x8228 | 0x190A => 2,
        0x1903 | 0x8D94 | 0x1909 | 0x1906 | 0x1902 => 1,
        0x84F9 => 2,
        0x8C40 => 3,
        0x8C42 => 4,
        _ => return None,
    })
}

fn type_bytes(ty: u32, comps: usize) -> Option<usize> {
    Some(match ty {
        0x1400 | 0x1401 => comps,
        0x1402 | 0x1403 | HALF_FLOAT | HALF_FLOAT_OES => 2 * comps,
        0x1404 | 0x1405 | FLOAT_T => 4 * comps,
        0x8363 | 0x8033 | 0x8034 => 2,
        0x8368 | 0x8C3B | 0x8C3E | 0x84FA => 4,
        0x8DAD => 8,
        _ => return None,
    })
}

/// Decodes client pixel data (`ArrayBufferView`) into texels.
#[allow(clippy::too_many_arguments)]
fn decode_texels(
    bytes: &[u8],
    w: usize,
    h: usize,
    d: usize,
    format: u32,
    ty: u32,
    up: &Unpack,
    integer: bool,
) -> Result<Texels, u32> {
    let comps = format_comps(format).ok_or(INVALID_ENUM)?;
    let bpp = type_bytes(ty, comps).ok_or(INVALID_ENUM)?;
    let row_px = if up.row_length > 0 { up.row_length } else { w };
    let row = (row_px * bpp).div_ceil(up.align) * up.align;
    let img_rows = if up.image_height > 0 {
        up.image_height
    } else {
        h
    };
    let start = up.skip_images * img_rows * row + up.skip_rows * row + up.skip_pixels * bpp;
    let need = if w == 0 || h == 0 || d == 0 {
        0
    } else {
        start + (d - 1) * img_rows * row + (h - 1) * row + w * bpp
    };
    if bytes.len() < need {
        return Err(INVALID_OPERATION);
    }
    let rd16 = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    let rd32 = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let mut f = Vec::new();
    let mut iv = Vec::new();
    let mut dv = Vec::new();
    for z in 0..d {
        for yy in 0..h {
            let sy = if up.flip { h - 1 - yy } else { yy };
            for x in 0..w {
                let o = start + z * img_rows * row + sy * row + x * bpp;
                if format == 0x1902 || format == 0x84F9 {
                    let (dep, st) = match ty {
                        0x1403 => (f32::from(rd16(o)) / 65535.0, 0),
                        0x1405 => (rd32(o) as f32 / 4_294_967_295.0, 0),
                        FLOAT_T => (f32::from_bits(rd32(o)), 0),
                        0x84FA => {
                            let v = rd32(o);
                            ((v >> 8) as f32 / 16_777_215.0, (v & 0xFF) as u8)
                        }
                        0x8DAD => (f32::from_bits(rd32(o)), bytes[o + 4]),
                        _ => (0.0, 0),
                    };
                    dv.push((dep, st));
                    continue;
                }
                let mut c = [0f32, 0.0, 0.0, 1.0];
                let mut ci = [0u32, 0, 0, 1];
                match ty {
                    0x8363 => {
                        let v = rd16(o);
                        c[0] = f32::from(v >> 11) / 31.0;
                        c[1] = f32::from((v >> 5) & 63) / 63.0;
                        c[2] = f32::from(v & 31) / 31.0;
                    }
                    0x8033 => {
                        let v = rd16(o);
                        for k in 0..4 {
                            c[k] = f32::from((v >> (12 - 4 * k)) & 15) / 15.0;
                        }
                    }
                    0x8034 => {
                        let v = rd16(o);
                        c[0] = f32::from(v >> 11) / 31.0;
                        c[1] = f32::from((v >> 6) & 31) / 31.0;
                        c[2] = f32::from((v >> 1) & 31) / 31.0;
                        c[3] = f32::from(v & 1);
                    }
                    0x8368 => {
                        let v = rd32(o);
                        let parts = [v & 1023, (v >> 10) & 1023, (v >> 20) & 1023, v >> 30];
                        for k in 0..4 {
                            ci[k] = parts[k];
                            c[k] = parts[k] as f32 / if k == 3 { 3.0 } else { 1023.0 };
                        }
                    }
                    0x8C3B => {
                        let v = rd32(o);
                        c[0] = uf11(v & 0x7FF);
                        c[1] = uf11((v >> 11) & 0x7FF);
                        c[2] = uf10(v >> 22);
                    }
                    0x8C3E => {
                        let v = rd32(o);
                        let e = (v >> 27) as i32 - 15 - 9;
                        let s = 2f32.powi(e);
                        c[0] = (v & 511) as f32 * s;
                        c[1] = ((v >> 9) & 511) as f32 * s;
                        c[2] = ((v >> 18) & 511) as f32 * s;
                    }
                    _ => {
                        let cs = bpp / comps;
                        for k in 0..comps {
                            let p = o + k * cs;
                            let (fv, iv_) = match ty {
                                0x1401 => (f32::from(bytes[p]) / 255.0, u32::from(bytes[p])),
                                0x1400 => {
                                    let v = bytes[p] as i8;
                                    ((f32::from(v) / 127.0).max(-1.0), v as i32 as u32)
                                }
                                0x1403 => (f32::from(rd16(p)) / 65535.0, u32::from(rd16(p))),
                                0x1402 => {
                                    let v = rd16(p) as i16;
                                    ((f32::from(v) / 32767.0).max(-1.0), v as i32 as u32)
                                }
                                0x1405 => (rd32(p) as f32 / 4_294_967_295.0, rd32(p)),
                                0x1404 => (rd32(p) as i32 as f32, rd32(p)),
                                FLOAT_T => (f32::from_bits(rd32(p)), rd32(p)),
                                _ => (lb_gl::math::f16_to_f32(rd16(p)), 0),
                            };
                            c[k] = fv;
                            ci[k] = iv_;
                        }
                        // Luminance/alpha keep their stored channel order.
                    }
                }
                if up.premultiply && comps == 4 && !integer {
                    for k in 0..3 {
                        c[k] *= c[3];
                    }
                }
                if integer {
                    iv.push(ci);
                } else {
                    f.push(c);
                }
            }
        }
    }
    Ok(if !dv.is_empty() {
        Texels::D(dv)
    } else if integer {
        Texels::I(iv)
    } else {
        Texels::F(f)
    })
}

fn uf11(v: u32) -> f32 {
    let e = (v >> 6) as i32;
    let m = (v & 63) as f32;
    if e == 0 {
        m / 64.0 * 2f32.powi(-14)
    } else {
        (1.0 + m / 64.0) * 2f32.powi(e - 15)
    }
}
fn uf10(v: u32) -> f32 {
    let e = (v >> 5) as i32;
    let m = (v & 31) as f32;
    if e == 0 {
        m / 32.0 * 2f32.powi(-14)
    } else {
        (1.0 + m / 32.0) * 2f32.powi(e - 15)
    }
}

/// Writes texels into a region of an image layer.
#[allow(clippy::too_many_arguments)]
fn write_texels(img: &mut Image, t: &Texels, x0: u32, y0: u32, z0: u32, w: u32, h: u32, d: u32) {
    let mut i = 0;
    for z in 0..d {
        for y in 0..h {
            for x in 0..w {
                let (px, py, pz) = (x0 + x, y0 + y, z0 + z);
                if px >= img.width || py >= img.height || pz >= img.layers {
                    i += 1;
                    continue;
                }
                let idx = img.index(px, py, pz);
                match t {
                    Texels::F(v) => {
                        let mut c = v[i];
                        if img.format.srgb {
                            // Client data is already sRGB-encoded.
                        }
                        if img.format.kind == Kind::Unorm {
                            c = c.map(|x| x.clamp(0.0, 1.0));
                        }
                        img.write(idx, c, [true; 4]);
                    }
                    Texels::I(v) => img.write_bits(idx, v[i], [true; 4]),
                    Texels::D(v) => {
                        if let Data::Ds(dd, ss) = &mut img.data {
                            if let Some(p) = dd.get_mut(idx) {
                                *p = gimg::quantize_depth(
                                    v[i].0.clamp(0.0, 1.0),
                                    img.format.depth_bits,
                                );
                            }
                            if let Some(p) = ss.get_mut(idx) {
                                *p = v[i].1;
                            }
                        }
                    }
                }
                i += 1;
            }
        }
    }
}

/// RGBA8 pixels of a DOM source (unpremultiplied unless asked) as texels.
fn dom_texels(px: &lb_paint::tiny_skia::Pixmap, up: &Unpack) -> (u32, u32, Texels) {
    let (w, h) = (px.width(), px.height());
    let data = px.data();
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h as usize {
        let sy = if up.flip { h as usize - 1 - y } else { y };
        for x in 0..w as usize {
            let o = (sy * w as usize + x) * 4;
            let a = f32::from(data[o + 3]) / 255.0;
            let mut c = [
                f32::from(data[o]) / 255.0,
                f32::from(data[o + 1]) / 255.0,
                f32::from(data[o + 2]) / 255.0,
                a,
            ];
            if !up.premultiply && a > 0.0 && a < 1.0 {
                // Demultiply like Skia (rounded 8-bit result).
                for v in c.iter_mut().take(3) {
                    *v = ((*v / a) * 255.0).round().min(255.0) / 255.0;
                }
            }
            out.push(c);
        }
    }
    // DOM sources are top-down; GL rows are bottom-up only for the API's
    // y axis, which uses the first row as y = 0 (no flip).
    (w, h, Texels::F(out))
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

fn loc_of(w: &Window, cx: &mut CallCtx<'_, '_, '_>, i: usize) -> Option<(u32, u32, u32, u32)> {
    let h = cx.arg_host(i).filter(|h| h.kind == K_OBJ)?;
    with_gl(w, h.id, |g| match g {
        GlObj::Location {
            program,
            index,
            elem,
            lgen,
        } => Some((*program, *index, *elem, *lgen)),
        _ => None,
    })
}

/// Numbers of a sequence or typed array argument (`kind` decides how a
/// typed array's bytes are read).
fn arg_numbers(cx: &mut CallCtx<'_, '_, '_>, i: usize) -> Option<Vec<f64>> {
    match cx.arg_view_kind(i) {
        Some(k) => {
            let b = cx.arg_bytes(i)?;
            Some(match k {
                ViewKind::Float32 => b
                    .chunks_exact(4)
                    .map(|c| f64::from(f32::from_ne_bytes([c[0], c[1], c[2], c[3]])))
                    .collect(),
                ViewKind::Int32 => b
                    .chunks_exact(4)
                    .map(|c| f64::from(i32::from_ne_bytes([c[0], c[1], c[2], c[3]])))
                    .collect(),
                ViewKind::Uint32 => b
                    .chunks_exact(4)
                    .map(|c| f64::from(u32::from_ne_bytes([c[0], c[1], c[2], c[3]])))
                    .collect(),
                ViewKind::Float64 => b
                    .chunks_exact(8)
                    .map(|c| f64::from_ne_bytes(c.try_into().unwrap_or([0; 8])))
                    .collect(),
                ViewKind::Int16 => b
                    .chunks_exact(2)
                    .map(|c| f64::from(i16::from_ne_bytes([c[0], c[1]])))
                    .collect(),
                ViewKind::Uint16 => b
                    .chunks_exact(2)
                    .map(|c| f64::from(u16::from_ne_bytes([c[0], c[1]])))
                    .collect(),
                ViewKind::Int8 => b.iter().map(|x| f64::from(*x as i8)).collect(),
                _ => b.iter().map(|x| f64::from(*x)).collect(),
            })
        }
        None => match cx.arg_value(i) {
            JsValue::Json(s) => serde_json::from_str::<Vec<serde_json::Value>>(&s)
                .ok()
                .map(|v| {
                    v.iter()
                        .map(|x| {
                            x.as_f64()
                                .or_else(|| x.as_bool().map(|b| f64::from(u8::from(b))))
                                .unwrap_or(f64::NAN)
                        })
                        .collect()
                }),
            _ => None,
        },
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UKind {
    F,
    I,
    U,
}

pub(super) fn register(reg: &mut Registry, rt: &Runtime) {
    register_programs(reg, rt);
    register_uniforms(reg, rt);
    register_attribs(reg, rt);
    register_textures(reg, rt);
    register_framebuffers(reg, rt);
    register_draws(reg, rt);
    register_state2(reg, rt);
}

fn register_programs(reg: &mut Registry, rt: &Runtime) {
    gl_op(reg, rt, "compileShader", |cx, w, gl| {
        let Some(id) = arg_res(cx, w, 0, gl) else {
            return Ok(());
        };
        let mut g = gl.borrow_mut();
        let opts = lb_gl::glsl::Options {
            webgl2: g.webgl2,
            extensions: glsl_extensions(&g),
            max_draw_buffers: if g.webgl2 || g.ext_enabled("WEBGL_draw_buffers") {
                8
            } else {
                1
            },
        };
        let Some(r) = g.res.get_mut(&id) else {
            return Ok(());
        };
        let stage = if r.shader_type == VERTEX_SHADER {
            lb_gl::glsl::Stage::Vertex
        } else {
            lb_gl::glsl::Stage::Fragment
        };
        let (sh, log) = lb_gl::glsl::compile(&r.source, stage, &opts);
        r.compiled = sh.is_some();
        r.shader = sh.map(Rc::new);
        r.info_log = log;
        Ok(())
    });
    gl_op(reg, rt, "getShaderInfoLog", |cx, w, gl| {
        let r = arg_res(cx, w, 0, gl)
            .and_then(|id| gl.borrow().res.get(&id).map(|r| r.info_log.clone()));
        match r {
            Some(s) => cx.ret(s),
            None => cx.ret(Arg::Null),
        }
    });
    gl_op(reg, rt, "linkProgram", |cx, w, gl| {
        let Some(p) = arg_res(cx, w, 0, gl) else {
            return Ok(());
        };
        let mut g = gl.borrow_mut();
        let shaders: Vec<(u32, bool, Option<Rc<lb_gl::ir::Shader>>)> = g
            .res
            .get(&p)
            .map(|r| {
                r.shaders
                    .iter()
                    .filter_map(|s| {
                        g.res
                            .get(s)
                            .map(|x| (x.shader_type, x.compiled, x.shader.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let vs = shaders.iter().find(|s| s.0 == VERTEX_SHADER).cloned();
        let fs = shaders.iter().find(|s| s.0 == FRAGMENT_SHADER).cloned();
        let bound: HashMap<String, u32> = g
            .res
            .get(&p)
            .map(|r| {
                r.bound_locations
                    .iter()
                    .map(|(k, v)| (k.clone(), *v as u32))
                    .collect()
            })
            .unwrap_or_default();
        let result: Result<Program, String> = match (&vs, &fs) {
            (None, None) => Err("No compiled shaders.\n".into()),
            (None, _) | (_, None) => Err(
                "The program must contain objects to form both a vertex and fragment shader.\n"
                    .into(),
            ),
            (Some((_, false, _)), _) => Err("Vertex shader is not compiled.\n".into()),
            (_, Some((_, false, _))) => Err("Fragment shader is not compiled.\n".into()),
            (Some((_, _, Some(v))), Some((_, _, Some(f)))) => lb_gl::program::link(
                v.clone(),
                f.clone(),
                &LinkOptions {
                    max_attribs: 16,
                    bound_locations: bound,
                },
            ),
            _ => Err("No compiled shaders.\n".into()),
        };
        if let Some(r) = g.res.get_mut(&p) {
            r.link_gen += 1;
            match result {
                Ok(prog) => {
                    r.udata = vec![0; prog.uniform_cells as usize];
                    r.block_bind = prog.blocks.iter().map(|b| b.binding).collect();
                    r.prog = Some(Rc::new(prog));
                    r.linked = true;
                    r.prog_log = String::new();
                }
                Err(log) => {
                    r.prog = None;
                    r.linked = false;
                    r.prog_log = log;
                }
            }
        }
        Ok(())
    });
    gl_op(reg, rt, "getProgramParameter", |cx, w, gl| {
        let p = cx.arg_u32(1)?;
        let Some(id) = arg_res(cx, w, 0, gl) else {
            return cx.ret(Arg::Null);
        };
        let g = gl.borrow();
        let webgl2 = g.webgl2;
        let Some(r) = g.res.get(&id) else {
            drop(g);
            return cx.ret(Arg::Null);
        };
        let prog = r.prog.clone();
        let v = match p {
            LINK_STATUS => Some(Arg::Bool(r.linked)),
            DELETE_STATUS => Some(Arg::Bool(r.deleted)),
            VALIDATE_STATUS => Some(Arg::Bool(r.linked)),
            ATTACHED_SHADERS => Some(Arg::Number(r.shaders.len() as f64)),
            ACTIVE_ATTRIBUTES => Some(Arg::Number(
                prog.as_ref().map_or(0, |p| p.attribs.len()) as f64
            )),
            ACTIVE_UNIFORMS => Some(Arg::Number(
                prog.as_ref().map_or(0, |p| p.uniforms.len()) as f64
            )),
            TRANSFORM_FEEDBACK_BUFFER_MODE if webgl2 => {
                Some(Arg::Number(f64::from(SEPARATE_ATTRIBS)))
            }
            TRANSFORM_FEEDBACK_VARYINGS if webgl2 => Some(Arg::Number(0.0)),
            ACTIVE_UNIFORM_BLOCKS if webgl2 => Some(Arg::Number(
                prog.as_ref().map_or(0, |p| p.blocks.len()) as f64,
            )),
            _ => None,
        };
        drop(g);
        match v {
            Some(a) => cx.ret(a),
            None => {
                gl.borrow_mut().set_error(INVALID_ENUM);
                cx.ret(Arg::Null)
            }
        }
    });
    gl_op(reg, rt, "getProgramInfoLog", |cx, w, gl| {
        let r = arg_res(cx, w, 0, gl)
            .and_then(|id| gl.borrow().res.get(&id).map(|r| r.prog_log.clone()));
        match r {
            Some(s) => cx.ret(s),
            None => cx.ret(Arg::Null),
        }
    });
    gl_op(reg, rt, "getAttribLocation", |cx, w, gl| {
        let name = cx.arg_string(1)?;
        let loc = arg_res(cx, w, 0, gl).and_then(|id| {
            let g = gl.borrow();
            let p = g.res.get(&id)?.prog.clone()?;
            p.attribs
                .iter()
                .find(|a| a.name == name)
                .map(|a| a.location as i32)
        });
        cx.ret(loc.unwrap_or(-1))
    });
    gl_op(reg, rt, "getUniformLocation", |cx, w, gl| {
        let name = cx.arg_string(1)?;
        let Some(p) = arg_res(cx, w, 0, gl) else {
            return cx.ret(Arg::Null);
        };
        let found = {
            let g = gl.borrow();
            g.res.get(&p).and_then(|r| {
                let prog = r.prog.as_ref()?;
                if name.starts_with("gl_")
                    || name.starts_with("webgl_")
                    || name.starts_with("_webgl_")
                {
                    return None;
                }
                prog.location(&name).map(|(i, e)| (i, e, r.link_gen))
            })
        };
        let Some((index, elem, lgen)) = found else {
            return cx.ret(Arg::Null);
        };
        let id = new_gl(
            w,
            GlObj::Location {
                program: p,
                index,
                elem,
                lgen,
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
            let v = {
                let g = gl.borrow();
                g.res.get(&p).and_then(|r| r.prog.clone()).and_then(|prog| {
                    if attr {
                        prog.attribs
                            .get(i)
                            .map(|a| (a.name.clone(), 1, a.ty.gl_enum()))
                    } else {
                        prog.uniforms
                            .get(i)
                            .map(|u| (u.name.clone(), u.size as i32, u.ty.gl_enum()))
                    }
                })
            };
            let Some((name, size, ty)) = v else {
                gl.borrow_mut().set_error(INVALID_VALUE);
                return cx.ret(Arg::Null);
            };
            let id = new_gl(w, GlObj::ActiveInfo { name, size, ty });
            cx.ret(w.obj_arg(id))
        });
    }
    gl_op(reg, rt, "getUniform", |cx, w, gl| {
        let Some(p) = arg_res(cx, w, 0, gl) else {
            return cx.ret(Arg::Null);
        };
        let Some((lp, index, elem, lgen)) = loc_of(w, cx, 1) else {
            return cx.ret(Arg::Null);
        };
        let v = {
            let g = gl.borrow();
            let r = g.res.get(&p);
            r.filter(|r| lp == p && r.link_gen == lgen).and_then(|r| {
                let prog = r.prog.as_ref()?;
                let u = prog.uniforms.get(index as usize)?;
                let n = u.ty.comps() as usize;
                let off = (u.offset + elem * u.ty.comps()) as usize;
                Some((u.ty, r.udata.get(off..off + n)?.to_vec()))
            })
        };
        let Some((ty, cells)) = v else {
            gl.borrow_mut().set_error(INVALID_OPERATION);
            return cx.ret(Arg::Null);
        };
        use lb_gl::glsl::types::{Base, Scalar};
        let one = cells.len() == 1;
        match ty.base {
            Base::S(Scalar::Float) if one => cx.ret(f64::from(f32::from_bits(cells[0]))),
            Base::S(Scalar::Float) => cx.ret(Arg::Float32Array(
                cells.iter().map(|c| f32::from_bits(*c)).collect(),
            )),
            Base::S(Scalar::Int) | Base::Sampler(_) if one => cx.ret(f64::from(cells[0] as i32)),
            Base::S(Scalar::Int) => {
                cx.ret(Arg::Int32Array(cells.iter().map(|c| *c as i32).collect()))
            }
            Base::S(Scalar::Uint) if one => cx.ret(f64::from(cells[0])),
            Base::S(Scalar::Uint) => cx.ret(Arg::Uint32Array(cells)),
            Base::S(Scalar::Bool) if one => cx.ret(cells[0] != 0),
            Base::S(Scalar::Bool) => cx.ret(Arg::Array(
                cells.iter().map(|c| Arg::Bool(*c != 0)).collect(),
            )),
            _ => cx.ret(Arg::Null),
        }
    });
    gl_op(reg, rt, "getUniformBlockIndex", |cx, w, gl| {
        let name = cx.arg_string(1)?;
        let v = arg_res(cx, w, 0, gl).and_then(|p| {
            let g = gl.borrow();
            let prog = g.res.get(&p)?.prog.clone()?;
            prog.blocks.iter().position(|b| b.name == name)
        });
        cx.ret(v.map_or(4_294_967_295.0, |i| i as f64))
    });
    gl_op(reg, rt, "getActiveUniformBlockName", |cx, w, gl| {
        let i = cx.arg_u32(1)? as usize;
        let v = arg_res(cx, w, 0, gl).and_then(|p| {
            let g = gl.borrow();
            g.res
                .get(&p)?
                .prog
                .clone()?
                .blocks
                .get(i)
                .map(|b| b.name.clone())
        });
        match v {
            Some(n) => cx.ret(n),
            None => {
                gl.borrow_mut().set_error(INVALID_VALUE);
                cx.ret(Arg::Null)
            }
        }
    });
    gl_op(reg, rt, "getActiveUniformBlockParameter", |cx, w, gl| {
        let i = cx.arg_u32(1)? as usize;
        let pname = cx.arg_u32(2)?;
        let v = arg_res(cx, w, 0, gl).and_then(|p| {
            let g = gl.borrow();
            let r = g.res.get(&p)?;
            let prog = r.prog.clone()?;
            let b = prog.blocks.get(i)?;
            Some(match pname {
                0x8A3F => Arg::Number(f64::from(r.block_bind.get(i).copied().unwrap_or(0))),
                0x8A40 => Arg::Number(f64::from(b.data_size)),
                0x8A42 => Arg::Number(b.members.len() as f64),
                0x8A43 => Arg::Uint32Array(b.members.clone()),
                0x8A44 => Arg::Bool(b.vs),
                0x8A46 => Arg::Bool(b.fs),
                _ => return None,
            })
        });
        match v {
            Some(a) => cx.ret(a),
            None => {
                gl.borrow_mut().set_error(INVALID_ENUM);
                cx.ret(Arg::Null)
            }
        }
    });
    gl_op(reg, rt, "uniformBlockBinding", |cx, w, gl| {
        let i = cx.arg_u32(1)? as usize;
        let b = cx.arg_u32(2)?;
        if let Some(p) = arg_res(cx, w, 0, gl) {
            let mut g = gl.borrow_mut();
            let bad = match g.res.get_mut(&p).and_then(|r| r.block_bind.get_mut(i)) {
                Some(slot) if b < 72 => {
                    *slot = b;
                    false
                }
                _ => true,
            };
            if bad {
                g.set_error(INVALID_VALUE);
            }
        }
        Ok(())
    });
    gl_op(reg, rt, "getUniformIndices", |cx, w, gl| {
        let names = match cx.arg_value(1) {
            JsValue::Json(s) => serde_json::from_str::<Vec<String>>(&s).unwrap_or_default(),
            _ => Vec::new(),
        };
        let v = arg_res(cx, w, 0, gl).and_then(|p| {
            let g = gl.borrow();
            let prog = g.res.get(&p)?.prog.clone()?;
            Some(
                names
                    .iter()
                    .map(|n| {
                        prog.uniforms
                            .iter()
                            .position(|u| {
                                &u.name == n || u.name.strip_suffix("[0]") == Some(n.as_str())
                            })
                            .map_or(4_294_967_295.0, |i| i as f64)
                    })
                    .map(Arg::Number)
                    .collect::<Vec<_>>(),
            )
        });
        match v {
            Some(a) => cx.ret(Arg::Array(a)),
            None => cx.ret(Arg::Null),
        }
    });
    gl_op(reg, rt, "getActiveUniforms", |cx, w, gl| {
        let idx: Vec<usize> = arg_numbers(cx, 1)
            .unwrap_or_default()
            .into_iter()
            .map(|x| x as usize)
            .collect();
        let pname = cx.arg_u32(2)?;
        let v = arg_res(cx, w, 0, gl).and_then(|p| {
            let g = gl.borrow();
            let prog = g.res.get(&p)?.prog.clone()?;
            let mut out = Vec::new();
            for i in idx {
                let u = prog.uniforms.get(i)?;
                let b = u.block;
                out.push(match pname {
                    0x8A37 => Arg::Number(f64::from(u.ty.gl_enum())),
                    0x8A38 => Arg::Number(f64::from(u.size)),
                    0x8A3A => Arg::Number(b.map_or(-1.0, |x| f64::from(x.0))),
                    0x8A3B => Arg::Number(b.map_or(-1.0, |x| f64::from(x.1))),
                    0x8A3C => Arg::Number(b.map_or(-1.0, |x| f64::from(x.2))),
                    0x8A3D => Arg::Number(b.map_or(-1.0, |x| f64::from(x.3))),
                    0x8A3E => Arg::Bool(b.is_some_and(|x| x.4)),
                    _ => return None,
                });
            }
            Some(out)
        });
        match v {
            Some(a) => cx.ret(Arg::Array(a)),
            None => {
                gl.borrow_mut().set_error(INVALID_VALUE);
                cx.ret(Arg::Null)
            }
        }
    });
    gl_op(reg, rt, "getFragDataLocation", |cx, w, gl| {
        let name = cx.arg_string(1)?;
        let v = arg_res(cx, w, 0, gl).and_then(|p| {
            let g = gl.borrow();
            let prog = g.res.get(&p)?.prog.clone()?;
            let o = prog
                .fs
                .outputs
                .iter()
                .enumerate()
                .find(|(_, o)| o.name == name)?;
            Some(o.1.location.unwrap_or(o.0 as u32) as i32)
        });
        cx.ret(v.unwrap_or(-1))
    });
    gl_op(reg, rt, "validateProgram", |_cx, _w, _gl| Ok(()));
}

fn register_uniforms(reg: &mut Registry, rt: &Runtime) {
    let names: Vec<&'static str> = IFACES
        .iter()
        .filter_map(|i| crate::generated::chromium::TABLE.interface(i))
        .flat_map(|d| d.operations.iter().map(|o| o.name.get()))
        .filter(|n| n.starts_with("uniform") && !n.starts_with("uniformBlock"))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    for name in names {
        let vector = name.ends_with('v');
        let matrix = name.starts_with("uniformMatrix");
        let core = name
            .trim_start_matches("uniformMatrix")
            .trim_start_matches("uniform")
            .trim_end_matches('v');
        let (kind, comps, mat) = if matrix {
            let d = core.trim_end_matches('f');
            let (c, r) = match d.split_once('x') {
                Some((c, r)) => (c.parse::<u8>().unwrap_or(2), r.parse::<u8>().unwrap_or(2)),
                None => {
                    let n = d.parse::<u8>().unwrap_or(2);
                    (n, n)
                }
            };
            (UKind::F, u32::from(c) * u32::from(r), Some((c, r)))
        } else {
            let n = core
                .chars()
                .next()
                .and_then(|c| c.to_digit(10))
                .unwrap_or(1);
            let k = if core.ends_with("ui") {
                UKind::U
            } else if core.ends_with('i') {
                UKind::I
            } else {
                UKind::F
            };
            (k, n, None)
        };
        gl_op(reg, rt, name, move |cx, w, gl| {
            let Some((lp, index, elem, lgen)) = loc_of(w, cx, 0) else {
                return Ok(());
            };
            let first = if matrix { 2 } else { 1 };
            let transpose = matrix && cx.arg_bool(1);
            let mut vals: Vec<f64> = if vector {
                match arg_numbers(cx, first) {
                    Some(v) => v,
                    None => {
                        return Err(HostError::type_error(format!(
                            "Failed to execute '{name}' on 'WebGLRenderingContext': The provided value cannot be converted to a sequence."
                        )));
                    }
                }
            } else {
                let mut v = Vec::new();
                for i in 1..=comps as usize {
                    v.push(cx.arg_f64(i)?);
                }
                v
            };
            // WebGL 2 srcOffset / srcLength.
            if vector && cx.len() > first + 1 {
                let off = cx.arg_f64(first + 1)?.max(0.0) as usize;
                let len = if cx.len() > first + 2 {
                    cx.arg_f64(first + 2)?.max(0.0) as usize
                } else {
                    0
                };
                if off > vals.len() {
                    gl.borrow_mut().set_error(INVALID_VALUE);
                    return Ok(());
                }
                vals = vals[off..].to_vec();
                if len > 0 {
                    if len > vals.len() {
                        gl.borrow_mut().set_error(INVALID_VALUE);
                        return Ok(());
                    }
                    vals.truncate(len);
                }
            }
            let mut g = gl.borrow_mut();
            let cur = g.bound.get(&0x8B8D).copied();
            if cur != Some(lp) {
                g.set_error(INVALID_OPERATION);
                return Ok(());
            }
            if transpose && !g.webgl2 {
                g.set_error(INVALID_VALUE);
                return Ok(());
            }
            let max_units = 32;
            let Some(r) = g.res.get_mut(&lp) else {
                return Ok(());
            };
            if r.link_gen != lgen {
                g.set_error(INVALID_OPERATION);
                return Ok(());
            }
            let Some(prog) = r.prog.clone() else {
                return Ok(());
            };
            let Some(u) = prog.uniforms.get(index as usize) else {
                return Ok(());
            };
            use lb_gl::glsl::types::{Base, Scalar};
            let ok = match (u.ty.base, kind) {
                (Base::S(Scalar::Float), UKind::F) => {
                    u.ty.comps() == comps && (mat.is_some() == (u.ty.cols > 1))
                }
                (Base::S(Scalar::Int), UKind::I) | (Base::S(Scalar::Uint), UKind::U) => {
                    u.ty.comps() == comps
                }
                (Base::S(Scalar::Bool), _) => u.ty.comps() == comps && mat.is_none(),
                (Base::Sampler(_), UKind::I) => comps == 1,
                _ => false,
            };
            if !ok {
                g.set_error(INVALID_OPERATION);
                return Ok(());
            }
            if vals.is_empty() || vals.len() % comps as usize != 0 {
                g.set_error(INVALID_VALUE);
                return Ok(());
            }
            let count = vals.len() / comps as usize;
            if count > 1 && u.size == 1 {
                g.set_error(INVALID_OPERATION);
                return Ok(());
            }
            if u.ty.is_sampler() && vals.iter().any(|v| *v < 0.0 || *v >= f64::from(max_units)) {
                g.set_error(INVALID_VALUE);
                return Ok(());
            }
            let n = count.min((u.size - elem) as usize);
            let Some(r) = g.res.get_mut(&lp) else {
                return Ok(());
            };
            for e in 0..n {
                let mut chunk: Vec<f64> =
                    vals[e * comps as usize..(e + 1) * comps as usize].to_vec();
                if let (true, Some((c, rr))) = (transpose, mat) {
                    let src = chunk.clone();
                    // Given rows of length c: element (row i, col j) → col j row i.
                    for j in 0..c as usize {
                        for i in 0..rr as usize {
                            chunk[j * rr as usize + i] = src[i * c as usize + j];
                        }
                    }
                }
                for (k, v) in chunk.iter().enumerate() {
                    let cell = match (u.ty.base, kind) {
                        (Base::S(Scalar::Bool), UKind::F) => u32::from(*v != 0.0),
                        (Base::S(Scalar::Bool), _) => u32::from(*v != 0.0),
                        (_, UKind::F) => (*v as f32).to_bits(),
                        (_, UKind::I) => (*v as i64) as i32 as u32,
                        (_, UKind::U) => (*v as i64) as u32,
                    };
                    let off = (u.offset + (elem + e as u32) * comps) as usize + k;
                    if let Some(slot) = r.udata.get_mut(off) {
                        *slot = cell;
                    }
                }
            }
            Ok(())
        });
    }
}

fn register_attribs(reg: &mut Registry, rt: &Runtime) {
    for (name, on) in [
        ("enableVertexAttribArray", true),
        ("disableVertexAttribArray", false),
    ] {
        gl_op(reg, rt, name, move |cx, _w, gl| {
            let i = cx.arg_u32(0)? as usize;
            let mut g = gl.borrow_mut();
            match g.attribs.get_mut(i) {
                Some(a) => a.enabled = on,
                None => g.set_error(INVALID_VALUE),
            }
            Ok(())
        });
    }
    for name in ["vertexAttribPointer", "vertexAttribIPointer"] {
        gl_op(reg, rt, name, move |cx, _w, gl| {
            let int = name == "vertexAttribIPointer";
            let i = cx.arg_u32(0)? as usize;
            let size = cx.arg_u32(1)?;
            let ty = cx.arg_u32(2)?;
            let (normalized, stride, offset) = if int {
                (false, cx.arg_i32(3)?, cx.arg_f64(4)?)
            } else {
                (cx.arg_bool(3), cx.arg_i32(4)?, cx.arg_f64(5)?)
            };
            let mut g = gl.borrow_mut();
            let valid_ty = match ty {
                0x1400..=0x1403 => true,
                0x1406 => !int,
                0x1404 | 0x1405 => g.webgl2,
                0x140B | 0x8D9F | 0x8368 => g.webgl2 && !int,
                _ => false,
            };
            if !valid_ty {
                g.set_error(INVALID_ENUM);
                return Ok(());
            }
            if i >= 16 || !(1..=4).contains(&size) || !(0..=255).contains(&stride) || offset < 0.0 {
                g.set_error(INVALID_VALUE);
                return Ok(());
            }
            if matches!(ty, 0x8D9F | 0x8368) && size != 4 {
                g.set_error(INVALID_OPERATION);
                return Ok(());
            }
            let tsz = match ty {
                0x1400 | 0x1401 => 1,
                0x1402 | 0x1403 | 0x140B => 2,
                _ => 4,
            };
            let off = offset as u32;
            if off % tsz != 0 || stride as u32 % tsz != 0 {
                g.set_error(INVALID_OPERATION);
                return Ok(());
            }
            let buffer = g.bound.get(&0x8894).copied();
            if buffer.is_none() && off != 0 {
                g.set_error(INVALID_OPERATION);
                return Ok(());
            }
            let a = &mut g.attribs[i];
            a.buffer = buffer;
            a.size = size;
            a.ty = ty;
            a.normalized = normalized;
            a.integer = int;
            a.stride = stride as u32;
            a.offset = off;
            Ok(())
        });
    }
    gl_op(reg, rt, "vertexAttribDivisor", |cx, _w, gl| {
        let i = cx.arg_u32(0)? as usize;
        let d = cx.arg_u32(1)?;
        let mut g = gl.borrow_mut();
        match g.attribs.get_mut(i) {
            Some(a) => a.divisor = d,
            None => g.set_error(INVALID_VALUE),
        }
        Ok(())
    });
    for (name, n, k) in [
        ("vertexAttrib1f", 1, UKind::F),
        ("vertexAttrib2f", 2, UKind::F),
        ("vertexAttrib3f", 3, UKind::F),
        ("vertexAttrib4f", 4, UKind::F),
        ("vertexAttrib1fv", 1, UKind::F),
        ("vertexAttrib2fv", 2, UKind::F),
        ("vertexAttrib3fv", 3, UKind::F),
        ("vertexAttrib4fv", 4, UKind::F),
        ("vertexAttribI4i", 4, UKind::I),
        ("vertexAttribI4iv", 4, UKind::I),
        ("vertexAttribI4ui", 4, UKind::U),
        ("vertexAttribI4uiv", 4, UKind::U),
    ] {
        gl_op(reg, rt, name, move |cx, _w, gl| {
            let i = cx.arg_u32(0)? as usize;
            let vals: Vec<f64> = if name.ends_with('v') {
                let v = arg_numbers(cx, 1).unwrap_or_default();
                if v.len() < n {
                    gl.borrow_mut().set_error(INVALID_VALUE);
                    return Ok(());
                }
                v
            } else {
                let mut v = Vec::new();
                for k in 1..=n {
                    v.push(cx.arg_f64(k)?);
                }
                v
            };
            let mut g = gl.borrow_mut();
            let Some(slot) = g.generic.get_mut(i) else {
                g.set_error(INVALID_VALUE);
                return Ok(());
            };
            let mut out = match k {
                UKind::F => [0, 0, 0, 1f32.to_bits()],
                _ => [0, 0, 0, 1],
            };
            for (j, v) in vals.iter().take(n).enumerate() {
                out[j] = match k {
                    UKind::F => (*v as f32).to_bits(),
                    UKind::I => (*v as i64) as i32 as u32,
                    UKind::U => (*v as i64) as u32,
                };
            }
            *slot = out;
            Ok(())
        });
    }
    gl_op(reg, rt, "getVertexAttrib", |cx, w, gl| {
        let i = cx.arg_u32(0)? as usize;
        let p = cx.arg_u32(1)?;
        let (a, gv) = {
            let g = gl.borrow();
            (g.attribs.get(i).copied(), g.generic.get(i).copied())
        };
        let (Some(a), Some(gv)) = (a, gv) else {
            gl.borrow_mut().set_error(INVALID_VALUE);
            return cx.ret(Arg::Null);
        };
        match p {
            0x8622 => cx.ret(a.enabled),
            0x8623 => cx.ret(if a.size == 0 { 4 } else { a.size }),
            0x8624 => cx.ret(a.stride),
            0x8625 => cx.ret(if a.ty == 0 { FLOAT } else { a.ty }),
            0x886A => cx.ret(a.normalized),
            0x88FD => cx.ret(a.integer),
            0x88FE => cx.ret(a.divisor),
            0x889F => cx.ret(opt_obj(w, a.buffer)),
            0x8626 => cx.ret(Arg::Float32Array(
                gv.iter().map(|c| f32::from_bits(*c)).collect(),
            )),
            _ => {
                gl.borrow_mut().set_error(INVALID_ENUM);
                cx.ret(Arg::Null)
            }
        }
    });
    gl_op(reg, rt, "getVertexAttribOffset", |cx, _w, gl| {
        let i = cx.arg_u32(0)? as usize;
        let v = gl.borrow().attribs.get(i).map_or(0, |a| a.offset);
        cx.ret(v)
    });
    gl_op(reg, rt, "bindVertexArray", |cx, w, gl| {
        let obj = arg_res(cx, w, 0, gl);
        gl.borrow_mut().bind_vao(obj);
        Ok(())
    });
    gl_op(reg, rt, "createVertexArray", |cx, w, gl| {
        if gl.borrow().lost {
            return cx.ret(Arg::Null);
        }
        let id = new_res(w, gl, ResKind::VertexArray, false);
        if let Some(r) = gl.borrow_mut().res.get_mut(&id) {
            r.vao = Some(Box::new(Vao {
                attribs: vec![Attrib::default(); 16],
                element: None,
            }));
        }
        cx.ret(w.obj_arg(id))
    });
    method(
        reg,
        rt,
        "OESVertexArrayObject",
        "createVertexArrayOES",
        |cx, w| {
            let gl = this_ext(cx, w)?;
            let id = new_res(w, &gl, ResKind::VertexArray, true);
            if let Some(r) = gl.borrow_mut().res.get_mut(&id) {
                r.vao = Some(Box::new(Vao {
                    attribs: vec![Attrib::default(); 16],
                    element: None,
                }));
            }
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
            gl.borrow_mut().bind_vao(obj);
            Ok(())
        },
    );
    method(
        reg,
        rt,
        "ANGLEInstancedArrays",
        "vertexAttribDivisorANGLE",
        |cx, w| {
            super::super::need_args(cx, 2, "vertexAttribDivisorANGLE", "ANGLEInstancedArrays")?;
            let gl = this_ext(cx, w)?;
            let i = cx.arg_u32(0)? as usize;
            let d = cx.arg_u32(1)?;
            let mut g = gl.borrow_mut();
            match g.attribs.get_mut(i) {
                Some(a) => a.divisor = d,
                None => g.set_error(INVALID_VALUE),
            }
            Ok(())
        },
    );
}

/// Reads the texture image arguments of `texImage2D`/`texSubImage2D`
/// starting at `i` (either width/height/border?/format/type/pixels or
/// format/type/source). Returns (width, height, format, type, texels).
fn tex_source(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    gl: &Rc<RefCell<Gl>>,
    i: usize,
    sized: bool,
    border: bool,
    op: &str,
    integer: bool,
) -> Result<Option<(u32, u32, u32, u32, Texels)>, HostError> {
    let iface = if gl.borrow().webgl2 {
        "WebGL2RenderingContext"
    } else {
        "WebGLRenderingContext"
    };
    if sized {
        let wd = cx.arg_i32(i)?;
        let ht = cx.arg_i32(i + 1)?;
        let j = if border { i + 3 } else { i + 2 };
        if border && cx.arg_i32(i + 2)? != 0 {
            gl.borrow_mut().set_error(INVALID_VALUE);
            return Ok(None);
        }
        let format = cx.arg_u32(j)?;
        let ty = cx.arg_u32(j + 1)?;
        if wd < 0 || ht < 0 {
            gl.borrow_mut().set_error(INVALID_VALUE);
            return Ok(None);
        }
        let up = gl.borrow().unpack();
        let k = j + 2;
        let kind = cx.arg_kind(k);
        // A DOM source in the long form (WebGL 2).
        if kind == ValueKind::Object && cx.arg_view_kind(k).is_none() && cx.arg_host(k).is_some() {
            let src = super::super::canvas::image_source(cx, w, k, op, iface)?;
            let super::super::canvas::Source::Pixels(px, _) = src else {
                return Ok(None);
            };
            let (sw, sh, t) = dom_texels(&px, &up);
            let _ = (sw, sh);
            return Ok(Some((wd as u32, ht as u32, format, ty, t)));
        }
        let bytes = if matches!(kind, ValueKind::Null | ValueKind::Undefined) {
            let comps = format_comps(format).unwrap_or(4);
            let bpp = type_bytes(ty, comps).unwrap_or(4);
            let row = (wd as usize * bpp).div_ceil(up.align) * up.align;
            vec![0u8; row * ht as usize + wd as usize * bpp]
        } else if cx.arg_view_kind(k).is_some() {
            let mut b = cx.arg_bytes(k).unwrap_or_default();
            if cx.len() > k + 1 {
                let el = match cx.arg_view_kind(k) {
                    Some(ViewKind::Int16 | ViewKind::Uint16 | ViewKind::Float16) => 2,
                    Some(ViewKind::Int32 | ViewKind::Uint32 | ViewKind::Float32) => 4,
                    Some(ViewKind::Float64 | ViewKind::BigInt64 | ViewKind::BigUint64) => 8,
                    _ => 1,
                };
                let off = cx.arg_f64(k + 1)?.max(0.0) as usize * el;
                if off > b.len() {
                    gl.borrow_mut().set_error(INVALID_VALUE);
                    return Ok(None);
                }
                b.drain(..off);
            }
            b
        } else if kind == ValueKind::Number && gl.borrow().webgl2 {
            // PIXEL_UNPACK_BUFFER offset.
            let off = cx.arg_f64(k)?.max(0.0) as usize;
            let g = gl.borrow();
            match g.bound.get(&0x88EF).and_then(|b| g.res.get(b)) {
                Some(r) => r.bytes.get(off..).unwrap_or_default().to_vec(),
                None => {
                    drop(g);
                    gl.borrow_mut().set_error(INVALID_OPERATION);
                    return Ok(None);
                }
            }
        } else {
            return Err(HostError::type_error(format!(
                "Failed to execute '{op}' on '{iface}': parameter {} is not of type 'ArrayBufferView'.",
                k + 1
            )));
        };
        let null = matches!(kind, ValueKind::Null | ValueKind::Undefined);
        let up = if null {
            Unpack {
                flip: false,
                premultiply: false,
                ..up
            }
        } else {
            up
        };
        let t = match decode_texels(
            &bytes,
            wd as usize,
            ht as usize,
            1,
            format,
            ty,
            &up,
            integer,
        ) {
            Ok(t) => t,
            Err(e) => {
                gl.borrow_mut().set_error(e);
                return Ok(None);
            }
        };
        return Ok(Some((wd as u32, ht as u32, format, ty, t)));
    }
    let format = cx.arg_u32(i)?;
    let ty = cx.arg_u32(i + 1)?;
    let up = gl.borrow().unpack();
    // ImageData or a canvas image source.
    if let Ok(b) = super::super::canvas::arg_image_data(cx, w, i + 2, op, iface) {
        let mut pm = lb_paint::tiny_skia::Pixmap::new(b.width.max(1), b.height.max(1));
        if let Some(p) = pm.as_mut() {
            let mut d = b.data.clone();
            lb_paint::decode::premultiply(&mut d);
            if d.len() == p.data().len() {
                p.data_mut().copy_from_slice(&d);
            }
        }
        let Some(pm) = pm else { return Ok(None) };
        // ImageData is unpremultiplied: keep exact values unless asked to
        // premultiply.
        let mut texels = Vec::with_capacity(b.data.len() / 4);
        for y in 0..b.height as usize {
            let sy = if up.flip {
                b.height as usize - 1 - y
            } else {
                y
            };
            for x in 0..b.width as usize {
                let o = (sy * b.width as usize + x) * 4;
                let mut c = [0f32; 4];
                for k in 0..4 {
                    c[k] = f32::from(b.data[o + k]) / 255.0;
                }
                if up.premultiply {
                    for k in 0..3 {
                        c[k] *= c[3];
                    }
                }
                texels.push(c);
            }
        }
        let _ = pm;
        return Ok(Some((b.width, b.height, format, ty, Texels::F(texels))));
    }
    let src = super::super::canvas::image_source(cx, w, i + 2, op, iface)?;
    let super::super::canvas::Source::Pixels(px, _) = src else {
        return Ok(None);
    };
    let (sw, sh, t) = dom_texels(&px, &up);
    Ok(Some((sw, sh, format, ty, t)))
}

fn register_textures(reg: &mut Registry, rt: &Runtime) {
    gl_op(reg, rt, "createTexture", |cx, w, gl| {
        if gl.borrow().lost {
            return cx.ret(Arg::Null);
        }
        let id = new_res(w, gl, ResKind::Texture, false);
        if let Some(r) = gl.borrow_mut().res.get_mut(&id) {
            r.tex = Some(Box::new(Texture::new()));
        }
        cx.ret(w.obj_arg(id))
    });
    gl_op(reg, rt, "bindTexture", |cx, w, gl| {
        let target = cx.arg_u32(0)?;
        let obj = arg_res(cx, w, 1, gl);
        let mut g = gl.borrow_mut();
        let Some(slot) = tslot(target).filter(|s| g.webgl2 || *s < 2) else {
            g.set_error(INVALID_ENUM);
            return Ok(());
        };
        if let Some(id) = obj {
            let Some(t) = g.tex_mut(id) else {
                return Ok(());
            };
            if t.target != 0 && t.target != target {
                g.set_error(INVALID_OPERATION);
                return Ok(());
            }
            t.target = target;
            if target == gtex::TEXTURE_CUBE_MAP && t.faces.len() < 6 {
                t.faces.resize_with(6, Vec::new);
            }
        }
        let u = g.unit();
        g.units[u].tex[slot] = obj;
        match obj {
            Some(id) => g.bound.insert(SLOT_PNAMES[slot], id),
            None => g.bound.remove(&SLOT_PNAMES[slot]),
        };
        Ok(())
    });
    gl_op(reg, rt, "activeTexture", |cx, _w, gl| {
        let t = cx.arg_u32(0)?;
        let mut g = gl.borrow_mut();
        if !(TEXTURE0..TEXTURE0 + 32).contains(&t) {
            g.set_error(INVALID_ENUM);
            return Ok(());
        }
        g.active_texture = t;
        let u = g.unit();
        for (slot, p) in SLOT_PNAMES.iter().enumerate() {
            match g.units[u].tex[slot] {
                Some(id) => g.bound.insert(*p, id),
                None => g.bound.remove(p),
            };
        }
        match g.units[u].sampler {
            Some(id) => g.bound.insert(0x8919, id),
            None => g.bound.remove(&0x8919),
        };
        Ok(())
    });
    gl_op(reg, rt, "deleteTexture", |cx, w, gl| {
        if let Some(id) = arg_res(cx, w, 0, gl) {
            let mut g = gl.borrow_mut();
            if let Some(r) = g.res.get_mut(&id) {
                r.deleted = true;
            }
            g.bound.retain(|_, v| *v != id);
            for u in &mut g.units {
                for t in &mut u.tex {
                    if *t == Some(id) {
                        *t = None;
                    }
                }
            }
        }
        Ok(())
    });
    gl_op(reg, rt, "texImage2D", |cx, w, gl| {
        let target = cx.arg_u32(0)?;
        let level = cx.arg_i32(1)?;
        let internal = cx.arg_u32(2)?;
        let sized = cx.len() >= 9;
        let integer =
            lb_gl::image::sized(internal).is_some_and(|f| matches!(f.kind, Kind::Int | Kind::Uint));
        let Some((wd, ht, format, ty, texels)) =
            tex_source(cx, w, gl, 3, sized, true, "texImage2D", integer)?
        else {
            return Ok(());
        };
        let mut g = gl.borrow_mut();
        let (id, slot, face) = match g.tex_for(target) {
            Ok(v) => v,
            Err(e) => {
                g.set_error(e);
                return Ok(());
            }
        };
        if level < 0 || slot == 2 || slot == 3 {
            g.set_error(INVALID_VALUE);
            return Ok(());
        }
        let fmt = match g.tex_format(internal, format, ty) {
            Ok(f) => f,
            Err(e) => {
                g.set_error(e);
                return Ok(());
            }
        };
        if slot == 1 && wd != ht {
            g.set_error(INVALID_VALUE);
            return Ok(());
        }
        let Some(t) = g.tex_mut(id) else {
            return Ok(());
        };
        if t.immutable_levels > 0 {
            g.set_error(INVALID_OPERATION);
            return Ok(());
        }
        let mut img = Image::new(fmt, wd, ht, 1, 1);
        write_texels(&mut img, &texels, 0, 0, 0, wd, ht, 1);
        t.set_level(face, level as u32, Some(img));
        Ok(())
    });
    gl_op(reg, rt, "texSubImage2D", |cx, w, gl| {
        let target = cx.arg_u32(0)?;
        let level = cx.arg_i32(1)?;
        let xo = cx.arg_i32(2)?;
        let yo = cx.arg_i32(3)?;
        let sized = cx.len() >= 9;
        let integer = {
            let g = gl.borrow();
            image_target(target)
                .and_then(|(s, f)| {
                    g.tex_bound(s)
                        .and_then(|id| g.res.get(&id))
                        .and_then(|r| r.tex.as_ref())
                        .and_then(|t| t.level(f, level.max(0) as u32))
                })
                .is_some_and(|i| matches!(i.format.kind, Kind::Int | Kind::Uint))
        };
        let Some((wd, ht, _format, _ty, texels)) =
            tex_source(cx, w, gl, 4, sized, false, "texSubImage2D", integer)?
        else {
            return Ok(());
        };
        let mut g = gl.borrow_mut();
        let (id, _slot, face) = match g.tex_for(target) {
            Ok(v) => v,
            Err(e) => {
                g.set_error(e);
                return Ok(());
            }
        };
        let Some(img) = g
            .tex_mut(id)
            .and_then(|t| t.level_mut(face, level.max(0) as u32))
        else {
            g.set_error(INVALID_OPERATION);
            return Ok(());
        };
        if xo < 0 || yo < 0 || xo as u32 + wd > img.width || yo as u32 + ht > img.height {
            g.set_error(INVALID_VALUE);
            return Ok(());
        }
        write_texels(img, &texels, xo as u32, yo as u32, 0, wd, ht, 1);
        Ok(())
    });
    for (name, three) in [("texStorage2D", false), ("texStorage3D", true)] {
        gl_op(reg, rt, name, move |cx, _w, gl| {
            let target = cx.arg_u32(0)?;
            let levels = cx.arg_i32(1)?;
            let internal = cx.arg_u32(2)?;
            let wd = cx.arg_i32(3)?;
            let ht = cx.arg_i32(4)?;
            let dp = if three { cx.arg_i32(5)? } else { 1 };
            let mut g = gl.borrow_mut();
            let Some(slot) = tslot(target) else {
                g.set_error(INVALID_ENUM);
                return Ok(());
            };
            let Some(id) = g.tex_bound(slot) else {
                g.set_error(INVALID_OPERATION);
                return Ok(());
            };
            let Some(fmt) = gimg::sized(internal) else {
                g.set_error(INVALID_ENUM);
                return Ok(());
            };
            if levels < 1 || wd < 1 || ht < 1 || dp < 1 {
                g.set_error(INVALID_VALUE);
                return Ok(());
            }
            let Some(t) = g.tex_mut(id) else {
                return Ok(());
            };
            if t.immutable_levels > 0 {
                g.set_error(INVALID_OPERATION);
                return Ok(());
            }
            let faces = if slot == 1 { 6 } else { 1 };
            let (mut w0, mut h0, mut d0) = (wd as u32, ht as u32, dp as u32);
            for l in 0..levels as u32 {
                for f in 0..faces {
                    t.set_level(f, l, Some(Image::new(fmt, w0, h0, d0, 1)));
                }
                w0 = (w0 / 2).max(1);
                h0 = (h0 / 2).max(1);
                if slot == 2 {
                    d0 = (d0 / 2).max(1);
                }
            }
            t.immutable_levels = levels as u32;
            Ok(())
        });
    }
    for (name, sub) in [("texImage3D", false), ("texSubImage3D", true)] {
        gl_op(reg, rt, name, move |cx, _w, gl| {
            let target = cx.arg_u32(0)?;
            let level = cx.arg_i32(1)?.max(0) as u32;
            let (internal, x0, y0, z0, base) = if sub {
                (0, cx.arg_i32(2)?, cx.arg_i32(3)?, cx.arg_i32(4)?, 5)
            } else {
                (cx.arg_u32(2)?, 0, 0, 0, 3)
            };
            let wd = cx.arg_i32(base)?;
            let ht = cx.arg_i32(base + 1)?;
            let dp = cx.arg_i32(base + 2)?;
            let fi = if sub { base + 3 } else { base + 4 };
            let format = cx.arg_u32(fi)?;
            let ty = cx.arg_u32(fi + 1)?;
            let up = gl.borrow().unpack();
            let k = fi + 2;
            let integer = gimg::sized(internal)
                .is_some_and(|f| matches!(f.kind, Kind::Int | Kind::Uint))
                || matches!(format, 0x8D94 | 0x8228 | 0x8D98 | 0x8D99);
            let bytes = if matches!(cx.arg_kind(k), ValueKind::Null | ValueKind::Undefined) {
                let comps = format_comps(format).unwrap_or(4);
                let bpp = type_bytes(ty, comps).unwrap_or(4);
                vec![
                    0u8;
                    (wd.max(0) as usize * bpp + up.align) * ht.max(0) as usize * dp.max(0) as usize
                ]
            } else {
                match cx.arg_bytes(k) {
                    Some(b) => b,
                    None => {
                        gl.borrow_mut().set_error(INVALID_OPERATION);
                        return Ok(());
                    }
                }
            };
            let mut g = gl.borrow_mut();
            if wd < 0 || ht < 0 || dp < 0 {
                g.set_error(INVALID_VALUE);
                return Ok(());
            }
            let texels = match decode_texels(
                &bytes,
                wd as usize,
                ht as usize,
                dp as usize,
                format,
                ty,
                &up,
                integer,
            ) {
                Ok(t) => t,
                Err(e) => {
                    g.set_error(e);
                    return Ok(());
                }
            };
            let Some(slot) = tslot(target).filter(|s| *s >= 2) else {
                g.set_error(INVALID_ENUM);
                return Ok(());
            };
            let Some(id) = g.tex_bound(slot) else {
                g.set_error(INVALID_OPERATION);
                return Ok(());
            };
            if sub {
                let Some(img) = g.tex_mut(id).and_then(|t| t.level_mut(0, level)) else {
                    g.set_error(INVALID_OPERATION);
                    return Ok(());
                };
                write_texels(
                    img,
                    &texels,
                    x0.max(0) as u32,
                    y0.max(0) as u32,
                    z0.max(0) as u32,
                    wd as u32,
                    ht as u32,
                    dp as u32,
                );
                return Ok(());
            }
            let fmt = match g.tex_format(internal, format, ty) {
                Ok(f) => f,
                Err(e) => {
                    g.set_error(e);
                    return Ok(());
                }
            };
            let Some(t) = g.tex_mut(id) else {
                return Ok(());
            };
            let mut img = Image::new(fmt, wd as u32, ht as u32, dp as u32, 1);
            write_texels(&mut img, &texels, 0, 0, 0, wd as u32, ht as u32, dp as u32);
            t.set_level(0, level, Some(img));
            Ok(())
        });
    }
    gl_op(reg, rt, "compressedTexImage2D", |cx, _w, gl| {
        // Compressed data is not decoded: the level reads as black.
        let target = cx.arg_u32(0)?;
        let level = cx.arg_i32(1)?.max(0) as u32;
        let wd = cx.arg_i32(3)?.max(0) as u32;
        let ht = cx.arg_i32(4)?.max(0) as u32;
        let mut g = gl.borrow_mut();
        let (id, _, face) = match g.tex_for(target) {
            Ok(v) => v,
            Err(e) => {
                g.set_error(e);
                return Ok(());
            }
        };
        if let Some(t) = g.tex_mut(id) {
            let mut img = Image::new(gimg::RGBA8, wd, ht, 1, 1);
            img.clear_color([0.0, 0.0, 0.0, 1.0], [true; 4], None);
            t.set_level(face, level, Some(img));
        }
        Ok(())
    });
    for name in ["copyTexImage2D", "copyTexSubImage2D"] {
        gl_op(reg, rt, name, move |cx, _w, gl| {
            let sub = name == "copyTexSubImage2D";
            let target = cx.arg_u32(0)?;
            let level = cx.arg_i32(1)?.max(0) as u32;
            let (internal, xo, yo, x, y, wd, ht) = if sub {
                (
                    0,
                    cx.arg_i32(2)?,
                    cx.arg_i32(3)?,
                    cx.arg_i32(4)?,
                    cx.arg_i32(5)?,
                    cx.arg_i32(6)?,
                    cx.arg_i32(7)?,
                )
            } else {
                (
                    cx.arg_u32(2)?,
                    0,
                    0,
                    cx.arg_i32(3)?,
                    cx.arg_i32(4)?,
                    cx.arg_i32(5)?,
                    cx.arg_i32(6)?,
                )
            };
            if wd < 0 || ht < 0 {
                gl.borrow_mut().set_error(INVALID_VALUE);
                return Ok(());
            }
            let mut g = gl.borrow_mut();
            let (id, _, face) = match g.tex_for(target) {
                Ok(v) => v,
                Err(e) => {
                    g.set_error(e);
                    return Ok(());
                }
            };
            // Source pixels.
            let b = match g.take_bufs(true) {
                Ok(b) => b,
                Err(e) => {
                    g.set_error(e);
                    return Ok(());
                }
            };
            let src = b
                .colors
                .first()
                .and_then(|c| c.as_ref())
                .map(|c| c.0.resolved());
            let (fw, fh) = (b.width as i32, b.height as i32);
            g.put_bufs(b);
            let Some(src) = src else {
                g.set_error(INVALID_OPERATION);
                return Ok(());
            };
            let fmt = if sub {
                match g
                    .tex_mut(id)
                    .and_then(|t| t.level(face, level).map(|i| i.format))
                {
                    Some(f) => f,
                    None => {
                        g.set_error(INVALID_OPERATION);
                        return Ok(());
                    }
                }
            } else {
                match gimg::unsized_format(internal, UNSIGNED_BYTE)
                    .or_else(|| gimg::sized(internal))
                {
                    Some(f) => f,
                    None => {
                        g.set_error(INVALID_ENUM);
                        return Ok(());
                    }
                }
            };
            let Some(t) = g.tex_mut(id) else {
                return Ok(());
            };
            if !sub {
                t.set_level(
                    face,
                    level,
                    Some(Image::new(fmt, wd as u32, ht as u32, 1, 1)),
                );
            }
            let Some(dst) = t.level_mut(face, level) else {
                return Ok(());
            };
            for r in 0..ht {
                for c in 0..wd {
                    let (sx, sy) = (x + c, y + r);
                    let (dx, dy) = ((xo + c) as u32, (yo + r) as u32);
                    if dx >= dst.width || dy >= dst.height {
                        continue;
                    }
                    let v = if sx >= 0 && sy >= 0 && sx < fw && sy < fh {
                        src.read(src.index(sx as u32, sy as u32, 0))
                    } else {
                        [0.0; 4]
                    };
                    // Luminance/alpha destinations take the matching
                    // source channels.
                    let v = match dst.format.swizzle {
                        gimg::Swizzle::Alpha => [v[3], 0.0, 0.0, 1.0],
                        gimg::Swizzle::LuminanceAlpha => [v[0], v[3], 0.0, 1.0],
                        _ => v,
                    };
                    let i = dst.index(dx, dy, 0);
                    dst.write(i, v, [true; 4]);
                }
            }
            Ok(())
        });
    }
    for name in ["texParameteri", "texParameterf"] {
        gl_op(reg, rt, name, move |cx, _w, gl| {
            let target = cx.arg_u32(0)?;
            let p = cx.arg_u32(1)?;
            let v = cx.arg_f64(2)?;
            let mut g = gl.borrow_mut();
            let Some(slot) = tslot(target).filter(|s| g.webgl2 || *s < 2) else {
                g.set_error(INVALID_ENUM);
                return Ok(());
            };
            let Some(id) = g.tex_bound(slot) else {
                g.set_error(INVALID_OPERATION);
                return Ok(());
            };
            let webgl2 = g.webgl2;
            let aniso = g.ext_enabled("EXT_texture_filter_anisotropic");
            let Some(t) = g.tex_mut(id) else {
                return Ok(());
            };
            let iv = v as u32;
            let ok = match p {
                0x2801 if matches!(iv, 0x2600 | 0x2601 | 0x2700..=0x2703) => {
                    t.params.min_filter = iv;
                    true
                }
                0x2800 if matches!(iv, 0x2600 | 0x2601) => {
                    t.params.mag_filter = iv;
                    true
                }
                0x2802 | 0x2803 | 0x8072
                    if matches!(
                        iv,
                        gtex::REPEAT | gtex::CLAMP_TO_EDGE | gtex::MIRRORED_REPEAT
                    ) && (p != 0x8072 || webgl2) =>
                {
                    t.params.wrap[match p {
                        0x2802 => 0,
                        0x2803 => 1,
                        _ => 2,
                    }] = iv;
                    true
                }
                0x813A if webgl2 => {
                    t.params.min_lod = v as f32;
                    true
                }
                0x813B if webgl2 => {
                    t.params.max_lod = v as f32;
                    true
                }
                0x813C if webgl2 => {
                    t.base_level = v.max(0.0) as u32;
                    true
                }
                0x813D if webgl2 => {
                    t.max_level = v.max(0.0) as u32;
                    true
                }
                0x884C if webgl2 => {
                    t.params.compare_mode = iv;
                    true
                }
                0x884D if webgl2 => {
                    t.params.compare_func = iv;
                    true
                }
                MAX_TEXTURE_MAX_ANISOTROPY_EXT if aniso => true,
                _ => false,
            };
            if !ok {
                g.set_error(INVALID_ENUM);
            }
            Ok(())
        });
    }
    gl_op(reg, rt, "getTexParameter", |cx, _w, gl| {
        let target = cx.arg_u32(0)?;
        let p = cx.arg_u32(1)?;
        let g = gl.borrow();
        let Some(slot) = tslot(target) else {
            drop(g);
            gl.borrow_mut().set_error(INVALID_ENUM);
            return cx.ret(Arg::Null);
        };
        let Some(t) = g
            .tex_bound(slot)
            .and_then(|id| g.res.get(&id))
            .and_then(|r| r.tex.as_deref())
        else {
            drop(g);
            gl.borrow_mut().set_error(INVALID_OPERATION);
            return cx.ret(Arg::Null);
        };
        let v = match p {
            0x2801 => Arg::Number(f64::from(t.params.min_filter)),
            0x2800 => Arg::Number(f64::from(t.params.mag_filter)),
            0x2802 => Arg::Number(f64::from(t.params.wrap[0])),
            0x2803 => Arg::Number(f64::from(t.params.wrap[1])),
            0x8072 if g.webgl2 => Arg::Number(f64::from(t.params.wrap[2])),
            0x813A if g.webgl2 => Arg::Number(f64::from(t.params.min_lod)),
            0x813B if g.webgl2 => Arg::Number(f64::from(t.params.max_lod)),
            0x813C if g.webgl2 => Arg::Number(f64::from(t.base_level)),
            0x813D if g.webgl2 => Arg::Number(f64::from(t.max_level)),
            0x884C if g.webgl2 => Arg::Number(f64::from(t.params.compare_mode)),
            0x884D if g.webgl2 => Arg::Number(f64::from(t.params.compare_func)),
            0x912F if g.webgl2 => Arg::Bool(t.immutable_levels > 0),
            0x82DF if g.webgl2 => Arg::Number(f64::from(t.immutable_levels)),
            MAX_TEXTURE_MAX_ANISOTROPY_EXT if g.ext_enabled("EXT_texture_filter_anisotropic") => {
                Arg::Number(1.0)
            }
            _ => {
                drop(g);
                gl.borrow_mut().set_error(INVALID_ENUM);
                return cx.ret(Arg::Null);
            }
        };
        drop(g);
        cx.ret(v)
    });
    gl_op(reg, rt, "generateMipmap", |cx, _w, gl| {
        let target = cx.arg_u32(0)?;
        let mut g = gl.borrow_mut();
        let Some(slot) = tslot(target) else {
            g.set_error(INVALID_ENUM);
            return Ok(());
        };
        let webgl2 = g.webgl2;
        let Some(id) = g.tex_bound(slot) else {
            g.set_error(INVALID_OPERATION);
            return Ok(());
        };
        let Some(t) = g.tex_mut(id) else {
            return Ok(());
        };
        let ok = t
            .level(0, t.base_level)
            .is_some_and(|b| webgl2 || (b.width.is_power_of_two() && b.height.is_power_of_two()));
        if !ok {
            g.set_error(INVALID_OPERATION);
            return Ok(());
        }
        t.generate_mipmap();
        Ok(())
    });
    // Samplers (WebGL 2).
    gl_op(reg, rt, "createSampler", |cx, w, gl| {
        let id = new_res(w, gl, ResKind::Sampler, false);
        if let Some(r) = gl.borrow_mut().res.get_mut(&id) {
            r.sampler = Some(SamplerParams::default());
        }
        cx.ret(w.obj_arg(id))
    });
    gl_op(reg, rt, "bindSampler", |cx, w, gl| {
        let unit = cx.arg_u32(0)? as usize;
        let obj = arg_res(cx, w, 1, gl);
        let mut g = gl.borrow_mut();
        if unit >= g.units.len() {
            g.set_error(INVALID_VALUE);
            return Ok(());
        }
        g.units[unit].sampler = obj;
        if unit == g.unit() {
            match obj {
                Some(id) => g.bound.insert(0x8919, id),
                None => g.bound.remove(&0x8919),
            };
        }
        Ok(())
    });
    for name in ["samplerParameteri", "samplerParameterf"] {
        gl_op(reg, rt, name, |cx, w, gl| {
            let p = cx.arg_u32(1)?;
            let v = cx.arg_f64(2)?;
            let Some(id) = arg_res(cx, w, 0, gl) else {
                return Ok(());
            };
            let mut g = gl.borrow_mut();
            let Some(s) = g.res.get_mut(&id).and_then(|r| r.sampler.as_mut()) else {
                return Ok(());
            };
            let iv = v as u32;
            match p {
                0x2801 => s.min_filter = iv,
                0x2800 => s.mag_filter = iv,
                0x2802 => s.wrap[0] = iv,
                0x2803 => s.wrap[1] = iv,
                0x8072 => s.wrap[2] = iv,
                0x813A => s.min_lod = v as f32,
                0x813B => s.max_lod = v as f32,
                0x884C => s.compare_mode = iv,
                0x884D => s.compare_func = iv,
                _ => g.set_error(INVALID_ENUM),
            }
            Ok(())
        });
    }
    gl_op(reg, rt, "getSamplerParameter", |cx, w, gl| {
        let p = cx.arg_u32(1)?;
        let Some(id) = arg_res(cx, w, 0, gl) else {
            return cx.ret(Arg::Null);
        };
        let s = gl.borrow().res.get(&id).and_then(|r| r.sampler);
        let Some(s) = s else { return cx.ret(Arg::Null) };
        let v = match p {
            0x2801 => f64::from(s.min_filter),
            0x2800 => f64::from(s.mag_filter),
            0x2802 => f64::from(s.wrap[0]),
            0x2803 => f64::from(s.wrap[1]),
            0x8072 => f64::from(s.wrap[2]),
            0x813A => f64::from(s.min_lod),
            0x813B => f64::from(s.max_lod),
            0x884C => f64::from(s.compare_mode),
            0x884D => f64::from(s.compare_func),
            _ => {
                gl.borrow_mut().set_error(INVALID_ENUM);
                return cx.ret(Arg::Null);
            }
        };
        cx.ret(v)
    });
}

fn att_point(g: &Gl, a: u32) -> Option<u32> {
    let max = if g.webgl2 || g.ext_enabled("WEBGL_draw_buffers") {
        8
    } else {
        1
    };
    match a {
        DEPTH_ATTACHMENT | STENCIL_ATTACHMENT | DEPTH_STENCIL_ATTACHMENT => Some(a),
        _ if (COLOR_ATTACHMENT0..COLOR_ATTACHMENT0 + max).contains(&a) => Some(a),
        _ => None,
    }
}

fn set_att(f: &mut Fbo, point: u32, att: Option<Att>) {
    match point {
        DEPTH_ATTACHMENT => f.depth = att,
        STENCIL_ATTACHMENT => f.stencil = att,
        DEPTH_STENCIL_ATTACHMENT => {
            f.depth = att;
            f.stencil = att;
        }
        _ => f.color[(point - COLOR_ATTACHMENT0) as usize] = att,
    }
}

fn register_framebuffers(reg: &mut Registry, rt: &Runtime) {
    gl_op(reg, rt, "createFramebuffer", |cx, w, gl| {
        if gl.borrow().lost {
            return cx.ret(Arg::Null);
        }
        let id = new_res(w, gl, ResKind::Framebuffer, false);
        if let Some(r) = gl.borrow_mut().res.get_mut(&id) {
            r.fbo = Some(Box::new(Fbo {
                read: COLOR_ATTACHMENT0,
                ..Default::default()
            }));
        }
        cx.ret(w.obj_arg(id))
    });
    gl_op(reg, rt, "framebufferTexture2D", |cx, w, gl| {
        let target = cx.arg_u32(0)?;
        let point = cx.arg_u32(1)?;
        let textarget = cx.arg_u32(2)?;
        let tex = arg_res(cx, w, 3, gl);
        let level = cx.arg_i32(4)?;
        let mut g = gl.borrow_mut();
        let fbp = match target {
            0x8D40 | 0x8CA9 => 0x8CA6,
            0x8CA8 if g.webgl2 => 0x8CAA,
            _ => {
                g.set_error(INVALID_ENUM);
                return Ok(());
            }
        };
        let Some(point) = att_point(&g, point) else {
            g.set_error(INVALID_ENUM);
            return Ok(());
        };
        let Some((slot, face)) = image_target(textarget).filter(|(s, _)| *s < 2) else {
            g.set_error(INVALID_ENUM);
            return Ok(());
        };
        let Some(fb) = g.bound.get(&fbp).copied() else {
            g.set_error(INVALID_OPERATION);
            return Ok(());
        };
        if let Some(t) = tex {
            let tt = g
                .res
                .get(&t)
                .and_then(|r| r.tex.as_ref())
                .map(|t| t.target)
                .unwrap_or(0);
            if tt != SLOT_TARGETS[slot] {
                g.set_error(INVALID_OPERATION);
                return Ok(());
            }
            if level != 0 && !g.webgl2 {
                g.set_error(INVALID_VALUE);
                return Ok(());
            }
        }
        let att = tex.map(|obj| Att {
            obj,
            kind: AttKind::Tex {
                level: level.max(0) as u32,
                face: face as u32,
                layer: 0,
            },
        });
        if let Some(f) = g.res.get_mut(&fb).and_then(|r| r.fbo.as_deref_mut()) {
            set_att(f, point, att);
        }
        Ok(())
    });
    gl_op(reg, rt, "framebufferTextureLayer", |cx, w, gl| {
        let point = cx.arg_u32(1)?;
        let tex = arg_res(cx, w, 2, gl);
        let level = cx.arg_i32(3)?.max(0) as u32;
        let layer = cx.arg_i32(4)?.max(0) as u32;
        let mut g = gl.borrow_mut();
        let Some(point) = att_point(&g, point) else {
            g.set_error(INVALID_ENUM);
            return Ok(());
        };
        let Some(fb) = g.bound.get(&0x8CA6).copied() else {
            g.set_error(INVALID_OPERATION);
            return Ok(());
        };
        let att = tex.map(|obj| Att {
            obj,
            kind: AttKind::Tex {
                level,
                face: 0,
                layer,
            },
        });
        if let Some(f) = g.res.get_mut(&fb).and_then(|r| r.fbo.as_deref_mut()) {
            set_att(f, point, att);
        }
        Ok(())
    });
    gl_op(reg, rt, "framebufferRenderbuffer", |cx, w, gl| {
        let target = cx.arg_u32(0)?;
        let point = cx.arg_u32(1)?;
        let rb = arg_res(cx, w, 3, gl);
        let mut g = gl.borrow_mut();
        let fbp = if target == 0x8CA8 && g.webgl2 {
            0x8CAA
        } else {
            0x8CA6
        };
        let Some(point) = att_point(&g, point) else {
            g.set_error(INVALID_ENUM);
            return Ok(());
        };
        let Some(fb) = g.bound.get(&fbp).copied() else {
            g.set_error(INVALID_OPERATION);
            return Ok(());
        };
        let att = rb.map(|obj| Att {
            obj,
            kind: AttKind::Rb,
        });
        // WebGL 1 DEPTH_STENCIL renderbuffers attach to both points.
        let point = match (point, rb.and_then(|r| g.res.get(&r)).map(|r| r.rb_format)) {
            (DEPTH_STENCIL_ATTACHMENT, _) => DEPTH_STENCIL_ATTACHMENT,
            (p, _) => p,
        };
        if let Some(f) = g.res.get_mut(&fb).and_then(|r| r.fbo.as_deref_mut()) {
            set_att(f, point, att);
        }
        Ok(())
    });
    for name in ["renderbufferStorage", "renderbufferStorageMultisample"] {
        gl_op(reg, rt, name, move |cx, _w, gl| {
            let ms = name == "renderbufferStorageMultisample";
            let (samples, fmt, wd, ht) = if ms {
                (
                    cx.arg_i32(1)?,
                    cx.arg_u32(2)?,
                    cx.arg_i32(3)?,
                    cx.arg_i32(4)?,
                )
            } else {
                (0, cx.arg_u32(1)?, cx.arg_i32(2)?, cx.arg_i32(3)?)
            };
            let mut g = gl.borrow_mut();
            let Some(id) = g.bound.get(&0x8CA7).copied() else {
                g.set_error(INVALID_OPERATION);
                return Ok(());
            };
            let f = if fmt == 0x84F9 {
                gimg::sized(0x88F0)
            } else {
                gimg::sized(fmt)
            };
            let f = f.or_else(|| (fmt == 0x8D62).then(|| gimg::sized(0x8D62)).flatten());
            let Some(f) = f else {
                g.set_error(INVALID_ENUM);
                return Ok(());
            };
            if wd < 0 || ht < 0 || wd > 16384 || ht > 16384 || samples < 0 || samples > 4 {
                g.set_error(INVALID_VALUE);
                return Ok(());
            }
            let s = if samples > 0 { 4 } else { 1 };
            if let Some(r) = g.res.get_mut(&id) {
                let mut img = Image::new(f, wd as u32, ht as u32, 1, s);
                img.clear_depth(1.0, None);
                r.rb = Some(img);
                r.rb_format = fmt;
                r.rb_samples = if samples > 0 { 4 } else { 0 };
            }
            Ok(())
        });
    }
    gl_op(reg, rt, "getRenderbufferParameter", |cx, _w, gl| {
        let p = cx.arg_u32(1)?;
        let g = gl.borrow();
        let Some(r) = g.bound.get(&0x8CA7).and_then(|id| g.res.get(id)) else {
            drop(g);
            gl.borrow_mut().set_error(INVALID_OPERATION);
            return cx.ret(Arg::Null);
        };
        let img = r.rb.as_ref();
        let bits = |k: usize| {
            img.map_or(0, |i| {
                if (k as u8) < i.format.channels && i.format.kind != Kind::Depth {
                    u32::from(i.format.bits[k])
                } else {
                    0
                }
            })
        };
        let v = match p {
            0x8D42 => img.map_or(0, |i| i.width),
            0x8D43 => img.map_or(0, |i| i.height),
            0x8D44 => {
                if r.rb_format == 0 {
                    0x8056
                } else {
                    r.rb_format
                }
            }
            0x8D50 => bits(0),
            0x8D51 => bits(1),
            0x8D52 => bits(2),
            0x8D53 => bits(3),
            0x8D54 => img.map_or(0, |i| u32::from(i.format.depth_bits)),
            0x8D55 => img.map_or(0, |i| {
                if matches!(i.format.kind, Kind::Stencil | Kind::DepthStencil) {
                    8
                } else {
                    0
                }
            }),
            0x8CAB => r.rb_samples,
            _ => {
                drop(g);
                gl.borrow_mut().set_error(INVALID_ENUM);
                return cx.ret(Arg::Null);
            }
        };
        drop(g);
        cx.ret(v)
    });
    gl_op(reg, rt, "checkFramebufferStatus", |cx, _w, gl| {
        let target = cx.arg_u32(0)?;
        let g = gl.borrow();
        let p = if target == 0x8CA8 { 0x8CAA } else { 0x8CA6 };
        let v = match g.bound.get(&p) {
            Some(id) => g.fbo_status(*id),
            None => FRAMEBUFFER_COMPLETE,
        };
        drop(g);
        cx.ret(v)
    });
    gl_op(reg, rt, "getFramebufferAttachmentParameter", |cx, w, gl| {
        let target = cx.arg_u32(0)?;
        let point = cx.arg_u32(1)?;
        let p = cx.arg_u32(2)?;
        let g = gl.borrow();
        let fbp = if target == 0x8CA8 { 0x8CAA } else { 0x8CA6 };
        let Some(fb) = g.bound.get(&fbp).and_then(|id| g.fbo(*id)) else {
            drop(g);
            return cx.ret(Arg::Null);
        };
        let att = match point {
            DEPTH_ATTACHMENT => fb.depth,
            STENCIL_ATTACHMENT => fb.stencil,
            DEPTH_STENCIL_ATTACHMENT => fb.depth,
            _ if point >= COLOR_ATTACHMENT0 && point < COLOR_ATTACHMENT0 + 8 => {
                fb.color[(point - COLOR_ATTACHMENT0) as usize]
            }
            _ => None,
        };
        let v = match (p, att) {
            (0x8CD0, None) => Arg::Number(0.0),
            (0x8CD0, Some(a)) => Arg::Number(f64::from(if matches!(a.kind, AttKind::Rb) {
                0x8D41
            } else {
                0x1702
            })),
            (0x8CD1, Some(a)) => opt_obj(w, Some(a.obj)),
            (
                0x8CD2,
                Some(Att {
                    kind: AttKind::Tex { level, .. },
                    ..
                }),
            ) => Arg::Number(f64::from(level)),
            (
                0x8CD3,
                Some(Att {
                    obj,
                    kind: AttKind::Tex { face, .. },
                }),
            ) => {
                let cube = g
                    .res
                    .get(&obj)
                    .and_then(|r| r.tex.as_ref())
                    .is_some_and(|t| t.target == gtex::TEXTURE_CUBE_MAP);
                Arg::Number(if cube { f64::from(0x8515 + face) } else { 0.0 })
            }
            (
                0x8CD4,
                Some(Att {
                    kind: AttKind::Tex { layer, .. },
                    ..
                }),
            ) => Arg::Number(f64::from(layer)),
            (0x8210, Some(_)) => Arg::Number(f64::from(0x2601)),
            (0x8211, Some(a)) => {
                let k = g.att_image(&a).map(|i| i.format.kind);
                Arg::Number(f64::from(match k {
                    Some(Kind::Float) => 0x1406,
                    Some(Kind::Int) => 0x1404,
                    Some(Kind::Uint) => 0x1405,
                    _ => 0x8C17,
                }))
            }
            (0x8212..=0x8217, Some(a)) => {
                let i = g.att_image(&a);
                let n = i.map_or(0, |i| match p {
                    0x8212..=0x8215 => {
                        let k = (p - 0x8212) as usize;
                        if (k as u8) < i.format.channels
                            && !matches!(
                                i.format.kind,
                                Kind::Depth | Kind::DepthStencil | Kind::Stencil
                            )
                        {
                            u32::from(i.format.bits[k])
                        } else {
                            0
                        }
                    }
                    0x8216 => u32::from(i.format.depth_bits),
                    _ => {
                        if matches!(i.format.kind, Kind::Stencil | Kind::DepthStencil) {
                            8
                        } else {
                            0
                        }
                    }
                });
                Arg::Number(f64::from(n))
            }
            _ => Arg::Null,
        };
        drop(g);
        cx.ret(v)
    });
    for name in ["drawBuffers"] {
        gl_op(reg, rt, name, |cx, _w, gl| {
            let list: Vec<u32> = arg_numbers(cx, 0)
                .unwrap_or_default()
                .into_iter()
                .map(|x| x as u32)
                .collect();
            set_draw_buffers(gl, list);
            Ok(())
        });
    }
    method(reg, rt, "WebGLDrawBuffers", "drawBuffersWEBGL", |cx, w| {
        super::super::need_args(cx, 1, "drawBuffersWEBGL", "WebGLDrawBuffers")?;
        let gl = this_ext(cx, w)?;
        let list: Vec<u32> = arg_numbers(cx, 0)
            .unwrap_or_default()
            .into_iter()
            .map(|x| x as u32)
            .collect();
        set_draw_buffers(&gl, list);
        Ok(())
    });
    gl_op(reg, rt, "readBuffer", |cx, _w, gl| {
        let src = cx.arg_u32(0)?;
        let mut g = gl.borrow_mut();
        match g.bound.get(&0x8CAA).copied() {
            Some(fb) => {
                if let Some(f) = g.res.get_mut(&fb).and_then(|r| r.fbo.as_deref_mut()) {
                    f.read = src;
                }
            }
            None => {
                if src != 0x0405 && src != 0 {
                    g.set_error(INVALID_OPERATION);
                    return Ok(());
                }
            }
        }
        g.state.insert(0x0C02, JsValue::Number(f64::from(src)));
        Ok(())
    });
    gl_op(reg, rt, "blitFramebuffer", |cx, _w, gl| {
        let mut v = [0i32; 8];
        for (i, x) in v.iter_mut().enumerate() {
            *x = cx.arg_i32(i)?;
        }
        let mask = cx.arg_u32(8)?;
        let filter = cx.arg_u32(9)?;
        let mut g = gl.borrow_mut();
        let src = match g.take_bufs(true) {
            Ok(b) => b,
            Err(e) => {
                g.set_error(e);
                return Ok(());
            }
        };
        let s_color = src
            .colors
            .first()
            .and_then(|c| c.as_ref())
            .map(|c| c.0.resolved());
        let s_depth = src.depth.as_ref().map(|d| d.0.resolved());
        g.put_bufs(src);
        let mut dst = match g.take_bufs(false) {
            Ok(b) => b,
            Err(e) => {
                g.set_error(e);
                return Ok(());
            }
        };
        let [sx0, sy0, sx1, sy1, dx0, dy0, dx1, dy1] = v;
        let (dw, dh) = (dst.width as i32, dst.height as i32);
        let scissor = g.enabled.contains(&SCISSOR_TEST).then_some(g.scissor);
        let lin = filter == 0x2601;
        for dy in dy0.min(dy1)..dy0.max(dy1) {
            for dx in dx0.min(dx1)..dx0.max(dx1) {
                if dx < 0 || dy < 0 || dx >= dw || dy >= dh {
                    continue;
                }
                if let Some(s) = scissor
                    && (dx < s[0] || dy < s[1] || dx >= s[0] + s[2] || dy >= s[1] + s[3])
                {
                    continue;
                }
                let fx = (f64::from(dx - dx0) + 0.5) / f64::from(dx1 - dx0) * f64::from(sx1 - sx0)
                    + f64::from(sx0);
                let fy = (f64::from(dy - dy0) + 0.5) / f64::from(dy1 - dy0) * f64::from(sy1 - sy0)
                    + f64::from(sy0);
                let (sx, sy) = (fx.floor() as i32, fy.floor() as i32);
                if mask & COLOR_BUFFER_BIT != 0
                    && let Some(sc) = &s_color
                    && sx >= 0
                    && sy >= 0
                    && (sx as u32) < sc.width
                    && (sy as u32) < sc.height
                {
                    let val = if lin {
                        let mut acc = [0f32; 4];
                        let (u, vv) = (fx - 0.5, fy - 0.5);
                        let (x0, y0) = (u.floor() as i32, vv.floor() as i32);
                        let (a, b) = ((u - u.floor()) as f32, (vv - vv.floor()) as f32);
                        for (oy, wy) in [(0, 1.0 - b), (1, b)] {
                            for (ox, wx) in [(0, 1.0 - a), (1, a)] {
                                let px = (x0 + ox).clamp(0, sc.width as i32 - 1) as u32;
                                let py = (y0 + oy).clamp(0, sc.height as i32 - 1) as u32;
                                let t = sc.read(sc.index(px, py, 0));
                                for k in 0..4 {
                                    acc[k] += t[k] * wx * wy;
                                }
                            }
                        }
                        acc
                    } else {
                        sc.read(sc.index(sx as u32, sy as u32, 0))
                    };
                    for (img, _) in dst.colors.iter_mut().flatten() {
                        let s = img.samples as usize;
                        let base = img.index(dx as u32, dy as u32, 0);
                        for k in 0..s {
                            img.write(base + k, val, [true; 4]);
                        }
                    }
                }
                if mask & DEPTH_BUFFER_BIT != 0
                    && let (Some(sd), Some((dd, _))) = (&s_depth, dst.depth.as_mut())
                    && sx >= 0
                    && sy >= 0
                    && (sx as u32) < sd.width
                    && (sy as u32) < sd.height
                {
                    let si = sd.index(sx as u32, sy as u32, 0);
                    let dv = if let Data::Ds(d, _) = &sd.data {
                        d.get(si).copied().unwrap_or(1.0)
                    } else {
                        1.0
                    };
                    let base = dd.index(dx as u32, dy as u32, 0);
                    let s = dd.samples as usize;
                    if let Data::Ds(d, _) = &mut dd.data {
                        for k in 0..s {
                            if let Some(p) = d.get_mut(base + k) {
                                *p = dv;
                            }
                        }
                    }
                }
            }
        }
        g.put_bufs(dst);
        Ok(())
    });
    for (name, kind) in [
        ("clearBufferfv", 0u8),
        ("clearBufferiv", 1),
        ("clearBufferuiv", 2),
        ("clearBufferfi", 3),
    ] {
        gl_op(reg, rt, name, move |cx, _w, gl| {
            let buffer = cx.arg_u32(0)?;
            let draw = cx.arg_i32(1)?;
            let vals: Vec<f64> = if kind == 3 {
                vec![cx.arg_f64(2)?, cx.arg_f64(3)?]
            } else {
                arg_numbers(cx, 2).unwrap_or_default()
            };
            let mut g = gl.borrow_mut();
            let mut b = match g.take_bufs(false) {
                Ok(b) => b,
                Err(e) => {
                    g.set_error(e);
                    return Ok(());
                }
            };
            let rect = g.enabled.contains(&SCISSOR_TEST).then_some(g.scissor);
            let mask = g.color_mask;
            match buffer {
                0x1800 => {
                    if let Some(Some((img, _))) = b.colors.get_mut(draw.max(0) as usize) {
                        let v: [f64; 4] =
                            std::array::from_fn(|k| vals.get(k).copied().unwrap_or(0.0));
                        match kind {
                            0 => img.clear_color(v.map(|x| x as f32), mask, rect),
                            1 => img.clear_bits(v.map(|x| x as i64 as i32 as u32), mask, rect),
                            _ => img.clear_bits(v.map(|x| x as i64 as u32), mask, rect),
                        }
                    }
                }
                0x1801 => {
                    if let Some((img, _)) = b.depth.as_mut() {
                        img.clear_depth(vals.first().copied().unwrap_or(1.0) as f32, rect);
                    }
                }
                0x1802 => {
                    let wm = g.num(0x0B98) as u32 as u8;
                    if let Some((img, _)) = b.stencil.as_mut().or(b.depth.as_mut()) {
                        img.clear_stencil(
                            vals.first().copied().unwrap_or(0.0) as i32 as u8,
                            wm,
                            rect,
                        );
                    }
                }
                0x84F9 => {
                    let wm = g.num(0x0B98) as u32 as u8;
                    if let Some((img, _)) = b.depth.as_mut() {
                        img.clear_depth(vals[0] as f32, rect);
                        img.clear_stencil(vals[1] as i32 as u8, wm, rect);
                    }
                }
                _ => g.set_error(INVALID_ENUM),
            }
            g.put_bufs(b);
            Ok(())
        });
    }
    gl_op(reg, rt, "invalidateFramebuffer", |_cx, _w, _gl| Ok(()));
    gl_op(reg, rt, "invalidateSubFramebuffer", |_cx, _w, _gl| Ok(()));
}

fn set_draw_buffers(gl: &Rc<RefCell<Gl>>, list: Vec<u32>) {
    let mut g = gl.borrow_mut();
    match g.bound.get(&0x8CA6).copied() {
        Some(fb) => {
            let ok = list
                .iter()
                .enumerate()
                .all(|(i, b)| *b == 0 || *b == COLOR_ATTACHMENT0 + i as u32);
            if !ok || list.len() > 8 {
                g.set_error(INVALID_OPERATION);
                return;
            }
            if let Some(f) = g.res.get_mut(&fb).and_then(|r| r.fbo.as_deref_mut()) {
                f.draw = list.clone();
            }
        }
        None => {
            if list.len() != 1 || !(list[0] == 0 || list[0] == 0x0405) {
                g.set_error(INVALID_OPERATION);
                return;
            }
        }
    }
    for k in 0..8u32 {
        let v = list.get(k as usize).copied().unwrap_or(0);
        g.state.insert(0x8825 + k, JsValue::Number(f64::from(v)));
    }
}

fn register_draws(reg: &mut Registry, rt: &Runtime) {
    gl_op(reg, rt, "clear", |cx, _w, gl| {
        let mask = cx.arg_u32(0)?;
        if mask & !(COLOR_BUFFER_BIT | DEPTH_BUFFER_BIT | STENCIL_BUFFER_BIT) != 0 {
            gl.borrow_mut().set_error(INVALID_VALUE);
            return Ok(());
        }
        gl.borrow_mut().clear_buffers(mask);
        Ok(())
    });
    for name in ["drawArrays", "drawArraysInstanced"] {
        gl_op(reg, rt, name, move |cx, _w, gl| {
            let mode = cx.arg_u32(0)?;
            let first = cx.arg_i32(1)?;
            let count = cx.arg_i32(2)?;
            let inst = if name == "drawArraysInstanced" {
                cx.arg_i32(3)?
            } else {
                1
            };
            if first < 0 || count < 0 || inst < 0 {
                gl.borrow_mut().set_error(INVALID_VALUE);
                return Ok(());
            }
            gl.borrow_mut().draw(
                mode,
                Indices::Seq(first as u32, count as u32),
                inst as u32,
                None,
            );
            Ok(())
        });
    }
    for name in ["drawElements", "drawElementsInstanced", "drawRangeElements"] {
        gl_op(reg, rt, name, move |cx, _w, gl| {
            let ranged = name == "drawRangeElements";
            let mode = cx.arg_u32(0)?;
            let count = cx.arg_i32(if ranged { 3 } else { 1 })?;
            let ty = cx.arg_u32(if ranged { 4 } else { 2 })?;
            let off = cx.arg_f64(if ranged { 5 } else { 3 })?;
            let inst = if name == "drawElementsInstanced" {
                cx.arg_i32(4)?
            } else {
                1
            };
            draw_elements(gl, mode, count, ty, off, inst);
            Ok(())
        });
    }
    method(
        reg,
        rt,
        "ANGLEInstancedArrays",
        "drawArraysInstancedANGLE",
        |cx, w| {
            super::super::need_args(cx, 4, "drawArraysInstancedANGLE", "ANGLEInstancedArrays")?;
            let gl = this_ext(cx, w)?;
            let mode = cx.arg_u32(0)?;
            let first = cx.arg_i32(1)?;
            let count = cx.arg_i32(2)?;
            let inst = cx.arg_i32(3)?;
            if first < 0 || count < 0 || inst < 0 {
                gl.borrow_mut().set_error(INVALID_VALUE);
                return Ok(());
            }
            gl.borrow_mut().draw(
                mode,
                Indices::Seq(first as u32, count as u32),
                inst as u32,
                None,
            );
            Ok(())
        },
    );
    method(
        reg,
        rt,
        "ANGLEInstancedArrays",
        "drawElementsInstancedANGLE",
        |cx, w| {
            super::super::need_args(cx, 5, "drawElementsInstancedANGLE", "ANGLEInstancedArrays")?;
            let gl = this_ext(cx, w)?;
            let mode = cx.arg_u32(0)?;
            let count = cx.arg_i32(1)?;
            let ty = cx.arg_u32(2)?;
            let off = cx.arg_f64(3)?;
            let inst = cx.arg_i32(4)?;
            draw_elements(&gl, mode, count, ty, off, inst);
            Ok(())
        },
    );
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
        let iface = if gl.borrow().webgl2 {
            "WebGL2RenderingContext"
        } else {
            "WebGLRenderingContext"
        };
        if cx.arg_kind(6) == ValueKind::Number && gl.borrow().webgl2 {
            // PIXEL_PACK_BUFFER target.
            let off = cx.arg_f64(6)?.max(0.0) as usize;
            let mut g = gl.borrow_mut();
            let Some(buf) = g.bound.get(&0x88EB).copied() else {
                g.set_error(INVALID_OPERATION);
                return Ok(());
            };
            let mut tmp = vec![0u8; (wd as usize * 16 + 8) * ht.max(1) as usize];
            if let Err(e) = g.read_rect(x, y, wd, ht, format, ty, &mut tmp) {
                g.set_error(e);
                return Ok(());
            }
            if let Some(r) = g.res.get_mut(&buf) {
                let n = r.bytes.len().saturating_sub(off).min(tmp.len());
                r.bytes[off..off + n].copy_from_slice(&tmp[..n]);
            }
            return Ok(());
        }
        if cx.arg_kind(6) != ValueKind::Object || cx.arg_view_kind(6).is_none() {
            return Err(HostError::type_error(format!(
                "Failed to execute 'readPixels' on '{iface}': parameter 7 is not of type 'ArrayBufferView'."
            )));
        }
        let vk = cx.arg_view_kind(6);
        let view_ok = match ty {
            UNSIGNED_BYTE => matches!(vk, Some(ViewKind::Uint8 | ViewKind::Uint8Clamped)),
            FLOAT_T => vk == Some(ViewKind::Float32),
            INT_T => vk == Some(ViewKind::Int32),
            UNSIGNED_INT => vk == Some(ViewKind::Uint32),
            _ => false,
        };
        if !view_ok {
            gl.borrow_mut().set_error(INVALID_OPERATION);
            return Ok(());
        }
        let el = if ty == UNSIGNED_BYTE { 1 } else { 4 };
        let dst_off = if cx.len() > 7 {
            cx.arg_f64(7)?.max(0.0) as usize * el
        } else {
            0
        };
        let mut buf = cx.arg_bytes(6).unwrap_or_default();
        if dst_off > buf.len() {
            gl.borrow_mut().set_error(INVALID_VALUE);
            return Ok(());
        }
        let r = gl
            .borrow_mut()
            .read_rect(x, y, wd, ht, format, ty, &mut buf[dst_off..]);
        match r {
            Ok(()) => {
                cx.write_arg_bytes(6, 0, &buf);
            }
            Err(e) => gl.borrow_mut().set_error(e),
        }
        Ok(())
    });
}

fn draw_elements(gl: &Rc<RefCell<Gl>>, mode: u32, count: i32, ty: u32, off: f64, inst: i32) {
    let mut g = gl.borrow_mut();
    if count < 0 || off < 0.0 || inst < 0 {
        g.set_error(INVALID_VALUE);
        return;
    }
    let size = match ty {
        UNSIGNED_BYTE => 1,
        UNSIGNED_SHORT => 2,
        UNSIGNED_INT if g.webgl2 || g.ext_enabled("OES_element_index_uint") => 4,
        _ => {
            g.set_error(INVALID_ENUM);
            return;
        }
    };
    let off = off as usize;
    if off % size != 0 {
        g.set_error(INVALID_OPERATION);
        return;
    }
    let Some(buf) = g.bound.get(&0x8895).and_then(|id| g.res.get(id)) else {
        g.set_error(INVALID_OPERATION);
        return;
    };
    let b = &buf.bytes;
    if off + count as usize * size > b.len() {
        g.set_error(INVALID_OPERATION);
        return;
    }
    let idx: Vec<u32> = (0..count as usize)
        .map(|k| {
            let p = off + k * size;
            match size {
                1 => u32::from(b[p]),
                2 => u32::from(u16::from_le_bytes([b[p], b[p + 1]])),
                _ => u32::from_le_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]]),
            }
        })
        .collect();
    let restart = g.webgl2.then_some(match size {
        1 => 0xFF,
        2 => 0xFFFF,
        _ => 0xFFFF_FFFF,
    });
    g.draw(mode, Indices::List(idx), inst as u32, restart);
}

fn register_state2(reg: &mut Registry, rt: &Runtime) {
    gl_op(reg, rt, "blendFunc", |cx, _w, gl| {
        let s = cx.arg_f64(0)?;
        let d = cx.arg_f64(1)?;
        for (p, v) in [(0x80C9, s), (0x80CB, s), (0x80C8, d), (0x80CA, d)] {
            set_state(gl, p, JsValue::Number(v));
        }
        Ok(())
    });
    gl_op(reg, rt, "stencilFuncSeparate", |cx, _w, gl| {
        let face = cx.arg_u32(0)?;
        let vals = [cx.arg_f64(1)?, cx.arg_f64(2)?, cx.arg_f64(3)?];
        for (k, p) in [0x0B92, 0x0B97, 0x0B93].iter().enumerate() {
            if face != 0x0405 {
                set_state(gl, *p, JsValue::Number(vals[k]));
            }
            if face != 0x0404 {
                set_state(gl, back_of(*p), JsValue::Number(vals[k]));
            }
        }
        Ok(())
    });
    gl_op(reg, rt, "stencilOpSeparate", |cx, _w, gl| {
        let face = cx.arg_u32(0)?;
        let vals = [cx.arg_f64(1)?, cx.arg_f64(2)?, cx.arg_f64(3)?];
        for (k, p) in [0x0B94, 0x0B95, 0x0B96].iter().enumerate() {
            if face != 0x0405 {
                set_state(gl, *p, JsValue::Number(vals[k]));
            }
            if face != 0x0404 {
                set_state(gl, back_of(*p), JsValue::Number(vals[k]));
            }
        }
        Ok(())
    });
    gl_op(reg, rt, "stencilMaskSeparate", |cx, _w, gl| {
        let face = cx.arg_u32(0)?;
        let v = cx.arg_f64(1)?;
        if face != 0x0405 {
            set_state(gl, 0x0B98, JsValue::Number(v));
        }
        if face != 0x0404 {
            set_state(gl, 0x8CA5, JsValue::Number(v));
        }
        Ok(())
    });
    gl_op(reg, rt, "bindBuffer", |cx, w, gl| {
        let target = cx.arg_u32(0)?;
        let obj = arg_res(cx, w, 1, gl);
        let mut g = gl.borrow_mut();
        let pname = match target {
            ARRAY_BUFFER => 0x8894,
            ELEMENT_ARRAY_BUFFER => 0x8895,
            0x8F36 | 0x8F37 | 0x8C8E if g.webgl2 => {
                if target == 0x8C8E {
                    0x8C8F
                } else {
                    target
                }
            }
            0x88EB if g.webgl2 => 0x88ED,
            0x88EC if g.webgl2 => 0x88EF,
            0x8A11 if g.webgl2 => 0x8A28,
            _ => {
                g.set_error(INVALID_ENUM);
                return Ok(());
            }
        };
        match obj {
            Some(id) => g.bound.insert(pname, id),
            None => g.bound.remove(&pname),
        };
        Ok(())
    });
    for name in ["bindBufferBase", "bindBufferRange"] {
        gl_op(reg, rt, name, move |cx, w, gl| {
            let target = cx.arg_u32(0)?;
            let index = cx.arg_u32(1)? as usize;
            let obj = arg_res(cx, w, 2, gl);
            let (off, size) = if name == "bindBufferRange" {
                (
                    cx.arg_f64(3)?.max(0.0) as usize,
                    cx.arg_f64(4)?.max(0.0) as usize,
                )
            } else {
                (0, 0)
            };
            let mut g = gl.borrow_mut();
            match target {
                0x8A11 => {
                    if index >= g.ubo.len() {
                        g.set_error(INVALID_VALUE);
                        return Ok(());
                    }
                    g.ubo[index] = (obj, off, size);
                    match obj {
                        Some(id) => g.bound.insert(0x8A28, id),
                        None => g.bound.remove(&0x8A28),
                    };
                }
                0x8C8E => {
                    match obj {
                        Some(id) => g.bound.insert(0x8C8F, id),
                        None => g.bound.remove(&0x8C8F),
                    };
                }
                _ => g.set_error(INVALID_ENUM),
            }
            Ok(())
        });
    }
    gl_op(reg, rt, "getBufferSubData", |cx, _w, gl| {
        let target = cx.arg_u32(0)?;
        let src = cx.arg_f64(1)?.max(0.0) as usize;
        let pname = match target {
            ARRAY_BUFFER => 0x8894,
            ELEMENT_ARRAY_BUFFER => 0x8895,
            0x8A11 => 0x8A28,
            0x88EB => 0x88ED,
            0x88EC => 0x88EF,
            0x8C8E => 0x8C8F,
            t => t,
        };
        let data = {
            let g = gl.borrow();
            g.bound
                .get(&pname)
                .and_then(|id| g.res.get(id))
                .map(|r| r.bytes.clone())
        };
        let Some(data) = data else {
            gl.borrow_mut().set_error(INVALID_OPERATION);
            return Ok(());
        };
        let Some(dst) = cx.arg_bytes(2) else {
            return Ok(());
        };
        if src + dst.len() > data.len() {
            gl.borrow_mut().set_error(INVALID_VALUE);
            return Ok(());
        }
        cx.write_arg_bytes(2, 0, &data[src..src + dst.len()]);
        Ok(())
    });
    gl_op(reg, rt, "copyBufferSubData", |cx, _w, gl| {
        let rt_ = cx.arg_u32(0)?;
        let wt = cx.arg_u32(1)?;
        let ro = cx.arg_f64(2)?.max(0.0) as usize;
        let wo = cx.arg_f64(3)?.max(0.0) as usize;
        let n = cx.arg_f64(4)?.max(0.0) as usize;
        let pn = |t: u32| match t {
            ARRAY_BUFFER => 0x8894,
            ELEMENT_ARRAY_BUFFER => 0x8895,
            0x8A11 => 0x8A28,
            0x88EB => 0x88ED,
            0x88EC => 0x88EF,
            0x8C8E => 0x8C8F,
            t => t,
        };
        let mut g = gl.borrow_mut();
        let (Some(a), Some(b)) = (
            g.bound.get(&pn(rt_)).copied(),
            g.bound.get(&pn(wt)).copied(),
        ) else {
            g.set_error(INVALID_OPERATION);
            return Ok(());
        };
        let src = g
            .res
            .get(&a)
            .and_then(|r| r.bytes.get(ro..ro + n).map(<[u8]>::to_vec));
        let ok = match (src, g.res.get_mut(&b)) {
            (Some(s), Some(r)) if wo + n <= r.bytes.len() => {
                r.bytes[wo..wo + n].copy_from_slice(&s);
                true
            }
            _ => false,
        };
        if !ok {
            g.set_error(INVALID_VALUE);
        }
        Ok(())
    });
    gl_op(reg, rt, "getInternalformatParameter", |cx, _w, gl| {
        let fmt = cx.arg_u32(1)?;
        let p = cx.arg_u32(2)?;
        let renderable = gimg::sized(fmt)
            .is_some_and(|f| !matches!(f.kind, Kind::Int | Kind::Uint | Kind::Snorm));
        if p != 0x80A9 {
            gl.borrow_mut().set_error(INVALID_ENUM);
            return cx.ret(Arg::Null);
        }
        cx.ret(Arg::Int32Array(if renderable { vec![4] } else { vec![] }))
    });
}
