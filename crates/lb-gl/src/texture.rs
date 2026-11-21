//! Texture objects, completeness and sampling.

use crate::exec::Textures;
use crate::glsl::types::{Dim, Scalar};
use crate::image::{Image, Kind, Swizzle, srgb_to_linear};

pub const TEXTURE_2D: u32 = 0x0DE1;
pub const TEXTURE_CUBE_MAP: u32 = 0x8513;
pub const TEXTURE_3D: u32 = 0x806F;
pub const TEXTURE_2D_ARRAY: u32 = 0x8C1A;

pub const NEAREST: u32 = 0x2600;
pub const LINEAR: u32 = 0x2601;
pub const NEAREST_MIPMAP_NEAREST: u32 = 0x2700;
pub const LINEAR_MIPMAP_NEAREST: u32 = 0x2701;
pub const NEAREST_MIPMAP_LINEAR: u32 = 0x2702;
pub const LINEAR_MIPMAP_LINEAR: u32 = 0x2703;
pub const REPEAT: u32 = 0x2901;
pub const CLAMP_TO_EDGE: u32 = 0x812F;
pub const MIRRORED_REPEAT: u32 = 0x8370;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SamplerParams {
    pub min_filter: u32,
    pub mag_filter: u32,
    pub wrap: [u32; 3],
    pub min_lod: f32,
    pub max_lod: f32,
    pub compare_mode: u32,
    pub compare_func: u32,
}

