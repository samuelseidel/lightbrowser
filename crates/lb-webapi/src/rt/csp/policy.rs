//! The Content Security Policy model (CSP Level 3 as Chrome 153 has it):
//! parsing delivered policies, matching URLs, nonces and hashes, and the
//! console texts Chrome prints. Ported from Chromium's network service
//! (`services/network/public/cpp/content_security_policy/`: parser,
//! fallbacks, browser-side navigation checks) and Blink
//! (`core/frame/csp/`: renderer-side checks and messages). No window
//! state here: checks return [`Violation`]s, which `super` turns into
//! console messages, `securitypolicyviolation` events and reports.

use std::collections::HashMap;

use url::Url;

/// Samples in violations are cut to this many characters (Blink's
/// `kMaxSampleLength`).
pub const MAX_SAMPLE: usize = 40;

/// A directive name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dir {
    BaseUri,
    BlockAllMixedContent,
    ChildSrc,
    ConnectSrc,
    DefaultSrc,
    FencedFrameSrc,
    FontSrc,
    FormAction,
    FrameAncestors,
    FrameSrc,
    ImgSrc,
    ManifestSrc,
    MediaSrc,
    ObjectSrc,
    ReportTo,
    ReportUri,
    RequireTrustedTypesFor,
    Sandbox,
    ScriptSrc,
    ScriptSrcAttr,
    ScriptSrcElem,
    StyleSrc,
    StyleSrcAttr,
    StyleSrcElem,
    TrustedTypes,
    UpgradeInsecureRequests,
    WorkerSrc,
}

const NAMES: [(Dir, &str); 27] = [
    (Dir::BaseUri, "base-uri"),
    (Dir::BlockAllMixedContent, "block-all-mixed-content"),
    (Dir::ChildSrc, "child-src"),
    (Dir::ConnectSrc, "connect-src"),
    (Dir::DefaultSrc, "default-src"),
    (Dir::FencedFrameSrc, "fenced-frame-src"),
    (Dir::FontSrc, "font-src"),
    (Dir::FormAction, "form-action"),
    (Dir::FrameAncestors, "frame-ancestors"),
    (Dir::FrameSrc, "frame-src"),
    (Dir::ImgSrc, "img-src"),
    (Dir::ManifestSrc, "manifest-src"),
    (Dir::MediaSrc, "media-src"),
    (Dir::ObjectSrc, "object-src"),
    (Dir::ReportTo, "report-to"),
    (Dir::ReportUri, "report-uri"),
    (Dir::RequireTrustedTypesFor, "require-trusted-types-for"),
    (Dir::Sandbox, "sandbox"),
    (Dir::ScriptSrc, "script-src"),
    (Dir::ScriptSrcAttr, "script-src-attr"),
    (Dir::ScriptSrcElem, "script-src-elem"),
    (Dir::StyleSrc, "style-src"),
    (Dir::StyleSrcAttr, "style-src-attr"),
    (Dir::StyleSrcElem, "style-src-elem"),
    (Dir::TrustedTypes, "trusted-types"),
    (Dir::UpgradeInsecureRequests, "upgrade-insecure-requests"),
    (Dir::WorkerSrc, "worker-src"),
];

impl Dir {
    pub fn name(self) -> &'static str {
        NAMES
            .iter()
            .find(|(d, _)| *d == self)
            .map_or("", |(_, n)| n)
    }

    /// Chrome's `ToCSPDirectiveName` (ASCII case-insensitive).
    pub fn parse(name: &str) -> Option<Dir> {
        NAMES
            .iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(name))
            .map(|(d, _)| *d)
    }

    /// `CSPFallbackDirective`: where a missing directive falls back to,
    /// for a check that started at `original`.
    fn fallback(self, original: Dir) -> Option<Dir> {
        use Dir::*;
        match self {
            ConnectSrc | FontSrc | ImgSrc | ManifestSrc | MediaSrc | ObjectSrc | ScriptSrc
            | StyleSrc => Some(DefaultSrc),
            ScriptSrcAttr | ScriptSrcElem => Some(ScriptSrc),
            StyleSrcAttr | StyleSrcElem => Some(StyleSrc),
            FencedFrameSrc => Some(FrameSrc),
            FrameSrc | WorkerSrc => Some(ChildSrc),
            ChildSrc if original == WorkerSrc => Some(ScriptSrc),
            ChildSrc => Some(DefaultSrc),
            _ => None,
        }
    }

    fn supported_in_meta(self) -> bool {
        !matches!(self, Dir::FrameAncestors | Dir::ReportUri | Dir::Sandbox)
    }

    fn supported_in_report_only(self) -> bool {
        !matches!(self, Dir::Sandbox | Dir::UpgradeInsecureRequests)
    }

    fn is_source_list(self) -> bool {
        use Dir::*;
        matches!(
            self,
            BaseUri
                | ChildSrc
                | ConnectSrc
                | DefaultSrc
                | FencedFrameSrc
                | FontSrc
                | FormAction
                | FrameAncestors
                | FrameSrc
                | ImgSrc
                | ManifestSrc
                | MediaSrc
                | ObjectSrc
                | ScriptSrc
                | ScriptSrcAttr
                | ScriptSrcElem
                | StyleSrc
                | StyleSrcAttr
                | StyleSrcElem
                | WorkerSrc
        )
    }

    /// Blink's `IsScriptDirective` (default-src counts).
    fn is_script(self) -> bool {
        matches!(
            self,
            Dir::ScriptSrc | Dir::ScriptSrcAttr | Dir::ScriptSrcElem | Dir::DefaultSrc
        )
    }

    fn is_style(self) -> bool {
        matches!(
            self,
            Dir::StyleSrc | Dir::StyleSrcAttr | Dir::StyleSrcElem | Dir::DefaultSrc
        )
    }

    /// `'strict-dynamic'` applies (Blink's `CheckDynamic`).
    fn dynamic_applies(self) -> bool {
        matches!(
            self,
            Dir::ScriptSrc | Dir::ScriptSrcAttr | Dir::ScriptSrcElem | Dir::WorkerSrc
        )
    }
}

/// Enforced or report-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    Enforce,
    Report,
}

impl Disposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Disposition::Enforce => "enforce",
            Disposition::Report => "report",
        }
    }
}

/// How a policy was delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Header,
    Meta,
}

/// Hash algorithms of hash sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HashAlg {
    Sha256,
    Sha384,
    Sha512,
    Ed25519,
}

/// One hash source (or a digest of content to compare).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hash {
    pub alg: HashAlg,
    pub value: Vec<u8>,
}

const PORT_UNSPECIFIED: i32 = -1;

/// A scheme or host source expression (`mojom::CSPSource`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub scheme: String,
    pub host: String,
    pub host_wildcard: bool,
    pub port: i32,
    pub port_wildcard: bool,
    pub path: String,
}

impl Default for Source {
    fn default() -> Self {
        Source {
            scheme: String::new(),
            host: String::new(),
            host_wildcard: false,
            port: PORT_UNSPECIFIED,
            port_wildcard: false,
            path: String::new(),
        }
    }
}

impl Source {
    fn scheme_only(&self) -> bool {
        self.host.is_empty() && !self.host_wildcard
    }

    /// The network service's serialization (browser-side messages).
    fn to_text(&self) -> String {
        if self.scheme_only() {
            return format!("{}:", self.scheme);
        }
        let mut t = String::new();
        if !self.scheme.is_empty() {
            t.push_str(&self.scheme);
            t.push_str("://");
        }
        if self.host_wildcard {
            if self.host.is_empty() {
                t.push('*');
            } else {
                t.push_str("*.");
                t.push_str(&self.host);
            }
        } else {
            t.push_str(&self.host);
        }
        if self.port_wildcard {
            t.push_str(":*");
        }
        if self.port != PORT_UNSPECIFIED {
            t.push_str(&format!(":{}", self.port));
        }
        t.push_str(&self.path);
        t
    }
}

