//! `lightbrowser mcp` end to end: spawns the binary with pipes, serves test
//! pages from loopback and speaks JSON-RPC to it. No network beyond
//! 127.0.0.1 (one test resolves a `.invalid` name, which never exists).
#![cfg(feature = "chrome")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use serde_json::{Value, json};

const FORM: &str = r#"<!doctype html><html><head><title>Smoke form</title></head><body>
<h1>Contact</h1>
<form id="f" onsubmit="document.getElementById('out').textContent='sent:' + document.getElementById('name').value; return false">
<label for="name">Name</label><input id="name" name="name">
<button type="submit" id="go">Send</button></form>
<p id="out">idle</p>
<a href="/two">Next page</a>
<button id="b" onclick="document.getElementById('out').textContent='clicked'">Click me</button>
</body></html>"#;

const TWO: &str =
    "<!doctype html><html><head><title>Two</title></head><body><p>second page</p></body></html>";

/// Serves the test pages on a free loopback port until the process ends.
fn page_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || serve(stream));
        }
    });
    port
}

fn serve(mut stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() {
        return;
    }
    // Drain the headers.
    loop {
        let mut l = String::new();
        if reader.read_line(&mut l).unwrap_or(0) <= 2 {
            break;
        }
    }
    let path = first.split_whitespace().nth(1).unwrap_or("/");
    let (status, extra, body) = match path {
        "/" => ("200 OK", "Content-Type: text/html\r\n", FORM),
        "/two" => ("200 OK", "Content-Type: text/html\r\n", TWO),
        "/redir" => (
            "302 Found",
            "Location: http://blocked.example/landing\r\n",
            "",
        ),
        _ => ("404 Not Found", "Content-Type: text/plain\r\n", "nope"),
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
}

/// A running `lightbrowser mcp`.
struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: u64,
}

impl Mcp {
    fn start(extra: &[&str]) -> Mcp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_lightbrowser"))
            .arg("mcp")
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn lightbrowser mcp");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Mcp {
            stdin: child.stdin.take(),
            child,
            lines: rx,
            next_id: 0,
        }
    }

    fn write(&mut self, v: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    /// The next line on stdout, which must be one JSON message.
    fn read(&mut self) -> Value {
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(90))
            .expect("a reply within 90 s");
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("stdout carries protocol messages only, got {line:?}: {e}"))
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let r = self.read();
        assert_eq!(r["id"], id, "{r}");
        r
    }

    fn initialize(&mut self, version: &str) -> Value {
        let r = self.request(
            "initialize",
            json!({
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": {"name": "mcp-test", "version": "0"}
            }),
        );
        self.write(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        r
    }

    /// Calls a tool; returns (isError, content blocks).
    fn tool(&mut self, name: &str, args: Value) -> (bool, Vec<Value>) {
        let r = self.request("tools/call", json!({"name": name, "arguments": args}));
        let res = &r["result"];
        assert!(
            res.is_object(),
            "tool failures are results, not errors: {r}"
        );
        let content = res["content"].as_array().expect("content").clone();
        (res["isError"].as_bool().expect("isError"), content)
    }

    /// A tool call that must succeed with one text block.
    fn ok(&mut self, name: &str, args: Value) -> String {
        let (err, content) = self.tool(name, args);
        let text = text_of(&content);
        assert!(!err, "{name} failed: {text}");
        text
    }

    /// A tool call that must fail; returns its text.
    fn fails(&mut self, name: &str, args: Value) -> String {
        let (err, content) = self.tool(name, args);
        let text = text_of(&content);
        assert!(err, "{name} should have failed, got: {text}");
        text
    }

    /// Closes stdin and waits for a clean exit.
    fn finish(mut self) {
        drop(self.stdin.take());
        let start = std::time::Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "exit status {status}");
                return;
            }
            if start.elapsed() > Duration::from_secs(30) {
                let _ = self.child.kill();
                panic!("lightbrowser mcp did not exit after stdin closed");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text_of(content: &[Value]) -> String {
    content
        .iter()
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `[ref=eN]` on the first snapshot line containing `needle`.
fn ref_of(snapshot: &str, needle: &str) -> String {
    let line = snapshot
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?} in:\n{snapshot}"));
    let start = line
        .find("[ref=")
        .unwrap_or_else(|| panic!("no ref on {line:?}"))
        + 5;
    let end = line[start..].find(']').unwrap() + start;
    line[start..end].to_owned()
}

fn base64_decode(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        if let Some(v) = val(c) {
            acc = (acc << 6) | u32::from(v);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
                acc &= (1 << bits) - 1;
            }
        }
    }
    out
}

