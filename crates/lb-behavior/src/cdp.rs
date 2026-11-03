//! Conversion of the [`InputEvent`] stream into CDP input commands.
//!
//! The generators stamp each event relative to the start of its own sequence
//! and put the pauses between sequences in [`InputEvent::Wait`]. This module
//! folds both into one absolute timeline (`offset_ms` from the start of the
//! stream) and maps each event to the parameters of the CDP command that
//! injects it:
//!
//! | Source event | CDP command | `type` |
//! |---|---|---|
//! | `Mouse(Move)` | `Input.dispatchMouseEvent` | `mouseMoved` |
//! | `Mouse(Down)` | `Input.dispatchMouseEvent` | `mousePressed` |
//! | `Mouse(Up)` | `Input.dispatchMouseEvent` | `mouseReleased` |
//! | `Mouse(Click)` | none | the browser derives `click` from press + release |
//! | `Scroll` | `Input.dispatchMouseEvent` | `mouseWheel` |
//! | `Keyboard(Down)` | `Input.dispatchKeyEvent` | `keyDown` (printable) or `rawKeyDown` |
//! | `Keyboard(Press)` | none | the `keyDown` carries `text`, which makes the browser fire `keypress` |
//! | `Keyboard(Up)` | `Input.dispatchKeyEvent` | `keyUp` |
//! | `Wait` | none | folded into later offsets |
//!
//! The parameter structs serialise exactly as CDP expects (camelCase, unset
//! optionals omitted), so `serde_json::to_value(&cmd.params)` can go straight
//! into a CDP message. Key data uses a US QWERTY layout.
//!
//! Shift: the keyboard generator emits `Shift` key events around shifted
//! characters; the conversion tracks whether Shift is held and sets the
//! Shift modifier bit on every key event while it is (and on shifted
//! characters in streams without Shift events). The wheel position is the
//! last mouse position seen in the stream.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::InputEvent;
use crate::keyboard::{KeyEventType, KeystrokeEvent};
use crate::mouse::{Button, MouseEvent, MouseEventType};
use crate::scroll::ScrollEvent;

/// CDP modifier bit for Shift (Alt = 1, Ctrl = 2, Meta = 4, Shift = 8).
pub const MODIFIER_SHIFT: u32 = 8;

/// `type` of an `Input.dispatchMouseEvent` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MouseEventKind {
    MousePressed,
    MouseReleased,
    MouseMoved,
    MouseWheel,
}

/// `button` of an `Input.dispatchMouseEvent` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MouseButton {
    None,
    Left,
    Middle,
    Right,
}

/// Parameters of `Input.dispatchMouseEvent`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchMouseEvent {
    #[serde(rename = "type")]
    pub kind: MouseEventKind,
    pub x: f64,
    pub y: f64,
    /// Modifier bitmask (Alt 1, Ctrl 2, Meta 4, Shift 8).
    pub modifiers: u32,
    pub button: MouseButton,
    /// Bitmask of buttons held while the event fires (left = 1).
    pub buttons: u32,
    /// 1 for press and release, 0 otherwise.
    pub click_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_y: Option<f64>,
}

/// `type` of an `Input.dispatchKeyEvent` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeyEventKind {
    KeyDown,
    RawKeyDown,
    Char,
    KeyUp,
}

/// Parameters of `Input.dispatchKeyEvent`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchKeyEvent {
    #[serde(rename = "type")]
    pub kind: KeyEventKind,
    /// Modifier bitmask (Alt 1, Ctrl 2, Meta 4, Shift 8).
    pub modifiers: u32,
    /// Text the key produces; set on `keyDown`/`char` for printable keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Text the key would produce without modifiers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unmodified_text: Option<String>,
    /// DOM `KeyboardEvent.key`.
    pub key: String,
    /// DOM `KeyboardEvent.code`; empty when the key has no physical code.
    pub code: String,
    pub windows_virtual_key_code: u32,
    pub native_virtual_key_code: u32,
    /// DOM `KeyboardEvent.location` (1 = left modifier key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<u32>,
}

/// Parameters of `Input.insertText`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InsertText {
    pub text: String,
}

