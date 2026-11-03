//! Default request headers per browser family, in the order the browser
//! sends them. Order is part of the fingerprint, so this is a `Vec`, not a map.
//!
//! Chrome: the per-kind sets in [`crate::header_profile`] (captured from
//! Chrome 153); [`navigation_headers`] is its top-level navigation set.
//! Firefox: the observed Firefox 128 order for a top-level GET over HTTP/2,
//! plus the small per-kind overlay in [`generic_request_headers`] until the
//! real Firefox sets are captured (M4b).

use http::Method;
use lb_persona::{BrowserFamily, Persona};
use url::Url;

use crate::HttpVersion;
use crate::header_profile::{
    BuildInput, HeaderProfile, RequestKind, RequestMeta, Site, referrer_for, title_case,
};

/// `(name, value)` pairs in wire order. Names are lowercase for HTTP/2 and
/// are re-cased by the h1 transport.
pub type OrderedHeaders = Vec<(String, String)>;

/// Firefox 128 `Accept` for a document navigation.
const FIREFOX_ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,\
image/avif,image/webp,image/png,image/svg+xml,*/*;q=0.8";
const ACCEPT_ENCODING: &str = "gzip, deflate, br, zstd";
const PRIORITY: &str = "u=0, i";

fn push(h: &mut OrderedHeaders, name: &str, value: &str) {
    h.push((name.to_owned(), value.to_owned()));
}

/// Headers for a top-level document navigation over HTTP/2 (lowercase
/// names). `sec-ch-ua*` only for Chrome.
///
/// `host` is not emitted: over HTTP/2 it travels as `:authority`, and the h1
/// transport adds `Host` itself where the browser sends it. The parameter is
/// kept so callers need not change when a header depends on it.
///
/// Order, Chrome 153 (from `profiles/headers/chrome_153.json`):
/// sec-ch-ua, sec-ch-ua-mobile, sec-ch-ua-platform, accept-language,
/// upgrade-insecure-requests, user-agent, accept, sec-fetch-site,
/// sec-fetch-mode, sec-fetch-user, sec-fetch-dest, accept-encoding, [cookie],
/// priority.
///
/// Observed order, Firefox 128 (h2):
/// user-agent, accept, accept-language, accept-encoding,
/// [cookie], upgrade-insecure-requests, sec-fetch-dest, sec-fetch-mode,
/// sec-fetch-site, sec-fetch-user, priority, te.
pub fn navigation_headers(persona: &Persona, _host: &str) -> OrderedHeaders {
    let mut h = OrderedHeaders::new();
    match persona.family {
        BrowserFamily::Chrome => {
            let profile = HeaderProfile::builtin(BrowserFamily::Chrome)
                .expect("chrome header profile is built in");
            let meta = RequestMeta::navigation();
            let url = Url::parse("https://example.invalid/").expect("static url");
            return profile.build(&BuildInput {
                persona,
                version: HttpVersion::Http2,
                meta: &meta,
                url: &url,
                method: &Method::GET,
                body_len: 0,
                cookie: None,
                caller: &Vec::new(),
                hop: 0,
                site: Site::None,
            });
        }
        BrowserFamily::Firefox => {
            push(&mut h, "user-agent", &persona.user_agent);
            push(&mut h, "accept", FIREFOX_ACCEPT);
            push(&mut h, "accept-language", &persona.accept_language);
            push(&mut h, "accept-encoding", ACCEPT_ENCODING);
            push(&mut h, "upgrade-insecure-requests", "1");
            push(&mut h, "sec-fetch-dest", "document");
            push(&mut h, "sec-fetch-mode", "navigate");
            push(&mut h, "sec-fetch-site", "none");
            push(&mut h, "sec-fetch-user", "?1");
            push(&mut h, "priority", PRIORITY);
            push(&mut h, "te", "trailers");
        }
    }
    h
}

/// Insert the `cookie` header where the browser puts it, for headers built by
/// [`navigation_headers`]. Any existing `cookie` header is replaced.
///
/// - Chrome (153): right before `priority` (last on HTTP/1.1, which has no
///   `priority`).
/// - Firefox: right after `accept-encoding`, i.e. before
///   `upgrade-insecure-requests`.
///
/// The family is recognised from the header set itself: only Firefox sends
/// `te: trailers`. If the anchor header is missing, the cookie is appended.
pub fn with_cookie(mut headers: OrderedHeaders, value: &str) -> OrderedHeaders {
    headers.retain(|(n, _)| n != "cookie");
    let firefox = headers.iter().any(|(n, _)| n == "te");
    let pos = if firefox {
        headers
            .iter()
            .position(|(n, _)| n == "upgrade-insecure-requests")
            .unwrap_or(headers.len())
    } else {
        headers
            .iter()
            .position(|(n, _)| n == "priority")
            .unwrap_or(headers.len())
    };
    headers.insert(pos, ("cookie".to_owned(), value.to_owned()));
    headers
}

/// Adjusts default headers for an HTTP/1.1 connection.
///
/// Chrome over HTTP/1.1 (checked against Chromium 153 with `lb-fptest serve
/// --alpn http/1.1`) sends `Connection: keep-alive` right after `Host` and no
/// `Priority` header (that one is only sent on h2/h3). The h1 transport adds
/// `Host` in front. Firefox is left as is until it is checked against a real
/// Firefox capture (M4b).
pub fn for_http1(persona: &Persona, mut headers: OrderedHeaders) -> OrderedHeaders {
    if persona.family == BrowserFamily::Chrome {
        headers.retain(|(n, _)| n != "priority" && n != "connection");
        headers.insert(0, ("connection".into(), "keep-alive".into()));
    }
    headers
}

/// Overlay the page's headers on the defaults. A page header replaces the
/// first default of the same name (case-insensitive) in place and removes any
/// further defaults of that name. Repeats of a name within the page's list,
/// and names without a default, are appended in order.
pub fn merge_headers(base: OrderedHeaders, caller: &OrderedHeaders) -> OrderedHeaders {
    let mut out = base;
    let default_len = out.len();
    let mut replaced: Vec<String> = Vec::new();
    for (name, value) in caller {
        let lname = name.to_ascii_lowercase();
        if replaced.contains(&lname) {
            out.push((name.clone(), value.clone()));
            continue;
        }
        let pos = out[..default_len.min(out.len())]
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case(&lname));
        match pos {
            Some(i) => {
                out[i].1 = value.clone();
                // Drop later defaults of the same name (only below `default_len`).
                let mut j = i + 1;
                while j < default_len.min(out.len()) {
                    if out[j].0.eq_ignore_ascii_case(&lname) {
                        out.remove(j);
                    } else {
                        j += 1;
                    }
                }
            }
            None => out.push((name.clone(), value.clone())),
        }
        replaced.push(lname);
    }
    out
}

/// `Accept` of non-navigation requests for a family without a captured
/// per-kind profile (Firefox 128 values, not yet verified against a capture).
fn generic_accept(kind: RequestKind) -> &'static str {
    match kind {
        RequestKind::Style => "text/css,*/*;q=0.1",
        RequestKind::Image | RequestKind::Favicon => {
            "image/avif,image/webp,image/png,image/svg+xml,image/*;q=0.8,*/*;q=0.5"
        }
        _ => "*/*",
    }
}

