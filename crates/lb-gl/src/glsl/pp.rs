//! GLSL ES preprocessor and tokenizer (ANGLE's directives and messages).
//!
//! The source is split into pp-tokens per line; directives are handled and
//! macros expanded; the result is the token stream for the parser, each
//! token with its line number.

use std::collections::{HashMap, HashSet};

use super::Log;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    /// Integer literal: value, unsigned suffix.
    Int(u32, bool),
    Float(f32),
    Punct(&'static str),
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub text: String,
    pub line: u32,
}

#[derive(Clone, Debug, PartialEq)]
enum Pp {
    Ident(String),
    Num(String),
    Punct(&'static str),
    Other(char),
}

#[derive(Clone, Debug)]
struct PpTok {
    t: Pp,
    /// Whitespace before the token (function-like macro detection).
    space: bool,
}

impl PpTok {
    fn text(&self) -> String {
        match &self.t {
            Pp::Ident(s) | Pp::Num(s) => s.clone(),
            Pp::Punct(p) => (*p).to_owned(),
            Pp::Other(c) => c.to_string(),
        }
    }
}

const PUNCTS: &[&str] = &[
    "<<=", ">>=", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "^^", "+=", "-=",
    "*=", "/=", "%=", "&=", "^=", "|=", "##", "+", "-", "*", "/", "%", "<", ">", "=", "!", "~",
    "&", "|", "^", "(", ")", "[", "]", "{", "}", ",", ";", ".", "?", ":", "#",
];

/// Removes comments and line continuations; keeps line breaks.
fn strip(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == '\\' && b.get(i + 1) == Some(&'\n') {
            // A continuation joins lines; keep the line count with a
            // marker that tokenizing turns into nothing but a line break
            // after the joined line.
            out.push('\u{1}');
            i += 2;
        } else if c == '\\' && b.get(i + 1) == Some(&'\r') && b.get(i + 2) == Some(&'\n') {
            out.push('\u{1}');
            i += 3;
        } else if c == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                if b[i] == '\\' && b.get(i + 1) == Some(&'\n') {
                    out.push('\u{1}');
                    i += 2;
                    continue;
                }
                i += 1;
            }
            out.push(' ');
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i < b.len() && !(b[i] == '*' && b.get(i + 1) == Some(&'/')) {
                if b[i] == '\n' {
                    out.push('\u{1}');
                }
                i += 1;
            }
            i += 2;
            out.push(' ');
        } else if c == '\r' {
            if b.get(i + 1) != Some(&'\n') {
                out.push('\n');
            }
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn tokenize_line(line: &str) -> Vec<PpTok> {
    let b: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut space = true;
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() || c == '\u{1}' {
            space = true;
            i += 1;
            continue;
        }
        let start = i;
        let t = if c.is_ascii_alphabetic() || c == '_' {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == '_') {
                i += 1;
            }
            Pp::Ident(b[start..i].iter().collect())
        } else if c.is_ascii_digit() || (c == '.' && b.get(i + 1).is_some_and(char::is_ascii_digit))
        {
            while i < b.len() {
                let d = b[i];
                let sign = (d == '+' || d == '-')
                    && matches!(b[i - 1], 'e' | 'E')
                    && !b[start..i].iter().any(|c| *c == 'x' || *c == 'X');
                if d.is_ascii_alphanumeric() || d == '_' || d == '.' || sign {
                    i += 1;
                } else {
                    break;
                }
            }
            Pp::Num(b[start..i].iter().collect())
        } else {
            let rest: String = b[i..b.len().min(i + 3)].iter().collect();
            match PUNCTS.iter().find(|p| rest.starts_with(**p)) {
                Some(p) => {
                    i += p.len();
                    Pp::Punct(p)
                }
                None => {
                    i += 1;
                    Pp::Other(c)
                }
            }
        };
        out.push(PpTok { t, space });
        space = false;
    }
    out
}

#[derive(Clone)]
struct Macro {
    params: Option<Vec<String>>,
    body: Vec<PpTok>,
    predefined: bool,
}

/// Extension behaviour requested by `#extension`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtBehavior {
    Enable,
    Disable,
}

