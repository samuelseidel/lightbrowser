//! The egress policy (ADR 0044) at the transport level, with no external
//! network. Servers are bound to loopback or to this machine's own address on
//! its default route (found with a connected UDP socket, which sends
//! nothing); tests that need that address print a message and pass without
//! it. Every server here is on this machine; "outside" means "not a
//! loopback address".
//!
//! What is checked:
//! - loopback mode reaches loopback servers (by IP and by `localhost`);
//! - an outside address is refused with `NetError::Blocked` before any SYN:
//!   the listener has no queued connection afterwards (a SYN would have
//!   completed the handshake in the kernel and queued one), the pool has no
//!   group, no connection and no TLS session for it;
//! - `--allow-host` forms (IP, CIDR, name through a recording hosts
//!   override) allow it; a name that is not allowed is never looked up (the
//!   override records every lookup);
//! - the names `localhost` and `*.localhost` must resolve to loopback;
//! - redirects are checked per hop; numeric spellings of an address are
//!   refused like the address;
//! - proxies: a refused target never reaches the proxy (the loopback
//!   `tests/proxy/proxy.py` logs nothing), an allowed one does, and the
//!   proxy's own address must be allowed;
//! - WebSocket handshakes are refused the same way.
//!
//! These tests have no browser to record: the refusal logic is ours, the
//! error texts it ends in were recorded from Chrome in `lb-page`'s
//! `tests/egress/chrome.json`.

use std::io::{BufRead, BufReader};
use std::net::{IpAddr, SocketAddr, TcpListener as StdListener};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lb_net::fetch::NetContext;
use lb_net::websocket::{WsConnect, WsEvent};
use lb_net::{
    ClientHelloProfile, ConnectionPool, EgressMode, EgressPolicy, NetError, Proxy, Request,
    Response,
};
use lb_persona::Persona;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn ctx(policy: Option<EgressPolicy>, pooled: bool) -> NetContext {
    let path = format!(
        "{}/../../profiles/tls/chrome_153_linux.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let profile = ClientHelloProfile::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut c = NetContext::new(Arc::new(Persona::default_chrome()), Arc::new(profile)).unwrap();
    c.danger_accept_invalid_certs = true;
    if pooled {
        c.pool = Some(ConnectionPool::new());
    }
    if let Some(p) = policy {
        c = c.with_egress(Arc::new(p));
    }
    c
}

fn loopback_with(allow: &[&str]) -> EgressPolicy {
    EgressPolicy::new(EgressMode::Loopback, allow).unwrap()
}

/// This machine's address on its default route (no packet is sent).
fn outside_ip() -> Option<IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

macro_rules! outside_or_skip {
    () => {
        match outside_ip() {
            Some(ip) => ip,
            None => {
                eprintln!("skipped: this machine has no non-loopback address on a default route");
                return;
            }
        }
    };
}

/// A listener nobody accepts from: any connection that completed the
/// handshake sits in its backlog.
struct Silent(StdListener);

impl Silent {
    fn bind(ip: IpAddr) -> Silent {
        let l = StdListener::bind(SocketAddr::new(ip, 0)).unwrap();
        l.set_nonblocking(true).unwrap();
        Silent(l)
    }
    fn port(&self) -> u16 {
        self.0.local_addr().unwrap().port()
    }
    /// No SYN reached the listener.
    fn untouched(&self) -> bool {
        std::thread::sleep(Duration::from_millis(100));
        matches!(self.0.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
    }
}

/// A tiny HTTP/1.1 server: `routes(path)` gives (status, extra headers,
/// body). Counts requests.
struct Site {
    port: u16,
    hits: Arc<AtomicUsize>,
}

async fn site(
    bind: IpAddr,
    routes: impl Fn(&str) -> (u16, Vec<(String, String)>, String) + Send + Sync + 'static,
) -> Site {
    let listener = TcpListener::bind(SocketAddr::new(bind, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let (h2, routes) = (hits.clone(), Arc::new(routes));
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let (hits, routes) = (h2.clone(), routes.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                }
                hits.fetch_add(1, Ordering::SeqCst);
                let head = String::from_utf8_lossy(&buf).to_string();
                let path = head.split(' ').nth(1).unwrap_or("/").to_owned();
                let (status, extra, body) = routes(&path);
                let mut out = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\n", body.len());
                for (k, v) in extra {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str("Connection: close\r\n\r\n");
                out.push_str(&body);
                let _ = sock.write_all(out.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Site { port, hits }
}

fn ok_site() -> impl Fn(&str) -> (u16, Vec<(String, String)>, String) + Send + Sync + 'static {
    |_| (200, vec![], "hello".into())
}

fn get(url: &str) -> Request {
    Request::get(url.parse().unwrap())
}

async fn blocked(c: &mut NetContext, url: &str) -> lb_net::Blocked {
    match lb_net::fetch(c, get(url)).await {
        Err(NetError::Blocked(b)) => b,
        Err(e) => panic!("{url}: expected a refusal, got error {e}"),
        Ok(r) => panic!("{url}: expected a refusal, got {}", r.status),
    }
}

async fn reaches(c: &mut NetContext, url: &str) -> Response {
    lb_net::fetch(c, get(url))
        .await
        .unwrap_or_else(|e| panic!("{url}: {e}"))
}

/// Install a hosts override on the context and its pool; it records every
/// lookup it is asked for.
fn hosts(c: &NetContext, map: &[(&str, IpAddr)]) -> Arc<Mutex<Vec<String>>> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let map: Vec<(String, IpAddr)> = map.iter().map(|(h, i)| (h.to_string(), *i)).collect();
    let calls2 = calls.clone();
    let hook: Arc<lb_net::pool::ResolveHook> = Arc::new(move |host: &str| {
        calls2.lock().unwrap().push(host.to_owned());
        map.iter()
            .find(|(h, _)| h == host)
            .map(|(_, ip)| vec![*ip])
    });
    c.set_resolve_hook(Some(hook.clone()));
    if let Some(p) = &c.pool {
        p.set_resolve_hook(Some(hook));
    }
    calls
}

#[tokio::test]
async fn loopback_mode_reaches_loopback_servers() {
    let s = site("127.0.0.1".parse().unwrap(), ok_site()).await;
    for pooled in [false, true] {
        let mut c = ctx(Some(EgressPolicy::loopback()), pooled);
        for host in ["127.0.0.1", "localhost", "LOCALHOST.", "0x7f.1"] {
            let r = reaches(&mut c, &format!("http://{host}:{}/", s.port)).await;
            assert_eq!(r.status.as_u16(), 200, "{host}");
        }
    }
    assert_eq!(s.hits.load(Ordering::SeqCst), 8);
}

#[tokio::test]
async fn loopback_ipv6_and_mapped_forms() {
    // Skip quietly where the box has no IPv6 loopback.
    let Ok(l) = TcpListener::bind("[::1]:0").await else {
        eprintln!("skipped: no IPv6 loopback");
        return;
    };
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b).await;
            let _ = s
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi")
                .await;
        }
    });
    let mut c = ctx(Some(EgressPolicy::loopback()), true);
    assert_eq!(reaches(&mut c, &format!("http://[::1]:{port}/")).await.status, 200);
}

