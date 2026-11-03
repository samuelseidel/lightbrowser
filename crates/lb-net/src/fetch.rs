//! The request pipeline: DNS → TCP → TLS (profile) → h1/h2 (ALPN) →
//! decode body → update cookie jar.
//!
//! Owner: M1 task 5.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::{Method, StatusCode};
use lb_persona::Persona;
use tokio::net::TcpStream;
use url::Url;

use crate::HttpVersion;
use crate::cookies::CookieJar;
use crate::error::NetError;
use crate::header_profile::{BuildInput, HeaderProfile, RequestMeta, Site};
use crate::headers::{self, OrderedHeaders};
use crate::profile::ClientHelloProfile;
use crate::tls::TlsConnector;
use crate::{decompress, h1, h2};

/// Redirects followed before giving up.
const MAX_REDIRECTS: usize = 10;
/// Time allowed for one TCP connect attempt (per resolved address).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// An outgoing request. Headers are ordered.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub url: Url,
    /// Headers the page set (`fetch` init, `setRequestHeader`), in order.
    /// They are merged into the browser's own header set for the request
    /// kind (see [`crate::header_profile`]).
    pub headers: OrderedHeaders,
    pub body: Bytes,
    /// What the request is for. `None` means a typed top-level navigation.
    pub meta: Option<RequestMeta>,
}

impl Request {
    pub fn get(url: Url) -> Self {
        Self {
            method: Method::GET,
            url,
            headers: Vec::new(),
            body: Bytes::new(),
            meta: None,
        }
    }

    /// A request of the given kind (script, image, `fetch`, ...) made by
    /// `initiator`.
    pub fn for_kind(url: Url, meta: RequestMeta) -> Self {
        Self {
            meta: Some(meta),
            ..Self::get(url)
        }
    }
}

/// What a transport returns: status, headers in arrival order and the
/// undecoded body.
#[derive(Debug, Clone)]
pub struct RawResponse {
    pub status: StatusCode,
    pub headers: OrderedHeaders,
    pub body: Bytes,
    pub version: HttpVersion,
}

/// One redirect that was followed on the way to a [`Response`].
#[derive(Debug, Clone)]
pub struct RedirectHop {
    /// The URL that was requested and answered with a redirect.
    pub url: Url,
    pub method: Method,
    /// Request headers as sent on this hop.
    pub request_headers: OrderedHeaders,
    pub status: StatusCode,
    /// Response headers of the redirect, as received.
    pub response_headers: OrderedHeaders,
    /// The `Location` value as received.
    pub location: String,
    /// `location` resolved against `url`: the next hop's URL.
    pub target: Url,
    pub version: HttpVersion,
}

/// A decoded response.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: StatusCode,
    pub headers: OrderedHeaders,
    pub body: Bytes,
    pub version: HttpVersion,
    pub final_url: Url,
    /// Method of the final request (a redirect can turn `POST` into `GET`).
    pub method: Method,
    /// Request headers as sent on the final hop.
    pub request_headers: OrderedHeaders,
    /// Redirects followed, in order. Empty when there were none.
    pub redirects: Vec<RedirectHop>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Per-context networking state: persona, TLS profile and cookies.
pub struct NetContext {
    pub persona: Arc<Persona>,
    pub profile: Arc<ClientHelloProfile>,
    /// Per-request-kind header sets. `None` (Firefox until M4b) falls back to
    /// the navigation set with a small per-kind overlay.
    pub header_profile: Option<Arc<HeaderProfile>>,
    pub cookies: CookieJar,
    /// Maximum body size, enforced on the wire and again after decoding.
    /// Default 32 MiB.
    pub max_body: usize,
    /// Skip certificate chain and hostname checks. Only for our own local test
    /// servers with self-signed certificates. Default `false`. Changing it
    /// between requests rebuilds the cached connector.
    pub danger_accept_invalid_certs: bool,
    /// Lazily built, then reused for every https request of this context. The
    /// flag records the `danger_accept_invalid_certs` it was built with.
    connector: Option<(bool, Arc<TlsConnector>)>,
    /// Lazily built DNS resolver (system configuration).
    resolver: Option<hickory_resolver::TokioResolver>,
}

