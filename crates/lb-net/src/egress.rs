//! Egress policy: which destinations the network stack may open a
//! connection to (ADR 0044).
//!
//! LightBrowser is for internal validation: it is pointed at loopback pages
//! and our own test servers. A tool that wants the stack to *enforce* that
//! turns the policy on; nothing changes for anyone who does not (the default
//! is [`EgressMode::Open`], which checks nothing).
//!
//! Modes:
//! - `open`: no checks.
//! - `loopback`: loopback addresses (`127.0.0.0/8`, `::1`, also as
//!   IPv4-mapped IPv6), the names `localhost` and `*.localhost`, and
//!   anything added with an allow entry.
//! - `allowlist`: only the allow entries (no implicit loopback).
//!
//! Allow entries are a name glob (`*.lab.test`, `build-*.internal`; `*`
//! matches any run of characters), an IP address (`10.0.0.5`, `::1`,
//! `[fd00::1]`) or a CIDR block (`10.0.0.0/8`, `fd00::/8`).
//!
//! Two checks, because a name is not an address:
//! - [`EgressPolicy::check_name`] runs before any DNS lookup. A name that
//!   is not allowed is never resolved, so no resolver traffic leaves for
//!   it. For an IP literal it is the address check.
//! - [`EgressPolicy::check_resolved`] (or [`EgressPolicy::check_addr`])
//!   runs on every address a name resolved to, before the connection. A
//!   name that an allow *entry* names may resolve to any address (the
//!   operator vouched for it); the implicit loopback names (`localhost`,
//!   `*.localhost`) must resolve to loopback, so a hosts file or a DNS
//!   answer cannot turn them into an outside destination.
//!
//! Host strings are normalised the way URLs are before matching: ASCII case,
//! one trailing dot, IDN to punycode, numeric IPv4 forms (`0x7f.1`), IPv6
//! brackets and an optional `:port` (ignored). A string that is not a valid
//! host is refused.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

/// How much of the network a context may reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EgressMode {
    /// No checks (the default).
    #[default]
    Open,
    /// Loopback, `localhost` names and the allow entries.
    Loopback,
    /// Only the allow entries.
    AllowList,
}

impl fmt::Display for EgressMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EgressMode::Open => "open",
            EgressMode::Loopback => "loopback",
            EgressMode::AllowList => "allowlist",
        })
    }
}

impl FromStr for EgressMode {
    type Err = EgressError;
    fn from_str(s: &str) -> Result<Self, EgressError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "open" => Ok(EgressMode::Open),
            "loopback" => Ok(EgressMode::Loopback),
            "allowlist" => Ok(EgressMode::AllowList),
            other => Err(EgressError(format!(
                "unknown egress mode {other:?} (open, loopback or allowlist)"
            ))),
        }
    }
}

/// A policy that could not be built: a bad mode or allow entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressError(pub String);

impl fmt::Display for EgressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EgressError {}

/// What a refusal was about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedKind {
    /// A host name that no rule allows (refused before any lookup).
    Name,
    /// An address (an IP literal, or one a name resolved to).
    Address,
    /// Not a valid host at all.
    Invalid,
}

/// A destination the policy refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    pub target: String,
    pub kind: BlockedKind,
    pub mode: EgressMode,
}

impl fmt::Display for Blocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            BlockedKind::Name => "host name",
            BlockedKind::Address => "address",
            BlockedKind::Invalid => "host",
        };
        write!(
            f,
            "egress policy ({}) does not allow {what} {:?}",
            self.mode, self.target
        )
    }
}

impl std::error::Error for Blocked {}

/// An address block: `addr` has its host bits cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IpNet {
    addr: IpAddr,
    bits: u8,
}

impl IpNet {
    fn new(addr: IpAddr, bits: u8) -> Result<IpNet, EgressError> {
        let addr = match addr {
            // `::ffff:a.b.c.d/N` is the IPv4 block `a.b.c.d/(N - 96)`.
            IpAddr::V6(a) if bits >= 96 && a.to_ipv4_mapped().is_some() => {
                return IpNet::new(
                    IpAddr::V4(a.to_ipv4_mapped().unwrap_or(Ipv4Addr::UNSPECIFIED)),
                    bits - 96,
                );
            }
            a => a,
        };
        let max = if addr.is_ipv4() { 32 } else { 128 };
        if bits > max {
            return Err(EgressError(format!(
                "prefix length /{bits} is too long for {addr}"
            )));
        }
        let masked = match addr {
            IpAddr::V4(a) => IpAddr::V4(Ipv4Addr::from(u32::from(a) & mask4(bits))),
            IpAddr::V6(a) => IpAddr::V6(Ipv6Addr::from(u128::from(a) & mask6(bits))),
        };
        Ok(IpNet { addr: masked, bits })
    }

    fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => u32::from(a) & mask4(self.bits) == u32::from(n),
            (IpAddr::V6(n), IpAddr::V6(a)) => u128::from(a) & mask6(self.bits) == u128::from(n),
            _ => false,
        }
    }
}

fn mask4(bits: u8) -> u32 {
    if bits == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(bits))
    }
}

fn mask6(bits: u8) -> u128 {
    if bits == 0 {
        0
    } else {
        u128::MAX << (128 - u32::from(bits))
    }
}

/// A host after normalisation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Host {
    /// Lowercase ASCII (punycode), no trailing dot.
    Name(String),
    /// IPv4-mapped IPv6 is already unmapped.
    Ip(IpAddr),
}

/// `ip` with an IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) as the IPv4
/// address it stands for.
pub fn unmap(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(a) => a.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// Normalise a host as it appears in a URL, a `CONNECT` target or an
/// authority: optional brackets and `:port`, case, one trailing dot, IDN,
/// numeric IPv4 forms. `None`: not a valid host.
fn normalize(input: &str) -> Option<Host> {
    let s = input.trim();
    let host = if let Some(rest) = s.strip_prefix('[') {
        let (inner, after) = rest.split_once(']')?;
        if !after.is_empty() && !is_port(after.strip_prefix(':')?) {
            return None;
        }
        // Brackets hold an IPv6 literal only.
        let ip = inner.parse::<Ipv6Addr>().ok()?;
        return Some(Host::Ip(unmap(IpAddr::V6(ip))));
    } else if let Ok(ip) = s.parse::<IpAddr>() {
        // A bare IPv6 literal has colons that are not a port.
        return Some(Host::Ip(unmap(ip)));
    } else if let Some((h, p)) = s.rsplit_once(':') {
        if !is_port(p) {
            return None;
        }
        h
    } else {
        s
    };
    if host.is_empty() {
        return None;
    }
    match url::Host::parse(host).ok()? {
        url::Host::Domain(d) => {
            let d = d.to_ascii_lowercase();
            let d = d.strip_suffix('.').unwrap_or(&d);
            if d.is_empty() || d.ends_with('.') || d.starts_with('.') {
                return None;
            }
            Some(Host::Name(d.to_owned()))
        }
        url::Host::Ipv4(a) => Some(Host::Ip(IpAddr::V4(a))),
        url::Host::Ipv6(a) => Some(Host::Ip(unmap(IpAddr::V6(a)))),
    }
}

fn is_port(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 5
        && p.bytes().all(|b| b.is_ascii_digit())
        && p.parse::<u32>().is_ok_and(|n| n <= 65535)
}

/// `*` matches any run of characters (dots included); everything else is
/// literal. Both sides are lowercase ASCII.
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

fn is_localhost(name: &str) -> bool {
    name == "localhost" || name.ends_with(".localhost")
}

/// The egress policy of a network context. Cheap to share (`Arc`).
#[derive(Debug, Clone, Default)]
pub struct EgressPolicy {
    mode: EgressMode,
    /// Allowed name globs, normalised.
    names: Vec<String>,
    /// Allowed addresses (single addresses are /32 and /128 blocks).
    nets: Vec<IpNet>,
    /// The entries as given, for [`describe`](Self::describe).
    given: Vec<String>,
}

impl EgressPolicy {
    /// No checks.
    pub fn open() -> EgressPolicy {
        EgressPolicy::default()
    }

    /// Loopback only.
    pub fn loopback() -> EgressPolicy {
        EgressPolicy {
            mode: EgressMode::Loopback,
            ..Default::default()
        }
    }

    /// A policy of `mode` with `allow` entries (name globs, IPs, CIDR
    /// blocks). Entries are accepted in every mode, but only `loopback` and
    /// `allowlist` act on them; `open` with entries is refused as a
    /// mistake (the entries would do nothing).
    pub fn new<S: AsRef<str>>(mode: EgressMode, allow: &[S]) -> Result<EgressPolicy, EgressError> {
        if mode == EgressMode::Open && !allow.is_empty() {
            return Err(EgressError(
                "allow entries have no effect with egress mode open".into(),
            ));
        }
        let mut p = EgressPolicy {
            mode,
            ..Default::default()
        };
        for e in allow {
            p.add(e.as_ref())?;
        }
        Ok(p)
    }

    /// [`new`](Self::new) with the mode as text (`open`, `loopback`,
    /// `allowlist`), as the command line gives it.
    pub fn parse<S: AsRef<str>>(mode: &str, allow: &[S]) -> Result<EgressPolicy, EgressError> {
        EgressPolicy::new(mode.parse()?, allow)
    }

    /// Add one allow entry.
    pub fn add(&mut self, entry: &str) -> Result<(), EgressError> {
        let entry = entry.trim();
        let bad = |why: &str| EgressError(format!("invalid allow entry {entry:?}: {why}"));
        if entry.is_empty() {
            return Err(bad("empty"));
        }
        let (host, bits) = match entry.rsplit_once('/') {
            Some((h, b)) => (
                h,
                Some(
                    b.parse::<u8>()
                        .map_err(|_| bad("prefix length is not a number"))?,
                ),
            ),
            None => (entry, None),
        };
        // Brackets hold an IPv6 address only.
        let parsed = match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            Some(inner) => inner.parse::<Ipv6Addr>().ok().map(IpAddr::V6),
            None => host.parse::<IpAddr>().ok(),
        };
        if let Some(ip) = parsed {
            let (ip, bits) = match bits {
                // `IpNet::new` turns a mapped IPv6 block into an IPv4 one.
                Some(b) => (ip, b),
                None => {
                    let ip = unmap(ip);
                    (ip, if ip.is_ipv4() { 32 } else { 128 })
                }
            };
            self.nets.push(IpNet::new(ip, bits)?);
        } else if bits.is_some() {
            return Err(bad("a prefix length needs an IP address"));
        } else if host.contains(':') || host.contains('[') {
            return Err(bad(
                "ports are not part of an entry, and IPv6 needs a valid address",
            ));
        } else if host.contains('*') {
            self.names.push(pattern(host).map_err(|w| bad(&w))?);
        } else {
            // A plain name: numeric IPv4 forms (`0x7f.1`) are addresses.
            match normalize(host).ok_or_else(|| bad("not a valid host name"))? {
                Host::Ip(ip) => self
                    .nets
                    .push(IpNet::new(ip, if ip.is_ipv4() { 32 } else { 128 })?),
                Host::Name(n) => self.names.push(pattern(&n).map_err(|w| bad(&w))?),
            }
        }
        self.given.push(entry.to_owned());
        Ok(())
    }

    pub fn mode(&self) -> EgressMode {
        self.mode
    }

    /// The policy checks nothing.
    pub fn is_open(&self) -> bool {
        self.mode == EgressMode::Open
    }

    /// `loopback (allow: a, b)`, for logs and error messages.
    pub fn describe(&self) -> String {
        if self.given.is_empty() {
            self.mode.to_string()
        } else {
            format!("{} (allow: {})", self.mode, self.given.join(", "))
        }
    }

    fn refuse(&self, target: &str, kind: BlockedKind) -> Blocked {
        Blocked {
            target: target.to_owned(),
            kind,
            mode: self.mode,
        }
    }

    /// May the stack look up `host` and connect to what it resolves to?
    /// Run it before any DNS query. `host` is a name, an IP literal (this
    /// is then the address check), `[v6]`, or either with `:port`.
    pub fn check_name(&self, host: &str) -> Result<(), Blocked> {
        if self.is_open() {
            return Ok(());
        }
        match normalize(host) {
            None => Err(self.refuse(host, BlockedKind::Invalid)),
            Some(Host::Ip(ip)) => self.check_addr(ip),
            Some(Host::Name(n)) => {
                if self.name_listed(&n) || (self.mode == EgressMode::Loopback && is_localhost(&n)) {
                    Ok(())
                } else {
                    Err(self.refuse(&n, BlockedKind::Name))
                }
            }
        }
    }

    /// May the stack connect to `ip`? Loopback (`loopback` mode) or an
    /// allowed address; an IPv4-mapped IPv6 address counts as the IPv4
    /// address.
    pub fn check_addr(&self, ip: IpAddr) -> Result<(), Blocked> {
        if self.is_open() {
            return Ok(());
        }
        let ip = unmap(ip);
        if (self.mode == EgressMode::Loopback && ip.is_loopback())
            || self.nets.iter().any(|n| n.contains(ip))
        {
            Ok(())
        } else {
            Err(self.refuse(&ip.to_string(), BlockedKind::Address))
        }
    }

    /// May the stack connect to `ip`, an address `host` resolved to? A
    /// name an allow entry lists may resolve to any address; any other name
    /// (the implicit `localhost` names included) must resolve to an address
    /// [`check_addr`](Self::check_addr) allows.
    pub fn check_resolved(&self, host: &str, ip: IpAddr) -> Result<(), Blocked> {
        if self.is_open() {
            return Ok(());
        }
        if let Some(Host::Name(n)) = normalize(host)
            && self.name_listed(&n)
        {
            return Ok(());
        }
        self.check_addr(ip)
    }

    fn name_listed(&self, name: &str) -> bool {
        self.names.iter().any(|p| glob(p, name))
    }
}

