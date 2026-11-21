//! Scalar semantics shared by constant folding and the interpreter.

use crate::glsl::ast::BOp;
use crate::glsl::builtins::Bi;
use crate::glsl::types::Scalar;

#[inline]
pub fn f(x: u32) -> f32 {
    f32::from_bits(x)
}
#[inline]
pub fn fb(x: f32) -> u32 {
    x.to_bits()
}

/// Converts one component between scalar kinds (constructor rules).
pub fn conv(from: Scalar, to: Scalar, v: u32) -> u32 {
    use Scalar::*;
    match (from, to) {
        (a, b) if a == b => v,
        (Float, Int) => (f(v) as i32) as u32,
        (Float, Uint) => {
            let x = f(v);
            if x < 0.0 { (x as i32) as u32 } else { x as u32 }
        }
        (Float, Bool) => u32::from(f(v) != 0.0),
        (Int, Float) => fb(v as i32 as f32),
        (Uint, Float) => fb(v as f32),
        (Int | Uint, Int | Uint) => v,
        (Int | Uint, Bool) => u32::from(v != 0),
        (Bool, Float) => fb(if v != 0 { 1.0 } else { 0.0 }),
        (Bool, Int | Uint) => u32::from(v != 0),
        _ => v,
    }
}

/// Component-wise binary operation; comparisons give 0/1.
pub fn bin(op: BOp, s: Scalar, x: u32, y: u32) -> u32 {
    match s {
        Scalar::Float => {
            let (a, b) = (f(x), f(y));
            match op {
                BOp::Add => fb(a + b),
                BOp::Sub => fb(a - b),
                BOp::Mul => fb(a * b),
                BOp::Div => fb(a / b),
                BOp::Lt => u32::from(a < b),
                BOp::Gt => u32::from(a > b),
                BOp::Le => u32::from(a <= b),
                BOp::Ge => u32::from(a >= b),
                BOp::Eq => u32::from(a == b),
                BOp::Ne => u32::from(a != b),
                _ => 0,
            }
        }
        Scalar::Int => {
            let (a, b) = (x as i32, y as i32);
            (match op {
                BOp::Add => a.wrapping_add(b),
                BOp::Sub => a.wrapping_sub(b),
                BOp::Mul => a.wrapping_mul(b),
                BOp::Div => {
                    if b == 0 {
                        if a < 0 { i32::MIN } else { i32::MAX }
                    } else {
                        a.wrapping_div(b)
                    }
                }
                BOp::Mod => {
                    if b == 0 {
                        a
                    } else {
                        a.wrapping_rem(b)
                    }
                }
                BOp::Shl => a.wrapping_shl(b as u32),
                BOp::Shr => a.wrapping_shr(b as u32),
                BOp::And => a & b,
                BOp::Or => a | b,
                BOp::Xor => a ^ b,
                BOp::Lt => i32::from(a < b),
                BOp::Gt => i32::from(a > b),
                BOp::Le => i32::from(a <= b),
                BOp::Ge => i32::from(a >= b),
                BOp::Eq => i32::from(a == b),
                BOp::Ne => i32::from(a != b),
                BOp::LXor => i32::from((a != 0) != (b != 0)),
            }) as u32
        }
        Scalar::Uint => {
            let (a, b) = (x, y);
            match op {
                BOp::Add => a.wrapping_add(b),
                BOp::Sub => a.wrapping_sub(b),
                BOp::Mul => a.wrapping_mul(b),
                BOp::Div => a.checked_div(b).unwrap_or(u32::MAX),
                BOp::Mod => a.checked_rem(b).unwrap_or(a),
                BOp::Shl => a.wrapping_shl(b),
                BOp::Shr => a.wrapping_shr(b),
                BOp::And => a & b,
                BOp::Or => a | b,
                BOp::Xor => a ^ b,
                BOp::Lt => u32::from(a < b),
                BOp::Gt => u32::from(a > b),
                BOp::Le => u32::from(a <= b),
                BOp::Ge => u32::from(a >= b),
                BOp::Eq => u32::from(a == b),
                BOp::Ne => u32::from(a != b),
                BOp::LXor => u32::from((a != 0) != (b != 0)),
            }
        }
        Scalar::Bool => match op {
            BOp::Eq => u32::from(x == y),
            BOp::Ne | BOp::LXor => u32::from(x != y),
            BOp::And => x & y,
            BOp::Or => x | y,
            _ => 0,
        },
    }
}

