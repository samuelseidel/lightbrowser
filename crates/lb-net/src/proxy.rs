//! Proxies per browser context, as Chrome 153 uses them (ADR 0027).
//!
//! - **HTTP proxy, `https:`/`wss:`/`ws:`**: a `CONNECT host:port` tunnel,
//!   then TLS (or the WebSocket handshake) inside it. The ClientHello and
//!   the HTTP/2 settings are the direct ones: the tunnel is only TCP.
//! - **HTTP proxy, `http:`**: forward requests in absolute form
//!   (`GET http://host/path HTTP/1.1`); `Connection` becomes
//!   `Proxy-Connection` (see [`forward_headers`]).
//! - **SOCKS5** (RFC 1928): every scheme is tunnelled; the target host name
//!   goes to the proxy (remote DNS, address type 3), as Chrome sends it.
//!   Username/password (RFC 1929) when credentials are configured (Chrome
//!   has no SOCKS authentication; without credentials we offer only "no
//!   authentication", as Chrome does).
//!
//! Recorded from Chrome for Testing 153.0.8010.12 through `tests/proxy/
//! proxy.py` on loopback (`tests/proxy/chrome.jsonl`): `CONNECT` carries
//! `Host`, `Proxy-Connection: keep-alive`, `User-Agent`, then
//! `Proxy-Authorization` once credentials are known. The first request
//! goes without credentials; a `407` with `Proxy-Authenticate: Basic`
//! gets the credentials (configured here, or from CDP `Fetch.authRequired`
//! in `lb-page`) and the request is sent again; after that every request
//! through the proxy carries them up front (Chrome's auth cache).
//!
//! Bypass rules follow Chrome's `ProxyBypassRules`: `,`/`;` separated host
//! patterns with `*` wildcards (a leading `.` means `*.`), optional
//! `scheme://` and `:port`, IP ranges in CIDR form, `<local>` (names without
//! a dot), `*`. Loopback and link-local destinations (`localhost`,
//! `*.localhost`, `127.0.0.0/8`, `::1`, `169.254.0.0/16`, `fe80::/10`) are
//! never proxied unless the list has `<-loopback>`.

use std::net::IpAddr;
use std::sync::Mutex;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use url::Url;

use crate::error::NetError;
use crate::headers::OrderedHeaders;
use crate::pool::Dialer;

/// Proxy protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyKind {
    Http,
    Socks5,
}

/// Username and password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl Credentials {
    /// `Basic` value for `Proxy-Authorization`.
    pub fn basic(&self) -> String {
        format!(
            "Basic {}",
            base64(format!("{}:{}", self.username, self.password).as_bytes())
        )
    }
}

/// A `407` the client could not answer: the caller asks for credentials
/// (CDP `Fetch.authRequired`), stores them with
/// [`Proxy::provide_credentials`] and sends the request again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyChallenge {
    /// `host:port` of the proxy.
    pub origin: String,
    /// Auth scheme (`basic`), lowercase as Chrome reports it.
    pub scheme: String,
    pub realm: String,
}

/// The proxy of a browser context.
#[derive(Debug)]
pub struct Proxy {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    bypass: Vec<Rule>,
    /// `<-loopback>` given: loopback destinations go through the proxy.
    proxy_loopback: bool,
    /// Credentials given with the configuration (`user:pass@` in the
    /// proxy URL): the answer to a challenge, not sent before one.
    configured: Option<Credentials>,
    /// Credentials that answered a challenge: sent up front from then on.
    cache: Mutex<Option<Credentials>>,
}

#[derive(Debug, Clone)]
enum Rule {
    Local,
    Host {
        scheme: Option<String>,
        pattern: String,
        port: Option<u16>,
    },
    Cidr {
        net: IpAddr,
        bits: u8,
    },
}

