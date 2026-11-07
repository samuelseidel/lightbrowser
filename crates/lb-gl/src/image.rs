//! Images (texture levels, renderbuffers, the drawing buffer) and their
//! internal formats.

use crate::math::{f16_to_f32, f32_to_f16};

/// How a format stores values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Unorm,
    Snorm,
    Float,
    Int,
    Uint,
    Depth,
    DepthStencil,
    Stencil,
}

/// Channel mapping for sampling unsized luminance/alpha formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Swizzle {
    Rgba,
    Luminance,
    Alpha,
    LuminanceAlpha,
}

/// An internal format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    /// Sized GL enum (or the unsized base format for WebGL 1 textures).
    pub gl: u32,
    pub kind: Kind,
    /// Real channels (1-4); the rest read as 0, 0, 0, 1.
    pub channels: u8,
    /// Bits per channel (unorm quantization; 16 marks half floats).
    pub bits: [u8; 4],
    pub srgb: bool,
    pub swizzle: Swizzle,
    /// Depth bits (depth formats).
    pub depth_bits: u8,
}

const fn fmt(gl: u32, kind: Kind, channels: u8, bits: [u8; 4]) -> Format {
    Format {
        gl,
        kind,
        channels,
        bits,
        srgb: false,
        swizzle: Swizzle::Rgba,
        depth_bits: 0,
    }
}

pub const RGBA8: Format = fmt(0x8058, Kind::Unorm, 4, [8; 4]);
pub const RGB8: Format = fmt(0x8051, Kind::Unorm, 3, [8, 8, 8, 0]);

/// Sized internal format by GL enum.
pub fn sized(gl: u32) -> Option<Format> {
    use Kind::*;
    let mut f = match gl {
        0x8058 => RGBA8,
        0x8051 => RGB8,
        0x8229 => fmt(gl, Unorm, 1, [8, 0, 0, 0]),    // R8
        0x822B => fmt(gl, Unorm, 2, [8, 8, 0, 0]),    // RG8
        0x8D62 => fmt(gl, Unorm, 3, [5, 6, 5, 0]),    // RGB565
        0x8056 => fmt(gl, Unorm, 4, [4; 4]),          // RGBA4
        0x8057 => fmt(gl, Unorm, 4, [5, 5, 5, 1]),    // RGB5_A1
        0x8059 => fmt(gl, Unorm, 4, [10, 10, 10, 2]), // RGB10_A2
        0x8C41 | 0x8C43 => {
            let mut f = if gl == 0x8C43 { RGBA8 } else { RGB8 };
            f.gl = gl;
            f.srgb = true;
            f
        }
        0x8F94 => fmt(gl, Snorm, 1, [8, 0, 0, 0]),
        0x8F95 => fmt(gl, Snorm, 2, [8, 8, 0, 0]),
        0x8F96 => fmt(gl, Snorm, 3, [8, 8, 8, 0]),
        0x8F97 => fmt(gl, Snorm, 4, [8; 4]),
        0x822D => fmt(gl, Float, 1, [16, 0, 0, 0]), // R16F
        0x822F => fmt(gl, Float, 2, [16, 16, 0, 0]),
        0x881B => fmt(gl, Float, 3, [16, 16, 16, 0]),
        0x881A => fmt(gl, Float, 4, [16; 4]),
        0x822E => fmt(gl, Float, 1, [32, 0, 0, 0]), // R32F
        0x8230 => fmt(gl, Float, 2, [32, 32, 0, 0]),
        0x8815 => fmt(gl, Float, 3, [32, 32, 32, 0]),
        0x8814 => fmt(gl, Float, 4, [32; 4]),
        0x8C3A => fmt(gl, Float, 3, [11, 11, 10, 0]), // R11F_G11F_B10F
        0x8C3D => fmt(gl, Float, 3, [9, 9, 9, 0]),    // RGB9_E5
        0x8231 => fmt(gl, Int, 1, [8, 0, 0, 0]),
        0x8232 => fmt(gl, Uint, 1, [8, 0, 0, 0]),
        0x8233 => fmt(gl, Int, 1, [16, 0, 0, 0]),
        0x8234 => fmt(gl, Uint, 1, [16, 0, 0, 0]),
        0x8235 => fmt(gl, Int, 1, [32, 0, 0, 0]),
        0x8236 => fmt(gl, Uint, 1, [32, 0, 0, 0]),
        0x8237 => fmt(gl, Int, 2, [8, 8, 0, 0]),
        0x8238 => fmt(gl, Uint, 2, [8, 8, 0, 0]),
        0x8239 => fmt(gl, Int, 2, [16, 16, 0, 0]),
        0x823A => fmt(gl, Uint, 2, [16, 16, 0, 0]),
        0x823B => fmt(gl, Int, 2, [32, 32, 0, 0]),
        0x823C => fmt(gl, Uint, 2, [32, 32, 0, 0]),
        0x8D8F => fmt(gl, Int, 3, [8, 8, 8, 0]),
        0x8D7D => fmt(gl, Uint, 3, [8, 8, 8, 0]),
        0x8D89 => fmt(gl, Int, 3, [16, 16, 16, 0]),
        0x8D77 => fmt(gl, Uint, 3, [16, 16, 16, 0]),
        0x8D83 => fmt(gl, Int, 3, [32, 32, 32, 0]),
        0x8D71 => fmt(gl, Uint, 3, [32, 32, 32, 0]),
        0x8D8E => fmt(gl, Int, 4, [8; 4]),
        0x8D7C => fmt(gl, Uint, 4, [8; 4]),
        0x8D88 => fmt(gl, Int, 4, [16; 4]),
        0x8D76 => fmt(gl, Uint, 4, [16; 4]),
        0x8D82 => fmt(gl, Int, 4, [32; 4]),
        0x8D70 => fmt(gl, Uint, 4, [32; 4]),
        0x906F => fmt(gl, Uint, 4, [10, 10, 10, 2]), // RGB10_A2UI
        0x81A5 => Format {
            depth_bits: 16,
            ..fmt(gl, Depth, 1, [0; 4])
        },
        0x81A6 => Format {
            depth_bits: 24,
            ..fmt(gl, Depth, 1, [0; 4])
        },
        0x8CAC => Format {
            depth_bits: 32,
            ..fmt(gl, Depth, 1, [0; 4])
        },
        0x88F0 => Format {
            depth_bits: 24,
            ..fmt(gl, DepthStencil, 1, [0; 4])
        },
        0x8CAD => Format {
            depth_bits: 32,
            ..fmt(gl, DepthStencil, 1, [0; 4])
        },
        0x8D48 => fmt(gl, Stencil, 1, [8, 0, 0, 0]),
        // WebGL 1 renderbuffer DEPTH_STENCIL.
        0x84F9 => Format {
            depth_bits: 24,
            ..fmt(gl, DepthStencil, 1, [0; 4])
        },
        _ => return None,
    };
    if f.kind == Float && f.bits[0] == 16 {
        f.bits = [16; 4];
    }
    Some(f)
}

