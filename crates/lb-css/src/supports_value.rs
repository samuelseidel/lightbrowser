//! Property values against Chrome's grammars, for `CSS.supports()` and
//! `@supports`.
//!
//! The grammars are CSS value definition syntax (`supports.grammar`,
//! one property or type per line), seeded from the specs (@webref/css)
//! and edited until Chrome 153 gives the same answers on the recorded
//! corpus (`lb-page/tests/css_supports/corpus.jsonl`). The file is parsed
//! once into an arena; a value is matched by collecting every position
//! each grammar node can end at (so `||`, `&&` and repetitions need no
//! backtracking rules), memoized per node and position.
//!
//! In Rust, not the file: the numeric types with math functions (`calc()`
//! and the rest, typed arithmetic as in Chrome), `<length-percentage>` and
//! the other mixed types, identifiers, strings, URLs, hex and named
//! colors, relative color channel keywords, and the comma rule (a comma
//! next to an omitted part is omitted too, never doubled or trailing).

use std::collections::HashMap;
use std::sync::OnceLock;

use cssparser::{Parser, Token};

use crate::supports_data::GRAMMAR;

/// A node index (`NONE`: no node, as in an empty function).
type Id = u32;
const NONE: Id = Id::MAX;
/// An index into [`Grammar::names`] (`NO_NAME`: none).
type Name = u16;
const NO_NAME: Name = Name::MAX;

/// Kids of a group: a range of [`Grammar::kids`].
#[derive(Clone, Copy)]
struct Span(u32, u16);

/// A grammar node (12 bytes: the grammar stays small in memory).
#[derive(Clone, Copy)]
enum Node {
    Kw(Name),
    /// Punctuation; `true` when quoted (`','`: a comma that is never omitted).
    Lit(u8, bool),
    /// A type, its range (an index into [`Grammar::ranges`]: `<length
    /// [0,∞]>`, a string's length), and the keywords a `<custom-ident>`
    /// excludes (`<custom-ident ~none ~auto>`, the `~` text as a name).
    Type(Name, u16, Name),
    Prop(Name),
    Func(Name, Id),
    /// `( ... )` or `'[' ... ']'` in the grammar: a block of that kind.
    Block(u8, Id),
    Seq(Span),
    All(Span),
    Any(Span),
    /// `a ^^ b`: as `||`, but parsed as Blink does: the first part (in
    /// order) that matches takes the longest match, no backtracking.
    Greedy(Span),
    One(Span),
    Mult(Id, u16, u16, bool),
    /// `!`: the group must not be empty.
    Req(Id),
}

pub(crate) struct Grammar {
    nodes: Vec<Node>,
    kids: Vec<Id>,
    names: Vec<&'static str>,
    ranges: Vec<(f32, f32)>,
    /// Per name: the property (or `<'name'>` line) and the type it names.
    prop_of: Vec<Id>,
    type_of: Vec<Id>,
    /// The properties Chrome knows, sorted.
    props: Vec<&'static str>,
}

/// The parsed grammar file (constant data, parsed on first use).
pub(crate) fn grammar() -> &'static Grammar {
    static G: OnceLock<Grammar> = OnceLock::new();
    G.get_or_init(|| Grammar::parse(GRAMMAR))
}

