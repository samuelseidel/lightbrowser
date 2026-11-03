//! Canvas text: the `font` attribute, font resolution and glyph outlines.
//!
//! - Widths and vertical metrics come from lb-layout's per-OS metric
//!   tables (`profiles/fonts/<os>/*.json`) through the same font matching
//!   as layout, so `measureText` agrees with DOM text measurement and the
//!   persona's installed-font list decides which family is used.
//! - Glyph shapes come from small outline packs (`profiles/fonts/outlines/`,
//!   derived from Liberation Sans/Serif/Mono Regular and Bold, SIL OFL 1.1,
//!   about 205 KB in all) built offline with skrifa
//!   (`examples/gen_glyph_pack.rs`). Each family is drawn with the pack of
//!   its style class (sans, serif, mono) at the table's advances. Italic is
//!   synthesized with a 1/4 skew, like Skia's fake italic. Characters
//!   without a glyph (emoji, CJK) advance but draw nothing.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::rc::Rc;

use lb_css::{
    Calc, ComputedStyle, Decl, DeclarationBlock, Defaults, FamilyName, FontSizeSpec, FontStyle,
    FontWeightSpec, LonghandId, OsKind, Sv, Unit,
};
use lb_layout::{FontAvailability, FontDb, TextFont, resolve_font};
use lb_persona::PersonaProfile;
use tiny_skia::Transform;

use crate::path::{PathData, map};

// The packs (`profiles/fonts/outlines/*.lbgp`) are embedded
// zstd-compressed by `build.rs` (M6 binary size, ADR 0013): 210 KB become
// about 90 KB. A pack is unpacked the first time a renderer draws with it.
include!(concat!(env!("OUT_DIR"), "/glyph_packs.rs"));

/// Pack `idx`, unpacked.
fn unpack(idx: usize) -> Vec<u8> {
    let (packed, len) = PACKS[idx];
    zstd::bulk::decompress(packed, len).expect("bundled glyph pack decompresses")
}

/// Total size of the bundled outline packs in bytes.
pub fn bundled_outline_bytes() -> usize {
    PACKS.iter().map(|p| p.1).sum()
}

/// Synthetic italic skew (Skia's fake italic).
const ITALIC_SKEW: f32 = 0.25;

#[derive(Clone, Copy, Debug)]
struct Glyph {
    bbox: [i16; 4],
    off: usize,
    len: usize,
}

/// One parsed outline pack (see `examples/gen_glyph_pack.rs`).
#[derive(Debug)]
pub struct GlyphPack {
    upem: f32,
    glyphs: HashMap<u32, Glyph>,
    data: Box<[u8]>,
}

impl GlyphPack {
    fn parse(data: Vec<u8>) -> Option<GlyphPack> {
        let b = &data[..];
        let u16_at = |i: usize| Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?));
        let u32_at = |i: usize| Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?));
        if b.get(..4)? != b"LBGP" || u16_at(4)? != 1 {
            return None;
        }
        let upem = f32::from(u16_at(6)?);
        let n = usize::from(u16_at(8)?);
        const ENTRY: usize = 4 + 2 + 8 + 8;
        let data_start = 10 + n * ENTRY;
        let mut glyphs = HashMap::with_capacity(n);
        for i in 0..n {
            let e = 10 + i * ENTRY;
            let cp = u32_at(e)?;
            let mut bbox = [0i16; 4];
            for (k, v) in bbox.iter_mut().enumerate() {
                *v = u16_at(e + 6 + 2 * k)? as i16;
            }
            let off = data_start + u32_at(e + 14)? as usize;
            let len = u32_at(e + 18)? as usize;
            b.get(off..off + len)?;
            glyphs.insert(cp, Glyph { bbox, off, len });
        }
        Some(GlyphPack {
            upem,
            glyphs,
            data: data.into_boxed_slice(),
        })
    }

    fn glyph(&self, c: char) -> Option<Glyph> {
        self.glyphs.get(&(c as u32)).copied()
    }

    /// Appends glyph `g` to `path`; `pt` maps font units (y up) to the
    /// path's space.
    fn outline(&self, g: Glyph, path: &mut PathData, pt: &dyn Fn(f64, f64) -> (f64, f64)) {
        let d = &self.data[g.off..g.off + g.len];
        let id = Transform::identity();
        let mut i = 0;
        let read = |i: &mut usize| -> Option<(f64, f64)> {
            let x = i16::from_le_bytes(d.get(*i..*i + 2)?.try_into().ok()?);
            let y = i16::from_le_bytes(d.get(*i + 2..*i + 4)?.try_into().ok()?);
            *i += 4;
            Some(pt(f64::from(x) / 2.0, f64::from(y) / 2.0))
        };
        while i < d.len() {
            let tag = d[i];
            i += 1;
            let ok = (|| -> Option<()> {
                match tag {
                    0 => {
                        let (x, y) = read(&mut i)?;
                        path.move_to(&id, x, y);
                    }
                    1 => {
                        let (x, y) = read(&mut i)?;
                        path.line_to(&id, x, y);
                    }
                    2 => {
                        let (cx, cy) = read(&mut i)?;
                        let (x, y) = read(&mut i)?;
                        path.quad_to(&id, cx, cy, x, y);
                    }
                    3 => {
                        let (ax, ay) = read(&mut i)?;
                        let (bx, by) = read(&mut i)?;
                        let (x, y) = read(&mut i)?;
                        path.bezier_to(&id, ax, ay, bx, by, x, y);
                    }
                    4 => path.close(),
                    _ => return None,
                }
                Some(())
            })();
            if ok.is_none() {
                return;
            }
        }
    }
}

