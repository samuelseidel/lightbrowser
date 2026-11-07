//! A small, tolerant WebIDL parser for Blink's IDL dialect.
//!
//! It keeps what the binding tables need (definitions, members, argument
//! counts, coarse types, extended attributes) and skips the rest
//! (dictionary bodies, default values, typedef targets). Blink's extended
//! attributes (`[Exposed(Window Foo, Worker Bar)]`, quoted values,
//! `[LegacyFactoryFunction=Image(...)]`) parse without pre-processing.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Num(String),
    Str(String),
    Punct(char),
    /// `...`
    Ellipsis,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub file: String,
    pub line: u32,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

fn lex(src: &str, file: &str) -> Result<Vec<(Tok, u32)>, ParseError> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut line = 1u32;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        match c {
            b'\n' => {
                line += 1;
                i += 1;
            }
            c if c.is_ascii_whitespace() => i += 1,
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    if b[i] == b'\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i += 2;
            }
            b'"' => {
                let start = i + 1;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += 1;
                }
                out.push((Tok::Str(src[start..i.min(b.len())].to_owned()), line));
                i += 1;
            }
            b'.' if b.get(i + 1) == Some(&b'.') && b.get(i + 2) == Some(&b'.') => {
                out.push((Tok::Ellipsis, line));
                i += 3;
            }
            c if c.is_ascii_digit()
                || (c == b'-' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
                || (c == b'.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) =>
            {
                let start = i;
                i += 1;
                while i < b.len()
                    && (b[i].is_ascii_alphanumeric()
                        || b[i] == b'.'
                        || ((b[i] == b'-' || b[i] == b'+') && matches!(b[i - 1], b'e' | b'E')))
                {
                    i += 1;
                }
                out.push((Tok::Num(src[start..i].to_owned()), line));
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                // WebIDL strips one leading underscore from identifiers.
                let s = &src[start..i];
                let s = s.strip_prefix('_').unwrap_or(s);
                out.push((Tok::Ident(s.to_owned()), line));
            }
            b'{' | b'}' | b'(' | b')' | b'[' | b']' | b';' | b':' | b',' | b'=' | b'<' | b'>'
            | b'?' | b'*' | b'-' | b'.' => {
                out.push((Tok::Punct(c as char), line));
                i += 1;
            }
            _ => {
                return Err(ParseError {
                    file: file.to_owned(),
                    line,
                    message: format!("unexpected character {:?}", c as char),
                });
            }
        }
    }
    Ok(out)
}

/// One extended attribute, e.g. `Exposed=(Window,Worker)`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtAttr {
    pub name: String,
    pub value: ExtValue,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExtValue {
    None,
    /// `=Ident`, `="string"` or `=123`.
    Ident(String),
    /// `=(A, B)` or `(A B, C D)`: comma-separated groups of tokens.
    List(Vec<Vec<String>>),
    /// `=Name(args)` (`LegacyFactoryFunction`).
    NamedArgs(String, Vec<Argument>),
}

/// The extended attributes on one construct.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExtAttrs(pub Vec<ExtAttr>);

impl ExtAttrs {
    pub fn get(&self, name: &str) -> Option<&ExtValue> {
        self.0.iter().find(|a| a.name == name).map(|a| &a.value)
    }
    pub fn has(&self, name: &str) -> bool {
        self.get(name).is_some()
    }
    /// Value of `Name=Ident`.
    pub fn ident(&self, name: &str) -> Option<&str> {
        match self.get(name)? {
            ExtValue::Ident(s) => Some(s),
            _ => None,
        }
    }
    /// Identifiers of `Name=X`, `Name=(X, Y)` or `Name(X a, Y b)` (first
    /// token of each group).
    pub fn idents(&self, name: &str) -> Vec<String> {
        match self.get(name) {
            Some(ExtValue::Ident(s)) => vec![s.clone()],
            Some(ExtValue::List(groups)) => {
                groups.iter().filter_map(|g| g.first().cloned()).collect()
            }
            _ => Vec::new(),
        }
    }
}