/// A parsed source list (`mojom::CSPSourceList`).
#[derive(Clone, Debug, Default)]
pub struct SourceList {
    pub sources: Vec<Source>,
    pub nonces: Vec<String>,
    pub hashes: Vec<Hash>,
    pub allow_self: bool,
    pub allow_star: bool,
    pub allow_inline: bool,
    pub allow_inline_speculation_rules: bool,
    pub allow_eval: bool,
    pub allow_wasm_eval: bool,
    pub allow_wasm_unsafe_eval: bool,
    pub allow_dynamic: bool,
    pub allow_unsafe_hashes: bool,
    pub allow_trusted_types_eval: bool,
    pub report_sample: bool,
}

impl SourceList {
    fn hash_or_nonce(&self) -> bool {
        !self.nonces.is_empty() || !self.hashes.is_empty()
    }

    /// Blink's `CSPSourceListAllowAllInline`.
    fn allow_all_inline(&self, dir: Dir) -> bool {
        if !dir.is_script() && !dir.is_style() {
            return false;
        }
        self.allow_inline && !self.hash_or_nonce() && (!dir.is_script() || !self.allow_dynamic)
    }

    fn allow_nonce(&self, nonce: &str) -> bool {
        let n = nonce.trim_matches(is_ws);
        !n.is_empty() && self.nonces.iter().any(|x| x == n)
    }

    fn to_text(&self) -> String {
        if !self.allow_self && !self.allow_star && self.sources.is_empty() {
            return "'none'".into();
        }
        if self.allow_star {
            return "*".into();
        }
        let mut parts = Vec::new();
        if self.allow_self {
            parts.push("'self'".to_string());
        }
        parts.extend(self.sources.iter().map(Source::to_text));
        parts.join(" ")
    }
}

/// `'self'` of a policy (`ComputeSelfOrigin`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelfOrigin {
    pub scheme: String,
    pub host: String,
    pub port: i32,
}

impl SelfOrigin {
    pub fn of(url: &Url) -> SelfOrigin {
        if url.scheme() == "file" {
            return SelfOrigin {
                scheme: "file".into(),
                host: String::new(),
                port: PORT_UNSPECIFIED,
            };
        }
        SelfOrigin {
            scheme: url.scheme().to_owned(),
            host: url.host_str().unwrap_or("").to_owned(),
            port: url
                .port()
                .map(i32::from)
                .or_else(|| default_port(url.scheme()))
                .unwrap_or(PORT_UNSPECIFIED),
        }
    }

    fn as_source(&self) -> Source {
        Source {
            scheme: self.scheme.clone(),
            host: self.host.clone(),
            port: self.port,
            ..Default::default()
        }
    }
}

/// Sandbox flags a `sandbox` directive turns on (`allow-*` lifts one).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sandbox {
    /// The `allow-*` tokens given, lowercased.
    pub allow: Vec<String>,
}

impl Sandbox {
    pub fn allows(&self, token: &str) -> bool {
        self.allow.iter().any(|t| t == token)
    }
}

/// One policy.
#[derive(Clone, Debug)]
pub struct Policy {
    /// The serialized policy, as delivered (`originalPolicy`).
    pub header: String,
    pub disposition: Disposition,
    pub delivery: Delivery,
    pub self_origin: SelfOrigin,
    directives: HashMap<Dir, SourceList>,
    raw: HashMap<Dir, String>,
    /// `report-uri` endpoints (resolved URLs) or the `report-to` group.
    pub report_endpoints: Vec<String>,
    pub use_reporting_api: bool,
    pub sandbox: Option<Sandbox>,
    pub upgrade_insecure_requests: bool,
}

impl Policy {
    fn report_only(&self) -> bool {
        self.disposition == Disposition::Report
    }

    /// The directive that applies to `dir` (`OperativeDirective`).
    fn operative(&self, dir: Dir) -> (Dir, Option<&SourceList>) {
        let mut d = dir;
        loop {
            if let Some(l) = self.directives.get(&d) {
                return (d, Some(l));
            }
            match d.fallback(dir) {
                Some(n) => d = n,
                None => return (dir, None),
            }
        }
    }

    /// `"name value"` as delivered, for Blink's messages.
    fn raw_text(&self, dir: Dir) -> String {
        format!(
            "{} {}",
            dir.name(),
            self.raw.get(&dir).map_or("", String::as_str)
        )
    }

    /// Whether this policy holds a directive.
    pub fn has(&self, dir: Dir) -> bool {
        self.directives.contains_key(&dir) || self.raw.contains_key(&dir)
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn is_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0c' | '\r' | ' ')
}

fn split_trim(s: &str, sep: char) -> impl Iterator<Item = &str> {
    s.split(sep)
        .map(|p| p.trim_matches(is_ws))
        .filter(|p| !p.is_empty())
}

/// Parse a `Content-Security-Policy` (or `-Report-Only`, or `<meta>`)
/// value: comma-separated policies. `base` is the document URL
/// (`'self'`, `report-uri`). Parse errors (Chrome's console texts) are
/// appended to `errors`.
pub fn parse(
    value: &str,
    disposition: Disposition,
    delivery: Delivery,
    base: &Url,
    errors: &mut Vec<String>,
) -> Vec<Policy> {
    split_trim(value, ',')
        .map(|h| parse_one(h, disposition, delivery, base, errors))
        .collect()
}

fn parse_one(
    header: &str,
    disposition: Disposition,
    delivery: Delivery,
    base: &Url,
    errors: &mut Vec<String>,
) -> Policy {
    let mut p = Policy {
        header: header.to_owned(),
        disposition,
        delivery,
        self_origin: SelfOrigin::of(base),
        directives: HashMap::new(),
        raw: HashMap::new(),
        report_endpoints: Vec::new(),
        use_reporting_api: false,
        sandbox: None,
        upgrade_insecure_requests: false,
    };
    for token in split_trim(header, ';') {
        let (name, value) = match token.find(is_ws) {
            Some(i) => (&token[..i], token[i + 1..].trim_matches(is_ws)),
            None => (token, ""),
        };
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            errors.push(format!("The Content-Security-Policy directive name '{name}' contains one or more invalid characters. Only ASCII alphanumeric characters or dashes '-' are allowed in directive names."));
            continue;
        }
        let Some(dir) = Dir::parse(name) else {
            errors.push(unrecognized(name));
            continue;
        };
        if p.raw.contains_key(&dir) {
            errors.push(format!(
                "Ignoring duplicate Content-Security-Policy directive '{name}'."
            ));
            continue;
        }
        p.raw.insert(dir, value.to_owned());
        if !value
            .chars()
            .all(|c| is_ws(c) || (c.is_ascii_graphic() || c == ' ') && c != ',' && c != ';')
        {
            errors.push(format!("The value for the Content-Security-Policy directive '{name}' contains one or more invalid characters. In a source expression, non-whitespace characters outside ASCII 0x21-0x7E must be Punycode-encoded, as described in RFC 3492 (https://tools.ietf.org/html/rfc3492), if part of the hostname and percent-encoded, as described in RFC 3986, section 2.1 (http://tools.ietf.org/html/rfc3986#section-2.1), if part of the path."));
            continue;
        }
        if disposition == Disposition::Report && !dir.supported_in_report_only() {
            errors.push(format!("The Content Security Policy directive '{name}' is ignored when delivered in a report-only policy."));
            continue;
        }
        if delivery == Delivery::Meta && !dir.supported_in_meta() {
            errors.push(format!("The Content Security Policy directive '{name}' is ignored when delivered via a <meta> element."));
            continue;
        }
        if dir.is_source_list() {
            let list = parse_source_list(dir, value, errors);
            p.directives.insert(dir, list);
            continue;
        }
        match dir {
            Dir::Sandbox => {
                p.sandbox = Some(Sandbox {
                    allow: value
                        .split(is_ws)
                        .filter(|t| !t.is_empty())
                        .map(str::to_ascii_lowercase)
                        .collect(),
                });
            }
            Dir::UpgradeInsecureRequests => {
                p.upgrade_insecure_requests = true;
                warn_not_empty(name, value, errors);
            }
            Dir::BlockAllMixedContent => warn_not_empty(name, value, errors),
            Dir::ReportTo => {
                p.use_reporting_api = true;
                p.report_endpoints.clear();
                let vals: Vec<&str> = value.split(is_ws).filter(|t| !t.is_empty()).collect();
                if vals.len() > 1 {
                    errors.push("The Content Security Policy directive 'report-to' contains more than one endpoint. Only the first one will be used, the other ones will be ignored.".into());
                }
                if let Some(v) = vals.first() {
                    p.report_endpoints.push((*v).to_owned());
                }
            }
            Dir::ReportUri if !p.use_reporting_api => {
                for uri in value.split(is_ws).filter(|t| !t.is_empty()) {
                    match base.join(uri) {
                        Ok(u) => {
                            if base.scheme() == "https" && !potentially_trustworthy(&u) {
                                errors.push(format!("The Content Security Policy directive 'report-uri' specifies as endpoint '{uri}'. This endpoint will be ignored since it violates the policy for Mixed Content."));
                                continue;
                            }
                            p.report_endpoints.push(u.to_string());
                        }
                        Err(_) => errors.push(format!("The Content Security Policy directive 'report-uri' specifies an invalid endpoint '{uri}'. It will be ignored.")),
                    }
                }
            }
            _ => {}
        }
    }
    p
}

