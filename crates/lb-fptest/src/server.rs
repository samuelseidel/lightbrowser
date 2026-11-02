//! Local TLS capture server.
//!
//! Accepts TCP connections, reads the raw ClientHello, then finishes a real
//! TLS handshake (rustls, self-signed cert made in memory) and answers one
//! tiny HTTP/1.1 or HTTP/2 response per request. One [`Capture`] per
//! connection goes out on a channel once the first request is read (or the
//! connection fails).
//!
//! Local test use only: bind to loopback and point our own client or a
//! local Chromium at it.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;

use crate::clienthello::{self, ClientHello, ParseError};
use crate::h2::{self, Frame, Priority};

const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Close an idle connection after the first response.
const IDLE_TIMEOUT: Duration = Duration::from_secs(3);

/// What the client sent on the HTTP/2 connection preface.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct H2Capture {
    /// First client SETTINGS frame, `(id, value)` in wire order.
    pub settings: Vec<(u16, u32)>,
    /// First connection-level WINDOW_UPDATE increment.
    pub window_update: Option<u32>,
    /// PRIORITY frames seen before the first request.
    pub priority_frames: Vec<Priority>,
    /// Priority block on the first HEADERS frame, if flagged.
    pub headers_priority: Option<Priority>,
    /// Pseudo-headers of the first request, in order.
    pub pseudo_header_order: Vec<String>,
    /// All header names of the first request; `None` = Huffman-coded name.
    pub header_names: Vec<Option<String>>,
    /// Frame type names in arrival order up to the first HEADERS.
    pub frame_order: Vec<String>,
}

impl H2Capture {
    /// Akamai HTTP/2 fingerprint text.
    pub fn akamai(&self) -> String {
        let prio: Vec<String> = self
            .priority_frames
            .iter()
            .map(|p| {
                format!(
                    "{}:{}:{}:{}",
                    p.stream,
                    u8::from(p.exclusive),
                    p.depends_on,
                    p.weight as u16 + 1
                )
            })
            .collect();
        crate::fingerprint::akamai_h2(
            &self.settings,
            self.window_update.unwrap_or(0),
            &prio,
            &self.pseudo_header_order,
        )
    }
}

/// The first HTTP/1.1 request head.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct H1Capture {
    pub request_line: String,
    /// Header names and values, in order, as sent.
    pub headers: Vec<(String, String)>,
}

/// Everything recorded for one client connection.
#[derive(Debug, Clone)]
pub struct Capture {
    pub peer: SocketAddr,
    /// The TLS record bytes that carried the ClientHello.
    pub raw_client_hello: Vec<u8>,
    pub client_hello: Option<ClientHello>,
    pub negotiated_alpn: Option<String>,
    pub h2: Option<H2Capture>,
    pub h1: Option<H1Capture>,
    /// First failure, if the connection did not get as far as a request.
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// ALPN the server offers, in preference order.
    pub alpn: Vec<Vec<u8>>,
    /// Response body for every request.
    pub body: Vec<u8>,
    /// Extra SAN names for the self-signed cert (always has localhost and
    /// 127.0.0.1).
    pub hostnames: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            alpn: vec![b"h2".to_vec(), b"http/1.1".to_vec()],
            body: b"lb-fptest ok\n".to_vec(),
            hostnames: vec!["fptest.localhost".into()],
        }
    }
}

/// A running capture server. Dropping it stops accepting.
pub struct CaptureServer {
    addr: SocketAddr,
    cert_der: CertificateDer<'static>,
    task: JoinHandle<()>,
}

impl Drop for CaptureServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl CaptureServer {
    /// Binds `addr` (use loopback) and starts accepting.
    pub async fn bind(
        addr: &str,
        cfg: ServerConfig,
    ) -> io::Result<(Self, mpsc::UnboundedReceiver<Capture>)> {
        let mut names = vec!["localhost".to_owned(), "127.0.0.1".to_owned()];
        names.extend(cfg.hostnames.iter().cloned());
        let ck = rcgen::generate_simple_self_signed(names).map_err(io::Error::other)?;
        let cert_der = ck.cert.der().clone();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der()));

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(io::Error::other)?
            .with_no_client_auth()
            .with_single_cert(vec![cert_der.clone()], key)
            .map_err(io::Error::other)?;
        tls.alpn_protocols = cfg.alpn.clone();
        let acceptor = TlsAcceptor::from(Arc::new(tls));

        let listener = TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let (tx, rx) = mpsc::unbounded_channel();
        let body: Arc<[u8]> = cfg.body.into();
        let task = tokio::spawn(async move {
            while let Ok((sock, peer)) = listener.accept().await {
                let (acceptor, tx, body) = (acceptor.clone(), tx.clone(), body.clone());
                tokio::spawn(async move {
                    let mut cap = Some(Capture {
                        peer,
                        raw_client_hello: Vec::new(),
                        client_hello: None,
                        negotiated_alpn: None,
                        h2: None,
                        h1: None,
                        error: None,
                    });
                    if let Err(e) = handle(sock, acceptor, &body, &mut cap, &tx).await
                        && let Some(mut c) = cap.take()
                    {
                        c.error = Some(e.to_string());
                        let _ = tx.send(c);
                    }
                    if let Some(c) = cap.take() {
                        let _ = tx.send(c);
                    }
                });
            }
        });
        Ok((
            Self {
                addr,
                cert_der,
                task,
            },
            rx,
        ))
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// DER of the self-signed server certificate, for clients that want to
    /// trust it instead of skipping verification.
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }
}

