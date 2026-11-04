//! Recursive-descent parser with ANGLE's semantic checks; builds the typed
//! tree in one pass (like ANGLE's parse context).

use std::collections::HashMap;
use std::rc::Rc;

use super::ast::*;
use super::builtins::{self, Bi, Ctx};
use super::fold;
use super::pp::{ExtBehavior, PpOut, Tok, Token};
use super::types::*;
use super::{Log, Options, Stage};

/// A parsed shader.
pub struct Unit {
    pub version: u32,
    pub vars: Vec<Var>,
    pub funcs: Vec<Func>,
    pub structs: Vec<StructDef>,
    pub blocks: Vec<UniformBlock>,
    /// Global variables in declaration order with initializers.
    pub globals: Vec<(VarId, Option<Expr>)>,
    pub main: FuncId,
    /// Built-in variables by kind.
    pub builtins: HashMap<&'static str, VarId>,
    pub uses_frag_data: bool,
    pub uses_frag_color: bool,
}

#[derive(Clone, Debug)]
enum Sym {
    Var(VarId),
    Funcs(Vec<FuncId>),
    Struct(u32),
    /// Member of an unnamed uniform block: (block variable, member).
    Member(VarId, u32),
}

/// Syntax errors abort parsing (the message is already logged).
type R<T> = Result<T, ()>;

#[derive(Clone, Copy, Default)]
struct Quals {
    storage: Option<&'static str>,
    prec: Option<Prec>,
    interp: Interp,
    invariant: bool,
    centroid: bool,
    location: Option<u32>,
    binding: Option<u32>,
    row_major: bool,
    has_layout: bool,
}

const RESERVED_ES1: &[&str] = &[
    "asm",
    "class",
    "union",
    "enum",
    "typedef",
    "template",
    "this",
    "packed",
    "goto",
    "switch",
    "default",
    "inline",
    "noinline",
    "volatile",
    "public",
    "static",
    "extern",
    "external",
    "interface",
    "flat",
    "long",
    "short",
    "double",
    "half",
    "fixed",
    "unsigned",
    "superp",
    "input",
    "output",
    "hvec2",
    "hvec3",
    "hvec4",
    "dvec2",
    "dvec3",
    "dvec4",
    "fvec2",
    "fvec3",
    "fvec4",
    "sampler1D",
    "sampler3D",
    "sampler1DShadow",
    "sampler2DShadow",
    "sampler2DRect",
    "sampler3DRect",
    "sampler2DRectShadow",
    "sizeof",
    "cast",
    "namespace",
    "using",
    "case",
    "uint",
    "layout",
    "centroid",
    "smooth",
    "lowp_",
    "resource",
];
const RESERVED_ES3: &[&str] = &[
    "attribute",
    "varying",
    "coherent",
    "volatile",
    "restrict",
    "readonly",
    "writeonly",
    "resource",
    "atomic_uint",
    "noperspective",
    "patch",
    "sample",
    "subroutine",
    "common",
    "partition",
    "active",
    "asm",
    "class",
    "union",
    "enum",
    "typedef",
    "template",
    "this",
    "goto",
    "inline",
    "noinline",
    "public",
    "static",
    "extern",
    "external",
    "interface",
    "long",
    "short",
    "double",
    "half",
    "fixed",
    "unsigned",
    "superp",
    "input",
    "output",
    "hvec2",
    "hvec3",
    "hvec4",
    "dvec2",
    "dvec3",
    "dvec4",
    "fvec2",
    "fvec3",
    "fvec4",
    "sampler3DRect",
    "filter",
    "image1D",
    "image2D",
    "image3D",
    "imageCube",
    "sizeof",
    "cast",
    "namespace",
    "using",
];

pub struct Parser<'a> {
    toks: &'a [Token],
    pos: usize,
    log: &'a mut Log,
    version: u32,
    stage: Stage,
    last_line: u32,
    derivatives: bool,
    texture_lod: bool,
    frag_depth: bool,
    draw_buffers: bool,
    ext_enabled_webgl: Vec<&'static str>,
    pub vars: Vec<Var>,
    pub funcs: Vec<Func>,
    pub structs: Vec<StructDef>,
    pub blocks: Vec<UniformBlock>,
    globals: Vec<(VarId, Option<Expr>)>,
    scopes: Vec<HashMap<String, Sym>>,
    precs: Vec<HashMap<&'static str, Prec>>,
    cur_func: Option<FuncId>,
    loops: u32,
    switches: u32,
    returned: bool,
    builtins: HashMap<&'static str, VarId>,
    uses_frag_data: bool,
    uses_frag_color: bool,
    max_draw_buffers: u32,
    def_order: Vec<FuncId>,
}

fn zero_expr() -> Expr {
    Expr {
        ty: Type::FLOAT,
        prec: Prec::None,
        cv: Some(Rc::new(vec![0])),
        k: EK::Const,
    }
}