/// Component-wise unary float built-ins.
pub fn unary_f(bi: Bi, x: f32) -> f32 {
    match bi {
        Bi::Radians => x * (std::f32::consts::PI / 180.0),
        Bi::Degrees => x * (180.0 / std::f32::consts::PI),
        Bi::Sin => x.sin(),
        Bi::Cos => x.cos(),
        Bi::Tan => x.tan(),
        Bi::Asin => x.asin(),
        Bi::Acos => x.acos(),
        Bi::Atan => x.atan(),
        Bi::Sinh => x.sinh(),
        Bi::Cosh => x.cosh(),
        Bi::Tanh => x.tanh(),
        Bi::Asinh => x.asinh(),
        Bi::Acosh => x.acosh(),
        Bi::Atanh => x.atanh(),
        Bi::Exp => x.exp(),
        Bi::Log => x.ln(),
        Bi::Exp2 => x.exp2(),
        Bi::Log2 => x.log2(),
        Bi::Sqrt => x.sqrt(),
        Bi::InvSqrt => 1.0 / x.sqrt(),
        Bi::Floor => x.floor(),
        Bi::Ceil => x.ceil(),
        Bi::Fract => x - x.floor(),
        Bi::Trunc => x.trunc(),
        Bi::Round | Bi::RoundEven => x.round_ties_even(),
        Bi::Abs => x.abs(),
        Bi::Sign => {
            if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                0.0
            }
        }
        _ => x,
    }
}

pub fn fmod(x: f32, y: f32) -> f32 {
    x - y * (x / y).floor()
}

pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub fn fclamp(x: f32, lo: f32, hi: f32) -> f32 {
    x.max(lo).min(hi)
}

/// IEEE half from f32 (round to nearest even).
pub fn f32_to_f16(v: f32) -> u16 {
    let x = v.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let exp = ((x >> 23) & 0xff) as i32;
    let man = x & 0x7f_ffff;
    if exp == 255 {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 31 {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = 1u32 << (shift - 1);
        let mut r = m >> shift;
        let rem = m & ((1 << shift) - 1);
        if rem > half || (rem == half && (r & 1) == 1) {
            r += 1;
        }
        return sign | r as u16;
    }
    let mut r = ((e as u32) << 10) | (man >> 13);
    let rem = man & 0x1fff;
    if rem > 0x1000 || (rem == 0x1000 && (r & 1) == 1) {
        r += 1;
    }
    sign | r as u16
}

pub fn f16_to_f32(h: u16) -> f32 {
    let sign = u32::from(h & 0x8000) << 16;
    let exp = u32::from((h >> 10) & 0x1f);
    let man = u32::from(h & 0x3ff);
    let bits = if exp == 0 {
        if man == 0 {
            sign
        } else {
            let mut e = 127 - 15 + 1;
            let mut m = man;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            sign | ((e as u32) << 23) | ((m & 0x3ff) << 13)
        }
    } else if exp == 31 {
        sign | 0x7f80_0000 | (man << 13)
    } else {
        sign | ((exp + 127 - 15) << 23) | (man << 13)
    };
    f32::from_bits(bits)
}

/// Column-major matrix product: `a` is `ar`x`ac`, `b` is `ac`x`bc`.
pub fn matmul(a: &[f32], ar: usize, ac: usize, b: &[f32], bc: usize) -> Vec<f32> {
    let mut out = vec![0.0; ar * bc];
    for c in 0..bc {
        for r in 0..ar {
            let mut s = 0.0;
            for k in 0..ac {
                s += a[k * ar + r] * b[c * ac + k];
            }
            out[c * ar + r] = s;
        }
    }
    out
}

pub fn determinant(m: &[f32], n: usize) -> f32 {
    match n {
        2 => m[0] * m[3] - m[2] * m[1],
        3 => {
            m[0] * (m[4] * m[8] - m[7] * m[5]) - m[3] * (m[1] * m[8] - m[7] * m[2])
                + m[6] * (m[1] * m[5] - m[4] * m[2])
        }
        _ => {
            let mut d = 0.0;
            for c in 0..4 {
                let minor: Vec<f32> = (0..4)
                    .filter(|cc| *cc != c)
                    .flat_map(|cc| (1..4).map(move |r| m[cc * 4 + r]))
                    .collect();
                let s = if c % 2 == 0 { 1.0 } else { -1.0 };
                d += s * m[c * 4] * determinant(&minor, 3);
            }
            d
        }
    }
}

pub fn inverse(m: &[f32], n: usize) -> Vec<f32> {
    let det = determinant(m, n);
    let mut out = vec![0.0; n * n];
    for c in 0..n {
        for r in 0..n {
            // cofactor of element (row c, column r) → out[c][r] (transposed)
            let minor: Vec<f32> = (0..n)
                .filter(|cc| *cc != r)
                .flat_map(|cc| {
                    (0..n)
                        .filter(move |rr| *rr != c)
                        .map(move |rr| m[cc * n + rr])
                })
                .collect();
            let s = if (r + c) % 2 == 0 { 1.0 } else { -1.0 };
            let cof = if n == 2 {
                minor[0]
            } else {
                determinant(&minor, n - 1)
            };
            out[c * n + r] = s * cof / det;
        }
    }
    out
}
