//! The draw pipeline: vertex fetch and shading, primitive assembly,
//! clipping, culling, rasterization (triangles, lines, points) with
//! multisampling, fragment shading in 4x4 tiles, and per-sample tests,
//! blending and writes.

use std::collections::HashMap;

use crate::exec::{Exec, Textures, W};
use crate::glsl::ast::{Builtin, Interp};
use crate::glsl::types::{Base, Scalar};
use crate::image::{Image, Kind, linear_to_srgb, quantize_depth, srgb_to_linear};
use crate::ir::Shader;
use crate::math::f16_to_f32;
use crate::program::Program;

pub const POINTS: u32 = 0;
pub const LINES: u32 = 1;
pub const LINE_LOOP: u32 = 2;
pub const LINE_STRIP: u32 = 3;
pub const TRIANGLES: u32 = 4;
pub const TRIANGLE_STRIP: u32 = 5;
pub const TRIANGLE_FAN: u32 = 6;

/// A vertex attribute array.
#[derive(Clone, Copy)]
pub struct VertexArray<'a> {
    pub data: &'a [u8],
    pub size: u32,
    pub ty: u32,
    pub normalized: bool,
    pub integer: bool,
    pub stride: u32,
    pub offset: usize,
    pub divisor: u32,
}

#[derive(Clone, Copy)]
pub enum AttribSrc<'a> {
    Array(VertexArray<'a>),
    /// Generic value (`vertexAttrib4f` etc.), raw cells.
    Value([u32; 4]),
}

#[derive(Clone, Copy, Debug)]
pub struct StencilFace {
    pub func: u32,
    pub reference: i32,
    pub value_mask: u32,
    pub write_mask: u32,
    pub fail: u32,
    pub zfail: u32,
    pub zpass: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Blend {
    /// RGB and alpha equations.
    pub eq: [u32; 2],
    pub src: [u32; 2],
    pub dst: [u32; 2],
    pub color: [f32; 4],
}

/// Fixed-function state of a draw.
#[derive(Clone, Debug)]
pub struct Fixed {
    pub viewport: [i32; 4],
    pub depth_range: [f32; 2],
    pub scissor: Option<[i32; 4]>,
    /// Culled face (FRONT, BACK, FRONT_AND_BACK) when culling is on.
    pub cull: Option<u32>,
    pub front_ccw: bool,
    pub polygon_offset: Option<(f32, f32)>,
    pub depth_func: Option<u32>,
    pub depth_mask: bool,
    pub stencil: Option<[StencilFace; 2]>,
    pub blend: Option<Blend>,
    pub color_mask: [bool; 4],
    pub rasterizer_discard: bool,
    pub alpha_to_coverage: bool,
    pub sample_coverage: Option<(f32, bool)>,
}

pub enum Indices {
    Seq(u32, u32),
    List(Vec<u32>),
}

/// Render target of a draw (one image per fragment output location).
pub struct Target<'a> {
    pub width: u32,
    pub height: u32,
    pub samples: u32,
    pub colors: Vec<Option<&'a mut Image>>,
    pub depth: Option<(&'a mut [f32], u8)>,
    pub stencil: Option<&'a mut [u8]>,
}

pub struct Draw<'a> {
    pub prog: &'a Program,
    /// Program uniform storage.
    pub uniforms: &'a [u32],
    /// Bytes bound to each program uniform block.
    pub blocks: Vec<Option<&'a [u8]>>,
    /// Sources per attribute location.
    pub attribs: Vec<AttribSrc<'a>>,
    pub textures: &'a dyn Textures,
    pub mode: u32,
    pub indices: Indices,
    pub instances: u32,
    pub fixed: &'a Fixed,
    /// Primitive restart index for element draws (WebGL 2).
    pub restart: Option<u32>,
}

/// Sample positions inside a pixel (bottom-left origin).
fn sample_pos(samples: u32) -> &'static [(f32, f32)] {
    if samples >= 4 {
        &[
            (0.375, 0.875),
            (0.875, 0.625),
            (0.125, 0.375),
            (0.625, 0.125),
        ]
    } else {
        &[(0.5, 0.5)]
    }
}

fn read_component(data: &[u8], off: usize, ty: u32, normalized: bool, integer: bool) -> u32 {
    let b = |n: usize| data.get(off..off + n);
    let fl = |x: f32| x.to_bits();
    match ty {
        0x1406 => b(4).map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])),
        0x140B => b(2).map_or(0, |s| fl(f16_to_f32(u16::from_le_bytes([s[0], s[1]])))),
        0x1400 => {
            let v = b(1).map_or(0, |s| s[0] as i8);
            if integer {
                v as i32 as u32
            } else if normalized {
                fl((f32::from(v) / 127.0).max(-1.0))
            } else {
                fl(f32::from(v))
            }
        }
        0x1401 => {
            let v = b(1).map_or(0, |s| s[0]);
            if integer {
                u32::from(v)
            } else if normalized {
                fl(f32::from(v) / 255.0)
            } else {
                fl(f32::from(v))
            }
        }
        0x1402 => {
            let v = b(2).map_or(0, |s| i16::from_le_bytes([s[0], s[1]]));
            if integer {
                v as i32 as u32
            } else if normalized {
                fl((f32::from(v) / 32767.0).max(-1.0))
            } else {
                fl(f32::from(v))
            }
        }
        0x1403 => {
            let v = b(2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]));
            if integer {
                u32::from(v)
            } else if normalized {
                fl(f32::from(v) / 65535.0)
            } else {
                fl(f32::from(v))
            }
        }
        0x1404 => {
            let v = b(4).map_or(0, |s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]));
            if integer {
                v as u32
            } else if normalized {
                fl((v as f32 / 2_147_483_647.0).max(-1.0))
            } else {
                fl(v as f32)
            }
        }
        0x1405 => {
            let v = b(4).map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
            if integer {
                v
            } else if normalized {
                fl(v as f32 / 4_294_967_295.0)
            } else {
                fl(v as f32)
            }
        }
        _ => 0,
    }
}

