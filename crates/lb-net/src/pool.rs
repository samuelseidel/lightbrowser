//! Connection reuse as Chrome does it (ADR 0018).
//!
//! One [`ConnectionPool`] per browser context; contexts never share
//! connections or TLS sessions. Like Chrome's socket pools, connections are
//! grouped by scheme, host, port and network partition (Chrome's
//! `NetworkAnonymizationKey`: the top-level site and whether the frame is
//! cross-site to it). Per group:
//!
//! - **HTTP/1.1:** at most [`MAX_SOCKETS_PER_GROUP`] (6) sockets, busy,
//!   idle or connecting. A response that leaves the connection reusable
//!   (`crate::h1`) puts it back idle; the next request takes the most
//!   recently used idle socket, after checking that the peer has not closed
//!   it and that it has been idle less than [`USED_IDLE_TIMEOUT`] (300 s, as
//!   Chrome). Requests beyond the limit wait; a freed socket goes to the
//!   most urgent waiting request (urgency from the `priority` value, as in
//!   Chrome's priority-ordered pool), then the oldest.
//! - **HTTP/2:** one session carries every request of the group. While the
//!   protocol of a server is unknown, waiting requests each start a
//!   connection (up to 6, as Chrome does); the first that negotiates `h2`
//!   becomes the session, the others are closed after their handshake
//!   without sending anything. Once a server is known to speak h2, only
//!   one connection is started and the others wait for it (up to
//!   [`H2_THROTTLE`], Chrome's `kHTTP2ThrottleMs`). Sessions stay until the
//!   server closes them.
//! - A request that fails on a reused connection before any response byte
//!   arrived (the server closed the idle connection) is sent again once on
//!   a new one, as Chrome's `ShouldResendRequest`; likewise an HTTP/2
//!   request refused by `GOAWAY` or a closed session.
//! - TLS sessions are cached per group key (see `crate::tls::SessionCache`).
//!
//! Every pooled connection is driven by one network thread
//! ([`net_runtime`]), like Chrome's network service, so pages on different
//! worker threads share the pool. A context whose pages all run on one
//! thread (`lightbrowser fetch`: one page) can keep its connections on that
//! thread instead ([`ConnectionPool::use_calling_thread`]): no extra thread,
//! no extra allocator arena. Nothing is written to disk.
//!
//! Not modelled: connection coalescing across host names (HTTP/2 IP
//! pooling), privacy-mode groups (credentials `omit` requests share the
//! credentialed group), preconnects, and the browser-wide limit of 256
//! sockets is per context here.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use url::Url;

use crate::HttpVersion;
use crate::error::NetError;
use crate::header_profile::{RequestKind, RequestMeta};
use crate::profile::ClientHelloProfile;
use crate::tls::{SessionCache, SessionKey, TlsConnector};

/// Chrome's limit of sockets per group (`kMaxSocketsPerGroup`).
pub const MAX_SOCKETS_PER_GROUP: usize = 6;
/// Chrome's limit of sockets per pool (`kMaxSocketsPerPool`, 256). Above it
/// the oldest idle socket of any group is closed first.
pub const MAX_SOCKETS_PER_POOL: usize = 256;
/// How long a used socket may stay idle (Chrome:
/// `kUsedIdleSocketTimeout`, 300 s).
pub const USED_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// How long other requests wait for the one connection to a server known
/// to speak HTTP/2 before opening their own (`kHTTP2ThrottleMs`).
pub const H2_THROTTLE: Duration = Duration::from_millis(300);
/// Interval of the idle-socket sweep.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(10);
/// Time allowed for one TCP connect attempt (per resolved address).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The network thread's runtime. Started on first use; every pooled
/// connection and its HTTP/2 driver lives here.
pub fn net_runtime() -> &'static Handle {
    static RT: OnceLock<Handle> = OnceLock::new();
    RT.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("lb-net".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("network runtime");
                let _ = tx.send(rt.handle().clone());
                rt.block_on(std::future::pending::<()>());
            })
            .expect("network thread");
        rx.recv().expect("network runtime handle")
    })
}