impl Proxy {
    /// Parse Chrome's `proxyServer` value (`[scheme://]host[:port]`, also
    /// `http=...;https=...` lists, of which the `https` entry, else the
    /// first, is used) and `proxyBypassList`. `direct://` or an empty
    /// value means no proxy (`Ok(None)`). Credentials may be given as
    /// `scheme://user:pass@host:port` (an LightBrowser extension: Chrome
    /// takes them from the auth handler only).
    pub fn parse(server: &str, bypass: Option<&str>) -> Result<Option<Proxy>, NetError> {
        let server = server.trim();
        if server.is_empty() {
            return Ok(None);
        }
        let entries: Vec<&str> = server
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        let pick = entries
            .iter()
            .find_map(|e| e.strip_prefix("https="))
            .or_else(|| {
                entries
                    .first()
                    .map(|e| e.split_once('=').map_or(*e, |(_, v)| v))
            })
            .unwrap_or("");
        // A list of fallbacks: the first one is used.
        let uri = pick.split(',').next().unwrap_or("").trim();
        if uri.is_empty() || uri.eq_ignore_ascii_case("direct://") {
            return Ok(None);
        }
        let with_scheme = if uri.contains("://") {
            uri.to_owned()
        } else {
            format!("http://{uri}")
        };
        let bad = || NetError::Proxy(format!("net::ERR_INVALID_ARGUMENT: proxy {uri}"));
        let url = Url::parse(&with_scheme).map_err(|_| bad())?;
        let kind = match url.scheme() {
            "http" => ProxyKind::Http,
            "socks5" => ProxyKind::Socks5,
            other => {
                return Err(NetError::Proxy(format!(
                    "net::ERR_NOT_IMPLEMENTED: {other} proxies are not supported"
                )));
            }
        };
        let host = url.host_str().ok_or_else(bad)?.to_owned();
        let port = url.port().unwrap_or(match kind {
            ProxyKind::Http => 80,
            ProxyKind::Socks5 => 1080,
        });
        let configured = (!url.username().is_empty()).then(|| Credentials {
            username: pct_decode(url.username()),
            password: pct_decode(url.password().unwrap_or("")),
        });
        let mut proxy_loopback = false;
        let mut rules = Vec::new();
        for raw in bypass.unwrap_or("").split([',', ';']) {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            if raw == "<-loopback>" {
                proxy_loopback = true;
            } else if raw == "<local>" {
                rules.push(Rule::Local);
            } else if let Some(r) = parse_rule(raw) {
                rules.push(r);
            }
        }
        Ok(Some(Proxy {
            kind,
            host,
            port,
            bypass: rules,
            proxy_loopback,
            configured,
            cache: Mutex::new(None),
        }))
    }

    /// Whether requests to `url` go through the proxy.
    pub fn applies_to(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let ip = bare.parse::<IpAddr>().ok();
        if !self.proxy_loopback && implicitly_bypassed(bare, ip) {
            return false;
        }
        let port = url.port_or_known_default();
        !self.bypass.iter().any(|r| match r {
            Rule::Local => ip.is_none() && !bare.contains('.'),
            Rule::Cidr { net, bits } => ip.is_some_and(|ip| in_cidr(ip, *net, *bits)),
            Rule::Host {
                scheme,
                pattern,
                port: p,
            } => {
                scheme.as_deref().is_none_or(|s| s == url.scheme())
                    && p.is_none_or(|p| Some(p) == port)
                    && wildcard(pattern, &host.to_ascii_lowercase())
            }
        })
    }

    /// Whether `url` is sent to the proxy as a forward request (HTTP proxy,
    /// `http:`) rather than through a tunnel.
    pub fn forwards(&self, url: &Url) -> bool {
        self.kind == ProxyKind::Http && url.scheme() == "http"
    }

    /// `host:port` of the proxy, as Chrome names it in auth challenges.
    pub fn origin(&self) -> String {
        let h = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("{h}:{}", self.port)
    }

    /// Store credentials for the proxy (an answer to a challenge): every
    /// later request through it carries them.
    pub fn provide_credentials(&self, c: Credentials) {
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = Some(c);
    }

