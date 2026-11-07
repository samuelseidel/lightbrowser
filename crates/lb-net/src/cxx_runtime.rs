//! The one C++ runtime function BoringSSL needs from the system's
//! libstdc++ (M6 start-up cost, ADR 0013).
//!
//! `libcrypto.a` is C except `by_dir.cc` (the X509 hash-directory
//! lookup), whose bounds-checked `std::vector` access references
//! `std::__throw_out_of_range_fmt`. That single symbol made the binary
//! load `libstdc++.so.6`: about 1.5 MB of mapped pages, its relocations
//! and static initializers in every process. Defined here, the linker
//! resolves it locally and (`--as-needed`) drops the library. V8 brings
//! its own C++ library, linked statically.
//!
//! BoringSSL is built without C++ exceptions, so in libstdc++ the throw
//! could only end in `std::terminate`; this ends the same way. LightBrowser
//! never uses a certificate directory, so it is not reached.

/// `std::__throw_out_of_range_fmt(const char* fmt, ...)`. The variadic
/// arguments are never read, so the plain C ABI matches the callers.
// SAFETY (`unsafe` attribute): a libstdc++ symbol nothing else in the
// binary defines (a second definition would fail the link).
#[unsafe(export_name = "_ZSt24__throw_out_of_range_fmtPKcz")]
extern "C" fn throw_out_of_range_fmt(_fmt: *const std::ffi::c_char) -> ! {
    eprintln!("fatal: C++ std::out_of_range in BoringSSL");
    std::process::abort()
}
