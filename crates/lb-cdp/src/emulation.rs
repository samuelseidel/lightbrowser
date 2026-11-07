//! Persona checks for `Emulation.*` and `Network.setUserAgentOverride`.
//!
//! A value that matches the persona is accepted (and needs no change). A
//! value that contradicts it is rejected with an error that says which
//! persona field it contradicts. A viewport smaller than the persona's
//! screen is allowed (windows can be resized) and is passed to the host.
//!
//! Time zone, locale, `acceptLanguage` and geolocation are not checked
//! against the persona (ADR 0039): they follow a proxy's exit, as Chrome
//! takes any valid value. This module parses them as Chrome's bindings do
//! and holds Chrome's rules for them (locale validity, the
//! `Accept-Language` q-values, `navigator.languages` parsing).

use lb_css::MediaEnv;
use lb_persona::{BrowserFamily, PersonaProfile};
use serde::Deserialize;
use serde_json::Value;

use crate::host::{GeoPosition, GeolocationOverride, ViewportOverride};
use crate::protocol::CdpError;

fn reject(method: &str, what: String) -> CdpError {
    CdpError::server(format!(
        "{method}: {what}; it contradicts the persona (use LightBrowser.setPersona to change it)"
    ))
}

/// `Emulation.setDeviceMetricsOverride` params.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceMetrics {
    #[serde(default)]
    pub width: f64,
    #[serde(default)]
    pub height: f64,
    #[serde(default)]
    pub device_scale_factor: f64,
    #[serde(default)]
    pub mobile: bool,
    #[serde(default)]
    pub screen_width: Option<f64>,
    #[serde(default)]
    pub screen_height: Option<f64>,
    #[serde(default)]
    pub position_x: Option<f64>,
    #[serde(default)]
    pub position_y: Option<f64>,
    #[serde(default)]
    pub screen_orientation: Option<ScreenOrientation>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ScreenOrientation {
    #[serde(rename = "type")]
    pub kind: String,
    pub angle: i64,
}

fn is_mobile(p: &PersonaProfile) -> bool {
    p.client_hints.as_ref().is_some_and(|h| h.mobile)
}

/// Checks device metrics. Returns the viewport to apply, or `None` when the
/// override equals the persona's own inner size (nothing to change).
pub fn check_device_metrics(
    p: &PersonaProfile,
    m: &DeviceMetrics,
) -> Result<Option<ViewportOverride>, CdpError> {
    const M: &str = "Emulation.setDeviceMetricsOverride";
    let s = &p.screen;
    if m.mobile != is_mobile(p) {
        return Err(reject(
            M,
            format!(
                "mobile={} but the persona is a {} device",
                m.mobile,
                if is_mobile(p) { "mobile" } else { "desktop" }
            ),
        ));
    }
    if m.device_scale_factor != 0.0 && (m.device_scale_factor - s.device_pixel_ratio).abs() > 1e-6 {
        return Err(reject(
            M,
            format!(
                "deviceScaleFactor {} differs from screen.device_pixel_ratio {}",
                m.device_scale_factor, s.device_pixel_ratio
            ),
        ));
    }
    for (given, want, name) in [
        (m.screen_width, s.width, "screenWidth"),
        (m.screen_height, s.height, "screenHeight"),
    ] {
        if let Some(v) = given
            && v != 0.0
            && v != f64::from(want)
        {
            return Err(reject(
                M,
                format!(
                    "{name} {v} differs from the persona screen ({}x{})",
                    s.width, s.height
                ),
            ));
        }
    }
    if m.position_x.unwrap_or(0.0) != 0.0 || m.position_y.unwrap_or(0.0) != 0.0 {
        return Err(reject(M, "positionX/positionY are not supported".into()));
    }
    if let Some(o) = &m.screen_orientation {
        let landscape = s.width >= s.height;
        let want = if landscape {
            "landscapePrimary"
        } else {
            "portraitPrimary"
        };
        if o.kind != want || o.angle != 0 {
            return Err(reject(
                M,
                format!(
                    "screenOrientation {} {} differs from the persona screen ({want} 0)",
                    o.kind, o.angle
                ),
            ));
        }
    }
    if m.width < 0.0 || m.height < 0.0 {
        return Err(CdpError::invalid_params("width and height must be >= 0"));
    }
    let (w, h) = (m.width as u32, m.height as u32);
    if w == 0 && h == 0 {
        return Ok(None);
    }
    let (w, h) = (
        if w == 0 { s.inner_width } else { w },
        if h == 0 { s.inner_height } else { h },
    );
    if w > s.avail_width.max(s.width) || h > s.avail_height.max(s.height) {
        return Err(reject(
            M,
            format!(
                "viewport {w}x{h} does not fit the persona screen ({}x{})",
                s.width, s.height
            ),
        ));
    }
    if w == s.inner_width && h == s.inner_height {
        return Ok(None);
    }
    Ok(Some(ViewportOverride {
        width: w,
        height: h,
    }))
}

