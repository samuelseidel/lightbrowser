//! Trusted user input (CDP `Input.*`) through the DOM event pipeline, in
//! Chrome's order, with default actions: hover boundary events, pointer
//! and mouse events, focus, click/dblclick/auxclick/contextmenu, checkbox
//! and radio toggling, label activation, links, form submission, keyboard
//! events, text editing in text controls and contenteditable, Tab focus
//! navigation and wheel scrolling.
//!
//! Also the UI event interfaces (`UIEvent`, `MouseEvent`, `PointerEvent`,
//! `WheelEvent`, `KeyboardEvent`, `InputEvent`, `FocusEvent`) and the text
//! control selection API (`select()`, `selectionStart`, ...), `labels`,
//! `control` and `isContentEditable`.
//!
//! Every event fired here has `isTrusted` true. Synthetic `element.click()`
//! shares the activation code but stays untrusted.

use std::rc::Rc;

use lb_dom::{Document, NodeFlags, NodeId, NodeType, local_name};
use lb_js::{Arg, CallCtx, HostError, HostRef, ValueKind};

use super::events::{EventData, EventExtra, dispatch, new_event};
use super::{
    Dict, Js, K_OBJ, NavRequest, Obj, Runtime, WINDOW_REF, Window, ctor, getter, method, setter,
    this_node,
};
use crate::registry::Registry;

// ---------------------------------------------------------------------------
// Input descriptions (what the page layer passes in)
// ---------------------------------------------------------------------------

/// CDP modifier bits.
pub const MOD_ALT: u32 = 1;
pub const MOD_CTRL: u32 = 2;
pub const MOD_META: u32 = 4;
pub const MOD_SHIFT: u32 = 8;

/// `Input.dispatchMouseEvent` type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Move,
    Press,
    Release,
    Wheel,
}

/// A mouse button (`MouseEvent.button` numbering).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    None,
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

impl MouseButton {
    pub fn parse(s: &str) -> MouseButton {
        match s {
            "left" => MouseButton::Left,
            "middle" => MouseButton::Middle,
            "right" => MouseButton::Right,
            "back" => MouseButton::Back,
            "forward" => MouseButton::Forward,
            _ => MouseButton::None,
        }
    }

    /// `MouseEvent.button`.
    fn index(self) -> i16 {
        match self {
            MouseButton::None | MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::Back => 3,
            MouseButton::Forward => 4,
        }
    }

    /// Bit in `MouseEvent.buttons`.
    fn bit(self) -> u16 {
        match self {
            MouseButton::None => 0,
            MouseButton::Left => 1,
            MouseButton::Right => 2,
            MouseButton::Middle => 4,
            MouseButton::Back => 8,
            MouseButton::Forward => 16,
        }
    }
}

/// One `Input.dispatchMouseEvent`.
#[derive(Debug, Clone)]
pub struct MouseInput {
    pub kind: MouseKind,
    /// CSS pixels relative to the viewport.
    pub x: f64,
    pub y: f64,
    pub button: MouseButton,
    /// Buttons held after this event; derived when `None`.
    pub buttons: Option<u16>,
    pub click_count: u32,
    pub modifiers: u32,
    pub delta_x: f64,
    pub delta_y: f64,
    /// `mouse` or `pen`.
    pub pointer_type: String,
}

/// `Input.dispatchKeyEvent` type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    /// `keyDown`: keydown, then keypress and text input for `text`.
    Down,
    /// `rawKeyDown`: keydown only.
    RawDown,
    Up,
    /// `char`: keypress and text input.
    Char,
}

/// One `Input.dispatchKeyEvent`.
#[derive(Debug, Clone)]
pub struct KeyInput {
    pub kind: KeyKind,
    pub key: String,
    pub code: String,
    pub text: String,
    pub key_code: u32,
    pub location: u32,
    pub modifiers: u32,
    pub auto_repeat: bool,
    /// Editing commands (`selectAll`, ...).
    pub commands: Vec<String>,
}

// ---------------------------------------------------------------------------
// Event data
// ---------------------------------------------------------------------------

/// Pointer fields of a `PointerEvent`.
#[derive(Debug, Clone, Default)]
pub struct PointerData {
    pub id: i32,
    pub width: f64,
    pub height: f64,
    pub pressure: f64,
    pub pointer_type: String,
    pub primary: bool,
}

/// State of `MouseEvent` and its subclasses.
#[derive(Debug, Clone, Default)]
pub struct MouseData {
    pub screen_x: f64,
    pub screen_y: f64,
    pub client_x: f64,
    pub client_y: f64,
    pub movement_x: f64,
    pub movement_y: f64,
    pub button: i16,
    pub buttons: u16,
    pub modifiers: u32,
    pub related: Option<HostRef>,
    pub detail: i32,
    pub pointer: Option<PointerData>,
    /// `deltaX`, `deltaY`, `deltaZ`, `deltaMode`.
    pub wheel: Option<[f64; 4]>,
    /// Legacy `wheelDeltaX`, `wheelDeltaY`: `-delta x` the persona's wheel
    /// ratio for trusted wheels (120 per notch), the init's values or
    /// `-delta` for `new WheelEvent(...)` (Blink).
    pub wheel_delta: [f64; 2],
}

/// State of a `KeyboardEvent`.
#[derive(Debug, Clone, Default)]
pub struct KeyData {
    pub key: String,
    pub code: String,
    pub location: u32,
    pub modifiers: u32,
    pub repeat: bool,
    pub composing: bool,
    pub key_code: u32,
    pub char_code: u32,
}

/// Per-document input state.
#[derive(Debug, Default)]
pub(crate) struct InputState {
    /// Element under the pointer.
    hover: Option<NodeId>,
    /// Target of the last mousedown, for click.
    down: Option<NodeId>,
    buttons: u16,
    last: Option<(f64, f64)>,
    /// Value of the focused text control when it got focus (for `change`).
    focus_value: Option<(NodeId, String)>,
    /// A contenteditable whose whole content is selected.
    select_all: Option<NodeId>,
    /// Clock time of the last activation-triggering input.
    activation: Option<f64>,
}

/// Selection of a text control, in UTF-16 code units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sel {
    pub start: usize,
    pub end: usize,
    /// 0 none, 1 forward, 2 backward.
    pub dir: u8,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn local(d: &Document, n: NodeId) -> String {
    d.local_name(n).map(|l| l.to_string()).unwrap_or_default()
}

fn is_element(d: &Document, n: NodeId) -> bool {
    d.node_type(n) == NodeType::Element
}

/// Input types that take typed text.
const TEXT_TYPES: &[&str] = &[
    "text", "search", "url", "tel", "email", "password", "number",
];

/// The element is a text `<input>` or a `<textarea>`.
fn text_control(d: &Document, n: NodeId) -> bool {
    match local(d, n).as_str() {
        "textarea" => true,
        "input" => TEXT_TYPES.contains(&super::dom::input_type(d, n).as_str()),
        _ => false,
    }
}

/// `<input>` types with a selection API.
fn has_selection(d: &Document, n: NodeId) -> bool {
    match local(d, n).as_str() {
        "textarea" => true,
        "input" => matches!(
            super::dom::input_type(d, n).as_str(),
            "text" | "search" | "url" | "tel" | "password"
        ),
        _ => false,
    }
}

fn disabled(d: &Document, n: NodeId) -> bool {
    matches!(
        local(d, n).as_str(),
        "input" | "button" | "select" | "textarea" | "fieldset" | "optgroup" | "option"
    ) && (d.has_attribute(n, "disabled")
        || d.ancestors(n).any(|a| {
            d.is_html_element_named(a, &local_name!("fieldset")) && d.has_attribute(a, "disabled")
        }))
}

/// The contenteditable host of `n` (`n` or an ancestor), if editable.
fn editable_host(d: &Document, n: NodeId) -> Option<NodeId> {
    std::iter::once(n).chain(d.ancestors(n)).find_map(|a| {
        if !is_element(d, a) {
            return None;
        }
        match d
            .get_attribute(a, "contenteditable")
            .map(|v| v.to_ascii_lowercase())
        {
            Some(v) if v.is_empty() || v == "true" || v == "plaintext-only" => Some(Some(a)),
            Some(v) if v == "false" => Some(None),
            _ => None,
        }
    })?
}

/// The element `focus()` on `n` focuses: `n`, or for a host with
/// `delegatesFocus` the first focusable element of its shadow tree.
pub(crate) fn focus_delegate(d: &Document, n: NodeId) -> NodeId {
    let Some(sr) = d.shadow_root(n) else {
        return n;
    };
    if !d.shadow_root_data(sr).is_some_and(|x| x.delegates_focus) {
        return n;
    }
    d.shadow_including_descendants(sr)
        .into_iter()
        .find(|&c| focusable(d, c) && d.shadow_root(c).is_none())
        .unwrap_or(n)
}

fn focusable(d: &Document, n: NodeId) -> bool {
    if !is_element(d, n) || disabled(d, n) {
        return false;
    }
    if d.has_attribute(n, "tabindex") {
        return true;
    }
    match local(d, n).as_str() {
        "input" => super::dom::input_type(d, n) != "hidden",
        "select" | "textarea" | "button" | "iframe" | "summary" => true,
        "a" | "area" => d.has_attribute(n, "href"),
        _ => editable_host(d, n) == Some(n),
    }
}

/// Elements reached by Tab, in order.
fn tab_order(d: &Document) -> Vec<NodeId> {
    let mut items: Vec<(i32, usize, NodeId)> = Vec::new();
    for (i, n) in d.descendants(Document::DOCUMENT).enumerate() {
        if !focusable(d, n) {
            continue;
        }
        let ti = d
            .get_attribute(n, "tabindex")
            .and_then(|t| t.trim().parse::<i32>().ok())
            .unwrap_or(0);
        if ti >= 0 {
            items.push((if ti == 0 { i32::MAX } else { ti }, i, n));
        }
    }
    items.sort();
    items.into_iter().map(|(_, _, n)| n).collect()
}