/// Effective format of an unsized WebGL 1 `texImage2D(format, type)`.
pub fn unsized_format(format: u32, ty: u32) -> Option<Format> {
    let base = |gl: u32, ch: u8, sw: Swizzle, bits: [u8; 4], kind: Kind| Format {
        gl,
        kind,
        channels: ch,
        bits,
        srgb: false,
        swizzle: sw,
        depth_bits: 0,
    };
    let (kind, b) = match ty {
        0x1401 => (Kind::Unorm, 8),
        0x1406 => (Kind::Float, 32),
        0x8D61 | 0x140B => (Kind::Float, 16),
        0x8363 => {
            return (format == 0x1907).then(|| Format {
                gl: 0x1907,
                ..sized(0x8D62).expect("565")
            });
        }
        0x8033 => {
            return (format == 0x1908).then(|| Format {
                gl: 0x1908,
                ..sized(0x8056).expect("4444")
            });
        }
        0x8034 => {
            return (format == 0x1908).then(|| Format {
                gl: 0x1908,
                ..sized(0x8057).expect("5551")
            });
        }
        0x1403 | 0x1405 | 0x84FA if matches!(format, 0x1902 | 0x84F9) => {
            let f = if format == 0x84F9 {
                0x88F0
            } else if ty == 0x1403 {
                0x81A5
            } else {
                0x81A6
            };
            return sized(f).map(|f| Format { gl: format, ..f });
        }
        _ => return None,
    };
    Some(match format {
        0x1908 => base(0x1908, 4, Swizzle::Rgba, [b; 4], kind),
        0x1907 => base(0x1907, 3, Swizzle::Rgba, [b, b, b, 0], kind),
        0x1909 => base(0x1909, 1, Swizzle::Luminance, [b, 0, 0, 0], kind),
        0x1906 => base(0x1906, 1, Swizzle::Alpha, [b, 0, 0, 0], kind),
        0x190A => base(0x190A, 2, Swizzle::LuminanceAlpha, [b, b, 0, 0], kind),
        0x8C40 => Format {
            srgb: true,
            ..base(0x8C40, 3, Swizzle::Rgba, [8, 8, 8, 0], kind)
        },
        0x8C42 => Format {
            srgb: true,
            ..base(0x8C42, 4, Swizzle::Rgba, [8; 4], kind)
        },
        _ => return None,
    })
}