fn warn_not_empty(name: &str, value: &str, errors: &mut Vec<String>) {
    if !value.is_empty() {
        errors.push(format!("The Content Security Policy directive '{name}' should be empty, but was delivered with a value of '{value}'. The directive has been applied, and the value ignored."));
    }
}

fn unrecognized(name: &str) -> String {
    let l = name.to_ascii_lowercase();
    match l.as_str() {
        "allow" => "The 'allow' directive has been replaced with 'default-src'. Please use that directive instead, as 'allow' has no effect.".into(),
        "options" => "The 'options' directive has been replaced with the 'unsafe-inline' and 'unsafe-eval' source expressions for the 'script-src' and 'style-src' directives. Please use those directives instead, as 'options' has no effect.".into(),
        "policy-uri" => "The 'policy-uri' directive has been removed from the specification. Please specify a complete policy via the Content-Security-Policy header.".into(),
        "plugin-types" => "The Content-Security-Policy directive 'plugin-types' has been removed from the specification. If you want to block plugins, consider specifying \"object-src 'none'\" instead.".into(),
        _ => format!("Unrecognized Content-Security-Policy directive '{name}'."),
    }
}

/// `ParseSourceList`.
fn parse_source_list(dir: Dir, value: &str, errors: &mut Vec<String>) -> SourceList {
    let value = value.trim_matches(is_ws);
    let mut l = SourceList::default();
    if value.eq_ignore_ascii_case("'none'") {
        return l;
    }
    let tokens: Vec<&str> = value.split(is_ws).filter(|t| !t.is_empty()).collect();
    let dname = dir.name();
    let mut contains_none = false;
    for &e in &tokens {
        let eq = |k: &str| e.eq_ignore_ascii_case(k);
        if eq("'none'") {
            contains_none = true;
            continue;
        }
        if eq("'self'") {
            l.allow_self = true;
            continue;
        }
        if eq("*") {
            l.allow_star = true;
            continue;
        }
        if Dir::parse(e).is_some() {
            errors.push(format!("The Content-Security-Policy directive '{dname}' contains '{e}' as a source expression. Did you want to add it as a directive and forget a semicolon?"));
        }
        if let Some(s) = parse_source(dir, e, errors) {
            l.sources.push(s);
            continue;
        }
        if dir == Dir::FrameAncestors {
            errors.push(format!("The Content-Security-Policy directive 'frame-ancestors' does not support the source expression '{e}'"));
            continue;
        }
        if eq("'unsafe-inline'") {
            l.allow_inline = true;
        } else if eq("'inline-speculation-rules'") {
            if matches!(dir, Dir::ScriptSrc | Dir::ScriptSrcElem) {
                l.allow_inline_speculation_rules = true;
            } else {
                errors.push(format!("The Content-Security-Policy directive '{dname}' contains '{e}' as a source expression that is permitted only for 'script-src' and 'script-src-elem' directives. It will be ignored."));
            }
        } else if eq("'unsafe-eval'") {
            l.allow_eval = true;
        } else if eq("'wasm-eval'") {
            l.allow_wasm_eval = true;
        } else if eq("'wasm-unsafe-eval'") {
            l.allow_wasm_unsafe_eval = true;
        } else if eq("'strict-dynamic'") {
            l.allow_dynamic = true;
        } else if eq("'unsafe-hashes'") {
            l.allow_unsafe_hashes = true;
        } else if eq("'report-sha256'") || eq("'report-sha384'") || eq("'report-sha512'") {
        } else if eq("'report-sample'") {
            l.report_sample = true;
        } else if eq("'trusted-types-eval'") {
            l.allow_trusted_types_eval = true;
        } else if let Some(n) = parse_nonce(e) {
            l.nonces.push(n);
        } else if let Some(h) = parse_hash(e) {
            l.hashes.push(h);
        } else if prefixed_hash("'url-", e).is_some() || prefixed_hash("'eval-", e).is_some() {
            // URL and eval hashes are off in Chrome 153 (no message).
        } else {
            errors.push(format!("The source list for the Content Security Policy directive '{dname}' contains an invalid source: '{e}'. It will be ignored."));
        }
    }
    if contains_none
        && tokens.iter().any(|t| {
            !t.eq_ignore_ascii_case("'report-sample'") && !t.eq_ignore_ascii_case("'none'")
        })
    {
        errors.push(format!("The Content-Security-Policy directive '{dname}' contains the keyword 'none' alongside with other source expressions. The keyword 'none' must be the only source expression in the directive value, otherwise it is ignored."));
    }
    l
}

/// `ParseSource`: scheme-source or host-source.
fn parse_source(dir: Dir, expr: &str, errors: &mut Vec<String>) -> Option<Source> {
    let mut s = Source::default();
    let mut e = expr;
    let mut pos = e.find([':', '/']);
    if let Some(p) = pos
        && e.as_bytes()[p] == b':'
    {
        if p + 1 == e.len() {
            s.scheme = parse_scheme(&e[..p])?;
            return Some(s);
        }
        if e.as_bytes()[p + 1] == b'/' {
            if p + 2 >= e.len() || e.as_bytes()[p + 2] != b'/' {
                return None;
            }
            s.scheme = parse_scheme(&e[..p])?;
            e = &e[p + 3..];
            pos = e.find([':', '/']);
        }
    }
    let host_end = pos.unwrap_or(e.len());
    parse_host(&e[..host_end], &mut s)?;
    let Some(pos) = pos else { return Some(s) };
    e = &e[pos..];
    if e.starts_with(':') {
        let port_end = e.find('/');
        let port = &e[1..port_end.unwrap_or(e.len())];
        if port.is_empty() {
            return None;
        }
        if port == "*" {
            s.port_wildcard = true;
        } else if port.bytes().all(|b| b.is_ascii_digit()) {
            s.port = port.parse().ok()?;
        } else {
            return None;
        }
        let Some(pe) = port_end else { return Some(s) };
        e = &e[pe..];
    }
    if e.is_empty() {
        return Some(s);
    }
    let q = e.find(['#', '?']);
    let path = &e[..q.unwrap_or(e.len())];
    if !path.starts_with('/') {
        return None;
    }
    s.path = percent_encoding::percent_decode_str(path)
        .decode_utf8_lossy()
        .into_owned();
    if let Some(q) = q {
        let ignoring = if e.as_bytes()[q] == b'?' {
            "The query component, including the '?', will be ignored."
        } else {
            "The fragment identifier, including the '#', will be ignored."
        };
        errors.push(format!("The source list for Content Security Policy directive '{}' contains a source with an invalid path: '{e}'. {ignoring}", dir.name()));
    }
    Some(s)
}