/// A parsed canvas `font` value (the `font` shorthand, sizes resolved
/// against the 10px canvas default).
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasFont {
    pub style: FontStyle,
    pub weight: f32,
    pub size: f32,
    pub families: Rc<[FamilyName]>,
}

impl Default for CanvasFont {
    /// `10px sans-serif`.
    fn default() -> Self {
        CanvasFont {
            style: FontStyle::Normal,
            weight: 400.0,
            size: 10.0,
            families: Rc::from(vec![FamilyName {
                name: "sans-serif".into(),
                generic: true,
            }]),
        }
    }
}

fn calc_px(c: &Calc, em: f32) -> f32 {
    match c {
        Calc::Dim(v, u) => match u {
            Unit::Px => *v,
            Unit::Pct => v * em / 100.0,
            Unit::Em => v * em,
            Unit::Rem => v * 16.0,
            Unit::Ex | Unit::Ch => v * em * 0.5,
            Unit::Vw | Unit::Vh | Unit::Vmin | Unit::Vmax => 0.0,
        },
        Calc::Num(v) => *v,
        Calc::Sum(xs) => xs.iter().map(|x| calc_px(x, em)).sum(),
        Calc::Mul(x, k) => calc_px(x, em) * k,
        Calc::Min(xs) => xs
            .iter()
            .map(|x| calc_px(x, em))
            .fold(f32::INFINITY, f32::min),
        Calc::Max(xs) => xs
            .iter()
            .map(|x| calc_px(x, em))
            .fold(f32::NEG_INFINITY, f32::max),
        Calc::Clamp(lo, v, hi) => calc_px(v, em).min(calc_px(hi, em)).max(calc_px(lo, em)),
    }
}

impl CanvasFont {
    /// Parses a CSS `font` shorthand value. `None` if invalid (the canvas
    /// keeps its current font).
    pub fn parse(s: &str) -> Option<CanvasFont> {
        let s = s.trim();
        if s.is_empty() || s.contains([';', '{', '}', '!']) {
            return None;
        }
        let block = DeclarationBlock::parse(&format!("font: {s}"));
        let mut f = CanvasFont::default();
        let mut seen = 0;
        for d in &block.decls {
            let Decl::Value(v) = &d.value else {
                return None;
            };
            match (d.id, v) {
                (LonghandId::FontStyle, Sv::FontStyle(st)) => f.style = *st,
                (LonghandId::FontWeight, Sv::FontWeight(w)) => {
                    f.weight = match w {
                        FontWeightSpec::Absolute(w) => *w,
                        FontWeightSpec::Bolder => 700.0,
                        FontWeightSpec::Lighter => 100.0,
                    }
                }
                (LonghandId::FontSize, Sv::FontSize(sz)) => {
                    f.size = match sz {
                        FontSizeSpec::Keyword(k) => 16.0 * k,
                        FontSizeSpec::Larger => 12.0,
                        FontSizeSpec::Smaller => 10.0 / 1.2,
                        FontSizeSpec::Length(c) => calc_px(c, 10.0),
                    }
                }
                (LonghandId::FontFamily, Sv::FontFamily(fams)) => f.families = fams.clone(),
                _ => continue,
            }
            seen += 1;
        }
        (seen == 4 && f.size.is_finite() && f.size >= 0.0).then_some(f)
    }

