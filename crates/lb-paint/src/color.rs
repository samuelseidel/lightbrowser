//! Canvas colors: CSS `<color>` parsing (through lb-css) and the canvas
//! serialization Chrome uses for `fillStyle` / `strokeStyle` /
//! `shadowColor`.

use lb_css::{Color, ColorValue, Decl, DeclarationBlock, LonghandId, Sv};

/// Parses a CSS `<color>` (named, hex, `rgb[a]()`, `hsl[a]()`, system
/// colors, `transparent`). `currentcolor` resolves to black (the canvas
/// element's default `color`). `None` if invalid.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    // Reject anything that could escape the declaration we build.
    if s.is_empty() || s.contains([';', '{', '}', '!']) {
        return None;
    }
    let block = DeclarationBlock::parse(&format!("color: {s}"));
    if block.decls.len() != 1 {
        return None;
    }
    match &block.decls[0] {
        d if d.id == LonghandId::Color => match &d.value {
            Decl::Value(Sv::Color(ColorValue::Rgba(c))) => Some(*c),
            Decl::Value(Sv::Color(ColorValue::CurrentColor)) => Some(Color::BLACK),
            _ => None,
        },
        _ => None,
    }
}

/// Chrome's canvas color serialization: `#rrggbb` when opaque, otherwise
/// `rgba(r, g, b, a)`.
pub fn serialize_color(c: Color) -> String {
    if c.a >= 1.0 {
        format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
    } else {
        let a = (c.a * 1000.0).round() / 1000.0;
        format!("rgba({}, {}, {}, {})", c.r, c.g, c.b, lb_css::fmt_num(a))
    }
}

/// A tiny-skia color with `alpha` multiplied in.
pub(crate) fn to_skia(c: Color, alpha: f32) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(
        c.r,
        c.g,
        c.b,
        (c.a * alpha * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_serialize() {
        assert_eq!(serialize_color(parse_color("#f60").unwrap()), "#ff6600");
        assert_eq!(serialize_color(parse_color("red").unwrap()), "#ff0000");
        assert_eq!(
            serialize_color(parse_color("rgba(102, 204, 0, 0.7)").unwrap()),
            "rgba(102, 204, 0, 0.7)"
        );
        assert_eq!(
            serialize_color(parse_color("hsl(120, 100%, 50%)").unwrap()),
            "#00ff00"
        );
        assert_eq!(
            serialize_color(parse_color("transparent").unwrap()),
            "rgba(0, 0, 0, 0)"
        );
        assert!(parse_color("nope").is_none());
        assert!(parse_color("red; background: blue").is_none());
        assert!(parse_color("inherit").is_none());
        assert!(parse_color("").is_none());
    }
}
