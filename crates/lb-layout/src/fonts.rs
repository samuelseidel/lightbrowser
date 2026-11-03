//! Font metric tables (`profiles/fonts/<os>/*.json`) and text measurement.
//!
//! Tables hold per-character advance widths and vertical metrics for the
//! fonts a persona's OS ships. They are generated offline from font files
//! (`examples/gen_font_tables.rs`); nothing reads font files at runtime.
//! Width of a string = sum of advances (no shaping or kerning) plus
//! letter/word spacing.

use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use lb_css::{ComputedStyle, FamilyName, FontStyle, OsKind};
use serde::Deserialize;

// The tables (`profiles/fonts/<os>/*.json` and `derived.json`) are
// embedded as one zstd frame made by `build.rs` (M6 binary size, ADR
// 0013): 114 KB of JSON become about 8 KB. `build.rs` lists the files.
include!(concat!(env!("OUT_DIR"), "/font_tables.rs"));

/// The bundled JSON, decompressed (one frame; [`BUNDLED`] and
/// [`DERIVED`] are byte ranges into it). Decompressed per [`FontDb::for_os`]
/// call and dropped with it: about 0.1 ms, next to the JSON parsing.
fn unpack_tables() -> String {
    let raw = zstd::bulk::decompress(PACKED_TABLES, PACKED_TABLES_LEN)
        .expect("bundled font tables decompress");
    String::from_utf8(raw).expect("bundled font tables are UTF-8")
}

#[derive(Deserialize)]
struct DerivedFile {
    format: u32,
    families: Vec<DerivedEntry>,
}

#[derive(Deserialize)]
struct DerivedEntry {
    family: String,
    os: String,
    base: String,
    /// Advance-width factor applied to the base table.
    scale: f32,
    /// Vertical metrics in em.
    ascent: f32,
    descent: f32,
    line_gap: f32,
}

/// Number of bundled table files (checked against the directory in tests).
pub fn bundled_table_count() -> usize {
    BUNDLED.len()
}

#[derive(Deserialize)]
struct TableFile {
    format: u32,
    family: String,
    #[allow(dead_code)]
    os: String,
    approximate: bool,
    source: String,
    units_per_em: f32,
    ascent: f32,
    descent: f32,
    line_gap: f32,
    faces: Vec<FaceFile>,
}

#[derive(Deserialize)]
struct FaceFile {
    weight: u16,
    style: String,
    default_advance: f32,
    ranges: Vec<(u32, Vec<u16>)>,
}

/// Advance widths of one face (weight + style), in font units.
#[derive(Debug)]
pub struct FontFace {
    pub weight: u16,
    pub italic: bool,
    default_advance: f32,
    ranges: Vec<(u32, Vec<u16>)>,
}

impl FontFace {
    /// Advance of `c` in font units. Characters outside the table get
    /// the face's average width, or one em for wide (CJK) characters.
    pub fn advance(&self, c: char, upem: f32) -> f32 {
        let cp = c as u32;
        for (start, adv) in &self.ranges {
            if cp >= *start && ((cp - start) as usize) < adv.len() {
                return adv[(cp - start) as usize] as f32;
            }
        }
        if is_zero_width(c) {
            0.0
        } else if is_wide(c) {
            upem
        } else {
            self.default_advance
        }
    }
}

fn is_zero_width(c: char) -> bool {
    matches!(
        c,
        '\u{200b}'..='\u{200f}' | '\u{2060}' | '\u{feff}' | '\u{ad}'
    ) || ('\u{300}'..='\u{36f}').contains(&c)
}

fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115f | 0x2e80..=0xa4cf | 0xac00..=0xd7a3 | 0xf900..=0xfaff
        | 0xfe30..=0xfe4f | 0xff00..=0xff60 | 0xffe0..=0xffe6 | 0x1f300..=0x1faff
        | 0x20000..=0x3fffd)
}

/// One font family's metric table.
#[derive(Debug)]
pub struct FontFamily {
    pub name: String,
    /// Built from a similar font rather than the real one.
    pub approximate: bool,
    pub source: String,
    pub units_per_em: f32,
    pub ascent: f32,
    pub descent: f32,
    pub line_gap: f32,
    faces: Vec<FontFace>,
}

/// Vertical metrics at a size, rounded per line metric as Chrome does
/// (`SimpleFontData`: ascent, descent and line gap each rounded).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LineMetrics {
    pub ascent: f32,
    pub descent: f32,
    pub line_gap: f32,
}