#[test]
fn every_protocol_version_is_negotiated() {
    let mut m = Mcp::start(&[]);
    for v in ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"] {
        let r = m.initialize(v);
        assert_eq!(r["result"]["protocolVersion"], v, "{r}");
        assert_eq!(r["result"]["serverInfo"]["name"], "lightbrowser");
        assert!(r["result"]["serverInfo"]["version"].is_string());
        assert_eq!(r["result"]["capabilities"]["tools"]["listChanged"], false);
    }
    // A version we do not speak gets our newest.
    let r = m.initialize("2031-01-01");
    assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(m.request("ping", json!({}))["result"], json!({}));
    m.finish();
}

#[test]
fn tools_list_has_every_tool_with_a_valid_schema() {
    let mut m = Mcp::start(&[]);
    m.initialize("2025-06-18");
    let r = m.request("tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().expect("tools");
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "browser_click",
            "browser_close",
            "browser_evaluate",
            "browser_extract",
            "browser_navigate",
            "browser_press_key",
            "browser_screenshot",
            "browser_snapshot",
            "browser_type",
        ]
    );
    for t in tools {
        assert!(!t["description"].as_str().unwrap().is_empty());
        let s = &t["inputSchema"];
        assert_eq!(s["type"], "object", "{t}");
        let props = s["properties"].as_object().expect("properties");
        for (name, p) in props {
            assert!(p["type"].is_string(), "{name} has a type in {t}");
        }
        for req in s["required"].as_array().expect("required") {
            assert!(props.contains_key(req.as_str().unwrap()), "{t}");
        }
    }
    let nav = tools
        .iter()
        .find(|t| t["name"] == "browser_navigate")
        .unwrap();
    assert_eq!(
        nav["inputSchema"]["properties"]["wait_until"]["enum"],
        json!(["load", "domcontentloaded", "networkidle"])
    );
    m.finish();
}

#[test]
fn protocol_errors_use_standard_codes() {
    let mut m = Mcp::start(&[]);
    m.initialize("2025-06-18");
    let r = m.request("resources/list", json!({}));
    assert_eq!(r["error"]["code"], -32601, "{r}");
    let r = m.request("tools/call", json!({}));
    assert_eq!(r["error"]["code"], -32602, "{r}");
    let r = m.request("tools/call", json!({"name": "browser_nope"}));
    assert_eq!(r["error"]["code"], -32602, "{r}");
    let r = m.request(
        "tools/call",
        json!({"name": "browser_click", "arguments": 5}),
    );
    assert_eq!(r["error"]["code"], -32602, "{r}");
    // Garbage on the wire is answered, not fatal.
    m.write(&json!("just a string"));
    let r = m.read();
    assert_eq!(r["error"]["code"], -32600, "{r}");
    assert_eq!(m.request("ping", json!({}))["result"], json!({}));
    // Tool failures are results with isError.
    let t = m.fails("browser_click", json!({}));
    assert!(t.contains("missing required argument"), "{t}");
    m.finish();
}