    /// Chrome's serialization: `[italic ][weight ]<size>px <families>`.
    pub fn to_css(&self) -> String {
        let mut s = String::new();
        match self.style {
            FontStyle::Normal => {}
            st => {
                s.push_str(st.as_str());
                s.push(' ');
            }
        }
        if self.weight != 400.0 {
            if self.weight == 700.0 {
                s.push_str("bold ");
            } else {
                s.push_str(&lb_css::fmt_num(self.weight));
                s.push(' ');
            }
        }
        s.push_str(&lb_css::fmt_px(self.size));
        s.push(' ');
        let fams: Vec<String> = self.families.iter().map(FamilyName::to_css).collect();
        s.push_str(&fams.join(", "));
        s
    }
}

/// Fonts of one persona: metric tables, installed list and outline packs.
pub struct FontSet {
    db: FontDb,
    avail: FontAvailability,
    defaults: Defaults,
    packs: [OnceCell<Option<GlyphPack>>; 6],
}

impl std::fmt::Debug for FontSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontSet").finish_non_exhaustive()
    }
}

/// Text laid out for drawing or measuring.
#[derive(Clone, Debug, PartialEq)]
pub struct TextMetrics {
    pub width: f64,
    pub actual_bounding_box_left: f64,
    pub actual_bounding_box_right: f64,
    pub actual_bounding_box_ascent: f64,
    pub actual_bounding_box_descent: f64,
    pub font_bounding_box_ascent: f64,
    pub font_bounding_box_descent: f64,
    pub em_height_ascent: f64,
    pub em_height_descent: f64,
    pub hanging_baseline: f64,
    pub alphabetic_baseline: f64,
    pub ideographic_baseline: f64,
}

/// `textAlign`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Start,
    End,
    Left,
    Right,
    Center,
}

impl TextAlign {
    pub fn parse(s: &str) -> Option<TextAlign> {
        Some(match s {
            "start" => TextAlign::Start,
            "end" => TextAlign::End,
            "left" => TextAlign::Left,
            "right" => TextAlign::Right,
            "center" => TextAlign::Center,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            TextAlign::Start => "start",
            TextAlign::End => "end",
            TextAlign::Left => "left",
            TextAlign::Right => "right",
            TextAlign::Center => "center",
        }
    }
}

/// `textBaseline`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TextBaseline {
    Top,
    Hanging,
    Middle,
    #[default]
    Alphabetic,
    Ideographic,
    Bottom,
}

impl TextBaseline {
    pub fn parse(s: &str) -> Option<TextBaseline> {
        Some(match s {
            "top" => TextBaseline::Top,
            "hanging" => TextBaseline::Hanging,
            "middle" => TextBaseline::Middle,
            "alphabetic" => TextBaseline::Alphabetic,
            "ideographic" => TextBaseline::Ideographic,
            "bottom" => TextBaseline::Bottom,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            TextBaseline::Top => "top",
            TextBaseline::Hanging => "hanging",
            TextBaseline::Middle => "middle",
            TextBaseline::Alphabetic => "alphabetic",
            TextBaseline::Ideographic => "ideographic",
            TextBaseline::Bottom => "bottom",
        }
    }
}

/// Everything text drawing needs besides the string.
#[derive(Clone, Debug)]
pub struct TextStyle {
    pub font: TextFont,
    pub align: TextAlign,
    pub baseline: TextBaseline,
    pub rtl: bool,
}

/// Canvas text preprocessing: ASCII whitespace becomes a space.
pub fn prepare_text(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\t' | '\n' | '\x0c' | '\r' => ' ',
            c => c,
        })
        .collect()
}

struct Laid {
    /// (character, pen x in px).
    glyphs: Vec<(char, f32)>,
    width: f32,
}

impl FontSet {
    /// Fonts for a persona (OS tables, installed list); without one,
    /// Windows tables with every family installed.
    pub fn new(profile: Option<&PersonaProfile>) -> FontSet {
        let os = profile.map_or(OsKind::Windows, |p| OsKind::from_name(&p.os.name));
        let avail = match profile {
            Some(p) => FontAvailability::from_list(p.fonts.available.iter().map(String::as_str)),
            None => FontAvailability::everything(),
        };
        FontSet {
            db: FontDb::for_os(os),
            avail,
            defaults: Defaults::for_os(os),
            packs: Default::default(),
        }
    }

    /// The metric-table font a canvas font resolves to (layout's matching).
    pub fn resolve(&self, f: &CanvasFont, letter_spacing: f32, word_spacing: f32) -> TextFont {
        let mut style = ComputedStyle::initial(&self.defaults);
        style.font_family = f.families.clone();
        style.font_size = f.size;
        style.font_weight = f.weight;
        style.font_style = f.style;
        let mut tf = resolve_font(&self.db, &self.avail, &style);
        tf.letter_spacing = letter_spacing;
        tf.word_spacing = word_spacing;
        tf
    }

