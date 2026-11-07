//! Differences from Chrome that public bot-detection pages showed (the
//! community test pages: sannysoft, rebrowser, CreepJS, browserscan,
//! incolumitas), each reproduced on a loopback server. Expected values
//! were recorded in local Chrome. Included from `pages.rs`.

use lb_js::JsValue;
use lb_page::WaitUntil;

use super::common::{Resource, browser, context, open, serve};

async fn eval_str(page: &lb_page::Page, src: &str) -> String {
    match page.evaluate(src).await.unwrap() {
        JsValue::String(s) => s,
        v => panic!("{v:?}"),
    }
}

/// An iframe removed while its navigation is pending (fpCollect's
/// `iframeChrome` test) must not hold the parent's `load` (sannysoft and
/// incolumitas never fired `load`).
#[tokio::test]
async fn removed_pending_iframe_does_not_block_load() {
    let srv = serve(vec![
        (
            "/",
            Resource::html(
                "<body><script>\
                 for (const src of [null, '/child']) {\
                   const f = document.createElement('iframe');\
                   if (src) f.src = src; else f.srcdoc = 'blank page';\
                   document.body.appendChild(f);\
                   window.t = typeof f.contentWindow.chrome;\
                   f.remove();\
                 }\
                 window.fired = 0; onload = () => fired++;</script></body>",
            ),
        ),
        ("/child", Resource::html("<p>child</p>")),
    ])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &srv.url("/"), WaitUntil::Load).await;
    assert_eq!(
        eval_str(&page, "document.readyState + ':' + fired + ':' + t").await,
        "complete:1:object"
    );
}

/// `DOMTokenList.supports` for `relList` and `sandbox` (Vite's
/// modulepreload check; browserscan did not render).
#[tokio::test]
async fn token_list_supports_matches_chrome() {
    let html = include_str!("rel_list.html");
    let srv = serve(vec![("/", Resource::html(html))]).await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &srv.url("/"), WaitUntil::Load).await;
    let got = eval_str(&page, "document.getElementById('o').textContent").await;
    let want = include_str!("rel_list.chrome.txt");
    assert_eq!(got.trim(), want.trim());
}

/// Blink's `NavigationRateLimiter`: history and location navigations past
/// 200 in 10 s are dropped without an exception, with one warning
/// (rebrowser's `ipcFloodingProtection`).
#[tokio::test]
async fn history_navigations_are_rate_limited() {
    let srv = serve(vec![(
        "/",
        Resource::html(
            "<body><script>\
             let ok = 0, errs = 0; const len0 = history.length;\
             for (let i = 0; i < 300; i++) { try { history.replaceState(null, '', '?r' + i); ok++; } catch (e) { errs++; } }\
             for (let i = 0; i < 5; i++) history.pushState(null, '', '?p' + i);\
             location.hash = 'h';\
             window.res = [ok, errs, location.search, location.hash, history.length - len0].join();\
             </script></body>",
        ),
    )])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &srv.url("/"), WaitUntil::Load).await;
    // As in Chrome 156: every call returns; the 201st and later do nothing.
    assert_eq!(eval_str(&page, "res").await, "300,0,?r199,,0");
    let warnings = page
        .console_messages()
        .await
        .unwrap()
        .into_iter()
        .filter(|m| m.text.starts_with("Throttling navigation"))
        .count();
    assert_eq!(warnings, 1);
}