/// Replays bytes already read before handing reads to the inner stream.
struct Rewind<S> {
    prefix: Vec<u8>,
    pos: usize,
    inner: S,
}

impl<S: AsyncRead + Unpin> AsyncRead for Rewind<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.pos < self.prefix.len() {
            let n = buf.remaining().min(self.prefix.len() - self.pos);
            let start = self.pos;
            buf.put_slice(&self.prefix[start..start + n]);
            self.pos += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Rewind<S> {
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

async fn read_some<R: AsyncRead + Unpin>(
    r: &mut R,
    buf: &mut Vec<u8>,
    t: Duration,
) -> io::Result<usize> {
    let mut tmp = [0u8; 16 * 1024];
    let n = timeout(t, r.read(&mut tmp))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "read timed out"))??;
    buf.extend_from_slice(&tmp[..n]);
    Ok(n)
}

fn emit(cap: &mut Option<Capture>, tx: &mpsc::UnboundedSender<Capture>) {
    if let Some(c) = cap.take() {
        let _ = tx.send(c);
    }
}

async fn handle(
    mut sock: TcpStream,
    acceptor: TlsAcceptor,
    body: &[u8],
    cap: &mut Option<Capture>,
    tx: &mpsc::UnboundedSender<Capture>,
) -> io::Result<()> {
    let mut buf = Vec::new();
    let hello_len = loop {
        match clienthello::reassemble(&buf) {
            Ok((_, used, _)) => break used,
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
    };
    if let Some(c) = cap.as_mut() {
        c.raw_client_hello = buf[..hello_len].to_vec();
        c.client_hello = Some(
            ClientHello::parse_records(&buf)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
        );
    }

    let stream = Rewind {
        prefix: buf,
        pos: 0,
        inner: sock,
    };
    let mut tls = timeout(READ_TIMEOUT, acceptor.accept(stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timed out"))??;
    let alpn = tls
        .get_ref()
        .1
        .alpn_protocol()
        .map(|p| String::from_utf8_lossy(p).into_owned());
    if let Some(c) = cap.as_mut() {
        c.negotiated_alpn = alpn.clone();
    }
    if alpn.as_deref() == Some("h2") {
        serve_h2(&mut tls, body, cap, tx).await
    } else {
        serve_h1(&mut tls, body, cap, tx).await
    }
}

async fn serve_h1<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
    body: &[u8],
    cap: &mut Option<Capture>,
    tx: &mpsc::UnboundedSender<Capture>,
) -> io::Result<()> {
    let mut buf = Vec::new();
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if read_some(s, &mut buf, READ_TIMEOUT).await? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "eof before request",
            ));
        }
    };
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_owned();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.to_owned(), v.trim_start().to_owned()))
        .collect();
    if let Some(c) = cap.as_mut() {
        c.h1 = Some(H1Capture {
            request_line,
            headers,
        });
    }
    emit(cap, tx);
    let mut resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    resp.extend_from_slice(body);
    s.write_all(&resp).await?;
    s.shutdown().await
}

fn frame_name(kind: u8) -> String {
    match kind {
        h2::DATA => "DATA".into(),
        h2::HEADERS => "HEADERS".into(),
        h2::PRIORITY => "PRIORITY".into(),
        0x3 => "RST_STREAM".into(),
        h2::SETTINGS => "SETTINGS".into(),
        h2::PING => "PING".into(),
        h2::GOAWAY => "GOAWAY".into(),
        h2::WINDOW_UPDATE => "WINDOW_UPDATE".into(),
        h2::CONTINUATION => "CONTINUATION".into(),
        k => format!("0x{k:02x}"),
    }
}

