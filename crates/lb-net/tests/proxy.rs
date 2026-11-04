//! Proxies (ADR 0027) against loopback servers only: `tests/proxy/proxy.py`
//! (HTTP `CONNECT`/forward and SOCKS5, every target reached at 127.0.0.1)
//! in front of `lb-fptest`'s capture server or a small HTTP server here.
//!
//! - Through an HTTP `CONNECT` tunnel and through SOCKS5, the ClientHello
//!   (JA4 and JA4_r), the HTTP/2 Akamai fingerprint and the request's
//!   header names equal those of a direct connection.
//! - What reaches the proxy matches Chrome 153's recording
//!   (`tests/proxy/chrome.jsonl`, Chrome for Testing 153.0.8010.12 through
//!   the same proxy.py): `CONNECT` head, forward request line and header
//!   order, SOCKS5 greeting and domain-name address (remote DNS), the
//!   `407` round and `Proxy-Authorization` sent up front afterwards.
//! - Bypass: loopback stays direct unless `<-loopback>`; listed hosts too.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lb_fptest::fingerprint::{self, Ja4};
use lb_fptest::server::{Capture, CaptureServer, ServerConfig};
use lb_net::fetch::NetContext;
use lb_net::header_profile::{RequestKind, RequestMeta};
use lb_net::{ClientHelloProfile, ConnectionPool, NetError, Proxy, Request};
use lb_persona::Persona;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct ProxyProc {
    child: Child,
    http: u16,
    socks: u16,
    log: Arc<Mutex<Vec<Value>>>,
}

impl Drop for ProxyProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl ProxyProc {
    fn start(auth: Option<&str>) -> ProxyProc {
        let script = format!("{}/tests/proxy/proxy.py", env!("CARGO_MANIFEST_DIR"));
        let mut cmd = Command::new("python3");
        cmd.args([&script, "--http", "0", "--socks", "0"]);
        if let Some(a) = auth {
            cmd.args(["--auth", a]);
        }
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("python3 proxy.py");
        let mut ready = String::new();
        BufReader::new(child.stderr.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        let ports: Vec<u16> = ready
            .split_whitespace()
            .skip(1)
            .map(|p| p.parse().unwrap())
            .collect();
        let log = Arc::new(Mutex::new(Vec::new()));
        let out = child.stdout.take().unwrap();
        let l = Arc::clone(&log);
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                l.lock().unwrap().push(serde_json::from_str(&line).unwrap());
            }
        });
        ProxyProc {
            child,
            http: ports[0],
            socks: ports[1],
            log,
        }
    }

    fn entries(&self) -> Vec<Value> {
        // The log thread trails the proxy by a moment.
        std::thread::sleep(Duration::from_millis(100));
        self.log.lock().unwrap().clone()
    }
}

