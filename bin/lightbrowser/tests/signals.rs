//! `serve` and `mcp` as services: SIGTERM/SIGINT stop them (exit 0), a
//! stuck page cannot keep them alive past the shutdown deadline, a closed
//! stderr does not abort them, and `--version --verbose` adds the commit
//! line. Loopback only; every port is picked by the OS.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, ChildStderr, Command, Stdio};
use std::time::{Duration, Instant};

fn repo() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Reads stderr until a line contains `marker` and returns that line and
/// the reader (dropping it closes our end of the pipe).
fn read_until(child: &mut Child, marker: &str) -> (String, BufReader<ChildStderr>) {
    let mut r = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            let _ = child.kill();
            panic!("no {marker:?} line on stderr");
        }
        if line.contains(marker) {
            return (line.trim().to_string(), r);
        }
    }
}

/// [`read_until`], then drains the rest of stderr on a thread.
#[cfg(feature = "chrome")]
fn wait_for_line(child: &mut Child, marker: &str) -> String {
    let (line, mut r) = read_until(child, marker);
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = r.read_to_end(&mut sink);
    });
    line
}

/// Waits up to `limit` for the child to exit; kills it and panics if not.
fn wait_exit(child: &mut Child, limit: Duration, what: &str) -> std::process::ExitStatus {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("still running {limit:?} after {what}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn signal(child: &Child, sig: libc::c_int) {
    // SAFETY: plain kill(2) on our own child.
    assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, sig) }, 0);
}

/// Sends `sig` and expects exit status 0 within 3 s.
fn stop_with(mut child: Child, sig: libc::c_int) {
    signal(&child, sig);
    let status = wait_exit(&mut child, Duration::from_secs(3), "the signal");
    assert_eq!(status.code(), Some(0), "exit status {status:?}");
}

/// The port in a "DevTools listening on ws://127.0.0.1:PORT/..." line.
fn port_of(line: &str) -> u16 {
    let rest = line.split("ws://127.0.0.1:").nth(1).expect(line);
    rest.split('/').next().unwrap().parse().expect(line)
}

/// `serve --port 0 ...extra`; returns the child, its port and stderr
/// (unread past the "DevTools listening" line).
fn spawn_serve(extra: &[&str]) -> (Child, u16, BufReader<ChildStderr>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightbrowser"))
        .args(["serve", "--port", "0", "--egress", "loopback"])
        .args(extra)
        .current_dir(repo())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (line, r) = read_until(&mut child, "DevTools listening");
    (child, port_of(&line), r)
}

fn serve() -> (Child, u16) {
    let (child, port, mut r) = spawn_serve(&[]);
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = r.read_to_end(&mut sink);
    });
    (child, port)
}

/// One HTTP request to the CDP server; the whole response.
fn http(port: u16, req: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

fn new_page(port: u16, url: &str) -> serde_json::Value {
    let resp = http(
        port,
        &format!("PUT /json/new?{url} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"),
    );
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    serde_json::from_str(resp.split_once("\r\n\r\n").unwrap().1).unwrap()
}

/// Opens `ws://127.0.0.1:port{path}` and sends one text frame (the test's
/// own minimal client: payloads under 126 bytes, zero mask key).
#[cfg(feature = "chrome")]
fn ws_send(port: u16, path: &str, text: &str) -> TcpStream {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(
        format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        .as_bytes(),
    )
    .unwrap();
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut b).unwrap();
        head.push(b[0]);
    }
    assert!(
        head.starts_with(b"HTTP/1.1 101"),
        "{}",
        String::from_utf8_lossy(&head)
    );
    assert!(text.len() < 126);
    let mut frame = vec![0x81, 0x80 | text.len() as u8, 0, 0, 0, 0];
    frame.extend_from_slice(text.as_bytes());
    s.write_all(&frame).unwrap();
    s
}

#[test]
fn serve_exits_0_on_sigterm() {
    let (child, port) = serve();
    // A page, so shutdown has something to close.
    new_page(port, "about:blank");
    stop_with(child, libc::SIGTERM);
}

#[test]
fn serve_exits_0_on_sigint() {
    let (child, _) = serve();
    stop_with(child, libc::SIGINT);
}

