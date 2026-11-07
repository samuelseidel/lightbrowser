//! Per-request-kind header sets: which headers a browser sends for a
//! navigation, script, stylesheet, image, font, `fetch`/XHR and CORS
//! preflight request, in which order and with which name case.
//!
//! The data is a profile file captured from a real browser on loopback
//! (`profiles/headers/*.json`, ADR 0011). Values (user agent, client hints,
//! language, referrer, cookie) come from the context's persona and the
//! request, never from the file, apart from fixed values such as `Accept`
//! and `Priority`.
//!
//! Only Chrome has a profile so far. Firefox keeps the navigation set in
//! [`crate::headers`] with a small per-kind overlay (M4b captures the real
//! thing).

use std::collections::HashSet;

use http::Method;
use lb_persona::{BrowserFamily, Persona};
use serde::Deserialize;
use url::{Host, Url};

use crate::HttpVersion;
use crate::client_hints::UaHint;
use crate::error::NetError;
use crate::headers::OrderedHeaders;

/// Whether `url` is a potentially trustworthy destination (Chrome's
/// `network::IsUrlPotentiallyTrustworthy`): `https:`/`wss:`, or `http:`/
/// `ws:` to `localhost`, `*.localhost` or a loopback address.
pub fn potentially_trustworthy(url: &Url) -> bool {
    match url.scheme() {
        "https" | "wss" => true,
        "http" | "ws" => match url.host() {
            Some(Host::Domain(d)) => {
                let d = d.to_ascii_lowercase();
                d == "localhost" || d.ends_with(".localhost")
            }
            Some(Host::Ipv4(a)) => a.is_loopback(),
            Some(Host::Ipv6(a)) => a.is_loopback(),
            None => false,
        },
        _ => false,
    }
}

/// Chrome's headers to a destination that is not potentially trustworthy
/// (recorded from Chrome for Testing 153 for every request kind,
/// `tests/proxy/insecure_serve.py`): no `sec-ch-*` client hints and no
/// `Sec-Fetch-*`, `Accept-Encoding: gzip, deflate` (no `br`/`zstd`), and
/// `Accept-Language` right after `Accept-Encoding`. A page's own
/// `Accept-Language` stays where it is.
fn insecure_destination(out: &mut OrderedHeaders, overrides: &[(String, String)]) {
    out.retain(|(n, _)| {
        let l = n.to_ascii_lowercase();
        !l.starts_with("sec-ch-") && !l.starts_with("sec-fetch-")
    });
    let page_lang = overrides.iter().any(|(n, _)| n == "accept-language");
    let Some(ae) = out
        .iter()
        .position(|(n, _)| n.eq_ignore_ascii_case("accept-encoding"))
    else {
        return;
    };
    out[ae].1 = "gzip, deflate".into();
    if page_lang {
        return;
    }
    if let Some(al) = out
        .iter()
        .position(|(n, _)| n.eq_ignore_ascii_case("accept-language"))
        && al < ae
    {
        let h = out.remove(al);
        out.insert(ae, h);
    }
}

/// The built-in Chrome 153 profile (`profiles/headers/chrome_153.json`).
pub const CHROME_153_JSON: &str = include_str!("../../../profiles/headers/chrome_153.json");

/// What a request is for. Picks the header set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestKind {
    /// Top-level navigation.
    Document,
    /// Frame navigation.
    Iframe,
    /// Classic `<script src>`.
    Script,
    /// Module script (CORS mode, sends `Origin`).
    ModuleScript,
    /// `<link rel=stylesheet>`.
    Style,
    /// `<img>` and other image elements.
    Image,
    /// `/favicon.ico` and `<link rel=icon>`; also CSS background images.
    Favicon,
    /// Web font (CORS mode, sends `Origin`).
    Font,
    /// `fetch()` and `XMLHttpRequest` (`Sec-Fetch-Dest: empty`).
    Fetch,
    /// CORS preflight (`OPTIONS`).
    Preflight,
}

impl RequestKind {
    fn key(self) -> &'static str {
        match self {
            RequestKind::Document => "document",
            RequestKind::Iframe => "iframe",
            RequestKind::Script => "script",
            RequestKind::ModuleScript => "module_script",
            RequestKind::Style => "style",
            RequestKind::Image => "image",
            RequestKind::Favicon => "favicon",
            RequestKind::Font => "font",
            RequestKind::Fetch => "fetch",
            RequestKind::Preflight => "preflight",
        }
    }
}

/// The `Access-Control-Request-*` pair of a preflight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightInfo {
    pub method: String,
    /// Header names of the actual request (lowercase, sorted by the caller).
    pub headers: Vec<String>,
}

