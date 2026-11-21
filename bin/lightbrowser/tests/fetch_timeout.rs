//! `fetch --timeout SECS` bounds the whole load, the wait for the response
//! included (GAP-095): against a loopback server that accepts and never
//! answers, the old binary hung until killed. Loopback only.

use std::io::Read;
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn timeout_covers_a_server_that_never_answers() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for s in listener.incoming().flatten() {
            // Keep each connection open and silent.
            held.push(s);
        }
    });
    let t0 = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightbrowser"))
        .args(["fetch", &format!("http://127.0.0.1:{port}/")])
        .args(["--timeout", "2", "--egress", "loopback"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run lightbrowser");
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break Some(s);
        }
        if t0.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut err = String::new();
    let _ = child.stderr.take().unwrap().read_to_string(&mut err);
    let status = status.unwrap_or_else(|| panic!("still running after 20 s; stderr: {err}"));
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    assert_eq!(status.code(), Some(1), "stderr: {err}");
    assert!(
        err.contains("timed out after 2 s waiting for a response from http://127.0.0.1:"),
        "stderr: {err}"
    );
}
