//! Builds the canvas glyph outline packs in `profiles/fonts/outlines/` from
//! font files with `skrifa`. A build-time tool only: the browser reads the
//! packs, never font files.
//!
//! ```bash
//! cargo run -p lb-paint --example gen_glyph_pack -- \
//!     /usr/share/fonts/truetype/liberation profiles/fonts/outlines
//! ```
//!
//! Pack format (`LBGP` version 1, little-endian):
//!
//! - header: magic `LBGP`, `u16` version, `u16` units per em, `u16` glyph
//!   count;
//! - one entry per glyph: `u32` codepoint (0 = `.notdef`), `u16` advance,
//!   `i16` x min, y min, x max, y max (font units), `u32` offset and `u32`
//!   length of its commands;
//! - commands: a tag byte (0 move, 1 line, 2 quad, 3 cubic, 4 close)
//!   followed by `i16` coordinates in half font units (y up).

use std::path::{Path, PathBuf};

use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};

/// Codepoint ranges stored in each pack (same as the metric tables).
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

/// (pack name, font file). Pack names avoid the fonts' reserved names
/// (SIL OFL 1.1 section 3): a pack is a derived work.
const PACKS: &[(&str, &str)] = &[
    ("sans-regular", "LiberationSans-Regular.ttf"),
    ("sans-bold", "LiberationSans-Bold.ttf"),
    ("serif-regular", "LiberationSerif-Regular.ttf"),
    ("serif-bold", "LiberationSerif-Bold.ttf"),
    ("mono-regular", "LiberationMono-Regular.ttf"),
    ("mono-bold", "LiberationMono-Bold.ttf"),
];

#[derive(Default)]
struct Pen {
    out: Vec<u8>,
}

impl Pen {
    fn pt(&mut self, x: f32, y: f32) {
        for v in [x, y] {
            let h = (v * 2.0).round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            self.out.extend_from_slice(&h.to_le_bytes());
        }
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.out.push(0);
        self.pt(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.out.push(1);
        self.pt(x, y);
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.out.push(2);
        self.pt(cx0, cy0);
        self.pt(x, y);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.out.push(3);
        self.pt(cx0, cy0);
        self.pt(cx1, cy1);
        self.pt(x, y);
    }
    fn close(&mut self) {
        self.out.push(4);
    }
}

fn build(path: &Path) -> Result<Vec<u8>, String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let font = FontRef::new(&data).map_err(|e| format!("{}: {e}", path.display()))?;
    let upem = font
        .metrics(Size::unscaled(), LocationRef::default())
        .units_per_em;
    let charmap = font.charmap();
    let gm = font.glyph_metrics(Size::unscaled(), LocationRef::default());
    let outlines = font.outline_glyphs();
    let mut glyphs: Vec<(u32, GlyphId)> = vec![(0, GlyphId::new(0))];
    for &(a, b) in RANGES {
        for cp in a..=b {
            if let Some(g) = char::from_u32(cp).and_then(|c| charmap.map(c)) {
                glyphs.push((cp, g));
            }
        }
    }
    let mut index = Vec::new();
    let mut cmds = Vec::new();
    for (cp, gid) in &glyphs {
        let mut pen = Pen::default();
        if let Some(o) = outlines.get(*gid) {
            o.draw(
                DrawSettings::unhinted(Size::unscaled(), LocationRef::default()),
                &mut pen,
            )
            .map_err(|e| format!("glyph {cp:#x}: {e:?}"))?;
        }
        let adv = gm.advance_width(*gid).unwrap_or(0.0).round() as u16;
        let b = gm.bounds(*gid);
        let (x0, y0, x1, y1) = b.map_or((0, 0, 0, 0), |b| {
            (
                b.x_min.floor() as i16,
                b.y_min.floor() as i16,
                b.x_max.ceil() as i16,
                b.y_max.ceil() as i16,
            )
        });
        index.extend_from_slice(&cp.to_le_bytes());
        index.extend_from_slice(&adv.to_le_bytes());
        for v in [x0, y0, x1, y1] {
            index.extend_from_slice(&v.to_le_bytes());
        }
        index.extend_from_slice(&(cmds.len() as u32).to_le_bytes());
        index.extend_from_slice(&(pen.out.len() as u32).to_le_bytes());
        cmds.extend_from_slice(&pen.out);
    }
    let mut out = b"LBGP".to_vec();
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&upem.to_le_bytes());
    out.extend_from_slice(&(glyphs.len() as u16).to_le_bytes());
    out.extend_from_slice(&index);
    out.extend_from_slice(&cmds);
    Ok(out)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [src, dst] = args.as_slice() else {
        eprintln!("usage: gen_glyph_pack <font dir> <output dir>");
        std::process::exit(2);
    };
    let dst = PathBuf::from(dst);
    std::fs::create_dir_all(&dst).expect("create output dir");
    let mut total = 0;
    for (name, file) in PACKS {
        let bytes = build(&Path::new(src).join(file)).unwrap_or_else(|e| panic!("{e}"));
        let out = dst.join(format!("{name}.lbgp"));
        std::fs::write(&out, &bytes).expect("write pack");
        total += bytes.len();
        println!("{} ({} bytes)", out.display(), bytes.len());
    }
    println!("total {total} bytes");
}