    fn pack_for(&self, tf: &TextFont) -> Option<&GlyphPack> {
        let n = tf.family.name.to_ascii_lowercase();
        let class = if ["mono", "courier", "consol", "menlo", "monaco"]
            .iter()
            .any(|k| n.contains(k))
        {
            2
        } else if [
            "serif", "times", "georgia", "cambria", "garamond", "palatino", "book",
        ]
        .iter()
        .any(|k| n.contains(k))
            && !n.contains("sans")
        {
            1
        } else {
            0
        };
        let idx = class * 2 + usize::from(tf.weight >= 600.0);
        self.packs[idx]
            .get_or_init(|| GlyphPack::parse(unpack(idx)))
            .as_ref()
    }

    fn lay_out(&self, tf: &TextFont, text: &str) -> Laid {
        let face = tf.family.face(tf.weight, tf.italic);
        let upem = tf.family.units_per_em;
        let k = tf.size / upem;
        let mut x = 0.0;
        let mut glyphs = Vec::with_capacity(text.len());
        for c in text.chars() {
            glyphs.push((c, x));
            x += face.advance(c, upem) * k + tf.letter_spacing;
            if c == ' ' || c == '\u{a0}' {
                x += tf.word_spacing;
            }
        }
        Laid { glyphs, width: x }
    }

    /// Horizontal offset of the text start from the anchor, and the
    /// alphabetic baseline's offset below the given y.
    fn anchor(&self, st: &TextStyle, width: f32) -> (f32, f32) {
        let ax = match (st.align, st.rtl) {
            (TextAlign::Left, _) | (TextAlign::Start, false) | (TextAlign::End, true) => 0.0,
            (TextAlign::Right, _) | (TextAlign::End, false) | (TextAlign::Start, true) => -width,
            (TextAlign::Center, _) => -width / 2.0,
        };
        let lm = st.font.line_metrics();
        let dy = match st.baseline {
            TextBaseline::Top => lm.ascent,
            TextBaseline::Hanging => lm.ascent * 0.8,
            TextBaseline::Middle => (lm.ascent - lm.descent) / 2.0,
            TextBaseline::Alphabetic => 0.0,
            TextBaseline::Ideographic | TextBaseline::Bottom => -lm.descent,
        };
        (ax, dy)
    }

    /// Text width in px (same sum as layout's `TextFont::measure`).
    pub fn width(&self, st: &TextStyle, text: &str) -> f32 {
        self.lay_out(&st.font, text).width
    }

    /// `measureText`.
    pub fn measure(&self, st: &TextStyle, text: &str) -> TextMetrics {
        let text = prepare_text(text);
        let tf = &st.font;
        let laid = self.lay_out(tf, &text);
        let (ax, dy) = self.anchor(st, laid.width);
        let (mut x0, mut x1, mut up, mut down) = (f32::INFINITY, f32::NEG_INFINITY, 0.0f32, 0.0f32);
        let mut ink = false;
        if let Some(pack) = self.pack_for(tf) {
            let s = tf.size / pack.upem;
            let skew = if tf.italic { ITALIC_SKEW } else { 0.0 };
            for &(c, px) in &laid.glyphs {
                let Some(g) = pack.glyph(c) else { continue };
                let [bx0, by0, bx1, by1] = g.bbox.map(f32::from);
                if bx0 == bx1 && by0 == by1 {
                    continue;
                }
                ink = true;
                let (sk0, sk1) = ((skew * by0).min(skew * by1), (skew * by0).max(skew * by1));
                x0 = x0.min(px + (bx0 + sk0) * s);
                x1 = x1.max(px + (bx1 + sk1) * s);
                up = up.max(by1 * s);
                down = down.min(by0 * s);
            }
        }
        if !ink {
            (x0, x1) = (0.0, 0.0);
        }
        let lm = tf.line_metrics();
        let fam = &tf.family;
        let em_a = tf.size * fam.ascent / (fam.ascent + fam.descent).max(1.0);
        let f = f64::from;
        TextMetrics {
            width: f(laid.width),
            actual_bounding_box_left: f(-(x0 + ax)),
            actual_bounding_box_right: f(x1 + ax),
            actual_bounding_box_ascent: f(up - dy),
            actual_bounding_box_descent: f(-down + dy),
            font_bounding_box_ascent: f(lm.ascent - dy),
            font_bounding_box_descent: f(lm.descent + dy),
            em_height_ascent: f(em_a - dy),
            em_height_descent: f(tf.size - em_a + dy),
            hanging_baseline: f(lm.ascent * 0.8 - dy),
            alphabetic_baseline: f(-dy),
            ideographic_baseline: f(-lm.descent - dy),
        }
    }

