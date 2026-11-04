//! Browser contexts: persona, cookie jar, storage partitions and the
//! network pool shared by every page of the context. Engine-neutral.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use lb_net::client_hints::ClientHintsStore;
use lb_net::cookies::CookieJar;
use lb_net::fetch::{NetContext, RedirectHop, Request};
use lb_net::header_profile::{RequestKind, RequestMeta};
use lb_net::{ClientHelloProfile, ConnectionPool};
use lb_persona::{Persona, PersonaProfile};
use lb_webapi::host::{Destination, NetRequest, NetResponse, StoragePartition};
use url::Url;

/// Running shared workers of a context by (script URL, name): their
/// inbox and control (ADR 0021).
#[cfg(feature = "chrome")]
pub(crate) type SharedWorkers = Mutex<
    std::collections::HashMap<
        (String, String),
        (lb_webapi::rt::Inbox, Arc<lb_webapi::rt::WorkerControl>),
    >,
>;

/// How to create a [`BrowserContext`].
#[derive(Clone)]
pub struct ContextOptions {
    /// Network persona (headers, TLS profile id, family).
    pub persona: Arc<Persona>,
    /// Full persona (navigator, screen, fonts). Optional until M4.
    pub profile: Option<Arc<PersonaProfile>>,
    /// The TLS/H2 profile named by `persona.tls_profile_id`.
    pub tls_profile: Arc<ClientHelloProfile>,
    /// Skip certificate checks (our own local test servers only).
    pub accept_invalid_certs: bool,
    /// Run timers and animation frames on a virtual clock that jumps to
    /// the next timer when the page is idle.
    pub virtual_time: bool,
}

impl std::fmt::Debug for ContextOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextOptions")
            .field("persona", &self.persona.user_agent)
            .field("tls_profile", &self.tls_profile.profile_id)
            .field("virtual_time", &self.virtual_time)
            .finish()
    }
}

struct Shared {
    opts: ContextOptions,
    cookies: CookieJar,
    /// Per-origin `Accept-CH` sets (memory only, like the cookie jar).
    client_hints: ClientHintsStore,
    local_storage: StoragePartition,
    /// Idle network contexts (TLS connector and resolver are reused).
    pool: Mutex<Vec<NetContext>>,
    /// The family's built-in header profile, parsed once for all network
    /// contexts of this browser context.
    header_profile: std::sync::OnceLock<Option<Arc<lb_net::header_profile::HeaderProfile>>>,
    /// Favicon URLs already requested by this context (Chrome asks once).
    favicons: Mutex<HashSet<String>>,
    /// The context's connections (ADR 0018): HTTP/1.1 keep-alive with at
    /// most 6 sockets per group, one HTTP/2 session per group, TLS session
    /// resumption. Never shared with another context.
    connections: ConnectionPool,
    /// The context's proxy (ADR 0027): set once, before the first request
    /// (`Target.createBrowserContext` `proxyServer`, `--proxy`). It caches
    /// the credentials that answered its challenges.
    proxy: std::sync::OnceLock<Arc<lb_net::Proxy>>,
    /// IndexedDB databases of every origin, in memory (ADR 0028), chrome
    /// build.
    #[cfg(feature = "chrome")]
    idb: lb_webapi::rt::idb::backend::IdbBackend,
    /// Running shared workers by (script URL, name), chrome build
    /// (ADR 0021); terminated with the context.
    #[cfg(feature = "chrome")]
    pub(crate) shared_workers: SharedWorkers,
    /// Service worker registrations, versions, clients and caches of this
    /// context, chrome build (ADR 0022); in memory only.
    #[cfg(feature = "chrome")]
    pub(crate) service_workers: Arc<crate::sw::SwShared>,
    /// The CDP host's worker inspector (ADR 0029): set when the context
    /// belongs to `serve`; worker threads announce themselves to it.
    #[cfg(feature = "chrome")]
    inspector: std::sync::OnceLock<Arc<crate::cdp::workers::Inspector>>,
}

#[cfg(feature = "chrome")]
impl Drop for Shared {
    fn drop(&mut self) {
        let workers = self
            .shared_workers
            .get_mut()
            .unwrap_or_else(|e| e.into_inner());
        for (_, (_, c)) in workers.drain() {
            c.terminate();
        }
        self.service_workers.terminate_all();
    }
}