#[tokio::test]
async fn open_mode_reaches_an_outside_address() {
    // The control for the refusal tests: with no policy the same server is
    // reachable, so the refusals below are the policy's doing.
    let ip = outside_or_skip!();
    let s = site(ip, ok_site()).await;
    for policy in [None, Some(EgressPolicy::open())] {
        let mut c = ctx(policy, true);
        assert_eq!(reaches(&mut c, &format!("http://{ip}:{}/", s.port)).await.status, 200);
    }
}

#[tokio::test]
async fn loopback_mode_refuses_an_outside_address_without_a_syn() {
    let ip = outside_or_skip!();
    let l = Silent::bind(ip);
    for pooled in [false, true] {
        let mut c = ctx(Some(EgressPolicy::loopback()), pooled);
        let b = blocked(&mut c, &format!("http://{ip}:{}/", l.port())).await;
        assert_eq!(b.target, ip.to_string());
        assert_eq!(b.kind, lb_net::egress::BlockedKind::Address);
        if let Some(p) = &c.pool {
            assert_eq!((p.groups(), p.opened()), (0, 0), "a refused origin has no pool entry");
        }
    }
    assert!(l.untouched(), "a connection reached the refused listener");
}

#[tokio::test]
async fn a_refused_https_origin_leaves_no_pool_entry_or_tls_session() {
    let ip = outside_or_skip!();
    let l = Silent::bind(ip);
    let mut c = ctx(Some(EgressPolicy::loopback()), true);
    let url = format!("https://{ip}:{}/", l.port());
    for _ in 0..3 {
        blocked(&mut c, &url).await;
    }
    let pool = c.pool.clone().unwrap();
    assert_eq!((pool.groups(), pool.opened()), (0, 0));
    let key = lb_net::pool::GroupKey {
        https: true,
        host: ip.to_string(),
        port: l.port(),
        partition: String::new(),
    };
    assert_eq!(pool.cached_sessions(&key), 0);
    assert!(l.untouched());
}

