//! Constant folding of typed expressions.

use super::ast::{BOp, UOp};
use super::builtins::Bi;
use super::types::{Base, Scalar, StructDef, Type};
use crate::math::{self, f, fb};

pub type Val = Vec<u32>;

fn kind(t: &Type) -> Scalar {
    t.scalar_kind().unwrap_or(Scalar::Float)
}

/// Binary operation on constants (`ty` is the result type).
pub fn bin(op: BOp, lt: &Type, l: &[u32], rt: &Type, r: &[u32], ty: &Type) -> Option<Val> {
    let s = kind(lt);
    match op {
        BOp::Eq | BOp::Ne => {
            let eq = l == r || (s == Scalar::Float && l.iter().zip(r).all(|(a, b)| f(*a) == f(*b)));
            let eq = if s == Scalar::Float {
                l.iter().zip(r).all(|(a, b)| f(*a) == f(*b))
            } else {
                eq
            };
            return Some(vec![u32::from(if op == BOp::Eq { eq } else { !eq })]);
        }
        BOp::Mul if lt.is_matrix() || rt.is_matrix() => {
            if lt.is_matrix() && rt.is_matrix() {
                let a: Vec<f32> = l.iter().map(|x| f(*x)).collect();
                let b: Vec<f32> = r.iter().map(|x| f(*x)).collect();
                let m = math::matmul(&a, lt.rows as usize, lt.cols as usize, &b, rt.cols as usize);
                return Some(m.into_iter().map(fb).collect());
            }
            if lt.is_matrix() && rt.is_vector() {
                let a: Vec<f32> = l.iter().map(|x| f(*x)).collect();
                let b: Vec<f32> = r.iter().map(|x| f(*x)).collect();
                let m = math::matmul(&a, lt.rows as usize, lt.cols as usize, &b, 1);
                return Some(m.into_iter().map(fb).collect());
            }
            if lt.is_vector() && rt.is_matrix() {
                // v * M = M^T v
                let mut out = Vec::new();
                for c in 0..rt.cols as usize {
                    let mut s = 0.0;
                    for k in 0..rt.rows as usize {
                        s += f(l[k]) * f(r[c * rt.rows as usize + k]);
                    }
                    out.push(fb(s));
                }
                return Some(out);
            }
        }
        _ => {}
    }
    let n = ty.comps().max(l.len().max(r.len()) as u32) as usize;
    let n = if matches!(op, BOp::Lt | BOp::Gt | BOp::Le | BOp::Ge | BOp::LXor) {
        1
    } else {
        n
    };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let a = if l.len() == 1 { l[0] } else { *l.get(i)? };
        let b = if r.len() == 1 { r[0] } else { *r.get(i)? };
        let rs = if matches!(op, BOp::Shl | BOp::Shr) {
            // Shift amount kind may differ; value kind decides.
            s
        } else {
            s
        };
        out.push(math::bin(op, rs, a, b));
    }
    Some(out)
}

pub fn un(op: UOp, t: &Type, v: &[u32]) -> Val {
    let s = kind(t);
    v.iter()
        .map(|x| match (op, s) {
            (UOp::Neg, Scalar::Float) => fb(-f(*x)),
            (UOp::Neg, _) => (*x as i32).wrapping_neg() as u32,
            (UOp::Not, _) => u32::from(*x == 0),
            (UOp::BitNot, _) => !*x,
        })
        .collect()
}

/// Flattened constructor (also used by code generation for the layout
/// of components): returns, for each output component, (argument index,
/// component index) or a constant (identity/zero fill).
#[derive(Clone, Copy, Debug)]
pub enum CtorSrc {
    Arg(usize, usize),
    Zero,
    One,
}

