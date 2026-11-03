//! Engine-neutral types the page layer provides to the Web API
//! implementations: network access, storage areas, request destinations.
//! Compiled for both families (lb-page uses them in either build).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use url::Url;

/// A boxed, single-threaded future (pages live on one worker thread).
pub type LocalFuture<T> = Pin<Box<dyn Future<Output = T>>>;

/// What a request is for. Picks `Accept` and `Sec-Fetch-*` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    Document,
    Iframe,
    Script,
    Style,
    Image,
    /// `fetch()` and `XMLHttpRequest`.
    Empty,
}

impl Destination {
    /// `Sec-Fetch-Dest` value.
    pub fn as_str(self) -> &'static str {
        match self {
            Destination::Document => "document",
            Destination::Iframe => "iframe",
            Destination::Script => "script",
            Destination::Style => "style",
            Destination::Image => "image",
            Destination::Empty => "empty",
        }
    }
}

/// A request from the page (subresource, `fetch`, XHR, navigation).
#[derive(Debug, Clone)]
pub struct NetRequest {
    pub method: String,
    pub url: Url,
    /// Headers set by script (`fetch` init, `setRequestHeader`), in order.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub destination: Destination,
    /// The document that made the request (`None` for a browser-initiated
    /// navigation).
    pub initiator: Option<Url>,
    /// `fetch` / XHR mode: `cors`, `no-cors`, `same-origin`, `navigate`.
    pub mode: &'static str,
    /// A navigation started by trusted user input (`Sec-Fetch-User: ?1`).
    pub user_activated: bool,
}

impl NetRequest {
    pub fn get(url: Url, destination: Destination, initiator: Option<Url>) -> Self {
        NetRequest {
            method: "GET".into(),
            url,
            headers: Vec::new(),
            body: Vec::new(),
            destination,
            initiator,
            mode: match destination {
                Destination::Document | Destination::Iframe => "navigate",
                Destination::Empty => "cors",
                _ => "no-cors",
            },
            user_activated: false,
        }
    }
}

/// A response, body decoded (`Content-Encoding`), not yet charset-decoded.
#[derive(Debug, Clone)]
pub struct NetResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    /// Shared, not copied: the page and the CDP body store
    /// (`Network.getResponseBody`) hold the same bytes.
    pub body: bytes::Bytes,
    /// URL after redirects.
    pub url: Url,
    pub redirected: bool,
}

impl NetResponse {
    /// First value of header `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Network and scheduling services of the browser context, provided by
/// the page layer.
pub trait NetHost {
    /// Perform a request (cookies, redirects, persona headers).
    fn fetch(&self, req: NetRequest) -> LocalFuture<Result<NetResponse, String>>;
    /// Run a future on the page's worker thread.
    fn spawn(&self, fut: LocalFuture<()>);
    /// `document.cookie` getter: non-HttpOnly cookies for `url`.
    fn cookie_string(&self, url: &Url) -> String;
    /// `document.cookie` setter.
    fn set_cookie_string(&self, url: &Url, cookie: &str);
    /// Open a WebSocket connection (ADR 0025). `None`: this host has no
    /// network for it (the page fails the connection).
    fn websocket(&self, open: WsOpen) -> Option<WsChannels> {
        let _ = open;
        None
    }
    /// The window's service worker answered `req` (`respondWith`, or a
    /// network error): the CDP layer reports it with `fromServiceWorker`.
    fn sw_answered(&self, req: &NetRequest, r: &Result<NetResponse, String>) {
        let _ = (req, r);
    }
}

/// A WebSocket the page opens.
#[derive(Debug, Clone)]
pub struct WsOpen {
    pub url: Url,
    pub protocols: Vec<String>,
    /// `Origin` header: the document's origin.
    pub origin: String,
}

/// From the page to a WebSocket connection.
#[derive(Debug, Clone)]
pub enum WsCommand {
    Text(String),
    Binary(Vec<u8>),
    Close(Option<u16>, String),
}

/// From a WebSocket connection to the page.
#[derive(Debug, Clone, PartialEq)]
pub enum WsEvent {
    Open {
        protocol: String,
        extensions: String,
    },
    Text(String),
    Binary(Vec<u8>),
    /// Bytes of queued messages that went on the wire.
    Flushed(usize),
    /// The connection failed (Chrome's console detail); `Closed` follows.
    Failed(String),
    Closed {
        code: u16,
        reason: String,
        clean: bool,
    },
}

/// The two ends a page holds of a WebSocket connection.
pub struct WsChannels {
    pub commands: tokio::sync::mpsc::UnboundedSender<WsCommand>,
    pub events: tokio::sync::mpsc::UnboundedReceiver<WsEvent>,
}

/// One `localStorage` / `sessionStorage` area: insertion-ordered pairs,
/// shared by every window of the same origin (and context). In memory only.
#[derive(Debug, Clone, Default)]
pub struct StorageArea(Arc<Mutex<Vec<(String, String)>>>);

impl StorageArea {
    pub fn new() -> Self {
        Self::default()
    }

