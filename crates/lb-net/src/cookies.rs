//! In-memory cookie jar, one per browsing context. Never persisted.
//!
//! Owner: M1 task 4. Thin wrapper over `cookie_store`, with Chrome 153's
//! rules on top (ADR 0037):
//!
//! - **SameSite.** Every access has a [`CookieScope`] (the requesting
//!   context's top-level site and whether it or an ancestor frame is
//!   cross-site with it). A cross-site access (null site for cookies, or a
//!   URL cross-site to it) gets and sets only `SameSite=None` cookies; a
//!   top-level navigation from another site ([`SameSiteContext::Lax`])
//!   leaves out `Strict` ones. Unspecified `SameSite` counts as `Lax`.
//!   `SameSite=None` needs `Secure`; `Secure` needs a secure URL (https,
//!   `localhost`, loopback).
//! - **CHIPS.** A `Partitioned; Secure` cookie lives in the partition of
//!   the access that set it: the top-level site and the ancestor chain bit
//!   (Chrome's `CookiePartitionKey::FromNetworkIsolationKey`), and is sent
//!   only in that partition. Unpartitioned cookies are shared by all.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use url::Url;

/// A cookie jar. Clones share the same cookies (one jar per browser
/// context, used by every request of that context, on any thread).
#[derive(Default, Clone)]
pub struct CookieJar {
    store: Arc<Mutex<cookie_store::CookieStore>>,
    /// Partitioned (CHIPS) cookies by [`CookieScope::partition`] key.
    parts: Arc<Mutex<HashMap<String, cookie_store::CookieStore>>>,
}

/// Who accesses cookies (ADR 0037): the requesting document's or worker's
/// top-level site and ancestor chain bit (for a frame navigation, the
/// parent document's).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct CookieScope {
    /// Schemeful top-level site (`https://example.com`).
    pub top_site: String,
    /// The context or one of its ancestors is cross-site with the
    /// top-level site: its site for cookies is null.
    pub cross_site: bool,
}

/// Chrome's same-site cookie context of one access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSiteContext {
    /// Same-site: every cookie.
    Strict,
    /// A top-level navigation from another site: not `Strict` cookies.
    Lax,
    /// Cross-site: `SameSite=None` cookies only.
    CrossSite,
}

/// Schemeful site: scheme and the last two labels of a domain, or the IP
/// literal (the rule of `Sec-Fetch-Site`; no public-suffix list).
pub fn site(u: &Url) -> String {
    let host = match u.host() {
        Some(url::Host::Domain(d)) => {
            let labels: Vec<&str> = d.split('.').collect();
            labels[labels.len().saturating_sub(2)..].join(".")
        }
        Some(h) => h.to_string(),
        None => return "null".into(),
    };
    format!("{}://{host}", u.scheme())
}

/// https, `localhost`, `*.localhost` or a loopback address: may set and
/// get `Secure` cookies (Chrome treats them as trustworthy).
fn is_secure(u: &Url) -> bool {
    match u.scheme() {
        "https" | "wss" => true,
        "http" | "ws" => match u.host() {
            Some(url::Host::Domain(d)) => d == "localhost" || d.ends_with(".localhost"),
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        },
        _ => false,
    }
}

impl CookieScope {
    /// A top-level document at `url` (or a request without a known
    /// requester).
    pub fn first_party(url: &Url) -> CookieScope {
        CookieScope {
            top_site: site(url),
            cross_site: false,
        }
    }

    /// The access to `url` is same-site: the site for cookies is not null
    /// and `url` is same-site with it.
    pub fn same_site(&self, url: &Url) -> bool {
        !self.cross_site && site(url) == self.top_site
    }

    /// Cookie partition key of an access to `url`: the top-level site and
    /// the ancestor chain bit (`!site_for_cookies.IsFirstParty(url)`).
    pub fn partition(&self, url: &Url) -> String {
        let bit = if self.same_site(url) { "0" } else { "1" };
        format!("{} {bit}", self.top_site)
    }

