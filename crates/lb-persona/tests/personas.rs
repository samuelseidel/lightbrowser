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
    // Personas live in one folder per family; reference/ also holds
    // tcp_public.json (ADR 0033), which is not a persona.
    let mut files = json_files("reference/chrome");
    files.extend(json_files("reference/firefox"));
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
            "R20_SCREEN_GEOMETRY",
            "R21_FONTS_OS",
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
fn r12_arch_is_a_violation() {
    let mut p = chrome_mac();
    p.client_hints.as_mut().unwrap().architecture = "x86".into();
    only(&p, "R12_UACH_PLATFORM");

    // 32-bit Chrome on 64-bit Windows: bitness stays 64, wow64 true.
    let mut p = chrome_win();
    p.client_hints.as_mut().unwrap().wow64 = true;
    assert!(consistency::validate(&p).is_valid());
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
    assert_eq!(ids.len(), 26);
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "{ids:?}");
    assert!(ids[0].starts_with("R01") && ids[25].starts_with("R26"));
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

// --- M7: OS personas (ADR 0015) ---

fn reference(name: &str) -> PersonaProfile {
    PersonaProfile::from_path(profiles_dir().join("reference/chrome").join(name)).unwrap()
}

fn ubuntu() -> PersonaProfile {
    reference("chrome153_ubuntu24_intel_uhd630_x11_1080p.json")
}

fn fedora() -> PersonaProfile {
    reference("chrome153_fedora42_amd_rx6600_wayland_1440p.json")
}

fn win11() -> PersonaProfile {
    reference("chrome153_win11_rtx4060_1080p.json")
}

fn mac_m4() -> PersonaProfile {
    reference("chrome153_macos26_apple_m4_1512x982.json")
}

#[test]
fn reference_chrome_personas_cover_every_os() {
    use lb_persona::{DisplayServer, OsKind};
    let all: Vec<PersonaProfile> = json_files("reference/chrome")
        .iter()
        .map(|p| PersonaProfile::from_path(p).unwrap())
        .collect();
    assert_eq!(all.len(), 8);
    let count = |k: OsKind| all.iter().filter(|p| p.os_kind() == k).count();
    assert_eq!(count(OsKind::Windows), 3);
    assert_eq!(count(OsKind::MacOs), 3);
    assert_eq!(count(OsKind::Linux), 2);
    assert!(
        all.iter()
            .any(|p| p.os.architecture == "x86_64" && p.os_kind() == OsKind::MacOs)
    );
    assert!(
        all.iter()
            .any(|p| p.display_server() == Some(DisplayServer::Wayland))
    );
    assert!(
        all.iter()
            .any(|p| p.display_server() == Some(DisplayServer::X11))
    );
}

/// Each OS adversarial persona breaks exactly one rule.
#[test]
fn os_adversarial_personas_trip_exactly_their_rule() {
    let cases = [
        ("chrome153_linux_with_d3d11.json", "R19_GPU_BACKEND"),
        ("chrome153_mac_arm_intel_gpu.json", "R18_ARCH_GPU"),
        ("chrome153_win11_fonts_pv10.json", "R17_PLATFORM_VERSION"),
        ("chrome153_linux_segoe_fonts.json", "R21_FONTS_OS"),
        ("chrome153_wayland_avail_top.json", "R20_SCREEN_GEOMETRY"),
        ("chrome153_mac_scrollbar17.json", "R24_PREFERENCES_OS"),
        (
            "chrome153_linux_mesa_kernel_mismatch.json",
            "R19_GPU_BACKEND",
        ),
        ("chrome153_mac_microsoft_voices.json", "R22_VOICES_OS"),
        ("chrome153_win_metal_renderer.json", "R19_GPU_BACKEND"),
        ("chrome153_intel_mac_macos27.json", "R26_MACOS_VERSION"),
        ("chrome153_devicememory12.json", "R25_HARDWARE"),
    ];
    for (file, rule) in cases {
        let p = PersonaProfile::from_path(profiles_dir().join("generated/adversarial").join(file))
            .unwrap();
        assert!(p.adversarial, "{file}");
        let r = consistency::validate(&p);
        assert_eq!(r.violated_rule_ids(), [rule], "{file}: {:?}", r.violations);
        assert!(r.warnings.is_empty(), "{file}: {:?}", r.warnings);
    }
}

