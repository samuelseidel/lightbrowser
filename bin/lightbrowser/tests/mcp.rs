//! `lightbrowser mcp` end to end: spawns the binary with pipes, serves test
//! pages from loopback and speaks JSON-RPC to it. No network beyond
//! 127.0.0.1: hosts the policy allows by flag are reached through a proxy
//! that is a loopback server of the test.
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

const TITLE_SUBSTRING: &str = "<!doctype html><title>Hello World</title><h1>Heading</h1>\
<p>Hello</p><p>World</p><p>o</p><button>Hello</button>";

const EDIT: &str = r#"<!doctype html><title>Edit</title>
<input aria-label="RO field" readonly value="r"><input aria-label="DIS field" disabled value="d">
<a href="/two">link L</a><select aria-label="select S"><option>a</option><option>b</option></select>
<div id="ce" aria-label="editable E" contenteditable>abc</div>
<textarea id="ta" aria-label="area A"></textarea><input type="checkbox" aria-label="check C">
<input id="gone" aria-label="gone G" value="g">
<button onclick="document.getElementById('gone').remove()">remove R</button>"#;

const DIALOGS: &str = r#"<!doctype html><title>Dialogs</title>
<script>alert('on load')</script>
<button onclick="alert('hello A'); const c = confirm('sure C?'); const p = prompt('name P', 'dflt'); document.getElementById('out').textContent = 'confirm=' + c + ' prompt=' + p">Ask</button>
<p id="out">idle</p>"#;

/// A page whose image and script ask hosts outside the policy; the
/// transport refuses both before any DNS lookup or connection.
const BLOCKED_SUB: &str = r#"<!doctype html><title>Blocked sub</title>
<img src="http://blocked.example/pixel.png" alt="px">
<script src="http://10.255.255.1/lib.js"></script>
<p>body</p>"#;

const HASH: &str = "<!doctype html><title>Hash</title><a href=\"#x\">jump</a><div id=\"x\">x</div>";

/// 60 text fields, then enough buttons for several 100 KB snapshot parts.
fn many_page() -> String {
    let mut h = String::from("<!doctype html><title>Many</title>");
    for i in 0..60 {
        h.push_str(&format!(
            "<input aria-label=\"field {i}\" value=\"attr{i}\">"
        ));
    }
    for i in 0..3000 {
        h.push_str(&format!(
            "<button onclick=\"document.title='picked {i}'\">Button {i:04} with a name that makes the line long enough</button>"
        ));
    }
    h
}

