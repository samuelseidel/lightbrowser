//! High-entropy User-Agent client hints: `Accept-CH`, `Critical-CH` and the
//! `Sec-CH-UA-*` request headers Chrome sends only to origins that asked.
//!
//! What Chrome 153 does (captured on loopback with
//! `tools/header_capture/accept_ch.js`, see
//! `profiles/headers/captures/chrome_153_linux_accept_ch.jsonl`):
//!
//! - Only potentially trustworthy origins count ([`is_trustworthy`]): https,
//!   and http to `localhost`, `*.localhost`, `127.0.0.0/8` or `::1`. An
//!   https connection with certificate errors does not count either.
//! - `Accept-CH` on a top-level navigation response (redirects included)
//!   replaces the origin's stored set. `Accept-CH` from a frame or a
//!   subresource is ignored.
//! - Later requests to that origin carry the hints: top-level navigations,
//!   same-origin iframes and the subresources of documents that are
//!   same-origin with the top-level document. Cross-origin frames and
//!   requests get none (no `Permissions-Policy` delegation yet).
//! - `Critical-CH`: when the response asks for a hint it listed in
//!   `Accept-CH` and the request lacked it, Chrome restarts the navigation
//!   once with the hints.
//!
//! The store lives in memory, one per browser context (shared by clones).
//! Values come from the persona ([`lb_persona::HighEntropyHints`]); header
//! order comes from the header profile (`profiles/headers/chrome_153.json`).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use lb_persona::{HighEntropyHints, format_brand_list};
use url::{Host, Url};

use crate::header_profile::{RequestKind, RequestMeta};

/// One high-entropy UA hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum UaHint {
    Arch,
    Bitness,
    /// `Sec-CH-UA-Full-Version` (deprecated, still honoured by Chrome 153).
    FullVersion,
    FullVersionList,
    Model,
    PlatformVersion,
    Wow64,
    FormFactors,
}

impl UaHint {
    pub const ALL: [UaHint; 8] = [
        UaHint::Arch,
        UaHint::Bitness,
        UaHint::FullVersion,
        UaHint::FullVersionList,
        UaHint::Model,
        UaHint::PlatformVersion,
        UaHint::Wow64,
        UaHint::FormFactors,
    ];

    /// Lowercase request header name.
    pub fn header(self) -> &'static str {
        match self {
            UaHint::Arch => "sec-ch-ua-arch",
            UaHint::Bitness => "sec-ch-ua-bitness",
            UaHint::FullVersion => "sec-ch-ua-full-version",
            UaHint::FullVersionList => "sec-ch-ua-full-version-list",
            UaHint::Model => "sec-ch-ua-model",
            UaHint::PlatformVersion => "sec-ch-ua-platform-version",
            UaHint::Wow64 => "sec-ch-ua-wow64",
            UaHint::FormFactors => "sec-ch-ua-form-factors",
        }
    }

    /// The hint a header name (any case) stands for.
    pub fn from_header(name: &str) -> Option<UaHint> {
        UaHint::ALL
            .into_iter()
            .find(|h| h.header().eq_ignore_ascii_case(name.trim()))
    }

    /// The structured-field header value from the persona.
    pub fn value(self, h: &HighEntropyHints) -> String {
        let q = |s: &str| format!("\"{s}\"");
        match self {
            UaHint::Arch => q(&h.architecture),
            UaHint::Bitness => q(&h.bitness),
            UaHint::FullVersion => q(&h.full_version),
            UaHint::FullVersionList => format_brand_list(&h.full_version_list),
            UaHint::Model => q(&h.model),
            UaHint::PlatformVersion => q(&h.platform_version),
            UaHint::Wow64 => if h.wow64 { "?1" } else { "?0" }.to_owned(),
            UaHint::FormFactors => h
                .form_factors
                .iter()
                .map(|f| q(f))
                .collect::<Vec<_>>()
                .join(", "),
        }
    }
}

/// The UA hints named in `Accept-CH` / `Critical-CH` values (comma lists,
/// any case). Other hints (`DPR`, `Viewport-Width`, ...) are not sent.
pub fn parse_hint_list<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<UaHint> {
    let mut out = Vec::new();
    for v in values {
        for token in v.split(',') {
            if let Some(h) = UaHint::from_header(token)
                && !out.contains(&h)
            {
                out.push(h);
            }
        }
    }
    out.sort();
    out
}