    fn with<R>(&self, f: impl FnOnce(&mut Vec<(String, String)>) -> R) -> R {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut g)
    }

    pub fn len(&self) -> usize {
        self.with(|v| v.len())
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn get(&self, key: &str) -> Option<String> {
        self.with(|v| v.iter().find(|(k, _)| k == key).map(|(_, x)| x.clone()))
    }
    pub fn key(&self, i: usize) -> Option<String> {
        self.with(|v| v.get(i).map(|(k, _)| k.clone()))
    }
    pub fn keys(&self) -> Vec<String> {
        self.with(|v| v.iter().map(|(k, _)| k.clone()).collect())
    }
    pub fn set(&self, key: &str, value: &str) {
        self.with(|v| match v.iter_mut().find(|(k, _)| k == key) {
            Some(e) => e.1 = value.to_owned(),
            None => v.push((key.to_owned(), value.to_owned())),
        })
    }
    /// Returns whether the key existed.
    pub fn remove(&self, key: &str) -> bool {
        self.with(|v| {
            let n = v.len();
            v.retain(|(k, _)| k != key);
            v.len() != n
        })
    }
    pub fn clear(&self) {
        self.with(|v| v.clear())
    }
}

/// Storage areas of one browser context, keyed by origin.
#[derive(Debug, Clone, Default)]
pub struct StoragePartition(Arc<Mutex<std::collections::HashMap<String, StorageArea>>>);

impl StoragePartition {
    pub fn new() -> Self {
        Self::default()
    }
    /// The area for `origin` (serialized origin, e.g. `https://a.test`).
    pub fn area(&self, origin: &str) -> StorageArea {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.entry(origin.to_owned()).or_default().clone()
    }
}

/// Console message captured from the page (forwarded by the CDP layer).
#[derive(Debug, Clone, PartialEq)]
pub struct ConsoleMessage {
    /// `log`, `info`, `warn`, `error`, `debug`, ... (`error` also for
    /// uncaught exceptions).
    pub level: String,
    pub text: String,
    /// Script URL and 1-based line, for exceptions.
    pub url: Option<String>,
    pub line: Option<u32>,
    /// Logged on a page for one of its dedicated workers (the worker's
    /// uncaught error): CDP reports it as `Log.entryAdded` with source
    /// `worker`, not as the page's own exception.
    pub from_worker: bool,
}

/// Decode a body to text: BOM, then `Content-Type` charset, then (for
/// HTML) a `<meta charset>` prescan of the first 1024 bytes, then UTF-8 if
/// valid, else windows-1252. Returns the text and the encoding name.
pub fn decode_text(body: &[u8], content_type: Option<&str>, html: bool) -> (String, &'static str) {
    let enc = sniff_encoding(body, content_type, html);
    let (text, used, _) = enc.decode(body);
    (text.into_owned(), used.name())
}

/// The encoding [`decode_text`] would use.
pub fn sniff_encoding(
    body: &[u8],
    content_type: Option<&str>,
    html: bool,
) -> &'static encoding_rs::Encoding {
    if let Some((enc, _)) = encoding_rs::Encoding::for_bom(body) {
        return enc;
    }
    if let Some(enc) = content_type
        .and_then(charset_param)
        .and_then(|c| encoding_rs::Encoding::for_label(c.as_bytes()))
    {
        return fix_utf16(enc);
    }
    if html && let Some(enc) = prescan_meta(&body[..body.len().min(1024)]) {
        return fix_utf16(enc);
    }
    if std::str::from_utf8(body).is_ok() || !html {
        encoding_rs::UTF_8
    } else {
        encoding_rs::WINDOWS_1252
    }
}