    /// The context of a subresource or frame access to `url`.
    pub fn context(&self, url: &Url) -> SameSiteContext {
        if self.same_site(url) {
            SameSiteContext::Strict
        } else {
            SameSiteContext::CrossSite
        }
    }
}

/// Whether a cookie with `same_site` goes with an access in `ctx`.
fn same_site_ok(same_site: Option<cookie::SameSite>, ctx: SameSiteContext) -> bool {
    match (ctx, same_site) {
        (SameSiteContext::Strict, _) => true,
        (_, Some(cookie::SameSite::None)) => true,
        (SameSiteContext::Lax, Some(cookie::SameSite::Strict)) => false,
        (SameSiteContext::Lax, _) => true,
        (SameSiteContext::CrossSite, _) => false,
    }
}

/// Chrome's checks before storing `c` from `url` in `ctx`.
fn may_set(c: &cookie::Cookie<'_>, url: &Url, ctx: SameSiteContext) -> bool {
    let secure = c.secure().unwrap_or(false);
    if secure && !is_secure(url) {
        return false;
    }
    if c.same_site() == Some(cookie::SameSite::None) && !secure {
        return false;
    }
    if c.partitioned().unwrap_or(false) && !secure {
        return false;
    }
    // Setting Lax and Strict cookies needs at least a lax context.
    ctx != SameSiteContext::CrossSite || c.same_site() == Some(cookie::SameSite::None)
}

impl CookieJar {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, cookie_store::CookieStore> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn parts(&self) -> std::sync::MutexGuard<'_, HashMap<String, cookie_store::CookieStore>> {
        self.parts.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `name=value` pairs for an access to `url` (HTTP or script), in the
    /// jar's order, partitioned cookies after unpartitioned ones.
    fn pairs(
        &self,
        url: &Url,
        scope: &CookieScope,
        ctx: SameSiteContext,
        http: bool,
    ) -> Vec<String> {
        let pick = |cs: Vec<&cookie_store::Cookie<'static>>| -> Vec<String> {
            cs.into_iter()
                .filter(|c| {
                    same_site_ok(c.same_site(), ctx) && (http || !c.http_only().unwrap_or(false))
                })
                .map(|c| format!("{}={}", c.name(), c.value()))
                .collect()
        };
        let mut out = pick(self.lock().matches(url));
        if let Some(p) = self.parts().get(&scope.partition(url)) {
            out.extend(pick(p.matches(url)));
        }
        out
    }

    fn store_one(&self, url: &Url, raw: &str, scope: &CookieScope, ctx: SameSiteContext) {
        let Ok(c) = cookie::Cookie::parse(raw.to_owned()) else {
            tracing::debug!("dropped Set-Cookie for {url}: unparsable");
            return;
        };
        if !may_set(&c, url, ctx) {
            tracing::debug!("dropped Set-Cookie for {url}: not allowed in this context");
            return;
        }
        let r = if c.partitioned().unwrap_or(false) {
            self.parts()
                .entry(scope.partition(url))
                .or_default()
                .insert_raw(&c, url)
                .map(|_| ())
        } else {
            self.lock().insert_raw(&c, url).map(|_| ())
        };
        if let Err(e) = r {
            tracing::debug!("dropped Set-Cookie for {url}: {e}");
        }
    }

    /// `Cookie:` header value for `url` (`name=value; name2=value2`), or
    /// `None` if nothing matches. A first-party access.
    pub fn header_for(&self, url: &Url) -> Option<String> {
        self.header_in(url, &CookieScope::first_party(url), SameSiteContext::Strict)
    }

    /// `Cookie:` header value of a request to `url` by `scope` in `ctx`.
    pub fn header_in(
        &self,
        url: &Url,
        scope: &CookieScope,
        ctx: SameSiteContext,
    ) -> Option<String> {
        let value = self.pairs(url, scope, ctx, true).join("; ");
        (!value.is_empty()).then_some(value)
    }

    /// Store every `Set-Cookie` value from a response for `url`. Invalid or
    /// rejected cookies are dropped silently, as browsers do. A
    /// first-party access.
    pub fn store_response(&mut self, url: &Url, set_cookie: &[String]) {
        self.store_response_in(
            url,
            set_cookie,
            &CookieScope::first_party(url),
            SameSiteContext::Strict,
        );
    }

    /// [`store_response`](Self::store_response) for a request by `scope`
    /// in `ctx`.
    pub fn store_response_in(
        &mut self,
        url: &Url,
        set_cookie: &[String],
        scope: &CookieScope,
        ctx: SameSiteContext,
    ) {
        for raw in set_cookie {
            self.store_one(url, raw, scope, ctx);
        }
    }

    /// `document.cookie` getter: the cookies for `url` without `HttpOnly`
    /// ones, `name=value; ...` (empty string if none). First-party.
    pub fn document_cookie(&self, url: &Url) -> String {
        self.document_cookie_in(url, &CookieScope::first_party(url))
    }

    /// `document.cookie` getter of a document at `url` with `scope`.
    pub fn document_cookie_in(&self, url: &Url, scope: &CookieScope) -> String {
        self.pairs(url, scope, scope.context(url), false).join("; ")
    }

    /// `document.cookie` setter: store one cookie string for `url`. An
    /// `HttpOnly` cookie from script is ignored, as browsers do.
    /// First-party.
    pub fn set_document_cookie(&mut self, url: &Url, cookie: &str) {
        self.set_document_cookie_in(url, cookie, &CookieScope::first_party(url));
    }

    /// `document.cookie` setter of a document at `url` with `scope`.
    pub fn set_document_cookie_in(&mut self, url: &Url, cookie: &str, scope: &CookieScope) {
        let Ok(parsed) = cookie::Cookie::parse(cookie.to_owned()) else {
            return;
        };
        if parsed.http_only().unwrap_or(false) {
            return;
        }
        self.store_one(url, cookie, scope, scope.context(url));
    }
}

