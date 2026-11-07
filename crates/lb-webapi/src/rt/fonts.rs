//! CSS Font Loading API: `document.fonts` (`FontFaceSet`), `FontFace`,
//! and `window.queryLocalFonts`.
//!
//! Font detection by measurement needs no code here: layout and canvas
//! `measureText` both resolve families through `lb-layout`'s tables and
//! the persona's `fonts.available` list (installed fonts measure with
//! their own table, others fall through to the next family).
//!
//! - `document.fonts`: status `loaded`, `size` counts added faces,
//!   `ready` is one resolved promise, `check()` answers like Chrome (true
//!   unless a matching face in the set is still unloaded; a syntax error
//!   throws), `load()` resolves with the matching loaded faces.
//! - `FontFace`: descriptors are stored as given; `family` reads back
//!   serialized as Chrome 153 does when the constructor's family needed it
//!   (quoted unless one identifier). `src`
//!   parses as Blink's `ConsumeFontFaceSrc` (invalid components dropped;
//!   none valid is a `SyntaxError` and status `error`). `local("Name")`
//!   loads when `Name` is a full or PostScript face name of the persona's
//!   OS release (`lb_layout::LocalFonts`), binary sources when they start
//!   with a font signature; `url()` sources are not fetched and end in
//!   `error` (`NetworkError`).
//! - `queryLocalFonts()` rejects with `SecurityError` (no user activation
//!   is tracked; Chrome rejects the same way without one).

use lb_js::{Arg, CallCtx, Handle, HostError, HostRef, Resolver, ValueKind};

use super::{
    K_OBJ, Obj, Runtime, Window, ctor, getter, illegal, method, need_args, setter, this_obj,
};
use crate::registry::Registry;

/// Font host objects (one `Obj` variant).
pub(crate) enum FontObj {
    Set {
        faces: Vec<u32>,
        ready: Option<Handle>,
    },
    Face(Box<Face>),
}

impl FontObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            FontObj::Set { .. } => "FontFaceSet",
            FontObj::Face(_) => "FontFace",
        }
    }
}

const DESCRIPTORS: [(&str, &str); 12] = [
    ("style", "normal"),
    ("weight", "normal"),
    ("stretch", "normal"),
    ("unicodeRange", "U+0-10FFFF"),
    ("variant", "normal"),
    ("featureSettings", "normal"),
    ("display", "auto"),
    ("ascentOverride", "normal"),
    ("descentOverride", "normal"),
    ("lineGapOverride", "normal"),
    ("sizeAdjust", "100%"),
    ("variationSettings", "normal"),
];

/// Where a face loads from.
#[derive(Clone, Debug)]
enum Source {
    /// The `local()` names of a `src` string (its `url()`s are not fetched).
    List { locals: Vec<String> },
    /// An `ArrayBuffer` / view; `true` when it starts like a font.
    Binary(bool),
    /// A `src` string with no valid component (kept for the message).
    Invalid(String),
}

pub(crate) struct Face {
    family: String,
    /// The constructor's family needed quoting: `family` reads back
    /// serialized (Blink's `font_family_needs_quoting_`, kept across sets).
    quote: bool,
    desc: Vec<String>,
    status: &'static str,
    source: Source,
    loaded: Option<(Handle, Option<Resolver>)>,
}

fn with_font<R>(w: &Window, id: u32, f: impl FnOnce(&mut FontObj) -> Option<R>) -> Option<R> {
    w.with_obj(id, |o| match o {
        Obj::Font(x) => f(x),
        _ => None,
    })
    .flatten()
}

/// Parses a `src` string as Blink's `ConsumeFontFaceSrc` (Chrome 153):
/// comma-separated `local(<family-name>)` and `url(...) [format(...)]
/// [tech(...)]` components; invalid ones are dropped, none valid is an
/// error.
fn parse_src(s: &str) -> Source {
    let mut p = cssparser::Parser::new(s);
    let items = p.parse_comma_separated_ignoring_errors(|p| src_component(p));
    if items.is_empty() {
        return Source::Invalid(s.to_owned());
    }
    Source::List {
        locals: items.into_iter().flatten().collect(),
    }
}

