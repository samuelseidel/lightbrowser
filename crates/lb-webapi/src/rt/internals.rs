//! Form-associated custom elements, `ElementInternals`, `CustomStateSet`
//! and `FormData` (ADR 0026).
//!
//! A form-associated custom element (FACE) is flagged in `lb-dom`
//! (`NodeFlags::FORM_ASSOCIATED`, `CUSTOM_INVALID` for `:invalid`) so
//! selectors and `form.elements` see it; its form owner, submission value
//! and validity live here. Owner and disabled-state changes are found
//! after each batch of DOM changes and become `formAssociatedCallback` /
//! `formDisabledCallback` reactions. `FormData` entries are strings
//! (a `Blob` value is stored as its string form).

use std::collections::HashMap;
use std::rc::Rc;

use lb_dom::{Document, Mutation, NodeFlags, NodeId, local_name};
use lb_js::{Arg, CallCtx, HostError, HostRef, ValueKind};

use super::custom::{Reaction, enqueue_callback};
use super::{
    Dict, Js, K_OBJ, Obj, Runtime, Window, ctor, getter, illegal, method, need_args, setter,
    this_node, this_obj,
};
use crate::registry::Registry;

/// A FACE's submission value (`setFormValue`).
#[derive(Clone, Debug)]
pub(crate) enum FormValue {
    Str(String),
    Entries(Vec<(String, String)>),
}

/// Validity flags in `ValidityState` order.
const FLAGS: [&str; 10] = [
    "valueMissing",
    "typeMismatch",
    "patternMismatch",
    "tooLong",
    "tooShort",
    "rangeUnderflow",
    "rangeOverflow",
    "stepMismatch",
    "badInput",
    "customError",
];

/// Per-element internals state.
#[derive(Default)]
pub(crate) struct IData {
    /// The `ElementInternals` object, once attached.
    pub obj: Option<u32>,
    pub form_associated: bool,
    pub owner: Option<NodeId>,
    pub disabled: bool,
    pub value: Option<FormValue>,
    pub flags: [bool; 10],
    pub message: String,
    /// `role` / `aria*` properties set on the internals.
    pub aria: HashMap<String, String>,
}

impl IData {
    fn invalid(&self) -> bool {
        self.flags.iter().any(|&f| f)
    }
}

/// Internals state by (slot, element).
#[derive(Default)]
pub(crate) struct InternalsState {
    pub map: HashMap<(u32, NodeId), IData>,
}

fn form_owner(d: &Document, n: NodeId) -> Option<NodeId> {
    if let Some(id) = d.get_attribute(n, "form") {
        if !d.is_connected(n) {
            return None;
        }
        let root = d.root(n);
        return d
            .get_element_by_id_in(root, id)
            .filter(|&f| d.is_html_element_named(f, &local_name!("form")));
    }
    d.ancestors(n)
        .find(|&a| d.is_html_element_named(a, &local_name!("form")))
}

fn is_disabled(d: &Document, n: NodeId) -> bool {
    if d.has_attribute(n, "disabled") {
        return true;
    }
    let mut child = n;
    for a in d.ancestors(n) {
        if d.is_html_element_named(a, &local_name!("fieldset")) && d.has_attribute(a, "disabled") {
            // Not when inside the fieldset's first legend.
            let first_legend = d
                .element_children(a)
                .find(|&c| d.is_html_element_named(c, &local_name!("legend")));
            if first_legend != Some(child) {
                return true;
            }
        }
        child = a;
    }
    false
}

