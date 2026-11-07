//! Client hints after `Accept-CH` / `Critical-CH` (M7, WP-E).
//!
//! - `golden_capture`: every request of
//!   `profiles/headers/captures/chrome_153_linux_accept_ch.jsonl` (Chrome for
//!   Testing 153, loopback, HTTP/1.1 and h2) is rebuilt with
//!   `HeaderProfile::build` from the same facts (kind, initiator, page
//!   headers, cookie, hints) and must match names, case, order and values.
//! - `pipeline_*`: `lb_net::fetch` against a local HTTP/1.1 server on
//!   127.0.0.1: the `Accept-CH` store, the `Critical-CH` restart, redirects,
//!   same-origin delegation, and no hints for Firefox.
//!
//! Loopback only.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http::Method;
use lb_net::fetch::NetContext;
use lb_net::header_profile::{BuildInput, HeaderProfile, RequestKind, RequestMeta, Site};
use lb_net::{ClientHelloProfile, HttpVersion, Request, UaHint};
use lb_persona::{BrandVersion, BrowserFamily, HighEntropyHints, Persona};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

const CAPTURE: &str =
    include_str!("../../../profiles/headers/captures/chrome_153_linux_accept_ch.jsonl");

/// The persona of the capture: Chrome for Testing 153 on Linux.
fn cft_linux() -> Persona {
    Persona {
        family: BrowserFamily::Chrome,
        user_agent: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
HeadlessChrome/153.0.0.0 Safari/537.36"
            .into(),
        accept_language: "en-US,en;q=0.9".into(),
        sec_ch_ua: Some(r#""Chromium";v="153", "Not_A Brand";v="8""#.into()),
        sec_ch_ua_platform: Some("Linux".into()),
        sec_ch_ua_mobile: Some("?0".into()),
        tls_profile_id: "chrome_153_linux".into(),
        high_entropy: Some(HighEntropyHints {
            platform: "Linux".into(),
            platform_version: String::new(),
            architecture: "x86".into(),
            bitness: "64".into(),
            model: String::new(),
            wow64: false,
            full_version_list: vec![
                BrandVersion::new("Chromium", "153.0.8010.12"),
                BrandVersion::new("Not_A Brand", "8.0.0.0"),
            ],
            full_version: "153.0.8010.12".into(),
            form_factors: vec!["Desktop".into()],
        }),
        tcp_profile_id: "linux_6".into(),
    }
}

/// Header names the capture page set itself, per request path
/// (`tools/header_capture/accept_ch.js`).
fn page_headers(path: &str) -> &'static [&'static str] {
    match path {
        "/api?combo1" | "/api?c-x-a" => &["X-A"],
        "/api?combo2" => &["X-B", "X-C"],
        "/api?combo3" => &["Accept"],
        "/api?combo4" => &["Pragma", "Cache-Control"],
        "/api?combo5" => &["X-Custom-Test", "x-lower-test"],
        "/api?combo6" => &["X-D", "X-E", "X-F", "X-G", "X-H"],
        "/api?fetch-post" | "/api?xhr-post" | "/api?beacon" => &["Content-Type"],
        "/api?fetch-auth" => &["Authorization", "Content-Type"],
        "/api?fetch-pre" => &["X-Pre", "Content-Type"],
        _ => &[],
    }
}

fn kind_of(path: &str) -> RequestKind {
    let p = path.split('?').next().unwrap();
    match p {
        "/frame.html" => RequestKind::Iframe,
        "/m.js" => RequestKind::ModuleScript,
        "/favicon.ico" | "/bg.png" => RequestKind::Favicon,
        "/api" => RequestKind::Fetch,
        _ if p.ends_with(".css") => RequestKind::Style,
        _ if p.ends_with(".js") => RequestKind::Script,
        _ if p.ends_with(".woff2") => RequestKind::Font,
        _ if p.ends_with(".png") => RequestKind::Image,
        _ => RequestKind::Document,
    }
}