/// The current value of a text control (dirty value or default).
pub(crate) fn control_value(w: &Window, slot: u32, n: NodeId) -> String {
    if let Some(v) = w.values.borrow().get(&(slot, n)) {
        return v.clone();
    }
    w.doc_slot(slot, |d| {
        if d.is_html_element_named(n, &local_name!("textarea")) {
            d.text_content(n).unwrap_or_default()
        } else {
            d.get_attribute(n, "value")
                .unwrap_or("")
                .replace(['\r', '\n'], "")
        }
    })
}

fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub(crate) fn selection(w: &Window, slot: u32, n: NodeId) -> Sel {
    if let Some(s) = w.selections.borrow().get(&(slot, n)) {
        let len = utf16(&control_value(w, slot, n)).len();
        return Sel {
            start: s.start.min(len),
            end: s.end.min(len),
            dir: s.dir,
        };
    }
    let len = utf16(&control_value(w, slot, n)).len();
    Sel {
        start: len,
        end: len,
        dir: 0,
    }
}

pub(crate) fn set_selection(w: &Window, slot: u32, n: NodeId, start: usize, end: usize, dir: u8) {
    let len = utf16(&control_value(w, slot, n)).len();
    let end = end.min(len);
    let start = start.min(end);
    w.selections
        .borrow_mut()
        .insert((slot, n), Sel { start, end, dir });
}

/// Replace UTF-16 range `[start, end)` of `value` with `text`.
fn splice(value: &str, start: usize, end: usize, text: &str) -> String {
    let u = utf16(value);
    let mut out: Vec<u16> = u[..start.min(u.len())].to_vec();
    out.extend(text.encode_utf16());
    out.extend_from_slice(&u[end.min(u.len())..]);
    String::from_utf16_lossy(&out)
}

/// Screen coordinates of a viewport point (window position plus the
/// browser frame, from the persona).
fn screen_point(w: &Window, x: f64, y: f64) -> (f64, f64) {
    let s = super::window::screen_dims(w);
    let (wx, wy) = super::window::window_position(w);
    let border = f64::from(s.outer_width.saturating_sub(s.inner_width)) / 2.0;
    let top = f64::from(s.outer_height.saturating_sub(s.inner_height)) - border;
    (f64::from(wx) + border + x, f64::from(wy) + top + y)
}

/// Transient user activation lasts 5 s in Chrome (`kActivationLifespan`).
const ACTIVATION_MS: f64 = 5000.0;

/// Note a trusted activation-triggering input (mousedown, keydown other
/// than Escape and modifiers).
fn activate_user(w: &Window) {
    w.input.borrow_mut().activation = Some(w.clock.now());
}

/// The window has transient user activation.
pub(crate) fn has_transient_activation(w: &Window) -> bool {
    w.input
        .borrow()
        .activation
        .is_some_and(|t| w.clock.now() - t <= ACTIVATION_MS)
}

/// Use up transient activation (`navigator.share()`, popups). True if
/// there was some.
pub(crate) fn consume_activation(w: &Window) -> bool {
    let had = has_transient_activation(w);
    w.input.borrow_mut().activation = None;
    had
}

#[allow(clippy::too_many_arguments)]
fn fire(
    js: &mut dyn Js,
    w: &Rc<Window>,
    target: HostRef,
    iface: &'static str,
    type_: &str,
    bubbles: bool,
    cancelable: bool,
    extra: EventExtra,
) -> bool {
    let ev = new_event(w, iface, type_, bubbles, cancelable, extra);
    w.with_obj(ev, |o| {
        if let Obj::Event(e) = o {
            e.composed = true;
        }
    });
    dispatch(js, w, target, ev)
}

#[allow(clippy::too_many_arguments)]
fn mouse_extra(
    w: &Window,
    x: f64,
    y: f64,
    button: i16,
    buttons: u16,
    modifiers: u32,
    detail: i32,
    related: Option<NodeId>,
    pointer: bool,
) -> EventExtra {
    let (sx, sy) = screen_point(w, x, y);
    let (px, py) = w.input.borrow().last.unwrap_or((x, y));
    EventExtra::Mouse(Box::new(MouseData {
        screen_x: sx,
        screen_y: sy,
        client_x: x,
        client_y: y,
        movement_x: x - px,
        movement_y: y - py,
        button,
        buttons,
        modifiers,
        related: related.map(|r| w.node_ref(0, r)),
        detail,
        pointer: pointer.then(|| PointerData {
            id: 1,
            width: 1.0,
            height: 1.0,
            pressure: if buttons != 0 { 0.5 } else { 0.0 },
            pointer_type: "mouse".into(),
            primary: true,
        }),
        wheel: None,
        wheel_delta: [0.0, 0.0],
    }))
}

/// The element under a viewport point (the root element if none).
fn hit(w: &Window, x: f64, y: f64) -> Option<NodeId> {
    let h = w.with_layout(0, |l, d| l.element_from_point(d, x as f32, y as f32));
    let h = h.map(|n| {
        w.with_doc(|d| {
            if is_element(d, n) {
                n
            } else {
                d.parent(n).unwrap_or(n)
            }
        })
    });
    h.or_else(|| w.with_doc(|d| d.document_element()))
}

fn connected(w: &Window, n: NodeId) -> bool {
    w.with_doc(|d| d.is_connected(n))
}

/// `n` and its ancestor elements, innermost first.
fn chain(w: &Window, n: NodeId) -> Vec<NodeId> {
    w.with_doc(|d| {
        std::iter::once(n)
            .chain(d.ancestors(n))
            .filter(|&a| is_element(d, a))
            .collect()
    })
}

fn common_ancestor(w: &Window, a: NodeId, b: NodeId) -> Option<NodeId> {
    let ca = chain(w, a);
    chain(w, b).into_iter().find(|x| ca.contains(x))
}

// ---------------------------------------------------------------------------
// Mouse
// ---------------------------------------------------------------------------

/// Dispatch one CDP mouse event.
pub fn dispatch_mouse(js: &mut dyn Js, w: &Rc<Window>, ev: &MouseInput) {
    match ev.kind {
        MouseKind::Move => mouse_move(js, w, ev),
        MouseKind::Press => mouse_press(js, w, ev),
        MouseKind::Release => mouse_release(js, w, ev),
        MouseKind::Wheel => mouse_wheel(js, w, ev),
    }
    w.input.borrow_mut().last = Some((ev.x, ev.y));
    w.after_mutation(js);
}

fn held(w: &Window, ev: &MouseInput) -> u16 {
    ev.buttons.unwrap_or(w.input.borrow().buttons)
}

/// Boundary events when the pointer moves from the old hover target to
/// `target`.
fn update_hover(js: &mut dyn Js, w: &Rc<Window>, ev: &MouseInput, target: NodeId) {
    let old = w.input.borrow().hover.filter(|&o| connected(w, o));
    if old == Some(target) {
        return;
    }
    let buttons = held(w, ev);
    let (x, y, m) = (ev.x, ev.y, ev.modifiers);
    let new_chain = chain(w, target);
    let old_chain = old.map(|o| chain(w, o)).unwrap_or_default();
    let leaving: Vec<NodeId> = old_chain
        .iter()
        .copied()
        .filter(|n| !new_chain.contains(n))
        .collect();
    let mut entering: Vec<NodeId> = new_chain
        .iter()
        .copied()
        .filter(|n| !old_chain.contains(n))
        .collect();
    entering.reverse();
    for pointer in [true, false] {
        let (out, leave, over, enter, iface) = if pointer {
            (
                "pointerout",
                "pointerleave",
                "pointerover",
                "pointerenter",
                "PointerEvent",
            )
        } else {
            (
                "mouseout",
                "mouseleave",
                "mouseover",
                "mouseenter",
                "MouseEvent",
            )
        };
        if let Some(o) = old {
            let e = mouse_extra(w, x, y, 0, buttons, m, 0, Some(target), pointer);
            fire(js, w, w.node_ref(0, o), iface, out, true, true, e);
            for &n in &leaving {
                let e = mouse_extra(w, x, y, 0, buttons, m, 0, Some(target), pointer);
                fire(js, w, w.node_ref(0, n), iface, leave, false, false, e);
            }
        }
        let e = mouse_extra(w, x, y, 0, buttons, m, 0, old, pointer);
        fire(js, w, w.node_ref(0, target), iface, over, true, true, e);
        for &n in &entering {
            let e = mouse_extra(w, x, y, 0, buttons, m, 0, old, pointer);
            fire(js, w, w.node_ref(0, n), iface, enter, false, false, e);
        }
    }
    w.input.borrow_mut().hover = Some(target);
}

fn mouse_move(js: &mut dyn Js, w: &Rc<Window>, ev: &MouseInput) {
    let Some(target) = hit(w, ev.x, ev.y) else {
        return;
    };
    update_hover(js, w, ev, target);
    let buttons = held(w, ev);
    let t = w.node_ref(0, target);
    let e = mouse_extra(w, ev.x, ev.y, -1, buttons, ev.modifiers, 0, None, true);
    fire(js, w, t, "PointerEvent", "pointermove", true, true, e);
    let e = mouse_extra(w, ev.x, ev.y, 0, buttons, ev.modifiers, 0, None, false);
    fire(js, w, t, "MouseEvent", "mousemove", true, true, e);
}