impl NetContext {
    pub fn new(persona: Arc<Persona>, profile: Arc<ClientHelloProfile>) -> Result<Self, NetError> {
        if let Some(fam) = profile.family()
            && fam != persona.family
        {
            return Err(NetError::FamilyMismatch {
                persona: persona.family.to_string(),
                profile: profile.profile_id.clone(),
            });
        }
        let header_profile = HeaderProfile::builtin(persona.family).map(Arc::new);
        Ok(Self {
            persona,
            profile,
            header_profile,
            cookies: CookieJar::new(),
            max_body: 32 << 20,
            danger_accept_invalid_certs: false,
            connector: None,
            resolver: None,
        })
    }

    /// The cached TLS connector for this context, built on first use.
    fn tls_connector(&mut self) -> Result<Arc<TlsConnector>, NetError> {
        if let Some((flag, c)) = &self.connector
            && *flag == self.danger_accept_invalid_certs
        {
            return Ok(Arc::clone(c));
        }
        let c = Arc::new(
            TlsConnector::new(Arc::clone(&self.profile))?
                .with_danger_accept_invalid_certs(self.danger_accept_invalid_certs),
        );
        self.connector = Some((self.danger_accept_invalid_certs, Arc::clone(&c)));
        Ok(c)
    }

    /// Resolve `host` (no brackets) to socket addresses. IP literals skip DNS.
    /// Uses hickory with the system configuration and falls back to the
    /// platform resolver (`getaddrinfo`) if hickory cannot be set up or fails.
    async fn resolve(&mut self, host: &str, port: u16) -> Result<Vec<SocketAddr>, NetError> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
        if self.resolver.is_none() {
            match hickory_resolver::TokioResolver::builder_tokio() {
                Ok(b) => self.resolver = Some(b.build()),
                Err(e) => tracing::debug!("hickory resolver unavailable: {e}"),
            }
        }
        if let Some(r) = &self.resolver {
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
    async fn connect_tcp(&mut self, host: &str, port: u16) -> Result<TcpStream, NetError> {
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

/// Perform `req` in `ctx`, following up to 10 redirects.
///
/// Per hop: resolve, connect, TLS (https only, profile-driven, ALPN picks
/// h1 or h2), send, store `Set-Cookie`, then follow `301/302/303/307/308`.
///
/// Request headers are the browser's header set for the request kind
/// (`req.meta`, a typed navigation by default), built per hop once the HTTP
/// version is known: persona values, `Referer`/`Origin`/`Sec-Fetch-Site`
/// derived from the initiator and the hop's URL, the jar's `Cookie`, then the
/// page's own `req.headers`. A page header replaces a persona-owned or
/// profile header of the same name in place; other page headers join the
/// browser's header group (see [`crate::header_profile`]).
///
/// Redirect rules: `303` becomes `GET` (`HEAD` stays `HEAD`), `301`/`302` on
/// `POST` become `GET`, `307`/`308` keep method and body. When the body is
/// dropped, the page's `Content-*` headers go too. Page-supplied
/// `Authorization`, `Proxy-Authorization` and `Cookie` are dropped on a
/// cross-origin hop (the jar supplies cookies per hop anyway).
/// `Sec-Fetch-Site` is the largest distance seen on any URL of the chain.
///
/// The returned body is decoded (`Content-Encoding`); the response headers are
/// returned as received, so `content-encoding` and `content-length` still
/// describe the wire form. `max_body` limits both sizes. Every redirect that
/// was followed is in [`Response::redirects`].
pub async fn fetch(ctx: &mut NetContext, req: Request) -> Result<Response, NetError> {
    let mut req = req;
    let mut redirects: Vec<RedirectHop> = Vec::new();
    let initiator = req.meta.as_ref().and_then(|m| m.initiator.clone());
    let mut site = Site::None;
    loop {
        // Distance of the chain so far, including this hop's URL.
        site = site.max(Site::between(initiator.as_ref(), &req.url));
        let hop = fetch_once(ctx, &req, redirects.len(), site).await?;

        if let Some(location) = redirect_location(hop.status, &hop.headers) {
            if redirects.len() >= MAX_REDIRECTS {
                return Err(NetError::Http(format!(
                    "too many redirects (more than {MAX_REDIRECTS})"
                )));
            }
            let next = req.url.join(&location)?;
            if !matches!(next.scheme(), "http" | "https") {
                return Err(NetError::UnsupportedScheme(next.scheme().to_owned()));
            }
            redirects.push(RedirectHop {
                url: req.url.clone(),
                method: req.method.clone(),
                request_headers: hop.sent,
                status: hop.status,
                response_headers: hop.headers,
                location,
                target: next.clone(),
                version: hop.version,
            });
            req = redirected_request(&req, hop.status, next);
            continue;
        }

        return Ok(Response {
            status: hop.status,
            headers: hop.headers,
            body: hop.body,
            version: hop.version,
            final_url: req.url,
            method: req.method,
            request_headers: hop.sent,
            redirects,
        });
    }
}

/// What one hop returned.
struct Hop {
    status: StatusCode,
    headers: OrderedHeaders,
    body: Bytes,
    version: HttpVersion,
    /// Request headers as sent.
    sent: OrderedHeaders,
}

/// One hop: send `req`, store cookies, decode the body.
async fn fetch_once(
    ctx: &mut NetContext,
    req: &Request,
    hop: usize,
    site: Site,
) -> Result<Hop, NetError> {
    let https = match req.url.scheme() {
        "https" => true,
        "http" => false,
        other => return Err(NetError::UnsupportedScheme(other.to_owned())),
    };
    let host = req
        .url
        .host_str()
        .ok_or_else(|| NetError::Http(format!("url has no host: {}", req.url)))?
        .to_owned();
    let port = req
        .url
        .port_or_known_default()
        .unwrap_or(if https { 443 } else { 80 });
    // `host_str` brackets IPv6 literals; resolution wants them bare.
    let bare_host = host.trim_start_matches('[').trim_end_matches(']');

    // Final headers for this hop. The set depends on the HTTP version
    // (ALPN), so it is built once the version is known.
    let cookie = ctx.cookies.header_for(&req.url);
    let persona = Arc::clone(&ctx.persona);
    let header_profile = ctx.header_profile.clone();
    let meta = req.meta.clone().unwrap_or_default();
    let wire = |version: HttpVersion| {
        let input = BuildInput {
            persona: &persona,
            version,
            meta: &meta,
            url: &req.url,
            method: &req.method,
            body_len: req.body.len(),
            cookie: cookie.as_deref(),
            caller: &req.headers,
            hop,
            site,
        };
        let headers = match &header_profile {
            Some(p) => p.build(&input),
            None => headers::generic_request_headers(&input),
        };
        Request {
            method: req.method.clone(),
            url: req.url.clone(),
            headers,
            body: req.body.clone(),
            meta: None,
        }
    };

    let connector = if https {
        Some(ctx.tls_connector()?)
    } else {
        None
    };
    let tcp = ctx.connect_tcp(bare_host, port).await?;

    let (raw, sent) = match connector {
        None => {
            let wire = wire(HttpVersion::Http1);
            let raw = h1::send_with_limit(tcp, &wire, ctx.max_body).await?;
            (raw, wire.headers)
        }
        Some(connector) => {
            let (stream, version) = connector.connect(bare_host, tcp).await?;
            let wire = wire(version);
            let raw = match version {
                HttpVersion::Http2 => {
                    h2::send_with_limit(stream, &wire, &ctx.profile, ctx.max_body).await?
                }
                HttpVersion::Http1 => h1::send_with_limit(stream, &wire, ctx.max_body).await?,
            };
            (raw, wire.headers)
        }
    };

    let set_cookies = header_values(&raw.headers, "set-cookie");
    if !set_cookies.is_empty() {
        ctx.cookies.store_response(&req.url, &set_cookies);
    }

    let encoding = header_values(&raw.headers, "content-encoding").join(",");
    let body = if raw.body.is_empty() || encoding.trim().is_empty() {
        raw.body
    } else {
        decompress::decode(&encoding, raw.body)?
    };
    if body.len() > ctx.max_body {
        return Err(NetError::BodyTooLarge {
            limit: ctx.max_body,
        });
    }
    Ok(Hop {
        status: raw.status,
        headers: raw.headers,
        body,
        version: raw.version,
        sent,
    })
}

/// All values of header `name` (case-insensitive), in arrival order.
fn header_values(headers: &OrderedHeaders, name: &str) -> Vec<String> {
    headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
        .collect()
}

/// `Location` of a followable redirect, if `status` is one and it is present.
fn redirect_location(status: StatusCode, headers: &OrderedHeaders) -> Option<String> {
    if !matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("location"))
        .map(|(_, v)| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// The request for the next hop after a redirect with `status` to `next`.
fn redirected_request(prev: &Request, status: StatusCode, next: Url) -> Request {
    let code = status.as_u16();
    let method = match code {
        303 if prev.method != Method::HEAD => Method::GET,
        301 | 302 if prev.method == Method::POST => Method::GET,
        _ => prev.method.clone(),
    };
    let drop_body = code == 303 || method != prev.method;
    let cross_origin = prev.url.origin() != next.origin();

    let mut headers = prev.headers.clone();
    headers.retain(|(n, _)| {
        let n = n.to_ascii_lowercase();
        if drop_body
            && matches!(
                n.as_str(),
                "content-type"
                    | "content-length"
                    | "content-encoding"
                    | "content-language"
                    | "content-location"
                    | "transfer-encoding"
            )
        {
            return false;
        }
        !(cross_origin
            && matches!(
                n.as_str(),
                "authorization" | "proxy-authorization" | "cookie"
            ))
    });
    Request {
        method,
        url: next,
        headers,
        body: if drop_body {
            Bytes::new()
        } else {
            prev.body.clone()
        },
        meta: prev.meta.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::Mutex;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    fn h(pairs: &[(&str, &str)]) -> OrderedHeaders {
        pairs
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    // ---- pure parts ----

    #[test]
    fn redirect_location_only_for_redirect_statuses() {
        let hs = h(&[("Location", " /next ")]);
        for code in [301u16, 302, 303, 307, 308] {
            assert_eq!(
                redirect_location(StatusCode::from_u16(code).unwrap(), &hs).as_deref(),
                Some("/next")
            );
        }
        assert_eq!(redirect_location(StatusCode::OK, &hs), None);
        assert_eq!(redirect_location(StatusCode::NOT_MODIFIED, &hs), None);
        assert_eq!(redirect_location(StatusCode::FOUND, &h(&[])), None);
    }

    #[test]
    fn redirect_method_and_body_rules() {
        let mut post = Request::get(url("https://a.test/form"));
        post.method = Method::POST;
        post.body = Bytes::from_static(b"k=v");
        post.headers = h(&[("Content-Type", "text/plain"), ("X-Keep", "1")]);
        let to = url("https://a.test/next");

        for code in [301u16, 302, 303] {
            let r = redirected_request(&post, StatusCode::from_u16(code).unwrap(), to.clone());
            assert_eq!(r.method, Method::GET, "{code}");
            assert!(r.body.is_empty(), "{code}");
            assert_eq!(r.headers, h(&[("X-Keep", "1")]), "{code}");
        }
        for code in [307u16, 308] {
            let r = redirected_request(&post, StatusCode::from_u16(code).unwrap(), to.clone());
            assert_eq!(r.method, Method::POST, "{code}");
            assert_eq!(r.body, Bytes::from_static(b"k=v"), "{code}");
            assert_eq!(r.headers.len(), 2, "{code}");
        }
        // 301/302 keep GET, 303 keeps HEAD.
        let get = Request::get(url("https://a.test/"));
        let r = redirected_request(&get, StatusCode::FOUND, to.clone());
        assert_eq!(r.method, Method::GET);
        let mut head = get.clone();
        head.method = Method::HEAD;
        let r = redirected_request(&head, StatusCode::SEE_OTHER, to);
        assert_eq!(r.method, Method::HEAD);
    }

    #[test]
    fn cross_origin_redirect_drops_credentials() {
        let mut r = Request::get(url("https://a.test/"));
        r.headers = h(&[("Authorization", "x"), ("Cookie", "a=b"), ("X-Keep", "1")]);
        let same = redirected_request(&r, StatusCode::FOUND, url("https://a.test/b"));
        assert_eq!(same.headers.len(), 3);
        let cross = redirected_request(&r, StatusCode::FOUND, url("https://b.test/"));
        assert_eq!(cross.headers, h(&[("X-Keep", "1")]));
        let port = redirected_request(&r, StatusCode::FOUND, url("https://a.test:8443/"));
        assert_eq!(port.headers, h(&[("X-Keep", "1")]));
    }

    #[test]
    fn context_rejects_family_mismatch() {
        let chrome = Arc::new(Persona::default_chrome());
        let ff = Arc::new(
            ClientHelloProfile::load(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../profiles/tls/firefox_128_linux.json"
            ))
            .unwrap(),
        );
        assert!(matches!(
            NetContext::new(chrome, ff),
            Err(NetError::FamilyMismatch { .. })
        ));
    }

    // ---- end to end against a local server (no external network) ----

    fn chrome_ctx() -> NetContext {
        let profile = ClientHelloProfile::load(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../profiles/tls/chrome_153_linux.json"
        ))
        .unwrap();
        NetContext::new(Arc::new(Persona::default_chrome()), Arc::new(profile)).unwrap()
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    /// Serve canned routes over plain HTTP/1.1, one request per connection.
    /// Returns the base URL and the raw requests received (head + body).
    async fn serve_http() -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let log = Arc::clone(&log);
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    // Read the head, then any body announced by content-length.
                    loop {
                        let n = sock.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        if let Some(end) = text.find("\r\n\r\n") {
                            let want = text[..end]
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .and_then(|v| v.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            if buf.len() >= end + 4 + want {
                                break;
                            }
                        }
                    }
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
                    log.lock().unwrap().push(text);
                    let reply: Vec<u8> = match path.as_str() {
                        "/start" => b"HTTP/1.1 302 Found\r\nLocation: /final\r\n\
Set-Cookie: sid=abc; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec(),
                        "/final" => {
                            let body = gzip(b"<p>hello</p>");
                            let mut r = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
Content-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            )
                            .into_bytes();
                            r.extend_from_slice(&body);
                            r
                        }
                        "/see-other" => b"HTTP/1.1 303 See Other\r\nLocation: /final\r\n\
Content-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec(),
                        "/temp" => b"HTTP/1.1 307 Temporary Redirect\r\nLocation: /echo\r\n\
Content-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec(),
                        "/loop" => b"HTTP/1.1 302 Found\r\nLocation: /loop\r\n\
Content-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec(),
                        "/big" => {
                            let body = vec![b'a'; 4096];
                            let mut r = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            )
                            .into_bytes();
                            r.extend_from_slice(&body);
                            r
                        }
                        _ => b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                            .to_vec(),
                    };
                    let _ = sock.write_all(&reply).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        (format!("http://127.0.0.1:{}", addr.port()), seen)
    }

    #[tokio::test]
    async fn http_redirect_cookie_and_gzip() {
        let (base, seen) = serve_http().await;
        let mut ctx = chrome_ctx();
        let resp = fetch(&mut ctx, Request::get(url(&format!("{base}/start"))))
            .await
            .unwrap();
        assert_eq!(resp.status, StatusCode::OK);
        assert_eq!(resp.final_url.path(), "/final");
        assert_eq!(&resp.body[..], b"<p>hello</p>");
        assert_eq!(resp.version, HttpVersion::Http1);

        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.len(), 2);
        let first = reqs[0].to_ascii_lowercase();
        assert!(first.contains("user-agent: mozilla/5.0"), "{first}");
        assert!(!first.contains("cookie:"), "{first}");
        assert!(
            reqs[1].to_ascii_lowercase().contains("cookie: sid=abc"),
            "{}",
            reqs[1]
        );
    }

    #[tokio::test]
    async fn http_caller_header_wins_and_keeps_position() {
        let (base, seen) = serve_http().await;
        let mut ctx = chrome_ctx();
        let mut req = Request::get(url(&format!("{base}/x")));
        req.headers = h(&[("User-Agent", "custom/1"), ("X-Extra", "yes")]);
        fetch(&mut ctx, req).await.unwrap();
        let raw = seen.lock().unwrap()[0].to_ascii_lowercase();
        assert!(raw.contains("user-agent: custom/1"), "{raw}");
        assert!(!raw.contains("mozilla/5.0"), "{raw}");
        assert!(raw.contains("x-extra: yes"), "{raw}");
        // Chrome order: sec-ch-ua first, user-agent after upgrade-insecure-requests.
        let pos = |n: &str| {
            raw.find(n)
                .unwrap_or_else(|| panic!("{n} missing in {raw}"))
        };
        assert!(pos("sec-ch-ua:") < pos("user-agent:"));
        assert!(pos("user-agent:") < pos("accept:"));
        assert!(pos("accept:") < pos("x-extra:"));
    }

    #[tokio::test]
    async fn http_post_303_becomes_get_and_307_keeps_body() {
        let (base, seen) = serve_http().await;
        let mut ctx = chrome_ctx();

        let mut post = Request::get(url(&format!("{base}/see-other")));
        post.method = Method::POST;
        post.body = Bytes::from_static(b"k=v");
        post.headers = h(&[("Content-Type", "application/x-www-form-urlencoded")]);
        let resp = fetch(&mut ctx, post).await.unwrap();
        assert_eq!(resp.final_url.path(), "/final");
        {
            let reqs = seen.lock().unwrap();
            assert!(reqs[0].starts_with("POST /see-other"), "{}", reqs[0]);
            assert!(reqs[1].starts_with("GET /final"), "{}", reqs[1]);
            assert!(!reqs[1].to_ascii_lowercase().contains("content-type"));
        }

        seen.lock().unwrap().clear();
        let mut post = Request::get(url(&format!("{base}/temp")));
        post.method = Method::POST;
        post.body = Bytes::from_static(b"k=v");
        let resp = fetch(&mut ctx, post).await.unwrap();
        assert_eq!(resp.status, StatusCode::OK);
        let reqs = seen.lock().unwrap();
        assert!(reqs[1].starts_with("POST /echo"), "{}", reqs[1]);
        assert!(reqs[1].ends_with("k=v"), "{}", reqs[1]);
    }

    #[tokio::test]
    async fn http_redirect_loop_errors() {
        let (base, seen) = serve_http().await;
        let mut ctx = chrome_ctx();
        let err = fetch(&mut ctx, Request::get(url(&format!("{base}/loop"))))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too many redirects"), "{err}");
        assert_eq!(seen.lock().unwrap().len(), MAX_REDIRECTS + 1);
    }

    #[tokio::test]
    async fn http_body_limit_enforced() {
        let (base, _seen) = serve_http().await;
        let mut ctx = chrome_ctx();
        ctx.max_body = 1024;
        let err = fetch(&mut ctx, Request::get(url(&format!("{base}/big"))))
            .await
            .unwrap_err();
        assert!(matches!(err, NetError::BodyTooLarge { .. }), "{err}");
    }

    #[tokio::test]
    async fn rejects_unsupported_scheme() {
        let mut ctx = chrome_ctx();
        let err = fetch(&mut ctx, Request::get(url("ftp://127.0.0.1/x")))
            .await
            .unwrap_err();
        assert!(matches!(err, NetError::UnsupportedScheme(_)), "{err}");
    }

    // ---- https against a local self-signed server ----

    fn self_signed() -> (
        boring::x509::X509,
        boring::pkey::PKey<boring::pkey::Private>,
    ) {
        use boring::asn1::Asn1Time;
        use boring::bn::BigNum;
        use boring::hash::MessageDigest;
        use boring::pkey::PKey;
        use boring::rsa::Rsa;
        use boring::x509::{X509, X509NameBuilder};

        let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_text("CN", "localhost").unwrap();
        let name = name.build();
        let mut b = X509::builder().unwrap();
        b.set_version(2).unwrap();
        let serial = BigNum::from_u32(1).unwrap().to_asn1_integer().unwrap();
        b.set_serial_number(&serial).unwrap();
        b.set_subject_name(&name).unwrap();
        b.set_issuer_name(&name).unwrap();
        b.set_pubkey(&key).unwrap();
        b.set_not_before(&Asn1Time::days_from_now(0).unwrap())
            .unwrap();
        b.set_not_after(&Asn1Time::days_from_now(1).unwrap())
            .unwrap();
        b.sign(&key, MessageDigest::sha256()).unwrap();
        (b.build(), key)
    }

    /// One-shot-per-connection HTTPS server speaking HTTP/1.1 (ALPN picks
    /// `http/1.1`). Returns its port.
    async fn serve_https() -> u16 {
        use boring::ssl::{SslAcceptor, SslMethod, select_next_proto};
        let (cert, key) = self_signed();
        let mut acc = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).unwrap();
        acc.set_private_key(&key).unwrap();
        acc.set_certificate(&cert).unwrap();
        acc.set_alpn_select_callback(|_, client| {
            select_next_proto(b"\x08http/1.1", client).ok_or(boring::ssl::AlpnError::NOACK)
        });
        let acc = acc.build();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let acc = acc.clone();
                tokio::spawn(async move {
                    let Ok(mut s) = tokio_boring::accept(&acc, sock).await else {
                        return;
                    };
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf).await;
                    let _ = s
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecure",
                        )
                        .await;
                    let _ = s.shutdown().await;
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn https_self_signed_needs_insecure_flag() {
        let port = serve_https().await;
        let target = url(&format!("https://127.0.0.1:{port}/"));
        let mut ctx = chrome_ctx();

        let err = fetch(&mut ctx, Request::get(target.clone()))
            .await
            .unwrap_err();
        assert!(matches!(err, NetError::Tls(_)), "{err}");

        ctx.danger_accept_invalid_certs = true;
        let resp = fetch(&mut ctx, Request::get(target)).await.unwrap();
        assert_eq!(resp.status, StatusCode::OK);
        assert_eq!(&resp.body[..], b"secure");
        assert_eq!(resp.version, HttpVersion::Http1);
    }
}
