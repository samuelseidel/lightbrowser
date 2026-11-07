//! Which hosts `lightbrowser mcp` may reach (internal-use rule, ADR 0041).
//!
//! Allowed by default: loopback addresses, `localhost`, `*.localhost` and
//! `*.test`. `--allow-host <glob>` adds hosts. The check runs per request
//! up front for `browser_navigate`, and at the transport through
//! [`lb_net::EgressPolicy`] (ADR 0044), which covers redirects, scripted
//! navigations and subresources before any DNS lookup.

use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use url::{Host, Url};

/// The host policy: the built-in loopback set plus `--allow-host` globs.
#[derive(Debug, Clone, Default)]
pub struct HostPolicy {
    /// Lower-case globs (`*` matches any run of characters, `?` one).
    allow: Vec<String>,
}

impl HostPolicy {
    pub fn new(allow: &[String]) -> HostPolicy {
        HostPolicy {
            allow: allow
                .iter()
                .map(|g| g.trim().to_ascii_lowercase())
                .collect(),
        }
    }

    /// May the browser request `url`? Schemes that never touch the network
    /// (`about:`, `data:`, `blob:`) are allowed; anything else that is not
    /// `http(s)`/`ws(s)` is not.
    pub fn allows(&self, url: &Url) -> bool {
        match url.scheme() {
            "about" | "data" | "blob" => return true,
            "http" | "https" | "ws" | "wss" => {}
            _ => return false,
        }
        match url.host() {
            None => false,
            Some(Host::Ipv4(ip)) => ip.is_loopback() || self.matches(&ip.to_string()),
            Some(Host::Ipv6(ip)) => ip.is_loopback() || self.matches(&ip.to_string()),
            Some(Host::Domain(d)) => {
                let d = d.trim_end_matches('.').to_ascii_lowercase();
                is_local_name(&d) || self.matches(&d)
            }
        }
    }

    fn matches(&self, host: &str) -> bool {
        self.allow.iter().any(|g| glob(g, host))
    }
}

/// `localhost`, `*.localhost`, `*.test`, and loopback IP literals that
/// arrive as text.
fn is_local_name(d: &str) -> bool {
    d == "localhost"
        || d.ends_with(".localhost")
        || d == "test"
        || d.ends_with(".test")
        || d.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// `*` and `?` wildcards, whole string.
fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] != '*' && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Refused URLs for tool error messages. Transport-level refusals (ADR 0044)
/// come back as `net::ERR_BLOCKED_BY_CLIENT`; the driver names the URL itself.
#[derive(Debug, Clone, Default)]
pub struct BlockedLog(Arc<Mutex<Vec<String>>>);

impl BlockedLog {
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
         with --allow-host <glob> (repeatable, matched against the host name) to allow more"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(p: &HostPolicy, u: &str) -> bool {
        p.allows(&Url::parse(u).unwrap())
    }

    #[test]
    fn default_policy_is_loopback_only() {
        let p = HostPolicy::default();
        for u in [
            "http://127.0.0.1:8080/",
            "http://127.1.2.3/x",
            "http://[::1]:9/",
            "http://localhost/",
            "https://app.localhost/",
            "http://a.b.test/",
            "http://test/",
            "http://LOCALHOST./",
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
    fn allow_host_globs_add_hosts() {
        let p = HostPolicy::new(&["*.corp.internal".into(), "Staging.Example.com".into()]);
        assert!(ok(&p, "https://a.corp.internal/"));
        assert!(ok(&p, "http://staging.example.com:81/"));
        assert!(!ok(&p, "http://corp.internal/"));
        assert!(!ok(&p, "http://evil.example.com/"));
        let any = HostPolicy::new(&["10.*".into()]);
        assert!(ok(&any, "http://10.1.2.3/"));
        assert!(!ok(&any, "http://11.1.2.3/"));
    }

    #[test]
    fn blocked_message_names_the_flag() {
        assert!(blocked_message(&["http://x.example/".into()]).contains("--allow-host"));
    }
}
