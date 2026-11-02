//! `getComputedStyle` serialization in Chrome's format.

use crate::computed::ComputedStyle;
use crate::props::{LonghandId, normalize_name};
use crate::values::{GridLine, LineHeight, LpAuto, fmt_num, fmt_px};

/// Properties [`computed_value`] can serialize, in Chrome's
/// `CSSStyleDeclaration` order (alphabetical, prefixed last).
pub fn computed_property_names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = LonghandId::ALL.iter().map(|l| l.name()).collect();
    v.sort_unstable();
    v
}

fn lp_auto(v: &LpAuto) -> String {
    v.to_css()
}

fn grid_line(g: &GridLine) -> String {
    g.to_css()
}

/// Serializes one property of a computed style, `None` if unknown.
/// Percentages and `auto` are returned as computed; `lb-layout` replaces
/// box sizes and offsets with used px values where Chrome does.
pub fn computed_value(s: &ComputedStyle, name: &str) -> Option<String> {
    if let Some(custom) = name.strip_prefix("--") {
        let _ = custom;
        return Some(s.custom_property(name).unwrap_or("").to_owned());
    }
    let name = normalize_name(name);
    let quad = |f: &dyn Fn(usize) -> String| -> String {
        let v: Vec<String> = (0..4).map(f).collect();
        if v[1] == v[3] {
            if v[0] == v[2] {
                if v[0] == v[1] {
                    return v[0].clone();
                }
                return format!("{} {}", v[0], v[1]);
            }
            return format!("{} {} {}", v[0], v[1], v[2]);
        }
        v.join(" ")
    };
    Some(match name.as_str() {
        "margin" => quad(&|i| lp_auto(&s.margin[i])),
        "padding" => quad(&|i| s.padding[i].to_css()),
        "border-width" => quad(&|i| fmt_px(s.border_width[i])),
        "border-style" => quad(&|i| s.border_style[i].as_str().to_owned()),
        "border-color" => quad(&|i| s.border_color[i].to_css()),
        "inset" => quad(&|i| lp_auto(&s.inset[i])),
        "overflow" => {
            if s.overflow_x == s.overflow_y {
                s.overflow_x.as_str().to_owned()
            } else {
                format!("{} {}", s.overflow_x.as_str(), s.overflow_y.as_str())
            }
        }
        "gap" => {
            let r = s.row_gap.as_ref().map_or("normal".into(), |l| l.to_css());
            let c = s.column_gap.as_ref().map_or("normal".into(), |l| l.to_css());
            if r == c { r } else { format!("{r} {c}") }
        }
        "flex" => format!(
            "{} {} {}",
            fmt_num(s.flex_grow),
            fmt_num(s.flex_shrink),
            lp_auto(&s.flex_basis)
        ),
        "flex-flow" => format!("{} {}", s.flex_direction.as_str(), s.flex_wrap.as_str()),
        other => {
            let id = LonghandId::from_name(other)?;
            longhand_value(s, id)
        }
    })
}