    /// Glyph outlines of `text` anchored at (`x`, `y`) in user space,
    /// squeezed to `max_width`, mapped through `ts` to device space.
    pub fn text_path(
        &self,
        st: &TextStyle,
        text: &str,
        x: f64,
        y: f64,
        max_width: Option<f64>,
        ts: &Transform,
    ) -> PathData {
        let mut path = PathData::new();
        let text = prepare_text(text);
        let tf = &st.font;
        let laid = self.lay_out(tf, &text);
        let Some(pack) = self.pack_for(tf) else {
            return path;
        };
        let (ax, dy) = self.anchor(st, laid.width);
        let sx = match max_width {
            Some(m) if f64::from(laid.width) > m && laid.width > 0.0 => m / f64::from(laid.width),
            _ => 1.0,
        };
        let s = f64::from(tf.size / pack.upem);
        let skew = if tf.italic {
            f64::from(ITALIC_SKEW)
        } else {
            0.0
        };
        let base_y = y + f64::from(dy);
        for &(c, px) in &laid.glyphs {
            let Some(g) = pack.glyph(c) else { continue };
            let pen = f64::from(ax + px);
            let pt = |gx: f64, gy: f64| {
                let ux = x + (pen + (gx + skew * gy) * s) * sx;
                let uy = base_y - gy * s;
                let p = map(ts, ux, uy);
                (p.x, p.y)
            };
            pack.outline(g, &mut path, &pt);
        }
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_parse() {
        for idx in 0..PACKS.len() {
            let g = GlyphPack::parse(unpack(idx)).expect("pack");
            assert!(g.glyph('A').is_some() && g.glyph('é').is_some());
            assert!(g.glyph('\u{1F600}').is_none());
        }
        assert!(bundled_outline_bytes() < 300_000);
    }

    #[test]
    fn font_shorthand() {
        let f = CanvasFont::parse("14px Arial").unwrap();
        assert_eq!(f.to_css(), "14px Arial");
        let f = CanvasFont::parse("italic bold 12px/30px Georgia, serif").unwrap();
        assert_eq!(f.to_css(), "italic bold 12px Georgia, serif");
        assert_eq!(
            CanvasFont::parse("600 2em \"Times New Roman\"")
                .unwrap()
                .to_css(),
            "600 20px \"Times New Roman\""
        );
        assert_eq!(CanvasFont::default().to_css(), "10px sans-serif");
        assert!(CanvasFont::parse("bold").is_none());
        assert!(CanvasFont::parse("inherit").is_none());
        assert!(CanvasFont::parse("12px a; color: red").is_none());
    }

    #[test]
    fn measure_matches_layout_tables() {
        let fs = FontSet::new(None);
        let f = CanvasFont::parse("16px Arial").unwrap();
        let st = TextStyle {
            font: fs.resolve(&f, 0.0, 0.0),
            align: TextAlign::Start,
            baseline: TextBaseline::Alphabetic,
            rtl: false,
        };
        let m = fs.measure(&st, "Hello world");
        assert!((m.width - f64::from(st.font.measure("Hello world"))).abs() < 1e-6);
        // Arial: H 1479, e 1139, l 455 ... at 2048 upem.
        let expected =
            (1479.0 + 1139.0 + 455.0 * 3.0 + 1139.0 * 3.0 + 569.0 + 1479.0 + 682.0) * 16.0 / 2048.0;
        assert!((m.width - expected).abs() < 1e-3, "{}", m.width);
        assert!(m.actual_bounding_box_ascent > 10.0 && m.actual_bounding_box_ascent < 13.0);
        assert!(m.actual_bounding_box_descent < 1.0);
        assert_eq!(m.font_bounding_box_ascent, 14.0);
        assert_eq!(m.font_bounding_box_descent, 3.0);
        assert_eq!(m.alphabetic_baseline, 0.0);
        let top = TextStyle {
            baseline: TextBaseline::Top,
            align: TextAlign::Center,
            ..st.clone()
        };
        let mt = fs.measure(&top, "Hello world");
        assert_eq!(mt.font_bounding_box_ascent, 0.0);
        assert!(
            (mt.actual_bounding_box_left - (m.width / 2.0 + m.actual_bounding_box_left)).abs()
                < 1e-3
        );
        // Unknown families fall back like layout does; a missing glyph
        // still advances.
        let e = fs.measure(&st, "\u{1F600}");
        assert_eq!(e.width, 16.0);
        assert!(
            !fs.text_path(&st, "Hi", 0.0, 20.0, None, &Transform::identity())
                .is_empty()
        );
    }
}
