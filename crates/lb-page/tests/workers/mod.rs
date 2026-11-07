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

/// The worker globals' surface, dumped in a dedicated, a shared and a
/// service worker (ADR 0022) by the same code as
/// `tools/dump_worker_surface.js`: own properties of
/// the global (names, descriptor kinds, lengths, order) and every
/// interface object's prototype and statics must equal the capture of
/// Chrome 153 (`tests/fixtures/chromium_153_worker_surface.json`).
#[tokio::test]
async fn surface_matches_chrome() {
    let tool = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/dump_worker_surface.js"),
    )
    .unwrap();
    // The capture's `dumpScope`, as an expression (no global binding).
    let start = tool.find("function dumpScope()").unwrap();
    let end = tool[start..].find("\n}\n").unwrap() + start + 2;
    let dump = &tool[start..end];
    let worker_src = |kind: &str| {
        let body = format!("({dump})()");
        match kind {
            "shared" => {
                format!("onconnect = (e) => e.ports[0].postMessage(JSON.stringify({body}));")
            }
            // A listener: the capture's footprint is the same as Chrome's.
            "service" => format!(
                "addEventListener('message', (e) => e.source.postMessage(JSON.stringify({body})));"
            ),
            _ => format!("postMessage(JSON.stringify({body}));"),
        }
    };
    let page = "<!doctype html><script>var R={};\
        var w=new Worker('/d.js');w.onmessage=e=>R.dedicated=e.data;\
        var s=new SharedWorker('/s.js');s.port.onmessage=e=>R.shared=e.data;\
        navigator.serviceWorker.onmessage=e=>R.service=e.data;\
        navigator.serviceWorker.register('/v.js').then(()=>navigator.serviceWorker.ready)\
        .then(r=>r.active.postMessage('dump'));</script>";
    let srv = super::common::serve(vec![
        ("/", Resource::html(page)),
        ("/d.js", Resource::js(&worker_src("dedicated"))),
        ("/s.js", Resource::js(&worker_src("shared"))),
        ("/v.js", Resource::js(&worker_src("service"))),
    ])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &srv.url("/"), WaitUntil::Load).await;
    let t0 = Instant::now();
    let got = loop {
        let v = page
            .evaluate("R.dedicated && R.shared && R.service ? JSON.stringify(R) : ''")
            .await
            .unwrap();
        if let lb_js::JsValue::String(s) = &v
            && !s.is_empty()
        {
            break s.clone();
        }
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "workers did not answer"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let got: serde_json::Value = serde_json::from_str(&got).unwrap();
    let want: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/chromium_153_worker_surface.json"),
        )
        .unwrap(),
    )
    .unwrap();
    // Engine built-ins that differ on the window too (not worker
    // specific): V8 152 lacks `Iterator.zip`, `Iterator.zipKeyed` and
    // `Iterator.prototype.join`, and Blink's `WebAssembly.compileStreaming`
    // / `instantiateStreaming` are not installed. Removed from both sides.
    let known = |name: &str, list: &serde_json::Value| -> serde_json::Value {
        let drop: &[&str] = match name {
            "Iterator" => &["zip", "zipKeyed", "join"],
            "WebAssembly" => &["compileStreaming", "instantiateStreaming"],
            _ => &[],
        };
        serde_json::Value::Array(
            list.as_array()
                .into_iter()
                .flatten()
                .filter(|p| !drop.contains(&p[0].as_str().unwrap_or("")))
                .cloned()
                .collect(),
        )
    };
    let mut bad = Vec::new();
    for kind in ["dedicated", "shared", "service"] {
        let g: serde_json::Value = serde_json::from_str(got[kind].as_str().unwrap()).unwrap();
        let w = &want[kind];
        for key in ["globalProps", "globalSymbols", "protoChain", "toString"] {
            if g[key] != w[key] {
                bad.push(format!("{kind} {key}:\n got {}\nwant {}", g[key], w[key]));
            }
        }
        for (name, wi) in w["interfaces"].as_object().unwrap() {
            let gi = &g["interfaces"][name];
            for key in [
                "length",
                "static",
                "proto",
                "protoSymbols",
                "parent",
                "toStringTag",
            ] {
                let (gv, wv) = match key {
                    "static" | "proto" => (known(name, &gi[key]), known(name, &wi[key])),
                    _ => (gi[key].clone(), wi[key].clone()),
                };
                if gv != wv {
                    bad.push(format!(
                        "{kind} {name}.{key}:\n got {}\nwant {}",
                        gi[key], wi[key]
                    ));
                }
            }
        }
        for (name, wn) in w["namespaces"].as_object().unwrap() {
            if known(name, &g["namespaces"][name]["props"]) != known(name, &wn["props"]) {
                bad.push(format!("{kind} namespace {name}"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} differences:\n{}",
        bad.len(),
        bad.join("\n")
    );
}
