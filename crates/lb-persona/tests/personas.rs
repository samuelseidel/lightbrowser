//! Loads the persona files under `profiles/` and checks the consistency rules.

use std::path::{Path, PathBuf};

use lb_persona::consistency::{self, rules};
use lb_persona::sfc32::Sfc32;
use lb_persona::{
    BrandVersion, BrowserFamily, Persona, PersonaError, PersonaProfile, validate_file,
};

fn profiles_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles")
}

fn json_files(dir: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![profiles_dir().join(dir)];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap_or_else(|e| panic!("{}: {e}", d.display())) {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "json") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn chrome_win() -> PersonaProfile {
    PersonaProfile::from_path(
        profiles_dir().join("reference/chrome/chrome153_win10_rtx3060_1080p.json"),
    )
    .unwrap()
}

fn chrome_mac() -> PersonaProfile {
    PersonaProfile::from_path(
        profiles_dir().join("reference/chrome/chrome153_macos15_apple_m2_retina.json"),
    )
    .unwrap()
}

#[test]
fn all_reference_personas_validate() {
    let files = json_files("reference");
    assert!(
        files.len() >= 5,
        "expected firefox and chrome references, found {files:?}"
    );
    for path in files {
        let profile =
            PersonaProfile::from_path(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let result = consistency::validate(&profile);
        assert!(
            result.is_valid(),
            "{}: {:?}",
            path.display(),
            result.violations
        );
        assert!(
            result.warnings.is_empty(),
            "{}: {:?}",
            path.display(),
            result.warnings
        );
        // The strict loader agrees.
        PersonaProfile::from_path_checked(&path).unwrap();
    }
}

#[test]
fn v1_files_load_as_firefox_without_client_hints() {
    for path in json_files("reference/firefox") {
        let p = PersonaProfile::from_path(&path).unwrap();
        assert_eq!(p.schema_version, "1.0");
        assert_eq!(
            p.browser.family,
            BrowserFamily::Firefox,
            "{}",
            path.display()
        );
        assert!(p.client_hints.is_none());
        assert_eq!(p.behavior.profile, "casual_user");
        let net = p.to_network();
        assert_eq!(net.family, BrowserFamily::Firefox);
        assert_eq!(net.sec_ch_ua, None);
        assert_eq!(net.sec_ch_ua_platform, None);
        assert_eq!(net.sec_ch_ua_mobile, None);
        assert!(net.tls_profile_id.starts_with("firefox_128_"));
        assert_eq!(p.performance_now_resolution_us(), 1000);
    }
}

#[test]
fn chrome_win_reference_matches_default_chrome_network_summary() {
    let net = chrome_win().to_network();
    assert_eq!(net, Persona::default_chrome());
}

#[test]
fn chrome_mac_reference_network_summary() {
    let net = chrome_mac().to_network();
    assert_eq!(net.family, BrowserFamily::Chrome);
    assert_eq!(net.sec_ch_ua, Persona::default_chrome().sec_ch_ua);
    assert_eq!(net.sec_ch_ua_platform.as_deref(), Some("macOS"));
    assert_eq!(net.sec_ch_ua_mobile.as_deref(), Some("?0"));
    assert_eq!(net.tls_profile_id, "chrome_153_linux");
    assert!(net.user_agent.contains("Macintosh") && net.user_agent.contains("Chrome/153.0.0.0"));
}

#[test]
fn persona_loaders_validate() {
    let good = profiles_dir().join("reference/chrome/chrome153_win10_rtx3060_1080p.json");
    assert_eq!(
        Persona::from_path(&good).unwrap(),
        Persona::default_chrome()
    );
    let text = std::fs::read_to_string(&good).unwrap();
    assert_eq!(
        Persona::load_json(&text).unwrap(),
        Persona::default_chrome()
    );

    for path in json_files("generated/adversarial") {
        match Persona::from_path(&path) {
            Err(PersonaError::Inconsistent(r)) => assert!(!r.is_valid()),
            other => panic!("{} should be rejected, got {other:?}", path.display()),
        }
    }
}

#[test]
fn unknown_schema_version_is_rejected() {
    let mut v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            profiles_dir().join("reference/chrome/chrome153_win10_rtx3060_1080p.json"),
        )
        .unwrap(),
    )
    .unwrap();
    v["schema_version"] = "3.0".into();
    assert!(matches!(
        PersonaProfile::from_json(&v.to_string()),
        Err(PersonaError::UnsupportedVersion(s)) if s == "3.0"
    ));
    assert!(matches!(
        PersonaProfile::from_json("{}"),
        Err(PersonaError::UnsupportedVersion(_))
    ));
    assert!(matches!(
        PersonaProfile::from_json("nope"),
        Err(PersonaError::Parse(_))
    ));
}