#[test]
fn drives_a_page_end_to_end() {
    let port = page_server();
    let base = format!("http://127.0.0.1:{port}");
    let mut m = Mcp::start(&[]);
    m.initialize("2025-11-25");

    // Navigate: title, final URL, status.
    let nav = m.ok("browser_navigate", json!({"url": format!("{base}/")}));
    assert!(nav.contains("Title: Smoke form"), "{nav}");
    assert!(nav.contains(&format!("Navigated to: {base}/")), "{nav}");
    assert!(nav.contains("Status: 200"), "{nav}");

    // Snapshot: roles, names, refs.
    let snap = m.ok("browser_snapshot", json!({}));
    assert!(snap.contains("- RootWebArea \"Smoke form\""), "{snap}");
    assert!(snap.contains("heading \"Contact\" [level=1]"), "{snap}");
    let name_ref = ref_of(&snap, "textbox \"Name\"");
    let send_ref = ref_of(&snap, "button \"Send\"");
    let click_ref = ref_of(&snap, "button \"Click me\"");
    let link_ref = ref_of(&snap, "link \"Next page\"");
    assert_ne!(name_ref, send_ref);

    // Type into the form field, then submit it with a click.
    let typed = m.ok(
        "browser_type",
        json!({"ref": name_ref, "text": "Ada Lovelace"}),
    );
    assert!(typed.contains("\"Ada Lovelace\""), "{typed}");
    let snap = m.ok("browser_snapshot", json!({}));
    assert!(
        snap.contains("value=\"Ada Lovelace\""),
        "live value: {snap}"
    );
    assert_eq!(
        ref_of(&snap, "textbox \"Name\""),
        name_ref,
        "refs are stable"
    );
    let clicked = m.ok("browser_click", json!({"ref": send_ref}));
    assert!(clicked.contains("Clicked"), "{clicked}");
    let out = m.ok(
        "browser_evaluate",
        json!({"expression": "document.getElementById('out').textContent"}),
    );
    assert_eq!(out, "\"sent:Ada Lovelace\"");

    // submit:true presses Enter in the field; the field is replaced.
    m.ok(
        "browser_type",
        json!({"ref": name_ref, "text": "Grace", "submit": true}),
    );
    let out = m.ok(
        "browser_evaluate",
        json!({"expression": "document.getElementById('out').textContent"}),
    );
    assert_eq!(out, "\"sent:Grace\"");

    // A plain button.
    m.ok("browser_click", json!({"ref": click_ref}));
    let out = m.ok(
        "browser_evaluate",
        json!({"expression": "document.getElementById('out').textContent"}),
    );
    assert_eq!(out, "\"clicked\"");

    // Evaluate returns JSON; errors are tool errors.
    let v = m.ok(
        "browser_evaluate",
        json!({"expression": "({a: [1, 2], b: 'x', c: null})"}),
    );
    assert_eq!(
        serde_json::from_str::<Value>(&v).unwrap(),
        json!({"a": [1, 2], "b": "x", "c": null})
    );
    let v = m.ok(
        "browser_evaluate",
        json!({"expression": "Promise.resolve(40 + 2)"}),
    );
    assert_eq!(v, "42");
    assert_eq!(
        m.ok("browser_evaluate", json!({"expression": "undefined"})),
        "undefined"
    );
    let e = m.fails(
        "browser_evaluate",
        json!({"expression": "throw new Error('boom')"}),
    );
    assert!(e.contains("boom"), "{e}");
    // The page's own globals are out of reach: this runs in an isolated world.
    let v = m.ok(
        "browser_evaluate",
        json!({"expression": "typeof window.__mcp_probe"}),
    );
    assert_eq!(v, "\"undefined\"");

    // Screenshot: an image block holding a PNG.
    let (err, content) = m.tool("browser_screenshot", json!({}));
    assert!(!err);
    assert_eq!(content[0]["type"], "image");
    assert_eq!(content[0]["mimeType"], "image/png");
    let png = base64_decode(content[0]["data"].as_str().unwrap());
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert!(png.len() > 100);

    // Extract text and html.
    let text = m.ok("browser_extract", json!({"format": "text"}));
    assert!(
        text.contains("Contact") && text.contains("Next page"),
        "{text}"
    );
    let html = m.ok("browser_extract", json!({"format": "html"}));
    assert!(
        html.contains("<html") && html.contains("<form id=\"f\""),
        "{html}"
    );
    let e = m.fails("browser_extract", json!({"format": "pdf"}));
    assert!(e.contains("text or html"), "{e}");

    // Press a key: Enter on the focused field submits the form again.
    let pressed = m.ok("browser_press_key", json!({"key": "Enter"}));
    assert!(pressed.contains("Pressed Enter"), "{pressed}");
    let e = m.fails("browser_press_key", json!({"key": "Bogus"}));
    assert!(e.contains("unknown key"), "{e}");

    // A click that navigates: the new page loads and old refs are gone.
    let nav = m.ok("browser_click", json!({"ref": link_ref}));
    assert!(nav.contains(&format!("navigated to {base}/two")), "{nav}");
    let e = m.fails("browser_click", json!({"ref": click_ref}));
    assert!(e.contains("unknown ref"), "{e}");
    let text = m.ok("browser_extract", json!({"format": "text"}));
    assert!(text.contains("second page"), "{text}");

    // A ref that never existed.
    let e = m.fails("browser_click", json!({"ref": "e999"}));
    assert!(e.contains("unknown ref") && e.contains("e999"), "{e}");
    let e = m.fails("browser_type", json!({"ref": "e999", "text": "x"}));
    assert!(e.contains("unknown ref"), "{e}");

    // Close, then the next call opens a fresh page.
    let c = m.ok("browser_close", json!({}));
    assert!(c.contains("closed"), "{c}");
    let nav = m.ok(
        "browser_navigate",
        json!({"url": format!("{base}/two"), "wait_until": "domcontentloaded"}),
    );
    assert!(nav.contains("Title: Two"), "{nav}");
    m.finish();
}