#[test]
fn golden_capture() {
    let profile = HeaderProfile::builtin(BrowserFamily::Chrome).unwrap();
    let persona = cft_linux();
    let mut checked = 0;
    let mut with_hints = 0;
    let mut restarts = 0;
    let mut skipped = 0;
    let mut seen_first: std::collections::HashSet<String> = Default::default();
    for line in CAPTURE.lines() {
        let rec: Value = serde_json::from_str(line).unwrap();
        if rec.get("_meta").is_some() {
            continue;
        }
        let h2 = rec["proto"] == "h2";
        let request = rec["request"].as_str().unwrap();
        let mut parts = request.split(' ');
        let method: Method = parts.next().unwrap().parse().unwrap();
        let path = parts.next().unwrap();
        let headers: Vec<(String, String)> = rec["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    p[0].as_str().unwrap().to_owned(),
                    p[1].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let get = |n: &str| {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(n))
                .map(|(_, v)| v.clone())
        };
        // Cross-site frames of the https page (B from H) also carry
        // `Sec-Fetch-Storage-Access`, which LightBrowser does not send yet
        // (open, docs/ROADMAP.md). They carry no client hints.
        if get("sec-fetch-storage-access").is_some() {
            assert!(
                headers
                    .iter()
                    .all(|(n, _)| UaHint::from_header(n).is_none())
            );
            skipped += 1;
            continue;
        }
        let authority = get(if h2 { ":authority" } else { "host" }).unwrap();
        let scheme = if h2 { "https" } else { "http" };
        let url = Url::parse(&format!("{scheme}://{authority}{path}")).unwrap();
        // The referrer stands in for the initiator: same-origin requests send
        // the full URL, cross-origin ones the origin, and both give the same
        // Referer, Origin and Sec-Fetch-Site back.
        // https to http requests send no Referer; their Origin names the page.
        let initiator = get("referer")
            .or_else(|| get("origin"))
            .map(|r| Url::parse(&r).unwrap());
        let kind = kind_of(path);
        let hints: Vec<UaHint> = headers
            .iter()
            .filter_map(|(n, _)| UaHint::from_header(n))
            .collect();
        let mut sorted = hints.clone();
        sorted.sort();

        let mut meta = RequestMeta::new(kind, initiator.clone());
        if let Some(m) = get("sec-fetch-mode") {
            meta.mode = m;
        }
        // sendBeacon sets its own priority. CSS background images have
        // `priority: i` on h2, not the favicon's `u=1, i`; LightBrowser does
        // not fetch them yet, so the profile has no kind for them.
        if path == "/api?beacon" || path == "/bg.png" {
            meta.priority = get("priority");
        }
        // A navigation seen twice in a row is the Critical-CH restart.
        let key = format!("{}{}", rec["run"], url);
        let restart = kind == RequestKind::Document && !seen_first.insert(key) && path == "/start";
        let hop = usize::from(matches!(path, "/final" | "/c-final"));
        // For the two redirect targets the chain's largest distance equals
        // the initiator-to-target one.
        let site = Site::between(initiator.as_ref(), &url);
        let caller: Vec<(String, String)> = page_headers(path)
            .iter()
            .map(|n| ((*n).to_owned(), get(n).expect("page header captured")))
            .collect();
        let cookie = get("cookie");
        let body_len = get("content-length").map_or(0, |v| v.parse().unwrap());
        let built = profile.build(&BuildInput {
            persona: &persona,
            version: if h2 {
                HttpVersion::Http2
            } else {
                HttpVersion::Http1
            },
            meta: &meta,
            url: &url,
            method: &method,
            body_len,
            cookie: cookie.as_deref(),
            caller: &caller,
            hop,
            site,
            hints: &sorted,
            restart,
        });
        let want: Vec<(String, String)> = headers
            .into_iter()
            .filter(|(n, _)| !n.starts_with(':'))
            .collect();
        assert_eq!(
            built, want,
            "{} {request} ({kind:?}, hints {hints:?}, restart {restart})",
            rec["origin"]
        );
        checked += 1;
        with_hints += usize::from(!hints.is_empty());
        restarts += usize::from(restart);
    }
    println!(
        "golden: {checked} requests, {with_hints} with hints, {restarts} restarts, {skipped} skipped"
    );
    assert!(checked >= 80 && with_hints >= 50 && restarts == 2 && skipped <= 4);
}

