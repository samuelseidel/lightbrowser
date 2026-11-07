//! `Input.*` on a page's worker: CDP parameters to lb-webapi's trusted
//! input pipeline, in the main world's realm.

use std::rc::Rc;

use lb_cdp::types::{KeyEvent, MouseEvent};
use lb_cdp::{HostError, HostResult};
use lb_webapi::rt::EngineJs;
use lb_webapi::rt::input::{self, KeyInput, KeyKind, MouseButton, MouseInput, MouseKind};

use crate::browser::WorkerState;
use crate::page::PageState;

/// Run `f` with the main world as the dispatching realm, then settle.
fn with_main(
    ws: &Rc<WorkerState>,
    p: &Rc<PageState>,
    timestamp: Option<f64>,
    f: impl FnOnce(&mut EngineJs<'_>, &Rc<lb_webapi::rt::Window>),
) -> HostResult<()> {
    let w = p.window().ok_or_else(|| HostError::new("Target closed"))?;
    // Replayed input keeps its timing on a virtual clock.
    if let Some(ts) = timestamp
        && w.clock.is_virtual()
    {
        w.clock.advance_to(ts * 1000.0 - w.clock.time_origin);
    }
    let realm = w.realm();
    ws.with_engine(|engine| {
        let mut js = EngineJs { engine, realm };
        f(&mut js, &w);
    })
    .ok_or_else(|| HostError::new("Target closed"))?;
    super::agent::settle(ws, &w);
    p.notify.notify_one();
    Ok(())
}

pub(super) fn mouse(ws: &Rc<WorkerState>, p: &Rc<PageState>, ev: MouseEvent) -> HostResult<()> {
    let kind = match ev.kind.as_str() {
        "mouseMoved" => MouseKind::Move,
        "mousePressed" => MouseKind::Press,
        "mouseReleased" => MouseKind::Release,
        "mouseWheel" => MouseKind::Wheel,
        other => return Err(HostError::new(format!("Unknown mouse event type: {other}"))),
    };
    let m = MouseInput {
        kind,
        x: ev.x,
        y: ev.y,
        button: MouseButton::parse(ev.button.as_deref().unwrap_or("none")),
        buttons: ev.buttons.and_then(|b| u16::try_from(b).ok()),
        click_count: ev.click_count.unwrap_or(0).max(0) as u32,
        modifiers: ev.modifiers.max(0) as u32,
        delta_x: ev.delta_x.unwrap_or(0.0),
        delta_y: ev.delta_y.unwrap_or(0.0),
        pointer_type: ev.pointer_type.unwrap_or_else(|| "mouse".into()),
    };
    let at = (m.x, m.y);
    with_main(ws, p, ev.timestamp, |js, w| {
        input::dispatch_mouse(js, w, &m)
    })?;
    // `LightBrowser.human*` continues from wherever the pointer is.
    if let Some(a) = p.agent() {
        a.st.borrow_mut().human.pointer = Some(at);
    }
    Ok(())
}

pub(super) fn key(ws: &Rc<WorkerState>, p: &Rc<PageState>, ev: KeyEvent) -> HostResult<()> {
    let kind = match ev.kind.as_str() {
        "keyDown" => KeyKind::Down,
        "rawKeyDown" => KeyKind::RawDown,
        "keyUp" => KeyKind::Up,
        "char" => KeyKind::Char,
        other => return Err(HostError::new(format!("Unknown key event type: {other}"))),
    };
    let k = KeyInput {
        kind,
        key: ev.key.unwrap_or_default(),
        code: ev.code.unwrap_or_default(),
        text: ev.text.unwrap_or_default(),
        key_code: ev.windows_virtual_key_code.unwrap_or(0).max(0) as u32,
        location: ev.location.unwrap_or(0).max(0) as u32,
        modifiers: ev.modifiers.max(0) as u32,
        auto_repeat: ev.auto_repeat,
        commands: ev.commands,
    };
    with_main(ws, p, ev.timestamp, |js, w| input::dispatch_key(js, w, &k))
}

pub(super) fn text(ws: &Rc<WorkerState>, p: &Rc<PageState>, text: &str) -> HostResult<()> {
    with_main(ws, p, None, |js, w| input::insert_text(js, w, text))
}
