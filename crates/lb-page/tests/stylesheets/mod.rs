//! Style sheets block scripts and the parser as in Chrome: each case in
//! `cases/` is served on loopback (files named `slow-*` answer after
//! 300 ms, `slower-*` after 600 ms; `<!--LOG-->` becomes `prelude.html`)
//! and its log must equal what Chrome 153 (Chrome for Testing
//! 153.0.8010.12) printed for the same pages, in `chrome.json`. Re-record
//! with `python3 serve.py cases 18901 &` and
//! `node chrome.mjs http://127.0.0.1:18901 $(ls cases) > chrome.json`.
//! Included from `pages.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lb_page::WaitUntil;

use super::common::{Resource, browser, context, open, serve_fn};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/stylesheets")
}

fn route(path: &str) -> Option<Resource> {
    let rel = path.trim_start_matches('/');
    let rel = if rel.ends_with('/') || rel.is_empty() {
        format!("{rel}index.html")
    } else {
        rel.to_owned()
    };
    let file = dir().join("cases").join(&rel);
    let body = std::fs::read(&file).ok()?;
    let name = file.file_name()?.to_str()?.to_owned();
    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
    let ct = match ext {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css",
        "js" => "text/javascript",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    };
    let body = if ext == "html" {
        let pre = std::fs::read_to_string(dir().join("prelude.html")).ok()?;
        String::from_utf8_lossy(&body)
            .replace("<!--LOG-->", pre.trim())
            .into_bytes()
    } else {
        body
    };
    let r = Resource::new(ct, body);
    Some(if name.starts_with("slower-") {
        r.delay(600)
    } else if name.starts_with("slow-") {
        r.delay(300)
    } else {
        r
    })
}

/// Chrome's log for `case`.
fn chrome(case: &str) -> String {
    let all: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir().join("chrome.json")).unwrap()).unwrap();
    all[case].as_str().unwrap().to_owned()
}

/// Lines before the first `script` line, sorted: the order of loads that
/// race on the network is not compared.
fn unordered_head(log: &str) -> Vec<String> {
    let lines: Vec<&str> = log.lines().collect();
    let cut = lines
        .iter()
        .position(|l| l.starts_with("script"))
        .unwrap_or(lines.len());
    let mut head: Vec<String> = lines[..cut].iter().map(|s| s.to_string()).collect();
    head.sort();
    head.extend(lines[cut..].iter().map(|s| s.to_string()));
    head
}

async fn run(case: &str) {
    let srv = serve_fn(Arc::new(route)).await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &srv.url(&format!("/{case}/")), WaitUntil::Load).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let got = match page.evaluate("L.join('\\n')").await.unwrap() {
        lb_js::JsValue::String(s) => s,
        v => panic!("{v:?}"),
    };
    let want = chrome(case);
    if case == "style_events" {
        assert_eq!(
            unordered_head(&got),
            unordered_head(&want),
            "{case}:\n{got}"
        );
    } else {
        assert_eq!(got, want, "{case}:\n{got}");
    }
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
    body_link_pause,
    body_pause,
    cssom_edit,
    defer_async,
    dynamic_link,
    link_body,
    link_error,
    link_head,
    link_import,
    media_print,
    media_queries,
    no_scripts,
    sheet_list,
    style_events,
    style_import,
    timer_while_blocked,
    two_links,
);