/// One CDP input command with its parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum CdpInputCommand {
    #[serde(rename = "Input.dispatchMouseEvent")]
    DispatchMouseEvent(DispatchMouseEvent),
    #[serde(rename = "Input.dispatchKeyEvent")]
    DispatchKeyEvent(DispatchKeyEvent),
    #[serde(rename = "Input.insertText")]
    InsertText(InsertText),
}

impl CdpInputCommand {
    /// The CDP method name.
    pub fn method(&self) -> &'static str {
        match self {
            Self::DispatchMouseEvent(_) => "Input.dispatchMouseEvent",
            Self::DispatchKeyEvent(_) => "Input.dispatchKeyEvent",
            Self::InsertText(_) => "Input.insertText",
        }
    }

    /// The CDP `params` object, ready to put in a protocol message.
    pub fn params(&self) -> Value {
        // The param structs hold only strings, numbers and enums, so
        // serialisation cannot fail.
        match self {
            Self::DispatchMouseEvent(p) => serde_json::to_value(p),
            Self::DispatchKeyEvent(p) => serde_json::to_value(p),
            Self::InsertText(p) => serde_json::to_value(p),
        }
        .unwrap_or(Value::Null)
    }
}

/// A CDP command scheduled `offset_ms` after the start of the stream.
///
/// Serialises as `{"offset_ms": .., "method": "..", "params": {..}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CdpInputEvent {
    /// Milliseconds since the start of the stream. Non-decreasing across a
    /// converted stream.
    pub offset_ms: u64,
    #[serde(flatten)]
    pub command: CdpInputCommand,
}

impl CdpInputEvent {
    /// A standalone `Input.insertText` command, for text the key map cannot
    /// type (for example IME input).
    pub fn insert_text(offset_ms: u64, text: impl Into<String>) -> Self {
        Self {
            offset_ms,
            command: CdpInputCommand::InsertText(InsertText { text: text.into() }),
        }
    }
}

/// Settings for [`events_to_cdp_with`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CdpOptions {
    /// Pointer position used for wheel events that come before any mouse
    /// event in the stream.
    pub initial_pointer: (f64, f64),
}

impl Default for CdpOptions {
    fn default() -> Self {
        Self {
            initial_pointer: (0.0, 0.0),
        }
    }
}

/// Total length of a converted stream in milliseconds.
pub fn duration_ms(events: &[CdpInputEvent]) -> u64 {
    events.last().map_or(0, |e| e.offset_ms)
}

