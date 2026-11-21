//! Test support for lb-net's integration tests (`#[macro_use] mod common;`):
//! one standard way to skip a test whose environment is missing (root,
//! iptables, IPv6, a default route).
//!
//! A skipped test counts as passed, so it must not be quiet:
//!
//! ```ignore
//! if let Err(why) = can_capture() {
//!     lb_skip!("{why}");
//!     return;
//! }
//! ```
//!
//! prints one line, `LB-SKIP <binary>::<test>: <reason>`, to the real
//! stderr. libtest does not capture that stream, so a plain `cargo test` log
//! shows the line without `--nocapture`; `tools/ci/skip_summary.sh` counts
//! the lines per reason. With `LB_REQUIRE_NO_SKIPS=1` the same call panics
//! instead, so a machine that is meant to have the environment (a root CI
//! job) fails rather than pass with nothing checked.
#![allow(dead_code)]

use std::io::Write;

/// `LB_REQUIRE_NO_SKIPS` as set in the environment: anything but empty or `0`.
fn strict_from(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// The line for `test` (the thread libtest named after the test) in `module`.
fn skip_line(module: &str, test: Option<&str>, reason: &str) -> String {
    let test = test.filter(|t| *t != "main").unwrap_or("?");
    format!("LB-SKIP {module}::{test}: {reason}")
}

/// Write the skip line to `out`, or panic with it when `strict`.
fn emit(strict: bool, out: &mut impl Write, line: &str) {
    assert!(
        !strict,
        "{line} (LB_REQUIRE_NO_SKIPS=1: this test must run, not skip)"
    );
    // A closed stderr must not turn a skip into a failure.
    let _ = writeln!(out, "{line}");
}

/// Skip the calling test: see the module comment. The caller returns right
/// after. Use through [`lb_skip!`], which names the binary.
pub fn skip(module: &str, reason: &str) {
    let strict = strict_from(std::env::var("LB_REQUIRE_NO_SKIPS").ok().as_deref());
    let thread = std::thread::current();
    let line = skip_line(module, thread.name(), reason);
    emit(strict, &mut std::io::stderr(), &line);
}

/// `lb_skip!("reason {x}"); return;` in a test body.
macro_rules! lb_skip {
    ($($arg:tt)*) => {
        $crate::common::skip(module_path!(), &format!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_names_binary_test_and_reason() {
        assert_eq!(
            skip_line("tcp_fingerprint", Some("ip_id"), "no root"),
            "LB-SKIP tcp_fingerprint::ip_id: no root"
        );
        assert_eq!(
            skip_line("egress", None, "no IPv6"),
            "LB-SKIP egress::?: no IPv6"
        );
    }

    #[test]
    fn only_a_real_value_turns_on_strict_mode() {
        assert!(strict_from(Some("1")));
        assert!(strict_from(Some("true")));
        assert!(!strict_from(Some("0")));
        assert!(!strict_from(Some("")));
        assert!(!strict_from(None));
    }

    #[test]
    fn a_skip_writes_one_line_and_returns() {
        let mut out = Vec::new();
        emit(false, &mut out, "LB-SKIP a::b: why");
        assert_eq!(String::from_utf8(out).unwrap(), "LB-SKIP a::b: why\n");
    }

    #[test]
    #[should_panic(expected = "LB-SKIP a::b: why (LB_REQUIRE_NO_SKIPS=1")]
    fn strict_mode_panics_instead_of_writing() {
        emit(true, &mut Vec::new(), "LB-SKIP a::b: why");
    }
}