#[tokio::test]
async fn allow_host_ip_cidr_and_name_forms_allow_an_outside_server() {
    let ip = outside_or_skip!();
    let s = site(ip, ok_site()).await;
    let url = format!("http://{ip}:{}/", s.port);
    // A /24 around the address, the exact address, and (for IPv4) a name
    // the hosts override maps to it.
    let cidr = match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            format!("{}.{}.{}.0/24", o[0], o[1], o[2])
        }
        IpAddr::V6(_) => format!("{ip}/64"),
    };
    let exact = match ip {
        IpAddr::V4(_) => format!("{ip}/32"),
        IpAddr::V6(_) => format!("{ip}/128"),
    };
    for entry in [exact, ip.to_string(), cidr] {
        for pooled in [false, true] {
            let mut c = ctx(Some(loopback_with(&[&entry])), pooled);
            assert_eq!(reaches(&mut c, &url).await.status, 200, "{entry}");
        }
    }
    // The name form: `*.lab.test` is allowed, resolves (through the
    // override) to the outside address; the address check passes because an
    // entry vouched for the name.
    for pooled in [false, true] {
        let mut c = ctx(Some(loopback_with(&["*.lab.test"])), pooled);
        let calls = hosts(&c, &[("app.lab.test", ip)]);
        let r = reaches(&mut c, &format!("http://app.lab.test:{}/", s.port)).await;
        assert_eq!(r.status.as_u16(), 200);
        assert_eq!(*calls.lock().unwrap(), vec!["app.lab.test".to_owned()]);
    }
    // An allow entry for another address does not help.
    let mut c = ctx(Some(loopback_with(&["10.255.255.254"])), true);
    blocked(&mut c, &url).await;
    assert!(s.hits.load(Ordering::SeqCst) >= 8);
}

