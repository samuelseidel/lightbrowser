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
//! - `FontFace`: descriptors are stored as given. `local("Name")` sources
//!   load when the persona has the font, binary sources when they start
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
    /// `local()` names and `url()`s of a `src` string.
    List { locals: Vec<String> },
    /// An `ArrayBuffer` / view that starts like a font.
    Binary(bool),
}

pub(crate) struct Face {
    family: String,
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

/// The `src` descriptor's `local()` names (`url()`s are not fetched).
fn parse_src(s: &str) -> Source {
    let mut locals = Vec::new();
    for part in s.split(',') {
        if let Some(rest) = part.trim().strip_prefix("local(") {
            let name = rest
                .trim_end_matches(')')
                .trim()
                .trim_matches(|c| c == '"' || c == '\'');
            locals.push(name.to_owned());
        }
    }
    Source::List { locals }
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

fn installed(w: &Window, name: &str) -> bool {
    w.profile.as_ref().is_some_and(|p| {
        p.fonts
            .available
            .iter()
            .any(|f| f.eq_ignore_ascii_case(name))
    })
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
        let (slot, _) = super::this_node(cx)?;
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
            cx.reject_promise(r, w.rt.err("NetworkError", "A network error occurred."))?;
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
                None => (parse_src(&cx.arg_string(1)?), "unloaded"),
            }
        } else {
            (parse_src(&cx.arg_string(1)?), "unloaded")
        };
        let d = super::Dict::arg(cx, 2);
        let mut desc = Vec::with_capacity(DESCRIPTORS.len());
        for (k, def) in DESCRIPTORS {
            desc.push(d.string(cx, k)?.unwrap_or_else(|| def.to_owned()));
        }
        d.release(cx);
        let id = w.new_obj(Obj::Font(Box::new(FontObj::Face(Box::new(Face {
            family,
            desc,
            status,
            source,
            loaded: None,
        })))));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    getter(reg, rt, F, "family", |cx, w| {
        let id = this_face(cx, w)?;
        let v = face_info(w, id).map(|(f, _)| f).unwrap_or_default();
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