#[test]
fn generated_personas_validate() {
    for os in ["windows", "macos", "linux"] {
        let files = json_files(&format!("generated/{os}"));
        assert!(files.len() >= 5, "{os}: {files:?}");
        for path in files {
            let p = PersonaProfile::from_path_checked(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_eq!(p.os_kind().as_str(), os);
            assert!(!p.adversarial);
            assert_eq!(p.provenance.as_ref().map(|v| v.captured), Some(false));
        }
    }
}

#[test]
fn generator_is_consistent_and_deterministic() {
    use lb_persona::OsKind;
    use lb_persona::generate::{Catalog, generate};
    let cat = Catalog::load(&profiles_dir().join("catalog")).unwrap();
    for os in [OsKind::Windows, OsKind::MacOs, OsKind::Linux] {
        // generate() refuses any persona that breaks a rule.
        let a = generate(&cat, os, 50, 42).unwrap();
        assert_eq!(a.len(), 50);
        for p in &a {
            assert_eq!(p.os_kind(), os);
            assert!(consistency::validate(p).is_valid());
        }
        let b = generate(&cat, os, 5, 42).unwrap();
        assert_eq!(&a[..5], &b[..]);
        let c = generate(&cat, os, 5, 43).unwrap();
        assert_ne!(a[0].profile_id, c[0].profile_id);
    }
    // The committed files are what the example writes for seed 1.
    let ours = generate(&cat, OsKind::Linux, 5, 1).unwrap();
    let file = PersonaProfile::from_path(
        profiles_dir().join("generated/linux/chrome153_linux_s1_000.json"),
    )
    .unwrap();
    assert_eq!(ours[0], file);
}

/// Every installed font of every persona and catalog set has a metric table
/// (`profiles/fonts/<os>/`) or a derived entry, so width probes detect it.
#[test]
fn every_persona_font_has_a_table() {
    use std::collections::HashSet;
    let fonts = profiles_dir().join("fonts");
    let mut have: HashSet<(String, String)> = HashSet::new();
    for os in ["windows", "macos", "linux"] {
        for entry in std::fs::read_dir(fonts.join(os)).unwrap() {
            let path = entry.unwrap().path();
            let t: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            have.insert((
                os.into(),
                t["family"].as_str().unwrap().to_ascii_lowercase(),
            ));
        }
    }
    let derived: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fonts.join("derived.json")).unwrap())
            .unwrap();
    for e in derived["families"].as_array().unwrap() {
        have.insert((
            e["os"].as_str().unwrap().into(),
            e["family"].as_str().unwrap().to_ascii_lowercase(),
        ));
    }
    let mut files = json_files("reference/chrome");
    for os in ["windows", "macos", "linux"] {
        files.extend(json_files(&format!("generated/{os}")));
    }
    for path in files {
        let p = PersonaProfile::from_path(&path).unwrap();
        let os = p.os_kind().as_str();
        for f in &p.fonts.available {
            assert!(
                have.contains(&(os.into(), f.to_ascii_lowercase())),
                "{}: no {os} table for {f}",
                path.display()
            );
        }
    }
    let cat: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(profiles_dir().join("catalog/fonts.json")).unwrap(),
    )
    .unwrap();
    for (set, list) in cat["sets"].as_object().unwrap() {
        let os = if set.starts_with("windows") {
            "windows"
        } else if set.starts_with("mac") {
            "macos"
        } else {
            "linux"
        };
        for f in list.as_array().unwrap() {
            let f = f.as_str().unwrap();
            assert!(
                have.contains(&(os.into(), f.to_ascii_lowercase())),
                "{set}: {f}"
            );
        }
    }
}

