//! TCP/IP SYN profile, loaded from `profiles/tcp/*.json`.
//!
//! A profile describes what an operating system's TCP stack puts on the wire
//! in the first packet of a connection: IP TTL and DF bit, TCP window, and the
//! option block (which options, in which order). Passive OS fingerprinting
//! (p0f, JA4T) reads exactly these values.

use std::path::Path;

use lb_persona::OsFamily;
use serde::{Deserialize, Serialize};

use crate::error::NetError;

/// One TCP option, in the order the stack writes it. `Nop` and `Eol` are
/// padding the stack puts between real options; their position is part of the
/// fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TcpOpt {
    Mss,
    Nop,
    /// Window scale.
    Ws,
    SackOk,
    /// Timestamps.
    Ts,
    Eol,
}

impl TcpOpt {
    /// IANA option kind.
    pub const fn kind(self) -> u8 {
        match self {
            Self::Eol => 0,
            Self::Nop => 1,
            Self::Mss => 2,
            Self::Ws => 3,
            Self::SackOk => 4,
            Self::Ts => 8,
        }
    }

    /// Encoded size in bytes.
    pub const fn len(self) -> usize {
        match self {
            Self::Eol | Self::Nop => 1,
            Self::Mss => 4,
            Self::Ws => 3,
            Self::SackOk => 2,
            Self::Ts => 10,
        }
    }

    pub const fn is_empty(self) -> bool {
        false
    }
}

/// The layout the Linux kernel writes for a client SYN. A profile with this
/// layout needs no packet rewriting.
pub const KERNEL_LAYOUT: [TcpOpt; 5] = [
    TcpOpt::Mss,
    TcpOpt::SackOk,
    TcpOpt::Ts,
    TcpOpt::Nop,
    TcpOpt::Ws,
];

/// What the IPv4 identification field of every outgoing packet holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IdPolicy {
    /// Leave the kernel's value (Linux: a per-connection counter from a random
    /// start).
    #[default]
    Kernel,
    /// Always 0, as macOS does for DF packets (RFC 6864).
    Zero,
    /// One counter shared by every connection of the profile, +1 per packet,
    /// from a random start, as Windows does.
    HostCounter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpParams {
    /// Initial IPv4 TTL / IPv6 hop limit.
    pub ttl: u8,
    /// Don't-fragment bit (IPv4 only).
    pub df: bool,
    /// Identification field policy (IPv4 only).
    #[serde(default)]
    pub id: IdPolicy,
}

/// The option layout of SYN retransmissions from `from_attempt` on
/// (1 = the first retransmission).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionVariant {
    pub from_attempt: u8,
    pub options: Vec<TcpOpt>,
}

