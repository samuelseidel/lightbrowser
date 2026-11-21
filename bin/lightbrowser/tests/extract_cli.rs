//! `lightbrowser fetch --dump markdown | links | json` end to end (ADR
//! 0052): the built binary fetches pages from a loopback `python3 -m
//! http.server` that serves the golden inputs in
//! `crates/lb-page/tests/extract/`, and its output must equal the golden
//! files there. Those files are hand-written expectations, not browser
//! recordings. Loopback only, no network. Chrome build only: the golden
//! pages need scripts (the firefox build does not run them yet).
#![cfg(feature = "chrome")]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../crates/lb-page/tests/extract")
}

/// `python3 -m http.server` on a free loopback port, stopped on drop.
struct Server {
    child: Child,
    port: u16,
}

impl Server {
    fn start() -> Server {
        let mut child = Command::new("python3")
            .args(["-u", "-m", "http.server", "0", "--bind", "127.0.0.1"])
            .arg("--directory")
            .arg(golden_dir())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("python3 -m http.server");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        // "Serving HTTP on 127.0.0.1 port 41234 (http://127.0.0.1:41234/) ..."
        let port = line
            .split(" port ")
            .nth(1)
            .and_then(|r| r.split(' ').next())
            .and_then(|p| p.parse().ok())
            .unwrap_or_else(|| panic!("no port in {line:?}"));
        Server { child, port }
    }

    fn host(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fetch(url: &str, dump: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lightbrowser"))
        .args(["fetch", url, "--dump", dump, "--timeout", "20"])
        .output()
        .expect("run lightbrowser")
}

fn stdout(o: &Output) -> String {
    assert_eq!(
        o.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout.clone()).unwrap()
}

fn golden(name: &str, ext: &str, host: &str) -> String {
    std::fs::read_to_string(golden_dir().join(format!("{name}.{ext}")))
        .unwrap()
        .replace("{HOST}", host)
}

/// Cases with ASCII-only sources (the server sends no charset).
const CASES: [(&str, &str); 8] = [
    ("headings", "html"),
    ("links_base", "html"),
    ("table", "html"),
    ("shadow", "html"),
    ("scripted", "html"),
    ("plain", "txt"),
    ("regress", "html"),
    ("regress3", "html"),
];

#[test]
fn markdown_links_and_json_equal_the_golden_files() {
    let srv = Server::start();
    let host = srv.host();
    for (name, ext) in CASES {
        let url = format!("http://{host}/{name}.{ext}");
        for (dump, gext) in [("markdown", "md"), ("links", "links"), ("json", "json")] {
            let got = stdout(&fetch(&url, dump));
            let want = golden(name, gext, &host);
            assert_eq!(got, want, "{name} --dump {dump}");
        }
    }
}

#[test]
fn json_text_equals_dump_text_and_html_is_unchanged() {
    let srv = Server::start();
    let url = format!("http://{}/headings.html", srv.host());
    let json = stdout(&fetch(&url, "json"));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let text = stdout(&fetch(&url, "text"));
    assert_eq!(format!("{}\n", v["text"].as_str().unwrap()), text);
    assert_eq!(v["status"], 200);
    assert_eq!(v["url"], v["final_url"]);
    let html = stdout(&fetch(&url, "html"));
    assert!(html.starts_with("<!DOCTYPE html>"), "{html}");
    assert!(html.contains("<h1>Main   heading</h1>"), "{html}");
}

#[test]
fn unknown_dump_value_lists_the_five_valid_ones() {
    let o = fetch("http://127.0.0.1:9/", "pdf");
    assert_eq!(o.status.code(), Some(2));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("--dump must be one of html, text, markdown, links, json, got \"pdf\""),
        "{err}"
    );
    assert!(o.stdout.is_empty());
}

#[test]
fn an_error_status_is_reported_in_json() {
    // A missing file is a 404 page: the JSON still comes out, with the
    // status, and the exit code is 1 as for any non-2xx/3xx status.
    let srv = Server::start();
    let url = format!("http://{}/missing.html", srv.host());
    let o = fetch(&url, "json");
    assert_eq!(o.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["status"], 404);
    assert_eq!(v["url"], url);
}
