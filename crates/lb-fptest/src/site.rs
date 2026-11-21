//! Local page server that records connection use.
//!
//! Serves a small site (an HTML page and its subresources) over plain
//! HTTP/1.1, or over TLS (rustls, self-signed, in memory) with HTTP/1.1 or
//! HTTP/2 chosen by ALPN. Unlike [`crate::server`] it keeps connections
//! open (HTTP/1.1 keep-alive, HTTP/2 multiplexing), and records for every
//! connection the ClientHello, the ALPN result and the paths requested on
//! it. That is what is needed to compare how many connections a browser
//! opens for a page, and which requests it sends over which connection.
//!
//! Local test use only: bind to loopback and point our own client or a
//! local Chromium at it.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::clienthello::{self, ClientHello, ParseError};
use crate::h2::{self as frames, Frame, Priority};
use crate::server::{Rewind, read_some};

/// How long the server keeps an idle connection (longer than any client
/// idle timeout we compare with, so the client decides).
const SERVER_IDLE: Duration = Duration::from_secs(600);
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// One resource of the site.
#[derive(Debug, Clone)]
pub struct Route {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
    /// Wait this long before answering.
    pub delay: Duration,
    /// HTTP/1.1: answer with `Connection: close` and close afterwards.
    pub close: bool,
}

impl Route {
    pub fn new(content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            content_type: content_type.to_owned(),
            body: body.into(),
            delay: Duration::ZERO,
            close: false,
        }
    }

    pub fn delayed(mut self, d: Duration) -> Self {
        self.delay = d;
        self
    }
}

/// A transparent 1x1 GIF.
pub const GIF_1X1: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";

/// Server settings.
#[derive(Clone)]
pub struct SiteConfig {
    /// `true`: TLS with [`Self::alpn`]; `false`: plain HTTP/1.1.
    pub tls: bool,
    /// ALPN the server offers, in preference order.
    pub alpn: Vec<Vec<u8>>,
    /// Extra SAN names for the self-signed cert (always has localhost and
    /// 127.0.0.1).
    pub hostnames: Vec<String>,
    /// Path (with query) → resource. Unknown paths get a 404.
    pub routes: HashMap<String, Route>,
}

impl SiteConfig {
    /// A page `/` with `images` images, `scripts` classic scripts and
    /// `styles` stylesheets, each subresource answered after `delay`.
    /// Also serves `/favicon.ico` (a GIF) and `/next` (a small page).
    pub fn subresource_page(
        tls: bool,
        images: usize,
        scripts: usize,
        styles: usize,
        delay: Duration,
    ) -> Self {
        let mut routes = HashMap::new();
        let mut html = String::from("<!doctype html><html><head><title>pool</title>\n");
        for i in 0..styles {
            html.push_str(&format!("<link rel=stylesheet href=\"/css/{i}.css\">\n"));
            routes.insert(
                format!("/css/{i}.css"),
                Route::new("text/css", format!("p.c{i}{{margin:0}}")).delayed(delay),
            );
        }
        html.push_str("</head><body><p>pool test</p>\n");
        for i in 0..images {
            html.push_str(&format!("<img src=\"/img/{i}.gif\" width=1 height=1>\n"));
            routes.insert(
                format!("/img/{i}.gif"),
                Route::new("image/gif", GIF_1X1).delayed(delay),
            );
        }
        for i in 0..scripts {
            html.push_str(&format!("<script src=\"/js/{i}.js\"></script>\n"));
            routes.insert(
                format!("/js/{i}.js"),
                Route::new("text/javascript", format!("window.s{i}=1;")).delayed(delay),
            );
        }
        html.push_str("</body></html>\n");
        routes.insert("/".into(), Route::new("text/html", html));
        routes.insert("/favicon.ico".into(), Route::new("image/gif", GIF_1X1));
        routes.insert(
            "/next".into(),
            Route::new("text/html", "<!doctype html><title>next</title><p>next"),
        );
        // A cross-site frame: the same server under another host name.
        routes.insert(
            "/frame".into(),
            Route::new(
                "text/html",
                "<!doctype html><title>frame</title><script>document.write('<iframe src=\"'\
                 + location.protocol + '//fptest.localhost:' + location.port + '/next\"></iframe>')\
                 </script>",
            ),
        );
        // `/xo?https://host:port`: the images of `/` from another origin.
        routes.insert(
            "/xo".into(),
            Route::new(
                "text/html",
                format!(
                    "<!doctype html><title>xo</title><script>for (let i = 0; i < {images}; i++) \
                     document.write('<img src=\"' + location.search.slice(1) + '/img/' + i + \
                     '.gif\" width=1 height=1>')</script>"
                ),
            ),
        );
        Self {
            tls,
            alpn: vec![b"h2".to_vec(), b"http/1.1".to_vec()],
            hostnames: vec!["fptest.localhost".into(), "a.localhost".into()],
            routes,
        }
    }
}