impl Default for SamplerParams {
    fn default() -> Self {
        SamplerParams {
            min_filter: NEAREST_MIPMAP_LINEAR,
            mag_filter: LINEAR,
            wrap: [REPEAT; 3],
            min_lod: -1000.0,
            max_lod: 1000.0,
            compare_mode: 0,
            compare_func: 0x0203,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Texture {
    /// Bound target (0 until first bind).
    pub target: u32,
    /// `[face][level]`; cube maps have six faces.
    pub faces: Vec<Vec<Option<Image>>>,
    pub params: SamplerParams,
    pub base_level: u32,
    pub max_level: u32,
    pub immutable_levels: u32,
}

impl Texture {
    pub fn new() -> Texture {
        Texture {
            max_level: 1000,
            ..Default::default()
        }
    }

    pub fn face_count(&self) -> usize {
        if self.target == TEXTURE_CUBE_MAP {
            6
        } else {
            1
        }
    }

    pub fn level(&self, face: usize, level: u32) -> Option<&Image> {
        self.faces.get(face)?.get(level as usize)?.as_ref()
    }

    pub fn level_mut(&mut self, face: usize, level: u32) -> Option<&mut Image> {
        self.faces.get_mut(face)?.get_mut(level as usize)?.as_mut()
    }

    pub fn set_level(&mut self, face: usize, level: u32, img: Option<Image>) {
        let n = self.face_count().max(face + 1);
        if self.faces.len() < n {
            self.faces.resize_with(n, Vec::new);
        }
        let f = &mut self.faces[face];
        if f.len() <= level as usize {
            f.resize_with(level as usize + 1, || None);
        }
        f[level as usize] = img;
    }

    /// Generates mipmaps from the base level (box filter).
    pub fn generate_mipmap(&mut self) {
        let base = self.base_level;
        for face in 0..self.faces.len() {
            let Some(b) = self.level(face, base).cloned() else {
                continue;
            };
            let mut prev = b;
            let mut lvl = base;
            while prev.width > 1
                || prev.height > 1
                || (self.target == TEXTURE_3D && prev.layers > 1)
            {
                let w = (prev.width / 2).max(1);
                let h = (prev.height / 2).max(1);
                let d = if self.target == TEXTURE_3D {
                    (prev.layers / 2).max(1)
                } else {
                    prev.layers
                };
                let mut next = Image::new(prev.format, w, h, d, 1);
                for z in 0..d {
                    for y in 0..h {
                        for x in 0..w {
                            let mut acc = [0f32; 4];
                            let mut n = 0.0;
                            let zs: &[u32] = if self.target == TEXTURE_3D {
                                &[0, 1]
                            } else {
                                &[0]
                            };
                            for dz in zs {
                                for dy in 0..2 {
                                    for dx in 0..2 {
                                        let sx = (x * 2 + dx).min(prev.width - 1);
                                        let sy = (y * 2 + dy).min(prev.height - 1);
                                        let sz = if self.target == TEXTURE_3D {
                                            (z * 2 + dz).min(prev.layers - 1)
                                        } else {
                                            z
                                        };
                                        let v = prev.read(prev.index(sx, sy, sz));
                                        for k in 0..4 {
                                            acc[k] += if prev.format.srgb && k < 3 {
                                                srgb_to_linear(v[k])
                                            } else {
                                                v[k]
                                            };
                                        }
                                        n += 1.0;
                                    }
                                }
                            }
                            let mut c = acc.map(|a| a / n);
                            if prev.format.srgb {
                                for v in c.iter_mut().take(3) {
                                    *v = crate::image::linear_to_srgb(*v);
                                }
                            }
                            let i = next.index(x, y, z);
                            next.write(i, c, [true; 4]);
                        }
                    }
                }
                lvl += 1;
                self.set_level(face, lvl, Some(next.clone()));
                prev = next;
            }
        }
    }

    /// Mip levels used for sampling (`None` when incomplete).
    pub fn complete_levels(
        &self,
        p: &SamplerParams,
        webgl2: bool,
        float_linear: bool,
    ) -> Option<u32> {
        let base = self.base_level;
        let b = self.level(0, base)?;
        if b.width == 0 || b.height == 0 {
            return None;
        }
        let fmt = b.format;
        let mip = !matches!(p.min_filter, NEAREST | LINEAR);
        // Cube faces must match the base.
        for face in 1..self.face_count() {
            let f = self.level(face, base)?;
            if f.width != b.width || f.height != b.height || f.format != fmt {
                return None;
            }
        }
        let integer = matches!(fmt.kind, Kind::Int | Kind::Uint);
        let linear =
            p.mag_filter == LINEAR || !matches!(p.min_filter, NEAREST | NEAREST_MIPMAP_NEAREST);
        if integer && linear {
            return None;
        }
        if fmt.kind == Kind::Float && fmt.bits[0] == 32 && linear && !float_linear {
            return None;
        }
        if matches!(fmt.kind, Kind::Depth | Kind::DepthStencil)
            && linear
            && p.compare_mode == 0
            && webgl2
        {
            return None;
        }
        if !webgl2 {
            let pot = |v: u32| v.is_power_of_two();
            let npot = !pot(b.width) || !pot(b.height);
            if npot && (mip || p.wrap[0] != CLAMP_TO_EDGE || p.wrap[1] != CLAMP_TO_EDGE) {
                return None;
            }
        }
        if !mip {
            return Some(1);
        }
        // Full chain down to 1x1 (or max level).
        let mut w = b.width;
        let mut h = b.height;
        let mut d = b.layers;
        let mut n = 1;
        let max = if self.immutable_levels > 0 {
            (base + self.immutable_levels - 1).min(self.max_level)
        } else {
            self.max_level
        };
        let mut lvl = base;
        while (w > 1 || h > 1 || (self.target == TEXTURE_3D && d > 1)) && lvl < max {
            w = (w / 2).max(1);
            h = (h / 2).max(1);
            if self.target == TEXTURE_3D {
                d = (d / 2).max(1);
            }
            lvl += 1;
            for face in 0..self.face_count() {
                let l = self.level(face, lvl)?;
                if l.width != w
                    || l.height != h
                    || l.format != fmt
                    || (self.target == TEXTURE_3D && l.layers != d)
                {
                    return None;
                }
            }
            n += 1;
        }
        Some(n)
    }
}

/// One texture unit's textures for a draw.
#[derive(Default, Clone)]
pub struct Unit<'a> {
    pub d2: Option<&'a Texture>,
    pub cube: Option<&'a Texture>,
    pub d3: Option<&'a Texture>,
    pub array: Option<&'a Texture>,
    /// Sampler object parameters (WebGL 2).
    pub sampler: Option<SamplerParams>,
}

/// Bound textures for a draw.
pub struct Units<'a> {
    pub units: Vec<Unit<'a>>,
    pub webgl2: bool,
    pub float_linear: bool,
    /// Textures attached to the current framebuffer (feedback: sample as
    /// incomplete).
    pub feedback: Vec<*const Texture>,
}

struct Bound<'a> {
    tex: &'a Texture,
    params: SamplerParams,
    levels: u32,
}

