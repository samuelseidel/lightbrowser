//! In-memory cookie jar, one per browsing context. Never persisted.
//!
//! Owner: M1 task 4. Thin wrapper over `cookie_store`.

use std::sync::{Arc, Mutex};

use url::Url;

/// A cookie jar. Clones share the same cookies (one jar per browser
/// context, used by every request of that context, on any thread).
#[derive(Default, Clone)]
pub struct CookieJar {
    store: Arc<Mutex<cookie_store::CookieStore>>,
}

impl CookieJar {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, cookie_store::CookieStore> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `Cookie:` header value for `url` (`name=value; name2=value2`), or
    /// `None` if nothing matches.
    pub fn header_for(&self, url: &Url) -> Option<String> {
        let value = self
            .lock()
            .get_request_values(url)
            .map(|(n, v)| format!("{n}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        (!value.is_empty()).then_some(value)
    }

    /// Store every `Set-Cookie` value from a response for `url`. Invalid or
    /// rejected cookies are dropped silently, as browsers do.
    pub fn store_response(&mut self, url: &Url, set_cookie: &[String]) {
        let mut store = self.lock();
        for raw in set_cookie {
            if let Err(e) = store.parse(raw, url) {
                tracing::debug!("dropped Set-Cookie for {url}: {e}");
            }
        }
    }

    /// `document.cookie` getter: the cookies for `url` without `HttpOnly`
    /// ones, `name=value; ...` (empty string if none).
    pub fn document_cookie(&self, url: &Url) -> String {
        self.lock()
            .matches(url)
            .into_iter()
            .filter(|c| !c.http_only().unwrap_or(false))
            .map(|c| format!("{}={}", c.name(), c.value()))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// `document.cookie` setter: store one cookie string for `url`. An
    /// `HttpOnly` cookie from script is ignored, as browsers do.
    pub fn set_document_cookie(&mut self, url: &Url, cookie: &str) {
        let Ok(parsed) = cookie::Cookie::parse(cookie.to_owned()) else {
            return;
        };
        if parsed.http_only().unwrap_or(false) {
            return;
        }
        let _ = self.lock().insert_raw(&parsed, url);
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

    /// Drop every cookie.
    pub fn clear(&self) {
        self.lock().clear();
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

    #[test]
    fn secure_cookie_not_sent_over_http() {
        let mut jar = CookieJar::new();
        jar.store_response(&url("https://example.test/"), &["a=1; Secure".into()]);
        assert_eq!(jar.header_for(&url("http://example.test/")), None);
        assert!(jar.header_for(&url("https://example.test/")).is_some());
    }
}