fn ctx(proxy: Option<Proxy>) -> NetContext {
    let path = format!(
        "{}/../../profiles/tls/chrome_153_linux.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let profile = ClientHelloProfile::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut c = NetContext::new(Arc::new(Persona::default_chrome()), Arc::new(profile)).unwrap();
    c.danger_accept_invalid_certs = true;
    c.pool = Some(ConnectionPool::new());
    c.proxy = proxy.map(Arc::new);
    c
}

fn nav(url: &str) -> Request {
    Request::for_kind(
        url.parse().unwrap(),
        RequestMeta::new(RequestKind::Document, None),
    )
}

/// The fingerprint of one captured connection: JA4, JA4_r, Akamai H2 and
/// the HTTP/2 request header names.
fn fp(c: &Capture) -> (String, String, String, Vec<Option<String>>) {
    let ch = c.client_hello.as_ref().expect("client hello");
    let h2 = c.h2.as_ref().expect("h2");
    (
        fingerprint::ja4(ch),
        Ja4::from_client_hello(ch, 't').raw(),
        h2.akamai(),
        h2.header_names.clone(),
    )
}

async fn capture_one(
    proxy: Option<Proxy>,
    host: &str,
    server: &CaptureServer,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Capture>,
) -> Capture {
    let mut c = ctx(proxy);
    let url = format!("https://{host}:{}/", server.local_addr().port());
    let r = lb_net::fetch(&mut c, nav(&url)).await.expect("fetch");
    assert_eq!(r.status.as_u16(), 200);
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("capture in time")
        .expect("capture")
}

#[tokio::test]
async fn tunnels_keep_the_direct_fingerprint() {
    let p = ProxyProc::start(None);
    let (server, mut rx) = CaptureServer::bind("127.0.0.1:0", ServerConfig::default())
        .await
        .unwrap();
    let port = server.local_addr().port();
    let direct = capture_one(None, "localhost", &server, &mut rx).await;
    // `<-loopback>`: localhost goes through the proxy too (Chrome's
    // implicit bypass lifted, as Playwright does).
    let http_proxy = Proxy::parse(&format!("127.0.0.1:{}", p.http), Some("<-loopback>"))
        .unwrap()
        .unwrap();
    let via_connect = capture_one(Some(http_proxy), "localhost", &server, &mut rx).await;
    // SOCKS5 with a name that does not resolve here: the proxy resolves it.
    let socks = Proxy::parse(&format!("socks5://127.0.0.1:{}", p.socks), None)
        .unwrap()
        .unwrap();
    let via_socks = capture_one(Some(socks), "proxied.test", &server, &mut rx).await;

    let d = fp(&direct);
    let c = fp(&via_connect);
    let s = fp(&via_socks);
    println!("direct  {d:?}\nconnect {c:?}\nsocks   {s:?}");
    assert_eq!(d, c);
    // SNI differs (proxied.test vs localhost) but no fingerprint field does.
    assert_eq!(d, s);

    let log = p.entries();
    let connect: Vec<&Value> = log.iter().filter(|e| e["proxy"] == "http").collect();
    assert_eq!(connect.len(), 1, "{log:?}");
    assert_eq!(
        connect[0]["line"],
        format!("CONNECT localhost:{port} HTTP/1.1")
    );
    let ua = Persona::default_chrome().user_agent;
    assert_eq!(
        connect[0]["headers"],
        serde_json::json!([
            ["Host", format!("localhost:{port}")],
            ["Proxy-Connection", "keep-alive"],
            ["User-Agent", ua]
        ])
    );
    let socks: Vec<&Value> = log.iter().filter(|e| e["proxy"] == "socks").collect();
    assert_eq!(socks[0]["greeting"], serde_json::json!([5, 1, 0]));
    assert_eq!(socks[1]["request"], serde_json::json!([5, 1, 0, 3]));
    assert_eq!(socks[1]["host"], "proxied.test");
    assert_eq!(socks[1]["port"], port);
}

/// A plain HTTP server: answers every request with a small page, records
/// request heads.
async fn http_server() -> (u16, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let heads = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&heads);
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let h = Arc::clone(&h);
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut b = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = s.read(&mut b).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&b[..n]);
                }
                h.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf).into_owned());
                let _ = s
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                    .await;
            });
        }
    });
    (port, heads)
}

