//! Dedicated and shared workers, message ports and structured clone as in
//! Chrome (ADR 0021): each case in `cases/` is served on loopback
//! (`<!--LOG-->` becomes `prelude.html`; `/<case>/echo-cookie` answers the
//! request's `Cookie` header) and its log must equal what Chrome 153
//! (Chrome for Testing 153.0.8010.12) printed for the same pages, in
//! `chrome.json`. Re-record with `python3 serve.py cases 18911 &` and
//! `node chrome.mjs http://127.0.0.1:18911 $(ls cases) > chrome.json`.
//! Included from `pages.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lb_page::WaitUntil;

use super::common::{Resource, browser, context, open, serve_fn};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/workers")
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
    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
    let ct = match ext {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
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
    Some(Resource::new(ct, body))
}

/// Chrome's log for `case`.
fn chrome(case: &str) -> String {
    let all: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir().join("chrome.json")).unwrap()).unwrap();
    all[case].as_str().unwrap().to_owned()
}

async fn run(case: &str) {
    let srv = serve_fn(Arc::new(route)).await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &srv.url(&format!("/{case}/")), WaitUntil::Load).await;
    let t0 = Instant::now();
    let got = loop {
        let log = match page.evaluate("L.join('\\n')").await.unwrap() {
            lb_js::JsValue::String(s) => s,
            v => panic!("{v:?}"),
        };
        if log.ends_with("\nend") || log == "end" || t0.elapsed() > Duration::from_secs(5) {
            break log;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(got, chrome(case), "{case}:\n{got}");
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
    basic, clone, errors, imports, network, scope, shared, terminate, transfer, urls,
);