// ---- pipeline over a local server -------------------------------------

#[derive(Debug, Clone)]
struct Seen {
    path: String,
    /// Header names and values, wire case, in order.
    headers: Vec<(String, String)>,
}

impl Seen {
    fn names(&self) -> Vec<String> {
        self.headers
            .iter()
            .map(|(n, _)| n.to_ascii_lowercase())
            .collect()
    }
    fn hints(&self) -> Vec<String> {
        self.names()
            .into_iter()
            .filter(|n| UaHint::from_header(n).is_some())
            .collect()
    }
    fn get(&self, n: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(n))
            .map(|(_, v)| v.as_str())
    }
}

const ALL7: &str = "Sec-CH-UA-Platform-Version, Sec-CH-UA-Arch, Sec-CH-UA-Bitness, \
Sec-CH-UA-Full-Version-List, Sec-CH-UA-Model, Sec-CH-UA-WoW64, Sec-CH-UA-Form-Factors";

/// A tiny HTTP/1.1 server on 127.0.0.1 that logs every request and answers
/// per path (one request per connection).
async fn server() -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let log: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let log2 = Arc::clone(&log);
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let log = Arc::clone(&log2);
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = sock.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let mut lines = head.split("\r\n");
                let path = lines.next().unwrap().split(' ').nth(1).unwrap().to_owned();
                let headers = lines
                    .take_while(|l| !l.is_empty())
                    .filter_map(|l| l.split_once(": "))
                    .map(|(a, b)| (a.to_owned(), b.to_owned()))
                    .collect();
                log.lock().unwrap().push(Seen {
                    path: path.clone(),
                    headers,
                });
                let (status, extra) = match path.as_str() {
                    "/start" => (
                        "200 OK",
                        format!("Accept-CH: {ALL7}\r\nCritical-CH: Sec-CH-UA-Platform-Version\r\nSet-Cookie: sid=1; Path=/\r\n"),
                    ),
                    "/crit-again" => (
                        "200 OK",
                        "Accept-CH: Sec-CH-UA-Model\r\nCritical-CH: Sec-CH-UA-Model, Sec-CH-UA-Arch\r\n".to_owned(),
                    ),
                    "/redir" => (
                        "302 Found",
                        "Location: /landing\r\nAccept-CH: Sec-CH-UA-Arch\r\n".to_owned(),
                    ),
                    "/clear" => ("200 OK", "Accept-CH: \r\n".to_owned()),
                    _ => ("200 OK", String::new()),
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: 2\r\n{extra}Connection: close\r\n\r\nok"
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    (base, log)
}

fn ctx(persona: Persona) -> NetContext {
    let json = std::fs::read_to_string(format!(
        "{}/../../profiles/tls/{}.json",
        env!("CARGO_MANIFEST_DIR"),
        persona.tls_profile_id
    ))
    .unwrap();
    let tls = Arc::new(ClientHelloProfile::from_json(&json).unwrap());
    NetContext::new(Arc::new(persona), tls).unwrap()
}

fn url(base: &str, p: &str) -> Url {
    Url::parse(&format!("{base}{p}")).unwrap()
}

fn req(kind: RequestKind, u: Url, initiator: Option<&Url>, top: Option<&Url>) -> Request {
    let mut meta = RequestMeta::new(kind, initiator.cloned());
    meta.top_frame = top.cloned();
    if kind == RequestKind::Fetch {
        meta.mode = "cors".into();
    }
    Request {
        method: Method::GET,
        url: u,
        headers: Vec::new(),
        body: Bytes::new(),
        meta: Some(meta),
    }
}

const ORDER7: [&str; 7] = [
    "sec-ch-ua-arch",
    "sec-ch-ua-platform-version",
    "sec-ch-ua-model",
    "sec-ch-ua-bitness",
    "sec-ch-ua-wow64",
    "sec-ch-ua-full-version-list",
    "sec-ch-ua-form-factors",
];

