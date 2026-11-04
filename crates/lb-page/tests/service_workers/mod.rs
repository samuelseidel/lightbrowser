//! Service workers and the Cache API as in Chrome (ADR 0022): each case in
//! `cases/` is served on loopback (`<!--LOG-->` becomes `prelude.html`,
//! `//PRELUDE` in scripts becomes `sw_prelude.js`, `{{v}}` the `v` query
//! parameter; `allowed-*` files send `Service-Worker-Allowed: /`;
//! `/<case>/hits` counts the requests per path) and its log must equal
//! what Chrome 153 (Chrome for Testing 153.0.8010.12) printed, in
//! `chrome.json`. A load that ends with `again` is followed by a reload.
//! Re-record with `python3 serve.py cases 18921 &` and
//! `node chrome.mjs http://127.0.0.1:18921 $(ls cases) > chrome.json`
//! (fresh server: the hit counts start at 0). Included from `pages.rs`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lb_page::WaitUntil;

use super::common::{Resource, browser, context, open, serve_target};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/service_workers")
}

fn route(hits: &Mutex<HashMap<String, u32>>, path: &str, target: &str) -> Option<Resource> {
    let rel = path.trim_start_matches('/');
    if let Some(case) = rel.strip_suffix("hits") {
        let h = hits.lock().unwrap();
        let mut m = serde_json::Map::new();
        let mut keys: Vec<&String> = h.keys().filter(|k| k.starts_with(case)).collect();
        keys.sort();
        for k in keys {
            m.insert(k[case.len()..].to_owned(), h[k].into());
        }
        return Some(Resource::new(
            "application/json",
            serde_json::Value::Object(m).to_string(),
        ));
    }
    *hits.lock().unwrap().entry(rel.to_owned()).or_default() += 1;
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
        "js" => "text/javascript",
        "json" => "application/json",
        "txt" => "text/plain",
        "css" => "text/css",
        _ => "application/octet-stream",
    };
    let body = match ext {
        "html" => {
            let pre = std::fs::read_to_string(dir().join("prelude.html")).ok()?;
            String::from_utf8_lossy(&body)
                .replace("<!--LOG-->", pre.trim())
                .into_bytes()
        }
        "js" => {
            let pre = std::fs::read_to_string(dir().join("sw_prelude.js")).ok()?;
            let v = url::Url::parse(&format!("http://x{target}"))
                .ok()
                .and_then(|u| {
                    u.query_pairs()
                        .find(|(k, _)| k == "v")
                        .map(|(_, v)| v.into_owned())
                })
                .unwrap_or_default();
            String::from_utf8_lossy(&body)
                .replace("//PRELUDE", pre.trim())
                .replace("{{v}}", &v)
                .into_bytes()
        }
        _ => body,
    };
    let mut r = Resource::new(ct, body).header("Cache-Control", "no-store");
    if file
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("allowed-"))
    {
        r = r.header("Service-Worker-Allowed", "/");
    }
    Some(r)
}

/// Chrome's log for `case`.
fn chrome(case: &str) -> String {
    let all: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir().join("chrome.json")).unwrap()).unwrap();
    all[case].as_str().unwrap().to_owned()
}

async fn log_of(page: &lb_page::Page) -> String {
    let t0 = Instant::now();
    loop {
        let log = match page.evaluate("L.join('\\n')").await {
            Ok(lb_js::JsValue::String(s)) => s,
            _ => String::new(),
        };
        let last = log.rsplit('\n').next().unwrap_or("");
        if last == "end" || last == "again" || t0.elapsed() > Duration::from_secs(8) {
            return log;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn run(case: &str) {
    let hits = Arc::new(Mutex::new(HashMap::new()));
    let srv = serve_target(Arc::new(move |p: &str, t: &str| route(&hits, p, t))).await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &srv.url(&format!("/{case}/")), WaitUntil::Load).await;
    let mut logs = Vec::new();
    for _ in 0..5 {
        let l = log_of(&page).await;
        let more = l.ends_with("again");
        logs.push(l);
        if !more {
            break;
        }
        page.reload().await.unwrap();
        page.wait_for(WaitUntil::Load, Duration::from_secs(10))
            .await
            .unwrap();
    }
    let got = logs.join("\n");
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
    cache, claim, clients, errors, fetch, lifecycle, messages, navigation, navupdate, scope,
    update,
);