/// What happened on the wire for one [`BrowserContext::fetch_traced`] call,
/// for CDP `Network` events.
#[derive(Debug, Clone)]
pub struct FetchTrace {
    /// Redirects followed, in order (status, `Location`, headers sent and
    /// received on each hop). Empty when there were none.
    pub redirects: Vec<RedirectHop>,
    /// Method of the final request.
    pub method: String,
    /// Request headers as sent on the final hop (wire case and order).
    pub request_headers: Vec<(String, String)>,
    pub http_version: lb_net::HttpVersion,
}

/// A browsing context group: one persona, one cookie jar, one storage
/// partition. Cheap to clone; usable from any thread.
#[derive(Clone)]
pub struct BrowserContext(Arc<Shared>);

impl std::fmt::Debug for BrowserContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("BrowserContext").field(&self.0.opts).finish()
    }
}

/// A [`BrowserContext`] that does not keep it alive.
#[cfg(feature = "chrome")]
#[derive(Clone)]
pub(crate) struct WeakContext(std::sync::Weak<Shared>);

#[cfg(feature = "chrome")]
impl WeakContext {
    pub(crate) fn upgrade(&self) -> Option<BrowserContext> {
        self.0.upgrade().map(BrowserContext)
    }
}

/// Why a page operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageError {
    Net(String),
    Js(String),
    /// The page or browser is gone.
    Closed,
    Timeout,
    /// Another navigation replaced this one.
    Aborted,
    Setup(String),
}

impl std::fmt::Display for PageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PageError::Net(e) => write!(f, "network error: {e}"),
            PageError::Js(e) => write!(f, "script error: {e}"),
            PageError::Closed => f.write_str("page closed"),
            PageError::Timeout => f.write_str("timed out"),
            PageError::Aborted => f.write_str("navigation aborted"),
            PageError::Setup(e) => write!(f, "setup failed: {e}"),
        }
    }
}

impl std::error::Error for PageError {}

impl BrowserContext {
    /// A context with its own cookie jar and storage. Fails when the TLS
    /// profile's family does not match the persona.
    pub fn new(opts: ContextOptions) -> Result<BrowserContext, PageError> {
        // Validate the persona/profile pair once up front.
        let probe = NetContext::new(opts.persona.clone(), opts.tls_profile.clone())
            .map_err(|e| PageError::Setup(e.to_string()))?;
        let cookies = CookieJar::new();
        let ctx = BrowserContext(Arc::new(Shared {
            opts,
            cookies,
            client_hints: ClientHintsStore::new(),
            local_storage: StoragePartition::new(),
            pool: Mutex::new(Vec::new()),
            header_profile: std::sync::OnceLock::new(),
            favicons: Mutex::new(HashSet::new()),
            connections: ConnectionPool::new(),
            proxy: std::sync::OnceLock::new(),
            #[cfg(feature = "chrome")]
            idb: Default::default(),
            #[cfg(feature = "chrome")]
            shared_workers: Mutex::new(std::collections::HashMap::new()),
            #[cfg(feature = "chrome")]
            service_workers: Arc::default(),
            #[cfg(feature = "chrome")]
            inspector: std::sync::OnceLock::new(),
        }));
        ctx.put_back(probe);
        Ok(ctx)
    }

    pub fn options(&self) -> &ContextOptions {
        &self.0.opts
    }

    pub fn persona(&self) -> &Arc<Persona> {
        &self.0.opts.persona
    }

    pub fn profile(&self) -> Option<&Arc<PersonaProfile>> {
        self.0.opts.profile.as_ref()
    }

    /// The context's cookie jar (shared; changes are visible to pages).
    pub fn cookies(&self) -> CookieJar {
        self.0.cookies.clone()
    }

    /// The context's `Accept-CH` store (shared by its pages).
    pub fn client_hints(&self) -> &ClientHintsStore {
        &self.0.client_hints
    }

    /// Running shared workers of this context (ADR 0021).
    #[cfg(feature = "chrome")]
    pub(crate) fn shared_workers(&self) -> &SharedWorkers {
        &self.0.shared_workers
    }

    /// Make this context's workers CDP targets (ADR 0029). Only the first
    /// call counts.
    #[cfg(feature = "chrome")]
    pub(crate) fn set_inspector(&self, i: Arc<crate::cdp::workers::Inspector>) {
        let _ = self.0.inspector.set(i);
    }

