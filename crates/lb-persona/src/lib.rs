//! Persona model shared by every layer: browser family, validation and
//! deterministic fingerprint noise.
//!
//! The persona schema and consistency rules come from stealth-browser
//! (`consistency/`) and are vendored here in M4.

pub mod sfc32;

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
}
