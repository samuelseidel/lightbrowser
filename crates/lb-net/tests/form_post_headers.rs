//! Replays `profiles/headers/captures/chrome_153_linux_forms.jsonl` (Chrome
//! 153 form POST navigations, captured on loopback) through
//! `HeaderProfile::build`: header names, case and order must match, and so
//! must every value that does not come from the persona.

use http::Method;
use lb_net::HeaderProfile;
use lb_net::header_profile::{BuildInput, RequestKind, RequestMeta, Site};
use lb_net::headers::OrderedHeaders;
use lb_persona::{BrowserFamily, Persona};
use url::Url;

const CAPTURE: &str =
    include_str!("../../../profiles/headers/captures/chrome_153_linux_forms.jsonl");

/// Values the persona decides (the capture is Linux Chromium; the persona is
/// not), so only their position is compared.
const PERSONA: &[&str] = &[
    "user-agent",
    "sec-ch-ua",
    "sec-ch-ua-mobile",
    "sec-ch-ua-platform",
    "accept-language",
];

/// Paths that belong to a form submission (the POST and its redirect hops).
const SUBMIT: &[&str] = &["/submit", "/submit200", "/submit307", "/done"];

#[test]
fn form_post_capture_replays() {
    let profile = HeaderProfile::builtin(BrowserFamily::Chrome).unwrap();
    let persona = Persona::default_chrome();
    let mut checked = 0;
    let mut posts = 0;
    for line in CAPTURE.lines().filter(|l| !l.trim().is_empty()) {
        let rec: serde_json::Value = serde_json::from_str(line).unwrap();
        let scenario = rec["page"].as_str().unwrap();
        let proto = rec["proto"].as_str().unwrap();
        let (method, target) = rec["request"].as_str().unwrap().split_once(' ').unwrap();
        let captured: OrderedHeaders = rec["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    p[0].as_str().unwrap().to_owned(),
                    p[1].as_str().unwrap().to_owned(),
                )
            })
            .filter(|(n, _)| !n.starts_with(':'))
            .collect();
        let get = |name: &str| {
            captured
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        };
        let h1 = proto == "h1";
        let url = if h1 {
            Url::parse(&format!("http://{}{target}", get("host").unwrap())).unwrap()
        } else {
            Url::parse(&format!("https://fptest.localhost:18491{target}")).unwrap()
        };
        let path = url.path().to_owned();

        // Request facts, from the scenario (not from the headers under test).
        let submit = SUBMIT.contains(&path.as_str());
        let initiator = get("referer").map(|r| Url::parse(&r).unwrap());
        let kind = match get("sec-fetch-dest").as_deref() {
            Some("iframe") => RequestKind::Iframe,
            _ => RequestKind::Document,
        };
        let hop = usize::from(path == "/done" || (scenario == "307-click" && path == "/submit200"));
        let mut meta = RequestMeta::new(kind, initiator.clone());
        meta.mode = "navigate".into();
        meta.form_post = submit;
        meta.user_activated = initiator.is_none() || (submit && scenario.ends_with("-click"));
        let mut caller = OrderedHeaders::new();
        if let Some(ct) = get("content-type") {
            caller.push(("Content-Type".into(), ct));
        }
        let body_len = get("content-length").map_or(0, |v| v.parse().unwrap());
        let method = Method::from_bytes(method.as_bytes()).unwrap();
        let cookie = get("cookie");
        let built = profile.build(&BuildInput {
            persona: &persona,
            version: if h1 {
                lb_net::HttpVersion::Http1
            } else {
                lb_net::HttpVersion::Http2
            },
            meta: &meta,
            url: &url,
            method: &method,
            body_len,
            cookie: cookie.as_deref(),
            caller: &caller,
            hop,
            site: Site::between(initiator.as_ref(), &url),
            // Captured without `Accept-CH`: no high-entropy hints, no retry.
            hints: &[],
            restart: false,
        });

        let what = format!("{scenario} {proto} {method} {target}");
        let names = |h: &OrderedHeaders| h.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        assert_eq!(names(&built), names(&captured), "{what}");
        for ((n, want), (_, got)) in captured.iter().zip(&built) {
            if !PERSONA.contains(&n.to_ascii_lowercase().as_str()) {
                assert_eq!(got, want, "{what}: {n}");
            }
        }
        checked += 1;
        posts += usize::from(method == Method::POST);
    }
    assert!(checked >= 50, "only {checked} requests replayed");
    assert!(posts >= 19, "only {posts} POSTs replayed");
}
