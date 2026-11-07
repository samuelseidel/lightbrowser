//! Typed tree → interpreter IR. User functions are inlined (GLSL ES has
//! no recursion); values are lists of cells, so swizzles, constant
//! indexing and field selection cost nothing.

use std::collections::HashMap;

use super::ast::*;
use super::builtins::{Bi, Lod};
use super::fold::{self, CtorSrc};
use super::parse::Unit;
use super::types::*;
use super::{Log, Stage};
use crate::ir::*;

/// Code size limit (operations, after inlining).
const MAX_OPS: usize = 2_000_000;

struct Place {
    base: Reg,
    dynoff: Option<Reg>,
    cells: Vec<u32>,
}

struct Gen<'a> {
    u: &'a Unit,
    next: Reg,
    mark: Reg,
    high: Reg,
    var_regs: Vec<Option<Reg>>,
    used: Vec<bool>,
    blocks: Vec<Vec<Node>>,
    rets: Vec<Vec<Reg>>,
    ops: usize,
    too_big: bool,
    stage: Stage,
}

fn kind(t: &Type) -> Scalar {
    t.scalar_kind().unwrap_or(Scalar::Int)
}

/// Scalar kind of every cell of a type.
fn cell_kinds(t: &Type, structs: &[StructDef], out: &mut Vec<Scalar>) {
    let n = t.array.max(1);
    for _ in 0..n {
        match t.base {
            Base::Struct(s) => {
                for (_, ft, _) in &structs[s as usize].fields {
                    cell_kinds(ft, structs, out);
                }
            }
            Base::S(k) => out.extend(std::iter::repeat_n(k, t.comps() as usize)),
            _ => out.extend(std::iter::repeat_n(Scalar::Int, t.comps() as usize)),
        }
    }
}

fn pure(e: &Expr) -> bool {
    match &e.k {
        EK::Var(_) | EK::Const => true,
        EK::Assign(..) | EK::IncDec { .. } | EK::Call(..) => false,
        EK::Bin(_, a, b) | EK::Logic(_, a, b) | EK::Comma(a, b) | EK::Index(a, b) => {
            pure(a) && pure(b)
        }
        EK::Un(_, a) | EK::Swz(a, _) | EK::Field(a, _) => pure(a),
        EK::Bi(bi, args) => *bi != Bi::Modf && args.iter().all(pure),
        EK::Ctor(args) => args.iter().all(pure),
        EK::Cond(a, b, c) => pure(a) && pure(b) && pure(c),
    }
}

