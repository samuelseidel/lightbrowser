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
use lb_persona::{FontDefaults, PersonaProfile};
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
        '\u{200b}'..='\u{200f}' | '\u{2060}' | '\u{feff}' | '\u{ad}' | '\u{fe00}'..='\u{fe0f}'
    ) || ('\u{300}'..='\u{36f}').contains(&c)
        || ('\u{1f3fb}'..='\u{1f3ff}').contains(&c)
}

/// Characters Chrome draws from the OS colour emoji font (no text font we
/// have a table for covers them): the emoji blocks of plane 1.
pub fn is_emoji(c: char) -> bool {
    matches!(c as u32,
        0x1f004 | 0x1f0cf | 0x1f18e | 0x1f191..=0x1f19a | 0x1f1e6..=0x1f1ff
        | 0x1f300..=0x1f3fa | 0x1f400..=0x1f64f | 0x1f680..=0x1f6ff
        | 0x1f7e0..=0x1f7f0 | 0x1f90c..=0x1f9ff | 0x1fa70..=0x1faff)
}

/// Advance in em of the emoji glyphs of an OS emoji font. Noto Color
/// Emoji is captured (Chrome 153 on Linux measures 136 px at its 109 ppem
/// bitmap strike, `tests/fixtures/os/`); Segoe UI Emoji and Apple Color
/// Emoji are not captured (M7 surface reference).
pub fn emoji_advance_em(family: &str) -> f32 {
    match family.to_ascii_lowercase().as_str() {
        "noto color emoji" => 136.0 / 109.0,
        "apple color emoji" => 1.25,
        "segoe ui emoji" => 1.3,
        _ => 1.0,
    }
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
/// Cheap to clone: clones share the tables until one adds a family.
#[derive(Debug, Default, Clone)]
pub struct FontDb {
    families: Rc<HashMap<String, Rc<FontFamily>>>,
    os: Option<OsKind>,
    /// Font settings of the OS, for callers without a persona.
    os_prefs: Option<Rc<FontPrefs>>,
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
            families: Rc::new(HashMap::new()),
            os: Some(os),
            os_prefs: Some(Rc::new(FontPrefs::for_os(os))),
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

    /// Adds a table for every listed family this OS has none for, taken
    /// from another OS (Windows, then macOS, then Linux): a persona that
    /// lists a foreign font (Segoe UI on Linux) gets it detected, as a real
    /// system with that font installed would. No-op (no unpacking) when
    /// every family has a table, as for every consistent persona.
    pub fn add_foreign<'a>(&mut self, names: impl IntoIterator<Item = &'a str>) {
        let missing: Vec<String> = names
            .into_iter()
            .map(str::to_ascii_lowercase)
            .filter(|n| self.get(n).is_none())
            .collect();
        if missing.is_empty() {
            return;
        }
        for dir in ["windows", "macos", "linux"] {
            let os = match dir {
                "windows" => OsKind::Windows,
                "macos" => OsKind::MacOs,
                _ => OsKind::Linux,
            };
            if self.os == Some(os) {
                continue;
            }
            let other = FontDb::for_os(os);
            for n in &missing {
                if self.get(n).is_none()
                    && let Some(t) = other.get(n)
                {
                    Rc::make_mut(&mut self.families).insert(n.clone(), t.clone());
                }
            }
        }
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
        Rc::make_mut(&mut self.families).insert(fam.name.to_ascii_lowercase(), Rc::new(fam));
    }

    pub fn get(&self, name: &str) -> Option<&Rc<FontFamily>> {
        self.families.get(&name.to_ascii_lowercase())
    }

    pub fn family_names(&self) -> impl Iterator<Item = &str> {
        self.families.values().map(|f| f.name.as_str())
    }

    /// Fallback family for a generic keyword on this OS when the persona's
    /// default font for it has no table or is not installed.
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
    /// Advance of an emoji in em (the persona's emoji font, which Chrome
    /// falls back to for [`is_emoji`] characters).
    pub emoji_advance: f32,
}

impl TextFont {
    /// Advance of one character in px (no spacing).
    pub fn advance(&self, face: &FontFace, c: char) -> f32 {
        if is_emoji(c) {
            self.emoji_advance * self.size
        } else {
            face.advance(c, self.family.units_per_em) * self.size / self.family.units_per_em
        }
    }

    /// Width of `text` in px: sum of advances plus spacing.
    pub fn measure(&self, text: &str) -> f32 {
        let face = self.family.face(self.weight, self.italic);
        let mut w = 0.0;
        for c in text.chars() {
            w += self.advance(face, c) + self.letter_spacing;
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
/// `font-family` that the persona has (directly or through an alias) and we
/// have a table for, then the generic mapping, then the standard font.
pub fn resolve_font(db: &FontDb, available: &FontAvailability, style: &ComputedStyle) -> TextFont {
    let family = pick_family(db, available, &style.font_family)
        .or_else(|| available.generic(db, "-webkit-standard"))
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
        emoji_advance: available.emoji_advance(db),
    }
}

fn pick_family<'a>(
    db: &'a FontDb,
    available: &FontAvailability,
    families: &[FamilyName],
) -> Option<&'a Rc<FontFamily>> {
    for f in families {
        if f.generic {
            if let Some(g) = available.generic(db, &f.name) {
                return Some(g);
            }
            continue;
        }
        let lower = f.name.to_ascii_lowercase();
        if matches!(lower.as_str(), "-apple-system" | "blinkmacsystemfont") {
            if db.os == Some(OsKind::MacOs)
                && let Some(g) = available.generic(db, "system-ui")
            {
                return Some(g);
            }
            continue;
        }
        if let Some(t) = available.family(db, &lower) {
            return Some(t);
        }
    }
    None
}