    /// Forget learned credentials (a challenge was cancelled or failed).
    pub fn clear_credentials(&self) {
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// The credentials sent up front, if a challenge was answered.
    pub fn cached_credentials(&self) -> Option<Credentials> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// TCP to the proxy, then a tunnel to `host:port` (`CONNECT` or
    /// SOCKS5). `user_agent` goes into `CONNECT` as Chrome sends it.
    pub(crate) async fn tunnel(
        &self,
        dialer: &Dialer,
        host: &str,
        port: u16,
        user_agent: &str,
    ) -> Result<TcpStream, NetError> {
        match self.kind {
            ProxyKind::Http => self.connect_tunnel(dialer, host, port, user_agent).await,
            ProxyKind::Socks5 => self.socks5(dialer, host, port).await,
        }
    }

    /// TCP to the proxy itself (forward requests). This hop always uses the
    /// host's own stack: the destination sees the proxy's SYN, not ours, so
    /// a TCP profile does not apply (open question T13 in
    /// docs/tcp-handshake/REMAINING-STEPS.md).
    pub(crate) async fn dial(&self, dialer: &Dialer) -> Result<TcpStream, NetError> {
        let h = self.host.trim_start_matches('[').trim_end_matches(']');
        dialer
            .connect(h, self.port, None)
            .await
            .map_err(|_| NetError::Proxy("net::ERR_PROXY_CONNECTION_FAILED".into()))
    }

    async fn connect_tunnel(
        &self,
        dialer: &Dialer,
        host: &str,
        port: u16,
        user_agent: &str,
    ) -> Result<TcpStream, NetError> {
        let target = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        let mut sent_auth = false;
        let mut tcp = self.dial(dialer).await?;
        loop {
            let creds = self.cached_credentials();
            let mut head = format!(
                "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nProxy-Connection: keep-alive\r\nUser-Agent: {user_agent}\r\n"
            );
            if let Some(c) = &creds {
                head.push_str(&format!("Proxy-Authorization: {}\r\n", c.basic()));
            }
            head.push_str("\r\n");
            tcp.write_all(head.as_bytes())
                .await
                .map_err(|_| tunnel_failed())?;
            let resp = read_head(&mut tcp).await?;
            match resp.status {
                200..=299 => return Ok(tcp),
                407 => {
                    let challenge = ProxyChallenge {
                        origin: self.origin(),
                        scheme: resp.auth_scheme.clone().unwrap_or_else(|| "basic".into()),
                        realm: resp.realm.clone().unwrap_or_default(),
                    };
                    if challenge.scheme != "basic" {
                        return Err(NetError::Proxy("net::ERR_PROXY_AUTH_UNSUPPORTED".into()));
                    }
                    if creds.is_some() && sent_auth {
                        // Rejected credentials: ask again.
                        self.clear_credentials();
                        return Err(NetError::ProxyAuth(challenge));
                    }
                    let next = if creds.is_some() {
                        creds
                    } else {
                        self.configured.clone()
                    };
                    let Some(next) = next else {
                        return Err(NetError::ProxyAuth(challenge));
                    };
                    self.provide_credentials(next);
                    sent_auth = true;
                    // Chrome answers on the same connection when the 407
                    // left it reusable, else on a new one.
                    let mut body = resp.content_length.unwrap_or(0);
                    let mut reusable = resp.keep_alive && resp.content_length.is_some();
                    while body > 0 && reusable {
                        let mut buf = [0u8; 4096];
                        let n = tcp
                            .read(&mut buf[..body.min(4096)])
                            .await
                            .map_err(|_| tunnel_failed())?;
                        if n == 0 {
                            reusable = false;
                        }
                        body -= n;
                    }
                    if !reusable {
                        tcp = self.dial(dialer).await?;
                    }
                }
                _ => return Err(tunnel_failed()),
            }
        }
    }

    async fn socks5(&self, dialer: &Dialer, host: &str, port: u16) -> Result<TcpStream, NetError> {
        let fail = || NetError::Proxy("net::ERR_SOCKS_CONNECTION_FAILED".into());
        let creds = self
            .cached_credentials()
            .or_else(|| self.configured.clone());
        let mut tcp = self.dial(dialer).await?;
        let greeting: &[u8] = if creds.is_some() {
            &[5, 2, 0, 2]
        } else {
            &[5, 1, 0]
        };
        tcp.write_all(greeting).await.map_err(|_| fail())?;
        let mut r = [0u8; 2];
        tcp.read_exact(&mut r).await.map_err(|_| fail())?;
        match (r[0], r[1], &creds) {
            (5, 0, _) => {}
            (5, 2, Some(c)) => {
                let (u, p) = (c.username.as_bytes(), c.password.as_bytes());
                if u.len() > 255 || p.len() > 255 {
                    return Err(fail());
                }
                let mut m = vec![1, u.len() as u8];
                m.extend_from_slice(u);
                m.push(p.len() as u8);
                m.extend_from_slice(p);
                tcp.write_all(&m).await.map_err(|_| fail())?;
                tcp.read_exact(&mut r).await.map_err(|_| fail())?;
                if r[1] != 0 {
                    return Err(NetError::Proxy(
                        "net::ERR_PROXY_AUTH_UNSUPPORTED: SOCKS5 credentials rejected".into(),
                    ));
                }
            }
            _ => return Err(fail()),
        }
        // CONNECT with the host name (remote DNS), or the IP literal.
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let mut req = vec![5, 1, 0];
        match bare.parse::<IpAddr>() {
            Ok(IpAddr::V4(a)) => {
                req.push(1);
                req.extend_from_slice(&a.octets());
            }
            Ok(IpAddr::V6(a)) => {
                req.push(4);
                req.extend_from_slice(&a.octets());
            }
            Err(_) => {
                if bare.len() > 255 {
                    return Err(fail());
                }
                req.push(3);
                req.push(bare.len() as u8);
                req.extend_from_slice(bare.as_bytes());
            }
        }
        req.extend_from_slice(&port.to_be_bytes());
        tcp.write_all(&req).await.map_err(|_| fail())?;
        let mut h = [0u8; 4];
        tcp.read_exact(&mut h).await.map_err(|_| fail())?;
        if h[0] != 5 || h[1] != 0 {
            return Err(fail());
        }
        let skip = match h[3] {
            1 => 4,
            4 => 16,
            3 => {
                let mut l = [0u8; 1];
                tcp.read_exact(&mut l).await.map_err(|_| fail())?;
                usize::from(l[0])
            }
            _ => return Err(fail()),
        } + 2;
        let mut rest = vec![0u8; skip];
        tcp.read_exact(&mut rest).await.map_err(|_| fail())?;
        Ok(tcp)
    }
}

/// Headers of a forward request (HTTP proxy, `http:` URL), as Chrome
/// writes them: `Connection` is renamed `Proxy-Connection` in place, and
/// known credentials go in as `Proxy-Authorization` after it and after
/// `Pragma`/`Cache-Control` when those follow.
pub(crate) fn forward_headers(mut headers: OrderedHeaders, proxy: &Proxy) -> OrderedHeaders {
    let mut at = None;
    for (i, (n, _)) in headers.iter_mut().enumerate() {
        if n.eq_ignore_ascii_case("connection") {
            *n = "Proxy-Connection".into();
            at = Some(i + 1);
            break;
        }
    }
    if let Some(c) = proxy.cached_credentials() {
        let mut i = at.unwrap_or_else(|| {
            headers
                .iter()
                .position(|(n, _)| n.eq_ignore_ascii_case("host"))
                .map_or(0, |i| i + 1)
        });
        while i < headers.len()
            && (headers[i].0.eq_ignore_ascii_case("pragma")
                || headers[i].0.eq_ignore_ascii_case("cache-control"))
        {
            i += 1;
        }
        headers.insert(i, ("Proxy-Authorization".into(), c.basic()));
    }
    headers
}

/// A forward request's `407`: the challenge, or `None` when it is not one
/// we can answer. Stores the configured credentials when the request went
/// without any, so the caller sends it again.
pub(crate) fn forward_challenge(
    proxy: &Proxy,
    headers: &OrderedHeaders,
    sent_credentials: bool,
) -> Result<bool, NetError> {
    let value = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("proxy-authenticate"))
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    let (scheme, realm) = parse_challenge(value);
    let challenge = ProxyChallenge {
        origin: proxy.origin(),
        scheme: scheme.unwrap_or_else(|| "basic".into()),
        realm: realm.unwrap_or_default(),
    };
    if challenge.scheme != "basic" {
        return Err(NetError::Proxy("net::ERR_PROXY_AUTH_UNSUPPORTED".into()));
    }
    if sent_credentials {
        proxy.clear_credentials();
        return Err(NetError::ProxyAuth(challenge));
    }
    match proxy.configured.clone() {
        Some(c) => {
            proxy.provide_credentials(c);
            Ok(true)
        }
        None => Err(NetError::ProxyAuth(challenge)),
    }
}