/// A potentially trustworthy origin (Secure Contexts): https and wss, and
/// http/ws to loopback (`localhost`, `*.localhost`, `127.0.0.0/8`, `::1`).
/// Chrome sends no client hints at all elsewhere.
pub fn is_trustworthy(url: &Url) -> bool {
    match url.scheme() {
        "https" | "wss" => true,
        "http" | "ws" => match url.host() {
            Some(Host::Domain(d)) => {
                let d = d.trim_end_matches('.').to_ascii_lowercase();
                d == "localhost" || d.ends_with(".localhost")
            }
            Some(Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
            Some(Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
            None => false,
        },
        _ => false,
    }
}

/// The hints a request for `url` carries, by Chrome's rules: a top-level
/// navigation gets its own origin's set; a frame navigation or subresource
/// gets the top-level origin's set only when the request, the frame that
/// made it and the top-level document are all same-origin (no
/// `Permissions-Policy` delegation to other origins). Preflights get none,
/// and nothing goes to an origin that is not trustworthy.
pub fn hints_for_request(store: &ClientHintsStore, meta: &RequestMeta, url: &Url) -> Vec<UaHint> {
    if !is_trustworthy(url) {
        return Vec::new();
    }
    match meta.kind {
        RequestKind::Document => store.get(url),
        RequestKind::Preflight => Vec::new(),
        _ => {
            let Some(top) = meta.top_frame.as_ref().or(meta.initiator.as_ref()) else {
                return Vec::new();
            };
            let same = |u: &Url| u.origin() == top.origin();
            if !same(url) || !meta.initiator.as_ref().is_none_or(same) {
                return Vec::new();
            }
            store.get(top)
        }
    }
}

/// Per-origin `Accept-CH` sets of one browser context, in memory only.
/// Clones share the store.
#[derive(Debug, Clone, Default)]
pub struct ClientHintsStore(Arc<Mutex<HashMap<String, Vec<UaHint>>>>);

impl ClientHintsStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(url: &Url) -> String {
        url.origin().ascii_serialization()
    }

    /// The hints `url`'s origin asked for (empty when it never did).
    pub fn get(&self, url: &Url) -> Vec<UaHint> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&Self::key(url))
            .cloned()
            .unwrap_or_default()
    }

    /// Record an `Accept-CH` response header of a top-level navigation to
    /// `url`. The new list replaces the old one (an empty list clears it).
    /// Ignored for origins that are not trustworthy.
    pub fn accept(&self, url: &Url, accept_ch: &[String]) {
        if !is_trustworthy(url) {
            return;
        }
        let hints = parse_hint_list(accept_ch.iter().map(String::as_str));
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if hints.is_empty() {
            map.remove(&Self::key(url));
        } else {
            map.insert(Self::key(url), hints);
        }
    }

    /// Forget every origin (cookies and site data cleared).
    pub fn clear(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lb_persona::BrandVersion;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn trustworthy_origins() {
        for ok in [
            "https://example.test/",
            "http://localhost:8080/",
            "http://a.localhost/",
            "http://127.0.0.1:1/",
            "http://127.9.9.9/",
            "http://[::1]:80/",
        ] {
            assert!(is_trustworthy(&u(ok)), "{ok}");
        }
        for no in [
            "http://example.test/",
            "http://10.0.0.1/",
            "http://localhost.example/",
            "file:///tmp/x",
        ] {
            assert!(!is_trustworthy(&u(no)), "{no}");
        }
    }

    #[test]
    fn hint_lists_parse_case_insensitively_and_skip_others() {
        let h = parse_hint_list([
            "Sec-CH-UA-Platform-Version, sec-ch-ua-arch,DPR",
            " Sec-CH-UA-Model ,Sec-CH-UA-Arch",
        ]);
        assert_eq!(h, [UaHint::Arch, UaHint::Model, UaHint::PlatformVersion]);
        assert!(parse_hint_list(["Viewport-Width, Sec-CH-UA"]).is_empty());
    }

    #[test]
    fn store_replaces_per_origin_and_ignores_insecure() {
        let s = ClientHintsStore::new();
        let a = u("http://127.0.0.1:1/x");
        s.accept(&a, &["Sec-CH-UA-Arch, Sec-CH-UA-Model".into()]);
        assert_eq!(
            s.get(&u("http://127.0.0.1:1/other")),
            [UaHint::Arch, UaHint::Model]
        );
        assert!(s.get(&u("http://127.0.0.1:2/")).is_empty(), "per origin");
        let shared = s.clone();
        shared.accept(&a, &["Sec-CH-UA-Bitness".into()]);
        assert_eq!(s.get(&a), [UaHint::Bitness], "replaced, shared by clones");
        s.accept(&a, &[String::new()]);
        assert!(s.get(&a).is_empty(), "empty Accept-CH clears");
        let insecure = u("http://example.test/");
        s.accept(&insecure, &["Sec-CH-UA-Arch".into()]);
        assert!(s.get(&insecure).is_empty());
    }

    #[test]
    fn delegation_is_same_origin_only() {
        let s = ClientHintsStore::new();
        let top = u("http://127.0.0.1:1/page");
        s.accept(&top, &["Sec-CH-UA-Arch".into()]);
        let other = u("http://127.0.0.1:2/frame");
        let meta = |kind, init: Option<&Url>, top_frame: Option<&Url>| {
            let mut m = RequestMeta::new(kind, init.cloned());
            m.top_frame = top_frame.cloned();
            m
        };
        let arch = [UaHint::Arch];
        let nav = meta(RequestKind::Document, None, None);
        assert_eq!(
            hints_for_request(&s, &nav, &u("http://127.0.0.1:1/next")),
            arch
        );
        assert!(
            hints_for_request(&s, &nav, &other).is_empty(),
            "own origin's set"
        );
        let img = meta(RequestKind::Image, Some(&top), None);
        assert_eq!(
            hints_for_request(&s, &img, &u("http://127.0.0.1:1/i.png")),
            arch
        );
        assert!(hints_for_request(&s, &img, &u("http://127.0.0.1:2/i.png")).is_empty());
        let frame = meta(RequestKind::Iframe, Some(&top), Some(&top));
        assert_eq!(
            hints_for_request(&s, &frame, &u("http://127.0.0.1:1/f")),
            arch
        );
        assert!(
            hints_for_request(&s, &frame, &other).is_empty(),
            "cross-origin frame"
        );
        // Requests of a cross-origin frame get nothing, even to the top origin.
        let in_frame = meta(RequestKind::Fetch, Some(&other), Some(&top));
        assert!(hints_for_request(&s, &in_frame, &u("http://127.0.0.1:1/api")).is_empty());
        assert!(hints_for_request(&s, &in_frame, &u("http://127.0.0.1:2/api")).is_empty());
        let pre = meta(RequestKind::Preflight, Some(&top), None);
        assert!(hints_for_request(&s, &pre, &u("http://127.0.0.1:1/api")).is_empty());
    }

    #[test]
    fn values_are_structured_fields() {
        let h = HighEntropyHints {
            platform: "Linux".into(),
            platform_version: String::new(),
            architecture: "x86".into(),
            bitness: "64".into(),
            model: String::new(),
            wow64: false,
            full_version_list: vec![
                BrandVersion::new("Chromium", "153.0.8010.12"),
                BrandVersion::new("Not_A Brand", "8.0.0.0"),
            ],
            full_version: "153.0.8010.12".into(),
            form_factors: vec!["Desktop".into()],
        };
        // Captured from Chrome for Testing 153 on Linux.
        assert_eq!(UaHint::PlatformVersion.value(&h), "\"\"");
        assert_eq!(UaHint::Arch.value(&h), "\"x86\"");
        assert_eq!(UaHint::Bitness.value(&h), "\"64\"");
        assert_eq!(UaHint::Model.value(&h), "\"\"");
        assert_eq!(UaHint::Wow64.value(&h), "?0");
        assert_eq!(
            UaHint::FullVersionList.value(&h),
            "\"Chromium\";v=\"153.0.8010.12\", \"Not_A Brand\";v=\"8.0.0.0\""
        );
        assert_eq!(UaHint::FormFactors.value(&h), "\"Desktop\"");
        assert_eq!(UaHint::FullVersion.value(&h), "\"153.0.8010.12\"");
    }
}