// ---------------------------------------------------------------------------
// Grammar text
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum GTok {
    Type(&'static str),
    Func(&'static str),
    Kw(&'static str),
    Num(u16),
    Lit(char, bool),
    Op(&'static str),
}

fn gtokens(s: &'static str) -> Vec<GTok> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'-' || c == b'_';
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'<' {
            let (start, mut depth) = (i + 1, 1);
            i += 1;
            while i < b.len() && depth > 0 {
                depth += match b[i] {
                    b'<' => 1,
                    b'>' => -1,
                    _ => 0,
                };
                i += 1;
            }
            out.push(GTok::Type(s[start..i - 1].trim()));
        } else if c == b'\'' {
            let end = i + 1 + s[i + 1..].find('\'').unwrap_or(0);
            out.push(GTok::Lit(s[i + 1..end].chars().next().unwrap_or(' '), true));
            i = end + 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            out.push(GTok::Num(s[start..i].parse().unwrap_or(0)));
        } else if c.is_ascii_alphabetic()
            || c == b'_'
            || (c == b'-'
                && b.get(i + 1)
                    .is_some_and(|&n| n.is_ascii_alphabetic() || n == b'-'))
        {
            let start = i;
            while i < b.len() && ident(b[i]) {
                i += 1;
            }
            if b.get(i) == Some(&b'(') {
                out.push(GTok::Func(&s[start..i]));
                i += 1;
            } else {
                out.push(GTok::Kw(&s[start..i]));
            }
        } else if s[i..].starts_with("||") || s[i..].starts_with("&&") || s[i..].starts_with("^^") {
            out.push(GTok::Op(&s[i..i + 2]));
            i += 2;
        } else {
            let op = &s[i..i + 1];
            match c {
                b'[' | b']' | b'|' | b'?' | b'*' | b'+' | b'#' | b'!' | b'{' | b'}' | b'('
                | b')' => out.push(GTok::Op(op)),
                _ => out.push(GTok::Lit(c as char, false)),
            }
            i += 1;
        }
    }
    out
}

/// The grammar being built: nodes, kids and the name interner.
struct Builder {
    g: Grammar,
    interned: HashMap<&'static str, Name>,
}

impl Builder {
    fn name(&mut self, s: &'static str) -> Name {
        if let Some(&n) = self.interned.get(s) {
            return n;
        }
        let n = self.g.names.len() as Name;
        self.g.names.push(s);
        self.interned.insert(s, n);
        n
    }

    fn span(&mut self, kids: &[Id]) -> Span {
        let s = Span(self.g.kids.len() as u32, kids.len() as u16);
        self.g.kids.extend_from_slice(kids);
        s
    }
}

struct GParser<'g> {
    t: Vec<GTok>,
    i: usize,
    b: &'g mut Builder,
}

impl GParser<'_> {
    fn peek(&self) -> Option<GTok> {
        self.t.get(self.i).copied()
    }

    fn push(&mut self, n: Node) -> Id {
        self.b.g.nodes.push(n);
        (self.b.g.nodes.len() - 1) as Id
    }

    fn group(&mut self, kids: &[Id], wrap: fn(Span) -> Node) -> Id {
        let s = self.b.span(kids);
        self.push(wrap(s))
    }

    fn list(
        &mut self,
        op: &'static str,
        next: fn(&mut Self) -> Option<Id>,
        wrap: fn(Span) -> Node,
    ) -> Option<Id> {
        let mut kids = vec![next(self)?];
        while self.peek() == Some(GTok::Op(op)) {
            self.i += 1;
            kids.push(next(self)?);
        }
        Some(if kids.len() == 1 {
            kids[0]
        } else {
            self.group(&kids, wrap)
        })
    }

    fn one(&mut self) -> Option<Id> {
        self.list("|", Self::any, Node::One)
    }

    fn any(&mut self) -> Option<Id> {
        self.list("||", Self::greedy, Node::Any)
    }

    fn greedy(&mut self) -> Option<Id> {
        self.list("^^", Self::all, Node::Greedy)
    }

    fn all(&mut self) -> Option<Id> {
        self.list("&&", Self::seq, Node::All)
    }

    fn seq(&mut self) -> Option<Id> {
        let mut kids = Vec::new();
        loop {
            match self.peek() {
                None => break,
                Some(GTok::Op("|" | "||" | "&&" | "^^" | "]" | ")")) => break,
                Some(GTok::Lit(']', _)) => break,
                _ => {}
            }
            let t = self.term()?;
            kids.push(self.mult(t)?);
        }
        match kids.len() {
            0 => None,
            1 => Some(kids[0]),
            _ => Some(self.group(&kids, Node::Seq)),
        }
    }

    /// The inside of a function or block: empty (`NONE`) or a grammar.
    fn inner(&mut self, close: GTok) -> Option<Id> {
        let n = if self.peek() == Some(close) {
            NONE
        } else {
            self.one()?
        };
        (self.peek() == Some(close)).then(|| {
            self.i += 1;
            n
        })
    }

    fn term(&mut self) -> Option<Id> {
        let t = self.peek()?;
        self.i += 1;
        Some(match t {
            GTok::Op("[") => {
                let n = self.one()?;
                (self.peek() == Some(GTok::Op("]"))).then_some(())?;
                self.i += 1;
                // A group is a node of its own, so `[ a b ]?` repeats the group.
                self.group(&[n], Node::Seq)
            }
            GTok::Op("(") => {
                let n = self.inner(GTok::Op(")"))?;
                self.push(Node::Block(b'(', n))
            }
            GTok::Lit('[', _) => {
                let n = self.inner(GTok::Lit(']', true))?;
                self.push(Node::Block(b'[', n))
            }
            GTok::Func(name) => {
                let n = self.inner(GTok::Op(")"))?;
                let name = self.b.name(name);
                self.push(Node::Func(name, n))
            }
            GTok::Type(inner) => {
                if let Some(p) = inner.strip_prefix('\'') {
                    let name = self.b.name(p.trim_end_matches('\''));
                    self.push(Node::Prop(name))
                } else {
                    let (inner, except) = match inner.find('~') {
                        Some(i) => (inner[..i].trim(), &inner[i..]),
                        None => (inner, ""),
                    };
                    let (name, range) = match inner.find('[') {
                        Some(i) if !inner.starts_with("boolean-expr") => (
                            inner[..i].trim(),
                            Some(inner[i + 1..].trim_end_matches(']')),
                        ),
                        _ => (inner, None),
                    };
                    let (lo, hi) = range.map_or((f32::NEG_INFINITY, f32::INFINITY), |r| {
                        let mut it = r.split(',').map(range_num);
                        (
                            it.next().unwrap_or(f32::NEG_INFINITY),
                            it.next().unwrap_or(f32::INFINITY),
                        )
                    });
                    let range = match self.b.g.ranges.iter().position(|r| *r == (lo, hi)) {
                        Some(i) => i,
                        None => {
                            self.b.g.ranges.push((lo, hi));
                            self.b.g.ranges.len() - 1
                        }
                    } as u16;
                    let name = self.b.name(name);
                    let except = if except.is_empty() {
                        NO_NAME
                    } else {
                        self.b.name(except)
                    };
                    self.push(Node::Type(name, range, except))
                }
            }
            GTok::Kw(k) => {
                let k = self.b.name(k);
                self.push(Node::Kw(k))
            }
            GTok::Lit(c, q) => self.push(Node::Lit(c as u8, q)),
            GTok::Op(o) if matches!(o, "+" | "*") => self.push(Node::Lit(o.as_bytes()[0], false)),
            _ => return None,
        })
    }

    fn mult(&mut self, mut n: Id) -> Option<Id> {
        loop {
            let (lo, hi, comma) = match self.peek() {
                Some(GTok::Op("?")) => (0, 1, false),
                Some(GTok::Op("*")) => (0, u16::MAX, false),
                Some(GTok::Op("+")) => (1, u16::MAX, false),
                Some(GTok::Op("!")) => {
                    self.i += 1;
                    n = self.push(Node::Req(n));
                    continue;
                }
                Some(GTok::Op("#")) => {
                    if self.t.get(self.i + 1) == Some(&GTok::Op("{")) {
                        self.i += 1;
                        let (lo, hi) = self.braces()?;
                        n = self.push(Node::Mult(n, lo, hi, true));
                        continue;
                    }
                    (1, u16::MAX, true)
                }
                Some(GTok::Op("{")) => {
                    let (lo, hi) = self.braces()?;
                    n = self.push(Node::Mult(n, lo, hi, false));
                    continue;
                }
                _ => return Some(n),
            };
            self.i += 1;
            n = self.push(Node::Mult(n, lo, hi, comma));
        }
    }

    /// `{m}`, `{m,n}`, `{m,}` at `{`.
    fn braces(&mut self) -> Option<(u16, u16)> {
        self.i += 1;
        let GTok::Num(lo) = self.peek()? else {
            return None;
        };
        self.i += 1;
        let mut hi = lo;
        if self.peek() == Some(GTok::Lit(',', false)) {
            self.i += 1;
            hi = match self.peek()? {
                GTok::Num(h) => {
                    self.i += 1;
                    h
                }
                _ => u16::MAX,
            };
        }
        (self.peek() == Some(GTok::Op("}"))).then_some(())?;
        self.i += 1;
        Some((lo, hi))
    }
}

fn range_num(s: &str) -> f32 {
    let s = s.trim();
    let neg = s.starts_with('-');
    let s = s.trim_start_matches(['-', '+']);
    if s.starts_with('∞') {
        return if neg {
            f32::NEG_INFINITY
        } else {
            f32::INFINITY
        };
    }
    let end = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let v: f32 = s[..end].parse().unwrap_or(0.0);
    if neg { -v } else { v }
}

impl Grammar {
    fn parse(text: &'static str) -> Grammar {
        let mut b = Builder {
            g: Grammar {
                nodes: Vec::new(),
                kids: Vec::new(),
                names: Vec::new(),
                ranges: vec![(f32::NEG_INFINITY, f32::INFINITY)],
                prop_of: Vec::new(),
                type_of: Vec::new(),
                props: Vec::new(),
            },
            interned: HashMap::new(),
        };
        let mut defs = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((name, syntax)) = line.split_once(" = ") else {
                continue;
            };
            let mut p = GParser {
                t: gtokens(syntax),
                i: 0,
                b: &mut b,
            };
            let root = p.one().filter(|_| p.i == p.t.len());
            debug_assert!(root.is_some(), "bad grammar line: {line}");
            let Some(root) = root else { continue };
            // `<'name'>`: a property Chrome does not know, for references.
            let (key, is_type, known) = match name.strip_prefix('<') {
                Some(t) => match t.strip_prefix('\'') {
                    Some(p) => (p.trim_end_matches(['\'', '>']), false, false),
                    None => (t.trim_end_matches('>'), true, false),
                },
                None => (name, false, true),
            };
            let n = b.name(key);
            defs.push((n, is_type, root));
            if known {
                b.g.props.push(key);
            }
        }
        let mut g = b.g;
        g.prop_of = vec![NONE; g.names.len()];
        g.type_of = vec![NONE; g.names.len()];
        for (n, is_type, root) in defs {
            let table = if is_type {
                &mut g.type_of
            } else {
                &mut g.prop_of
            };
            table[n as usize] = root;
        }
        g.props.sort_unstable();
        g.nodes.shrink_to_fit();
        g.kids.shrink_to_fit();
        g.names.shrink_to_fit();
        g
    }

    /// Whether Chrome knows the property (lowercase name).
    pub(crate) fn has_property(&self, name: &str) -> bool {
        self.props.binary_search(&name).is_ok()
    }

    fn lookup(&self, table: &[Id], name: &str) -> Option<Id> {
        let i = self.names.iter().position(|n| *n == name)?;
        Some(table[i]).filter(|&r| r != NONE)
    }

    fn kids(&self, s: Span) -> &[Id] {
        &self.kids[s.0 as usize..s.0 as usize + s.1 as usize]
    }
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum Cv {
    Ident(String),
    Num(f32, bool),
    Pct(f32),
    Dim(f32, String),
    Str(String),
    Url,
    Hash(String),
    Delim(char),
    Comma,
    Colon,
    Semicolon,
    Func(String, Vec<Comp>),
    Block(char, Vec<Comp>),
    Other,
}

#[derive(Debug)]
pub(crate) struct Comp {
    pub(crate) cv: Cv,
    /// Whitespace before it.
    pub(crate) ws: bool,
}

/// The component values of `text`, or `None` when a token is bad (bad
/// string or URL, unmatched closing bracket).
pub(crate) fn components(text: &str) -> Option<Vec<Comp>> {
    let mut p = Parser::new(text);
    let mut ok = true;
    let out = comps(&mut p, &mut ok);
    ok.then_some(out)
}

fn comps(p: &mut Parser<'_>, ok: &mut bool) -> Vec<Comp> {
    let mut out = Vec::new();
    let mut ws = false;
    while let Ok(t) = p.next_including_whitespace() {
        let t = t.clone();
        let block = |p: &mut Parser<'_>, ok: &mut bool| {
            p.parse_nested_block(|p| Ok::<_, cssparser::ParseError<()>>(comps(p, ok)))
                .unwrap_or_default()
        };
        let cv = match t {
            Token::WhiteSpace(_) => {
                ws = true;
                continue;
            }
            Token::Ident(s) => Cv::Ident(s.to_ascii_lowercase()),
            Token::Number {
                value, int_value, ..
            } => Cv::Num(value, int_value.is_some()),
            Token::Percentage { unit_value, .. } => Cv::Pct(unit_value * 100.0),
            Token::Dimension { value, unit, .. } => Cv::Dim(value, unit.to_ascii_lowercase()),
            Token::QuotedString(s) => Cv::Str(s.to_string()),
            Token::UnquotedUrl(_) => Cv::Url,
            Token::Hash(h) | Token::IDHash(h) => Cv::Hash(h.to_ascii_lowercase()),
            Token::Delim(c) => Cv::Delim(c),
            Token::Comma => Cv::Comma,
            Token::Colon => Cv::Colon,
            Token::Semicolon => Cv::Semicolon,
            Token::Function(name) => {
                let name = name.to_ascii_lowercase();
                Cv::Func(name, block(p, ok))
            }
            Token::ParenthesisBlock => Cv::Block('(', block(p, ok)),
            Token::SquareBracketBlock => Cv::Block('[', block(p, ok)),
            Token::CurlyBracketBlock => Cv::Block('{', block(p, ok)),
            Token::BadString(_)
            | Token::BadUrl(_)
            | Token::CloseParenthesis
            | Token::CloseSquareBracket
            | Token::CloseCurlyBracket => {
                *ok = false;
                Cv::Other
            }
            _ => Cv::Other,
        };
        out.push(Comp { cv, ws });
        ws = false;
    }
    out
}