fn tunnel_failed() -> NetError {
    NetError::Proxy("net::ERR_TUNNEL_CONNECTION_FAILED".into())
}

struct Head {
    status: u16,
    content_length: Option<usize>,
    keep_alive: bool,
    auth_scheme: Option<String>,
    realm: Option<String>,
}

/// Read a response head byte by byte, so nothing after it is consumed
/// (the TLS handshake follows on the same stream).
async fn read_head(tcp: &mut TcpStream) -> Result<Head, NetError> {
    let mut buf = Vec::with_capacity(256);
    let mut b = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if buf.len() > 64 * 1024 {
            return Err(tunnel_failed());
        }
        let n = tcp.read(&mut b).await.map_err(|_| tunnel_failed())?;
        if n == 0 {
            return Err(tunnel_failed());
        }
        buf.push(b[0]);
    }
    let mut hs = [httparse::EMPTY_HEADER; 64];
    let mut r = httparse::Response::new(&mut hs);
    match r.parse(&buf) {
        Ok(httparse::Status::Complete(_)) => {}
        _ => return Err(tunnel_failed()),
    }
    let mut head = Head {
        status: r.code.unwrap_or(0),
        content_length: None,
        keep_alive: r.version == Some(1),
        auth_scheme: None,
        realm: None,
    };
    for h in r.headers.iter() {
        let v = String::from_utf8_lossy(h.value).into_owned();
        if h.name.eq_ignore_ascii_case("content-length") {
            head.content_length = v.trim().parse().ok();
        } else if h.name.eq_ignore_ascii_case("connection")
            || h.name.eq_ignore_ascii_case("proxy-connection")
        {
            let l = v.to_ascii_lowercase();
            if l.contains("close") {
                head.keep_alive = false;
            } else if l.contains("keep-alive") {
                head.keep_alive = true;
            }
        } else if h.name.eq_ignore_ascii_case("proxy-authenticate") && head.auth_scheme.is_none() {
            let (s, realm) = parse_challenge(&v);
            head.auth_scheme = s;
            head.realm = realm;
        }
    }
    Ok(head)
}