impl<'a> Units<'a> {
    fn bound(&self, unit: i32, dim: Dim) -> Option<Bound<'a>> {
        let u = self.units.get(usize::try_from(unit).ok()?)?;
        let tex = match dim {
            Dim::D2 => u.d2,
            Dim::Cube => u.cube,
            Dim::D3 => u.d3,
            Dim::D2Array => u.array,
        }?;
        if self.feedback.contains(&(tex as *const Texture)) {
            return None;
        }
        let params = u.sampler.unwrap_or(tex.params);
        let levels = tex.complete_levels(&params, self.webgl2, self.float_linear)?;
        Some(Bound {
            tex,
            params,
            levels,
        })
    }
}

fn wrap(c: i32, size: i32, mode: u32) -> i32 {
    match mode {
        REPEAT => c.rem_euclid(size),
        MIRRORED_REPEAT => {
            let period = 2 * size;
            let m = c.rem_euclid(period);
            if m < size { m } else { period - 1 - m }
        }
        _ => c.clamp(0, size - 1),
    }
}

/// Texel value with format swizzle and sRGB decoding.
fn texel(img: &Image, x: i32, y: i32, z: i32) -> [f32; 4] {
    let v = img.read(img.index(x as u32, y as u32, z as u32));
    let f = img.format;
    let mut v = match f.swizzle {
        Swizzle::Rgba => v,
        Swizzle::Luminance => [v[0], v[0], v[0], 1.0],
        Swizzle::Alpha => [0.0, 0.0, 0.0, v[0]],
        Swizzle::LuminanceAlpha => [v[0], v[0], v[0], v[1]],
    };
    if f.srgb {
        for c in v.iter_mut().take(3) {
            *c = srgb_to_linear(*c);
        }
    }
    v
}

fn cube_face(p: [f32; 3]) -> (usize, f32, f32) {
    let (x, y, z) = (p[0], p[1], p[2]);
    let (ax, ay, az) = (x.abs(), y.abs(), z.abs());
    let (face, sc, tc, ma) = if ax >= ay && ax >= az {
        if x >= 0.0 {
            (0, -z, -y, ax)
        } else {
            (1, z, -y, ax)
        }
    } else if ay >= az {
        if y >= 0.0 {
            (2, x, z, ay)
        } else {
            (3, x, -z, ay)
        }
    } else if z >= 0.0 {
        (4, x, -y, az)
    } else {
        (5, -x, -y, az)
    };
    let ma = if ma == 0.0 { 1.0 } else { ma };
    (face, (sc / ma + 1.0) / 2.0, (tc / ma + 1.0) / 2.0)
}

