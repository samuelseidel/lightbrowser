//! Form submission against a loopback server: `method="post"` sends a POST
//! navigation with the encoded body and Chrome's form-POST headers
//! (`profiles/headers/captures/chrome_153_linux_forms.jsonl`), GET forms
//! keep using the query.
#![cfg(feature = "chrome")]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Resource, Server, browser, context, open, serve};
use lb_cdp::types::MouseEvent;
use lb_cdp::{ContextOptions, EvaluateRequest, NavigateRequest, PageHost};
use lb_page::{CdpHost, CdpHostOptions, Page, WaitUntil};
use lb_persona::PersonaProfile;

type Logged = (String, String, Vec<(String, String)>, Vec<u8>);

const LOGIN: &str = "<!DOCTYPE html><body>\
    <form id=f method=post action='/session?next=%2Fhome'>\
    <input name=email value='a b@x.test'>\
    <input type=password name=password value='p&s=w%\u{f6}'>\
    <input type=checkbox name=remember checked>\
    <input type=checkbox name=off>\
    <input name=gone value=x disabled>\
    <textarea name=note>l1\nl2</textarea>\
    <button id=go name=btn value=1>Sign in</button>\
    </form></body>";

fn redirect(to: &str) -> Resource {
    let mut r = Resource::html("").header("Location", to);
    r.status = 303;
    r
}