/// A form-associated element was upgraded: flag it and reset its form
/// owner (`formAssociatedCallback` when it has one).
pub(crate) fn upgraded(_js: &mut dyn Js, w: &Rc<Window>, slot: u32, n: NodeId) {
    let (owner, disabled) = w.doc_slot_mut(slot, |d| {
        d.set_flags(n, NodeFlags::FORM_ASSOCIATED, true);
        (form_owner(d, n), is_disabled(d, n))
    });
    {
        let mut st = w.internals.borrow_mut();
        let e = st.map.entry((slot, n)).or_default();
        e.form_associated = true;
        e.owner = owner;
        e.disabled = disabled;
    }
    if owner.is_some() {
        enqueue_callback(w, slot, n, Reaction::FormAssociated(owner));
    }
    if disabled {
        enqueue_callback(w, slot, n, Reaction::FormDisabled(true));
    }
}

/// Form owner and disabled-state changes after DOM changes.
pub(crate) fn on_mutations(w: &Rc<Window>, _batch: &[(u32, Mutation)]) {
    let faces: Vec<(u32, NodeId)> = w
        .internals
        .borrow()
        .map
        .iter()
        .filter(|(_, v)| v.form_associated)
        .map(|(k, _)| *k)
        .collect();
    let mut faces = faces;
    faces.sort();
    for (slot, n) in faces {
        let (owner, disabled) = w.doc_slot(slot, |d| (form_owner(d, n), is_disabled(d, n)));
        let (old_owner, old_disabled) = {
            let mut st = w.internals.borrow_mut();
            let e = st.map.entry((slot, n)).or_default();
            let old = (e.owner, e.disabled);
            e.owner = owner;
            e.disabled = disabled;
            old
        };
        if owner != old_owner {
            enqueue_callback(w, slot, n, Reaction::FormAssociated(owner));
        }
        if disabled != old_disabled {
            enqueue_callback(w, slot, n, Reaction::FormDisabled(disabled));
        }
    }
}

/// `form.reset()`: `formResetCallback` of the form's FACEs.
pub(crate) fn form_reset(js: &mut dyn Js, w: &Rc<Window>, slot: u32, form: NodeId) {
    let mut faces: Vec<NodeId> = w
        .internals
        .borrow()
        .map
        .iter()
        .filter(|((s, _), v)| *s == slot && v.form_associated && v.owner == Some(form))
        .map(|((_, n), _)| *n)
        .collect();
    faces.sort();
    for n in faces {
        enqueue_callback(w, slot, n, Reaction::FormReset);
    }
    super::custom::process(js, w);
}

/// The form data set entries of FACE `n`.
pub(crate) fn submission_entries(w: &Window, d: &Document, n: NodeId) -> Vec<(String, String)> {
    let st = w.internals.borrow();
    let Some(e) = st.map.get(&(0, n)) else {
        return Vec::new();
    };
    if is_disabled(d, n) {
        return Vec::new();
    }
    match &e.value {
        None => Vec::new(),
        Some(FormValue::Str(s)) => match d.get_attribute(n, "name").filter(|x| !x.is_empty()) {
            Some(name) => vec![(name.to_owned(), s.clone())],
            None => Vec::new(),
        },
        Some(FormValue::Entries(v)) => v.clone(),
    }
}

/// `form.checkValidity()` / `reportValidity()`: fires `invalid` at each
/// invalid control the form owns.
fn form_check(js: &mut dyn Js, w: &Rc<Window>, slot: u32, form: NodeId) -> bool {
    let controls: Vec<NodeId> = w.doc_slot(slot, |d| {
        d.descendants(form)
            .filter(|&c| {
                if d.flags(c).contains(NodeFlags::FORM_ASSOCIATED) {
                    return d.flags(c).contains(NodeFlags::CUSTOM_INVALID) && !is_disabled(d, c);
                }
                (d.is_html_element_named(c, &local_name!("input"))
                    || d.is_html_element_named(c, &local_name!("textarea"))
                    || d.is_html_element_named(c, &local_name!("select")))
                    && d.has_attribute(c, "required")
                    && !is_disabled(d, c)
                    && w.values.borrow().get(&(slot, c)).map_or_else(
                        || d.get_attribute(c, "value").unwrap_or("").is_empty(),
                        String::is_empty,
                    )
            })
            .collect()
    });
    for &c in &controls {
        super::events::fire_event(js, w, w.node_ref(slot, c), "invalid", false, true);
    }
    controls.is_empty()
}

