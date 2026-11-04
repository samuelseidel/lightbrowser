//! WebSocket client (RFC 6455) as Chrome 153 runs it (ADR 0025): its own
//! connection (HTTP/1.1 only: ALPN offers `http/1.1` alone on `wss:`, the
//! persona's ClientHello otherwise), Chrome's handshake request with its
//! header order (captured from local Chrome on loopback), the context's
//! cookies (sent, and stored from the `101`), `permessage-deflate` offered
//! as Chrome offers it (`client_max_window_bits`) and used for every
//! message when the server accepts it, masked client frames, pings
//! answered, the closing handshake both ways.
//!
//! [`NetContext::websocket`] starts a connection on the network thread;
//! the caller talks to it through channels ([`WsCommand`] in,
//! [`WsEvent`] out), so a page or worker thread never blocks on it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use url::Url;

use crate::fetch::NetContext;

/// What a page asks for.
#[derive(Debug, Clone)]
pub struct WsConnect {
    /// `ws:` or `wss:` URL (fragment-free).
    pub url: Url,
    pub protocols: Vec<String>,
    /// The `Origin` header (the requesting document's origin).
    pub origin: String,
}

/// From the page to the connection.
#[derive(Debug, Clone)]
pub enum WsCommand {
    Text(String),
    Binary(Vec<u8>),
    /// Start the closing handshake (`None`: a close frame without a code).
    Close(Option<u16>, String),
}

/// From the connection to the page.
#[derive(Debug, Clone, PartialEq)]
pub enum WsEvent {
    /// The handshake succeeded.
    Open {
        protocol: String,
        extensions: String,
    },
    Text(String),
    Binary(Vec<u8>),
    /// This many bytes of queued messages went on the wire
    /// (`bufferedAmount`).
    Flushed(usize),
    /// The connection failed (handshake or protocol error); the message
    /// is Chrome's console text after "WebSocket connection to 'url'
    /// failed: ". A `Closed` with 1006 follows.
    Failed(String),
    /// The connection is closed.
    Closed {
        code: u16,
        reason: String,
        clean: bool,
    },
}