/// Pixel storage.
#[derive(Clone, Debug)]
pub enum Data {
    /// 4 bytes per sample (unorm formats, values quantized to the format).
    U8(Vec<u8>),
    /// 4 floats per sample (float and snorm formats).
    F32(Vec<f32>),
    I32(Vec<i32>),
    U32(Vec<u32>),
    /// Depth and/or stencil planes (empty when absent).
    Ds(Vec<f32>, Vec<u8>),
}

#[derive(Clone, Debug)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// Layers (3D textures, arrays).
    pub layers: u32,
    pub samples: u32,
    pub format: Format,
    pub data: Data,
    /// Set once a draw rasterized into the image (readback noise applies
    /// to rendered pixels only).
    pub drawn: bool,
}

impl Image {
    pub fn new(format: Format, width: u32, height: u32, layers: u32, samples: u32) -> Image {
        let n = width as usize * height as usize * layers.max(1) as usize * samples.max(1) as usize;
        let data = match format.kind {
            Kind::Unorm => Data::U8(vec![0; n * 4]),
            Kind::Snorm | Kind::Float => Data::F32(vec![0.0; n * 4]),
            Kind::Int => Data::I32(vec![0; n * 4]),
            Kind::Uint => Data::U32(vec![0; n * 4]),
            Kind::Depth => Data::Ds(vec![0.0; n], Vec::new()),
            Kind::DepthStencil => Data::Ds(vec![0.0; n], vec![0; n]),
            Kind::Stencil => Data::Ds(Vec::new(), vec![0; n]),
        };
        let mut img = Image {
            width,
            height,
            layers: layers.max(1),
            samples: samples.max(1),
            format,
            data,
            drawn: false,
        };
        if format.kind == Kind::Unorm && format.channels < 4 {
            // Missing alpha reads as 1.
            if let Data::U8(d) = &mut img.data {
                for px in d.as_chunks_mut::<4>().0.iter_mut() {
                    px[3] = 255;
                }
            }
        }
        img
    }

    pub fn texels(&self) -> usize {
        self.width as usize * self.height as usize * self.layers as usize
    }

    #[inline]
    pub fn index(&self, x: u32, y: u32, layer: u32) -> usize {
        ((layer as usize * self.height as usize + y as usize) * self.width as usize + x as usize)
            * self.samples as usize
    }

    /// Value of a sample as normalized floats (color formats), with the
    /// format's channel defaults.
    pub fn read(&self, i: usize) -> [f32; 4] {
        match &self.data {
            Data::U8(d) => {
                let p = &d[i * 4..i * 4 + 4];
                [
                    f32::from(p[0]) / 255.0,
                    f32::from(p[1]) / 255.0,
                    f32::from(p[2]) / 255.0,
                    f32::from(p[3]) / 255.0,
                ]
            }
            Data::F32(d) => [d[i * 4], d[i * 4 + 1], d[i * 4 + 2], d[i * 4 + 3]],
            Data::I32(d) => std::array::from_fn(|k| d[i * 4 + k] as f32),
            Data::U32(d) => std::array::from_fn(|k| d[i * 4 + k] as f32),
            Data::Ds(dep, _) => [dep.get(i).copied().unwrap_or(0.0), 0.0, 0.0, 1.0],
        }
    }

    /// Raw 32-bit components of a sample (integer formats; float bits
    /// otherwise).
    pub fn read_bits(&self, i: usize) -> [u32; 4] {
        match &self.data {
            Data::I32(d) => std::array::from_fn(|k| d[i * 4 + k] as u32),
            Data::U32(d) => std::array::from_fn(|k| d[i * 4 + k]),
            _ => self.read(i).map(f32::to_bits),
        }
    }