/// Wait until the page's URL ends with `suffix`.
async fn wait_url(page: &Page, suffix: &str) -> String {
    let start = Instant::now();
    loop {
        let u = page.url().await.unwrap();
        if u.ends_with(suffix) {
            return u;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "stuck at {u}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn find<'a>(log: &'a [Logged], method: &str, path: &str) -> &'a Logged {
    log.iter()
        .find(|(m, t, _, _)| m == method && t.split('?').next() == Some(path))
        .unwrap_or_else(|| panic!("no {method} {path} in {:?}", targets(log)))
}

fn targets(log: &[Logged]) -> Vec<String> {
    log.iter().map(|(m, t, _, _)| format!("{m} {t}")).collect()
}

fn header<'a>(r: &'a Logged, name: &str) -> Option<&'a str> {
    r.2.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

fn names(r: &Logged) -> Vec<&str> {
    r.2.iter().map(|(n, _)| n.as_str()).collect()
}

fn server_routes() -> Vec<(&'static str, Resource)> {
    vec![
        (
            "/login",
            Resource::html(LOGIN).header("Set-Cookie", "sid=abc; Path=/"),
        ),
        ("/session", redirect("/account")),
        ("/account", Resource::html("<p id=ok>signed in</p>")),
        ("/echo-page", Resource::html("<p>posted</p>")),
    ]
}

#[tokio::test]
async fn post_form_sends_urlencoded_body_and_chrome_headers() {
    let srv = serve(server_routes()).await;
    let b = browser();
    let ctx = context(false);
    let page = open(&b, &ctx, &srv.url("/login"), WaitUntil::Load).await;
    page.evaluate("document.getElementById('f').submit()")
        .await
        .unwrap();
    wait_url(&page, "/account").await;
    page.wait_for(WaitUntil::Load, Duration::from_secs(10))
        .await
        .unwrap();
    assert!(common::body(&page).await.contains("signed in"));

    let log = srv.requests();
    // Credentials never end up in a URL.
    assert!(
        log.iter().all(|(_, t, _, _)| !t.contains("password")),
        "{:?}",
        targets(&log)
    );
    let post = find(&log, "POST", "/session");
    // The action URL as is (its own query stays).
    assert_eq!(post.1, "/session?next=%2Fhome");
    assert_eq!(
        String::from_utf8_lossy(&post.3),
        "email=a+b%40x.test&password=p%26s%3Dw%25%C3%B6&remember=on&note=l1%0D%0Al2"
    );
    // Chrome 153 over HTTP/1.1, script submit (no Sec-Fetch-User).
    assert_eq!(
        names(post),
        [
            "host",
            "connection",
            "content-length",
            "cache-control",
            "sec-ch-ua",
            "sec-ch-ua-mobile",
            "sec-ch-ua-platform",
            "accept-language",
            "upgrade-insecure-requests",
            "content-type",
            "user-agent",
            "origin",
            "accept",
            "sec-fetch-site",
            "sec-fetch-mode",
            "sec-fetch-dest",
            "referer",
            "accept-encoding",
            "cookie",
        ]
    );
    let origin = format!("http://127.0.0.1:{}", srv.port);
    assert_eq!(header(post, "origin"), Some(origin.as_str()));
    assert_eq!(header(post, "cache-control"), Some("max-age=0"));
    assert_eq!(
        header(post, "content-type"),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(header(post, "content-length"), Some("74"));
    assert_eq!(header(post, "sec-fetch-site"), Some("same-origin"));
    assert_eq!(header(post, "sec-fetch-mode"), Some("navigate"));
    assert_eq!(header(post, "sec-fetch-dest"), Some("document"));
    assert_eq!(header(post, "referer"), Some(srv.url("/login").as_str()));
    assert_eq!(header(post, "cookie"), Some("sid=abc"));

    // The 303 hop: a GET without body headers or Origin, still with
    // Cache-Control, in the redirect order.
    let get = find(&log, "GET", "/account");
    assert!(get.3.is_empty());
    assert_eq!(
        names(get),
        [
            "host",
            "connection",
            "cache-control",
            "accept-language",
            "upgrade-insecure-requests",
            "user-agent",
            "accept",
            "sec-fetch-site",
            "sec-fetch-mode",
            "sec-fetch-dest",
            "sec-ch-ua",
            "sec-ch-ua-mobile",
            "sec-ch-ua-platform",
            "referer",
            "accept-encoding",
            "cookie",
        ]
    );
}

/// Submit the login form with `enctype` through `button.click()` (an
/// untrusted click still names the submitter); return the POST.
async fn submit_with(enctype: &str) -> (Server, Logged) {
    let srv = serve(server_routes()).await;
    let b = browser();
    let ctx = context(false);
    let page = open(&b, &ctx, &srv.url("/login"), WaitUntil::Load).await;
    page.evaluate(&format!(
        "const f = document.getElementById('f'); f.setAttribute('enctype', '{enctype}'); \
         f.setAttribute('action', '/echo-page'); document.getElementById('go').click()"
    ))
    .await
    .unwrap();
    wait_url(&page, "/echo-page").await;
    let post = find(&srv.requests(), "POST", "/echo-page").clone();
    (srv, post)
}

#[tokio::test]
async fn post_form_multipart_and_text_plain() {
    let (_srv, post) = submit_with("multipart/form-data").await;
    let ct = header(&post, "content-type").unwrap();
    let boundary = ct
        .strip_prefix("multipart/form-data; boundary=")
        .unwrap()
        .to_owned();
    assert!(boundary.starts_with("----WebKitFormBoundary"), "{boundary}");
    assert_eq!(boundary.len(), "----WebKitFormBoundary".len() + 16);
    let part = |n: &str, v: &str| {
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{n}\"\r\n\r\n{v}\r\n")
    };
    let want = [
        part("email", "a b@x.test"),
        part("password", "p&s=w%\u{f6}"),
        part("remember", "on"),
        part("note", "l1\r\nl2"),
        part("btn", "1"),
        format!("--{boundary}--\r\n"),
    ]
    .concat();
    assert_eq!(String::from_utf8(post.3.clone()).unwrap(), want);
    assert_eq!(
        header(&post, "content-length"),
        Some(want.len().to_string().as_str())
    );

    let (_srv, post) = submit_with("text/plain").await;
    assert_eq!(header(&post, "content-type"), Some("text/plain"));
    assert_eq!(
        String::from_utf8(post.3.clone()).unwrap(),
        "email=a b@x.test\r\npassword=p&s=w%\u{f6}\r\nremember=on\r\nnote=l1\r\nl2\r\nbtn=1\r\n"
    );
}

#[tokio::test]
async fn get_form_still_uses_the_query() {
    let srv = serve(vec![
        (
            "/search",
            Resource::html(
                "<form id=f action='/results#top'><input name=q value='a b'>\
                 <select name=s><option>x<option selected value=y>Y</select></form>",
            ),
        ),
        ("/results", Resource::html("<p>results</p>")),
    ])
    .await;
    let b = browser();
    let ctx = context(false);
    let page = open(&b, &ctx, &srv.url("/search"), WaitUntil::Load).await;
    page.evaluate("document.getElementById('f').submit()")
        .await
        .unwrap();
    let url = wait_url(&page, "#top").await;
    assert!(url.ends_with("/results?q=a+b&s=y#top"), "{url}");
    let get = find(&srv.requests(), "GET", "/results").clone();
    assert!(get.3.is_empty());
    assert_eq!(header(&get, "content-type"), None);
    assert_eq!(header(&get, "origin"), None);
    assert_eq!(header(&get, "cache-control"), None);
}

#[tokio::test]
async fn post_form_in_iframe() {
    let srv = serve(vec![
        (
            "/outer",
            Resource::html("<iframe id=fr src='/login' width=600 height=300></iframe>"),
        ),
        (
            "/login",
            Resource::html(&LOGIN.replace(
                "</form>",
                "</form><script>const f = document.getElementById('f'); \
                 f.setAttribute('action', '/echo-page'); setTimeout(() => f.submit(), 0)</script>",
            )),
        ),
        ("/echo-page", Resource::html("<p>posted</p>")),
    ])
    .await;
    let b = browser();
    let ctx = context(false);
    let _page = open(&b, &ctx, &srv.url("/outer"), WaitUntil::Load).await;
    let start = Instant::now();
    let post = loop {
        let log = srv.requests();
        if let Some(r) = log.iter().find(|(m, _, _, _)| m == "POST") {
            break r.clone();
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{:?}",
            targets(&log)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(post.1, "/echo-page");
    assert_eq!(header(&post, "sec-fetch-dest"), Some("iframe"));
    assert_eq!(header(&post, "cache-control"), Some("max-age=0"));
    assert!(header(&post, "origin").is_some());
    assert!(String::from_utf8_lossy(&post.3).starts_with("email=a+b%40x.test&"));
}

/// A trusted click on the submit button (CDP `Input.dispatchMouseEvent`):
/// the POST carries `Sec-Fetch-User: ?1` and the submitter's entry.
#[tokio::test]
async fn trusted_click_posts_with_sec_fetch_user() {
    let srv = serve(server_routes()).await;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    let p = PersonaProfile::from_path_checked(
        root.join("reference/chrome/chrome153_win10_rtx3060_1080p.json"),
    )
    .unwrap();
    let mut opts = CdpHostOptions::new(Arc::new(p), root.join("tls"));
    opts.workers = 1;
    let h = CdpHost::new(opts).unwrap();
    h.create_context("C", ContextOptions::default()).unwrap();
    h.create_page("C", "T").await.unwrap();
    h.navigate(
        "T",
        NavigateRequest {
            url: srv.url("/login"),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let ctx_id = h.main_context("T").unwrap();
    let r = h
        .evaluate(
            "T",
            EvaluateRequest {
                context_id: ctx_id,
                expression:
                    "(() => { const r = document.getElementById('go').getBoundingClientRect(); \
                             return [r.x + r.width / 2, r.y + r.height / 2]; })()"
                        .into(),
                return_by_value: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let v = r.result.value.unwrap();
    let (x, y) = (v[0].as_f64().unwrap(), v[1].as_f64().unwrap());
    for kind in ["mouseMoved", "mousePressed", "mouseReleased"] {
        h.dispatch_mouse_event(
            "T",
            MouseEvent {
                kind: kind.into(),
                x,
                y,
                button: Some("left".into()),
                click_count: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    let start = Instant::now();
    let (post, get) = loop {
        let log = srv.requests();
        let post = log.iter().find(|(m, _, _, _)| m == "POST").cloned();
        let get = log
            .iter()
            .find(|(m, t, _, _)| m == "GET" && t == "/account")
            .cloned();
        if let (Some(p), Some(g)) = (post, get) {
            break (p, g);
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{:?}",
            targets(&log)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(header(&post, "sec-fetch-user"), Some("?1"));
    assert_eq!(header(&get, "sec-fetch-user"), Some("?1"));
    assert!(
        String::from_utf8_lossy(&post.3).ends_with("&note=l1%0D%0Al2&btn=1"),
        "{}",
        String::from_utf8_lossy(&post.3)
    );
    let n = names(&post);
    let at = |h: &str| n.iter().position(|x| *x == h).unwrap();
    assert_eq!(at("sec-fetch-user"), at("sec-fetch-mode") + 1);
}