// ---------------------------------------------------------------------------
// Math functions
// ---------------------------------------------------------------------------

/// A math type: exponents of length, angle, time, frequency, resolution,
/// flex and percent.
type Ty = [i8; 7];
const NUMBER: Ty = [0; 7];
const LEN: usize = 0;
const ANG: usize = 1;
const TIME: usize = 2;
const FREQ: usize = 3;
const RES: usize = 4;
const PCT: usize = 6;

fn base(i: usize) -> Ty {
    let mut t = NUMBER;
    t[i] = 1;
    t
}

pub(crate) const LENGTH_UNITS: &[&str] = &[
    "px", "em", "rem", "ex", "rex", "ch", "rch", "ic", "ric", "cap", "rcap", "lh", "rlh", "vw",
    "vh", "vi", "vb", "vmin", "vmax", "svw", "svh", "svi", "svb", "svmin", "svmax", "lvw", "lvh",
    "lvi", "lvb", "lvmin", "lvmax", "dvw", "dvh", "dvi", "dvb", "dvmin", "dvmax", "cqw", "cqh",
    "cqi", "cqb", "cqmin", "cqmax", "cm", "mm", "q", "in", "pt", "pc",
];

fn unit_base(u: &str) -> Option<usize> {
    Some(match u {
        "deg" | "grad" | "rad" | "turn" => ANG,
        "s" | "ms" => TIME,
        "hz" | "khz" => FREQ,
        "dpi" | "dpcm" | "dppx" | "x" => RES,
        _ if LENGTH_UNITS.contains(&u) => LEN,
        _ => return None,
    })
}

