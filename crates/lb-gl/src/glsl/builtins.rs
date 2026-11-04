//! Built-in functions: overload resolution (GLSL ES has no implicit
//! conversions, so matching is exact).

use super::types::{Base, Dim, Scalar, Type};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lod {
    Implicit,
    Bias,
    Explicit,
    Grad,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TexOp {
    pub proj: bool,
    pub lod: Lod,
    pub offset: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bi {
    Radians,
    Degrees,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sinh,
    Cosh,
    Tanh,
    Asinh,
    Acosh,
    Atanh,
    Exp,
    Log,
    Exp2,
    Log2,
    Sqrt,
    InvSqrt,
    Floor,
    Ceil,
    Fract,
    Trunc,
    Round,
    RoundEven,
    Abs,
    Sign,
    IsNan,
    IsInf,
    FloatBitsToInt,
    FloatBitsToUint,
    IntBitsToFloat,
    UintBitsToFloat,
    Atan2,
    Pow,
    Mod,
    Min,
    Max,
    Step,
    Clamp,
    Mix,
    MixBool,
    Smoothstep,
    Modf,
    Length,
    Distance,
    Dot,
    Cross,
    Normalize,
    Faceforward,
    Reflect,
    Refract,
    MatrixCompMult,
    OuterProduct,
    Transpose,
    Determinant,
    Inverse,
    LessThan,
    LessThanEqual,
    GreaterThan,
    GreaterThanEqual,
    Equal,
    NotEqual,
    Any,
    All,
    Not,
    PackSnorm,
    UnpackSnorm,
    PackUnorm,
    UnpackUnorm,
    PackHalf,
    UnpackHalf,
    DFdx,
    DFdy,
    Fwidth,
    Tex(TexOp),
    TexFetch(bool),
    TexSize,
}

/// What the shader may use.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ctx {
    pub vertex: bool,
    pub version: u32,
    pub derivatives: bool,
    pub texture_lod: bool,
}

fn is_f(t: &Type) -> bool {
    t.is_sv(Scalar::Float)
}
fn is_i(t: &Type) -> bool {
    t.is_sv(Scalar::Int)
}
fn is_u(t: &Type) -> bool {
    t.is_sv(Scalar::Uint)
}
fn is_b(t: &Type) -> bool {
    t.is_sv(Scalar::Bool)
}
fn same(a: &Type, b: &Type) -> bool {
    a == b
}

/// Resolves a built-in call; `None` when no overload matches (or the
/// name is not a built-in).
pub fn resolve(name: &str, a: &[Type], c: Ctx) -> Option<(Bi, Type)> {
    let es3 = c.version >= 300;
    let n = a.len();
    let a0 = a.first().copied().unwrap_or(Type::VOID);
    let a1 = a.get(1).copied().unwrap_or(Type::VOID);
    let a2 = a.get(2).copied().unwrap_or(Type::VOID);
    let fl = Type::FLOAT;
    let un_f = |bi: Bi| (n == 1 && is_f(&a0)).then_some((bi, a0));
    let un_f3 = |bi: Bi| (es3 && n == 1 && is_f(&a0)).then_some((bi, a0));
    let r = match name {
        "radians" => un_f(Bi::Radians),
        "degrees" => un_f(Bi::Degrees),
        "sin" => un_f(Bi::Sin),
        "cos" => un_f(Bi::Cos),
        "tan" => un_f(Bi::Tan),
        "asin" => un_f(Bi::Asin),
        "acos" => un_f(Bi::Acos),
        "atan" if n == 1 => un_f(Bi::Atan),
        "atan" => (n == 2 && is_f(&a0) && same(&a0, &a1)).then_some((Bi::Atan2, a0)),
        "sinh" => un_f3(Bi::Sinh),
        "cosh" => un_f3(Bi::Cosh),
        "tanh" => un_f3(Bi::Tanh),
        "asinh" => un_f3(Bi::Asinh),
        "acosh" => un_f3(Bi::Acosh),
        "atanh" => un_f3(Bi::Atanh),
        "pow" => (n == 2 && is_f(&a0) && same(&a0, &a1)).then_some((Bi::Pow, a0)),
        "exp" => un_f(Bi::Exp),
        "log" => un_f(Bi::Log),
        "exp2" => un_f(Bi::Exp2),
        "log2" => un_f(Bi::Log2),
        "sqrt" => un_f(Bi::Sqrt),
        "inversesqrt" => un_f(Bi::InvSqrt),
        "abs" | "sign" => {
            let bi = if name == "abs" { Bi::Abs } else { Bi::Sign };
            (n == 1 && (is_f(&a0) || (es3 && is_i(&a0)))).then_some((bi, a0))
        }
        "floor" => un_f(Bi::Floor),
        "ceil" => un_f(Bi::Ceil),
        "fract" => un_f(Bi::Fract),
        "trunc" => un_f3(Bi::Trunc),
        "round" => un_f3(Bi::Round),
        "roundEven" => un_f3(Bi::RoundEven),
        "isnan" | "isinf" if es3 && n == 1 && is_f(&a0) => Some((
            if name == "isnan" {
                Bi::IsNan
            } else {
                Bi::IsInf
            },
            Type::vec(Scalar::Bool, a0.rows),
        )),
        "floatBitsToInt" if es3 && n == 1 && is_f(&a0) => {
            Some((Bi::FloatBitsToInt, Type::vec(Scalar::Int, a0.rows)))
        }
        "floatBitsToUint" if es3 && n == 1 && is_f(&a0) => {
            Some((Bi::FloatBitsToUint, Type::vec(Scalar::Uint, a0.rows)))
        }
        "intBitsToFloat" if es3 && n == 1 && is_i(&a0) => {
            Some((Bi::IntBitsToFloat, Type::vec(Scalar::Float, a0.rows)))
        }
        "uintBitsToFloat" if es3 && n == 1 && is_u(&a0) => {
            Some((Bi::UintBitsToFloat, Type::vec(Scalar::Float, a0.rows)))
        }
        "mod" => (n == 2 && is_f(&a0) && (same(&a0, &a1) || a1 == fl)).then_some((Bi::Mod, a0)),
        "min" | "max" => {
            let bi = if name == "min" { Bi::Min } else { Bi::Max };
            let num = is_f(&a0) || (es3 && (is_i(&a0) || is_u(&a0)));
            let s = Type::scalar(a0.scalar_kind().unwrap_or(Scalar::Float));
            (n == 2 && num && (same(&a0, &a1) || a1 == s)).then_some((bi, a0))
        }
        "clamp" => {
            let num = is_f(&a0) || (es3 && (is_i(&a0) || is_u(&a0)));
            let s = Type::scalar(a0.scalar_kind().unwrap_or(Scalar::Float));
            (n == 3 && num && ((same(&a0, &a1) && same(&a0, &a2)) || (a1 == s && a2 == s)))
                .then_some((Bi::Clamp, a0))
        }
        "mix" => {
            if n == 3 && is_f(&a0) && same(&a0, &a1) && (same(&a0, &a2) || a2 == fl) {
                Some((Bi::Mix, a0))
            } else if es3
                && n == 3
                && is_f(&a0)
                && same(&a0, &a1)
                && is_b(&a2)
                && a2.rows == a0.rows
            {
                Some((Bi::MixBool, a0))
            } else {
                None
            }
        }
        "step" => (n == 2 && is_f(&a1) && (same(&a0, &a1) || a0 == fl)).then_some((Bi::Step, a1)),
        "smoothstep" => {
            (n == 3 && is_f(&a2) && ((same(&a0, &a2) && same(&a1, &a2)) || (a0 == fl && a1 == fl)))
                .then_some((Bi::Smoothstep, a2))
        }
        "modf" => (es3 && n == 2 && is_f(&a0) && same(&a0, &a1)).then_some((Bi::Modf, a0)),
        "length" => (n == 1 && is_f(&a0)).then_some((Bi::Length, fl)),
        "distance" => (n == 2 && is_f(&a0) && same(&a0, &a1)).then_some((Bi::Distance, fl)),
        "dot" => (n == 2 && is_f(&a0) && same(&a0, &a1)).then_some((Bi::Dot, fl)),
        "cross" => (n == 2 && a0 == Type::vec(Scalar::Float, 3) && same(&a0, &a1))
            .then_some((Bi::Cross, a0)),
        "normalize" => un_f(Bi::Normalize),
        "faceforward" => (n == 3 && is_f(&a0) && same(&a0, &a1) && same(&a0, &a2))
            .then_some((Bi::Faceforward, a0)),
        "reflect" => (n == 2 && is_f(&a0) && same(&a0, &a1)).then_some((Bi::Reflect, a0)),
        "refract" => {
            (n == 3 && is_f(&a0) && same(&a0, &a1) && a2 == fl).then_some((Bi::Refract, a0))
        }
        "matrixCompMult" => {
            (n == 2 && a0.is_matrix() && same(&a0, &a1) && (es3 || a0.rows == a0.cols))
                .then_some((Bi::MatrixCompMult, a0))
        }
        "outerProduct"
            if es3 && n == 2 && a0.is_vector() && is_f(&a0) && a1.is_vector() && is_f(&a1) =>
        {
            Some((Bi::OuterProduct, Type::mat(a1.rows, a0.rows)))
        }
        "transpose" if es3 && n == 1 && a0.is_matrix() => {
            Some((Bi::Transpose, Type::mat(a0.rows, a0.cols)))
        }
        "determinant" if es3 && n == 1 && a0.is_matrix() && a0.rows == a0.cols => {
            Some((Bi::Determinant, fl))
        }
        "inverse" if es3 && n == 1 && a0.is_matrix() && a0.rows == a0.cols => {
            Some((Bi::Inverse, a0))
        }
        "lessThan" | "lessThanEqual" | "greaterThan" | "greaterThanEqual" => {
            let bi = match name {
                "lessThan" => Bi::LessThan,
                "lessThanEqual" => Bi::LessThanEqual,
                "greaterThan" => Bi::GreaterThan,
                _ => Bi::GreaterThanEqual,
            };
            let num = a0.is_vector() && (is_f(&a0) || is_i(&a0) || (es3 && is_u(&a0)));
            (n == 2 && num && same(&a0, &a1)).then_some((bi, Type::vec(Scalar::Bool, a0.rows)))
        }
        "equal" | "notEqual" => {
            let bi = if name == "equal" {
                Bi::Equal
            } else {
                Bi::NotEqual
            };
            let ok = a0.is_vector() && (is_f(&a0) || is_i(&a0) || is_b(&a0) || (es3 && is_u(&a0)));
            (n == 2 && ok && same(&a0, &a1)).then_some((bi, Type::vec(Scalar::Bool, a0.rows)))
        }
        "any" | "all" => {
            let bi = if name == "any" { Bi::Any } else { Bi::All };
            (n == 1 && a0.is_vector() && is_b(&a0)).then_some((bi, Type::BOOL))
        }
        "not" => (n == 1 && a0.is_vector() && is_b(&a0)).then_some((Bi::Not, a0)),
        "packSnorm2x16" | "packUnorm2x16" | "packHalf2x16"
            if es3 && n == 1 && a0 == Type::vec(Scalar::Float, 2) =>
        {
            Some((
                match name {
                    "packSnorm2x16" => Bi::PackSnorm,
                    "packUnorm2x16" => Bi::PackUnorm,
                    _ => Bi::PackHalf,
                },
                Type::UINT,
            ))
        }
        "unpackSnorm2x16" | "unpackUnorm2x16" | "unpackHalf2x16"
            if es3 && n == 1 && a0 == Type::UINT =>
        {
            Some((
                match name {
                    "unpackSnorm2x16" => Bi::UnpackSnorm,
                    "unpackUnorm2x16" => Bi::UnpackUnorm,
                    _ => Bi::UnpackHalf,
                },
                Type::vec(Scalar::Float, 2),
            ))
        }
        "dFdx" | "dFdy" | "fwidth" if !c.vertex && (es3 || c.derivatives) => {
            let bi = match name {
                "dFdx" => Bi::DFdx,
                "dFdy" => Bi::DFdy,
                _ => Bi::Fwidth,
            };
            un_f(bi)
        }
        _ => texture(name, a, c),
    };
    r
}

fn coord_dims(dim: Dim) -> u8 {
    match dim {
        Dim::D2 => 2,
        Dim::D3 | Dim::Cube | Dim::D2Array => 3,
    }
}

fn texture(name: &str, a: &[Type], c: Ctx) -> Option<(Bi, Type)> {
    let Base::Sampler(s) = a.first()?.base else {
        return None;
    };
    if a[0].array > 0 {
        return None;
    }
    let es3 = c.version >= 300;
    let fv = |k: u8| Type::vec(Scalar::Float, k);
    let iv = |k: u8| Type::vec(Scalar::Int, k);
    let ret = if s.shadow {
        Type::FLOAT
    } else {
        Type::vec(s.ret, 4)
    };
    let args = &a[1..];
    let tex = |proj, lod, offset| Bi::Tex(TexOp { proj, lod, offset });
    if !es3 {
        let lod_ok = c.vertex;
        let ext_ok = !c.vertex && c.texture_lod;
        let d2 = s.dim == Dim::D2 && !s.shadow && s.ret == Scalar::Float;
        let cube = s.dim == Dim::Cube && !s.shadow && s.ret == Scalar::Float;
        let f = Type::FLOAT;
        let m = |want: &[Type]| args == want;
        let r = match name {
            "texture2D" if d2 && m(&[fv(2)]) => tex(false, Lod::Implicit, false),
            "texture2D" if d2 && !c.vertex && m(&[fv(2), f]) => tex(false, Lod::Bias, false),
            "texture2DProj" if d2 && (m(&[fv(3)]) || m(&[fv(4)])) => {
                tex(true, Lod::Implicit, false)
            }
            "texture2DProj" if d2 && !c.vertex && (m(&[fv(3), f]) || m(&[fv(4), f])) => {
                tex(true, Lod::Bias, false)
            }
            "textureCube" if cube && m(&[fv(3)]) => tex(false, Lod::Implicit, false),
            "textureCube" if cube && !c.vertex && m(&[fv(3), f]) => tex(false, Lod::Bias, false),
            "texture2DLod" if d2 && lod_ok && m(&[fv(2), f]) => tex(false, Lod::Explicit, false),
            "texture2DProjLod" if d2 && lod_ok && (m(&[fv(3), f]) || m(&[fv(4), f])) => {
                tex(true, Lod::Explicit, false)
            }
            "textureCubeLod" if cube && lod_ok && m(&[fv(3), f]) => {
                tex(false, Lod::Explicit, false)
            }
            "texture2DLodEXT" if d2 && ext_ok && m(&[fv(2), f]) => tex(false, Lod::Explicit, false),
            "texture2DProjLodEXT" if d2 && ext_ok && (m(&[fv(3), f]) || m(&[fv(4), f])) => {
                tex(true, Lod::Explicit, false)
            }
            "textureCubeLodEXT" if cube && ext_ok && m(&[fv(3), f]) => {
                tex(false, Lod::Explicit, false)
            }
            "texture2DGradEXT" if d2 && c.texture_lod && m(&[fv(2), fv(2), fv(2)]) => {
                tex(false, Lod::Grad, false)
            }
            "texture2DProjGradEXT"
                if d2
                    && c.texture_lod
                    && (m(&[fv(3), fv(2), fv(2)]) || m(&[fv(4), fv(2), fv(2)])) =>
            {
                tex(true, Lod::Grad, false)
            }
            "textureCubeGradEXT" if cube && c.texture_lod && m(&[fv(3), fv(3), fv(3)]) => {
                tex(false, Lod::Grad, false)
            }
            _ => return None,
        };
        return Some((r, ret));
    }
    let cd = coord_dims(s.dim) + u8::from(s.shadow);
    let od = if s.dim == Dim::D3 { 3 } else { 2 };
    let gd = if matches!(s.dim, Dim::D3 | Dim::Cube) {
        3
    } else {
        2
    };
    let f = Type::FLOAT;
    let cube_shadow = s.dim == Dim::Cube && s.shadow;
    let arr_shadow = s.dim == Dim::D2Array && s.shadow;
    // Projective coordinates: 2D takes vec3 or vec4, others vec4.
    let proj_ok = |t: &Type| match (s.dim, s.shadow) {
        (Dim::D2, false) => *t == fv(3) || *t == fv(4),
        (Dim::D2, true) | (Dim::D3, false) => *t == fv(4),
        _ => false,
    };
    let fragment = !c.vertex;
    let r = match name {
        "texture" => match args {
            [p] if *p == fv(cd) => tex(false, Lod::Implicit, false),
            [p, b] if *p == fv(cd) && *b == f && fragment && !arr_shadow => {
                tex(false, Lod::Bias, false)
            }
            _ => return None,
        },
        "textureProj" => match args {
            [p] if proj_ok(p) => tex(true, Lod::Implicit, false),
            [p, b] if proj_ok(p) && *b == f && fragment => tex(true, Lod::Bias, false),
            _ => return None,
        },
        "textureLod" => match args {
            [p, l] if *p == fv(cd) && *l == f && !cube_shadow && !arr_shadow => {
                tex(false, Lod::Explicit, false)
            }
            _ => return None,
        },
        "textureOffset" if s.dim != Dim::Cube && !arr_shadow => match args {
            [p, o] if *p == fv(cd) && *o == iv(od) => tex(false, Lod::Implicit, true),
            [p, o, b] if *p == fv(cd) && *o == iv(od) && *b == f && fragment => {
                tex(false, Lod::Bias, true)
            }
            _ => return None,
        },
        "texelFetch" if s.dim != Dim::Cube && !s.shadow => match args {
            [p, l] if *p == iv(coord_dims(s.dim)) && *l == Type::INT => Bi::TexFetch(false),
            _ => return None,
        },
        "texelFetchOffset" if s.dim != Dim::Cube && !s.shadow => match args {
            [p, l, o] if *p == iv(coord_dims(s.dim)) && *l == Type::INT && *o == iv(od) => {
                Bi::TexFetch(true)
            }
            _ => return None,
        },
        "textureProjOffset" => match args {
            [p, o] if proj_ok(p) && *o == iv(od) => tex(true, Lod::Implicit, true),
            [p, o, b] if proj_ok(p) && *o == iv(od) && *b == f && fragment => {
                tex(true, Lod::Bias, true)
            }
            _ => return None,
        },
        "textureLodOffset" if s.dim != Dim::Cube && !arr_shadow => match args {
            [p, l, o]
                if *p == fv(cd) && *l == f && *o == iv(od) && !(s.shadow && s.dim != Dim::D2) =>
            {
                tex(false, Lod::Explicit, true)
            }
            _ => return None,
        },
        "textureProjLod" => match args {
            [p, l] if proj_ok(p) && *l == f => tex(true, Lod::Explicit, false),
            _ => return None,
        },
        "textureProjLodOffset" => match args {
            [p, l, o] if proj_ok(p) && *l == f && *o == iv(od) => tex(true, Lod::Explicit, true),
            _ => return None,
        },
        "textureGrad" => match args {
            [p, x, y] if *p == fv(cd) && *x == fv(gd) && *y == fv(gd) => {
                tex(false, Lod::Grad, false)
            }
            _ => return None,
        },
        "textureGradOffset" if s.dim != Dim::Cube => match args {
            [p, x, y, o] if *p == fv(cd) && *x == fv(gd) && *y == fv(gd) && *o == iv(od) => {
                tex(false, Lod::Grad, true)
            }
            _ => return None,
        },
        "textureProjGrad" => match args {
            [p, x, y] if proj_ok(p) && *x == fv(gd) && *y == fv(gd) => tex(true, Lod::Grad, false),
            _ => return None,
        },
        "textureProjGradOffset" => match args {
            [p, x, y, o] if proj_ok(p) && *x == fv(gd) && *y == fv(gd) && *o == iv(od) => {
                tex(true, Lod::Grad, true)
            }
            _ => return None,
        },
        "textureSize" => match args {
            [l] if *l == Type::INT => {
                let k = if matches!(s.dim, Dim::D3 | Dim::D2Array) {
                    3
                } else {
                    2
                };
                return Some((Bi::TexSize, iv(k)));
            }
            _ => return None,
        },
        _ => return None,
    };
    Some((r, ret))
}

/// Whether `name` is any built-in function name (for redeclaration and
/// "no matching overloaded function" messages).
pub fn is_builtin_name(name: &str) -> bool {
    const NAMES: &[&str] = &[
        "radians",
        "degrees",
        "sin",
        "cos",
        "tan",
        "asin",
        "acos",
        "atan",
        "sinh",
        "cosh",
        "tanh",
        "asinh",
        "acosh",
        "atanh",
        "pow",
        "exp",
        "log",
        "exp2",
        "log2",
        "sqrt",
        "inversesqrt",
        "abs",
        "sign",
        "floor",
        "ceil",
        "fract",
        "trunc",
        "round",
        "roundEven",
        "isnan",
        "isinf",
        "floatBitsToInt",
        "floatBitsToUint",
        "intBitsToFloat",
        "uintBitsToFloat",
        "mod",
        "min",
        "max",
        "clamp",
        "mix",
        "step",
        "smoothstep",
        "modf",
        "length",
        "distance",
        "dot",
        "cross",
        "normalize",
        "faceforward",
        "reflect",
        "refract",
        "matrixCompMult",
        "outerProduct",
        "transpose",
        "determinant",
        "inverse",
        "lessThan",
        "lessThanEqual",
        "greaterThan",
        "greaterThanEqual",
        "equal",
        "notEqual",
        "any",
        "all",
        "not",
        "packSnorm2x16",
        "unpackSnorm2x16",
        "packUnorm2x16",
        "unpackUnorm2x16",
        "packHalf2x16",
        "unpackHalf2x16",
        "dFdx",
        "dFdy",
        "fwidth",
        "texture2D",
        "texture2DProj",
        "texture2DLod",
        "texture2DProjLod",
        "textureCube",
        "textureCubeLod",
        "texture2DLodEXT",
        "texture2DProjLodEXT",
        "textureCubeLodEXT",
        "texture2DGradEXT",
        "texture2DProjGradEXT",
        "textureCubeGradEXT",
        "texture",
        "textureProj",
        "textureLod",
        "textureOffset",
        "texelFetch",
        "texelFetchOffset",
        "textureProjOffset",
        "textureLodOffset",
        "textureProjLod",
        "textureProjLodOffset",
        "textureGrad",
        "textureGradOffset",
        "textureProjGrad",
        "textureProjGradOffset",
        "textureSize",
    ];
    NAMES.contains(&name)
}