/// `depth` nested elements around one button.
fn deep_page(open: &str, close: &str, depth: usize) -> String {
    format!(
        "<!doctype html><title>deep</title>{}<button>deepbtn</button>{}",
        open.repeat(depth),
        close.repeat(depth)
    )
}

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
    let html = "Content-Type: text/html\r\n";
    let (status, extra, body) = match path {
        "/" => ("200 OK", html, FORM.to_owned()),
        "/two" => ("200 OK", html, TWO.to_owned()),
        "/title-substring" => ("200 OK", html, TITLE_SUBSTRING.to_owned()),
        "/edit" => ("200 OK", html, EDIT.to_owned()),
        "/hash" => ("200 OK", html, HASH.to_owned()),
        "/dialogs" => ("200 OK", html, DIALOGS.to_owned()),
        "/blocked-sub" => ("200 OK", html, BLOCKED_SUB.to_owned()),
        "/many" => ("200 OK", html, many_page()),
        "/deep-div" => ("200 OK", html, deep_page("<div>", "</div>", 3000)),
        "/deep-group" => (
            "200 OK",
            html,
            deep_page("<div role=group>", "</div>", 3000),
        ),
        "/redir" => (
            "302 Found",
            "Location: http://blocked.example/landing\r\n",
            String::new(),
        ),
        _ => (
            "404 Not Found",
            "Content-Type: text/plain\r\n",
            "nope".to_owned(),
        ),
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

/// Every part of a snapshot, following the `start_line` it names; returns
/// the text and the number of parts.
fn snapshot_all(m: &mut Mcp) -> (String, usize) {
    let mut text = String::new();
    let mut start = 1u64;
    let mut parts = 0;
    loop {
        let part = m.ok("browser_snapshot", json!({"start_line": start}));
        parts += 1;
        text.push_str(&part);
        let Some(i) = part
            .rfind("start_line=")
            .filter(|_| part.contains("[snapshot cut"))
        else {
            return (text, parts);
        };
        let n: String = part[i + 11..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        let n: u64 = n.parse().unwrap();
        assert!(n > start, "the next part moves forward");
        start = n;
        assert!(parts < 40, "runs away");
    }
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
    assert!(e.contains("text, html or markdown"), "{e}");

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
    // (The transport-level policy, ADR 0044, reports the refusal as
    // ERR_BLOCKED_BY_CLIENT without the host, so the text names the flag.)
    assert!(e.contains("--allow-host"), "{e}");

    // A script-triggered top-frame navigation to a blocked host is stopped too.
    m.ok("browser_navigate", json!({"url": format!("{base}/")}));
    // The refusal is noted by the first tool result after it happened. On a
    // loaded machine that can be the result of this very call (the timer
    // fires before the result is made), so keep every result's text.
    let mut notes = m.ok(
        "browser_evaluate",
        json!({"expression": "setTimeout(() => { location.href = 'http://blocked.example/js' }, 0)"}),
    );
    let mut href = String::new();
    for _ in 0..50 {
        if notes.contains("blocked.example/js") {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
        let t = m.ok("browser_evaluate", json!({"expression": "location.href"}));
        if href.is_empty() {
            href = t.clone();
        }
        notes.push_str(&t);
    }
    if href.is_empty() {
        href = m.ok("browser_evaluate", json!({"expression": "location.href"}));
    }
    assert!(
        href.starts_with(&format!("\"{base}/\"")),
        "the page stayed where it was: {href}"
    );
    // ... and a tool result said so (the transport's refusal reaches the
    // note through `Network.loadingFailed`).
    assert!(
        notes.contains("the host policy blocked requests to http://blocked.example/js"),
        "{notes}"
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

/// A forward proxy on loopback: answers every absolute-form request with a
/// page and records the request lines it saw.
fn proxy_server() -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let log = log.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).unwrap_or(0) == 0 {
                    return;
                }
                loop {
                    let mut l = String::new();
                    if reader.read_line(&mut l).unwrap_or(0) <= 2 {
                        break;
                    }
                }
                log.lock().unwrap().push(first.trim_end().to_owned());
                let body = "<!doctype html><title>Via proxy</title><p>proxied</p>";
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    (port, seen)
}

#[test]
fn allow_host_adds_hosts() {
    // Hermetic: the allowed host is reached through a loopback proxy, so no
    // name is ever looked up and nothing leaves the machine.
    let (pport, seen) = proxy_server();
    let proxy = format!("http://127.0.0.1:{pport}");
    let mut m = Mcp::start(&["--allow-host", "*.example", "--proxy", &proxy]);
    m.initialize("2025-06-18");
    let nav = m.ok(
        "browser_navigate",
        json!({"url": "http://corp.example/page"}),
    );
    assert!(nav.contains("Title: Via proxy"), "{nav}");
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|l| l == "GET http://corp.example/page HTTP/1.1"),
        "{:?}",
        seen.lock().unwrap()
    );
    // Still blocked: a host the flag does not cover, and the proxy never sees it.
    let before = seen.lock().unwrap().len();
    let e = m.fails("browser_navigate", json!({"url": "http://example.com/"}));
    assert!(e.contains("host policy"), "{e}");
    assert_eq!(seen.lock().unwrap().len(), before);
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
    // A timeout that overflows the clock is refused up front (it used to
    // panic at the first navigation).
    let out = Command::new(env!("CARGO_BIN_EXE_lightbrowser"))
        .args(["mcp", "--timeout", "18446744073709551615"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--timeout"));
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

#[test]
fn snapshot_keeps_text_that_is_part_of_an_ancestors_name() {
    let port = page_server();
    let mut m = Mcp::start(&[]);
    m.initialize("2025-06-18");
    m.ok(
        "browser_navigate",
        json!({"url": format!("http://127.0.0.1:{port}/title-substring")}),
    );
    let snap = m.ok("browser_snapshot", json!({}));
    // The page title "Hello World" contains these texts; they are content.
    assert!(snap.contains("- RootWebArea \"Hello World\""), "{snap}");
    for t in ["Hello", "World", "o"] {
        assert!(snap.contains(&format!("- text \"{t}\"")), "{t}: {snap}");
    }
    // A heading or button named by its text does not repeat it.
    assert!(snap.contains("heading \"Heading\""), "{snap}");
    assert!(!snap.contains("text \"Heading\""), "{snap}");
    assert_eq!(snap.matches("text \"Hello\"").count(), 1, "{snap}");
    m.finish();
}

/// A page nested thousands deep killed the server with a stack overflow.
#[test]
fn deeply_nested_pages_are_snapshotted() {
    let port = page_server();
    let mut m = Mcp::start(&[]);
    m.initialize("2025-06-18");
    for page in ["deep-div", "deep-group"] {
        m.ok(
            "browser_navigate",
            json!({"url": format!("http://127.0.0.1:{port}/{page}")}),
        );
        // The first part is within the limit; the button comes in a later
        // part when the groups before it fill the first.
        let first = m.ok("browser_snapshot", json!({}));
        assert!(
            first.len() < 100 * 1024 + 200,
            "{page}: {} bytes",
            first.len()
        );
        let (snap, _) = snapshot_all(&mut m);
        assert!(snap.contains("button \"deepbtn\" [ref="), "{page}");
        // The server is still there.
        assert_eq!(m.request("ping", json!({}))["result"], json!({}));
    }
    m.finish();
}

#[test]
fn browser_type_refuses_what_cannot_take_text() {
    let port = page_server();
    let mut m = Mcp::start(&[]);
    m.initialize("2025-06-18");
    m.ok(
        "browser_navigate",
        json!({"url": format!("http://127.0.0.1:{port}/edit")}),
    );
    let snap = m.ok("browser_snapshot", json!({}));
    let eval = |m: &mut Mcp, js: &str| m.ok("browser_evaluate", json!({"expression": js}));
    for (name, why) in [
        ("RO field", "read-only"),
        ("DIS field", "disabled"),
        ("link L", "not editable"),
        ("select S", "cannot be typed into"),
        ("check C", "does not take typed text"),
    ] {
        let r = ref_of(&snap, &format!("\"{name}\""));
        let e = m.fails("browser_type", json!({"ref": r, "text": "zzz"}));
        assert!(e.contains(why) && e.contains(&r), "{name}: {e}");
    }
    // Nothing was typed anywhere.
    assert_eq!(
        eval(&mut m, "document.querySelector('[readonly]').value"),
        "\"r\""
    );
    assert_eq!(
        eval(&mut m, "document.querySelector('[disabled]').value"),
        "\"d\""
    );

    // A contenteditable element's content is replaced, not appended to.
    let ce = ref_of(&snap, "\"editable E\"");
    m.ok("browser_type", json!({"ref": ce, "text": "XY"}));
    assert_eq!(
        eval(&mut m, "document.getElementById('ce').textContent"),
        "\"XY\""
    );

    // One line break per CRLF pair (and per lone CR).
    let ta = ref_of(&snap, "\"area A\"");
    m.ok("browser_type", json!({"ref": ta, "text": "a\r\nb\rc\n"}));
    assert_eq!(
        eval(&mut m, "document.getElementById('ta').value"),
        "\"a\\nb\\nc\\n\""
    );

    // An element the page removed is a stale ref, as for browser_click.
    let gone = ref_of(&snap, "\"gone G\"");
    m.ok(
        "browser_click",
        json!({"ref": ref_of(&snap, "\"remove R\"")}),
    );
    let e = m.fails("browser_type", json!({"ref": gone, "text": "x"}));
    assert!(e.contains("no longer usable"), "{e}");
    let e = m.fails("browser_click", json!({"ref": gone}));
    assert!(
        e.contains("no longer usable") || e.contains("no visible box"),
        "{e}"
    );
    m.finish();
}

/// Every node and every live field value is reachable: the part limit moves
/// the rest to `start_line`.
#[test]
fn long_snapshots_come_in_parts() {
    let port = page_server();
    let mut m = Mcp::start(&[]);
    m.initialize("2025-06-18");
    m.ok(
        "browser_navigate",
        json!({"url": format!("http://127.0.0.1:{port}/many")}),
    );
    let first = m.ok("browser_snapshot", json!({}));
    assert!(first.len() <= 100 * 1024 + 200, "{} bytes", first.len());
    assert!(first.contains("button \"Button 0000"), "{}", &first[..400]);
    assert!(!first.contains("Button 2999"));
    // Type into the 56th field, past what the old 50-field limit covered.
    let f55 = ref_of(&first, "\"field 55\"");
    m.ok("browser_type", json!({"ref": f55, "text": "typed55"}));

    let (text, parts) = snapshot_all(&mut m);
    assert!(parts >= 3, "{parts} parts");
    assert!(text.contains("value=\"typed55\""), "live value of field 55");
    assert!(text.contains("value=\"attr54\""));
    for i in [0, 1500, 2999] {
        assert!(text.contains(&format!("Button {i:04} ")), "button {i}");
    }
    // A ref from a late part works.
    let r = ref_of(
        text.lines()
            .find(|l| l.contains("Button 2999"))
            .map(|_| text.as_str())
            .unwrap(),
        "Button 2999",
    );
    m.ok("browser_click", json!({"ref": r}));
    assert_eq!(
        m.ok("browser_evaluate", json!({"expression": "document.title"})),
        "\"picked 2999\""
    );
    let e = m.fails("browser_snapshot", json!({"start_line": 999_999}));
    assert!(e.contains("past the end"), "{e}");
    let e = m.fails("browser_snapshot", json!({"start_line": 0}));
    assert!(e.contains("start_line"), "{e}");
    m.finish();
}

/// A `#fragment` navigation starts no document; the URL still has to be
/// right in the result of the navigation and of the click that caused it.
#[test]
fn same_document_navigations_report_the_new_url() {
    let port = page_server();
    let base = format!("http://127.0.0.1:{port}");
    let mut m = Mcp::start(&[]);
    m.initialize("2025-06-18");
    m.ok("browser_navigate", json!({"url": format!("{base}/hash")}));
    let nav = m.ok("browser_navigate", json!({"url": format!("{base}/hash#y")}));
    assert!(
        nav.contains(&format!("Navigated to: {base}/hash#y")),
        "{nav}"
    );
    let snap = m.ok("browser_snapshot", json!({}));
    assert!(snap.contains(&format!("Page URL: {base}/hash#y")), "{snap}");
    let link = ref_of(&snap, "link \"jump\"");
    let click = m.ok("browser_click", json!({"ref": link}));
    assert!(
        click.contains(&format!("The URL changed to {base}/hash#x")),
        "{click}"
    );
    assert!(click.contains("refs are still valid"), "{click}");
    // The ref still works: nothing was reloaded.
    m.ok(
        "browser_click",
        json!({"ref": ref_of(&snap, "link \"jump\"")}),
    );
    m.finish();
}

/// Requests the transport (egress policy, ADR 0044) refuses come back as a
/// note on the tool result that caused them (ADR 0051). The page is on
/// loopback; its image and script ask hosts no entry allows, which are
/// refused before any lookup, so nothing leaves the machine.
#[test]
fn blocked_requests_are_noted_in_tool_results() {
    let port = page_server();
    let base = format!("http://127.0.0.1:{port}");
    let mut m = Mcp::start(&["--allow-host", "*.lab.example"]);
    m.initialize("2025-06-18");
    let nav = m.ok(
        "browser_navigate",
        json!({"url": format!("{base}/blocked-sub")}),
    );
    assert!(nav.contains("Title: Blocked sub"), "{nav}");
    let note = nav
        .lines()
        .find(|l| l.starts_with("Note: the host policy blocked requests to "))
        .unwrap_or_else(|| panic!("no blocked-request note in {nav:?}"));
    assert!(note.contains("http://blocked.example/pixel.png"), "{note}");
    assert!(note.contains("http://10.255.255.1/lib.js"), "{note}");
    assert!(
        note.ends_with("(--allow-host <entry> allows more hosts)"),
        "{note}"
    );
    // A note is given once: the next result has none.
    let t = m.ok("browser_evaluate", json!({"expression": "1 + 1"}));
    assert_eq!(t, "2");
    // A request a tool makes itself is noted on that tool's result.
    let t = m.ok(
        "browser_evaluate",
        json!({"expression": "fetch('http://other.example/api').then(() => 'loaded', () => 'refused')"}),
    );
    assert!(t.starts_with("\"refused\""), "{t}");
    assert!(
        t.contains("Note: the host policy blocked requests to http://other.example/api"),
        "{t}"
    );
    m.finish();
}

/// JavaScript dialogs: nobody can answer them, so the server accepts
/// `alert`, dismisses `confirm` and `prompt`, and says so in the result.
#[test]
fn javascript_dialogs_are_answered_and_noted() {
    let port = page_server();
    let base = format!("http://127.0.0.1:{port}");
    let mut m = Mcp::start(&[]);
    m.initialize("2025-06-18");
    // An alert while the page loads does not hold up the navigation.
    let nav = m.ok(
        "browser_navigate",
        json!({"url": format!("{base}/dialogs")}),
    );
    assert!(nav.contains("Title: Dialogs"), "{nav}");
    assert!(
        nav.contains("Note: a JavaScript alert dialog (\"on load\") was accepted"),
        "{nav}"
    );
    let snap = m.ok("browser_snapshot", json!({}));
    let mut text = m.ok(
        "browser_click",
        json!({"ref": ref_of(&snap, "button \"Ask\"")}),
    );
    // The notes belong to the click; read the result of the handler too.
    let out = m.ok(
        "browser_evaluate",
        json!({"expression": "document.getElementById('out').textContent"}),
    );
    text.push_str(&out);
    for want in [
        "Note: a JavaScript alert dialog (\"hello A\") was accepted",
        "Note: a JavaScript confirm dialog (\"sure C?\") was dismissed",
        "Note: a JavaScript prompt dialog (\"name P\") was dismissed",
    ] {
        assert!(text.contains(want), "{want:?} not in {text}");
    }
    // Dismissed: confirm() is false and prompt() is null, as in Chrome.
    assert!(out.starts_with("\"confirm=false prompt=null\""), "{out}");
    // Each note is given once.
    let again = m.ok("browser_evaluate", json!({"expression": "1"}));
    assert_eq!(again, "1");
    m.finish();
}

fn extract_golden_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../crates/lb-page/tests/extract")
}

/// `python3 -m http.server` on a free loopback port serving the extract
/// goldens (as `tests/extract_cli.rs` does), stopped on drop.
struct GoldenServer {
    child: Child,
    port: u16,
}

impl GoldenServer {
    fn start() -> GoldenServer {
        let mut child = Command::new("python3")
            .args(["-u", "-m", "http.server", "0", "--bind", "127.0.0.1"])
            .arg("--directory")
            .arg(extract_golden_dir())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("python3 -m http.server");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port = line
            .split(" port ")
            .nth(1)
            .and_then(|r| r.split(' ').next())
            .and_then(|p| p.parse().ok())
            .unwrap_or_else(|| panic!("no port in {line:?}"));
        GoldenServer { child, port }
    }
}

impl Drop for GoldenServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `browser_extract` markdown is `lightbrowser fetch --dump markdown`'s
/// output (ADR 0052): the same golden files, byte for byte.
#[test]
fn extract_markdown_equals_the_fetch_golden_files() {
    let srv = GoldenServer::start();
    let host = format!("127.0.0.1:{}", srv.port);
    let mut m = Mcp::start(&[]);
    m.initialize("2025-06-18");
    for (name, ext) in [
        ("headings", "html"),
        ("links_base", "html"),
        ("table", "html"),
        ("shadow", "html"),
        ("scripted", "html"),
        ("plain", "txt"),
        ("regress", "html"),
        ("regress3", "html"),
    ] {
        m.ok(
            "browser_navigate",
            json!({"url": format!("http://{host}/{name}.{ext}")}),
        );
        let got = m.ok("browser_extract", json!({"format": "markdown"}));
        let want = std::fs::read_to_string(extract_golden_dir().join(format!("{name}.md")))
            .unwrap()
            .replace("{HOST}", &host);
        assert_eq!(got, want, "{name}.{ext} markdown");
    }
    let e = m.fails("browser_extract", json!({"format": "pdf"}));
    assert!(e.contains("text, html or markdown"), "{e}");
    m.finish();
}