/// `local()` faces of a default macOS 26 install load for a macOS 26
/// persona, as in Chrome (CreepJS read "macOS Mavericks" from 2 of 51).
#[tokio::test]
async fn macos26_local_faces_load() {
    let srv = serve(vec![(
        "/",
        Resource::html(
            "<body><script>\
             window.res = null;\
             const names = ['Geneva', 'Kohinoor Devanagari Medium', 'PingFang HK Light', 'Futura Bold',\
               'InaiMathi Bold', 'Galvji', 'STIX Two Math Regular', 'Segoe UI', 'Calibri', 'DejaVu Sans'];\
             Promise.all(names.map(n => new FontFace('f', `local(\"${n}\")`).load().then(() => n, () => null)))\
               .then(v => { res = v.filter(Boolean).join(); });\
             </script></body>",
        ),
    )])
    .await;
    let profiles = concat!(env!("CARGO_MANIFEST_DIR"), "/../../profiles");
    let p = std::sync::Arc::new(
        lb_persona::PersonaProfile::from_path_checked(format!(
            "{profiles}/reference/chrome/chrome153_macos26_apple_m4_1512x982.json"
        ))
        .unwrap(),
    );
    let tls = format!("{profiles}/tls");
    let ctx = lb_page::BrowserContext::new(
        lb_page::context_options_for(&p, std::path::Path::new(&tls), false, true).unwrap(),
    )
    .unwrap();
    let b = browser();
    let page = open(&b, &ctx, &srv.url("/"), WaitUntil::Load).await;
    let mut res = String::new();
    for _ in 0..50 {
        if let JsValue::String(s) = page.evaluate("res").await.unwrap() {
            res = s;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        res,
        "Geneva,Kohinoor Devanagari Medium,PingFang HK Light,Futura Bold,InaiMathi Bold,Galvji,STIX Two Math Regular"
    );
}

/// `import()` resolves against the calling script's own URL, for external
/// modules and classic scripts, at any time after they ran (Nuxt's lazy
/// pages on browserscan loaded from `/` instead of `/dist/`). Lines as
/// printed by Chrome 153 headless shell for the same files.
#[tokio::test]
async fn dynamic_import_resolves_against_the_script() {
    let log = "<pre id=o></pre><script>window.log = (s) => { document.getElementById('o').textContent += s + '\\n'; };\
               import('./dist/b.js').then((m) => log('inline ' + m.u));</script>";
    let srv = serve(vec![
        (
            "/",
            Resource::html(&format!(
                "{log}<script src=/dist/classic.js></script>\
                 <script type=module src=/dist/entry.js></script>"
            )),
        ),
        (
            "/dist/classic.js",
            Resource::js(
                "import('./b.js').then((m) => log('classic ' + m.u));\
                 setTimeout(() => import('./c.js').then((m) => log('classic timer ' + m.u)), 10);",
            ),
        ),
        (
            "/dist/entry.js",
            Resource::js(
                "const lazy = () => import('./b.js');\
                 lazy().then((m) => log('direct ' + m.u));\
                 setTimeout(() => lazy().then((m) => log('timer ' + m.u)), 20);\
                 import('./d.js').then((m) => m.go()).then((u) => log('nested ' + u));",
            ),
        ),
        ("/dist/b.js", Resource::js("export const u = 'dist/b';")),
        ("/dist/c.js", Resource::js("export const u = 'dist/c';")),
        (
            "/dist/d.js",
            Resource::js("export const go = () => import('./b.js').then((m) => m.u);"),
        ),
        ("/b.js", Resource::js("export const u = 'root/b';")),
        ("/c.js", Resource::js("export const u = 'root/c';")),
    ])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &srv.url("/"), WaitUntil::Load).await;
    let mut got = String::new();
    for _ in 0..100 {
        got = eval_str(&page, "document.getElementById('o').textContent").await;
        if got.lines().count() >= 6 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let mut lines: Vec<&str> = got.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        [
            "classic dist/b",
            "classic timer dist/c",
            "direct dist/b",
            "inline dist/b",
            "nested dist/b",
            "timer dist/b"
        ]
    );
}

/// The legacy `navigator.getUserMedia` / `webkitGetUserMedia` check their
/// arguments as Chrome does and return `undefined` (they threw
/// `NotSupportedError` on sannysoft). `gum.chrome.txt` is Chrome 153's output.
#[tokio::test]
async fn legacy_get_user_media_matches_chrome() {
    let html = format!(
        "<pre id=o></pre><script>{}</script>",
        include_str!("gum.js")
    );
    let srv = serve(vec![("/", Resource::html(&html))]).await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &srv.url("/"), WaitUntil::Load).await;
    let got = eval_str(&page, "document.getElementById('o').textContent").await;
    assert_eq!(got.trim(), include_str!("gum.chrome.txt").trim());
}