pub struct PpOut {
    pub tokens: Vec<Token>,
    pub version: u32,
    /// Extension name → enabled.
    pub extensions: HashMap<String, ExtBehavior>,
    /// Last line seen (for errors at the end of input).
    pub last_line: u32,
    /// Fatal: compilation stops after preprocessing.
    pub fatal: bool,
}

/// Preprocesses `src`. `webgl2` allows `#version 300 es`; `supported` are
/// the GLSL extension names the context offers (enabled WebGL
/// extensions); `vertex` selects stage-specific extension macros.
pub fn preprocess(src: &str, webgl2: bool, supported: &[&str], log: &mut Log) -> PpOut {
    let text = strip(src);
    let mut lines: Vec<(u32, Vec<PpTok>)> = Vec::new();
    let mut line_no = 1u32;
    for raw in text.split('\n') {
        let joined = raw.matches('\u{1}').count() as u32;
        lines.push((line_no, tokenize_line(raw)));
        line_no += 1 + joined;
    }
    // #version: first directive, before any token.
    let mut version = 100;
    let mut first_tok_seen = false;
    let mut macros: HashMap<String, Macro> = HashMap::new();
    let mut extensions = HashMap::new();
    let mut out = Vec::new();
    // Conditional stack: (active, any branch taken, parent active, else seen).
    let mut cond: Vec<(bool, bool, bool, bool)> = Vec::new();
    let mut line_offset: i64 = 0;
    let mut fatal = false;
    let mut last_line = 1;
    let mut version_done = false;

    let def = |macros: &mut HashMap<String, Macro>, name: &str, v: &str| {
        macros.insert(
            name.to_owned(),
            Macro {
                params: None,
                body: tokenize_line(v),
                predefined: true,
            },
        );
    };
    let mut pending: Vec<PpTok> = Vec::new();
    let mut pending_lines: Vec<u32> = Vec::new();

    for (idx, (ln, toks)) in lines.iter().enumerate() {
        let line = (i64::from(*ln) + line_offset).max(0) as u32;
        let active = cond.last().is_none_or(|c| c.0);
        if !toks.is_empty() {
            last_line = line;
        }
        if toks.first().is_some_and(|t| t.t == Pp::Punct("#")) {
            let name = match toks.get(1).map(|t| &t.t) {
                Some(Pp::Ident(s)) => s.clone(),
                None => String::new(),
                Some(_) => "?".into(),
            };
            let args = if toks.len() > 2 { &toks[2..] } else { &[][..] };
            // Conditionals are processed even in skipped regions.
            match name.as_str() {
                "if" | "ifdef" | "ifndef" => {
                    if !active {
                        cond.push((false, true, false, false));
                        continue;
                    }
                    let v = match name.as_str() {
                        "ifdef" | "ifndef" => {
                            let d = match args.first().map(|t| &t.t) {
                                Some(Pp::Ident(s)) => macros.contains_key(s),
                                _ => {
                                    log.error(line, "#", "invalid macro name");
                                    fatal = true;
                                    false
                                }
                            };
                            if name == "ifdef" { d } else { !d }
                        }
                        _ => match eval_if(args, &macros, line, log) {
                            Some(v) => v != 0,
                            None => {
                                fatal = true;
                                false
                            }
                        },
                    };
                    cond.push((v, v, true, false));
                    continue;
                }
                "elif" => {
                    let Some(top) = cond.last_mut() else {
                        log.error(line, "elif", "unexpected #elif found without matching #if");
                        fatal = true;
                        continue;
                    };
                    if top.3 {
                        log.error(line, "elif", "unexpected #elif found after #else");
                        fatal = true;
                        continue;
                    }
                    if !top.2 || top.1 {
                        top.0 = false;
                        continue;
                    }
                    let v = eval_if(args, &macros, line, log);
                    let top = cond.last_mut().expect("cond");
                    match v {
                        Some(v) => {
                            top.0 = v != 0;
                            top.1 = v != 0;
                        }
                        None => {
                            top.0 = false;
                            fatal = true;
                        }
                    }
                    continue;
                }
                "else" => {
                    match cond.last_mut() {
                        Some(top) if !top.3 => {
                            top.0 = top.2 && !top.1;
                            top.1 = true;
                            top.3 = true;
                        }
                        Some(_) => {
                            log.error(line, "else", "unexpected #else found after another #else");
                            fatal = true;
                        }
                        None => {
                            log.error(line, "else", "unexpected #else found without matching #if");
                            fatal = true;
                        }
                    }
                    continue;
                }
                "endif" => {
                    if cond.pop().is_none() {
                        log.error(
                            line,
                            "endif",
                            "unexpected #endif found without matching #if",
                        );
                        fatal = true;
                    }
                    continue;
                }
                _ => {}
            }
            if !active {
                continue;
            }
            match name.as_str() {
                "version" => {
                    if first_tok_seen || version_done {
                        log.error(
                            line,
                            "version",
                            "#version directive must occur before anything else, except for comments and white space",
                        );
                        fatal = true;
                        continue;
                    }
                    version_done = true;
                    let num = args.first().map(PpTok::text).unwrap_or_default();
                    let profile = args.get(1).map(PpTok::text);
                    match (num.as_str(), profile.as_deref()) {
                        ("100", None) => version = 100,
                        ("300", Some("es")) if webgl2 => version = 300,
                        ("300", Some("es")) => {
                            log.error(line, "", "unsupported shader version 300");
                            log.raw("ERROR: 0:?: '' : syntax error\n");
                            fatal = true;
                        }
                        ("300", _) => {
                            log.error(line, "300", "Version number not supported");
                            fatal = true;
                        }
                        (n, _) => {
                            log.error(line, n, "client/version number not supported");
                            fatal = true;
                        }
                    }
                    if !fatal {
                        def(&mut macros, "__VERSION__", &version.to_string());
                    }
                }
                "define" => {
                    first_tok_seen = true;
                    let Some(Pp::Ident(mname)) = args.first().map(|t| &t.t) else {
                        log.error(line, "#", "invalid macro name");
                        fatal = true;
                        continue;
                    };
                    if mname.starts_with("GL_") {
                        log.error(line, mname, "macro name reserved");
                        fatal = true;
                        continue;
                    }
                    if macros.get(mname).is_some_and(|m| m.predefined) {
                        log.error(line, mname, "predefined macro redefined");
                        fatal = true;
                        continue;
                    }
                    let mut rest = &args[1..];
                    let mut params = None;
                    if rest
                        .first()
                        .is_some_and(|t| t.t == Pp::Punct("(") && !t.space)
                    {
                        let mut ps = Vec::new();
                        let mut j = 1;
                        loop {
                            match rest.get(j).map(|t| &t.t) {
                                Some(Pp::Punct(")")) => {
                                    j += 1;
                                    break;
                                }
                                Some(Pp::Ident(p)) => {
                                    ps.push(p.clone());
                                    j += 1;
                                    if rest.get(j).map(|t| &t.t) == Some(&Pp::Punct(",")) {
                                        j += 1;
                                    }
                                }
                                _ => {
                                    log.error(line, mname, "invalid macro name");
                                    fatal = true;
                                    break;
                                }
                            }
                        }
                        rest = &rest[j.min(rest.len())..];
                        params = Some(ps);
                    }
                    macros.insert(
                        mname.clone(),
                        Macro {
                            params,
                            body: rest.to_vec(),
                            predefined: false,
                        },
                    );
                }
                "undef" => {
                    first_tok_seen = true;
                    if let Some(Pp::Ident(n)) = args.first().map(|t| &t.t) {
                        if macros.get(n).is_some_and(|m| m.predefined) {
                            log.error(line, n, "predefined macro undefined");
                            fatal = true;
                        } else {
                            macros.remove(n);
                        }
                    }
                }
                "error" => {
                    first_tok_seen = true;
                    let msg: Vec<String> = args.iter().map(PpTok::text).collect();
                    log.error(line, "", &format!(" {}", msg.join(" ")));
                    fatal = true;
                }
                "pragma" => {
                    first_tok_seen = true;
                    let p = args.first().map(PpTok::text).unwrap_or_default();
                    if p != "STDGL" && p != "optimize" && p != "debug" {
                        log.warning(line, &p, "unrecognized pragma");
                    }
                }
                "extension" => {
                    first_tok_seen = true;
                    let ename = args.first().map(PpTok::text).unwrap_or_default();
                    let beh = args.get(2).map(PpTok::text).unwrap_or_default();
                    let ok = supported.contains(&ename.as_str());
                    let b = match beh.as_str() {
                        "require" | "enable" => ExtBehavior::Enable,
                        "warn" => ExtBehavior::Enable,
                        "disable" => ExtBehavior::Disable,
                        _ => {
                            log.error(line, &beh, "invalid extension behavior");
                            fatal = true;
                            continue;
                        }
                    };
                    if ename == "all" {
                        if b == ExtBehavior::Enable && beh != "warn" {
                            log.error(
                                line,
                                &beh,
                                "extension 'all' cannot have 'require' or 'enable' behavior",
                            );
                            fatal = true;
                        }
                        continue;
                    }
                    if !ok {
                        if beh == "require" {
                            log.error(line, &ename, "extension is not supported");
                            fatal = true;
                        } else if beh != "disable" {
                            log.warning(line, &ename, "extension is not supported");
                        }
                        continue;
                    }
                    extensions.insert(ename, b);
                }
                "line" => {
                    first_tok_seen = true;
                    let expanded = expand(args, &macros, line, log, &mut fatal);
                    if let Some(Pp::Num(n)) = expanded.first().map(|t| &t.t)
                        && let Ok(v) = n.parse::<i64>()
                    {
                        line_offset = v - i64::from(lines[idx].0) - 1;
                    }
                }
                "" => {}
                other => {
                    log.error(line, other, "invalid directive name");
                    fatal = true;
                }
            }
            continue;
        }
        if !active || toks.is_empty() {
            continue;
        }
        if !version_done {
            version_done = true;
            def(&mut macros, "__VERSION__", "100");
        }
        if !first_tok_seen {
            first_tok_seen = true;
        }
        // Predefined macros once the version is known.
        if !macros.contains_key("GL_ES") {
            predefine(&mut macros, version, supported);
        }
        pending.extend(toks.iter().cloned());
        pending_lines.extend(std::iter::repeat_n(line, toks.len()));
        // Expand line by line unless a function-like macro call spans
        // lines (its arguments continue on the next line).
        if open_call(&pending, &macros) && idx + 1 < lines.len() {
            continue;
        }
        let first_line = pending_lines.first().copied().unwrap_or(line);
        let expanded = expand_lines(&pending, &pending_lines, &macros, log, &mut fatal);
        for (t, l) in expanded {
            match convert(&t, l, version, log) {
                Some(tok) => out.push(tok),
                None => {
                    fatal = true;
                }
            }
        }
        let _ = first_line;
        pending.clear();
        pending_lines.clear();
        if fatal {
            break;
        }
    }
    if !version_done {
        def(&mut macros, "__VERSION__", "100");
    }
    if !cond.is_empty() && !fatal {
        log.error(
            last_line,
            "",
            "unexpected end of file found in conditional block",
        );
        fatal = true;
    }
    PpOut {
        tokens: out,
        version,
        extensions,
        last_line,
        fatal,
    }
}

