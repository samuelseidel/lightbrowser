//! Connection reuse (ADR 0018) against `lb-fptest`'s page server on
//! 127.0.0.1. Loopback only.
//!
//! The expected numbers come from local Chrome for Testing 153.0.8010.12
//! loading the same pages from the same server (`lb-fptest site`, driven by
//! Playwright after Chrome's start-up traffic settled; see ADR 0018):
//!
//! - HTTP/1.1, 1 page + 24 subresources + favicon: 6 connections, the
//!   page's connection reused, every connection carrying 4 or 5 requests.
//! - The same over TLS: connections 2 and 3 resume the session of the
//!   first (two tickets, single use) with ClientHello JA4
//!   `t13d1518h2_8daaf6152771_a12c4694fc1a`, `pre_shared_key` last; the
//!   rest are full handshakes.
//! - HTTP/2: one connection for everything, request HEADERS priority
//!   blocks as listed in [`CHROME_H2_PRIORITIES`].
//! - 12 simultaneous requests to an HTTPS server not seen before: 6
//!   connections; the first to negotiate h2 carries all 12 requests, the
//!   other 5 are closed after their handshake without a frame.
//! - Network partitions: a cross-site frame's request and the same URL as a
//!   top-level navigation use different connections.

use std::sync::Arc;
use std::time::Duration;

use lb_fptest::clienthello::is_grease;
use lb_fptest::fingerprint::Ja4;
use lb_fptest::site::{ConnRecord, Route, SiteConfig, SiteServer};
use lb_net::fetch::NetContext;
use lb_net::{ClientHelloProfile, ConnectionPool, HttpVersion, Request, RequestKind, RequestMeta};
use lb_persona::Persona;
use url::Url;

/// Chrome 153's resumed ClientHello (lb-fptest on loopback).
const CHROME_RESUMED_JA4: &str = "t13d1518h2_8daaf6152771_a12c4694fc1a";
/// Chrome 153's full ClientHello (the `chrome_153_linux` profile).
const CHROME_FULL_JA4: &str = "t13d1517h2_8daaf6152771_344585cda68d";
/// HEADERS priority blocks (`stream:exclusive:depends_on:weight`) Chrome
/// sent for `/`, then `/css/0.css`, `/css/1.css` (`u=0`), `/js/0.js`,
/// `/js/1.js` (`u=2`), six images (`i`, urgency 3) and, once those were
/// done, the favicon (`u=1, i`).
const CHROME_H2_PRIORITIES: &[&str] = &[
    "s1:1:0:256",
    "s3:1:0:256",
    "s5:1:3:256",
    "s7:1:5:183",
    "s9:1:7:183",
    "s11:1:9:147",
    "s13:1:11:147",
    "s15:1:13:147",
    "s17:1:15:147",
    "s19:1:17:147",
    "s21:1:19:147",
    "s23:1:0:220",
];

fn profile() -> Arc<ClientHelloProfile> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../profiles/tls/chrome_153_linux.json"
    );
    Arc::new(ClientHelloProfile::load(path).unwrap())
}

/// A pooled network context (one per concurrent request, all sharing
/// `pool`, as the contexts of one browser context do).
fn net(pool: &ConnectionPool) -> NetContext {
    let mut n = NetContext::new(Arc::new(Persona::default_chrome()), profile()).unwrap();
    n.danger_accept_invalid_certs = true;
    n.pool = Some(pool.clone());
    n
}

fn sub(url: Url, kind: RequestKind, page: &Url, priority: Option<&str>) -> Request {
    let mut m = RequestMeta::new(kind, Some(page.clone()));
    m.priority = priority.map(str::to_owned);
    Request::for_kind(url, m)
}

async fn get(pool: &ConnectionPool, req: Request) -> lb_net::Response {
    let resp = lb_net::fetch(&mut net(pool), req).await.unwrap();
    assert_eq!(resp.status, 200, "{}", resp.final_url);
    resp
}

/// Start `reqs` one after the other (2 ms apart, so the wire order is the
/// list order) and wait for all.
async fn all(pool: &ConnectionPool, reqs: Vec<Request>) -> Vec<lb_net::Response> {
    all_with_gap(pool, reqs, Duration::from_millis(2)).await
}