/// The schema declares every field the persona files use
/// (`additionalProperties: false` everywhere).
#[test]
fn schema_declares_every_persona_field() {
    fn walk(schema: &serde_json::Value, v: &serde_json::Value, at: &str, file: &str) {
        let schema = schema
            .get("oneOf")
            .and_then(|o| o.as_array())
            .and_then(|o| o.iter().find(|s| s["type"] == "object"))
            .unwrap_or(schema);
        match v {
            serde_json::Value::Object(map) => {
                let Some(props) = schema.get("properties").and_then(|p| p.as_object()) else {
                    return;
                };
                for (k, child) in map {
                    let sub = props
                        .get(k)
                        .unwrap_or_else(|| panic!("{file}: {at}.{k} is not in the schema"));
                    walk(sub, child, &format!("{at}.{k}"), file);
                }
            }
            serde_json::Value::Array(items) => {
                if let Some(item) = schema.get("items") {
                    for (i, x) in items.iter().enumerate() {
                        walk(item, x, &format!("{at}[{i}]"), file);
                    }
                }
            }
            _ => {}
        }
    }
    let schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(profiles_dir().join("schemas/persona_v2.schema.json")).unwrap(),
    )
    .unwrap();
    let mut files = json_files("reference/chrome");
    files.extend(json_files("generated"));
    for path in files {
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        walk(&schema, &v, "", &path.display().to_string());
    }
}

#[test]
fn os_accessors_have_os_defaults() {
    use lb_persona::{DisplayServer, OsKind};
    let w = chrome_win();
    assert_eq!(w.os_kind(), OsKind::Windows);
    assert_eq!(w.display_server(), None);
    assert_eq!(w.windows_major(), Some(10));
    let d = w.font_defaults();
    assert_eq!(
        (
            d.sans_serif.as_str(),
            d.monospace.as_str(),
            d.system_ui.as_str()
        ),
        ("Arial", "Consolas", "Segoe UI")
    );
    assert_eq!(w.window_position(), (0, 0));
    let wheel = w.wheel();
    assert_eq!((wheel.px_per_notch, wheel.wheel_delta_per_px), (100.0, 1.2));
    let a = w.audio_defaults();
    assert_eq!(
        (a.sample_rate, a.base_latency, a.output_latency, a.channels),
        (48_000, 0.01, 0.02, 2)
    );
    let g = w.webgpu().unwrap();
    assert_eq!(
        (g.vendor.as_str(), g.architecture.as_str()),
        ("nvidia", "ampere")
    );
    let c = w.codecs();
    assert!(c.hevc && c.h264_power_efficient && c.av1_hw);
    assert!(w.os_apis().share && w.os_apis().bluetooth);
    let h = w.high_entropy_hints().unwrap();
    assert_eq!(h.platform_version, "10.0.0");
    assert_eq!(h.form_factors, ["Desktop"]);
    // Blink's alternates: Helvetica falls back to Arial on Windows.
    assert!(
        w.font_aliases()
            .contains(&("Helvetica".into(), "Arial".into()))
    );

    let m = chrome_mac();
    assert_eq!(m.macos_version(), Some((15, 0, 0)));
    assert_eq!(m.apple_chip().as_deref(), Some("M2"));
    assert_eq!(m.font_defaults().system_ui, "system-ui");
    assert_eq!(m.wheel().wheel_delta_per_px, 3.0);
    assert!(!m.codecs().av1_hw);
    assert_eq!(m.webgpu().unwrap().architecture, "metal-3");

    let u = ubuntu();
    assert_eq!(u.display_server(), Some(DisplayServer::X11));
    assert_eq!(u.linux_distro().map(|d| d.1), Some("24.04".into()));
    let d = u.font_defaults();
    assert_eq!(
        (
            d.system_ui.as_str(),
            d.monospace.as_str(),
            d.cursive.as_str(),
            d.system_font_px
        ),
        ("Ubuntu Sans", "DejaVu Sans Mono", "Times New Roman", 16.0)
    );
    let aliases = u.font_aliases();
    assert!(aliases.contains(&("Arial".into(), "Liberation Sans".into())));
    assert!(aliases.contains(&("Courier New".into(), "Liberation Mono".into())));
    assert!(
        !aliases.iter().any(|(a, _)| a == "Calibri"),
        "no Carlito installed"
    );
    assert_eq!(u.window_position(), (64, 32));
    assert_eq!(u.wheel().px_per_notch, 120.0);
    assert_eq!(u.webgpu(), None);
    assert!(!u.os_apis().share && !u.os_apis().bluetooth);
    assert!(!u.codecs().hevc);
    assert_eq!(u.high_entropy_hints().unwrap().platform_version, "");

    let f = fedora();
    assert_eq!(f.display_server(), Some(DisplayServer::Wayland));
    assert_eq!(f.window_position(), (0, 0));
    assert_eq!(f.font_defaults().system_ui, "Adwaita Sans");
    assert_eq!(f.os.kernel.as_deref(), Some("6.14.5-300.fc42.x86_64"));

    // Absent fields take the OS default; `webgpu: null` means no adapter.
    let mut u2 = ubuntu();
    u2.webgpu = None;
    u2.media = None;
    u2.apis = None;
    u2.behavior.wheel = None;
    assert_eq!(u2.webgpu(), None);
    assert!(!u2.os_apis().share);
    assert_eq!(u2.wheel().wheel_delta_per_px, 1.0);
    let mut w2 = chrome_win();
    w2.webgpu = Some(None);
    assert_eq!(w2.webgpu(), None);
}