fn parse_scheme(s: &str) -> Option<String> {
    let mut c = s.chars();
    if !c.next()?.is_ascii_alphabetic() {
        return None;
    }
    if !c.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    Some(s.to_ascii_lowercase())
}

fn parse_host(host: &str, s: &mut Source) -> Option<()> {
    let mut host = host;
    if host.is_empty() {
        return None;
    }
    if let Some(rest) = host.strip_prefix('*') {
        if rest.is_empty() {
            s.host_wildcard = true;
            return Some(());
        }
        host = rest.strip_prefix('.')?;
        s.host_wildcard = true;
    }
    if host.is_empty() {
        return None;
    }
    let pieces: Vec<&str> = host.split('.').collect();
    for (i, p) in pieces.iter().enumerate() {
        if (p.is_empty() && i + 1 < pieces.len())
            || !p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return None;
        }
    }
    s.host = host.to_ascii_lowercase();
    Some(())
}

fn is_base64(s: &str) -> bool {
    let body = s.trim_end_matches('=');
    let pad = s.len() - body.len();
    !body.is_empty()
        && pad <= 2
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'_' | b'/'))
}

fn parse_nonce(e: &str) -> Option<String> {
    if e.len() < 8 || !e[..7].eq_ignore_ascii_case("'nonce-") {
        return None;
    }
    let sub = &e[7..e.len() - 1];
    (is_base64(sub) && e.ends_with('\'')).then(|| sub.to_owned())
}

fn parse_unquoted_hash(e: &str) -> Option<Hash> {
    const PREFIXES: [(&str, HashAlg); 7] = [
        ("sha256-", HashAlg::Sha256),
        ("sha384-", HashAlg::Sha384),
        ("sha512-", HashAlg::Sha512),
        ("sha-256-", HashAlg::Sha256),
        ("sha-384-", HashAlg::Sha384),
        ("sha-512-", HashAlg::Sha512),
        ("ed25519-", HashAlg::Ed25519),
    ];
    for (p, alg) in PREFIXES {
        if e.len() >= p.len() && e[..p.len()].eq_ignore_ascii_case(p) {
            let sub = &e[p.len()..];
            if !is_base64(sub) {
                return None;
            }
            let value = base64url_decode(sub.trim_end_matches('='))?;
            return Some(Hash { alg, value });
        }
    }
    None
}

fn parse_hash(e: &str) -> Option<Hash> {
    if e.len() < 2 || !e.starts_with('\'') || !e.ends_with('\'') {
        return None;
    }
    parse_unquoted_hash(&e[1..e.len() - 1])
}

fn prefixed_hash(prefix: &str, e: &str) -> Option<Hash> {
    if e.len() <= prefix.len() + 1
        || !e[..prefix.len()].eq_ignore_ascii_case(prefix)
        || !e.ends_with('\'')
    {
        return None;
    }
    parse_unquoted_hash(&e[prefix.len()..e.len() - 1])
}

/// Base64 or base64url without padding (Chrome accepts both).
fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Standard base64 with padding.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    s
}

fn digest(alg: HashAlg, data: &[u8]) -> Option<Vec<u8>> {
    use boring::hash::{MessageDigest, hash};
    let md = match alg {
        HashAlg::Sha256 => MessageDigest::sha256(),
        HashAlg::Sha384 => MessageDigest::sha384(),
        HashAlg::Sha512 => MessageDigest::sha512(),
        HashAlg::Ed25519 => return None,
    };
    hash(md, data).ok().map(|d| d.to_vec())
}

/// `'sha256-...'` of `content` (Blink's `GetSha256String`).
pub fn sha256_source(content: &str) -> String {
    match digest(HashAlg::Sha256, content.as_bytes()) {
        Some(d) => format!("sha256-{}", base64(&d)),
        None => "sha256-...".into(),
    }
}

// ---------------------------------------------------------------------------
// Matching (Blink's csp_source.cc and source_list_directive.cc)
// ---------------------------------------------------------------------------

fn default_port(scheme: &str) -> Option<i32> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