fn type_size(ty: u32) -> usize {
    match ty {
        0x1400 | 0x1401 => 1,
        0x1402 | 0x1403 | 0x140B => 2,
        _ => 4,
    }
}

fn fetch(src: &AttribSrc<'_>, vertex: u32, instance: u32, shader_int: bool) -> [u32; 4] {
    match src {
        AttribSrc::Value(v) => *v,
        AttribSrc::Array(a) => {
            let elem = if a.divisor == 0 {
                vertex
            } else {
                instance / a.divisor
            };
            let packed = matches!(a.ty, 0x8D9F | 0x8368);
            let esize = if packed {
                4
            } else {
                type_size(a.ty) * a.size as usize
            };
            let stride = if a.stride == 0 {
                esize
            } else {
                a.stride as usize
            };
            let off = a.offset + elem as usize * stride;
            let one = if shader_int { 1 } else { 1f32.to_bits() };
            let mut out = [0, 0, 0, one];
            if packed {
                let w = a
                    .data
                    .get(off..off + 4)
                    .map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
                let signed = a.ty == 0x8D9F;
                for (k, (shift, bits)) in [(0u32, 10u32), (10, 10), (20, 10), (30, 2)]
                    .iter()
                    .enumerate()
                {
                    let raw = (w >> shift) & ((1 << bits) - 1);
                    let v = if signed {
                        let x = ((raw << (32 - bits)) as i32) >> (32 - bits);
                        if a.normalized {
                            (x as f32 / ((1 << (bits - 1)) - 1) as f32).max(-1.0)
                        } else {
                            x as f32
                        }
                    } else if a.normalized {
                        raw as f32 / ((1 << bits) - 1) as f32
                    } else {
                        raw as f32
                    };
                    out[k] = v.to_bits();
                }
                return out;
            }
            for k in 0..a.size.min(4) as usize {
                out[k] = read_component(
                    a.data,
                    off + k * type_size(a.ty),
                    a.ty,
                    a.normalized,
                    a.integer,
                );
                if !a.integer && shader_int {
                    out[k] = f32::from_bits(out[k]) as i32 as u32;
                } else if a.integer && !shader_int {
                    out[k] = (out[k] as i32 as f32).to_bits();
                }
            }
            out
        }
    }
}

/// Shaded vertex: clip position, point size and varying cells.
#[derive(Clone)]
struct VOut {
    pos: [f32; 4],
    psize: f32,
    var: Vec<f32>,
}

fn load_uniforms(e: &mut Exec<'_>, sh: &Shader, d: &Draw<'_>, vs: bool) {
    for u in &d.prog.uniforms {
        let Some(cell) = (if vs { u.vs } else { u.fs }) else {
            continue;
        };
        if let Some((b, off, astride, mstride, row_major)) = u.block {
            let Some(bytes) = d.blocks.get(b as usize).copied().flatten() else {
                continue;
            };
            let rd = |o: usize| {
                bytes
                    .get(o..o + 4)
                    .map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
            };
            let ty = u.ty;
            let comps = ty.comps();
            for el in 0..u.size {
                let base = off as usize + (el * astride) as usize;
                for c in 0..u32::from(ty.cols.max(1)) {
                    for r in 0..u32::from(ty.rows) {
                        let o = if ty.cols > 1 {
                            if row_major {
                                base + (r * mstride + c * 4) as usize
                            } else {
                                base + (c * mstride + r * 4) as usize
                            }
                        } else {
                            base + (r * 4) as usize
                        };
                        let mut v = rd(o);
                        if ty.base == Base::S(Scalar::Bool) {
                            v = u32::from(v != 0);
                        }
                        e.splat(cell + el * comps + c * u32::from(ty.rows) + r, v);
                    }
                }
            }
            continue;
        }
        let n = u.ty.cells(&sh.structs) * u.size;
        for k in 0..n {
            e.splat(
                cell + k,
                d.uniforms
                    .get((u.offset + k) as usize)
                    .copied()
                    .unwrap_or(0),
            );
        }
    }
}

