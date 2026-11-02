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

    /// `Cookie:` header value for `url`, or `None` if nothing matches.
    pub fn header_for(&self, _url: &Url) -> Option<String> {
        let _ = &self.store;
        None
    }

    /// Store every `Set-Cookie` value from a response for `url`.
    pub fn store_response(&mut self, _url: &Url, _set_cookie: &[String]) {}
}
