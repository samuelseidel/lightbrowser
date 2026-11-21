//! Setup and response-head time limits (GAP-095), with loopback servers
//! that accept and then never answer: a request ends with an error at the
//! context's limit (lowered here to 500 ms) instead of hanging.
//!
//! - HTTP: the server reads the request and sends nothing (direct and
//!   through the connection pool);
//! - TLS: the server reads the ClientHello and sends nothing (direct and
//!   pooled);
//! - proxies: an HTTP proxy that reads `CONNECT` and a SOCKS5 proxy that
//!   reads the greeting, both silent;
//! - WebSocket: a silent server ends the opening handshake.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lb_net::fetch::{NetContext, NetTimeouts};
use lb_net::websocket::{WsConnect, WsEvent};
use lb_net::{ClientHelloProfile, ConnectionPool, NetError, Proxy, Request};
use lb_persona::Persona;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

const SHORT: Duration = Duration::from_millis(500);

fn ctx(pooled: bool) -> NetContext {
    let path = format!(
        "{}/../../profiles/tls/chrome_153_linux.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let profile = ClientHelloProfile::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut c = NetContext::new(Arc::new(Persona::default_chrome()), Arc::new(profile)).unwrap();
    c.danger_accept_invalid_certs = true;
    c.timeouts = NetTimeouts {
        tls_handshake: SHORT,
        proxy_handshake: SHORT,
        response_headers: SHORT,
        ws_handshake: SHORT,
    };
    if pooled {
        let pool = ConnectionPool::new();
        pool.use_calling_thread();
        c.pool = Some(pool);
    }
    c
}

/// A server that accepts, reads what comes and never writes.
async fn silent_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while let Ok(n) = s.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                }
            });
        }
    });
    port
}

/// Fetch `url` in `c`: it must fail, quickly.
async fn fails_fast(c: &mut NetContext, url: &str) -> NetError {
    let t0 = Instant::now();
    let r = tokio::time::timeout(
        Duration::from_secs(10),
        lb_net::fetch(c, Request::get(url.parse().unwrap())),
    )
    .await
    .expect("the request ended within 10 s");
    assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
    match r {
        Err(e) => e,
        Ok(r) => panic!("unexpected response {}", r.status),
    }
}

#[tokio::test]
async fn silent_http_server() {
    let port = silent_server().await;
    for pooled in [false, true] {
        let e = fails_fast(&mut ctx(pooled), &format!("http://127.0.0.1:{port}/")).await;
        assert!(e.to_string().contains("timed out"), "pooled {pooled}: {e}");
    }
}

#[tokio::test]
async fn silent_tls_handshake() {
    let port = silent_server().await;
    for pooled in [false, true] {
        let e = fails_fast(&mut ctx(pooled), &format!("https://127.0.0.1:{port}/")).await;
        assert!(
            matches!(&e, NetError::Tls(m) if m.contains("timed out")),
            "pooled {pooled}: {e}"
        );
    }
}

async fn silent_proxy(scheme: &str) -> NetError {
    let proxy_port = silent_server().await;
    let mut c = ctx(false);
    // Loopback targets bypass a proxy unless `<-loopback>`.
    let p = Proxy::parse(
        &format!("{scheme}://127.0.0.1:{proxy_port}"),
        Some("<-loopback>"),
    )
    .unwrap()
    .unwrap();
    c.proxy = Some(Arc::new(p));
    fails_fast(&mut c, "https://127.0.0.1:9/").await
}

#[tokio::test]
async fn silent_connect_proxy() {
    let e = silent_proxy("http").await;
    assert!(
        matches!(&e, NetError::Proxy(m) if m == "net::ERR_TIMED_OUT"),
        "{e}"
    );
}

#[tokio::test]
async fn silent_socks5_proxy() {
    let e = silent_proxy("socks5").await;
    assert!(
        matches!(&e, NetError::Proxy(m) if m == "net::ERR_TIMED_OUT"),
        "{e}"
    );
}

#[tokio::test]
async fn silent_websocket_server() {
    let port = silent_server().await;
    let mut c = ctx(false);
    let (_tx, mut ev) = c.websocket(WsConnect {
        url: format!("ws://127.0.0.1:{port}/").parse().unwrap(),
        protocols: Vec::new(),
        origin: format!("http://127.0.0.1:{port}"),
    });
    let first = tokio::time::timeout(Duration::from_secs(5), ev.recv())
        .await
        .expect("an event within 5 s");
    assert_eq!(
        first,
        Some(WsEvent::Failed(
            "WebSocket opening handshake timed out".into()
        ))
    );
}
