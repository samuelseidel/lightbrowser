//! Release link options for binary size (M6, ADR 0013). Only the final
//! link of `lightbrowser` changes; no dependency is rebuilt.
//!
//! - `--icf=safe`: fold identical functions whose address is not taken
//!   (mostly V8 template instances): about -5 MB.
//! - `-z pack-relative-relocs`: DT_RELR instead of RELA for the PIE's
//!   relative relocations: about -2 MB. Needs glibc >= 2.36 at run time.
//!
//! Both need lld, the default linker of `x86_64-unknown-linux-gnu` since
//! Rust 1.90. Set `LB_PLAIN_LINK=1` to link without them.

fn main() {
    println!("cargo:rerun-if-env-changed=LB_PLAIN_LINK");
    let release = std::env::var("PROFILE").is_ok_and(|p| p == "release");
    let target = std::env::var("TARGET").unwrap_or_default();
    if release
        && target == "x86_64-unknown-linux-gnu"
        && std::env::var_os("LB_PLAIN_LINK").is_none()
    {
        println!("cargo:rustc-link-arg-bins=-Wl,--icf=safe");
        println!("cargo:rustc-link-arg-bins=-Wl,-z,pack-relative-relocs");
    }
}
