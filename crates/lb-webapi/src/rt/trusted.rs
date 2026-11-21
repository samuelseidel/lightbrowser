//! Trusted Types (`trustedTypes` / `TrustedTypePolicyFactory`) as used by
//! Cloudflare Turnstile challenge pages (`require-trusted-types-for 'script'`
//! and `trusted-types … default` in the challenge CSP).
//!
//! Enough for `createPolicy` + `createHTML` / `createScript` /
//! `createScriptURL`, `emptyHTML` / `emptyScript`, `isHTML` / `isScript` /
//! `isScriptURL`, and `defaultPolicy`. `TrustedScript` is marked
//! `[IsCodeLike]` so `eval` / `Function` compile its source (ADR path via
//! lb-js modify-codegen callback). CSP policy-name allowlists and sink
//! enforcement are not applied yet: string sinks still accept DOMString, and
//! `TrustedHTML` values stringify via `toString` into `innerHTML`.

use std::collections::HashMap;
use std::rc::Rc;

use lb_js::{Arg, CallCtx, Handle, HostError, HostRef, ValueKind};

use super::{Dict, K_OBJ, Obj, Runtime, Window, getter, illegal, method, need_args, this_obj};
use crate::registry::Registry;

/// Trusted Types host objects.
pub(crate) enum TrustedObj {
    Factory {
        policies: HashMap<String, u32>,
        default_policy: Option<u32>,
        empty_html: u32,
        empty_script: u32,
    },
    Policy {
        name: String,
        create_html: Option<Handle>,
        create_script: Option<Handle>,
        create_script_url: Option<Handle>,
    },
    Html(String),
    Script(String),
    ScriptUrl(String),
}

impl TrustedObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            TrustedObj::Factory { .. } => "TrustedTypePolicyFactory",
            TrustedObj::Policy { .. } => "TrustedTypePolicy",
            TrustedObj::Html(_) => "TrustedHTML",
            TrustedObj::Script(_) => "TrustedScript",
            TrustedObj::ScriptUrl(_) => "TrustedScriptURL",
        }
    }
}

fn with_trusted<R>(w: &Window, id: u32, f: impl FnOnce(&mut TrustedObj) -> Option<R>) -> Option<R> {
    w.with_obj(id, |o| match o {
        Obj::Trusted(t) => f(t),
        _ => None,
    })
    .flatten()
}

fn this_trusted(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
) -> Result<(u32, &'static str), HostError> {
    let id = this_obj(cx)?;
    let name = with_trusted(w, id, |t| Some(t.iface_name())).ok_or_else(illegal)?;
    Ok((id, name))
}

fn data_string(w: &Window, id: u32) -> Option<String> {
    with_trusted(w, id, |t| match t {
        TrustedObj::Html(s) | TrustedObj::Script(s) | TrustedObj::ScriptUrl(s) => Some(s.clone()),
        _ => None,
    })
}

