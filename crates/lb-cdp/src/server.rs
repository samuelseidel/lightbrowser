//! Transport: one TCP listener on loopback that answers the DevTools HTTP
//! endpoints and upgrades `/devtools/...` to WebSocket.
//!
//! - `GET /json/version`: browser info with `webSocketDebuggerUrl`.
//! - `GET /json/list` (or `/json`): page targets.
//! - `GET /json/protocol`: the domains, commands and events we answer.
//! - `PUT|GET /json/new?<url>`: opens a page in the default context.
//! - `GET /json/close/<id>`, `/json/activate/<id>`.
//! - `ws://host/devtools/browser/<id>`: a browser connection (flat sessions).
//! - `ws://host/devtools/page/<targetId>`: a connection bound to one page.
//!
//! Like Chrome, requests whose `Host` is not an IP or `localhost` are
//! refused (DNS rebinding), and WebSocket upgrades that carry an `Origin`
//! header are refused unless the origin is allowed. That keeps web pages
//! from driving the browser.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::pin;
use std::rc::Rc;
use std::task::Poll;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;

use crate::browser::{ConnId, Shared, dispatch};
use crate::host::{HostError, HostEvent, PageHost};
use crate::protocol::{self, Command};
use crate::types::TargetInfo;

/// Everything bound for a client goes through one queue, in order.
#[derive(Debug)]
pub(crate) enum Outgoing {
    /// A page event from the host; routed when processed.
    Host(Box<HostEvent>),
    /// A ready message for one connection.
    Send(ConnId, String),
}

/// Server options.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Must be a loopback address. Port 0 picks a free port.
    pub addr: SocketAddr,
    /// `Origin` values allowed to open a WebSocket (`*` allows all).
    /// Clients like Playwright send no `Origin` and are always allowed.
    pub allow_origins: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            addr: SocketAddr::from(([127, 0, 0, 1], 9222)),
            allow_origins: Vec::new(),
        }
    }
}

/// A bound CDP server. Run it inside a tokio `LocalSet`.
pub struct Server<H> {
    shared: Rc<Shared<H>>,
    listener: TcpListener,
    rx: mpsc::UnboundedReceiver<Outgoing>,
    config: ServerConfig,
}

impl<H: PageHost + 'static> Server<H> {
    /// Binds the listener and creates the default browser context.
    pub async fn bind(config: ServerConfig, host: Rc<H>) -> io::Result<Server<H>> {
        if !config.addr.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "refusing to listen on {}: the CDP server binds to loopback only",
                    config.addr
                ),
            ));
        }
        let listener = TcpListener::bind(config.addr).await?;
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Shared::new(host, tx).map_err(|e: HostError| io::Error::other(e.0))?;
        Ok(Server {
            shared,
            listener,
            rx,
            config,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.listener.local_addr().expect("bound listener")
    }

    /// `ws://127.0.0.1:<port>/devtools/browser/<id>`.
    pub fn browser_ws_url(&self) -> String {
        format!(
            "ws://{}/devtools/browser/{}",
            self.local_addr(),
            self.shared.browser_ws_id
        )
    }

    /// Serves until `Browser.close`. Must run inside a `LocalSet`.
    pub async fn run(self) -> io::Result<()> {
        let Server {
            shared,
            listener,
            mut rx,
            config,
        } = self;
        let pump_shared = shared.clone();
        tokio::task::spawn_local(async move {
            while let Some(item) = rx.recv().await {
                pump_shared.process(item);
            }
        });
        let mut shutdown = shared.shutdown.subscribe();
        let config = Rc::new(config);
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, _) = match accepted {
                        Ok(a) => a,
                        Err(e) if e.kind() == io::ErrorKind::ConnectionAborted => continue,
                        Err(e) => return Err(e),
                    };
                    // CDP traffic is many small frames written back to back
                    // (an event, then a reply). With Nagle on, the second
                    // waits for the client's delayed ACK (40 ms on Linux).
                    // Chrome's sockets set TCP_NODELAY too.
                    let _ = stream.set_nodelay(true);
                    let sh = shared.clone();
                    let cfg = config.clone();
                    tokio::task::spawn_local(async move {
                        let _ = handle_connection(sh, cfg, stream).await;
                    });
                }
                _ = shutdown.changed() => {
                    // Let queued replies reach their sockets.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    return Ok(());
                }
            }
        }
    }
}

/// A parsed HTTP request head.
struct RequestHead {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl RequestHead {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

async fn read_head(stream: &mut TcpStream) -> io::Result<RequestHead> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            buf.truncate(end);
            break;
        }
        if buf.len() > 16 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request head too large",
            ));
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("").to_string();
    let path = first.next().unwrap_or("/").to_string();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))
        .collect();
    Ok(RequestHead {
        method,
        path,
        headers,
    })
}