/// Request facts that decide which headers are sent. Everything fingerprint
/// related still comes from the persona.
#[derive(Debug, Clone)]
pub struct RequestMeta {
    pub kind: RequestKind,
    /// `Sec-Fetch-Mode` for [`RequestKind::Fetch`]: `cors`, `no-cors` or
    /// `same-origin`. Other kinds have a fixed mode.
    pub mode: String,
    /// The document (or stylesheet, for fonts) that made the request. `None`
    /// for a browser-initiated navigation: `Sec-Fetch-Site: none`, no
    /// `Referer`, no `Origin`.
    pub initiator: Option<Url>,
    /// Navigation caused by a user gesture: adds `Sec-Fetch-User: ?1`.
    /// True for typed or `Page.navigate` navigations.
    pub user_activated: bool,
    /// Replaces the profile's `Priority` value (for example `u=4, i` for
    /// `sendBeacon`). HTTP/2 only.
    pub priority: Option<String>,
    /// Only for [`RequestKind::Preflight`].
    pub preflight: Option<PreflightInfo>,
    /// The top-level document of the frame that made the request. Its
    /// origin's `Accept-CH` set decides which high-entropy client hints a
    /// subresource or frame request carries ([`crate::client_hints`]).
    /// `None`: the initiator is the top-level document (a top-level
    /// navigation uses its own target).
    pub top_frame: Option<Url>,
}

impl RequestMeta {
    pub fn new(kind: RequestKind, initiator: Option<Url>) -> Self {
        RequestMeta {
            kind,
            mode: "cors".into(),
            user_activated: kind == RequestKind::Document && initiator.is_none(),
            initiator,
            priority: None,
            preflight: None,
            top_frame: None,
        }
    }

    /// A typed top-level navigation: the default for [`crate::Request::get`].
    pub fn navigation() -> Self {
        Self::new(RequestKind::Document, None)
    }
}

impl Default for RequestMeta {
    fn default() -> Self {
        Self::navigation()
    }
}

/// `Sec-Fetch-Site`, in increasing distance. A redirect chain reports the
/// largest value seen on any hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Site {
    None,
    SameOrigin,
    SameSite,
    CrossSite,
}

impl Site {
    pub fn as_str(self) -> &'static str {
        match self {
            Site::None => "none",
            Site::SameOrigin => "same-origin",
            Site::SameSite => "same-site",
            Site::CrossSite => "cross-site",
        }
    }

    /// Site relation of a request for `target` made by `initiator`.
    pub fn between(initiator: Option<&Url>, target: &Url) -> Site {
        let Some(i) = initiator else {
            return Site::None;
        };
        if i.origin() == target.origin() {
            return Site::SameOrigin;
        }
        if i.scheme() == target.scheme() && registrable(i) == registrable(target) {
            Site::SameSite
        } else {
            Site::CrossSite
        }
    }
}

/// The part of the host that decides "same site": the last two labels of a
/// domain, the whole address for an IP literal. (No public-suffix list.)
fn registrable(u: &Url) -> String {
    match u.host() {
        Some(Host::Domain(d)) => {
            let labels: Vec<&str> = d.rsplit('.').take(2).collect();
            labels.join(".")
        }
        Some(h) => h.to_string(),
        None => String::new(),
    }
}

/// `Referer` for a request, default policy `strict-origin-when-cross-origin`.
pub fn referrer_for(initiator: Option<&Url>, target: &Url) -> Option<String> {
    let i = initiator?;
    if !matches!(i.scheme(), "http" | "https") {
        return None;
    }
    // https page to http target: nothing.
    if i.scheme() == "https" && target.scheme() == "http" {
        return None;
    }
    if i.origin() == target.origin() {
        let mut r = i.clone();
        r.set_fragment(None);
        let _ = r.set_username("");
        let _ = r.set_password(None);
        return Some(r.to_string());
    }
    // Cross-origin: origin only.
    Some(format!("{}/", i.origin().ascii_serialization()))
}

/// `Origin` for a request, or `None` when the browser sends none.
fn origin_for(meta: &RequestMeta, target: &Url, method: &Method) -> Option<String> {
    let i = meta.initiator.as_ref()?;
    let safe = matches!(*method, Method::GET | Method::HEAD);
    let send = match meta.kind {
        RequestKind::ModuleScript | RequestKind::Font | RequestKind::Preflight => true,
        RequestKind::Fetch => match meta.mode.as_str() {
            "no-cors" => !safe,
            "same-origin" => !safe,
            _ => !safe || i.origin() != target.origin(),
        },
        _ => false,
    };
    send.then(|| i.origin().ascii_serialization())
}

/// One entry of a request kind's header list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Slot {
    /// Value computed from the persona or request.
    Computed(String),
    /// Fixed value.
    Fixed(String, String),
    /// The renderer header group. `true` also puts `Origin` in it.
    Group { origin: bool },
}

impl Slot {
    fn parse(s: &str) -> Slot {
        if let Some(rest) = s.strip_prefix('@') {
            return Slot::Group {
                origin: rest.ends_with("+origin"),
            };
        }
        match s.split_once(": ") {
            Some((n, v)) => Slot::Fixed(n.to_owned(), v.to_owned()),
            None => Slot::Computed(s.to_owned()),
        }
    }

