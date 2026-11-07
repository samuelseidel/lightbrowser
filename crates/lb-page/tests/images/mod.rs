//! Image reuse within a document as in Chrome: each case in `cases/` is
//! served on loopback (`slow-*` files answer after 300 ms; `<!--LOG-->`
//! becomes `prelude.html`) and its log and the server's request counts
//! must equal what Chrome 153 (Chrome for Testing 153.0.8010.12) gave for
//! the same pages, in `chrome.json`. Elements asking for a URL that is
//! loading wait on that request; one that another element holds loaded is
//! complete at once; failed loads are fetched again. Blink keeps the
//! waiting elements in a hash set, so the order of their `load` events is
//! not compared (each must still be its own task). The order of an
//! image `load` task against a `setTimeout(0)` is not compared either.
//! Re-record with
//! `python3 serve.py 18931 &` and
//! `node chrome.mjs http://127.0.0.1:18931 $(ls cases | sed 's/.html//') > chrome.json`.
//! Included from `pages.rs`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lb_page::WaitUntil;

use super::common::{Resource, browser, context, open, serve_fn};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/images")
}

fn route(path: &str) -> Option<Resource> {
    let mut parts = path.trim_start_matches('/').splitn(2, '/');
    let case = parts.next()?;
    let name = parts.next().unwrap_or("");
    if name.is_empty() || name == "index.html" {
        let pre = std::fs::read_to_string(dir().join("prelude.html")).ok()?;
        let html =
            std::fs::read_to_string(dir().join("cases").join(format!("{case}.html"))).ok()?;
        return Some(
            Resource::html(&html.replace("<!--LOG-->", pre.trim()))
                .header("Cache-Control", "no-store"),
        );
    }
    let (file, delay) = match name.strip_prefix("slow-") {
        Some(f) => (f, 300),
        None => (name, 0),
    };
    if file != "a.png" && file != "b.png" {
        return None;
    }
    let body = std::fs::read(dir().join(file)).ok()?;
    Some(
        Resource::new("image/png", body)
            .header("Cache-Control", "no-store")
            .delay(delay),
    )
}

/// Lines of a log whose order Blink leaves to a hash set, sorted; each
/// `load`/`error` line must be followed by its own microtask line.
fn normalise(case: &str, log: &str) -> Vec<String> {
    let lines: Vec<String> = log.lines().map(str::to_owned).collect();
    if !matches!(case, "script_same" | "error_same" | "parse_same") {
        return lines;
    }
    let mut out = lines.clone();
    out.sort();
    out
}

fn tasks_separate(log: &str) {
    let lines: Vec<&str> = log.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        let id = l
            .strip_prefix("load ")
            .or_else(|| l.strip_prefix("error "))
            .and_then(|r| r.split(' ').next());
        if let Some(id) = id
            && lines.iter().any(|m| *m == format!("micro {id}"))
        {
            assert_eq!(
                lines.get(i + 1).copied(),
                Some(format!("micro {id}").as_str()),
                "{log}"
            );
        }
    }
}

async fn run(case: &str) {
    let srv = serve_fn(Arc::new(route)).await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &srv.url(&format!("/{case}/")), WaitUntil::Load).await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let got = match page.evaluate("L.join('\\n')").await.unwrap() {
        lb_js::JsValue::String(s) => s,
        v => panic!("{v:?}"),
    };
    let all: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir().join("chrome.json")).unwrap()).unwrap();
    let want = all[case]["log"].as_str().unwrap();
    assert_eq!(
        normalise(case, &got),
        normalise(case, want),
        "{case}:\n{got}"
    );
    tasks_separate(&got);
    let mut hits: BTreeMap<String, u64> = BTreeMap::new();
    let prefix = format!("/{case}/");
    for (_, target, _, _) in srv.requests() {
        if let Some(rest) = target.strip_prefix(&prefix) {
            *hits.entry(rest.to_owned()).or_default() += 1;
        }
    }
    let want_hits: BTreeMap<String, u64> =
        serde_json::from_value(all[case]["hits"].clone()).unwrap();
    assert_eq!(hits, want_hits, "{case}: requests");
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

cases!(after_load, error_same, parse_same, reset_src, script_same);