impl LineMetrics {
    /// `line-height: normal`.
    pub fn normal_line_height(&self) -> f32 {
        self.ascent + self.descent + self.line_gap
    }
}

impl FontFamily {
    fn from_json(json: &str) -> Result<FontFamily, String> {
        let t: TableFile = serde_json::from_str(json).map_err(|e| e.to_string())?;
        if t.format != 1 {
            return Err(format!("unsupported font table format {}", t.format));
        }
        Ok(FontFamily {
            name: t.family,
            approximate: t.approximate,
            source: t.source,
            units_per_em: t.units_per_em,
            ascent: t.ascent,
            descent: t.descent,
            line_gap: t.line_gap,
            faces: t
                .faces
                .into_iter()
                .map(|f| FontFace {
                    weight: f.weight,
                    italic: f.style != "normal",
                    default_advance: f.default_advance,
                    ranges: f.ranges,
                })
                .collect(),
        })
    }

    /// An approximate family: `base`'s advances scaled, own vertical
    /// metrics.
    fn derived(base: &FontFamily, e: &DerivedEntry) -> FontFamily {
        let upem = base.units_per_em;
        let scale = |v: f32| (v * e.scale).round();
        FontFamily {
            name: e.family.clone(),
            approximate: true,
            source: format!("{} advances x {} (derived.json)", base.name, e.scale),
            units_per_em: upem,
            ascent: (e.ascent * upem).round(),
            descent: (e.descent * upem).round(),
            line_gap: (e.line_gap * upem).round(),
            faces: base
                .faces
                .iter()
                .map(|f| FontFace {
                    weight: f.weight,
                    italic: f.italic,
                    default_advance: scale(f.default_advance),
                    ranges: f
                        .ranges
                        .iter()
                        .map(|(start, adv)| {
                            (
                                *start,
                                adv.iter()
                                    .map(|a| scale(f32::from(*a)).min(f32::from(u16::MAX)) as u16)
                                    .collect(),
                            )
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    /// Closest face: style first, then weight (CSS font matching, roughly).
    pub fn face(&self, weight: f32, italic: bool) -> &FontFace {
        let score = |f: &FontFace| {
            let style = if f.italic == italic { 0.0 } else { 10_000.0 };
            let w = f.weight as f32;
            // Prefer bolder faces for weights above 500, lighter below.
            let dir = if weight > 500.0 && w < weight || weight <= 500.0 && w > weight {
                1000.0
            } else {
                0.0
            };
            style + dir + (w - weight).abs()
        };
        self.faces
            .iter()
            .min_by(|a, b| score(a).total_cmp(&score(b)))
            .expect("font table has faces")
    }

    pub fn line_metrics(&self, size: f32) -> LineMetrics {
        let k = size / self.units_per_em;
        LineMetrics {
            ascent: (self.ascent * k).round(),
            descent: (self.descent * k).round(),
            line_gap: (self.line_gap * k).round(),
        }
    }
}

/// The metric tables available for one OS, by lowercase family name.
#[derive(Debug, Default)]
pub struct FontDb {
    families: HashMap<String, Rc<FontFamily>>,
    os: Option<OsKind>,
}

impl FontDb {
    /// The bundled tables for `os` (Linux falls back to its own set,
    /// unknown OSes to Windows).
    pub fn for_os(os: OsKind) -> FontDb {
        let dir = match os {
            OsKind::Other => "windows",
            o => o.as_str(),
        };
        let mut db = FontDb {
            families: HashMap::new(),
            os: Some(os),
        };
        let tables = unpack_tables();
        for (d, range) in BUNDLED {
            if *d == dir {
                let fam =
                    FontFamily::from_json(&tables[range.clone()]).expect("bundled font table");
                db.insert(fam);
            }
        }
        let derived: DerivedFile =
            serde_json::from_str(&tables[DERIVED]).expect("derived font list");
        debug_assert_eq!(derived.format, 1);
        for e in derived.families.iter().filter(|e| e.os == dir) {
            if db.get(&e.family).is_some() {
                continue;
            }
            if let Some(base) = db.get(&e.base).cloned() {
                db.insert(FontFamily::derived(&base, e));
            }
        }
        db
    }

    /// Adds every `*.json` table in `dir` (replacing same-named families).
    pub fn load_dir(&mut self, dir: &Path) -> std::io::Result<usize> {
        let mut n = 0;
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                let json = std::fs::read_to_string(&path)?;
                let fam = FontFamily::from_json(&json)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                self.insert(fam);
                n += 1;
            }
        }
        Ok(n)
    }

    fn insert(&mut self, fam: FontFamily) {
        self.families
            .insert(fam.name.to_ascii_lowercase(), Rc::new(fam));
    }

    pub fn get(&self, name: &str) -> Option<&Rc<FontFamily>> {
        self.families.get(&name.to_ascii_lowercase())
    }

    pub fn family_names(&self) -> impl Iterator<Item = &str> {
        self.families.values().map(|f| f.name.as_str())
    }

    /// Family used for a generic keyword on this OS (Chrome defaults).
    pub fn generic(&self, generic: &str) -> Option<&Rc<FontFamily>> {
        let mac = self.os == Some(OsKind::MacOs);
        let linux = self.os == Some(OsKind::Linux);
        let candidates: &[&str] = match generic {
            "serif" | "-webkit-body" | "ui-serif" => {
                if mac {
                    &["Times", "Times New Roman"]
                } else if linux {
                    &["Liberation Serif", "DejaVu Serif"]
                } else {
                    &["Times New Roman"]
                }
            }
            "sans-serif" | "ui-sans-serif" | "ui-rounded" => {
                if mac {
                    &["Helvetica", "Arial"]
                } else if linux {
                    &["Liberation Sans", "DejaVu Sans"]
                } else {
                    &["Arial"]
                }
            }
            "monospace" | "ui-monospace" => {
                if mac {
                    &["Courier", "Courier New"]
                } else if linux {
                    &["Liberation Mono", "DejaVu Sans Mono"]
                } else {
                    &["Courier New"]
                }
            }
            "system-ui" | "-apple-system" | "blinkmacsystemfont" => {
                if mac {
                    &["system-ui", "Helvetica"]
                } else if linux {
                    &["DejaVu Sans", "Liberation Sans"]
                } else {
                    &["Segoe UI", "Arial"]
                }
            }
            "cursive" | "fantasy" | "math" | "emoji" | "fangsong" => {
                if mac {
                    &["Helvetica"]
                } else if linux {
                    &["DejaVu Sans"]
                } else {
                    &["Arial"]
                }
            }
            _ => &[],
        };
        candidates.iter().find_map(|n| self.get(n))
    }
}

/// Resolved font for a run of text.
#[derive(Clone, Debug)]
pub struct TextFont {
    pub family: Rc<FontFamily>,
    pub size: f32,
    pub weight: f32,
    pub italic: bool,
    pub letter_spacing: f32,
    pub word_spacing: f32,
}

impl TextFont {
    /// Width of `text` in px: sum of advances plus spacing.
    pub fn measure(&self, text: &str) -> f32 {
        let face = self.family.face(self.weight, self.italic);
        let k = self.size / self.family.units_per_em;
        let upem = self.family.units_per_em;
        let mut w = 0.0;
        for c in text.chars() {
            w += face.advance(c, upem) * k + self.letter_spacing;
            if c == ' ' || c == '\u{a0}' {
                w += self.word_spacing;
            }
        }
        w
    }

    pub fn line_metrics(&self) -> LineMetrics {
        self.family.line_metrics(self.size)
    }

    /// Used line height for `style` in px.
    pub fn line_height(&self, style: &ComputedStyle) -> f32 {
        let lm = self.line_metrics();
        let factor = lm.normal_line_height() / self.size.max(f32::MIN_POSITIVE);
        style.line_height_px(factor)
    }
}

/// Picks the font for a computed style: the first family in
/// `font-family` that the persona has (and we have a table for), then the
/// generic mapping, then the OS standard font.
pub fn resolve_font(db: &FontDb, available: &FontAvailability, style: &ComputedStyle) -> TextFont {
    let family = pick_family(db, available, &style.font_family)
        .or_else(|| db.generic("serif"))
        .or_else(|| db.families.values().next())
        .cloned()
        .expect("font db is never empty");
    TextFont {
        family,
        size: style.font_size,
        weight: style.font_weight,
        italic: style.font_style != FontStyle::Normal,
        letter_spacing: style.letter_spacing.unwrap_or(0.0),
        word_spacing: style.word_spacing,
    }
}

fn pick_family<'a>(
    db: &'a FontDb,
    available: &FontAvailability,
    families: &[FamilyName],
) -> Option<&'a Rc<FontFamily>> {
    for f in families {
        if f.generic {
            if let Some(g) = db.generic(&f.name) {
                return Some(g);
            }
            continue;
        }
        let lower = f.name.to_ascii_lowercase();
        if matches!(lower.as_str(), "-apple-system" | "blinkmacsystemfont") {
            if db.os == Some(OsKind::MacOs)
                && let Some(g) = db.generic("system-ui")
            {
                return Some(g);
            }
            continue;
        }
        if available.has(&lower)
            && let Some(t) = db.get(&lower)
        {
            return Some(t);
        }
    }
    None
}

/// The persona's installed font list (`fonts.available`).
#[derive(Clone, Debug, Default)]
pub struct FontAvailability {
    names: std::collections::HashSet<String>,
    /// No list given: every table counts as installed.
    all: bool,
}

impl FontAvailability {
    pub fn from_list<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        FontAvailability {
            names: names.into_iter().map(|n| n.to_ascii_lowercase()).collect(),
            all: false,
        }
    }

    /// Every family with a table is available.
    pub fn everything() -> Self {
        FontAvailability {
            names: Default::default(),
            all: true,
        }
    }

    pub fn has(&self, lower_name: &str) -> bool {
        self.all || self.names.contains(lower_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_tables_load_and_cover_the_directory() {
        for os in [OsKind::Windows, OsKind::MacOs, OsKind::Linux] {
            let db = FontDb::for_os(os);
            assert!(db.generic("serif").is_some());
            assert!(db.generic("sans-serif").is_some());
            assert!(db.generic("monospace").is_some());
            assert!(db.generic("system-ui").is_some());
        }
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../profiles/fonts");
        let mut files = 0;
        for os in std::fs::read_dir(dir).unwrap() {
            let os = os.unwrap().path();
            if os.is_dir() {
                files += std::fs::read_dir(os)
                    .unwrap()
                    .filter(|e| {
                        e.as_ref()
                            .is_ok_and(|e| e.path().extension().is_some_and(|x| x == "json"))
                    })
                    .count();
            }
        }
        assert_eq!(files, bundled_table_count(), "embed every table in BUNDLED");
    }

    #[test]
    fn derived_tables_cover_reference_personas() {
        let win = FontDb::for_os(OsKind::Windows);
        let consolas = win.get("Consolas").unwrap();
        assert!(consolas.approximate);
        let courier = win.get("Courier New").unwrap();
        let (c, k) = (consolas.face(400.0, false), courier.face(400.0, false));
        assert!(c.advance('m', 2048.0) < k.advance('m', 2048.0));
        let mac = FontDb::for_os(OsKind::MacOs);
        assert!(mac.get("Menlo").is_some());
        assert!(win.get("Menlo").is_none());
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../profiles/reference/chrome"
        );
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let p: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let os = OsKind::from_name(p["os"]["name"].as_str().unwrap());
            let db = FontDb::for_os(os);
            for f in p["fonts"]["available"].as_array().unwrap() {
                let f = f.as_str().unwrap();
                assert!(db.get(f).is_some(), "{}: no table for {f}", path.display());
            }
        }
    }

    #[test]
    fn arial_metrics() {
        let db = FontDb::for_os(OsKind::Windows);
        let arial = db.get("arial").unwrap().clone();
        assert!(!arial.approximate);
        let f = TextFont {
            family: arial,
            size: 16.0,
            weight: 400.0,
            italic: false,
            letter_spacing: 0.0,
            word_spacing: 0.0,
        };
        // Arial advances (2048 upem): H 1479, e 1139, l 455, o 1139,
        // space 569, w 1479, r 682, d 1139.
        let expected =
            (1479.0 + 1139.0 + 455.0 * 3.0 + 1139.0 * 3.0 + 569.0 + 1479.0 + 682.0) * 16.0 / 2048.0;
        assert!((f.measure("Hello world") - expected).abs() < 1e-3);
        assert_eq!(f.line_metrics().normal_line_height(), 18.0);
        let bold = TextFont {
            weight: 700.0,
            ..f.clone()
        };
        assert!(bold.measure("Hello") > f.measure("Hello"));
    }
}