#[tokio::test]
async fn pipeline_critical_ch_restart_then_hints_everywhere_same_origin() {
    let (base, log) = server().await;
    let persona = Persona::default_chrome(); // Windows 10
    let mut c = ctx(persona.clone());
    let start = url(&base, "/start");
    let r = lb_net::fetch(&mut c, Request::get(start.clone()))
        .await
        .unwrap();
    assert_eq!(r.status, 200);
    {
        let seen = log.lock().unwrap();
        assert_eq!(seen.len(), 2, "Critical-CH restarts once");
        assert!(seen[0].hints().is_empty() && seen[0].get("cookie").is_none());
        let retry = &seen[1];
        assert_eq!(retry.path, "/start");
        assert_eq!(retry.hints(), ORDER7, "restart carries the hints in order");
        assert_eq!(
            retry.get("cookie"),
            Some("sid=1"),
            "cookie from the first response"
        );
        // Captured restart order: hints after Accept, before Sec-Fetch-*.
        let n = retry.names();
        let pos = |x: &str| n.iter().position(|y| y == x).unwrap();
        assert!(
            pos("accept") < pos("sec-ch-ua")
                && pos("sec-ch-ua-form-factors") < pos("sec-fetch-site")
        );
        assert_eq!(retry.get("sec-ch-ua-platform-version"), Some("\"10.0.0\""));
        assert_eq!(retry.get("sec-ch-ua-arch"), Some("\"x86\""));
        assert_eq!(retry.get("sec-ch-ua-wow64"), Some("?0"));
        assert_eq!(retry.get("sec-ch-ua-form-factors"), Some("\"Desktop\""));
        assert_eq!(
            retry.get("sec-ch-ua-full-version-list"),
            Some(
                r#""Google Chrome";v="153.0.8010.12", "Not_A Brand";v="8.0.0.0", "Chromium";v="153.0.8010.12""#
            )
        );
    }
    assert_eq!(r.request_headers, {
        let seen = log.lock().unwrap();
        seen[1].headers.clone()
    });

    // A later navigation: hints in the navigation order, no restart.
    lb_net::fetch(
        &mut c,
        req(
            RequestKind::Document,
            url(&base, "/next"),
            Some(&start),
            None,
        ),
    )
    .await
    .unwrap();
    // Same-origin subresource and same-origin iframe: hints.
    lb_net::fetch(
        &mut c,
        req(RequestKind::Image, url(&base, "/i.png"), Some(&start), None),
    )
    .await
    .unwrap();
    lb_net::fetch(
        &mut c,
        req(
            RequestKind::Iframe,
            url(&base, "/f.html"),
            Some(&start),
            Some(&start),
        ),
    )
    .await
    .unwrap();
    // Preflight: never.
    lb_net::fetch(
        &mut c,
        req(
            RequestKind::Preflight,
            url(&base, "/pre"),
            Some(&start),
            None,
        ),
    )
    .await
    .unwrap();
    // A request made by a cross-origin frame to the top origin: none.
    let other = Url::parse("http://127.0.0.1:1/frame").unwrap();
    lb_net::fetch(
        &mut c,
        req(
            RequestKind::Fetch,
            url(&base, "/api"),
            Some(&other),
            Some(&start),
        ),
    )
    .await
    .unwrap();
    let seen = log.lock().unwrap();
    let by = |p: &str| seen.iter().find(|s| s.path == p).unwrap().clone();
    let next = by("/next");
    assert_eq!(next.hints(), ORDER7);
    let n = next.names();
    assert_eq!(
        &n[..4],
        ["host", "connection", "sec-ch-ua", "sec-ch-ua-mobile"]
    );
    assert_eq!(n[5], "sec-ch-ua-platform", "arch sits before platform");
    assert_eq!(by("/i.png").hints().len(), 7);
    assert_eq!(by("/f.html").hints(), ORDER7);
    assert!(by("/pre").hints().is_empty());
    assert!(by("/api").hints().is_empty());
}