/// `Emulation.setUserAgentOverride` / `Network.setUserAgentOverride` params.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserAgentOverride {
    #[serde(default)]
    pub user_agent: String,
    #[serde(default)]
    pub accept_language: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub user_agent_metadata: Option<Value>,
}

/// The browser family a user-agent string claims.
pub fn ua_family(ua: &str) -> Option<BrowserFamily> {
    if ua.contains("Firefox/") || ua.contains("Gecko/20100101") {
        Some(BrowserFamily::Firefox)
    } else if ua.contains("Chrome/") || ua.contains("Chromium/") {
        Some(BrowserFamily::Chrome)
    } else {
        None
    }
}

pub fn check_user_agent(p: &PersonaProfile, o: &UserAgentOverride) -> Result<(), CdpError> {
    const M: &str = "Emulation.setUserAgentOverride";
    if !o.user_agent.is_empty() && o.user_agent != p.browser.user_agent {
        let what = match ua_family(&o.user_agent) {
            Some(f) if f != p.browser.family => {
                format!("user agent claims {f}, the persona is {}", p.browser.family)
            }
            _ => format!(
                "user agent {:?} differs from the persona's {:?}",
                o.user_agent, p.browser.user_agent
            ),
        };
        return Err(reject(M, what));
    }
    if let Some(pl) = o.platform.as_deref().filter(|s| !s.is_empty())
        && pl != p.os.platform
    {
        return Err(reject(
            M,
            format!(
                "platform {pl:?} differs from the persona's {:?}",
                p.os.platform
            ),
        ));
    }
    if let Some(meta) = &o.user_agent_metadata {
        check_ua_metadata(p, meta)?;
    }
    Ok(())
}

