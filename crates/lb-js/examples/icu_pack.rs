//! Writes `crates/lb-js/icu/icudt78.dat.zst`: the ICU data embedded in the
//! prebuilt V8 archive, zstd-compressed (ADR 0013). Run after a `v8`
//! upgrade:
//!
//! ```text
//! cargo run --release -p lb-js --example icu_pack [-- path/to/librusty_v8.a]
//! ```
//!
//! The `icu_data` tests in `tests/v8.rs` check the checked-in file against
//! the archive.

use std::path::PathBuf;

fn main() {
    let archive = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| lb_js::icu_data::find_archive(&std::env::current_exe().expect("exe")))
        .expect("librusty_v8.a not found; pass its path");
    let ar = std::fs::read(&archive).expect("read archive");
    let data = lb_js::icu_data::extract_from_archive(&ar).expect("extract ICU data");
    let mut c = zstd::bulk::Compressor::new(22).expect("compressor");
    // A window that covers the whole package (it is ~10.4 MiB), within the
    // decoder's default limit (2^27).
    c.set_parameter(zstd::zstd_safe::CParameter::WindowLog(24))
        .expect("window");
    c.include_contentsize(true).expect("content size");
    let packed = c.compress(&data).expect("compress");
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("icu/icudt78.dat.zst");
    std::fs::write(&out, &packed).expect("write");
    println!(
        "{}: {} -> {} bytes ({})",
        archive.display(),
        data.len(),
        packed.len(),
        out.display()
    );
}