fn predefine(macros: &mut HashMap<String, Macro>, _version: u32, supported: &[&str]) {
    for (n, v) in [("GL_ES", "1"), ("GL_FRAGMENT_PRECISION_HIGH", "1")] {
        macros.insert(
            n.into(),
            Macro {
                params: None,
                body: tokenize_line(v),
                predefined: true,
            },
        );
    }
    for e in supported {
        macros.insert(
            (*e).to_owned(),
            Macro {
                params: None,
                body: tokenize_line("1"),
                predefined: true,
            },
        );
    }
}

/// Whether the tokens end inside an unclosed function-like macro call.
fn open_call(toks: &[PpTok], macros: &HashMap<String, Macro>) -> bool {
    let mut i = 0;
    while i < toks.len() {
        if let Pp::Ident(n) = &toks[i].t
            && macros.get(n).is_some_and(|m| m.params.is_some())
        {
            if i + 1 == toks.len() {
                return true;
            }
            if toks[i + 1].t == Pp::Punct("(") {
                let mut depth = 0;
                let mut j = i + 1;
                while j < toks.len() {
                    match toks[j].t {
                        Pp::Punct("(") => depth += 1,
                        Pp::Punct(")") => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                if j >= toks.len() {
                    return true;
                }
                i = j;
            }
        }
        i += 1;
    }
    false
}

fn expand_lines(
    toks: &[PpTok],
    lines: &[u32],
    macros: &HashMap<String, Macro>,
    log: &mut Log,
    fatal: &mut bool,
) -> Vec<(PpTok, u32)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let line = lines[i];
        // Expand one top-level token (with its call arguments) at a time,
        // so tokens keep their own line.
        let mut end = i + 1;
        if let Pp::Ident(n) = &toks[i].t {
            if n == "__LINE__" {
                out.push((
                    PpTok {
                        t: Pp::Num(line.to_string()),
                        space: true,
                    },
                    line,
                ));
                i += 1;
                continue;
            }
            if macros.get(n).is_some_and(|m| m.params.is_some())
                && toks.get(i + 1).is_some_and(|t| t.t == Pp::Punct("("))
            {
                let mut depth = 0;
                let mut j = i + 1;
                while j < toks.len() {
                    match toks[j].t {
                        Pp::Punct("(") => depth += 1,
                        Pp::Punct(")") => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                end = (j + 1).min(toks.len());
            }
        }
        for t in expand(&toks[i..end], macros, line, log, fatal) {
            out.push((t, line));
        }
        i = end;
    }
    out
}

/// Expands macros in `toks` (one level of rescanning with a hide set).
fn expand(
    toks: &[PpTok],
    macros: &HashMap<String, Macro>,
    line: u32,
    log: &mut Log,
    fatal: &mut bool,
) -> Vec<PpTok> {
    let mut hide = HashSet::new();
    let mut depth = 0;
    expand_with(toks, macros, &mut hide, line, log, fatal, &mut depth)
}

fn expand_with(
    toks: &[PpTok],
    macros: &HashMap<String, Macro>,
    hide: &mut HashSet<String>,
    line: u32,
    log: &mut Log,
    fatal: &mut bool,
    depth: &mut u32,
) -> Vec<PpTok> {
    *depth += 1;
    if *depth > 1000 {
        log.error(line, "", "macro recursion too deep");
        *fatal = true;
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        let Pp::Ident(name) = &t.t else {
            out.push(t.clone());
            i += 1;
            continue;
        };
        if name == "__LINE__" {
            out.push(PpTok {
                t: Pp::Num(line.to_string()),
                space: t.space,
            });
            i += 1;
            continue;
        }
        let Some(m) = macros.get(name).filter(|_| !hide.contains(name)) else {
            out.push(t.clone());
            i += 1;
            continue;
        };
        match &m.params {
            None => {
                hide.insert(name.clone());
                let mut body = expand_with(&m.body, macros, hide, line, log, fatal, depth);
                hide.remove(name);
                if let Some(f) = body.first_mut() {
                    f.space = t.space;
                }
                out.extend(body);
                i += 1;
            }
            Some(params) => {
                if toks.get(i + 1).map(|x| &x.t) != Some(&Pp::Punct("(")) {
                    out.push(t.clone());
                    i += 1;
                    continue;
                }
                // Collect arguments.
                let mut args: Vec<Vec<PpTok>> = vec![Vec::new()];
                let mut d = 0;
                let mut j = i + 1;
                let mut closed = false;
                while j < toks.len() {
                    let tj = &toks[j];
                    match tj.t {
                        Pp::Punct("(") => {
                            d += 1;
                            if d > 1 {
                                args.last_mut().expect("arg").push(tj.clone());
                            }
                        }
                        Pp::Punct(")") => {
                            d -= 1;
                            if d == 0 {
                                closed = true;
                                break;
                            }
                            args.last_mut().expect("arg").push(tj.clone());
                        }
                        Pp::Punct(",") if d == 1 => args.push(Vec::new()),
                        _ => args.last_mut().expect("arg").push(tj.clone()),
                    }
                    j += 1;
                }
                if !closed {
                    log.error(
                        line,
                        name,
                        "unexpected end of file found in macro invocation",
                    );
                    *fatal = true;
                    return out;
                }
                if params.is_empty() && args.len() == 1 && args[0].is_empty() {
                    args.clear();
                }
                if args.len() != params.len() {
                    let msg = if args.len() < params.len() {
                        "macro has too few arguments"
                    } else {
                        "macro has too many arguments"
                    };
                    log.error(line, name, msg);
                    *fatal = true;
                    return out;
                }
                let expanded_args: Vec<Vec<PpTok>> = args
                    .iter()
                    .map(|a| expand_with(a, macros, hide, line, log, fatal, depth))
                    .collect();
                let mut body = Vec::new();
                for bt in &m.body {
                    if let Pp::Ident(n) = &bt.t
                        && let Some(k) = params.iter().position(|p| p == n)
                    {
                        let mut a = expanded_args[k].clone();
                        if let Some(f) = a.first_mut() {
                            f.space = bt.space;
                        }
                        body.extend(a);
                        continue;
                    }
                    body.push(bt.clone());
                }
                hide.insert(name.clone());
                let mut res = expand_with(&body, macros, hide, line, log, fatal, depth);
                hide.remove(name);
                if let Some(f) = res.first_mut() {
                    f.space = t.space;
                }
                out.extend(res);
                i = j + 1;
            }
        }
    }
    *depth -= 1;
    out
}

/// Evaluates a `#if` / `#elif` expression; `None` after an error.
fn eval_if(
    args: &[PpTok],
    macros: &HashMap<String, Macro>,
    line: u32,
    log: &mut Log,
) -> Option<i64> {
    // `defined` first, then expansion.
    let mut pre = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i].t == Pp::Ident("defined".into()) {
            let (name, skip) = match (args.get(i + 1).map(|t| &t.t), args.get(i + 2).map(|t| &t.t))
            {
                (Some(Pp::Punct("(")), Some(Pp::Ident(n))) => (n.clone(), 4),
                (Some(Pp::Ident(n)), _) => (n.clone(), 2),
                _ => {
                    log.error(line, "defined", "invalid macro name");
                    return None;
                }
            };
            pre.push(PpTok {
                t: Pp::Num(if macros.contains_key(&name) { "1" } else { "0" }.into()),
                space: true,
            });
            i += skip;
            continue;
        }
        pre.push(args[i].clone());
        i += 1;
    }
    let mut fatal = false;
    let toks = expand(&pre, macros, line, log, &mut fatal);
    if fatal {
        return None;
    }
    let mut p = IfParser {
        t: &toks,
        i: 0,
        err: None,
    };
    let v = p.ternary();
    if let Some(e) = p.err.take() {
        log.error(line, &e, "unexpected token after conditional expression");
        return None;
    }
    if p.i < toks.len() {
        log.error(
            line,
            &toks[p.i].text(),
            "unexpected token after conditional expression",
        );
        return None;
    }
    if toks.is_empty() {
        log.error(
            line,
            "",
            "unexpected end of file found in conditional expression",
        );
        return None;
    }
    Some(v)
}