#[test]
fn adversarial_chrome_reports_expected_rules() {
    let p = PersonaProfile::from_path(
        profiles_dir().join("generated/adversarial/chrome153_inconsistent_client_hints_tls.json"),
    )
    .unwrap();
    assert!(p.adversarial);
    let r = consistency::validate(&p);
    assert!(!r.is_valid());
    assert_eq!(
        r.violated_rule_ids(),
        [
            "R03_GPU_VENDOR_RENDERER",
            "R05_FONT_OS",
            "R07_SCREEN_RESOLUTION",
            "R08_HARDWARE_CONCURRENCY",
            "R10_SEEDS_FORMAT",
            "R11_UACH_BRANDS",
            "R12_UACH_PLATFORM",
            "R13_TLS_FAMILY_VERSION",
            "R15_NAVIGATOR_CONSTANTS",
        ]
    );
    assert!(r.has_warning("R16_PERF_NOW_RESOLUTION"));
}

#[test]
fn adversarial_missing_client_hints_reports_r14() {
    let p = PersonaProfile::from_path(
        profiles_dir().join("generated/adversarial/chrome153_missing_client_hints.json"),
    )
    .unwrap();
    let r = consistency::validate(&p);
    assert_eq!(r.violated_rule_ids(), ["R14_CLIENT_HINTS_PRESENCE"]);
}

#[test]
fn every_adversarial_file_fails() {
    for path in json_files("generated/adversarial") {
        let p = PersonaProfile::from_path(&path).unwrap();
        assert!(
            !consistency::validate(&p).is_valid(),
            "{} should fail",
            path.display()
        );
    }
}

// --- individual rules, by mutating the Windows and macOS references ---

fn only(p: &PersonaProfile, id: &str) {
    let r = consistency::validate(p);
    assert_eq!(r.violated_rule_ids(), [id], "{:?}", r.violations);
}

#[test]
fn r11_grease_brand_wrong() {
    let mut p = chrome_win();
    let h = p.client_hints.as_mut().unwrap();
    h.brands[1] = BrandVersion::new("Not A;Brand", "99");
    only(&p, "R11_UACH_BRANDS");
    let r = consistency::validate(&p);
    assert!(
        r.violations[0].description.contains("GREASE"),
        "{}",
        r.violations[0].description
    );
}

#[test]
fn r11_wrong_order_and_version() {
    let mut p = chrome_win();
    p.client_hints.as_mut().unwrap().brands.swap(0, 2);
    only(&p, "R11_UACH_BRANDS");
    assert!(
        consistency::validate(&p).violations[0]
            .description
            .contains("wrong order")
    );

    let mut p = chrome_win();
    p.client_hints.as_mut().unwrap().brands[0].version = "152".into();
    only(&p, "R11_UACH_BRANDS");

    let mut p = chrome_win();
    p.client_hints.as_mut().unwrap().full_version_list[1].version = "8".into();
    only(&p, "R11_UACH_BRANDS");
}

#[test]
fn r11_brands_follow_the_ua_major() {
    // A Chrome 131 persona must carry Chrome 131's brand list, not 153's.
    let mut p = chrome_win();
    p.browser.user_agent = p.browser.user_agent.replace("Chrome/153", "Chrome/131");
    p.browser.app_version = p.browser.app_version.replace("Chrome/153", "Chrome/131");
    p.browser.version = "131.0.6778.86".into();
    p.tls.profile_id = "chrome_131_linux".into();
    let h = p.client_hints.as_mut().unwrap();
    h.brands = lb_persona::uach::chrome_brands(131);
    h.full_version_list = lb_persona::uach::chrome_full_version_list(131, "131.0.6778.86");
    assert!(consistency::validate(&p).is_valid());
    assert_eq!(
        lb_persona::format_brand_list(&p.client_hints.as_ref().unwrap().brands),
        r#""Google Chrome";v="131", "Chromium";v="131", "Not_A Brand";v="24""#
    );
}

