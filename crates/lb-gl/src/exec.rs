//! Lane-parallel interpreter. Each operation runs on `W` lanes (16
//! vertices, or a 4x4 pixel tile made of four 2x2 quads for fragments).
//! Operations compute every lane; only stores to variables are masked,
//! so lanes diverge through masks as on a GPU (helper lanes included,
//! which gives derivatives).

use crate::glsl::types::{Dim, Scalar};
use crate::ir::*;
use crate::math::{self, f, fb};

pub const W: usize = 16;
pub type Mask = u32;
pub const ALL: Mask = (1 << W) - 1;

/// Texture access for `texture*` calls.
pub trait Textures {
    /// Samples one lane. `coord` holds s, t, r/layer; `lod` is final.
    #[allow(clippy::too_many_arguments)]
    fn sample(
        &self,
        unit: i32,
        dim: Dim,
        ret: Scalar,
        coord: [f32; 3],
        lod: f32,
        offset: [i32; 3],
        compare: Option<f32>,
    ) -> [u32; 4];
    fn fetch(&self, unit: i32, dim: Dim, ret: Scalar, coord: [i32; 3], lod: i32) -> [u32; 4];
    fn size(&self, unit: i32, dim: Dim, lod: i32) -> [i32; 3];
}

/// No textures bound (constant evaluation, tests).
pub struct NoTextures;
impl Textures for NoTextures {
    fn sample(
        &self,
        _: i32,
        _: Dim,
        _: Scalar,
        _: [f32; 3],
        _: f32,
        _: [i32; 3],
        _: Option<f32>,
    ) -> [u32; 4] {
        [0, 0, 0, 1f32.to_bits()]
    }
    fn fetch(&self, _: i32, _: Dim, _: Scalar, _: [i32; 3], _: i32) -> [u32; 4] {
        [0, 0, 0, 1f32.to_bits()]
    }
    fn size(&self, _: i32, _: Dim, _: i32) -> [i32; 3] {
        [0; 3]
    }
}

pub struct Exec<'a> {
    pub regs: Vec<[u32; W]>,
    pub tex: &'a dyn Textures,
    /// Remaining operation budget (a runaway shader stops the draw).
    pub budget: u64,
    pub aborted: bool,
    brk: Vec<Mask>,
    cont: Vec<Mask>,
    ret: Vec<Mask>,
    mask: Mask,
}

const LOOP_LIMIT: u32 = 1 << 20;

impl<'a> Exec<'a> {
    pub fn new(cells: u32, tex: &'a dyn Textures, budget: u64) -> Self {
        Exec {
            regs: vec![[0; W]; cells as usize],
            tex,
            budget,
            aborted: false,
            brk: Vec::new(),
            cont: Vec::new(),
            ret: Vec::new(),
            mask: ALL,
        }
    }

    /// Sets a cell to the same value in every lane.
    pub fn splat(&mut self, r: Reg, v: u32) {
        self.regs[r as usize] = [v; W];
    }

    /// Runs the code; returns the lanes that were not discarded.
    pub fn run(&mut self, code: &[Node], mask: Mask) -> Mask {
        self.brk.clear();
        self.cont.clear();
        self.ret.clear();
        let mut alive = mask;
        let out = self.block(code, mask, &mut alive);
        let _ = out;
        alive
    }

    fn block(&mut self, code: &[Node], mut m: Mask, alive: &mut Mask) -> Mask {
        for n in code {
            if m == 0 || self.aborted {
                return 0;
            }
            m = self.node(n, m, alive);
        }
        m
    }

