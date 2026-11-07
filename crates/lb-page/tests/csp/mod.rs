//! Content Security Policy as Chrome 153 enforces it (ADR 0036). Each case
//! in `cases/<case>/` is served on loopback by `serve.py` (headers from
//! `<file>.headers`; the same server Chrome was recorded against, started
//! per test on a free port). Three things must equal Chrome's, from
//! `chrome.json`: the page's log `L` (from `prelude.js`: results and every
//! `securitypolicyviolation` with all its fields), the console messages
//! about Content Security Policy, and the requests the server saw (blocked
//! loads never reach it). Lines are compared as sorted lists (violation
//! events are tasks; their order against other tasks is timing).
//! `bypass_CASE` loads CASE with `Page.setBypassCSP` (Playwright's
//! `bypassCSP`) on (`CASE@bypass` in `chrome.json`).
//!
//! The cases are OS-neutral; they were recorded with Chrome for Testing
//! 153.0.8010.12 headless shell (macOS, and Linux for `frame_ancestors`,
//! `img_src`, `report_only`, `unsafe_eval`, `wasm_unsafe_eval`). Re-record (loopback
//! only; the port must stay 19110, the test maps it):
//! `python3 serve.py 19110 &` then
//! `CHROME=.../chrome-headless-shell PLAYWRIGHT_MJS=.../playwright-core/index.mjs node chrome.mjs http://127.0.0.1:19110 $(ls cases) rebrowser_bypass@bypass script_src_header@bypass > chrome.json`.
//! Included from `pages.rs`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use lb_page::WaitUntil;

use super::common::{browser, context, open};

/// The port Chrome's recording ran on.
const RECORDED_PORT: &str = "19110";

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/csp")
}

/// `serve.py` on a free loopback port, stopped on drop.
struct PyServer {
    child: Child,
    port: u16,
}

impl PyServer {
    fn start() -> PyServer {
        let mut child = Command::new("python3")
            .arg(dir().join("serve.py"))
            .arg("0")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("python3 serve.py");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port = line.trim().parse().expect("port");
        PyServer { child, port }
    }

    /// The requests the server saw under `/<case>/`.
    fn requests(&self, case: &str) -> Vec<String> {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        write!(
            s,
            "GET /__log/{case} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let body = out.split("\r\n\r\n").nth(1).unwrap_or("[]");
        serde_json::from_str(body).unwrap()
    }
}

impl Drop for PyServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Ports, and `blob:` UUIDs, out of a line.
fn norm(s: &str, port: &str) -> String {
    let s = s.replace(port, "PORT");
    let mut out = String::new();
    let mut rest = s.as_str();
    while let Some(i) = rest.find("blob:http://127.0.0.1:PORT/") {
        let start = i + "blob:http://127.0.0.1:PORT/".len();
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let uuid_len = tail
            .bytes()
            .take_while(|b| b.is_ascii_hexdigit() || *b == b'-')
            .count();
        out.push_str("UUID");
        rest = &tail[uuid_len..];
    }
    out.push_str(rest);
    out
}

/// Chrome reports a CSS background image's violation at the parser
/// position of the style recalc that asked for it, which varies between
/// runs (lines 8 and 9 of `img_src` seen): those fields are not compared.
fn no_bg_position(s: String) -> String {
    if !s.contains("bg.png\",\"violatedDirective") {
        return s;
    }
    match s.find(",\"sourceFile\"") {
        Some(i) => {
            let tail = &s[i..];
            let sample = tail.find(",\"sample\"").unwrap_or(0);
            let status = tail.find(",\"lineNumber\"").unwrap_or(tail.len());
            format!("{}{}}}", &s[..i], &tail[sample..status])
        }
        None => s,
    }
}

fn sorted(lines: impl IntoIterator<Item = String>, port: &str) -> Vec<String> {
    let mut v: Vec<String> = lines
        .into_iter()
        .map(|l| no_bg_position(norm(&l, port)))
        .collect();
    v.sort();
    v
}

/// Chrome's recording of `case`: (log lines, console lines, requests).
fn chrome(case: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    let all: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir().join("chrome.json")).unwrap()).unwrap();
    let c = &all[case];
    let strs = |k: &str| -> Vec<String> {
        c[k].as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let log = c["log"]
        .as_str()
        .unwrap_or_default()
        .split('\n')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    (
        sorted(log, RECORDED_PORT),
        sorted(strs("console"), RECORDED_PORT),
        sorted(strs("requests"), RECORDED_PORT),
    )
}

fn is_csp_text(t: &str) -> bool {
    let l = t.to_ascii_lowercase();
    l.contains("content security policy") || l.contains("content-security-policy")
}

async fn run(case: &str) {
    let (dir_name, bypass) = match case.strip_prefix("bypass_") {
        Some(d) => (d, true),
        None => (case, false),
    };
    let srv = PyServer::start();
    let port = srv.port.to_string();
    let seen = srv.requests(dir_name).len();
    let (b, ctx) = (browser(), context(false));
    let url = format!("http://127.0.0.1:{}/{dir_name}/", srv.port);
    let page = if bypass {
        let p = b.new_page(&ctx).await.unwrap();
        p.set_bypass_csp(true).await.unwrap();
        p.goto(&url, WaitUntil::Load, Duration::from_secs(20))
            .await
            .unwrap();
        p
    } else {
        open(&b, &ctx, &url, WaitUntil::Load).await
    };
    let t0 = Instant::now();
    let log = loop {
        let log = match page
            .evaluate("typeof L === 'undefined' ? '' : L.join('\\n')")
            .await
            .unwrap()
        {
            lb_js::JsValue::String(s) => s,
            v => panic!("{v:?}"),
        };
        if log.ends_with("\nend") || log == "end" || t0.elapsed() > Duration::from_secs(10) {
            break log;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    // Reports and late requests.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let console: Vec<String> = page
        .frame_tree_console_messages()
        .await
        .unwrap()
        .into_iter()
        .filter(|m| is_csp_text(&m.text))
        .map(|m| {
            let level = match m.level.as_str() {
                "warn" => "warning",
                l => l,
            };
            format!("{level} {}", m.text)
        })
        .collect();
    let requests: Vec<String> = srv.requests(dir_name).into_iter().skip(seen).collect();
    let got = (
        sorted(log.split('\n').map(str::to_owned), &port),
        sorted(console, &port),
        sorted(requests, &port),
    );
    let key = if bypass {
        format!("{dir_name}@bypass")
    } else {
        case.to_owned()
    };
    let want = chrome(&key);
    assert_eq!(got.0, want.0, "{key}: log");
    assert_eq!(got.1, want.1, "{key}: console");
    assert_eq!(got.2, want.2, "{key}: requests");
}

macro_rules! cases {
    ($($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() {
                run(stringify!($name)).await;
            }
        )*
    };
}

cases!(
    rebrowser_bypass,
    bypass_rebrowser_bypass,
    script_src_header,
    bypass_script_src_header,
    default_src_fallback,
    nonce,
    hash,
    eval_blocked,
    eval_report_sample,
    unsafe_eval,
    wasm_blocked,
    wasm_unsafe_eval,
    event_handlers,
    unsafe_hashes,
    javascript_url,
    report_only,
    multiple_policies,
    meta_policy,
    img_src,
    style_src,
    font_src,
    connect_src,
    frame_src,
    frame_ancestors,
    child_src,
    worker_src,
    worker_policy,
    base_uri,
    form_action,
    strict_dynamic,
    source_matching,
    iframe_inherit,
    violation_event,
    report_uri,
    sandbox,
    upgrade_insecure,
    parse_errors,
);
