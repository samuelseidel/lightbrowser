//! Named access as in Chrome 153 (ADR 0040): the `WindowProperties` named
//! properties object (`window[id]`, `window[name]`, child frames by name),
//! the document's named items, `HTMLFormElement` and `HTMLCollection`
//! named properties. Each case in `cases/` is served on loopback and its
//! log must equal what Chrome 153 (headless shell 153.0.8010.12, Linux;
//! macOS gave the same logs) printed, in `chrome.json`. Re-record with
//! `python3 -m http.server 19180 --bind 127.0.0.1 --directory cases &` and
//! `CHROME=... node chrome.mjs http://127.0.0.1:19180 $(ls cases | grep html$ | grep -v child | sed 's/.html//') > chrome.json`.
//! Included from `pages.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lb_page::WaitUntil;

use super::common::{Resource, browser, context, open, serve_fn};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/named")
}

fn route(path: &str) -> Option<Resource> {
    let name = path.trim_start_matches('/');
    if name.contains('/') || name.contains("..") {
        return None;
    }
    let body = std::fs::read_to_string(dir().join("cases").join(name)).ok()?;
    Some(if name.ends_with(".js") {
        Resource::js(&body)
    } else {
        Resource::html(&body)
    })
}

/// Chrome's log for `case`.
fn chrome(case: &str) -> String {
    let all: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir().join("chrome.json")).unwrap()).unwrap();
    all[case].as_str().unwrap_or_default().to_owned()
}

async fn run(case: &str) {
    let srv = serve_fn(Arc::new(route)).await;
    let (b, ctx) = (browser(), context(false));
    let url = srv.url(&format!("/{case}.html"));
    let port = url
        .trim_start_matches("http://127.0.0.1:")
        .split('/')
        .next()
        .unwrap()
        .to_owned();
    let page = open(&b, &ctx, &url, WaitUntil::Load).await;
    let t0 = Instant::now();
    let got = loop {
        let log = match page.evaluate("L.join('\\n')").await.unwrap() {
            lb_js::JsValue::String(s) => s,
            v => panic!("{v:?}"),
        };
        if log.ends_with("\nend") || t0.elapsed() > Duration::from_secs(10) {
            break log;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    // Chrome was recorded on port 19180.
    let got = got.replace(&format!("127.0.0.1:{port}"), "127.0.0.1:PORT");
    let want = chrome(case).replace("127.0.0.1:19180", "127.0.0.1:PORT");
    let (g, w): (Vec<&str>, Vec<&str>) = (got.lines().collect(), want.lines().collect());
    let mut diff = String::new();
    for i in 0..g.len().max(w.len()) {
        let (a, b) = (g.get(i).copied(), w.get(i).copied());
        if a != b {
            diff.push_str(&format!(
                "line {i}\n  got:  {}\n  want: {}\n",
                a.unwrap_or("<none>"),
                b.unwrap_or("<none>")
            ));
        }
    }
    assert!(diff.is_empty(), "{case} differs from Chrome:\n{diff}");
}

#[tokio::test]
async fn window_properties_object() {
    run("proto").await;
}

#[tokio::test]
async fn window_properties_object_writes() {
    run("proto_mutate").await;
}

#[tokio::test]
async fn window_named_by_id() {
    run("ids").await;
}

#[tokio::test]
async fn window_named_by_name() {
    run("names").await;
}

#[tokio::test]
async fn window_named_frames() {
    run("frames").await;
}

#[tokio::test]
async fn document_named_items() {
    run("document").await;
}

#[tokio::test]
async fn form_named_items() {
    run("form").await;
}

#[tokio::test]
async fn collection_named_items() {
    run("collection").await;
}