/// Register Trusted Types on Window / WorkerGlobalScope.
pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    for global in ["Window", "WorkerGlobalScope"] {
        getter(reg, rt, global, "trustedTypes", |cx, w| {
            let id = w.cached_obj((0, 0, "trustedTypes", String::new()), || {
                // Placeholders; replaced with real empty_* ids below.
                let empty_html = 0;
                let empty_script = 0;
                Obj::Trusted(Box::new(TrustedObj::Factory {
                    policies: HashMap::new(),
                    default_policy: None,
                    empty_html,
                    empty_script,
                }))
            });
            // Ensure emptyHTML / emptyScript exist (first access).
            let (eh, es) = with_trusted(w, id, |t| match t {
                TrustedObj::Factory {
                    empty_html,
                    empty_script,
                    ..
                } => Some((*empty_html, *empty_script)),
                _ => None,
            })
            .unwrap_or((0, 0));
            if eh == 0 {
                let empty_html = w.new_obj(Obj::Trusted(Box::new(TrustedObj::Html(String::new()))));
                let empty_script =
                    w.new_obj(Obj::Trusted(Box::new(TrustedObj::Script(String::new()))));
                cx.mark_code_like(HostRef::new(K_OBJ, empty_script), String::new());
                with_trusted(w, id, |t| {
                    if let TrustedObj::Factory {
                        empty_html: eh,
                        empty_script: es,
                        ..
                    } = t
                    {
                        *eh = empty_html;
                        *es = empty_script;
                    }
                    Some(())
                });
            } else {
                let _ = es;
            }
            cx.ret(w.obj_arg(id))
        });
    }

    getter(reg, rt, "TrustedTypePolicyFactory", "emptyHTML", |cx, w| {
        let (id, _) = this_trusted(cx, w)?;
        let eh = with_trusted(w, id, |t| match t {
            TrustedObj::Factory { empty_html, .. } => Some(*empty_html),
            _ => None,
        })
        .ok_or_else(illegal)?;
        cx.ret(w.obj_arg(eh))
    });
    getter(
        reg,
        rt,
        "TrustedTypePolicyFactory",
        "emptyScript",
        |cx, w| {
            let (id, _) = this_trusted(cx, w)?;
            let es = with_trusted(w, id, |t| match t {
                TrustedObj::Factory { empty_script, .. } => Some(*empty_script),
                _ => None,
            })
            .ok_or_else(illegal)?;
            cx.ret(w.obj_arg(es))
        },
    );
    getter(
        reg,
        rt,
        "TrustedTypePolicyFactory",
        "defaultPolicy",
        |cx, w| {
            let (id, _) = this_trusted(cx, w)?;
            let dp = with_trusted(w, id, |t| match t {
                TrustedObj::Factory { default_policy, .. } => Some(*default_policy),
                _ => None,
            })
            .ok_or_else(illegal)?;
            match dp {
                Some(p) => cx.ret(w.obj_arg(p)),
                None => cx.ret(Arg::Null),
            }
        },
    );

    method(
        reg,
        rt,
        "TrustedTypePolicyFactory",
        "createPolicy",
        create_policy,
    );
    method(reg, rt, "TrustedTypePolicyFactory", "isHTML", |cx, w| {
        is_kind(cx, w, |t| matches!(t, TrustedObj::Html(_)))
    });
    method(reg, rt, "TrustedTypePolicyFactory", "isScript", |cx, w| {
        is_kind(cx, w, |t| matches!(t, TrustedObj::Script(_)))
    });
    method(
        reg,
        rt,
        "TrustedTypePolicyFactory",
        "isScriptURL",
        |cx, w| is_kind(cx, w, |t| matches!(t, TrustedObj::ScriptUrl(_))),
    );
    method(
        reg,
        rt,
        "TrustedTypePolicyFactory",
        "getAttributeType",
        |cx, w| {
            let _ = this_trusted(cx, w)?;
            need_args(cx, 2, "getAttributeType", "TrustedTypePolicyFactory")?;
            let tag = cx.arg_string(0)?.to_ascii_lowercase();
            let attr = cx.arg_string(1)?.to_ascii_lowercase();
            cx.ret(match (tag.as_str(), attr.as_str()) {
                ("script", "src") => Arg::String("TrustedScriptURL".into()),
                ("iframe", "srcdoc") | ("*", "srcdoc") => Arg::String("TrustedHTML".into()),
                (_, "href") if tag == "script" => Arg::String("TrustedScriptURL".into()),
                _ => Arg::Null,
            })
        },
    );
    method(
        reg,
        rt,
        "TrustedTypePolicyFactory",
        "getPropertyType",
        |cx, w| {
            let _ = this_trusted(cx, w)?;
            need_args(cx, 2, "getPropertyType", "TrustedTypePolicyFactory")?;
            let tag = cx.arg_string(0)?.to_ascii_lowercase();
            let prop = cx.arg_string(1)?;
            cx.ret(match (tag.as_str(), prop.as_str()) {
                ("script", "text" | "innerText" | "textContent") => {
                    Arg::String("TrustedScript".into())
                }
                (_, "innerHTML" | "outerHTML") => Arg::String("TrustedHTML".into()),
                ("iframe", "srcdoc") => Arg::String("TrustedHTML".into()),
                _ => Arg::Null,
            })
        },
    );
    // getTypeMapping: optional introspection; null is fine for Turnstile.
    method(
        reg,
        rt,
        "TrustedTypePolicyFactory",
        "getTypeMapping",
        |cx, w| {
            let _ = this_trusted(cx, w)?;
            cx.ret(Arg::Null)
        },
    );

    getter(reg, rt, "TrustedTypePolicy", "name", |cx, w| {
        let (id, _) = this_trusted(cx, w)?;
        let name = with_trusted(w, id, |t| match t {
            TrustedObj::Policy { name, .. } => Some(name.clone()),
            _ => None,
        })
        .ok_or_else(illegal)?;
        cx.ret(name)
    });
    method(reg, rt, "TrustedTypePolicy", "createHTML", |cx, w| {
        create_typed(cx, w, Kind::Html)
    });
    method(reg, rt, "TrustedTypePolicy", "createScript", |cx, w| {
        create_typed(cx, w, Kind::Script)
    });
    method(reg, rt, "TrustedTypePolicy", "createScriptURL", |cx, w| {
        create_typed(cx, w, Kind::ScriptUrl)
    });

    for iface in ["TrustedHTML", "TrustedScript", "TrustedScriptURL"] {
        method(reg, rt, iface, "toString", |cx, w| {
            let (id, _) = this_trusted(cx, w)?;
            let s = data_string(w, id).ok_or_else(illegal)?;
            cx.ret(s)
        });
        method(reg, rt, iface, "toJSON", |cx, w| {
            let (id, _) = this_trusted(cx, w)?;
            let s = data_string(w, id).ok_or_else(illegal)?;
            cx.ret(s)
        });
    }
}