/// A meta-declared UTF-16 means UTF-8 (HTML spec).
fn fix_utf16(enc: &'static encoding_rs::Encoding) -> &'static encoding_rs::Encoding {
    if enc == encoding_rs::UTF_16LE || enc == encoding_rs::UTF_16BE {
        encoding_rs::UTF_8
    } else {
        enc
    }
}

/// `charset` parameter of a `Content-Type` value.
pub fn charset_param(ct: &str) -> Option<String> {
    ct.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| v.trim().trim_matches('"').trim_matches('\'').to_owned())
    })
}

/// Simplified HTML prescan: `<meta charset=x>` or
/// `<meta http-equiv=content-type content="...charset=x">`.
fn prescan_meta(head: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let lower: Vec<u8> = head.iter().map(|b| b.to_ascii_lowercase()).collect();
    let text = String::from_utf8_lossy(&lower);
    let mut rest: &str = &text;
    while let Some(p) = rest.find("<meta") {
        let tag_end = rest[p..].find('>').map_or(rest.len(), |e| p + e);
        let tag = &rest[p..tag_end];
        if let Some(c) = tag.find("charset") {
            let after = tag[c + 7..].trim_start();
            if let Some(v) = after.strip_prefix('=') {
                let v = v.trim_start().trim_start_matches(['"', '\'']);
                let label: String = v
                    .chars()
                    .take_while(|ch| !matches!(ch, '"' | '\'' | ';' | ' ' | '/' | '>'))
                    .collect();
                if let Some(enc) = encoding_rs::Encoding::for_label(label.as_bytes()) {
                    return Some(enc);
                }
            }
        }
        rest = &rest[tag_end..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffing() {
        assert_eq!(
            sniff_encoding(b"\xEF\xBB\xBFhi", None, true),
            encoding_rs::UTF_8
        );
        assert_eq!(
            sniff_encoding(b"hi", Some("text/html; charset=ISO-8859-2"), true).name(),
            "ISO-8859-2"
        );
        assert_eq!(
            sniff_encoding(b"<meta charset=\"windows-1251\"><p>", None, true).name(),
            "windows-1251"
        );
        assert_eq!(
            sniff_encoding(
                b"<meta http-equiv=Content-Type content='text/html; charset=shift_jis'>",
                None,
                true
            )
            .name(),
            "Shift_JIS"
        );
        assert_eq!(
            sniff_encoding(b"caf\xe9", None, true),
            encoding_rs::WINDOWS_1252
        );
        assert_eq!(
            sniff_encoding("café".as_bytes(), None, true),
            encoding_rs::UTF_8
        );
        let (t, e) = decode_text(b"caf\xe9", Some("text/html"), true);
        assert_eq!((t.as_str(), e), ("café", "windows-1252"));
    }

    #[test]
    fn storage_area() {
        let s = StorageArea::new();
        s.set("a", "1");
        s.set("b", "2");
        s.set("a", "3");
        assert_eq!(s.keys(), ["a", "b"]);
        assert_eq!(s.get("a").as_deref(), Some("3"));
        assert!(s.remove("a"));
        assert!(!s.remove("a"));
        assert_eq!(s.len(), 1);
    }
}