pub fn ctor_plan(ty: &Type, args: &[Type], structs: &[StructDef]) -> Vec<CtorSrc> {
    if ty.is_array() || ty.is_struct() {
        // Element-wise copy of each argument.
        let mut out = Vec::new();
        for (i, a) in args.iter().enumerate() {
            for k in 0..a.cells(structs) as usize {
                out.push(CtorSrc::Arg(i, k));
            }
        }
        return out;
    }
    let n = ty.comps() as usize;
    if args.len() == 1 && args[0].is_scalar() {
        if ty.is_matrix() {
            let mut out = Vec::new();
            for c in 0..ty.cols as usize {
                for r in 0..ty.rows as usize {
                    out.push(if c == r {
                        CtorSrc::Arg(0, 0)
                    } else {
                        CtorSrc::Zero
                    });
                }
            }
            return out;
        }
        return vec![CtorSrc::Arg(0, 0); n];
    }
    if args.len() == 1 && args[0].is_matrix() && ty.is_matrix() {
        let a = args[0];
        let mut out = Vec::new();
        for c in 0..ty.cols as usize {
            for r in 0..ty.rows as usize {
                out.push(if c < a.cols as usize && r < a.rows as usize {
                    CtorSrc::Arg(0, c * a.rows as usize + r)
                } else if c == r {
                    CtorSrc::One
                } else {
                    CtorSrc::Zero
                });
            }
        }
        return out;
    }
    let mut out = Vec::new();
    'outer: for (i, a) in args.iter().enumerate() {
        for k in 0..a.comps() as usize {
            if out.len() == n {
                break 'outer;
            }
            out.push(CtorSrc::Arg(i, k));
        }
    }
    out
}

pub fn ctor(ty: &Type, args: &[(Type, &[u32])], structs: &[StructDef]) -> Val {
    let types: Vec<Type> = args.iter().map(|a| a.0).collect();
    let plan = ctor_plan(ty, &types, structs);
    let to = kind(ty);
    plan.into_iter()
        .map(|s| match s {
            CtorSrc::Arg(i, k) => {
                let (t, v) = &args[i];
                if ty.is_struct() || ty.is_array() {
                    v[k]
                } else {
                    math::conv(kind(t), to, v[k])
                }
            }
            CtorSrc::Zero => math::conv(Scalar::Float, to, fb(0.0)),
            CtorSrc::One => math::conv(Scalar::Float, to, fb(1.0)),
        })
        .collect()
}

