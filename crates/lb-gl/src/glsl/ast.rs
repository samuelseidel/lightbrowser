//! Typed syntax tree produced by the parser.

use std::rc::Rc;

use super::builtins::Bi;
use super::types::{Prec, Type};

pub type VarId = u32;
pub type FuncId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Builtin {
    Position,
    PointSize,
    VertexId,
    InstanceId,
    FragCoord,
    FrontFacing,
    PointCoord,
    FragColor,
    FragData,
    FragDepth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Storage {
    Local,
    Global,
    Const,
    Uniform,
    /// Vertex attribute or fragment varying input.
    Input,
    /// Vertex varying output or fragment output.
    Output,
    ParamIn,
    ParamOut,
    ParamInOut,
    Builtin(Builtin),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Interp {
    #[default]
    Smooth,
    Flat,
    NoPerspective,
}

#[derive(Clone, Debug)]
pub struct Var {
    pub name: String,
    pub ty: Type,
    pub prec: Prec,
    pub storage: Storage,
    /// Folded value of a `const` variable.
    pub cv: Option<Rc<Vec<u32>>>,
    pub interp: Interp,
    pub invariant: bool,
    pub location: Option<u32>,
    /// Uniform block this variable stands for (its struct type is the
    /// block's members).
    pub block: Option<u32>,
    pub used: bool,
    pub line: u32,
}

#[derive(Clone, Debug)]
pub struct UniformBlock {
    pub name: String,
    /// Instance name ("" for none).
    pub instance: String,
    pub var: VarId,
    pub binding: Option<u32>,
    /// Member row-major flags (`layout(row_major)`).
    pub row_major: Vec<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BOp {
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
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    LXor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub ty: Type,
    pub prec: Prec,
    /// Folded constant value (raw 32-bit cells).
    pub cv: Option<Rc<Vec<u32>>>,
    pub k: EK,
}

#[derive(Clone, Debug)]
pub enum EK {
    Var(VarId),
    Const,
    Bin(BOp, Box<Expr>, Box<Expr>),
    /// Short-circuit `&&` (true) / `||` (false).
    Logic(bool, Box<Expr>, Box<Expr>),
    Un(UOp, Box<Expr>),
    IncDec {
        pre: bool,
        inc: bool,
        e: Box<Expr>,
    },
    Assign(Option<BOp>, Box<Expr>, Box<Expr>),
    Swz(Box<Expr>, [u8; 4]),
    Index(Box<Expr>, Box<Expr>),
    Field(Box<Expr>, u32),
    Call(FuncId, Vec<Expr>),
    Bi(Bi, Vec<Expr>),
    Ctor(Vec<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    Comma(Box<Expr>, Box<Expr>),
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Expr(Expr),
    Decl(Vec<(VarId, Option<Expr>)>),
    Block(Vec<Stmt>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        inc: Option<Expr>,
        body: Box<Stmt>,
    },
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    /// Selector and segments: (labels — `None` for `default` — , body).
    Switch(Expr, Vec<(Vec<Option<u32>>, Vec<Stmt>)>),
    Break,
    Continue,
    Return(Option<Expr>),
    Discard,
    Empty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamQual {
    In,
    Out,
    InOut,
}

#[derive(Clone, Debug)]
pub struct Func {
    pub name: String,
    pub ret: Type,
    pub ret_prec: Prec,
    pub params: Vec<(VarId, ParamQual)>,
    pub param_types: Vec<(Type, ParamQual)>,
    pub body: Option<Vec<Stmt>>,
    pub calls: Vec<FuncId>,
    pub line: u32,
}