fn this_internals(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, NodeId), HostError> {
    let id = this_obj(cx)?;
    w.with_obj(id, |o| match o {
        Obj::ElementInternals(s, n) => Some((*s, *n)),
        _ => None,
    })
    .flatten()
    .ok_or_else(illegal)
}

/// The FACE behind `this`, or Chrome's NotSupportedError.
fn this_face(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    what: &str,
) -> Result<(u32, NodeId), HostError> {
    let (s, n) = this_internals(cx, w)?;
    let face = w
        .internals
        .borrow()
        .map
        .get(&(s, n))
        .is_some_and(|e| e.form_associated);
    if !face {
        return Err(w.rt.err(
            "NotSupportedError",
            format!("{what}: The target element is not a form-associated custom element."),
        ));
    }
    Ok((s, n))
}

fn with_data<R>(w: &Window, key: (u32, NodeId), f: impl FnOnce(&mut IData) -> R) -> R {
    f(w.internals.borrow_mut().map.entry(key).or_default())
}

fn validity_flag(w: &Window, key: (u32, NodeId), i: usize) -> bool {
    w.internals
        .borrow()
        .map
        .get(&key)
        .is_some_and(|e| e.flags[i])
}

fn will_validate(w: &Window, (s, n): (u32, NodeId)) -> bool {
    w.doc_slot(s, |d| {
        !is_disabled(d, n)
            && !d.has_attribute(n, "readonly")
            && !d
                .ancestors(n)
                .any(|a| d.is_html_element_named(a, &local_name!("datalist")))
    })
}

fn check(js: &mut dyn Js, w: &Rc<Window>, key: (u32, NodeId)) -> bool {
    let invalid = will_validate(w, key)
        && w.internals
            .borrow()
            .map
            .get(&key)
            .is_some_and(IData::invalid);
    if invalid {
        super::events::fire_event(js, w, w.node_ref(key.0, key.1), "invalid", false, true);
    }
    !invalid
}

/// `role` and the string-valued ARIA reflection of `ElementInternals`.
const ARIA: &[&str] = &[
    "role",
    "ariaAtomic",
    "ariaAutoComplete",
    "ariaBrailleLabel",
    "ariaBrailleRoleDescription",
    "ariaBusy",
    "ariaChecked",
    "ariaColCount",
    "ariaColIndex",
    "ariaColIndexText",
    "ariaColSpan",
    "ariaCurrent",
    "ariaDescription",
    "ariaDisabled",
    "ariaExpanded",
    "ariaHasPopup",
    "ariaHidden",
    "ariaInvalid",
    "ariaKeyShortcuts",
    "ariaLabel",
    "ariaLevel",
    "ariaLive",
    "ariaModal",
    "ariaMultiLine",
    "ariaMultiSelectable",
    "ariaOrientation",
    "ariaPlaceholder",
    "ariaPosInSet",
    "ariaPressed",
    "ariaReadOnly",
    "ariaRelevant",
    "ariaRequired",
    "ariaRoleDescription",
    "ariaRowCount",
    "ariaRowIndex",
    "ariaRowIndexText",
    "ariaRowSpan",
    "ariaSelected",
    "ariaSetSize",
    "ariaSort",
    "ariaValueMax",
    "ariaValueMin",
    "ariaValueNow",
    "ariaValueText",
];

fn form_data_list(w: &Window, id: u32) -> Option<Vec<(String, String)>> {
    w.with_obj(id, |o| match o {
        Obj::FormData(v) => Some(v.clone()),
        _ => None,
    })
    .flatten()
}

