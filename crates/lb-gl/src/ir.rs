//! Interpreter IR: structured control flow over per-cell operations.
//!
//! Every value lives in 32-bit cells (float bits, int, uint or 0/1 for
//! bool). An operation works on one cell across all lanes; control flow is
//! structured, so lanes diverge only by masks (see `exec`).

use std::collections::HashMap;

use crate::glsl::Stage;
use crate::glsl::ast::{Builtin, Interp};
use crate::glsl::types::{Dim, Prec, Scalar, StructDef, Type};

pub type Reg = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FOp {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    Pow,
    Atan2,
    Mod,
    /// step(edge = a, x = b)
    Step,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Shl,
    Shr,
    And,
    Or,
    Xor,
    Min,
    Max,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FUn {
    Neg,
    Abs,
    Sign,
    Floor,
    Ceil,
    Fract,
    Trunc,
    Round,
    Sqrt,
    InvSqrt,
    Exp,
    Log,
    Exp2,
    Log2,
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
    Radians,
    Degrees,
    IsNan,
    IsInf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IUn {
    Neg,
    Abs,
    Sign,
    Not,
    /// Boolean not (0/1).
    LNot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DerivKind {
    X,
    Y,
    Width,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackKind {
    Snorm,
    Unorm,
    Half,
}

/// A texture operation (cells of `coord` etc. are contiguous runs).
#[derive(Clone, Debug)]
pub struct TexArgs {
    /// Destination: 4 cells (or 1 for shadow lookups).
    pub d: Reg,
    /// Cell holding the texture unit.
    pub unit: Reg,
    pub dim: Dim,
    pub ret: Scalar,
    pub shadow: bool,
    /// Coordinate cells (already divided for projective lookups).
    pub coord: Vec<Reg>,
    /// Depth reference for shadow samplers.
    pub compare: Option<Reg>,
    pub bias: Option<Reg>,
    pub lod: Option<Reg>,
    pub grad: Option<(Vec<Reg>, Vec<Reg>)>,
    pub offset: Option<Vec<Reg>>,
    /// Implicit derivatives (fragment shaders).
    pub implicit: bool,
}

#[derive(Clone, Debug)]
pub enum Op {
    Const {
        d: Reg,
        v: u32,
    },
    Mov {
        d: Reg,
        s: Reg,
    },
    /// Masked move (writes active lanes only).
    MovM {
        d: Reg,
        s: Reg,
    },
    F {
        op: FOp,
        d: Reg,
        a: Reg,
        b: Reg,
    },
    /// Integer op; `u` selects unsigned semantics.
    I {
        op: IOp,
        u: bool,
        d: Reg,
        a: Reg,
        b: Reg,
    },
    Cmp {
        k: Scalar,
        op: CmpOp,
        d: Reg,
        a: Reg,
        b: Reg,
    },
    FU {
        op: FUn,
        d: Reg,
        a: Reg,
    },
    IU {
        op: IUn,
        d: Reg,
        a: Reg,
    },
    Cvt {
        from: Scalar,
        to: Scalar,
        d: Reg,
        a: Reg,
    },
    /// d = a * b + c
    Fma {
        d: Reg,
        a: Reg,
        b: Reg,
        c: Reg,
    },
    /// d = a * (1 - t) + b * t
    Mix {
        d: Reg,
        a: Reg,
        b: Reg,
        t: Reg,
    },
    Clamp {
        k: Scalar,
        d: Reg,
        x: Reg,
        lo: Reg,
        hi: Reg,
    },
    Smooth {
        d: Reg,
        e0: Reg,
        e1: Reg,
        x: Reg,
    },
    /// d = c ? a : b
    Select {
        d: Reg,
        c: Reg,
        a: Reg,
        b: Reg,
    },
    /// d = clamp(idx, 0, n - 1) * stride (+ add)
    Index {
        d: Reg,
        idx: Reg,
        n: u32,
        stride: u32,
        add: Option<Reg>,
        u: bool,
    },
    /// d = cell[base + off]
    LoadDyn {
        d: Reg,
        base: Reg,
        off: Reg,
    },
    /// cell[base + off] = s (masked)
    StoreDyn {
        base: Reg,
        off: Reg,
        s: Reg,
    },
    Deriv {
        k: DerivKind,
        d: Reg,
        s: Reg,
    },
    Pack {
        k: PackKind,
        d: Reg,
        a: Reg,
        b: Reg,
    },
    Unpack {
        k: PackKind,
        d0: Reg,
        d1: Reg,
        s: Reg,
    },
    /// Determinant (`inv` false) or inverse of an n x n matrix in
    /// contiguous cells.
    Mat {
        inv: bool,
        n: u8,
        d: Reg,
        s: Reg,
    },
    Tex(Box<TexArgs>),
    TexFetch {
        d: Reg,
        unit: Reg,
        dim: Dim,
        ret: Scalar,
        coord: Vec<Reg>,
        lod: Reg,
        offset: Option<Vec<Reg>>,
    },
    TexSize {
        d: Reg,
        unit: Reg,
        dim: Dim,
        lod: Reg,
    },
}

#[derive(Clone, Debug)]
pub enum Node {
    Ops(Vec<Op>),
    If {
        c: Reg,
        t: Vec<Node>,
        e: Vec<Node>,
    },
    Loop {
        pre: Vec<Node>,
        cond: Option<Reg>,
        body: Vec<Node>,
        cont: Vec<Node>,
        post_test: bool,
    },
    /// Scope that `Break` leaves (switch).
    Breakable(Vec<Node>),
    /// Scope that `Return` leaves (an inlined function).
    Func(Vec<Node>),
    Break,
    Continue,
    Return,
    Discard,
}

/// A shader interface variable.
#[derive(Clone, Debug)]
pub struct IoVar {
    pub name: String,
    pub ty: Type,
    pub prec: Prec,
    pub cell: Reg,
    pub used: bool,
    pub location: Option<u32>,
    pub interp: Interp,
    pub invariant: bool,
}

#[derive(Clone, Debug)]
pub struct BlockInfo {
    pub name: String,
    pub instance: String,
    /// Cells of the block's struct variable.
    pub cell: Reg,
    pub ty: Type,
    pub binding: Option<u32>,
    pub row_major: Vec<bool>,
    pub used: bool,
}

/// A compiled shader.
#[derive(Clone, Debug)]
pub struct Shader {
    pub stage: Stage,
    pub version: u32,
    pub cells: u32,
    pub code: Vec<Node>,
    pub uniforms: Vec<IoVar>,
    pub inputs: Vec<IoVar>,
    pub outputs: Vec<IoVar>,
    pub builtins: HashMap<Builtin, Reg>,
    pub structs: Vec<StructDef>,
    pub blocks: Vec<BlockInfo>,
    /// Number of `gl_FragData` entries (ES 1.00 with draw buffers).
    pub frag_data: u32,
    pub writes_frag_depth: bool,
    pub uses_point_size: bool,
}