    fn node(&mut self, n: &Node, m: Mask, alive: &mut Mask) -> Mask {
        match n {
            Node::Ops(ops) => {
                self.mask = m;
                let cost = ops.len() as u64;
                if self.budget < cost {
                    self.aborted = true;
                    return 0;
                }
                self.budget -= cost;
                for op in ops {
                    self.op(op);
                }
                m
            }
            Node::If { c, t, e } => {
                let cm = self.bits(*c) & m;
                let em = m & !cm;
                let a = if cm != 0 { self.block(t, cm, alive) } else { 0 };
                let b = if em != 0 {
                    if e.is_empty() {
                        em
                    } else {
                        self.block(e, em, alive)
                    }
                } else {
                    0
                };
                a | b
            }
            Node::Loop {
                pre,
                cond,
                body,
                cont,
                post_test,
            } => {
                self.brk.push(0);
                self.cont.push(0);
                let mut act = m;
                let mut out = 0;
                let mut iters = 0;
                loop {
                    if !*post_test {
                        act = self.block(pre, act, alive);
                        if let Some(c) = cond {
                            let cm = self.bits(*c) & act;
                            out |= act & !cm;
                            act = cm;
                        }
                    }
                    if act == 0 || self.aborted {
                        break;
                    }
                    *self.cont.last_mut().expect("loop") = 0;
                    act = self.block(body, act, alive);
                    act |= *self.cont.last().expect("loop");
                    act = self.block(cont, act, alive);
                    if *post_test && let Some(c) = cond {
                        let cm = self.bits(*c) & act;
                        out |= act & !cm;
                        act = cm;
                    }
                    iters += 1;
                    if iters >= LOOP_LIMIT {
                        self.aborted = true;
                        break;
                    }
                }
                self.cont.pop();
                let b = self.brk.pop().unwrap_or(0);
                (out | b) & *alive
            }
            Node::Breakable(body) => {
                self.brk.push(0);
                let r = self.block(body, m, alive);
                let b = self.brk.pop().unwrap_or(0);
                (r | b) & *alive
            }
            Node::Func(body) => {
                self.ret.push(0);
                let r = self.block(body, m, alive);
                let b = self.ret.pop().unwrap_or(0);
                (r | b) & *alive
            }
            Node::Break => {
                if let Some(b) = self.brk.last_mut() {
                    *b |= m;
                }
                0
            }
            Node::Continue => {
                if let Some(c) = self.cont.last_mut() {
                    *c |= m;
                }
                0
            }
            Node::Return => {
                if let Some(r) = self.ret.last_mut() {
                    *r |= m;
                }
                0
            }
            Node::Discard => {
                *alive &= !m;
                0
            }
        }
    }

    fn bits(&self, r: Reg) -> Mask {
        let v = &self.regs[r as usize];
        let mut m = 0;
        for (l, x) in v.iter().enumerate() {
            if *x != 0 {
                m |= 1 << l;
            }
        }
        m
    }

    #[inline]
    fn r(&self, r: Reg) -> [u32; W] {
        self.regs[r as usize]
    }