    #[cfg(feature = "chrome")]
    pub(crate) fn inspector(&self) -> Option<&Arc<crate::cdp::workers::Inspector>> {
        self.0.inspector.get()
    }

    /// Shorten the idle timeout after which this context's service
    /// workers are stopped (30 s as in Chrome; ADR 0029). For tests.
    #[cfg(feature = "chrome")]
    #[doc(hidden)]
    pub fn set_sw_idle_timeout(&self, d: std::time::Duration) {
        self.0.service_workers.set_idle_timeout(d);
    }

    /// The service worker registry of this context (ADR 0022).
    #[cfg(feature = "chrome")]
    pub(crate) fn service_workers(&self) -> &Arc<crate::sw::SwShared> {
        &self.0.service_workers
    }

    /// A handle that does not keep the context alive (worker threads hold
    /// one, so a context whose pages are gone drops and ends its workers).
    #[cfg(feature = "chrome")]
    pub(crate) fn downgrade(&self) -> WeakContext {
        WeakContext(Arc::downgrade(&self.0))
    }

    /// Route this context's requests through `proxy` (ADR 0027). Only the
    /// first call counts; call it before the first request.
    pub fn set_proxy(&self, proxy: Arc<lb_net::Proxy>) {
        let _ = self.0.proxy.set(proxy);
    }

    /// The context's IndexedDB backend (ADR 0028).
    #[cfg(feature = "chrome")]
    pub fn idb(&self) -> &lb_webapi::rt::idb::backend::IdbBackend {
        &self.0.idb
    }

    /// The context's proxy, if any.
    pub fn proxy(&self) -> Option<&Arc<lb_net::Proxy>> {
        self.0.proxy.get()
    }

    /// The context's connection pool.
    pub fn connections(&self) -> &ConnectionPool {
        &self.0.connections
    }

    /// `localStorage` areas of this context.
    pub fn local_storage(&self) -> &StoragePartition {
        &self.0.local_storage
    }

    pub(crate) fn take_net(&self) -> Result<NetContext, PageError> {
        let pooled = self.0.pool.lock().unwrap_or_else(|e| e.into_inner()).pop();
        let mut net = match pooled {
            Some(n) => n,
            None => {
                let persona = self.0.opts.persona.clone();
                let headers = self
                    .0
                    .header_profile
                    .get_or_init(|| {
                        lb_net::header_profile::HeaderProfile::builtin(persona.family).map(Arc::new)
                    })
                    .clone();
                NetContext::with_header_profile(persona, self.0.opts.tls_profile.clone(), headers)
                    .map_err(|e| PageError::Setup(e.to_string()))?
            }
        };
        net.cookies = self.0.cookies.clone();
        net.client_hints = self.0.client_hints.clone();
        net.danger_accept_invalid_certs = self.0.opts.accept_invalid_certs;
        net.pool = Some(self.0.connections.clone());
        net.proxy = self.0.proxy.get().cloned();
        Ok(net)
    }

    pub(crate) fn put_back(&self, mut net: NetContext) {
        net.cookies = self.0.cookies.clone();
        net.client_hints = self.0.client_hints.clone();
        let mut pool = self.0.pool.lock().unwrap_or_else(|e| e.into_inner());
        if pool.len() < 8 {
            pool.push(net);
        }
    }

    /// Perform a request for a page: the browser's header set for the
    /// request's destination (persona values, `Sec-Fetch-*`, `Referer`,
    /// `Origin`, cookies), then redirects. See `lb_net::header_profile`.
    pub async fn fetch(&self, req: NetRequest) -> Result<NetResponse, String> {
        self.fetch_traced(req).await.map(|(r, _)| r)
    }

    /// [`fetch`](Self::fetch) for a request made inside a page whose
    /// top-level document is `top_frame`: its origin's `Accept-CH` set
    /// decides the client hints of frame and subresource requests
    /// (same-origin only, see `lb_net::client_hints`).
    pub async fn fetch_in_page(
        &self,
        req: NetRequest,
        top_frame: Option<Url>,
    ) -> Result<NetResponse, String> {
        self.fetch_as(req, None, top_frame).await.map(|(r, _)| r)
    }

    /// Like [`fetch`](Self::fetch), also returning the redirect chain and the
    /// request headers that went on the wire.
    pub async fn fetch_traced(&self, req: NetRequest) -> Result<(NetResponse, FetchTrace), String> {
        self.fetch_as(req, None, None).await
    }