    /// Stores a normalized color (quantized to the format).
    pub fn write(&mut self, i: usize, c: [f32; 4], mask: [bool; 4]) {
        let f = self.format;
        match &mut self.data {
            Data::U8(d) => {
                for k in 0..4 {
                    if !mask[k] {
                        continue;
                    }
                    if k as u8 >= f.channels {
                        d[i * 4 + k] = if k == 3 { 255 } else { 0 };
                        continue;
                    }
                    d[i * 4 + k] = quantize_unorm(c[k], f.bits[k]);
                }
            }
            Data::F32(d) => {
                for k in 0..4 {
                    if !mask[k] {
                        continue;
                    }
                    let v = if k as u8 >= f.channels {
                        if k == 3 { 1.0 } else { 0.0 }
                    } else if f.kind == Kind::Snorm {
                        let q = (c[k].clamp(-1.0, 1.0) * 127.0).round_ties_even();
                        q / 127.0
                    } else if f.bits[k] == 16 {
                        f16_to_f32(f32_to_f16(c[k]))
                    } else {
                        c[k]
                    };
                    d[i * 4 + k] = v;
                }
            }
            Data::I32(_) | Data::U32(_) => {
                let bits = c.map(f32::to_bits);
                self.write_bits(i, bits, mask);
            }
            Data::Ds(..) => {}
        }
    }

    /// Stores integer components (integer formats), clamped to the format.
    pub fn write_bits(&mut self, i: usize, v: [u32; 4], mask: [bool; 4]) {
        let f = self.format;
        match &mut self.data {
            Data::I32(d) => {
                for k in 0..4 {
                    if mask[k] {
                        let x = v[k] as i32;
                        let b = f.bits[k.min(3)].max(1);
                        let x = if k as u8 >= f.channels {
                            i32::from(k == 3)
                        } else if b < 32 {
                            x.clamp(-(1 << (b - 1)), (1 << (b - 1)) - 1)
                        } else {
                            x
                        };
                        d[i * 4 + k] = x;
                    }
                }
            }
            Data::U32(d) => {
                for k in 0..4 {
                    if mask[k] {
                        let b = f.bits[k.min(3)].max(1);
                        let x = if k as u8 >= f.channels {
                            u32::from(k == 3)
                        } else if b < 32 {
                            v[k].min((1u32 << b) - 1)
                        } else {
                            v[k]
                        };
                        d[i * 4 + k] = x;
                    }
                }
            }
            _ => {}
        }
    }

    /// Fills every sample.
    pub fn clear_color(&mut self, c: [f32; 4], mask: [bool; 4], rect: Option<[i32; 4]>) {
        let (x0, y0, x1, y1) = clip_rect(rect, self.width, self.height);
        let s = self.samples as usize;
        for layer in 0..self.layers {
            for y in y0..y1 {
                for x in x0..x1 {
                    let base = self.index(x, y, layer);
                    for k in 0..s {
                        self.write(base + k, c, mask);
                    }
                }
            }
        }
    }

    pub fn clear_bits(&mut self, v: [u32; 4], mask: [bool; 4], rect: Option<[i32; 4]>) {
        let (x0, y0, x1, y1) = clip_rect(rect, self.width, self.height);
        let s = self.samples as usize;
        for layer in 0..self.layers {
            for y in y0..y1 {
                for x in x0..x1 {
                    let base = self.index(x, y, layer);
                    for k in 0..s {
                        self.write_bits(base + k, v, mask);
                    }
                }
            }
        }
    }

    pub fn clear_depth(&mut self, d: f32, rect: Option<[i32; 4]>) {
        let (x0, y0, x1, y1) = clip_rect(rect, self.width, self.height);
        let s = self.samples as usize;
        let d = quantize_depth(d.clamp(0.0, 1.0), self.format.depth_bits);
        let (w, h) = (self.width as usize, self.height as usize);
        if let Data::Ds(dep, _) = &mut self.data
            && !dep.is_empty()
        {
            for layer in 0..self.layers as usize {
                for y in y0..y1 {
                    for x in x0..x1 {
                        let base = ((layer * h + y as usize) * w + x as usize) * s;
                        dep[base..base + s].fill(d);
                    }
                }
            }
        }
    }

    pub fn clear_stencil(&mut self, v: u8, write_mask: u8, rect: Option<[i32; 4]>) {
        let (x0, y0, x1, y1) = clip_rect(rect, self.width, self.height);
        let s = self.samples as usize;
        let (w, h) = (self.width as usize, self.height as usize);
        if let Data::Ds(_, st) = &mut self.data
            && !st.is_empty()
        {
            for layer in 0..self.layers as usize {
                for y in y0..y1 {
                    for x in x0..x1 {
                        let base = ((layer * h + y as usize) * w + x as usize) * s;
                        for p in &mut st[base..base + s] {
                            *p = (*p & !write_mask) | (v & write_mask);
                        }
                    }
                }
            }
        }
    }