pub(crate) const MATH_FNS: &[&str] = &[
    "calc",
    "-webkit-calc",
    "min",
    "max",
    "clamp",
    "round",
    "mod",
    "rem",
    "abs",
    "sign",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "atan2",
    "pow",
    "sqrt",
    "hypot",
    "log",
    "exp",
    "progress",
    "sibling-index",
    "sibling-count",
];

/// Splits `args` at top-level commas (`None` for an empty part).
fn split_commas(args: &[Comp]) -> Option<Vec<&[Comp]>> {
    let parts: Vec<&[Comp]> = args.split(|c| matches!(c.cv, Cv::Comma)).collect();
    parts.iter().all(|p| !p.is_empty()).then_some(parts)
}

struct MathCx<'a> {
    /// The base a percentage resolves to, when it mixes with other types.
    pct: Option<usize>,
    /// Relative color channel keywords usable as numbers.
    chan: &'a [&'a str],
    /// `size` is a length (the calculation of `calc-size()`).
    size: bool,
}

impl MathCx<'_> {
    fn func(&self, name: &str, args: &[Comp]) -> Option<Ty> {
        let parts = if args.is_empty() {
            Vec::new()
        } else {
            split_commas(args)?
        };
        let same = |ps: &[&[Comp]]| -> Option<Ty> {
            let mut t = self.sum(ps.first()?)?;
            for p in &ps[1..] {
                t = self.add(t, self.sum(p)?)?;
            }
            Some(t)
        };
        let number = |p: &[Comp]| self.sum(p) == Some(NUMBER);
        match (name, parts.len()) {
            ("calc" | "-webkit-calc", 1) => self.sum(parts[0]),
            ("min" | "max" | "hypot", n) if n >= 1 => same(&parts),
            ("clamp", 3) => {
                let none = |p: &[Comp]| matches!(p, [Comp { cv: Cv::Ident(i), .. }] if i == "none");
                let ps: Vec<&[Comp]> = parts.iter().copied().filter(|p| !none(p)).collect();
                if none(parts[1]) {
                    return None;
                }
                same(&ps)
            }
            ("round", 1..=3) => {
                let mut ps = &parts[..];
                if let [
                    Comp {
                        cv: Cv::Ident(i), ..
                    },
                ] = ps[0]
                    && matches!(i.as_str(), "nearest" | "up" | "down" | "to-zero")
                {
                    ps = &ps[1..];
                }
                match ps.len() {
                    1 => self.sum(ps[0]).filter(|t| *t == NUMBER),
                    2 => same(ps),
                    _ => None,
                }
            }
            ("mod" | "rem", 2) => same(&parts),
            ("abs", 1) => self.sum(parts[0]),
            ("sign", 1) => self.sum(parts[0]).map(|_| NUMBER),
            ("sin" | "cos" | "tan", 1) => {
                let t = self.sum(parts[0])?;
                (t == NUMBER || t == base(ANG)).then_some(NUMBER)
            }
            ("asin" | "acos" | "atan", 1) => number(parts[0]).then_some(base(ANG)),
            ("atan2", 2) => same(&parts).map(|_| base(ANG)),
            ("pow", 2) => (number(parts[0]) && number(parts[1])).then_some(NUMBER),
            ("sqrt" | "exp", 1) => number(parts[0]).then_some(NUMBER),
            ("log", 1 | 2) => parts.iter().all(|p| number(p)).then_some(NUMBER),
            ("progress", 3) => same(&parts).map(|_| NUMBER),
            ("sibling-index" | "sibling-count", 0) => Some(NUMBER),
            _ => None,
        }
    }

    /// `<calc-sum>`: the whole slice.
    fn sum(&self, c: &[Comp]) -> Option<Ty> {
        let mut t: Option<Ty> = None;
        let mut start = 0;
        let mut i = 0;
        loop {
            let op = c
                .get(i)
                .is_some_and(|x| matches!(x.cv, Cv::Delim('+' | '-')) && i > start);
            if i == c.len() || op {
                if op && !(c[i].ws && c.get(i + 1).is_some_and(|n| n.ws)) {
                    return None;
                }
                let pt = self.product(&c[start..i])?;
                t = Some(match t {
                    None => pt,
                    Some(prev) => self.add(prev, pt)?,
                });
                if i == c.len() {
                    return t;
                }
                start = i + 1;
            }
            i += 1;
        }
    }

    fn add(&self, a: Ty, b: Ty) -> Option<Ty> {
        if a == b {
            return Some(a);
        }
        let p = self.pct?;
        let fix = |t: Ty| if t == base(PCT) { base(p) } else { t };
        (fix(a) == fix(b) && fix(a) == base(p)).then_some(base(p))
    }

    fn product(&self, c: &[Comp]) -> Option<Ty> {
        let mut t = self.value(c.first()?)?;
        let mut i = 1;
        while i < c.len() {
            let div = match c[i].cv {
                Cv::Delim('*') => false,
                Cv::Delim('/') => true,
                _ => return None,
            };
            let v = self.value(c.get(i + 1)?)?;
            for k in 0..7 {
                t[k] = if div { t[k] - v[k] } else { t[k] + v[k] };
            }
            i += 2;
        }
        Some(t)
    }

    fn value(&self, c: &Comp) -> Option<Ty> {
        match &c.cv {
            Cv::Num(..) => Some(NUMBER),
            Cv::Pct(_) => Some(base(PCT)),
            Cv::Dim(_, u) => unit_base(u).map(base),
            Cv::Ident(i) if self.size && i == "size" => Some(base(LEN)),
            Cv::Ident(i) => (matches!(i.as_str(), "e" | "pi" | "infinity" | "-infinity" | "nan")
                || self.chan.contains(&i.as_str()))
            .then_some(NUMBER),
            Cv::Block('(', inner) => self.sum(inner),
            Cv::Func(name, args) if MATH_FNS.contains(&name.as_str()) => self.func(name, args),
            _ => None,
        }
    }
}