/// Convert with [`CdpOptions::default`].
pub fn events_to_cdp(events: &[InputEvent]) -> Vec<CdpInputEvent> {
    events_to_cdp_with(events, &CdpOptions::default())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Modality {
    Mouse,
    Keyboard,
    Scroll,
}

/// Convert an event stream to CDP commands on one absolute timeline.
///
/// A new generator sequence starts (and is shifted to the end of everything
/// before it) after a `Wait`, when the modality changes, or when a raw
/// timestamp goes backwards. The output is sorted by `offset_ms` and keeps the
/// source order for equal offsets.
pub fn events_to_cdp_with(events: &[InputEvent], opts: &CdpOptions) -> Vec<CdpInputEvent> {
    let mut out = Vec::with_capacity(events.len());
    // End of everything emitted or waited so far.
    let mut clock: u64 = 0;
    // Offset added to raw timestamps of the current sequence.
    let mut base: u64 = 0;
    let mut current: Option<(Modality, u64)> = None;
    let mut pointer = opts.initial_pointer;
    let mut shift = false;

    for event in events {
        let (modality, raw_ts) = match event {
            InputEvent::Wait(w) => {
                clock = clock.saturating_add(w.duration_ms);
                current = None;
                continue;
            }
            InputEvent::Mouse(m) => (Modality::Mouse, m.timestamp_ms),
            InputEvent::Keyboard(k) => (Modality::Keyboard, k.timestamp_ms),
            InputEvent::Scroll(s) => (Modality::Scroll, s.timestamp_ms),
        };
        let continues = matches!(current, Some((m, last)) if m == modality && raw_ts >= last);
        if !continues {
            base = clock;
        }
        current = Some((modality, raw_ts));
        let offset_ms = base.saturating_add(raw_ts);
        clock = clock.max(offset_ms);

        let command = match event {
            InputEvent::Mouse(m) => {
                pointer = (m.x, m.y);
                mouse_command(m)
            }
            InputEvent::Keyboard(k) => key_command(k, &mut shift),
            InputEvent::Scroll(s) => Some(wheel_command(s, pointer)),
            InputEvent::Wait(_) => None,
        };
        if let Some(command) = command {
            out.push(CdpInputEvent { offset_ms, command });
        }
    }
    out
}

fn mouse_command(m: &MouseEvent) -> Option<CdpInputCommand> {
    let pressed = match m.button {
        Button::Left => MouseButton::Left,
        Button::Middle => MouseButton::Middle,
        Button::Right => MouseButton::Right,
    };
    let (kind, button, click_count) = match m.event_type {
        MouseEventType::Move => (
            MouseEventKind::MouseMoved,
            if m.buttons & 1 != 0 {
                MouseButton::Left
            } else if m.buttons & 2 != 0 {
                MouseButton::Right
            } else if m.buttons & 4 != 0 {
                MouseButton::Middle
            } else {
                MouseButton::None
            },
            0,
        ),
        MouseEventType::Down => (MouseEventKind::MousePressed, pressed, m.click_count.max(1)),
        MouseEventType::Up => (MouseEventKind::MouseReleased, pressed, m.click_count.max(1)),
        // The browser synthesises `click` from the press and release.
        MouseEventType::Click => return None,
    };
    Some(CdpInputCommand::DispatchMouseEvent(DispatchMouseEvent {
        kind,
        x: m.x,
        y: m.y,
        modifiers: 0,
        button,
        buttons: u32::from(m.buttons),
        click_count,
        delta_x: None,
        delta_y: None,
    }))
}

fn wheel_command(s: &ScrollEvent, pointer: (f64, f64)) -> CdpInputCommand {
    CdpInputCommand::DispatchMouseEvent(DispatchMouseEvent {
        kind: MouseEventKind::MouseWheel,
        x: pointer.0,
        y: pointer.1,
        modifiers: 0,
        button: MouseButton::None,
        buttons: 0,
        click_count: 0,
        delta_x: Some(s.delta_x),
        delta_y: Some(s.delta_y),
    })
}

fn key_command(k: &KeystrokeEvent, shift: &mut bool) -> Option<CdpInputCommand> {
    let info = key_info(&k.key);
    if k.key == "Shift" {
        match k.event_type {
            KeyEventType::Down => *shift = true,
            KeyEventType::Up => *shift = false,
            KeyEventType::Press => {}
        }
    }
    let kind = match k.event_type {
        // `keyDown` with `text` also fires keypress and inserts the text, so
        // the separate Press event needs no command of its own.
        KeyEventType::Press => return None,
        KeyEventType::Down if info.text.is_some() => KeyEventKind::KeyDown,
        KeyEventType::Down => KeyEventKind::RawKeyDown,
        KeyEventType::Up => KeyEventKind::KeyUp,
    };
    let with_text = kind == KeyEventKind::KeyDown;
    let shifted = *shift || (info.shift && k.event_type == KeyEventType::Down);
    Some(CdpInputCommand::DispatchKeyEvent(DispatchKeyEvent {
        kind,
        modifiers: if shifted { MODIFIER_SHIFT } else { 0 },
        text: if with_text { info.text.clone() } else { None },
        unmodified_text: if with_text {
            info.unmodified.clone()
        } else {
            None
        },
        key: info.key,
        code: info.code,
        windows_virtual_key_code: info.vk,
        native_virtual_key_code: info.vk,
        location: info.location,
    }))
}

/// Key data for one source key name.
struct KeyInfo {
    key: String,
    code: String,
    vk: u32,
    text: Option<String>,
    unmodified: Option<String>,
    shift: bool,
    location: Option<u32>,
}

/// Map a source key (a character or a name such as `Backspace`) to CDP key
/// data on a US QWERTY layout. Unknown characters keep their text with an
/// empty `code` and virtual key 0.
fn key_info(key: &str) -> KeyInfo {
    let named = |code: &str, vk: u32, text: Option<&str>| KeyInfo {
        key: key.to_string(),
        code: code.to_string(),
        vk,
        text: text.map(str::to_string),
        unmodified: text.map(str::to_string),
        shift: false,
        location: code
            .ends_with("Left")
            .then_some(1)
            .filter(|_| matches!(key, "Shift" | "Control" | "Alt" | "Meta")),
    };
    match key {
        "Enter" => return named("Enter", 13, Some("\r")),
        "Tab" => return named("Tab", 9, None),
        "Backspace" => return named("Backspace", 8, None),
        "Delete" => return named("Delete", 46, None),
        "Escape" => return named("Escape", 27, None),
        "Shift" => return named("ShiftLeft", 16, None),
        "Control" => return named("ControlLeft", 17, None),
        "Alt" => return named("AltLeft", 18, None),
        "Meta" => return named("MetaLeft", 91, None),
        "ArrowLeft" => return named("ArrowLeft", 37, None),
        "ArrowUp" => return named("ArrowUp", 38, None),
        "ArrowRight" => return named("ArrowRight", 39, None),
        "ArrowDown" => return named("ArrowDown", 40, None),
        "Home" => return named("Home", 36, None),
        "End" => return named("End", 35, None),
        "PageUp" => return named("PageUp", 33, None),
        "PageDown" => return named("PageDown", 34, None),
        _ => {}
    }
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        // Unknown multi-character name: pass through without text.
        return named("", 0, None);
    };
    let (code, vk, base, shift) = char_key(c);
    KeyInfo {
        key: key.to_string(),
        code,
        vk,
        text: Some(key.to_string()),
        unmodified: Some(base.to_string()),
        shift,
        location: None,
    }
}