// The Firefox build's `serve` uses `MockHost`: no page runs a script or loads
// a URL there, so the next two tests mean something only with the real host.
#[cfg(feature = "chrome")]
#[test]
fn a_page_stuck_in_a_script_cannot_outlive_the_deadline() {
    let (mut child, port) = serve();
    let t = new_page(port, "about:blank");
    let id = t["id"].as_str().unwrap();
    let _ws = ws_send(
        port,
        &format!("/devtools/page/{id}"),
        r#"{"id":1,"method":"Runtime.evaluate","params":{"expression":"for(;;){}"}}"#,
    );
    std::thread::sleep(Duration::from_millis(500));
    // One signal only: the shutdown deadline (5 s) has to end it.
    signal(&child, libc::SIGTERM);
    wait_exit(&mut child, Duration::from_secs(8), "one SIGTERM");
}

/// A loopback page whose cookie is refused (another domain), which makes
/// lb-net log at debug level.
#[cfg(feature = "chrome")]
fn cookie_server() -> u16 {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let body = "<!doctype html><title>c</title><p>c</p>";
            let _ = s.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nSet-Cookie: a=b; Domain=other.example\r\n\
Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    port
}

#[cfg(feature = "chrome")]
#[test]
fn closed_stderr_does_not_abort_serve() {
    let site = cookie_server();
    let (mut child, port, stderr) = spawn_serve(&["--log", "debug"]);
    // The launcher read its line and went away.
    drop(stderr);
    new_page(port, &format!("http://127.0.0.1:{site}/first"));
    std::thread::sleep(Duration::from_millis(500));
    // Still alive, still answering, still loading pages.
    let v = http(
        port,
        "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
    );
    assert!(v.starts_with("HTTP/1.1 200"), "{v}");
    let second = format!("http://127.0.0.1:{site}/second");
    new_page(port, &second);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let list = http(port, "GET /json/list HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        if list.contains(&second) && list.contains("\"title\": \"c\"") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "second page never loaded: {list}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(child.try_wait().unwrap().is_none(), "serve exited");
    stop_with(child, libc::SIGTERM);
}

/// `mcp` with stdout piped; returns the child, its stdin and a reader of
/// its replies.
#[cfg(feature = "chrome")]
fn spawn_mcp() -> (
    Child,
    std::process::ChildStdin,
    BufReader<std::process::ChildStdout>,
) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightbrowser"))
        .args(["mcp", "--timeout", "60"])
        .current_dir(repo())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_line(&mut child, "lightbrowser mcp");
    let stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    (child, stdin, stdout)
}

#[cfg(feature = "chrome")]
fn reply(out: &mut BufReader<std::process::ChildStdout>) -> String {
    let mut line = String::new();
    out.read_line(&mut line).unwrap();
    line
}

#[cfg(feature = "chrome")]
#[test]
fn mcp_exits_0_on_sigterm() {
    let (child, mut stdin, mut out) = spawn_mcp();
    // A reply means the loop (and so the handler) is up.
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"ping"}}"#).unwrap();
    assert!(reply(&mut out).contains("\"id\":1"));
    stop_with(child, libc::SIGTERM);
}

#[cfg(feature = "chrome")]
#[test]
fn mcp_stops_during_a_tool_call() {
    let (child, mut stdin, mut out) = spawn_mcp();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{{}},"clientInfo":{{"name":"t","version":"1"}}}}}}"#
    )
    .unwrap();
    assert!(reply(&mut out).contains("\"id\":1"));
    // A tool call that never finishes on its own (--timeout is 60 s).
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"browser_evaluate","arguments":{{"expression":"(()=>{{for(;;){{}}}})()"}}}}}}"#
    )
    .unwrap();
    std::thread::sleep(Duration::from_secs(1));
    stop_with(child, libc::SIGTERM);
}

#[test]
fn version_verbose_adds_a_commit_line() {
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_lightbrowser"))
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    let plain = run(&["--version"]);
    assert_eq!(plain.lines().count(), 1, "{plain}");
    assert!(plain.starts_with(&format!("lightbrowser {} (", env!("CARGO_PKG_VERSION"))));
    let verbose = run(&["--version", "--verbose"]);
    let mut lines = verbose.lines();
    assert_eq!(lines.next(), plain.lines().next());
    let commit = lines.next().expect("commit line");
    assert!(commit.starts_with("commit "), "{commit}");
}
