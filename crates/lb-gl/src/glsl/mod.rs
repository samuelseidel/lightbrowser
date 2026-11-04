//! GLSL ES 1.00 / 3.00 front end: preprocessor, parser with ANGLE's
//! semantic checks and messages, and code generation to the interpreter's
//! IR.

pub mod ast;
pub mod builtins;
pub mod codegen;
pub mod fold;
pub mod parse;
pub mod pp;
pub mod types;

/// Info log in ANGLE's format.
#[derive(Default, Debug, Clone)]
pub struct Log {
    pub text: String,
    pub errors: u32,
}

impl Log {
    pub fn error(&mut self, line: u32, token: &str, msg: &str) {
        self.text
            .push_str(&format!("ERROR: 0:{line}: '{token}' : {msg}\n"));
        self.errors += 1;
    }
    pub fn warning(&mut self, line: u32, token: &str, msg: &str) {
        self.text
            .push_str(&format!("WARNING: 0:{line}: '{token}' : {msg}\n"));
    }
    /// A message without location (link-style compile errors).
    pub fn raw(&mut self, s: &str) {
        self.text.push_str(s);
        if s.starts_with("ERROR") {
            self.errors += 1;
        }
    }
}

/// Shader stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Vertex,
    Fragment,
}

/// Compile options from the context.
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub webgl2: bool,
    /// GLSL extension names whose WebGL extension is enabled
    /// (`GL_OES_standard_derivatives`, `GL_EXT_shader_texture_lod`,
    /// `GL_EXT_frag_depth`, `GL_EXT_draw_buffers`).
    pub extensions: Vec<&'static str>,
    pub max_draw_buffers: u32,
}

/// Compiles one shader. Returns the compiled shader (on success) and the
/// info log.
pub fn compile(src: &str, stage: Stage, opts: &Options) -> (Option<crate::ir::Shader>, String) {
    let mut log = Log::default();
    let exts: Vec<&str> = opts.extensions.to_vec();
    let pp = pp::preprocess(src, opts.webgl2, &exts, &mut log);
    if pp.fatal || log.errors > 0 {
        return (None, log.text);
    }
    let parsed = parse::parse(&pp, stage, opts, &mut log);
    let Some(unit) = parsed else {
        return (None, log.text);
    };
    if log.errors > 0 {
        return (None, log.text);
    }
    match codegen::generate(unit, stage, &mut log) {
        Some(s) if log.errors == 0 => (Some(s), log.text),
        _ => (None, log.text),
    }
}