/// Serializes one longhand.
pub fn longhand_value(s: &ComputedStyle, id: LonghandId) -> String {
    use LonghandId as L;
    match id {
        L::Display => s.display.as_str().into(),
        L::Position => s.position.as_str().into(),
        L::Float => s.float.as_str().into(),
        L::Clear => s.clear.as_str().into(),
        L::Visibility => s.visibility.as_str().into(),
        L::Opacity => fmt_num(s.opacity),
        L::OverflowX => s.overflow_x.as_str().into(),
        L::OverflowY => s.overflow_y.as_str().into(),
        L::Width => lp_auto(&s.width),
        L::Height => lp_auto(&s.height),
        L::MinWidth => lp_auto(&s.min_width),
        L::MinHeight => lp_auto(&s.min_height),
        L::MaxWidth => s.max_width.as_ref().map_or("none".into(), |l| l.to_css()),
        L::MaxHeight => s.max_height.as_ref().map_or("none".into(), |l| l.to_css()),
        L::MarginTop => lp_auto(&s.margin[0]),
        L::MarginRight => lp_auto(&s.margin[1]),
        L::MarginBottom => lp_auto(&s.margin[2]),
        L::MarginLeft => lp_auto(&s.margin[3]),
        L::PaddingTop => s.padding[0].to_css(),
        L::PaddingRight => s.padding[1].to_css(),
        L::PaddingBottom => s.padding[2].to_css(),
        L::PaddingLeft => s.padding[3].to_css(),
        L::BorderTopWidth => fmt_px(s.border_width[0]),
        L::BorderRightWidth => fmt_px(s.border_width[1]),
        L::BorderBottomWidth => fmt_px(s.border_width[2]),
        L::BorderLeftWidth => fmt_px(s.border_width[3]),
        L::BorderTopStyle => s.border_style[0].as_str().into(),
        L::BorderRightStyle => s.border_style[1].as_str().into(),
        L::BorderBottomStyle => s.border_style[2].as_str().into(),
        L::BorderLeftStyle => s.border_style[3].as_str().into(),
        L::BorderTopColor => s.border_color[0].to_css(),
        L::BorderRightColor => s.border_color[1].to_css(),
        L::BorderBottomColor => s.border_color[2].to_css(),
        L::BorderLeftColor => s.border_color[3].to_css(),
        L::Top => lp_auto(&s.inset[0]),
        L::Right => lp_auto(&s.inset[1]),
        L::Bottom => lp_auto(&s.inset[2]),
        L::Left => lp_auto(&s.inset[3]),
        L::BoxSizing => s.box_sizing.as_str().into(),
        L::FlexDirection => s.flex_direction.as_str().into(),
        L::FlexWrap => s.flex_wrap.as_str().into(),
        L::FlexGrow => fmt_num(s.flex_grow),
        L::FlexShrink => fmt_num(s.flex_shrink),
        L::FlexBasis => lp_auto(&s.flex_basis),
        L::Order => s.order.to_string(),
        L::RowGap => s.row_gap.as_ref().map_or("normal".into(), |l| l.to_css()),
        L::ColumnGap => s.column_gap.as_ref().map_or("normal".into(), |l| l.to_css()),
        L::GridTemplateRows | L::GridTemplateColumns => {
            let t = if id == L::GridTemplateRows {
                &s.grid_template_rows
            } else {
                &s.grid_template_columns
            };
            match t {
                None => "none".into(),
                Some(t) => t.iter().map(|e| e.to_css()).collect::<Vec<_>>().join(" "),
            }
        }
        L::GridRowStart => grid_line(&s.grid_row_start),
        L::GridRowEnd => grid_line(&s.grid_row_end),
        L::GridColumnStart => grid_line(&s.grid_column_start),
        L::GridColumnEnd => grid_line(&s.grid_column_end),
        L::AlignItems => s.align_items.as_str().into(),
        L::AlignSelf => s.align_self.as_str().into(),
        L::AlignContent => s.align_content.as_str().into(),
        L::JustifyContent => s.justify_content.as_str().into(),
        L::JustifyItems => s.justify_items.as_str().into(),
        L::JustifySelf => s.justify_self.as_str().into(),
        L::FontFamily => s
            .font_family
            .iter()
            .map(|f| f.to_css())
            .collect::<Vec<_>>()
            .join(", "),
        L::FontSize => fmt_px(s.font_size),
        L::FontWeight => fmt_num(s.font_weight),
        L::FontStyle => s.font_style.as_str().into(),
        L::LineHeight => match s.line_height {
            LineHeight::Normal => "normal".into(),
            LineHeight::Number(n) => fmt_px(n * s.font_size),
            LineHeight::Px(v) => fmt_px(v),
        },
        L::Color => s.color.to_css(),
        L::BackgroundColor => s.background_color.to_css(),
        L::TextAlign => s.text_align.as_str().into(),
        L::WhiteSpace => s.white_space.as_str().into(),
        L::ZIndex => s.z_index.map_or("auto".into(), |z| z.to_string()),
        L::Transform => match &s.transform {
            None => "none".into(),
            Some(t) => match t.matrix {
                Some(m) => format!(
                    "matrix({}, {}, {}, {}, {}, {})",
                    fmt_num(m[0]),
                    fmt_num(m[1]),
                    fmt_num(m[2]),
                    fmt_num(m[3]),
                    fmt_num(m[4]),
                    fmt_num(m[5])
                ),
                None => t.css.to_string(),
            },
        },
        L::PointerEvents => s.pointer_events.as_str().into(),
        L::Cursor => s.cursor.to_string(),
        L::LetterSpacing => s.letter_spacing.map_or("normal".into(), fmt_px),
        L::WordSpacing => fmt_px(s.word_spacing),
        L::Direction => s.direction.as_str().into(),
        L::AspectRatio => s
            .aspect_ratio
            .map_or("auto".into(), |r| format!("{} / 1", fmt_num(r))),
    }
}