/// Chrome's DNS-rebinding guard: the Host must be an IP or `localhost`.
fn host_allowed(host: Option<&str>) -> bool {
    let Some(h) = host else { return true };
    let name = if let Some(rest) = h.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        h.rsplit_once(':').map_or(h, |(n, _)| n)
    };
    name.eq_ignore_ascii_case("localhost")
        || name.parse::<std::net::IpAddr>().is_ok()
        || name.to_ascii_lowercase().ends_with(".localhost")
}

async fn write_http(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> io::Result<()> {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.shutdown().await
}

async fn write_json(stream: &mut TcpStream, v: &Value) -> io::Result<()> {
    let body = serde_json::to_string_pretty(v).unwrap_or_default();
    write_http(stream, "200 OK", "application/json; charset=UTF-8", &body).await
}

fn target_json(info: &TargetInfo, host: &str) -> Value {
    let ws = format!("{host}/devtools/page/{}", info.target_id);
    json!({
        "description": "",
        "devtoolsFrontendUrl": format!("/devtools/inspector.html?ws={ws}"),
        "id": info.target_id,
        "title": info.title,
        "type": info.kind,
        "url": info.url,
        "webSocketDebuggerUrl": format!("ws://{ws}"),
    })
}

async fn handle_connection<H: PageHost + 'static>(
    shared: Rc<Shared<H>>,
    config: Rc<ServerConfig>,
    mut stream: TcpStream,
) -> io::Result<()> {
    let head = tokio::time::timeout(Duration::from_secs(10), read_head(&mut stream))
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    if !host_allowed(head.header("host")) {
        return write_http(
            &mut stream,
            "500 Internal Server Error",
            "text/plain",
            "Host header is specified and is not an IP address or localhost.",
        )
        .await;
    }
    let host = head.header("host").map(str::to_string).unwrap_or_else(|| {
        stream
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_default()
    });
    let is_upgrade = head
        .header("upgrade")
        .is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
    let (path, query) = match head.path.split_once('?') {
        Some((p, q)) => (p.to_string(), Some(q.to_string())),
        None => (head.path.clone(), None),
    };

    if is_upgrade {
        if let Some(origin) = head.header("origin")
            && !config.allow_origins.iter().any(|o| o == "*" || o == origin)
        {
            let msg = format!(
                "Rejected an incoming WebSocket connection from the {origin} origin. \
Use --remote-allow-origins={origin} to allow connections from this origin."
            );
            return write_http(&mut stream, "403 Forbidden", "text/plain", &msg).await;
        }
        let page = if let Some(id) = path.strip_prefix("/devtools/page/") {
            if shared.state.borrow().target(id).is_none() {
                return write_http(
                    &mut stream,
                    "404 Not Found",
                    "text/plain",
                    "No such target id",
                )
                .await;
            }
            Some(id.to_string())
        } else if path.starts_with("/devtools/browser") {
            None
        } else {
            return write_http(&mut stream, "404 Not Found", "text/plain", "Unknown path").await;
        };
        let Some(key) = head.header("sec-websocket-key") else {
            return write_http(
                &mut stream,
                "400 Bad Request",
                "text/plain",
                "Missing Sec-WebSocket-Key",
            )
            .await;
        };
        let accept = derive_accept_key(key.as_bytes());
        let resp = format!(
            "HTTP/1.1 101 WebSocket Protocol Handshake\r\nUpgrade: WebSocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        stream.write_all(resp.as_bytes()).await?;
        let ws = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
        serve_ws(shared, ws, page).await;
        return Ok(());
    }

    match (head.method.as_str(), path.trim_end_matches('/')) {
        (_, "/json/version") => {
            let v = shared.version_info();
            let body = json!({
                "Browser": v["product"],
                "Protocol-Version": "1.3",
                "User-Agent": v["userAgent"],
                "V8-Version": v["jsVersion"],
                "WebKit-Version": "537.36",
                "webSocketDebuggerUrl": format!("ws://{host}/devtools/browser/{}", shared.browser_ws_id),
            });
            write_json(&mut stream, &body).await
        }
        (_, "/json" | "/json/list") => {
            let list: Vec<Value> = shared
                .page_infos()
                .iter()
                .map(|i| target_json(i, &host))
                .collect();
            write_json(&mut stream, &Value::Array(list)).await
        }
        (_, "/json/protocol") => write_json(&mut stream, &crate::methods::protocol_json()).await,
        ("PUT" | "GET", "/json/new") => {
            let url = query.unwrap_or_else(|| "about:blank".into());
            match shared.create_target(None, &url).await {
                Ok(id) => {
                    let info = shared.page_infos().into_iter().find(|i| i.target_id == id);
                    match info {
                        Some(i) => write_json(&mut stream, &target_json(&i, &host)).await,
                        None => {
                            write_http(
                                &mut stream,
                                "500 Internal Server Error",
                                "text/plain",
                                "target closed",
                            )
                            .await
                        }
                    }
                }
                Err(e) => {
                    write_http(
                        &mut stream,
                        "500 Internal Server Error",
                        "text/plain",
                        &e.message,
                    )
                    .await
                }
            }
        }
        (_, p) if p.starts_with("/json/close/") => {
            let id = &p["/json/close/".len()..];
            match shared.close_target(id) {
                Ok(()) => {
                    write_http(&mut stream, "200 OK", "text/plain", "Target is closing").await
                }
                Err(_) => {
                    write_http(
                        &mut stream,
                        "404 Not Found",
                        "text/plain",
                        &format!("No such target id: {id}"),
                    )
                    .await
                }
            }
        }
        (_, p) if p.starts_with("/json/activate/") => {
            let id = &p["/json/activate/".len()..];
            if shared.state.borrow().target(id).is_some() {
                write_http(&mut stream, "200 OK", "text/plain", "Target activated").await
            } else {
                write_http(
                    &mut stream,
                    "404 Not Found",
                    "text/plain",
                    &format!("No such target id: {id}"),
                )
                .await
            }
        }
        _ => write_http(&mut stream, "404 Not Found", "text/plain", "Unknown path").await,
    }
}

async fn serve_ws<H: PageHost + 'static>(
    shared: Rc<Shared<H>>,
    ws: WebSocketStream<TcpStream>,
    page: Option<String>,
) {
    let (mut sink, mut source) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let conn = shared.open_conn(tx, page);
    let writer = tokio::task::spawn_local(async move {
        while let Some(text) = rx.recv().await {
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let mut shutdown = shared.shutdown.subscribe();
    loop {
        let msg = tokio::select! {
            m = source.next() => m,
            _ = shutdown.changed() => break,
        };
        let text = match msg {
            Some(Ok(Message::Text(t))) => t.to_string(),
            Some(Ok(Message::Binary(b))) => String::from_utf8_lossy(&b).into_owned(),
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
            Some(Ok(_)) => continue,
        };
        run_command(shared.clone(), conn, text).await;
    }
    shared.close_conn(conn);
    // Dropping the connection's sender ends the writer.
    let _ = tokio::time::timeout(Duration::from_millis(200), writer).await;
}

/// Runs a command. If it completes without waiting it is answered in line,
/// keeping replies in order; otherwise it continues on its own task so a
/// slow call (a navigation, an awaited promise) does not block the socket.
async fn run_command<H: PageHost + 'static>(shared: Rc<Shared<H>>, conn: ConnId, text: String) {
    let mut fut = Box::pin(answer(shared, conn, text));
    let ready = std::future::poll_fn(|cx| Poll::Ready(fut.as_mut().poll(cx).is_ready())).await;
    if !ready {
        tokio::task::spawn_local(fut);
    }
}

async fn answer<H: PageHost + 'static>(shared: Rc<Shared<H>>, conn: ConnId, text: String) {
    let reply = match Command::parse(&text) {
        Err(f) => protocol::error_response(f.id, f.session_id.as_deref(), &f.error),
        Ok(cmd) => {
            let sid = cmd.session_id.as_deref();
            match dispatch(&shared, conn, &cmd).await {
                Ok(v) => protocol::response(cmd.id, sid, v),
                Err(e) => protocol::error_response(Some(cmd.id), sid, &e),
            }
        }
    };
    shared.queue_text(conn, reply);
}

/// Runs `fut` on a fresh current-thread runtime inside a `LocalSet`, the
/// threading model the server needs.
pub fn block_on_local<F: Future>(fut: F) -> io::Result<F::Output> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let local = tokio::task::LocalSet::new();
    Ok(local.block_on(&rt, async move {
        let fut = pin!(fut);
        fut.await
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_guard() {
        assert!(host_allowed(Some("127.0.0.1:9222")));
        assert!(host_allowed(Some("localhost:9222")));
        assert!(host_allowed(Some("[::1]:9222")));
        assert!(host_allowed(None));
        assert!(!host_allowed(Some("evil.example:9222")));
    }
}