/// One cookie with its attributes (the CDP `Network.Cookie` view).
#[derive(Debug, Clone, PartialEq)]
pub struct CookieInfo {
    pub name: String,
    pub value: String,
    /// The host for host-only cookies, `.example.com` for domain cookies.
    pub domain: String,
    pub path: String,
    /// Expiry in seconds since the epoch; `None` for session cookies.
    pub expires: Option<f64>,
    pub http_only: bool,
    pub secure: bool,
    /// `Strict`, `Lax` or `None`.
    pub same_site: Option<String>,
}

fn cookie_info(c: &cookie_store::Cookie<'_>) -> CookieInfo {
    let domain = match &c.domain {
        cookie_store::CookieDomain::HostOnly(h) => h.clone(),
        cookie_store::CookieDomain::Suffix(d) => format!(".{}", d.trim_start_matches('.')),
        _ => String::new(),
    };
    let expires = match &c.expires {
        cookie_store::CookieExpiration::AtUtc(t) => Some(t.unix_timestamp() as f64),
        _ => None,
    };
    CookieInfo {
        name: c.name().to_owned(),
        value: c.value().to_owned(),
        domain,
        path: String::from(&c.path),
        expires,
        http_only: c.http_only().unwrap_or(false),
        secure: c.secure().unwrap_or(false),
        same_site: c.same_site().map(|s| s.to_string()),
    }
}

impl CookieJar {
    /// Every unexpired cookie, with attributes.
    pub fn all(&self) -> Vec<CookieInfo> {
        self.lock().iter_unexpired().map(cookie_info).collect()
    }

    /// The cookies a request to `url` would send, with attributes.
    pub fn for_url(&self, url: &Url) -> Vec<CookieInfo> {
        self.lock()
            .matches(url)
            .into_iter()
            .map(cookie_info)
            .collect()
    }

