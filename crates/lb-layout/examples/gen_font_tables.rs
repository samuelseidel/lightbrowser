//! Generates `profiles/fonts/<os>/*.json` metric tables from local font
//! files with `skrifa`. A build-time tool only: the browser reads the JSON.
//!
//! ```text
//! cargo run -p lb-layout --example gen_font_tables -- [FONT_DIR] [OUT_DIR]
//! ```
//!
//! Defaults: `/usr/share/fonts` and `profiles/fonts`. Families we have no
//! file for use a metric-compatible substitute (exact advances, e.g.
//! Liberation Sans for Arial) or a similar design marked `approximate`, with
//! vertical metrics taken from the real font's documented values.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use skrifa::instance::{LocationRef, Size};
use skrifa::{FontRef, MetadataProvider};

/// Codepoint ranges stored in each table.
const RANGES: &[(u32, u32)] = &[
    (0x20, 0x7e),
    (0xa0, 0xff),
    (0x2013, 0x2014),
    (0x2018, 0x2019),
    (0x201c, 0x201d),
    (0x2022, 0x2022),
    (0x2026, 0x2026),
    (0x20ac, 0x20ac),
    (0x2122, 0x2122),
];

struct Face {
    file: &'static str,
    weight: u16,
    italic: bool,
}

struct Family {
    os: &'static str,
    family: &'static str,
    faces: &'static [Face],
    approximate: bool,
    source: &'static str,
    /// Documented vertical metrics of the real font: (ascent, descent,
    /// line gap) in units of `upem`, plus `upem`.
    vmetrics: Option<(i32, i32, i32, u16)>,
    /// Advance width scale applied to the substitute (approximations only).
    width_scale: f32,
}

const LIB_SANS: &[Face] = &[
    Face {
        file: "truetype/liberation/LiberationSans-Regular.ttf",
        weight: 400,
        italic: false,
    },
    Face {
        file: "truetype/liberation/LiberationSans-Bold.ttf",
        weight: 700,
        italic: false,
    },
    Face {
        file: "truetype/liberation/LiberationSans-Italic.ttf",
        weight: 400,
        italic: true,
    },
    Face {
        file: "truetype/liberation/LiberationSans-BoldItalic.ttf",
        weight: 700,
        italic: true,
    },
];
const LIB_SERIF: &[Face] = &[
    Face {
        file: "truetype/liberation/LiberationSerif-Regular.ttf",
        weight: 400,
        italic: false,
    },
    Face {
        file: "truetype/liberation/LiberationSerif-Bold.ttf",
        weight: 700,
        italic: false,
    },
    Face {
        file: "truetype/liberation/LiberationSerif-Italic.ttf",
        weight: 400,
        italic: true,
    },
    Face {
        file: "truetype/liberation/LiberationSerif-BoldItalic.ttf",
        weight: 700,
        italic: true,
    },
];
const LIB_MONO: &[Face] = &[
    Face {
        file: "truetype/liberation/LiberationMono-Regular.ttf",
        weight: 400,
        italic: false,
    },
    Face {
        file: "truetype/liberation/LiberationMono-Bold.ttf",
        weight: 700,
        italic: false,
    },
    Face {
        file: "truetype/liberation/LiberationMono-Italic.ttf",
        weight: 400,
        italic: true,
    },
    Face {
        file: "truetype/liberation/LiberationMono-BoldItalic.ttf",
        weight: 700,
        italic: true,
    },
];
const CARLITO: &[Face] = &[
    Face {
        file: "truetype/crosextra/Carlito-Regular.ttf",
        weight: 400,
        italic: false,
    },
    Face {
        file: "truetype/crosextra/Carlito-Bold.ttf",
        weight: 700,
        italic: false,
    },
    Face {
        file: "truetype/crosextra/Carlito-Italic.ttf",
        weight: 400,
        italic: true,
    },
    Face {
        file: "truetype/crosextra/Carlito-BoldItalic.ttf",
        weight: 700,
        italic: true,
    },
];
const CALADEA: &[Face] = &[
    Face {
        file: "truetype/crosextra/Caladea-Regular.ttf",
        weight: 400,
        italic: false,
    },
    Face {
        file: "truetype/crosextra/Caladea-Bold.ttf",
        weight: 700,
        italic: false,
    },
    Face {
        file: "truetype/crosextra/Caladea-Italic.ttf",
        weight: 400,
        italic: true,
    },
    Face {
        file: "truetype/crosextra/Caladea-BoldItalic.ttf",
        weight: 700,
        italic: true,
    },
];
const DEJAVU_SANS: &[Face] = &[
    Face {
        file: "truetype/dejavu/DejaVuSans.ttf",
        weight: 400,
        italic: false,
    },
    Face {
        file: "truetype/dejavu/DejaVuSans-Bold.ttf",
        weight: 700,
        italic: false,
    },
];
const DEJAVU_SERIF: &[Face] = &[
    Face {
        file: "truetype/dejavu/DejaVuSerif.ttf",
        weight: 400,
        italic: false,
    },
    Face {
        file: "truetype/dejavu/DejaVuSerif-Bold.ttf",
        weight: 700,
        italic: false,
    },
];
const DEJAVU_MONO: &[Face] = &[
    Face {
        file: "truetype/dejavu/DejaVuSansMono.ttf",
        weight: 400,
        italic: false,
    },
    Face {
        file: "truetype/dejavu/DejaVuSansMono-Bold.ttf",
        weight: 700,
        italic: false,
    },
];
const INTER: &[Face] = &[
    Face {
        file: "opentype/inter/Inter-Regular.otf",
        weight: 400,
        italic: false,
    },
    Face {
        file: "opentype/inter/Inter-Bold.otf",
        weight: 700,
        italic: false,
    },
    Face {
        file: "opentype/inter/Inter-Italic.otf",
        weight: 400,
        italic: true,
    },
    Face {
        file: "opentype/inter/Inter-BoldItalic.otf",
        weight: 700,
        italic: true,
    },
];