/// `(code, virtual key, unshifted char, needs shift)` for a printable char.
fn char_key(c: char) -> (String, u32, char, bool) {
    match c {
        'a'..='z' => (
            format!("Key{}", c.to_ascii_uppercase()),
            c.to_ascii_uppercase() as u32,
            c,
            false,
        ),
        'A'..='Z' => (format!("Key{c}"), c as u32, c.to_ascii_lowercase(), true),
        '0'..='9' => (format!("Digit{c}"), c as u32, c, false),
        ' ' => ("Space".into(), 32, ' ', false),
        _ => {
            // US layout punctuation: (unshifted, shifted, code, vk).
            const PUNCT: &[(char, char, &str, u32)] = &[
                ('`', '~', "Backquote", 192),
                ('-', '_', "Minus", 189),
                ('=', '+', "Equal", 187),
                ('[', '{', "BracketLeft", 219),
                (']', '}', "BracketRight", 221),
                ('\\', '|', "Backslash", 220),
                (';', ':', "Semicolon", 186),
                ('\'', '"', "Quote", 222),
                (',', '<', "Comma", 188),
                ('.', '>', "Period", 190),
                ('/', '?', "Slash", 191),
            ];
            const SHIFTED_DIGITS: &str = ")!@#$%^&*(";
            if let Some(&(lo, _, code, vk)) = PUNCT.iter().find(|p| p.0 == c || p.1 == c) {
                return (code.into(), vk, lo, lo != c);
            }
            if let Some(d) = SHIFTED_DIGITS.chars().position(|s| s == c) {
                let digit = char::from(b'0' + d as u8);
                return (format!("Digit{digit}"), digit as u32, digit, true);
            }
            (String::new(), 0, c, false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BehaviorProfile, Point, ScrollDirection, WaitEvent, human_click_events,
        human_scroll_events, human_type_events,
    };

    fn session() -> Vec<InputEvent> {
        let mut profile = BehaviorProfile::casual_user();
        profile.scroll.direction_change_probability = 0.0;
        let mut events = human_click_events(
            Point::new(10.0, 10.0),
            Point::new(400.0, 300.0),
            &profile,
            7,
        );
        events.extend(human_type_events("Hi, there!\n", &profile, 8));
        events.extend(human_scroll_events(
            ScrollDirection::Down,
            600.0,
            &profile,
            9,
        ));
        events
    }

    fn mouse_kinds(out: &[CdpInputEvent]) -> Vec<MouseEventKind> {
        out.iter()
            .filter_map(|e| match &e.command {
                CdpInputCommand::DispatchMouseEvent(m) => Some(m.kind),
                _ => None,
            })
            .collect()
    }

    fn key_events(out: &[CdpInputEvent]) -> Vec<&DispatchKeyEvent> {
        out.iter()
            .filter_map(|e| match &e.command {
                CdpInputCommand::DispatchKeyEvent(k) => Some(k),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn offsets_are_monotonic() {
        let out = events_to_cdp(&session());
        assert!(!out.is_empty());
        assert!(out.windows(2).all(|w| w[0].offset_ms <= w[1].offset_ms));
    }

    #[test]
    fn sequences_keep_click_type_scroll_order() {
        let out = events_to_cdp(&session());
        let first_key = out
            .iter()
            .position(|e| matches!(e.command, CdpInputCommand::DispatchKeyEvent(_)))
            .unwrap();
        let first_wheel = out
            .iter()
            .position(|e| matches!(&e.command, CdpInputCommand::DispatchMouseEvent(m) if m.kind == MouseEventKind::MouseWheel))
            .unwrap();
        let released = out
            .iter()
            .position(|e| matches!(&e.command, CdpInputCommand::DispatchMouseEvent(m) if m.kind == MouseEventKind::MouseReleased))
            .unwrap();
        assert!(released < first_key, "click comes before typing");
        assert!(first_key < first_wheel, "typing comes before scroll");
        // Each later sequence starts after the previous one ended.
        let last_key = out
            .iter()
            .rposition(|e| matches!(e.command, CdpInputCommand::DispatchKeyEvent(_)))
            .unwrap();
        assert!(out[first_key].offset_ms >= out[released].offset_ms);
        assert!(out[first_wheel].offset_ms >= out[last_key].offset_ms);
    }

    #[test]
    fn waits_shift_the_following_sequence() {
        let events = vec![
            InputEvent::Wait(WaitEvent { duration_ms: 500 }),
            InputEvent::Scroll(ScrollEvent {
                delta_x: 0.0,
                delta_y: 100.0,
                timestamp_ms: 20,
                scroll_type: crate::ScrollType::Wheel,
            }),
            InputEvent::Wait(WaitEvent { duration_ms: 300 }),
            InputEvent::Scroll(ScrollEvent {
                delta_x: 0.0,
                delta_y: 100.0,
                timestamp_ms: 0,
                scroll_type: crate::ScrollType::Wheel,
            }),
        ];
        let out = events_to_cdp(&events);
        assert_eq!(
            out.iter().map(|e| e.offset_ms).collect::<Vec<_>>(),
            vec![520, 820]
        );
    }

    #[test]
    fn mouse_down_up_pair_and_click_dropped() {
        let out = events_to_cdp(&session());
        let kinds = mouse_kinds(&out);
        let pressed = kinds
            .iter()
            .filter(|k| **k == MouseEventKind::MousePressed)
            .count();
        let released = kinds
            .iter()
            .filter(|k| **k == MouseEventKind::MouseReleased)
            .count();
        assert_eq!(pressed, 1);
        assert_eq!(released, 1);
        let p = kinds
            .iter()
            .position(|k| *k == MouseEventKind::MousePressed)
            .unwrap();
        let r = kinds
            .iter()
            .position(|k| *k == MouseEventKind::MouseReleased)
            .unwrap();
        assert!(p < r);
        assert!(kinds[..p].iter().all(|k| *k == MouseEventKind::MouseMoved));

        let mice: Vec<&DispatchMouseEvent> = out
            .iter()
            .filter_map(|e| match &e.command {
                CdpInputCommand::DispatchMouseEvent(m) => Some(m),
                _ => None,
            })
            .collect();
        let (down, up) = (mice[p], mice[r]);
        assert_eq!((down.x, down.y), (up.x, up.y));
        assert_eq!(
            (down.button, up.button),
            (MouseButton::Left, MouseButton::Left)
        );
        assert_eq!((down.click_count, up.click_count), (1, 1));
        assert_eq!((down.buttons, up.buttons), (1, 0));
        let offsets: Vec<u64> = out.iter().map(|e| e.offset_ms).collect();
        assert!(
            offsets.iter().all(|&o| o > 0),
            "think-time wait precedes the first move"
        );
    }

    #[test]
    fn key_down_up_pair_in_order() {
        let out = events_to_cdp(&session());
        let keys = key_events(&out);
        assert!(!keys.is_empty());
        let mut held: Vec<&str> = Vec::new();
        let mut downs = 0;
        for k in &keys {
            match k.kind {
                KeyEventKind::KeyDown | KeyEventKind::RawKeyDown => {
                    assert!(!held.contains(&k.key.as_str()), "{} pressed twice", k.key);
                    held.push(&k.key);
                    downs += 1;
                }
                KeyEventKind::KeyUp => {
                    let at = held
                        .iter()
                        .position(|h| *h == k.key)
                        .expect("up after down");
                    held.remove(at);
                }
                KeyEventKind::Char => unreachable!(),
            }
        }
        assert!(held.is_empty());
        assert!(downs > 0);
        assert_eq!(
            keys.iter()
                .filter(|k| k.kind == KeyEventKind::KeyUp)
                .count(),
            downs
        );
    }

    #[test]
    fn shift_events_and_modifiers() {
        let mut profile = BehaviorProfile::casual_user();
        profile.keyboard.error_rate = 0.0;
        let out = events_to_cdp(&human_type_events("aB", &profile, 3));
        let keys = key_events(&out);
        let names: Vec<(&str, KeyEventKind, u32)> = keys
            .iter()
            .map(|k| (k.key.as_str(), k.kind, k.modifiers))
            .collect();
        let shift_down = names
            .iter()
            .position(|n| n.0 == "Shift" && n.1 == KeyEventKind::RawKeyDown)
            .expect("Shift down");
        let b_down = names
            .iter()
            .position(|n| n.0 == "B" && n.1 == KeyEventKind::KeyDown)
            .unwrap();
        let shift_up = names
            .iter()
            .position(|n| n.0 == "Shift" && n.1 == KeyEventKind::KeyUp)
            .unwrap();
        assert!(shift_down < b_down && b_down < shift_up);
        assert_eq!(names[shift_down].2, MODIFIER_SHIFT);
        assert_eq!(names[b_down].2, MODIFIER_SHIFT);
        assert_eq!(names[shift_up].2, 0);
        assert_eq!(keys[shift_down].location, Some(1));
        assert_eq!(keys[shift_down].code, "ShiftLeft");
        let a = names.iter().find(|n| n.0 == "a").unwrap();
        assert_eq!(a.2, 0);
    }

    #[test]
    fn typed_text_survives_conversion() {
        let profile = BehaviorProfile::power_user();
        for seed in 0..20 {
            let events = human_type_events("Hi, 5!\n", &profile, seed);
            let out = events_to_cdp(&events);
            let mut result = String::new();
            for k in key_events(&out)
                .iter()
                .filter(|k| matches!(k.kind, KeyEventKind::KeyDown | KeyEventKind::RawKeyDown))
            {
                if k.key == "Backspace" {
                    result.pop();
                } else if let Some(t) = &k.text {
                    result.push_str(t);
                } else {
                    assert_eq!(k.key, "Shift");
                }
            }
            assert_eq!(result, "Hi, 5!\r");
        }
    }

    #[test]
    fn right_double_click() {
        let mut rng = crate::rng::seeded_rng(1);
        let (mice, _) = crate::mouse::generate_click_in(
            crate::Point::new(0.0, 0.0),
            &crate::TargetBox::new(50.0, 50.0, 40.0, 20.0),
            crate::ClickOptions {
                button: Button::Right,
                click_count: 2,
            },
            &BehaviorProfile::casual_user().mouse,
            &mut rng,
        );
        let ev: Vec<InputEvent> = mice.into_iter().map(InputEvent::Mouse).collect();
        let out = events_to_cdp(&ev);
        let presses: Vec<&DispatchMouseEvent> = out
            .iter()
            .filter_map(|e| match &e.command {
                CdpInputCommand::DispatchMouseEvent(m)
                    if m.kind == MouseEventKind::MousePressed =>
                {
                    Some(m)
                }
                _ => None,
            })
            .collect();
        assert_eq!(presses.len(), 2);
        assert_eq!(presses[1].click_count, 2);
        assert!(
            presses
                .iter()
                .all(|p| p.button == MouseButton::Right && p.buttons == 2)
        );
    }

    #[test]
    fn key_mapping() {
        let a = key_info("a");
        assert_eq!((a.code.as_str(), a.vk, a.shift), ("KeyA", 65, false));
        let cap = key_info("A");
        assert_eq!((cap.code.as_str(), cap.vk, cap.shift), ("KeyA", 65, true));
        assert_eq!(cap.unmodified.as_deref(), Some("a"));
        let bang = key_info("!");
        assert_eq!(
            (bang.code.as_str(), bang.vk, bang.shift),
            ("Digit1", 49, true)
        );
        assert_eq!(bang.unmodified.as_deref(), Some("1"));
        let q = key_info("?");
        assert_eq!((q.code.as_str(), q.vk, q.shift), ("Slash", 191, true));
        let sp = key_info(" ");
        assert_eq!((sp.code.as_str(), sp.vk), ("Space", 32));
        let bs = key_info("Backspace");
        assert_eq!((bs.vk, bs.text), (8, None));
        let ent = key_info("Enter");
        assert_eq!((ent.vk, ent.text.as_deref()), (13, Some("\r")));
        let accent = key_info("\u{e9}");
        assert_eq!((accent.code.as_str(), accent.vk), ("", 0));
        assert_eq!(accent.text.as_deref(), Some("\u{e9}"));
    }

    #[test]
    fn wheel_uses_last_pointer_and_keeps_deltas() {
        let out = events_to_cdp(&session());
        let last_move = out
            .iter()
            .rev()
            .find_map(|e| match &e.command {
                CdpInputCommand::DispatchMouseEvent(m) if m.kind != MouseEventKind::MouseWheel => {
                    Some((m.x, m.y))
                }
                _ => None,
            })
            .unwrap();
        let wheels: Vec<&DispatchMouseEvent> = out
            .iter()
            .filter_map(|e| match &e.command {
                CdpInputCommand::DispatchMouseEvent(m) if m.kind == MouseEventKind::MouseWheel => {
                    Some(m)
                }
                _ => None,
            })
            .collect();
        assert!(!wheels.is_empty());
        for w in &wheels {
            assert_eq!((w.x, w.y), last_move);
            assert!(w.delta_y.is_some() && w.delta_x.is_some());
        }
        let total: f64 = wheels.iter().filter_map(|w| w.delta_y).sum();
        assert!((total - 600.0).abs() < 1.0, "total scroll {total}");
    }

    #[test]
    fn json_shape_matches_cdp() {
        let out = events_to_cdp(&session());
        let press = out
            .iter()
            .find(|e| matches!(&e.command, CdpInputCommand::DispatchMouseEvent(m) if m.kind == MouseEventKind::MousePressed))
            .unwrap();
        assert_eq!(press.command.method(), "Input.dispatchMouseEvent");
        let p = press.command.params();
        assert_eq!(p["type"], "mousePressed");
        assert_eq!(p["button"], "left");
        assert_eq!(p["clickCount"], 1);
        assert!(p.get("deltaX").is_none());

        let down = out
            .iter()
            .find(|e| matches!(&e.command, CdpInputCommand::DispatchKeyEvent(k) if k.kind == KeyEventKind::KeyDown))
            .unwrap();
        let p = down.command.params();
        assert_eq!(p["type"], "keyDown");
        assert!(p["windowsVirtualKeyCode"].as_u64().unwrap() > 0);
        assert!(p["text"].is_string());

        let json = serde_json::to_string(&out).unwrap();
        let back: Vec<CdpInputEvent> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, out);
        let v: Value = serde_json::from_str(&json).unwrap();
        assert!(v[0]["offset_ms"].is_u64());
        assert!(v[0]["method"].is_string());
        assert!(v[0]["params"].is_object());
    }

    #[test]
    fn insert_text_command() {
        let c = CdpInputEvent::insert_text(5, "caf\u{e9}");
        assert_eq!(c.command.method(), "Input.insertText");
        assert_eq!(c.command.params()["text"], "caf\u{e9}");
        assert_eq!(duration_ms(&[c]), 5);
    }

    #[test]
    fn empty_stream() {
        assert!(events_to_cdp(&[]).is_empty());
        assert_eq!(duration_ms(&[]), 0);
    }

    #[test]
    fn initial_pointer_used_before_any_mouse_event() {
        let events = vec![InputEvent::Scroll(ScrollEvent {
            delta_x: 0.0,
            delta_y: 50.0,
            timestamp_ms: 0,
            scroll_type: crate::ScrollType::Wheel,
        })];
        let out = events_to_cdp_with(
            &events,
            &CdpOptions {
                initial_pointer: (640.0, 360.0),
            },
        );
        let CdpInputCommand::DispatchMouseEvent(m) = &out[0].command else {
            panic!()
        };
        assert_eq!((m.x, m.y), (640.0, 360.0));
    }
}
