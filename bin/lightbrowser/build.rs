//! Release link options for binary size and memory (M6, ADR 0013 and
//! 0016). Only the final link of `lightbrowser` changes; no dependency is
//! rebuilt.
//!
//! - `--icf=safe`: fold identical functions whose address is not taken
//!   (mostly V8 template instances): about -5 MB.
//! - `-z pack-relative-relocs`: DT_RELR instead of RELA for the PIE's
//!   relative relocations: about -2 MB. Needs glibc >= 2.36 at run time.
//! - No unwind tables: `.eh_frame` (and so `.eh_frame_hdr`) is discarded
//!   by a linker script inserted into the default layout: -3.6 MB. Nothing
//!   in the release binary unwinds: Rust builds with `panic = "abort"`,
//!   V8, ICU, libc++ and BoringSSL are built without C++ exceptions, and
//!   no thread is cancelled. What is lost is stack walking for crash
//!   reports (Rust `RUST_BACKTRACE`, V8's fatal-error "C stack trace",
//!   gdb without debug info), which the stripped binary has no symbols
//!   for anyway. Set `LB_KEEP_UNWIND=1` to keep them.
//! - Hot code first: `--symbol-ordering-file=hot-symbols.txt` puts the
//!   functions a page load runs (made by `tools/hot_symbols/gen.py` from a
//!   training workload, not the bench corpus) at the start of `.text`, so
//!   they share pages instead of being spread over ~29 MB of code. The
//!   kernel maps the 64 KB around each code page fault, so a scattered hot
//!   set costs resident memory and faults: `serve` crawling the bench
//!   corpus -4 to -5 MB RSS (ADR 0016). Symbols a build lacks are ignored
//!   (`--no-warn-symbol-ordering`); a stale list only costs locality.
//!   `LB_NO_ORDER=1` links without it.
//!
//! All need lld, the default linker of `x86_64-unknown-linux-gnu` since
//! Rust 1.90. Set `LB_PLAIN_LINK=1` to link without any of them. For size
//! work, `LB_LINK_MAP=<path>` also writes lld's link map there (about
//! 60 MB; only the final link reruns).

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=LB_PLAIN_LINK");
    println!("cargo:rerun-if-env-changed=LB_KEEP_UNWIND");
    println!("cargo:rerun-if-env-changed=LB_LINK_MAP");
    println!("cargo:rerun-if-env-changed=LB_NO_ORDER");
    println!("cargo:rerun-if-changed=hot-symbols.txt");
    let release = std::env::var("PROFILE").is_ok_and(|p| p == "release");
    let target = std::env::var("TARGET").unwrap_or_default();
    if !release
        || target != "x86_64-unknown-linux-gnu"
        || std::env::var_os("LB_PLAIN_LINK").is_some()
    {
        return;
    }
    if let Some(map) = std::env::var_os("LB_LINK_MAP") {
        // Size work only: write a link map to this path.
        println!(
            "cargo:rustc-link-arg-bins=-Wl,-Map={}",
            map.to_string_lossy()
        );
    }
    let order = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"))
        .join("hot-symbols.txt");
    if std::env::var_os("LB_NO_ORDER").is_none() && order.is_file() {
        println!(
            "cargo:rustc-link-arg-bins=-Wl,--symbol-ordering-file={}",
            order.display()
        );
        println!("cargo:rustc-link-arg-bins=-Wl,--no-warn-symbol-ordering");
    }
    println!("cargo:rustc-link-arg-bins=-Wl,--icf=safe");
    println!("cargo:rustc-link-arg-bins=-Wl,-z,pack-relative-relocs");
    if std::env::var_os("LB_KEEP_UNWIND").is_none() {
        // `INSERT AFTER` keeps lld's default layout for everything else.
        let script = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"))
            .join("discard-eh-frame.ld");
        std::fs::write(
            &script,
            "SECTIONS { /DISCARD/ : { *(.eh_frame) } } INSERT AFTER .text;\n",
        )
        .expect("write linker script");
        println!("cargo:rustc-link-arg-bins=-Wl,-T,{}", script.display());
    }
}