    fn name(&self) -> Option<&str> {
        match self {
            Slot::Computed(n) | Slot::Fixed(n, _) => Some(n),
            Slot::Group { .. } => None,
        }
    }
}

#[derive(Deserialize)]
struct Raw {
    profile_id: String,
    family: String,
    h1: RawH1,
    requests: std::collections::BTreeMap<String, Vec<String>>,
    renderer_group: RawGroup,
}

#[derive(Deserialize)]
struct RawH1 {
    prefix: Vec<String>,
    drop: Vec<String>,
    lowercase_prefixes: Vec<String>,
}

#[derive(Deserialize)]
struct RawGroup {
    base: Vec<String>,
    observed: Vec<RawCombo>,
    /// Orders seen with high-entropy client hints, per hint set.
    #[serde(default)]
    observed_hints: Vec<RawHintCombo>,
}

#[derive(Deserialize)]
struct RawHintCombo {
    hints: Vec<String>,
    extra: Vec<String>,
    order: Vec<String>,
}

#[derive(Deserialize)]
struct RawCombo {
    extra: Vec<String>,
    order: Vec<String>,
}

/// Everything [`HeaderProfile::build`] needs to know about one hop.
pub struct BuildInput<'a> {
    pub persona: &'a Persona,
    pub version: HttpVersion,
    pub meta: &'a RequestMeta,
    pub url: &'a Url,
    pub method: &'a Method,
    pub body_len: usize,
    /// `Cookie` value from the jar, if any.
    pub cookie: Option<&'a str>,
    /// Headers the page set (`fetch` init, `setRequestHeader`), in order.
    pub caller: &'a OrderedHeaders,
    /// 0 for the first request, 1.. for redirect hops.
    pub hop: usize,
    /// `Sec-Fetch-Site` for this hop (the chain maximum).
    pub site: Site,
    /// High-entropy client hints this request carries (the origin asked
    /// for them with `Accept-CH`; see [`crate::client_hints`]).
    pub hints: &'a [UaHint],
    /// The `Critical-CH` retry of a navigation (`document_restart` list).
    pub restart: bool,
}

/// Header sets for one browser build.
#[derive(Debug, Clone)]
pub struct HeaderProfile {
    pub profile_id: String,
    pub family: BrowserFamily,
    kinds: std::collections::BTreeMap<String, Vec<Slot>>,
    h1_prefix: Vec<Slot>,
    h1_drop: Vec<String>,
    h1_lower_prefixes: Vec<String>,
    group_base: Vec<String>,
    group_observed: Vec<(Vec<String>, Vec<String>)>,
    /// `(hint header names, sorted; extras; order)`.
    group_hinted: Vec<HintCombo>,
}

type HintCombo = (Vec<String>, Vec<String>, Vec<String>);

/// Names whose value the persona decides; a page header of the same name
/// replaces the value in place.
const GROUP_MEMBERS: &[&str] = &[
    "user-agent",
    "accept-language",
    "sec-ch-ua",
    "sec-ch-ua-mobile",
    "sec-ch-ua-platform",
];

