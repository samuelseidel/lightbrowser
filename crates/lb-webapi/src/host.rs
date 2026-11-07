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

/// Storage key of a window or worker (ADR 0037; the partitioning itself is
/// in `rt::partition`): Chrome's `blink::StorageKey` without a nonce
/// (credentialless and fenced frames are not supported).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StorageKey {
    /// Serialized origin (`null` when opaque).
    pub origin: String,
    /// Schemeful top-level site (`null` when opaque).
    pub top_site: String,
    /// The context or one of its ancestors is cross-site with the
    /// top-level site (Chrome's `kCrossSite` ancestor chain bit).
    pub cross_site: bool,
}

/// Schemeful site of `url`: scheme and the last two labels of a domain,
/// or the whole IP literal (the rule `Sec-Fetch-Site` and the connection
/// pool use; no public-suffix list). `null` for opaque origins.
pub fn site(url: &Url) -> String {
    let origin = url.origin();
    if !origin.is_tuple() {
        return "null".into();
    }
    let host = match url.host() {
        Some(url::Host::Domain(d)) => {
            let labels: Vec<&str> = d.split('.').collect();
            labels[labels.len().saturating_sub(2)..].join(".")
        }
        Some(h) => h.to_string(),
        None => String::new(),
    };
    format!("{}://{host}", url.scheme())
}

impl StorageKey {
    /// The key of a top-level document (or worker) at `url`.
    pub fn first_party(url: &Url) -> StorageKey {
        StorageKey {
            origin: url.origin().ascii_serialization(),
            top_site: site(url),
            cross_site: !url.origin().is_tuple(),
        }
    }

    /// The key of a frame document at `url` (its creator's URL for
    /// `about:blank` / `about:srcdoc`) whose parent has key `parent`.
    pub fn for_frame(parent: &StorageKey, url: &Url) -> StorageKey {
        let s = site(url);
        StorageKey {
            origin: url.origin().ascii_serialization(),
            cross_site: parent.cross_site || s == "null" || s != parent.top_site,
            top_site: parent.top_site.clone(),
        }
    }

    /// First-party: same-site with the top-level site, no cross-site
    /// ancestor.
    pub fn is_first_party(&self) -> bool {
        !self.cross_site
    }

    /// Chrome's serialization (`StorageKey::Serialize`): `origin/`
    /// first-party, `origin/^0site` for a third-party origin,
    /// `origin/^31` for a same-site origin below a cross-site ancestor.
    /// Storage tables are keyed by it.
    pub fn serialize(&self) -> String {
        let base = format!("{}/", self.origin);
        if !self.cross_site {
            return base;
        }
        let own = Url::parse(&self.origin)
            .map(|u| site(&u))
            .unwrap_or_else(|_| "null".into());
        if own != "null" && own == self.top_site {
            format!("{base}^31")
        } else {
            format!("{base}^0{}", self.top_site)
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
    /// Storage key of the document or worker that makes the request (for a
    /// frame navigation: the parent document's), ADR 0037: its cookie
    /// partition and site for cookies. `None`: a top-level navigation, or
    /// first-party of the initiator.
    pub storage_key: Option<StorageKey>,
    /// Content Security Policy (ADR 0036): may the request follow a
    /// redirect to this URL? A refused hop fails the request with
    /// [`CSP_BLOCKED`] and the URL. `None`: every redirect is followed.
    pub redirect_check: Option<RedirectCheck>,
}

/// The error text of a request whose redirect [`NetRequest::redirect_check`]
/// refused, followed by a space and the refused URL (Chrome's
/// `net::ERR_BLOCKED_BY_CSP`).
pub const CSP_BLOCKED: &str = "net::ERR_BLOCKED_BY_CSP";

/// A redirect check (see [`NetRequest::redirect_check`]); it may run on
/// the network thread.
#[derive(Clone)]
pub struct RedirectCheck(pub std::sync::Arc<dyn Fn(&Url) -> bool + Send + Sync>);

impl std::fmt::Debug for RedirectCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RedirectCheck")
    }
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
            storage_key: None,
            redirect_check: None,
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
    /// `document.cookie` getter of a document with storage key `key`
    /// (ADR 0037): its site for cookies and cookie partition (CHIPS).
    fn cookie_string_in(&self, url: &Url, key: &StorageKey) -> String {
        let _ = key;
        self.cookie_string(url)
    }
    /// `document.cookie` setter of a document with storage key `key`.
    fn set_cookie_string_in(&self, url: &Url, cookie: &str, key: &StorageKey) {
        let _ = key;
        self.set_cookie_string(url, cookie)
    }
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
    /// Run `f` with ICU's time zone and default locale for the realms of
    /// this window's browser context (ADR 0039: the CDP overrides, else
    /// the persona's), under the context's lock so that changes reach the
    /// isolates in order. False: the host does not know them (the
    /// window's persona decides).
    fn with_intl(&self, f: &mut dyn FnMut(&str, &str)) -> bool {
        let _ = f;
        false
    }
    /// The window's `navigator.languages` under a CDP `acceptLanguage`
    /// override (ADR 0039); `None`: the persona's. Workers have none
    /// (Chrome keeps theirs).
    fn languages_override(&self) -> Option<Vec<String>> {
        None
    }
    /// What `navigator.geolocation` reports once permitted (ADR 0039):
    /// `None` when nothing overrides the position (there is no location
    /// provider, so requests wait for their timeout).
    fn geolocation(&self) -> Option<GeoAnswer> {
        None
    }
}