fn this_form_data(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u32, HostError> {
    let id = this_obj(cx)?;
    form_data_list(w, id).map(|_| id).ok_or_else(illegal)
}

fn edit_form_data(w: &Window, id: u32, f: impl FnOnce(&mut Vec<(String, String)>)) {
    w.with_obj(id, |o| {
        if let Obj::FormData(v) = o {
            f(v);
        }
    });
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    method(reg, rt, "HTMLElement", "attachInternals", |cx, w| {
        const OP: &str = "Failed to execute 'attachInternals' on 'HTMLElement'";
        let (slot, n) = this_node(cx, w)?;
        let err = |w: &Window, m: &str| w.rt.err("NotSupportedError", format!("{OP}: {m}"));
        if w.doc_slot(slot, |d| d.is_value(n).is_some()) {
            return Err(err(
                w,
                "Unable to attach ElementInternals to a customized built-in element.",
            ));
        }
        let Some(def) = super::custom::definition_of(w, slot, n) else {
            return Err(err(
                w,
                "Unable to attach ElementInternals to non-custom elements.",
            ));
        };
        if def.disable_internals {
            return Err(err(
                w,
                "ElementInternals is disabled by disabledFeature static field.",
            ));
        }
        if w.internals
            .borrow()
            .map
            .get(&(slot, n))
            .is_some_and(|e| e.obj.is_some())
        {
            return Err(err(
                w,
                "ElementInternals for the specified element was already attached.",
            ));
        }
        let state = w.doc_slot(slot, |d| d.custom_state(n));
        if !matches!(
            state,
            lb_dom::CustomState::Precustomized | lb_dom::CustomState::Custom
        ) {
            return Err(err(
                w,
                "The attachInternals() function cannot be called prior to the execution of the custom element constructor.",
            ));
        }
        let id = w.new_obj(Obj::ElementInternals(slot, n));
        with_data(w, (slot, n), |e| {
            e.obj = Some(id);
            e.form_associated |= def.form_associated;
        });
        cx.ret(w.obj_arg(id))
    });
    getter(reg, rt, "ElementInternals", "form", |cx, w| {
        let key = this_face(
            cx,
            w,
            "Failed to read the 'form' property from 'ElementInternals'",
        )?;
        let owner = w.doc_slot(key.0, |d| form_owner(d, key.1));
        cx.ret(w.opt_node_arg(key.0, owner))
    });
    getter(reg, rt, "ElementInternals", "willValidate", |cx, w| {
        let key = this_face(
            cx,
            w,
            "Failed to read the 'willValidate' property from 'ElementInternals'",
        )?;
        cx.ret(will_validate(w, key))
    });
    getter(reg, rt, "ElementInternals", "validationMessage", |cx, w| {
        let key = this_face(
            cx,
            w,
            "Failed to read the 'validationMessage' property from 'ElementInternals'",
        )?;
        let m = w
            .internals
            .borrow()
            .map
            .get(&key)
            .filter(|e| e.invalid())
            .map(|e| e.message.clone())
            .unwrap_or_default();
        cx.ret(m)
    });
    getter(reg, rt, "ElementInternals", "validity", |cx, w| {
        let key = this_face(
            cx,
            w,
            "Failed to read the 'validity' property from 'ElementInternals'",
        )?;
        let id = w.cached_obj((key.0, key.1.0, "internalsValidity", String::new()), || {
            Obj::InternalsValidity(key.0, key.1)
        });
        cx.ret(w.obj_arg(id))
    });
    getter(reg, rt, "ElementInternals", "labels", |cx, w| {
        let key = this_face(
            cx,
            w,
            "Failed to read the 'labels' property from 'ElementInternals'",
        )?;
        let id = w.new_obj(Obj::NodeList(super::coll::NodeListSrc::Static(
            key.0,
            Vec::new(),
        )));
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, "ElementInternals", "setFormValue", |cx, w| {
        need_args(cx, 1, "setFormValue", "ElementInternals")?;
        let key = this_face(
            cx,
            w,
            "Failed to execute 'setFormValue' on 'ElementInternals'",
        )?;
        let v = match cx.arg_kind(0) {
            ValueKind::Null | ValueKind::Undefined => None,
            _ => match cx
                .arg_host(0)
                .filter(|h| h.kind == K_OBJ)
                .and_then(|h| form_data_list(w, h.id))
            {
                Some(list) => Some(FormValue::Entries(list)),
                None => Some(FormValue::Str(cx.arg_string(0)?)),
            },
        };
        with_data(w, key, |e| e.value = v);
        Ok(())
    });
    method(reg, rt, "ElementInternals", "setValidity", |cx, w| {
        let key = this_face(
            cx,
            w,
            "Failed to execute 'setValidity' on 'ElementInternals'",
        )?;
        let d = Dict::arg(cx, 0);
        let mut sorted: Vec<&str> = FLAGS.to_vec();
        sorted.sort_unstable();
        let mut flags = [false; 10];
        for name in sorted {
            let v = d.bool(cx, name)?;
            let i = FLAGS.iter().position(|f| *f == name).unwrap_or(0);
            flags[i] = v;
        }
        d.release(cx);
        let message = super::opt_string(cx, 1)?.unwrap_or_default();
        if flags.iter().any(|&f| f) && message.is_empty() {
            return Err(HostError::type_error(
                "Failed to execute 'setValidity' on 'ElementInternals': The second argument should not be empty if one or more flags in the first argument are true.",
            ));
        }
        let invalid = flags.iter().any(|&f| f);
        with_data(w, key, |e| {
            e.flags = flags;
            e.message = if invalid { message } else { String::new() };
        });
        w.doc_slot_mut(key.0, |d| {
            d.set_flags(key.1, NodeFlags::CUSTOM_INVALID, invalid)
        });
        Ok(())
    });
    for name in ["checkValidity", "reportValidity"] {
        method(reg, rt, "ElementInternals", name, move |cx, w| {
            let key = this_face(
                cx,
                w,
                &format!("Failed to execute '{name}' on 'ElementInternals'"),
            )?;
            let ok = check(cx, w, key);
            cx.ret(ok)
        });
    }
    getter(reg, rt, "ElementInternals", "shadowRoot", |cx, w| {
        let (s, n) = this_internals(cx, w)?;
        let r = w.doc_slot(s, |d| d.shadow_root(n));
        cx.ret(w.opt_node_arg(s, r))
    });
    getter(reg, rt, "ElementInternals", "states", |cx, w| {
        let (s, n) = this_internals(cx, w)?;
        let id = w.cached_obj((s, n.0, "customStates", String::new()), || {
            Obj::CustomStateSet(s, n)
        });
        cx.ret(w.obj_arg(id))
    });
    for &name in ARIA {
        getter(reg, rt, "ElementInternals", name, move |cx, w| {
            let key = this_internals(cx, w)?;
            let v = w
                .internals
                .borrow()
                .map
                .get(&key)
                .and_then(|e| e.aria.get(name).cloned());
            cx.ret(v.map_or(Arg::Null, Arg::String))
        });
        setter(reg, rt, "ElementInternals", name, move |cx, w| {
            let key = this_internals(cx, w)?;
            let v = match cx.arg_kind(0) {
                ValueKind::Null | ValueKind::Undefined => None,
                _ => Some(cx.arg_string(0)?),
            };
            with_data(w, key, |e| match v {
                Some(v) => {
                    e.aria.insert(name.to_owned(), v);
                }
                None => {
                    e.aria.remove(name);
                }
            });
            Ok(())
        });
    }

    // ValidityState of internals
    for (i, name) in FLAGS.iter().enumerate() {
        getter(reg, rt, "ValidityState", name, move |cx, w| {
            let id = this_obj(cx)?;
            let key = w
                .with_obj(id, |o| match o {
                    Obj::InternalsValidity(s, n) => Some((*s, *n)),
                    _ => None,
                })
                .flatten()
                .ok_or_else(illegal)?;
            cx.ret(validity_flag(w, key, i))
        });
    }
    getter(reg, rt, "ValidityState", "valid", |cx, w| {
        let id = this_obj(cx)?;
        let key = w
            .with_obj(id, |o| match o {
                Obj::InternalsValidity(s, n) => Some((*s, *n)),
                _ => None,
            })
            .flatten()
            .ok_or_else(illegal)?;
        cx.ret(
            !w.internals
                .borrow()
                .map
                .get(&key)
                .is_some_and(IData::invalid),
        )
    });

    // CustomStateSet
    fn this_states(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, NodeId), HostError> {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| match o {
            Obj::CustomStateSet(s, n) => Some((*s, *n)),
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)
    }
    fn states(w: &Window, (s, n): (u32, NodeId)) -> Vec<String> {
        w.doc_slot(s, |d| d.custom_states(n).to_vec())
    }
    method(reg, rt, "CustomStateSet", "add", |cx, w| {
        need_args(cx, 1, "add", "CustomStateSet")?;
        let key = this_states(cx, w)?;
        let v = cx.arg_string(0)?;
        let mut list = states(w, key);
        if !list.contains(&v) {
            list.push(v);
            w.doc_slot_mut(key.0, |d| d.set_custom_states(key.1, list));
        }
        let me = cx.this_handle();
        cx.ret(Arg::Handle(me))
    });
    method(reg, rt, "CustomStateSet", "delete", |cx, w| {
        need_args(cx, 1, "delete", "CustomStateSet")?;
        let key = this_states(cx, w)?;
        let v = cx.arg_string(0)?;
        let mut list = states(w, key);
        let had = list.contains(&v);
        list.retain(|x| *x != v);
        w.doc_slot_mut(key.0, |d| d.set_custom_states(key.1, list));
        cx.ret(had)
    });
    method(reg, rt, "CustomStateSet", "has", |cx, w| {
        need_args(cx, 1, "has", "CustomStateSet")?;
        let key = this_states(cx, w)?;
        let v = cx.arg_string(0)?;
        cx.ret(states(w, key).contains(&v))
    });
    method(reg, rt, "CustomStateSet", "clear", |cx, w| {
        let key = this_states(cx, w)?;
        w.doc_slot_mut(key.0, |d| d.set_custom_states(key.1, Vec::new()));
        Ok(())
    });
    getter(reg, rt, "CustomStateSet", "size", |cx, w| {
        let key = this_states(cx, w)?;
        cx.ret(states(w, key).len() as u32)
    });
    for (name, pairs) in [("values", false), ("keys", false), ("entries", true)] {
        method(reg, rt, "CustomStateSet", name, move |cx, w| {
            let key = this_states(cx, w)?;
            let items = states(w, key)
                .into_iter()
                .map(|s| {
                    if pairs {
                        Arg::Array(vec![Arg::String(s.clone()), Arg::String(s)])
                    } else {
                        Arg::String(s)
                    }
                })
                .collect();
            super::misc::ret_iterator(cx, w, items)
        });
    }
    method(reg, rt, "CustomStateSet", "forEach", |cx, w| {
        need_args(cx, 1, "forEach", "CustomStateSet")?;
        let key = this_states(cx, w)?;
        let f = cx.arg_handle(0);
        let me = Arg::Handle(cx.this_handle());
        for s in states(w, key) {
            cx.call(
                f,
                Arg::Undefined,
                &[Arg::String(s.clone()), Arg::String(s), me.clone()],
            )?;
        }
        Ok(())
    });

    // HTMLFormElement validity
    for name in ["checkValidity", "reportValidity"] {
        method(reg, rt, "HTMLFormElement", name, |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let ok = form_check(cx, w, slot, n);
            cx.ret(ok)
        });
    }

    // FormData
    ctor(reg, rt, "FormData", |cx, w| {
        let list = if !cx.is_empty() && cx.arg_kind(0) != ValueKind::Undefined {
            let form = cx
                .arg_host(0)
                .and_then(|h| w.decode_node(h))
                .filter(|&(s, n)| {
                    w.doc_slot(s, |d| d.is_html_element_named(n, &local_name!("form")))
                });
            let Some((s, f)) = form else {
                return Err(HostError::type_error(
                    "Failed to construct 'FormData': parameter 1 is not of type 'HTMLFormElement'.",
                ));
            };
            w.doc_slot(s, |d| super::window::form_entries(w, d, f))
        } else {
            Vec::new()
        };
        let id = w.new_obj(Obj::FormData(list));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    for (name, set) in [("append", false), ("set", true)] {
        method(reg, rt, "FormData", name, move |cx, w| {
            need_args(cx, 2, name, "FormData")?;
            let id = this_form_data(cx, w)?;
            let k = cx.arg_string(0)?;
            let v = cx.arg_string(1)?;
            edit_form_data(w, id, |list| {
                if set && let Some(i) = list.iter().position(|(x, _)| *x == k) {
                    list[i].1 = v;
                    let mut seen = false;
                    list.retain(|(x, _)| {
                        if *x != k {
                            return true;
                        }
                        let keep = !seen;
                        seen = true;
                        keep
                    });
                    return;
                }
                list.push((k, v));
            });
            Ok(())
        });
    }
    method(reg, rt, "FormData", "delete", |cx, w| {
        need_args(cx, 1, "delete", "FormData")?;
        let id = this_form_data(cx, w)?;
        let k = cx.arg_string(0)?;
        edit_form_data(w, id, |list| list.retain(|(x, _)| *x != k));
        Ok(())
    });
    method(reg, rt, "FormData", "get", |cx, w| {
        need_args(cx, 1, "get", "FormData")?;
        let id = this_form_data(cx, w)?;
        let k = cx.arg_string(0)?;
        let v = form_data_list(w, id)
            .unwrap_or_default()
            .into_iter()
            .find(|(x, _)| *x == k)
            .map(|(_, v)| v);
        cx.ret(v.map_or(Arg::Null, Arg::String))
    });
    method(reg, rt, "FormData", "getAll", |cx, w| {
        need_args(cx, 1, "getAll", "FormData")?;
        let id = this_form_data(cx, w)?;
        let k = cx.arg_string(0)?;
        let v = form_data_list(w, id)
            .unwrap_or_default()
            .into_iter()
            .filter(|(x, _)| *x == k)
            .map(|(_, v)| Arg::String(v))
            .collect();
        cx.ret(Arg::Array(v))
    });
    method(reg, rt, "FormData", "has", |cx, w| {
        need_args(cx, 1, "has", "FormData")?;
        let id = this_form_data(cx, w)?;
        let k = cx.arg_string(0)?;
        cx.ret(
            form_data_list(w, id)
                .unwrap_or_default()
                .iter()
                .any(|(x, _)| *x == k),
        )
    });
    for (name, part) in [("entries", 0), ("keys", 1), ("values", 2)] {
        method(reg, rt, "FormData", name, move |cx, w| {
            let id = this_form_data(cx, w)?;
            let items = form_data_list(w, id)
                .unwrap_or_default()
                .into_iter()
                .map(|(k, v)| match part {
                    0 => Arg::Array(vec![Arg::String(k), Arg::String(v)]),
                    1 => Arg::String(k),
                    _ => Arg::String(v),
                })
                .collect();
            super::misc::ret_iterator(cx, w, items)
        });
    }
    method(reg, rt, "FormData", "forEach", |cx, w| {
        need_args(cx, 1, "forEach", "FormData")?;
        let id = this_form_data(cx, w)?;
        let f = cx.arg_handle(0);
        let me = w.obj_arg(id);
        for (k, v) in form_data_list(w, id).unwrap_or_default() {
            cx.call(
                f,
                Arg::Undefined,
                &[Arg::String(v), Arg::String(k), me.clone()],
            )?;
        }
        Ok(())
    });
}