    /// Store a cookie as if `url` had set it (`domain` with a leading dot
    /// makes a domain cookie, otherwise it is host-only).
    pub fn set(&self, url: &Url, c: &CookieInfo) -> Result<(), String> {
        let mut b = cookie::Cookie::build((c.name.clone(), c.value.clone()))
            .path(c.path.clone())
            .secure(c.secure)
            .http_only(c.http_only);
        if let Some(d) = c.domain.strip_prefix('.') {
            b = b.domain(d.to_owned());
        }
        if let Some(e) = c.expires
            && let Ok(t) = cookie::time::OffsetDateTime::from_unix_timestamp(e as i64)
        {
            b = b.expires(t);
        }
        if let Some(s) = &c.same_site {
            b = b.same_site(match s.to_ascii_lowercase().as_str() {
                "strict" => cookie::SameSite::Strict,
                "lax" => cookie::SameSite::Lax,
                _ => cookie::SameSite::None,
            });
        }
        let raw = b.build();
        self.lock()
            .insert_raw(&raw, url)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Remove cookies named `name` whose domain and path match (`None`
    /// matches any). Returns how many went.
    pub fn remove(&self, name: &str, domain: Option<&str>, path: Option<&str>) -> usize {
        let hits: Vec<(String, String)> = self
            .lock()
            .iter_any()
            .filter(|c| c.name() == name)
            .map(|c| {
                let d = match &c.domain {
                    cookie_store::CookieDomain::HostOnly(h) => h.clone(),
                    cookie_store::CookieDomain::Suffix(d) => d.clone(),
                    _ => String::new(),
                };
                (d, String::from(&c.path))
            })
            .filter(|(d, p)| {
                domain.is_none_or(|x| x.trim_start_matches('.') == d.trim_start_matches('.'))
                    && path.is_none_or(|x| x == p)
            })
            .collect();
        let mut store = self.lock();
        hits.iter()
            .filter(|(d, p)| store.remove(d, p, name).is_some())
            .count()
    }

    /// Drop every cookie, partitioned ones too.
    pub fn clear(&self) {
        self.lock().clear();
        self.parts().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_and_document_cookie_hides_http_only() {
        let mut jar = CookieJar::new();
        let other = jar.clone();
        let u = url("https://example.test/");
        jar.store_response(&u, &["a=1; Path=/; HttpOnly".into(), "b=2; Path=/".into()]);
        assert_eq!(other.document_cookie(&u), "b=2");
        let mut o2 = other.clone();
        o2.set_document_cookie(&u, "c=3; path=/");
        o2.set_document_cookie(&u, "d=4; HttpOnly");
        let mut parts: Vec<String> = jar
            .header_for(&u)
            .unwrap()
            .split("; ")
            .map(str::to_owned)
            .collect();
        parts.sort();
        assert_eq!(parts, ["a=1", "b=2", "c=3"]);
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn set_then_get() {
        let mut jar = CookieJar::new();
        let u = url("https://example.test/a");
        assert_eq!(jar.header_for(&u), None);
        jar.store_response(&u, &["a=1; Path=/".into(), "b=2; Path=/".into()]);
        let h = jar.header_for(&url("https://example.test/other")).unwrap();
        let mut parts: Vec<_> = h.split("; ").collect();
        parts.sort();
        assert_eq!(parts, ["a=1", "b=2"]);
    }

    #[test]
    fn overwrite_same_name() {
        let mut jar = CookieJar::new();
        let u = url("https://example.test/");
        jar.store_response(&u, &["a=1".into()]);
        jar.store_response(&u, &["a=2".into()]);
        assert_eq!(jar.header_for(&u).as_deref(), Some("a=2"));
    }

    #[test]
    fn domain_isolation() {
        let mut jar = CookieJar::new();
        jar.store_response(&url("https://one.test/"), &["a=1".into()]);
        assert_eq!(jar.header_for(&url("https://two.test/")), None);
        assert_eq!(
            jar.header_for(&url("https://one.test/")).as_deref(),
            Some("a=1")
        );
        // Host-only cookie is not sent to a subdomain.
        assert_eq!(jar.header_for(&url("https://sub.one.test/")), None);
    }

    #[test]
    fn domain_attribute_matches_subdomains() {
        let mut jar = CookieJar::new();
        jar.store_response(
            &url("https://www.one.test/"),
            &["a=1; Domain=one.test".into()],
        );
        assert_eq!(
            jar.header_for(&url("https://api.one.test/")).as_deref(),
            Some("a=1")
        );
    }

    #[test]
    fn expired_cookie_dropped() {
        let mut jar = CookieJar::new();
        let u = url("https://example.test/");
        jar.store_response(&u, &["a=1".into(), "b=2; Max-Age=0".into()]);
        assert_eq!(jar.header_for(&u).as_deref(), Some("a=1"));
        // An already-expired cookie removes the stored one.
        jar.store_response(&u, &["a=; Expires=Thu, 01 Jan 1970 00:00:00 GMT".into()]);
        assert_eq!(jar.header_for(&u), None);
    }

    #[test]
    fn garbage_set_cookie_ignored() {
        let mut jar = CookieJar::new();
        let u = url("https://example.test/");
        jar.store_response(&u, &["".into(), "no-equals".into()]);
        assert_eq!(jar.header_for(&u), None);
    }

    /// The rules Chrome 153 showed in `lb-page` `tests/partition/` (case
    /// `cookies`): A = 127.0.0.1, B = localhost, http.
    #[test]
    fn same_site_and_partitioned_cookies_as_in_chrome() {
        let mut jar = CookieJar::new();
        let a = url("http://127.0.0.1:1/cookies/x");
        let b = url("http://localhost:1/cookies/x");
        let top_a = CookieScope::first_party(&a);
        // A frame of B in a page of A: cross-site, partition (A, cross).
        let b_in_a = CookieScope {
            top_site: "http://127.0.0.1".into(),
            cross_site: true,
        };
        // A frame of A below a B frame in a page of A (ABA).
        let aba = b_in_a.clone();
        jar.set_document_cookie_in(&a, "a_lax=1", &top_a);
        jar.set_document_cookie_in(&a, "a_none=1; SameSite=None; Secure", &top_a);
        jar.set_document_cookie_in(&a, "a_part=1; SameSite=None; Secure; Partitioned", &top_a);
        jar.set_document_cookie_in(&a, "a_nosec=1; SameSite=None; Partitioned", &top_a);
        jar.set_document_cookie_in(&b, "b_lax=1", &b_in_a);
        jar.set_document_cookie_in(&b, "b_none=1; SameSite=None; Secure", &b_in_a);
        jar.set_document_cookie_in(&b, "b_part=1; SameSite=None; Secure; Partitioned", &b_in_a);
        let sorted = |s: String| {
            let mut v: Vec<String> = s.split("; ").map(str::to_owned).collect();
            v.sort();
            v.join("; ")
        };
        assert_eq!(
            sorted(jar.document_cookie_in(&a, &top_a)),
            "a_lax=1; a_none=1; a_part=1"
        );
        assert_eq!(
            sorted(jar.document_cookie_in(&b, &b_in_a)),
            "b_none=1; b_part=1"
        );
        assert_eq!(jar.document_cookie_in(&a, &aba), "a_none=1");
        // B first-party: the unpartitioned third-party cookie only.
        assert_eq!(
            jar.document_cookie_in(&b, &CookieScope::first_party(&b)),
            "b_none=1"
        );
        // Secure needs a secure URL; loopback and localhost count.
        jar.store_response(&url("http://example.test/"), &["s=1; Secure".into()]);
        assert_eq!(jar.header_for(&url("https://example.test/")), None);
    }

    #[test]
    fn secure_cookie_not_sent_over_http() {
        let mut jar = CookieJar::new();
        jar.store_response(&url("https://example.test/"), &["a=1; Secure".into()]);
        assert_eq!(jar.header_for(&url("http://example.test/")), None);
        assert!(jar.header_for(&url("https://example.test/")).is_some());
    }
}