const EXACT_ARIAL: &str =
    "Liberation Sans (metric-compatible with Arial: same advances and vertical metrics)";
const EXACT_TNR: &str = "Liberation Serif (metric-compatible with Times New Roman)";
const EXACT_COURIER: &str = "Liberation Mono (metric-compatible with Courier New)";
const EXACT_CALIBRI: &str = "Carlito (metric-compatible with Calibri)";

fn families() -> Vec<Family> {
    let exact = |os, family, faces, source| Family {
        os,
        family,
        faces,
        approximate: false,
        source,
        vmetrics: None,
        width_scale: 1.0,
    };
    let approx = |os, family, faces, source, vmetrics, width_scale| Family {
        os,
        family,
        faces,
        approximate: true,
        source,
        vmetrics,
        width_scale,
    };
    let mut v = Vec::new();
    for os in ["windows", "macos"] {
        v.push(exact(os, "Arial", LIB_SANS, EXACT_ARIAL));
        v.push(exact(os, "Times New Roman", LIB_SERIF, EXACT_TNR));
        v.push(exact(os, "Courier New", LIB_MONO, EXACT_COURIER));
        v.push(approx(
            os,
            "Verdana",
            DEJAVU_SANS,
            "DejaVu Sans advances (Verdana-like design); Verdana's documented hhea metrics",
            Some((2059, 430, 0, 2048)),
            1.0,
        ));
        v.push(approx(
            os,
            "Tahoma",
            DEJAVU_SANS,
            "DejaVu Sans advances scaled by 0.9 (Tahoma is a narrower Verdana); Tahoma's documented hhea metrics",
            Some((2049, 423, 0, 2048)),
            0.9,
        ));
        v.push(approx(
            os,
            "Georgia",
            DEJAVU_SERIF,
            "DejaVu Serif advances; Georgia's documented hhea metrics",
            Some((1878, 449, 0, 2048)),
            1.0,
        ));
    }
    v.push(exact("windows", "Calibri", CARLITO, EXACT_CALIBRI));
    v.push(approx(
        "windows",
        "Segoe UI",
        LIB_SANS,
        "Arial-compatible advances (no Segoe UI file); Segoe UI's documented hhea metrics",
        Some((2210, 514, 0, 2048)),
        1.0,
    ));
    v.push(approx(
        "macos",
        "Helvetica",
        LIB_SANS,
        "Arial-compatible advances (Arial copies Helvetica's widths); vertical metrics are Arial's",
        None,
        1.0,
    ));
    v.push(approx(
        "macos",
        "Helvetica Neue",
        LIB_SANS,
        "Arial-compatible advances; vertical metrics are Arial's",
        None,
        1.0,
    ));
    v.push(approx(
        "macos",
        "Times",
        LIB_SERIF,
        "Times New Roman-compatible advances",
        None,
        1.0,
    ));
    v.push(approx(
        "macos",
        "Courier",
        LIB_MONO,
        "Courier New-compatible advances",
        None,
        1.0,
    ));
    v.push(approx(
        "macos",
        "system-ui",
        INTER,
        "Inter (similar to SF Pro, which has no local file)",
        None,
        1.0,
    ));
    for (family, faces) in [
        ("Liberation Sans", LIB_SANS),
        ("Liberation Serif", LIB_SERIF),
        ("Liberation Mono", LIB_MONO),
        ("DejaVu Sans", DEJAVU_SANS),
        ("DejaVu Serif", DEJAVU_SERIF),
        ("DejaVu Sans Mono", DEJAVU_MONO),
        // M7: fontconfig's Calibri and Cambria aliases (SIL OFL files).
        ("Carlito", CARLITO),
        ("Caladea", CALADEA),
    ] {
        v.push(exact("linux", family, faces, "local font file"));
    }
    v
}

