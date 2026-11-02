//! Persona model shared by every layer: browser family, validation and
//! deterministic fingerprint noise.
//!
//! - [`Persona`]: the small network-facing summary (headers, TLS profile id).
//! - [`PersonaProfile`]: the full persona (`persona_v2.schema.json`; v1 files
//!   load as Firefox). [`PersonaProfile::to_network`] derives a [`Persona`].
//! - [`consistency`]: rules R01-R16, vendored from stealth-browser
//!   (`consistency/`, R01-R10) plus the Chrome rules. [`validate_file`]
//!   backs `lightbrowser validate`.
//! - [`sfc32`]: deterministic noise.

pub mod consistency;
pub mod profile;
pub mod sfc32;
pub mod uach;

pub use consistency::{
    ValidateOutcome, ValidationResult, Violation, Warning, validate, validate_file,
};
pub use profile::{BrandVersion, ClientHints, PersonaError, PersonaProfile, format_brand_list};

/// The network-facing summary of a persona: what `lb-net` needs. The full
/// schema (`profiles/schemas/persona_v2.schema.json`) is [`PersonaProfile`];
/// get this from it with [`PersonaProfile::to_network`], or load it directly
/// with [`Persona::load_json`] / [`Persona::from_path`].
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
        }
    }
}

use std::fmt;
use std::str::FromStr;

/// The real browser a persona claims to be. Each family builds its own
/// binary with the matching JS engine, IDL set and network fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
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
}