struct IfParser<'a> {
    t: &'a [PpTok],
    i: usize,
    err: Option<String>,
}

impl IfParser<'_> {
    fn peek(&self) -> Option<&Pp> {
        self.t.get(self.i).map(|t| &t.t)
    }
    fn eat(&mut self, p: &str) -> bool {
        if matches!(self.peek(), Some(Pp::Punct(x)) if *x == p) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn ternary(&mut self) -> i64 {
        let c = self.bin(0);
        if self.eat("?") {
            let a = self.ternary();
            self.eat(":");
            let b = self.ternary();
            if c != 0 { a } else { b }
        } else {
            c
        }
    }
    fn bin(&mut self, level: usize) -> i64 {
        const LEVELS: &[&[&str]] = &[
            &["||"],
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
        let mut l = self.bin(level + 1);
        loop {
            let op = match self.peek() {
                Some(Pp::Punct(p)) if LEVELS[level].contains(p) => *p,
                _ => return l,
            };
            self.i += 1;
            let r = self.bin(level + 1);
            l = match op {
                "||" => i64::from(l != 0 || r != 0),
                "&&" => i64::from(l != 0 && r != 0),
                "|" => l | r,
                "^" => l ^ r,
                "&" => l & r,
                "==" => i64::from(l == r),
                "!=" => i64::from(l != r),
                "<" => i64::from(l < r),
                ">" => i64::from(l > r),
                "<=" => i64::from(l <= r),
                ">=" => i64::from(l >= r),
                "<<" => l.wrapping_shl(r as u32),
                ">>" => l.wrapping_shr(r as u32),
                "+" => l.wrapping_add(r),
                "-" => l.wrapping_sub(r),
                "*" => l.wrapping_mul(r),
                "/" if r != 0 => l / r,
                "%" if r != 0 => l % r,
                _ => {
                    self.err.get_or_insert_with(|| "division by zero".into());
                    0
                }
            };
        }
    }
    fn unary(&mut self) -> i64 {
        if self.eat("-") {
            return self.unary().wrapping_neg();
        }
        if self.eat("+") {
            return self.unary();
        }
        if self.eat("!") {
            return i64::from(self.unary() == 0);
        }
        if self.eat("~") {
            return !self.unary();
        }
        if self.eat("(") {
            let v = self.ternary();
            self.eat(")");
            return v;
        }
        match self.t.get(self.i) {
            Some(PpTok { t: Pp::Num(n), .. }) => {
                self.i += 1;
                parse_int(n).map_or(0, |(v, _)| i64::from(v))
            }
            Some(t) => {
                self.err.get_or_insert_with(|| t.text());
                self.i += 1;
                0
            }
            None => 0,
        }
    }
}

/// Parses an integer literal (decimal, octal, hex; `u` suffix).
fn parse_int(s: &str) -> Option<(u32, bool)> {
    let (body, unsigned) = match s.strip_suffix(['u', 'U']) {
        Some(b) => (b, true),
        None => (s, false),
    };
    let v = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        if h.is_empty() || !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        u64::from_str_radix(h, 16).unwrap_or(u64::MAX)
    } else if body.len() > 1 && body.starts_with('0') {
        if !body.chars().all(|c| ('0'..='7').contains(&c)) {
            return None;
        }
        u64::from_str_radix(body, 8).unwrap_or(u64::MAX)
    } else {
        if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        body.parse::<u64>().unwrap_or(u64::MAX)
    };
    Some((v.min(u64::from(u32::MAX) + 1) as u32, unsigned))
        .map(|(x, u)| (if v > u64::from(u32::MAX) { u32::MAX } else { x }, u))
}