/// A type, reduced to what stubs need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Type {
    /// Base name (`DOMString`, `unsigned long`, `sequence`, `Promise`,
    /// `union`, an interface name, ...).
    pub name: String,
    pub nullable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Argument {
    pub name: String,
    pub ty: Type,
    pub optional: bool,
    pub variadic: bool,
}

/// Number of required arguments (WebIDL `length`).
pub fn required_args(args: &[Argument]) -> usize {
    args.iter()
        .take_while(|a| !a.optional && !a.variadic)
        .count()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Special {
    Getter,
    Setter,
    Deleter,
    LegacyCaller,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MemberKind {
    Const {
        value: String,
    },
    Attribute {
        readonly: bool,
        is_static: bool,
        stringifier: bool,
        inherit: bool,
        ty: Type,
    },
    Operation {
        is_static: bool,
        specials: Vec<Special>,
        stringifier: bool,
        args: Vec<Argument>,
        ret: Type,
    },
    Constructor {
        args: Vec<Argument>,
    },
    /// Bare `stringifier;`.
    Stringifier,
    Iterable {
        pair: bool,
        is_async: bool,
    },
    Maplike {
        readonly: bool,
    },
    Setlike {
        readonly: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    /// Empty for unnamed special operations, constructors and declarations.
    pub name: String,
    pub kind: MemberKind,
    pub ext: ExtAttrs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    Interface,
    Mixin,
    CallbackInterface,
    Namespace,
    Dictionary,
    Enum,
    Typedef,
    Callback,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Definition {
    pub kind: DefKind,
    pub name: String,
    pub partial: bool,
    pub parent: Option<String>,
    pub ext: ExtAttrs,
    pub members: Vec<Member>,
    /// Path of the file, relative to the IDL root.
    pub file: String,
}

/// `Target includes Mixin;`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Includes {
    pub target: String,
    pub mixin: String,
    pub file: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct IdlFile {
    pub definitions: Vec<Definition>,
    pub includes: Vec<Includes>,
}

struct Parser<'a> {
    toks: Vec<(Tok, u32)>,
    pos: usize,
    file: &'a str,
}

type PResult<T> = Result<T, ParseError>;

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.0)
    }
    fn peek_at(&self, n: usize) -> Option<&Tok> {
        self.toks.get(self.pos + n).map(|t| &t.0)
    }
    fn line(&self) -> u32 {
        self.toks
            .get(self.pos.min(self.toks.len().saturating_sub(1)))
            .map_or(0, |t| t.1)
    }
    fn err<T>(&self, message: impl Into<String>) -> PResult<T> {
        Err(ParseError {
            file: self.file.to_owned(),
            line: self.line(),
            message: message.into(),
        })
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).map(|t| t.0.clone());
        self.pos += 1;
        t
    }
    fn is_punct(&self, c: char) -> bool {
        self.peek() == Some(&Tok::Punct(c))
    }
    fn is_ident(&self, s: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(i)) if i == s)
    }
    fn eat_punct(&mut self, c: char) -> bool {
        if self.is_punct(c) {
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
    fn expect_punct(&mut self, c: char) -> PResult<()> {
        if self.eat_punct(c) {
            Ok(())
        } else {
            self.err(format!("expected '{c}', found {:?}", self.peek()))
        }
    }
    fn ident(&mut self) -> PResult<String> {
        match self.next() {
            Some(Tok::Ident(s)) => Ok(s),
            t => {
                self.pos -= 1;
                self.err(format!("expected identifier, found {t:?}"))
            }
        }
    }

    /// Skip a balanced group starting at an opening bracket.
    fn skip_group(&mut self) -> PResult<()> {
        let mut depth = 0i32;
        loop {
            match self.next() {
                Some(Tok::Punct('{' | '(' | '[')) => depth += 1,
                Some(Tok::Punct('}' | ')' | ']')) => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                Some(_) => {}
                None => return self.err("unbalanced brackets"),
            }
        }
    }

    fn skip_to_semicolon(&mut self) -> PResult<()> {
        loop {
            match self.peek() {
                Some(Tok::Punct(';')) => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(Tok::Punct('{' | '(' | '[')) => self.skip_group()?,
                Some(_) => self.pos += 1,
                None => return self.err("missing ';'"),
            }
        }
    }

    fn ext_attrs(&mut self) -> PResult<ExtAttrs> {
        let mut out = Vec::new();
        if !self.eat_punct('[') {
            return Ok(ExtAttrs(out));
        }
        loop {
            if self.eat_punct(']') {
                break;
            }
            let name = self.ident()?;
            let value = if self.eat_punct('=') {
                match self.peek().cloned() {
                    Some(Tok::Punct('(')) => ExtValue::List(self.token_groups()?),
                    Some(Tok::Ident(v)) => {
                        self.pos += 1;
                        if self.is_punct('(') {
                            ExtValue::NamedArgs(v, self.arguments()?)
                        } else {
                            ExtValue::Ident(v)
                        }
                    }
                    Some(Tok::Str(v) | Tok::Num(v)) => {
                        self.pos += 1;
                        ExtValue::Ident(v)
                    }
                    Some(Tok::Punct('*')) => {
                        self.pos += 1;
                        ExtValue::Ident("*".into())
                    }
                    t => return self.err(format!("bad extended attribute value {t:?}")),
                }
            } else if self.is_punct('(') {
                ExtValue::List(self.token_groups()?)
            } else {
                ExtValue::None
            };
            out.push(ExtAttr { name, value });
            if !self.eat_punct(',') {
                self.expect_punct(']')?;
                break;
            }
        }
        Ok(ExtAttrs(out))
    }

    /// `( a b, c d )` as token groups.
    fn token_groups(&mut self) -> PResult<Vec<Vec<String>>> {
        self.expect_punct('(')?;
        let mut groups = vec![Vec::new()];
        loop {
            match self.next() {
                Some(Tok::Punct(')')) => break,
                Some(Tok::Punct(',')) => groups.push(Vec::new()),
                Some(Tok::Ident(s) | Tok::Str(s) | Tok::Num(s)) => {
                    groups.last_mut().expect("non-empty").push(s)
                }
                Some(Tok::Punct(c)) => groups.last_mut().expect("non-empty").push(c.to_string()),
                Some(Tok::Ellipsis) => {}
                None => return self.err("unterminated extended attribute list"),
            }
        }
        groups.retain(|g| !g.is_empty());
        Ok(groups)
    }

    fn ty(&mut self) -> PResult<Type> {
        // Types may carry their own extended attributes.
        self.ext_attrs()?;
        if self.is_punct('(') {
            self.skip_group()?;
            let nullable = self.eat_punct('?');
            return Ok(Type {
                name: "union".into(),
                nullable,
            });
        }
        let first = self.ident()?;
        let name = match first.as_str() {
            "unsigned" | "unrestricted" => {
                let mut n = format!("{first} {}", self.ident()?);
                if self.eat_ident("long") {
                    n.push_str(" long");
                }
                n
            }
            "long" => {
                if self.eat_ident("long") {
                    "long long".into()
                } else {
                    first
                }
            }
            _ => first,
        };
        if self.is_punct('<') {
            let mut depth = 0;
            loop {
                match self.next() {
                    Some(Tok::Punct('<')) => depth += 1,
                    Some(Tok::Punct('>')) => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    Some(Tok::Punct('(' | '[')) => {
                        self.pos -= 1;
                        self.skip_group()?;
                    }
                    Some(_) => {}
                    None => return self.err("unterminated generic type"),
                }
            }
        }
        let nullable = self.eat_punct('?');
        Ok(Type { name, nullable })
    }

    fn arguments(&mut self) -> PResult<Vec<Argument>> {
        self.expect_punct('(')?;
        let mut args = Vec::new();
        if self.eat_punct(')') {
            return Ok(args);
        }
        loop {
            self.ext_attrs()?;
            let optional = self.eat_ident("optional");
            let ty = self.ty()?;
            let variadic = matches!(self.peek(), Some(Tok::Ellipsis));
            if variadic {
                self.pos += 1;
            }
            let name = self.ident()?;
            if self.eat_punct('=') {
                // Default value: skip to ',' or ')' at depth 0.
                loop {
                    match self.peek() {
                        Some(Tok::Punct(',' | ')')) => break,
                        Some(Tok::Punct('{' | '[' | '(')) => self.skip_group()?,
                        Some(_) => self.pos += 1,
                        None => return self.err("unterminated default value"),
                    }
                }
            }
            args.push(Argument {
                name,
                ty,
                optional,
                variadic,
            });
            if self.eat_punct(')') {
                return Ok(args);
            }
            self.expect_punct(',')?;
        }
    }

    fn file(&mut self) -> PResult<IdlFile> {
        let mut out = IdlFile::default();
        while self.peek().is_some() {
            if self.eat_punct(';') {
                continue;
            }
            let ext = self.ext_attrs()?;
            let partial = self.eat_ident("partial");
            let kw = self.ident()?;
            let file = self.file.to_owned();
            match kw.as_str() {
                "interface" | "namespace" => {
                    let mixin = kw == "interface" && self.eat_ident("mixin");
                    let kind = if kw == "namespace" {
                        DefKind::Namespace
                    } else if mixin {
                        DefKind::Mixin
                    } else {
                        DefKind::Interface
                    };
                    out.definitions
                        .push(self.body_definition(kind, partial, ext, file)?);
                }
                "callback" => {
                    if self.eat_ident("interface") {
                        out.definitions.push(self.body_definition(
                            DefKind::CallbackInterface,
                            partial,
                            ext,
                            file,
                        )?);
                    } else {
                        let name = self.ident()?;
                        self.skip_to_semicolon()?;
                        out.definitions.push(Definition {
                            kind: DefKind::Callback,
                            name,
                            partial,
                            parent: None,
                            ext,
                            members: Vec::new(),
                            file,
                        });
                    }
                }
                "dictionary" | "enum" => {
                    let name = self.ident()?;
                    let mut parent = None;
                    if self.eat_punct(':') {
                        parent = Some(self.ident()?);
                    }
                    self.skip_to_semicolon()?;
                    out.definitions.push(Definition {
                        kind: if kw == "enum" {
                            DefKind::Enum
                        } else {
                            DefKind::Dictionary
                        },
                        name,
                        partial,
                        parent,
                        ext,
                        members: Vec::new(),
                        file,
                    });
                }
                "typedef" => {
                    self.ty()?;
                    let name = self.ident()?;
                    self.expect_punct(';')?;
                    out.definitions.push(Definition {
                        kind: DefKind::Typedef,
                        name,
                        partial,
                        parent: None,
                        ext,
                        members: Vec::new(),
                        file,
                    });
                }
                _ => {
                    if self.eat_ident("includes") {
                        let mixin = self.ident()?;
                        self.expect_punct(';')?;
                        out.includes.push(Includes {
                            target: kw,
                            mixin,
                            file,
                        });
                    } else {
                        return self.err(format!("unexpected '{kw}' at top level"));
                    }
                }
            }
        }
        Ok(out)
    }

    fn body_definition(
        &mut self,
        kind: DefKind,
        partial: bool,
        ext: ExtAttrs,
        file: String,
    ) -> PResult<Definition> {
        let name = self.ident()?;
        let mut parent = None;
        if self.eat_punct(':') {
            parent = Some(self.ident()?);
        }
        self.expect_punct('{')?;
        let mut members = Vec::new();
        while !self.eat_punct('}') {
            if self.peek().is_none() {
                return self.err(format!("unterminated body of {name}"));
            }
            members.push(self.member()?);
        }
        self.expect_punct(';')?;
        Ok(Definition {
            kind,
            name,
            partial,
            parent,
            ext,
            members,
            file,
        })
    }

    fn member(&mut self) -> PResult<Member> {
        let ext = self.ext_attrs()?;
        let unnamed = |kind| Member {
            name: String::new(),
            kind,
            ext: ext.clone(),
        };
        if self.eat_ident("const") {
            self.ty()?;
            let name = self.ident()?;
            self.expect_punct('=')?;
            let mut value = String::new();
            while !self.is_punct(';') {
                match self.next() {
                    Some(Tok::Ident(s) | Tok::Num(s) | Tok::Str(s)) => value.push_str(&s),
                    Some(Tok::Punct(c)) => value.push(c),
                    _ => return self.err("bad constant"),
                }
            }
            self.expect_punct(';')?;
            return Ok(Member {
                name,
                kind: MemberKind::Const { value },
                ext,
            });
        }
        if self.is_ident("constructor") && self.peek_at(1) == Some(&Tok::Punct('(')) {
            self.pos += 1;
            let args = self.arguments()?;
            self.expect_punct(';')?;
            return Ok(unnamed(MemberKind::Constructor { args }));
        }
        if self.is_ident("stringifier") && self.peek_at(1) == Some(&Tok::Punct(';')) {
            self.pos += 2;
            return Ok(unnamed(MemberKind::Stringifier));
        }
        // iterable / async iterable / maplike / setlike
        let readonly_decl = self.is_ident("readonly")
            && matches!(self.peek_at(1), Some(Tok::Ident(s)) if s == "maplike" || s == "setlike");
        if readonly_decl {
            self.pos += 1;
        }
        let is_async = if self.is_ident("async")
            && matches!(self.peek_at(1), Some(Tok::Ident(s)) if s == "iterable")
        {
            self.pos += 1;
            true
        } else {
            false
        };
        if let Some(Tok::Ident(kw)) = self.peek().cloned()
            && matches!(
                kw.as_str(),
                "iterable" | "async_iterable" | "maplike" | "setlike"
            )
            && self.peek_at(1) == Some(&Tok::Punct('<'))
        {
            self.pos += 1;
            // Count type arguments at depth 1.
            let mut depth = 0;
            let mut commas = 0;
            loop {
                match self.next() {
                    Some(Tok::Punct('<')) => depth += 1,
                    Some(Tok::Punct('>')) => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    Some(Tok::Punct(',')) if depth == 1 => commas += 1,
                    Some(Tok::Punct('(' | '[')) => {
                        self.pos -= 1;
                        self.skip_group()?;
                    }
                    Some(_) => {}
                    None => return self.err("unterminated iterable"),
                }
            }
            self.skip_to_semicolon()?;
            let kind = match kw.as_str() {
                "maplike" => MemberKind::Maplike {
                    readonly: readonly_decl,
                },
                "setlike" => MemberKind::Setlike {
                    readonly: readonly_decl,
                },
                _ => MemberKind::Iterable {
                    pair: commas > 0,
                    is_async: is_async || kw == "async_iterable",
                },
            };
            return Ok(unnamed(kind));
        }

        let mut is_static = false;
        let mut stringifier = false;
        let mut inherit = false;
        let mut readonly = false;
        let mut specials = Vec::new();
        while let Some(Tok::Ident(s)) = self.peek() {
            match s.as_str() {
                "static" => is_static = true,
                "stringifier" => stringifier = true,
                "inherit" => inherit = true,
                "readonly" => readonly = true,
                "getter" => specials.push(Special::Getter),
                "setter" => specials.push(Special::Setter),
                "deleter" => specials.push(Special::Deleter),
                "legacycaller" => specials.push(Special::LegacyCaller),
                _ => break,
            }
            self.pos += 1;
        }
        if self.eat_ident("attribute") {
            let ty = self.ty()?;
            // Blink allows dashed names (`margin-top`, CSS properties).
            let mut name = self.ident()?;
            while self.is_punct('-') && matches!(self.peek_at(1), Some(Tok::Ident(_))) {
                self.pos += 1;
                name.push('-');
                name.push_str(&self.ident()?);
            }
            self.expect_punct(';')?;
            return Ok(Member {
                name,
                kind: MemberKind::Attribute {
                    readonly,
                    is_static,
                    stringifier,
                    inherit,
                    ty,
                },
                ext,
            });
        }
        let ret = self.ty()?;
        let name = if self.is_punct('(') {
            String::new()
        } else {
            self.ident()?
        };
        let args = self.arguments()?;
        self.expect_punct(';')?;
        Ok(Member {
            name,
            kind: MemberKind::Operation {
                is_static,
                specials,
                stringifier,
                args,
                ret,
            },
            ext,
        })
    }
}

/// Parse one IDL file. `file` is used in errors and recorded on definitions.
pub fn parse_idl(src: &str, file: &str) -> Result<IdlFile, ParseError> {
    let toks = lex(src, file)?;
    Parser { toks, pos: 0, file }.file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blink_dialect() {
        let src = r#"
            // comment
            [Exposed(Window Foo, Worker Bar), RuntimeEnabled="X",
             LegacyFactoryFunction=Image(optional unsigned long width, optional unsigned long height)]
            interface A : B {
                const unsigned short ONE = 0x01;
                [Reflect] attribute DOMString? title;
                readonly attribute (DOMString or long) u;
                static Promise<sequence<long>> make(long a, optional any b = {}, long... rest);
                getter DOMString (unsigned long index);
                getter unsigned long long (DOMString name);
                stringifier;
                iterable<DOMString, long>;
                readonly setlike<DOMString>;
                constructor(DOMString type, optional EventInit init = {});
                [CEReactions] undefined required();
            };
            A includes M;
            partial interface mixin M { attribute EventHandler onfoo; };
            dictionary D : E { required long x = 1; };
            enum En { "a", "b" };
            typedef (long or DOMString) T;
            callback F = undefined (long x);
        "#;
        let f = parse_idl(src, "t.idl").unwrap();
        let a = &f.definitions[0];
        assert_eq!(a.name, "A");
        assert_eq!(a.parent.as_deref(), Some("B"));
        assert_eq!(a.ext.idents("Exposed"), ["Window", "Worker"]);
        assert_eq!(a.ext.ident("RuntimeEnabled"), Some("X"));
        assert!(
            matches!(a.ext.get("LegacyFactoryFunction"), Some(ExtValue::NamedArgs(n, args)) if n == "Image" && required_args(args) == 0)
        );
        let names: Vec<_> = a.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "ONE", "title", "u", "make", "", "", "", "", "", "", "required"
            ]
        );
        match &a.members[3].kind {
            MemberKind::Operation {
                is_static, args, ..
            } => {
                assert!(*is_static);
                assert_eq!(required_args(args), 1);
            }
            k => panic!("{k:?}"),
        }
        assert!(matches!(
            a.members[7].kind,
            MemberKind::Iterable {
                pair: true,
                is_async: false
            }
        ));
        assert!(matches!(
            a.members[8].kind,
            MemberKind::Setlike { readonly: true }
        ));
        assert_eq!(f.includes[0].mixin, "M");
        assert_eq!(f.definitions[1].kind, DefKind::Mixin);
        assert!(f.definitions[1].partial);
        assert_eq!(f.definitions.len(), 6);
    }
}
