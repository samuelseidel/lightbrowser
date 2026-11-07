//! The egress policy (ADR 0044) through pages: every surface that makes a
//! request on a page's behalf goes through the context's policy, and a
//! refused request looks to the page like a request Chrome blocked.
//!
//! `tests/egress/chrome.json` is local Chrome 153.0.8010.12 (headless shell,
//! loopback only) on the same `probes.html` with the blocked host failed by
//! `Fetch.failRequest` (`BlockedByClient`): the navigation's error text, and
//! what `fetch`, XHR, an image, a script, a worker's `fetch` and a WebSocket
//! report. Here the same page runs with the loopback policy and a host the
//! policy refuses. The expectation for every surface but WebSocket's
//! console text is Chrome's recording; Chrome cannot be told to block a
//! WebSocket, so only its events (`error`, close 1006, not clean) are
//! recorded, and the text we produce for it differs from the recorded
//! `net::ERR_NAME_NOT_RESOLVED` line in the code alone (checked in
//! `lb-net`'s `tests/egress.rs`).
//!
//! No external network: the page is on loopback, the refused host is never
//! looked up (a recording hosts override proves it).
//! Re-record: `crates/lb-page/tests/egress/chrome.mjs`.
#![cfg(feature = "chrome")]

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Resource, browser, context, serve};
use lb_js::JsValue;
use lb_net::EgressPolicy;
use lb_page::{PageError, WaitUntil};