#[test]
fn blocks_hosts_outside_the_policy() {
    let port = page_server();
    let base = format!("http://127.0.0.1:{port}");
    let mut m = Mcp::start(&[]);
    m.initialize("2025-06-18");

    // A third-party host: refused before any lookup, and the error names the flag.
    let e = m.fails("browser_navigate", json!({"url": "http://example.com/"}));
    assert!(
        e.contains("--allow-host") && e.contains("example.com"),
        "{e}"
    );
    let e = m.fails("browser_navigate", json!({"url": "https://10.255.255.1/"}));
    assert!(e.contains("--allow-host"), "{e}");
    let e = m.fails("browser_navigate", json!({"url": "file:///etc/passwd"}));
    assert!(e.contains("--allow-host"), "{e}");

    // A redirect from an allowed page to a blocked host is stopped at the hop.
    let e = m.fails("browser_navigate", json!({"url": format!("{base}/redir")}));
    assert!(
        e.contains("blocked.example") && e.contains("--allow-host"),
        "{e}"
    );

    // A script-triggered top-frame navigation to a blocked host is stopped too.
    m.ok("browser_navigate", json!({"url": format!("{base}/")}));
    m.ok(
        "browser_evaluate",
        json!({"expression": "setTimeout(() => { location.href = 'http://blocked.example/js' }, 0)"}),
    );
    std::thread::sleep(Duration::from_millis(500));
    let nav = m.ok("browser_evaluate", json!({"expression": "location.href"}));
    assert!(
        nav.starts_with(&format!("\"{base}/\"")),
        "the page stayed where it was: {nav}"
    );
    // The tool result says what the policy refused meanwhile.
    assert!(
        nav.contains("blocked.example/js") && nav.contains("--allow-host"),
        "{nav}"
    );

    // Bad input is a tool error too.
    let e = m.fails("browser_navigate", json!({"url": "not a url"}));
    assert!(e.contains("invalid URL"), "{e}");
    let e = m.fails(
        "browser_navigate",
        json!({"url": format!("{base}/"), "wait_until": "soon"}),
    );
    assert!(e.contains("wait_until"), "{e}");

    // Only the built-in set is allowed: other TLDs are not.
    let e = m.fails(
        "browser_navigate",
        json!({"url": "http://nothing.invalid/"}),
    );
    assert!(e.contains("--allow-host"), "{e}");
    m.finish();
}

#[test]
fn allow_host_adds_hosts() {
    let mut m = Mcp::start(&["--allow-host", "*.invalid", "--timeout", "20"]);
    m.initialize("2025-06-18");
    // Allowed by the flag, so the policy lets it through; the name never
    // resolves, which is a navigation failure, not a policy block.
    // (If no resolver answers, the navigation times out instead.)
    let e = m.fails(
        "browser_navigate",
        json!({"url": "http://nothing.invalid/"}),
    );
    assert!(!e.contains("host policy"), "{e}");
    assert!(
        e.contains("navigation to") || e.contains("timed out"),
        "{e}"
    );
    // Still blocked: a host the flag does not cover.
    let e = m.fails("browser_navigate", json!({"url": "http://example.com/"}));
    assert!(e.contains("host policy"), "{e}");
    m.finish();
}

#[test]
fn exits_cleanly_when_stdin_closes() {
    let mut m = Mcp::start(&[]);
    m.initialize("2025-06-18");
    // Even with a page open.
    let port = page_server();
    m.ok(
        "browser_navigate",
        json!({"url": format!("http://127.0.0.1:{port}/")}),
    );
    m.finish();
}

#[test]
fn bad_options_exit_with_usage() {
    let out = Command::new(env!("CARGO_BIN_EXE_lightbrowser"))
        .args(["mcp", "--allow-host"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(
        out.stdout.is_empty(),
        "nothing on stdout but protocol messages"
    );
    let out = Command::new(env!("CARGO_BIN_EXE_lightbrowser"))
        .args(["mcp", "--bogus"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// The server must not listen on any socket (Linux: look at the process's
/// sockets in /proc).
#[cfg(target_os = "linux")]
#[test]
fn listens_on_nothing() {
    let mut m = Mcp::start(&[]);
    m.initialize("2025-06-18");
    let port = page_server();
    m.ok(
        "browser_navigate",
        json!({"url": format!("http://127.0.0.1:{port}/")}),
    );
    let pid = m.child.id();
    let mut inodes = Vec::new();
    for e in std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .flatten()
    {
        if let Ok(t) = std::fs::read_link(e.path())
            && let Some(i) = t.to_string_lossy().strip_prefix("socket:[")
        {
            inodes.push(i.trim_end_matches(']').to_owned());
        }
    }
    for table in ["tcp", "tcp6", "udp", "udp6"] {
        let mut s = String::new();
        std::fs::File::open(format!("/proc/{pid}/net/{table}"))
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        for line in s.lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let (state, inode) = (cols[3], cols[9]);
            if inodes.iter().any(|i| i == inode) {
                // 0A is TCP LISTEN; UDP sockets bound to a port are listeners too.
                assert!(
                    !(table.starts_with("tcp") && state == "0A"),
                    "listening socket: {line}"
                );
                assert!(!table.starts_with("udp"), "udp socket: {line}");
            }
        }
    }
    m.finish();
}