/// `Basic realm="x"` → (`basic`, `x`).
fn parse_challenge(v: &str) -> (Option<String>, Option<String>) {
    let v = v.trim();
    if v.is_empty() {
        return (None, None);
    }
    let (scheme, rest) = v.split_once(' ').unwrap_or((v, ""));
    let realm = rest.split(',').find_map(|p| {
        let (k, val) = p.trim().split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("realm")
            .then(|| val.trim().trim_matches('"').to_owned())
    });
    (Some(scheme.to_ascii_lowercase()), realm)
}

fn parse_rule(raw: &str) -> Option<Rule> {
    let raw = raw.to_ascii_lowercase();
    let (scheme, rest) = match raw.split_once("://") {
        Some((s, r)) => (Some(s.to_owned()), r.to_owned()),
        None => (None, raw.clone()),
    };
    // CIDR: an IP literal with a prefix length.
    if let Some((addr, bits)) = rest.split_once('/')
        && let Ok(bits) = bits.parse::<u8>()
        && let Ok(net) = addr
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
    {
        return Some(Rule::Cidr { net, bits });
    }
    // host[:port], [v6][:port]
    let (host, port) = if let Some(end) = rest.strip_prefix('[').and_then(|r| r.find(']')) {
        let host = &rest[..end + 2];
        let port = rest[end + 2..]
            .strip_prefix(':')
            .and_then(|p| p.parse().ok());
        (host.to_owned(), port)
    } else {
        match rest.rsplit_once(':') {
            Some((h, p)) if p.parse::<u16>().is_ok() => (h.to_owned(), p.parse().ok()),
            _ => (rest.clone(), None),
        }
    };
    let pattern = if host.starts_with('.') {
        format!("*{host}")
    } else {
        host
    };
    if pattern.is_empty() {
        return None;
    }
    Some(Rule::Host {
        scheme,
        pattern,
        port,
    })
}

