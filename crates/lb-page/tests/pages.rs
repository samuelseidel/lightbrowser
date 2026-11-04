//! Pages with scripts against a loopback server: the DOM after load.
#![cfg(feature = "chrome")]

mod common;
mod frames;
mod report;
mod stylesheets;

use std::time::Duration;

use common::{Resource, body, browser, context, open, serve};
use lb_js::JsValue;
use lb_page::WaitUntil;

#[tokio::test]
async fn inline_script_appends_nodes() {
    let s = serve(vec![(
        "/",
        Resource::html(
            "<!DOCTYPE html><html><head><title>t</title></head><body><div id=a></div>\
             <script>var s = document.createElement('span'); s.textContent = 'hi';\
             document.getElementById('a').appendChild(s);\
             document.body.appendChild(document.createElement('p')).className = 'x';</script></body></html>",
        ),
    )])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
    let html = page.content().await.unwrap();
    assert!(
        html.starts_with("<!DOCTYPE html><html><head><title>t</title></head>"),
        "{html}"
    );
    assert!(
        html.contains("<div id=\"a\"><span>hi</span></div>"),
        "{html}"
    );
    assert!(html.contains("<p class=\"x\"></p></body></html>"), "{html}");
}

#[tokio::test]
async fn document_write_during_parse() {
    let s = serve(vec![(
        "/",
        Resource::html(
            "<body><p>before</p><script>document.write('<p id=w>written ' + document.getElementsByTagName('p').length + '</p>')</script><p>after</p></body>",
        ),
    )])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
    let html = body(&page).await;
    assert!(html.contains("<p>before</p>"), "{html}");
    let w = html.find("<p id=\"w\">written 1</p>").expect(&html);
    let a = html.find("<p>after</p>").expect(&html);
    assert!(w < a, "{html}");
}

#[tokio::test]
async fn external_deferred_script_sets_inner_html() {
    let s = serve(vec![
        (
            "/",
            Resource::html(
                "<head><script defer src=/d.js></script><script>window.order = ['inline'];</script></head>\
                 <body><div id=t>old</div><script src=/blocking.js></script></body>",
            ),
        ),
        (
            "/d.js",
            Resource::js(
                "order.push('defer:' + document.readyState);\
                 document.getElementById('t').innerHTML = '<b>deferred</b><i>' + order.join() + '</i>';",
            )
            .delay(50),
        ),
        ("/blocking.js", Resource::js("order.push('blocking:' + !!document.getElementById('t'));")),
    ])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
    let html = body(&page).await;
    assert!(
        html.contains(
            "<div id=\"t\"><b>deferred</b><i>inline,blocking:true,defer:interactive</i></div>"
        ),
        "{html}"
    );
    // Subresource headers follow the destination.
    let reqs = s.requests();
    let js = reqs.iter().find(|r| r.1 == "/d.js").unwrap();
    let get = |k: &str| js.2.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    assert_eq!(get("sec-fetch-dest").as_deref(), Some("script"));
    assert_eq!(get("accept").as_deref(), Some("*/*"));
    assert_eq!(get("referer"), Some(s.url("/")));
}

#[tokio::test]
async fn timer_chain_then_mutation() {
    let s = serve(vec![(
        "/",
        Resource::html(
            "<body><ul id=l></ul><script>\
             var n = 0;\
             function step() { var li = document.createElement('li'); li.textContent = n; \
               document.getElementById('l').appendChild(li); if (++n < 5) setTimeout(step, 10); \
               else document.body.setAttribute('data-done', performance.now() > 0); }\
             setTimeout(step, 10);</script></body>",
        ),
    )])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(
        &b,
        &ctx,
        &s.url("/"),
        WaitUntil::Time(Duration::from_millis(300)),
    )
    .await;
    let html = page.content().await.unwrap();
    assert!(
        html.contains("<body data-done=\"true\"><ul id=\"l\"><li>0</li><li>1</li><li>2</li><li>3</li><li>4</li></ul>"),
        "{html}"
    );
}