#[test]
fn r12_platform_mismatch() {
    let mut p = chrome_win();
    p.client_hints.as_mut().unwrap().platform = "macOS".into();
    only(&p, "R12_UACH_PLATFORM");

    let mut p = chrome_mac();
    p.client_hints.as_mut().unwrap().mobile = true;
    only(&p, "R12_UACH_PLATFORM");
}

#[test]
fn r12_warns_on_arch_and_platform_version() {
    let mut p = chrome_mac();
    p.client_hints.as_mut().unwrap().architecture = "x86".into();
    p.client_hints.as_mut().unwrap().platform_version = "13.0.0".into();
    let r = consistency::validate(&p);
    assert!(r.is_valid());
    assert_eq!(
        r.warnings
            .iter()
            .filter(|w| w.rule_id == "R12_UACH_PLATFORM")
            .count(),
        2
    );
}

#[test]
fn r13_tls_family_and_major() {
    let mut p = chrome_win();
    p.tls.profile_id = "firefox_128_win10".into();
    only(&p, "R13_TLS_FAMILY_VERSION");

    let mut p = chrome_win();
    p.tls.profile_id = "chrome_131_linux".into();
    only(&p, "R13_TLS_FAMILY_VERSION");

    // chrome_15 must not be accepted for Chrome 153.
    let mut p = chrome_win();
    p.tls.profile_id = "chrome_15_linux".into();
    only(&p, "R13_TLS_FAMILY_VERSION");

    // OS suffix is irrelevant for Chrome.
    let mut p = chrome_mac();
    p.tls.profile_id = "chrome_153_win10".into();
    assert!(consistency::validate(&p).is_valid());
}

#[test]
fn r13_version_vs_ua_major_and_r06_for_firefox() {
    let mut p = chrome_win();
    p.browser.version = "152.0.1.2".into();
    let r = consistency::validate(&p);
    assert!(r.has_violation("R13_TLS_FAMILY_VERSION"));

    let mut ff = PersonaProfile::from_path(
        profiles_dir().join("reference/firefox/win10_rtx3060_1080p.json"),
    )
    .unwrap();
    ff.tls.profile_id = "firefox_115_win10".into();
    only(&ff, "R06_TLS_PROFILE");
    ff.tls.profile_id = "chrome_153_linux".into();
    let r = consistency::validate(&ff);
    assert!(r.has_violation("R13_TLS_FAMILY_VERSION") && r.has_violation("R06_TLS_PROFILE"));
}

#[test]
fn r14_presence() {
    let mut p = chrome_win();
    p.client_hints = None;
    only(&p, "R14_CLIENT_HINTS_PRESENCE");

    let mut ff = PersonaProfile::from_path(
        profiles_dir().join("reference/firefox/win10_rtx3060_1080p.json"),
    )
    .unwrap();
    ff.client_hints = chrome_win().client_hints;
    only(&ff, "R14_CLIENT_HINTS_PRESENCE");
}

#[test]
fn r15_navigator_constants() {
    let mut p = chrome_win();
    p.browser.vendor = String::new();
    only(&p, "R15_NAVIGATOR_CONSTANTS");

    let mut p = chrome_win();
    p.browser.oscpu = Some("Windows NT 10.0; Win64; x64".into());
    only(&p, "R15_NAVIGATOR_CONSTANTS");

    let mut p = chrome_win();
    p.browser.app_version = "5.0 (Windows)".into();
    only(&p, "R15_NAVIGATOR_CONSTANTS");
}

#[test]
fn r16_is_a_warning() {
    let mut p = chrome_win();
    p.timing.as_mut().unwrap().performance_now_resolution_us = Some(1000);
    let r = consistency::validate(&p);
    assert!(r.is_valid() && r.has_warning("R16_PERF_NOW_RESOLUTION"));

    p.timing = None;
    assert_eq!(p.performance_now_resolution_us(), 100);
    assert!(consistency::validate(&p).warnings.is_empty());
}