type SrcResult<T> = Result<T, cssparser::ParseError<()>>;

fn bad<T>() -> SrcResult<T> {
    Err(cssparser::ParseError::unexpected_token())
}

/// One `src` component: `Some(name)` for `local()`, `None` for `url()`.
fn src_component(p: &mut cssparser::Parser<'_>) -> SrcResult<Option<String>> {
    use cssparser::Token;
    let local = match p.next()?.clone() {
        Token::Function(f) if f.eq_ignore_ascii_case("local") => {
            Some(p.parse_nested_block(|p| local_name(p))?)
        }
        Token::Function(f) if f.eq_ignore_ascii_case("url") => {
            p.parse_nested_block(|p| {
                p.expect_string()?;
                Ok(())
            })?;
            None
        }
        Token::UnquotedUrl(_) => None,
        _ => return bad(),
    };
    if local.is_none() {
        if p.try_parse(|p| p.expect_function_matching("format"))
            .is_ok()
        {
            p.parse_nested_block(|p| {
                let ok = match p.next()?.clone() {
                    Token::QuotedString(f) => matches!(
                        f.to_ascii_lowercase().as_str(),
                        "woff"
                            | "woff2"
                            | "truetype"
                            | "opentype"
                            | "collection"
                            | "woff-variations"
                            | "woff2-variations"
                            | "truetype-variations"
                            | "opentype-variations"
                    ),
                    Token::Ident(f) => matches!(
                        f.to_ascii_lowercase().as_str(),
                        "woff" | "woff2" | "truetype" | "opentype" | "collection"
                    ),
                    _ => return bad(),
                };
                if !ok {
                    return bad();
                }
                p.expect_exhausted()?;
                Ok(())
            })?;
        }
        if p.try_parse(|p| p.expect_function_matching("tech")).is_ok() {
            p.parse_nested_block(|p| {
                p.parse_comma_separated(|p| {
                    let t = p.expect_ident()?.to_ascii_lowercase();
                    match t.as_str() {
                        "variations" | "color-colrv1" | "color-colrv0" | "color-sbix"
                        | "color-cbdt" | "features-opentype" | "features-aat" | "palettes" => {
                            Ok(())
                        }
                        _ => bad(),
                    }
                })?;
                Ok(())
            })?;
        }
    }
    p.expect_exhausted()?;
    Ok(local)
}

/// The argument of `local()`: one string, or identifiers joined by single
/// spaces (one CSS-wide keyword or `default` alone is invalid).
fn local_name(p: &mut cssparser::Parser<'_>) -> SrcResult<String> {
    use cssparser::Token;
    let name = match p.next()?.clone() {
        Token::QuotedString(s) => s.to_string(),
        Token::Ident(first) => {
            let mut name = first.to_string();
            let mut words = 1;
            while let Ok(w) = p.try_parse(|p| p.expect_ident_cloned()) {
                name.push(' ');
                name.push_str(&w);
                words += 1;
            }
            if words == 1 && (is_css_wide_keyword(&name) || name.eq_ignore_ascii_case("default")) {
                return bad();
            }
            name
        }
        _ => return bad(),
    };
    p.expect_exhausted()?;
    Ok(name)
}

fn is_css_wide_keyword(s: &str) -> bool {
    // `revert-rule` counts in Chrome 153 (CSSRevertRule is on).
    [
        "initial",
        "inherit",
        "unset",
        "revert",
        "revert-layer",
        "revert-rule",
    ]
    .iter()
    .any(|k| s.eq_ignore_ascii_case(k))
}

