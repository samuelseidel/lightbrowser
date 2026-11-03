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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpParams {
    /// Initial IPv4 TTL / IPv6 hop limit.
    pub ttl: u8,
    /// Don't-fragment bit (IPv4 only).
    pub df: bool,
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

    /// True if sockets alone cannot produce this SYN, so connections need the
    /// packet shim (`nfq`): a different option layout than the kernel's, a
    /// window the kernel would not choose, or ECN flags.
    pub fn needs_shim(&self) -> bool {
        self.tcp.options != KERNEL_LAYOUT
            || self.tcp.window != Self::kernel_window(self.tcp.mss)
            || self.tcp.ecn
    }

    /// The socket mark that selects this profile in the shim.
    pub fn mark(&self) -> u32 {
        super::MARK_BASE | u32::from(self.slot)
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