impl Bound<'_> {
    /// Filters one level at normalized coordinates.
    fn filter(
        &self,
        face: usize,
        level: u32,
        s: f32,
        t: f32,
        r: f32,
        linear: bool,
        offset: [i32; 3],
        cube: bool,
        layer: Option<i32>,
        compare: Option<f32>,
    ) -> [f32; 4] {
        let Some(img) = self.tex.level(face, level) else {
            return [0.0, 0.0, 0.0, 1.0];
        };
        let (w, h, d) = (img.width as i32, img.height as i32, img.layers as i32);
        let wraps = if cube {
            [CLAMP_TO_EDGE; 3]
        } else {
            self.params.wrap
        };
        let z_fixed = layer.map(|l| l.clamp(0, d - 1));
        let cmp = |v: [f32; 4]| -> [f32; 4] {
            match compare {
                Some(rf) => {
                    let rf = rf.clamp(0.0, 1.0);
                    let dv = v[0];
                    let pass = match self.params.compare_func {
                        0x0200 => false,
                        0x0201 => rf < dv,
                        0x0202 => rf == dv,
                        0x0203 => rf <= dv,
                        0x0204 => rf > dv,
                        0x0205 => rf != dv,
                        0x0206 => rf >= dv,
                        _ => true,
                    };
                    let x = if pass { 1.0 } else { 0.0 };
                    [x, 0.0, 0.0, 1.0]
                }
                None => v,
            }
        };
        if !linear {
            let x = wrap((s * w as f32).floor() as i32 + offset[0], w, wraps[0]);
            let y = wrap((t * h as f32).floor() as i32 + offset[1], h, wraps[1]);
            let z = z_fixed.unwrap_or_else(|| {
                if d > 1 {
                    wrap((r * d as f32).floor() as i32 + offset[2], d, wraps[2])
                } else {
                    0
                }
            });
            return cmp(texel(img, x, y, z));
        }
        let u = s * w as f32 - 0.5;
        let v = t * h as f32 - 0.5;
        let (i0, j0) = (u.floor() as i32 + offset[0], v.floor() as i32 + offset[1]);
        let (a, b) = (u - u.floor(), v - v.floor());
        let xs = [wrap(i0, w, wraps[0]), wrap(i0 + 1, w, wraps[0])];
        let ys = [wrap(j0, h, wraps[1]), wrap(j0 + 1, h, wraps[1])];
        let (zs, c) = match z_fixed {
            Some(z) => ([z, z], 0.0),
            None if d > 1 => {
                let ww = r * d as f32 - 0.5;
                let k0 = ww.floor() as i32 + offset[2];
                (
                    [wrap(k0, d, wraps[2]), wrap(k0 + 1, d, wraps[2])],
                    ww - ww.floor(),
                )
            }
            None => ([0, 0], 0.0),
        };
        let mut out = [0f32; 4];
        for (zi, wz) in [(0, 1.0 - c), (1, c)] {
            if wz == 0.0 {
                continue;
            }
            for (yi, wy) in [(0, 1.0 - b), (1, b)] {
                for (xi, wx) in [(0, 1.0 - a), (1, a)] {
                    let wgt = wx * wy * wz;
                    if wgt == 0.0 {
                        continue;
                    }
                    let tv = cmp(texel(img, xs[xi], ys[yi], zs[zi]));
                    for k in 0..4 {
                        out[k] += tv[k] * wgt;
                    }
                }
            }
        }
        out
    }
}