/// How the stack retransmits an unanswered SYN.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SynRetransmit {
    /// Milliseconds after the first SYN at which each retransmission is sent.
    /// The shim drops the kernel's own retransmissions and sends these.
    pub schedule_ms: Vec<u32>,
    /// Different option layouts for later retransmissions (Windows drops
    /// window scaling).
    #[serde(default)]
    pub option_variants: Vec<OptionVariant>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TcpParams {
    /// Window field of the SYN (unscaled).
    pub window: u16,
    /// MSS option value for an Ethernet path.
    pub mss: u16,
    /// Window scale shift count.
    pub window_scale: u8,
    /// Options in wire order. Padding to a 4-byte boundary is added with zero
    /// bytes (EOL), as the real stacks do.
    pub options: Vec<TcpOpt>,
    /// ECE and CWR set on the SYN (ECN setup).
    #[serde(default)]
    pub ecn: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expected {
    /// JA4T-style value: `window_options_mss_wscale`.
    pub ja4t: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Provenance {
    /// `captured` (measured on a real stack of ours) or
    /// `documented-unverified` (from public signatures, not yet re-captured).
    pub status: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TcpProfile {
    pub profile_id: String,
    pub description: String,
    /// `windows`, `macos` or `linux`.
    pub os: String,
    /// Selects this profile in the packet shim. Carried in the low 16 bits of
    /// the socket mark. Unique per profile, never zero.
    pub slot: u16,
    pub ip: IpParams,
    pub tcp: TcpParams,
    /// Retransmission timing of an unanswered SYN. `None` keeps the kernel's,
    /// which depends on its version (Linux 6.1+: 1, 2, 3, 4, 6, 10 s because of
    /// `tcp_syn_linear_timeouts`; older: 1, 3, 7, 15 s).
    #[serde(default)]
    pub syn_retransmit: Option<SynRetransmit>,
    pub expected: Expected,
    #[serde(default)]
    pub provenance: Provenance,
}

impl TcpProfile {
    pub fn load(path: &Path) -> Result<Self, NetError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| NetError::Profile(format!("{}: {e}", path.display())))?;
        let p: Self = serde_json::from_str(&text)
            .map_err(|e| NetError::Profile(format!("{}: {e}", path.display())))?;
        p.validate()?;
        Ok(p)
    }

    /// Load every `*.json` in `dir`, sorted by `profile_id`. Slots must be
    /// unique across the directory.
    pub fn load_dir(dir: &Path) -> Result<Vec<Self>, NetError> {
        let rd = std::fs::read_dir(dir)
            .map_err(|e| NetError::Profile(format!("{}: {e}", dir.display())))?;
        let mut out = Vec::new();
        for entry in rd {
            let path = entry
                .map_err(|e| NetError::Profile(format!("{}: {e}", dir.display())))?
                .path();
            if path.extension().is_some_and(|e| e == "json") {
                out.push(Self::load(&path)?);
            }
        }
        out.sort_by(|a, b| a.profile_id.cmp(&b.profile_id));
        for (i, a) in out.iter().enumerate() {
            if let Some(b) = out[i + 1..].iter().find(|b| b.slot == a.slot) {
                return Err(NetError::Profile(format!(
                    "profiles {} and {} share slot {}",
                    a.profile_id, b.profile_id, a.slot
                )));
            }
        }
        Ok(out)
    }

    /// The operating system, parsed from `os`.
    pub fn os(&self) -> Result<OsFamily, NetError> {
        self.os
            .parse()
            .map_err(|e: String| NetError::Profile(format!("{}: {e}", self.profile_id)))
    }

    /// Checks the profile describes a SYN that can exist and that its stored
    /// `expected.ja4t` agrees with its own fields.
    pub fn validate(&self) -> Result<(), NetError> {
        let bad = |m: String| Err(NetError::Profile(format!("{}: {m}", self.profile_id)));
        self.os()?;
        if self.slot == 0 {
            return bad("slot must not be 0".into());
        }
        let count = |o: TcpOpt| self.tcp.options.iter().filter(|x| **x == o).count();
        for o in [TcpOpt::Mss, TcpOpt::Ws, TcpOpt::SackOk, TcpOpt::Ts] {
            if count(o) > 1 {
                return bad(format!("option {o:?} appears more than once"));
            }
        }
        if count(TcpOpt::Mss) == 0 {
            return bad("an MSS option is required".into());
        }
        if let Some(pos) = self.tcp.options.iter().position(|o| *o == TcpOpt::Eol)
            && pos + 1 != self.tcp.options.len()
        {
            return bad("eol must be the last option".into());
        }
        let size: usize = self.tcp.options.iter().map(|o| o.len()).sum();
        if size.next_multiple_of(4) > 40 {
            return bad(format!("options need {size} bytes, the limit is 40"));
        }
        if self.tcp.window_scale > 14 {
            return bad("window_scale must be 14 or less".into());
        }
        if self.slot >= super::MARK_ALL as u16 {
            return bad(format!("slot must be below {}", super::MARK_ALL));
        }
        if let Some(r) = &self.syn_retransmit {
            let s = &r.schedule_ms;
            if s.is_empty() || s.len() > 12 || s[0] < 100 || s.windows(2).any(|w| w[1] <= w[0]) {
                return bad(
                    "syn_retransmit.schedule_ms must be 1 to 12 increasing times of at least 100 ms"
                        .into(),
                );
            }
            for v in &r.option_variants {
                let mut copy = self.clone();
                copy.syn_retransmit = None;
                copy.tcp.options.clone_from(&v.options);
                copy.expected.ja4t = copy.ja4t();
                copy.validate()?;
            }
        }
        let ja4t = self.ja4t();
        if ja4t != self.expected.ja4t {
            return bad(format!(
                "expected.ja4t {:?} does not match the fields ({ja4t})",
                self.expected.ja4t
            ));
        }
        Ok(())
    }

    /// JA4T-style value from the profile fields. An EOL ends the option list
    /// and appears once, as `0`.
    pub fn ja4t(&self) -> String {
        let kinds: Vec<String> = self
            .tcp
            .options
            .iter()
            .map(|o| o.kind().to_string())
            .collect();
        let has = |o| self.tcp.options.contains(&o);
        format!(
            "{}_{}_{}_{}",
            self.tcp.window,
            kinds.join("-"),
            if has(TcpOpt::Mss) {
                self.tcp.mss.to_string()
            } else {
                String::new()
            },
            if has(TcpOpt::Ws) {
                self.tcp.window_scale.to_string()
            } else {
                String::new()
            },
        )
    }

    /// The window the Linux kernel puts in a SYN that advertises `mss`
    /// (a whole number of segments that fits 65535; 64240 for 1460).
    pub const fn kernel_window(mss: u16) -> u16 {
        (65535 / mss) * mss
    }

    /// True if the SYN itself needs rewriting: a different option layout than
    /// the kernel's, a window the kernel would not choose, or ECN flags.
    pub fn needs_syn_rewrite(&self) -> bool {
        self.tcp.options != KERNEL_LAYOUT
            || self.tcp.window != Self::kernel_window(self.tcp.mss)
            || self.tcp.ecn
    }

    /// True if every packet of the connection must pass the shim: it rewrites
    /// the IP ID or controls SYN retransmissions.
    pub fn needs_every_packet(&self) -> bool {
        self.ip.id != IdPolicy::Kernel || self.syn_retransmit.is_some()
    }

    /// True if sockets alone cannot produce this connection, so it needs the
    /// packet shim (`nfq`).
    pub fn needs_shim(&self) -> bool {
        self.needs_syn_rewrite() || self.needs_every_packet()
    }

    /// The socket mark that selects this profile in the shim. [`MARK_ALL`]
    /// asks for every packet of the connection, not just the SYN.
    ///
    /// [`MARK_ALL`]: super::MARK_ALL
    pub fn mark(&self) -> u32 {
        let all = if self.needs_every_packet() {
            super::MARK_ALL
        } else {
            0
        };
        super::MARK_BASE | all | u32::from(self.slot)
    }

    /// `TCP_WINDOW_CLAMP` value that makes the kernel pick `window_scale`.
    /// The kernel uses `ilog2(clamp) - 15` as the shift.
    pub fn window_clamp(&self) -> u32 {
        1u32 << (15 + u32::from(self.tcp.window_scale))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profiles_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/tcp")
    }

    #[test]
    fn shipped_profiles_load_and_are_self_consistent() {
        let all = TcpProfile::load_dir(&profiles_dir()).unwrap();
        let ids: Vec<_> = all.iter().map(|p| p.profile_id.as_str()).collect();
        assert_eq!(ids, ["linux_6", "macos_14", "windows_10_11"]);
    }

    #[test]
    fn only_linux_runs_without_the_shim() {
        let all = TcpProfile::load_dir(&profiles_dir()).unwrap();
        for p in all {
            assert_eq!(p.needs_shim(), p.os != "linux", "{}", p.profile_id);
        }
    }

    #[test]
    fn kernel_window_rounds_to_whole_segments() {
        assert_eq!(TcpProfile::kernel_window(1460), 64240);
    }

    #[test]
    fn validate_rejects_a_wrong_ja4t_and_double_options() {
        let mut p = TcpProfile::load(&profiles_dir().join("windows_10_11.json")).unwrap();
        p.expected.ja4t = "1_2_3_4".into();
        assert!(p.validate().is_err());
        let mut p = TcpProfile::load(&profiles_dir().join("windows_10_11.json")).unwrap();
        p.tcp.options.push(TcpOpt::Ws);
        assert!(p.validate().is_err());
    }

    #[test]
    fn window_clamp_gives_the_requested_shift() {
        let p = TcpProfile::load(&profiles_dir().join("windows_10_11.json")).unwrap();
        assert_eq!(p.window_clamp().ilog2() - 15, 8);
    }
}
