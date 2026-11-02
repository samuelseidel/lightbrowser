//! In-memory cookie jar, one per browsing context. Never persisted.
//!
//! Owner: M1 task 4. Thin wrapper over `cookie_store`.

use url::Url;

#[derive(Default)]
pub struct CookieJar {
    store: cookie_store::CookieStore,
}

impl CookieJar {
    pub fn new() -> Self {
        Self::default()
    }

    /// `Cookie:` header value for `url` (`name=value; name2=value2`), or
    /// `None` if nothing matches.
    pub fn header_for(&self, url: &Url) -> Option<String> {
        let value = self
            .store
            .get_request_values(url)
            .map(|(n, v)| format!("{n}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        (!value.is_empty()).then_some(value)
    }

    /// Store every `Set-Cookie` value from a response for `url`. Invalid or
    /// rejected cookies are dropped silently, as browsers do.
    pub fn store_response(&mut self, url: &Url, set_cookie: &[String]) {
        for raw in set_cookie {
            if let Err(e) = self.store.parse(raw, url) {
                tracing::debug!("dropped Set-Cookie for {url}: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