/// The value of a math function of numbers, when it has one.
fn eval_number(c: &Comp) -> Option<f32> {
    fn sum(c: &[Comp]) -> Option<f32> {
        let mut total = 0.0;
        let mut sign = 1.0;
        let mut start = 0;
        for i in 0..=c.len() {
            let op = c.get(i).and_then(|x| match x.cv {
                Cv::Delim('+') if i > start => Some(1.0),
                Cv::Delim('-') if i > start => Some(-1.0),
                _ => None,
            });
            if i == c.len() || op.is_some() {
                total += sign * product(&c[start..i])?;
                sign = op.unwrap_or(1.0);
                start = i + 1;
            }
        }
        Some(total)
    }
    fn product(c: &[Comp]) -> Option<f32> {
        let mut v = value(c.first()?)?;
        let mut i = 1;
        while i < c.len() {
            let r = value(c.get(i + 1)?)?;
            match c[i].cv {
                Cv::Delim('*') => v *= r,
                Cv::Delim('/') => v /= r,
                _ => return None,
            }
            i += 2;
        }
        Some(v)
    }
    fn value(c: &Comp) -> Option<f32> {
        match &c.cv {
            Cv::Num(v, _) => Some(*v),
            Cv::Ident(i) => match i.as_str() {
                "e" => Some(std::f32::consts::E),
                "pi" => Some(std::f32::consts::PI),
                "infinity" => Some(f32::INFINITY),
                "-infinity" => Some(f32::NEG_INFINITY),
                _ => None,
            },
            Cv::Block('(', inner) => sum(inner),
            Cv::Func(f, args) => {
                let parts = split_commas(args)?;
                let vals: Option<Vec<f32>> = parts.iter().map(|p| sum(p)).collect();
                let vals = vals?;
                match (f.as_str(), vals.as_slice()) {
                    ("calc" | "-webkit-calc", [x]) => Some(*x),
                    ("min", [_, ..]) => vals.iter().copied().reduce(f32::min),
                    ("max", [_, ..]) => vals.iter().copied().reduce(f32::max),
                    ("clamp", [lo, x, hi]) => Some(x.min(*hi).max(*lo)),
                    ("abs", [x]) => Some(x.abs()),
                    ("sign", [x]) => Some(if *x > 0.0 {
                        1.0
                    } else if *x < 0.0 {
                        -1.0
                    } else {
                        0.0
                    }),
                    _ => None,
                }
            }
            _ => None,
        }
    }
    value(c)
}

/// The type of a math function, `pct` the base percentages resolve to.
fn math_type(c: &Comp, pct: Option<usize>, chan: &[&str]) -> Option<Ty> {
    let Cv::Func(name, args) = &c.cv else {
        return None;
    };
    if !MATH_FNS.contains(&name.as_str()) {
        return None;
    }
    let t = MathCx {
        pct,
        chan,
        size: false,
    }
    .func(name, args)?;
    // A percentage that mixed with the base resolves to it.
    Some(match pct {
        Some(p) if t == base(PCT) => base(p),
        _ => t,
    })
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

/// `calc-size( <basis>, <calculation> )`: the basis a sizing keyword, a
/// length-percentage or another `calc-size()`; `size` a length in the
/// calculation.
fn calc_size_ok(c: &Comp, auto: bool) -> bool {
    let Cv::Func(f, args) = &c.cv else {
        return false;
    };
    if f != "calc-size" {
        return false;
    }
    let Some(comma) = args.iter().position(|x| matches!(x.cv, Cv::Comma)) else {
        return false;
    };
    let (basis, rest) = (&args[..comma], &args[comma + 1..]);
    let basis_ok = match basis {
        [
            Comp {
                cv: Cv::Ident(i), ..
            },
        ] => {
            matches!(
                i.as_str(),
                "min-content" | "max-content" | "fit-content" | "stretch"
            ) || (auto && i == "auto")
        }
        [b] => calc_size_ok(b, auto) || length_percentage(b),
        _ => false,
    };
    // Blink parses one calculation and ignores what follows it
    // (`calc-size(1px, size size)` is valid): the calculation ends at the
    // first component that does not continue it.
    let is_op = |i: usize| match rest.get(i).map(|x| &x.cv) {
        Some(Cv::Delim('*' | '/')) => true,
        Some(Cv::Delim('+' | '-')) => rest[i].ws,
        _ => false,
    };
    let mut end = 1;
    while end < rest.len() && is_op(end) {
        end += 2;
    }
    if rest.is_empty() || end > rest.len() {
        return false;
    }
    let cx = MathCx {
        pct: Some(LEN),
        chan: &[],
        size: true,
    };
    basis_ok
        && cx
            .sum(&rest[..end])
            .map(|t| if t == base(PCT) { base(LEN) } else { t })
            == Some(base(LEN))
}

fn length_percentage(c: &Comp) -> bool {
    match &c.cv {
        Cv::Dim(_, u) => unit_base(u) == Some(LEN),
        Cv::Num(v, _) => *v == 0.0,
        Cv::Pct(_) => true,
        _ => math_type(c, Some(LEN), &[]) == Some(base(LEN)),
    }
}

/// Channel keywords of a relative color function (`rgb(from red r g b)`).
fn channels(func: &str) -> &'static [&'static str] {
    match func {
        "rgb" | "rgba" => &["r", "g", "b", "alpha"],
        "hsl" | "hsla" => &["h", "s", "l", "alpha"],
        "hwb" => &["h", "w", "b", "alpha"],
        "lab" | "oklab" => &["l", "a", "b", "alpha"],
        "lch" | "oklch" => &["l", "c", "h", "alpha"],
        "color" => &["r", "g", "b", "x", "y", "z", "alpha"],
        "alpha" => &["alpha"],
        _ => &[],
    }
}

const WIDE: &[&str] = &["initial", "inherit", "unset", "revert", "revert-layer"];

const GENERIC_FAMILIES: &[&str] = &[
    "serif",
    "sans-serif",
    "cursive",
    "fantasy",
    "monospace",
    "system-ui",
    "math",
    "emoji",
    "fangsong",
    "ui-serif",
    "ui-sans-serif",
    "ui-monospace",
    "ui-rounded",
    "-webkit-body",
    "-webkit-pictograph",
];

/// Checks a grammar cannot say: the percentages of `color-mix()` and
/// `dynamic-range-limit-mix()` must not sum to zero.
fn func_ok(name: &str, args: &[Comp]) -> bool {
    if name != "color-mix" && name != "dynamic-range-limit-mix" {
        return true;
    }
    // A literal percentage's value; `None` for a math function (not zero).
    let pcts: Vec<Option<f32>> = args
        .iter()
        .filter_map(|c| match &c.cv {
            Cv::Pct(p) => Some(Some(*p)),
            Cv::Func(f, _) if MATH_FNS.contains(&f.as_str()) => Some(None),
            _ => None,
        })
        .collect();
    let all = if name == "color-mix" {
        2
    } else {
        pcts.len().max(1)
    };
    !(pcts.len() == all && pcts.iter().all(|p| *p == Some(0.0)))
}

