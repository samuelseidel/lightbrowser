//! GLSL types, precision and ANGLE-style type names.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scalar {
    Float,
    Int,
    Uint,
    Bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dim {
    D2,
    D3,
    Cube,
    D2Array,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SamplerTy {
    pub dim: Dim,
    /// Float, Int or Uint result.
    pub ret: Scalar,
    pub shadow: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Base {
    Void,
    S(Scalar),
    Sampler(SamplerTy),
    /// Index into the shader's struct table.
    Struct(u32),
}

/// A type: base, vector size (`rows`), matrix columns (`cols`, 1 for
/// vectors) and array length (0 = not an array).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Type {
    pub base: Base,
    pub rows: u8,
    pub cols: u8,
    pub array: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Prec {
    #[default]
    None,
    Low,
    Medium,
    High,
}

impl Prec {
    pub fn name(self) -> &'static str {
        match self {
            Prec::None => "",
            Prec::Low => "lowp",
            Prec::Medium => "mediump",
            Prec::High => "highp",
        }
    }
}

#[derive(Clone, Debug)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<(String, Type, Prec)>,
}

impl Type {
    pub const VOID: Type = Type {
        base: Base::Void,
        rows: 1,
        cols: 1,
        array: 0,
    };
    pub const fn scalar(s: Scalar) -> Type {
        Type {
            base: Base::S(s),
            rows: 1,
            cols: 1,
            array: 0,
        }
    }
    pub const fn vec(s: Scalar, n: u8) -> Type {
        Type {
            base: Base::S(s),
            rows: n,
            cols: 1,
            array: 0,
        }
    }
    pub const fn mat(c: u8, r: u8) -> Type {
        Type {
            base: Base::S(Scalar::Float),
            rows: r,
            cols: c,
            array: 0,
        }
    }
    pub const FLOAT: Type = Type::scalar(Scalar::Float);
    pub const INT: Type = Type::scalar(Scalar::Int);
    pub const UINT: Type = Type::scalar(Scalar::Uint);
    pub const BOOL: Type = Type::scalar(Scalar::Bool);

    pub fn scalar_kind(&self) -> Option<Scalar> {
        match self.base {
            Base::S(s) => Some(s),
            _ => None,
        }
    }
    pub fn is_array(&self) -> bool {
        self.array > 0
    }
    pub fn elem(&self) -> Type {
        Type { array: 0, ..*self }
    }
    pub fn is_scalar(&self) -> bool {
        matches!(self.base, Base::S(_)) && self.rows == 1 && self.cols == 1 && self.array == 0
    }
    pub fn is_vector(&self) -> bool {
        matches!(self.base, Base::S(_)) && self.rows > 1 && self.cols == 1 && self.array == 0
    }
    pub fn is_matrix(&self) -> bool {
        self.cols > 1 && self.array == 0
    }
    pub fn is_numeric(&self) -> bool {
        matches!(
            self.base,
            Base::S(Scalar::Float | Scalar::Int | Scalar::Uint)
        ) && self.array == 0
    }
    pub fn is_integer(&self) -> bool {
        matches!(self.base, Base::S(Scalar::Int | Scalar::Uint)) && self.array == 0
    }
    pub fn is_sampler(&self) -> bool {
        matches!(self.base, Base::Sampler(_))
    }
    pub fn is_struct(&self) -> bool {
        matches!(self.base, Base::Struct(_))
    }
    /// Scalar or vector of `s` (no array).
    pub fn is_sv(&self, s: Scalar) -> bool {
        self.base == Base::S(s) && self.cols == 1 && self.array == 0
    }
    /// Number of scalar components of one element (no array).
    pub fn comps(&self) -> u32 {
        u32::from(self.rows) * u32::from(self.cols)
    }
    /// Cells occupied (structs need the table).
    pub fn cells(&self, structs: &[StructDef]) -> u32 {
        let one = match self.base {
            Base::Void => 0,
            Base::S(_) | Base::Sampler(_) => self.comps(),
            Base::Struct(i) => structs[i as usize]
                .fields
                .iter()
                .map(|(_, t, _)| t.cells(structs))
                .sum(),
        };
        one * self.array.max(1)
    }
    /// Whether the type (recursively) contains a sampler.
    pub fn has_sampler(&self, structs: &[StructDef]) -> bool {
        match self.base {
            Base::Sampler(_) => true,
            Base::Struct(i) => structs[i as usize]
                .fields
                .iter()
                .any(|(_, t, _)| t.has_sampler(structs)),
            _ => false,
        }
    }
    pub fn has_array(&self, structs: &[StructDef]) -> bool {
        if self.array > 0 {
            return true;
        }
        match self.base {
            Base::Struct(i) => structs[i as usize]
                .fields
                .iter()
                .any(|(_, t, _)| t.has_array(structs)),
            _ => false,
        }
    }

