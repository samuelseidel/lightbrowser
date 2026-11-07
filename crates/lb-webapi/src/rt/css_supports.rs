//! The `CSS` namespace: `CSS.supports()` (both overloads, decided by
//! lb-css's [`lb_css::supports_declaration`] / [`lb_css::supports_condition`]
//! with Chrome 153's property data) and `CSS.escape()`. `CSS` is exposed
//! on windows only, as in Chrome.

use lb_js::HostError;

use super::{Runtime, method};
use crate::registry::Registry;

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    method(reg, rt, "CSS", "supports", |cx, _w| match cx.len() {
        0 => Err(HostError::type_error(
            "Failed to execute 'supports' on 'CSS': 1 argument required, but only 0 present.",
        )),
        1 => {
            let text = cx.arg_string(0)?;
            cx.ret(lb_css::supports_condition(&text))
        }
        _ => {
            let name = cx.arg_string(0)?;
            let value = cx.arg_string(1)?;
            cx.ret(lb_css::supports_declaration(&name, &value))
        }
    });
    method(reg, rt, "CSS", "escape", |cx, _w| {
        if cx.is_empty() {
            return Err(HostError::type_error(
                "Failed to execute 'escape' on 'CSS': 1 argument required, but only 0 present.",
            ));
        }
        let s = cx.arg_string(0)?;
        cx.ret(escape(&s))
    });
}

/// CSSOM's "serialize an identifier".
fn escape(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        let n = c as u32;
        if n == 0 {
            out.push('\u{FFFD}');
        } else if (1..=0x1f).contains(&n)
            || n == 0x7f
            || (i == 0 && c.is_ascii_digit())
            || (i == 1 && c.is_ascii_digit() && chars[0] == '-')
        {
            out.push_str(&format!("\\{n:x} "));
        } else if i == 0 && c == '-' && chars.len() == 1 {
            out.push_str("\\-");
        } else if n >= 0x80 || c == '-' || c == '_' || c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::escape;

    #[test]
    fn escapes_like_cssom() {
        assert_eq!(escape("a b"), "a\\ b");
        assert_eq!(escape("1a"), "\\31 a");
        assert_eq!(escape("-1"), "-\\31 ");
        assert_eq!(escape("-"), "\\-");
        assert_eq!(escape("a\u{0}"), "a\u{FFFD}");
        assert_eq!(escape("#x.y"), "\\#x\\.y");
        assert_eq!(escape("é_-"), "é_-");
    }
}