impl<'a> Parser<'a> {
    // ---------------------------------------------------------------
    // Tokens
    // ---------------------------------------------------------------

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.tok)
    }
    fn peek_at(&self, k: usize) -> Option<&Tok> {
        self.toks.get(self.pos + k).map(|t| &t.tok)
    }
    fn line(&self) -> u32 {
        self.toks
            .get(self.pos)
            .or_else(|| self.toks.last())
            .map_or(self.last_line, |t| t.line)
    }
    fn is_punct(&self, p: &str) -> bool {
        matches!(self.peek(), Some(Tok::Punct(x)) if *x == p)
    }
    fn is_ident(&self, s: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(x)) if x == s)
    }
    fn eat_punct(&mut self, p: &str) -> bool {
        if self.is_punct(p) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn eat_ident(&mut self, s: &str) -> bool {
        if self.is_ident(s) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn syntax_error<T>(&mut self) -> R<T> {
        match self.toks.get(self.pos) {
            Some(t) => {
                let (l, s) = (t.line, t.text.clone());
                if matches!(&t.tok, Tok::Ident(x) if self.is_reserved(x))
                    && !self.log.text.contains("Illegal use of reserved word")
                {
                    self.log.error(l, &s, "Illegal use of reserved word");
                }
                self.log.error(l, &s, "syntax error");
            }
            None => {
                if self.toks.is_empty() {
                    self.log.raw("ERROR: 1:1: '' : syntax error\n");
                } else {
                    let l = self.last_line;
                    self.log.error(l, "", "syntax error");
                }
            }
        }
        Err(())
    }
    fn expect(&mut self, p: &str) -> R<()> {
        if self.eat_punct(p) {
            Ok(())
        } else {
            self.syntax_error()
        }
    }
    fn ident(&mut self) -> R<String> {
        match self.peek() {
            Some(Tok::Ident(s)) if !self.is_keyword(s) => {
                let s = s.clone();
                self.pos += 1;
                Ok(s)
            }
            Some(Tok::Ident(s)) if self.is_reserved(s) => {
                let s = s.clone();
                let l = self.line();
                self.log.error(l, &s, "Illegal use of reserved word");
                self.syntax_error()
            }
            _ => self.syntax_error(),
        }
    }
    fn err(&mut self, line: u32, tok: &str, msg: &str) {
        self.log.error(line, tok, msg);
    }

    fn is_reserved(&self, s: &str) -> bool {
        if self.version >= 300 {
            RESERVED_ES3.contains(&s)
        } else {
            RESERVED_ES1.contains(&s)
        }
    }

    fn is_keyword(&self, s: &str) -> bool {
        const KW: &[&str] = &[
            "attribute",
            "const",
            "uniform",
            "varying",
            "break",
            "continue",
            "do",
            "for",
            "while",
            "if",
            "else",
            "in",
            "out",
            "inout",
            "true",
            "false",
            "lowp",
            "mediump",
            "highp",
            "precision",
            "invariant",
            "discard",
            "return",
            "struct",
        ];
        const KW3: &[&str] = &[
            "centroid", "flat", "smooth", "layout", "switch", "case", "default",
        ];
        if self.version >= 300 && matches!(s, "attribute" | "varying") {
            return true;
        }
        KW.contains(&s)
            || (self.version >= 300 && KW3.contains(&s))
            || type_keyword(s, self.version).is_some()
            || self.is_reserved(s)
    }

    // ---------------------------------------------------------------
    // Symbols
    // ---------------------------------------------------------------

    fn lookup(&self, name: &str) -> Option<&Sym> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }
    fn declare(&mut self, name: &str, sym: Sym, line: u32) -> bool {
        let scope = self.scopes.last_mut().expect("scope");
        if let Some(old) = scope.get(name) {
            let both_funcs = matches!((old, &sym), (Sym::Funcs(_), Sym::Funcs(_)));
            if !both_funcs {
                self.log.error(line, name, "redefinition");
                return false;
            }
        }
        scope.insert(name.to_owned(), sym);
        true
    }
    fn add_var(&mut self, v: Var) -> VarId {
        self.vars.push(v);
        (self.vars.len() - 1) as VarId
    }

    fn default_prec(&self, t: &Type) -> Prec {
        let key = match t.base {
            Base::S(Scalar::Float) => "float",
            Base::S(Scalar::Int | Scalar::Uint) => "int",
            Base::Sampler(_) => t.keyword().unwrap_or("sampler2D"),
            _ => return Prec::None,
        };
        self.precs
            .iter()
            .rev()
            .find_map(|m| m.get(key).copied())
            .unwrap_or(Prec::None)
    }

    fn type_name(&self, e: &Expr) -> String {
        let mut s = String::new();
        if e.cv.is_some() {
            s.push_str("const ");
        } else if let EK::Var(v) = e.k {
            match self.vars[v as usize].storage {
                Storage::Uniform => s.push_str("uniform "),
                Storage::Output
                | Storage::Builtin(
                    Builtin::Position
                    | Builtin::PointSize
                    | Builtin::FragColor
                    | Builtin::FragData
                    | Builtin::FragDepth,
                ) => s.push_str("out "),
                Storage::Input => s.push_str("in "),
                _ => {}
            }
        }
        if e.prec != Prec::None {
            s.push_str(e.prec.name());
            s.push(' ');
        }
        s.push_str(&e.ty.describe(&self.structs));
        s
    }

    // ---------------------------------------------------------------
    // Built-in variables
    // ---------------------------------------------------------------

    fn add_builtins(&mut self) {
        let hp = Prec::High;
        let mp = Prec::Medium;
        let add = |p: &mut Self, name: &'static str, ty: Type, prec: Prec, b: Builtin| {
            let id = p.add_var(Var {
                name: name.into(),
                ty,
                prec,
                storage: Storage::Builtin(b),
                cv: None,
                interp: Interp::Smooth,
                invariant: false,
                location: None,
                block: None,
                used: false,
                line: 0,
            });
            p.scopes[0].insert(name.into(), Sym::Var(id));
            p.builtins.insert(name, id);
        };
        let v4 = Type::vec(Scalar::Float, 4);
        match self.stage {
            Stage::Vertex => {
                add(self, "gl_Position", v4, hp, Builtin::Position);
                add(self, "gl_PointSize", Type::FLOAT, hp, Builtin::PointSize);
                if self.version >= 300 {
                    add(self, "gl_VertexID", Type::INT, hp, Builtin::VertexId);
                    add(self, "gl_InstanceID", Type::INT, hp, Builtin::InstanceId);
                }
            }
            Stage::Fragment => {
                add(self, "gl_FragCoord", v4, mp, Builtin::FragCoord);
                add(
                    self,
                    "gl_FrontFacing",
                    Type::BOOL,
                    Prec::None,
                    Builtin::FrontFacing,
                );
                add(
                    self,
                    "gl_PointCoord",
                    Type::vec(Scalar::Float, 2),
                    mp,
                    Builtin::PointCoord,
                );
                if self.version < 300 {
                    add(self, "gl_FragColor", v4, mp, Builtin::FragColor);
                    let n = if self.draw_buffers {
                        self.max_draw_buffers.max(1)
                    } else {
                        1
                    };
                    add(
                        self,
                        "gl_FragData",
                        Type { array: n, ..v4 },
                        mp,
                        Builtin::FragData,
                    );
                    if self.frag_depth {
                        add(self, "gl_FragDepthEXT", Type::FLOAT, hp, Builtin::FragDepth);
                    }
                } else {
                    add(self, "gl_FragDepth", Type::FLOAT, hp, Builtin::FragDepth);
                }
            }
        }
        // Built-in constants.
        let consts: &[(&str, i32)] = &[
            ("gl_MaxVertexAttribs", 16),
            ("gl_MaxVertexUniformVectors", 256),
            ("gl_MaxVaryingVectors", 15),
            ("gl_MaxVertexTextureImageUnits", 16),
            ("gl_MaxCombinedTextureImageUnits", 32),
            ("gl_MaxTextureImageUnits", 16),
            ("gl_MaxFragmentUniformVectors", 224),
            (
                "gl_MaxDrawBuffers",
                if self.draw_buffers || self.version >= 300 {
                    self.max_draw_buffers as i32
                } else {
                    1
                },
            ),
        ];
        for (n, v) in consts {
            let id = self.add_var(Var {
                name: (*n).into(),
                ty: Type::INT,
                prec: Prec::Medium,
                storage: Storage::Const,
                cv: Some(Rc::new(vec![*v as u32])),
                interp: Interp::Smooth,
                invariant: false,
                location: None,
                block: None,
                used: false,
                line: 0,
            });
            self.scopes[0].insert((*n).into(), Sym::Var(id));
        }
        if self.version >= 300 {
            for (n, v) in [
                ("gl_MaxVertexOutputVectors", 16),
                ("gl_MaxFragmentInputVectors", 15),
                ("gl_MinProgramTexelOffset", -8),
                ("gl_MaxProgramTexelOffset", 7),
            ] {
                let id = self.add_var(Var {
                    name: n.into(),
                    ty: Type::INT,
                    prec: Prec::Medium,
                    storage: Storage::Const,
                    cv: Some(Rc::new(vec![v as u32])),
                    interp: Interp::Smooth,
                    invariant: false,
                    location: None,
                    block: None,
                    used: false,
                    line: 0,
                });
                self.scopes[0].insert(n.into(), Sym::Var(id));
            }
        }
    }

    // ---------------------------------------------------------------
    // Types and qualifiers
    // ---------------------------------------------------------------

    fn is_type_start(&self) -> bool {
        match self.peek() {
            Some(Tok::Ident(s)) => {
                type_keyword(s, self.version).is_some()
                    || s == "struct"
                    || matches!(self.lookup(s), Some(Sym::Struct(_)))
            }
            _ => false,
        }
    }

    fn is_qualifier_start(&self) -> bool {
        match self.peek() {
            Some(Tok::Ident(s)) => {
                matches!(
                    s.as_str(),
                    "const" | "uniform" | "lowp" | "mediump" | "highp" | "invariant"
                ) || (self.version < 300 && matches!(s.as_str(), "attribute" | "varying"))
                    || (self.version >= 300
                        && matches!(
                            s.as_str(),
                            "in" | "out" | "centroid" | "flat" | "smooth" | "layout"
                        ))
            }
            _ => false,
        }
    }

    fn qualifiers(&mut self) -> R<Quals> {
        let mut q = Quals::default();
        loop {
            let Some(Tok::Ident(s)) = self.peek().cloned() else {
                break;
            };
            let line = self.line();
            match s.as_str() {
                "const" => q.storage = Some("const"),
                "uniform" => q.storage = Some("uniform"),
                "attribute" if self.version < 300 => {
                    if self.stage != Stage::Vertex {
                        self.err(line, "attribute", "supported in vertex shaders only");
                    }
                    q.storage = Some("attribute");
                }
                "varying" if self.version < 300 => q.storage = Some("varying"),
                "in" if self.version >= 300 => q.storage = Some("in"),
                "out" if self.version >= 300 => q.storage = Some("out"),
                "centroid" if self.version >= 300 => q.centroid = true,
                "flat" if self.version >= 300 => q.interp = Interp::Flat,
                "smooth" if self.version >= 300 => q.interp = Interp::Smooth,
                "invariant" => q.invariant = true,
                "lowp" => q.prec = Some(Prec::Low),
                "mediump" => q.prec = Some(Prec::Medium),
                "highp" => q.prec = Some(Prec::High),
                "layout" if self.version >= 300 => {
                    self.pos += 1;
                    self.expect("(")?;
                    q.has_layout = true;
                    loop {
                        let id = self.ident_any()?;
                        if self.eat_punct("=") {
                            let v = match self.peek().cloned() {
                                Some(Tok::Int(v, _)) => {
                                    self.pos += 1;
                                    v
                                }
                                _ => return self.syntax_error(),
                            };
                            match id.as_str() {
                                "location" => q.location = Some(v),
                                "binding" => q.binding = Some(v),
                                _ => self.err(line, &id, "invalid layout qualifier"),
                            }
                        } else {
                            match id.as_str() {
                                "row_major" => q.row_major = true,
                                "column_major" | "std140" | "shared" | "packed" => {}
                                _ => self.err(line, &id, "invalid layout qualifier"),
                            }
                        }
                        if !self.eat_punct(",") {
                            break;
                        }
                    }
                    self.expect(")")?;
                    continue;
                }
                _ => break,
            }
            self.pos += 1;
        }
        Ok(q)
    }

    fn ident_any(&mut self) -> R<String> {
        match self.peek() {
            Some(Tok::Ident(s)) => {
                let s = s.clone();
                self.pos += 1;
                Ok(s)
            }
            _ => self.syntax_error(),
        }
    }

    /// Type specifier (keyword, struct name or struct definition), with
    /// an optional `[N]` in ES 3.00.
    fn type_spec(&mut self) -> R<Type> {
        let line = self.line();
        let name = match self.peek() {
            Some(Tok::Ident(s)) => s.clone(),
            _ => return self.syntax_error(),
        };
        let mut t = if name == "struct" {
            self.pos += 1;
            self.struct_def()?
        } else if let Some(t) = type_keyword(&name, self.version) {
            self.pos += 1;
            t
        } else if let Some(Sym::Struct(i)) = self.lookup(&name) {
            let i = *i;
            self.pos += 1;
            Type {
                base: Base::Struct(i),
                rows: 1,
                cols: 1,
                array: 0,
            }
        } else {
            return self.syntax_error();
        };
        if self.version >= 300 && self.is_punct("[") {
            self.pos += 1;
            if self.eat_punct("]") {
                // Unsized: only valid in array constructors.
                t.array = u32::MAX;
            } else {
                let n = self.array_size(line)?;
                self.expect("]")?;
                t.array = n;
            }
        }
        Ok(t)
    }

    fn struct_def(&mut self) -> R<Type> {
        let line = self.line();
        let name = if let Some(Tok::Ident(_)) = self.peek() {
            self.ident()?
        } else {
            String::new()
        };
        self.expect("{")?;
        let mut fields: Vec<(String, Type, Prec)> = Vec::new();
        while !self.is_punct("}") {
            let q = self.qualifiers()?;
            let base = self.type_spec()?;
            loop {
                let fl = self.line();
                let fname = self.ident()?;
                let mut t = base;
                if self.eat_punct("[") {
                    let n = self.array_size(fl)?;
                    self.expect("]")?;
                    t.array = n;
                }
                if t.base == Base::Void {
                    self.err(fl, &fname, "illegal use of type 'void'");
                }
                if fields.iter().any(|f| f.0 == fname) {
                    self.err(fl, &fname, "duplicate field name in structure");
                }
                let p = q.prec.unwrap_or_else(|| self.default_prec(&t));
                fields.push((fname, t, p));
                if !self.eat_punct(",") {
                    break;
                }
            }
            self.expect(";")?;
        }
        self.expect("}")?;
        if fields.is_empty() {
            self.err(line, &name, "structure has no fields");
        }
        let idx = self.structs.len() as u32;
        self.structs.push(StructDef {
            name: name.clone(),
            fields,
        });
        if !name.is_empty() {
            self.declare(&name, Sym::Struct(idx), line);
        }
        Ok(Type {
            base: Base::Struct(idx),
            rows: 1,
            cols: 1,
            array: 0,
        })
    }

    fn array_size(&mut self, line: u32) -> R<u32> {
        let e = self.cond_expr()?;
        match (&e.cv, e.ty.scalar_kind()) {
            (Some(v), Some(Scalar::Int | Scalar::Uint)) if e.ty.is_scalar() => {
                let n = v[0] as i32;
                if n <= 0 {
                    self.err(line, "", "array size must be greater than zero");
                    Ok(1)
                } else {
                    Ok(n as u32)
                }
            }
            (None, _) => {
                self.err(line, "", "constant expression required");
                self.err(line, "", "array size must be a constant integer expression");
                Ok(1)
            }
            _ => {
                self.err(line, "", "array size must be a constant integer expression");
                Ok(1)
            }
        }
    }

    // ---------------------------------------------------------------
    // Top level
    // ---------------------------------------------------------------

    fn external(&mut self) -> R<()> {
        let line = self.line();
        if self.eat_punct(";") {
            return Ok(());
        }
        if self.eat_ident("precision") {
            let p = match self.peek() {
                Some(Tok::Ident(s)) if s == "lowp" => Prec::Low,
                Some(Tok::Ident(s)) if s == "mediump" => Prec::Medium,
                Some(Tok::Ident(s)) if s == "highp" => Prec::High,
                _ => return self.syntax_error(),
            };
            self.pos += 1;
            let t = self.type_spec()?;
            let key: &'static str = match t.base {
                Base::S(Scalar::Float) if t.is_scalar() => "float",
                Base::S(Scalar::Int) if t.is_scalar() => "int",
                Base::Sampler(_) => t.keyword().unwrap_or("sampler2D"),
                _ => {
                    self.err(
                        line,
                        "precision",
                        "illegal type argument for default precision qualifier",
                    );
                    "none"
                }
            };
            self.precs.last_mut().expect("precs").insert(key, p);
            return self.expect(";");
        }
        // `invariant name, ...;`
        if self.is_ident("invariant")
            && matches!(self.peek_at(1), Some(Tok::Ident(n)) if !self.is_keyword(n) && type_keyword(n, self.version).is_none())
        {
            self.pos += 1;
            loop {
                let l = self.line();
                let n = self.ident()?;
                match self.lookup(&n).cloned() {
                    Some(Sym::Var(v)) => self.vars[v as usize].invariant = true,
                    _ => self.err(l, &n, "undeclared identifier"),
                }
                if !self.eat_punct(",") {
                    break;
                }
            }
            return self.expect(";");
        }
        let q = self.qualifiers()?;
        // `layout(std140) uniform;`
        if q.has_layout && self.is_punct(";") {
            self.pos += 1;
            return Ok(());
        }
        // Interface block.
        if q.storage == Some("uniform")
            && self.version >= 300
            && matches!(self.peek(), Some(Tok::Ident(n)) if type_keyword(n, self.version).is_none() && !matches!(self.lookup(n), Some(Sym::Struct(_))))
            && self.peek_at(1) == Some(&Tok::Punct("{"))
        {
            return self.uniform_block(q);
        }
        let t = self.type_spec()?;
        if self.eat_punct(";") {
            // Struct-only or empty declaration.
            return Ok(());
        }
        let name_line = self.line();
        let name = self.ident()?;
        if self.is_punct("(") {
            return self.function(q, t, name, name_line);
        }
        self.declarators(q, t, name, name_line, true)?;
        Ok(())
    }

    fn uniform_block(&mut self, q: Quals) -> R<()> {
        let line = self.line();
        let bname = self.ident()?;
        self.expect("{")?;
        let mut fields = Vec::new();
        let mut row_major = Vec::new();
        while !self.is_punct("}") {
            let fq = self.qualifiers()?;
            let base = self.type_spec()?;
            loop {
                let fl = self.line();
                let fname = self.ident()?;
                let mut t = base;
                if self.eat_punct("[") {
                    let n = self.array_size(fl)?;
                    self.expect("]")?;
                    t.array = n;
                }
                if t.has_sampler(&self.structs) {
                    self.err(
                        fl,
                        &fname,
                        "unsupported type - sampler types are not allowed in interface blocks",
                    );
                }
                let p = fq.prec.unwrap_or_else(|| self.default_prec(&t));
                fields.push((fname, t, p));
                row_major.push(fq.row_major || q.row_major);
                if !self.eat_punct(",") {
                    break;
                }
            }
            self.expect(";")?;
        }
        self.expect("}")?;
        let sidx = self.structs.len() as u32;
        self.structs.push(StructDef {
            name: bname.clone(),
            fields: fields.clone(),
        });
        let mut ty = Type {
            base: Base::Struct(sidx),
            rows: 1,
            cols: 1,
            array: 0,
        };
        let mut instance = String::new();
        if let Some(Tok::Ident(_)) = self.peek() {
            instance = self.ident()?;
            if self.eat_punct("[") {
                let n = self.array_size(line)?;
                self.expect("]")?;
                ty.array = n;
            }
        }
        self.expect(";")?;
        let bidx = self.blocks.len() as u32;
        let vid = self.add_var(Var {
            name: if instance.is_empty() {
                bname.clone()
            } else {
                instance.clone()
            },
            ty,
            prec: Prec::None,
            storage: Storage::Uniform,
            cv: None,
            interp: Interp::Smooth,
            invariant: false,
            location: None,
            block: Some(bidx),
            used: false,
            line,
        });
        self.blocks.push(UniformBlock {
            name: bname.clone(),
            instance: instance.clone(),
            var: vid,
            binding: q.binding,
            row_major,
        });
        if instance.is_empty() {
            for (i, (fname, ..)) in fields.iter().enumerate() {
                self.declare(fname, Sym::Member(vid, i as u32), line);
            }
        } else {
            self.declare(&instance, Sym::Var(vid), line);
        }
        Ok(())
    }

    fn storage_of(&self, q: &Quals) -> Storage {
        match q.storage {
            Some("const") => Storage::Const,
            Some("uniform") => Storage::Uniform,
            Some("attribute") => Storage::Input,
            Some("varying") => {
                if self.stage == Stage::Vertex {
                    Storage::Output
                } else {
                    Storage::Input
                }
            }
            Some("in") => Storage::Input,
            Some("out") => Storage::Output,
            _ => {
                if self.cur_func.is_some() {
                    Storage::Local
                } else {
                    Storage::Global
                }
            }
        }
    }

    /// `name [= init] (, name2 ...)` after the type; returns the
    /// declaration statement.
    fn declarators(
        &mut self,
        q: Quals,
        t: Type,
        first: String,
        first_line: u32,
        global: bool,
    ) -> R<Stmt> {
        let mut decls = Vec::new();
        let mut name = first;
        let mut line = first_line;
        loop {
            let mut ty = t;
            if self.eat_punct("[") {
                if self.version >= 300 && self.is_punct("]") {
                    self.pos += 1;
                    ty.array = u32::MAX;
                } else {
                    let n = self.array_size(line)?;
                    self.expect("]")?;
                    ty.array = n;
                }
            }
            let init = if self.eat_punct("=") {
                Some(self.assign_expr()?)
            } else {
                None
            };
            if let Some(d) = self.declare_var(&q, ty, &name, line, init, global) {
                decls.push(d);
            }
            if !self.eat_punct(",") {
                break;
            }
            line = self.line();
            name = self.ident()?;
        }
        self.expect(";")?;
        Ok(Stmt::Decl(decls))
    }

    fn declare_var(
        &mut self,
        q: &Quals,
        mut ty: Type,
        name: &str,
        line: u32,
        init: Option<Expr>,
        global: bool,
    ) -> Option<(VarId, Option<Expr>)> {
        let storage = self.storage_of(q);
        if ty.base == Base::Void {
            self.err(line, name, "illegal use of type 'void'");
            return None;
        }
        if name.starts_with("gl_") {
            self.err(line, name, "reserved built-in name");
            return None;
        }
        if name.contains("__") {
            self.log.warning(line, name, "all identifiers containing two consecutive underscores (__) are reserved - unintented behaviors are possible");
        }
        if ty.array == u32::MAX {
            match &init {
                Some(e) if e.ty.is_array() => ty.array = e.ty.array,
                _ => {
                    self.err(line, name, "implicitly sized arrays need to be initialized");
                    ty.array = 1;
                }
            }
        }
        match storage {
            Storage::Uniform | Storage::Input | Storage::Output if !global => {
                self.err(
                    line,
                    q.storage.unwrap_or(""),
                    "only allowed at global scope",
                );
            }
            _ => {}
        }
        if storage == Storage::Input && self.stage == Stage::Vertex {
            if ty.is_struct()
                || ty.base == Base::S(Scalar::Bool)
                || (self.version < 300 && ty.is_array())
            {
                self.err(line, name, "cannot be bool, struct or array");
            }
        }
        if ty.is_sampler()
            && !matches!(
                storage,
                Storage::Uniform | Storage::Local | Storage::ParamIn
            )
        {
            self.err(line, q.storage.unwrap_or(""), "samplers must be uniform");
        }
        let prec = match q.prec {
            Some(p) => p,
            None => {
                let p = self.default_prec(&ty);
                let needs = matches!(
                    ty.base,
                    Base::S(Scalar::Float | Scalar::Int | Scalar::Uint) | Base::Sampler(_)
                );
                if p == Prec::None && needs && storage != Storage::Const {
                    let what = match ty.base {
                        Base::S(Scalar::Float) => "float",
                        Base::S(_) => "int",
                        _ => ty.keyword().unwrap_or("sampler"),
                    };
                    self.err(line, "", &format!("No precision specified for ({what})"));
                }
                p
            }
        };
        let mut cv = None;
        if let Some(e) = &init {
            match storage {
                Storage::Uniform | Storage::Input | Storage::Output => {
                    self.err(
                        line,
                        q.storage.unwrap_or(""),
                        " cannot initialize this type of qualifier ",
                    );
                    return self.declare_plain(q, ty, prec, storage, name, line, global);
                }
                _ => {}
            }
            if !self.assignable(&ty, &e.ty) {
                let target = format!(
                    "{}{}{}",
                    if storage == Storage::Const {
                        "const "
                    } else {
                        ""
                    },
                    if prec != Prec::None {
                        format!("{} ", prec.name())
                    } else {
                        String::new()
                    },
                    ty.describe(&self.structs)
                );
                let from = self.type_name(e);
                self.err(
                    line,
                    "=",
                    &format!("cannot convert from '{from}' to '{target}'"),
                );
                return self.declare_plain(q, ty, prec, storage, name, line, global);
            }
            if storage == Storage::Const {
                match &e.cv {
                    Some(v) => cv = Some(v.clone()),
                    None => {
                        if self.version < 300 || !self.is_const_like(e) {
                            self.err(line, "=", "assigning non-constant to 'const highp float'");
                        }
                    }
                }
            }
            if global && self.version < 300 && e.cv.is_none() && storage != Storage::Const {
                // ES 1.00 globals need constant initializers.
                self.err(
                    line,
                    "=",
                    "global variable initializers must be constant expressions",
                );
            }
        } else if storage == Storage::Const {
            self.err(
                line,
                name,
                "variables with qualifier 'const' must be initialized",
            );
            return self.declare_plain(q, ty, prec, storage, name, line, global);
        }
        let id = self.add_var(Var {
            name: name.to_owned(),
            ty,
            prec,
            storage,
            cv,
            interp: q.interp,
            invariant: q.invariant,
            location: q.location,
            block: None,
            used: false,
            line,
        });
        if !self.declare(name, Sym::Var(id), line) {
            return None;
        }
        if global {
            self.globals.push((id, init.clone()));
        }
        Some((id, init))
    }

    /// Declares a variable after an initializer error (ANGLE keeps it).
    #[allow(clippy::too_many_arguments)]
    fn declare_plain(
        &mut self,
        q: &Quals,
        ty: Type,
        prec: Prec,
        storage: Storage,
        name: &str,
        line: u32,
        global: bool,
    ) -> Option<(VarId, Option<Expr>)> {
        let id = self.add_var(Var {
            name: name.to_owned(),
            ty,
            prec,
            storage: if storage == Storage::Const {
                Storage::Global
            } else {
                storage
            },
            cv: None,
            interp: q.interp,
            invariant: q.invariant,
            location: q.location,
            block: None,
            used: false,
            line,
        });
        if !self.declare(name, Sym::Var(id), line) {
            return None;
        }
        if global {
            self.globals.push((id, None));
        }
        Some((id, None))
    }

    fn is_const_like(&self, _e: &Expr) -> bool {
        false
    }

    fn assignable(&self, to: &Type, from: &Type) -> bool {
        to == from
    }

    fn function(&mut self, _q: Quals, ret: Type, name: String, line: u32) -> R<()> {
        self.expect("(")?;
        let mut params: Vec<(String, Type, Prec, ParamQual, u32)> = Vec::new();
        if !self.is_punct(")") {
            // `void` alone.
            if self.is_ident("void") && self.peek_at(1) == Some(&Tok::Punct(")")) {
                self.pos += 1;
            } else {
                loop {
                    let pl = self.line();
                    let mut pq = ParamQual::In;
                    let mut prec = None;
                    let mut is_const = false;
                    loop {
                        match self.peek() {
                            Some(Tok::Ident(s)) if s == "in" => pq = ParamQual::In,
                            Some(Tok::Ident(s)) if s == "out" => pq = ParamQual::Out,
                            Some(Tok::Ident(s)) if s == "inout" => pq = ParamQual::InOut,
                            Some(Tok::Ident(s)) if s == "const" => is_const = true,
                            Some(Tok::Ident(s)) if s == "lowp" => prec = Some(Prec::Low),
                            Some(Tok::Ident(s)) if s == "mediump" => prec = Some(Prec::Medium),
                            Some(Tok::Ident(s)) if s == "highp" => prec = Some(Prec::High),
                            _ => break,
                        }
                        self.pos += 1;
                    }
                    let _ = is_const;
                    let mut t = self.type_spec()?;
                    let pname = if let Some(Tok::Ident(s)) = self.peek()
                        && !self.is_keyword(s)
                    {
                        self.ident()?
                    } else {
                        String::new()
                    };
                    if self.eat_punct("[") {
                        let n = self.array_size(pl)?;
                        self.expect("]")?;
                        t.array = n;
                    }
                    if t.base == Base::Void {
                        self.err(pl, &pname, "illegal use of type 'void'");
                    }
                    let p = prec.unwrap_or_else(|| self.default_prec(&t));
                    params.push((pname, t, p, pq, pl));
                    if !self.eat_punct(",") {
                        break;
                    }
                }
            }
        }
        self.expect(")")?;
        let ptypes: Vec<(Type, ParamQual)> = params.iter().map(|p| (p.1, p.3)).collect();
        // Find an existing prototype.
        let existing = match self.scopes[0].get(&name) {
            Some(Sym::Funcs(list)) => list.iter().copied().find(|f| {
                self.funcs[*f as usize]
                    .param_types
                    .iter()
                    .map(|p| p.0)
                    .eq(ptypes.iter().map(|p| p.0))
            }),
            Some(_) => {
                self.err(line, &name, "redefinition");
                None
            }
            None => None,
        };
        if self.version >= 300 && builtins::is_builtin_name(&name) {
            self.err(
                line,
                &name,
                "Name of a built-in function cannot be redeclared as function",
            );
        }
        if name == "main" && (!params.is_empty() || ret != Type::VOID) {
            self.err(line, "main", "function cannot take any parameter(s)");
        }
        let fid = match existing {
            Some(f) => {
                if self.funcs[f as usize].ret != ret {
                    self.err(
                        line,
                        &name,
                        "function must have the same return type in all of its declarations",
                    );
                }
                if self.funcs[f as usize]
                    .param_types
                    .iter()
                    .map(|p| p.1)
                    .ne(ptypes.iter().map(|p| p.1))
                {
                    self.err(line, &name, "function must have the same parameter qualifiers in all of its declarations");
                }
                f
            }
            None => {
                self.funcs.push(Func {
                    name: name.clone(),
                    ret,
                    ret_prec: if ret.base == Base::Void {
                        Prec::None
                    } else {
                        self.default_prec(&ret)
                    },
                    params: Vec::new(),
                    param_types: ptypes.clone(),
                    body: None,
                    calls: Vec::new(),
                    line,
                });
                let f = (self.funcs.len() - 1) as FuncId;
                let scope = &mut self.scopes[0];
                match scope.get_mut(&name) {
                    Some(Sym::Funcs(list)) => list.push(f),
                    Some(_) => {}
                    None => {
                        scope.insert(name.clone(), Sym::Funcs(vec![f]));
                    }
                }
                f
            }
        };
        if self.eat_punct(";") {
            return Ok(());
        }
        if !self.is_punct("{") {
            return self.syntax_error();
        }
        if self.funcs[fid as usize].body.is_some() {
            self.err(line, &name, "function already has a body");
        }
        // Definition.
        self.scopes.push(HashMap::new());
        self.precs.push(HashMap::new());
        let mut pids = Vec::new();
        for (pname, t, p, pq, pl) in params {
            let id = self.add_var(Var {
                name: pname.clone(),
                ty: t,
                prec: p,
                storage: match pq {
                    ParamQual::In => Storage::ParamIn,
                    ParamQual::Out => Storage::ParamOut,
                    ParamQual::InOut => Storage::ParamInOut,
                },
                cv: None,
                interp: Interp::Smooth,
                invariant: false,
                location: None,
                block: None,
                used: false,
                line: pl,
            });
            if !pname.is_empty() {
                self.declare(&pname, Sym::Var(id), pl);
            }
            pids.push((id, pq));
        }
        self.funcs[fid as usize].params = pids;
        self.cur_func = Some(fid);
        self.returned = false;
        self.pos += 1;
        let mut body = Vec::new();
        while !self.is_punct("}") {
            if self.peek().is_none() {
                return self.syntax_error();
            }
            body.push(self.statement()?);
        }
        self.pos += 1;
        self.scopes.pop();
        self.precs.pop();
        if ret.base != Base::Void && !self.returned {
            self.err(line, &name, "Function does not return a value");
        }
        self.cur_func = None;
        self.funcs[fid as usize].body = Some(body);
        self.def_order.push(fid);
        Ok(())
    }

    // ---------------------------------------------------------------
    // Statements
    // ---------------------------------------------------------------

    fn statement(&mut self) -> R<Stmt> {
        let line = self.line();
        if self.is_punct("{") {
            self.pos += 1;
            self.scopes.push(HashMap::new());
            self.precs.push(HashMap::new());
            let mut list = Vec::new();
            while !self.is_punct("}") {
                if self.peek().is_none() {
                    return self.syntax_error();
                }
                list.push(self.statement()?);
            }
            self.pos += 1;
            self.scopes.pop();
            self.precs.pop();
            return Ok(Stmt::Block(list));
        }
        if self.eat_punct(";") {
            return Ok(Stmt::Empty);
        }
        let kw = match self.peek() {
            Some(Tok::Ident(s)) => s.clone(),
            _ => String::new(),
        };
        match kw.as_str() {
            "if" => {
                self.pos += 1;
                self.expect("(")?;
                let c = self.expr()?;
                self.check_bool(&c, line);
                self.expect(")")?;
                let t = self.scoped_statement()?;
                let e = if self.eat_ident("else") {
                    Some(Box::new(self.scoped_statement()?))
                } else {
                    None
                };
                return Ok(Stmt::If(c, Box::new(t), e));
            }
            "for" => return self.for_loop(),
            "while" => {
                self.pos += 1;
                if self.version < 300 {
                    self.err(line, "while", "This type of loop is not allowed");
                }
                self.expect("(")?;
                let c = self.expr()?;
                self.check_bool(&c, line);
                self.expect(")")?;
                self.loops += 1;
                let body = self.scoped_statement()?;
                self.loops -= 1;
                return Ok(Stmt::While(c, Box::new(body)));
            }
            "do" => {
                self.pos += 1;
                if self.version < 300 {
                    self.err(line, "do", "This type of loop is not allowed");
                }
                self.loops += 1;
                let body = self.scoped_statement()?;
                self.loops -= 1;
                if !self.eat_ident("while") {
                    return self.syntax_error();
                }
                self.expect("(")?;
                let c = self.expr()?;
                self.check_bool(&c, line);
                self.expect(")")?;
                self.expect(";")?;
                return Ok(Stmt::DoWhile(Box::new(body), c));
            }
            "switch" if self.version >= 300 => return self.switch(),
            "break" => {
                self.pos += 1;
                if self.loops == 0 && self.switches == 0 {
                    self.err(
                        line,
                        "",
                        "break statement only allowed in loops and switch statements",
                    );
                }
                self.expect(";")?;
                return Ok(Stmt::Break);
            }
            "continue" => {
                self.pos += 1;
                if self.loops == 0 {
                    self.err(line, "", "continue statement only allowed in loops");
                }
                self.expect(";")?;
                return Ok(Stmt::Continue);
            }
            "discard" => {
                self.pos += 1;
                if self.stage != Stage::Fragment {
                    self.err(
                        line,
                        "discard",
                        "discard supported in fragment shaders only",
                    );
                }
                self.expect(";")?;
                return Ok(Stmt::Discard);
            }
            "return" => {
                self.pos += 1;
                let f = self.cur_func.expect("in function");
                let ret = self.funcs[f as usize].ret;
                if self.eat_punct(";") {
                    if ret.base != Base::Void {
                        self.err(line, "return", "non-void function must return a value");
                    }
                    return Ok(Stmt::Return(None));
                }
                let e = self.expr()?;
                self.expect(";")?;
                self.returned = true;
                if ret.base == Base::Void {
                    self.err(line, "return", "void function cannot return a value");
                } else if e.ty != ret {
                    self.err(line, "return", "function return is not matching type:");
                }
                return Ok(Stmt::Return(Some(e)));
            }
            _ => {}
        }
        // Declaration?
        if self.is_qualifier_start()
            || (self.is_type_start() && self.decl_follows())
            || self.is_ident("precision")
        {
            if self.eat_ident("precision") {
                let p = match self.peek() {
                    Some(Tok::Ident(s)) if s == "lowp" => Prec::Low,
                    Some(Tok::Ident(s)) if s == "mediump" => Prec::Medium,
                    Some(Tok::Ident(s)) if s == "highp" => Prec::High,
                    _ => return self.syntax_error(),
                };
                self.pos += 1;
                let t = self.type_spec()?;
                let key: &'static str = match t.base {
                    Base::S(Scalar::Float) => "float",
                    Base::S(Scalar::Int) => "int",
                    Base::Sampler(_) => t.keyword().unwrap_or("sampler2D"),
                    _ => "none",
                };
                self.precs.last_mut().expect("precs").insert(key, p);
                self.expect(";")?;
                return Ok(Stmt::Empty);
            }
            let q = self.qualifiers()?;
            let t = self.type_spec()?;
            if self.eat_punct(";") {
                return Ok(Stmt::Empty);
            }
            let l = self.line();
            let name = self.ident()?;
            return self.declarators(q, t, name, l, false);
        }
        let e = self.expr()?;
        self.expect(";")?;
        Ok(Stmt::Expr(e))
    }

    /// After a type at statement start: a declaration (type followed by an
    /// identifier or `[`), not a constructor call.
    fn decl_follows(&self) -> bool {
        let mut k = 1;
        if self.is_ident("struct") {
            return true;
        }
        if self.peek_at(1) == Some(&Tok::Punct("[")) {
            // `float[2] a` (ES3) vs `float[2](...)`
            let mut depth = 0;
            loop {
                match self.peek_at(k) {
                    Some(Tok::Punct("[")) => depth += 1,
                    Some(Tok::Punct("]")) => {
                        depth -= 1;
                        if depth == 0 {
                            k += 1;
                            break;
                        }
                    }
                    None => return false,
                    _ => {}
                }
                k += 1;
            }
        }
        matches!(self.peek_at(k), Some(Tok::Ident(_)))
    }

    fn scoped_statement(&mut self) -> R<Stmt> {
        self.scopes.push(HashMap::new());
        let s = self.statement();
        self.scopes.pop();
        s
    }

    fn check_bool(&mut self, e: &Expr, line: u32) {
        if e.ty != Type::BOOL {
            self.err(line, "", "boolean expression expected");
            self.err(line, "", "boolean expression expected");
        }
    }

    fn for_loop(&mut self) -> R<Stmt> {
        let line = self.line();
        self.pos += 1;
        self.expect("(")?;
        self.scopes.push(HashMap::new());
        let init = if self.eat_punct(";") {
            None
        } else {
            Some(Box::new(self.statement()?))
        };
        let cond = if self.is_punct(";") {
            None
        } else {
            let c = self.expr()?;
            self.check_bool(&c, line);
            Some(c)
        };
        self.expect(";")?;
        let inc = if self.is_punct(")") {
            None
        } else {
            Some(self.expr()?)
        };
        self.expect(")")?;
        if self.version < 300 {
            self.validate_es1_loop(line, init.as_deref(), cond.as_ref(), inc.as_ref());
        }
        self.loops += 1;
        let body = self.statement();
        self.loops -= 1;
        self.scopes.pop();
        Ok(Stmt::For {
            init,
            cond,
            inc,
            body: Box::new(body?),
        })
    }

    /// ES 1.00 Appendix A loop limitations (ANGLE's ValidateLimitations).
    fn validate_es1_loop(
        &mut self,
        line: u32,
        init: Option<&Stmt>,
        cond: Option<&Expr>,
        inc: Option<&Expr>,
    ) {
        let index = match init {
            Some(Stmt::Decl(d)) if d.len() == 1 => {
                let (v, e) = &d[0];
                let var = &self.vars[*v as usize];
                if !matches!(var.ty.base, Base::S(Scalar::Float | Scalar::Int))
                    || !var.ty.is_scalar()
                {
                    self.err(line, &var.name.clone(), "Invalid type for loop index");
                    return;
                }
                if e.as_ref().is_none_or(|e| e.cv.is_none()) {
                    self.err(
                        line,
                        &var.name.clone(),
                        "Loop index cannot be initialized with non-constant expression",
                    );
                    return;
                }
                *v
            }
            _ => {
                self.err(line, "for", "Missing init declaration");
                return;
            }
        };
        let name = self.vars[index as usize].name.clone();
        match cond {
            Some(Expr {
                k: EK::Bin(op, a, b),
                ..
            }) if matches!(
                op,
                BOp::Lt | BOp::Gt | BOp::Le | BOp::Ge | BOp::Eq | BOp::Ne
            ) && matches!(a.k, EK::Var(v) if v == index) =>
            {
                if b.cv.is_none() {
                    self.err(
                        line,
                        &name,
                        "Loop index cannot be compared with non-constant expression",
                    );
                }
            }
            _ => self.err(line, "for", "Invalid condition"),
        }
        match inc {
            Some(Expr {
                k: EK::IncDec { e, .. },
                ..
            }) if matches!(e.k, EK::Var(v) if v == index) => {}
            Some(Expr {
                k: EK::Assign(Some(BOp::Add | BOp::Sub), a, b),
                ..
            }) if matches!(a.k, EK::Var(v) if v == index) => {
                if b.cv.is_none() {
                    self.err(
                        line,
                        &name,
                        "Loop index cannot be modified by non-constant expression",
                    );
                }
            }
            _ => self.err(line, "for", "Invalid expression"),
        }
    }

    fn switch(&mut self) -> R<Stmt> {
        let line = self.line();
        self.pos += 1;
        self.expect("(")?;
        let sel = self.expr()?;
        if !(sel.ty == Type::INT || sel.ty == Type::UINT) {
            self.err(
                line,
                "switch",
                "init-expression in a switch statement must be a scalar integer",
            );
        }
        self.expect(")")?;
        self.expect("{")?;
        self.switches += 1;
        self.scopes.push(HashMap::new());
        let mut segs: Vec<(Vec<Option<u32>>, Vec<Stmt>)> = Vec::new();
        let mut seen = Vec::new();
        while !self.is_punct("}") {
            if self.peek().is_none() {
                return self.syntax_error();
            }
            let l = self.line();
            if self.eat_ident("case") {
                let e = self.cond_expr()?;
                self.expect(":")?;
                let v = match (&e.cv, e.ty == sel.ty) {
                    (Some(v), true) => Some(v[0]),
                    _ => {
                        self.err(l, "case", "case label must be a scalar integer constant of the same type as the switch init-expression");
                        None
                    }
                };
                if let Some(v) = v {
                    if seen.contains(&Some(v)) {
                        self.err(l, "case", "duplicate case label");
                    }
                    seen.push(Some(v));
                }
                match segs.last_mut() {
                    Some(s) if s.1.is_empty() => s.0.push(v),
                    _ => segs.push((vec![v], Vec::new())),
                }
                continue;
            }
            if self.eat_ident("default") {
                self.expect(":")?;
                if seen.contains(&None) {
                    self.err(l, "default", "duplicate default label");
                }
                seen.push(None);
                match segs.last_mut() {
                    Some(s) if s.1.is_empty() => s.0.push(None),
                    _ => segs.push((vec![None], Vec::new())),
                }
                continue;
            }
            let st = self.statement()?;
            match segs.last_mut() {
                Some(s) => s.1.push(st),
                None => {
                    self.err(l, "switch", "statement before the first label");
                    segs.push((Vec::new(), vec![st]));
                }
            }
        }
        self.pos += 1;
        self.scopes.pop();
        self.switches -= 1;
        // Labels map to u32 (sel type int or uint).
        let segs = segs
            .into_iter()
            .map(|(ls, b)| {
                (
                    ls.into_iter()
                        .map(|l| l.map(Some).unwrap_or(None))
                        .collect::<Vec<Option<u32>>>(),
                    b,
                )
            })
            .collect();
        Ok(Stmt::Switch(sel, segs))
    }

    // ---------------------------------------------------------------
    // Expressions
    // ---------------------------------------------------------------

    pub fn expr(&mut self) -> R<Expr> {
        let mut e = self.assign_expr()?;
        while self.eat_punct(",") {
            let r = self.assign_expr()?;
            let ty = r.ty;
            let prec = r.prec;
            e = Expr {
                ty,
                prec,
                cv: None,
                k: EK::Comma(Box::new(e), Box::new(r)),
            };
        }
        Ok(e)
    }

    fn assign_expr(&mut self) -> R<Expr> {
        let lhs = self.cond_expr()?;
        let line = self.line();
        let op = match self.peek() {
            Some(Tok::Punct(p)) => match *p {
                "=" => Some(None),
                "+=" => Some(Some(BOp::Add)),
                "-=" => Some(Some(BOp::Sub)),
                "*=" => Some(Some(BOp::Mul)),
                "/=" => Some(Some(BOp::Div)),
                "%=" => Some(Some(BOp::Mod)),
                "<<=" => Some(Some(BOp::Shl)),
                ">>=" => Some(Some(BOp::Shr)),
                "&=" => Some(Some(BOp::And)),
                "^=" => Some(Some(BOp::Xor)),
                "|=" => Some(Some(BOp::Or)),
                _ => None,
            },
            _ => None,
        };
        let Some(op) = op else { return Ok(lhs) };
        let tok = match self.peek() {
            Some(Tok::Punct(p)) => *p,
            _ => "=",
        };
        self.pos += 1;
        if self.version < 300
            && matches!(
                op,
                Some(BOp::Mod | BOp::Shl | BOp::Shr | BOp::And | BOp::Xor | BOp::Or)
            )
        {
            self.err(line, tok, "supported in GLSL ES 3.00 and above only");
        }
        let rhs = self.assign_expr()?;
        let ok_lv = self.check_lvalue(&lhs, line);
        match op {
            None => {
                if lhs.ty != rhs.ty {
                    if lhs.ty.comps() != rhs.ty.comps() || lhs.ty.is_matrix() != rhs.ty.is_matrix()
                    {
                        self.err(line, "=", "dimension mismatch");
                    }
                    let (f, t) = (self.type_name(&rhs), self.type_name(&lhs));
                    self.err(
                        line,
                        "assign",
                        &format!("cannot convert from '{f}' to '{t}'"),
                    );
                    return Ok(lhs);
                }
                if lhs.ty.has_sampler(&self.structs) {
                    self.err(line, "=", "sampler can't be l-values");
                }
                if self.version >= 300 && lhs.ty.has_array(&self.structs) && false {
                    self.err(line, "=", "cannot assign arrays");
                }
                let _ = ok_lv;
                let ty = lhs.ty;
                let prec = lhs.prec;
                Ok(Expr {
                    ty,
                    prec,
                    cv: None,
                    k: EK::Assign(None, Box::new(lhs), Box::new(rhs)),
                })
            }
            Some(b) => {
                let r = self.binary_typed(b, tok, lhs.clone(), rhs.clone(), line);
                let Some(r) = r else { return Ok(lhs) };
                if r.ty != lhs.ty {
                    let (f, t) = (self.type_name(&r), self.type_name(&lhs));
                    self.err(line, tok, &format!("cannot convert from '{f}' to '{t}'"));
                    return Ok(lhs);
                }
                let ty = lhs.ty;
                let prec = lhs.prec;
                Ok(Expr {
                    ty,
                    prec,
                    cv: None,
                    k: EK::Assign(Some(b), Box::new(lhs), Box::new(rhs)),
                })
            }
        }
    }

    /// Checks an l-value; logs ANGLE's message.
    fn check_lvalue(&mut self, e: &Expr, line: u32) -> bool {
        match &e.k {
            EK::Var(v) => {
                let var = &self.vars[*v as usize];
                let msg = match var.storage {
                    Storage::Const => Some("l-value required (can't modify a const)".to_owned()),
                    Storage::Uniform => Some(format!(
                        "l-value required (can't modify a uniform \"{}\")",
                        var.name
                    )),
                    Storage::Input if self.version < 300 && self.stage == Stage::Vertex => {
                        Some(format!(
                            "l-value required (can't modify an attribute \"{}\")",
                            var.name
                        ))
                    }
                    Storage::Input if self.version < 300 => Some(format!(
                        "l-value required (can't modify a varying \"{}\")",
                        var.name
                    )),
                    Storage::Input => Some(format!(
                        "l-value required (can't modify an input \"{}\")",
                        var.name
                    )),
                    Storage::Builtin(
                        Builtin::FragCoord
                        | Builtin::FrontFacing
                        | Builtin::PointCoord
                        | Builtin::VertexId
                        | Builtin::InstanceId,
                    ) => Some(format!("l-value required (can't modify {})", var.name)),
                    _ => None,
                };
                if e.cv.is_some() && msg.is_none() {
                    self.err(line, "assign", "l-value required (can't modify a const)");
                    return false;
                }
                if let Some(m) = msg {
                    self.err(line, "assign", &m);
                    return false;
                }
                true
            }
            EK::Swz(b, idx) => {
                let n = e.ty.rows as usize;
                for i in 0..n {
                    if idx[..i].contains(&idx[i]) {
                        self.err(
                            line,
                            "assign",
                            "l-value of swizzle cannot have duplicate components",
                        );
                        return false;
                    }
                }
                self.check_lvalue(b, line)
            }
            EK::Index(b, _) | EK::Field(b, _) => self.check_lvalue(b, line),
            _ => {
                self.err(line, "assign", "l-value required (can't modify a const)");
                false
            }
        }
    }

    fn cond_expr(&mut self) -> R<Expr> {
        let c = self.bin_expr(0)?;
        if !self.is_punct("?") {
            return Ok(c);
        }
        let line = self.line();
        self.pos += 1;
        let a = self.expr()?;
        self.expect(":")?;
        let b = self.assign_expr()?;
        if c.ty != Type::BOOL {
            self.err(line, "", "boolean expression expected");
            self.err(line, "", "boolean expression expected");
            return Ok(a);
        }
        if a.ty != b.ty {
            let (x, y) = (self.type_name(&a), self.type_name(&b));
            self.err(
                line,
                "?:",
                &format!("mismatching ternary operator operand types '{x} and '{y}'"),
            );
            return Ok(b);
        }
        let ty = a.ty;
        let prec = a.prec.max(b.prec);
        let cv = match &c.cv {
            Some(v) if a.cv.is_some() && b.cv.is_some() => {
                if v[0] != 0 {
                    a.cv.clone()
                } else {
                    b.cv.clone()
                }
            }
            _ => None,
        };
        Ok(Expr {
            ty,
            prec,
            cv,
            k: EK::Cond(Box::new(c), Box::new(a), Box::new(b)),
        })
    }

    fn bin_expr(&mut self, level: usize) -> R<Expr> {
        const LEVELS: &[&[&str]] = &[
            &["||"],
            &["^^"],
            &["&&"],
            &["|"],
            &["^"],
            &["&"],
            &["==", "!="],
            &["<", ">", "<=", ">="],
            &["<<", ">>"],
            &["+", "-"],
            &["*", "/", "%"],
        ];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut l = self.bin_expr(level + 1)?;
        loop {
            let op = match self.peek() {
                Some(Tok::Punct(p)) if LEVELS[level].contains(p) => *p,
                _ => return Ok(l),
            };
            let line = self.line();
            self.pos += 1;
            let r = self.bin_expr(level + 1)?;
            l = match op {
                "&&" | "||" => {
                    if l.ty != Type::BOOL || r.ty != Type::BOOL {
                        let (a, b) = (self.type_name(&l), self.type_name(&r));
                        self.wrong_operands(line, op, &a, &b);
                        l
                    } else {
                        let and = op == "&&";
                        let cv = match (&l.cv, &r.cv) {
                            (Some(a), Some(b)) => Some(Rc::new(vec![u32::from(if and {
                                a[0] != 0 && b[0] != 0
                            } else {
                                a[0] != 0 || b[0] != 0
                            })])),
                            _ => None,
                        };
                        Expr {
                            ty: Type::BOOL,
                            prec: Prec::None,
                            cv,
                            k: EK::Logic(and, Box::new(l), Box::new(r)),
                        }
                    }
                }
                _ => {
                    let b = match op {
                        "^^" => BOp::LXor,
                        "|" => BOp::Or,
                        "^" => BOp::Xor,
                        "&" => BOp::And,
                        "==" => BOp::Eq,
                        "!=" => BOp::Ne,
                        "<" => BOp::Lt,
                        ">" => BOp::Gt,
                        "<=" => BOp::Le,
                        ">=" => BOp::Ge,
                        "<<" => BOp::Shl,
                        ">>" => BOp::Shr,
                        "+" => BOp::Add,
                        "-" => BOp::Sub,
                        "*" => BOp::Mul,
                        "/" => BOp::Div,
                        _ => BOp::Mod,
                    };
                    if self.version < 300
                        && matches!(
                            b,
                            BOp::Or | BOp::Xor | BOp::And | BOp::Shl | BOp::Shr | BOp::Mod
                        )
                    {
                        self.err(line, op, "supported in GLSL ES 3.00 and above only");
                        l
                    } else {
                        match self.binary_typed(b, op, l.clone(), r, line) {
                            Some(e) => e,
                            None => l,
                        }
                    }
                }
            };
        }
    }

    fn wrong_operands(&mut self, line: u32, op: &str, a: &str, b: &str) {
        self.err(
            line,
            op,
            &format!(
                "wrong operand types - no operation '{op}' exists that takes a left-hand operand of type '{a}' and a right operand of type '{b}' (or there is no acceptable conversion)"
            ),
        );
    }

    /// Types a binary operation; logs and returns `None` when invalid.
    fn binary_typed(&mut self, op: BOp, tok: &str, l: Expr, r: Expr, line: u32) -> Option<Expr> {
        let (lt, rt) = (l.ty, r.ty);
        let fail = |p: &mut Self| {
            let (a, b) = (p.type_name(&l), p.type_name(&r));
            p.wrong_operands(line, tok, &a, &b);
            None
        };
        if lt.is_array() || rt.is_array() {
            if matches!(op, BOp::Eq | BOp::Ne) && self.version >= 300 && lt == rt {
            } else {
                return fail(self);
            }
        }
        let ty = match op {
            BOp::Eq | BOp::Ne => {
                if lt != rt || lt.has_sampler(&self.structs) || lt.base == Base::Void {
                    return fail(self);
                }
                Type::BOOL
            }
            BOp::LXor => {
                if lt != Type::BOOL || rt != Type::BOOL {
                    return fail(self);
                }
                Type::BOOL
            }
            BOp::Lt | BOp::Gt | BOp::Le | BOp::Ge => {
                if lt != rt || !lt.is_scalar() || !lt.is_numeric() {
                    return fail(self);
                }
                Type::BOOL
            }
            BOp::Shl | BOp::Shr => {
                if !lt.is_integer() || !rt.is_integer() || lt.is_matrix() {
                    return fail(self);
                }
                if !(rt.is_scalar() || rt.rows == lt.rows) || (lt.is_scalar() && !rt.is_scalar()) {
                    return fail(self);
                }
                lt
            }
            BOp::And | BOp::Or | BOp::Xor | BOp::Mod => {
                if !lt.is_integer() || lt.scalar_kind() != rt.scalar_kind() {
                    return fail(self);
                }
                if lt.rows != rt.rows && !lt.is_scalar() && !rt.is_scalar() {
                    return fail(self);
                }
                if lt.is_scalar() { rt } else { lt }
            }
            BOp::Add | BOp::Sub | BOp::Mul | BOp::Div => {
                if !lt.is_numeric() || !rt.is_numeric() || lt.scalar_kind() != rt.scalar_kind() {
                    return fail(self);
                }
                if op == BOp::Mul
                    && (lt.is_matrix() || rt.is_matrix())
                    && !(lt.is_scalar() || rt.is_scalar())
                {
                    if lt.is_matrix() && rt.is_matrix() {
                        if lt.cols != rt.rows {
                            return fail(self);
                        }
                        Type::mat(rt.cols, lt.rows)
                    } else if lt.is_matrix() {
                        if lt.cols != rt.rows {
                            return fail(self);
                        }
                        Type::vec(Scalar::Float, lt.rows)
                    } else {
                        if rt.rows != lt.rows {
                            return fail(self);
                        }
                        Type::vec(Scalar::Float, rt.cols)
                    }
                } else if lt == rt {
                    lt
                } else if lt.is_scalar() {
                    rt
                } else if rt.is_scalar() {
                    lt
                } else {
                    return fail(self);
                }
            }
        };
        let prec = if ty == Type::BOOL {
            Prec::None
        } else {
            l.prec.max(r.prec)
        };
        let cv = match (&l.cv, &r.cv) {
            (Some(a), Some(b)) => {
                if matches!(op, BOp::Div | BOp::Mod) && lt.is_integer() && b.contains(&0) {
                    self.log
                        .warning(line, tok, "Divide by zero error during constant folding");
                }
                fold::bin(op, &lt, a, &rt, b, &ty).map(Rc::new)
            }
            _ => None,
        };
        Some(Expr {
            ty,
            prec,
            cv,
            k: EK::Bin(op, Box::new(l), Box::new(r)),
        })
    }

    fn unary(&mut self) -> R<Expr> {
        let line = self.line();
        let op = match self.peek() {
            Some(Tok::Punct(p)) if matches!(*p, "+" | "-" | "!" | "~" | "++" | "--") => *p,
            _ => return self.postfix(),
        };
        self.pos += 1;
        let e = self.unary()?;
        let bad = |p: &mut Self, e: &Expr| {
            let tn = p.type_name(e);
            p.err(
                line,
                op,
                &format!("wrong operand type - no operation '{op}' exists that takes an operand of type {tn} (or there is no acceptable conversion)"),
            );
        };
        match op {
            "+" => {
                if !e.ty.is_numeric() {
                    bad(self, &e);
                }
                Ok(e)
            }
            "-" => {
                if !e.ty.is_numeric() {
                    bad(self, &e);
                    let t = e.ty;
                    return Ok(Expr {
                        ty: t,
                        prec: e.prec,
                        cv: None,
                        k: e.k,
                    });
                }
                let cv = e.cv.as_ref().map(|v| Rc::new(fold::un(UOp::Neg, &e.ty, v)));
                Ok(Expr {
                    ty: e.ty,
                    prec: e.prec,
                    cv,
                    k: EK::Un(UOp::Neg, Box::new(e)),
                })
            }
            "!" => {
                if e.ty != Type::BOOL {
                    bad(self, &e);
                    return Ok(e);
                }
                let cv = e.cv.as_ref().map(|v| Rc::new(fold::un(UOp::Not, &e.ty, v)));
                Ok(Expr {
                    ty: e.ty,
                    prec: e.prec,
                    cv,
                    k: EK::Un(UOp::Not, Box::new(e)),
                })
            }
            "~" => {
                if self.version < 300 {
                    self.err(line, "~", "supported in GLSL ES 3.00 and above only");
                    return Ok(e);
                }
                if !e.ty.is_integer() {
                    bad(self, &e);
                    return Ok(e);
                }
                let cv =
                    e.cv.as_ref()
                        .map(|v| Rc::new(fold::un(UOp::BitNot, &e.ty, v)));
                Ok(Expr {
                    ty: e.ty,
                    prec: e.prec,
                    cv,
                    k: EK::Un(UOp::BitNot, Box::new(e)),
                })
            }
            _ => {
                if !e.ty.is_numeric() {
                    bad(self, &e);
                    return Ok(e);
                }
                self.check_lvalue(&e, line);
                Ok(Expr {
                    ty: e.ty,
                    prec: e.prec,
                    cv: None,
                    k: EK::IncDec {
                        pre: true,
                        inc: op == "++",
                        e: Box::new(e),
                    },
                })
            }
        }
    }

    fn postfix(&mut self) -> R<Expr> {
        let mut e = self.primary()?;
        loop {
            let line = self.line();
            if self.eat_punct("[") {
                let idx = self.expr()?;
                self.expect("]")?;
                e = self.index(e, idx, line);
            } else if self.eat_punct(".") {
                let name = self.ident_any()?;
                if self.is_punct("(") && name == "length" && e.ty.is_array() {
                    self.pos += 1;
                    self.expect(")")?;
                    if self.version < 300 {
                        self.err(
                            line,
                            "length",
                            "methods supported in GLSL ES 3.00 and above only",
                        );
                    }
                    e = Expr {
                        ty: Type::INT,
                        prec: Prec::High,
                        cv: Some(Rc::new(vec![e.ty.array])),
                        k: EK::Const,
                    };
                    continue;
                }
                e = self.field(e, &name, line);
            } else if self.is_punct("++") || self.is_punct("--") {
                let inc = self.is_punct("++");
                self.pos += 1;
                let op = if inc { "++" } else { "--" };
                if !e.ty.is_numeric() {
                    let tn = self.type_name(&e);
                    self.err(line, op, &format!("wrong operand type - no operation '{op}' exists that takes an operand of type {tn} (or there is no acceptable conversion)"));
                    continue;
                }
                self.check_lvalue(&e, line);
                e = Expr {
                    ty: e.ty,
                    prec: e.prec,
                    cv: None,
                    k: EK::IncDec {
                        pre: false,
                        inc,
                        e: Box::new(e),
                    },
                };
            } else {
                return Ok(e);
            }
        }
    }

    fn index(&mut self, e: Expr, idx: Expr, line: u32) -> Expr {
        if !(idx.ty == Type::INT || idx.ty == Type::UINT) {
            self.err(line, "[]", "integer expression required");
            return e;
        }
        let (ety, size) = if e.ty.is_array() {
            (e.ty.elem(), e.ty.array)
        } else if e.ty.is_matrix() {
            (Type::vec(Scalar::Float, e.ty.rows), u32::from(e.ty.cols))
        } else if e.ty.is_vector() {
            (
                Type::scalar(e.ty.scalar_kind().unwrap_or(Scalar::Float)),
                u32::from(e.ty.rows),
            )
        } else {
            self.err(
                line,
                "[]",
                " left of '[' is not of type array, matrix, or vector  ",
            );
            return e;
        };
        if let Some(v) = &idx.cv {
            let i = v[0] as i32;
            if i < 0 || i as u32 >= size {
                if e.ty.is_array() {
                    self.err(line, "[]", "array index out of range");
                } else if e.ty.is_matrix() {
                    self.err(line, "[]", "matrix field selection out of range");
                } else {
                    self.err(line, "[]", "vector field selection out of range");
                }
                return Expr {
                    ty: ety,
                    prec: e.prec,
                    cv: None,
                    k: EK::Index(Box::new(e), Box::new(zero_int())),
                };
            }
        } else if e.ty.has_sampler(&self.structs)
            && self.version < 300
            && self.stage == Stage::Fragment
        {
            // ES 1.00 allows only constant-index-expressions for samplers;
            // loop indices count as such (ANGLE accepts them).
        }
        let cv = match (&e.cv, &idx.cv) {
            (Some(v), Some(i)) => {
                let n = ety.cells(&self.structs) as usize;
                let s = i[0] as usize * n;
                v.get(s..s + n).map(|x| Rc::new(x.to_vec()))
            }
            _ => None,
        };
        let prec = e.prec;
        Expr {
            ty: ety,
            prec,
            cv,
            k: EK::Index(Box::new(e), Box::new(idx)),
        }
    }

    fn field(&mut self, e: Expr, name: &str, line: u32) -> Expr {
        if let Base::Struct(s) = e.ty.base
            && !e.ty.is_array()
        {
            let def = &self.structs[s as usize];
            let Some(i) = def.fields.iter().position(|f| f.0 == name) else {
                self.err(line, name, " no such field in structure");
                return e;
            };
            let (ft, fp) = (def.fields[i].1, def.fields[i].2);
            let off: u32 = def.fields[..i]
                .iter()
                .map(|f| f.1.cells(&self.structs))
                .sum();
            let n = ft.cells(&self.structs) as usize;
            let cv =
                e.cv.as_ref()
                    .and_then(|v| v.get(off as usize..off as usize + n))
                    .map(|x| Rc::new(x.to_vec()));
            return Expr {
                ty: ft,
                prec: fp,
                cv,
                k: EK::Field(Box::new(e), i as u32),
            };
        }
        if !(e.ty.is_vector() || e.ty.is_scalar()) || e.ty.base == Base::Void {
            self.err(
                line,
                ".",
                "field selection requires structure or vector on left hand side",
            );
            return e;
        }
        // Swizzle.
        let sets = ["xyzw", "rgba", "stpq"];
        let chars: Vec<char> = name.chars().collect();
        let set = sets.iter().find(|s| s.contains(chars[0]));
        let ok_set = set.is_some_and(|s| chars.iter().all(|c| s.contains(*c)));
        if !ok_set || chars.len() > 4 {
            if chars.len() > 4 {
                self.err(line, name, "illegal vector field selection");
            } else {
                self.err(
                    line,
                    name,
                    "illegal - vector component fields not from the same set",
                );
            }
            return Expr {
                ty: Type::FLOAT,
                prec: e.prec,
                cv: None,
                k: EK::Swz(Box::new(e), [0; 4]),
            }
            .tap_invalid();
        }
        let set = set.expect("set");
        let mut idx = [0u8; 4];
        for (i, c) in chars.iter().enumerate() {
            let k = set.find(*c).expect("in set") as u8;
            if k >= e.ty.rows {
                self.err(line, name, "vector field selection out of range");
                return Expr {
                    ty: Type::FLOAT,
                    prec: e.prec,
                    cv: None,
                    k: EK::Swz(Box::new(e), [0; 4]),
                }
                .tap_invalid();
            }
            idx[i] = k;
        }
        let ty = Type::vec(
            e.ty.scalar_kind().unwrap_or(Scalar::Float),
            chars.len() as u8,
        );
        let cv =
            e.cv.as_ref()
                .map(|v| Rc::new(idx[..chars.len()].iter().map(|k| v[*k as usize]).collect()));
        Expr {
            ty,
            prec: e.prec,
            cv,
            k: EK::Swz(Box::new(e), idx),
        }
    }

    fn primary(&mut self) -> R<Expr> {
        let line = self.line();
        let tok = match self.peek() {
            Some(t) => t.clone(),
            None => return self.syntax_error(),
        };
        match tok {
            Tok::Int(v, u) => {
                self.pos += 1;
                if u && self.version < 300 {
                    return self.syntax_error();
                }
                Ok(Expr {
                    ty: if u { Type::UINT } else { Type::INT },
                    prec: Prec::None,
                    cv: Some(Rc::new(vec![v])),
                    k: EK::Const,
                })
            }
            Tok::Float(v) => {
                self.pos += 1;
                Ok(Expr {
                    ty: Type::FLOAT,
                    prec: Prec::None,
                    cv: Some(Rc::new(vec![v.to_bits()])),
                    k: EK::Const,
                })
            }
            Tok::Punct("(") => {
                self.pos += 1;
                let e = self.expr()?;
                self.expect(")")?;
                Ok(e)
            }
            Tok::Ident(name) => {
                if name == "true" || name == "false" {
                    self.pos += 1;
                    return Ok(Expr {
                        ty: Type::BOOL,
                        prec: Prec::None,
                        cv: Some(Rc::new(vec![u32::from(name == "true")])),
                        k: EK::Const,
                    });
                }
                // Constructor or function call.
                let is_type = type_keyword(&name, self.version).is_some()
                    || matches!(self.lookup(&name), Some(Sym::Struct(_)));
                if is_type {
                    let t = self.type_spec()?;
                    if !self.is_punct("(") {
                        return self.syntax_error();
                    }
                    self.pos += 1;
                    let args = self.call_args()?;
                    return Ok(self.constructor(t, args, line));
                }
                if self.is_keyword(&name) {
                    if self.is_reserved(&name) {
                        self.err(line, &name, "Illegal use of reserved word");
                    }
                    return self.syntax_error();
                }
                self.pos += 1;
                if self.is_punct("(") {
                    self.pos += 1;
                    let args = self.call_args()?;
                    return Ok(self.call(&name, args, line));
                }
                match self.lookup(&name).cloned() {
                    Some(Sym::Var(v)) => {
                        let var = &mut self.vars[v as usize];
                        var.used = true;
                        let ty = var.ty;
                        let prec = var.prec;
                        let cv = if var.storage == Storage::Const {
                            var.cv.clone()
                        } else {
                            None
                        };
                        if let Storage::Builtin(b) = var.storage {
                            match b {
                                Builtin::FragData => self.uses_frag_data = true,
                                Builtin::FragColor => self.uses_frag_color = true,
                                _ => {}
                            }
                            if self.uses_frag_data && self.uses_frag_color {
                                self.err(
                                    line,
                                    &name,
                                    "cannot use both gl_FragData and gl_FragColor",
                                );
                            }
                        }
                        Ok(Expr {
                            ty,
                            prec,
                            cv,
                            k: EK::Var(v),
                        })
                    }
                    Some(Sym::Member(v, i)) => {
                        let var = &self.vars[v as usize];
                        let base = Expr {
                            ty: var.ty,
                            prec: Prec::None,
                            cv: None,
                            k: EK::Var(v),
                        };
                        self.vars[v as usize].used = true;
                        let Base::Struct(s) = base.ty.base else {
                            return Ok(zero_expr());
                        };
                        let (ft, fp) = {
                            let f = &self.structs[s as usize].fields[i as usize];
                            (f.1, f.2)
                        };
                        Ok(Expr {
                            ty: ft,
                            prec: fp,
                            cv: None,
                            k: EK::Field(Box::new(base), i),
                        })
                    }
                    _ => {
                        self.err(line, &name, "undeclared identifier");
                        Ok(Expr {
                            prec: Prec::High,
                            ..zero_expr()
                        })
                    }
                }
            }
            _ => self.syntax_error(),
        }
    }

    fn call_args(&mut self) -> R<Vec<Expr>> {
        let mut args = Vec::new();
        if self.eat_punct(")") {
            return Ok(args);
        }
        if self.is_ident("void") && self.peek_at(1) == Some(&Tok::Punct(")")) {
            self.pos += 2;
            return Ok(args);
        }
        loop {
            args.push(self.assign_expr()?);
            if self.eat_punct(")") {
                return Ok(args);
            }
            self.expect(",")?;
        }
    }

    fn constructor(&mut self, mut ty: Type, args: Vec<Expr>, line: u32) -> Expr {
        let structs_len = self.structs.clone();
        let ty0 = ty;
        let bad = move || {
            let mut t = ty0;
            if t.array == u32::MAX {
                t.array = 1;
            }
            Expr {
                ty: t,
                prec: Prec::None,
                cv: Some(Rc::new(vec![0; t.cells(&structs_len) as usize])),
                k: EK::Const,
            }
        };
        if args.is_empty() {
            self.err(
                line,
                "constructor",
                "constructor does not have any arguments",
            );
            return bad();
        }
        if ty.is_array() {
            if ty.array == u32::MAX {
                ty.array = args.len() as u32;
            }
            let el = ty.elem();
            if args.len() as u32 != ty.array || args.iter().any(|a| a.ty != el) {
                self.err(
                    line,
                    "constructor",
                    "Array constructor argument has an incorrect type",
                );
                return bad();
            }
        } else if let Base::Struct(s) = ty.base {
            let fields: Vec<Type> = self.structs[s as usize]
                .fields
                .iter()
                .map(|f| f.1)
                .collect();
            if fields.len() != args.len() {
                self.err(line, "constructor", "Number of constructor parameters does not match the number of structure fields");
                return bad();
            }
            if fields.iter().zip(&args).any(|(f, a)| *f != a.ty) {
                self.err(
                    line,
                    "constructor",
                    "Structure constructor arguments do not match structure fields",
                );
                return bad();
            }
        } else {
            if args.iter().any(|a| a.ty.is_struct()) {
                self.err(
                    line,
                    "constructor",
                    "a struct cannot be used as a constructor argument for this type",
                );
                return bad();
            }
            if args
                .iter()
                .any(|a| a.ty.is_array() || a.ty.is_sampler() || a.ty.base == Base::Void)
            {
                self.err(line, "constructor", "cannot convert");
                return bad();
            }
            let need = ty.comps();
            let have: u32 = args.iter().map(|a| a.ty.comps()).sum();
            let single_scalar = args.len() == 1 && args[0].ty.is_scalar();
            let mat_from_mat = ty.is_matrix() && args.len() == 1 && args[0].ty.is_matrix();
            if ty.is_matrix() && args.iter().any(|a| a.ty.is_matrix()) && args.len() > 1 {
                self.err(
                    line,
                    "constructor",
                    "constructing matrix from matrix can only take one argument",
                );
                return bad();
            }
            if !single_scalar && !mat_from_mat {
                // Too many: the last argument must still be used.
                let without_last: u32 = args[..args.len() - 1].iter().map(|a| a.ty.comps()).sum();
                if without_last >= need {
                    self.err(line, "constructor", "too many arguments");
                    return bad();
                }
                if have < need {
                    self.err(
                        line,
                        "constructor",
                        "not enough data provided for construction",
                    );
                    return bad();
                }
                if ty.is_matrix() && self.version < 300 && have != need && false {
                    return bad();
                }
            }
        }
        let prec = args.iter().map(|a| a.prec).max().unwrap_or(Prec::None);
        let prec = if ty.base == Base::S(Scalar::Bool) || ty.is_struct() {
            Prec::None
        } else {
            prec
        };
        let cv = if args.iter().all(|a| a.cv.is_some()) {
            let a: Vec<(Type, &[u32])> = args
                .iter()
                .map(|a| (a.ty, a.cv.as_ref().expect("cv").as_slice()))
                .collect();
            Some(Rc::new(fold::ctor(&ty, &a, &self.structs)))
        } else {
            None
        };
        Expr {
            ty,
            prec,
            cv,
            k: EK::Ctor(args),
        }
    }

    fn call(&mut self, name: &str, args: Vec<Expr>, line: u32) -> Expr {
        let types: Vec<Type> = args.iter().map(|a| a.ty).collect();
        // User functions first.
        if let Some(Sym::Funcs(list)) = self.lookup(name).cloned() {
            let found = list.iter().copied().find(|f| {
                self.funcs[*f as usize]
                    .param_types
                    .iter()
                    .map(|p| p.0)
                    .eq(types.iter().copied())
            });
            if let Some(f) = found {
                let quals: Vec<ParamQual> = self.funcs[f as usize]
                    .param_types
                    .iter()
                    .map(|p| p.1)
                    .collect();
                for (a, q) in args.iter().zip(&quals) {
                    if *q != ParamQual::In && !self.is_lvalue_silent(a) {
                        self.err(line, "assign", "l-value required (can't modify a const)");
                    }
                }
                if let Some(cur) = self.cur_func {
                    self.funcs[cur as usize].calls.push(f);
                }
                let func = &self.funcs[f as usize];
                return Expr {
                    ty: func.ret,
                    prec: func.ret_prec,
                    cv: None,
                    k: EK::Call(f, args),
                };
            }
        }
        if let Some(Sym::Var(_)) = self.lookup(name) {
            self.err(line, name, "function name expected");
            return zero_expr();
        }
        let ctx = Ctx {
            vertex: self.stage == Stage::Vertex,
            version: self.version,
            derivatives: self.derivatives,
            texture_lod: self.texture_lod,
        };
        match builtins::resolve(name, &types, ctx) {
            Some((bi, ty)) => {
                if bi == Bi::Modf && !self.is_lvalue_silent(&args[1]) {
                    self.err(line, "modf", "l-value required");
                }
                let prec = match bi {
                    Bi::Tex(_) | Bi::TexFetch(_) => args[0].prec,
                    Bi::TexSize => Prec::High,
                    _ if ty.base == Base::S(Scalar::Bool) => Prec::None,
                    _ => args.iter().map(|a| a.prec).max().unwrap_or(Prec::None),
                };
                let cv = if args.iter().all(|a| a.cv.is_some())
                    && !matches!(bi, Bi::DFdx | Bi::DFdy | Bi::Fwidth)
                {
                    let a: Vec<(Type, &[u32])> = args
                        .iter()
                        .map(|a| (a.ty, a.cv.as_ref().expect("cv").as_slice()))
                        .collect();
                    fold::builtin(bi, &a, &ty).map(Rc::new)
                } else {
                    None
                };
                Expr {
                    ty,
                    prec,
                    cv,
                    k: EK::Bi(bi, args),
                }
            }
            None => {
                // Extension-gated names give ANGLE's "extension is disabled".
                let ext = match name {
                    "dFdx" | "dFdy" | "fwidth"
                        if self.stage == Stage::Fragment && self.version < 300 =>
                    {
                        Some("GL_OES_standard_derivatives")
                    }
                    "texture2DLodEXT"
                    | "texture2DProjLodEXT"
                    | "textureCubeLodEXT"
                    | "texture2DGradEXT"
                    | "texture2DProjGradEXT"
                    | "textureCubeGradEXT"
                        if self.version < 300 =>
                    {
                        Some("GL_EXT_shader_texture_lod")
                    }
                    _ => None,
                };
                if let Some(e) = ext
                    && self.ext_enabled_webgl.contains(&e)
                {
                    self.err(line, e, "extension is disabled");
                    let lax = Ctx {
                        derivatives: true,
                        texture_lod: true,
                        ..ctx
                    };
                    if let Some((bi, ty)) = builtins::resolve(name, &types, lax) {
                        let prec = args.iter().map(|a| a.prec).max().unwrap_or(Prec::None);
                        return Expr {
                            ty,
                            prec,
                            cv: None,
                            k: EK::Bi(bi, args),
                        };
                    }
                } else {
                    self.err(line, name, "no matching overloaded function found");
                }
                // ANGLE continues with a float of the call's precision.
                Expr {
                    ty: Type::FLOAT,
                    prec: self.default_prec(&Type::FLOAT),
                    cv: if args.is_empty() {
                        Some(Rc::new(vec![0]))
                    } else {
                        None
                    },
                    k: EK::Const,
                }
                .with_cv_if_none()
            }
        }
    }

    fn is_lvalue_silent(&self, e: &Expr) -> bool {
        match &e.k {
            EK::Var(v) => matches!(
                self.vars[*v as usize].storage,
                Storage::Local
                    | Storage::Global
                    | Storage::Output
                    | Storage::ParamIn
                    | Storage::ParamOut
                    | Storage::ParamInOut
                    | Storage::Builtin(
                        Builtin::Position
                            | Builtin::PointSize
                            | Builtin::FragColor
                            | Builtin::FragData
                            | Builtin::FragDepth
                    )
            ),
            EK::Swz(b, _) | EK::Index(b, _) | EK::Field(b, _) => self.is_lvalue_silent(b),
            _ => false,
        }
    }
}

