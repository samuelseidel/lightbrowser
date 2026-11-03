//! Persona model shared by every layer: browser family, validation and
//! deterministic fingerprint noise.
//!
//! The persona schema and consistency rules come from stealth-browser
//! (`consistency/`) and are vendored here in M4.

pub mod sfc32;

/// The persona a browsing context presents. M1 carries only what the network
/// layer needs; the full schema (`profiles/schemas/persona_v2.schema.json`)
/// arrives in M4 and is validated by the vendored consistency rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Persona {
    pub family: BrowserFamily,
    /// Full `User-Agent` string.
    pub user_agent: String,
    /// `Accept-Language` value, e.g. `en-US,en;q=0.9`.
    pub accept_language: String,
    /// Chrome only: `sec-ch-ua` value, e.g. `"Chromium";v="131", ...`.
    pub sec_ch_ua: Option<String>,
    /// Chrome only: `sec-ch-ua-platform` value without quotes, e.g. `Windows`.
    pub sec_ch_ua_platform: Option<String>,
    /// Chrome only: `sec-ch-ua-mobile` (`?0` / `?1`).
    pub sec_ch_ua_mobile: Option<String>,
    /// `profile_id` of the TLS/H2 profile in `profiles/tls/`.
    pub tls_profile_id: String,
    /// `profile_id` of the TCP/IP SYN profile in `profiles/tcp/`. Its OS must
    /// match the platform in `user_agent` (see [`Persona::ua_os`]).
    pub tcp_profile_id: String,
}

impl Persona {
    /// Built-in Chrome 153 on Windows 10 persona, used when none is given.
    ///
    /// TLS/H2 come from `chrome_153_linux`, captured from Chromium
    /// 153.0.8010.12 with lb-fptest; Chrome's ClientHello and H2 preface do
    /// not depend on the OS. The `sec-ch-ua` GREASE brand and brand order
    /// follow Chromium's per-major-version rule (for 153: `Not_A Brand`, v=8,
    /// order brand, GREASE, Chromium).
    pub fn default_chrome() -> Self {
        Self {
            family: BrowserFamily::Chrome,
            user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/153.0.0.0 Safari/537.36"
                .into(),
            accept_language: "en-US,en;q=0.9".into(),
            sec_ch_ua: Some(
                r#""Google Chrome";v="153", "Not_A Brand";v="8", "Chromium";v="153""#.into(),
            ),
            sec_ch_ua_platform: Some("Windows".into()),
            sec_ch_ua_mobile: Some("?0".into()),
            tls_profile_id: "chrome_153_linux".into(),
            tcp_profile_id: "windows_10_11".into(),
        }
    }

    /// Built-in Firefox 128 on Windows 10 persona (matches the captured
    /// `firefox_128_win10` TLS profile from stealth-browser).
    pub fn default_firefox() -> Self {
        Self {
            family: BrowserFamily::Firefox,
            user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:128.0) Gecko/20100101 \
Firefox/128.0"
                .into(),
            accept_language: "en-US,en;q=0.5".into(),
            sec_ch_ua: None,
            sec_ch_ua_platform: None,
            sec_ch_ua_mobile: None,
            tls_profile_id: "firefox_128_win10".into(),
            tcp_profile_id: "windows_10_11".into(),
        }
    }

    /// The operating system the `user_agent` claims, or `None` if the string
    /// names none we know (Android and iOS are not supported personas).
    pub fn ua_os(&self) -> Option<OsFamily> {
        let ua = self.user_agent.as_str();
        if ua.contains("Android") || ua.contains("iPhone") || ua.contains("iPad") {
            None
        } else if ua.contains("Windows NT") {
            Some(OsFamily::Windows)
        } else if ua.contains("Macintosh") || ua.contains("Mac OS X") {
            Some(OsFamily::Macos)
        } else if ua.contains("Linux") || ua.contains("X11") {
            Some(OsFamily::Linux)
        } else {
            None
        }
    }
}

/// The desktop operating system a persona claims. Picks the TCP/IP stack
/// behaviour on the wire (`profiles/tcp/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OsFamily {
    Windows,
    Macos,
    Linux,
}

impl OsFamily {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Macos => "macos",
            Self::Linux => "linux",
        }
    }
}

impl fmt::Display for OsFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for OsFamily {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "windows" | "win" => Ok(Self::Windows),
            "macos" | "mac" | "darwin" => Ok(Self::Macos),
            "linux" => Ok(Self::Linux),
            _ => Err(format!("unknown os: {s:?}")),
        }
    }
}

use std::fmt;
use std::str::FromStr;

/// The real browser a persona claims to be. Each family builds its own
/// binary with the matching JS engine, IDL set and network fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BrowserFamily {
    /// V8, Chromium IDL, BoringSSL Chrome ClientHello, Chrome H2 settings.
    Chrome,
    /// SpiderMonkey, Gecko IDL, Firefox/NSS-style ClientHello, Firefox H2 settings.
    Firefox,
}

impl BrowserFamily {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Firefox => "firefox",
        }
    }
}

impl fmt::Display for BrowserFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownFamily(pub String);

impl fmt::Display for UnknownFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown browser family: {:?}", self.0)
    }
}

impl std::error::Error for UnknownFamily {}

impl FromStr for BrowserFamily {
    type Err = UnknownFamily;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "chrome" | "chromium" => Ok(Self::Chrome),
            "firefox" | "gecko" => Ok(Self::Firefox),
            _ => Err(UnknownFamily(s.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_round_trip() {
        for f in [BrowserFamily::Chrome, BrowserFamily::Firefox] {
            assert_eq!(f.as_str().parse::<BrowserFamily>(), Ok(f));
        }
        assert!("safari".parse::<BrowserFamily>().is_err());
    }

    #[test]
    fn ua_os_follows_the_user_agent() {
        let mut p = Persona::default_chrome();
        assert_eq!(p.ua_os(), Some(OsFamily::Windows));
        p.user_agent = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36".into();
        assert_eq!(p.ua_os(), Some(OsFamily::Macos));
        p.user_agent = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36".into();
        assert_eq!(p.ua_os(), Some(OsFamily::Linux));
        p.user_agent = "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36".into();
        assert_eq!(p.ua_os(), None);
    }

    #[test]
    fn os_family_round_trip() {
        for o in [OsFamily::Windows, OsFamily::Macos, OsFamily::Linux] {
            assert_eq!(o.as_str().parse::<OsFamily>(), Ok(o));
        }
        assert!("beos".parse::<OsFamily>().is_err());
    }
}