/// A geolocation answer (CDP `Emulation.setGeolocationOverride`).
#[derive(Debug, Clone, PartialEq)]
pub enum GeoAnswer {
    Position(GeoCoords),
    /// `POSITION_UNAVAILABLE`.
    Unavailable,
}

/// `GeolocationPosition` values; `None` members are `null`.
#[derive(Debug, Clone, PartialEq)]
pub struct GeoCoords {
    pub latitude: f64,
    pub longitude: f64,
    pub accuracy: f64,
    pub altitude: Option<f64>,
    pub altitude_accuracy: Option<f64>,
    pub heading: Option<f64>,
    pub speed: Option<f64>,
    /// `timestamp`: wall-clock ms since the epoch.
    pub timestamp: f64,
}

/// A WebSocket the page opens.
#[derive(Debug, Clone)]
pub struct WsOpen {
    pub url: Url,
    pub protocols: Vec<String>,
    /// `Origin` header: the document's origin.
    pub origin: String,
    /// The document's storage key (its cookie scope, ADR 0037).
    pub storage_key: Option<StorageKey>,
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
/// shared by every window with the same storage key (and context, and for
/// `sessionStorage` page). In memory only. Windows holding its `Storage`
/// object watch it for `storage` events (ADR 0037).
#[derive(Clone, Default)]
pub struct StorageArea {
    items: Arc<Mutex<Vec<(String, String)>>>,
    watchers: Arc<Mutex<Vec<StorageWatcher>>>,
}

impl std::fmt::Debug for StorageArea {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageArea")
            .field("len", &self.len())
            .finish()
    }
}

/// A change to a storage area, for the `storage` events of the other
/// windows using it.
#[derive(Debug, Clone, PartialEq)]
pub struct StorageChange {
    /// `None`: `clear()`.
    pub key: Option<String>,
    pub old: Option<String>,
    pub new: Option<String>,
    /// The URL of the document that made the change.
    pub url: String,
}

/// A window watching a storage area: its id and how to reach it (`false`
/// once it is gone).
struct StorageWatcher {
    id: u64,
    notify: Box<dyn Fn(&StorageChange) -> bool + Send>,
}

impl StorageArea {
    pub fn new() -> Self {
        Self::default()
    }

    fn with<R>(&self, f: impl FnOnce(&mut Vec<(String, String)>) -> R) -> R {
        let mut g = self.items.lock().unwrap_or_else(|e| e.into_inner());
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
    /// Returns the previous value.
    pub fn set(&self, key: &str, value: &str) -> Option<String> {
        self.with(|v| match v.iter_mut().find(|(k, _)| k == key) {
            Some(e) => Some(std::mem::replace(&mut e.1, value.to_owned())),
            None => {
                v.push((key.to_owned(), value.to_owned()));
                None
            }
        })
    }
    /// Returns whether the key existed.
    pub fn remove(&self, key: &str) -> bool {
        self.take(key).is_some()
    }
    /// Remove `key`, returning its value.
    pub fn take(&self, key: &str) -> Option<String> {
        self.with(|v| {
            let i = v.iter().position(|(k, _)| k == key)?;
            Some(v.remove(i).1)
        })
    }
    /// Returns whether anything was removed.
    pub fn clear(&self) -> bool {
        self.with(|v| {
            let had = !v.is_empty();
            v.clear();
            had
        })
    }

    /// Watch this area as window `id` (once per window).
    pub fn watch(&self, id: u64, notify: Box<dyn Fn(&StorageChange) -> bool + Send>) {
        let mut g = self.watchers.lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|w| w.id != id);
        g.push(StorageWatcher { id, notify });
    }

    /// Tell every watcher but `from` about `change`.
    pub fn notify(&self, from: u64, change: &StorageChange) {
        let mut g = self.watchers.lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|w| w.id == from || (w.notify)(change));
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