fn check_ua_metadata(p: &PersonaProfile, meta: &Value) -> Result<(), CdpError> {
    const M: &str = "Emulation.setUserAgentOverride";
    let Some(h) = &p.client_hints else {
        return Err(reject(
            M,
            "userAgentMetadata given but the persona has no client hints".into(),
        ));
    };
    let str_field = |k: &str| meta.get(k).and_then(Value::as_str);
    for (key, want) in [
        ("platform", h.platform.as_str()),
        ("platformVersion", h.platform_version.as_str()),
        ("architecture", h.architecture.as_str()),
        ("bitness", h.bitness.as_str()),
        ("model", h.model.as_str()),
    ] {
        if let Some(v) = str_field(key)
            && v != want
        {
            return Err(reject(
                M,
                format!("userAgentMetadata.{key} {v:?} differs from the persona's {want:?}"),
            ));
        }
    }
    if let Some(v) = meta.get("wow64").and_then(Value::as_bool)
        && v != h.wow64
    {
        return Err(reject(
            M,
            format!("userAgentMetadata.wow64 {v} differs from the persona"),
        ));
    }
    if let (Some(ff), Some(he)) = (
        meta.get("formFactors").and_then(Value::as_array),
        p.high_entropy_hints(),
    ) {
        let given: Vec<&str> = ff.iter().filter_map(Value::as_str).collect();
        if given != he.form_factors {
            return Err(reject(
                M,
                format!(
                    "userAgentMetadata.formFactors {given:?} differs from the persona's {:?}",
                    he.form_factors
                ),
            ));
        }
    }
    if let Some(m) = meta.get("mobile").and_then(Value::as_bool)
        && m != h.mobile
    {
        return Err(reject(
            M,
            format!("userAgentMetadata.mobile {m} differs from the persona"),
        ));
    }
    if let Some(brands) = meta.get("brands").and_then(Value::as_array) {
        let given: Vec<(String, String)> = brands
            .iter()
            .map(|b| {
                (
                    b.get("brand")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    b.get("version")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                )
            })
            .collect();
        let want: Vec<(String, String)> = h
            .brands
            .iter()
            .map(|b| (b.brand.clone(), b.version.clone()))
            .collect();
        if given != want {
            return Err(reject(
                M,
                "userAgentMetadata.brands differ from the persona's client hints".into(),
            ));
        }
    }
    Ok(())
}

/// A string param as Chrome's protocol bindings read it: `Ok(None)` when
/// absent, `-32602` for another type.
pub fn opt_string<'a>(p: &'a Value, key: &str) -> Result<Option<&'a str>, CdpError> {
    match p.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(CdpError::invalid_params(format!(
            "Failed to deserialize params.{key} - BINDINGS: string value expected"
        ))),
    }
}

/// A mandatory string param (Chrome's bindings errors).
pub fn req_string<'a>(p: &'a Value, key: &str) -> Result<&'a str, CdpError> {
    opt_string(p, key)?.ok_or_else(|| {
        CdpError::invalid_params(format!(
            "Failed to deserialize params.{key} - BINDINGS: mandatory field missing"
        ))
    })
}

/// An optional number param (`null`, a string ... are `-32602`).
pub fn opt_double(p: &Value, key: &str) -> Result<Option<f64>, CdpError> {
    match p.get(key) {
        None => Ok(None),
        Some(v) => v.as_f64().map(Some).ok_or_else(|| {
            CdpError::invalid_params(format!(
                "Failed to deserialize params.{key} - BINDINGS: double value expected"
            ))
        }),
    }
}

/// The locale string as Blink hands it to ICU (`String::Ascii()`: other
/// characters become `?`).
pub fn icu_locale_tag(locale: &str) -> String {
    locale
        .chars()
        .map(|c| if c.is_ascii() { c } else { '?' })
        .collect()
}

/// Whether Blink's `LocaleController` takes `locale` for
/// `Emulation.setLocaleOverride`: ICU's locale id parser finds a language
/// subtag (the text before the first `-`, `_`, `@` or `.`, at most 11
/// characters, its buffer; an `i-` prefix belongs to it, an `x-` prefix
/// leaves none; `root` is ICU's root locale, without one). Otherwise
/// Chrome answers "Invalid locale name". Recorded in local Chrome 153
/// (`tests/emulation/chrome.json`).
pub fn locale_is_valid(locale: &str) -> bool {
    let tag = icu_locale_tag(locale);
    let b = tag.as_bytes();
    let prefix = b.len() >= 2 && matches!(b[1], b'-' | b'_');
    if prefix && b[0] | 0x20 == b'x' {
        return false;
    }
    let skip = if prefix && b[0] | 0x20 == b'i' { 2 } else { 0 };
    let lang_len = tag[skip..]
        .find(['-', '_', '@', '.'])
        .unwrap_or(tag.len() - skip)
        + skip;
    let lang = &tag[..lang_len];
    (1..=11).contains(&lang.len()) && !lang.eq_ignore_ascii_case("root")
}