/// Runs a draw; returns false if a shader ran out of its budget.
pub fn draw(d: &Draw<'_>, t: &mut Target<'_>, budget: &mut u64) -> bool {
    let p = d.prog;
    let vs = &p.vs;
    let fs = &p.fs;
    let mut ve = Exec::new(vs.cells, d.textures, *budget);
    load_uniforms(&mut ve, vs, d, true);
    let pos_cell = vs.builtins.get(&Builtin::Position).copied();
    let ps_cell = vs.builtins.get(&Builtin::PointSize).copied();
    let vid_cell = vs.builtins.get(&Builtin::VertexId).copied();
    let iid_cell = vs.builtins.get(&Builtin::InstanceId).copied();
    let nvar: usize = p.varyings.iter().map(|v| v.cells as usize).sum();
    // Attribute inputs: (cell, location, int).
    let mut inputs = Vec::new();
    for a in &p.attribs {
        let int = matches!(a.ty.base, Base::S(Scalar::Int | Scalar::Uint));
        for c in 0..u32::from(a.ty.cols.max(1)) {
            inputs.push((
                a.cell + c * u32::from(a.ty.rows),
                a.location + c,
                int,
                u32::from(a.ty.rows),
            ));
        }
    }
    let mut fe = Exec::new(fs.cells, d.textures, 0);
    load_uniforms(&mut fe, fs, d, false);
    let mut ok = true;
    let seq: Vec<u32> = match &d.indices {
        Indices::Seq(first, count) => (*first..first + count).collect(),
        Indices::List(l) => l.clone(),
    };
    for inst in 0..d.instances.max(1) {
        // Unique vertices.
        let mut slot_of: HashMap<u32, usize> = HashMap::new();
        let mut uniq: Vec<u32> = Vec::new();
        let mut slots: Vec<Option<usize>> = Vec::with_capacity(seq.len());
        for &v in &seq {
            if Some(v) == d.restart {
                slots.push(None);
                continue;
            }
            let s = *slot_of.entry(v).or_insert_with(|| {
                uniq.push(v);
                uniq.len() - 1
            });
            slots.push(Some(s));
        }
        let mut outs: Vec<VOut> = Vec::with_capacity(uniq.len());
        for chunk in uniq.chunks(W) {
            for (l, v) in chunk.iter().enumerate() {
                for (cell, loc, int, rows) in &inputs {
                    let src = d
                        .attribs
                        .get(*loc as usize)
                        .copied()
                        .unwrap_or(AttribSrc::Value([0, 0, 0, 1f32.to_bits()]));
                    let val = fetch(&src, *v, inst, *int);
                    for k in 0..*rows {
                        ve.regs[(cell + k) as usize][l] = val[k as usize];
                    }
                }
                if let Some(c) = vid_cell {
                    ve.regs[c as usize][l] = *v;
                }
                if let Some(c) = iid_cell {
                    ve.regs[c as usize][l] = inst;
                }
            }
            ve.run(&vs.code, crate::exec::ALL);
            if ve.aborted {
                *budget = 0;
                return false;
            }
            for l in 0..chunk.len() {
                let rd = |c: u32| f32::from_bits(ve.regs[c as usize][l]);
                let pos = pos_cell.map_or([0.0; 4], |c| [rd(c), rd(c + 1), rd(c + 2), rd(c + 3)]);
                let psize = ps_cell.map_or(1.0, rd);
                let mut var = Vec::with_capacity(nvar);
                for v in &p.varyings {
                    for k in 0..v.cells {
                        var.push(rd(v.vs + k));
                    }
                }
                outs.push(VOut { pos, psize, var });
            }
        }
        *budget = ve.budget;
        if d.fixed.rasterizer_discard {
            continue;
        }
        fe.budget = *budget;
        let mut r = Raster {
            d,
            t,
            fe: &mut fe,
            fs,
            nvar,
        };
        // Primitive assembly per restart segment.
        for seg in slots.split(Option::is_none) {
            let s: Vec<usize> = seg.iter().map(|x| x.expect("slot")).collect();
            let n = s.len();
            match d.mode {
                TRIANGLES => {
                    for k in (0..n / 3).map(|k| k * 3) {
                        r.triangle([&outs[s[k]], &outs[s[k + 1]], &outs[s[k + 2]]]);
                    }
                }
                TRIANGLE_STRIP => {
                    for k in 2..n {
                        if k % 2 == 0 {
                            r.triangle([&outs[s[k - 2]], &outs[s[k - 1]], &outs[s[k]]]);
                        } else {
                            r.triangle([&outs[s[k - 1]], &outs[s[k - 2]], &outs[s[k]]]);
                        }
                    }
                }
                TRIANGLE_FAN => {
                    for k in 2..n {
                        r.triangle([&outs[s[0]], &outs[s[k - 1]], &outs[s[k]]]);
                    }
                }
                LINES => {
                    for k in (0..n / 2).map(|k| k * 2) {
                        r.line(&outs[s[k]], &outs[s[k + 1]]);
                    }
                }
                LINE_STRIP | LINE_LOOP => {
                    for k in 1..n {
                        r.line(&outs[s[k - 1]], &outs[s[k]]);
                    }
                    if d.mode == LINE_LOOP && n > 1 {
                        r.line(&outs[s[n - 1]], &outs[s[0]]);
                    }
                }
                POINTS => {
                    for k in 0..n {
                        r.point(&outs[s[k]]);
                    }
                }
                _ => {}
            }
            if r.fe.aborted {
                ok = false;
                break;
            }
        }
        *budget = fe.budget;
        if !ok {
            break;
        }
    }
    ok
}

/// A window-space vertex.
#[derive(Clone)]
struct WV {
    x: f64,
    y: f64,
    z: f32,
    /// 1 / w_clip
    iw: f32,
    var: Vec<f32>,
}