#[tokio::test]
async fn pipeline_redirect_accept_ch_counts_and_restart_happens_once() {
    let (base, log) = server().await;
    let mut c = ctx(Persona::default_chrome());
    // Accept-CH on a redirect response applies to the next hop.
    lb_net::fetch(&mut c, Request::get(url(&base, "/redir")))
        .await
        .unwrap();
    // Critical-CH names Arch (not in this response's Accept-CH, so ignored)
    // and Model (asked, not sent): one restart, then done even though Arch
    // is still missing.
    lb_net::fetch(&mut c, Request::get(url(&base, "/crit-again")))
        .await
        .unwrap();
    // An empty Accept-CH clears the origin's set.
    lb_net::fetch(&mut c, Request::get(url(&base, "/clear")))
        .await
        .unwrap();
    lb_net::fetch(&mut c, Request::get(url(&base, "/after")))
        .await
        .unwrap();
    let seen = log.lock().unwrap();
    let paths: Vec<&str> = seen.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/redir",
            "/landing",
            "/crit-again",
            "/crit-again",
            "/clear",
            "/after"
        ]
    );
    assert!(seen[0].hints().is_empty());
    assert_eq!(seen[1].hints(), ["sec-ch-ua-arch"]);
    // Redirect hop order: client hints after Sec-Fetch-Dest.
    let n = seen[1].names();
    assert!(n.iter().position(|x| x == "sec-fetch-dest") < n.iter().position(|x| x == "sec-ch-ua"));
    assert_eq!(seen[2].hints(), ["sec-ch-ua-arch"]);
    assert_eq!(
        seen[3].hints(),
        ["sec-ch-ua-model"],
        "Accept-CH replaced the set"
    );
    assert_eq!(seen[4].hints(), ["sec-ch-ua-model"]);
    assert!(seen[5].hints().is_empty(), "cleared");
}

#[tokio::test]
async fn pipeline_firefox_never_sends_hints_or_restarts() {
    let (base, log) = server().await;
    let mut c = ctx(Persona::default_firefox());
    lb_net::fetch(&mut c, Request::get(url(&base, "/start")))
        .await
        .unwrap();
    lb_net::fetch(&mut c, Request::get(url(&base, "/next")))
        .await
        .unwrap();
    let seen = log.lock().unwrap();
    assert_eq!(seen.len(), 2, "no restart");
    assert!(
        seen.iter()
            .all(|s| s.hints().is_empty() && s.get("sec-ch-ua").is_none())
    );
}

/// Hint values follow each reference persona (`high_entropy_hints()`).
#[tokio::test]
async fn pipeline_values_follow_every_reference_persona() {
    let dir = format!(
        "{}/../../profiles/reference/chrome",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut n = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let profile = lb_persona::PersonaProfile::from_path(&path).unwrap();
        let he = profile.high_entropy_hints().unwrap();
        let (base, log) = server().await;
        let mut c = ctx(profile.to_network());
        lb_net::fetch(&mut c, Request::get(url(&base, "/start")))
            .await
            .unwrap();
        let seen = log.lock().unwrap();
        let retry = &seen[1];
        assert_eq!(
            retry.get("sec-ch-ua-platform-version").unwrap(),
            format!("\"{}\"", he.platform_version),
            "{}",
            path.display()
        );
        assert_eq!(
            retry.get("sec-ch-ua-arch").unwrap(),
            format!("\"{}\"", he.architecture)
        );
        assert_eq!(
            retry.get("sec-ch-ua-bitness").unwrap(),
            format!("\"{}\"", he.bitness)
        );
        assert_eq!(
            retry.get("sec-ch-ua-full-version-list").unwrap(),
            lb_persona::format_brand_list(&he.full_version_list)
        );
        assert_eq!(
            retry.get("sec-ch-ua-platform").unwrap(),
            format!("\"{}\"", he.platform)
        );
        n += 1;
    }
    assert!(n >= 8, "{n} reference personas");
}