fn is_default_port(port: i32, scheme: &str) -> bool {
    default_port(scheme) == Some(port)
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum SchemeMatch {
    No,
    Upgrade,
    Exact,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum PortMatch {
    No,
    Wildcard,
    Upgrade,
    Exact,
}

fn scheme_matches(s: &Source, protocol: &str, self_protocol: &str) -> SchemeMatch {
    let scheme = if s.scheme.is_empty() {
        self_protocol
    } else {
        &s.scheme
    };
    if scheme == protocol {
        SchemeMatch::Exact
    } else if (scheme == "http" && protocol == "https") || (scheme == "ws" && protocol == "wss") {
        SchemeMatch::Upgrade
    } else {
        SchemeMatch::No
    }
}

/// A URL's host as KURL has it: empty for non-standard schemes.
fn url_host(url: &Url) -> &str {
    match url.scheme() {
        "http" | "https" | "ws" | "wss" | "ftp" | "file" => url.host_str().unwrap_or(""),
        _ => "",
    }
}

fn host_matches(s: &Source, host: &str) -> bool {
    if s.host_wildcard {
        if s.host.is_empty() {
            return true;
        }
        return host.len() > s.host.len() + 1
            && host.ends_with(&s.host)
            && host.as_bytes()[host.len() - s.host.len() - 1] == b'.';
    }
    s.host == host
}

fn path_matches(s: &Source, url_path: &str) -> bool {
    if s.path.is_empty() || (s.path == "/" && url_path.is_empty()) {
        return true;
    }
    let path = percent_encoding::percent_decode_str(url_path).decode_utf8_lossy();
    if s.path.ends_with('/') {
        return path.starts_with(&s.path);
    }
    path == s.path
}

fn port_matches(s: &Source, self_protocol: &str, port: i32, protocol: &str) -> PortMatch {
    if s.port_wildcard {
        return PortMatch::Wildcard;
    }
    if port == s.port {
        return if port == PORT_UNSPECIFIED {
            PortMatch::Wildcard
        } else {
            PortMatch::Exact
        };
    }
    let is_scheme_http = if s.scheme.is_empty() {
        self_protocol == "http"
    } else {
        s.scheme == "http"
    };
    if (s.port == 80 || ((s.port == PORT_UNSPECIFIED || s.port == 443) && is_scheme_http))
        && (port == 443 || (port == PORT_UNSPECIFIED && default_port(protocol) == Some(443)))
    {
        return PortMatch::Upgrade;
    }
    if port == PORT_UNSPECIFIED {
        return if is_default_port(s.port, protocol) {
            PortMatch::Exact
        } else {
            PortMatch::No
        };
    }
    if s.port == PORT_UNSPECIFIED {
        return if is_default_port(port, protocol) {
            PortMatch::Exact
        } else {
            PortMatch::No
        };
    }
    PortMatch::No
}

fn url_port(url: &Url) -> i32 {
    url.port().map_or(PORT_UNSPECIFIED, i32::from)
}

/// `CSPSourceMatches`.
fn source_matches(s: &Source, self_protocol: &str, url: &Url, redirected: bool) -> bool {
    let protocol = url.scheme();
    let sm = scheme_matches(s, protocol, self_protocol);
    if sm == SchemeMatch::No {
        return false;
    }
    if s.scheme_only() {
        return true;
    }
    let paths = redirected || path_matches(s, url.path());
    let pm = port_matches(s, self_protocol, url_port(url), protocol);
    let can_upgrade_port = matches!(pm, PortMatch::Upgrade | PortMatch::Wildcard);
    if (sm == SchemeMatch::Upgrade || pm == PortMatch::Upgrade)
        && (sm != SchemeMatch::Upgrade || !can_upgrade_port)
    {
        return false;
    }
    host_matches(s, url_host(url)) && pm != PortMatch::No && paths
}

/// `CSPSourceMatchesAsSelf`.
fn matches_as_self(s: &Source, url: &Url) -> bool {
    let protocol = url.scheme();
    let sm = scheme_matches(s, protocol, &s.scheme);
    if protocol == "file" && sm == SchemeMatch::Exact {
        return true;
    }
    let hosts = host_matches(s, url_host(url));
    let pm = port_matches(s, &s.scheme, url_port(url), protocol);
    if sm == SchemeMatch::Exact && hosts && matches!(pm, PortMatch::Exact | PortMatch::Wildcard) {
        return true;
    }
    let ports_ok = pm == PortMatch::Exact
        || ((is_default_port(s.port, &s.scheme) || s.port == PORT_UNSPECIFIED)
            && url
                .port()
                .is_none_or(|p| is_default_port(i32::from(p), protocol)));
    hosts && ports_ok && (protocol == "https" || protocol == "wss" || s.scheme == "http")
}

/// `CSPSourceListAllows`.
fn list_allows(l: &SourceList, self_origin: &SelfOrigin, url: &Url, redirected: bool) -> bool {
    let protocol = url.scheme();
    if l.allow_star
        && (matches!(protocol, "http" | "https")
            || (!protocol.is_empty() && protocol.eq_ignore_ascii_case(&self_origin.scheme)))
    {
        return true;
    }
    if l.allow_self && matches_as_self(&self_origin.as_source(), url) {
        return true;
    }
    if l.sources
        .iter()
        .any(|s| source_matches(s, &self_origin.scheme, url, redirected))
    {
        return true;
    }
    // '*' matches ws: and wss: too (`AllowedOnlyIfWildcardMatchesWs`).
    l.allow_star && matches!(protocol, "ws" | "wss")
}

/// Whether `url` is potentially trustworthy (loopback, https, ...).
pub fn potentially_trustworthy(url: &Url) -> bool {
    match url.scheme() {
        "https" | "wss" | "file" | "data" | "about" | "blob" => true,
        _ => match url.host() {
            Some(url::Host::Domain(d)) => {
                let d = d.trim_end_matches('.');
                d == "localhost" || d.ends_with(".localhost")
            }
            Some(url::Host::Ipv4(a)) => a.is_loopback(),
            Some(url::Host::Ipv6(a)) => a.is_loopback(),
            None => false,
        },
    }
}

// ---------------------------------------------------------------------------
// Checks
// ---------------------------------------------------------------------------

/// What a violation blocked (`blockedURI` before stripping).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocked {
    Url(Url),
    Inline,
    Eval,
    WasmEval,
}

/// One policy's violation.
#[derive(Clone, Debug)]
pub struct Violation {
    /// The effective directive (`violatedDirective`, `effectiveDirective`).
    pub directive: Dir,
    pub blocked: Blocked,
    /// The console message, with the disposition sentence (`None`: not
    /// logged, as for an `eval` that throws).
    pub console: Option<String>,
    /// A second console message (the `default-src` note of a blocked
    /// `eval` that throws).
    pub note: Option<String>,
    pub disposition: Disposition,
    pub header: String,
    pub sample: String,
    pub endpoints: Vec<String>,
    pub use_reporting_api: bool,
    /// Inline checks dispatch the event at the element.
    pub at_element: bool,
}

/// The result of a check over every policy.
#[derive(Debug, Default)]
pub struct Outcome {
    pub allowed: bool,
    pub violations: Vec<Violation>,
}

impl Outcome {
    fn allowed() -> Outcome {
        Outcome {
            allowed: true,
            violations: Vec::new(),
        }
    }
}

/// Extra facts of a URL check.
#[derive(Clone, Debug, Default)]
pub struct UrlCheck<'a> {
    /// The element's nonce (`script-src-elem`, `style-src-elem`).
    pub nonce: &'a str,
    /// A parser-inserted script (`'strict-dynamic'` does not apply).
    pub parser_inserted: bool,
    /// The request followed a redirect (paths are not matched); the URL
    /// before redirects is what the violation reports.
    pub original: Option<&'a Url>,
    /// `integrity` hashes of a script element.
    pub integrity: &'a [Hash],
}

/// Inline content kinds (`ContentSecurityPolicy::InlineType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inline {
    Script,
    ScriptAttribute,
    Navigation,
    Style,
    StyleAttribute,
}

impl Inline {
    fn directive(self) -> Dir {
        match self {
            Inline::Script | Inline::Navigation => Dir::ScriptSrcElem,
            Inline::ScriptAttribute => Dir::ScriptSrcAttr,
            Inline::Style => Dir::StyleSrcElem,
            Inline::StyleAttribute => Dir::StyleSrcAttr,
        }
    }

    fn is_script(self) -> bool {
        matches!(
            self,
            Inline::Script | Inline::ScriptAttribute | Inline::Navigation
        )
    }

    fn hashes_need_unsafe_hashes(self) -> bool {
        matches!(
            self,
            Inline::Navigation | Inline::ScriptAttribute | Inline::StyleAttribute
        )
    }
}

fn disposition_sentence(report_only: bool) -> &'static str {
    if report_only {
        " The policy is report-only, so the violation has been logged but no further action has been taken."
    } else {
        " The action has been blocked."
    }
}

fn sample_of(content: &str) -> String {
    content
        .trim_matches(is_ws)
        .chars()
        .take(MAX_SAMPLE)
        .collect()
}

/// `KURL::ElidedString`.
fn elided(url: &Url) -> String {
    let s = url.as_str();
    if s.len() <= 1024 {
        return s.to_owned();
    }
    let head: String = s.chars().take(511).collect();
    let tail: String = s
        .chars()
        .rev()
        .take(510)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}...{tail}")
}

/// The policies of a document or worker (its CSP list).
#[derive(Clone, Debug, Default)]
pub struct CspList {
    pub policies: Vec<Policy>,
}

impl CspList {
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

    /// Some policy has an operative directive for `dir`.
    pub fn restricts(&self, dir: Dir) -> bool {
        self.policies.iter().any(|p| p.operative(dir).1.is_some())
    }

    /// Some operative list for `dir` holds nonces, hashes or
    /// `'strict-dynamic'` (its checks depend on the element).
    pub fn element_dependent(&self, dir: Dir) -> bool {
        self.policies.iter().any(|p| {
            p.operative(dir)
                .1
                .is_some_and(|l| !l.nonces.is_empty() || !l.hashes.is_empty() || l.allow_dynamic)
        })
    }

    /// Some policy came in a header (nonce hiding applies).
    pub fn header_delivered(&self) -> bool {
        self.policies.iter().any(|p| p.delivery == Delivery::Header)
    }

