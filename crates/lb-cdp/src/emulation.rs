//! Persona checks for `Emulation.*` and `Network.setUserAgentOverride`.
//!
//! A value that matches the persona is accepted (and needs no change). A
//! value that contradicts it is rejected with an error that says which
//! persona field it contradicts. A viewport smaller than the persona's
//! screen is allowed (windows can be resized) and is passed to the host.

use lb_css::MediaEnv;
use lb_persona::{BrowserFamily, PersonaProfile};
use serde::Deserialize;
use serde_json::Value;

use crate::host::ViewportOverride;
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

fn first_language(list: &str) -> &str {
    list.split(',')
        .next()
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
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
    if let Some(al) = o.accept_language.as_deref().filter(|s| !s.is_empty()) {
        check_language(M, p, first_language(al))?;
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

fn check_language(method: &str, p: &PersonaProfile, tag: &str) -> Result<(), CdpError> {
    if !tag.eq_ignore_ascii_case(&p.locale.language) {
        return Err(reject(
            method,
            format!(
                "locale {tag:?} differs from the persona's {:?}",
                p.locale.language
            ),
        ));
    }
    Ok(())
}

/// `Emulation.setLocaleOverride`: empty clears, otherwise must be the
/// persona's language.
pub fn check_locale(p: &PersonaProfile, locale: Option<&str>) -> Result<(), CdpError> {
    match locale.filter(|l| !l.is_empty()) {
        None => Ok(()),
        Some(l) => check_language("Emulation.setLocaleOverride", p, l),
    }
}

/// `Emulation.setTimezoneOverride`: empty clears, otherwise must be the
/// persona's IANA zone.
pub fn check_timezone(p: &PersonaProfile, tz: &str) -> Result<(), CdpError> {
    if tz.is_empty() || tz == p.locale.timezone {
        return Ok(());
    }
    Err(reject(
        "Emulation.setTimezoneOverride",
        format!(
            "timezone {tz:?} differs from the persona's {:?}",
            p.locale.timezone
        ),
    ))
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
            check_user_agent(&p, &ua(json!({"userAgent": "", "acceptLanguage": "de-DE"}))).is_err()
        );
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

        check_locale(&p, Some("en-US")).unwrap();
        check_locale(&p, Some("")).unwrap();
        assert!(check_locale(&p, Some("fr-FR")).is_err());
        check_timezone(&p, "America/New_York").unwrap();
        assert!(check_timezone(&p, "Europe/Berlin").is_err());

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
}
