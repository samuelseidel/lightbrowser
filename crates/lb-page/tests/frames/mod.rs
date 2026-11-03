//! Nested browsing contexts (`<iframe>`) on a loopback server. Included
//! from `pages.rs` (one test binary for both).

use lb_js::JsValue;
use lb_page::WaitUntil;

use super::common::{Resource, browser, context, open, serve};

mod cross_realm;

async fn eval(page: &lb_page::Page, src: &str) -> JsValue {
    page.evaluate(src).await.unwrap()
}

fn s(v: JsValue) -> String {
    match v {
        JsValue::String(s) => s,
        v => panic!("{v:?}"),
    }
}

#[tokio::test]
async fn dynamic_about_blank_iframe_is_synchronous() {
    let srv = serve(vec![(
        "/",
        Resource::html(
            "<body><script>\
             window.log = []; try {\
             const f = document.createElement('iframe');\
             log.push('before:' + f.contentWindow);\
             f.onload = () => log.push('load');\
             document.body.appendChild(f);\
             log.push('after:' + (f.contentWindow !== null));\
             const w = f.contentWindow;\
             log.push('doc:' + w.document.readyState + ':' + w.document.body.outerHTML);\
             log.push('self:' + (w.self === w) + ',' + (w !== window) + ',' + (w.parent === window) + ',' + (w.top === window));\
             log.push('frameElement:' + (w.frameElement === f) + ',' + (window.frameElement === null));\
             log.push('frames:' + window.length + ',' + (window[0] === w) + ',' + (window.frames[0] === w) + ',' + Object.keys(window).includes('0'));\
             log.push('nav:' + (w.navigator.userAgent === navigator.userAgent) + ',' + (w.navigator.plugins.length === navigator.plugins.length) + ',' + (w.screen.width === screen.width) + ',' + (!!w.chrome === !!window.chrome));\
             const clean = new Set(Object.getOwnPropertyNames(w));\
             log.push('extra:' + Object.getOwnPropertyNames(window).filter(k => !clean.has(k) && !/^\\d+$/.test(k) && k !== 'log').join('|'));\
             log.push('toString:' + w.Function.prototype.toString.call(document.createElement).includes('[native code]'));\
             log.push('doc2:' + (f.contentDocument === w.document) + ',' + (w.document.defaultView === w));\
             } catch (e) { log.push('ERR ' + e.stack); }</script></body>",
        ),
    )])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &srv.url("/"), WaitUntil::Load).await;
    let log = s(eval(&page, "log.join('\\n')").await);
    let want = [
        "before:null",
        "load",
        "after:true",
        "doc:complete:<body></body>",
        "self:true,true,true,true",
        "frameElement:true,true",
        "frames:1,true,true,true",
        "nav:true,true,true,true",
        "extra:",
        "toString:true",
        "doc2:true,true",
    ];
    assert_eq!(log, want.join("\n"));
}

#[tokio::test]
async fn srcdoc_and_same_origin_src_load_and_message() {
    let srv = serve(vec![
        (
            "/",
            Resource::html(
                "<body><script>window.log = []; addEventListener('message', e => log.push('msg:' + JSON.stringify(e.data) + ':' + e.origin + ':' + (e.source === frames[1])));\
                 addEventListener('load', () => log.push('top-load:' + frames.length));</script>\
                 <iframe id=a srcdoc='<p id=x>hi</p><script>parent.log.push(\"a-script:\" + document.getElementById(\"x\").textContent + \":\" + location.href)</script>' onload='log.push(\"a-load\")'></iframe>\
                 <iframe id=b src=/child onload='log.push(\"b-load:\" + this.contentDocument.title)'></iframe></body>",
            ),
        ),
        (
            "/child",
            Resource::html(
                "<title>child</title><script>parent.log.push('b-script:' + location.pathname + ':' + (frameElement.id));\
                 parent.postMessage({n: 1, s: 'x'}, '*');</script>",
            )
            .delay(30),
        ),
    ])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &srv.url("/"), WaitUntil::Load).await;
    // The message task runs after load.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let log = s(eval(&page, "log.join('\\n')").await);
    assert!(log.contains("a-script:hi:about:srcdoc"), "{log}");
    assert!(log.contains("a-load"), "{log}");
    assert!(log.contains("b-script:/child:b"), "{log}");
    assert!(log.contains("b-load:child"), "{log}");
    assert!(
        log.contains("msg:{\"n\":1,\"s\":\"x\"}:http://127.0.0.1"),
        "{log}"
    );
    assert!(log.contains(":true"), "{log}");
    // The top-level load waits for both frames.
    let top = log.find("top-load:2").expect(&log);
    assert!(log.find("b-load").unwrap() < top, "{log}");
    assert!(log.find("a-load").unwrap() < top, "{log}");
}

#[tokio::test]
async fn removal_detaches_and_cross_origin_is_blocked() {
    let other = serve(vec![("/x", Resource::html("<p>other</p>"))]).await;
    let other_url = other.url("/x");
    let srv = serve(vec![(
        "/",
        Resource::html(&format!(
            "<body><iframe id=c src='{other_url}'></iframe><script>window.log = [];\
             const f = document.createElement('iframe'); document.body.appendChild(f);\
             const w = f.contentWindow; f.remove();\
             log.push('removed:' + f.contentWindow + ',' + window.length);</script></body>"
        )),
    )])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &srv.url("/"), WaitUntil::Load).await;
    let log = s(eval(&page, "log.join('\\n')").await);
    assert_eq!(log, "removed:null,1");
    let r = s(eval(
        &page,
        "(() => { const c = document.getElementById('c'); const w = c.contentWindow; \
         let e1; try { w.document; } catch (e) { e1 = e.name + ': ' + e.message; } \
         return [String(c.contentDocument), typeof w.postMessage, w === frames[0], e1].join('|'); })()",
    )
    .await);
    assert!(r.starts_with("null|function|true|SecurityError: "), "{r}");
    assert!(r.contains("Blocked a frame with origin"), "{r}");
}