fn zero_int() -> Expr {
    Expr {
        ty: Type::INT,
        prec: Prec::None,
        cv: Some(Rc::new(vec![0])),
        k: EK::Const,
    }
}

trait Tap {
    fn tap_invalid(self) -> Self;
    fn with_cv_if_none(self) -> Self;
}
impl Tap for Expr {
    /// An invalid swizzle continues as `const float` (ANGLE's recovery).
    fn tap_invalid(self) -> Self {
        Expr {
            ty: Type::FLOAT,
            prec: Prec::None,
            cv: Some(Rc::new(vec![0])),
            k: EK::Const,
        }
    }
    fn with_cv_if_none(self) -> Self {
        let cv = self.cv.clone().or_else(|| Some(Rc::new(vec![0])));
        Expr { cv, ..self }
    }
}

/// Parses a preprocessed shader.
pub fn parse(pp: &PpOut, stage: Stage, opts: &Options, log: &mut Log) -> Option<Unit> {
    let enabled = |name: &str| {
        opts.extensions.contains(&name)
            && pp
                .extensions
                .get(name)
                .is_some_and(|b| *b == ExtBehavior::Enable)
    };
    let mut p = Parser {
        toks: &pp.tokens,
        pos: 0,
        log,
        version: pp.version,
        stage,
        last_line: pp.last_line,
        derivatives: enabled("GL_OES_standard_derivatives"),
        texture_lod: enabled("GL_EXT_shader_texture_lod"),
        frag_depth: enabled("GL_EXT_frag_depth"),
        draw_buffers: enabled("GL_EXT_draw_buffers"),
        ext_enabled_webgl: opts.extensions.clone(),
        vars: Vec::new(),
        funcs: Vec::new(),
        structs: Vec::new(),
        blocks: Vec::new(),
        globals: Vec::new(),
        scopes: vec![HashMap::new(), HashMap::new()],
        precs: vec![HashMap::new()],
        cur_func: None,
        loops: 0,
        switches: 0,
        returned: false,
        builtins: HashMap::new(),
        uses_frag_data: false,
        uses_frag_color: false,
        max_draw_buffers: opts.max_draw_buffers.max(1),
        def_order: Vec::new(),
    };
    // Default precisions.
    {
        let d = p.precs.last_mut().expect("precs");
        match stage {
            Stage::Vertex => {
                d.insert("float", Prec::High);
                d.insert("int", Prec::High);
                d.insert("sampler2D", Prec::Low);
                d.insert("samplerCube", Prec::Low);
            }
            Stage::Fragment => {
                d.insert("int", Prec::Medium);
                d.insert("sampler2D", Prec::Low);
                d.insert("samplerCube", Prec::Low);
            }
        }
    }
    p.add_builtins();
    if pp.tokens.is_empty() {
        p.log.raw("ERROR: 1:1: '' : syntax error\n");
        return None;
    }
    while p.pos < p.toks.len() {
        if p.external().is_err() {
            return None;
        }
    }
    // main().
    let main = p
        .funcs
        .iter()
        .position(|f| f.name == "main" && f.body.is_some());
    let Some(main) = main else {
        p.log.raw("ERROR: -1:-1: '' : Missing main()\n");
        return None;
    };
    // Call graph: recursion and undefined functions.
    let n = p.funcs.len();
    let mut state = vec![0u8; n];
    let mut stack: Vec<usize> = Vec::new();
    fn dfs(
        f: usize,
        funcs: &[Func],
        state: &mut [u8],
        stack: &mut Vec<usize>,
        log: &mut Log,
    ) -> bool {
        state[f] = 1;
        stack.push(f);
        for c in &funcs[f].calls {
            let c = *c as usize;
            if state[c] == 1 {
                let start = stack.iter().position(|x| *x == c).unwrap_or(0);
                let mut chain: Vec<&str> = stack[start..]
                    .iter()
                    .map(|i| funcs[*i].name.as_str())
                    .collect();
                chain.push(&funcs[c].name);
                log.raw(&format!(
                    "ERROR: Recursive function call in the following call chain: {}\n",
                    chain.join(" <- ")
                ));
                return false;
            }
            if state[c] == 0 && !dfs(c, funcs, state, stack, log) {
                return false;
            }
        }
        stack.pop();
        state[f] = 2;
        true
    }
    for f in p.def_order.clone().into_iter().map(|f| f as usize) {
        if state[f] == 0
            && p.funcs[f].body.is_some()
            && !dfs(f, &p.funcs, &mut state, &mut stack, p.log)
        {
            return None;
        }
    }
    for f in 0..n {
        if p.funcs[f].body.is_none() {
            continue;
        }
        for c in p.funcs[f].calls.clone() {
            if p.funcs[c as usize].body.is_none() {
                let (cn, fname) = (p.funcs[c as usize].name.clone(), p.funcs[f].name.clone());
                p.log.raw(&format!(
                    "ERROR: Function {cn}() called by {fname}() is undefined\n"
                ));
                return None;
            }
        }
    }
    // ES 3.00 multiple fragment outputs need locations.
    if stage == Stage::Fragment && p.version >= 300 {
        let outs: Vec<usize> = p
            .vars
            .iter()
            .enumerate()
            .filter(|(_, v)| v.storage == Storage::Output)
            .map(|(i, _)| i)
            .collect();
        if outs.len() > 1 {
            for i in &outs {
                if p.vars[*i].location.is_none() {
                    let (l, n) = (p.vars[*i].line, p.vars[*i].name.clone());
                    p.log.error(l, &n, "when EXT_blend_func_extended extension is not enabled, must explicitly specify all locations when using multiple fragment outputs");
                }
            }
        }
    }
    Some(Unit {
        version: p.version,
        vars: p.vars,
        funcs: p.funcs,
        structs: p.structs,
        blocks: p.blocks,
        globals: p.globals,
        main: main as FuncId,
        builtins: p.builtins,
        uses_frag_data: p.uses_frag_data,
        uses_frag_color: p.uses_frag_color,
    })
}