async fn all_with_gap(
    pool: &ConnectionPool,
    reqs: Vec<Request>,
    gap: Duration,
) -> Vec<lb_net::Response> {
    let mut tasks = Vec::new();
    for r in reqs {
        let pool = pool.clone();
        tasks.push(tokio::spawn(async move { get(&pool, r).await }));
        if !gap.is_zero() {
            tokio::time::sleep(gap).await;
        }
    }
    let mut out = Vec::new();
    for t in tasks {
        out.push(t.await.unwrap());
    }
    out
}

/// What a page with `images` images, 2 scripts and 2 stylesheets asks for
/// after the document, in Chrome's order.
fn subresources(page: &Url, images: usize) -> Vec<Request> {
    let u = |p: &str| page.join(p).unwrap();
    let mut v = vec![
        sub(u("/css/0.css"), RequestKind::Style, page, Some("u=0")),
        sub(u("/css/1.css"), RequestKind::Style, page, Some("u=0")),
        sub(u("/js/0.js"), RequestKind::Script, page, Some("u=2")),
        sub(u("/js/1.js"), RequestKind::Script, page, Some("u=2")),
    ];
    for i in 0..images {
        v.push(sub(
            u(&format!("/img/{i}.gif")),
            RequestKind::Image,
            page,
            Some("i"),
        ));
    }
    v
}

/// The document, then its subresources at once (as the preload scanner
/// finds them), then the favicon.
async fn load_page(pool: &ConnectionPool, page: &Url, images: usize) {
    get(pool, Request::get(page.clone())).await;
    all_with_gap(pool, subresources(page, images), Duration::ZERO).await;
    get(
        pool,
        sub(
            page.join("/favicon.ico").unwrap(),
            RequestKind::Favicon,
            page,
            None,
        ),
    )
    .await;
}

fn requests(r: &ConnRecord) -> Vec<&str> {
    r.requests.iter().map(|q| q.path.as_str()).collect()
}

fn ja4(r: &ConnRecord) -> String {
    Ja4::from_client_hello(r.client_hello.as_ref().unwrap(), 't').hashed()
}

#[tokio::test]
async fn http1_keeps_six_connections_alive_like_chrome() {
    let delay = Duration::from_millis(50);
    let srv = SiteServer::bind(
        "127.0.0.1:0",
        SiteConfig::subresource_page(false, 20, 2, 2, delay),
    )
    .await
    .unwrap();
    let page: Url = format!("http://127.0.0.1:{}/", srv.local_addr().port())
        .parse()
        .unwrap();
    let pool = ConnectionPool::new();
    load_page(&pool, &page, 20).await;

    let recs = srv.records();
    assert_eq!(recs.len(), 6, "connections: {recs:#?}");
    assert_eq!(recs.iter().map(|r| r.requests.len()).sum::<usize>(), 26);
    // The document's connection carried subresources too, and every
    // connection more than one request (Chrome: 4 or 5 each).
    assert_eq!(requests(&recs[0])[0], "/");
    for r in &recs {
        assert!(r.requests.len() >= 3, "{:?}", requests(r));
        assert!(!r.closed, "connection {} was closed", r.id);
    }
    assert_eq!(pool.opened(), 6);
    assert_eq!(pool.idle(&key(&page)), 6);

    // A later navigation reuses the most recently used idle connection.
    let next = get(&pool, Request::get(page.join("/next").unwrap())).await;
    assert!(next.connection_reused);
    assert_eq!(srv.records().len(), 6);
    let last_favicon = srv
        .records()
        .iter()
        .find(|r| requests(r).contains(&"/favicon.ico"))
        .unwrap()
        .id;
    assert_eq!(
        srv.records()[last_favicon].requests.last().unwrap().path,
        "/next"
    );
}

fn key(page: &Url) -> lb_net::pool::GroupKey {
    lb_net::pool::GroupKey::for_request(page, &RequestMeta::navigation()).unwrap()
}

