//! Which hosts `lightbrowser mcp` may reach (internal-use rule, ADR 0041).
//!
//! Allowed by default: loopback addresses, `localhost`, `*.localhost` and
//! `*.test`. `--allow-host <entry>` adds hosts; an entry is what
//! [`lb_net::EgressPolicy`] accepts (a name glob with `*`, an IP address, a
//! CIDR block, or a name pinned to addresses, ADR 0044).
//!
//! There is one policy, not two: [`HostPolicy`] builds the transport's
//! [`lb_net::EgressPolicy`] (handed to every browser context, where it
//! covers redirects, scripted navigations and subresources before any DNS
//! lookup) and asks that same policy, through
//! [`lb_net::EgressPolicy::check_url`], when `browser_navigate` checks a URL
//! up front. So the early check and the transport cannot disagree about an
//! entry or a host, and an entry the transport cannot parse stops the
//! server at startup.

use std::sync::{Arc, Mutex};

use lb_net::EgressPolicy;
use url::Url;

/// Hosts every `lightbrowser mcp` may reach on top of loopback and the
/// `localhost` names (which egress mode `loopback` allows by itself).
const BUILT_IN: [&str; 2] = ["*.test", "test"];

/// The host policy: the built-in loopback set plus `--allow-host` entries.
#[derive(Debug, Clone)]
pub struct HostPolicy {
    egress: Arc<EgressPolicy>,
}

impl HostPolicy {
    /// The policy for these `--allow-host` entries; an entry the egress
    /// policy refuses (a `?`, a port, a bare `*`, ...) is an error.
    pub fn new(allow: &[String]) -> Result<HostPolicy, String> {
        let mut entries: Vec<String> = BUILT_IN.iter().map(|s| (*s).to_owned()).collect();
        entries.extend(allow.iter().cloned());
        let egress =
            EgressPolicy::parse("loopback", &entries).map_err(|e| format!("--allow-host: {e}"))?;
        Ok(HostPolicy {
            egress: Arc::new(egress),
        })
    }

    /// The transport policy for `CdpHostOptions::egress`.
    pub fn egress(&self) -> Arc<EgressPolicy> {
        Arc::clone(&self.egress)
    }

    /// May the browser request `url`? Schemes that never touch the network
    /// (`about:`, `data:`, `blob:`) are allowed; anything else that is not
    /// `http(s)`/`ws(s)` is not. For the network schemes the answer is the
    /// transport's name check of the URL's host.
    pub fn allows(&self, url: &Url) -> bool {
        match url.scheme() {
            "about" | "data" | "blob" => true,
            "http" | "https" | "ws" | "wss" => self.egress.check_url(url).is_ok(),
            _ => false,
        }
    }
}

/// URLs of requests the transport refused (`net::ERR_BLOCKED_BY_CLIENT`)
/// since the last tool result. The driver's event reader writes it from
/// `Network.requestWillBeSent` and `Network.loadingFailed`; tool results
/// take it as a note (or, for `browser_navigate`, as the error).
#[derive(Debug, Clone, Default)]
pub struct BlockedLog(Arc<Mutex<Vec<String>>>);

impl BlockedLog {
    /// Most URLs kept between two tool results; later ones are dropped.
    const MAX: usize = 20;

    pub fn push(&self, url: String) {
        let mut v = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if v.len() < Self::MAX && !v.contains(&url) {
            v.push(url);
        }
    }

    pub fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn clear(&self) {
        self.take();
    }
}

/// The tool error for a refused navigation.
pub fn blocked_message(urls: &[String]) -> String {
    let what = urls.join(", ");
    format!(
        "navigation blocked by the host policy: {what}. lightbrowser mcp only visits loopback \
         addresses, localhost, *.localhost and *.test by default (internal-use rule); restart it \
         with --allow-host <entry> (repeatable: a host name glob, an IP address or a CIDR block) \
         to allow more"
    )
}