/// One request as the server saw it.
#[derive(Debug, Clone)]
pub struct RequestRecord {
    pub path: String,
    /// Time since the server started.
    pub at: Duration,
    /// Request header names and values (HTTP/1.1: wire case; HTTP/2:
    /// lowercase), in order.
    pub headers: Vec<(String, String)>,
    /// HTTP/2 stream id.
    pub stream: Option<u32>,
}

/// Everything recorded for one connection.
#[derive(Debug, Clone)]
pub struct ConnRecord {
    /// Accept order, from 0.
    pub id: usize,
    pub peer: SocketAddr,
    pub client_hello: Option<ClientHello>,
    pub alpn: Option<String>,
    pub requests: Vec<RequestRecord>,
    /// HTTP/2: the client's frames in arrival order (type name, stream),
    /// and the priority block of every HEADERS frame.
    pub h2_frames: Vec<(String, u32)>,
    pub h2_priorities: Vec<Priority>,
    /// The connection has ended.
    pub closed: bool,
    pub error: Option<String>,
}

impl ConnRecord {
    /// The ClientHello offers a session to resume (`pre_shared_key`).
    pub fn resumed(&self) -> bool {
        self.client_hello
            .as_ref()
            .is_some_and(crate::profile::is_resumption)
    }
}

/// A running page server. Dropping it stops accepting (open connections
/// end with their tasks).
pub struct SiteServer {
    addr: SocketAddr,
    records: Arc<Mutex<Vec<ConnRecord>>>,
    task: JoinHandle<()>,
}

impl Drop for SiteServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

type Records = Arc<Mutex<Vec<ConnRecord>>>;