    fn op(&mut self, op: &Op) {
        macro_rules! lanes {
            ($d:expr, |$l:ident| $body:expr) => {{
                let mut out = [0u32; W];
                for $l in 0..W {
                    out[$l] = $body;
                }
                self.regs[$d as usize] = out;
            }};
        }
        match op {
            Op::Const { d, v } => self.regs[*d as usize] = [*v; W],
            Op::Mov { d, s } => self.regs[*d as usize] = self.regs[*s as usize],
            Op::MovM { d, s } => {
                let m = self.mask;
                if m == ALL {
                    self.regs[*d as usize] = self.regs[*s as usize];
                } else {
                    let src = self.r(*s);
                    let dst = &mut self.regs[*d as usize];
                    for l in 0..W {
                        if m & (1 << l) != 0 {
                            dst[l] = src[l];
                        }
                    }
                }
            }
            Op::F { op, d, a, b } => {
                let (a, b) = (self.r(*a), self.r(*b));
                match op {
                    FOp::Add => lanes!(*d, |l| fb(f(a[l]) + f(b[l]))),
                    FOp::Sub => lanes!(*d, |l| fb(f(a[l]) - f(b[l]))),
                    FOp::Mul => lanes!(*d, |l| fb(f(a[l]) * f(b[l]))),
                    FOp::Div => lanes!(*d, |l| fb(f(a[l]) / f(b[l]))),
                    FOp::Min => lanes!(*d, |l| fb(f(a[l]).min(f(b[l])))),
                    FOp::Max => lanes!(*d, |l| fb(f(a[l]).max(f(b[l])))),
                    FOp::Pow => lanes!(*d, |l| fb(f(a[l]).powf(f(b[l])))),
                    FOp::Atan2 => lanes!(*d, |l| fb(f(a[l]).atan2(f(b[l])))),
                    FOp::Mod => lanes!(*d, |l| fb(math::fmod(f(a[l]), f(b[l])))),
                    FOp::Step => lanes!(*d, |l| fb(if f(b[l]) < f(a[l]) { 0.0 } else { 1.0 })),
                }
            }
            Op::I { op, u, d, a, b } => {
                let (a, b) = (self.r(*a), self.r(*b));
                let bop = match op {
                    IOp::Add => crate::glsl::ast::BOp::Add,
                    IOp::Sub => crate::glsl::ast::BOp::Sub,
                    IOp::Mul => crate::glsl::ast::BOp::Mul,
                    IOp::Div => crate::glsl::ast::BOp::Div,
                    IOp::Mod => crate::glsl::ast::BOp::Mod,
                    IOp::Shl => crate::glsl::ast::BOp::Shl,
                    IOp::Shr => crate::glsl::ast::BOp::Shr,
                    IOp::And => crate::glsl::ast::BOp::And,
                    IOp::Or => crate::glsl::ast::BOp::Or,
                    IOp::Xor => crate::glsl::ast::BOp::Xor,
                    IOp::Min | IOp::Max => {
                        let min = *op == IOp::Min;
                        if *u {
                            lanes!(*d, |l| if (a[l] < b[l]) == min { a[l] } else { b[l] });
                        } else {
                            lanes!(*d, |l| if ((a[l] as i32) < (b[l] as i32)) == min {
                                a[l]
                            } else {
                                b[l]
                            });
                        }
                        return;
                    }
                };
                match bop {
                    crate::glsl::ast::BOp::Add => lanes!(*d, |l| a[l].wrapping_add(b[l])),
                    crate::glsl::ast::BOp::Sub => lanes!(*d, |l| a[l].wrapping_sub(b[l])),
                    crate::glsl::ast::BOp::Mul => lanes!(*d, |l| a[l].wrapping_mul(b[l])),
                    crate::glsl::ast::BOp::And => lanes!(*d, |l| a[l] & b[l]),
                    crate::glsl::ast::BOp::Or => lanes!(*d, |l| a[l] | b[l]),
                    crate::glsl::ast::BOp::Xor => lanes!(*d, |l| a[l] ^ b[l]),
                    _ => {
                        let s = if *u { Scalar::Uint } else { Scalar::Int };
                        lanes!(*d, |l| math::bin(bop, s, a[l], b[l]))
                    }
                }
            }
            Op::Cmp { k, op, d, a, b } => {
                let (a, b) = (self.r(*a), self.r(*b));
                match k {
                    Scalar::Float => match op {
                        CmpOp::Lt => lanes!(*d, |l| u32::from(f(a[l]) < f(b[l]))),
                        CmpOp::Gt => lanes!(*d, |l| u32::from(f(a[l]) > f(b[l]))),
                        CmpOp::Le => lanes!(*d, |l| u32::from(f(a[l]) <= f(b[l]))),
                        CmpOp::Ge => lanes!(*d, |l| u32::from(f(a[l]) >= f(b[l]))),
                        CmpOp::Eq => lanes!(*d, |l| u32::from(f(a[l]) == f(b[l]))),
                        CmpOp::Ne => lanes!(*d, |l| u32::from(f(a[l]) != f(b[l]))),
                    },
                    Scalar::Int => match op {
                        CmpOp::Lt => lanes!(*d, |l| u32::from((a[l] as i32) < (b[l] as i32))),
                        CmpOp::Gt => lanes!(*d, |l| u32::from((a[l] as i32) > (b[l] as i32))),
                        CmpOp::Le => lanes!(*d, |l| u32::from((a[l] as i32) <= (b[l] as i32))),
                        CmpOp::Ge => lanes!(*d, |l| u32::from((a[l] as i32) >= (b[l] as i32))),
                        CmpOp::Eq => lanes!(*d, |l| u32::from(a[l] == b[l])),
                        CmpOp::Ne => lanes!(*d, |l| u32::from(a[l] != b[l])),
                    },
                    _ => match op {
                        CmpOp::Lt => lanes!(*d, |l| u32::from(a[l] < b[l])),
                        CmpOp::Gt => lanes!(*d, |l| u32::from(a[l] > b[l])),
                        CmpOp::Le => lanes!(*d, |l| u32::from(a[l] <= b[l])),
                        CmpOp::Ge => lanes!(*d, |l| u32::from(a[l] >= b[l])),
                        CmpOp::Eq => lanes!(*d, |l| u32::from(a[l] == b[l])),
                        CmpOp::Ne => lanes!(*d, |l| u32::from(a[l] != b[l])),
                    },
                }
            }
            Op::FU { op, d, a } => {
                let a = self.r(*a);
                match op {
                    FUn::Neg => lanes!(*d, |l| a[l] ^ 0x8000_0000),
                    FUn::Abs => lanes!(*d, |l| a[l] & 0x7fff_ffff),
                    FUn::IsNan => lanes!(*d, |l| u32::from(f(a[l]).is_nan())),
                    FUn::IsInf => lanes!(*d, |l| u32::from(f(a[l]).is_infinite())),
                    FUn::Floor => lanes!(*d, |l| fb(f(a[l]).floor())),
                    FUn::Fract => lanes!(*d, |l| fb(f(a[l]) - f(a[l]).floor())),
                    FUn::Sqrt => lanes!(*d, |l| fb(f(a[l]).sqrt())),
                    FUn::InvSqrt => lanes!(*d, |l| fb(1.0 / f(a[l]).sqrt())),
                    _ => {
                        let bi = fun_bi(*op);
                        lanes!(*d, |l| fb(math::unary_f(bi, f(a[l]))))
                    }
                }
            }
            Op::IU { op, d, a } => {
                let a = self.r(*a);
                match op {
                    IUn::Neg => lanes!(*d, |l| (a[l] as i32).wrapping_neg() as u32),
                    IUn::Abs => lanes!(*d, |l| (a[l] as i32).wrapping_abs() as u32),
                    IUn::Sign => lanes!(*d, |l| (a[l] as i32).signum() as u32),
                    IUn::Not => lanes!(*d, |l| !a[l]),
                    IUn::LNot => lanes!(*d, |l| u32::from(a[l] == 0)),
                }
            }
            Op::Cvt { from, to, d, a } => {
                let a = self.r(*a);
                lanes!(*d, |l| math::conv(*from, *to, a[l]))
            }
            Op::Fma { d, a, b, c } => {
                let (a, b, c) = (self.r(*a), self.r(*b), self.r(*c));
                lanes!(*d, |l| fb(f(a[l]) * f(b[l]) + f(c[l])))
            }
            Op::Mix { d, a, b, t } => {
                let (a, b, t) = (self.r(*a), self.r(*b), self.r(*t));
                lanes!(*d, |l| fb(f(a[l]) * (1.0 - f(t[l])) + f(b[l]) * f(t[l])))
            }
            Op::Clamp { k, d, x, lo, hi } => {
                let (x, lo, hi) = (self.r(*x), self.r(*lo), self.r(*hi));
                match k {
                    Scalar::Float => lanes!(*d, |l| fb(math::fclamp(f(x[l]), f(lo[l]), f(hi[l])))),
                    Scalar::Int => {
                        lanes!(*d, |l| (x[l] as i32).max(lo[l] as i32).min(hi[l] as i32)
                            as u32)
                    }
                    _ => lanes!(*d, |l| x[l].max(lo[l]).min(hi[l])),
                }
            }
            Op::Smooth { d, e0, e1, x } => {
                let (e0, e1, x) = (self.r(*e0), self.r(*e1), self.r(*x));
                lanes!(*d, |l| fb(math::smoothstep(f(e0[l]), f(e1[l]), f(x[l]))))
            }
            Op::Select { d, c, a, b } => {
                let (c, a, b) = (self.r(*c), self.r(*a), self.r(*b));
                lanes!(*d, |l| if c[l] != 0 { a[l] } else { b[l] })
            }
            Op::Index {
                d,
                idx,
                n,
                stride,
                add,
                u,
            } => {
                let i = self.r(*idx);
                let ad = add.map(|r| self.r(r));
                let n = *n as i64;
                lanes!(*d, |l| {
                    let raw = if *u {
                        i64::from(i[l])
                    } else {
                        i64::from(i[l] as i32)
                    };
                    let k = raw.clamp(0, n - 1) as u32 * stride;
                    k + ad.map_or(0, |a| a[l])
                })
            }
            Op::LoadDyn { d, base, off } => {
                let o = self.r(*off);
                let len = self.regs.len();
                let out: [u32; W] = std::array::from_fn(|l| {
                    let r = (*base as usize + o[l] as usize).min(len - 1);
                    self.regs[r][l]
                });
                self.regs[*d as usize] = out;
            }
            Op::StoreDyn { base, off, s } => {
                let o = self.r(*off);
                let src = self.r(*s);
                let len = self.regs.len();
                for l in 0..W {
                    if self.mask & (1 << l) != 0 {
                        let r = (*base as usize + o[l] as usize).min(len - 1);
                        self.regs[r][l] = src[l];
                    }
                }
            }
            Op::Deriv { k, d, s } => {
                let v = self.r(*s);
                lanes!(*d, |l| {
                    let q = l & !3;
                    let i = l & 3;
                    let dx = f(v[q | (i | 1)]) - f(v[q | (i & !1)]);
                    let dy = f(v[q | (i | 2)]) - f(v[q | (i & !2)]);
                    fb(match k {
                        DerivKind::X => dx,
                        DerivKind::Y => dy,
                        DerivKind::Width => dx.abs() + dy.abs(),
                    })
                })
            }
            Op::Pack { k, d, a, b } => {
                let (a, b) = (self.r(*a), self.r(*b));
                lanes!(*d, |l| {
                    let p = |x: f32| -> u32 {
                        match k {
                            PackKind::Snorm => u32::from(
                                (x.clamp(-1.0, 1.0) * 32767.0).round_ties_even() as i16 as u16,
                            ),
                            PackKind::Unorm => {
                                u32::from((x.clamp(0.0, 1.0) * 65535.0).round_ties_even() as u16)
                            }
                            PackKind::Half => u32::from(math::f32_to_f16(x)),
                        }
                    };
                    p(f(a[l])) | (p(f(b[l])) << 16)
                })
            }
            Op::Unpack { k, d0, d1, s } => {
                let s = self.r(*s);
                let un = |x: u32| -> f32 {
                    let h = x as u16;
                    match k {
                        PackKind::Snorm => (f32::from(h as i16) / 32767.0).clamp(-1.0, 1.0),
                        PackKind::Unorm => f32::from(h) / 65535.0,
                        PackKind::Half => math::f16_to_f32(h),
                    }
                };
                lanes!(*d0, |l| fb(un(s[l])));
                lanes!(*d1, |l| fb(un(s[l] >> 16)));
            }
            Op::Mat { inv, n, d, s } => {
                let n = *n as usize;
                let cnt = if *inv { n * n } else { 1 };
                let mut outs = vec![[0u32; W]; cnt];
                for l in 0..W {
                    let m: Vec<f32> = (0..n * n)
                        .map(|i| f(self.regs[*s as usize + i][l]))
                        .collect();
                    if *inv {
                        for (i, v) in math::inverse(&m, n).into_iter().enumerate() {
                            outs[i][l] = fb(v);
                        }
                    } else {
                        outs[0][l] = fb(math::determinant(&m, n));
                    }
                }
                for (i, o) in outs.into_iter().enumerate() {
                    self.regs[*d as usize + i] = o;
                }
            }
            Op::Tex(t) => self.texture(t),
            Op::TexFetch {
                d,
                unit,
                dim,
                ret,
                coord,
                lod,
                offset,
            } => {
                let u = self.r(*unit);
                let c: Vec<[u32; W]> = coord.iter().map(|r| self.r(*r)).collect();
                let lod = self.r(*lod);
                let off: Vec<[u32; W]> = offset
                    .as_ref()
                    .map(|o| o.iter().map(|r| self.r(*r)).collect())
                    .unwrap_or_default();
                let mut out = [[0u32; W]; 4];
                for l in 0..W {
                    if self.mask & (1 << l) == 0 {
                        continue;
                    }
                    let mut p = [0i32; 3];
                    for (k, cc) in c.iter().enumerate() {
                        p[k] = cc[l] as i32 + off.get(k).map_or(0, |o| o[l] as i32);
                    }
                    let v = self.tex.fetch(u[l] as i32, *dim, *ret, p, lod[l] as i32);
                    for k in 0..4 {
                        out[k][l] = v[k];
                    }
                }
                for (k, o) in out.into_iter().enumerate() {
                    self.regs[*d as usize + k] = o;
                }
            }
            Op::TexSize { d, unit, dim, lod } => {
                let u = self.r(*unit);
                let lod = self.r(*lod);
                let mut out = [[0u32; W]; 3];
                for l in 0..W {
                    let s = self.tex.size(u[l] as i32, *dim, lod[l] as i32);
                    for k in 0..3 {
                        out[k][l] = s[k] as u32;
                    }
                }
                for (k, o) in out.into_iter().enumerate() {
                    self.regs[*d as usize + k] = o;
                }
            }
        }
    }