struct Raster<'a, 'b, 'c, 'e> {
    d: &'a Draw<'a>,
    t: &'b mut Target<'c>,
    fe: &'b mut Exec<'e>,
    fs: &'a Shader,
    nvar: usize,
}

const SUB: f64 = 256.0;

fn snap(v: f64) -> f64 {
    (v * SUB).round() / SUB
}

impl Raster<'_, '_, '_, '_> {
    fn to_window(&self, v: &VOut) -> WV {
        let f = self.d.fixed;
        let [vx, vy, vw, vh] = f.viewport;
        let iw = 1.0 / v.pos[3];
        let (nx, ny, nz) = (v.pos[0] * iw, v.pos[1] * iw, v.pos[2] * iw);
        let x = f64::from(vx) + (f64::from(nx) + 1.0) * f64::from(vw) / 2.0;
        let y = f64::from(vy) + (f64::from(ny) + 1.0) * f64::from(vh) / 2.0;
        let [n, fa] = f.depth_range;
        let z = n + (fa - n) * (nz + 1.0) / 2.0;
        WV {
            x: snap(x),
            y: snap(y),
            z,
            iw,
            var: v.var.clone(),
        }
    }

    /// Clips a triangle in clip space against the six planes.
    fn triangle(&mut self, v: [&VOut; 3]) {
        let inside = |p: &[f32; 4], k: usize| -> f32 {
            match k {
                0 => p[3] + p[0],
                1 => p[3] - p[0],
                2 => p[3] + p[1],
                3 => p[3] - p[1],
                4 => p[3] + p[2],
                _ => p[3] - p[2],
            }
        };
        let all_in = v.iter().all(|x| (0..6).all(|k| inside(&x.pos, k) >= 0.0))
            && v.iter().all(|x| x.pos[3] > 0.0);
        let provoking = v[2].var.clone();
        if all_in {
            let w = [
                self.to_window(v[0]),
                self.to_window(v[1]),
                self.to_window(v[2]),
            ];
            self.raster_tri(&w, &provoking);
            return;
        }
        for k in 0..6 {
            if v.iter().all(|x| inside(&x.pos, k) < 0.0) {
                return;
            }
        }
        let mut poly: Vec<VOut> = v.iter().map(|x| (*x).clone()).collect();
        for k in 0..7 {
            let mut out = Vec::new();
            for i in 0..poly.len() {
                let a = &poly[i];
                let b = &poly[(i + 1) % poly.len()];
                let (da, db) = if k == 6 {
                    (a.pos[3] - 1e-6, b.pos[3] - 1e-6)
                } else {
                    (inside(&a.pos, k), inside(&b.pos, k))
                };
                if da >= 0.0 {
                    out.push(a.clone());
                }
                if (da >= 0.0) != (db >= 0.0) {
                    let t = da / (da - db);
                    let lerp = |x: f32, y: f32| x + (y - x) * t;
                    out.push(VOut {
                        pos: std::array::from_fn(|j| lerp(a.pos[j], b.pos[j])),
                        psize: a.psize,
                        var: a
                            .var
                            .iter()
                            .zip(&b.var)
                            .map(|(x, y)| lerp(*x, *y))
                            .collect(),
                    });
                }
            }
            poly = out;
            if poly.len() < 3 {
                return;
            }
        }
        let w: Vec<WV> = poly.iter().map(|x| self.to_window(x)).collect();
        for i in 1..w.len() - 1 {
            self.raster_tri(&[w[0].clone(), w[i].clone(), w[i + 1].clone()], &provoking);
        }
    }

    fn bounds(&self) -> (i32, i32, i32, i32) {
        let (mut x0, mut y0, mut x1, mut y1) = (0, 0, self.t.width as i32, self.t.height as i32);
        if let Some(s) = self.d.fixed.scissor {
            x0 = x0.max(s[0]);
            y0 = y0.max(s[1]);
            x1 = x1.min(s[0].saturating_add(s[2]));
            y1 = y1.min(s[1].saturating_add(s[3]));
        }
        (x0, y0, x1, y1)
    }