#[test]
fn os_fields_round_trip() {
    for p in [ubuntu(), fedora(), win11(), mac_m4()] {
        let text = serde_json::to_string(&p).unwrap();
        assert_eq!(PersonaProfile::from_json(&text).unwrap(), p);
    }
    // `webgpu: null` survives a round trip.
    let text = serde_json::to_string(&ubuntu()).unwrap();
    assert!(text.contains("\"webgpu\":null"), "{text}");
}

#[test]
fn chrome_tls_profile_is_os_independent() {
    // R06 is Firefox-only: every Chrome persona uses chrome_153_linux.
    for path in json_files("reference/chrome") {
        let p = PersonaProfile::from_path(&path).unwrap();
        assert_eq!(p.tls.profile_id, "chrome_153_linux");
        assert!(!consistency::validate(&p).has_violation("R06_TLS_PROFILE"));
    }
}

#[test]
fn r17_platform_version_per_os() {
    let mut p = chrome_win();
    p.client_hints.as_mut().unwrap().platform_version = "15.0.0".into();
    only(&p, "R17_PLATFORM_VERSION");
    let mut p = win11();
    p.os.build = "10.0.22631".into(); // 23H2 reports 15.0.0
    only(&p, "R17_PLATFORM_VERSION");
    let mut p = win11();
    p.os.build = "10.0.19045".into(); // a Windows 10 build
    only(&p, "R17_PLATFORM_VERSION");
    let mut p = mac_m4();
    p.client_hints.as_mut().unwrap().platform_version = "26.1".into();
    only(&p, "R17_PLATFORM_VERSION");
    let mut p = mac_m4();
    p.client_hints.as_mut().unwrap().platform_version = "26.0.0".into();
    only(&p, "R17_PLATFORM_VERSION");
    let mut p = ubuntu();
    p.client_hints.as_mut().unwrap().platform_version = "6.8.0".into();
    only(&p, "R17_PLATFORM_VERSION");
}

#[test]
fn r18_wow64_and_apple_gpu() {
    let mut p = chrome_mac();
    p.client_hints.as_mut().unwrap().wow64 = true;
    only(&p, "R18_ARCH_GPU");
    let mut p = chrome_mac();
    p.os.architecture = "x86_64".into();
    p.client_hints.as_mut().unwrap().architecture = "x86".into();
    only(&p, "R18_ARCH_GPU");
}