    /// [`fetch_in_page`](Self::fetch_in_page) that returns a proxy's
    /// unanswered `407` as its challenge (`Err(Ok(..))`), for CDP
    /// `Fetch.authRequired`; other failures are `Err(Err(text))`.
    pub async fn fetch_in_page_auth(
        &self,
        req: NetRequest,
        top_frame: Option<Url>,
    ) -> Result<NetResponse, Result<lb_net::ProxyChallenge, String>> {
        self.fetch_raw(req, None, top_frame)
            .await
            .map(|(r, _)| r)
            .map_err(|e| match e {
                Ok(lb_net::NetError::ProxyAuth(ch)) => Ok(ch),
                Ok(e) => Err(e.to_string()),
                Err(e) => Err(e),
            })
    }

    async fn fetch_as(
        &self,
        req: NetRequest,
        kind: Option<RequestKind>,
        top_frame: Option<Url>,
    ) -> Result<(NetResponse, FetchTrace), String> {
        self.fetch_raw(req, kind, top_frame)
            .await
            .map_err(|e| match e {
                Ok(e) => e.to_string(),
                Err(e) => e,
            })
    }

    async fn fetch_raw(
        &self,
        req: NetRequest,
        kind: Option<RequestKind>,
        top_frame: Option<Url>,
    ) -> Result<(NetResponse, FetchTrace), Result<lb_net::NetError, String>> {
        // The pool limits sockets per group (6, as Chrome) and reuses them.
        let mut net = self.take_net().map_err(|e| Err(e.to_string()))?;
        let url = req.url.clone();
        let wire = Request {
            method: http::Method::from_bytes(req.method.as_bytes())
                .map_err(|e| Err(e.to_string()))?,
            url: url.clone(),
            headers: req.headers.clone(),
            body: bytes::Bytes::from(req.body.clone()),
            meta: Some({
                let mut m = request_meta(&req, kind);
                m.top_frame = top_frame;
                m
            }),
        };
        let r = lb_net::fetch(&mut net, wire).await;
        self.put_back(net);
        let resp = r.map_err(Ok)?;
        let trace = FetchTrace {
            redirects: resp.redirects,
            method: resp.method.to_string(),
            request_headers: resp.request_headers,
            http_version: resp.version,
        };
        Ok((
            NetResponse {
                status: resp.status.as_u16(),
                status_text: resp.status.canonical_reason().unwrap_or("").to_owned(),
                headers: resp.headers,
                body: resp.body,
                redirected: resp.final_url != url,
                url: resp.final_url,
            },
            trace,
        ))
    }

    /// Request the favicon of a loaded top-level document, the way Chrome
    /// does: `icon` (the `<link rel=icon>` href, already resolved) or else
    /// `/favicon.ico` on the page's origin, with the image header set,
    /// `Referer` = the page and the context's cookies.
    ///
    /// Call it once after the document's `load` event, from the page's
    /// worker (fire and forget; the load does not wait for it). Chrome
    /// asks once per icon URL, so repeated calls for the same URL return
    /// `None`, and so do pages that are not http(s). The response is only
    /// returned for tests and tracing; the body is not used.
    pub async fn request_favicon(
        &self,
        page_url: &Url,
        icon: Option<Url>,
    ) -> Option<Result<NetResponse, String>> {
        if !matches!(page_url.scheme(), "http" | "https") {
            return None;
        }
        let icon = icon.or_else(|| page_url.join("/favicon.ico").ok())?;
        if !matches!(icon.scheme(), "http" | "https") {
            return None;
        }
        let first = self
            .0
            .favicons
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(icon.to_string());
        if !first {
            return None;
        }
        let req = NetRequest::get(icon, Destination::Image, Some(page_url.clone()));
        Some(
            self.fetch_as(req, Some(RequestKind::Favicon), Some(page_url.clone()))
                .await
                .map(|(r, _)| r),
        )
    }
}

/// The header-set selector for a page request.
fn request_meta(req: &NetRequest, kind: Option<RequestKind>) -> RequestMeta {
    let kind = kind.unwrap_or(match req.destination {
        Destination::Document => RequestKind::Document,
        Destination::Iframe => RequestKind::Iframe,
        Destination::Script if req.mode == "cors" => RequestKind::ModuleScript,
        Destination::Script => RequestKind::Script,
        Destination::Style => RequestKind::Style,
        Destination::Image => RequestKind::Image,
        Destination::Empty => RequestKind::Fetch,
    });
    let mut meta = RequestMeta::new(kind, req.initiator.clone());
    meta.mode = req.mode.to_owned();
    meta
}