/// DNS and TCP connect. The resolver is built on first use.
#[derive(Default)]
pub(crate) struct Dialer {
    resolver: OnceLock<Option<hickory_resolver::TokioResolver>>,
}

impl Dialer {
    /// Resolve `host` (no brackets) to socket addresses. IP literals skip
    /// DNS. Uses hickory with the system configuration and falls back to
    /// the platform resolver (`getaddrinfo`).
    pub(crate) async fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Vec<std::net::SocketAddr>, NetError> {
        use std::net::{IpAddr, SocketAddr};
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
        let resolver =
            self.resolver
                .get_or_init(|| match hickory_resolver::TokioResolver::builder_tokio() {
                    Ok(b) => Some(b.build()),
                    Err(e) => {
                        tracing::debug!("hickory resolver unavailable: {e}");
                        None
                    }
                });
        if let Some(r) = resolver {
            match r.lookup_ip(host).await {
                Ok(ips) => {
                    let addrs: Vec<_> = ips.iter().map(|ip| SocketAddr::new(ip, port)).collect();
                    if !addrs.is_empty() {
                        return Ok(addrs);
                    }
                }
                Err(e) => tracing::debug!("hickory lookup of {host} failed: {e}"),
            }
        }
        let addrs: Vec<_> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|e| NetError::Dns(format!("{host}: {e}")))?
            .collect();
        if addrs.is_empty() {
            return Err(NetError::Dns(format!("{host}: no addresses")));
        }
        Ok(addrs)
    }

    /// Resolve and connect, trying each address in turn.
    pub(crate) async fn connect(&self, host: &str, port: u16) -> Result<TcpStream, NetError> {
        let addrs = self.resolve(host, port).await?;
        let mut last = None;
        for addr in addrs {
            match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
                Ok(Ok(tcp)) => {
                    // Small writes (h2 preface, request headers) should not wait.
                    let _ = tcp.set_nodelay(true);
                    return Ok(tcp);
                }
                Ok(Err(e)) => last = Some(e),
                Err(_) => {
                    last = Some(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("connect to {addr} timed out"),
                    ));
                }
            }
        }
        Err(NetError::Connect(last.unwrap_or_else(|| {
            std::io::Error::other("no address to connect to")
        })))
    }
}

