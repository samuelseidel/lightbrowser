//! `lightbrowser mcp` in the firefox build (ADR 0051): there is no JS
//! engine to drive yet (M4b), so the command refuses with a message and
//! exit code 1, before it reads stdin or writes to stdout.
#![cfg(not(feature = "chrome"))]

use std::process::{Command, Stdio};

#[test]
fn mcp_refuses_without_a_js_engine() {
    let out = Command::new(env!("CARGO_BIN_EXE_lightbrowser"))
        .arg("mcp")
        .stdin(Stdio::null())
        .output()
        .expect("run lightbrowser mcp");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        out.stdout.is_empty(),
        "nothing on stdout but protocol messages: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(
            "error: lightbrowser mcp needs the chrome build: the firefox build has no JS engine yet (M4b)"
        ),
        "{err}"
    );
    // Options are still checked first: a bad one is a usage error (2).
    let out = Command::new(env!("CARGO_BIN_EXE_lightbrowser"))
        .args(["mcp", "--bogus"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stdout.is_empty());
}