    /// Resolved RGBA8 rows, bottom-up (GL order), of a color image.
    pub fn resolve_rgba8(&self, layer: u32) -> Vec<u8> {
        let (w, h, s) = (
            self.width as usize,
            self.height as usize,
            self.samples as usize,
        );
        let mut out = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let base = self.index(x as u32, y as u32, layer);
                let o = (y * w + x) * 4;
                match &self.data {
                    Data::U8(d) => {
                        for k in 0..4 {
                            let sum: u32 = (0..s).map(|j| u32::from(d[(base + j) * 4 + k])).sum();
                            out[o + k] = ((sum + s as u32 / 2) / s as u32) as u8;
                        }
                    }
                    _ => {
                        let mut acc = [0f32; 4];
                        for j in 0..s {
                            let v = self.read(base + j);
                            for k in 0..4 {
                                acc[k] += v[k];
                            }
                        }
                        for k in 0..4 {
                            out[o + k] = quantize_unorm(acc[k] / s as f32, 8);
                        }
                    }
                }
            }
        }
        out
    }

    /// Resolved single-sample copy (blits, `readPixels` of other types).
    pub fn resolved(&self) -> Image {
        if self.samples == 1 {
            return self.clone();
        }
        let mut out = Image::new(self.format, self.width, self.height, self.layers, 1);
        let s = self.samples as usize;
        for layer in 0..self.layers {
            for y in 0..self.height {
                for x in 0..self.width {
                    let base = self.index(x, y, layer);
                    let o = out.index(x, y, layer);
                    match (&self.data, &mut out.data) {
                        (Data::U8(d), Data::U8(od)) => {
                            for k in 0..4 {
                                let sum: u32 =
                                    (0..s).map(|j| u32::from(d[(base + j) * 4 + k])).sum();
                                od[o * 4 + k] = ((sum + s as u32 / 2) / s as u32) as u8;
                            }
                        }
                        (Data::Ds(dd, ss), Data::Ds(od, os)) => {
                            if !dd.is_empty() {
                                od[o] = dd[base];
                            }
                            if !ss.is_empty() {
                                os[o] = ss[base];
                            }
                        }
                        _ => {
                            let mut acc = [0f32; 4];
                            for j in 0..s {
                                let v = self.read(base + j);
                                for k in 0..4 {
                                    acc[k] += v[k] / s as f32;
                                }
                            }
                            if matches!(self.format.kind, Kind::Int | Kind::Uint) {
                                let v = self.read_bits(base);
                                out.write_bits(o, v, [true; 4]);
                            } else {
                                out.write(o, acc, [true; 4]);
                            }
                        }
                    }
                }
            }
        }
        out
    }
}

fn clip_rect(rect: Option<[i32; 4]>, w: u32, h: u32) -> (u32, u32, u32, u32) {
    match rect {
        None => (0, 0, w, h),
        Some(r) => {
            let x0 = r[0].clamp(0, w as i32) as u32;
            let y0 = r[1].clamp(0, h as i32) as u32;
            let x1 = (r[0].saturating_add(r[2])).clamp(0, w as i32) as u32;
            let y1 = (r[1].saturating_add(r[3])).clamp(0, h as i32) as u32;
            (x0, y0, x1.max(x0), y1.max(y0))
        }
    }
}

/// Rounds to the format's unorm precision, stored as 8 bits (bit
/// replication for fewer bits).
pub fn quantize_unorm(v: f32, bits: u8) -> u8 {
    let v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
    if bits >= 8 || bits == 0 {
        return (v * 255.0 + 0.5) as u8;
    }
    let max = (1u32 << bits) - 1;
    let q = (v * max as f32 + 0.5) as u32;
    // Expand to 8 bits.
    ((q * 255 + max / 2) / max).min(255) as u8
}

pub fn quantize_depth(d: f32, bits: u8) -> f32 {
    match bits {
        16 => (d * 65535.0).round() / 65535.0,
        24 => (f64::from(d) * 16_777_215.0).round() as f32 / 16_777_215.0,
        _ => d,
    }
}

pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c < 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}