#[tokio::test]
async fn tls_sessions_resume_like_chrome() {
    let mut cfg = SiteConfig::subresource_page(true, 20, 2, 2, Duration::from_millis(50));
    cfg.alpn = vec![b"http/1.1".to_vec()];
    let srv = SiteServer::bind("127.0.0.1:0", cfg).await.unwrap();
    let page: Url = format!("https://localhost:{}/", srv.local_addr().port())
        .parse()
        .unwrap();
    let pool = ConnectionPool::new();
    load_page(&pool, &page, 20).await;

    let recs = srv.records();
    assert_eq!(recs.len(), 6, "connections: {recs:#?}");
    assert!(!recs[0].resumed());
    assert_eq!(ja4(&recs[0]), CHROME_FULL_JA4);
    let resumed: Vec<_> = recs.iter().filter(|r| r.resumed()).collect();
    assert_eq!(resumed.len(), 2, "two tickets, each used once");
    for r in &recs {
        let want = if r.resumed() {
            CHROME_RESUMED_JA4
        } else {
            CHROME_FULL_JA4
        };
        assert_eq!(ja4(r), want, "connection {}", r.id);
        if r.resumed() {
            // pre_shared_key must be last; GREASE stays just before it.
            let ids = r.client_hello.as_ref().unwrap().extension_ids();
            assert_eq!(ids.last(), Some(&41));
            assert!(is_grease(ids[ids.len() - 2]));
            assert!(!ids.contains(&42), "no early_data, as in Chrome");
        }
    }
    // The connections that resumed got new tickets in turn.
    assert!(pool.cached_sessions(&key(&page)) > 0);
}

#[tokio::test]
async fn h2_one_session_with_chrome_priorities() {
    let srv = SiteServer::bind(
        "127.0.0.1:0",
        SiteConfig::subresource_page(true, 6, 2, 2, Duration::from_millis(200)),
    )
    .await
    .unwrap();
    let page: Url = format!("https://localhost:{}/", srv.local_addr().port())
        .parse()
        .unwrap();
    let pool = ConnectionPool::new();
    let first = get(&pool, Request::get(page.clone())).await;
    assert_eq!(first.version, HttpVersion::Http2);
    assert!(!first.connection_reused);
    let subs = all(&pool, subresources(&page, 6)).await;
    assert!(
        subs.iter()
            .all(|r| r.connection_reused && r.connection_id == first.connection_id)
    );
    get(
        &pool,
        sub(
            page.join("/favicon.ico").unwrap(),
            RequestKind::Favicon,
            &page,
            None,
        ),
    )
    .await;

    let recs = srv.records();
    assert_eq!(recs.len(), 1, "one HTTP/2 connection: {recs:#?}");
    let r = &recs[0];
    assert_eq!(r.requests.len(), 12);
    let prios: Vec<String> = r
        .h2_priorities
        .iter()
        .map(|p| {
            format!(
                "s{}:{}:{}:{}",
                p.stream,
                u8::from(p.exclusive),
                p.depends_on,
                p.weight as u16 + 1
            )
        })
        .collect();
    assert_eq!(prios, CHROME_H2_PRIORITIES);
    let frames: Vec<String> = r.h2_frames[..3]
        .iter()
        .map(|(n, s)| format!("{n}:{s}"))
        .collect();
    assert_eq!(frames, ["SETTINGS:0", "WINDOW_UPDATE:0", "HEADERS:1"]);
}

#[tokio::test]
async fn unknown_h2_server_race_like_chrome() {
    let srv = SiteServer::bind(
        "127.0.0.1:0",
        SiteConfig::subresource_page(true, 12, 0, 0, Duration::from_millis(50)),
    )
    .await
    .unwrap();
    let base: Url = format!("https://localhost:{}/", srv.local_addr().port())
        .parse()
        .unwrap();
    let page: Url = "http://127.0.0.1:1/xo".parse().unwrap();
    let pool = ConnectionPool::new();
    let reqs = (0..12)
        .map(|i| {
            sub(
                base.join(&format!("/img/{i}.gif")).unwrap(),
                RequestKind::Image,
                &page,
                None,
            )
        })
        .collect();
    // All at once, as the page's script adds them.
    let resps = all_with_gap(&pool, reqs, Duration::ZERO).await;
    assert!(resps.iter().all(|r| r.version == HttpVersion::Http2));
    // Let the spare handshakes finish and close.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let recs = srv.records();
    assert_eq!(recs.len(), 6, "connections: {recs:#?}");
    let used: Vec<_> = recs.iter().filter(|r| !r.requests.is_empty()).collect();
    assert_eq!(used.len(), 1);
    assert_eq!(used[0].requests.len(), 12);
    for r in recs.iter().filter(|r| r.requests.is_empty()) {
        assert_eq!(r.alpn.as_deref(), Some("h2"), "handshake completed");
        assert!(r.h2_frames.is_empty(), "nothing sent: {:?}", r.h2_frames);
    }
    // The server is now known to speak h2: after the session is gone, one
    // connection serves the next burst.
    assert_eq!(pool.opened(), 6);
}