/// SVG path data as Blink's path parser takes it (`path("M0 0 L1 1")`).
pub(crate) fn path_data_ok(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    let ws = |i: &mut usize| {
        while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r' | b'\x0c') {
            *i += 1;
        }
    };
    // A number, then optional white space and one comma.
    let number = |i: &mut usize, flag: bool| -> bool {
        ws(i);
        let start = *i;
        if flag {
            if *i < b.len() && matches!(b[*i], b'0' | b'1') {
                *i += 1;
            } else {
                return false;
            }
        } else {
            if *i < b.len() && matches!(b[*i], b'+' | b'-') {
                *i += 1;
            }
            let digits = |i: &mut usize| {
                let s = *i;
                while *i < b.len() && b[*i].is_ascii_digit() {
                    *i += 1;
                }
                *i > s
            };
            let int = digits(i);
            let frac = *i < b.len() && b[*i] == b'.' && {
                *i += 1;
                digits(i)
            };
            if !int && !frac {
                *i = start;
                return false;
            }
            if *i < b.len() && matches!(b[*i], b'e' | b'E') {
                let save = *i;
                *i += 1;
                if *i < b.len() && matches!(b[*i], b'+' | b'-') {
                    *i += 1;
                }
                if !digits(i) {
                    *i = save;
                }
            }
        }
        ws(i);
        if *i < b.len() && b[*i] == b',' {
            *i += 1;
        }
        true
    };
    ws(&mut i);
    if i == b.len() {
        return true;
    }
    let mut cmd = 0u8;
    let mut first = true;
    while i < b.len() {
        ws(&mut i);
        if i == b.len() {
            break;
        }
        let c = b[i];
        if c.is_ascii_alphabetic() {
            cmd = c;
            i += 1;
            if first && !matches!(cmd, b'M' | b'm') {
                return false;
            }
        } else if first || matches!(cmd, b'Z' | b'z') {
            return false;
        } else if cmd == b'M' {
            cmd = b'L';
        } else if cmd == b'm' {
            cmd = b'l';
        }
        first = false;
        let args: &[bool] = match cmd.to_ascii_uppercase() {
            b'Z' => &[],
            b'M' | b'L' | b'T' => &[false; 2],
            b'H' | b'V' => &[false],
            b'C' => &[false; 6],
            b'S' | b'Q' => &[false; 4],
            b'A' => &[false, false, false, true, true, false, false],
            _ => return false,
        };
        for &flag in args {
            if !number(&mut i, flag) {
                return false;
            }
        }
    }
    true
}

pub(crate) struct Matcher {
    g: &'static Grammar,
    memo: HashMap<(Id, usize, u32, usize), Vec<u32>>,
    steps: u32,
}

/// Whether `value` (component values) matches property `prop`'s grammar.
pub(crate) fn matches_property(prop: &str, value: &[Comp]) -> bool {
    let g = grammar();
    let Some(root) = g.lookup(&g.prop_of, prop) else {
        return false;
    };
    let mut m = Matcher {
        g,
        memo: HashMap::new(),
        steps: 0,
    };
    m.ends(root, value, 0, &[]).contains(&(value.len() as u32))
}

/// Whether `value` matches type `<name>` of the grammar file.
pub(crate) fn matches_type(name: &str, value: &[Comp]) -> bool {
    let g = grammar();
    let Some(root) = g.lookup(&g.type_of, name) else {
        return false;
    };
    let mut m = Matcher {
        g,
        memo: HashMap::new(),
        steps: 0,
    };
    m.ends(root, value, 0, &[]).contains(&(value.len() as u32))
}

fn push(out: &mut Vec<u32>, e: u32) {
    if !out.contains(&e) {
        out.push(e);
    }
}