/// Chrome's `Accept-Language` for an `acceptLanguage` override
/// (`net::HttpUtil::GenerateAcceptLanguageHeader`): the comma-separated
/// entries as given (not trimmed), the first without a q-value, then
/// `;q=0.9`, `;q=0.8` ... down to `;q=0.1`. `de-DE, en ,fr_FR` gives
/// `de-DE, en ;q=0.9,fr_FR;q=0.8` (recorded).
pub fn accept_language_header(list: &str) -> String {
    let entries: Vec<&str> = list.split(',').collect();
    lb_persona::locale::accept_language_header(&entries)
}

/// `navigator.languages` under an `acceptLanguage` override (Blink's
/// `NavigatorLanguage` `ParseAndSanitize`): split at commas, trimmed, `_`
/// at index 2 becomes `-`; q-values stay (recorded:
/// `fr-CH,fr;q=0.9` gives `["fr-CH", "fr;q=0.9"]`).
pub fn navigator_languages(list: &str) -> Vec<String> {
    let mut out: Vec<String> = list
        .split(',')
        .map(|t| {
            let mut t = t.trim().to_owned();
            if t.len() >= 3 && t.as_bytes()[2] == b'_' {
                t.replace_range(2..3, "-");
            }
            t
        })
        .collect();
    if out.is_empty() {
        out.push("en-US".into());
    }
    out
}

/// `Emulation.setGeolocationOverride` params as Chrome reads them: a
/// position when latitude, longitude and accuracy are all given (checked
/// as `device::ValidateGeoposition`: "Invalid geolocation"), else
/// position unavailable. `now_ms` stamps the position.
pub fn geolocation(p: &Value, now_ms: f64) -> Result<GeolocationOverride, CdpError> {
    let lat = opt_double(p, "latitude")?;
    let lng = opt_double(p, "longitude")?;
    let acc = opt_double(p, "accuracy")?;
    let altitude = opt_double(p, "altitude")?;
    let altitude_accuracy = opt_double(p, "altitudeAccuracy")?;
    let heading = opt_double(p, "heading")?;
    let speed = opt_double(p, "speed")?;
    let (Some(latitude), Some(longitude), Some(accuracy)) = (lat, lng, acc) else {
        return Ok(GeolocationOverride::Unavailable);
    };
    if !((-90.0..=90.0).contains(&latitude)
        && (-180.0..=180.0).contains(&longitude)
        && accuracy >= 0.0)
    {
        return Err(CdpError::server("Invalid geolocation"));
    }
    Ok(GeolocationOverride::Position(GeoPosition {
        latitude,
        longitude,
        accuracy,
        altitude,
        altitude_accuracy: altitude_accuracy.filter(|v| *v >= 0.0),
        heading: heading.filter(|v| (0.0..=360.0).contains(v)),
        speed: speed.filter(|v| *v >= 0.0),
        timestamp: now_ms,
    }))
}

/// `Emulation.setEmulatedMedia` params.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct EmulatedMedia {
    #[serde(default)]
    pub media: Option<String>,
    #[serde(default)]
    pub features: Option<Vec<MediaFeature>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct MediaFeature {
    pub name: String,
    pub value: String,
}