/// A name glob in normal form: lowercase ASCII, labels IDN-converted.
fn pattern(p: &str) -> Result<String, String> {
    let p = p.to_ascii_lowercase();
    let p = p.strip_suffix('.').unwrap_or(&p);
    if p.chars().all(|c| c == '*' || c == '.') {
        return Err("a pattern of only wildcards allows everything; use --egress open".into());
    }
    let mut out = Vec::new();
    for label in p.split('.') {
        if label.is_ascii() {
            out.push(label.to_owned());
        } else if label.contains('*') {
            return Err(
                "wildcards in internationalised labels are not supported; write the xn-- form"
                    .into(),
            );
        } else {
            match url::Host::parse(label) {
                Ok(url::Host::Domain(d)) => out.push(d),
                _ => return Err("not a valid host name".into()),
            }
        }
    }
    let out = out.join(".");
    if out
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'*'))
    {
        Ok(out)
    } else {
        Err("host names use letters, digits, '-', '_', '.' and '*'".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lb(allow: &[&str]) -> EgressPolicy {
        EgressPolicy::new(EgressMode::Loopback, allow).unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn open_checks_nothing() {
        let p = EgressPolicy::open();
        assert!(p.is_open());
        assert!(p.check_name("example.com").is_ok());
        assert!(p.check_name("not a host").is_ok());
        assert!(p.check_addr(ip("8.8.8.8")).is_ok());
        assert!(p.check_resolved("example.com", ip("8.8.8.8")).is_ok());
    }

    #[test]
    fn loopback_addresses_ipv4_ipv6_and_mapped() {
        let p = lb(&[]);
        for ok in [
            "127.0.0.1",
            "127.255.255.254",
            "::1",
            "[::1]",
            "[::1]:8080",
            "127.0.0.1:80",
            "::ffff:127.0.0.1",
            "[::ffff:127.0.0.1]",
            "[::ffff:7f00:1]:9",
            "0x7f.1",
            "2130706433",
        ] {
            assert!(p.check_name(ok).is_ok(), "{ok}");
        }
        for no in [
            "0.0.0.0",
            "128.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "8.8.8.8",
            "::",
            "::2",
            "::ffff:10.0.0.1",
            "[::ffff:0a00:1]",
            "fe80::1",
            "::7f00:1",
            "64:ff9b::7f00:1",
        ] {
            assert!(p.check_name(no).is_err(), "{no}");
        }
        assert!(p.check_addr(ip("::ffff:127.0.0.1")).is_ok());
        assert!(p.check_addr(ip("::ffff:10.0.0.1")).is_err());
    }

    #[test]
    fn localhost_names_case_dots_and_idn() {
        let p = lb(&[]);
        for ok in [
            "localhost",
            "LOCALHOST",
            "localhost.",
            "a.localhost",
            "A.B.Localhost.",
            "localhost:3000",
            // Fullwidth letters fold to ASCII under IDNA.
            "\u{ff4c}ocalhost",
            "xn--bcher-kva.localhost",
            "b\u{fc}cher.localhost",
        ] {
            assert!(p.check_name(ok).is_ok(), "{ok}");
        }
        for no in [
            "notlocalhost",
            "localhost.com",
            "localhost.evil.test",
            "evil-localhost",
            "localhost..",
            ".localhost",
            "",
            "a b.localhost",
            "[localhost]",
            "localhost:99999",
            "localhost:",
        ] {
            assert!(p.check_name(no).is_err(), "{no:?}");
        }
    }

    #[test]
    fn refused_name_kinds() {
        let p = lb(&[]);
        let e = p.check_name("example.com").unwrap_err();
        assert_eq!(e.kind, BlockedKind::Name);
        assert_eq!(e.target, "example.com");
        assert_eq!(
            p.check_name("10.0.0.1").unwrap_err().kind,
            BlockedKind::Address
        );
        assert_eq!(p.check_name("a b").unwrap_err().kind, BlockedKind::Invalid);
        assert!(e.to_string().contains("loopback"), "{e}");
    }

    #[test]
    fn name_globs() {
        let p = lb(&["*.lab.test", "build-*.internal", "Exact.Test."]);
        for ok in [
            "a.lab.test",
            "A.B.LAB.TEST",
            "a.lab.test.",
            "build-7.internal",
            "build-.internal",
            "exact.test",
            "EXACT.TEST:8443",
        ] {
            assert!(p.check_name(ok).is_ok(), "{ok}");
        }
        for no in [
            "lab.test",
            "xlab.test",
            "a.lab.test.evil.com",
            "build.internal",
            "builder.internal",
            "sub.exact.test",
            "exact.test.evil",
        ] {
            assert!(p.check_name(no).is_err(), "{no}");
        }
    }

    #[test]
    fn idn_patterns_and_hosts() {
        let p = lb(&["b\u{fc}cher.test", "*.k\u{e4}se.test"]);
        assert!(p.check_name("xn--bcher-kva.test").is_ok());
        assert!(p.check_name("B\u{dc}CHER.test").is_ok());
        assert!(p.check_name("x.xn--kse-qla.test").is_ok());
        assert!(p.check_name("x.k\u{e4}se.test").is_ok());
        assert!(p.check_name("bucher.test").is_err());
        assert!(EgressPolicy::new(EgressMode::Loopback, &["*\u{fc}.test"]).is_err());
    }

    #[test]
    fn ip_and_cidr_entries() {
        let p = lb(&[
            "10.0.0.5",
            "192.168.88.0/24",
            "[fd00::1]",
            "fd10::/16",
            "172.16.0.0/12",
        ]);
        for ok in [
            "10.0.0.5",
            "10.0.0.5:81",
            "192.168.88.1",
            "192.168.88.255",
            "fd00::1",
            "[fd00::1]:80",
            "fd10:1::9",
            "172.31.255.255",
            "::ffff:10.0.0.5",
            "::ffff:192.168.88.9",
        ] {
            assert!(p.check_name(ok).is_ok(), "{ok}");
        }
        for no in [
            "10.0.0.6",
            "192.168.89.1",
            "fd00::2",
            "fd11::1",
            "172.32.0.1",
            "11.0.0.5",
        ] {
            assert!(p.check_name(no).is_err(), "{no}");
        }
        // Loopback stays allowed in loopback mode.
        assert!(p.check_name("127.0.0.1").is_ok());
    }

    #[test]
    fn cidr_edges() {
        let mut p = lb(&[]);
        p.add("10.1.2.3/8").unwrap(); // host bits are cleared
        assert!(p.check_addr(ip("10.200.0.1")).is_ok());
        p.add("0.0.0.0/0").unwrap();
        assert!(p.check_addr(ip("8.8.8.8")).is_ok());
        // An IPv6 block that holds IPv4-mapped addresses is an IPv4 block.
        let mut q = lb(&[]);
        q.add("::ffff:10.0.0.0/104").unwrap();
        assert!(q.check_addr(ip("10.1.1.1")).is_ok());
        assert!(q.check_addr(ip("11.1.1.1")).is_err());
        assert!(EgressPolicy::new(EgressMode::Loopback, &["10.0.0.0/33"]).is_err());
        assert!(EgressPolicy::new(EgressMode::Loopback, &["fd00::/129"]).is_err());
        assert!(EgressPolicy::new(EgressMode::Loopback, &["10.0.0.0/x"]).is_err());
        assert!(EgressPolicy::new(EgressMode::Loopback, &["example.test/8"]).is_err());
    }

    #[test]
    fn bad_entries() {
        for e in [
            "",
            "  ",
            "*",
            "*.*",
            "host:8080",
            "[::1]:80",
            "10.0.0.1:80",
            "a b",
            "a/b",
            "http://x.test",
            "[10.0.0.1]",
            "ex%ample.test",
        ] {
            assert!(
                EgressPolicy::new(EgressMode::Loopback, &[e]).is_err(),
                "{e:?} was accepted"
            );
        }
        // Open mode with entries is a mistake.
        assert!(EgressPolicy::new(EgressMode::Open, &["a.test"]).is_err());
        assert!(EgressPolicy::parse("loose", &[] as &[&str]).is_err());
    }

    #[test]
    fn numeric_ipv4_entry_forms_are_addresses() {
        let p = lb(&["0x0a.1"]); // 10.0.0.1
        assert!(p.check_addr(ip("10.0.0.1")).is_ok());
        assert!(p.check_name("10.0.0.1").is_ok());
    }

    #[test]
    fn allowlist_mode_has_no_implicit_loopback() {
        let p = EgressPolicy::new(EgressMode::AllowList, &["127.0.0.1", "ok.test"]).unwrap();
        assert!(p.check_name("127.0.0.1").is_ok());
        assert!(p.check_name("ok.test").is_ok());
        assert!(p.check_name("127.0.0.2").is_err());
        assert!(p.check_name("::1").is_err());
        assert!(p.check_name("localhost").is_err());
        let none = EgressPolicy::new(EgressMode::AllowList, &[] as &[&str]).unwrap();
        assert!(none.check_name("127.0.0.1").is_err());
        assert_eq!(none.mode(), EgressMode::AllowList);
    }

    #[test]
    fn resolved_addresses_are_checked() {
        let p = lb(&["*.lab.test", "10.0.0.0/8"]);
        // The implicit localhost names must resolve to loopback.
        assert!(p.check_resolved("foo.localhost", ip("127.0.0.1")).is_ok());
        assert!(p.check_resolved("foo.localhost", ip("::1")).is_ok());
        assert!(
            p.check_resolved("foo.localhost", ip("93.184.216.34"))
                .is_err()
        );
        assert!(p.check_resolved("localhost", ip("192.168.1.1")).is_err());
        // ... unless the operator allowed that address.
        assert!(p.check_resolved("localhost", ip("10.1.1.1")).is_ok());
        // A name an entry lists may resolve anywhere.
        assert!(p.check_resolved("a.lab.test", ip("203.0.113.9")).is_ok());
        // An IP literal is its own address.
        assert!(p.check_resolved("203.0.113.9", ip("203.0.113.9")).is_err());
        assert!(p.check_resolved("127.0.0.1", ip("127.0.0.1")).is_ok());
    }

    #[test]
    fn describe_and_mode_text() {
        assert_eq!(EgressPolicy::open().describe(), "open");
        assert_eq!(lb(&[]).describe(), "loopback");
        assert_eq!(
            lb(&["a.test", "10.0.0.0/8"]).describe(),
            "loopback (allow: a.test, 10.0.0.0/8)"
        );
        assert_eq!(
            "Loopback".parse::<EgressMode>().unwrap(),
            EgressMode::Loopback
        );
        assert_eq!(
            "allowlist".parse::<EgressMode>().unwrap(),
            EgressMode::AllowList
        );
        assert!("allow-list".parse::<EgressMode>().is_err());
    }

    #[test]
    fn glob_matcher() {
        assert!(glob("a*b", "ab"));
        assert!(glob("a*b", "axxb"));
        assert!(glob("*.x", "a.b.x"));
        assert!(!glob("*.x", "x"));
        assert!(glob("a*b*c", "aXbYbZc"));
        assert!(!glob("a*b*c", "aXbYbZ"));
        assert!(glob("abc", "abc"));
        assert!(!glob("abc", "abcd"));
    }
}