    fn texture(&mut self, t: &TexArgs) {
        let unit = self.r(t.unit);
        let c: Vec<[u32; W]> = t.coord.iter().map(|r| self.r(*r)).collect();
        let cmp = t.compare.map(|r| self.r(r));
        let bias = t.bias.map(|r| self.r(r));
        let lod = t.lod.map(|r| self.r(r));
        let off: Vec<[u32; W]> = t
            .offset
            .as_ref()
            .map(|o| o.iter().map(|r| self.r(*r)).collect())
            .unwrap_or_default();
        let grad: Option<(Vec<[u32; W]>, Vec<[u32; W]>)> = t.grad.as_ref().map(|(x, y)| {
            (
                x.iter().map(|r| self.r(*r)).collect(),
                y.iter().map(|r| self.r(*r)).collect(),
            )
        });
        let n = if t.shadow { 1 } else { 4 };
        let mut out = [[0u32; W]; 4];
        // Texel-space scale of the base level for LOD computation.
        let mut scale_cache: Option<(i32, [f32; 3])> = None;
        for l in 0..W {
            if self.mask & (1 << l) == 0 {
                continue;
            }
            let u = unit[l] as i32;
            let mut p = [0f32; 3];
            for (k, cc) in c.iter().enumerate().take(3) {
                p[k] = f(cc[l]);
            }
            let scale = match scale_cache {
                Some((cu, s)) if cu == u => s,
                _ => {
                    let s = self.tex.size(u, t.dim, 0);
                    let s = [s[0] as f32, s[1] as f32, s[2] as f32];
                    scale_cache = Some((u, s));
                    s
                }
            };
            let dims = match t.dim {
                Dim::D2 | Dim::D2Array => 2,
                Dim::D3 => 3,
                Dim::Cube => 3,
            };
            let rho = |dx: [f32; 3], dy: [f32; 3]| -> f32 {
                let (mut sx, mut sy) = (0.0, 0.0);
                let cube_scale = if t.dim == Dim::Cube {
                    let m = p[0].abs().max(p[1].abs()).max(p[2].abs()).max(1e-20);
                    scale[0] / (2.0 * m)
                } else {
                    1.0
                };
                for k in 0..dims {
                    let s = if t.dim == Dim::Cube {
                        cube_scale
                    } else {
                        scale[k]
                    };
                    sx += (dx[k] * s) * (dx[k] * s);
                    sy += (dy[k] * s) * (dy[k] * s);
                }
                sx.sqrt().max(sy.sqrt())
            };
            let level = if let Some(lv) = &lod {
                f(lv[l])
            } else if let Some((gx, gy)) = &grad {
                let mut dx = [0f32; 3];
                let mut dy = [0f32; 3];
                for k in 0..gx.len().min(3) {
                    dx[k] = f(gx[k][l]);
                    dy[k] = f(gy[k][l]);
                }
                rho(dx, dy).log2()
            } else if t.implicit {
                let q = l & !3;
                let i = l & 3;
                let mut dx = [0f32; 3];
                let mut dy = [0f32; 3];
                for (k, cc) in c.iter().enumerate().take(dims) {
                    dx[k] = f(cc[q | (i | 1)]) - f(cc[q | (i & !1)]);
                    dy[k] = f(cc[q | (i | 2)]) - f(cc[q | (i & !2)]);
                }
                rho(dx, dy).log2() + bias.map_or(0.0, |b| f(b[l]))
            } else {
                0.0
            };
            let mut o = [0i32; 3];
            for (k, oo) in off.iter().enumerate().take(3) {
                o[k] = oo[l] as i32;
            }
            let v = self
                .tex
                .sample(u, t.dim, t.ret, p, level, o, cmp.map(|c| f(c[l])));
            for k in 0..n {
                out[k][l] = v[k];
            }
        }
        for (k, o) in out.into_iter().enumerate().take(n) {
            self.regs[t.d as usize + k] = o;
        }
    }
}

fn fun_bi(op: FUn) -> crate::glsl::builtins::Bi {
    use crate::glsl::builtins::Bi;
    match op {
        FUn::Sign => Bi::Sign,
        FUn::Ceil => Bi::Ceil,
        FUn::Trunc => Bi::Trunc,
        FUn::Round => Bi::RoundEven,
        FUn::Exp => Bi::Exp,
        FUn::Log => Bi::Log,
        FUn::Exp2 => Bi::Exp2,
        FUn::Log2 => Bi::Log2,
        FUn::Sin => Bi::Sin,
        FUn::Cos => Bi::Cos,
        FUn::Tan => Bi::Tan,
        FUn::Asin => Bi::Asin,
        FUn::Acos => Bi::Acos,
        FUn::Atan => Bi::Atan,
        FUn::Sinh => Bi::Sinh,
        FUn::Cosh => Bi::Cosh,
        FUn::Tanh => Bi::Tanh,
        FUn::Asinh => Bi::Asinh,
        FUn::Acosh => Bi::Acosh,
        FUn::Atanh => Bi::Atanh,
        FUn::Radians => Bi::Radians,
        FUn::Degrees => Bi::Degrees,
        _ => Bi::Abs,
    }
}