impl HeaderProfile {
    /// Parse a profile file.
    pub fn from_json(json: &str) -> Result<HeaderProfile, NetError> {
        let raw: Raw = serde_json::from_str(json)?;
        let family = match raw.family.as_str() {
            "chrome" => BrowserFamily::Chrome,
            "firefox" => BrowserFamily::Firefox,
            other => return Err(NetError::Profile(format!("unknown family {other:?}"))),
        };
        let kinds = raw
            .requests
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().map(|s| Slot::parse(s)).collect()))
            .collect();
        let lower = |v: &[String]| v.iter().map(|s| s.to_ascii_lowercase()).collect::<Vec<_>>();
        let observed = raw
            .renderer_group
            .observed
            .iter()
            .map(|c| (lower(&c.extra), lower(&c.order)))
            .collect();
        let hinted = raw
            .renderer_group
            .observed_hints
            .iter()
            .map(|c| {
                let mut h = lower(&c.hints);
                h.sort();
                (h, lower(&c.extra), lower(&c.order))
            })
            .collect();
        let p = HeaderProfile {
            profile_id: raw.profile_id,
            family,
            kinds,
            h1_prefix: raw.h1.prefix.iter().map(|s| Slot::parse(s)).collect(),
            h1_drop: lower(&raw.h1.drop),
            h1_lower_prefixes: lower(&raw.h1.lowercase_prefixes),
            group_base: lower(&raw.renderer_group.base),
            group_observed: observed,
            group_hinted: hinted,
        };
        for k in [
            RequestKind::Document,
            RequestKind::Iframe,
            RequestKind::Script,
            RequestKind::ModuleScript,
            RequestKind::Style,
            RequestKind::Image,
            RequestKind::Favicon,
            RequestKind::Font,
            RequestKind::Fetch,
            RequestKind::Preflight,
        ] {
            if !p.kinds.contains_key(k.key()) {
                return Err(NetError::Profile(format!(
                    "header profile {} has no {:?} list",
                    p.profile_id,
                    k.key()
                )));
            }
        }
        if !p.kinds.contains_key("document_redirect") {
            return Err(NetError::Profile(
                "header profile has no document_redirect list".into(),
            ));
        }
        Ok(p)
    }

    /// The profile built into the binary for `family`, if there is one.
    pub fn builtin(family: BrowserFamily) -> Option<HeaderProfile> {
        match family {
            BrowserFamily::Chrome => Some(
                HeaderProfile::from_json(CHROME_153_JSON)
                    .expect("built-in header profile chrome_153.json is valid"),
            ),
            BrowserFamily::Firefox => None,
        }
    }

    /// Read a profile from a file (`profiles/headers/*.json`).
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<HeaderProfile, NetError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| NetError::Profile(format!("{}: {e}", path.display())))?;
        HeaderProfile::from_json(&text)
    }

    /// Headers for one request, in wire order. HTTP/2 names are lowercase;
    /// HTTP/1.1 names have the browser's case (profile slots) or the case the
    /// page used (page headers).
    pub fn build(&self, input: &BuildInput<'_>) -> OrderedHeaders {
        let meta = input.meta;
        let key = if meta.kind == RequestKind::Document
            && input.restart
            && self.kinds.contains_key("document_restart")
        {
            "document_restart"
        } else if meta.kind == RequestKind::Document && input.hop > 0 {
            "document_redirect"
        } else {
            meta.kind.key()
        };
        let slots = &self.kinds[key];
        let has_group = slots.iter().any(|s| matches!(s, Slot::Group { .. }));
        let h1 = input.version == HttpVersion::Http1;

        // Page headers: those with a slot (or a persona-owned name) replace
        // the value in place; the rest are "extras" that join the renderer
        // group, or trail the list when the kind has no group.
        let mut overrides: Vec<(String, String)> = Vec::new();
        let mut extras: Vec<(String, String)> = Vec::new();
        for (n, v) in input.caller {
            let l = n.to_ascii_lowercase();
            let slot_named = slots.iter().any(|s| s.name() == Some(l.as_str()));
            // With a group, the page's Accept moves into the group
            // (observed); without one it replaces the slot.
            let moves = has_group && l == "accept";
            if GROUP_MEMBERS.contains(&l.as_str()) || (slot_named && !moves) {
                overrides.push((l, v.clone()));
            } else {
                extras.push((n.clone(), v.clone()));
            }
        }

        let origin = origin_for(meta, input.url, input.method);
        let referer = referrer_for(meta.initiator.as_ref(), input.url);
        let value = |name: &str| -> Option<String> {
            if let Some((_, v)) = overrides.iter().rev().find(|(n, _)| n == name) {
                return Some(v.clone());
            }
            self.computed(name, input, origin.as_deref(), referer.as_deref())
        };

        let mut out: OrderedHeaders = Vec::new();
        let mut push = |name: &str, value: String, page_case: Option<&str>| {
            if h1 {
                let n = page_case.map_or_else(|| self.h1_name(name), str::to_owned);
                out.push((n, value));
            } else {
                out.push((name.to_ascii_lowercase(), value));
            }
        };

        if h1 {
            for s in &self.h1_prefix {
                match s {
                    Slot::Fixed(n, v) => push(n, v.clone(), None),
                    Slot::Computed(n) => {
                        if let Some(v) = value(n) {
                            push(n, v, None);
                        }
                    }
                    Slot::Group { .. } => {}
                }
            }
        }
        for slot in slots {
            match slot {
                Slot::Fixed(n, v) => {
                    if h1 && self.h1_drop.iter().any(|d| d == n) {
                        continue;
                    }
                    // Moved into the group by the page's own value.
                    if has_group && n == "accept" && extras_has(&extras, "accept") {
                        continue;
                    }
                    let v = if n == "priority" {
                        meta.priority.clone().unwrap_or_else(|| v.clone())
                    } else {
                        overrides
                            .iter()
                            .rev()
                            .find(|(on, _)| on == n)
                            .map_or_else(|| v.clone(), |(_, ov)| ov.clone())
                    };
                    push(n, v, None);
                }
                Slot::Computed(n) => {
                    if h1 && self.h1_drop.iter().any(|d| d == n) {
                        continue;
                    }
                    if let Some(v) = value(n) {
                        push(n, v, None);
                    }
                }
                Slot::Group {
                    origin: with_origin,
                } => {
                    let mut group_extras = extras.clone();
                    if *with_origin && let Some(o) = &origin {
                        group_extras.insert(0, ("Origin".into(), o.clone()));
                    }
                    for (n, v, page_case) in self.group(&group_extras, input.hints, &value) {
                        push(&n, v, page_case.as_deref());
                    }
                }
            }
        }
        // Page headers on a kind without a group go last.
        if !has_group {
            for (n, v) in &extras {
                push(&n.to_ascii_lowercase(), v.clone(), Some(n));
            }
        }
        if !potentially_trustworthy(input.url) {
            insecure_destination(&mut out, &overrides);
        }
        out
    }

    /// The renderer group in Blink's order: `(lowercase name, value, page
    /// case for extras)`.
    fn group(
        &self,
        extras: &[(String, String)],
        hints: &[UaHint],
        value: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<(String, String, Option<String>)> {
        let want: HashSet<String> = extras.iter().map(|(n, _)| n.to_ascii_lowercase()).collect();
        let mut hint_names: Vec<String> = hints.iter().map(|h| h.header().to_owned()).collect();
        hint_names.sort();
        // Largest observed combination contained in the page's set, among
        // the orders seen with exactly this hint set (or without hints).
        let mut order: Vec<String> = self.group_base.clone();
        let mut best = 0usize;
        let mut found = hint_names.is_empty();
        let mut covered: HashSet<&str> = HashSet::new();
        if hint_names.is_empty() {
            for (set, o) in &self.group_observed {
                if set.len() > best && set.iter().all(|s| want.contains(s)) {
                    best = set.len();
                    order = o.clone();
                    covered = set.iter().map(String::as_str).collect();
                }
            }
        } else {
            for (h, set, o) in &self.group_hinted {
                if *h != hint_names || !set.iter().all(|s| want.contains(s)) {
                    continue;
                }
                if !found || set.len() > best {
                    found = true;
                    best = set.len();
                    order = o.clone();
                    covered = set.iter().map(String::as_str).collect();
                }
            }
        }
        if !found {
            // A hint set we never saw: the order without hints, the hints
            // right after user-agent (Blink's hash order is not modelled).
            for (set, o) in &self.group_observed {
                if set.len() > best && set.iter().all(|s| want.contains(s)) {
                    best = set.len();
                    order = o.clone();
                    covered = set.iter().map(String::as_str).collect();
                }
            }
            let at = order
                .iter()
                .position(|n| n == "user-agent")
                .map_or(order.len(), |p| p + 1);
            for (k, n) in hint_names.iter().enumerate() {
                order.insert(at + k, n.clone());
            }
        }
        // Unseen page headers go right after user-agent.
        let mut leftover: Vec<String> = Vec::new();
        for (n, _) in extras {
            let l = n.to_ascii_lowercase();
            if !covered.contains(l.as_str()) && !leftover.contains(&l) {
                leftover.push(l);
            }
        }
        if !leftover.is_empty() {
            let at = order
                .iter()
                .position(|n| n == "user-agent")
                .map_or(order.len(), |p| p + 1);
            for (k, l) in leftover.into_iter().enumerate() {
                order.insert(at + k, l);
            }
        }
        let mut out = Vec::new();
        for name in order {
            if GROUP_MEMBERS.contains(&name.as_str()) || UaHint::from_header(&name).is_some() {
                if let Some(v) = value(&name) {
                    out.push((name, v, None));
                }
                continue;
            }
            // Page headers of this name, in the order the page set them.
            for (n, v) in extras.iter().filter(|(n, _)| n.eq_ignore_ascii_case(&name)) {
                out.push((name.clone(), v.clone(), Some(n.clone())));
            }
        }
        out
    }

    fn computed(
        &self,
        name: &str,
        input: &BuildInput<'_>,
        origin: Option<&str>,
        referer: Option<&str>,
    ) -> Option<String> {
        let p = input.persona;
        let meta = input.meta;
        match name {
            "host" => {
                let h = input.url.host_str()?;
                Some(match input.url.port() {
                    Some(port) => format!("{h}:{port}"),
                    None => h.to_owned(),
                })
            }
            "user-agent" => Some(p.user_agent.clone()),
            "accept-language" => Some(p.accept_language.clone()),
            "sec-ch-ua" => p.sec_ch_ua.clone(),
            "sec-ch-ua-mobile" => p.sec_ch_ua_mobile.clone(),
            // The persona stores the platform unquoted.
            "sec-ch-ua-platform" => p.sec_ch_ua_platform.as_ref().map(|v| format!("\"{v}\"")),
            "referer" => referer.map(str::to_owned),
            "origin" => origin.map(str::to_owned),
            "cookie" => input.cookie.map(str::to_owned),
            "content-length" => {
                let has_body = input.body_len > 0
                    || matches!(*input.method, Method::POST | Method::PUT | Method::PATCH);
                has_body.then(|| input.body_len.to_string())
            }
            "sec-fetch-site" => Some(input.site.as_str().to_owned()),
            "sec-fetch-mode" => Some(meta.mode.clone()),
            "sec-fetch-user" => meta.user_activated.then(|| "?1".to_owned()),
            "access-control-request-method" => meta.preflight.as_ref().map(|p| p.method.clone()),
            n if n.starts_with("sec-ch-ua-") => {
                let hint = UaHint::from_header(n)?;
                if !input.hints.contains(&hint) {
                    return None;
                }
                p.high_entropy.as_ref().map(|h| hint.value(h))
            }
            "access-control-request-headers" => meta
                .preflight
                .as_ref()
                .filter(|p| !p.headers.is_empty())
                .map(|p| p.headers.join(",")),
            _ => None,
        }
    }

    /// HTTP/1.1 wire name for a profile slot.
    pub fn h1_name(&self, lower: &str) -> String {
        if self.h1_lower_prefixes.iter().any(|p| lower.starts_with(p)) {
            return lower.to_owned();
        }
        title_case(lower)
    }
}