impl NetContext {
    /// Open a WebSocket on the network thread (or this thread's runtime
    /// when the context has no pool). Events arrive on the returned
    /// receiver; commands go to the returned sender.
    pub fn websocket(
        &mut self,
        req: WsConnect,
    ) -> (UnboundedSender<WsCommand>, UnboundedReceiver<WsEvent>) {
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let (ev_tx, ev_rx) = unbounded_channel();
        let tls = if req.url.scheme() == "wss" {
            self.tls_connector().ok()
        } else {
            None
        };
        let ctx = self.detached();
        let fut = async move {
            run(ctx, tls, req, cmd_rx, ev_tx).await;
        };
        if self.pool.is_some() {
            crate::pool::net_runtime().spawn(fut);
        } else {
            tokio::spawn(fut);
        }
        (cmd_tx, ev_rx)
    }
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

async fn run(
    ctx: NetContext,
    tls: Option<Arc<crate::tls::TlsConnector>>,
    req: WsConnect,
    cmds: UnboundedReceiver<WsCommand>,
    events: UnboundedSender<WsEvent>,
) {
    let fail = |events: &UnboundedSender<WsEvent>, msg: String| {
        let _ = events.send(WsEvent::Failed(msg));
        let _ = events.send(WsEvent::Closed {
            code: 1006,
            reason: String::new(),
            clean: false,
        });
    };
    let (stream, open) = match handshake(&ctx, tls, &req).await {
        Ok(v) => v,
        Err(msg) => return fail(&events, msg),
    };
    let _ = events.send(WsEvent::Open {
        protocol: open.protocol.clone(),
        extensions: open.extensions.clone(),
    });
    let (rd, wr) = tokio::io::split(stream);
    let (ctl_tx, ctl_rx) = unbounded_channel::<Ctl>();
    let close_sent = Arc::new(AtomicBool::new(false));
    let writer = write_loop(
        wr,
        cmds,
        ctl_rx,
        events.clone(),
        open.deflate.clone(),
        close_sent.clone(),
    );
    let reader = read_loop(rd, ctl_tx, events.clone(), open.deflate, close_sent);
    tokio::pin!(writer, reader);
    let mut writer_done = false;
    let r = loop {
        tokio::select! {
            r = &mut reader => break r,
            _ = &mut writer, if !writer_done => writer_done = true,
        }
    };
    match r {
        ReadEnd::Closed { code, reason } => {
            let _ = events.send(WsEvent::Closed {
                code,
                reason,
                clean: true,
            });
        }
        ReadEnd::Dropped => {
            let _ = events.send(WsEvent::Closed {
                code: 1006,
                reason: String::new(),
                clean: false,
            });
        }
        ReadEnd::Failed(msg) => fail(&events, msg),
    }
}

/// Negotiated `permessage-deflate` parameters (`None`: not in use).
#[derive(Clone, Default)]
struct Deflate {
    on: bool,
    /// We may compress (the server allowed our full window).
    compress: bool,
    client_no_context: bool,
    server_no_context: bool,
}

struct Opened {
    protocol: String,
    extensions: String,
    deflate: Deflate,
}

fn b64(b: &[u8]) -> String {
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

/// The `http(s):` URL cookies are looked up and stored for.
fn cookie_url(u: &Url) -> Url {
    let mut c = u.clone();
    let _ = c.set_scheme(if u.scheme() == "wss" { "https" } else { "http" });
    c
}

async fn handshake(
    ctx: &NetContext,
    tls: Option<Arc<crate::tls::TlsConnector>>,
    req: &WsConnect,
) -> Result<(Box<dyn Io>, Opened), String> {
    let url = &req.url;
    let host = url
        .host_str()
        .ok_or_else(|| "Invalid URL".to_owned())?
        .to_owned();
    let port = url.port_or_known_default().unwrap_or(80);
    let tcp =
        ctx.dialer.connect(&host, port).await.map_err(|_| {
            "Error in connection establishment: net::ERR_CONNECTION_REFUSED".to_owned()
        })?;
    let mut stream: Box<dyn Io> = match tls {
        Some(c) if url.scheme() == "wss" => {
            let (s, _) = c.connect_http1_only(&host, tcp).await.map_err(|_| {
                "Error in connection establishment: net::ERR_SSL_PROTOCOL_ERROR".to_owned()
            })?;
            Box::new(s)
        }
        _ if url.scheme() == "wss" => return Err("Error in connection establishment".into()),
        _ => Box::new(tcp),
    };
    let mut key = [0u8; 16];
    boring::rand::rand_bytes(&mut key).map_err(|e| e.to_string())?;
    let key = b64(&key);
    let mut path = url.path().to_owned();
    if let Some(q) = url.query() {
        path.push('?');
        path.push_str(q);
    }
    let host_header = match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.clone(),
    };
    let cookie = ctx.cookies.header_for(&cookie_url(url));
    // Chrome 153's handshake, in its order.
    let mut lines = vec![
        format!("GET {path} HTTP/1.1"),
        format!("Host: {host_header}"),
        "Connection: Upgrade".into(),
        "Pragma: no-cache".into(),
        "Cache-Control: no-cache".into(),
        format!("User-Agent: {}", ctx.persona.user_agent),
        "Upgrade: websocket".into(),
        format!("Origin: {}", req.origin),
        "Sec-WebSocket-Version: 13".into(),
        "Accept-Encoding: gzip, deflate, br, zstd".into(),
        format!("Accept-Language: {}", ctx.persona.accept_language),
    ];
    if let Some(c) = cookie {
        lines.push(format!("Cookie: {c}"));
    }
    lines.push(format!("Sec-WebSocket-Key: {key}"));
    lines.push("Sec-WebSocket-Extensions: permessage-deflate; client_max_window_bits".into());
    if !req.protocols.is_empty() {
        lines.push(format!(
            "Sec-WebSocket-Protocol: {}",
            req.protocols.join(", ")
        ));
    }
    let mut head = lines.join("\r\n");
    head.push_str("\r\n\r\n");
    stream
        .write_all(head.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    // The response head, byte by byte past it (frames may follow at once).
    let mut buf = Vec::with_capacity(512);
    loop {
        let mut b = [0u8; 1];
        let n = stream
            .read(&mut b)
            .await
            .map_err(|_| "Connection closed before receiving a handshake response".to_owned())?;
        if n == 0 {
            return Err("Connection closed before receiving a handshake response".into());
        }
        buf.push(b[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
        if buf.len() > 256 * 1024 {
            return Err("Error during WebSocket handshake: Response header too large".into());
        }
    }
    let mut hs = [httparse::EMPTY_HEADER; 64];
    let mut resp = httparse::Response::new(&mut hs);
    resp.parse(&buf)
        .map_err(|_| "Error during WebSocket handshake: Invalid status line".to_owned())?;
    let status = resp.code.unwrap_or(0);
    let headers: Vec<(String, String)> = resp
        .headers
        .iter()
        .map(|h| {
            (
                h.name.to_ascii_lowercase(),
                String::from_utf8_lossy(h.value).trim().to_owned(),
            )
        })
        .collect();
    let set_cookie: Vec<String> = headers
        .iter()
        .filter(|(k, _)| k == "set-cookie")
        .map(|(_, v)| v.clone())
        .collect();
    if !set_cookie.is_empty() {
        let mut jar = ctx.cookies.clone();
        jar.store_response(&cookie_url(url), &set_cookie);
    }
    if status != 101 {
        return Err(format!(
            "Error during WebSocket handshake: Unexpected response code: {status}"
        ));
    }
    let get = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| k == n)
            .map(|(_, v)| v.as_str())
    };
    if !get("upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket")) {
        return Err("Error during WebSocket handshake: 'Upgrade' header is missing".into());
    }
    if !get("connection").is_some_and(|v| {
        v.split(',')
            .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
    }) {
        return Err("Error during WebSocket handshake: 'Connection' header is missing".into());
    }
    let want = boring::hash::hash(
        boring::hash::MessageDigest::sha1(),
        format!("{key}{GUID}").as_bytes(),
    )
    .map(|d| b64(&d))
    .map_err(|e| e.to_string())?;
    if get("sec-websocket-accept") != Some(want.as_str()) {
        return Err(
            "Error during WebSocket handshake: Incorrect 'Sec-WebSocket-Accept' header value"
                .into(),
        );
    }
    let protocol = get("sec-websocket-protocol").unwrap_or("").to_owned();
    if !protocol.is_empty() && !req.protocols.contains(&protocol) {
        return Err(if req.protocols.is_empty() {
            format!(
                "Error during WebSocket handshake: Response must not include 'Sec-WebSocket-Protocol' header if not present in request: {protocol}"
            )
        } else {
            format!(
                "Error during WebSocket handshake: 'Sec-WebSocket-Protocol' header value '{protocol}' in response does not match any of sent values"
            )
        });
    }
    if protocol.is_empty() && !req.protocols.is_empty() {
        return Err("Error during WebSocket handshake: Sent non-empty 'Sec-WebSocket-Protocol' header but no response was received".into());
    }
    let extensions = get("sec-websocket-extensions").unwrap_or("").to_owned();
    let mut deflate = Deflate::default();
    if !extensions.is_empty() {
        let mut parts = extensions.split(';').map(str::trim);
        if parts.next() != Some("permessage-deflate") || extensions.contains(',') {
            return Err(format!(
                "Error during WebSocket handshake: Found an unsupported extension 'x' in 'Sec-WebSocket-Extensions' header: {extensions}"
            ));
        }
        deflate.on = true;
        deflate.compress = true;
        for p in parts {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            match k.trim() {
                "server_no_context_takeover" => deflate.server_no_context = true,
                "client_no_context_takeover" => deflate.client_no_context = true,
                "client_max_window_bits" => {
                    if v.trim().trim_matches('"').parse::<u8>().unwrap_or(15) < 15 {
                        deflate.compress = false;
                    }
                }
                "server_max_window_bits" => {}
                other => {
                    return Err(format!(
                        "Error during WebSocket handshake: Received an unexpected permessage-deflate extension parameter: {other}"
                    ));
                }
            }
        }
    }
    Ok((
        stream,
        Opened {
            protocol,
            extensions,
            deflate,
        },
    ))
}

enum Ctl {
    Pong(Vec<u8>),
    /// Answer the server's close frame with this code.
    CloseEcho(Option<u16>),
}

enum ReadEnd {
    Closed { code: u16, reason: String },
    Dropped,
    Failed(String),
}

/// A client frame (masked).
fn frame(op: u8, rsv1: bool, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(0x80 | if rsv1 { 0x40 } else { 0 } | op);
    let n = payload.len();
    if n < 126 {
        out.push(0x80 | n as u8);
    } else if n < 65536 {
        out.push(0x80 | 126);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(n as u64).to_be_bytes());
    }
    let mut mask = [0u8; 4];
    let _ = boring::rand::rand_bytes(&mut mask);
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

fn deflate_message(c: &mut Compress, data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    let mut input = data;
    loop {
        out.reserve(4096);
        let before_in = c.total_in();
        c.compress_vec(input, &mut out, FlushCompress::Sync).ok()?;
        let used = (c.total_in() - before_in) as usize;
        input = &input[used..];
        if input.is_empty() && out.len() < out.capacity() {
            break;
        }
    }
    if out.ends_with(&[0, 0, 0xff, 0xff]) {
        out.truncate(out.len() - 4);
    }
    Some(out)
}

fn inflate_message(d: &mut Decompress, data: &[u8]) -> Option<Vec<u8>> {
    let mut input = data.to_vec();
    input.extend_from_slice(&[0, 0, 0xff, 0xff]);
    let mut out = Vec::with_capacity(data.len() * 3 + 64);
    let mut pos = 0;
    loop {
        out.reserve(8192);
        let before_in = d.total_in();
        let st = d
            .decompress_vec(&input[pos..], &mut out, FlushDecompress::Sync)
            .ok()?;
        pos += (d.total_in() - before_in) as usize;
        if pos >= input.len() && out.len() < out.capacity() {
            break;
        }
        if st == Status::StreamEnd {
            break;
        }
        if out.len() > 64 << 20 {
            return None;
        }
    }
    Some(out)
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut wr: W,
    mut cmds: UnboundedReceiver<WsCommand>,
    mut ctl: UnboundedReceiver<Ctl>,
    events: UnboundedSender<WsEvent>,
    deflate: Deflate,
    close_sent: Arc<AtomicBool>,
) {
    let mut comp = Compress::new(Compression::default(), false);
    loop {
        let (bytes, counted) = tokio::select! {
            c = ctl.recv() => match c {
                Some(Ctl::Pong(p)) => (frame(0xA, false, &p), 0),
                Some(Ctl::CloseEcho(code)) => {
                    if close_sent.swap(true, Ordering::SeqCst) {
                        continue;
                    }
                    let payload = code.map(|c| c.to_be_bytes().to_vec()).unwrap_or_default();
                    (frame(0x8, false, &payload), 0)
                }
                None => return,
            },
            c = cmds.recv() => match c {
                Some(WsCommand::Text(_) | WsCommand::Binary(_)) if close_sent.load(Ordering::SeqCst) => {
                    continue;
                }
                Some(WsCommand::Text(s)) => (data_frame(1, s.as_bytes(), &deflate, &mut comp), s.len()),
                Some(WsCommand::Binary(b)) => (data_frame(2, &b, &deflate, &mut comp), b.len()),
                Some(WsCommand::Close(code, reason)) => {
                    if close_sent.swap(true, Ordering::SeqCst) {
                        continue;
                    }
                    let mut payload = Vec::new();
                    if let Some(c) = code {
                        payload.extend_from_slice(&c.to_be_bytes());
                        payload.extend_from_slice(reason.as_bytes());
                    }
                    (frame(0x8, false, &payload), 0)
                }
                None => {
                    // The page went away: close as Chrome does on unload.
                    if !close_sent.swap(true, Ordering::SeqCst) {
                        let _ = wr.write_all(&frame(0x8, false, &1001u16.to_be_bytes())).await;
                    }
                    return;
                }
            },
        };
        if wr.write_all(&bytes).await.is_err() || wr.flush().await.is_err() {
            return;
        }
        if counted > 0 {
            let _ = events.send(WsEvent::Flushed(counted));
        }
    }
}

fn data_frame(op: u8, data: &[u8], d: &Deflate, comp: &mut Compress) -> Vec<u8> {
    if d.on && d.compress {
        if d.client_no_context {
            comp.reset();
        }
        if let Some(c) = deflate_message(comp, data) {
            return frame(op, true, &c);
        }
    }
    frame(op, false, data)
}

async fn read_loop<R: AsyncRead + Unpin>(
    mut rd: R,
    ctl: UnboundedSender<Ctl>,
    events: UnboundedSender<WsEvent>,
    deflate: Deflate,
    close_sent: Arc<AtomicBool>,
) -> ReadEnd {
    let mut inflater = Decompress::new(false);
    let mut msg: Vec<u8> = Vec::new();
    let mut msg_op = 0u8;
    let mut msg_compressed = false;
    loop {
        let mut h = [0u8; 2];
        if rd.read_exact(&mut h).await.is_err() {
            return ReadEnd::Dropped;
        }
        let fin = h[0] & 0x80 != 0;
        let rsv1 = h[0] & 0x40 != 0;
        let op = h[0] & 0x0f;
        if h[0] & 0x30 != 0 || (rsv1 && (!deflate.on || op == 0 || op >= 8)) {
            return ReadEnd::Failed(
                "One or more reserved bits are on: reserved1 = 1, reserved2 = 0, reserved3 = 0"
                    .into(),
            );
        }
        if h[1] & 0x80 != 0 {
            return ReadEnd::Failed(
                "A server must not mask any frames that it sends to the client.".into(),
            );
        }
        let mut len = u64::from(h[1] & 0x7f);
        if len == 126 {
            let mut b = [0u8; 2];
            if rd.read_exact(&mut b).await.is_err() {
                return ReadEnd::Dropped;
            }
            len = u64::from(u16::from_be_bytes(b));
        } else if len == 127 {
            let mut b = [0u8; 8];
            if rd.read_exact(&mut b).await.is_err() {
                return ReadEnd::Dropped;
            }
            len = u64::from_be_bytes(b);
        }
        if len > 256 << 20 {
            return ReadEnd::Failed("Max frame length exceeded".into());
        }
        let mut payload = vec![0u8; len as usize];
        if rd.read_exact(&mut payload).await.is_err() {
            return ReadEnd::Dropped;
        }
        match op {
            0x8 => {
                let (code, reason) = if payload.len() >= 2 {
                    let c = u16::from_be_bytes([payload[0], payload[1]]);
                    match std::str::from_utf8(&payload[2..]) {
                        Ok(r) => (c, r.to_owned()),
                        Err(_) => {
                            return ReadEnd::Failed(
                                "Received a close frame with an invalid close reason.".into(),
                            );
                        }
                    }
                } else {
                    (1005, String::new())
                };
                if !close_sent.load(Ordering::SeqCst) {
                    let echo = (code != 1005).then_some(code);
                    let _ = ctl.send(Ctl::CloseEcho(echo));
                }
                // Wait (briefly) for the server to close the TCP connection.
                let mut sink = [0u8; 512];
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    while let Ok(n) = rd.read(&mut sink).await {
                        if n == 0 {
                            break;
                        }
                    }
                })
                .await;
                return ReadEnd::Closed { code, reason };
            }
            0x9 => {
                let _ = ctl.send(Ctl::Pong(payload));
            }
            0xA => {}
            0x0..=0x2 => {
                if op != 0 {
                    msg_op = op;
                    msg_compressed = rsv1;
                    msg.clear();
                } else if msg_op == 0 {
                    return ReadEnd::Failed("Received unexpected continuation frame.".into());
                }
                msg.extend_from_slice(&payload);
                if !fin {
                    continue;
                }
                let data = if msg_compressed {
                    if deflate.server_no_context {
                        inflater.reset(false);
                    }
                    match inflate_message(&mut inflater, &msg) {
                        Some(d) => d,
                        None => {
                            return ReadEnd::Failed("Received a broken compressed message.".into());
                        }
                    }
                } else {
                    std::mem::take(&mut msg)
                };
                msg.clear();
                let ev = if msg_op == 1 {
                    match String::from_utf8(data) {
                        Ok(s) => WsEvent::Text(s),
                        Err(_) => {
                            return ReadEnd::Failed(
                                "Could not decode a text frame as UTF-8.".into(),
                            );
                        }
                    }
                } else {
                    WsEvent::Binary(data)
                };
                msg_op = 0;
                let _ = events.send(ev);
            }
            _ => {
                return ReadEnd::Failed(format!("Unrecognized frame opcode: {op}"));
            }
        }
    }
}