/// The note for requests the transport refused while a tool ran.
pub fn blocked_note(urls: &[String]) -> String {
    format!(
        "the host policy blocked requests to {} (--allow-host <entry> allows more hosts)",
        urls.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lb_net::EgressMode;

    fn ok(p: &HostPolicy, u: &str) -> bool {
        p.allows(&Url::parse(u).unwrap())
    }

    fn default_policy() -> HostPolicy {
        HostPolicy::new(&[]).unwrap()
    }

    #[test]
    fn default_policy_is_loopback_only() {
        let p = default_policy();
        for u in [
            "http://127.0.0.1:8080/",
            "http://127.1.2.3/x",
            "http://[::1]:9/",
            "http://localhost/",
            "https://app.localhost/",
            "http://a.b.test/",
            "http://test/",
            "http://LOCALHOST./",
            "ws://127.0.0.1:1/",
            "about:blank",
            "data:text/html,hi",
        ] {
            assert!(ok(&p, u), "{u}");
        }
        for u in [
            "http://example.com/",
            "https://example.test.com/",
            "http://localhost.example.com/",
            "http://10.0.0.1/",
            "http://192.168.1.1/",
            "http://[2001:db8::1]/",
            "http://notlocalhost/",
            "ftp://127.0.0.1/",
            "file:///etc/passwd",
            "javascript:1",
        ] {
            assert!(!ok(&p, u), "{u}");
        }
    }

    #[test]
    fn allow_host_entries_add_hosts() {
        let p = HostPolicy::new(&["*.corp.internal".into(), "Staging.Example.com".into()]).unwrap();
        assert!(ok(&p, "https://a.corp.internal/"));
        assert!(ok(&p, "http://staging.example.com:81/"));
        assert!(!ok(&p, "http://corp.internal/"));
        assert!(!ok(&p, "http://evil.example.com/"));
        let net = HostPolicy::new(&["10.0.0.0/8".into()]).unwrap();
        assert!(ok(&net, "http://10.1.2.3/"));
        assert!(!ok(&net, "http://11.1.2.3/"));
        // Entries the transport cannot parse stop the server.
        for bad in ["?x.example", "*", "a.example:80", "0.0.0.0/0"] {
            let e = HostPolicy::new(&[bad.into()]).unwrap_err();
            assert!(e.starts_with("--allow-host:"), "{e}");
        }
    }

    #[test]
    fn blocked_message_names_the_flag() {
        assert!(blocked_message(&["http://x.example/".into()]).contains("--allow-host"));
        assert!(blocked_note(&["http://x.example/".into()]).contains("x.example"));
    }

    #[test]
    fn blocked_log_keeps_each_url_once_and_is_bounded() {
        let log = BlockedLog::default();
        log.push("http://a.example/".into());
        log.push("http://a.example/".into());
        for i in 0..50 {
            log.push(format!("http://b{i}.example/"));
        }
        let v = log.take();
        assert_eq!(v.len(), BlockedLog::MAX);
        assert_eq!(v[0], "http://a.example/");
        assert!(log.take().is_empty());
    }

    /// The verdict of the transport for one hop of a direct request, as
    /// `lb_net::fetch` reaches it: the policy `run` hands to the browser
    /// (egress mode loopback plus `*.test`, `test` and the entries), then
    /// the name check of the URL's host without IPv6 brackets. `None`: the
    /// policy cannot be built, so the server refuses to start.
    fn transport(entry: &str, url: &Url) -> Option<bool> {
        let p = EgressPolicy::new(EgressMode::Loopback, &["*.test", "test", entry]).ok()?;
        let host = url.host_str()?;
        Some(
            p.check_name(host.trim_start_matches('[').trim_end_matches(']'))
                .is_ok(),
        )
    }

    /// The verdict of the tool-level check `browser_navigate` makes first.
    fn tool(entry: &str, url: &Url) -> Option<bool> {
        HostPolicy::new(&[entry.to_owned()])
            .ok()
            .map(|p| p.allows(url))
    }

    /// One table through both layers: the same verdict every time, and the
    /// verdict the table expects (`None`: refused at startup).
    #[test]
    fn tool_and_transport_agree_on_every_entry() {
        let cases: &[(&str, &str, Option<bool>)] = &[
            // Name globs.
            ("*.lab.test", "http://a.lab.test/", Some(true)),
            ("*.lab.example", "http://a.lab.example/", Some(true)),
            ("*.lab.example", "http://a.b.lab.example:8080/", Some(true)),
            ("*.lab.example", "http://lab.example/", Some(false)),
            (
                "*.lab.example",
                "http://a.lab.example.evil.com/",
                Some(false),
            ),
            // `?` is not a wildcard of the transport: refused as an entry.
            ("?x.example", "http://ax.example/", None),
            ("?x.test", "http://ax.test/", None),
            // Upper case in the entry and in the URL.
            ("*.CORP.Example", "http://WWW.corp.EXAMPLE/", Some(true)),
            ("Corp.Example", "http://corp.example/", Some(true)),
            // A trailing dot on either side.
            ("corp.example.", "http://corp.example/", Some(true)),
            ("corp.example", "http://corp.example./", Some(true)),
            // A port is not part of an entry; a port in the URL is ignored.
            ("corp.example:8080", "http://corp.example:8080/", None),
            ("corp.example", "http://corp.example:8080/", Some(true)),
            // IDN: the entry is converted to punycode as the URL host is.
            (
                "b\u{fc}cher.example",
                "http://xn--bcher-kva.example/",
                Some(true),
            ),
            (
                "b\u{fc}cher.example",
                "http://B\u{dc}CHER.example/",
                Some(true),
            ),
            (
                "xn--bcher-kva.example",
                "http://b\u{fc}cher.example/",
                Some(true),
            ),
            ("b\u{fc}cher.example", "http://bucher.example/", Some(false)),
            // IP addresses, also in the URL's other numeric forms.
            ("10.0.0.5", "http://10.0.0.5:8080/", Some(true)),
            ("10.0.0.5", "http://0x0a.0.0.5/", Some(true)),
            ("10.0.0.5", "http://10.0.0.6/", Some(false)),
            ("0x0a.0.0.5", "http://10.0.0.5/", Some(true)),
            ("[fd00::1]", "http://[fd00::1]/", Some(true)),
            ("fd00::1", "http://[fd00::1]:81/", Some(true)),
            ("[fd00::1]", "http://[fd00::2]/", Some(false)),
            ("[fd00::1]", "http://[::ffff:10.0.0.1]/", Some(false)),
            // CIDR blocks.
            ("10.0.0.0/8", "http://10.200.1.1/", Some(true)),
            ("10.0.0.0/8", "http://11.0.0.1/", Some(false)),
            ("10.0.0.0/8", "http://[::ffff:10.0.0.1]/", Some(true)),
            ("fd00::/8", "http://[fd12::1]/", Some(true)),
            ("0.0.0.0/0", "http://8.8.8.8/", None),
            // The localhost names (implicit) and entries naming them.
            ("localhost", "http://localhost/", Some(true)),
            ("localhost", "http://a.localhost/", Some(true)),
            ("*.localhost", "http://x.y.localhost/", Some(true)),
            ("*.localhost", "http://localhost.example/", Some(false)),
            // A name glob that looks like an address matches names only:
            // an IP literal is checked against addresses.
            ("10.*", "http://10.1.2.3/", Some(false)),
            ("10.*", "http://10.lab.example/", Some(true)),
            ("1.2.3.4.example", "http://1.2.3.4.example/", Some(true)),
            ("1.2.3.4.example", "http://1.2.3.4/", Some(false)),
            // Wildcards only, and pins (the name check allows a pinned
            // name; its addresses are checked after the lookup).
            ("*", "http://example.com/", None),
            ("*.*", "http://example.com/", None),
            ("pin.example@10.0.0.1", "http://pin.example/", Some(true)),
            ("pin.example@10.0.0.1", "http://10.0.0.1/", Some(false)),
        ];
        for &(entry, u, want) in cases {
            let url = Url::parse(u).unwrap();
            let (t, n) = (tool(entry, &url), transport(entry, &url));
            assert_eq!(
                t, n,
                "{entry:?} / {u}: tool {t:?}, transport {n:?} disagree"
            );
            assert_eq!(n, want, "{entry:?} / {u}");
        }
    }
}
