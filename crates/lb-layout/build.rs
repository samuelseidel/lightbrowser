//! Packs the bundled font metric tables into one zstd frame (M6 binary
//! size, ADR 0013). `src/fonts.rs` includes the generated
//! `font_tables.rs` (the byte range of every table) and the frame.

use std::fmt::Write as _;
use std::path::PathBuf;

/// Bundled tables (OS directory, path under `profiles/fonts/`), in the
/// order `FontDb::for_os` loads them.
const TABLES: &[(&str, &str)] = &[
    ("windows", "windows/arial.json"),
    ("windows", "windows/times_new_roman.json"),
    ("windows", "windows/courier_new.json"),
    ("windows", "windows/verdana.json"),
    ("windows", "windows/tahoma.json"),
    ("windows", "windows/georgia.json"),
    ("windows", "windows/calibri.json"),
    ("windows", "windows/segoe_ui.json"),
    ("macos", "macos/arial.json"),
    ("macos", "macos/times_new_roman.json"),
    ("macos", "macos/courier_new.json"),
    ("macos", "macos/verdana.json"),
    ("macos", "macos/tahoma.json"),
    ("macos", "macos/georgia.json"),
    ("macos", "macos/helvetica.json"),
    ("macos", "macos/helvetica_neue.json"),
    ("macos", "macos/times.json"),
    ("macos", "macos/courier.json"),
    ("macos", "macos/system-ui.json"),
    ("linux", "linux/liberation_sans.json"),
    ("linux", "linux/liberation_serif.json"),
    ("linux", "linux/liberation_mono.json"),
    ("linux", "linux/dejavu_sans.json"),
    ("linux", "linux/dejavu_serif.json"),
    ("linux", "linux/dejavu_sans_mono.json"),
    ("linux", "linux/carlito.json"),
    ("linux", "linux/caladea.json"),
];

/// Approximate tables for persona fonts without their own table.
const DERIVED: &str = "derived.json";

fn main() {
    let manifest =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let fonts = manifest.join("../../profiles/fonts");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let mut all = String::new();
    let mut index = String::from(
        "/// Bundled tables: OS directory and byte range in the unpacked JSON.\n         const BUNDLED: &[(&str, std::ops::Range<usize>)] = &[\n",
    );
    let add = |rel: &str, all: &mut String| {
        let path = fonts.join(rel);
        println!("cargo:rerun-if-changed={}", path.display());
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let start = all.len();
        all.push_str(&text);
        start..all.len()
    };
    for (dir, file) in TABLES {
        let r = add(file, &mut all);
        writeln!(index, "    ({dir:?}, {}..{}),", r.start, r.end).unwrap();
    }
    index.push_str("];\n");
    let r = add(DERIVED, &mut all);
    writeln!(
        index,
        "/// `derived.json`: byte range in the unpacked JSON.\n         const DERIVED: std::ops::Range<usize> = {}..{};\n         /// Length of the unpacked JSON.\n         const PACKED_TABLES_LEN: usize = {};\n         /// All tables, one zstd frame.\n         const PACKED_TABLES: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/font_tables.zst\"));",
        r.start,
        r.end,
        all.len()
    )
    .unwrap();
    let packed = zstd::bulk::compress(all.as_bytes(), 19).expect("compress font tables");
    std::fs::write(out.join("font_tables.zst"), packed).expect("write font_tables.zst");
    std::fs::write(out.join("font_tables.rs"), index).expect("write font_tables.rs");
    pack_local_fonts(&manifest.join("../../profiles/local_fonts"), &out);
}

/// Packs `profiles/local_fonts/*.json` (the `local()` face names of each
/// OS release) into one zstd frame, like the metric tables: `src/local_fonts.rs`
/// includes `local_fonts.rs` (list id and byte range of each file).
fn pack_local_fonts(dir: &std::path::Path, out: &std::path::Path) {
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.expect("local_fonts entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let mut all = String::new();
    let mut index = String::from(
        "/// Bundled `local()` lists: id (file stem) and byte range in the unpacked JSON.\n         const LOCAL_LISTS: &[(&str, std::ops::Range<usize>)] = &[\n",
    );
    for path in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        let text =
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("file stem");
        let start = all.len();
        all.push_str(&text);
        writeln!(index, "    ({id:?}, {start}..{}),", all.len()).unwrap();
    }
    writeln!(
        index,
        "];\n         /// Length of the unpacked JSON.\n         const PACKED_LOCAL_LEN: usize = {};\n         /// All lists, one zstd frame.\n         const PACKED_LOCAL: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/local_fonts.zst\"));",
        all.len()
    )
    .unwrap();
    let packed = zstd::bulk::compress(all.as_bytes(), 19).expect("compress local font lists");
    std::fs::write(out.join("local_fonts.zst"), packed).expect("write local_fonts.zst");
    std::fs::write(out.join("local_fonts.rs"), index).expect("write local_fonts.rs");
}