#[test]
fn vendored_rules_still_fire() {
    let mut p = chrome_mac();
    p.hardware.gpu.vendor_unmasked = "Google Inc. (NVIDIA)".into();
    p.hardware.gpu.renderer_unmasked =
        "ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 Direct3D11 vs_5_0 ps_5_0, D3D11)".into();
    let r = consistency::validate(&p);
    assert!(r.has_violation("R02_MACOS_NO_NVIDIA"));

    // Chrome-style vendor/renderer mismatch (R03) is caught.
    let mut p = chrome_win();
    p.hardware.gpu.renderer_unmasked =
        "ANGLE (AMD, AMD Radeon RX 6700 XT Direct3D11 vs_5_0 ps_5_0, D3D11)".into();
    only(&p, "R03_GPU_VENDOR_RENDERER");

    // NVIDIA on Windows needs s3tc (R04).
    let mut p = chrome_win();
    p.webgl
        .extensions
        .retain(|e| e != "WEBGL_compressed_texture_s3tc");
    only(&p, "R04_WEBGL_EXTENSIONS");

    let mut p = chrome_win();
    p.os.platform = "MacIntel".into();
    only(&p, "R01_UA_PLATFORM");

    let mut p = chrome_win();
    p.fingerprint_seeds.webgl_noise_seed = p.fingerprint_seeds.canvas_seed.clone();
    only(&p, "R10_SEEDS_FORMAT");
    p.fingerprint_seeds.webgl_noise_seed = "UPPERCASE_NOT_HEX_XXXX_1234567890".into();
    only(&p, "R10_SEEDS_FORMAT");

    let mut p = chrome_win();
    p.locale.timezone = "Europe/Paris".into();
    let r = consistency::validate(&p);
    assert!(r.is_valid() && r.has_warning("R09_TIMEZONE_LOCALE"));
}

#[test]
fn adversarial_flag_does_not_skip_rules() {
    let mut p = chrome_win();
    p.adversarial = true;
    p.os.platform = "MacIntel".into();
    assert!(!consistency::validate(&p).is_valid());
}

#[test]
fn rule_table_has_unique_sorted_ids() {
    let ids: Vec<_> = rules().iter().map(|r| r.id).collect();
    assert_eq!(ids.len(), 16);
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "{ids:?}");
    assert!(ids[0].starts_with("R01") && ids[15].starts_with("R16"));
}

#[test]
fn seeds_decode_to_sfc32() {
    let p = chrome_win();
    let bytes = p.fingerprint_seeds.canvas_bytes().unwrap();
    assert_eq!(bytes[0], 0x5e);
    assert_eq!(bytes[15], 0x67);
    assert_eq!(Sfc32::new(bytes).next_u32(), Sfc32::new(bytes).next_u32());
    assert!(lb_persona::profile::FingerprintSeeds::bytes("xyz").is_none());
}

#[test]
fn json_round_trip() {
    for p in [chrome_win(), chrome_mac()] {
        let text = serde_json::to_string(&p).unwrap();
        let back = PersonaProfile::from_json(&text).unwrap();
        assert_eq!(p, back);
    }
    let ff = PersonaProfile::from_path(
        profiles_dir().join("reference/firefox/macos13_intel_iris_xe_1080p.json"),
    )
    .unwrap();
    assert_eq!(
        ff,
        PersonaProfile::from_json(&serde_json::to_string(&ff).unwrap()).unwrap()
    );
}

#[test]
fn validate_file_reports() {
    let ok = validate_file(
        profiles_dir().join("reference/chrome/chrome153_macos15_apple_m2_retina.json"),
    );
    assert!(ok.ok, "{}", ok.report);
    assert!(ok.report.contains("OK: 0 warning(s)"));

    let bad = validate_file(
        profiles_dir().join("generated/adversarial/chrome153_inconsistent_client_hints_tls.json"),
    );
    assert!(!bad.ok);
    for id in [
        "R11_UACH_BRANDS",
        "R12_UACH_PLATFORM",
        "R13_TLS_FAMILY_VERSION",
    ] {
        assert!(bad.report.contains(id), "{id} missing in\n{}", bad.report);
    }
    assert!(bad.report.contains("INVALID"));

    let missing = validate_file(profiles_dir().join("nope.json"));
    assert!(!missing.ok && missing.report.contains("ERROR"));
}
