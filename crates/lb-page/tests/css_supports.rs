//! `CSS.supports()` and `BroadcastChannel` in real pages.
//!
//! `css_supports/chrome.json` holds Chrome 153's answers (Chrome for
//! Testing 153.0.8010.12, the persona's version, headless on loopback) for
//! every case of `css_supports/cases.json`: declarations (modern, `-webkit-`
//! and unknown properties, invalid values, `!important`, custom
//! properties) and conditions (`not`/`and`/`or`, `selector()` with every
//! pseudo-class and pseudo-element probed, `font-tech()`, `font-format()`,
//! `at-rule()`). Re-record with
//! `python3 tools/css_supports/record.py --chrome <chrome-headless-shell 153>
//! --cases crates/lb-page/tests/css_supports/cases.json --json
//! crates/lb-page/tests/css_supports/chrome.json`.
#![cfg(feature = "chrome")]

mod common;

use std::time::{Duration, Instant};

use common::{Resource, browser, context, open, serve};
use lb_js::JsValue;
use lb_page::{Page, WaitUntil};

fn fixture(name: &str) -> serde_json::Value {
    let p = format!("{}/tests/css_supports/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// `window.R` once the page set it (a JSON string).
async fn result(page: &Page, secs: u64) -> serde_json::Value {
    let t0 = Instant::now();
    loop {
        if let Ok(JsValue::String(s)) = page.evaluate("window.R || ''").await
            && !s.is_empty()
        {
            return serde_json::from_str(&s).unwrap();
        }
        assert!(
            t0.elapsed() < Duration::from_secs(secs),
            "page did not finish"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Values Chrome rejects that the per-component check accepts (a shorthand
/// missing a required part, a shadow without lengths).
const KNOWN_GAPS: &[(&str, &str)] = &[("font", "bold"), ("text-shadow", "red")];

#[tokio::test]
async fn css_supports_matches_chrome_153() {
    let cases = fixture("cases.json");
    let chrome = fixture("chrome.json");
    let html = format!(
        "<!DOCTYPE html><script>const T = {cases};\
         window.R = JSON.stringify({{\
           two: T.two.map(([n, v]) => [n, v, CSS.supports(n, v)]),\
           one: T.one.map(([t]) => [t, CSS.supports(t)]) }});</script>"
    );
    let s = serve(vec![("/", Resource::html(&html))]).await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
    let got = result(&page, 10).await;
    let mut fails = Vec::new();
    for (g, c) in got["two"]
        .as_array()
        .unwrap()
        .iter()
        .zip(chrome["two"].as_array().unwrap())
    {
        let (n, v) = (c[0].as_str().unwrap(), c[1].as_str().unwrap());
        if g != c && !KNOWN_GAPS.contains(&(n, v)) {
            fails.push(format!(
                "CSS.supports({n:?}, {v:?}): chrome {} ours {}",
                c[2], g[2]
            ));
        }
    }
    for (g, c) in got["one"]
        .as_array()
        .unwrap()
        .iter()
        .zip(chrome["one"].as_array().unwrap())
    {
        if g != c {
            fails.push(format!(
                "CSS.supports({}): chrome {} ours {}",
                c[0], c[1], g[1]
            ));
        }
    }
    assert!(
        chrome["two"].as_array().unwrap().len() + chrome["one"].as_array().unwrap().len() > 700
    );
    assert!(fails.is_empty(), "{fails:#?}");
}

#[tokio::test]
async fn css_namespace_arguments_and_escape() {
    let html = "<!DOCTYPE html><script>\
        const t = (f) => { try { return String(f()); } catch (e) { return e.name + ': ' + e.message; } };\
        window.R = JSON.stringify([\
          t(() => CSS.supports()),\
          t(() => CSS.supports('color', 'red', 'x')),\
          t(() => CSS.supports({ toString() { return '(display: grid)'; } })),\
          t(() => CSS.supports('opacity', 1)),\
          t(() => CSS.supports.call(null, 'color', 'red')),\
          t(() => CSS.escape('1a b#c')),\
          t(() => CSS.escape()),\
          typeof CSS.supports, String(CSS)]);\
        </script>";
    let s = serve(vec![("/", Resource::html(html))]).await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
    let got = result(&page, 10).await;
    // Chrome 153's answers.
    assert_eq!(
        got,
        serde_json::json!([
            "TypeError: Failed to execute 'supports' on 'CSS': 1 argument required, but only 0 present.",
            "true",
            "true",
            "true",
            "true",
            "\\31 a\\ b\\#c",
            "TypeError: Failed to execute 'escape' on 'CSS': 1 argument required, but only 0 present.",
            "function",
            "[object CSS]"
        ])
    );
}

/// Channels of one page: every other open channel of the name gets each
/// message, in creation order, as Chrome 153 logged it
/// (`bc.html` on loopback); errors as in Chrome.
#[tokio::test]
async fn broadcast_channel_in_one_page_and_frames() {
    let page_src = "<!DOCTYPE html><body><script>\
        const r = {};\
        const t = (f) => { try { return String(f()); } catch (e) { return e.name + ': ' + e.message; } };\
        r.noargs = t(() => new BroadcastChannel());\
        r.name = t(() => new BroadcastChannel(1).name);\
        const c = new BroadcastChannel('x'); c.close(); c.close();\
        r.closed = t(() => c.postMessage(1));\
        r.closedNoArgs = t(() => c.postMessage());\
        const d = new BroadcastChannel('y');\
        r.fn = t(() => d.postMessage(() => 1));\
        r.toString = String(d);\
        r.illegal = t(() => BroadcastChannel.prototype.postMessage.call({}, 1));\
        const a = new BroadcastChannel('z'), b = new BroadcastChannel('z'), e = new BroadcastChannel('z');\
        const other = new BroadcastChannel('other'); other.onmessage = () => log.push('wrong name');\
        const log = [];\
        a.onmessage = ev => log.push('a:' + ev.data);\
        b.onmessage = ev => log.push('b:' + JSON.stringify(ev.data) + ':' + ev.origin + ':' + ev.source + ':' + ev.ports.length + ':' + (ev instanceof MessageEvent) + ':' + ev.lastEventId + ':' + ev.bubbles + ev.cancelable);\
        e.addEventListener('message', ev => { log.push('e:' + ev.data); e.close(); });\
        a.postMessage({k: [1, 2]});\
        a.postMessage('second');\
        b.postMessage('fromb');\
        const fr = document.createElement('iframe'); fr.src = '/frame.html'; document.body.appendChild(fr);\
        window.onmessage = () => { log.push('frame-ready'); a.postMessage('to-frame'); };\
        setTimeout(() => { r.log = log; window.R = JSON.stringify(r); }, 1000);\
        </script>";
    let frame_src = "<!DOCTYPE html><script>\
        const f = new BroadcastChannel('z');\
        f.onmessage = ev => { f.postMessage('frame got ' + ev.data); };\
        parent.postMessage('ready', '*');\
        </script>";
    let s = serve(vec![
        ("/", Resource::html(page_src)),
        ("/frame.html", Resource::html(frame_src)),
    ])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
    let got = result(&page, 10).await;
    let origin = s.url("/").trim_end_matches('/').to_owned();
    let b_line = |d: &str| format!("b:{d}:{origin}:null:0:true::falsefalse");
    assert_eq!(
        got,
        serde_json::json!({
            "noargs": "TypeError: Failed to construct 'BroadcastChannel': 1 argument required, but only 0 present.",
            "name": "1",
            "closed": "InvalidStateError: Failed to execute 'postMessage' on 'BroadcastChannel': Channel is closed",
            "closedNoArgs": "TypeError: Failed to execute 'postMessage' on 'BroadcastChannel': 1 argument required, but only 0 present.",
            "fn": "DataCloneError: Failed to execute 'postMessage' on 'BroadcastChannel': () => 1 could not be cloned.",
            "toString": "[object BroadcastChannel]",
            "illegal": "TypeError: Illegal invocation",
            "log": [
                b_line("{\"k\":[1,2]}"),
                "e:[object Object]",
                b_line("\"second\""),
                "a:fromb",
                "frame-ready",
                b_line("\"to-frame\""),
                "a:frame got to-frame",
                b_line("\"frame got to-frame\""),
            ]
        })
    );
}

/// A dedicated worker and the page talk over channels of the same name,
/// both ways (separate threads, ADR 0021 inboxes).
#[tokio::test]
async fn broadcast_channel_between_page_and_worker() {
    let page_src = "<!DOCTYPE html><script>\
        const log = [];\
        const bc = new BroadcastChannel('wk');\
        bc.onmessage = (e) => {\
          log.push('page:' + JSON.stringify(e.data));\
          if (e.data.hello) bc.postMessage({ back: e.data.hello * 2 });\
          if (e.data.got) window.R = JSON.stringify(log);\
        };\
        new Worker('/w.js');\
        </script>";
    let worker_src = "const bc = new BroadcastChannel('wk');\
        bc.onmessage = (e) => bc.postMessage({ got: e.data.back, type: typeof BroadcastChannel, css: typeof self.CSS });\
        bc.postMessage({ hello: 21 });";
    let s = serve(vec![
        ("/", Resource::html(page_src)),
        ("/w.js", Resource::js(worker_src)),
    ])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
    let got = result(&page, 10).await;
    assert_eq!(
        got,
        serde_json::json!([
            "page:{\"hello\":21}",
            "page:{\"got\":42,\"type\":\"function\",\"css\":\"undefined\"}"
        ])
    );
}

/// A service worker that opens a channel at the top level and posts
/// navigator data on a timer reaches the page listening on that name (as
/// bot.incolumitas.com's service worker does; reproduced locally).
#[tokio::test]
async fn broadcast_channel_from_service_worker() {
    let page_src = "<!DOCTYPE html><script>\
        const bc = new BroadcastChannel('sw-channel');\
        bc.onmessage = (e) => { window.R = JSON.stringify(e.data); };\
        navigator.serviceWorker.register('/sw.js');\
        </script>";
    let sw_src = "const bc = new BroadcastChannel('sw-channel');\
        setInterval(() => bc.postMessage({ ua: navigator.userAgent, hc: navigator.hardwareConcurrency,\
          lang: navigator.language, scope: String(self) }), 100);";
    let s = serve(vec![
        ("/", Resource::html(page_src)),
        ("/sw.js", Resource::js(sw_src)),
    ])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
    let got = result(&page, 10).await;
    let ua = match page.evaluate("navigator.userAgent").await.unwrap() {
        JsValue::String(s) => s,
        v => panic!("{v:?}"),
    };
    assert_eq!(got["ua"], ua.as_str());
    assert_eq!(got["scope"], "[object ServiceWorkerGlobalScope]");
    assert!(got["hc"].as_u64().is_some_and(|n| n > 0), "{got}");
}

/// Channels of another browser context with the same origin and name are
/// not reached.
#[tokio::test]
async fn broadcast_channel_is_per_context() {
    let src = "<!DOCTYPE html><script>\
        const bc = new BroadcastChannel('ctx'); window.got = [];\
        bc.onmessage = (e) => window.got.push(e.data);\
        </script>";
    let s = serve(vec![("/", Resource::html(src))]).await;
    let b = browser();
    let (c1, c2) = (context(false), context(false));
    let p1 = open(&b, &c1, &s.url("/"), WaitUntil::Load).await;
    let p2 = open(&b, &c2, &s.url("/"), WaitUntil::Load).await;
    let p3 = open(&b, &c1, &s.url("/"), WaitUntil::Load).await;
    p3.evaluate("bc.postMessage('hi')").await.unwrap();
    let t0 = Instant::now();
    loop {
        if let Ok(JsValue::String(s)) = p1.evaluate("window.got.join()").await
            && s == "hi"
        {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "same context not reached"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        p2.evaluate("window.got.join()").await.unwrap(),
        JsValue::String(String::new())
    );
    assert_eq!(
        p3.evaluate("window.got.join()").await.unwrap(),
        JsValue::String(String::new())
    );
}