async fn serve_h2<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
    body: &[u8],
    cap: &mut Option<Capture>,
    tx: &mpsc::UnboundedSender<Capture>,
) -> io::Result<()> {
    // Server preface: empty SETTINGS.
    s.write_all(&Frame::new(h2::SETTINGS, 0, 0, vec![]).encode())
        .await?;
    s.flush().await?;

    let mut buf = Vec::new();
    while buf.len() < h2::PREFACE.len() {
        if read_some(s, &mut buf, READ_TIMEOUT).await? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "eof before h2 preface",
            ));
        }
    }
    if !buf.starts_with(h2::PREFACE) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad h2 preface"));
    }
    buf.drain(..h2::PREFACE.len());

    let mut h = H2Capture::default();
    let mut got_settings = false;
    let mut pending: Option<(u32, Option<Priority>, Vec<u8>)> = None; // HEADERS awaiting CONTINUATION
    let mut answered = false;
    loop {
        let Some((f, used)) = Frame::parse(&buf) else {
            let t = if answered { IDLE_TIMEOUT } else { READ_TIMEOUT };
            match read_some(s, &mut buf, t).await {
                Ok(0) => break,
                Ok(_) => continue,
                Err(e) if answered && e.kind() == io::ErrorKind::TimedOut => break,
                Err(e) => return Err(e),
            }
        };
        buf.drain(..used);
        if !answered {
            h.frame_order.push(frame_name(f.kind));
        }
        let mut block_done: Option<(u32, Option<Priority>, Vec<u8>)> = None;
        match f.kind {
            h2::SETTINGS if f.flags & h2::FLAG_ACK == 0 => {
                if !got_settings {
                    h.settings = h2::parse_settings(&f.payload);
                    got_settings = true;
                }
                s.write_all(&Frame::new(h2::SETTINGS, h2::FLAG_ACK, 0, vec![]).encode())
                    .await?;
            }
            h2::WINDOW_UPDATE if f.stream == 0 && h.window_update.is_none() => {
                h.window_update = h2::parse_window_update(&f.payload);
            }
            h2::PRIORITY if !answered => {
                h.priority_frames
                    .extend(h2::parse_priority(f.stream, &f.payload));
            }
            h2::PING if f.flags & h2::FLAG_ACK == 0 => {
                s.write_all(&Frame::new(h2::PING, h2::FLAG_ACK, 0, f.payload.clone()).encode())
                    .await?;
            }
            h2::GOAWAY => break,
            h2::HEADERS => {
                let (prio, frag) = h2::headers_fragment(&f)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad HEADERS"))?;
                let entry = (f.stream, prio, frag.to_vec());
                if f.flags & h2::FLAG_END_HEADERS != 0 {
                    block_done = Some(entry);
                } else {
                    pending = Some(entry);
                }
            }
            h2::CONTINUATION => {
                if let Some(mut p) = pending.take() {
                    p.2.extend_from_slice(&f.payload);
                    if f.flags & h2::FLAG_END_HEADERS != 0 {
                        block_done = Some(p);
                    } else {
                        pending = Some(p);
                    }
                }
            }
            _ => {}
        }
        if let Some((stream, prio, block)) = block_done {
            if !answered {
                let names = h2::hpack_names(&block).unwrap_or_default();
                h.pseudo_header_order = names
                    .iter()
                    .map_while(|n| n.as_ref().filter(|n| n.starts_with(':')).cloned())
                    .collect();
                h.header_names = names;
                h.headers_priority = prio;
                if let Some(c) = cap.as_mut() {
                    c.h2 = Some(h.clone());
                }
                emit(cap, tx);
                answered = true;
            }
            // :status 200 (static index 8), then the body.
            s.write_all(
                &Frame::new(h2::HEADERS, h2::FLAG_END_HEADERS, stream, vec![0x88]).encode(),
            )
            .await?;
            s.write_all(&Frame::new(h2::DATA, h2::FLAG_END_STREAM, stream, body.to_vec()).encode())
                .await?;
        }
        s.flush().await?;
    }
    if !answered && let Some(c) = cap.as_mut() {
        c.h2 = Some(h);
    }
    // GOAWAY(last stream 0, NO_ERROR); ignore errors on a closing socket.
    let _ = s
        .write_all(&Frame::new(h2::GOAWAY, 0, 0, vec![0; 8]).encode())
        .await;
    let _ = s.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::ServerName;

    async fn client(
        addr: SocketAddr,
        cert: &[u8],
        alpn: &[&[u8]],
    ) -> tokio_rustls::client::TlsStream<TcpStream> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(cert.to_vec())).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        cfg.alpn_protocols = alpn.iter().map(|a| a.to_vec()).collect();
        let tcp = TcpStream::connect(addr).await.unwrap();
        tokio_rustls::TlsConnector::from(Arc::new(cfg))
            .connect(ServerName::try_from("localhost").unwrap(), tcp)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn captures_h1() {
        let (srv, mut rx) = CaptureServer::bind("127.0.0.1:0", ServerConfig::default())
            .await
            .unwrap();
        let mut c = client(srv.local_addr(), srv.cert_der(), &[b"http/1.1"]).await;
        c.write_all(b"GET /x HTTP/1.1\r\nHost: localhost\r\nUser-Agent: t\r\n\r\n")
            .await
            .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).await.unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 200 OK"));
        assert!(resp.ends_with(b"lb-fptest ok\n"));

        let cap = rx.recv().await.unwrap();
        assert!(cap.error.is_none(), "{:?}", cap.error);
        let ch = cap.client_hello.unwrap();
        assert_eq!(ch.server_name.as_deref(), Some("localhost"));
        assert_eq!(ch.alpn, vec!["http/1.1"]);
        assert_eq!(
            ClientHello::parse_records(&cap.raw_client_hello).unwrap(),
            ch
        );
        assert_eq!(cap.negotiated_alpn.as_deref(), Some("http/1.1"));
        let h1 = cap.h1.unwrap();
        assert_eq!(h1.request_line, "GET /x HTTP/1.1");
        assert_eq!(h1.headers[1], ("User-Agent".into(), "t".into()));
    }

    #[tokio::test]
    async fn captures_h2_preface() {
        let (srv, mut rx) = CaptureServer::bind("127.0.0.1:0", ServerConfig::default())
            .await
            .unwrap();
        let mut c = client(srv.local_addr(), srv.cert_der(), &[b"h2"]).await;
        let mut out = h2::PREFACE.to_vec();
        let mut settings = Vec::new();
        for (id, v) in [(1u16, 65536u32), (2, 0), (4, 6291456), (6, 262144)] {
            settings.extend_from_slice(&id.to_be_bytes());
            settings.extend_from_slice(&v.to_be_bytes());
        }
        out.extend(Frame::new(h2::SETTINGS, 0, 0, settings).encode());
        out.extend(
            Frame::new(h2::WINDOW_UPDATE, 0, 0, 15663105u32.to_be_bytes().to_vec()).encode(),
        );
        // HEADERS with priority (exclusive, dep 0, weight 255), m,a,s,p,
        // split over a CONTINUATION frame.
        let mut p = 0x8000_0000u32.to_be_bytes().to_vec();
        p.push(255);
        p.extend_from_slice(&[0x82, 0x41, 0x09]);
        p.extend_from_slice(b"localhost");
        out.extend(Frame::new(h2::HEADERS, h2::FLAG_PRIORITY | h2::FLAG_END_STREAM, 1, p).encode());
        out.extend(
            Frame::new(h2::CONTINUATION, h2::FLAG_END_HEADERS, 1, vec![0x87, 0x84]).encode(),
        );
        c.write_all(&out).await.unwrap();

        let cap = rx.recv().await.unwrap();
        assert!(cap.error.is_none(), "{:?}", cap.error);
        assert_eq!(cap.negotiated_alpn.as_deref(), Some("h2"));
        let h = cap.h2.unwrap();
        assert_eq!(
            h.settings,
            vec![(1, 65536), (2, 0), (4, 6291456), (6, 262144)]
        );
        assert_eq!(h.window_update, Some(15663105));
        assert_eq!(
            h.pseudo_header_order,
            [":method", ":authority", ":scheme", ":path"]
        );
        assert_eq!(
            h.frame_order,
            ["SETTINGS", "WINDOW_UPDATE", "HEADERS", "CONTINUATION"]
        );
        let prio = h.headers_priority.unwrap();
        assert!(prio.exclusive && prio.weight == 255);
        assert_eq!(
            h.akamai(),
            "1:65536;2:0;4:6291456;6:262144|15663105|0|m,a,s,p"
        );

        // The server answered stream 1 after its SETTINGS and the ACK.
        let mut got = Vec::new();
        loop {
            let mut tmp = [0u8; 4096];
            let n = c.read(&mut tmp).await.unwrap();
            assert!(n > 0);
            got.extend_from_slice(&tmp[..n]);
            let mut rest = &got[..];
            let mut kinds = Vec::new();
            while let Some((f, used)) = Frame::parse(rest) {
                kinds.push((f.kind, f.flags));
                rest = &rest[used..];
            }
            if kinds.contains(&(h2::DATA, h2::FLAG_END_STREAM)) {
                assert_eq!(kinds[0], (h2::SETTINGS, 0));
                assert!(kinds.contains(&(h2::SETTINGS, h2::FLAG_ACK)));
                break;
            }
        }
    }

    #[tokio::test]
    async fn reports_non_tls_client() {
        let (srv, mut rx) = CaptureServer::bind("127.0.0.1:0", ServerConfig::default())
            .await
            .unwrap();
        let mut c = TcpStream::connect(srv.local_addr()).await.unwrap();
        c.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
        let cap = rx.recv().await.unwrap();
        assert!(cap.client_hello.is_none());
        assert!(cap.error.is_some());
    }
}