fn mouse_press(js: &mut dyn Js, w: &Rc<Window>, ev: &MouseInput) {
    let Some(target) = hit(w, ev.x, ev.y) else {
        return;
    };
    update_hover(js, w, ev, target);
    let first = w.input.borrow().buttons == 0;
    let buttons = ev
        .buttons
        .unwrap_or(w.input.borrow().buttons | ev.button.bit());
    w.input.borrow_mut().buttons = buttons;
    activate_user(w);
    let t = w.node_ref(0, target);
    let b = ev.button.index();
    let detail = ev.click_count as i32;
    let ok = if first {
        let e = mouse_extra(w, ev.x, ev.y, b, buttons, ev.modifiers, 0, None, true);
        fire(js, w, t, "PointerEvent", "pointerdown", true, true, e)
    } else {
        let e = mouse_extra(w, ev.x, ev.y, b, buttons, ev.modifiers, 0, None, true);
        fire(js, w, t, "PointerEvent", "pointermove", true, true, e);
        true
    };
    let mut prevented = !ok;
    if ok {
        let e = mouse_extra(w, ev.x, ev.y, b, buttons, ev.modifiers, detail, None, false);
        prevented = !fire(js, w, t, "MouseEvent", "mousedown", true, true, e);
    }
    w.input.borrow_mut().down = Some(target);
    if !prevented && ev.button == MouseButton::Left {
        // Focus the nearest focusable ancestor (or clear focus).
        let to = w.with_doc(|d| {
            std::iter::once(target)
                .chain(d.ancestors(target))
                .find(|&a| focusable(d, a))
        });
        if to != w.focused.get() {
            super::window::focus(js, w, to);
        }
        if let Some(f) = to {
            w.input.borrow_mut().select_all = None;
            let _ = f;
        }
    }
}

fn mouse_release(js: &mut dyn Js, w: &Rc<Window>, ev: &MouseInput) {
    let Some(target) = hit(w, ev.x, ev.y) else {
        return;
    };
    update_hover(js, w, ev, target);
    let buttons = ev
        .buttons
        .unwrap_or(w.input.borrow().buttons & !ev.button.bit());
    w.input.borrow_mut().buttons = buttons;
    let t = w.node_ref(0, target);
    let b = ev.button.index();
    let detail = ev.click_count as i32;
    let e = mouse_extra(w, ev.x, ev.y, b, buttons, ev.modifiers, 0, None, true);
    if buttons == 0 {
        fire(js, w, t, "PointerEvent", "pointerup", true, true, e);
    } else {
        fire(js, w, t, "PointerEvent", "pointermove", true, true, e);
    }
    let e = mouse_extra(w, ev.x, ev.y, b, buttons, ev.modifiers, detail, None, false);
    fire(js, w, t, "MouseEvent", "mouseup", true, true, e);
    let down = w.input.borrow_mut().down.take();
    let Some(click_target) = down
        .filter(|&d| connected(w, d))
        .and_then(|d| common_ancestor(w, d, target))
    else {
        return;
    };
    let extra = || mouse_extra(w, ev.x, ev.y, b, buttons, ev.modifiers, detail, None, true);
    match ev.button {
        MouseButton::Left => {
            let id = new_event(w, "PointerEvent", "click", true, true, extra());
            w.with_obj(id, |o| {
                if let Obj::Event(e) = o {
                    e.composed = true;
                }
            });
            activate(js, w, 0, click_target, id);
            if ev.click_count == 2 && connected(w, click_target) {
                let e = mouse_extra(w, ev.x, ev.y, b, buttons, ev.modifiers, 2, None, false);
                fire(
                    js,
                    w,
                    w.node_ref(0, click_target),
                    "MouseEvent",
                    "dblclick",
                    true,
                    true,
                    e,
                );
            }
        }
        MouseButton::None => {}
        _ => {
            fire(
                js,
                w,
                w.node_ref(0, click_target),
                "PointerEvent",
                "auxclick",
                true,
                true,
                extra(),
            );
            if ev.button == MouseButton::Right {
                fire(js, w, t, "PointerEvent", "contextmenu", true, true, extra());
            }
        }
    }
}

/// `wheelDelta` per CSS px of trusted wheels, from the persona's wheel
/// model (1.2 without a persona: Chrome on Windows at 100%).
pub(crate) fn wheel_ratio(w: &Window) -> f64 {
    w.profile
        .as_deref()
        .map_or(1.2, |p| p.wheel().wheel_delta_per_px)
}