fn extras_has(extras: &[(String, String)], lower: &str) -> bool {
    extras.iter().any(|(n, _)| n.eq_ignore_ascii_case(lower))
}

/// `accept-language` -> `Accept-Language`.
pub(crate) fn title_case(lower: &str) -> String {
    let mut out = String::with_capacity(lower.len());
    let mut up = true;
    for c in lower.chars() {
        out.push(if up { c.to_ascii_uppercase() } else { c });
        up = c == '-';
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> HeaderProfile {
        HeaderProfile::from_json(CHROME_153_JSON).unwrap()
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn names(h: &OrderedHeaders) -> Vec<&str> {
        h.iter().map(|(n, _)| n.as_str()).collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        kind: RequestKind,
        version: HttpVersion,
        init: Option<&str>,
        target: &str,
        method: Method,
        body: usize,
        cookie: Option<&str>,
        caller: &[(&str, &str)],
    ) -> OrderedHeaders {
        let persona = Persona::default_chrome();
        let initiator = init.map(url);
        let target = url(target);
        let mut meta = RequestMeta::new(kind, initiator.clone());
        if kind == RequestKind::Script && init.is_some() {
            meta.mode = "no-cors".into();
        }
        let caller: OrderedHeaders = caller
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect();
        profile().build(&BuildInput {
            persona: &persona,
            version,
            meta: &meta,
            url: &target,
            method: &method,
            body_len: body,
            cookie,
            caller: &caller,
            hop: 0,
            site: Site::between(initiator.as_ref(), &target),
            hints: &[],
            restart: false,
        })
    }

    /// Every request kind to a non-trustworthy `http:` origin equals
    /// Chrome 153's recording (`captures/chrome_153_linux_insecure.jsonl`):
    /// names, order and values (the recording's headless User-Agent aside).
    #[test]
    fn insecure_destination_matches_chrome() {
        let text =
            include_str!("../../../profiles/headers/captures/chrome_153_linux_insecure.jsonl");
        let base = "http://insecure.test:18520/";
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            let path = v["line"].as_str().unwrap().split(' ').nth(1).unwrap();
            let (kind, init) = match path {
                "/" => (RequestKind::Document, None),
                "/frame.html" => (RequestKind::Iframe, Some(base)),
                "/script.js" => (RequestKind::Script, Some(base)),
                "/module.js" => (RequestKind::ModuleScript, Some(base)),
                "/style.css" => (RequestKind::Style, Some(base)),
                "/img.png" => (RequestKind::Image, Some(base)),
                "/favicon.ico" => (RequestKind::Favicon, Some(base)),
                "/font.woff2" => (RequestKind::Font, Some(base)),
                "/api" => (RequestKind::Fetch, Some(base)),
                other => panic!("{other}"),
            };
            let h = build(
                kind,
                HttpVersion::Http1,
                init,
                &format!("http://insecure.test:18520{path}"),
                Method::GET,
                0,
                None,
                &[],
            );
            let want: Vec<(String, String)> = v["headers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| (p[0].as_str().unwrap().into(), p[1].as_str().unwrap().into()))
                .filter(|(n, _)| n != "User-Agent")
                .collect();
            let got: Vec<(String, String)> =
                h.into_iter().filter(|(n, _)| n != "User-Agent").collect();
            assert_eq!(got, want, "{path}");
        }
        assert!(potentially_trustworthy(&url("http://a.localhost/")));
        assert!(potentially_trustworthy(&url("http://[::1]/")));
        assert!(!potentially_trustworthy(&url("http://10.0.0.1/")));
    }

    #[test]
    fn builtin_profile_parses() {
        let p = profile();
        assert_eq!(p.family, BrowserFamily::Chrome);
        assert!(HeaderProfile::builtin(BrowserFamily::Firefox).is_none());
    }

    #[test]
    fn document_h1_names_and_case() {
        let h = build(
            RequestKind::Document,
            HttpVersion::Http1,
            None,
            "http://127.0.0.1:8080/",
            Method::GET,
            0,
            None,
            &[],
        );
        assert_eq!(
            names(&h),
            [
                "Host",
                "Connection",
                "sec-ch-ua",
                "sec-ch-ua-mobile",
                "sec-ch-ua-platform",
                "Accept-Language",
                "Upgrade-Insecure-Requests",
                "User-Agent",
                "Accept",
                "Sec-Fetch-Site",
                "Sec-Fetch-Mode",
                "Sec-Fetch-User",
                "Sec-Fetch-Dest",
                "Accept-Encoding",
            ]
        );
        assert_eq!(h[0].1, "127.0.0.1:8080");
    }

    #[test]
    fn document_h2_is_lowercase_with_cookie_then_priority() {
        let h = build(
            RequestKind::Document,
            HttpVersion::Http2,
            None,
            "https://a.test/",
            Method::GET,
            0,
            Some("a=1"),
            &[],
        );
        assert!(h.iter().all(|(n, _)| *n == n.to_ascii_lowercase()));
        let n = names(&h);
        // Captured with a raw h2 decoder (accept_ch capture); the first
        // capture's decoder moved `cookie` last.
        assert_eq!(&n[n.len() - 2..], ["cookie", "priority"]);
        assert_eq!(n[0], "sec-ch-ua");
        assert!(!n.contains(&"host"));
    }

    #[test]
    fn script_group_order_and_referer() {
        let h = build(
            RequestKind::Script,
            HttpVersion::Http1,
            Some("http://127.0.0.1:1/page#frag"),
            "http://127.0.0.1:1/a.js",
            Method::GET,
            0,
            Some("sid=abc"),
            &[],
        );
        assert_eq!(
            names(&h),
            [
                "Host",
                "Connection",
                "sec-ch-ua-platform",
                "Accept-Language",
                "sec-ch-ua",
                "User-Agent",
                "sec-ch-ua-mobile",
                "Accept",
                "Sec-Fetch-Site",
                "Sec-Fetch-Mode",
                "Sec-Fetch-Dest",
                "Referer",
                "Accept-Encoding",
                "Cookie",
            ]
        );
        let get = |n: &str| h.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
        assert_eq!(get("Referer"), Some("http://127.0.0.1:1/page"));
        assert_eq!(get("Sec-Fetch-Mode"), Some("no-cors"));
        assert_eq!(get("Sec-Fetch-Site"), Some("same-origin"));
    }

    #[test]
    fn fetch_post_with_content_type_and_origin() {
        let h = build(
            RequestKind::Fetch,
            HttpVersion::Http2,
            Some("https://a.test/p"),
            "https://a.test/api",
            Method::POST,
            7,
            None,
            &[("content-type", "application/json")],
        );
        assert_eq!(
            names(&h),
            [
                "content-length",
                "sec-ch-ua-platform",
                "accept-language",
                "sec-ch-ua",
                "content-type",
                "sec-ch-ua-mobile",
                "user-agent",
                "accept",
                "origin",
                "sec-fetch-site",
                "sec-fetch-mode",
                "sec-fetch-dest",
                "referer",
                "accept-encoding",
                "priority",
            ]
        );
    }

    #[test]
    fn same_origin_get_fetch_has_no_origin_cross_origin_has() {
        let same = build(
            RequestKind::Fetch,
            HttpVersion::Http2,
            Some("https://a.test/p"),
            "https://a.test/api",
            Method::GET,
            0,
            None,
            &[],
        );
        assert!(!names(&same).contains(&"origin"));
        let cross = build(
            RequestKind::Fetch,
            HttpVersion::Http2,
            Some("https://a.test/p"),
            "https://b.test/api",
            Method::GET,
            0,
            None,
            &[],
        );
        let n = names(&cross);
        let i = n.iter().position(|x| *x == "origin").unwrap();
        assert_eq!(n[i - 1], "accept");
        assert_eq!(n[i + 1], "sec-fetch-site");
        let get = |n: &str| cross.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
        assert_eq!(get("sec-fetch-site"), Some("cross-site"));
        assert_eq!(get("referer"), Some("https://a.test/"));
    }

    #[test]
    fn module_script_and_font_put_origin_first_in_group() {
        for kind in [RequestKind::ModuleScript, RequestKind::Font] {
            let h = build(
                kind,
                HttpVersion::Http2,
                Some("https://a.test/p"),
                "https://a.test/x",
                Method::GET,
                0,
                None,
                &[],
            );
            assert_eq!(
                names(&h)[..3],
                ["origin", "sec-ch-ua-platform", "accept-language"]
            );
        }
    }

    #[test]
    fn page_headers_keep_case_on_h1_and_unseen_ones_follow_user_agent() {
        // A trustworthy destination (https): `http://a.test` gets no client
        // hints (see `insecure_destination_matches_chrome`).
        let h = build(
            RequestKind::Fetch,
            HttpVersion::Http1,
            Some("https://a.test/p"),
            "https://a.test/api",
            Method::GET,
            0,
            None,
            &[("X-Custom-Zzz", "1")],
        );
        let n = names(&h);
        let i = n.iter().position(|x| *x == "User-Agent").unwrap();
        assert_eq!(n[i + 1], "X-Custom-Zzz");
        assert_eq!(n[i + 2], "sec-ch-ua-mobile");
    }

    #[test]
    fn page_accept_moves_into_group() {
        let h = build(
            RequestKind::Fetch,
            HttpVersion::Http2,
            Some("https://a.test/p"),
            "https://a.test/api",
            Method::GET,
            0,
            None,
            &[("Accept", "application/json")],
        );
        let n = names(&h);
        assert_eq!(n.iter().filter(|x| **x == "accept").count(), 1);
        assert_eq!(n[..3], ["sec-ch-ua-platform", "accept-language", "accept"]);
    }

    #[test]
    fn page_user_agent_replaces_in_place() {
        let h = build(
            RequestKind::Fetch,
            HttpVersion::Http2,
            Some("https://a.test/p"),
            "https://a.test/api",
            Method::GET,
            0,
            None,
            &[("User-Agent", "custom/1")],
        );
        let ua = h.iter().find(|(n, _)| n == "user-agent").unwrap();
        assert_eq!(ua.1, "custom/1");
        assert_eq!(h.iter().filter(|(n, _)| n == "user-agent").count(), 1);
    }

    #[test]
    fn redirect_hop_reorders_document_headers() {
        let persona = Persona::default_chrome();
        let target = url("https://a.test/next");
        let meta = RequestMeta::navigation();
        let caller = Vec::new();
        let h = profile().build(&BuildInput {
            persona: &persona,
            version: HttpVersion::Http2,
            meta: &meta,
            url: &target,
            method: &Method::GET,
            body_len: 0,
            cookie: None,
            caller: &caller,
            hop: 1,
            site: Site::None,
            hints: &[],
            restart: false,
        });
        assert_eq!(names(&h)[0], "accept-language");
        let n = names(&h);
        let ch = n.iter().position(|x| *x == "sec-ch-ua").unwrap();
        let sfd = n.iter().position(|x| *x == "sec-fetch-dest").unwrap();
        assert!(ch > sfd);
    }

    #[test]
    fn preflight_has_no_client_hints_and_language_last() {
        let persona = Persona::default_chrome();
        let target = url("https://b.test/api");
        let init = url("https://a.test/p");
        let mut meta = RequestMeta::new(RequestKind::Preflight, Some(init.clone()));
        meta.mode = "cors".into();
        meta.preflight = Some(PreflightInfo {
            method: "POST".into(),
            headers: vec!["content-type".into(), "x-pre".into()],
        });
        let caller = Vec::new();
        let h = profile().build(&BuildInput {
            persona: &persona,
            version: HttpVersion::Http1,
            meta: &meta,
            url: &target,
            method: &Method::OPTIONS,
            body_len: 0,
            cookie: None,
            caller: &caller,
            hop: 0,
            site: Site::between(Some(&init), &target),
            // Even when the origin asked for every hint.
            hints: &UaHint::ALL,
            restart: false,
        });
        assert_eq!(
            names(&h),
            [
                "Host",
                "Connection",
                "Accept",
                "Access-Control-Request-Method",
                "Access-Control-Request-Headers",
                "Origin",
                "User-Agent",
                "Sec-Fetch-Mode",
                "Sec-Fetch-Site",
                "Sec-Fetch-Dest",
                "Referer",
                "Accept-Encoding",
                "Accept-Language",
            ]
        );
    }

    #[test]
    fn site_and_referrer_rules() {
        let a = url("https://a.example.test/x");
        assert_eq!(Site::between(None, &a), Site::None);
        assert_eq!(
            Site::between(Some(&a), &url("https://a.example.test/y")),
            Site::SameOrigin
        );
        assert_eq!(
            Site::between(Some(&a), &url("https://b.example.test/")),
            Site::SameSite
        );
        assert_eq!(
            Site::between(Some(&a), &url("https://other.test/")),
            Site::CrossSite
        );
        // IP literals and localhost are separate sites.
        assert_eq!(
            Site::between(
                Some(&url("http://127.0.0.1:1/")),
                &url("http://localhost:1/")
            ),
            Site::CrossSite
        );
        assert_eq!(
            Site::between(
                Some(&url("http://127.0.0.1:1/")),
                &url("http://127.0.0.2:1/")
            ),
            Site::CrossSite
        );
        // Downgrade sends nothing.
        assert_eq!(referrer_for(Some(&a), &url("http://a.example.test/")), None);
        assert_eq!(referrer_for(None, &a), None);
    }
}