/// Folds a built-in call on constant arguments (the common ones).
pub fn builtin(bi: Bi, args: &[(Type, &[u32])], ty: &Type) -> Option<Val> {
    let fl = |i: usize| -> Vec<f32> { args[i].1.iter().map(|x| f(*x)).collect() };
    let n = ty.comps() as usize;
    let at = |v: &Vec<f32>, i: usize| if v.len() == 1 { v[0] } else { v[i] };
    let s0 = args.first().map(|a| kind(&a.0)).unwrap_or(Scalar::Float);
    let out: Vec<u32> = match bi {
        Bi::Radians
        | Bi::Degrees
        | Bi::Sin
        | Bi::Cos
        | Bi::Tan
        | Bi::Asin
        | Bi::Acos
        | Bi::Atan
        | Bi::Sinh
        | Bi::Cosh
        | Bi::Tanh
        | Bi::Asinh
        | Bi::Acosh
        | Bi::Atanh
        | Bi::Exp
        | Bi::Log
        | Bi::Exp2
        | Bi::Log2
        | Bi::Sqrt
        | Bi::InvSqrt
        | Bi::Floor
        | Bi::Ceil
        | Bi::Fract
        | Bi::Trunc
        | Bi::Round
        | Bi::RoundEven
            if s0 == Scalar::Float =>
        {
            fl(0)
                .into_iter()
                .map(|x| fb(math::unary_f(bi, x)))
                .collect()
        }
        Bi::Abs | Bi::Sign if s0 == Scalar::Float => fl(0)
            .into_iter()
            .map(|x| fb(math::unary_f(bi, x)))
            .collect(),
        Bi::Abs => args[0]
            .1
            .iter()
            .map(|x| (*x as i32).wrapping_abs() as u32)
            .collect(),
        Bi::Sign => args[0]
            .1
            .iter()
            .map(|x| (*x as i32).signum() as u32)
            .collect(),
        Bi::Pow | Bi::Atan2 | Bi::Mod | Bi::Step if s0 == Scalar::Float || bi == Bi::Step => {
            let (a, b) = (fl(0), fl(1));
            (0..n)
                .map(|i| {
                    let (x, y) = (at(&a, i), at(&b, i));
                    fb(match bi {
                        Bi::Pow => x.powf(y),
                        Bi::Atan2 => x.atan2(y),
                        Bi::Mod => math::fmod(x, y),
                        _ => {
                            if y < x {
                                0.0
                            } else {
                                1.0
                            }
                        }
                    })
                })
                .collect()
        }
        Bi::Min | Bi::Max | Bi::Clamp => {
            let get = |k: usize, i: usize| {
                let v = args[k].1;
                if v.len() == 1 { v[0] } else { v[i] }
            };
            (0..n)
                .map(|i| {
                    let x = get(0, i);
                    let pick = |a: u32, b: u32, min: bool| -> u32 {
                        let less = match s0 {
                            Scalar::Float => f(a) < f(b),
                            Scalar::Int => (a as i32) < (b as i32),
                            _ => a < b,
                        };
                        if less == min { a } else { b }
                    };
                    match bi {
                        Bi::Min => pick(x, get(1, i), true),
                        Bi::Max => pick(x, get(1, i), false),
                        _ => pick(pick(x, get(1, i), false), get(2, i), true),
                    }
                })
                .collect()
        }
        Bi::Mix => {
            let (a, b, t) = (fl(0), fl(1), fl(2));
            (0..n)
                .map(|i| fb(at(&a, i) * (1.0 - at(&t, i)) + at(&b, i) * at(&t, i)))
                .collect()
        }
        Bi::Smoothstep => {
            let (a, b, x) = (fl(0), fl(1), fl(2));
            (0..n)
                .map(|i| fb(math::smoothstep(at(&a, i), at(&b, i), at(&x, i))))
                .collect()
        }
        Bi::Dot => {
            let (a, b) = (fl(0), fl(1));
            vec![fb(a.iter().zip(&b).map(|(x, y)| x * y).sum())]
        }
        Bi::Length => {
            let a = fl(0);
            vec![fb(a.iter().map(|x| x * x).sum::<f32>().sqrt())]
        }
        Bi::Distance => {
            let (a, b) = (fl(0), fl(1));
            vec![fb(a
                .iter()
                .zip(&b)
                .map(|(x, y)| (x - y) * (x - y))
                .sum::<f32>()
                .sqrt())]
        }
        Bi::Normalize => {
            let a = fl(0);
            let l = a.iter().map(|x| x * x).sum::<f32>().sqrt();
            a.iter().map(|x| fb(x / l)).collect()
        }
        Bi::Cross => {
            let (a, b) = (fl(0), fl(1));
            vec![
                fb(a[1] * b[2] - b[1] * a[2]),
                fb(a[2] * b[0] - b[2] * a[0]),
                fb(a[0] * b[1] - b[0] * a[1]),
            ]
        }
        Bi::LessThan
        | Bi::LessThanEqual
        | Bi::GreaterThan
        | Bi::GreaterThanEqual
        | Bi::Equal
        | Bi::NotEqual => {
            let op = match bi {
                Bi::LessThan => BOp::Lt,
                Bi::LessThanEqual => BOp::Le,
                Bi::GreaterThan => BOp::Gt,
                Bi::GreaterThanEqual => BOp::Ge,
                Bi::Equal => BOp::Eq,
                _ => BOp::Ne,
            };
            (0..n)
                .map(|i| math::bin(op, s0, args[0].1[i], args[1].1[i]))
                .collect()
        }
        Bi::Any => vec![u32::from(args[0].1.iter().any(|x| *x != 0))],
        Bi::All => vec![u32::from(args[0].1.iter().all(|x| *x != 0))],
        Bi::Not => args[0].1.iter().map(|x| u32::from(*x == 0)).collect(),
        Bi::FloatBitsToInt | Bi::FloatBitsToUint | Bi::IntBitsToFloat | Bi::UintBitsToFloat => {
            args[0].1.to_vec()
        }
        _ => return None,
    };
    let _ = Base::Void;
    (out.len() == n).then_some(out)
}