#[test]
fn r19_wrapper_and_limits() {
    let mut p = ubuntu();
    p.hardware.gpu.vendor_unmasked = "Google Inc. (Intel Open Source Technology Center)".into();
    only(&p, "R19_GPU_BACKEND");
    let mut p = chrome_win();
    p.webgl.parameters.insert(
        "ALIASED_LINE_WIDTH_RANGE".into(),
        serde_json::json!([1, 7.99]),
    );
    only(&p, "R19_GPU_BACKEND");
    let mut p = chrome_mac();
    p.hardware.gpu.renderer_unmasked = "ANGLE (Apple, Apple M2, OpenGL 4.1 Metal - 88.1)".into();
    assert!(consistency::validate(&p).has_violation("R19_GPU_BACKEND"));
}

#[test]
fn r20_screen_per_os() {
    let mut p = chrome_win();
    p.screen.avail_height = 1000; // an 80 px taskbar
    p.screen.outer_height = 1000;
    p.screen.inner_height = 897;
    only(&p, "R20_SCREEN_GEOMETRY");
    let mut p = chrome_mac();
    p.screen.avail_top = 0;
    only(&p, "R20_SCREEN_GEOMETRY");
    let mut p = fedora();
    p.screen.window_x = Some(10);
    only(&p, "R20_SCREEN_GEOMETRY");
    let mut p = chrome_win();
    p.screen.outer_width = 1936; // dw 16 needs a restored window
    p.screen.avail_width = 1920;
    only(&p, "R20_SCREEN_GEOMETRY");
}

#[test]
fn r21_fonts_per_release_and_defaults() {
    let mut p = chrome_win();
    p.fonts.available.push("Segoe UI Variable Text".into());
    only(&p, "R21_FONTS_OS");
    let mut p = win11();
    p.fonts
        .available
        .retain(|f| !f.starts_with("Segoe UI Variable"));
    only(&p, "R21_FONTS_OS");
    let mut p = ubuntu();
    p.fonts.available.retain(|f| !f.starts_with("Ubuntu"));
    only(&p, "R21_FONTS_OS"); // Ubuntu fonts missing, and the system-ui default
    let mut p = chrome_mac();
    p.fonts.available.push("DejaVu Sans".into());
    only(&p, "R21_FONTS_OS");
    let mut p = chrome_win();
    p.fonts.defaults = Some(lb_persona::profile::FontDefaultsInfo {
        monospace: Some("Fira Code".into()),
        ..Default::default()
    });
    only(&p, "R21_FONTS_OS");
}

#[test]
fn r22_r24_r25_r26() {
    let mut p = ubuntu();
    p.speech_voices = chrome_mac().speech_voices;
    only(&p, "R22_VOICES_OS");

    let mut p = chrome_win();
    p.speech_voices.retain(|v| !v.name.starts_with("Microsoft"));
    only(&p, "R22_VOICES_OS");

    let mut p = chrome_mac();
    p.preferences.as_mut().unwrap().forced_colors = true;
    only(&p, "R24_PREFERENCES_OS");
    let mut p = chrome_mac();
    p.preferences.as_mut().unwrap().color_gamut = "srgb".into();
    only(&p, "R24_PREFERENCES_OS"); // a 30-bit screen is wide gamut

    let mut p = mac_m4();
    p.hardware.hardware_concurrency = 8;
    only(&p, "R25_HARDWARE");
    let mut p = chrome_win();
    p.hardware.device_memory = 16.0;
    assert!(consistency::validate(&p).is_valid());
    p.hardware.hardware_concurrency = 14;
    assert!(
        consistency::validate(&p).is_valid(),
        "14 threads is a real CPU"
    );

    let mut p = mac_m4();
    p.os.version = "14.7.1".into();
    p.client_hints.as_mut().unwrap().platform_version = "14.7.1".into();
    only(&p, "R26_MACOS_VERSION");
}

#[test]
fn r23_audio_is_a_warning() {
    let mut p = chrome_win();
    p.audio.as_mut().unwrap().sample_rate = Some(96_000);
    let r = consistency::validate(&p);
    assert!(r.is_valid() && r.has_warning("R23_AUDIO_OS"));
}
