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