#[tokio::test]
async fn virtual_time_skips_waiting() {
    let s = serve(vec![(
        "/",
        Resource::html(
            "<body><script>var t0 = performance.now();\
             setTimeout(function () { document.body.setAttribute('data-t', Math.round(performance.now() - t0)); }, 60000);\
             </script></body>",
        ),
    )])
    .await;
    let (b, ctx) = (browser(), context(true));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::NetworkIdle).await;
    let start = std::time::Instant::now();
    loop {
        if let JsValue::String(v) = page
            .evaluate("document.body.getAttribute('data-t') || ''")
            .await
            .unwrap()
            && !v.is_empty()
        {
            assert!(v.parse::<f64>().unwrap() >= 60000.0, "{v}");
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "virtual clock did not advance"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn xhr_and_fetch_update_dom() {
    let s = serve(vec![
        (
            "/",
            Resource::html(
                "<body><div id=x></div><div id=f></div><script>\
                 var x = new XMLHttpRequest(); var states = [];\
                 x.onreadystatechange = function () { states.push(x.readyState); };\
                 x.onload = function () { var j = JSON.parse(x.responseText);\
                   document.getElementById('x').textContent = j.method + ' ' + j.body + ' ' + j.ct + ' ' + x.status + ' ' + x.getResponseHeader('X-Reply') + ' ' + states.join(''); };\
                 x.open('POST', '/echo?q=1'); x.setRequestHeader('X-Test', 'yes'); x.send('ping');\
                 fetch('/data.json', {headers: {'X-Test': 'f'}}).then(function (r) { return r.json().then(function (j) {\
                   document.getElementById('f').textContent = r.status + ' ' + r.ok + ' ' + r.headers.get('content-type') + ' ' + j.items.join('+'); }); });\
                 </script></body>",
            ),
        ),
        ("/data.json", Resource::new("application/json", r#"{"items":[1,2,3]}"#)),
    ])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::NetworkIdle).await;
    let html = body(&page).await;
    assert!(
        html.contains("<div id=\"x\">POST ping text/plain;charset=UTF-8 200 yes 1234</div>"),
        "{html}"
    );
    assert!(
        html.contains("<div id=\"f\">200 true application/json 1+2+3</div>"),
        "{html}"
    );
    let reqs = s.requests();
    let echo = reqs.iter().find(|r| r.1 == "/echo?q=1").unwrap();
    assert!(echo.2.iter().any(|(k, v)| k == "x-test" && v == "yes"));
    let data = reqs.iter().find(|r| r.1 == "/data.json").unwrap();
    assert!(
        data.2
            .iter()
            .any(|(k, v)| k == "sec-fetch-mode" && v == "cors")
    );
}

#[tokio::test]
async fn events_capture_bubble_and_handlers() {
    let s = serve(vec![(
        "/",
        Resource::html(
            "<body><div id=outer><button id=b onclick=\"log.push('attr')\">x</button></div><pre id=out></pre><script>\
             var log = [];\
             var outer = document.getElementById('outer'), b = document.getElementById('b');\
             outer.addEventListener('ping', function (e) { log.push('capture:' + e.eventPhase); }, true);\
             outer.addEventListener('ping', function (e) { log.push('bubble:' + e.eventPhase + ':' + (e.target === b)); });\
             b.addEventListener('ping', function (e) { log.push('target:' + e.detail.n); }, {once: true});\
             document.addEventListener('ping', function (e) { log.push('doc'); e.stopPropagation(); });\
             window.addEventListener('ping', function () { log.push('window'); });\
             var ok = b.dispatchEvent(new CustomEvent('ping', {bubbles: true, detail: {n: 7}}));\
             b.dispatchEvent(new CustomEvent('ping', {bubbles: false, detail: {n: 8}}));\
             b.onclick = function (e) { log.push('prop:' + (this === b)); return false; };\
             var c = new MouseEvent('click', {bubbles: true, cancelable: true});\
             var notCanceled = b.dispatchEvent(c);\
             log.push('canceled:' + !notCanceled + ':' + c.defaultPrevented);\
             document.addEventListener('DOMContentLoaded', function () { log.push('dcl'); });\
             window.onload = function () { log.push('load:' + document.readyState); document.getElementById('out').textContent = log.join(' '); };\
             </script></body>",
        ),
    )])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
    let out = page
        .evaluate("document.getElementById('out').textContent")
        .await
        .unwrap();
    assert_eq!(
        out,
        JsValue::String(
            "capture:1 target:7 bubble:3:true doc capture:1 prop:true canceled:true:true dcl load:complete".into()
        )
    );
}

#[tokio::test]
async fn mutation_observer_records() {
    let s = serve(vec![(
        "/",
        Resource::html(
            "<body><div id=t data-x=1>a</div><script>\
             var out = [];\
             var t = document.getElementById('t');\
             var mo = new MutationObserver(function (recs, obs) {\
               out.push(recs.map(function (r) { return r.type + ':' + (r.attributeName || '') + ':' + (r.oldValue || '') + ':' + r.addedNodes.length + ':' + r.removedNodes.length; }).join(','));\
               out.push(obs === mo);\
               document.body.setAttribute('data-mo', out.join('|'));\
             });\
             mo.observe(t, {childList: true, attributes: true, attributeOldValue: true, subtree: true, characterData: true});\
             t.setAttribute('data-x', '2');\
             t.appendChild(document.createElement('i'));\
             t.firstChild.data = 'b';\
             t.removeChild(t.lastChild);\
             Promise.resolve().then(function () { out.push('promise'); });\
             out.push('sync');\
             </script></body>",
        ),
    )])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
    let v = page
        .evaluate("document.body.getAttribute('data-mo')")
        .await
        .unwrap();
    assert_eq!(
        v,
        JsValue::String(
            "sync|attributes:data-x:1:0:0,childList:::1:0,characterData:::0:0,childList:::0:1|true"
                .into()
        )
    );
    let v = page.evaluate("out.join('|')").await.unwrap();
    assert!(matches!(v, JsValue::String(s) if s.ends_with("|true|promise")));
}

#[tokio::test]
async fn cookies_storage_location_history() {
    let s = serve(vec![(
        "/p",
        Resource::html(
            "<body><script>\
             document.cookie = 'a=1; path=/';\
             localStorage.setItem('k', 'v'); localStorage.n = '2';\
             history.pushState({s: 1}, '', '/p2#frag');\
             document.body.setAttribute('data-r', [document.cookie, localStorage.getItem('n'), Object.keys(localStorage).join('+'),\
               location.pathname, location.hash, history.state.s, history.length, navigator.webdriver, navigator.userAgent.indexOf('Chrome/153') > 0].join(','));\
             </script></body>",
        )
        .header("Set-Cookie", "srv=1; Path=/")
        .header("Set-Cookie", "hidden=1; Path=/; HttpOnly"),
    )])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/p"), WaitUntil::Load).await;
    let v = page
        .evaluate("document.body.getAttribute('data-r')")
        .await
        .unwrap();
    let JsValue::String(v) = v else {
        panic!("{v:?}")
    };
    let parts: Vec<&str> = v.split(',').collect();
    let mut cookies: Vec<&str> = parts[0].split("; ").collect();
    cookies.sort();
    assert_eq!(cookies, ["a=1", "srv=1"]);
    assert_eq!(
        &parts[1..],
        ["2", "k+n", "/p2", "#frag", "1", "3", "false", "true"]
    );
}

#[tokio::test]
async fn script_errors_are_reported_and_parsing_continues() {
    let s = serve(vec![(
        "/",
        Resource::html(
            "<body><script>window.onerror = function (m) { window.seen = m; };\
             null.boom;</script><p id=after>ok</p><script>console.log('still', 1, {a: 2});</script></body>",
        ),
    )])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
    assert_eq!(
        page.evaluate("window.seen").await.unwrap(),
        JsValue::String(
            "Uncaught TypeError: Cannot read properties of null (reading 'boom')".into()
        )
    );
    assert_eq!(
        page.evaluate("document.getElementById('after').textContent")
            .await
            .unwrap(),
        JsValue::String("ok".into())
    );
    let logs = page.console_messages().await.unwrap();
    assert!(
        logs.iter()
            .any(|m| m.level == "log" && m.text == "still 1 {\"a\":2}"),
        "{logs:?}"
    );
}

#[tokio::test]
async fn async_and_dynamic_scripts_delay_load() {
    let s = serve(vec![
        (
            "/",
            Resource::html(
                "<body><script async src=/a.js></script><script>\
                 var s = document.createElement('script'); s.src = '/dyn.js'; s.onload = function () { window.dynLoaded = true; };\
                 document.head.appendChild(s);\
                 var i = document.createElement('script'); i.textContent = 'window.inlineDyn = 1'; document.body.appendChild(i);\
                 window.onload = function () { document.body.setAttribute('data-l', [window.a, window.dyn, window.dynLoaded, window.inlineDyn].join()); };\
                 </script></body>",
            ),
        ),
        ("/a.js", Resource::js("window.a = 'A';").delay(30)),
        ("/dyn.js", Resource::js("window.dyn = 'D';").delay(60)),
    ])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
    assert_eq!(
        page.evaluate("document.body.getAttribute('data-l')")
            .await
            .unwrap(),
        JsValue::String("A,D,true,1".into())
    );
}

#[tokio::test]
async fn module_scripts_with_imports() {
    let s = serve(vec![
        (
            "/",
            Resource::html("<body><script type=module>import {v} from './m.js'; document.body.setAttribute('data-m', v + ':' + (document.currentScript === null));</script></body>"),
        ),
        ("/m.js", Resource::js("import {w} from './dep/n.js'; export const v = 'mod-' + w;")),
        ("/dep/n.js", Resource::js("export const w = 42;")),
    ])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
    assert_eq!(
        page.evaluate("document.body.getAttribute('data-m')")
            .await
            .unwrap(),
        JsValue::String("mod-42:true".into())
    );
}

#[tokio::test]
async fn encoding_from_meta() {
    let mut body = b"<html><head><meta charset=windows-1251></head><body><p>".to_vec();
    body.extend_from_slice(&[0xcf, 0xf0, 0xe8, 0xe2, 0xe5, 0xf2]);
    body.extend_from_slice(b"</p><script>document.body.setAttribute('data-cs', document.characterSet)</script></body></html>");
    let s = serve(vec![("/", Resource::new("text/html", body))]).await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
    let html = body_of(&page).await;
    assert!(html.contains("<p>Привет</p>"), "{html}");
    assert!(html.contains("data-cs=\"windows-1251\""), "{html}");
}

async fn body_of(page: &lb_page::Page) -> String {
    page.content().await.unwrap()
}

#[tokio::test]
async fn script_navigation_replaces_document() {
    let s = serve(vec![
        ("/", Resource::html("<body><script>window.x = 1; location.href = '/next';</script></body>")),
        ("/next", Resource::html("<body><p>next</p><script>document.body.setAttribute('data-x', typeof window.x)</script></body>")),
    ])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = b.new_page(&ctx).await.unwrap();
    page.navigate(&s.url("/")).await.unwrap();
    let start = std::time::Instant::now();
    loop {
        let url = page.url().await.unwrap();
        if url.ends_with("/next") {
            page.wait_for(WaitUntil::Load, Duration::from_secs(5))
                .await
                .unwrap();
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let html = page.content().await.unwrap();
    assert!(
        html.contains("<body data-x=\"undefined\"><p>next</p>"),
        "{html}"
    );
}

#[tokio::test]
async fn unload_events_beacon_and_favicon() {
    let s = serve(vec![
        (
            "/",
            Resource::html(
                "<body><script>var log = [];\
                 addEventListener('pagehide', function (e) { log.push('pagehide:' + e.persisted + ':' + document.visibilityState); });\
                 document.addEventListener('visibilitychange', function () { log.push('visibilitychange:' + document.visibilityState); });\
                 addEventListener('unload', function () { log.push('unload');\
                   log.push(String(navigator.sendBeacon('/beacon', log.join(',')))); });\
                 </script></body>",
            ),
        ),
        (
            "/next",
            Resource::html("<head><link rel=icon href=/i.png></head><body>next</body>"),
        ),
        ("/beacon", Resource::new("text/plain", "")),
        ("/favicon.ico", Resource::new("image/x-icon", "")),
        ("/i.png", Resource::new("image/png", "")),
    ])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
    page.goto(&s.url("/next"), WaitUntil::Load, Duration::from_secs(5))
        .await
        .unwrap();
    let start = std::time::Instant::now();
    let find = |path: &str| s.requests().into_iter().find(|r| r.1 == path);
    while find("/beacon").is_none() || find("/i.png").is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            s.requests()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let beacon = find("/beacon").unwrap();
    assert_eq!(beacon.0, "POST");
    assert_eq!(
        String::from_utf8_lossy(&beacon.3),
        "pagehide:false:visible,visibilitychange:hidden,unload"
    );
    let has = |r: &(String, String, Vec<(String, String)>, Vec<u8>), k: &str, v: &str| {
        r.2.iter().any(|(a, b)| a == k && b == v)
    };
    assert!(
        has(&beacon, "content-type", "text/plain;charset=UTF-8"),
        "{beacon:?}"
    );
    assert!(has(&beacon, "sec-fetch-mode", "no-cors"), "{beacon:?}");
    let icon = find("/favicon.ico").expect("favicon of the first page");
    assert!(has(&icon, "sec-fetch-dest", "image"), "{icon:?}");
    assert!(has(&icon, "referer", &s.url("/")), "{icon:?}");
    let link = find("/i.png").unwrap();
    assert!(has(&link, "referer", &s.url("/next")), "{link:?}");
}