    fn raster_tri(&mut self, w: &[WV; 3], provoking: &[f32]) {
        let area = (w[1].x - w[0].x) * (w[2].y - w[0].y) - (w[2].x - w[0].x) * (w[1].y - w[0].y);
        if area == 0.0 || !area.is_finite() {
            return;
        }
        let ccw = area > 0.0;
        let front = ccw == self.d.fixed.front_ccw;
        if let Some(c) = self.d.fixed.cull {
            let culled = match c {
                0x0404 => front,
                0x0405 => !front,
                _ => true,
            };
            if culled {
                return;
            }
        }
        let (bx0, by0, bx1, by1) = self.bounds();
        let minx = w
            .iter()
            .map(|v| v.x)
            .fold(f64::MAX, f64::min)
            .floor()
            .max(f64::from(bx0)) as i32;
        let maxx = w
            .iter()
            .map(|v| v.x)
            .fold(f64::MIN, f64::max)
            .ceil()
            .min(f64::from(bx1)) as i32;
        let miny = w
            .iter()
            .map(|v| v.y)
            .fold(f64::MAX, f64::min)
            .floor()
            .max(f64::from(by0)) as i32;
        let maxy = w
            .iter()
            .map(|v| v.y)
            .fold(f64::MIN, f64::max)
            .ceil()
            .min(f64::from(by1)) as i32;
        if minx >= maxx || miny >= maxy {
            return;
        }
        let sgn = if ccw { 1.0 } else { -1.0 };
        // Edge i is opposite vertex i: from w[(i+1)%3] to w[(i+2)%3].
        let edge = |i: usize, px: f64, py: f64| -> f64 {
            let a = &w[(i + 1) % 3];
            let b = &w[(i + 2) % 3];
            ((b.x - a.x) * (py - a.y) - (b.y - a.y) * (px - a.x)) * sgn
        };
        let top_left: [bool; 3] = std::array::from_fn(|i| {
            let (a, b) = if ccw {
                (&w[(i + 1) % 3], &w[(i + 2) % 3])
            } else {
                (&w[(i + 2) % 3], &w[(i + 1) % 3])
            };
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            dy < 0.0 || (dy == 0.0 && dx < 0.0)
        });
        let covered = |px: f64, py: f64| -> bool {
            (0..3).all(|i| {
                let e = edge(i, px, py);
                e > 0.0 || (e == 0.0 && top_left[i])
            })
        };
        let area_abs = area.abs();
        let bary = |px: f64, py: f64| -> [f32; 3] {
            std::array::from_fn(|i| (edge(i, px, py) / area_abs) as f32)
        };
        // Polygon offset.
        let mut zoff = 0.0f32;
        if let Some((factor, units)) = self.d.fixed.polygon_offset {
            let dzdx = ((w[1].z - w[0].z) as f64 * (w[2].y - w[0].y)
                - (w[2].z - w[0].z) as f64 * (w[1].y - w[0].y))
                / area;
            let dzdy = ((w[2].z - w[0].z) as f64 * (w[1].x - w[0].x)
                - (w[1].z - w[0].z) as f64 * (w[2].x - w[0].x))
                / area;
            let m = dzdx.abs().max(dzdy.abs()) as f32;
            zoff = factor * m + units / 16_777_216.0;
        }
        let samples = sample_pos(self.t.samples);
        let interps: Vec<Interp> = self
            .d
            .prog
            .varyings
            .iter()
            .flat_map(|v| std::iter::repeat_n(v.interp, v.cells as usize))
            .collect();
        let tx0 = minx & !3;
        let ty0 = miny & !3;
        let mut ty = ty0;
        while ty < maxy {
            let mut tx = tx0;
            while tx < maxx {
                let mut cov = [0u32; W];
                let mut any = false;
                for (l, c) in cov.iter_mut().enumerate() {
                    let (px, py) = lane_pixel(tx, ty, l);
                    if px < minx || px >= maxx || py < miny || py >= maxy {
                        continue;
                    }
                    for (si, (sx, sy)) in samples.iter().enumerate() {
                        if covered(
                            f64::from(px) + f64::from(*sx),
                            f64::from(py) + f64::from(*sy),
                        ) {
                            *c |= 1 << si;
                        }
                    }
                    any |= *c != 0;
                }
                if any {
                    self.shade_tile(
                        tx,
                        ty,
                        &cov,
                        |px, py| bary(px, py),
                        w,
                        provoking,
                        &interps,
                        front,
                        zoff,
                        None,
                    );
                    if self.fe.aborted {
                        return;
                    }
                }
                tx += 4;
            }
            ty += 4;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn shade_tile(
        &mut self,
        tx: i32,
        ty: i32,
        cov: &[u32; W],
        bary: impl Fn(f64, f64) -> [f32; 3],
        w: &[WV; 3],
        provoking: &[f32],
        interps: &[Interp],
        front: bool,
        zoff: f32,
        point: Option<(f64, f64, f64)>,
    ) {
        let fs = self.fs;
        let samples = sample_pos(self.t.samples);
        let mut bc = [[0f32; 3]; W];
        for (l, b) in bc.iter_mut().enumerate() {
            let (px, py) = lane_pixel(tx, ty, l);
            *b = bary(f64::from(px) + 0.5, f64::from(py) + 0.5);
        }
        // Varyings.
        let mut k = 0;
        for v in &self.d.prog.varyings {
            for c in 0..v.cells {
                let idx = k;
                k += 1;
                let mut out = [0u32; W];
                for l in 0..W {
                    let b = bc[l];
                    let val = match interps.get(idx).copied().unwrap_or_default() {
                        Interp::Flat => provoking[idx],
                        Interp::NoPerspective => {
                            b[0] * w[0].var[idx] + b[1] * w[1].var[idx] + b[2] * w[2].var[idx]
                        }
                        Interp::Smooth => {
                            let den = b[0] * w[0].iw + b[1] * w[1].iw + b[2] * w[2].iw;
                            (b[0] * w[0].iw * w[0].var[idx]
                                + b[1] * w[1].iw * w[1].var[idx]
                                + b[2] * w[2].iw * w[2].var[idx])
                                / den
                        }
                    };
                    out[l] = val.to_bits();
                }
                self.fe.regs[(v.fs + c) as usize] = out;
            }
        }
        let _ = self.nvar;
        // Built-ins.
        let zc = |b: &[f32; 3]| b[0] * w[0].z + b[1] * w[1].z + b[2] * w[2].z;
        if let Some(c) = fs.builtins.get(&Builtin::FragCoord).copied() {
            for l in 0..W {
                let (px, py) = lane_pixel(tx, ty, l);
                let b = bc[l];
                let iw = b[0] * w[0].iw + b[1] * w[1].iw + b[2] * w[2].iw;
                let r = &mut self.fe.regs;
                r[c as usize][l] = (px as f32 + 0.5).to_bits();
                r[c as usize + 1][l] = (py as f32 + 0.5).to_bits();
                r[c as usize + 2][l] = (zc(&b) + zoff).clamp(0.0, 1.0).to_bits();
                r[c as usize + 3][l] = iw.to_bits();
            }
        }
        if let Some(c) = fs.builtins.get(&Builtin::FrontFacing).copied() {
            self.fe.splat(c, u32::from(front));
        }
        if let Some(c) = fs.builtins.get(&Builtin::PointCoord).copied() {
            for l in 0..W {
                let (px, py) = lane_pixel(tx, ty, l);
                let (s, t) = match point {
                    Some((cx, cy, size)) => (
                        0.5 + (f64::from(px) + 0.5 - cx) / size,
                        0.5 - (f64::from(py) + 0.5 - cy) / size,
                    ),
                    None => (0.0, 0.0),
                };
                self.fe.regs[c as usize][l] = (s as f32).to_bits();
                self.fe.regs[c as usize + 1][l] = (t as f32).to_bits();
            }
        }
        let alive = self.fe.run(&fs.code, crate::exec::ALL);
        if self.fe.aborted {
            return;
        }
        let depth_cell = if fs.writes_frag_depth {
            fs.builtins.get(&Builtin::FragDepth).copied()
        } else {
            None
        };
        for l in 0..W {
            let mut m = cov[l];
            if m == 0 || alive & (1 << l) == 0 {
                continue;
            }
            let (px, py) = lane_pixel(tx, ty, l);
            // Colors per output.
            let outs: Vec<(u32, [u32; 4], Scalar)> = self
                .d
                .prog
                .outputs
                .iter()
                .map(|(loc, cell, n, k)| {
                    let mut v = [
                        0u32,
                        0,
                        0,
                        if *k == Scalar::Float {
                            1f32.to_bits()
                        } else {
                            1
                        },
                    ];
                    for (j, x) in v.iter_mut().enumerate().take(*n as usize) {
                        *x = self.fe.regs[(cell + j as u32) as usize][l];
                    }
                    (*loc, v, *k)
                })
                .collect();
            if self.d.fixed.alpha_to_coverage
                && let Some((_, v, Scalar::Float)) = outs.iter().find(|o| o.0 == 0)
            {
                let a = f32::from_bits(v[3]).clamp(0.0, 1.0);
                let n = (a * samples.len() as f32).round() as usize;
                m &= (1u32 << n) - 1;
            }
            if let Some((v, inv)) = self.d.fixed.sample_coverage {
                let n = (v.clamp(0.0, 1.0) * samples.len() as f32).round() as usize;
                let cm = (1u32 << n) - 1;
                m &= if inv { !cm } else { cm };
            }
            let fd =
                depth_cell.map(|c| f32::from_bits(self.fe.regs[c as usize][l]).clamp(0.0, 1.0));
            for (si, (sx, sy)) in samples.iter().enumerate() {
                if m & (1 << si) == 0 {
                    continue;
                }
                let z = match (fd, point) {
                    (Some(z), _) => z,
                    (None, Some(_)) => w[0].z,
                    (None, None) => {
                        let b = bary(
                            f64::from(px) + f64::from(*sx),
                            f64::from(py) + f64::from(*sy),
                        );
                        zc(&b) + zoff
                    }
                };
                self.sample_ops(px, py, si, z.clamp(0.0, 1.0), front, &outs);
            }
        }
    }

    fn sample_ops(
        &mut self,
        px: i32,
        py: i32,
        si: usize,
        z: f32,
        front: bool,
        outs: &[(u32, [u32; 4], Scalar)],
    ) {
        let f = self.d.fixed;
        let s = self.t.samples as usize;
        let idx = (py as usize * self.t.width as usize + px as usize) * s + si;
        // Stencil.
        let face = if front { 0 } else { 1 };
        let mut stencil_pass = true;
        if let (Some(st), Some(buf)) = (f.stencil, self.t.stencil.as_deref_mut()) {
            let sf = st[face];
            let cur = u32::from(buf[idx]);
            let r = (sf.reference.clamp(0, 255) as u32) & sf.value_mask;
            let v = cur & sf.value_mask;
            stencil_pass = compare(sf.func, r as f32, v as f32);
            if !stencil_pass {
                buf[idx] = stencil_op(sf.fail, cur, sf.reference, sf.write_mask);
                return;
            }
        }
        let mut depth_pass = true;
        if let (Some(func), Some((buf, bits))) = (f.depth_func, self.t.depth.as_mut()) {
            let zq = quantize_depth(z, *bits);
            depth_pass = compare(func, zq, buf[idx]);
            if depth_pass && f.depth_mask {
                buf[idx] = zq;
            }
        }
        if let (Some(st), Some(buf)) = (f.stencil, self.t.stencil.as_deref_mut()) {
            let sf = st[face];
            let cur = u32::from(buf[idx]);
            let op = if depth_pass { sf.zpass } else { sf.zfail };
            buf[idx] = stencil_op(op, cur, sf.reference, sf.write_mask);
        }
        if !depth_pass || !stencil_pass {
            return;
        }
        for (loc, v, k) in outs {
            let Some(Some(img)) = self.t.colors.get_mut(*loc as usize) else {
                continue;
            };
            let ii = img.index(px as u32, py as u32, 0) + si;
            if matches!(img.format.kind, Kind::Int | Kind::Uint) {
                img.write_bits(ii, *v, f.color_mask);
                continue;
            }
            let _ = k;
            let mut src: [f32; 4] = v.map(f32::from_bits);
            let unorm = img.format.kind == Kind::Unorm;
            if unorm {
                for c in &mut src {
                    *c = if c.is_nan() { 0.0 } else { c.clamp(0.0, 1.0) };
                }
            }
            if let Some(b) = f.blend {
                let mut dst = img.read(ii);
                if img.format.srgb {
                    for c in dst.iter_mut().take(3) {
                        *c = srgb_to_linear(*c);
                    }
                }
                if img.format.channels < 4 {
                    dst[3] = 1.0;
                }
                let mut cc = b.color;
                if unorm {
                    for c in &mut cc {
                        *c = c.clamp(0.0, 1.0);
                    }
                }
                src = blend(&b, src, dst, cc);
            }
            if img.format.srgb {
                for c in src.iter_mut().take(3) {
                    *c = linear_to_srgb(*c);
                }
            }
            img.write(ii, src, f.color_mask);
        }
    }

    fn line(&mut self, a: &VOut, b: &VOut) {
        // Clip in clip space (parametric).
        let mut t0 = 0.0f32;
        let mut t1 = 1.0f32;
        for k in 0..7 {
            let d = |p: &[f32; 4]| -> f32 {
                match k {
                    0 => p[3] + p[0],
                    1 => p[3] - p[0],
                    2 => p[3] + p[1],
                    3 => p[3] - p[1],
                    4 => p[3] + p[2],
                    5 => p[3] - p[2],
                    _ => p[3] - 1e-6,
                }
            };
            let (da, db) = (d(&a.pos), d(&b.pos));
            if da < 0.0 && db < 0.0 {
                return;
            }
            if da < 0.0 {
                t0 = t0.max(da / (da - db));
            } else if db < 0.0 {
                t1 = t1.min(da / (da - db));
            }
        }
        if t0 >= t1 {
            return;
        }
        let at = |t: f32| VOut {
            pos: std::array::from_fn(|j| a.pos[j] + (b.pos[j] - a.pos[j]) * t),
            psize: a.psize,
            var: a
                .var
                .iter()
                .zip(&b.var)
                .map(|(x, y)| x + (y - x) * t)
                .collect(),
        };
        let (ca, cb) = (at(t0), at(t1));
        let wa = self.to_window(&ca);
        let wb = self.to_window(&cb);
        let provoking = b.var.clone();
        let (dx, dy) = (wb.x - wa.x, wb.y - wa.y);
        let xmajor = dx.abs() >= dy.abs();
        let len = if xmajor { dx.abs() } else { dy.abs() };
        if len == 0.0 {
            return;
        }
        let (bx0, by0, bx1, by1) = self.bounds();
        // Pixels whose center along the major axis lies in [start, end).
        let (s0, s1) = if xmajor { (wa.x, wb.x) } else { (wa.y, wb.y) };
        let (lo, hi) = if s0 <= s1 { (s0, s1) } else { (s1, s0) };
        let first = (lo - 0.5).ceil() as i32;
        let last = (hi - 0.5).ceil() as i32;
        let interps: Vec<Interp> = self
            .d
            .prog
            .varyings
            .iter()
            .flat_map(|v| std::iter::repeat_n(v.interp, v.cells as usize))
            .collect();
        let full = if self.t.samples >= 4 { 0xF } else { 1 };
        let tri = [wa.clone(), wb.clone(), wb.clone()];
        let mut pixels: Vec<(i32, i32, f32)> = Vec::new();
        for m in first..last {
            let c = f64::from(m) + 0.5;
            let t = ((c - s0) / (s1 - s0)).clamp(0.0, 1.0);
            let minor = if xmajor { wa.y + dy * t } else { wa.x + dx * t };
            let mi = minor.floor() as i32;
            let (px, py) = if xmajor { (m, mi) } else { (mi, m) };
            if px < bx0 || px >= bx1 || py < by0 || py >= by1 {
                continue;
            }
            pixels.push((px, py, t as f32));
        }
        // Shade pixel by pixel through lane 0 of a tile (lines are thin).
        for (px, py, t) in pixels {
            let tx = px & !3;
            let ty = py & !3;
            let mut cov = [0u32; W];
            let lane = pixel_lane(px - tx, py - ty);
            cov[lane] = full;
            let bary = |x: f64, y: f64| -> [f32; 3] {
                // Project onto the segment.
                let tt = if xmajor {
                    (x - wa.x) / dx
                } else {
                    (y - wa.y) / dy
                };
                let tt = if (x - f64::from(px) - 0.5).abs() < 1e-9
                    && (y - f64::from(py) - 0.5).abs() < 1e-9
                {
                    f64::from(t)
                } else {
                    tt
                };
                [(1.0 - tt) as f32, tt as f32, 0.0]
            };
            self.shade_tile(
                tx, ty, &cov, bary, &tri, &provoking, &interps, true, 0.0, None,
            );
            if self.fe.aborted {
                return;
            }
        }
    }

    fn point(&mut self, v: &VOut) {
        let p = v.pos;
        if p[3] <= 0.0 || (0..3).any(|k| p[k] < -p[3] || p[k] > p[3]) {
            return;
        }
        let wv = self.to_window(v);
        let size = f64::from(v.psize.clamp(1.0, 1024.0));
        let (cx, cy) = (wv.x, wv.y);
        let (bx0, by0, bx1, by1) = self.bounds();
        let x0 = ((cx - size / 2.0).floor() as i32).max(bx0);
        let x1 = ((cx + size / 2.0).ceil() as i32).min(bx1);
        let y0 = ((cy - size / 2.0).floor() as i32).max(by0);
        let y1 = ((cy + size / 2.0).ceil() as i32).min(by1);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let samples = sample_pos(self.t.samples);
        let interps: Vec<Interp> = vec![Interp::Flat; v.var.len()];
        let tri = [wv.clone(), wv.clone(), wv.clone()];
        let provoking = v.var.clone();
        let mut ty = y0 & !3;
        while ty < y1 {
            let mut tx = x0 & !3;
            while tx < x1 {
                let mut cov = [0u32; W];
                let mut any = false;
                for (l, c) in cov.iter_mut().enumerate() {
                    let (px, py) = lane_pixel(tx, ty, l);
                    if px < x0 || px >= x1 || py < y0 || py >= y1 {
                        continue;
                    }
                    for (si, (sx, sy)) in samples.iter().enumerate() {
                        let (x, y) = (
                            f64::from(px) + f64::from(*sx),
                            f64::from(py) + f64::from(*sy),
                        );
                        if x >= cx - size / 2.0
                            && x < cx + size / 2.0
                            && y >= cy - size / 2.0
                            && y < cy + size / 2.0
                        {
                            *c |= 1 << si;
                        }
                    }
                    any |= *c != 0;
                }
                if any {
                    self.shade_tile(
                        tx,
                        ty,
                        &cov,
                        |_, _| [1.0, 0.0, 0.0],
                        &tri,
                        &provoking,
                        &interps,
                        true,
                        0.0,
                        Some((cx, cy, size)),
                    );
                    if self.fe.aborted {
                        return;
                    }
                }
                tx += 4;
            }
            ty += 4;
        }
    }
}

/// Pixel of a lane in a 4x4 tile made of four 2x2 quads.
fn lane_pixel(tx: i32, ty: i32, l: usize) -> (i32, i32) {
    let q = l >> 2;
    let i = l & 3;
    (
        tx + ((q & 1) * 2 + (i & 1)) as i32,
        ty + ((q >> 1) * 2 + (i >> 1)) as i32,
    )
}

fn pixel_lane(dx: i32, dy: i32) -> usize {
    let (qx, qy) = ((dx / 2) as usize, (dy / 2) as usize);
    let (ix, iy) = ((dx % 2) as usize, (dy % 2) as usize);
    ((qy * 2 + qx) << 2) | (iy * 2 + ix)
}

fn compare(func: u32, a: f32, b: f32) -> bool {
    match func {
        0x0200 => false,
        0x0201 => a < b,
        0x0202 => a == b,
        0x0203 => a <= b,
        0x0204 => a > b,
        0x0205 => a != b,
        0x0206 => a >= b,
        _ => true,
    }
}

fn stencil_op(op: u32, cur: u32, reference: i32, write_mask: u32) -> u8 {
    let v = match op {
        0 => 0,
        0x1E01 => reference.clamp(0, 255) as u32,
        0x1E02 => (cur + 1).min(255),
        0x1E03 => cur.saturating_sub(1),
        0x150A => !cur & 0xFF,
        0x8507 => (cur + 1) & 0xFF,
        0x8508 => cur.wrapping_sub(1) & 0xFF,
        _ => cur,
    };
    ((cur & !write_mask) | (v & write_mask)) as u8
}

fn blend(b: &Blend, s: [f32; 4], d: [f32; 4], c: [f32; 4]) -> [f32; 4] {
    let factor = |f: u32, k: usize| -> f32 {
        match f {
            0 => 0.0,
            1 => 1.0,
            0x0300 => s[k],
            0x0301 => 1.0 - s[k],
            0x0306 => d[k],
            0x0307 => 1.0 - d[k],
            0x0302 => s[3],
            0x0303 => 1.0 - s[3],
            0x0304 => d[3],
            0x0305 => 1.0 - d[3],
            0x8001 => c[k],
            0x8002 => 1.0 - c[k],
            0x8003 => c[3],
            0x8004 => 1.0 - c[3],
            0x0308 => {
                if k == 3 {
                    1.0
                } else {
                    s[3].min(1.0 - d[3])
                }
            }
            _ => 1.0,
        }
    };
    std::array::from_fn(|k| {
        let a = usize::from(k == 3);
        let (sf, df) = (factor(b.src[a], k), factor(b.dst[a], k));
        match b.eq[a] {
            0x800A => s[k] * sf - d[k] * df,
            0x800B => d[k] * df - s[k] * sf,
            0x8007 => s[k].min(d[k]),
            0x8008 => s[k].max(d[k]),
            _ => s[k] * sf + d[k] * df,
        }
    })
}