/// Chrome's forward request for a typed `http:` navigation, recorded:
/// header names in order (`chrome.jsonl`, the plain run).
fn chrome_forward_names() -> Vec<String> {
    let path = format!("{}/tests/proxy/chrome.jsonl", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(path).unwrap();
    let rec: Value = text
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|v| v["line"] == "GET http://proxied.test:18510/ HTTP/1.1")
        .unwrap();
    rec["headers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h[0].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn http_forward_request_as_chrome() {
    let p = ProxyProc::start(None);
    let (port, heads) = http_server().await;
    let proxy = Proxy::parse(&format!("http://127.0.0.1:{}", p.http), None)
        .unwrap()
        .unwrap();
    let mut c = ctx(Some(proxy));
    let r = lb_net::fetch(&mut c, nav(&format!("http://proxied.test:{port}/a?b=1")))
        .await
        .unwrap();
    assert_eq!((r.status.as_u16(), &r.body[..]), (200, &b"ok"[..]));
    let log = p.entries();
    assert_eq!(
        log[0]["line"],
        format!("GET http://proxied.test:{port}/a?b=1 HTTP/1.1")
    );
    let names: Vec<String> = log[0]["headers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h[0].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, chrome_forward_names());
    assert!(heads.lock().unwrap()[0].starts_with("GET /a?b=1 HTTP/1.1\r\n"));

    // SOCKS5 tunnels plain http as well: origin-form, `Connection`.
    let socks = Proxy::parse(&format!("socks5://127.0.0.1:{}", p.socks), None)
        .unwrap()
        .unwrap();
    let mut c = ctx(Some(socks));
    lb_net::fetch(&mut c, nav(&format!("http://proxied.test:{port}/s")))
        .await
        .unwrap();
    let last = heads.lock().unwrap().last().unwrap().clone();
    assert!(
        last.starts_with("GET /s HTTP/1.1\r\nHost: proxied.test:"),
        "{last}"
    );
    assert!(last.contains("\r\nConnection: keep-alive\r\n"), "{last}");
}

#[tokio::test]
async fn proxy_auth_round_as_chrome() {
    let p = ProxyProc::start(Some("u1:p1"));
    let (server, mut rx) = CaptureServer::bind(
        "127.0.0.1:0",
        ServerConfig {
            alpn: vec![b"http/1.1".to_vec()],
            ..ServerConfig::default()
        },
    )
    .await
    .unwrap();
    let port = server.local_addr().port();
    let (hport, _) = http_server().await;

    // No credentials: the challenge comes back to the caller.
    let mut c = ctx(Some(
        Proxy::parse(&format!("127.0.0.1:{}", p.http), None)
            .unwrap()
            .unwrap(),
    ));
    let url = format!("https://proxied.test:{port}/");
    match lb_net::fetch(&mut c, nav(&url)).await {
        Err(NetError::ProxyAuth(ch)) => {
            assert_eq!(ch.origin, format!("127.0.0.1:{}", p.http));
            assert_eq!((ch.scheme.as_str(), ch.realm.as_str()), ("basic", "lbtest"));
        }
        other => panic!("{other:?}"),
    }
    // The answer (CDP Fetch.continueWithAuth in lb-page) is stored; the
    // request goes again with credentials.
    c.proxy
        .as_ref()
        .unwrap()
        .provide_credentials(lb_net::Credentials {
            username: "u1".into(),
            password: "p1".into(),
        });
    lb_net::fetch(&mut c, nav(&url)).await.unwrap();
    rx.recv().await.unwrap();

    // Configured credentials: the first CONNECT goes without, the 407 is
    // answered on the same connection, later ones carry them up front.
    let mut c = ctx(Some(
        Proxy::parse(&format!("http://u1:p1@127.0.0.1:{}", p.http), None)
            .unwrap()
            .unwrap(),
    ));
    let before = p.entries().len();
    lb_net::fetch(&mut c, nav(&url)).await.unwrap();
    rx.recv().await.unwrap();
    lb_net::fetch(&mut c, nav(&format!("http://proxied.test:{hport}/")))
        .await
        .unwrap();
    let log = p.entries()[before..].to_vec();
    let auth = |e: &Value| {
        e["headers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h[0] == "Proxy-Authorization" && h[1] == "Basic dTE6cDE=")
    };
    assert_eq!(log.len(), 3, "{log:?}");
    assert!(!auth(&log[0]));
    assert!(auth(&log[1]));
    assert_eq!(log[0]["conn"], log[1]["conn"]);
    assert_eq!(log[1]["req"], 2);
    // Forward request with cached credentials: Chrome's position.
    assert!(log[2]["line"].as_str().unwrap().starts_with("GET http://"));
    assert!(auth(&log[2]));

    // Wrong credentials: the second 407 is the caller's.
    let mut c = ctx(Some(
        Proxy::parse(&format!("http://u1:bad@127.0.0.1:{}", p.http), None)
            .unwrap()
            .unwrap(),
    ));
    assert!(matches!(
        lb_net::fetch(&mut c, nav(&url)).await,
        Err(NetError::ProxyAuth(_))
    ));
    // SOCKS5 with credentials (RFC 1929).
    let mut c = ctx(Some(
        Proxy::parse(&format!("socks5://u1:p1@127.0.0.1:{}", p.socks), None)
            .unwrap()
            .unwrap(),
    ));
    lb_net::fetch(&mut c, nav(&url)).await.unwrap();
    let log = p.entries();
    let s: Vec<&Value> = log.iter().filter(|e| e["proxy"] == "socks").collect();
    assert_eq!(s[0]["greeting"], serde_json::json!([5, 2, 0, 2]));
    assert_eq!(s[1]["ok"], true);
}

#[tokio::test]
async fn bypass_and_failures() {
    let p = ProxyProc::start(None);
    let (port, heads) = http_server().await;
    // Loopback is direct by default (Chrome's implicit rules).
    let mut c = ctx(Some(
        Proxy::parse(&format!("127.0.0.1:{}", p.http), Some("*.skip.test"))
            .unwrap()
            .unwrap(),
    ));
    lb_net::fetch(&mut c, nav(&format!("http://127.0.0.1:{port}/")))
        .await
        .unwrap();
    assert_eq!(heads.lock().unwrap().len(), 1);
    assert!(p.entries().is_empty());
    // A listed host is direct: it does not resolve, so it fails as DNS.
    let e = lb_net::fetch(&mut c, nav("http://a.skip.test/"))
        .await
        .unwrap_err();
    assert!(matches!(e, NetError::Dns(_)), "{e:?}");
    assert!(p.entries().is_empty());
    // A proxy that is not there: Chrome's error.
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    let mut c = ctx(Some(
        Proxy::parse(&format!("127.0.0.1:{dead_port}"), None)
            .unwrap()
            .unwrap(),
    ));
    let e = lb_net::fetch(&mut c, nav("https://proxied.test/"))
        .await
        .unwrap_err();
    assert_eq!(e.to_string(), "net::ERR_PROXY_CONNECTION_FAILED");
}