/// The persona's installed fonts (`fonts.available`) and font settings:
/// which family each generic keyword names (`font_defaults`), fontconfig
/// and Blink family aliases (`font_aliases`) and the emoji font. Without a
/// persona every table counts as installed and the OS defaults apply.
#[derive(Clone, Debug, Default)]
pub struct FontAvailability {
    names: std::collections::HashSet<String>,
    /// No list given: every table counts as installed.
    all: bool,
    prefs: Option<Rc<FontPrefs>>,
}

#[derive(Debug)]
struct FontPrefs {
    defaults: FontDefaults,
    /// Lowercase requested family -> family used.
    aliases: HashMap<String, String>,
}

impl FontPrefs {
    fn new(defaults: FontDefaults, aliases: Vec<(String, String)>) -> FontPrefs {
        FontPrefs {
            defaults,
            aliases: aliases
                .into_iter()
                .map(|(a, b)| (a.to_ascii_lowercase(), b))
                .collect(),
        }
    }

    /// OS defaults when there is no persona (every table installed):
    /// Blink's alternates, plus fontconfig's metric aliases on Linux.
    fn for_os(os: OsKind) -> FontPrefs {
        let aliases = lb_persona::os_font_aliases(os.to_persona())
            .into_iter()
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
            .collect();
        FontPrefs::new(os.font_defaults(), aliases)
    }
}

impl FontAvailability {
    pub fn from_list<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        FontAvailability {
            names: names.into_iter().map(|n| n.to_ascii_lowercase()).collect(),
            all: false,
            prefs: None,
        }
    }

    /// The persona's fonts: installed list, `font_defaults()` and
    /// `font_aliases()` (ADR 0015).
    pub fn for_persona(p: &PersonaProfile) -> Self {
        FontAvailability {
            prefs: Some(Rc::new(FontPrefs::new(p.font_defaults(), p.font_aliases()))),
            ..Self::from_list(p.fonts.available.iter().map(String::as_str))
        }
    }

    /// Every family with a table is available.
    pub fn everything() -> Self {
        FontAvailability {
            names: Default::default(),
            all: true,
            prefs: None,
        }
    }

    pub fn has(&self, lower_name: &str) -> bool {
        self.all || self.names.contains(lower_name)
    }

    fn with_prefs<R>(&self, db: &FontDb, f: impl FnOnce(&FontPrefs) -> R) -> R {
        match self.prefs.as_ref().or(db.os_prefs.as_ref()) {
            Some(p) => f(p),
            None => f(&FontPrefs::for_os(db.os.unwrap_or(OsKind::Windows))),
        }
    }

    /// The table for an installed family, or for the family an alias maps
    /// it to (Linux: Arial -> Liberation Sans; everywhere: Helvetica ->
    /// Arial when only Arial is installed). `lower` is lowercase.
    pub fn family<'a>(&self, db: &'a FontDb, lower: &str) -> Option<&'a Rc<FontFamily>> {
        if self.has(lower)
            && let Some(t) = db.get(lower)
        {
            return Some(t);
        }
        let target = self.with_prefs(db, |p| p.aliases.get(lower).cloned())?;
        let t = target.to_ascii_lowercase();
        if self.has(&t) { db.get(&t) } else { None }
    }

    /// The family a generic keyword resolves to: the persona's default
    /// font for it (through aliases), else the OS table list. `ui-*`
    /// keywords are generic only on macOS; elsewhere they match nothing,
    /// as in Chrome. `None` for unknown keywords.
    pub fn generic<'a>(&self, db: &'a FontDb, generic: &str) -> Option<&'a Rc<FontFamily>> {
        let mac = db.os == Some(OsKind::MacOs);
        let name = self.with_prefs(db, |p| {
            let d = &p.defaults;
            Some(match generic {
                "serif" => d.serif.clone(),
                "sans-serif" => d.sans_serif.clone(),
                "monospace" => d.monospace.clone(),
                "cursive" => d.cursive.clone(),
                "fantasy" => d.fantasy.clone(),
                "system-ui" => d.system_ui.clone(),
                "emoji" => d.emoji.clone(),
                "math" | "fangsong" | "-webkit-body" | "-webkit-standard" => d.standard.clone(),
                "ui-serif" if mac => d.serif.clone(),
                "ui-sans-serif" | "ui-rounded" if mac => d.system_ui.clone(),
                "ui-monospace" if mac => d.monospace.clone(),
                _ => return None,
            })
        })?;
        let lower = name.to_ascii_lowercase();
        self.family(db, &lower)
            // macOS `system-ui` is the table named `system-ui` (SF).
            .or_else(|| db.get(&lower).filter(|_| lower == "system-ui"))
            .or_else(|| db.generic(generic))
    }

    /// Advance in em of the persona's emoji font.
    pub fn emoji_advance(&self, db: &FontDb) -> f32 {
        self.with_prefs(db, |p| emoji_advance_em(&p.defaults.emoji))
    }

    /// The emoji font's table (vertical metrics of lines with emoji).
    pub fn emoji_family<'a>(&self, db: &'a FontDb) -> Option<&'a Rc<FontFamily>> {
        let name = self.with_prefs(db, |p| p.defaults.emoji.to_ascii_lowercase());
        db.get(&name)
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
            emoji_advance: 1.0,
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