/// Media type must be screen; each feature must be empty (no override) or
/// equal what the persona's media environment answers.
pub fn check_emulated_media(p: &PersonaProfile, m: &EmulatedMedia) -> Result<(), CdpError> {
    const M: &str = "Emulation.setEmulatedMedia";
    if let Some(media) = m.media.as_deref()
        && !media.is_empty()
        && media != "screen"
    {
        return Err(reject(
            M,
            format!("media type {media:?} is not supported (screen only)"),
        ));
    }
    let env = MediaEnv::from_persona(p);
    for f in m.features.iter().flatten() {
        if f.value.is_empty() {
            continue;
        }
        let want = match f.name.as_str() {
            "prefers-color-scheme" => {
                if env.prefers_dark {
                    "dark"
                } else {
                    "light"
                }
            }
            "prefers-reduced-motion" => {
                if env.prefers_reduced_motion {
                    "reduce"
                } else {
                    "no-preference"
                }
            }
            "forced-colors" => "none",
            "prefers-contrast" => "no-preference",
            "prefers-reduced-transparency" => "no-preference",
            "prefers-reduced-data" => "no-preference",
            other => {
                return Err(reject(
                    M,
                    format!("media feature {other:?} is not supported"),
                ));
            }
        };
        if f.value != want {
            return Err(reject(
                M,
                format!(
                    "{} {:?} differs from the persona ({want:?})",
                    f.name, f.value
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn persona() -> PersonaProfile {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../profiles/reference/chrome/chrome153_win10_rtx3060_1080p.json"
        );
        PersonaProfile::from_path(path).unwrap()
    }

    fn metrics(v: Value) -> DeviceMetrics {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn device_metrics() {
        let p = persona();
        // Playwright's default 1280x720 context: screen 1280x720 contradicts.
        let e = check_device_metrics(&p, &metrics(json!({"width":1280,"height":720,"screenWidth":1280,"screenHeight":720,"deviceScaleFactor":1,"mobile":false}))).unwrap_err();
        assert!(e.message.contains("screenWidth"), "{}", e.message);
        // Same viewport with the persona screen is fine and applied.
        let v = check_device_metrics(&p, &metrics(json!({"width":1280,"height":720,"screenWidth":1920,"screenHeight":1080,"deviceScaleFactor":1,"mobile":false,"screenOrientation":{"type":"landscapePrimary","angle":0}}))).unwrap();
        assert_eq!(
            v,
            Some(ViewportOverride {
                width: 1280,
                height: 720
            })
        );
        // The persona's own size is accepted and ignored.
        assert_eq!(
            check_device_metrics(
                &p,
                &metrics(json!({"width":1920,"height":937,"deviceScaleFactor":1,"mobile":false}))
            )
            .unwrap(),
            None
        );
        assert!(
            check_device_metrics(
                &p,
                &metrics(json!({"width":0,"height":0,"deviceScaleFactor":2,"mobile":false}))
            )
            .is_err()
        );
        assert!(
            check_device_metrics(
                &p,
                &metrics(json!({"width":400,"height":800,"deviceScaleFactor":0,"mobile":true}))
            )
            .is_err()
        );
        assert!(
            check_device_metrics(
                &p,
                &metrics(json!({"width":3000,"height":800,"deviceScaleFactor":0,"mobile":false}))
            )
            .is_err()
        );
    }

    #[test]
    fn user_agent_locale_timezone_media() {
        let p = persona();
        let ua = |v: Value| -> UserAgentOverride { serde_json::from_value(v).unwrap() };
        check_user_agent(&p, &ua(json!({"userAgent": ""}))).unwrap();
        check_user_agent(
            &p,
            &ua(json!({"userAgent": p.browser.user_agent, "acceptLanguage": "en-US"})),
        )
        .unwrap();
        let e = check_user_agent(&p, &ua(json!({"userAgent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:128.0) Gecko/20100101 Firefox/128.0"}))).unwrap_err();
        assert!(e.message.contains("claims firefox"), "{}", e.message);
        assert!(
            check_user_agent(&p, &ua(json!({"userAgent": "", "platform": "MacIntel"}))).is_err()
        );
        assert!(
            check_user_agent(
                &p,
                &ua(json!({"userAgent": "", "userAgentMetadata": {"platform": "macOS"}}))
            )
            .is_err()
        );
        check_user_agent(&p, &ua(json!({"userAgent": "", "userAgentMetadata": {"platform": "Windows", "mobile": false}}))).unwrap();
        check_user_agent(&p, &ua(json!({"userAgent": "", "userAgentMetadata": {"formFactors": ["Desktop"], "wow64": false}}))).unwrap();
        assert!(
            check_user_agent(
                &p,
                &ua(json!({"userAgent": "", "userAgentMetadata": {"formFactors": ["Mobile"]}}))
            )
            .is_err()
        );
        assert!(
            check_user_agent(
                &p,
                &ua(json!({"userAgent": "", "userAgentMetadata": {"wow64": true}}))
            )
            .is_err()
        );

        // Accept-Language follows a proxy now (ADR 0039).
        check_user_agent(&p, &ua(json!({"userAgent": "", "acceptLanguage": "de-DE"}))).unwrap();

        let media = |v: Value| -> EmulatedMedia { serde_json::from_value(v).unwrap() };
        check_emulated_media(&p, &media(json!({"media": "", "features": [{"name": "prefers-color-scheme", "value": "light"}, {"name": "prefers-reduced-motion", "value": ""}]}))).unwrap();
        assert!(
            check_emulated_media(
                &p,
                &media(json!({"features": [{"name": "prefers-color-scheme", "value": "dark"}]}))
            )
            .is_err()
        );
        assert!(check_emulated_media(&p, &media(json!({"media": "print"}))).is_err());
    }

    // Values recorded in local Chrome 153 (tests/emulation/chrome.json).
    #[test]
    fn chrome_locale_rules() {
        for ok in [
            "xx",
            "xx-YY",
            "de_DE",
            "EN-gb",
            "zh-Hant-TW",
            "iw",
            "en-US-u-ca-buddhist",
            "C",
            "\u{e9}",
            "abcdefghijk",
            "ab cd",
            "i-klingon",
            "x",
            "de@calendar=x",
            "de.UTF-8",
        ] {
            assert!(locale_is_valid(ok), "{ok}");
        }
        for bad in [
            "not a locale",
            "root",
            "ROOT",
            "abcdefghijkl",
            "x-foo",
            "X-foo",
            "@x",
            ".x",
            "-de",
            "_DE",
        ] {
            assert!(!locale_is_valid(bad), "{bad}");
        }
        assert_eq!(accept_language_header("de-DE"), "de-DE");
        assert_eq!(
            accept_language_header("fr-CH,fr;q=0.9,en;q=0.5"),
            "fr-CH,fr;q=0.9;q=0.9,en;q=0.5;q=0.8"
        );
        assert_eq!(
            accept_language_header("de-DE, en ,fr_FR"),
            "de-DE, en ;q=0.9,fr_FR;q=0.8"
        );
        assert_eq!(
            navigator_languages("de-DE, en ,fr_FR"),
            ["de-DE", "en", "fr-FR"]
        );
        assert_eq!(
            navigator_languages("fr-CH,fr;q=0.9,en;q=0.5"),
            ["fr-CH", "fr;q=0.9", "en;q=0.5"]
        );
        let g = |v: Value| geolocation(&v, 5.0);
        assert_eq!(g(json!({})).unwrap(), GeolocationOverride::Unavailable);
        assert_eq!(
            g(json!({"latitude": 1, "longitude": 2})).unwrap(),
            GeolocationOverride::Unavailable
        );
        for bad in [
            json!({"latitude": 91, "longitude": 0, "accuracy": 1}),
            json!({"latitude": 0, "longitude": 181, "accuracy": 1}),
            json!({"latitude": 1, "longitude": 2, "accuracy": -1}),
        ] {
            assert_eq!(g(bad).unwrap_err().message, "Invalid geolocation");
        }
        assert_eq!(g(json!({"latitude": "x"})).unwrap_err().code, -32602);
        let GeolocationOverride::Position(p) = g(json!({"latitude": 90, "longitude": -180,
            "accuracy": 0, "heading": 400, "speed": -1, "altitudeAccuracy": -1}))
        .unwrap() else {
            panic!()
        };
        assert_eq!(
            (p.altitude, p.altitude_accuracy, p.heading, p.speed),
            (None, None, None, None)
        );
    }
}