/// Request headers for a family without a per-kind profile (Firefox): the
/// navigation set with `Sec-Fetch-*`, `Accept` and `Upgrade-Insecure-Requests`
/// adjusted to the request kind, then the page's headers, `Referer` and
/// `Origin`. Names are Title-Case on HTTP/1.1.
pub fn generic_request_headers(input: &BuildInput<'_>) -> OrderedHeaders {
    let persona = input.persona;
    let meta = input.meta;
    let mut base = navigation_headers(persona, "");
    if input.version == HttpVersion::Http1 {
        base = for_http1(persona, base);
    }
    if let Some(c) = input.cookie {
        base = with_cookie(base, c);
    }
    let set = |h: &mut OrderedHeaders, name: &str, value: &str| {
        if let Some(e) = h.iter_mut().find(|(n, _)| n == name) {
            e.1 = value.to_owned();
        }
    };
    set(&mut base, "sec-fetch-site", input.site.as_str());
    match meta.kind {
        RequestKind::Document => {}
        RequestKind::Iframe => set(&mut base, "sec-fetch-dest", "iframe"),
        kind => {
            base.retain(|(n, _)| n != "upgrade-insecure-requests");
            set(&mut base, "accept", generic_accept(kind));
            let (mode, dest) = match kind {
                RequestKind::Style => ("no-cors", "style"),
                RequestKind::Image | RequestKind::Favicon => ("no-cors", "image"),
                RequestKind::Script => ("no-cors", "script"),
                RequestKind::ModuleScript => ("cors", "script"),
                RequestKind::Font => ("cors", "font"),
                _ => (meta.mode.as_str(), "empty"),
            };
            set(&mut base, "sec-fetch-mode", mode);
            set(&mut base, "sec-fetch-dest", dest);
        }
    }
    let navigation = matches!(meta.kind, RequestKind::Document | RequestKind::Iframe);
    if !(navigation && meta.user_activated) {
        base.retain(|(n, _)| n != "sec-fetch-user");
    }
    let mut out = merge_headers(base, input.caller);
    if let Some(r) = referrer_for(meta.initiator.as_ref(), input.url) {
        out.push(("referer".into(), r));
    }
    if let Some(i) = &meta.initiator {
        let safe = matches!(*input.method, Method::GET | Method::HEAD);
        let cross = i.origin() != input.url.origin();
        // Form POST navigations send Origin too (not checked against
        // Firefox yet, M4b).
        if (meta.kind == RequestKind::Fetch && (!safe || cross)) || (navigation && !safe) {
            out.push(("origin".into(), i.origin().ascii_serialization()));
        }
    }
    if input.version == HttpVersion::Http1 {
        for (n, _) in &mut out {
            if n == "te" {
                *n = "TE".into();
            } else if *n == n.to_ascii_lowercase() {
                *n = title_case(n);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(h: &OrderedHeaders) -> Vec<&str> {
        h.iter().map(|(n, _)| n.as_str()).collect()
    }

    fn get<'a>(h: &'a OrderedHeaders, name: &str) -> Option<&'a str> {
        h.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    #[test]
    fn chrome_order_and_values() {
        let p = Persona::default_chrome();
        let h = navigation_headers(&p, "example.test");
        assert_eq!(
            names(&h),
            [
                "sec-ch-ua",
                "sec-ch-ua-mobile",
                "sec-ch-ua-platform",
                "accept-language",
                "upgrade-insecure-requests",
                "user-agent",
                "accept",
                "sec-fetch-site",
                "sec-fetch-mode",
                "sec-fetch-user",
                "sec-fetch-dest",
                "accept-encoding",
                "priority",
            ]
        );
        assert_eq!(get(&h, "sec-ch-ua-platform"), Some("\"Windows\""));
        assert_eq!(get(&h, "sec-ch-ua-mobile"), Some("?0"));
        assert_eq!(get(&h, "user-agent"), Some(p.user_agent.as_str()));
        assert_eq!(get(&h, "accept-language"), Some("en-US,en;q=0.9"));
        assert_eq!(get(&h, "accept-encoding"), Some("gzip, deflate, br, zstd"));
        assert_eq!(get(&h, "priority"), Some("u=0, i"));
        assert!(
            get(&h, "accept")
                .unwrap()
                .ends_with("application/signed-exchange;v=b3;q=0.7")
        );
        assert!(get(&h, "te").is_none());
    }

    #[test]
    fn firefox_order_and_values() {
        let p = Persona::default_firefox();
        let h = navigation_headers(&p, "example.test");
        assert_eq!(
            names(&h),
            [
                "user-agent",
                "accept",
                "accept-language",
                "accept-encoding",
                "upgrade-insecure-requests",
                "sec-fetch-dest",
                "sec-fetch-mode",
                "sec-fetch-site",
                "sec-fetch-user",
                "priority",
                "te",
            ]
        );
        assert_eq!(
            get(&h, "accept"),
            Some(
                "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/png,image/svg+xml,*/*;q=0.8"
            )
        );
        assert_eq!(get(&h, "te"), Some("trailers"));
        assert_eq!(get(&h, "accept-language"), Some("en-US,en;q=0.5"));
    }

    #[test]
    fn chrome_has_client_hints_firefox_does_not() {
        let c = navigation_headers(&Persona::default_chrome(), "h");
        let f = navigation_headers(&Persona::default_firefox(), "h");
        assert!(c.iter().any(|(n, _)| n.starts_with("sec-ch-ua")));
        assert!(!f.iter().any(|(n, _)| n.starts_with("sec-ch-")));
    }

    #[test]
    fn deterministic() {
        for p in [Persona::default_chrome(), Persona::default_firefox()] {
            assert_eq!(navigation_headers(&p, "a"), navigation_headers(&p, "b"));
        }
    }

    #[test]
    fn names_are_lowercase() {
        for p in [Persona::default_chrome(), Persona::default_firefox()] {
            for (n, _) in navigation_headers(&p, "h") {
                assert_eq!(n, n.to_ascii_lowercase());
            }
        }
    }

    #[test]
    fn chrome_cookie_goes_before_priority() {
        let h = with_cookie(navigation_headers(&Persona::default_chrome(), "h"), "a=1");
        let n = names(&h);
        let i = n.iter().position(|x| *x == "cookie").unwrap();
        assert_eq!(n[i - 1], "accept-encoding");
        assert_eq!(n[i + 1], "priority");
        assert_eq!(i, n.len() - 2);
        assert_eq!(n.len(), 14);
    }

    #[test]
    fn firefox_cookie_goes_after_accept_encoding() {
        let h = with_cookie(navigation_headers(&Persona::default_firefox(), "h"), "a=1");
        let n = names(&h);
        let i = n.iter().position(|x| *x == "cookie").unwrap();
        assert_eq!(n[i - 1], "accept-encoding");
        assert_eq!(n[i + 1], "upgrade-insecure-requests");
        assert_eq!(n.len(), 12);
    }

    #[test]
    fn cookie_replaces_existing_and_appends_without_anchor() {
        let h = with_cookie(
            with_cookie(navigation_headers(&Persona::default_chrome(), "h"), "a=1"),
            "b=2",
        );
        assert_eq!(h.iter().filter(|(n, _)| n == "cookie").count(), 1);
        assert_eq!(get(&h, "cookie"), Some("b=2"));
        let h = with_cookie(vec![("x".into(), "y".into())], "a=1");
        assert_eq!(names(&h), ["x", "cookie"]);
    }

    #[test]
    fn chrome_http1_has_keep_alive_and_no_priority() {
        let p = Persona::default_chrome();
        let h = for_http1(&p, navigation_headers(&p, "example.test"));
        assert_eq!(h[0], ("connection".into(), "keep-alive".into()));
        assert!(h.iter().all(|(n, _)| n != "priority"));
        assert_eq!(h.len(), navigation_headers(&p, "example.test").len());

        let f = Persona::default_firefox();
        assert_eq!(
            for_http1(&f, navigation_headers(&f, "example.test")),
            navigation_headers(&f, "example.test")
        );
    }

    fn h(pairs: &[(&str, &str)]) -> OrderedHeaders {
        pairs
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn merge_replaces_in_place_and_appends() {
        let base = h(&[("user-agent", "A"), ("accept", "x"), ("priority", "u=0")]);
        let caller = h(&[("Accept", "y"), ("X-Test", "1"), ("x-test", "2")]);
        let got = merge_headers(base, &caller);
        assert_eq!(
            got,
            h(&[
                ("user-agent", "A"),
                ("accept", "y"),
                ("priority", "u=0"),
                ("X-Test", "1"),
                ("x-test", "2"),
            ])
        );
    }

    #[test]
    fn merge_with_empty_caller_is_identity() {
        let base = h(&[("a", "1"), ("b", "2")]);
        assert_eq!(merge_headers(base.clone(), &Vec::new()), base);
    }
}
