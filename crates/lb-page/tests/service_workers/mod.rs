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

/// Registrations belong to their browser context (in memory, ADR 0022): a
/// second context of the same browser sees none and its page is not
/// controlled, while a new page of the first context is.
#[tokio::test]
async fn registrations_are_per_context() {
    let hits = Arc::new(Mutex::new(HashMap::new()));
    let srv = serve_target(Arc::new(move |p: &str, t: &str| route(&hits, p, t))).await;
    let b = browser();
    let (c1, c2) = (context(false), context(false));
    let page = open(&b, &c1, &srv.url("/lifecycle/"), WaitUntil::Load).await;
    assert!(log_of(&page).await.ends_with("again"));
    let probe = "new Promise(r => navigator.serviceWorker.getRegistrations().then(rs => \
                 r(rs.length + ' ' + !!navigator.serviceWorker.controller)))";
    let check = |page: lb_page::Page| async move {
        page.evaluate(&format!("{probe}.then(v => window.R = v)"))
            .await
            .unwrap();
        let t0 = Instant::now();
        loop {
            if let Ok(lb_js::JsValue::String(s)) = page.evaluate("window.R || ''").await
                && !s.is_empty()
            {
                return s;
            }
            assert!(t0.elapsed() < Duration::from_secs(5));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let other = open(&b, &c2, &srv.url("/lifecycle/x.html"), WaitUntil::Load).await;
    assert_eq!(check(other).await, "0 false");
    let same = open(&b, &c1, &srv.url("/lifecycle/x.html"), WaitUntil::Load).await;
    assert_eq!(check(same).await, "1 true");
}

/// An idle version stops and the next event starts it again with fresh
/// globals, without fetching its script again (ADR 0029). Chrome 153
/// stopped it exactly 30 s after the last event and answered a message
/// and a fetch with a restarted worker (`tests/playwright/workers/
/// idle_chrome.json`: counter 1, 2, 3, then 1 after 42 s idle, then 2 for
/// a fetch, one script request); the timeout is shortened here.
#[tokio::test]
async fn idle_version_stops_and_restarts() {
    let page_src = "<!DOCTYPE html><title>idle</title><script>\
        navigator.serviceWorker.register('/idle/sw.js');\
        window.ask = () => new Promise((r) => { navigator.serviceWorker.onmessage = (e) => r(String(e.data)); \
          navigator.serviceWorker.controller.postMessage('inc'); });\
        window.ping = () => fetch('/idle/ping').then((r) => r.text());\
        </script>";
    let sw = "self.n = 0;\
        self.addEventListener('install', () => self.skipWaiting());\
        self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));\
        self.addEventListener('message', (e) => { self.n++; e.source.postMessage(self.n); });\
        self.addEventListener('fetch', (e) => { if (new URL(e.request.url).pathname === '/idle/ping') \
          e.respondWith(new Response(String(++self.n))); });";
    let fetches = Arc::new(Mutex::new(0u32));
    let f2 = fetches.clone();
    let srv = serve_target(Arc::new(move |p: &str, _t: &str| match p {
        "/idle/sw.js" => {
            *f2.lock().unwrap() += 1;
            Some(Resource::new("text/javascript", sw).header("Cache-Control", "no-store"))
        }
        "/idle/" => Some(Resource::new("text/html", page_src)),
        _ => None,
    }))
    .await;
    let (b, ctx) = (browser(), context(false));
    let timeout = Duration::from_millis(400);
    ctx.set_sw_idle_timeout(timeout);
    let page = open(&b, &ctx, &srv.url("/idle/"), WaitUntil::Load).await;
    let eval = |expr: &'static str| {
        let page = &page;
        async move {
            page.evaluate(&format!("window.R = ''; {expr}.then(v => window.R = v)"))
                .await
                .unwrap();
            let t0 = Instant::now();
            loop {
                if let Ok(lb_js::JsValue::String(s)) = page.evaluate("window.R").await
                    && !s.is_empty()
                {
                    return s;
                }
                assert!(t0.elapsed() < Duration::from_secs(5), "{expr}");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    };
    let t0 = Instant::now();
    while !matches!(
        page.evaluate("!!navigator.serviceWorker.controller").await,
        Ok(lb_js::JsValue::Bool(true))
    ) {
        assert!(t0.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(eval("ask()").await, "1");
    tokio::time::sleep(timeout / 2).await;
    assert_eq!(eval("ask()").await, "2");
    tokio::time::sleep(timeout / 2).await;
    // Still running: each event moved the deadline.
    assert_eq!(eval("ask()").await, "3");
    tokio::time::sleep(timeout * 2).await;
    // Stopped and started again for the message: fresh globals.
    assert_eq!(eval("ask()").await, "1");
    assert_eq!(eval("ping()").await, "2");
    tokio::time::sleep(timeout * 2).await;
    // A fetch starts it too.
    assert_eq!(eval("ping()").await, "1");
    assert_eq!(
        *fetches.lock().unwrap(),
        1,
        "the script is not fetched again"
    );
}