impl SiteServer {
    /// Binds `addr` (use loopback) and starts serving.
    pub async fn bind(addr: &str, cfg: SiteConfig) -> io::Result<Self> {
        let acceptor = if cfg.tls {
            let mut names = vec!["localhost".to_owned(), "127.0.0.1".to_owned()];
            names.extend(cfg.hostnames.iter().cloned());
            let ck = rcgen::generate_simple_self_signed(names).map_err(io::Error::other)?;
            let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der()));
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let mut tls = rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(io::Error::other)?
                .with_no_client_auth()
                .with_single_cert(vec![ck.cert.der().clone()], key)
                .map_err(io::Error::other)?;
            tls.alpn_protocols = cfg.alpn.clone();
            Some(TlsAcceptor::from(Arc::new(tls)))
        } else {
            None
        };
        let listener = TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let records: Records = Arc::new(Mutex::new(Vec::new()));
        let routes = Arc::new(cfg.routes);
        let start = Instant::now();
        let recs = Arc::clone(&records);
        let task = tokio::spawn(async move {
            while let Ok((sock, peer)) = listener.accept().await {
                let id = {
                    let mut r = recs.lock().unwrap();
                    let id = r.len();
                    r.push(ConnRecord {
                        id,
                        peer,
                        client_hello: None,
                        alpn: None,
                        requests: Vec::new(),
                        h2_frames: Vec::new(),
                        h2_priorities: Vec::new(),
                        closed: false,
                        error: None,
                    });
                    id
                };
                let (acceptor, recs, routes) =
                    (acceptor.clone(), Arc::clone(&recs), routes.clone());
                tokio::spawn(async move {
                    let site = Site {
                        id,
                        records: Arc::clone(&recs),
                        routes,
                        start,
                    };
                    let r = match acceptor {
                        Some(acc) => serve_tls(sock, acc, &site).await,
                        None => serve_h1(sock, &site).await,
                    };
                    let mut all = recs.lock().unwrap();
                    all[id].closed = true;
                    if let Err(e) = r {
                        all[id].error = Some(e.to_string());
                    }
                });
            }
        });
        Ok(Self {
            addr,
            records,
            task,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// A copy of the connection records so far, in accept order.
    pub fn records(&self) -> Vec<ConnRecord> {
        self.records.lock().unwrap().clone()
    }

    /// Shared handle on the records (for printing as connections end).
    pub fn records_handle(&self) -> Arc<Mutex<Vec<ConnRecord>>> {
        Arc::clone(&self.records)
    }
}

struct Site {
    id: usize,
    records: Records,
    routes: Arc<HashMap<String, Route>>,
    start: Instant,
}

impl Site {
    fn record(&self, path: &str, headers: Vec<(String, String)>, stream: Option<u32>) -> Route {
        self.records.lock().unwrap()[self.id]
            .requests
            .push(RequestRecord {
                path: path.to_owned(),
                at: self.start.elapsed(),
                headers,
                stream,
            });
        let bare = path.split('?').next().unwrap_or(path);
        self.routes
            .get(path)
            .or_else(|| self.routes.get(bare))
            .cloned()
            .unwrap_or(Route {
                status: 404,
                content_type: "text/plain".into(),
                body: b"not found".to_vec(),
                delay: Duration::ZERO,
                close: false,
            })
    }
}

async fn serve_tls(mut sock: TcpStream, acceptor: TlsAcceptor, site: &Site) -> io::Result<()> {
    let mut buf = Vec::new();
    loop {
        match clienthello::reassemble(&buf) {
            Ok(_) => break,
            Err(ParseError::Incomplete) => {
                if read_some(&mut sock, &mut buf, READ_TIMEOUT).await? == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "eof before ClientHello",
                    ));
                }
            }
            Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidData, e)),
        }
    }
    let ch = ClientHello::parse_records(&buf).ok();
    site.records.lock().unwrap()[site.id].client_hello = ch;
    let stream = Rewind::new(buf, sock);
    let tls = tokio::time::timeout(READ_TIMEOUT, acceptor.accept(stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timed out"))??;
    let alpn = tls
        .get_ref()
        .1
        .alpn_protocol()
        .map(|p| String::from_utf8_lossy(p).into_owned());
    site.records.lock().unwrap()[site.id].alpn = alpn.clone();
    if alpn.as_deref() == Some("h2") {
        serve_h2(tls, site).await
    } else {
        serve_h1(tls, site).await
    }
}

/// HTTP/1.1 with keep-alive: requests in turn until the client closes.
async fn serve_h1<S: AsyncRead + AsyncWrite + Unpin>(mut s: S, site: &Site) -> io::Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i;
            }
            let mut tmp = [0u8; 16 * 1024];
            let n = match tokio::time::timeout(SERVER_IDLE, s.read(&mut tmp)).await {
                Ok(Ok(n)) => n,
                Ok(Err(e)) if e.kind() == io::ErrorKind::ConnectionReset => 0,
                Ok(Err(e)) => return Err(e),
                Err(_) => 0,
            };
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&tmp[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..end]).into_owned();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or_default().to_owned();
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.to_owned(), v.trim_start().to_owned()))
            .collect();
        let body_len = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while buf.len() < end + 4 + body_len {
            if read_some(&mut s, &mut buf, READ_TIMEOUT).await? == 0 {
                return Ok(());
            }
        }
        buf.drain(..end + 4 + body_len);
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or("/")
            .to_owned();
        let route = site.record(&path, headers, None);
        if !route.delay.is_zero() {
            tokio::time::sleep(route.delay).await;
        }
        let mut resp = format!(
            "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: {}\r\n\r\n",
            route.status,
            route.content_type,
            route.body.len(),
            if route.close { "close" } else { "keep-alive" }
        )
        .into_bytes();
        resp.extend_from_slice(&route.body);
        s.write_all(&resp).await?;
        s.flush().await?;
        if route.close {
            let _ = s.shutdown().await;
            return Ok(());
        }
    }
}