    /// `upgrade-insecure-requests` is in some policy.
    pub fn upgrade_insecure(&self) -> bool {
        self.policies.iter().any(|p| p.upgrade_insecure_requests)
    }

    /// The `sandbox` tokens of the enforced policies, combined (`None`:
    /// no sandbox).
    pub fn sandbox(&self) -> Option<Sandbox> {
        let mut out: Option<Sandbox> = None;
        for p in &self.policies {
            if p.disposition != Disposition::Enforce {
                continue;
            }
            if let Some(s) = &p.sandbox {
                out = Some(match out {
                    None => s.clone(),
                    Some(o) => Sandbox {
                        allow: o
                            .allow
                            .into_iter()
                            .filter(|t| s.allow.contains(t))
                            .collect(),
                    },
                });
            }
        }
        out
    }

    /// The algorithms hash sources of script and style directives use.
    fn hash_algs(&self) -> Vec<HashAlg> {
        let mut v = Vec::new();
        for p in &self.policies {
            for (d, l) in &p.directives {
                if d.is_script() || d.is_style() {
                    for h in &l.hashes {
                        if !v.contains(&h.alg) {
                            v.push(h.alg);
                        }
                    }
                }
            }
        }
        v
    }

    /// Is a fetch of `url` for `dir` (an effective directive such as
    /// `script-src-elem`, `img-src`, `connect-src`) allowed?
    /// (`ContentSecurityPolicy::AllowFromSource`.)
    pub fn check_url(&self, dir: Dir, url: &Url, c: &UrlCheck<'_>) -> Outcome {
        let mut out = Outcome::allowed();
        for p in &self.policies {
            if let Some(v) = check_url_policy(p, dir, url, c) {
                if p.disposition == Disposition::Enforce {
                    out.allowed = false;
                }
                out.violations.push(v);
            }
        }
        out
    }

    /// Inline script, handler, `javascript:` URL or style
    /// (`ContentSecurityPolicy::AllowInline`). `content` is hashed when a
    /// policy lists hashes.
    pub fn check_inline(
        &self,
        kind: Inline,
        content: &str,
        nonce: &str,
        parser_inserted: bool,
    ) -> Outcome {
        let mut out = Outcome::allowed();
        if self.policies.is_empty() {
            return out;
        }
        let digests: Vec<Hash> = self
            .hash_algs()
            .into_iter()
            .filter_map(|alg| digest(alg, content.as_bytes()).map(|value| Hash { alg, value }))
            .collect();
        let mut sha: Option<String> = None;
        for p in &self.policies {
            let dir = kind.directive();
            let (op_dir, op) = p.operative(dir);
            // Allowed by a hash (with 'unsafe-hashes' for attributes)?
            let by_hash = match op {
                None => true,
                Some(l) => {
                    (!kind.hashes_need_unsafe_hashes() || l.allow_unsafe_hashes)
                        && digests.iter().any(|d| l.hashes.contains(d))
                }
            };
            if by_hash {
                continue;
            }
            let Some(l) = op else { continue };
            if l.allow_nonce(nonce) {
                continue;
            }
            if matches!(kind, Inline::Script)
                && !parser_inserted
                && p.operative(dir).1.is_some_and(|l| l.allow_dynamic)
                && dir.dynamic_applies()
            {
                continue;
            }
            if l.allow_all_inline(op_dir) {
                continue;
            }
            let what = match kind {
                Inline::Navigation => "Running the JavaScript URL",
                Inline::ScriptAttribute => "Executing inline event handler",
                Inline::Script => "Executing inline script",
                Inline::Style | Inline::StyleAttribute => "Applying inline style",
            };
            let hash_value = match kind {
                Inline::Navigation | Inline::ScriptAttribute => "sha256-...".to_string(),
                _ => sha.get_or_insert_with(|| sha256_source(content)).clone(),
            };
            let suffix = if l.allow_inline && l.hash_or_nonce() {
                " Note that 'unsafe-inline' is ignored if either a hash or nonce value is present in the source list.".to_string()
            } else {
                let mut s = format!(
                    " Either the 'unsafe-inline' keyword, a hash ('{hash_value}'), or a nonce ('nonce-...') is required to enable inline execution."
                );
                if kind.hashes_need_unsafe_hashes() && !l.allow_unsafe_hashes {
                    s.push_str(" Note that hashes do not apply to event handlers, style attributes and javascript: navigations unless the 'unsafe-hashes' keyword is present.");
                }
                if op_dir == Dir::DefaultSrc {
                    s.push_str(&format!(
                        " Note also that '{}-src' was not explicitly set, so 'default-src' is used as a fallback.",
                        if kind.is_script() { "script" } else { "style" }
                    ));
                }
                s
            };
            let ro = p.report_only();
            if !ro {
                out.allowed = false;
            }
            out.violations.push(Violation {
                directive: dir,
                blocked: Blocked::Inline,
                console: Some(format!(
                    "{what} violates the following Content Security Policy directive '{}'.{suffix}{}",
                    p.raw_text(op_dir),
                    disposition_sentence(ro)
                )),
                note: None,
                disposition: p.disposition,
                header: p.header.clone(),
                sample: if l.report_sample {
                    sample_of(content)
                } else {
                    String::new()
                },
                endpoints: p.report_endpoints.clone(),
                use_reporting_api: p.use_reporting_api,
                at_element: matches!(
                    kind,
                    Inline::Script | Inline::ScriptAttribute | Inline::Style | Inline::StyleAttribute
                ),
            });
        }
        out
    }

    /// Some policy restricts `eval` (also report-only ones: the engine
    /// then asks [`CspList::check_eval`] for each string).
    pub fn eval_restricted(&self) -> bool {
        self.eval_disabled_message().is_some()
    }

    /// The `EvalError` message (`EvalDisabledErrorMessage`).
    pub fn eval_disabled_message(&self) -> Option<String> {
        self.policies.iter().find_map(|p| {
            let (d, l) = p.operative(Dir::ScriptSrc);
            let l = l?;
            (!l.allow_eval).then(|| {
                format!("Evaluating a string as JavaScript violates the following Content Security Policy directive because 'unsafe-eval' is not an allowed source of script: {}\".\n", p.raw_text(d))
            })
        })
    }

    /// `eval`, `Function`, string timers (`AllowEval`). `will_throw`:
    /// the caller throws an `EvalError` (no console message then, except
    /// for report-only policies).
    pub fn check_eval(&self, content: &str, will_throw: bool) -> Outcome {
        let mut out = Outcome::allowed();
        for p in &self.policies {
            let (d, l) = p.operative(Dir::ScriptSrc);
            let Some(l) = l else { continue };
            if l.allow_eval {
                continue;
            }
            let ro = p.report_only();
            let suffix = if d == Dir::DefaultSrc {
                " Note that 'script-src' was not explicitly set, so 'default-src' is used as a fallback."
            } else {
                ""
            };
            let text = format!(
                "Evaluating a string as JavaScript violates the following Content Security Policy directive because 'unsafe-eval' is not an allowed source of script: \"{}\".{suffix}\n{}",
                p.raw_text(d),
                disposition_sentence(ro)
            );
            if !ro {
                out.allowed = false;
            }
            out.violations.push(Violation {
                directive: Dir::ScriptSrc,
                blocked: Blocked::Eval,
                console: (ro || !will_throw).then_some(text),
                note: (!suffix.is_empty() && !ro).then(|| suffix.to_owned()),
                disposition: p.disposition,
                header: p.header.clone(),
                sample: if l.report_sample {
                    content.chars().take(MAX_SAMPLE).collect::<String>()
                } else {
                    String::new()
                },
                endpoints: p.report_endpoints.clone(),
                use_reporting_api: p.use_reporting_api,
                at_element: false,
            });
        }
        // Blink trims the sample when it builds the event.
        for v in &mut out.violations {
            v.sample = sample_of(&v.sample);
        }
        out
    }