fn is_kind(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    pred: impl FnOnce(&TrustedObj) -> bool,
) -> Result<(), HostError> {
    let _ = this_trusted(cx, w)?;
    need_args(cx, 1, "isHTML", "TrustedTypePolicyFactory")?;
    let ok = cx
        .arg_host(0)
        .filter(|h| h.kind == K_OBJ)
        .and_then(|h| with_trusted(w, h.id, |t| Some(pred(t))))
        .unwrap_or(false);
    cx.ret(ok)
}

fn create_policy(cx: &mut CallCtx<'_, '_, '_>, w: &Rc<Window>) -> Result<(), HostError> {
    need_args(cx, 1, "createPolicy", "TrustedTypePolicyFactory")?;
    let (fid, _) = this_trusted(cx, w)?;
    let name = cx.arg_string(0)?;
    let exists = with_trusted(w, fid, |t| match t {
        TrustedObj::Factory { policies, .. } => Some(policies.contains_key(&name)),
        _ => None,
    })
    .unwrap_or(false);
    if exists {
        return Err(HostError::type_error(format!(
            "Failed to execute 'createPolicy' on 'TrustedTypePolicyFactory': Policy with name '{name}' already exists."
        )));
    }
    let opts = Dict::arg(cx, 1);
    let create_html = opts.get(cx, "createHTML")?;
    let create_script = opts.get(cx, "createScript")?;
    let create_script_url = opts.get(cx, "createScriptURL")?;
    // Keep callback handles; release the options bag.
    opts.release(cx);

    let pid = w.new_obj(Obj::Trusted(Box::new(TrustedObj::Policy {
        name: name.clone(),
        create_html,
        create_script,
        create_script_url,
    })));
    with_trusted(w, fid, |t| {
        if let TrustedObj::Factory {
            policies,
            default_policy,
            ..
        } = t
        {
            policies.insert(name.clone(), pid);
            if name == "default" {
                *default_policy = Some(pid);
            }
        }
        Some(())
    });
    cx.ret(w.obj_arg(pid))
}

#[derive(Clone, Copy)]
enum Kind {
    Html,
    Script,
    ScriptUrl,
}

fn create_typed(cx: &mut CallCtx<'_, '_, '_>, w: &Window, kind: Kind) -> Result<(), HostError> {
    let (pid, _) = this_trusted(cx, w)?;
    need_args(cx, 1, "createHTML", "TrustedTypePolicy")?;
    let input = cx.arg_string(0)?;
    let (pname, cb) = with_trusted(w, pid, |t| match t {
        TrustedObj::Policy {
            name,
            create_html,
            create_script,
            create_script_url,
            ..
        } => {
            let cb = match kind {
                Kind::Html => *create_html,
                Kind::Script => *create_script,
                Kind::ScriptUrl => *create_script_url,
            };
            Some((name.clone(), cb))
        }
        _ => None,
    })
    .ok_or_else(illegal)?;

    let method = match kind {
        Kind::Html => "createHTML",
        Kind::Script => "createScript",
        Kind::ScriptUrl => "createScriptURL",
    };
    let Some(cb) = cb else {
        return Err(HostError::type_error(format!(
            "Failed to execute '{method}' on 'TrustedTypePolicy': Policy '{pname}'s {method} function is null."
        )));
    };

    // Extra args after input are forwarded to the callback.
    let mut args: Vec<Arg> = vec![Arg::String(input)];
    for i in 1..cx.len() {
        args.push(Arg::Handle(cx.arg_handle(i)));
    }
    let result_h = cx
        .try_call_handle(cb, Arg::Undefined, &args)
        .map_err(|e| HostError::Error(e.message))?;
    // Release forwarded handles we created (not the callback).
    for a in args.iter().skip(1) {
        if let Arg::Handle(h) = a {
            cx.release(*h);
        }
    }

    let kind_r = cx.handle_kind(result_h);
    if matches!(kind_r, ValueKind::Null | ValueKind::Undefined) {
        cx.release(result_h);
        return Err(HostError::type_error(format!(
            "Failed to execute '{method}' on 'TrustedTypePolicy': Policy {method} returned null or undefined."
        )));
    }
    let value = cx.handle_string(result_h).unwrap_or_default();
    cx.release(result_h);

    let id = w.new_obj(Obj::Trusted(Box::new(match kind {
        Kind::Html => TrustedObj::Html(value.clone()),
        Kind::Script => TrustedObj::Script(value.clone()),
        Kind::ScriptUrl => TrustedObj::ScriptUrl(value.clone()),
    })));
    if matches!(kind, Kind::Script) {
        // WebIDL `[IsCodeLike]` on TrustedScript.
        cx.mark_code_like(HostRef::new(K_OBJ, id), value);
    }
    cx.ret(w.obj_arg(id))
}