    /// The GL enum of a non-struct type (`getActiveUniform`).
    pub fn gl_enum(&self) -> u32 {
        use Scalar::*;
        match self.base {
            Base::S(s) if self.cols > 1 => match (self.cols, self.rows) {
                (2, 2) => 0x8B5A,
                (3, 3) => 0x8B5B,
                (4, 4) => 0x8B5C,
                (2, 3) => 0x8B65,
                (2, 4) => 0x8B66,
                (3, 2) => 0x8B67,
                (3, 4) => 0x8B68,
                (4, 2) => 0x8B69,
                (4, 3) => 0x8B6A,
                _ => {
                    let _ = s;
                    0
                }
            },
            Base::S(Float) => [0x1406, 0x8B50, 0x8B51, 0x8B52][self.rows as usize - 1],
            Base::S(Int) => [0x1404, 0x8B53, 0x8B54, 0x8B55][self.rows as usize - 1],
            Base::S(Uint) => [0x1405, 0x8DC6, 0x8DC7, 0x8DC8][self.rows as usize - 1],
            Base::S(Bool) => [0x8B56, 0x8B57, 0x8B58, 0x8B59][self.rows as usize - 1],
            Base::Sampler(s) => match (s.ret, s.dim, s.shadow) {
                (Float, Dim::D2, false) => 0x8B5E,
                (Float, Dim::D3, _) => 0x8B5F,
                (Float, Dim::Cube, false) => 0x8B60,
                (Float, Dim::D2, true) => 0x8B62,
                (Float, Dim::D2Array, false) => 0x8DC1,
                (Float, Dim::D2Array, true) => 0x8DC4,
                (Float, Dim::Cube, true) => 0x8DC5,
                (Int, Dim::D2, _) => 0x8DCA,
                (Int, Dim::D3, _) => 0x8DCB,
                (Int, Dim::Cube, _) => 0x8DCC,
                (Int, Dim::D2Array, _) => 0x8DCF,
                (Uint, Dim::D2, _) => 0x8DD2,
                (Uint, Dim::D3, _) => 0x8DD3,
                (Uint, Dim::Cube, _) => 0x8DD4,
                (Uint, Dim::D2Array, _) => 0x8DD7,
                _ => 0,
            },
            _ => 0,
        }
    }

    /// ANGLE's type text: "4-component vector of float", "3X3 matrix of
    /// float", "float", "array[2] of float".
    pub fn describe(&self, structs: &[StructDef]) -> String {
        let mut s = String::new();
        if self.array > 0 {
            s.push_str(&format!("array[{}] of ", self.array));
        }
        let base = match self.base {
            Base::Void => "void".to_owned(),
            Base::S(Scalar::Float) => "float".into(),
            Base::S(Scalar::Int) => "int".into(),
            Base::S(Scalar::Uint) => "uint".into(),
            Base::S(Scalar::Bool) => "bool".into(),
            Base::Sampler(_) => self.keyword().unwrap_or("sampler").to_owned(),
            Base::Struct(i) => format!(
                "structure '{}'",
                structs.get(i as usize).map_or("", |d| d.name.as_str())
            ),
        };
        if self.cols > 1 {
            s.push_str(&format!("{}X{} matrix of ", self.cols, self.rows));
        } else if self.rows > 1 {
            s.push_str(&format!("{}-component vector of ", self.rows));
        }
        s.push_str(&base);
        s
    }

    /// The type keyword (`vec3`, `mat2x3`, `sampler2D`), when it has one.
    pub fn keyword(&self) -> Option<&'static str> {
        KEYWORD_TYPES
            .iter()
            .find(|(_, t)| t.base == self.base && t.rows == self.rows && t.cols == self.cols)
            .map(|(n, _)| *n)
    }
}