/// A connection: plain TCP or TLS over TCP.
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<tokio_boring::SslStream<TcpStream>>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(t) => t,
            Stream::Tls(s) => s.get_ref(),
        }
    }

    /// Chrome's `IsConnectedAndIdle`: nothing to read (no data, no EOF).
    fn is_connected_and_idle(&self) -> bool {
        let mut cx = Context::from_waker(std::task::Waker::noop());
        let mut b = [0u8; 1];
        let mut rb = ReadBuf::new(&mut b);
        matches!(self.tcp().poll_peek(&mut cx, &mut rb), Poll::Pending)
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_flush(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

/// A connection with its pool-wide id.
pub struct Conn {
    pub id: u64,
    pub stream: Stream,
}

/// A usable HTTP/2 session.
#[derive(Clone)]
pub(crate) struct H2Session {
    pub(crate) id: u64,
    pub(crate) send: ::h2::client::SendRequest<bytes::Bytes>,
    alive: Arc<AtomicBool>,
}

impl H2Session {
    fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
}

/// What a request got from the pool.
pub(crate) enum Lease {
    /// An HTTP/1.1 connection, new or idle (`reused`).
    H1 { conn: Conn, reused: bool },
    /// A new connection that negotiated h2: the holder starts the session
    /// with its own request, then [`ConnectionPool::publish`]es it.
    NewH2(Conn),
    /// A request on an existing session.
    H2(H2Session),
}

/// Connection group: Chrome's socket pool group id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupKey {
    pub https: bool,
    pub host: String,
    pub port: u16,
    /// Network partition: top-level site and `same_site`/`cross_site`, as
    /// Chrome's `NetworkAnonymizationKey` prints; empty when unknown.
    pub partition: String,
}

impl GroupKey {
    /// The group of a request for `url` described by `meta`.
    pub fn for_request(url: &Url, meta: &RequestMeta) -> Option<GroupKey> {
        let https = match url.scheme() {
            "https" => true,
            "http" => false,
            _ => return None,
        };
        Some(GroupKey {
            https,
            host: url.host_str()?.to_owned(),
            port: url.port_or_known_default()?,
            partition: partition(url, meta),
        })
    }

    fn session_key(&self) -> SessionKey {
        SessionKey {
            host: self.host.clone(),
            port: self.port,
            partition: self.partition.clone(),
        }
    }
}

/// Chrome's `NetworkAnonymizationKey` for a request: the top-level site and
/// whether the requesting frame (for a frame navigation: the frame's new
/// document) is cross-site to it. A top-level navigation's own site is the
/// top-level site, hop by hop.
fn partition(url: &Url, meta: &RequestMeta) -> String {
    // Schemeful site: the last two labels of a domain (the same rule as
    // `Sec-Fetch-Site`, no public-suffix list), or the IP literal.
    let site = |u: &Url| {
        let host = match u.host() {
            Some(url::Host::Domain(d)) => {
                let labels: Vec<&str> = d.split('.').collect();
                labels[labels.len().saturating_sub(2)..].join(".")
            }
            Some(h) => h.to_string(),
            None => String::new(),
        };
        format!("{}://{host}", u.scheme())
    };
    let (top, frame) = match meta.kind {
        RequestKind::Document => (site(url), site(url)),
        RequestKind::Iframe => {
            let Some(top) = meta.top_frame.as_ref().or(meta.initiator.as_ref()) else {
                return String::new();
            };
            (site(top), site(url))
        }
        _ => {
            let Some(frame) = meta.initiator.as_ref() else {
                return String::new();
            };
            let top = meta.top_frame.as_ref().unwrap_or(frame);
            (site(top), site(frame))
        }
    };
    let rel = if top == frame {
        "same_site"
    } else {
        "cross_site"
    };
    format!("{top} {rel}")
}

/// Request urgency from a `priority` header value (`u=N`, default 3), the
/// value Chrome derives both the header and the HTTP/2 weight from.
pub fn urgency(priority: Option<&str>) -> u8 {
    priority
        .and_then(|v| {
            v.split(',')
                .map(str::trim)
                .find_map(|p| p.strip_prefix("u="))
                .and_then(|n| n.parse::<u8>().ok())
        })
        .unwrap_or(3)
        .min(7)
}

struct Waiter {
    urgency: u8,
    seq: u64,
    tx: oneshot::Sender<Result<Lease, NetError>>,
}

struct Idle {
    conn: Conn,
    since: Instant,
}

#[derive(Default)]
enum H2State {
    #[default]
    None,
    /// A request is starting the session.
    Pending,
    Ready(H2Session),
}

#[derive(Default)]
struct Group {
    /// Idle HTTP/1.1 sockets, most recently used last.
    idle: Vec<Idle>,
    /// Sockets counted against the group limit: busy, idle and connecting
    /// HTTP/1.1 sockets and the HTTP/2 session.
    sockets: usize,
    connecting: usize,
    h2: H2State,
    /// The server negotiated h2 before (Chrome's `HttpServerProperties`).
    known_h2: bool,
    throttle_until: Option<Instant>,
    waiters: Vec<Waiter>,
}

#[derive(Default)]
struct State {
    groups: HashMap<GroupKey, Group>,
    total: usize,
    seq: u64,
    cleanup_running: bool,
}

/// How to open connections for a request: the context's TLS profile and
/// certificate policy.
#[derive(Clone)]
pub(crate) struct ConnectParams {
    pub(crate) profile: Arc<ClientHelloProfile>,
    pub(crate) insecure: bool,
}

struct Inner {
    state: Mutex<State>,
    sessions: Arc<SessionCache>,
    connector: Mutex<Option<(bool, Arc<TlsConnector>)>>,
    dialer: Dialer,
    next_id: AtomicU64,
    /// Requests run on the caller's runtime, not the network thread.
    inline: AtomicBool,
}

/// The connection pool of one browser context. Cheap to clone (shared).
#[derive(Clone)]
pub struct ConnectionPool {
    inner: Arc<Inner>,
}

impl Default for ConnectionPool {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ConnectionPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionPool")
            .field("opened", &self.opened())
            .finish()
    }
}