/// Face JSON, units per em and (ascent, descent, line gap).
type FaceTable = (String, u16, (f32, f32, f32));

fn slug(family: &str) -> String {
    family.to_ascii_lowercase().replace(' ', "_")
}

fn face_json(path: &Path, face: &Face, scale: f32) -> Result<FaceTable, String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let font = FontRef::new(&data).map_err(|e| format!("{}: {e}", path.display()))?;
    let metrics = font.metrics(Size::unscaled(), LocationRef::default());
    let glyphs = font.glyph_metrics(Size::unscaled(), LocationRef::default());
    let charmap = font.charmap();
    let avg = metrics
        .average_width
        .unwrap_or(metrics.units_per_em as f32 * 0.5)
        * scale;
    let mut out = String::new();
    write!(
        out,
        "    {{\"weight\": {}, \"style\": \"{}\", \"default_advance\": {}, \"ranges\": [",
        face.weight,
        if face.italic { "italic" } else { "normal" },
        avg.round() as i32
    )
    .unwrap();
    for (i, (lo, hi)) in RANGES.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write!(out, "[{lo}, [").unwrap();
        for (j, cp) in (*lo..=*hi).enumerate() {
            if j > 0 {
                out.push(',');
            }
            let adv = charmap
                .map(cp)
                .and_then(|g| glyphs.advance_width(g))
                .map(|a| a * scale)
                .unwrap_or(avg);
            write!(out, "{}", adv.round() as i32).unwrap();
        }
        out.push_str("]]");
    }
    out.push_str("]}");
    Ok((
        out,
        metrics.units_per_em,
        (metrics.ascent, -metrics.descent, metrics.leading),
    ))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let font_dir = PathBuf::from(args.next().unwrap_or_else(|| "/usr/share/fonts".into()));
    let out_dir = PathBuf::from(args.next().unwrap_or_else(|| "profiles/fonts".into()));
    for fam in families() {
        let mut faces = Vec::new();
        let mut upem = 0;
        let mut vm = (0.0, 0.0, 0.0);
        let mut ok = true;
        for face in fam.faces {
            match face_json(&font_dir.join(face.file), face, fam.width_scale) {
                Ok((json, u, m)) => {
                    if faces.is_empty() {
                        upem = u;
                        vm = m;
                    }
                    faces.push(json);
                }
                Err(e) => {
                    eprintln!("skip {} ({}): {e}", fam.family, fam.os);
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        // Advances are in the substitute's units; documented vertical
        // metrics are rescaled to them.
        let (ascent, descent, gap) = match fam.vmetrics {
            Some((a, d, g, u)) => {
                let k = upem as f32 / u as f32;
                (a as f32 * k, d as f32 * k, g as f32 * k)
            }
            None => vm,
        };
        let json = format!(
            "{{\n  \"format\": 1,\n  \"family\": {:?},\n  \"os\": {:?},\n  \"approximate\": {},\n  \
             \"source\": {:?},\n  \"units_per_em\": {},\n  \"ascent\": {},\n  \"descent\": {},\n  \
             \"line_gap\": {},\n  \"faces\": [\n{}\n  ]\n}}\n",
            fam.family,
            fam.os,
            fam.approximate,
            fam.source,
            upem,
            ascent.round() as i32,
            descent.round() as i32,
            gap.round() as i32,
            faces.join(",\n"),
        );
        let dir = out_dir.join(fam.os);
        std::fs::create_dir_all(&dir).expect("create output dir");
        let path = dir.join(format!("{}.json", slug(fam.family)));
        std::fs::write(&path, json).expect("write table");
        println!("wrote {}", path.display());
    }
}