/// Chrome's implicit bypass (`ProxyBypassRules::MatchesImplicitRules`).
fn implicitly_bypassed(host: &str, ip: Option<IpAddr>) -> bool {
    let h = host.to_ascii_lowercase();
    if h == "localhost" || h.ends_with(".localhost") || h == "localhost6" {
        return true;
    }
    match ip {
        Some(IpAddr::V4(a)) => a.is_loopback() || a.is_link_local(),
        Some(IpAddr::V6(a)) => {
            a.is_loopback()
                || (a.segments()[0] & 0xffc0) == 0xfe80
                || a.to_ipv4_mapped().is_some_and(|v| v.is_loopback())
        }
        None => false,
    }
}

fn in_cidr(ip: IpAddr, net: IpAddr, bits: u8) -> bool {
    let (a, b, len): (Vec<u8>, Vec<u8>, u8) = match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(b)) => (a.octets().to_vec(), b.octets().to_vec(), 32),
        (IpAddr::V6(a), IpAddr::V6(b)) => (a.octets().to_vec(), b.octets().to_vec(), 128),
        (IpAddr::V6(a), IpAddr::V4(b)) => match a.to_ipv4_mapped() {
            Some(a) => (a.octets().to_vec(), b.octets().to_vec(), 32),
            None => return false,
        },
        _ => return false,
    };
    let bits = bits.min(len) as usize;
    for i in 0..bits {
        let (x, y) = (a[i / 8] >> (7 - i % 8) & 1, b[i / 8] >> (7 - i % 8) & 1);
        if x != y {
            return false;
        }
    }
    true
}