impl ConnectionPool {
    pub fn new() -> Self {
        ConnectionPool {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                sessions: Arc::new(SessionCache::default()),
                connector: Mutex::new(None),
                dialer: Dialer::default(),
                next_id: AtomicU64::new(1),
                inline: AtomicBool::new(false),
            }),
        }
    }

    /// Run this pool's requests and connections on the runtime that makes
    /// them instead of the network thread. Only for a context whose
    /// requests all come from one thread (its connections are bound to
    /// that thread's runtime); call it before the first request.
    pub fn use_calling_thread(&self) {
        self.inner.inline.store(true, Ordering::Relaxed);
    }

    /// See [`use_calling_thread`](Self::use_calling_thread).
    pub fn uses_calling_thread(&self) -> bool {
        self.inner.inline.load(Ordering::Relaxed)
    }

    /// Connections opened so far (TCP connects that succeeded).
    pub fn opened(&self) -> u64 {
        self.inner.next_id.load(Ordering::Relaxed) - 1
    }

    /// TLS sessions cached for `key` (tests).
    pub fn cached_sessions(&self, key: &GroupKey) -> usize {
        self.inner.sessions.count(&key.session_key())
    }

    /// Idle HTTP/1.1 sockets of `key` (tests).
    pub fn idle(&self, key: &GroupKey) -> usize {
        self.lock().groups.get(key).map_or(0, |g| g.idle.len())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn connector(&self, params: &ConnectParams) -> Result<Arc<TlsConnector>, NetError> {
        let mut c = self
            .inner
            .connector
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some((flag, conn)) = &*c
            && *flag == params.insecure
            && conn.profile().profile_id == params.profile.profile_id
        {
            return Ok(Arc::clone(conn));
        }
        let conn = Arc::new(
            TlsConnector::with_sessions(
                Arc::clone(&params.profile),
                Arc::clone(&self.inner.sessions),
            )?
            .with_danger_accept_invalid_certs(params.insecure),
        );
        *c = Some((params.insecure, Arc::clone(&conn)));
        Ok(conn)
    }

    /// A connection or session for a request of `urgency` (0 most urgent)
    /// in group `key`. Waits when the group is at its limit.
    pub(crate) async fn acquire(
        &self,
        key: &GroupKey,
        urgency: u8,
        params: &ConnectParams,
    ) -> Result<Lease, NetError> {
        let rx = {
            let mut st = self.lock();
            let now = Instant::now();
            let mut closed = 0;
            let g = st.groups.entry(key.clone()).or_default();
            let mut found = None;
            if let H2State::Ready(s) = &g.h2 {
                if s.alive() {
                    found = Some(Lease::H2(s.clone()));
                } else {
                    g.h2 = H2State::None;
                    g.sockets -= 1;
                    closed += 1;
                }
            }
            if found.is_none() {
                while let Some(idle) = g.idle.pop() {
                    if now.duration_since(idle.since) < USED_IDLE_TIMEOUT
                        && idle.conn.stream.is_connected_and_idle()
                    {
                        found = Some(Lease::H1 {
                            conn: idle.conn,
                            reused: true,
                        });
                        break;
                    }
                    g.sockets -= 1;
                    closed += 1;
                }
            }
            st.total -= closed;
            if let Some(lease) = found {
                return Ok(lease);
            }
            let (tx, rx) = oneshot::channel();
            st.seq += 1;
            let seq = st.seq;
            if let Some(g) = st.groups.get_mut(key) {
                g.waiters.push(Waiter { urgency, seq, tx });
            }
            self.start_connects(&mut st, key, params);
            rx
        };
        rx.await
            .unwrap_or_else(|_| Err(NetError::Http("connection pool closed".into())))
    }

    /// Start connects for waiting requests while the group allows.
    fn start_connects(&self, st: &mut State, key: &GroupKey, params: &ConnectParams) {
        let now = Instant::now();
        loop {
            let total = st.total;
            let Some(g) = st.groups.get_mut(key) else {
                return;
            };
            g.waiters.retain(|w| !w.tx.is_closed());
            if g.waiters.len() <= g.connecting
                || g.sockets >= MAX_SOCKETS_PER_GROUP
                || !matches!(g.h2, H2State::None)
            {
                return;
            }
            if g.known_h2 && g.connecting > 0 && g.throttle_until.is_some_and(|t| now < t) {
                return;
            }
            if total >= MAX_SOCKETS_PER_POOL && !close_oldest_idle(st) {
                return;
            }
            let Some(g) = st.groups.get_mut(key) else {
                return;
            };
            g.sockets += 1;
            g.connecting += 1;
            if g.known_h2 && g.connecting == 1 {
                g.throttle_until = Some(now + H2_THROTTLE);
                let (pool, key, params) = (self.clone(), key.clone(), params.clone());
                tokio::spawn(async move {
                    tokio::time::sleep(H2_THROTTLE).await;
                    let mut st = pool.lock();
                    pool.start_connects(&mut st, &key, &params);
                });
            }
            st.total += 1;
            let (pool, key, params) = (self.clone(), key.clone(), params.clone());
            tokio::spawn(async move {
                let r = pool.open(&key, &params).await;
                pool.offer(&key, r, &params);
            });
        }
    }

    /// DNS, TCP and (https) the TLS handshake for `key`.
    async fn open(
        &self,
        key: &GroupKey,
        params: &ConnectParams,
    ) -> Result<(Conn, HttpVersion), NetError> {
        let host = key.host.trim_start_matches('[').trim_end_matches(']');
        let connector = if key.https {
            Some(self.connector(params)?)
        } else {
            None
        };
        let tcp = self.inner.dialer.connect(host, key.port).await?;
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        match connector {
            None => Ok((
                Conn {
                    id,
                    stream: Stream::Plain(tcp),
                },
                HttpVersion::Http1,
            )),
            Some(c) => {
                let (s, version) = c
                    .connect_resuming(host, tcp, Some(&key.session_key()))
                    .await?;
                Ok((
                    Conn {
                        id,
                        stream: Stream::Tls(Box::new(s)),
                    },
                    version,
                ))
            }
        }
    }

    /// A connect finished: hand the connection to a waiting request.
    fn offer(
        &self,
        key: &GroupKey,
        result: Result<(Conn, HttpVersion), NetError>,
        params: &ConnectParams,
    ) {
        let mut st = self.lock();
        let Some(g) = st.groups.get_mut(key) else {
            return;
        };
        g.connecting -= 1;
        let mut closed = 0;
        match result {
            Err(e) => {
                g.sockets -= 1;
                closed += 1;
                let _ = grant(g, Err(e));
            }
            Ok((conn, HttpVersion::Http2)) => {
                g.known_h2 = true;
                if matches!(g.h2, H2State::None) {
                    g.h2 = H2State::Pending;
                    if grant(g, Ok(Lease::NewH2(conn))).is_err() {
                        g.h2 = H2State::None;
                        g.sockets -= 1;
                        closed += 1;
                    }
                } else {
                    // Chrome closes the spare connection once a session
                    // exists, before sending anything on it.
                    drop(conn);
                    g.sockets -= 1;
                    closed += 1;
                }
            }
            Ok((conn, HttpVersion::Http1)) => {
                if let Err(Ok(Lease::H1 { conn, .. })) = grant(
                    g,
                    Ok(Lease::H1 {
                        conn,
                        reused: false,
                    }),
                ) {
                    g.idle.push(Idle {
                        conn,
                        since: Instant::now(),
                    });
                    self.ensure_cleanup(&mut st);
                }
            }
        }
        st.total -= closed;
        self.start_connects(&mut st, key, params);
    }

    /// The HTTP/1.1 request on `conn` is done; keep it idle if `reusable`.
    pub(crate) fn release(
        &self,
        key: &GroupKey,
        conn: Conn,
        reusable: bool,
        params: &ConnectParams,
    ) {
        let mut st = self.lock();
        let Some(g) = st.groups.get_mut(key) else {
            return;
        };
        if reusable {
            if let Err(Ok(Lease::H1 { conn, .. })) = grant(g, Ok(Lease::H1 { conn, reused: true }))
            {
                g.idle.push(Idle {
                    conn,
                    since: Instant::now(),
                });
                self.ensure_cleanup(&mut st);
            }
            return;
        }
        drop(conn);
        g.sockets -= 1;
        st.total -= 1;
        self.start_connects(&mut st, key, params);
    }

    /// A busy socket was closed (error) or a session start failed.
    pub(crate) fn discard(&self, key: &GroupKey, session_start: bool, params: &ConnectParams) {
        let mut st = self.lock();
        let Some(g) = st.groups.get_mut(key) else {
            return;
        };
        if session_start {
            g.h2 = H2State::None;
        }
        g.sockets -= 1;
        st.total -= 1;
        self.start_connects(&mut st, key, params);
    }

    /// The session started by a [`Lease::NewH2`] holder is ready: every
    /// waiting request of the group uses it. The pool watches its driver.
    pub(crate) fn publish(
        &self,
        key: &GroupKey,
        id: u64,
        send: ::h2::client::SendRequest<bytes::Bytes>,
        driver: impl Future<Output = ()> + Send + 'static,
    ) {
        let alive = Arc::new(AtomicBool::new(true));
        let session = H2Session {
            id,
            send,
            alive: Arc::clone(&alive),
        };
        let weak: Weak<Inner> = Arc::downgrade(&self.inner);
        let k = key.clone();
        tokio::spawn(async move {
            driver.await;
            alive.store(false, Ordering::Release);
            if let Some(inner) = weak.upgrade() {
                ConnectionPool { inner }.session_closed(&k, id);
            }
        });
        let mut st = self.lock();
        let Some(g) = st.groups.get_mut(key) else {
            return;
        };
        g.h2 = H2State::Ready(session.clone());
        // In request order, as Chrome resumes the requests that waited.
        g.waiters.sort_by_key(|w| w.seq);
        for w in g.waiters.drain(..) {
            let _ = w.tx.send(Ok(Lease::H2(session.clone())));
        }
    }

    /// Session `id` of `key` ended (closed by the server, `GOAWAY`, error).
    pub(crate) fn session_closed(&self, key: &GroupKey, id: u64) {
        let mut st = self.lock();
        let Some(g) = st.groups.get_mut(key) else {
            return;
        };
        if let H2State::Ready(s) = &g.h2
            && s.id == id
        {
            s.alive.store(false, Ordering::Release);
            g.h2 = H2State::None;
            g.sockets -= 1;
            st.total -= 1;
        }
    }

    /// Sweep idle sockets every 10 s while there are any (Chrome's
    /// `CleanupIdleSockets` timer).
    fn ensure_cleanup(&self, st: &mut State) {
        if st.cleanup_running {
            return;
        }
        st.cleanup_running = true;
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(CLEANUP_INTERVAL).await;
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let pool = ConnectionPool { inner };
                let mut st = pool.lock();
                let now = Instant::now();
                let mut closed = 0;
                let mut left = 0;
                for g in st.groups.values_mut() {
                    let before = g.idle.len();
                    g.idle.retain(|i| {
                        now.duration_since(i.since) < USED_IDLE_TIMEOUT
                            && i.conn.stream.is_connected_and_idle()
                    });
                    let gone = before - g.idle.len();
                    g.sockets -= gone;
                    closed += gone;
                    left += g.idle.len();
                }
                st.total -= closed;
                if left == 0 {
                    st.cleanup_running = false;
                    return;
                }
            }
        });
    }
}