/// HTTP/2 through the `h2` crate's server: every stream answered from the
/// routes, concurrently.
async fn serve_h2<S>(s: S, site: &Site) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let tap = Arc::new(Mutex::new(Vec::new()));
    let s = Tap {
        inner: s,
        seen: Arc::clone(&tap),
    };
    let update = || {
        let (names, prios) = client_frames(&tap.lock().unwrap());
        let mut all = site.records.lock().unwrap();
        all[site.id].h2_frames = names;
        all[site.id].h2_priorities = prios;
    };
    let mut conn = ::h2::server::handshake(s).await.map_err(io::Error::other)?;
    while let Some(r) = conn.accept().await {
        update();
        let Ok((req, mut respond)) = r else { break };
        let stream = u32::from(respond.stream_id());
        let path = req
            .uri()
            .path_and_query()
            .map(|p| p.as_str().to_owned())
            .unwrap_or_else(|| "/".into());
        let headers = req
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_owned(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();
        let route = site.record(&path, headers, Some(stream));
        tokio::spawn(async move {
            if !route.delay.is_zero() {
                tokio::time::sleep(route.delay).await;
            }
            let resp = http::Response::builder()
                .status(route.status)
                .header("content-type", route.content_type)
                .header("content-length", route.body.len())
                .body(())
                .unwrap();
            if let Ok(mut send) = respond.send_response(resp, route.body.is_empty()) {
                let _ = send.send_data(bytes::Bytes::from(route.body), true);
            }
        });
    }
    update();
    Ok(())
}

/// The client's HTTP/2 frames (after the preface): type names with their
/// stream, and the priority blocks of HEADERS frames.
fn client_frames(bytes: &[u8]) -> (Vec<(String, u32)>, Vec<Priority>) {
    let mut names = Vec::new();
    let mut prios = Vec::new();
    let Some(mut rest) = bytes.strip_prefix(frames::PREFACE) else {
        return (names, prios);
    };
    while let Some((f, used)) = Frame::parse(rest) {
        rest = &rest[used..];
        let name = match f.kind {
            frames::DATA => "DATA",
            frames::HEADERS => "HEADERS",
            frames::PRIORITY => "PRIORITY",
            0x3 => "RST_STREAM",
            frames::SETTINGS => "SETTINGS",
            frames::PING => "PING",
            frames::GOAWAY => "GOAWAY",
            frames::WINDOW_UPDATE => "WINDOW_UPDATE",
            frames::CONTINUATION => "CONTINUATION",
            _ => "OTHER",
        };
        names.push((name.to_owned(), f.stream));
        if f.kind == frames::HEADERS
            && let Some((Some(p), _)) = frames::headers_fragment(&f)
        {
            prios.push(p);
        }
    }
    (names, prios)
}

/// Copies everything read from `inner` (the decrypted client bytes).
struct Tap<S> {
    inner: S,
    seen: Arc<Mutex<Vec<u8>>>,
}

impl<S: AsyncRead + Unpin> AsyncRead for Tap<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = r {
            self.seen
                .lock()
                .unwrap()
                .extend_from_slice(&buf.filled()[before..]);
        }
        r
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Tap<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