/// `*` matches any run of characters (Chrome's `base::MatchPattern`; `?`
/// matches one).
fn wildcard(p: &str, s: &str) -> bool {
    let (p, s) = (p.as_bytes(), s.as_bytes());
    let (mut pi, mut si, mut star, mut mark) = (0, 0, None, 0);
    while si < s.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = std::str::from_utf8(&b[i + 1..i + 3])
                .map_err(|_| ())
                .and_then(|h| u8::from_str_radix(h, 16).map_err(|_| ()))
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn parse_servers() {
        let p = Proxy::parse("127.0.0.1:3128", None).unwrap().unwrap();
        assert_eq!(
            (p.kind, p.host.as_str(), p.port),
            (ProxyKind::Http, "127.0.0.1", 3128)
        );
        let p = Proxy::parse("socks5://h", None).unwrap().unwrap();
        assert_eq!((p.kind, p.port), (ProxyKind::Socks5, 1080));
        let p = Proxy::parse("http=a:1;https=b:2", None).unwrap().unwrap();
        assert_eq!(p.host, "b");
        let p = Proxy::parse("http://u%40x:p%3A@h:8", None)
            .unwrap()
            .unwrap();
        assert_eq!(
            p.configured,
            Some(Credentials {
                username: "u@x".into(),
                password: "p:".into()
            })
        );
        assert!(Proxy::parse("direct://", None).unwrap().is_none());
        assert!(Proxy::parse("", None).unwrap().is_none());
        assert!(Proxy::parse("socks4://h:1", None).is_err());
        assert_eq!(p.origin(), "h:8");
    }

    #[test]
    fn bypass_rules() {
        let p = Proxy::parse(
            "h:1",
            Some("*.example.com, .b.org;c.net:8080,<local>,10.0.0.0/8, https://s.io"),
        )
        .unwrap()
        .unwrap();
        assert!(!p.applies_to(&u("http://www.example.com/")));
        assert!(p.applies_to(&u("http://example.com/")));
        assert!(!p.applies_to(&u("http://x.b.org/")));
        assert!(!p.applies_to(&u("http://c.net:8080/")));
        assert!(p.applies_to(&u("http://c.net/")));
        assert!(!p.applies_to(&u("http://intranet/")));
        assert!(!p.applies_to(&u("http://10.1.2.3/")));
        assert!(p.applies_to(&u("http://11.1.2.3/")));
        assert!(!p.applies_to(&u("https://s.io/")));
        assert!(p.applies_to(&u("http://s.io/")));
        // Implicit loopback bypass, lifted by <-loopback>.
        for s in [
            "http://127.0.0.1:9/",
            "http://localhost/",
            "http://a.localhost/",
            "http://[::1]/",
            "http://169.254.1.1/",
        ] {
            assert!(!p.applies_to(&u(s)), "{s}");
        }
        let p = Proxy::parse("h:1", Some("<-loopback>")).unwrap().unwrap();
        assert!(p.applies_to(&u("http://127.0.0.1:9/")));
        assert!(p.applies_to(&u("http://localhost/")));
        let p = Proxy::parse("h:1", Some("*")).unwrap().unwrap();
        assert!(!p.applies_to(&u("https://a.b/")));
    }

    #[test]
    fn forward_header_order() {
        let p = Proxy::parse("h:1", None).unwrap().unwrap();
        let hs = |v: &[&str]| -> OrderedHeaders {
            v.iter()
                .map(|n| ((*n).to_owned(), "x".to_owned()))
                .collect()
        };
        let names = |h: &OrderedHeaders| h.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        let h = forward_headers(hs(&["Host", "Connection", "Upgrade-Insecure-Requests"]), &p);
        assert_eq!(
            names(&h),
            ["Host", "Proxy-Connection", "Upgrade-Insecure-Requests"]
        );
        p.provide_credentials(Credentials {
            username: "u1".into(),
            password: "p1".into(),
        });
        // Chrome: Host, Proxy-Connection, Pragma, Cache-Control,
        // Proxy-Authorization, Upgrade-Insecure-Requests (chrome.jsonl).
        let h = forward_headers(
            hs(&[
                "Host",
                "Connection",
                "Pragma",
                "Cache-Control",
                "Upgrade-Insecure-Requests",
            ]),
            &p,
        );
        assert_eq!(
            names(&h),
            [
                "Host",
                "Proxy-Connection",
                "Pragma",
                "Cache-Control",
                "Proxy-Authorization",
                "Upgrade-Insecure-Requests"
            ]
        );
        assert_eq!(h[4].1, "Basic dTE6cDE=");
    }

    #[test]
    fn helpers() {
        assert!(wildcard("*.a.com", "x.y.a.com"));
        assert!(!wildcard("*.a.com", "a.com"));
        assert!(wildcard("a?c", "abc"));
        assert_eq!(
            parse_challenge("Basic realm=\"lb test\""),
            (Some("basic".into()), Some("lb test".into()))
        );
        assert!(in_cidr(
            "192.168.5.1".parse().unwrap(),
            "192.168.0.0".parse().unwrap(),
            16
        ));
        assert!(!in_cidr(
            "192.169.5.1".parse().unwrap(),
            "192.168.0.0".parse().unwrap(),
            16
        ));
    }
}