/// Give `lease` to the most urgent (then oldest) waiting request. Returns
/// it back when nobody waits.
fn grant(g: &mut Group, mut lease: Result<Lease, NetError>) -> Result<(), Result<Lease, NetError>> {
    loop {
        let Some(i) = g
            .waiters
            .iter()
            .enumerate()
            .filter(|(_, w)| !w.tx.is_closed())
            .min_by_key(|(_, w)| (w.urgency, w.seq))
            .map(|(i, _)| i)
        else {
            g.waiters.clear();
            return Err(lease);
        };
        let w = g.waiters.remove(i);
        match w.tx.send(lease) {
            Ok(()) => return Ok(()),
            Err(back) => lease = back,
        }
    }
}

/// Close the oldest idle socket of any group (pool at its socket limit).
fn close_oldest_idle(st: &mut State) -> bool {
    let oldest = st
        .groups
        .iter()
        .filter_map(|(k, g)| g.idle.first().map(|i| (i.since, k.clone())))
        .min_by_key(|(since, _)| *since);
    let Some((_, key)) = oldest else {
        return false;
    };
    if let Some(g) = st.groups.get_mut(&key) {
        g.idle.remove(0);
        g.sockets -= 1;
        st.total -= 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn urgency_from_priority_header() {
        assert_eq!(urgency(Some("u=0, i")), 0);
        assert_eq!(urgency(Some("u=2")), 2);
        assert_eq!(urgency(Some("i")), 3);
        assert_eq!(urgency(None), 3);
        assert_eq!(urgency(Some("u=9")), 7);
    }

    #[test]
    fn partitions_follow_the_top_level_site() {
        let top = url("http://127.0.0.1:8000/");
        let nav = RequestMeta::navigation();
        assert_eq!(partition(&top, &nav), "http://127.0.0.1 same_site");
        // A cross-site frame of that page.
        let frame = url("http://fptest.localhost:8000/next");
        let mut m = RequestMeta::new(RequestKind::Iframe, Some(top.clone()));
        assert_eq!(partition(&frame, &m), "http://127.0.0.1 cross_site");
        // The same URL as a top-level navigation is another group.
        assert_eq!(partition(&frame, &nav), "http://fptest.localhost same_site");
        // A subresource of the cross-site frame.
        m = RequestMeta::new(RequestKind::Image, Some(frame.clone()));
        m.top_frame = Some(top.clone());
        assert_eq!(
            partition(&url("http://127.0.0.1:8000/i.gif"), &m),
            "http://127.0.0.1 cross_site"
        );
        // A subresource of the top-level page.
        let m = RequestMeta::new(RequestKind::Image, Some(top.clone()));
        let k = GroupKey::for_request(&url("https://cdn.test/x.gif"), &m).unwrap();
        assert_eq!(
            k,
            GroupKey {
                https: true,
                host: "cdn.test".into(),
                port: 443,
                partition: "http://127.0.0.1 same_site".into(),
            }
        );
    }
}