fn recording() -> serde_json::Value {
    let path = format!("{}/tests/egress/chrome.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn probes_page() -> Resource {
    let path = format!("{}/tests/egress/probes.html", env!("CARGO_MANIFEST_DIR"));
    Resource::html(&std::fs::read_to_string(path).unwrap())
}

/// A context under `policy` whose hosts override records every name it is
/// asked to look up (and knows none).
fn policed(policy: EgressPolicy) -> (lb_page::BrowserContext, Arc<Mutex<Vec<String>>>) {
    let ctx = context(false);
    ctx.set_egress(Arc::new(policy));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let c2 = calls.clone();
    ctx.set_resolve_hook(Some(Arc::new(move |host: &str| {
        c2.lock().unwrap().push(host.to_owned());
        None
    })));
    (ctx, calls)
}

async fn eval_str(page: &lb_page::Page, js: &str) -> String {
    match page.evaluate(js).await.unwrap() {
        JsValue::String(s) => s,
        other => panic!("{js}: {other:?}"),
    }
}

async fn wait_for(page: &lb_page::Page, cond: &str) {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        if page.evaluate(cond).await.unwrap() == JsValue::Bool(true) {
            return;
        }
        assert!(Instant::now() < end, "timed out waiting for {cond}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn every_surface_fails_as_chrome_reports_a_blocked_request() {
    let s = serve(vec![("/", probes_page())]).await;
    let (ctx, calls) = policed(EgressPolicy::loopback());
    let b = browser();
    let page = b.new_page(&ctx).await.unwrap();
    page.goto(&s.url("/"), WaitUntil::Load, Duration::from_secs(20))
        .await
        .unwrap();
    wait_for(&page, "window.R.done === true").await;
    let got: serde_json::Value = serde_json::from_str(
        &eval_str(
            &page,
            "var r = Object.assign({}, window.R); delete r.done; JSON.stringify(r)",
        )
        .await,
    )
    .unwrap();
    assert_eq!(got, recording()["probes"], "got {got:#}");
    // The page and nothing else reached the server, and no name was looked
    // up for the refused host.
    assert_eq!(
        s.requests()
            .iter()
            .filter(|r| r.1 != "/favicon.ico")
            .count(),
        1
    );
    assert!(
        calls.lock().unwrap().is_empty(),
        "{:?}",
        calls.lock().unwrap()
    );
}

#[tokio::test]
async fn a_blocked_navigation_carries_chrome_s_error_code() {
    let (ctx, calls) = policed(EgressPolicy::loopback());
    let b = browser();
    let page = b.new_page(&ctx).await.unwrap();
    let want = recording()["navigate"]["errorText"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(want, "net::ERR_BLOCKED_BY_CLIENT");
    for url in [
        "http://blocked.test:8099/",
        "https://blocked.test/",
        "http://10.1.2.3:8080/",
    ] {
        match page
            .goto(url, WaitUntil::Load, Duration::from_secs(20))
            .await
        {
            Err(PageError::Net(e)) => assert!(e.starts_with(&want), "{url}: {e}"),
            other => panic!("{url}: {other:?}"),
        }
    }
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn allowed_hosts_load_and_the_default_context_is_open() {
    let b = browser();
    let s = serve(vec![("/", Resource::html("<body>ok</body>"))]).await;
    // `127.0.0.1` is loopback; a name entry works through the override.
    let (ctx, _) = policed(EgressPolicy::loopback());
    let page = b.new_page(&ctx).await.unwrap();
    page.goto(&s.url("/"), WaitUntil::Load, Duration::from_secs(20))
        .await
        .unwrap();
    assert_eq!(eval_str(&page, "document.body.textContent").await, "ok");
    // Allowlist mode with no entries refuses even loopback.
    let ctx = context(false);
    ctx.set_egress(Arc::new(
        EgressPolicy::new(lb_net::EgressMode::AllowList, &[] as &[&str]).unwrap(),
    ));
    let page = b.new_page(&ctx).await.unwrap();
    assert!(matches!(
        page.goto(&s.url("/"), WaitUntil::Load, Duration::from_secs(20))
            .await,
        Err(PageError::Net(_))
    ));
    // No policy: open, as before.
    let page = b.new_page(&context(false)).await.unwrap();
    page.goto(&s.url("/"), WaitUntil::Load, Duration::from_secs(20))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_refused_favicon_is_not_requested() {
    let b = browser();
    let s = serve(vec![(
        "/",
        Resource::html("<link rel=icon href=\"http://blocked.test:8099/i.png\"><body>x</body>"),
    )])
    .await;
    let (ctx, calls) = policed(EgressPolicy::loopback());
    let page = b.new_page(&ctx).await.unwrap();
    page.goto(&s.url("/"), WaitUntil::Load, Duration::from_secs(20))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        calls.lock().unwrap().is_empty(),
        "{:?}",
        calls.lock().unwrap()
    );
    // And one that is allowed is requested.
    let s2 = serve(vec![("/", Resource::html("<body>y</body>"))]).await;
    let page = b.new_page(&ctx).await.unwrap();
    page.goto(&s2.url("/"), WaitUntil::Load, Duration::from_secs(20))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(s2.requests().iter().any(|r| r.1 == "/favicon.ico"));
}

#[tokio::test]
async fn a_service_worker_fetch_is_refused() {
    let s = serve(vec![
        (
            "/",
            Resource::html(
                "<body><script>\
                 navigator.serviceWorker.onmessage = function (e) { window.SW = e.data; };\
                 navigator.serviceWorker.register('/sw.js').then(function () { return navigator.serviceWorker.ready; })\
                 .then(function (r) { r.active.postMessage('go'); });\
                 </script></body>",
            ),
        ),
        (
            "/sw.js",
            Resource::js(
                "addEventListener('message', function (e) {\
                 fetch('http://blocked.test:8099/sw').then(\
                 function () { e.source.postMessage('resolved'); },\
                 function (err) { e.source.postMessage(err.name + ': ' + err.message); }); });",
            ),
        ),
    ])
    .await;
    let b = browser();
    let (ctx, calls) = policed(EgressPolicy::loopback());
    let page = b.new_page(&ctx).await.unwrap();
    page.goto(&s.url("/"), WaitUntil::Load, Duration::from_secs(20))
        .await
        .unwrap();
    wait_for(&page, "typeof window.SW === 'string'").await;
    assert_eq!(
        eval_str(&page, "window.SW").await,
        recording()["probes"]["worker"].as_str().unwrap()
    );
    assert!(calls.lock().unwrap().is_empty());
}