/// `FontFace.family` as Chrome 153 serializes it (Blink
/// `SerializeFontFamily`): as is when it is one CSS identifier and not a
/// CSS-wide keyword, `default` or a generic family name (case-sensitive in
/// 153), else a CSS string: `"` and backslash escaped, control characters
/// as a hex escape and a space.
fn serialize_family(f: &str) -> String {
    let generic = matches!(
        f,
        "cursive" | "fantasy" | "monospace" | "sans-serif" | "serif" | "system-ui" | "math"
    );
    if is_identifier(f) && !is_css_wide_keyword(f) && !f.eq_ignore_ascii_case("default") && !generic
    {
        return f.to_owned();
    }
    let mut out = String::with_capacity(f.len() + 2);
    out.push('"');
    for c in f.chars() {
        match c {
            '\0'..='\u{1f}' | '\u{7f}' => out.push_str(&format!("\\{:x} ", c as u32)),
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One CSS tokenizer `ident` without escapes: `-?` then a name-start code
/// point, then name code points.
fn is_identifier(s: &str) -> bool {
    let start = |c: char| c.is_ascii_alphabetic() || c == '_' || !c.is_ascii();
    let mut it = s.chars().peekable();
    if it.peek() == Some(&'-') {
        it.next();
    }
    match it.next() {
        Some(c) if start(c) => {}
        _ => return false,
    }
    it.all(|c| start(c) || c.is_ascii_digit() || c == '-')
}

fn is_font_data(b: &[u8]) -> bool {
    matches!(
        b.get(..4),
        Some([0, 1, 0, 0])
            | Some(b"OTTO")
            | Some(b"true")
            | Some(b"wOFF")
            | Some(b"wOF2")
            | Some(b"ttcf")
    )
}

/// `local(name)` resolves when `name` is a face name (full or PostScript,
/// any case) of the persona's OS release (`lb_layout::LocalFonts`, from
/// Chrome recordings and the OS font lists). A persona family the release
/// does not ship (an extra installed font) resolves by its family name;
/// without a list for the OS only the persona's families resolve.
fn installed(w: &Window, name: &str) -> bool {
    let Some(p) = w.profile.as_ref() else {
        return false;
    };
    let extra = p
        .fonts
        .available
        .iter()
        .any(|f| f.eq_ignore_ascii_case(name));
    match lb_layout::LocalFonts::for_persona(p) {
        Some(l) => l.face_family(name).is_some() || (extra && !l.has_family(name)),
        None => extra,
    }
}

/// Families of a CSS `font` shorthand, `None` when it does not parse.
fn font_families(s: &str) -> Option<Vec<String>> {
    lb_paint::CanvasFont::parse(s).map(|f| f.families.iter().map(|n| n.name.clone()).collect())
}

fn set_id(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u32, HostError> {
    let id = this_obj(cx)?;
    with_font(w, id, |f| matches!(f, FontObj::Set { .. }).then_some(id)).ok_or_else(illegal)
}

fn faces_of(w: &Window, set: u32) -> Vec<u32> {
    with_font(w, set, |f| match f {
        FontObj::Set { faces, .. } => Some(faces.clone()),
        _ => None,
    })
    .unwrap_or_default()
}

fn face_info(w: &Window, id: u32) -> Option<(String, &'static str)> {
    with_font(w, id, |f| match f {
        FontObj::Face(face) => Some((face.family.clone(), face.status)),
        _ => None,
    })
}

fn arg_face(cx: &mut CallCtx<'_, '_, '_>, w: &Window, i: usize) -> Option<u32> {
    let h = cx.arg_host(i).filter(|h| h.kind == K_OBJ)?;
    with_font(w, h.id, |f| matches!(f, FontObj::Face(_)).then_some(h.id))
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, "Document", "fonts", |cx, w| {
        let (slot, _) = super::this_node(cx, w)?;
        let id = w.cached_obj((slot, 0, "FontFaceSet", String::new()), || {
            Obj::Font(Box::new(FontObj::Set {
                faces: Vec::new(),
                ready: None,
            }))
        });
        cx.ret(w.obj_arg(id))
    });
    register_set(reg, rt);
    register_face(reg, rt);
    method(reg, rt, "Window", "queryLocalFonts", |cx, w| {
        let r = cx.ret_new_promise()?;
        cx.reject_promise(r, w.rt.err("SecurityError", "User activation is required."))
    });
}

fn register_set(reg: &mut Registry, rt: &Runtime) {
    const S: &str = "FontFaceSet";
    getter(reg, rt, S, "status", |cx, w| {
        let id = set_id(cx, w)?;
        let loading = faces_of(w, id)
            .into_iter()
            .any(|f| face_info(w, f).is_some_and(|(_, s)| s == "loading"));
        cx.ret(if loading { "loading" } else { "loaded" })
    });
    getter(reg, rt, S, "size", |cx, w| {
        let id = set_id(cx, w)?;
        cx.ret(faces_of(w, id).len() as u32)
    });
    getter(reg, rt, S, "ready", |cx, w| {
        let id = set_id(cx, w)?;
        let have = with_font(w, id, |f| match f {
            FontObj::Set { ready, .. } => Some(*ready),
            _ => None,
        })
        .flatten();
        let h = match have {
            Some(h) => h,
            None => {
                let (h, r) = cx.new_promise()?;
                cx.resolve_promise(r, w.obj_arg(id))?;
                with_font(w, id, |f| {
                    if let FontObj::Set { ready, .. } = f {
                        *ready = Some(h);
                    }
                    Some(())
                });
                h
            }
        };
        cx.ret(Arg::Handle(h))
    });
    method(reg, rt, S, "add", |cx, w| {
        need_args(cx, 1, "add", S)?;
        let id = set_id(cx, w)?;
        let Some(face) = arg_face(cx, w, 0) else {
            return Err(HostError::type_error(
                "Failed to execute 'add' on 'FontFaceSet': parameter 1 is not of type 'FontFace'.",
            ));
        };
        with_font(w, id, |f| {
            if let FontObj::Set { faces, .. } = f
                && !faces.contains(&face)
            {
                faces.push(face);
            }
            Some(())
        });
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, S, "delete", |cx, w| {
        need_args(cx, 1, "delete", S)?;
        let id = set_id(cx, w)?;
        let face = arg_face(cx, w, 0);
        let removed = with_font(w, id, |f| match f {
            FontObj::Set { faces, .. } => {
                let n = faces.len();
                faces.retain(|x| Some(*x) != face);
                Some(n != faces.len())
            }
            _ => None,
        })
        .unwrap_or(false);
        cx.ret(removed)
    });
    method(reg, rt, S, "has", |cx, w| {
        need_args(cx, 1, "has", S)?;
        let id = set_id(cx, w)?;
        let face = arg_face(cx, w, 0);
        let has = face.is_some_and(|f| faces_of(w, id).contains(&f));
        cx.ret(has)
    });
    method(reg, rt, S, "clear", |cx, w| {
        let id = set_id(cx, w)?;
        with_font(w, id, |f| {
            if let FontObj::Set { faces, .. } = f {
                faces.clear();
            }
            Some(())
        });
        Ok(())
    });
    method(reg, rt, S, "forEach", |cx, w| {
        need_args(cx, 1, "forEach", S)?;
        let id = set_id(cx, w)?;
        if cx.arg_kind(0) != ValueKind::Function {
            return Err(HostError::type_error(
                "Failed to execute 'forEach' on 'FontFaceSet': The callback provided as parameter 1 is not a function.",
            ));
        }
        let f = cx.arg_handle(0);
        let this = if cx.len() > 1 {
            Arg::Handle(cx.arg_handle(1))
        } else {
            Arg::Undefined
        };
        for face in faces_of(w, id) {
            let v = w.obj_arg(face);
            cx.call(f, this.clone(), &[v.clone(), v, w.obj_arg(id)])?;
        }
        Ok(())
    });
    for op in ["values", "keys", "entries"] {
        method(reg, rt, S, op, move |cx, w| {
            let id = set_id(cx, w)?;
            let items: Vec<Arg> = faces_of(w, id)
                .into_iter()
                .map(|f| {
                    if op == "entries" {
                        Arg::Array(vec![w.obj_arg(f), w.obj_arg(f)])
                    } else {
                        w.obj_arg(f)
                    }
                })
                .collect();
            super::misc::ret_iterator(cx, w, items)
        });
    }
    method(reg, rt, S, "check", |cx, w| {
        need_args(cx, 1, "check", S)?;
        let id = set_id(cx, w)?;
        let font = cx.arg_string(0)?;
        let Some(families) = font_families(&font) else {
            return Err(w.rt.err(
                "SyntaxError",
                format!("Failed to execute 'check' on 'FontFaceSet': Could not resolve '{font}' as a font."),
            ));
        };
        let pending = faces_of(w, id).into_iter().any(|f| {
            face_info(w, f).is_some_and(|(fam, st)| {
                st != "loaded" && families.iter().any(|x| x.eq_ignore_ascii_case(&fam))
            })
        });
        cx.ret(!pending)
    });
    method(reg, rt, S, "load", |cx, w| {
        need_args(cx, 1, "load", S)?;
        let id = set_id(cx, w)?;
        let font = cx.arg_string(0)?;
        let r = cx.ret_new_promise()?;
        let Some(families) = font_families(&font) else {
            return cx.reject_promise(
                r,
                w.rt.err(
                    "SyntaxError",
                    format!("Failed to execute 'load' on 'FontFaceSet': Could not resolve '{font}' as a font."),
                ),
            );
        };
        let mut out = Vec::new();
        for f in faces_of(w, id) {
            if let Some((fam, _)) = face_info(w, f)
                && families.iter().any(|x| x.eq_ignore_ascii_case(&fam))
            {
                load_face(w, f);
                out.push(w.obj_arg(f));
            }
        }
        cx.resolve_promise(r, Arg::Array(out))
    });
}

/// Runs a face's load; returns its new status.
fn load_face(w: &Window, id: u32) -> &'static str {
    let ok = with_font(w, id, |f| match f {
        FontObj::Face(face) => Some(match &face.source {
            Source::Binary(ok) => *ok,
            Source::List { locals, .. } => locals.iter().any(|n| installed(w, n)),
            Source::Invalid(_) => false,
        }),
        _ => None,
    })
    .unwrap_or(false);
    let status = if ok { "loaded" } else { "error" };
    with_font(w, id, |f| {
        if let FontObj::Face(face) = f {
            face.status = status;
        }
        Some(())
    });
    status
}

fn this_face(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u32, HostError> {
    let id = this_obj(cx)?;
    with_font(w, id, |f| matches!(f, FontObj::Face(_)).then_some(id)).ok_or_else(illegal)
}

/// The face's `loaded` promise (created on first use) and, while
/// pending, its resolver.
fn loaded_promise(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    id: u32,
) -> Result<(Handle, Option<Resolver>), HostError> {
    let have = with_font(w, id, |f| match f {
        FontObj::Face(face) => Some(face.loaded),
        _ => None,
    })
    .flatten();
    if let Some(p) = have {
        return Ok(p);
    }
    let (h, r) = cx.new_promise()?;
    with_font(w, id, |f| {
        if let FontObj::Face(face) = f {
            face.loaded = Some((h, Some(r)));
        }
        Some(())
    });
    Ok((h, Some(r)))
}

/// Settles the `loaded` promise for the current status.
fn settle(cx: &mut CallCtx<'_, '_, '_>, w: &Window, id: u32) -> Result<Handle, HostError> {
    let (h, r) = loaded_promise(cx, w, id)?;
    let status = face_info(w, id).map_or("error", |(_, s)| s);
    if let Some(r) = r
        && status != "unloaded"
        && status != "loading"
    {
        if status == "loaded" {
            cx.resolve_promise(r, w.obj_arg(id))?;
        } else {
            let src = with_font(w, id, |f| match f {
                FontObj::Face(face) => Some(face.source.clone()),
                _ => None,
            });
            let e = match src {
                Some(Source::Invalid(s)) => w.rt.err(
                    "SyntaxError",
                    format!("The source provided ('{s}') could not be parsed as a value list."),
                ),
                Some(Source::Binary(false)) => {
                    w.rt.err("SyntaxError", "Invalid font data in ArrayBuffer.")
                }
                _ => w.rt.err("NetworkError", "A network error occurred."),
            };
            cx.reject_promise(r, e)?;
        }
        with_font(w, id, |f| {
            if let FontObj::Face(face) = f {
                face.loaded = Some((h, None));
            }
            Some(())
        });
    }
    Ok(h)
}

/// A string source's initial status: `error` when it does not parse.
fn src_status(s: Source) -> (Source, &'static str) {
    let status = if matches!(s, Source::Invalid(_)) {
        "error"
    } else {
        "unloaded"
    };
    (s, status)
}

fn register_face(reg: &mut Registry, rt: &Runtime) {
    const F: &str = "FontFace";
    ctor(reg, rt, F, |cx, w| {
        need_args(cx, 2, F, F).map_err(|_| {
            HostError::type_error(format!(
                "Failed to construct 'FontFace': 2 arguments required, but only {} present.",
                cx.len()
            ))
        })?;
        let family = cx.arg_string(0)?;
        let (source, status) = if matches!(cx.arg_kind(1), ValueKind::Object) {
            match cx.arg_bytes(1) {
                Some(b) => {
                    let ok = is_font_data(&b);
                    (Source::Binary(ok), if ok { "loaded" } else { "error" })
                }
                None => src_status(parse_src(&cx.arg_string(1)?)),
            }
        } else {
            src_status(parse_src(&cx.arg_string(1)?))
        };
        // Content Security Policy (`font-src`, ADR 0036) for `url()`s.
        if !matches!(cx.arg_kind(1), ValueKind::Object) {
            let src = cx.arg_string(1)?;
            super::csp::check_font_src(cx, w, &src);
        }
        let d = super::Dict::arg(cx, 2);
        let mut desc = Vec::with_capacity(DESCRIPTORS.len());
        for (k, def) in DESCRIPTORS {
            desc.push(d.string(cx, k)?.unwrap_or_else(|| def.to_owned()));
        }
        d.release(cx);
        let quote = serialize_family(&family) != family;
        let id = w.new_obj(Obj::Font(Box::new(FontObj::Face(Box::new(Face {
            family,
            quote,
            desc,
            status,
            source,
            loaded: None,
        })))));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    getter(reg, rt, F, "family", |cx, w| {
        let id = this_face(cx, w)?;
        let v = with_font(w, id, |f| match f {
            FontObj::Face(face) if face.quote => Some(serialize_family(&face.family)),
            FontObj::Face(face) => Some(face.family.clone()),
            _ => None,
        })
        .unwrap_or_default();
        cx.ret(v)
    });
    setter(reg, rt, F, "family", |cx, w| {
        let id = this_face(cx, w)?;
        let v = cx.arg_string(0)?;
        with_font(w, id, |f| {
            if let FontObj::Face(face) = f {
                face.family = v;
            }
            Some(())
        });
        Ok(())
    });
    for (k, (attr, _)) in DESCRIPTORS.iter().enumerate() {
        getter(reg, rt, F, attr, move |cx, w| {
            let id = this_face(cx, w)?;
            let v = with_font(w, id, |f| match f {
                FontObj::Face(face) => Some(face.desc[k].clone()),
                _ => None,
            })
            .unwrap_or_default();
            cx.ret(v)
        });
        setter(reg, rt, F, attr, move |cx, w| {
            let id = this_face(cx, w)?;
            let v = cx.arg_string(0)?;
            with_font(w, id, |f| {
                if let FontObj::Face(face) = f {
                    face.desc[k] = v;
                }
                Some(())
            });
            Ok(())
        });
    }
    getter(reg, rt, F, "status", |cx, w| {
        let id = this_face(cx, w)?;
        let s = face_info(w, id).map_or("error", |(_, s)| s);
        cx.ret(s)
    });
    getter(reg, rt, F, "loaded", |cx, w| {
        let id = this_face(cx, w)?;
        let h = settle(cx, w, id)?;
        cx.ret(Arg::Handle(h))
    });
    method(reg, rt, F, "load", |cx, w| {
        let id = this_face(cx, w)?;
        if face_info(w, id).is_some_and(|(_, s)| s == "unloaded") {
            load_face(w, id);
        }
        let h = settle(cx, w, id)?;
        cx.ret(Arg::Handle(h))
    });
}