impl Gen<'_> {
    fn alloc(&mut self, n: u32) -> Reg {
        let r = self.next;
        self.next += n;
        self.high = self.high.max(self.next);
        r
    }
    fn emit(&mut self, op: Op) {
        self.ops += 1;
        if self.ops > MAX_OPS {
            self.too_big = true;
            return;
        }
        let b = self.blocks.last_mut().expect("block");
        match b.last_mut() {
            Some(Node::Ops(list)) => list.push(op),
            _ => b.push(Node::Ops(vec![op])),
        }
    }
    fn node(&mut self, n: Node) {
        self.blocks.last_mut().expect("block").push(n);
    }
    fn sub(&mut self, f: impl FnOnce(&mut Self)) -> Vec<Node> {
        self.blocks.push(Vec::new());
        f(self);
        self.blocks.pop().expect("block")
    }
    fn konst(&mut self, v: u32) -> Reg {
        let d = self.alloc(1);
        self.emit(Op::Const { d, v });
        d
    }
    fn consts(&mut self, v: &[u32]) -> Vec<Reg> {
        v.iter().map(|x| self.konst(*x)).collect()
    }
    fn copy(&mut self, v: &[Reg]) -> Vec<Reg> {
        let d = self.alloc(v.len() as u32);
        for (i, s) in v.iter().enumerate() {
            self.emit(Op::Mov {
                d: d + i as u32,
                s: *s,
            });
        }
        (d..d + v.len() as u32).collect()
    }
    fn contiguous(v: &[Reg]) -> bool {
        v.windows(2).all(|w| w[1] == w[0] + 1)
    }

    fn var_base(&mut self, v: VarId) -> Reg {
        self.used[v as usize] = true;
        match self.var_regs[v as usize] {
            Some(r) => r,
            None => {
                // A global declared after use cannot happen; allocate.
                let n = self.u.vars[v as usize].ty.cells(&self.u.structs);
                let r = self.alloc(n);
                self.var_regs[v as usize] = Some(r);
                r
            }
        }
    }

    // -----------------------------------------------------------------
    // Places
    // -----------------------------------------------------------------

    fn place(&mut self, e: &Expr) -> Option<Place> {
        match &e.k {
            EK::Var(v) => {
                if self.u.vars[*v as usize].storage == Storage::Const && e.cv.is_some() {
                    return None;
                }
                let base = self.var_base(*v);
                let n = e.ty.cells(&self.u.structs);
                Some(Place {
                    base,
                    dynoff: None,
                    cells: (0..n).collect(),
                })
            }
            EK::Field(b, i) => {
                let p = self.place(b)?;
                let Base::Struct(s) = b.ty.base else {
                    return None;
                };
                let def = &self.u.structs[s as usize];
                let off: u32 = def.fields[..*i as usize]
                    .iter()
                    .map(|f| f.1.cells(&self.u.structs))
                    .sum();
                let n = e.ty.cells(&self.u.structs);
                Some(Place {
                    base: p.base,
                    dynoff: p.dynoff,
                    cells: p.cells[off as usize..(off + n) as usize].to_vec(),
                })
            }
            EK::Swz(b, idx) => {
                let p = self.place(b)?;
                let n = e.ty.rows as usize;
                Some(Place {
                    base: p.base,
                    dynoff: p.dynoff,
                    cells: idx[..n].iter().map(|k| p.cells[*k as usize]).collect(),
                })
            }
            EK::Index(b, i) => {
                let p = self.place(b)?;
                let (stride, count) = if b.ty.is_array() {
                    (b.ty.elem().cells(&self.u.structs), b.ty.array)
                } else if b.ty.is_matrix() {
                    (u32::from(b.ty.rows), u32::from(b.ty.cols))
                } else {
                    (1, u32::from(b.ty.rows))
                };
                if let Some(cv) = &i.cv {
                    let k = (cv[0] as i32).clamp(0, count as i32 - 1) as u32;
                    let s = (k * stride) as usize;
                    return Some(Place {
                        base: p.base,
                        dynoff: p.dynoff,
                        cells: p.cells[s..s + stride as usize].to_vec(),
                    });
                }
                // Dynamic: needs a contiguous element layout.
                let first = &p.cells[..stride as usize];
                let regular = (0..count as usize).all(|k| {
                    p.cells[k * stride as usize..(k + 1) * stride as usize]
                        .iter()
                        .zip(first)
                        .all(|(a, b)| *a == *b + k as u32 * stride)
                });
                if !regular {
                    return None;
                }
                let idx = self.rv(i)[0];
                let d = self.alloc(1);
                self.emit(Op::Index {
                    d,
                    idx,
                    n: count,
                    stride,
                    add: p.dynoff,
                    u: i.ty == Type::UINT,
                });
                Some(Place {
                    base: p.base,
                    dynoff: Some(d),
                    cells: first.to_vec(),
                })
            }
            _ => None,
        }
    }

    fn load(&mut self, p: &Place) -> Vec<Reg> {
        match p.dynoff {
            None => p.cells.iter().map(|c| p.base + c).collect(),
            Some(off) => {
                let d = self.alloc(p.cells.len() as u32);
                for (i, c) in p.cells.iter().enumerate() {
                    self.emit(Op::LoadDyn {
                        d: d + i as u32,
                        base: p.base + c,
                        off,
                    });
                }
                (d..d + p.cells.len() as u32).collect()
            }
        }
    }

    fn store(&mut self, p: &Place, v: &[Reg]) {
        // Copy first when the source overlaps the destination.
        let dst: Vec<Reg> = p.cells.iter().map(|c| p.base + c).collect();
        let v = if p.dynoff.is_some() || v.iter().any(|r| dst.contains(r)) {
            self.copy(v)
        } else {
            v.to_vec()
        };
        for (i, c) in p.cells.iter().enumerate() {
            let s = v[i.min(v.len() - 1)];
            match p.dynoff {
                None => self.emit(Op::MovM { d: p.base + c, s }),
                Some(off) => self.emit(Op::StoreDyn {
                    base: p.base + c,
                    off,
                    s,
                }),
            }
        }
    }

    // -----------------------------------------------------------------
    // Expressions
    // -----------------------------------------------------------------

    fn rv(&mut self, e: &Expr) -> Vec<Reg> {
        if let Some(cv) = &e.cv
            && !matches!(
                e.k,
                EK::Assign(..) | EK::IncDec { .. } | EK::Call(..) | EK::Comma(..)
            )
        {
            return self.consts(&cv.clone());
        }
        match &e.k {
            EK::Var(_) | EK::Field(..) | EK::Swz(..) | EK::Index(..) => {
                if let Some(p) = self.place(e) {
                    return self.load(&p);
                }
                self.rv_derived(e)
            }
            EK::Const => self.consts(&e.cv.clone().map(|v| v.to_vec()).unwrap_or_default()),
            EK::Bin(op, a, b) => {
                let va = self.rv(a);
                let vb = self.rv(b);
                self.bin(*op, &va, &a.ty, &vb, &b.ty, &e.ty)
            }
            EK::Logic(and, a, b) => {
                let va = self.rv(a)[0];
                if pure(b) {
                    let vb = self.rv(b)[0];
                    let d = self.alloc(1);
                    self.emit(Op::I {
                        op: if *and { IOp::And } else { IOp::Or },
                        u: true,
                        d,
                        a: va,
                        b: vb,
                    });
                    return vec![d];
                }
                let t = self.copy(&[va])[0];
                let c = if *and {
                    t
                } else {
                    let c = self.alloc(1);
                    self.emit(Op::IU {
                        op: IUn::LNot,
                        d: c,
                        a: t,
                    });
                    c
                };
                let body = self.sub(|g| {
                    let vb = g.rv(b)[0];
                    g.emit(Op::MovM { d: t, s: vb });
                });
                self.node(Node::If {
                    c,
                    t: body,
                    e: Vec::new(),
                });
                vec![t]
            }
            EK::Un(op, a) => {
                let v = self.rv(a);
                let k = kind(&a.ty);
                v.iter()
                    .map(|s| {
                        let d = self.alloc(1);
                        match (op, k) {
                            (UOp::Neg, Scalar::Float) => self.emit(Op::FU {
                                op: FUn::Neg,
                                d,
                                a: *s,
                            }),
                            (UOp::Neg, _) => self.emit(Op::IU {
                                op: IUn::Neg,
                                d,
                                a: *s,
                            }),
                            (UOp::Not, _) => self.emit(Op::IU {
                                op: IUn::LNot,
                                d,
                                a: *s,
                            }),
                            (UOp::BitNot, _) => self.emit(Op::IU {
                                op: IUn::Not,
                                d,
                                a: *s,
                            }),
                        }
                        d
                    })
                    .collect()
            }
            EK::IncDec { pre, inc, e: x } => {
                let Some(p) = self.place(x) else {
                    return self.rv(x);
                };
                let cur = self.load(&p);
                let old = self.copy(&cur);
                let k = kind(&x.ty);
                let one = self.konst(if k == Scalar::Float {
                    1f32.to_bits()
                } else {
                    1
                });
                let new: Vec<Reg> = old
                    .iter()
                    .map(|s| {
                        let d = self.alloc(1);
                        if k == Scalar::Float {
                            self.emit(Op::F {
                                op: if *inc { FOp::Add } else { FOp::Sub },
                                d,
                                a: *s,
                                b: one,
                            });
                        } else {
                            self.emit(Op::I {
                                op: if *inc { IOp::Add } else { IOp::Sub },
                                u: k == Scalar::Uint,
                                d,
                                a: *s,
                                b: one,
                            });
                        }
                        d
                    })
                    .collect();
                self.store(&p, &new);
                if *pre { new } else { old }
            }
            EK::Assign(op, l, r) => {
                let Some(p) = self.place(l) else {
                    return self.rv(r);
                };
                let v = match op {
                    None => self.rv(r),
                    Some(b) => {
                        let cur = self.load(&p);
                        let cur = self.copy(&cur);
                        let vr = self.rv(r);
                        self.bin(*b, &cur, &l.ty, &vr, &r.ty, &l.ty)
                    }
                };
                self.store(&p, &v);
                self.load(&p)
            }
            EK::Call(f, args) => self.call(*f, args),
            EK::Bi(bi, args) => self.builtin(*bi, args, &e.ty),
            EK::Ctor(args) => {
                let types: Vec<Type> = args.iter().map(|a| a.ty).collect();
                let vals: Vec<Vec<Reg>> = args.iter().map(|a| self.rv(a)).collect();
                let plan = fold::ctor_plan(&e.ty, &types, &self.u.structs);
                let to = kind(&e.ty);
                let composite = e.ty.is_array() || e.ty.is_struct();
                plan.into_iter()
                    .map(|s| match s {
                        CtorSrc::Arg(i, k) => {
                            let from = kind(&types[i]);
                            let r = vals[i][k];
                            if composite || from == to {
                                r
                            } else {
                                let d = self.alloc(1);
                                self.emit(Op::Cvt { from, to, d, a: r });
                                d
                            }
                        }
                        CtorSrc::Zero => self.konst(crate::math::conv(Scalar::Float, to, 0)),
                        CtorSrc::One => {
                            self.konst(crate::math::conv(Scalar::Float, to, 1f32.to_bits()))
                        }
                    })
                    .collect()
            }
            EK::Cond(c, a, b) => {
                let vc = self.rv(c)[0];
                if pure(a) && pure(b) {
                    let va = self.rv(a);
                    let vb = self.rv(b);
                    return va
                        .iter()
                        .zip(&vb)
                        .map(|(x, y)| {
                            let d = self.alloc(1);
                            self.emit(Op::Select {
                                d,
                                c: vc,
                                a: *x,
                                b: *y,
                            });
                            d
                        })
                        .collect();
                }
                let n = e.ty.cells(&self.u.structs);
                let t = self.alloc(n);
                let res: Vec<Reg> = (t..t + n).collect();
                let tb = self.sub(|g| {
                    let v = g.rv(a);
                    for (i, s) in v.iter().enumerate() {
                        g.emit(Op::MovM { d: res[i], s: *s });
                    }
                });
                let eb = self.sub(|g| {
                    let v = g.rv(b);
                    for (i, s) in v.iter().enumerate() {
                        g.emit(Op::MovM { d: res[i], s: *s });
                    }
                });
                let c = self.copy(&[vc])[0];
                self.node(Node::If { c, t: tb, e: eb });
                res
            }
            EK::Comma(a, b) => {
                self.rv(a);
                self.rv(b)
            }
        }
    }

    /// Field/swizzle/index of a computed (non-place) value.
    fn rv_derived(&mut self, e: &Expr) -> Vec<Reg> {
        match &e.k {
            EK::Field(b, i) => {
                let v = self.rv(b);
                let Base::Struct(s) = b.ty.base else { return v };
                let def = &self.u.structs[s as usize];
                let off: u32 = def.fields[..*i as usize]
                    .iter()
                    .map(|f| f.1.cells(&self.u.structs))
                    .sum();
                let n = e.ty.cells(&self.u.structs);
                v[off as usize..(off + n) as usize].to_vec()
            }
            EK::Swz(b, idx) => {
                let v = self.rv(b);
                idx[..e.ty.rows as usize]
                    .iter()
                    .map(|k| v[*k as usize])
                    .collect()
            }
            EK::Index(b, i) => {
                let v = self.rv(b);
                let (stride, count) = if b.ty.is_array() {
                    (b.ty.elem().cells(&self.u.structs), b.ty.array)
                } else if b.ty.is_matrix() {
                    (u32::from(b.ty.rows), u32::from(b.ty.cols))
                } else {
                    (1, u32::from(b.ty.rows))
                };
                if let Some(cv) = &i.cv {
                    let k = (cv[0] as i32).clamp(0, count as i32 - 1) as u32;
                    return v[(k * stride) as usize..((k + 1) * stride) as usize].to_vec();
                }
                let v = if Self::contiguous(&v) {
                    v
                } else {
                    self.copy(&v)
                };
                let idx = self.rv(i)[0];
                let off = self.alloc(1);
                self.emit(Op::Index {
                    d: off,
                    idx,
                    n: count,
                    stride,
                    add: None,
                    u: i.ty == Type::UINT,
                });
                let d = self.alloc(stride);
                for k in 0..stride {
                    self.emit(Op::LoadDyn {
                        d: d + k,
                        base: v[0] + k,
                        off,
                    });
                }
                (d..d + stride).collect()
            }
            EK::Var(v) => {
                // A const variable without a folded value.
                let base = self.var_base(*v);
                let n = e.ty.cells(&self.u.structs);
                (base..base + n).collect()
            }
            _ => Vec::new(),
        }
    }

    fn bin(&mut self, op: BOp, a: &[Reg], at: &Type, b: &[Reg], bt: &Type, ty: &Type) -> Vec<Reg> {
        let k = kind(at);
        match op {
            BOp::Eq | BOp::Ne => {
                let mut kinds = Vec::new();
                cell_kinds(at, &self.u.structs, &mut kinds);
                let mut acc: Option<Reg> = None;
                for i in 0..a.len() {
                    let d = self.alloc(1);
                    self.emit(Op::Cmp {
                        k: kinds.get(i).copied().unwrap_or(Scalar::Int),
                        op: CmpOp::Eq,
                        d,
                        a: a[i],
                        b: b[i],
                    });
                    acc = Some(match acc {
                        None => d,
                        Some(p) => {
                            let x = self.alloc(1);
                            self.emit(Op::I {
                                op: IOp::And,
                                u: true,
                                d: x,
                                a: p,
                                b: d,
                            });
                            x
                        }
                    });
                }
                let r = acc.unwrap_or_else(|| self.konst(1));
                if op == BOp::Ne {
                    let d = self.alloc(1);
                    self.emit(Op::IU {
                        op: IUn::LNot,
                        d,
                        a: r,
                    });
                    return vec![d];
                }
                vec![r]
            }
            BOp::Lt | BOp::Gt | BOp::Le | BOp::Ge | BOp::LXor => {
                let d = self.alloc(1);
                let cop = match op {
                    BOp::Lt => CmpOp::Lt,
                    BOp::Gt => CmpOp::Gt,
                    BOp::Le => CmpOp::Le,
                    BOp::Ge => CmpOp::Ge,
                    _ => CmpOp::Ne,
                };
                self.emit(Op::Cmp {
                    k,
                    op: cop,
                    d,
                    a: a[0],
                    b: b[0],
                });
                vec![d]
            }
            BOp::Mul
                if (at.is_matrix() || bt.is_matrix()) && !at.is_scalar() && !bt.is_scalar() =>
            {
                // Column-major products.
                let (ar, ac) = (at.rows as usize, at.cols as usize);
                if at.is_matrix() {
                    let bc = if bt.is_matrix() { bt.cols as usize } else { 1 };
                    let mut out = Vec::with_capacity(ar * bc);
                    for c in 0..bc {
                        for r in 0..ar {
                            let mut acc = self.alloc(1);
                            self.emit(Op::F {
                                op: FOp::Mul,
                                d: acc,
                                a: a[r],
                                b: b[c * ac],
                            });
                            for kk in 1..ac {
                                let d = self.alloc(1);
                                self.emit(Op::Fma {
                                    d,
                                    a: a[kk * ar + r],
                                    b: b[c * ac + kk],
                                    c: acc,
                                });
                                acc = d;
                            }
                            out.push(acc);
                        }
                    }
                    return out;
                }
                // vector * matrix
                let (br, bc) = (bt.rows as usize, bt.cols as usize);
                let mut out = Vec::with_capacity(bc);
                for c in 0..bc {
                    let mut acc = self.alloc(1);
                    self.emit(Op::F {
                        op: FOp::Mul,
                        d: acc,
                        a: a[0],
                        b: b[c * br],
                    });
                    for kk in 1..br {
                        let d = self.alloc(1);
                        self.emit(Op::Fma {
                            d,
                            a: a[kk],
                            b: b[c * br + kk],
                            c: acc,
                        });
                        acc = d;
                    }
                    out.push(acc);
                }
                let _ = ar;
                out
            }
            _ => {
                let n = ty.comps() as usize;
                let u = k == Scalar::Uint;
                (0..n)
                    .map(|i| {
                        let x = if a.len() == 1 { a[0] } else { a[i] };
                        let y = if b.len() == 1 { b[0] } else { b[i] };
                        let d = self.alloc(1);
                        if k == Scalar::Float {
                            let f = match op {
                                BOp::Add => FOp::Add,
                                BOp::Sub => FOp::Sub,
                                BOp::Mul => FOp::Mul,
                                _ => FOp::Div,
                            };
                            self.emit(Op::F {
                                op: f,
                                d,
                                a: x,
                                b: y,
                            });
                        } else {
                            let io = match op {
                                BOp::Add => IOp::Add,
                                BOp::Sub => IOp::Sub,
                                BOp::Mul => IOp::Mul,
                                BOp::Div => IOp::Div,
                                BOp::Mod => IOp::Mod,
                                BOp::Shl => IOp::Shl,
                                BOp::Shr => IOp::Shr,
                                BOp::And => IOp::And,
                                BOp::Or => IOp::Or,
                                _ => IOp::Xor,
                            };
                            self.emit(Op::I {
                                op: io,
                                u,
                                d,
                                a: x,
                                b: y,
                            });
                        }
                        d
                    })
                    .collect()
            }
        }
    }

    fn call(&mut self, f: FuncId, args: &[Expr]) -> Vec<Reg> {
        let func = &self.u.funcs[f as usize];
        let Some(body) = &func.body else {
            return Vec::new();
        };
        // Arguments: evaluate in order; out/inout keep their place.
        let mut places = Vec::new();
        let mut params = Vec::new();
        for (a, (pid, q)) in args.iter().zip(&func.params) {
            let n = self.u.vars[*pid as usize].ty.cells(&self.u.structs);
            let base = self.alloc(n);
            match q {
                ParamQual::In => {
                    let v = self.rv(a);
                    for (i, s) in v.iter().enumerate() {
                        self.emit(Op::Mov {
                            d: base + i as u32,
                            s: *s,
                        });
                    }
                    places.push(None);
                }
                ParamQual::InOut => {
                    let p = self.place(a);
                    if let Some(p) = &p {
                        let v = self.load(p);
                        for (i, s) in v.iter().enumerate() {
                            self.emit(Op::Mov {
                                d: base + i as u32,
                                s: *s,
                            });
                        }
                    }
                    places.push(p);
                }
                ParamQual::Out => {
                    let p = self.place(a);
                    let z = self.konst(0);
                    for i in 0..n {
                        self.emit(Op::Mov { d: base + i, s: z });
                    }
                    places.push(p);
                }
            }
            params.push((*pid, base));
        }
        let saved: Vec<(VarId, Option<Reg>)> = params
            .iter()
            .map(|(p, _)| (*p, self.var_regs[*p as usize]))
            .collect();
        for (p, base) in &params {
            self.var_regs[*p as usize] = Some(*base);
        }
        let ret_n = func.ret.cells(&self.u.structs);
        let ret = self.alloc(ret_n);
        let ret_regs: Vec<Reg> = (ret..ret + ret_n).collect();
        let saved_mark = self.mark;
        self.mark = self.next;
        self.rets.push(ret_regs.clone());
        let body = body.clone();
        let nodes = self.sub(|g| {
            for s in &body {
                g.stmt(s);
            }
        });
        self.node(Node::Func(nodes));
        self.rets.pop();
        self.next = self.mark;
        self.mark = saved_mark;
        // Copy out.
        for ((pid, base), place) in params.iter().zip(&places) {
            if let Some(p) = place {
                let n = self.u.vars[*pid as usize].ty.cells(&self.u.structs);
                let v: Vec<Reg> = (*base..*base + n).collect();
                self.store(p, &v);
            }
        }
        for (p, r) in saved {
            self.var_regs[p as usize] = r;
        }
        ret_regs
    }

    fn unary_f(&mut self, op: FUn, v: &[Reg]) -> Vec<Reg> {
        v.iter()
            .map(|s| {
                let d = self.alloc(1);
                self.emit(Op::FU { op, d, a: *s });
                d
            })
            .collect()
    }

    fn f2(&mut self, op: FOp, a: Reg, b: Reg) -> Reg {
        let d = self.alloc(1);
        self.emit(Op::F { op, d, a, b });
        d
    }

    fn dot(&mut self, a: &[Reg], b: &[Reg]) -> Reg {
        let mut acc = self.f2(FOp::Mul, a[0], b[0]);
        for i in 1..a.len() {
            let d = self.alloc(1);
            self.emit(Op::Fma {
                d,
                a: a[i],
                b: b[i],
                c: acc,
            });
            acc = d;
        }
        acc
    }

    fn builtin(&mut self, bi: Bi, args: &[Expr], ty: &Type) -> Vec<Reg> {
        if bi == Bi::Modf {
            let x = self.rv(&args[0]);
            let ip = self.unary_f(FUn::Trunc, &x);
            if let Some(p) = self.place(&args[1]) {
                self.store(&p, &ip);
            }
            return x
                .iter()
                .zip(&ip)
                .map(|(a, b)| self.f2(FOp::Sub, *a, *b))
                .collect();
        }
        if let Bi::Tex(op) = bi {
            return self.texture(op, args, ty);
        }
        let v: Vec<Vec<Reg>> = args.iter().map(|a| self.rv(a)).collect();
        let n = ty.comps() as usize;
        let at = |v: &Vec<Reg>, i: usize| if v.len() == 1 { v[0] } else { v[i] };
        let k0 = args.first().map_or(Scalar::Float, |a| kind(&a.ty));
        let fu = match bi {
            Bi::Radians => Some(FUn::Radians),
            Bi::Degrees => Some(FUn::Degrees),
            Bi::Sin => Some(FUn::Sin),
            Bi::Cos => Some(FUn::Cos),
            Bi::Tan => Some(FUn::Tan),
            Bi::Asin => Some(FUn::Asin),
            Bi::Acos => Some(FUn::Acos),
            Bi::Atan => Some(FUn::Atan),
            Bi::Sinh => Some(FUn::Sinh),
            Bi::Cosh => Some(FUn::Cosh),
            Bi::Tanh => Some(FUn::Tanh),
            Bi::Asinh => Some(FUn::Asinh),
            Bi::Acosh => Some(FUn::Acosh),
            Bi::Atanh => Some(FUn::Atanh),
            Bi::Exp => Some(FUn::Exp),
            Bi::Log => Some(FUn::Log),
            Bi::Exp2 => Some(FUn::Exp2),
            Bi::Log2 => Some(FUn::Log2),
            Bi::Sqrt => Some(FUn::Sqrt),
            Bi::InvSqrt => Some(FUn::InvSqrt),
            Bi::Floor => Some(FUn::Floor),
            Bi::Ceil => Some(FUn::Ceil),
            Bi::Fract => Some(FUn::Fract),
            Bi::Trunc => Some(FUn::Trunc),
            Bi::Round | Bi::RoundEven => Some(FUn::Round),
            Bi::IsNan => Some(FUn::IsNan),
            Bi::IsInf => Some(FUn::IsInf),
            Bi::Abs if k0 == Scalar::Float => Some(FUn::Abs),
            Bi::Sign if k0 == Scalar::Float => Some(FUn::Sign),
            _ => None,
        };
        if let Some(op) = fu {
            return self.unary_f(op, &v[0]);
        }
        match bi {
            Bi::Abs | Bi::Sign => v[0]
                .iter()
                .map(|s| {
                    let d = self.alloc(1);
                    self.emit(Op::IU {
                        op: if bi == Bi::Abs { IUn::Abs } else { IUn::Sign },
                        d,
                        a: *s,
                    });
                    d
                })
                .collect(),
            Bi::FloatBitsToInt | Bi::FloatBitsToUint | Bi::IntBitsToFloat | Bi::UintBitsToFloat => {
                v[0].clone()
            }
            Bi::Atan2 | Bi::Pow | Bi::Mod | Bi::Step => {
                let op = match bi {
                    Bi::Atan2 => FOp::Atan2,
                    Bi::Pow => FOp::Pow,
                    Bi::Mod => FOp::Mod,
                    _ => FOp::Step,
                };
                (0..n)
                    .map(|i| self.f2(op, at(&v[0], i), at(&v[1], i)))
                    .collect()
            }
            Bi::Min | Bi::Max => (0..n)
                .map(|i| {
                    let (a, b) = (at(&v[0], i), at(&v[1], i));
                    if k0 == Scalar::Float {
                        self.f2(if bi == Bi::Min { FOp::Min } else { FOp::Max }, a, b)
                    } else {
                        let d = self.alloc(1);
                        self.emit(Op::I {
                            op: if bi == Bi::Min { IOp::Min } else { IOp::Max },
                            u: k0 == Scalar::Uint,
                            d,
                            a,
                            b,
                        });
                        d
                    }
                })
                .collect(),
            Bi::Clamp => (0..n)
                .map(|i| {
                    let d = self.alloc(1);
                    self.emit(Op::Clamp {
                        k: k0,
                        d,
                        x: at(&v[0], i),
                        lo: at(&v[1], i),
                        hi: at(&v[2], i),
                    });
                    d
                })
                .collect(),
            Bi::Mix => (0..n)
                .map(|i| {
                    let d = self.alloc(1);
                    self.emit(Op::Mix {
                        d,
                        a: at(&v[0], i),
                        b: at(&v[1], i),
                        t: at(&v[2], i),
                    });
                    d
                })
                .collect(),
            Bi::MixBool => (0..n)
                .map(|i| {
                    let d = self.alloc(1);
                    self.emit(Op::Select {
                        d,
                        c: at(&v[2], i),
                        a: at(&v[1], i),
                        b: at(&v[0], i),
                    });
                    d
                })
                .collect(),
            Bi::Smoothstep => (0..n)
                .map(|i| {
                    let d = self.alloc(1);
                    self.emit(Op::Smooth {
                        d,
                        e0: at(&v[0], i),
                        e1: at(&v[1], i),
                        x: at(&v[2], i),
                    });
                    d
                })
                .collect(),
            Bi::Dot => vec![self.dot(&v[0], &v[1])],
            Bi::Length => {
                let d = self.dot(&v[0], &v[0]);
                self.unary_f(FUn::Sqrt, &[d])
            }
            Bi::Distance => {
                let diff: Vec<Reg> = v[0]
                    .iter()
                    .zip(&v[1])
                    .map(|(a, b)| self.f2(FOp::Sub, *a, *b))
                    .collect();
                let d = self.dot(&diff, &diff);
                self.unary_f(FUn::Sqrt, &[d])
            }
            Bi::Normalize => {
                let d = self.dot(&v[0], &v[0]);
                let l = self.unary_f(FUn::Sqrt, &[d])[0];
                v[0].clone()
                    .iter()
                    .map(|x| self.f2(FOp::Div, *x, l))
                    .collect()
            }
            Bi::Cross => {
                let (a, b) = (&v[0], &v[1]);
                let mut out = Vec::new();
                for (i, j) in [(1, 2), (2, 0), (0, 1)] {
                    let x = self.f2(FOp::Mul, a[i], b[j]);
                    let y = self.f2(FOp::Mul, b[i], a[j]);
                    out.push(self.f2(FOp::Sub, x, y));
                }
                out
            }
            Bi::Faceforward => {
                let d = self.dot(&v[2], &v[1]);
                let z = self.konst(0);
                let c = self.alloc(1);
                self.emit(Op::Cmp {
                    k: Scalar::Float,
                    op: CmpOp::Lt,
                    d: c,
                    a: d,
                    b: z,
                });
                v[0].clone()
                    .iter()
                    .map(|x| {
                        let neg = self.alloc(1);
                        self.emit(Op::FU {
                            op: FUn::Neg,
                            d: neg,
                            a: *x,
                        });
                        let r = self.alloc(1);
                        self.emit(Op::Select {
                            d: r,
                            c,
                            a: *x,
                            b: neg,
                        });
                        r
                    })
                    .collect()
            }
            Bi::Reflect => {
                let (i_, n_) = (v[0].clone(), v[1].clone());
                let d = self.dot(&n_, &i_);
                let two = self.konst(2f32.to_bits());
                let d2 = self.f2(FOp::Mul, d, two);
                i_.iter()
                    .zip(&n_)
                    .map(|(x, nn)| {
                        let t = self.f2(FOp::Mul, d2, *nn);
                        self.f2(FOp::Sub, *x, t)
                    })
                    .collect()
            }
            Bi::Refract => {
                let (i_, n_, eta) = (v[0].clone(), v[1].clone(), v[2][0]);
                let d = self.dot(&n_, &i_);
                let one = self.konst(1f32.to_bits());
                let dd = self.f2(FOp::Mul, d, d);
                let omd = self.f2(FOp::Sub, one, dd);
                let ee = self.f2(FOp::Mul, eta, eta);
                let t = self.f2(FOp::Mul, ee, omd);
                let k = self.f2(FOp::Sub, one, t);
                let z = self.konst(0);
                let neg = self.alloc(1);
                self.emit(Op::Cmp {
                    k: Scalar::Float,
                    op: CmpOp::Lt,
                    d: neg,
                    a: k,
                    b: z,
                });
                let sk = self.unary_f(FUn::Sqrt, &[k])[0];
                let ed = self.f2(FOp::Mul, eta, d);
                let s = self.f2(FOp::Add, ed, sk);
                i_.iter()
                    .zip(&n_)
                    .map(|(x, nn)| {
                        let a = self.f2(FOp::Mul, eta, *x);
                        let b = self.f2(FOp::Mul, s, *nn);
                        let r = self.f2(FOp::Sub, a, b);
                        let o = self.alloc(1);
                        self.emit(Op::Select {
                            d: o,
                            c: neg,
                            a: z,
                            b: r,
                        });
                        o
                    })
                    .collect()
            }
            Bi::MatrixCompMult => v[0]
                .clone()
                .iter()
                .zip(&v[1].clone())
                .map(|(a, b)| self.f2(FOp::Mul, *a, *b))
                .collect(),
            Bi::OuterProduct => {
                let (c, r) = (v[0].clone(), v[1].clone());
                let mut out = Vec::new();
                for j in 0..r.len() {
                    for i in 0..c.len() {
                        out.push(self.f2(FOp::Mul, c[i], r[j]));
                    }
                }
                out
            }
            Bi::Transpose => {
                let at = args[0].ty;
                let (r, c) = (at.rows as usize, at.cols as usize);
                let mut out = Vec::new();
                for i in 0..r {
                    for j in 0..c {
                        out.push(v[0][j * r + i]);
                    }
                }
                out
            }
            Bi::Determinant | Bi::Inverse => {
                let src = if Self::contiguous(&v[0]) {
                    v[0].clone()
                } else {
                    self.copy(&v[0])
                };
                let n = args[0].ty.rows;
                let inv = bi == Bi::Inverse;
                let cnt = if inv { u32::from(n) * u32::from(n) } else { 1 };
                let d = self.alloc(cnt);
                self.emit(Op::Mat {
                    inv,
                    n,
                    d,
                    s: src[0],
                });
                (d..d + cnt).collect()
            }
            Bi::LessThan
            | Bi::LessThanEqual
            | Bi::GreaterThan
            | Bi::GreaterThanEqual
            | Bi::Equal
            | Bi::NotEqual => {
                let op = match bi {
                    Bi::LessThan => CmpOp::Lt,
                    Bi::LessThanEqual => CmpOp::Le,
                    Bi::GreaterThan => CmpOp::Gt,
                    Bi::GreaterThanEqual => CmpOp::Ge,
                    Bi::Equal => CmpOp::Eq,
                    _ => CmpOp::Ne,
                };
                (0..n)
                    .map(|i| {
                        let d = self.alloc(1);
                        self.emit(Op::Cmp {
                            k: k0,
                            op,
                            d,
                            a: v[0][i],
                            b: v[1][i],
                        });
                        d
                    })
                    .collect()
            }
            Bi::Any | Bi::All => {
                let mut acc = v[0][0];
                for i in 1..v[0].len() {
                    let d = self.alloc(1);
                    self.emit(Op::I {
                        op: if bi == Bi::Any { IOp::Or } else { IOp::And },
                        u: true,
                        d,
                        a: acc,
                        b: v[0][i],
                    });
                    acc = d;
                }
                vec![acc]
            }
            Bi::Not => v[0]
                .iter()
                .map(|s| {
                    let d = self.alloc(1);
                    self.emit(Op::IU {
                        op: IUn::LNot,
                        d,
                        a: *s,
                    });
                    d
                })
                .collect(),
            Bi::PackSnorm | Bi::PackUnorm | Bi::PackHalf => {
                let k = match bi {
                    Bi::PackSnorm => PackKind::Snorm,
                    Bi::PackUnorm => PackKind::Unorm,
                    _ => PackKind::Half,
                };
                let d = self.alloc(1);
                self.emit(Op::Pack {
                    k,
                    d,
                    a: v[0][0],
                    b: v[0][1],
                });
                vec![d]
            }
            Bi::UnpackSnorm | Bi::UnpackUnorm | Bi::UnpackHalf => {
                let k = match bi {
                    Bi::UnpackSnorm => PackKind::Snorm,
                    Bi::UnpackUnorm => PackKind::Unorm,
                    _ => PackKind::Half,
                };
                let d = self.alloc(2);
                self.emit(Op::Unpack {
                    k,
                    d0: d,
                    d1: d + 1,
                    s: v[0][0],
                });
                vec![d, d + 1]
            }
            Bi::DFdx | Bi::DFdy | Bi::Fwidth => {
                let k = match bi {
                    Bi::DFdx => DerivKind::X,
                    Bi::DFdy => DerivKind::Y,
                    _ => DerivKind::Width,
                };
                v[0].iter()
                    .map(|s| {
                        let d = self.alloc(1);
                        self.emit(Op::Deriv { k, d, s: *s });
                        d
                    })
                    .collect()
            }
            Bi::TexFetch(off) => {
                let Base::Sampler(s) = args[0].ty.base else {
                    return Vec::new();
                };
                let d = self.alloc(4);
                self.emit(Op::TexFetch {
                    d,
                    unit: v[0][0],
                    dim: s.dim,
                    ret: s.ret,
                    coord: v[1].clone(),
                    lod: v[2][0],
                    offset: if off { Some(v[3].clone()) } else { None },
                });
                (d..d + 4).collect()
            }
            Bi::TexSize => {
                let Base::Sampler(s) = args[0].ty.base else {
                    return Vec::new();
                };
                let k = ty.rows as u32;
                let d = self.alloc(3);
                self.emit(Op::TexSize {
                    d,
                    unit: v[0][0],
                    dim: s.dim,
                    lod: v[1][0],
                });
                (d..d + k).collect()
            }
            _ => Vec::new(),
        }
    }

    fn texture(&mut self, op: super::builtins::TexOp, args: &[Expr], ty: &Type) -> Vec<Reg> {
        let Base::Sampler(s) = args[0].ty.base else {
            return Vec::new();
        };
        let unit = self.rv(&args[0])[0];
        let p = self.rv(&args[1]);
        let mut rest = args[2..].iter();
        let mut coord: Vec<Reg>;
        let mut compare = None;
        if op.proj {
            let w = *p.last().expect("proj");
            let n = match (s.dim, s.shadow) {
                (super::types::Dim::D2, false) => 2,
                (super::types::Dim::D2, true) => 3,
                _ => 3,
            };
            coord = p[..n].iter().map(|c| self.f2(FOp::Div, *c, w)).collect();
            if s.shadow {
                compare = coord.pop();
            }
        } else {
            coord = p.clone();
            if s.shadow {
                compare = coord.pop();
            }
        }
        let mut bias = None;
        let mut lod = None;
        let mut grad = None;
        match op.lod {
            Lod::Implicit => {}
            Lod::Bias => {}
            Lod::Explicit => lod = Some(self.rv(rest.next().expect("lod"))[0]),
            Lod::Grad => {
                let gx = self.rv(rest.next().expect("dx"));
                let gy = self.rv(rest.next().expect("dy"));
                grad = Some((gx, gy));
            }
        }
        let offset = if op.offset {
            Some(self.rv(rest.next().expect("offset")))
        } else {
            None
        };
        if op.lod == Lod::Bias {
            bias = Some(self.rv(rest.next().expect("bias"))[0]);
        }
        let n = if s.shadow { 1 } else { 4 };
        let d = self.alloc(n);
        let _ = ty;
        self.emit(Op::Tex(Box::new(TexArgs {
            d,
            unit,
            dim: s.dim,
            ret: s.ret,
            shadow: s.shadow,
            coord,
            compare,
            bias,
            lod,
            grad,
            offset,
            implicit: self.stage == Stage::Fragment && matches!(op.lod, Lod::Implicit | Lod::Bias),
        })));
        (d..d + n).collect()
    }

    // -----------------------------------------------------------------
    // Statements
    // -----------------------------------------------------------------

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Empty => {}
            Stmt::Expr(e) => {
                self.rv(e);
                self.next = self.mark;
            }
            Stmt::Decl(list) => {
                for (v, init) in list {
                    let var = &self.u.vars[*v as usize];
                    let n = var.ty.cells(&self.u.structs);
                    let base = self.alloc(n);
                    self.var_regs[*v as usize] = Some(base);
                    self.mark = self.next;
                    match init {
                        Some(e) => {
                            let val = self.rv(e);
                            for (i, r) in val.iter().enumerate() {
                                self.emit(Op::Mov {
                                    d: base + i as u32,
                                    s: *r,
                                });
                            }
                        }
                        None => {
                            for i in 0..n {
                                self.emit(Op::Const { d: base + i, v: 0 });
                            }
                        }
                    }
                    self.next = self.mark;
                }
            }
            Stmt::Block(list) => {
                let m = self.mark;
                for s in list {
                    self.stmt(s);
                }
                self.mark = m;
                self.next = m;
            }
            Stmt::If(c, t, e) => {
                let cr = self.rv(c)[0];
                let tb = self.sub(|g| g.scoped(t));
                let eb = match e {
                    Some(e) => self.sub(|g| g.scoped(e)),
                    None => Vec::new(),
                };
                self.node(Node::If {
                    c: cr,
                    t: tb,
                    e: eb,
                });
                self.next = self.mark;
            }
            Stmt::For {
                init,
                cond,
                inc,
                body,
            } => {
                let m = self.mark;
                if let Some(i) = init {
                    self.stmt(i);
                }
                let mut cr = None;
                let pre = self.sub(|g| {
                    if let Some(c) = cond {
                        cr = Some(g.rv(c)[0]);
                    }
                });
                let b = self.sub(|g| g.scoped(body));
                let cont = self.sub(|g| {
                    if let Some(i) = inc {
                        g.rv(i);
                    }
                });
                self.node(Node::Loop {
                    pre,
                    cond: cr,
                    body: b,
                    cont,
                    post_test: false,
                });
                self.mark = m;
                self.next = m;
            }
            Stmt::While(c, body) => {
                let mut cr = None;
                let pre = self.sub(|g| cr = Some(g.rv(c)[0]));
                let b = self.sub(|g| g.scoped(body));
                self.node(Node::Loop {
                    pre,
                    cond: cr,
                    body: b,
                    cont: Vec::new(),
                    post_test: false,
                });
                self.next = self.mark;
            }
            Stmt::DoWhile(body, c) => {
                let b = self.sub(|g| g.scoped(body));
                let mut cr = None;
                let cont = self.sub(|g| cr = Some(g.rv(c)[0]));
                self.node(Node::Loop {
                    pre: Vec::new(),
                    cond: cr,
                    body: b,
                    cont,
                    post_test: true,
                });
                self.next = self.mark;
            }
            Stmt::Switch(sel, segs) => {
                let sv = self.rv(sel)[0];
                let u = sel.ty == Type::UINT;
                let n = segs.len() as u32;
                let start = self.konst(n);
                // Default first, then cases (in reverse, unique values).
                if let Some(di) = segs.iter().position(|(ls, _)| ls.contains(&None)) {
                    self.emit(Op::Const {
                        d: start,
                        v: di as u32,
                    });
                }
                for (i, (labels, _)) in segs.iter().enumerate().rev() {
                    for l in labels.iter().flatten() {
                        let c = self.konst(*l);
                        let m = self.alloc(1);
                        self.emit(Op::Cmp {
                            k: if u { Scalar::Uint } else { Scalar::Int },
                            op: CmpOp::Eq,
                            d: m,
                            a: sv,
                            b: c,
                        });
                        let iv = self.konst(i as u32);
                        self.emit(Op::Select {
                            d: start,
                            c: m,
                            a: iv,
                            b: start,
                        });
                    }
                }
                let start = self.copy(&[start])[0];
                self.mark = self.next;
                let m = self.mark;
                let body = self.sub(|g| {
                    for (i, (_, stmts)) in segs.iter().enumerate() {
                        let iv = g.konst(i as u32);
                        let c = g.alloc(1);
                        g.emit(Op::Cmp {
                            k: Scalar::Int,
                            op: CmpOp::Le,
                            d: c,
                            a: start,
                            b: iv,
                        });
                        let seg = g.sub(|g2| {
                            for s in stmts {
                                g2.stmt(s);
                            }
                        });
                        g.node(Node::If {
                            c,
                            t: seg,
                            e: Vec::new(),
                        });
                    }
                });
                self.node(Node::Breakable(body));
                self.mark = m;
                self.next = m;
            }
            Stmt::Break => self.node(Node::Break),
            Stmt::Continue => self.node(Node::Continue),
            Stmt::Discard => self.node(Node::Discard),
            Stmt::Return(e) => {
                if let Some(e) = e {
                    let v = self.rv(e);
                    let ret = self.rets.last().cloned().unwrap_or_default();
                    for (d, s) in ret.iter().zip(&v) {
                        self.emit(Op::MovM { d: *d, s: *s });
                    }
                }
                self.node(Node::Return);
                self.next = self.mark;
            }
        }
    }

    fn scoped(&mut self, s: &Stmt) {
        let m = self.mark;
        self.stmt(s);
        self.mark = m;
        self.next = m;
    }
}

