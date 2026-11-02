//! Default request headers per browser family, in the order the browser
//! sends them. Order is part of the fingerprint, so this is a `Vec`, not a map.
//!
//! Sources: the header sets and orders below are the observed order for the
//! given version (Chrome 131 and Firefox 128, as sent over HTTP/2 on a
//! top-level GET navigation). Values are not invented beyond those lists.
//!
//! Owner: M1 task 4.

use lb_persona::{BrowserFamily, Persona};

/// `(name, value)` pairs in wire order. Names are lowercase for HTTP/2 and
/// are re-cased by the h1 transport.
pub type OrderedHeaders = Vec<(String, String)>;

/// Chrome 131 `Accept` for a document navigation.
const CHROME_ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,\
image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7";
/// Firefox 128 `Accept` for a document navigation.
const FIREFOX_ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,\
image/avif,image/webp,image/png,image/svg+xml,*/*;q=0.8";
const ACCEPT_ENCODING: &str = "gzip, deflate, br, zstd";
const PRIORITY: &str = "u=0, i";

fn push(h: &mut OrderedHeaders, name: &str, value: &str) {
    h.push((name.to_owned(), value.to_owned()));
}

/// Headers for a top-level document navigation. `sec-ch-ua*` only for Chrome.
///
/// `host` is not emitted: over HTTP/2 it travels as `:authority`, and the h1
/// transport adds `Host` itself where the browser sends it. The parameter is
/// kept so callers need not change when a header depends on it.
///
/// Observed order, Chrome 131 (h2):
/// sec-ch-ua, sec-ch-ua-mobile, sec-ch-ua-platform, upgrade-insecure-requests,
/// user-agent, accept, sec-fetch-site, sec-fetch-mode, sec-fetch-user,
/// sec-fetch-dest, accept-encoding, accept-language, [cookie], priority.
///
/// Observed order, Firefox 128 (h2):
/// user-agent, accept, accept-language, accept-encoding,
/// [cookie], upgrade-insecure-requests, sec-fetch-dest, sec-fetch-mode,
/// sec-fetch-site, sec-fetch-user, priority, te.
pub fn navigation_headers(persona: &Persona, _host: &str) -> OrderedHeaders {
    let mut h = OrderedHeaders::new();
    match persona.family {
        BrowserFamily::Chrome => {
            if let Some(v) = &persona.sec_ch_ua {
                push(&mut h, "sec-ch-ua", v);
            }
            if let Some(v) = &persona.sec_ch_ua_mobile {
                push(&mut h, "sec-ch-ua-mobile", v);
            }
            if let Some(v) = &persona.sec_ch_ua_platform {
                // The persona stores the platform unquoted; the header value
                // is a quoted structured-header string.
                push(&mut h, "sec-ch-ua-platform", &format!("\"{v}\""));
            }
            push(&mut h, "upgrade-insecure-requests", "1");
            push(&mut h, "user-agent", &persona.user_agent);
            push(&mut h, "accept", CHROME_ACCEPT);
            push(&mut h, "sec-fetch-site", "none");
            push(&mut h, "sec-fetch-mode", "navigate");
            push(&mut h, "sec-fetch-user", "?1");
            push(&mut h, "sec-fetch-dest", "document");
            push(&mut h, "accept-encoding", ACCEPT_ENCODING);
            push(&mut h, "accept-language", &persona.accept_language);
            push(&mut h, "priority", PRIORITY);
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
/// - Chrome: right before `priority`.
/// - Firefox: right after `accept-encoding`, i.e. before
///   `upgrade-insecure-requests`.
///
/// The family is recognised from the header set itself: only Firefox sends
/// `te: trailers`. If the anchor header is missing, the cookie is appended.
pub fn with_cookie(mut headers: OrderedHeaders, value: &str) -> OrderedHeaders {
    headers.retain(|(n, _)| n != "cookie");
    let firefox = headers.iter().any(|(n, _)| n == "te");
    let anchor = if firefox {
        "upgrade-insecure-requests"
    } else {
        "priority"
    };
    let pos = headers
        .iter()
        .position(|(n, _)| n == anchor)
        .unwrap_or(headers.len());
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
                "upgrade-insecure-requests",
                "user-agent",
                "accept",
                "sec-fetch-site",
                "sec-fetch-mode",
                "sec-fetch-user",
                "sec-fetch-dest",
                "accept-encoding",
                "accept-language",
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
        assert_eq!(n[i - 1], "accept-language");
        assert_eq!(n[i + 1], "priority");
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
}