const fn smp(dim: Dim, ret: Scalar, shadow: bool) -> Type {
    Type {
        base: Base::Sampler(SamplerTy { dim, ret, shadow }),
        rows: 1,
        cols: 1,
        array: 0,
    }
}

/// Type keywords and the minimum GLSL version.
pub const KEYWORD_TYPES: &[(&str, Type)] = &[
    ("void", Type::VOID),
    ("float", Type::FLOAT),
    ("int", Type::INT),
    ("bool", Type::BOOL),
    ("uint", Type::UINT),
    ("vec2", Type::vec(Scalar::Float, 2)),
    ("vec3", Type::vec(Scalar::Float, 3)),
    ("vec4", Type::vec(Scalar::Float, 4)),
    ("ivec2", Type::vec(Scalar::Int, 2)),
    ("ivec3", Type::vec(Scalar::Int, 3)),
    ("ivec4", Type::vec(Scalar::Int, 4)),
    ("uvec2", Type::vec(Scalar::Uint, 2)),
    ("uvec3", Type::vec(Scalar::Uint, 3)),
    ("uvec4", Type::vec(Scalar::Uint, 4)),
    ("bvec2", Type::vec(Scalar::Bool, 2)),
    ("bvec3", Type::vec(Scalar::Bool, 3)),
    ("bvec4", Type::vec(Scalar::Bool, 4)),
    ("mat2", Type::mat(2, 2)),
    ("mat3", Type::mat(3, 3)),
    ("mat4", Type::mat(4, 4)),
    ("mat2x2", Type::mat(2, 2)),
    ("mat2x3", Type::mat(2, 3)),
    ("mat2x4", Type::mat(2, 4)),
    ("mat3x2", Type::mat(3, 2)),
    ("mat3x3", Type::mat(3, 3)),
    ("mat3x4", Type::mat(3, 4)),
    ("mat4x2", Type::mat(4, 2)),
    ("mat4x3", Type::mat(4, 3)),
    ("mat4x4", Type::mat(4, 4)),
    ("sampler2D", smp(Dim::D2, Scalar::Float, false)),
    ("samplerCube", smp(Dim::Cube, Scalar::Float, false)),
    ("sampler3D", smp(Dim::D3, Scalar::Float, false)),
    ("sampler2DShadow", smp(Dim::D2, Scalar::Float, true)),
    ("samplerCubeShadow", smp(Dim::Cube, Scalar::Float, true)),
    ("sampler2DArray", smp(Dim::D2Array, Scalar::Float, false)),
    (
        "sampler2DArrayShadow",
        smp(Dim::D2Array, Scalar::Float, true),
    ),
    ("isampler2D", smp(Dim::D2, Scalar::Int, false)),
    ("isampler3D", smp(Dim::D3, Scalar::Int, false)),
    ("isamplerCube", smp(Dim::Cube, Scalar::Int, false)),
    ("isampler2DArray", smp(Dim::D2Array, Scalar::Int, false)),
    ("usampler2D", smp(Dim::D2, Scalar::Uint, false)),
    ("usampler3D", smp(Dim::D3, Scalar::Uint, false)),
    ("usamplerCube", smp(Dim::Cube, Scalar::Uint, false)),
    ("usampler2DArray", smp(Dim::D2Array, Scalar::Uint, false)),
];

/// Whether a type keyword exists in the version (`None` if not a type).
pub fn type_keyword(name: &str, version: u32) -> Option<Type> {
    let (_, t) = KEYWORD_TYPES.iter().find(|(n, _)| *n == name)?;
    if version < 300 {
        let es1 = matches!(
            name,
            "void"
                | "float"
                | "int"
                | "bool"
                | "vec2"
                | "vec3"
                | "vec4"
                | "ivec2"
                | "ivec3"
                | "ivec4"
                | "bvec2"
                | "bvec3"
                | "bvec4"
                | "mat2"
                | "mat3"
                | "mat4"
                | "sampler2D"
                | "samplerCube"
        );
        if !es1 {
            return None;
        }
    }
    Some(*t)
}