impl Matcher {
    /// Every end position of `id` matched at `pos` of `inp`.
    fn ends(&mut self, id: Id, inp: &[Comp], pos: u32, chan: &'static [&'static str]) -> Vec<u32> {
        self.steps += 1;
        if self.steps > 2_000_000 {
            return Vec::new();
        }
        let key = (id, inp.as_ptr() as usize, pos, chan.as_ptr() as usize);
        if let Some(r) = self.memo.get(&key) {
            return r.clone();
        }
        // Guard against left recursion: no match while this one runs.
        self.memo.insert(key, Vec::new());
        let r = self.ends_uncached(id, inp, pos, chan);
        self.memo.insert(key, r.clone());
        r
    }

    fn ends_uncached(
        &mut self,
        id: Id,
        inp: &[Comp],
        pos: u32,
        chan: &'static [&'static str],
    ) -> Vec<u32> {
        let g = self.g;
        let at = inp.get(pos as usize);
        let mut out = Vec::new();
        match g.nodes[id as usize] {
            Node::Kw(k) => {
                if let Some(Comp {
                    cv: Cv::Ident(i), ..
                }) = at
                    && i.eq_ignore_ascii_case(g.names[k as usize])
                {
                    out.push(pos + 1);
                }
            }
            Node::Lit(c, _) => {
                let ok = match (c, at.map(|a| &a.cv)) {
                    (b',', Some(Cv::Comma)) => true,
                    (b':', Some(Cv::Colon)) => true,
                    (b';', Some(Cv::Semicolon)) => true,
                    (c, Some(Cv::Delim(d))) => c as char == *d,
                    _ => false,
                };
                if ok {
                    out.push(pos + 1);
                }
            }
            Node::Type(name, range, except) => {
                let t = g.type_of[name as usize];
                if t != NONE {
                    return self.ends(t, inp, pos, chan);
                }
                let except = if except == NO_NAME {
                    ""
                } else {
                    g.names[except as usize]
                };
                if let Some(Comp {
                    cv: Cv::Ident(i), ..
                }) = at
                    && except.split('~').any(|e| e.trim().eq_ignore_ascii_case(i))
                {
                    return out;
                }
                let (lo, hi) = g.ranges[range as usize];
                self.builtin(
                    g.names[name as usize],
                    lo,
                    hi,
                    except,
                    inp,
                    pos,
                    chan,
                    &mut out,
                );
            }
            Node::Prop(name) => {
                let t = g.prop_of[name as usize];
                if t != NONE {
                    return self.ends(t, inp, pos, chan);
                }
            }
            Node::Func(name, args) => {
                if let Some(Comp {
                    cv: Cv::Func(f, inner),
                    ..
                }) = at
                    && f.eq_ignore_ascii_case(g.names[name as usize])
                {
                    // A color function's channel keywords hold inside it
                    // when it is relative; other functions keep the outer ones.
                    let own = channels(f);
                    let chan = match inner.first() {
                        _ if own.is_empty() => chan,
                        Some(Comp {
                            cv: Cv::Ident(i), ..
                        }) if i == "from" => own,
                        _ => &[],
                    };
                    if self.whole(args, inner, chan) && func_ok(f, inner) {
                        out.push(pos + 1);
                    }
                }
            }
            Node::Block(c, args) => {
                if let Some(Comp {
                    cv: Cv::Block(b, inner),
                    ..
                }) = at
                    && *b == c as char
                    && self.whole(args, inner, chan)
                {
                    out.push(pos + 1);
                }
            }
            Node::Seq(kids) => return self.seq(g.kids(kids), inp, pos, chan),
            Node::One(kids) => {
                for &k in g.kids(kids) {
                    for e in self.ends(k, inp, pos, chan) {
                        push(&mut out, e);
                    }
                }
            }
            Node::Any(kids) | Node::All(kids) => {
                let all = matches!(g.nodes[id as usize], Node::All(_));
                let mut seen = Vec::new();
                self.combos(g.kids(kids), all, inp, pos, 0, chan, &mut seen, &mut out);
            }
            Node::Greedy(kids) => {
                let kids = g.kids(kids);
                let (mut p, mut used) = (pos, 0u32);
                'outer: loop {
                    for (i, &k) in kids.iter().enumerate() {
                        if used & (1 << i) != 0 {
                            continue;
                        }
                        if let Some(&e) = self.ends(k, inp, p, chan).iter().max()
                            && e > p
                        {
                            used |= 1 << i;
                            p = e;
                            continue 'outer;
                        }
                    }
                    break;
                }
                if used != 0 {
                    out.push(p);
                }
            }
            Node::Mult(n, lo, hi, comma) => {
                let mut cur = vec![pos];
                if lo == 0 {
                    out.push(pos);
                }
                let mut count = 0u16;
                while !cur.is_empty() && count < hi && count as usize <= inp.len() + 1 {
                    let mut next = Vec::new();
                    for &p in &cur {
                        let start = if comma && count > 0 {
                            match inp.get(p as usize) {
                                Some(Comp { cv: Cv::Comma, .. }) => p + 1,
                                _ => continue,
                            }
                        } else {
                            p
                        };
                        for e in self.ends(n, inp, start, chan) {
                            // An empty repetition adds nothing once `lo` is met.
                            if e == p && count >= lo {
                                continue;
                            }
                            push(&mut next, e);
                        }
                    }
                    count += 1;
                    if count >= lo {
                        for &e in &next {
                            push(&mut out, e);
                        }
                    }
                    cur = next;
                }
            }
            Node::Req(n) => {
                for e in self.ends(n, inp, pos, chan) {
                    if e != pos {
                        push(&mut out, e);
                    }
                }
            }
        }
        out
    }

    fn whole(&mut self, args: Id, inner: &[Comp], chan: &'static [&'static str]) -> bool {
        match args {
            NONE => inner.is_empty(),
            a => self.ends(a, inner, 0, chan).contains(&(inner.len() as u32)),
        }
    }

    /// A sequence, with the comma rule: a comma is consumed between two
    /// non-empty parts and omitted next to an empty one.
    fn seq(
        &mut self,
        kids: &[Id],
        inp: &[Comp],
        pos: u32,
        chan: &'static [&'static str],
    ) -> Vec<u32> {
        // State: position, start of the current comma segment, and what
        // the comma before it requires of the segment.
        #[derive(Clone, Copy, PartialEq)]
        enum Need {
            Nothing,
            NonEmpty,
            Empty,
        }
        let ok_seg = |need: Need, empty: bool| match need {
            Need::Nothing => true,
            Need::NonEmpty => !empty,
            Need::Empty => empty,
        };
        let mut states = vec![(pos, pos, Need::Nothing)];
        let is_comma = |k: &Id| matches!(self.g.nodes[*k as usize], Node::Lit(b',', false));
        // A comma first or last in a group separates it from what is
        // outside, so the part on that side counts as present.
        let last_comma = kids.last().is_some_and(is_comma);
        for (ki, &k) in kids.iter().enumerate() {
            let mut next = Vec::new();
            if is_comma(&k) {
                for &(p, seg, need) in &states {
                    let empty = p == seg && ki > 0;
                    if !ok_seg(need, empty) {
                        continue;
                    }
                    if !empty && matches!(inp.get(p as usize), Some(Comp { cv: Cv::Comma, .. })) {
                        next.push((p + 1, p + 1, Need::NonEmpty));
                    }
                    next.push((p, p, if empty { Need::Nothing } else { Need::Empty }));
                }
            } else {
                for &(p, seg, need) in &states {
                    for e in self.ends(k, inp, p, chan) {
                        let s = (e, seg, need);
                        if !next.contains(&s) {
                            next.push(s);
                        }
                    }
                }
            }
            states = next;
            if states.is_empty() {
                break;
            }
        }
        let mut out = Vec::new();
        for (p, seg, need) in states {
            if (last_comma && need == Need::NonEmpty) || ok_seg(need, p == seg) {
                push(&mut out, p);
            }
        }
        out
    }

    /// `||` (`all` false: one or more, any order) and `&&` (all, any order).
    #[allow(clippy::too_many_arguments)]
    fn combos(
        &mut self,
        kids: &[Id],
        all: bool,
        inp: &[Comp],
        pos: u32,
        used: u32,
        chan: &'static [&'static str],
        seen: &mut Vec<(u32, u32)>,
        out: &mut Vec<u32>,
    ) {
        if seen.contains(&(used, pos)) {
            return;
        }
        seen.push((used, pos));
        let full = (1u32 << kids.len()) - 1;
        if used == full {
            push(out, pos);
            return;
        }
        if used != 0 && !all {
            push(out, pos);
        }
        for (i, &k) in kids.iter().enumerate() {
            if used & (1 << i) != 0 {
                continue;
            }
            for e in self.ends(k, inp, pos, chan) {
                // An empty match only counts at the end of an `&&`.
                if e == pos && !all {
                    continue;
                }
                self.combos(kids, all, inp, e, used | (1 << i), chan, seen, out);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn builtin(
        &mut self,
        name: &str,
        lo: f32,
        hi: f32,
        except: &str,
        inp: &[Comp],
        pos: u32,
        chan: &'static [&'static str],
        out: &mut Vec<u32>,
    ) {
        let Some(c) = inp.get(pos as usize) else {
            return;
        };
        let inr = |v: f32| v >= lo && v <= hi;
        let math = |pct: Option<usize>, want: Ty| math_type(c, pct, chan) == Some(want);
        let ok = match name {
            "length" => match &c.cv {
                Cv::Dim(v, u) => unit_base(u) == Some(LEN) && inr(*v),
                Cv::Num(v, _) => *v == 0.0,
                _ => math(None, base(LEN)),
            },
            "length-percentage" => match &c.cv {
                Cv::Dim(v, u) => unit_base(u) == Some(LEN) && inr(*v),
                Cv::Num(v, _) => *v == 0.0,
                Cv::Pct(v) => inr(*v),
                _ => math(Some(LEN), base(LEN)),
            },
            "percentage" => match &c.cv {
                Cv::Pct(v) => inr(*v),
                _ => math(None, base(PCT)),
            },
            "number" => match &c.cv {
                Cv::Num(v, _) => inr(*v),
                Cv::Ident(i) if chan.contains(&i.as_str()) => true,
                _ => math(None, NUMBER),
            },
            "integer" => match &c.cv {
                Cv::Num(v, int) => *int && inr(*v),
                _ => math(None, NUMBER),
            },
            "angle" | "angle-percentage" => match &c.cv {
                Cv::Dim(v, u) => unit_base(u) == Some(ANG) && inr(*v),
                Cv::Pct(v) => name == "angle-percentage" && inr(*v),
                _ => math((name == "angle-percentage").then_some(ANG), base(ANG)),
            },
            "time" | "time-percentage" => match &c.cv {
                Cv::Dim(v, u) => {
                    unit_base(u) == Some(TIME) && inr(if u == "ms" { *v / 1000.0 } else { *v })
                }
                Cv::Pct(v) => name == "time-percentage" && inr(*v),
                _ => math((name == "time-percentage").then_some(TIME), base(TIME)),
            },
            "frequency" => match &c.cv {
                Cv::Dim(_, u) => unit_base(u) == Some(FREQ),
                _ => math(None, base(FREQ)),
            },
            "resolution" => match &c.cv {
                Cv::Dim(v, u) => unit_base(u) == Some(RES) && inr(*v),
                _ => math(None, base(RES)),
            },
            "flex" => matches!(&c.cv, Cv::Dim(v, u) if u == "fr" && inr(*v)),
            // A length without math functions (`overflow-clip-margin`).
            "length-lit" | "length-unit" => match &c.cv {
                Cv::Dim(v, u) => unit_base(u) == Some(LEN) && inr(*v),
                // `length-unit`: no unitless zero either.
                Cv::Num(v, _) => *v == 0.0 && name == "length-lit",
                _ => false,
            },
            "zero" => matches!(c.cv, Cv::Num(v, _) if v == 0.0),
            // A number whose range also bounds math functions, evaluated
            // (`cubic-bezier()`'s x values).
            "number-eval" => match &c.cv {
                Cv::Num(v, _) => inr(*v),
                _ => math(None, NUMBER) && eval_number(c).is_none_or(inr),
            },
            "dimension" => matches!(c.cv, Cv::Dim(..)),
            // A range bounds the length (printable ASCII only): `<string [4,4]>`.
            "string" => match &c.cv {
                Cv::Str(s) if lo.is_finite() || hi.is_finite() => {
                    let n = s.chars().count() as f32;
                    inr(n) && s.bytes().all(|b| (0x20..0x7f).contains(&b))
                }
                Cv::Str(_) => true,
                _ => false,
            },
            // `[1,∞]`: not empty.
            "path-data" => {
                matches!(&c.cv, Cv::Str(s) if path_data_ok(s) && (lo < 1.0 || !s.trim().is_empty()))
            }
            // `~auto`: no `auto` basis (`max-width`).
            "calc-size()" => calc_size_ok(c, !except.contains("auto")),
            "family-name" => {
                // A string, or identifiers: one that is not a CSS-wide
                // keyword or `default`, or several not starting with a
                // generic family.
                if matches!(c.cv, Cv::Str(_)) {
                    push(out, pos + 1);
                    return;
                }
                let idents: Vec<&str> = inp[pos as usize..]
                    .iter()
                    .map_while(|x| match &x.cv {
                        Cv::Ident(i) => Some(i.as_str()),
                        _ => None,
                    })
                    .collect();
                for n in 1..=idents.len() {
                    let ok = if n == 1 {
                        !WIDE.contains(&idents[0]) && idents[0] != "default"
                    } else {
                        !GENERIC_FAMILIES.contains(&idents[0])
                    };
                    if ok {
                        push(out, pos + n as u32);
                    }
                }
                return;
            }
            "url" => match &c.cv {
                Cv::Url => true,
                Cv::Func(f, a) => {
                    f == "url" && matches!(a.as_slice(), [Comp { cv: Cv::Str(_), .. }])
                }
                _ => false,
            },
            "ident" => matches!(c.cv, Cv::Ident(_)),
            "custom-ident" => {
                matches!(&c.cv, Cv::Ident(i) if !WIDE.contains(&i.as_str()) && i != "default")
            }
            "dashed-ident" | "custom-property-name" => {
                matches!(&c.cv, Cv::Ident(i) if i.starts_with("--") && i.len() > 2)
            }
            "hex-color" => matches!(&c.cv, Cv::Hash(h)
                if matches!(h.len(), 3 | 4 | 6 | 8) && h.bytes().all(|b| b.is_ascii_hexdigit())),
            "named-color" => {
                matches!(&c.cv, Cv::Ident(i) if cssparser::color::parse_named_color(i).is_ok())
            }
            "declaration-value" | "any-value" => {
                for (i, x) in inp[pos as usize..].iter().enumerate() {
                    if matches!(x.cv, Cv::Semicolon | Cv::Delim('!')) {
                        break;
                    }
                    push(out, pos + i as u32 + 1);
                }
                return;
            }
            _ => false,
        };
        if ok {
            push(out, pos + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grammar parses on first use and stays small in memory.
    #[test]
    fn grammar_is_compact() {
        assert_eq!(std::mem::size_of::<Node>(), 12);
        let g = grammar();
        assert!(g.props.len() > 700, "{}", g.props.len());
        let bytes = g.nodes.len() * 12 + g.kids.len() * 4 + g.names.len() * 24;
        assert!(bytes < 256 * 1024, "{bytes} bytes");
        assert!(g.has_property("-epub-word-break") && !g.has_property("animation-delay-start"));
    }

    /// Chrome 153 takes an empty value (white space only) for
    /// `timeline-trigger` alone, recorded for every property.
    #[test]
    fn only_timeline_trigger_takes_an_empty_value() {
        let g = grammar();
        let empty: Vec<&str> = g
            .props
            .iter()
            .copied()
            .filter(|p| matches_property(p, &[]))
            .collect();
        assert_eq!(empty, ["timeline-trigger"]);
    }
}