/// Generates the shader IR.
pub fn generate(u: Unit, stage: Stage, log: &mut Log) -> Option<Shader> {
    let mut g = Gen {
        u: &u,
        next: 0,
        mark: 0,
        high: 0,
        var_regs: vec![None; u.vars.len()],
        used: vec![false; u.vars.len()],
        blocks: vec![Vec::new()],
        rets: Vec::new(),
        ops: 0,
        too_big: false,
        stage,
    };
    // Static cells: interface variables, built-ins and globals.
    for (i, v) in u.vars.iter().enumerate() {
        let global = matches!(
            v.storage,
            Storage::Uniform
                | Storage::Input
                | Storage::Output
                | Storage::Builtin(_)
                | Storage::Global
        ) || (v.storage == Storage::Const
            && u.globals.iter().any(|(g, _)| *g as usize == i));
        if global {
            let n = v.ty.cells(&u.structs);
            let r = g.alloc(n);
            g.var_regs[i] = Some(r);
        }
    }
    g.mark = g.next;
    // Global initializers, then main.
    let mut init: Vec<(VarId, Option<Expr>)> = u.globals.clone();
    init.retain(|(v, _)| {
        matches!(
            u.vars[*v as usize].storage,
            Storage::Global | Storage::Const
        )
    });
    let main_body = u.funcs[u.main as usize].body.clone().unwrap_or_default();
    let code = g.sub(|g| {
        for (v, e) in &init {
            let base = g.var_regs[*v as usize].expect("global");
            match e {
                Some(e) => {
                    let val = g.rv(e);
                    for (i, r) in val.iter().enumerate() {
                        g.emit(Op::Mov {
                            d: base + i as u32,
                            s: *r,
                        });
                    }
                }
                None => {
                    let n = u.vars[*v as usize].ty.cells(&u.structs);
                    for i in 0..n {
                        g.emit(Op::Const { d: base + i, v: 0 });
                    }
                }
            }
            g.next = g.mark;
        }
        // Outputs start at zero.
        for (i, v) in u.vars.iter().enumerate() {
            if matches!(
                v.storage,
                Storage::Output
                    | Storage::Builtin(
                        Builtin::Position
                            | Builtin::PointSize
                            | Builtin::FragColor
                            | Builtin::FragData
                            | Builtin::FragDepth
                    )
            ) {
                let base = g.var_regs[i].expect("out");
                for k in 0..v.ty.cells(&u.structs) {
                    g.emit(Op::Const {
                        d: base + k,
                        v: if v.storage == Storage::Builtin(Builtin::PointSize) {
                            1f32.to_bits()
                        } else {
                            0
                        },
                    });
                }
            }
        }
        let body = g.sub(|g| {
            for s in &main_body {
                g.stmt(s);
            }
        });
        g.node(Node::Func(body));
    });
    if g.too_big {
        log.raw("ERROR: 0:0: '' : Expression too complex.\n");
        return None;
    }
    let io = |g: &Gen<'_>, filter: &dyn Fn(&Var) -> bool| -> Vec<IoVar> {
        u.vars
            .iter()
            .enumerate()
            .filter(|(_, v)| filter(v))
            .map(|(i, v)| IoVar {
                name: v.name.clone(),
                ty: v.ty,
                prec: v.prec,
                cell: g.var_regs[i].unwrap_or(0),
                used: g.used[i] || v.used && false,
                location: v.location,
                interp: v.interp,
                invariant: v.invariant,
            })
            .collect()
    };
    let uniforms = io(&g, &|v| v.storage == Storage::Uniform && v.block.is_none());
    let inputs = io(&g, &|v| v.storage == Storage::Input);
    let outputs = io(&g, &|v| v.storage == Storage::Output);
    let mut builtins = HashMap::new();
    let mut frag_data = 0;
    let mut writes_frag_depth = false;
    let mut uses_point_size = false;
    for (i, v) in u.vars.iter().enumerate() {
        if let Storage::Builtin(b) = v.storage
            && let Some(r) = g.var_regs[i]
        {
            builtins.insert(b, r);
            if g.used[i] {
                match b {
                    Builtin::FragData => frag_data = v.ty.array,
                    Builtin::FragDepth => writes_frag_depth = true,
                    Builtin::PointSize => uses_point_size = true,
                    _ => {}
                }
            }
        }
    }
    if stage == Stage::Fragment && u.uses_frag_color {
        frag_data = 0;
    }
    let blocks = u
        .blocks
        .iter()
        .map(|b| BlockInfo {
            name: b.name.clone(),
            instance: b.instance.clone(),
            cell: g.var_regs[b.var as usize].unwrap_or(0),
            ty: u.vars[b.var as usize].ty,
            binding: b.binding,
            row_major: b.row_major.clone(),
            used: g.used[b.var as usize],
        })
        .collect();
    let cells = g.high;
    Some(Shader {
        stage,
        version: u.version,
        cells,
        code,
        uniforms,
        inputs,
        outputs,
        builtins,
        structs: u.structs.clone(),
        blocks,
        frag_data,
        writes_frag_depth,
        uses_point_size,
    })
}
