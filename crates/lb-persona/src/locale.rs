//! Time zone and locale overrides of a persona (ADR 0039): an operator
//! matches a proxy exit's location without editing the persona file
//! (`lightbrowser serve|fetch|validate --timezone ... --locale ...`, the
//! Python SDK's `timezone=` / `locale=`).
//!
//! What changes is what a browser in that place shows: the IANA zone
//! (`Date`, `Intl`), and for `locale` the ICU default locale, the
//! `navigator.languages` list and its `Accept-Language` header. Nothing
//! else of the persona moves (its OS, GPU, fonts and seeds stay), so the
//! consistency rules still hold; R09 (locale against zone) only warns.

use crate::PersonaProfile;

/// Overrides for [`PersonaProfile::with_locale_override`]. `None` keeps
/// the persona's value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocaleOverride {
    /// IANA zone id (`Europe/Berlin`).
    pub timezone: Option<String>,
    /// BCP 47 tag of the ICU default locale and the first language
    /// (`de-DE`).
    pub locale: Option<String>,
    /// `navigator.languages`, in order (default: `locale` and its
    /// language subtag, the shape of the reference personas'
    /// `["en-US", "en"]`).
    pub languages: Option<Vec<String>>,
}

impl LocaleOverride {
    pub fn is_empty(&self) -> bool {
        self.timezone.is_none() && self.locale.is_none() && self.languages.is_none()
    }
}

/// Chrome's `Accept-Language` for a language list
/// (`net::HttpUtil::GenerateAcceptLanguageHeader`): the first entry as
/// is, then `;q=0.9`, `;q=0.8` ... down to `;q=0.1`, entries as given.
/// `["en-US", "en"]` gives `en-US,en;q=0.9`, the reference personas'
/// value; recorded in Chrome 153 for `acceptLanguage` overrides
/// (`lb-cdp` `tests/emulation/`).
pub fn accept_language_header<S: AsRef<str>>(languages: &[S]) -> String {
    let mut out = String::new();
    let mut q = 10u32;
    for lang in languages
        .iter()
        .map(AsRef::as_ref)
        .filter(|l| !l.is_empty())
    {
        if q == 10 {
            out.push_str(lang);
        } else {
            out.push_str(&format!(",{lang};q=0.{q}"));
        }
        q = q.saturating_sub(1).max(1);
    }
    out
}

/// Whether `tz` can be an IANA zone id (ICU decides in the chrome build;
/// this only refuses what no zone id looks like).
fn plausible_zone(tz: &str) -> bool {
    !tz.is_empty()
        && tz.len() <= 64
        && tz
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-+".contains(&b))
}

/// Whether `tag` looks like a BCP 47 language tag (`de`, `de-DE`,
/// `zh-Hant-TW`, `en-US-u-ca-buddhist`).
fn plausible_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let lang = parts.next().unwrap_or_default();
    (2..=8).contains(&lang.len())
        && lang.bytes().all(|b| b.is_ascii_alphabetic())
        && parts.all(|p| (1..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}

impl PersonaProfile {
    /// This persona with `o` applied (see the module docs). Fails on a
    /// value that cannot be a zone id or a language tag.
    pub fn with_locale_override(&self, o: &LocaleOverride) -> Result<PersonaProfile, String> {
        let mut p = self.clone();
        if let Some(tz) = &o.timezone {
            if !plausible_zone(tz) {
                return Err(format!("time zone {tz:?} is not an IANA zone id"));
            }
            p.locale.timezone = tz.clone();
            // Derived from the zone, which answers it now.
            p.locale.timezone_offset = None;
        }
        if let Some(tag) = &o.locale {
            if !plausible_tag(tag) {
                return Err(format!("locale {tag:?} is not a BCP 47 language tag"));
            }
            p.locale.language = tag.clone();
        }
        let languages = match (&o.languages, &o.locale) {
            (Some(l), _) => Some(l.clone()),
            (None, Some(tag)) => {
                let base = tag.split('-').next().unwrap_or(tag);
                let mut l = vec![tag.clone()];
                if base != tag {
                    l.push(base.to_owned());
                }
                Some(l)
            }
            (None, None) => None,
        };
        if let Some(l) = languages {
            if l.is_empty() || l.iter().any(|t| !plausible_tag(t)) {
                return Err(format!("languages {l:?} must be BCP 47 language tags"));
            }
            p.browser.accept_language = accept_language_header(&l);
            p.locale.languages = l;
        }
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn persona() -> PersonaProfile {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../profiles/reference/chrome/chrome153_win10_rtx3060_1080p.json"
        );
        PersonaProfile::from_path_checked(path).unwrap()
    }

    #[test]
    fn header_matches_the_reference_personas() {
        let p = persona();
        assert_eq!(
            accept_language_header(&p.locale.languages),
            p.browser.accept_language
        );
        assert_eq!(
            accept_language_header(&["de-DE", " en ", "fr_FR"]),
            "de-DE, en ;q=0.9,fr_FR;q=0.8"
        );
    }

    #[test]
    fn override_keeps_the_rules() {
        let p = persona();
        let o = LocaleOverride {
            timezone: Some("Europe/Berlin".into()),
            locale: Some("de-DE".into()),
            languages: None,
        };
        let q = p.with_locale_override(&o).unwrap();
        assert_eq!(q.locale.timezone, "Europe/Berlin");
        assert_eq!(q.locale.timezone_offset, None);
        assert_eq!(q.locale.language, "de-DE");
        assert_eq!(q.locale.languages, ["de-DE", "de"]);
        assert_eq!(q.browser.accept_language, "de-DE,de;q=0.9");
        assert!(crate::validate(&q).is_valid());
        // A VPN user: the persona's language, the exit's zone.
        let vpn = p
            .with_locale_override(&LocaleOverride {
                timezone: Some("Europe/Paris".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(vpn.locale.language, "en-US");
        assert!(crate::validate(&vpn).is_valid());
        for bad in [
            LocaleOverride {
                timezone: Some("Europe/Berlin now".into()),
                ..Default::default()
            },
            LocaleOverride {
                locale: Some("not a tag".into()),
                ..Default::default()
            },
            LocaleOverride {
                languages: Some(vec![]),
                ..Default::default()
            },
        ] {
            assert!(p.with_locale_override(&bad).is_err(), "{bad:?}");
        }
    }
}
