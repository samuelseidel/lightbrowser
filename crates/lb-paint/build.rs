//! Compresses the bundled glyph outline packs with zstd (M6 binary size,
//! ADR 0013). `src/font.rs` includes the generated `glyph_packs.rs`.

use std::fmt::Write as _;
use std::path::PathBuf;

/// Packs under `profiles/fonts/outlines/`, in `font.rs` index order
/// (class * 2 + bold).
const PACKS: [&str; 6] = [
    "sans-regular.lbgp",
    "sans-bold.lbgp",
    "serif-regular.lbgp",
    "serif-bold.lbgp",
    "mono-regular.lbgp",
    "mono-bold.lbgp",
];

fn main() {
    let manifest =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let dir = manifest.join("../../profiles/fonts/outlines");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let mut rs = String::from(
        "/// Glyph outline packs: one zstd frame each, and the unpacked length.\n\
         const PACKS: [(&[u8], usize); 6] = [\n",
    );
    for name in PACKS {
        let path = dir.join(name);
        println!("cargo:rerun-if-changed={}", path.display());
        let raw = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let packed = zstd::bulk::compress(&raw, 19).expect("compress glyph pack");
        let file = format!("{name}.zst");
        std::fs::write(out.join(&file), packed).expect("write glyph pack");
        writeln!(
            rs,
            "    (include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{file}\")), {}),",
            raw.len()
        )
        .unwrap();
    }
    rs.push_str("];\n");
    std::fs::write(out.join("glyph_packs.rs"), rs).expect("write glyph_packs.rs");
}
