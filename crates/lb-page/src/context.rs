//! Browser contexts: persona, cookie jar, storage partitions and the
//! network pool shared by every page of the context. Engine-neutral.

use std::sync::{Arc, Mutex};

use lb_net::ClientHelloProfile;
use lb_net::cookies::CookieJar;
use lb_net::fetch::{NetContext, Request};
use lb_persona::{Persona, PersonaProfile};
use lb_webapi::host::{Destination, NetRequest, NetResponse, StoragePartition};
use url::Url;

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
    local_storage: StoragePartition,
    /// Idle network contexts (TLS connector and resolver are reused).
    pool: Mutex<Vec<NetContext>>,
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
            local_storage: StoragePartition::new(),
            pool: Mutex::new(Vec::new()),
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

    /// `localStorage` areas of this context.
    pub fn local_storage(&self) -> &StoragePartition {
        &self.0.local_storage
    }

    fn take_net(&self) -> Result<NetContext, PageError> {
        let pooled = self.0.pool.lock().unwrap_or_else(|e| e.into_inner()).pop();
        let mut net = match pooled {
            Some(n) => n,
            None => NetContext::new(self.0.opts.persona.clone(), self.0.opts.tls_profile.clone())
                .map_err(|e| PageError::Setup(e.to_string()))?,
        };
        net.cookies = self.0.cookies.clone();
        net.danger_accept_invalid_certs = self.0.opts.accept_invalid_certs;
        Ok(net)
    }

    fn put_back(&self, mut net: NetContext) {
        net.cookies = self.0.cookies.clone();
        let mut pool = self.0.pool.lock().unwrap_or_else(|e| e.into_inner());
        if pool.len() < 8 {
            pool.push(net);
        }
    }

    /// Perform a request for a page: persona headers adjusted for the
    /// destination, the context's cookies, redirects.
    pub async fn fetch(&self, req: NetRequest) -> Result<NetResponse, String> {
        let mut net = self.take_net().map_err(|e| e.to_string())?;
        let url = req.url.clone();
        let wire = Request {
            method: http::Method::from_bytes(req.method.as_bytes()).map_err(|e| e.to_string())?,
            url: url.clone(),
            headers: request_headers(&req),
            body: bytes::Bytes::from(req.body),
        };
        let r = lb_net::fetch(&mut net, wire).await;
        self.put_back(net);
        let resp = r.map_err(|e| e.to_string())?;
        Ok(NetResponse {
            status: resp.status.as_u16(),
            status_text: resp.status.canonical_reason().unwrap_or("").to_owned(),
            headers: resp.headers,
            body: resp.body.to_vec(),
            redirected: resp.final_url != url,
            url: resp.final_url,
        })
    }
}

/// `Sec-Fetch-Site` for a request made by `initiator`.
pub(crate) fn fetch_site(initiator: Option<&Url>, target: &Url) -> &'static str {
    let Some(i) = initiator else { return "none" };
    if i.origin() == target.origin() {
        return "same-origin";
    }
    let site = |u: &Url| {
        let h = u.host_str().unwrap_or("");
        let labels: Vec<&str> = h.rsplit('.').take(2).collect();
        (u.scheme().to_owned(), labels.join("."))
    };
    if site(i) == site(target) {
        "same-site"
    } else {
        "cross-site"
    }
}

/// Caller headers for lb-net: they replace the persona's navigation
/// defaults in place (`Accept`, `Sec-Fetch-*`), then script headers and
/// `Referer` / `Origin` follow.
///
/// Known gap: lb-net always starts from the navigation header set, so
/// subresource requests still carry `Upgrade-Insecure-Requests` and
/// `Sec-Fetch-User`; per-destination header sets belong in lb-net.
fn request_headers(req: &NetRequest) -> Vec<(String, String)> {
    let mut h: Vec<(String, String)> = Vec::new();
    let mut put = |k: &str, v: &str| h.push((k.to_owned(), v.to_owned()));
    let site = fetch_site(req.initiator.as_ref(), &req.url);
    match req.destination {
        Destination::Document | Destination::Iframe => {
            put("sec-fetch-site", site);
            if req.destination == Destination::Iframe {
                put("sec-fetch-dest", "iframe");
            }
        }
        d => {
            let accept = match d {
                Destination::Style => "text/css,*/*;q=0.1",
                Destination::Image => {
                    "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8"
                }
                _ => "*/*",
            };
            put("accept", accept);
            put("sec-fetch-site", site);
            put("sec-fetch-mode", req.mode);
            put("sec-fetch-dest", d.as_str());
        }
    }
    let mut out = h;
    for (k, v) in &req.headers {
        if let Some(e) = out.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(k)) {
            e.1 = v.clone();
        } else {
            out.push((k.to_ascii_lowercase(), v.clone()));
        }
    }
    if let Some(i) = &req.initiator {
        let cross = i.origin() != req.url.origin();
        if req.destination == Destination::Empty && (cross || req.method != "GET") {
            out.push(("origin".into(), i.origin().ascii_serialization()));
        }
        // Default referrer policy strict-origin-when-cross-origin.
        let referer = if cross {
            format!("{}/", i.origin().ascii_serialization())
        } else {
            let mut r = i.clone();
            r.set_fragment(None);
            r.to_string()
        };
        if !(i.scheme() == "https" && req.url.scheme() == "http") {
            out.push(("referer".into(), referer));
        }
    }
    out
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
    fn subresource_headers() {
        let init = Url::parse("http://127.0.0.1:1/page#frag").unwrap();
        let req = NetRequest::get(
            Url::parse("http://127.0.0.1:1/a.js").unwrap(),
            Destination::Script,
            Some(init),
        );
        let h = request_headers(&req);
        let get = |n: &str| h.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
        assert_eq!(get("accept"), Some("*/*"));
        assert_eq!(get("sec-fetch-dest"), Some("script"));
        assert_eq!(get("sec-fetch-mode"), Some("no-cors"));
        assert_eq!(get("sec-fetch-site"), Some("same-origin"));
        assert_eq!(get("referer"), Some("http://127.0.0.1:1/page"));
    }
}