#[tokio::test]
async fn partitions_by_top_level_site_like_chrome() {
    let srv = SiteServer::bind(
        "127.0.0.1:0",
        SiteConfig::subresource_page(false, 0, 0, 0, Duration::ZERO),
    )
    .await
    .unwrap();
    let port = srv.local_addr().port();
    let top: Url = format!("http://127.0.0.1:{port}/frame").parse().unwrap();
    let other: Url = format!("http://localhost:{port}/next").parse().unwrap();
    let pool = ConnectionPool::new();
    get(&pool, Request::get(top.clone())).await;
    // The page's cross-site frame.
    get(
        &pool,
        Request::for_kind(
            other.clone(),
            RequestMeta::new(RequestKind::Iframe, Some(top.clone())),
        ),
    )
    .await;
    // The frame's URL as a top-level page: another partition.
    get(&pool, Request::get(other.clone())).await;
    // Back on the first site: its idle connection.
    let back = get(&pool, Request::get(top.join("/next").unwrap())).await;
    assert!(back.connection_reused);

    let recs = srv.records();
    let reqs: Vec<Vec<&str>> = recs.iter().map(requests).collect();
    assert_eq!(
        reqs,
        [vec!["/frame", "/next"], vec!["/next"], vec!["/next"]]
    );
}

#[tokio::test]
async fn contexts_never_share_connections() {
    let srv = SiteServer::bind(
        "127.0.0.1:0",
        SiteConfig::subresource_page(false, 0, 0, 0, Duration::ZERO),
    )
    .await
    .unwrap();
    let page: Url = format!("http://127.0.0.1:{}/next", srv.local_addr().port())
        .parse()
        .unwrap();
    let (a, b) = (ConnectionPool::new(), ConnectionPool::new());
    get(&a, Request::get(page.clone())).await;
    get(&b, Request::get(page.clone())).await;
    get(&a, Request::get(page.clone())).await;
    let reqs: Vec<usize> = srv.records().iter().map(|r| r.requests.len()).collect();
    assert_eq!(reqs, [2, 1]);
}

/// A server that answers the first request of each connection with
/// keep-alive, then closes the connection when the next request arrives
/// (an idle timeout racing the reuse). Chrome sends the request again on a
/// new connection.
#[tokio::test]
async fn stale_reused_connection_is_retried() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let acc = Arc::clone(&accepted);
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            acc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::spawn(async move {
                let mut served = 0;
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        continue;
                    }
                    buf.clear();
                    if served == 1 {
                        return; // close without answering
                    }
                    served += 1;
                    let _ = s
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                        .await;
                }
            });
        }
    });
    let pool = ConnectionPool::new();
    let url: Url = format!("http://127.0.0.1:{port}/").parse().unwrap();
    let a = get(&pool, Request::get(url.clone())).await;
    let b = get(&pool, Request::get(url.clone())).await;
    assert_eq!(&b.body[..], b"ok");
    assert_ne!(a.connection_id, b.connection_id);
    assert!(!b.connection_reused);
    assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// Responses that end the connection are not reused: `Connection: close`,
/// a body read until close, HTTP/1.0 without keep-alive.
#[tokio::test]
async fn non_persistent_responses_close() {
    let mut cfg = SiteConfig::subresource_page(false, 0, 0, 0, Duration::ZERO);
    let mut close = Route::new("text/plain", "bye");
    close.close = true;
    cfg.routes.insert("/close".into(), close);
    let srv = SiteServer::bind("127.0.0.1:0", cfg).await.unwrap();
    let base: Url = format!("http://127.0.0.1:{}/", srv.local_addr().port())
        .parse()
        .unwrap();
    let pool = ConnectionPool::new();
    get(&pool, Request::get(base.join("/close").unwrap())).await;
    let next = get(&pool, Request::get(base.join("/next").unwrap())).await;
    assert!(!next.connection_reused);
    assert_eq!(srv.records().len(), 2);
}