    fn wasm_allowed(p: &Policy) -> bool {
        match p.operative(Dir::ScriptSrc).1 {
            None => true,
            Some(l) => l.allow_eval || l.allow_wasm_unsafe_eval,
        }
    }

    /// The WebAssembly compile error message.
    pub fn wasm_disabled_message(&self) -> Option<String> {
        self.policies.iter().find_map(|p| {
            if Self::wasm_allowed(p) {
                return None;
            }
            let d = p.operative(Dir::ScriptSrc).0;
            Some(format!("Compiling or instantiating WebAssembly module violates the following Content Security policy directive because 'unsafe-eval' is not an allowed source of script in the following Content Security Policy directive: \"{}\".", p.raw_text(d)))
        })
    }

    /// WebAssembly compilation (`AllowWasmCodeGeneration`; it throws, so
    /// only report-only policies log).
    pub fn check_wasm(&self) -> Outcome {
        let mut out = Outcome::allowed();
        for p in &self.policies {
            if Self::wasm_allowed(p) {
                continue;
            }
            let d = p.operative(Dir::ScriptSrc).0;
            let ro = p.report_only();
            let suffix = if d == Dir::DefaultSrc {
                " Note that 'script-src' was not explicitly set, so 'default-src' is used as a fallback."
            } else {
                ""
            };
            if !ro {
                out.allowed = false;
            }
            out.violations.push(Violation {
                directive: Dir::ScriptSrc,
                blocked: Blocked::WasmEval,
                console: ro.then(|| format!("Compiling or instantiating a WebAssembly module violates the following Content Security policy directive because 'unsafe-eval' is not an allowed source of script: \"{}\".{suffix}{}", p.raw_text(d), disposition_sentence(true))),
                note: None,
                disposition: p.disposition,
                header: p.header.clone(),
                sample: String::new(),
                endpoints: p.report_endpoints.clone(),
                use_reporting_api: p.use_reporting_api,
                at_element: false,
            });
        }
        out
    }

    /// `frame-ancestors` of a framed document's policies against its
    /// ancestors (parent first), with the network service's message
    /// (Chrome names the framed document's origin).
    pub fn check_ancestors(&self, ancestors: &[Url]) -> Outcome {
        let mut out = Outcome::allowed();
        for p in &self.policies {
            let Some(l) = p.directives.get(&Dir::FrameAncestors) else {
                continue;
            };
            if ancestors
                .iter()
                .all(|a| list_allows(l, &p.self_origin, a, false))
            {
                continue;
            }
            let ro = p.report_only();
            let me = p.self_origin.as_source();
            let shown = format!(
                "{}://{}{}/",
                me.scheme,
                me.host,
                match (me.port, default_port(&me.scheme)) {
                    (port, Some(d)) if port == d => String::new(),
                    (PORT_UNSPECIFIED, _) => String::new(),
                    (port, _) => format!(":{port}"),
                }
            );
            let text = format!(
                "Framing '{shown}' violates the following {}Content Security Policy directive: \"frame-ancestors {}\". {}\n",
                if ro { "report-only " } else { "" },
                l.to_text(),
                if ro {
                    "The violation has been logged, but no further action has been taken."
                } else {
                    "The request has been blocked."
                }
            );
            if !ro {
                out.allowed = false;
            }
            out.violations.push(Violation {
                directive: Dir::FrameAncestors,
                blocked: Blocked::Url(Url::parse(&shown).unwrap_or_else(|_| ancestors[0].clone())),
                console: Some(text),
                note: None,
                disposition: p.disposition,
                header: p.header.clone(),
                sample: String::new(),
                endpoints: p.report_endpoints.clone(),
                use_reporting_api: p.use_reporting_api,
                at_element: false,
            });
        }
        out
    }

    /// A navigation checked by the browser process: `frame-src` (an
    /// iframe's navigation) or `form-action`, with the network service's
    /// messages. `shown` is the URL the message names (Chrome shows only
    /// the origin of a cross-origin frame).
    pub fn check_navigation(&self, dir: Dir, url: &Url, shown: &str, redirected: bool) -> Outcome {
        let mut out = Outcome::allowed();
        for p in &self.policies {
            let (d, l) = p.operative(dir);
            let Some(l) = l else { continue };
            if list_allows(l, &p.self_origin, url, redirected) {
                continue;
            }
            let ro = p.report_only();
            let action = match dir {
                Dir::FormAction => "Sending form data to '",
                Dir::FencedFrameSrc => "Framing '",
                _ => "Framing '",
            };
            let mut text = format!(
                "{action}{shown}' violates the following {}Content Security Policy directive: \"{} {}\". {}",
                if ro { "report-only " } else { "" },
                d.name(),
                l.to_text(),
                if ro {
                    "The violation has been logged, but no further action has been taken."
                } else {
                    "The request has been blocked."
                }
            );
            if d != dir {
                text.push_str(&format!(
                    " Note that '{}' was not explicitly set, so '{}' is used as a fallback.",
                    dir.name(),
                    d.name()
                ));
            }
            if l.allow_star {
                text.push_str(&format!(" Note that '*' matches only URLs with network schemes ('http', 'https', 'ws', 'wss'), or URLs whose scheme matches `self`'s scheme. The scheme '{}:' must be added explicitly.", url.scheme()));
            }
            text.push('\n');
            if !ro {
                out.allowed = false;
            }
            out.violations.push(Violation {
                directive: dir,
                blocked: Blocked::Url(url.clone()),
                console: Some(text),
                note: None,
                disposition: p.disposition,
                header: p.header.clone(),
                sample: String::new(),
                endpoints: p.report_endpoints.clone(),
                use_reporting_api: p.use_reporting_api,
                at_element: false,
            });
        }
        out
    }
}

/// One policy's `CSPDirectiveListAllowFromSource` with reporting.
fn check_url_policy(p: &Policy, dir: Dir, url: &Url, c: &UrlCheck<'_>) -> Option<Violation> {
    if dir == Dir::ObjectSrc && url.scheme() == "about" {
        return None;
    }
    if dir == Dir::WorkerSrc
        && p.operative(Dir::WorkerSrc)
            .1
            .is_some_and(|l| l.allow_dynamic)
    {
        return None;
    }
    let (op_dir, op) = p.operative(dir);
    let l = op?;
    if matches!(dir, Dir::ScriptSrcElem | Dir::StyleSrcElem) && l.allow_nonce(c.nonce) {
        return None;
    }
    if dir == Dir::ScriptSrcElem {
        if !c.parser_inserted && l.allow_dynamic {
            return None;
        }
        if !c.integrity.is_empty() && c.integrity.iter().all(|h| l.hashes.contains(h)) {
            return None;
        }
    }
    let redirected = c.original.is_some();
    let mut suffix = String::new();
    if list_allows(l, &p.self_origin, url, redirected) {
        if dir.dynamic_applies() && l.allow_dynamic {
            suffix.push_str(
                " Note that 'strict-dynamic' is present, so host-based allowlisting is disabled.",
            );
        } else {
            return None;
        }
    }
    let prefix = match dir {
        Dir::BaseUri => "Setting the document's base URI to '",
        Dir::ConnectSrc => "Connecting to '",
        Dir::DefaultSrc => "Fetching content from '",
        Dir::FontSrc => "Loading the font '",
        Dir::FormAction => "Sending form data to '",
        Dir::ImgSrc => "Loading the image '",
        Dir::ManifestSrc => "Loading a manifest from '",
        Dir::MediaSrc => "Loading media from  '",
        Dir::ObjectSrc => "Loading plugin data from '",
        Dir::ScriptSrc | Dir::ScriptSrcAttr | Dir::ScriptSrcElem => "Loading the script '",
        Dir::StyleSrc | Dir::StyleSrcAttr | Dir::StyleSrcElem => "Loading the stylesheet '",
        Dir::WorkerSrc => "Creating a worker from '",
        _ => "Fetching content from '",
    };
    if op_dir != dir {
        suffix.push_str(&format!(
            " Note that '{}' was not explicitly set, so '{}' is used as a fallback.",
            dir.name(),
            op_dir.name()
        ));
    }
    if l.allow_star {
        suffix.push_str(&format!(" Note that '*' matches only URLs with network schemes ('http', 'https', 'ws', 'wss'), or URLs whose scheme matches `self`'s scheme. The scheme '{}:' must be added explicitly.", url.scheme()));
    }
    let ro = p.report_only();
    Some(Violation {
        directive: dir,
        blocked: Blocked::Url(c.original.cloned().unwrap_or_else(|| url.clone())),
        console: Some(format!(
            "{prefix}{}' violates the following Content Security Policy directive: \"{}\".{suffix}{}",
            elided(url),
            p.raw_text(op_dir),
            disposition_sentence(ro)
        )),
        note: None,
        disposition: p.disposition,
        header: p.header.clone(),
        sample: String::new(),
        endpoints: p.report_endpoints.clone(),
        use_reporting_api: p.use_reporting_api,
        at_element: false,
    })
}

