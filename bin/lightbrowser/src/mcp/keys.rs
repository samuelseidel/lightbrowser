//! Key names to `Input.dispatchKeyEvent` parameters, the way Playwright's
//! keyboard sends them (US layout): a printable key is `keyDown` with its
//! `text`, a non-printable one is `rawKeyDown`, then `keyUp`. Combinations
//! (`Control+a`, `Shift+Tab`) press the modifiers first and release them
//! in reverse.

use serde_json::{Value, json};

const ALT: u32 = 1;
const CTRL: u32 = 2;
const META: u32 = 4;
const SHIFT: u32 = 8;

/// One key's description.
#[derive(Debug, Clone, PartialEq)]
struct Key {
    key: String,
    code: &'static str,
    vk: u32,
    text: String,
}

fn named(name: &str) -> Option<Key> {
    let k = |key: &str, code: &'static str, vk: u32, text: &str| {
        Some(Key {
            key: key.into(),
            code,
            vk,
            text: text.into(),
        })
    };
    match name {
        "Enter" | "Return" => k("Enter", "Enter", 13, "\r"),
        "Tab" => k("Tab", "Tab", 9, ""),
        "Backspace" => k("Backspace", "Backspace", 8, ""),
        "Delete" | "Del" => k("Delete", "Delete", 46, ""),
        "Escape" | "Esc" => k("Escape", "Escape", 27, ""),
        "Space" | " " => k(" ", "Space", 32, " "),
        "ArrowLeft" | "Left" => k("ArrowLeft", "ArrowLeft", 37, ""),
        "ArrowUp" | "Up" => k("ArrowUp", "ArrowUp", 38, ""),
        "ArrowRight" | "Right" => k("ArrowRight", "ArrowRight", 39, ""),
        "ArrowDown" | "Down" => k("ArrowDown", "ArrowDown", 40, ""),
        "Home" => k("Home", "Home", 36, ""),
        "End" => k("End", "End", 35, ""),
        "PageUp" => k("PageUp", "PageUp", 33, ""),
        "PageDown" => k("PageDown", "PageDown", 34, ""),
        "Insert" => k("Insert", "Insert", 45, ""),
        "F1" => k("F1", "F1", 112, ""),
        "F2" => k("F2", "F2", 113, ""),
        "F3" => k("F3", "F3", 114, ""),
        "F4" => k("F4", "F4", 115, ""),
        "F5" => k("F5", "F5", 116, ""),
        "F6" => k("F6", "F6", 117, ""),
        "F7" => k("F7", "F7", 118, ""),
        "F8" => k("F8", "F8", 119, ""),
        "F9" => k("F9", "F9", 120, ""),
        "F10" => k("F10", "F10", 121, ""),
        "F11" => k("F11", "F11", 122, ""),
        "F12" => k("F12", "F12", 123, ""),
        _ => None,
    }
}

/// The modifier a name stands for: its bit, `key`, `code`, virtual key.
fn modifier(name: &str) -> Option<(u32, &'static str, &'static str, u32)> {
    Some(match name {
        "Control" | "Ctrl" => (CTRL, "Control", "ControlLeft", 17),
        "Shift" => (SHIFT, "Shift", "ShiftLeft", 16),
        "Alt" | "Option" => (ALT, "Alt", "AltLeft", 18),
        "Meta" | "Command" | "Cmd" => (META, "Meta", "MetaLeft", 91),
        _ => return None,
    })
}

/// A single character on a US keyboard: (code, virtual key).
fn printable(c: char) -> (&'static str, u32) {
    const LETTERS: [&str; 26] = [
        "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK",
        "KeyL", "KeyM", "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV",
        "KeyW", "KeyX", "KeyY", "KeyZ",
    ];
    const DIGITS: [&str; 10] = [
        "Digit0", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8",
        "Digit9",
    ];
    match c {
        'a'..='z' => (
            LETTERS[(c as u8 - b'a') as usize],
            c.to_ascii_uppercase() as u32,
        ),
        'A'..='Z' => (LETTERS[(c as u8 - b'A') as usize], c as u32),
        '0'..='9' => (DIGITS[(c as u8 - b'0') as usize], c as u32),
        ';' => ("Semicolon", 186),
        '=' => ("Equal", 187),
        ',' => ("Comma", 188),
        '-' => ("Minus", 189),
        '.' => ("Period", 190),
        '/' => ("Slash", 191),
        '`' => ("Backquote", 192),
        '[' => ("BracketLeft", 219),
        '\\' => ("Backslash", 220),
        ']' => ("BracketRight", 221),
        '\'' => ("Quote", 222),
        ' ' => ("Space", 32),
        _ => ("", 0),
    }
}

fn single(c: char) -> Key {
    let (code, vk) = printable(c);
    Key {
        key: c.to_string(),
        code,
        vk,
        text: c.to_string(),
    }
}

fn event(kind: &str, k: &Key, modifiers: u32, text: bool) -> Value {
    let mut v = json!({
        "type": kind,
        "modifiers": modifiers,
        "key": k.key,
        "code": k.code,
        "windowsVirtualKeyCode": k.vk,
        "nativeVirtualKeyCode": k.vk,
    });
    if text && !k.text.is_empty() {
        v["text"] = json!(k.text);
        v["unmodifiedText"] = json!(k.text);
    }
    v
}