fn mouse_wheel(js: &mut dyn Js, w: &Rc<Window>, ev: &MouseInput) {
    let Some(target) = hit(w, ev.x, ev.y) else {
        return;
    };
    update_hover(js, w, ev, target);
    let buttons = held(w, ev);
    let mut extra = mouse_extra(w, ev.x, ev.y, 0, buttons, ev.modifiers, 0, None, false);
    if let EventExtra::Mouse(m) = &mut extra {
        m.wheel = Some([ev.delta_x, ev.delta_y, 0.0, 0.0]);
        // Chrome: wheelDelta = notches x 120 (`kTickMultiplier`), i.e.
        // -delta x the OS's ratio (Windows 1.2, Linux 1, macOS 3).
        let ratio = wheel_ratio(w);
        m.wheel_delta = [(-ev.delta_x * ratio).round(), (-ev.delta_y * ratio).round()];
    }
    if fire(
        js,
        w,
        w.node_ref(0, target),
        "WheelEvent",
        "wheel",
        true,
        true,
        extra,
    ) {
        let moved = w.with_layout(0, |l, d| {
            let (sx, sy) = l.window_scroll(d);
            l.scroll_window_to(d, sx + ev.delta_x as f32, sy + ev.delta_y as f32);
            l.window_scroll(d) != (sx, sy)
        });
        if moved {
            super::events::fire_event(
                js,
                w,
                w.node_ref(0, Document::DOCUMENT),
                "scroll",
                true,
                false,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Activation (click default actions)
// ---------------------------------------------------------------------------

/// Radios in the same group as `n` (same name and form owner), `n` included.
fn radio_group(d: &Document, n: NodeId) -> Vec<NodeId> {
    let Some(name) = d
        .get_attribute(n, "name")
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
    else {
        return vec![n];
    };
    let form = |x: NodeId| {
        d.ancestors(x)
            .find(|&a| d.is_html_element_named(a, &local_name!("form")))
    };
    let owner = form(n);
    d.descendants(Document::DOCUMENT)
        .filter(|&x| {
            d.is_html_element_named(x, &local_name!("input"))
                && super::dom::input_type(d, x) == "radio"
                && d.get_attribute(x, "name") == Some(name.as_str())
                && form(x) == owner
        })
        .collect()
}

/// The labeled control of a `<label>`.
pub(crate) fn label_control(d: &Document, label: NodeId) -> Option<NodeId> {
    if let Some(id) = d.get_attribute(label, "for") {
        return d.get_element_by_id(id).filter(|&c| labelable(d, c));
    }
    d.descendants(label).find(|&c| labelable(d, c))
}

fn labelable(d: &Document, n: NodeId) -> bool {
    match local(d, n).as_str() {
        "button" | "meter" | "output" | "progress" | "select" | "textarea" => true,
        "input" => super::dom::input_type(d, n) != "hidden",
        _ => false,
    }
}

/// Interactive content that keeps a click inside a label to itself.
fn interactive(d: &Document, n: NodeId) -> bool {
    match local(d, n).as_str() {
        "a" => d.has_attribute(n, "href"),
        "button" | "select" | "textarea" | "label" | "iframe" | "details" => true,
        "input" => super::dom::input_type(d, n) != "hidden",
        _ => false,
    }
}

/// Dispatch click event `ev` at `n` with the activation behavior: checkbox
/// and radio pre-activation (undone if canceled), then links, submit and
/// reset buttons and label activation. Trusted or not as `ev` says.
pub(crate) fn activate(js: &mut dyn Js, w: &Rc<Window>, slot: u32, n: NodeId, ev: u32) {
    let (local_n, ty, dis) = w.doc_slot(slot, |d| {
        (local(d, n), super::dom::input_type(d, n), disabled(d, n))
    });
    if dis && matches!(local_n.as_str(), "input" | "button" | "select" | "textarea") {
        return;
    }
    let trusted = w
        .with_obj(ev, |o| match o {
            Obj::Event(e) => e.trusted,
            _ => false,
        })
        .unwrap_or(false);
    // Pre-activation: toggle now, undo if a listener cancels.
    let toggles = local_n == "input" && matches!(ty.as_str(), "checkbox" | "radio");
    let mut undo: Vec<(NodeId, bool)> = Vec::new();
    if toggles {
        w.doc_slot_mut(slot, |d| {
            let was = super::dom::checked(d, n, "checked");
            if ty == "radio" {
                for r in radio_group(d, n) {
                    let c = super::dom::checked(d, r, "checked");
                    undo.push((r, c));
                    d.set_flags(r, NodeFlags::CHECKED_DIRTY, true);
                    d.set_flags(r, NodeFlags::CHECKED, r == n);
                }
            } else {
                undo.push((n, was));
                d.set_flags(n, NodeFlags::CHECKED_DIRTY, true);
                d.set_flags(n, NodeFlags::CHECKED, !was);
            }
        });
    }
    let ok = dispatch(js, w, w.node_ref(slot, n), ev);
    if toggles {
        if !ok {
            w.doc_slot_mut(slot, |d| {
                for (r, c) in &undo {
                    d.set_flags(*r, NodeFlags::CHECKED, *c);
                }
            });
        } else {
            let changed = w.doc_slot(slot, |d| {
                undo.iter()
                    .any(|(r, c)| *r == n && super::dom::checked(d, *r, "checked") != *c)
            });
            if changed {
                fire_simple(js, w, slot, n, "input", trusted);
                fire_simple(js, w, slot, n, "change", trusted);
            }
        }
    }
    if ok && slot == 0 {
        default_action(js, w, n, &local_n, &ty, trusted);
    }
    w.after_mutation(js);
}

fn fire_simple(js: &mut dyn Js, w: &Rc<Window>, slot: u32, n: NodeId, type_: &str, trusted: bool) {
    let ev = new_event(w, "Event", type_, true, false, EventExtra::None);
    w.with_obj(ev, |o| {
        if let Obj::Event(e) = o {
            e.trusted = trusted;
        }
    });
    dispatch(js, w, w.node_ref(slot, n), ev);
}

fn default_action(
    js: &mut dyn Js,
    w: &Rc<Window>,
    n: NodeId,
    local_n: &str,
    ty: &str,
    trusted: bool,
) {
    // Links.
    let link = w.with_doc(|d| {
        std::iter::once(n)
            .chain(d.ancestors(n))
            .find(|&a| {
                (d.is_html_element_named(a, &local_name!("a"))
                    || d.is_html_element_named(a, &local_name!("area")))
                    && d.has_attribute(a, "href")
            })
            .and_then(|a| d.get_attribute(a, "href").map(str::to_owned))
    });
    if let Some(href) = link {
        if let Some(u) = w.resolve_url(&href) {
            super::window::navigate_to(js, w, u, false);
        }
        return;
    }
    let form_of = |w: &Window, n: NodeId| {
        w.with_doc(|d| {
            if let Some(id) = d.get_attribute(n, "form") {
                return d
                    .get_element_by_id(id)
                    .filter(|&f| d.is_html_element_named(f, &local_name!("form")));
            }
            d.ancestors(n)
                .find(|&a| d.is_html_element_named(a, &local_name!("form")))
        })
    };
    let button_type = w.with_doc(|d| {
        d.get_attribute(n, "type")
            .map(|t| t.to_ascii_lowercase())
            .unwrap_or_else(|| "submit".into())
    });
    let is_submit = (local_n == "button" && button_type == "submit")
        || (local_n == "input" && matches!(ty, "submit" | "image"));
    let is_reset =
        (local_n == "button" && button_type == "reset") || (local_n == "input" && ty == "reset");
    if is_submit {
        if let Some(f) = form_of(w, n) {
            submit(js, w, f, Some(n), trusted);
        }
        return;
    }
    if is_reset {
        if let Some(f) = form_of(w, n) {
            reset_form(w, f);
        }
        return;
    }
    // Label activation.
    let label = w.with_doc(|d| {
        let mut inner_interactive = false;
        for a in std::iter::once(n).chain(d.ancestors(n)) {
            if d.is_html_element_named(a, &local_name!("label")) {
                return (!inner_interactive).then_some(a);
            }
            if interactive(d, a) {
                inner_interactive = true;
            }
        }
        None
    });
    if let Some(l) = label
        && let Some(c) = w.with_doc(|d| label_control(d, l))
        && c != n
    {
        if w.with_doc(|d| focusable(d, c)) && w.focused.get() != Some(c) {
            super::window::focus(js, w, Some(c));
        }
        let ev = new_event(w, "PointerEvent", "click", true, true, EventExtra::None);
        w.with_obj(ev, |o| {
            if let Obj::Event(e) = o {
                e.composed = true;
                e.trusted = trusted;
            }
        });
        activate(js, w, 0, c, ev);
    }
}

/// Submit `form` (from `submitter`, or implicitly): a `submit` event, then
/// the navigation unless it was canceled.
fn submit(js: &mut dyn Js, w: &Rc<Window>, form: NodeId, submitter: Option<NodeId>, trusted: bool) {
    let novalidate = w.with_doc(|d| d.has_attribute(form, "novalidate"));
    if !novalidate && !form_valid(w, form) {
        return;
    }
    let ev = new_event(w, "SubmitEvent", "submit", true, true, EventExtra::None);
    w.with_obj(ev, |o| {
        if let Obj::Event(e) = o {
            e.trusted = trusted;
        }
    });
    if dispatch(js, w, w.node_ref(0, form), ev) {
        super::form::submit_form(w, form, submitter, trusted);
    }
}

/// `required` controls must have a value (other constraints are not
/// checked).
fn form_valid(w: &Window, form: NodeId) -> bool {
    let controls: Vec<NodeId> = w.with_doc(|d| {
        d.descendants(form)
            .filter(|&c| d.has_attribute(c, "required") && !disabled(d, c))
            .collect()
    });
    controls.into_iter().all(|c| {
        let (l, ty) = w.with_doc(|d| (local(d, c), super::dom::input_type(d, c)));
        match (l.as_str(), ty.as_str()) {
            ("input", "checkbox" | "radio") => w.with_doc(|d| super::dom::checked(d, c, "checked")),
            ("input" | "textarea", _) => !control_value(w, 0, c).is_empty(),
            _ => true,
        }
    })
}

fn reset_form(w: &Window, form: NodeId) {
    let ids: Vec<NodeId> = w.with_doc(|d| d.descendants(form).collect());
    let mut vals = w.values.borrow_mut();
    let mut sels = w.selections.borrow_mut();
    for i in &ids {
        vals.remove(&(0, *i));
        sels.remove(&(0, *i));
    }
    drop((vals, sels));
    w.with_doc_mut(|d| {
        for i in ids {
            d.set_flags(i, NodeFlags::CHECKED_DIRTY, false);
        }
    });
}

/// `element.click()`: an untrusted click with the activation behavior.
pub(crate) fn synthetic_click(js: &mut dyn Js, w: &Rc<Window>, slot: u32, n: NodeId) {
    let ev = new_event(w, "PointerEvent", "click", true, true, EventExtra::None);
    w.with_obj(ev, |o| {
        if let Obj::Event(e) = o {
            e.composed = true;
            e.trusted = false;
        }
    });
    activate(js, w, slot, n, ev);
}

// ---------------------------------------------------------------------------
// Focus hooks
// ---------------------------------------------------------------------------

/// Before focus leaves `old`: `change` if a text control's value changed
/// since it got focus.
pub(crate) fn before_blur(js: &mut dyn Js, w: &Rc<Window>, old: NodeId) {
    let fv = w.input.borrow_mut().focus_value.take();
    if let Some((n, v)) = fv
        && n == old
        && connected(w, n)
        && control_value(w, 0, n) != v
    {
        fire_simple(js, w, 0, n, "change", true);
    }
}

/// After `n` got focus: remember a text control's value.
pub(crate) fn after_focus(w: &Window, n: NodeId) {
    let is_text = w.with_doc(|d| text_control(d, n));
    w.input.borrow_mut().focus_value = is_text.then(|| (n, control_value(w, 0, n)));
}

// ---------------------------------------------------------------------------
// Keyboard and text
// ---------------------------------------------------------------------------

fn key_target(w: &Window) -> HostRef {
    let f = w.focused.get().filter(|&f| connected(w, f));
    match f {
        Some(n) => w.node_ref(0, n),
        None => w.with_doc(|d| {
            d.body()
                .or_else(|| d.document_element())
                .map_or(w.node_ref(0, Document::DOCUMENT), |b| w.node_ref(0, b))
        }),
    }
}

fn key_extra(ev: &KeyInput, key_code: u32, char_code: u32) -> EventExtra {
    EventExtra::Keyboard(Box::new(KeyData {
        key: ev.key.clone(),
        code: ev.code.clone(),
        location: ev.location,
        modifiers: ev.modifiers,
        repeat: ev.auto_repeat,
        composing: false,
        key_code,
        char_code,
    }))
}

/// Dispatch one CDP key event.
pub fn dispatch_key(js: &mut dyn Js, w: &Rc<Window>, ev: &KeyInput) {
    let t = key_target(w);
    let key_code = if ev.key_code != 0 {
        ev.key_code
    } else {
        default_key_code(&ev.key)
    };
    match ev.kind {
        KeyKind::Down | KeyKind::RawDown => {
            if !matches!(
                ev.key.as_str(),
                "Escape" | "Shift" | "Control" | "Alt" | "Meta" | "CapsLock" | "AltGraph"
            ) {
                activate_user(w);
            }
            let ok = fire(
                js,
                w,
                t,
                "KeyboardEvent",
                "keydown",
                true,
                true,
                key_extra(ev, key_code, 0),
            );
            if ok {
                for c in &ev.commands {
                    run_command(js, w, c);
                }
                let text = if ev.kind == KeyKind::Down {
                    ev.text.clone()
                } else {
                    String::new()
                };
                let ctrl = ev.modifiers & (MOD_CTRL | MOD_META) != 0;
                if !text.is_empty() && !(ctrl && text != "\r") {
                    keypress_and_text(js, w, ev, &text);
                } else {
                    key_default(js, w, ev);
                }
            }
        }
        KeyKind::Char => keypress_and_text(js, w, ev, &ev.text.clone()),
        KeyKind::Up => {
            fire(
                js,
                w,
                t,
                "KeyboardEvent",
                "keyup",
                true,
                true,
                key_extra(ev, key_code, 0),
            );
            if ev.key == " " {
                // Space activates buttons and checkboxes on key up.
                if let Some(f) = w.focused.get().filter(|&f| connected(w, f)) {
                    let activates = w.with_doc(|d| match local(d, f).as_str() {
                        "button" => true,
                        "input" => matches!(
                            super::dom::input_type(d, f).as_str(),
                            "checkbox" | "radio" | "submit" | "button" | "reset" | "image"
                        ),
                        _ => false,
                    });
                    if activates {
                        keyboard_click(js, w, f);
                    }
                }
            }
        }
    }
    w.after_mutation(js);
}

fn keyboard_click(js: &mut dyn Js, w: &Rc<Window>, n: NodeId) {
    let ev = new_event(
        w,
        "PointerEvent",
        "click",
        true,
        true,
        EventExtra::Mouse(Box::new(MouseData {
            pointer: Some(PointerData {
                id: -1,
                pointer_type: String::new(),
                ..Default::default()
            }),
            ..Default::default()
        })),
    );
    w.with_obj(ev, |o| {
        if let Obj::Event(e) = o {
            e.composed = true;
        }
    });
    activate(js, w, 0, n, ev);
}

fn keypress_and_text(js: &mut dyn Js, w: &Rc<Window>, ev: &KeyInput, text: &str) {
    let t = key_target(w);
    let code = text.chars().next().map_or(0, |c| c as u32);
    let code = if text == "\r" { 13 } else { code };
    let ok = fire(
        js,
        w,
        t,
        "KeyboardEvent",
        "keypress",
        true,
        true,
        key_extra(ev, code, code),
    );
    if !ok {
        return;
    }
    if text == "\r" || text == "\n" {
        enter(js, w);
    } else if ev.key == " " && !focused_editable(w) {
        // Space on a button or checkbox clicks on key up.
    } else {
        insert_text(js, w, text);
    }
}

fn focused_editable(w: &Window) -> bool {
    w.focused
        .get()
        .is_some_and(|f| w.with_doc(|d| text_control(d, f) || editable_host(d, f).is_some()))
}

fn enter(js: &mut dyn Js, w: &Rc<Window>) {
    let Some(f) = w.focused.get().filter(|&f| connected(w, f)) else {
        return;
    };
    let (l, ty) = w.with_doc(|d| (local(d, f), super::dom::input_type(d, f)));
    match l.as_str() {
        "textarea" => edit_text(js, w, f, "insertLineBreak", Some("\n"), "\n"),
        "input"
            if TEXT_TYPES.contains(&ty.as_str())
                || matches!(ty.as_str(), "checkbox" | "radio" | "date" | "time") =>
        {
            implicit_submit(js, w, f)
        }
        "a" | "button" => keyboard_click(js, w, f),
        "input" if matches!(ty.as_str(), "submit" | "button" | "reset" | "image") => {
            keyboard_click(js, w, f)
        }
        _ => {
            if w.with_doc(|d| editable_host(d, f)).is_some() {
                edit_editable(js, w, f, "insertParagraph", None, Some("\n"));
            }
        }
    }
}

/// Enter in a text field: click the form's default button, or submit a
/// form that has no other blocking fields.
fn implicit_submit(js: &mut dyn Js, w: &Rc<Window>, field: NodeId) {
    let form = w.with_doc(|d| {
        d.ancestors(field)
            .find(|&a| d.is_html_element_named(a, &local_name!("form")))
    });
    let Some(form) = form else { return };
    let default_button = w.with_doc(|d| {
        d.descendants(form).find(|&c| {
            let l = local(d, c);
            (l == "button"
                && d.get_attribute(c, "type")
                    .is_none_or(|t| t.eq_ignore_ascii_case("submit")))
                || (l == "input"
                    && matches!(super::dom::input_type(d, c).as_str(), "submit" | "image"))
        })
    });
    match default_button {
        Some(b) => {
            if !w.with_doc(|d| disabled(d, b)) {
                keyboard_click(js, w, b);
            }
        }
        None => {
            let fields = w.with_doc(|d| {
                d.descendants(form)
                    .filter(|&c| {
                        local(d, c) == "input"
                            && TEXT_TYPES.contains(&super::dom::input_type(d, c).as_str())
                    })
                    .count()
            });
            if fields <= 1 {
                submit(js, w, form, None, true);
            }
        }
    }
}

/// Non-text keys: deletion, caret movement, Tab, select all.
/// The shortcut modifier of the persona's OS: Meta (Cmd) on macOS, Ctrl
/// elsewhere.
pub(crate) fn primary_modifier(w: &Window) -> u32 {
    let mac = w
        .profile
        .as_deref()
        .is_some_and(|p| p.os_kind() == lb_persona::OsKind::MacOs);
    if mac { MOD_META } else { MOD_CTRL }
}

fn key_default(js: &mut dyn Js, w: &Rc<Window>, ev: &KeyInput) {
    let shift = ev.modifiers & MOD_SHIFT != 0;
    let f = w.focused.get().filter(|&f| connected(w, f));
    match ev.key.as_str() {
        "Tab" => {
            let next = w.with_doc(|d| {
                let order = tab_order(d);
                if order.is_empty() {
                    return None;
                }
                let pos = f.and_then(|f| order.iter().position(|&x| x == f));
                Some(match (pos, shift) {
                    (None, false) => order[0],
                    (None, true) => order[order.len() - 1],
                    (Some(i), false) => order[(i + 1) % order.len()],
                    (Some(i), true) => order[(i + order.len() - 1) % order.len()],
                })
            });
            if let Some(n) = next {
                super::window::focus(js, w, Some(n));
                if w.with_doc(|d| has_selection(d, n)) {
                    let len = utf16(&control_value(w, 0, n)).len();
                    set_selection(w, 0, n, 0, len, 0);
                }
            }
        }
        // Select all: Cmd+A on macOS, Ctrl+A elsewhere.
        "a" | "A" if ev.modifiers & primary_modifier(w) != 0 => run_command(js, w, "selectAll"),
        "Backspace" | "Delete" => {
            let Some(f) = f else { return };
            let back = ev.key == "Backspace";
            if w.with_doc(|d| text_control(d, f)) {
                let value = control_value(w, 0, f);
                let s = selection(w, 0, f);
                let (start, end) = if s.start != s.end {
                    (s.start, s.end)
                } else if back {
                    (s.start.saturating_sub(1), s.start)
                } else {
                    (s.start, (s.start + 1).min(utf16(&value).len()))
                };
                if start == end {
                    return;
                }
                let kind = if back {
                    "deleteContentBackward"
                } else {
                    "deleteContentForward"
                };
                edit_range(js, w, f, kind, None, start, end, "");
            } else if w.with_doc(|d| editable_host(d, f)).is_some() {
                let kind = if back {
                    "deleteContentBackward"
                } else {
                    "deleteContentForward"
                };
                edit_editable(js, w, f, kind, None, None);
            }
        }
        "ArrowLeft" | "ArrowRight" | "Home" | "End" => {
            let Some(f) = f else { return };
            if !w.with_doc(|d| has_selection(d, f)) {
                return;
            }
            let len = utf16(&control_value(w, 0, f)).len();
            let s = selection(w, 0, f);
            let caret = match ev.key.as_str() {
                "ArrowLeft" if s.start != s.end && !shift => s.start,
                "ArrowLeft" => s.start.saturating_sub(1),
                "ArrowRight" if s.start != s.end && !shift => s.end,
                "ArrowRight" => (s.end + 1).min(len),
                "Home" => 0,
                _ => len,
            };
            if shift {
                let (a, b) = if caret < s.start {
                    (caret, s.end)
                } else {
                    (s.start, caret)
                };
                set_selection(w, 0, f, a, b, 0);
            } else {
                set_selection(w, 0, f, caret, caret, 0);
            }
        }
        _ => {}
    }
}

/// Editing commands (`Input.dispatchKeyEvent` `commands`).
fn run_command(_js: &mut dyn Js, w: &Rc<Window>, cmd: &str) {
    let Some(f) = w.focused.get().filter(|&f| connected(w, f)) else {
        return;
    };
    if cmd.eq_ignore_ascii_case("selectAll") {
        if w.with_doc(|d| text_control(d, f)) {
            let len = utf16(&control_value(w, 0, f)).len();
            set_selection(w, 0, f, 0, len, 0);
        } else if let Some(h) = w.with_doc(|d| editable_host(d, f)) {
            w.input.borrow_mut().select_all = Some(h);
        }
    }
}

/// `Input.insertText` and typed characters: insert into the focused text
/// control or contenteditable, replacing the selection.
pub fn insert_text(js: &mut dyn Js, w: &Rc<Window>, text: &str) {
    let Some(f) = w.focused.get().filter(|&f| connected(w, f)) else {
        return;
    };
    if w.with_doc(|d| text_control(d, f)) {
        let ro = w.with_doc(|d| d.has_attribute(f, "readonly") || disabled(d, f));
        if !ro {
            let text = if w.with_doc(|d| d.is_html_element_named(f, &local_name!("textarea"))) {
                text.replace("\r\n", "\n").replace('\r', "\n")
            } else {
                text.replace(['\r', '\n'], "")
            };
            edit_text(js, w, f, "insertText", Some(&text), &text);
        }
    } else if w.with_doc(|d| editable_host(d, f)).is_some() {
        edit_editable(js, w, f, "insertText", Some(text), Some(text));
    }
    w.after_mutation(js);
}

/// Replace the selection of text control `n` with `text`.
fn edit_text(
    js: &mut dyn Js,
    w: &Rc<Window>,
    n: NodeId,
    kind: &str,
    data: Option<&str>,
    text: &str,
) {
    let s = selection(w, 0, n);
    edit_range(js, w, n, kind, data, s.start, s.end, text);
}

#[allow(clippy::too_many_arguments)]
fn edit_range(
    js: &mut dyn Js,
    w: &Rc<Window>,
    n: NodeId,
    kind: &str,
    data: Option<&str>,
    start: usize,
    end: usize,
    text: &str,
) {
    let input_extra = || EventExtra::Input {
        data: data.map(str::to_owned),
        input_type: kind.to_owned(),
    };
    if !fire(
        js,
        w,
        w.node_ref(0, n),
        "InputEvent",
        "beforeinput",
        true,
        true,
        input_extra(),
    ) {
        return;
    }
    let value = control_value(w, 0, n);
    let max = w.with_doc(|d| {
        d.get_attribute(n, "maxlength")
            .and_then(|m| m.trim().parse::<usize>().ok())
    });
    let mut text = text.to_string();
    if let Some(m) = max {
        let room = m.saturating_sub(utf16(&value).len() - (end - start));
        if utf16(&text).len() > room {
            text = String::from_utf16_lossy(&utf16(&text)[..room]);
        }
    }
    let new = splice(&value, start, end, &text);
    w.values.borrow_mut().insert((0, n), new);
    let caret = start + utf16(&text).len();
    set_selection(w, 0, n, caret, caret, 0);
    fire(
        js,
        w,
        w.node_ref(0, n),
        "InputEvent",
        "input",
        true,
        false,
        input_extra(),
    );
}

/// Edit a contenteditable host: insert at the end (or replace everything
/// when all of it is selected), or delete one character at the end.
fn edit_editable(
    js: &mut dyn Js,
    w: &Rc<Window>,
    f: NodeId,
    kind: &str,
    data: Option<&str>,
    text: Option<&str>,
) {
    let Some(host) = w.with_doc(|d| editable_host(d, f)) else {
        return;
    };
    let input_extra = || EventExtra::Input {
        data: data.map(str::to_owned),
        input_type: kind.to_owned(),
    };
    if !fire(
        js,
        w,
        w.node_ref(0, host),
        "InputEvent",
        "beforeinput",
        true,
        true,
        input_extra(),
    ) {
        return;
    }
    let all = w.input.borrow_mut().select_all.take() == Some(host);
    w.with_doc_mut(|d| {
        if all {
            d.set_text_content(host, None);
        }
        match text {
            Some(t) => {
                let last = d
                    .last_child(host)
                    .filter(|&c| d.node_type(c) == NodeType::Text);
                match last {
                    Some(c) => {
                        let v = format!("{}{t}", d.character_data(c).unwrap_or(""));
                        d.set_character_data(c, &v);
                    }
                    None => {
                        let c = d.create_text(t);
                        let _ = d.append_child(host, c);
                    }
                }
            }
            None if !all => {
                let last = d
                    .descendants(host)
                    .filter(|&c| d.node_type(c) == NodeType::Text)
                    .last();
                if let Some(c) = last {
                    let mut v = d.character_data(c).unwrap_or("").to_string();
                    v.pop();
                    d.set_character_data(c, &v);
                }
            }
            None => {}
        }
    });
    w.after_mutation(js);
    fire(
        js,
        w,
        w.node_ref(0, host),
        "InputEvent",
        "input",
        true,
        false,
        input_extra(),
    );
}

fn default_key_code(key: &str) -> u32 {
    match key {
        "Backspace" => 8,
        "Tab" => 9,
        "Enter" => 13,
        "Shift" => 16,
        "Control" => 17,
        "Alt" => 18,
        "Escape" => 27,
        " " => 32,
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "Delete" => 46,
        k if k.chars().count() == 1 => {
            let c = k.chars().next().unwrap_or(' ');
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase() as u32
            } else {
                0
            }
        }
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Bindings: UI event interfaces, selection API, labels
// ---------------------------------------------------------------------------

fn this_event_data(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<EventData, HostError> {
    let id = match cx.this_host() {
        Some(h) if h.kind == K_OBJ => h.id,
        _ => return Err(super::illegal()),
    };
    w.with_obj(id, |o| match o {
        Obj::Event(e) => Some((**e).clone()),
        _ => None,
    })
    .flatten()
    .ok_or_else(super::illegal)
}

fn mouse_of(e: &EventData) -> MouseData {
    match &e.extra {
        EventExtra::Mouse(m) => (**m).clone(),
        _ => MouseData::default(),
    }
}

fn key_of(e: &EventData) -> KeyData {
    match &e.extra {
        EventExtra::Keyboard(k) => (**k).clone(),
        _ => KeyData::default(),
    }
}

fn modifier_state(mods: u32, key: &str) -> bool {
    match key {
        "Alt" => mods & MOD_ALT != 0,
        "Control" => mods & MOD_CTRL != 0,
        "Meta" => mods & MOD_META != 0,
        "Shift" => mods & MOD_SHIFT != 0,
        _ => false,
    }
}

/// Modifier bits from an init dictionary.
fn dict_mods(cx: &mut CallCtx<'_, '_, '_>, d: &Dict) -> Result<u32, HostError> {
    let mut m = 0;
    for (k, bit) in [
        ("altKey", MOD_ALT),
        ("ctrlKey", MOD_CTRL),
        ("metaKey", MOD_META),
        ("shiftKey", MOD_SHIFT),
    ] {
        if d.bool(cx, k)? {
            m |= bit;
        }
    }
    Ok(m)
}

fn dict_num(cx: &mut CallCtx<'_, '_, '_>, d: &Dict, k: &str) -> Result<f64, HostError> {
    Ok(d.number(cx, k)?.unwrap_or(0.0))
}

fn dict_host(
    cx: &mut CallCtx<'_, '_, '_>,
    d: &Dict,
    k: &str,
) -> Result<Option<HostRef>, HostError> {
    let h = d.get(cx, k)?;
    Ok(h.and_then(|h| {
        let r = cx.handle_host(h);
        cx.release(h);
        r
    }))
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    // Constructors with their init dictionaries.
    for iface in [
        "UIEvent",
        "MouseEvent",
        "PointerEvent",
        "WheelEvent",
        "KeyboardEvent",
        "InputEvent",
        "FocusEvent",
    ] {
        ctor(reg, rt, iface, move |cx, w| {
            if cx.is_empty() {
                return Err(HostError::type_error(format!(
                    "Failed to construct '{iface}': 1 argument required, but only 0 present."
                )));
            }
            let type_ = cx.arg_string(0)?;
            let d = Dict::arg(cx, 1);
            let mut e = EventData::new(
                iface,
                &type_,
                d.bool(cx, "bubbles")?,
                d.bool(cx, "cancelable")?,
            );
            e.composed = d.bool(cx, "composed")?;
            let detail = d.number(cx, "detail")?.unwrap_or(0.0) as i32;
            e.extra = match iface {
                "MouseEvent" | "PointerEvent" | "WheelEvent" => {
                    let mut m = MouseData {
                        screen_x: dict_num(cx, &d, "screenX")?,
                        screen_y: dict_num(cx, &d, "screenY")?,
                        client_x: dict_num(cx, &d, "clientX")?,
                        client_y: dict_num(cx, &d, "clientY")?,
                        movement_x: dict_num(cx, &d, "movementX")?,
                        movement_y: dict_num(cx, &d, "movementY")?,
                        button: dict_num(cx, &d, "button")? as i16,
                        buttons: dict_num(cx, &d, "buttons")? as u16,
                        modifiers: dict_mods(cx, &d)?,
                        related: dict_host(cx, &d, "relatedTarget")?,
                        detail,
                        pointer: None,
                        wheel: None,
                        wheel_delta: [0.0, 0.0],
                    };
                    if iface == "PointerEvent" {
                        m.pointer = Some(PointerData {
                            id: dict_num(cx, &d, "pointerId")? as i32,
                            width: d.number(cx, "width")?.unwrap_or(1.0),
                            height: d.number(cx, "height")?.unwrap_or(1.0),
                            pressure: dict_num(cx, &d, "pressure")?,
                            pointer_type: d.string(cx, "pointerType")?.unwrap_or_default(),
                            primary: d.bool(cx, "isPrimary")?,
                        });
                    }
                    if iface == "WheelEvent" {
                        let dx = dict_num(cx, &d, "deltaX")?;
                        let dy = dict_num(cx, &d, "deltaY")?;
                        m.wheel = Some([
                            dx,
                            dy,
                            dict_num(cx, &d, "deltaZ")?,
                            dict_num(cx, &d, "deltaMode")?,
                        ]);
                        // Blink: the init's wheelDeltaX/Y, else -delta.
                        let wx = dict_num(cx, &d, "wheelDeltaX")?.trunc();
                        let wy = dict_num(cx, &d, "wheelDeltaY")?.trunc();
                        m.wheel_delta = [
                            if wx != 0.0 { wx } else { -dx },
                            if wy != 0.0 { wy } else { -dy },
                        ];
                    }
                    EventExtra::Mouse(Box::new(m))
                }
                "KeyboardEvent" => EventExtra::Keyboard(Box::new(KeyData {
                    key: d.string(cx, "key")?.unwrap_or_default(),
                    code: d.string(cx, "code")?.unwrap_or_default(),
                    location: dict_num(cx, &d, "location")? as u32,
                    modifiers: dict_mods(cx, &d)?,
                    repeat: d.bool(cx, "repeat")?,
                    composing: d.bool(cx, "isComposing")?,
                    key_code: dict_num(cx, &d, "keyCode")? as u32,
                    char_code: dict_num(cx, &d, "charCode")? as u32,
                })),
                "InputEvent" => EventExtra::Input {
                    data: d.string(cx, "data")?,
                    input_type: d.string(cx, "inputType")?.unwrap_or_default(),
                },
                "FocusEvent" => EventExtra::Focus {
                    related: dict_host(cx, &d, "relatedTarget")?,
                },
                _ => EventExtra::Ui { detail },
            };
            d.release(cx);
            e.time = super::events::coarse_now(w);
            let id = w.new_obj(Obj::Event(Box::new(e)));
            cx.init_this(HostRef::new(K_OBJ, id))
        });
    }

    // UIEvent
    getter(reg, rt, "UIEvent", "detail", |cx, w| {
        let e = this_event_data(cx, w)?;
        cx.ret(match &e.extra {
            EventExtra::Mouse(m) => m.detail,
            EventExtra::Ui { detail } => *detail,
            _ => 0,
        })
    });
    getter(reg, rt, "UIEvent", "view", |cx, w| {
        let e = this_event_data(cx, w)?;
        if e.trusted || !matches!(e.extra, EventExtra::None) {
            cx.ret(w.host_arg(WINDOW_REF))
        } else {
            cx.ret(Arg::Null)
        }
    });
    getter(reg, rt, "UIEvent", "which", |cx, w| {
        let e = this_event_data(cx, w)?;
        cx.ret(match &e.extra {
            EventExtra::Keyboard(k) => k.key_code.max(k.char_code),
            EventExtra::Mouse(m) => (m.button + 1).max(0) as u32,
            _ => 0,
        })
    });
    getter(reg, rt, "UIEvent", "sourceCapabilities", |cx, _w| {
        cx.ret(Arg::Null)
    });

    // MouseEvent
    macro_rules! mouse_num {
        ($name:literal, |$w:ident, $e:ident, $m:ident| $body:expr) => {
            getter(reg, rt, "MouseEvent", $name, |cx, $w| {
                let $e = this_event_data(cx, $w)?;
                let $m = mouse_of(&$e);
                cx.ret(Arg::Number($body))
            });
        };
    }
    mouse_num!("screenX", |w, e, m| m.screen_x);
    mouse_num!("screenY", |w, e, m| m.screen_y);
    mouse_num!("clientX", |w, e, m| m.client_x);
    mouse_num!("clientY", |w, e, m| m.client_y);
    mouse_num!("x", |w, e, m| m.client_x);
    mouse_num!("y", |w, e, m| m.client_y);
    mouse_num!("movementX", |w, e, m| m.movement_x);
    mouse_num!("movementY", |w, e, m| m.movement_y);
    mouse_num!("button", |w, e, m| f64::from(
        m.button.max(if e.type_ == "pointermove" { -1 } else { 0 })
    ));
    mouse_num!("buttons", |w, e, m| f64::from(m.buttons));
    mouse_num!("pageX", |w, e, m| m.client_x
        + f64::from(w.with_layout(0, |l, d| l.window_scroll(d).0)));
    mouse_num!("pageY", |w, e, m| m.client_y
        + f64::from(w.with_layout(0, |l, d| l.window_scroll(d).1)));
    mouse_num!("layerX", |w, e, m| m.client_x
        + f64::from(w.with_layout(0, |l, d| l.window_scroll(d).0)));
    mouse_num!("layerY", |w, e, m| m.client_y
        + f64::from(w.with_layout(0, |l, d| l.window_scroll(d).1)));
    mouse_num!("offsetX", |w, e, m| m.client_x - target_origin(w, &e).0);
    mouse_num!("offsetY", |w, e, m| m.client_y - target_origin(w, &e).1);
    macro_rules! mods {
        ($iface:literal, $name:literal, $bit:expr, $of:ident) => {
            getter(reg, rt, $iface, $name, |cx, w| {
                let e = this_event_data(cx, w)?;
                cx.ret($of(&e).modifiers & $bit != 0)
            });
        };
    }
    mods!("MouseEvent", "altKey", MOD_ALT, mouse_of);
    mods!("MouseEvent", "ctrlKey", MOD_CTRL, mouse_of);
    mods!("MouseEvent", "metaKey", MOD_META, mouse_of);
    mods!("MouseEvent", "shiftKey", MOD_SHIFT, mouse_of);
    mods!("KeyboardEvent", "altKey", MOD_ALT, key_of);
    mods!("KeyboardEvent", "ctrlKey", MOD_CTRL, key_of);
    mods!("KeyboardEvent", "metaKey", MOD_META, key_of);
    mods!("KeyboardEvent", "shiftKey", MOD_SHIFT, key_of);
    getter(reg, rt, "MouseEvent", "relatedTarget", |cx, w| {
        let e = this_event_data(cx, w)?;
        cx.ret(super::events::related_of(&e).map_or(Arg::Null, |r| w.host_arg(r)))
    });
    getter(reg, rt, "MouseEvent", "fromElement", |cx, w| {
        let e = this_event_data(cx, w)?;
        let r = if matches!(e.type_.as_str(), "mouseover" | "mouseenter") {
            super::events::related_of(&e)
        } else {
            e.target
        };
        cx.ret(r.map_or(Arg::Null, |r| w.host_arg(r)))
    });
    getter(reg, rt, "MouseEvent", "toElement", |cx, w| {
        let e = this_event_data(cx, w)?;
        let r = if matches!(e.type_.as_str(), "mouseout" | "mouseleave") {
            super::events::related_of(&e)
        } else {
            e.target
        };
        cx.ret(r.map_or(Arg::Null, |r| w.host_arg(r)))
    });
    method(reg, rt, "MouseEvent", "getModifierState", |cx, w| {
        let e = this_event_data(cx, w)?;
        let k = cx.arg_string(0)?;
        cx.ret(modifier_state(mouse_of(&e).modifiers, &k))
    });
    method(reg, rt, "KeyboardEvent", "getModifierState", |cx, w| {
        let e = this_event_data(cx, w)?;
        let k = cx.arg_string(0)?;
        cx.ret(modifier_state(key_of(&e).modifiers, &k))
    });

    // PointerEvent
    macro_rules! pointer {
        ($name:literal, |$p:ident| $body:expr) => {
            getter(reg, rt, "PointerEvent", $name, |cx, w| {
                let e = this_event_data(cx, w)?;
                let $p = mouse_of(&e).pointer.unwrap_or_default();
                cx.ret($body)
            });
        };
    }
    pointer!("pointerId", |p| p.id);
    pointer!("width", |p| p.width);
    pointer!("height", |p| p.height);
    pointer!("pressure", |p| p.pressure);
    pointer!("tangentialPressure", |p| 0.0 * p.pressure);
    pointer!("tiltX", |p| {
        let _ = &p;
        0i32
    });
    pointer!("tiltY", |p| {
        let _ = &p;
        0i32
    });
    pointer!("twist", |p| {
        let _ = &p;
        0i32
    });
    pointer!("altitudeAngle", |p| std::f64::consts::FRAC_PI_2
        + 0.0 * p.pressure);
    pointer!("azimuthAngle", |p| 0.0 * p.pressure);
    pointer!("pointerType", |p| p.pointer_type);
    pointer!("isPrimary", |p| p.primary);
    pointer!("persistentDeviceId", |p| if p.pointer_type == "mouse" {
        1
    } else {
        0
    });
    method(reg, rt, "PointerEvent", "getCoalescedEvents", |cx, _w| {
        cx.ret(Arg::Array(Vec::new()))
    });
    method(reg, rt, "PointerEvent", "getPredictedEvents", |cx, _w| {
        cx.ret(Arg::Array(Vec::new()))
    });

    // WheelEvent
    for (i, name) in ["deltaX", "deltaY", "deltaZ", "deltaMode"]
        .into_iter()
        .enumerate()
    {
        getter(reg, rt, "WheelEvent", name, move |cx, w| {
            let e = this_event_data(cx, w)?;
            cx.ret(mouse_of(&e).wheel.map_or(0.0, |d| d[i]))
        });
    }
    // `long` attributes: truncated toward zero like Blink's int cast.
    getter(reg, rt, "WheelEvent", "wheelDeltaX", |cx, w| {
        let e = this_event_data(cx, w)?;
        cx.ret(mouse_of(&e).wheel_delta[0].trunc())
    });
    getter(reg, rt, "WheelEvent", "wheelDeltaY", |cx, w| {
        let e = this_event_data(cx, w)?;
        cx.ret(mouse_of(&e).wheel_delta[1].trunc())
    });
    getter(reg, rt, "WheelEvent", "wheelDelta", |cx, w| {
        let e = this_event_data(cx, w)?;
        cx.ret(mouse_of(&e).wheel_delta[1].trunc())
    });

    // KeyboardEvent
    getter(reg, rt, "KeyboardEvent", "key", |cx, w| {
        let e = this_event_data(cx, w)?;
        cx.ret(key_of(&e).key)
    });
    getter(reg, rt, "KeyboardEvent", "code", |cx, w| {
        let e = this_event_data(cx, w)?;
        cx.ret(key_of(&e).code)
    });
    getter(reg, rt, "KeyboardEvent", "location", |cx, w| {
        let e = this_event_data(cx, w)?;
        cx.ret(key_of(&e).location)
    });
    getter(reg, rt, "KeyboardEvent", "repeat", |cx, w| {
        let e = this_event_data(cx, w)?;
        cx.ret(key_of(&e).repeat)
    });
    getter(reg, rt, "KeyboardEvent", "isComposing", |cx, w| {
        let e = this_event_data(cx, w)?;
        cx.ret(key_of(&e).composing)
    });
    getter(reg, rt, "KeyboardEvent", "keyCode", |cx, w| {
        let e = this_event_data(cx, w)?;
        cx.ret(key_of(&e).key_code)
    });
    getter(reg, rt, "KeyboardEvent", "charCode", |cx, w| {
        let e = this_event_data(cx, w)?;
        cx.ret(key_of(&e).char_code)
    });

    // InputEvent
    getter(reg, rt, "InputEvent", "data", |cx, w| {
        let e = this_event_data(cx, w)?;
        cx.ret(match e.extra {
            EventExtra::Input { data: Some(d), .. } => Arg::String(d),
            _ => Arg::Null,
        })
    });
    getter(reg, rt, "InputEvent", "inputType", |cx, w| {
        let e = this_event_data(cx, w)?;
        cx.ret(match e.extra {
            EventExtra::Input { input_type, .. } => input_type,
            _ => String::new(),
        })
    });
    getter(reg, rt, "InputEvent", "isComposing", |cx, _w| cx.ret(false));
    getter(reg, rt, "InputEvent", "dataTransfer", |cx, _w| {
        cx.ret(Arg::Null)
    });
    method(reg, rt, "InputEvent", "getTargetRanges", |cx, _w| {
        cx.ret(Arg::Array(Vec::new()))
    });

    // FocusEvent
    getter(reg, rt, "FocusEvent", "relatedTarget", |cx, w| {
        let e = this_event_data(cx, w)?;
        cx.ret(super::events::related_of(&e).map_or(Arg::Null, |r| w.host_arg(r)))
    });

    register_controls(reg, rt);
}

/// Top-left of the event target's padding box (for `offsetX/Y`).
fn target_origin(w: &Window, e: &EventData) -> (f64, f64) {
    let Some((slot, n)) = e.target.and_then(|h| w.decode_node(h)) else {
        return (0.0, 0.0);
    };
    w.with_layout(slot, |l, d| {
        l.box_model(d, n).map_or((0.0, 0.0), |b| {
            (f64::from(b.padding.x), f64::from(b.padding.y))
        })
    })
}

fn this_control(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, NodeId), HostError> {
    let (slot, n) = this_node(cx, w)?;
    if w.doc_slot(slot, |d| is_element(d, n)) {
        Ok((slot, n))
    } else {
        Err(super::illegal())
    }
}

fn no_selection_error(w: &Window, slot: u32, n: NodeId, op: &str) -> HostError {
    let ty = w.doc_slot(slot, |d| super::dom::input_type(d, n));
    w.rt.err(
        "InvalidStateError",
        format!(
            "Failed to {op} on 'HTMLInputElement': The input element's type ('{ty}') does not support selection."
        ),
    )
}

fn register_controls(reg: &mut Registry, rt: &Runtime) {
    for iface in ["HTMLInputElement", "HTMLTextAreaElement"] {
        method(reg, rt, iface, "select", |cx, w| {
            let (slot, n) = this_control(cx, w)?;
            let len = utf16(&control_value(w, slot, n)).len();
            set_selection(w, slot, n, 0, len, 0);
            if slot == 0 && w.with_doc(|d| editable_host(d, n)).is_none() {
                // Chrome fires `select` asynchronously; queue it.
                let w2 = w.clone();
                w.queue_task(
                    super::TaskSource::UserInteraction,
                    Box::new(move |js, _| {
                        super::events::fire_event(
                            js,
                            &w2,
                            w2.node_ref(0, n),
                            "select",
                            true,
                            false,
                        );
                    }),
                );
            }
            Ok(())
        });
        getter(reg, rt, iface, "selectionStart", |cx, w| {
            let (slot, n) = this_control(cx, w)?;
            if !w.doc_slot(slot, |d| has_selection(d, n)) {
                return cx.ret(Arg::Null);
            }
            cx.ret(selection(w, slot, n).start as u32)
        });
        getter(reg, rt, iface, "selectionEnd", |cx, w| {
            let (slot, n) = this_control(cx, w)?;
            if !w.doc_slot(slot, |d| has_selection(d, n)) {
                return cx.ret(Arg::Null);
            }
            cx.ret(selection(w, slot, n).end as u32)
        });
        getter(reg, rt, iface, "selectionDirection", |cx, w| {
            let (slot, n) = this_control(cx, w)?;
            if !w.doc_slot(slot, |d| has_selection(d, n)) {
                return cx.ret(Arg::Null);
            }
            cx.ret(match selection(w, slot, n).dir {
                1 => "forward",
                2 => "backward",
                _ => "none",
            })
        });
        setter(reg, rt, iface, "selectionStart", |cx, w| {
            let (slot, n) = this_control(cx, w)?;
            if !w.doc_slot(slot, |d| has_selection(d, n)) {
                return Err(no_selection_error(
                    w,
                    slot,
                    n,
                    "set the 'selectionStart' property",
                ));
            }
            let v = cx.arg_u32(0)? as usize;
            let s = selection(w, slot, n);
            set_selection(w, slot, n, v, s.end.max(v), s.dir);
            Ok(())
        });
        setter(reg, rt, iface, "selectionEnd", |cx, w| {
            let (slot, n) = this_control(cx, w)?;
            if !w.doc_slot(slot, |d| has_selection(d, n)) {
                return Err(no_selection_error(
                    w,
                    slot,
                    n,
                    "set the 'selectionEnd' property",
                ));
            }
            let v = cx.arg_u32(0)? as usize;
            let s = selection(w, slot, n);
            set_selection(w, slot, n, s.start.min(v), v, s.dir);
            Ok(())
        });
        method(reg, rt, iface, "setSelectionRange", |cx, w| {
            let (slot, n) = this_control(cx, w)?;
            if !w.doc_slot(slot, |d| has_selection(d, n)) {
                return Err(no_selection_error(
                    w,
                    slot,
                    n,
                    "execute 'setSelectionRange'",
                ));
            }
            let a = cx.arg_u32(0)? as usize;
            let b = cx.arg_u32(1)? as usize;
            let dir = match super::opt_string(cx, 2)?.as_deref() {
                Some("forward") => 1,
                Some("backward") => 2,
                _ => 0,
            };
            set_selection(w, slot, n, a, b, dir);
            Ok(())
        });
        method(reg, rt, iface, "setRangeText", |cx, w| {
            let (slot, n) = this_control(cx, w)?;
            let text = cx.arg_string(0)?;
            let s = selection(w, slot, n);
            let (a, b) = if cx.len() >= 3 {
                (cx.arg_u32(1)? as usize, cx.arg_u32(2)? as usize)
            } else {
                (s.start, s.end)
            };
            let value = control_value(w, slot, n);
            let new = splice(&value, a, b.max(a), &text);
            w.values.borrow_mut().insert((slot, n), new);
            let end = a + utf16(&text).len();
            set_selection(w, slot, n, a, end, 0);
            Ok(())
        });
        getter(reg, rt, iface, "labels", |cx, w| {
            let (slot, n) = this_control(cx, w)?;
            let labels: Vec<Arg> = w
                .doc_slot(slot, |d| {
                    d.descendants(Document::DOCUMENT)
                        .filter(|&l| {
                            d.is_html_element_named(l, &local_name!("label"))
                                && label_control(d, l) == Some(n)
                        })
                        .collect::<Vec<_>>()
                })
                .into_iter()
                .map(|l| w.node_arg(slot, l))
                .collect();
            let arr = cx.make_handle(Arg::Array(labels))?;
            cx.ret(Arg::Handle(arr))
        });
    }
    for iface in [
        "HTMLSelectElement",
        "HTMLButtonElement",
        "HTMLMeterElement",
        "HTMLOutputElement",
        "HTMLProgressElement",
    ] {
        getter(reg, rt, iface, "labels", |cx, w| {
            let (slot, n) = this_control(cx, w)?;
            let labels: Vec<Arg> = w
                .doc_slot(slot, |d| {
                    d.descendants(Document::DOCUMENT)
                        .filter(|&l| {
                            d.is_html_element_named(l, &local_name!("label"))
                                && label_control(d, l) == Some(n)
                        })
                        .collect::<Vec<_>>()
                })
                .into_iter()
                .map(|l| w.node_arg(slot, l))
                .collect();
            let arr = cx.make_handle(Arg::Array(labels))?;
            cx.ret(Arg::Handle(arr))
        });
    }
    getter(reg, rt, "HTMLLabelElement", "control", |cx, w| {
        let (slot, n) = this_control(cx, w)?;
        let c = w.doc_slot(slot, |d| label_control(d, n));
        cx.ret(c.map_or(Arg::Null, |c| w.node_arg(slot, c)))
    });
    getter(reg, rt, "HTMLLabelElement", "form", |cx, w| {
        let (slot, n) = this_control(cx, w)?;
        let f = w.doc_slot(slot, |d| {
            label_control(d, n).and_then(|c| {
                d.ancestors(c)
                    .find(|&a| d.is_html_element_named(a, &local_name!("form")))
            })
        });
        cx.ret(f.map_or(Arg::Null, |f| w.node_arg(slot, f)))
    });
    getter(reg, rt, "HTMLElement", "isContentEditable", |cx, w| {
        let (slot, n) = this_control(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| editable_host(d, n).is_some()))
    });
    getter(reg, rt, "HTMLElement", "contentEditable", |cx, w| {
        let (slot, n) = this_control(cx, w)?;
        let v = w.doc_slot(slot, |d| {
            d.get_attribute(n, "contenteditable")
                .map(|v| v.to_ascii_lowercase())
        });
        cx.ret(match v.as_deref() {
            None => "inherit",
            Some("" | "true") => "true",
            Some("false") => "false",
            Some("plaintext-only") => "plaintext-only",
            Some(_) => "inherit",
        })
    });
    setter(reg, rt, "HTMLElement", "contentEditable", |cx, w| {
        let (slot, n) = this_control(cx, w)?;
        let v = cx.arg_string(0)?.to_ascii_lowercase();
        w.doc_slot_mut(slot, |d| match v.as_str() {
            "inherit" => {
                d.remove_attribute(n, "contenteditable");
                Ok(())
            }
            "true" | "false" | "plaintext-only" => {
                let _ = d.set_attribute(n, "contenteditable", &v);
                Ok(())
            }
            _ => Err(()),
        })
        .map_err(|_| {
            w.rt.err(
                "SyntaxError",
                format!(
                    "Failed to set the 'contentEditable' property on 'HTMLElement': The value provided ('{v}') is not one of 'true', 'false', 'plaintext-only', or 'inherit'."
                ),
            )
        })?;
        w.after_mutation(cx);
        Ok(())
    });
    method(reg, rt, "Element", "checkVisibility", |cx, w| {
        let (slot, n) = this_control(cx, w)?;
        let d = Dict::arg(cx, 0);
        let opacity = d.bool(cx, "checkOpacity")? || d.bool(cx, "opacityProperty")?;
        let vis = d.bool(cx, "checkVisibilityCSS")? || d.bool(cx, "visibilityProperty")?;
        d.release(cx);
        let visible = w.with_layout(slot, |l, doc| {
            if !doc.is_connected(n) || l.box_model(doc, n).is_none() {
                return false;
            }
            let chain: Vec<NodeId> = std::iter::once(n)
                .chain(doc.ancestors(n))
                .filter(|&a| is_element(doc, a))
                .collect();
            if chain
                .iter()
                .any(|&a| l.resolved_style_value(doc, a, "display") == "none")
            {
                return false;
            }
            if opacity
                && chain
                    .iter()
                    .any(|&a| l.resolved_style_value(doc, a, "opacity").trim() == "0")
            {
                return false;
            }
            if vis && l.resolved_style_value(doc, n, "visibility") != "visible" {
                return false;
            }
            true
        });
        cx.ret(visible)
    });
    getter(reg, rt, "HTMLOptionElement", "label", |cx, w| {
        let (slot, n) = this_control(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| match d.get_attribute(n, "label") {
            Some(l) if !l.is_empty() => l.to_owned(),
            _ => super::dom::option_text(d, n),
        }))
    });
    setter(reg, rt, "HTMLOptionElement", "label", |cx, w| {
        let (slot, n) = this_control(cx, w)?;
        let v = cx.arg_string(0)?;
        let _ = w.doc_slot_mut(slot, |d| d.set_attribute(n, "label", &v));
        w.after_mutation(cx);
        Ok(())
    });
    getter(reg, rt, "HTMLSelectElement", "selectedOptions", |cx, w| {
        let (slot, n) = this_control(cx, w)?;
        let sel: Vec<Arg> = w
            .doc_slot(slot, |d| {
                let opts = super::dom::select_options(d, n);
                if d.has_attribute(n, "multiple") {
                    opts.into_iter()
                        .filter(|&o| super::dom::checked(d, o, "selected"))
                        .collect::<Vec<_>>()
                } else {
                    let i = super::dom::selected_index(d, n);
                    opts.get(usize::try_from(i).unwrap_or(usize::MAX))
                        .copied()
                        .into_iter()
                        .collect()
                }
            })
            .into_iter()
            .map(|o| w.node_arg(slot, o))
            .collect();
        let arr = cx.make_handle(Arg::Array(sel))?;
        cx.ret(Arg::Handle(arr))
    });
    let _ = ValueKind::Undefined;
}

/// Request a navigation (used by the page layer for keyboard/link tests).
#[allow(dead_code)]
pub(crate) fn request(w: &Window, url: url::Url) {
    w.request_navigation(NavRequest::get(url, false));
}