/// Blink's `StripURLForUseInReport`: what a report may say about `url`
/// for a document of `origin`.
pub fn strip_for_report(origin: &url::Origin, url: &Url, dir: Dir) -> String {
    if url.cannot_be_a_base() || url.scheme() == "file" {
        return url.scheme().to_owned();
    }
    if matches!(url.scheme(), "blob" | "filesystem") {
        return url.scheme().to_owned();
    }
    let same = url.origin() == *origin;
    if !same && matches!(dir, Dir::FrameSrc | Dir::ObjectSrc | Dir::FencedFrameSrc) {
        return url.origin().ascii_serialization();
    }
    let mut u = url.clone();
    u.set_fragment(None);
    let _ = u.set_username("");
    let _ = u.set_password(None);
    u.to_string()
}

/// `upgrade-insecure-requests`: `http:` to `https:` (and `ws:` to
/// `wss:`) unless potentially trustworthy.
pub fn upgrade(url: &Url) -> Option<Url> {
    let to = match url.scheme() {
        "http" => "https",
        "ws" => "wss",
        _ => return None,
    };
    if potentially_trustworthy(url) {
        return None;
    }
    let mut u = url.clone();
    let port80 = u.port() == Some(80);
    if u.set_scheme(to).is_err() {
        return None;
    }
    if port80 {
        let _ = u.set_port(Some(443));
    }
    Some(u)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn list(v: &str, base: &str) -> CspList {
        let mut e = Vec::new();
        CspList {
            policies: parse(v, Disposition::Enforce, Delivery::Header, &u(base), &mut e),
        }
    }

    /// The parse errors Chrome 153 printed for the `parse_errors` case.
    #[test]
    fn parse_errors_as_chrome() {
        let mut e = Vec::new();
        parse(
            "script-src 'self' 'none' foo$; img-src 'self'; img-src *; bogus-directive x; upgrade-insecure-requests 1; frame-ancestors 'unsafe-inline'; style-src https://a/b?c",
            Disposition::Enforce,
            Delivery::Header,
            &u("http://127.0.0.1:1/"),
            &mut e,
        );
        assert_eq!(
            e,
            [
                "The source list for the Content Security Policy directive 'script-src' contains an invalid source: 'foo$'. It will be ignored.",
                "The Content-Security-Policy directive 'script-src' contains the keyword 'none' alongside with other source expressions. The keyword 'none' must be the only source expression in the directive value, otherwise it is ignored.",
                "Ignoring duplicate Content-Security-Policy directive 'img-src'.",
                "Unrecognized Content-Security-Policy directive 'bogus-directive'.",
                "The Content Security Policy directive 'upgrade-insecure-requests' should be empty, but was delivered with a value of '1'. The directive has been applied, and the value ignored.",
                "The Content-Security-Policy directive 'frame-ancestors' does not support the source expression ''unsafe-inline''",
                "The source list for Content Security Policy directive 'style-src' contains a source with an invalid path: '/b?c'. The query component, including the '?', will be ignored.",
            ]
        );
    }

    /// The image URLs of the `source_matching` case that Chrome 153
    /// blocked, and those it let through.
    #[test]
    fn source_matching_as_chrome() {
        let base = "http://127.0.0.1:19110/source_matching/";
        let l = list(
            "img-src 127.0.0.1:19110/source_matching/dir/ http://localhost:19110/source_matching/file.png *.localhost:* https://127.0.0.1:19110 data:",
            base,
        );
        let ok = |s: &str| {
            l.check_url(Dir::ImgSrc, &u(s), &UrlCheck::default())
                .allowed
        };
        assert!(ok("http://127.0.0.1:19110/source_matching/dir/a.png"));
        assert!(!ok("http://127.0.0.1:19110/source_matching/dir"));
        assert!(!ok("http://127.0.0.1:19110/source_matching/other.png"));
        assert!(ok("http://localhost:19110/source_matching/file.png"));
        assert!(ok("http://localhost:19110/source_matching/file.png?q=1"));
        assert!(!ok("http://localhost:19110/source_matching/file2.png"));
        assert!(ok("http://a.localhost:1/x.png"));
        assert!(!ok("http://localhost:1/x.png"));
        assert!(ok("https://127.0.0.1:19110/source_matching/tls.png"));
        assert!(!ok("http://127.0.0.1:1/x.png"));
        assert!(ok("data:image/png;base64,AA=="));
        let after = u("http://127.0.0.1:19110/source_matching/after.png");
        let orig = u("http://127.0.0.1:19110/source_matching/dir/redirect");
        let c = UrlCheck {
            original: Some(&orig),
            ..Default::default()
        };
        assert!(l.check_url(Dir::ImgSrc, &after, &c).allowed);
        assert!(
            !l.check_url(Dir::ImgSrc, &u("http://localhost:1/far.png"), &c)
                .allowed
        );
    }

    #[test]
    fn upgrade_insecure_requests_skips_trustworthy_hosts() {
        assert_eq!(upgrade(&u("http://127.0.0.1:8/x")), None);
        assert_eq!(upgrade(&u("http://a.localhost/x")), None);
        assert_eq!(upgrade(&u("https://example.test/x")), None);
        assert_eq!(
            upgrade(&u("http://example.test/x")).map(|u| u.to_string()),
            Some("https://example.test/x".into())
        );
        assert_eq!(
            upgrade(&u("http://example.test:80/x")).map(|u| u.to_string()),
            Some("https://example.test/x".into())
        );
        assert_eq!(
            upgrade(&u("ws://example.test:8080/x")).map(|u| u.to_string()),
            Some("wss://example.test:8080/x".into())
        );
    }

    #[test]
    fn self_matches_websocket_and_hash_source() {
        let l = list("connect-src 'self'", "http://127.0.0.1:19110/x/");
        assert!(
            l.check_url(
                Dir::ConnectSrc,
                &u("ws://127.0.0.1:19110/ws"),
                &UrlCheck::default()
            )
            .allowed
        );
        assert_eq!(
            sha256_source("log('inline ran');"),
            "sha256-GGYnolcguDFeCZYEsAV91x0KU9SelJSWpDyvMdxAx6Y="
        );
    }
}