#[tokio::test]
async fn a_name_that_is_not_allowed_is_never_resolved() {
    for pooled in [false, true] {
        let mut c = ctx(Some(EgressPolicy::loopback()), pooled);
        let calls = hosts(&c, &[("example.test", "93.184.216.34".parse().unwrap())]);
        for url in [
            "http://example.test/",
            "https://example.test/",
            "http://EXAMPLE.test:8080/x",
            "http://sub.example.test./",
            "http://notlocalhost/",
            "http://localhost.example.test/",
        ] {
            let b = blocked(&mut c, url).await;
            assert_eq!(b.kind, lb_net::egress::BlockedKind::Name, "{url}");
        }
        assert!(
            calls.lock().unwrap().is_empty(),
            "looked up: {:?}",
            calls.lock().unwrap()
        );
        if let Some(p) = &c.pool {
            assert_eq!((p.groups(), p.opened()), (0, 0));
        }
    }
    // In allowlist mode even `localhost` needs an entry.
    let mut c = ctx(
        Some(EgressPolicy::new(EgressMode::AllowList, &["only.test"]).unwrap()),
        true,
    );
    let calls = hosts(&c, &[("localhost", "127.0.0.1".parse().unwrap())]);
    blocked(&mut c, "http://localhost:1/").await;
    blocked(&mut c, "http://127.0.0.1:1/").await;
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn localhost_names_must_resolve_to_loopback() {
    let ip = outside_or_skip!();
    let outside = Silent::bind(ip);
    let inside = site("127.0.0.1".parse().unwrap(), ok_site()).await;
    for pooled in [false, true] {
        let mut c = ctx(Some(EgressPolicy::loopback()), pooled);
        // Names that look like localhost but are made to resolve elsewhere
        // (a hosts file, a DNS answer, a rebinding attack) are refused at
        // the address check, so no SYN goes out.
        let calls = hosts(
            &c,
            &[("evil.localhost", ip), ("good.localhost", "127.0.0.1".parse().unwrap())],
        );
        let b = blocked(&mut c, &format!("http://evil.localhost:{}/", outside.port())).await;
        assert_eq!(b.kind, lb_net::egress::BlockedKind::Address);
        assert_eq!(
            reaches(&mut c, &format!("http://good.localhost:{}/", inside.port)).await.status,
            200
        );
        assert_eq!(calls.lock().unwrap().len(), 2);
    }
    assert!(outside.untouched());
}

#[tokio::test]
async fn redirects_are_checked_at_every_hop() {
    let ip = outside_or_skip!();
    let outside = Silent::bind(ip);
    let inside_ip: IpAddr = "127.0.0.1".parse().unwrap();
    let target = format!("http://{ip}:{}/secret", outside.port());
    let t2 = target.clone();
    let hop = site(inside_ip, move |p| match p {
        "/to-ip" => (302, vec![("Location".into(), t2.clone())], String::new()),
        "/to-name" => (
            302,
            vec![("Location".into(), "http://example.test/x".into())],
            String::new(),
        ),
        "/to-local" => (302, vec![("Location".into(), "/landed".into())], String::new()),
        _ => (200, vec![], "landed".into()),
    })
    .await;
    for pooled in [false, true] {
        let mut c = ctx(Some(EgressPolicy::loopback()), pooled);
        let calls = hosts(&c, &[]);
        let before = hop.hits.load(Ordering::SeqCst);
        // Allowed, then refused: the first hop is made, the second is not.
        blocked(&mut c, &format!("http://127.0.0.1:{}/to-ip", hop.port)).await;
        let b = blocked(&mut c, &format!("http://127.0.0.1:{}/to-name", hop.port)).await;
        assert_eq!(b.target, "example.test");
        assert_eq!(hop.hits.load(Ordering::SeqCst) - before, 2);
        assert!(calls.lock().unwrap().is_empty());
        // A redirect inside the policy is followed.
        let r = reaches(&mut c, &format!("http://127.0.0.1:{}/to-local", hop.port)).await;
        assert_eq!((r.status.as_u16(), r.redirects.len()), (200, 1));
    }
    assert!(outside.untouched());
}

#[tokio::test]
async fn numeric_spellings_of_an_outside_address_are_refused() {
    let ip = outside_or_skip!();
    let IpAddr::V4(v4) = ip else {
        eprintln!("skipped: the default-route address is IPv6");
        return;
    };
    let l = Silent::bind(ip);
    let n = u32::from(v4);
    let o = v4.octets();
    let spellings = [
        format!("{n}"),
        format!("0x{n:x}"),
        format!("0{:o}.{}.{}.{}", o[0], o[1], o[2], o[3]),
        format!("[::ffff:{v4}]"),
        format!("[::ffff:{:02x}{:02x}:{:02x}{:02x}]", o[0], o[1], o[2], o[3]),
    ];
    let mut c = ctx(Some(EgressPolicy::loopback()), true);
    for sp in spellings {
        blocked(&mut c, &format!("http://{sp}:{}/", l.port())).await;
    }
    assert!(l.untouched());
}

#[tokio::test]
async fn open_mode_is_the_default_and_changes_nothing() {
    let s = site("127.0.0.1".parse().unwrap(), ok_site()).await;
    let mut c = ctx(None, true);
    assert!(c.egress.is_open());
    assert_eq!(reaches(&mut c, &format!("http://127.0.0.1:{}/", s.port)).await.status, 200);
}

// Proxies ------------------------------------------------------------------

struct ProxyProc {
    child: Child,
    http: u16,
    socks: u16,
    log: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Drop for ProxyProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl ProxyProc {
    fn start() -> ProxyProc {
        let script = format!("{}/tests/proxy/proxy.py", env!("CARGO_MANIFEST_DIR"));
        let mut child = Command::new("python3")
            .args([&script, "--http", "0", "--socks", "0"])
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

    fn entries(&self) -> Vec<serde_json::Value> {
        std::thread::sleep(Duration::from_millis(150));
        self.log.lock().unwrap().clone()
    }
}

fn via(c: &mut NetContext, proxy: &str) {
    c.proxy = Some(Arc::new(Proxy::parse(proxy, None).unwrap().unwrap()));
}

#[tokio::test]
async fn a_refused_target_never_reaches_the_proxy() {
    let p = ProxyProc::start();
    for pooled in [false, true] {
        for proxy in [
            format!("127.0.0.1:{}", p.http),
            format!("socks5://127.0.0.1:{}", p.socks),
        ] {
            let mut c = ctx(Some(EgressPolicy::loopback()), pooled);
            via(&mut c, &proxy);
            // `http:` (a forward request on the HTTP proxy), `https:` (a
            // CONNECT tunnel) and an outside IP literal.
            for url in ["http://example.test/", "https://example.test/", "http://10.1.2.3:8080/"] {
                blocked(&mut c, url).await;
            }
        }
    }
    assert!(p.entries().is_empty(), "the proxy saw {:?}", p.entries());
}

#[tokio::test]
async fn an_allowed_target_goes_through_the_proxy() {
    let p = ProxyProc::start();
    let s = site("127.0.0.1".parse().unwrap(), ok_site()).await;
    for proxy in [
        format!("127.0.0.1:{}", p.http),
        format!("socks5://127.0.0.1:{}", p.socks),
    ] {
        for pooled in [false, true] {
            let mut c = ctx(Some(loopback_with(&["*.lab.test"])), pooled);
            via(&mut c, &proxy);
            // The proxy resolves the name (it reaches 127.0.0.1 for every
            // target), so nothing is looked up here.
            let calls = hosts(&c, &[]);
            let r = reaches(&mut c, &format!("http://app.lab.test:{}/", s.port)).await;
            assert_eq!(r.status.as_u16(), 200, "{proxy}");
            assert!(calls.lock().unwrap().is_empty());
        }
    }
    let seen = p.entries();
    assert!(!seen.is_empty());
    assert!(
        seen.iter().any(|e| e.to_string().contains("app.lab.test")),
        "{seen:?}"
    );
    assert_eq!(s.hits.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn the_proxys_own_address_must_be_allowed() {
    let ip = outside_or_skip!();
    let l = Silent::bind(ip);
    for kind in ["", "socks5://"] {
        for pooled in [false, true] {
            let mut c = ctx(Some(loopback_with(&["*.lab.test"])), pooled);
            via(&mut c, &format!("{kind}{ip}:{}", l.port()));
            // The target is allowed (and proxied: it is not loopback); the
            // proxy's address is not.
            let b = blocked(&mut c, "http://app.lab.test/").await;
            assert_eq!(b.target, ip.to_string(), "{kind}");
        }
    }
    assert!(l.untouched(), "the proxy address was dialled");
}

#[tokio::test]
async fn an_allowed_proxy_address_is_dialled() {
    let ip = outside_or_skip!();
    let l = StdListener::bind(SocketAddr::new(ip, 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    std::thread::spawn(move || {
        for _ in l.incoming().flatten() {
            h.fetch_add(1, Ordering::SeqCst); // accept, say nothing, close
        }
    });
    let mut c = ctx(
        Some(loopback_with(&[&format!("{ip}/32"), "*.lab.test"])),
        true,
    );
    via(&mut c, &format!("{ip}:{port}"));
    let err = lb_net::fetch(&mut c, get("http://app.lab.test/"))
        .await
        .unwrap_err();
    assert!(!matches!(err, NetError::Blocked(_)), "{err}");
    assert!(hits.load(Ordering::SeqCst) >= 1);
}

// WebSocket ----------------------------------------------------------------

async fn ws_failure(c: &mut NetContext, url: &str) -> String {
    let (_tx, mut events) = c.websocket(WsConnect {
        url: url.parse().unwrap(),
        protocols: vec![],
        origin: "http://127.0.0.1".into(),
    });
    let mut failed = None;
    while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(10), events.recv()).await {
        match ev {
            WsEvent::Failed(m) => failed = Some(m),
            WsEvent::Closed { code, clean, .. } => {
                assert_eq!((code, clean), (1006, false));
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    failed.expect("the connection should have failed")
}

#[tokio::test]
async fn websocket_connections_are_refused_too() {
    let ip = outside_or_skip!();
    let l = Silent::bind(ip);
    // The text is Chrome's for a connection that cannot be established,
    // with the code of a blocked request.
    let want = "Error in connection establishment: net::ERR_BLOCKED_BY_CLIENT";
    for pooled in [false, true] {
        let mut c = ctx(Some(EgressPolicy::loopback()), pooled);
        let calls = hosts(&c, &[("example.test", ip)]);
        assert_eq!(ws_failure(&mut c, &format!("ws://{ip}:{}/", l.port())).await, want);
        assert_eq!(ws_failure(&mut c, &format!("wss://{ip}:{}/", l.port())).await, want);
        assert_eq!(ws_failure(&mut c, "ws://example.test/").await, want);
        assert!(calls.lock().unwrap().is_empty());
        // Through a proxy the target is checked as well.
        let p = ProxyProc::start();
        via(&mut c, &format!("127.0.0.1:{}", p.http));
        assert_eq!(ws_failure(&mut c, "ws://example.test/").await, want);
        assert!(p.entries().is_empty());
    }
    assert!(l.untouched());
}

#[tokio::test]
async fn websocket_to_loopback_is_not_refused() {
    // A server that answers 200 instead of 101: the handshake is attempted
    // (the server is reached) and fails for that reason, not the policy.
    let s = site("127.0.0.1".parse().unwrap(), ok_site()).await;
    let mut c = ctx(Some(EgressPolicy::loopback()), true);
    let m = ws_failure(&mut c, &format!("ws://127.0.0.1:{}/", s.port)).await;
    assert!(m.contains("Unexpected response code: 200"), "{m}");
    assert_eq!(s.hits.load(Ordering::SeqCst), 1);
}