/// Converts a pp-token to a parser token.
fn convert(t: &PpTok, line: u32, version: u32, log: &mut Log) -> Option<Token> {
    let text = t.text();
    let tok = match &t.t {
        Pp::Ident(s) => Tok::Ident(s.clone()),
        Pp::Punct(p) if *p == "#" || *p == "##" => {
            log.error(line, p, "syntax error");
            return None;
        }
        Pp::Punct(p) => Tok::Punct(p),
        Pp::Other(c) => {
            log.error(line, &c.to_string(), "invalid character");
            return None;
        }
        Pp::Num(n) => {
            let is_hex = n.starts_with("0x") || n.starts_with("0X");
            let is_float =
                !is_hex && (n.contains('.') || n.contains(['e', 'E']) || n.ends_with(['f', 'F']));
            if is_float {
                let mut body = n.as_str();
                if let Some(b) = body.strip_suffix(['f', 'F']) {
                    if version < 300 {
                        log.error(
                            line,
                            n,
                            "Floating-point suffix unsupported prior to GLSL ES 3.00",
                        );
                        log.error(line, n, "syntax error");
                        return None;
                    }
                    body = b;
                }
                match body.parse::<f64>() {
                    Ok(v) => Tok::Float(v as f32),
                    Err(_) => {
                        log.error(line, n, "invalid number");
                        return None;
                    }
                }
            } else {
                if version < 300 && n.ends_with(['u', 'U']) {
                    log.error(line, n, "invalid number");
                    return None;
                }
                match parse_int(n) {
                    Some((v, u)) => {
                        let big = {
                            let body = n.trim_end_matches(['u', 'U']);
                            let full = if let Some(h) =
                                body.strip_prefix("0x").or_else(|| body.strip_prefix("0X"))
                            {
                                u64::from_str_radix(h, 16).ok()
                            } else if body.len() > 1 && body.starts_with('0') {
                                u64::from_str_radix(body, 8).ok()
                            } else {
                                body.parse::<u64>().ok()
                            };
                            full.is_none_or(|f| f > u64::from(u32::MAX))
                        };
                        if big {
                            log.warning(line, n, "Integer overflow");
                        }
                        Tok::Int(v, u)
                    }
                    None => {
                        log.error(line, n, "invalid number");
                        log.error(line, ";", "syntax error");
                        return None;
                    }
                }
            }
        }
    };
    Some(Token { tok, text, line })
}