/// The `Input.dispatchKeyEvent` parameters that press `spec`: a key name
/// (`Enter`, `ArrowDown`, `a`) or a combination (`Control+a`).
pub fn press_events(spec: &str) -> Result<Vec<Value>, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("key must not be empty".into());
    }
    // `+` alone is the plus key; `Control++` presses Control and plus.
    let mut parts: Vec<&str> = spec.split('+').collect();
    if spec.ends_with('+') && spec.len() > 1 {
        parts.pop();
        parts.pop();
        parts.push("+");
    } else if spec == "+" {
        parts = vec!["+"];
    }
    let (main, mods) = parts.split_last().ok_or("key must not be empty")?;
    let mut out = Vec::new();
    let mut bits = 0;
    let mut pressed = Vec::new();
    for m in mods {
        let (bit, key, code, vk) = modifier(m).ok_or_else(|| {
            format!("unknown modifier {m:?} in {spec:?}; use Control, Shift, Alt or Meta")
        })?;
        bits |= bit;
        let k = Key {
            key: key.into(),
            code,
            vk,
            text: String::new(),
        };
        out.push(event("rawKeyDown", &k, bits, false));
        pressed.push((k, bit));
    }
    if let Some((bit, key, code, vk)) = modifier(main) {
        // A lone modifier press.
        let k = Key {
            key: key.into(),
            code,
            vk,
            text: String::new(),
        };
        out.push(event("rawKeyDown", &k, bits | bit, false));
        out.push(event("keyUp", &k, bits, false));
    } else {
        let mut k = match named(main) {
            Some(k) => k,
            None if main.chars().count() == 1 => single(main.chars().next().unwrap_or(' ')),
            None => {
                return Err(format!(
                    "unknown key {main:?}; use a name such as Enter, Tab, ArrowDown, F5 or a single character"
                ));
            }
        };
        // Shift turns a letter into its capital, as the keyboard does.
        if bits & SHIFT != 0 && k.key.len() == 1 {
            k.key = k.key.to_ascii_uppercase();
            k.text = k.key.clone();
        }
        // Control, Alt or Meta make a shortcut, not text (Enter keeps its).
        let text = bits & (CTRL | ALT | META) == 0 || k.key == "Enter";
        let kind = if text && !k.text.is_empty() {
            "keyDown"
        } else {
            "rawKeyDown"
        };
        out.push(event(kind, &k, bits, text));
        out.push(event("keyUp", &k, bits, false));
    }
    for (k, bit) in pressed.into_iter().rev() {
        bits &= !bit;
        out.push(event("keyUp", &k, bits, false));
    }
    Ok(out)
}

/// The events that type one character: a key press for a character on the
/// US layout, `None` for the rest (those go through `Input.insertText`).
pub fn char_events(c: char) -> Option<Vec<Value>> {
    if c == '\n' || c == '\r' {
        return press_events("Enter").ok();
    }
    let (code, _) = printable(c);
    if code.is_empty() {
        return None;
    }
    let k = single(c);
    Some(vec![
        event("keyDown", &k, 0, true),
        event("keyUp", &k, 0, false),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(v: &[Value]) -> Vec<String> {
        v.iter()
            .map(|e| {
                format!(
                    "{}:{}",
                    e["type"].as_str().unwrap(),
                    e["key"].as_str().unwrap()
                )
            })
            .collect()
    }

    #[test]
    fn enter_types_a_carriage_return() {
        let ev = press_events("Enter").unwrap();
        assert_eq!(kinds(&ev), ["keyDown:Enter", "keyUp:Enter"]);
        assert_eq!(ev[0]["text"], "\r");
        assert_eq!(ev[0]["windowsVirtualKeyCode"], 13);
    }

    #[test]
    fn navigation_keys_are_raw() {
        let ev = press_events("ArrowDown").unwrap();
        assert_eq!(kinds(&ev), ["rawKeyDown:ArrowDown", "keyUp:ArrowDown"]);
        assert!(ev[0].get("text").is_none());
    }

    #[test]
    fn letters_carry_text_and_code() {
        let ev = press_events("a").unwrap();
        assert_eq!(ev[0]["text"], "a");
        assert_eq!(ev[0]["code"], "KeyA");
        assert_eq!(ev[0]["windowsVirtualKeyCode"], 65);
    }

    #[test]
    fn combinations_press_modifiers_around_the_key() {
        let ev = press_events("Control+a").unwrap();
        assert_eq!(
            kinds(&ev),
            [
                "rawKeyDown:Control",
                "rawKeyDown:a",
                "keyUp:a",
                "keyUp:Control"
            ]
        );
        assert_eq!(ev[1]["modifiers"], 2);
        assert!(ev[1].get("text").is_none(), "a shortcut is not text");
        assert_eq!(ev[3]["modifiers"], 0);
        let ev = press_events("Shift+a").unwrap();
        assert_eq!(ev[1]["key"], "A");
        assert_eq!(ev[1]["text"], "A");
        assert_eq!(press_events("Shift+Tab").unwrap().len(), 4);
    }

    #[test]
    fn plus_and_bad_names() {
        assert_eq!(press_events("+").unwrap()[0]["key"], "+");
        assert_eq!(press_events("Control++").unwrap()[1]["key"], "+");
        assert!(press_events("").is_err());
        assert!(press_events("Hyper+a").is_err());
        assert!(press_events("Bogus").unwrap_err().contains("unknown key"));
    }

    #[test]
    fn typed_characters() {
        let ev = char_events('x').unwrap();
        assert_eq!(kinds(&ev), ["keyDown:x", "keyUp:x"]);
        assert_eq!(char_events('\n').unwrap()[0]["key"], "Enter");
        assert!(char_events('é').is_none());
        assert!(char_events('!').is_none());
    }
}