/// `Sec-Fetch-Site` for a request made by `initiator`.
#[cfg(test)]
pub(crate) fn fetch_site(initiator: Option<&Url>, target: &Url) -> &'static str {
    lb_net::header_profile::Site::between(initiator, target).as_str()
}

/// Open a WebSocket in `ctx` (lb-net's client on the network thread) and
/// bridge its channels to the page's types (ADR 0025).
#[cfg(feature = "chrome")]
pub(crate) fn open_websocket(
    ctx: &BrowserContext,
    open: lb_webapi::host::WsOpen,
) -> Option<lb_webapi::host::WsChannels> {
    use lb_net::websocket::{WsCommand as NC, WsConnect, WsEvent as NE};
    use lb_webapi::host::{WsChannels, WsCommand, WsEvent};
    let mut net = ctx.take_net().ok()?;
    let (tx, mut rx) = net.websocket(WsConnect {
        url: open.url,
        protocols: open.protocols,
        origin: open.origin,
    });
    ctx.put_back(net);
    let (ptx, mut prx) = tokio::sync::mpsc::unbounded_channel::<WsCommand>();
    let (etx, erx) = tokio::sync::mpsc::unbounded_channel::<WsEvent>();
    // The page's commands; when the page drops its sender, so does this
    // task, and the connection closes (1001, as on unload).
    tokio::task::spawn_local(async move {
        while let Some(c) = prx.recv().await {
            let c = match c {
                WsCommand::Text(s) => NC::Text(s),
                WsCommand::Binary(b) => NC::Binary(b),
                WsCommand::Close(code, reason) => NC::Close(code, reason),
            };
            if tx.send(c).is_err() {
                break;
            }
        }
    });
    tokio::task::spawn_local(async move {
        while let Some(e) = rx.recv().await {
            let e = match e {
                NE::Open {
                    protocol,
                    extensions,
                } => WsEvent::Open {
                    protocol,
                    extensions,
                },
                NE::Text(s) => WsEvent::Text(s),
                NE::Binary(b) => WsEvent::Binary(b),
                NE::Flushed(n) => WsEvent::Flushed(n),
                NE::Failed(m) => WsEvent::Failed(m),
                NE::Closed {
                    code,
                    reason,
                    clean,
                } => WsEvent::Closed {
                    code,
                    reason,
                    clean,
                },
            };
            if etx.send(e).is_err() {
                break;
            }
        }
    });
    Some(WsChannels {
        commands: ptx,
        events: erx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites() {
        let a = Url::parse("https://a.example.test/x").unwrap();
        assert_eq!(fetch_site(None, &a), "none");
        assert_eq!(
            fetch_site(Some(&a), &Url::parse("https://a.example.test/y").unwrap()),
            "same-origin"
        );
        assert_eq!(
            fetch_site(Some(&a), &Url::parse("https://b.example.test/").unwrap()),
            "same-site"
        );
        assert_eq!(
            fetch_site(Some(&a), &Url::parse("https://other.test/").unwrap()),
            "cross-site"
        );
    }

    #[test]
    fn meta_follows_destination_and_mode() {
        let init = Url::parse("http://127.0.0.1:1/page").unwrap();
        let u = |p: &str| Url::parse(&format!("http://127.0.0.1:1/{p}")).unwrap();
        let mut req = NetRequest::get(u("a.js"), Destination::Script, Some(init.clone()));
        assert_eq!(request_meta(&req, None).kind, RequestKind::Script);
        req.mode = "cors";
        assert_eq!(request_meta(&req, None).kind, RequestKind::ModuleScript);
        let req = NetRequest::get(u("x"), Destination::Empty, Some(init.clone()));
        let m = request_meta(&req, None);
        assert_eq!((m.kind, m.mode.as_str()), (RequestKind::Fetch, "cors"));
        let nav = NetRequest::get(u("p"), Destination::Document, None);
        let m = request_meta(&nav, None);
        assert!(m.user_activated && m.initiator.is_none());
        let scripted = NetRequest::get(u("p"), Destination::Document, Some(init));
        assert!(!request_meta(&scripted, None).user_activated);
    }
}