impl Textures for Units<'_> {
    fn sample(
        &self,
        unit: i32,
        dim: Dim,
        ret: Scalar,
        coord: [f32; 3],
        lod: f32,
        offset: [i32; 3],
        compare: Option<f32>,
    ) -> [u32; 4] {
        let default = match ret {
            Scalar::Float => [0, 0, 0, 1f32.to_bits()],
            _ => [0, 0, 0, 1],
        };
        let Some(b) = self.bound(unit, dim) else {
            return default;
        };
        let p = b.params;
        let base = b.tex.base_level;
        let lod = if lod.is_nan() {
            0.0
        } else {
            lod.clamp(p.min_lod, p.max_lod)
        };
        let mag = lod <= 0.0;
        let filt = if mag { p.mag_filter } else { p.min_filter };
        let compare = if p.compare_mode == 0x884E {
            compare
        } else {
            None
        };
        let (face, s, t, r, layer) = match dim {
            Dim::Cube => {
                let (f, s, t) = cube_face(coord);
                (f, s, t, 0.0, None)
            }
            Dim::D2Array => (
                0,
                coord[0],
                coord[1],
                0.0,
                Some(coord[2].round_ties_even() as i32),
            ),
            Dim::D3 => (0, coord[0], coord[1], coord[2], None),
            Dim::D2 => (0, coord[0], coord[1], 0.0, None),
        };
        let cube = dim == Dim::Cube;
        let max_lvl = (b.levels - 1) as f32;
        let v = match filt {
            NEAREST | LINEAR => b.filter(
                face,
                base,
                s,
                t,
                r,
                filt == LINEAR,
                offset,
                cube,
                layer,
                compare,
            ),
            NEAREST_MIPMAP_NEAREST | LINEAR_MIPMAP_NEAREST => {
                let l = if lod <= 0.5 {
                    0.0
                } else {
                    ((lod + 0.5).ceil() - 1.0).min(max_lvl)
                };
                b.filter(
                    face,
                    base + l as u32,
                    s,
                    t,
                    r,
                    filt == LINEAR_MIPMAP_NEAREST,
                    offset,
                    cube,
                    layer,
                    compare,
                )
            }
            _ => {
                let lin = filt == LINEAR_MIPMAP_LINEAR;
                if lod >= max_lvl {
                    b.filter(
                        face,
                        base + max_lvl as u32,
                        s,
                        t,
                        r,
                        lin,
                        offset,
                        cube,
                        layer,
                        compare,
                    )
                } else {
                    let l0 = lod.max(0.0).floor();
                    let fr = lod.max(0.0) - l0;
                    let a = b.filter(
                        face,
                        base + l0 as u32,
                        s,
                        t,
                        r,
                        lin,
                        offset,
                        cube,
                        layer,
                        compare,
                    );
                    let c = b.filter(
                        face,
                        base + l0 as u32 + 1,
                        s,
                        t,
                        r,
                        lin,
                        offset,
                        cube,
                        layer,
                        compare,
                    );
                    std::array::from_fn(|k| a[k] * (1.0 - fr) + c[k] * fr)
                }
            }
        };
        match ret {
            Scalar::Float => v.map(f32::to_bits),
            _ => {
                // Integer textures: nearest texel's raw bits.
                let Some(img) = b.tex.level(face, base) else {
                    return default;
                };
                let (w, h) = (img.width as i32, img.height as i32);
                let x = wrap((s * w as f32).floor() as i32 + offset[0], w, p.wrap[0]);
                let y = wrap((t * h as f32).floor() as i32 + offset[1], h, p.wrap[1]);
                let z = layer.unwrap_or(if img.layers > 1 {
                    wrap(
                        (r * img.layers as f32).floor() as i32,
                        img.layers as i32,
                        p.wrap[2],
                    )
                } else {
                    0
                });
                img.read_bits(img.index(
                    x as u32,
                    y as u32,
                    z.clamp(0, img.layers as i32 - 1) as u32,
                ))
            }
        }
    }

    fn fetch(&self, unit: i32, dim: Dim, ret: Scalar, c: [i32; 3], lod: i32) -> [u32; 4] {
        let zero = [0u32; 4];
        let Some(b) = self.bound(unit, dim) else {
            return zero;
        };
        let lvl = b.tex.base_level as i32 + lod;
        if lod < 0 || lod as u32 >= b.levels {
            return zero;
        }
        let Some(img) = b.tex.level(0, lvl as u32) else {
            return zero;
        };
        if c[0] < 0
            || c[1] < 0
            || c[2] < 0
            || c[0] >= img.width as i32
            || c[1] >= img.height as i32
            || c[2] >= img.layers as i32
        {
            return zero;
        }
        match ret {
            Scalar::Float => texel(img, c[0], c[1], c[2]).map(f32::to_bits),
            _ => img.read_bits(img.index(c[0] as u32, c[1] as u32, c[2] as u32)),
        }
    }

    fn size(&self, unit: i32, dim: Dim, lod: i32) -> [i32; 3] {
        let u = usize::try_from(unit).ok().and_then(|u| self.units.get(u));
        let tex = u.and_then(|u| match dim {
            Dim::D2 => u.d2,
            Dim::Cube => u.cube,
            Dim::D3 => u.d3,
            Dim::D2Array => u.array,
        });
        let Some(tex) = tex else { return [0; 3] };
        match tex.level(0, (tex.base_level as i32 + lod.max(0)) as u32) {
            Some(img) => [img.width as i32, img.height as i32, img.layers as i32],
            None => [0; 3],
        }
    }
}
