//! Consistency rules for personas.
//!
//! R01-R10 are vendored from stealth-browser's `consistency/src/lib.rs`
//! (same ids and messages). R11-R16 are new and cover Chrome and the v2
//! fields. R17-R26 (M7, ADR 0015) check a Chrome persona against its OS:
//! UA-CH platform version, architecture and GPU, GPU backend and limits,
//! screen geometry, fonts, voices, audio, preferences, hardware and macOS
//! versions. They skip Firefox personas (M4b). Differences from the
//! vendored engine:
//!
//! - Rules always run. The `adversarial` flag only documents that a file is
//!   meant to be invalid; it no longer makes the engine skip the checks.
//! - R03/R04 understand Chrome's `Google Inc. (NVIDIA)` style unmasked vendor
//!   as well as Firefox's `NVIDIA Corporation`.
//! - R06 applies to Firefox personas only; Chrome is covered by R13.
//! - R08 accepts the thread counts of current CPUs (11, 14, 18, 22); the
//!   Apple chip check moved to R25.
//! - R12's architecture check is a violation (was a warning); its platform
//!   version check moved to R17.

use crate::os::{
    DisplayServer, Distro, OsKind, apple_chip, apple_chip_min_macos, apple_chip_threads, version3,
    windows_build_number, windows_build_platform_major,
};
use crate::profile::{BrandVersion, PersonaProfile};
use crate::uach::{chrome_brands, chrome_full_version_list, grease_brand, ua_major};
use crate::{BrowserFamily, format_brand_list};

/// A rule broken by a persona. The persona must not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub rule_id: &'static str,
    pub description: String,
    pub field_a: String,
    pub value_a: String,
    /// Empty when only one field is involved.
    pub field_b: String,
    pub value_b: String,
}

/// A suspicious but possible value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub rule_id: &'static str,
    pub description: String,
    pub field: String,
    pub value: String,
    pub detail: String,
}

/// What [`validate`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationResult {
    pub violations: Vec<Violation>,
    pub warnings: Vec<Warning>,
}

impl ValidationResult {
    /// True when no rule was violated (warnings are allowed).
    pub fn is_valid(&self) -> bool {
        self.violations.is_empty()
    }

    /// True if any violation has this rule id.
    pub fn has_violation(&self, rule_id: &str) -> bool {
        self.violations.iter().any(|v| v.rule_id == rule_id)
    }

    /// True if any warning has this rule id.
    pub fn has_warning(&self, rule_id: &str) -> bool {
        self.warnings.iter().any(|w| w.rule_id == rule_id)
    }

    /// Sorted, de-duplicated ids of all violated rules.
    pub fn violated_rule_ids(&self) -> Vec<&'static str> {
        let mut ids: Vec<_> = self.violations.iter().map(|v| v.rule_id).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    fn violate(
        &mut self,
        rule_id: &'static str,
        description: impl Into<String>,
        a: (&str, &str),
        b: (&str, &str),
    ) {
        self.violations.push(Violation {
            rule_id,
            description: description.into(),
            field_a: a.0.to_owned(),
            value_a: a.1.to_owned(),
            field_b: b.0.to_owned(),
            value_b: b.1.to_owned(),
        });
    }

    fn violate_one(
        &mut self,
        rule_id: &'static str,
        description: impl Into<String>,
        a: (&str, &str),
    ) {
        self.violate(rule_id, description, a, ("", ""));
    }

    fn warn(
        &mut self,
        rule_id: &'static str,
        description: impl Into<String>,
        field: &str,
        value: &str,
        detail: impl Into<String>,
    ) {
        self.warnings.push(Warning {
            rule_id,
            description: description.into(),
            field: field.to_owned(),
            value: value.to_owned(),
            detail: detail.into(),
        });
    }
}

/// One entry of the rule table.
pub struct Rule {
    pub id: &'static str,
    pub description: &'static str,
    check: fn(&PersonaProfile, &mut ValidationResult),
}

/// All rules in the order they run.
pub fn rules() -> &'static [Rule] {
    &RULES
}

const RULES: [Rule; 26] = [
    Rule {
        id: "R01_UA_PLATFORM",
        description: "User-Agent OS token must match navigator.platform",
        check: r01_ua_platform,
    },
    Rule {
        id: "R02_MACOS_NO_NVIDIA",
        description: "macOS profiles must not use NVIDIA GPU (NVIDIA dropped macOS support after 10.14)",
        check: r02_macos_no_nvidia,
    },
    Rule {
        id: "R03_GPU_VENDOR_RENDERER",
        description: "GPU unmasked vendor string must be consistent with GPU unmasked renderer string",
        check: r03_gpu_vendor_renderer,
    },
    Rule {
        id: "R04_WEBGL_EXTENSIONS",
        description: "WebGL extension list must be consistent with GPU vendor and platform",
        check: r04_webgl_extensions,
    },
    Rule {
        id: "R05_FONT_OS",
        description: "Font availability list must be consistent with the declared OS",
        check: r05_font_os,
    },
    Rule {
        id: "R06_TLS_PROFILE",
        description: "TLS profile identifier must be consistent with Firefox version and OS (Firefox personas)",
        check: r06_tls_profile,
    },
    Rule {
        id: "R07_SCREEN_RESOLUTION",
        description: "Screen resolution and pixel ratio must be within realistic bounds",
        check: r07_screen_resolution,
    },
    Rule {
        id: "R08_HARDWARE_CONCURRENCY",
        description: "hardware_concurrency must be a realistic CPU thread count",
        check: r08_hardware_concurrency,
    },
    Rule {
        id: "R09_TIMEZONE_LOCALE",
        description: "Locale language tag and timezone should be geographically consistent (warning only)",
        check: r09_timezone_locale,
    },
    Rule {
        id: "R10_SEEDS_FORMAT",
        description: "fingerprint_seeds must be 32-char lowercase hex strings, all distinct from each other",
        check: r10_seeds_format,
    },
    Rule {
        id: "R11_UACH_BRANDS",
        description: "client_hints brands and GREASE brand must follow Chromium's rule for the Chrome major",
        check: r11_uach_brands,
    },
    Rule {
        id: "R12_UACH_PLATFORM",
        description: "client_hints platform, mobile and architecture must match the User-Agent OS",
        check: r12_uach_platform,
    },
    Rule {
        id: "R13_TLS_FAMILY_VERSION",
        description: "tls.profile_id prefix must match the browser family and the User-Agent major",
        check: r13_tls_family_version,
    },
    Rule {
        id: "R14_CLIENT_HINTS_PRESENCE",
        description: "family=chrome requires client_hints; family=firefox forbids them",
        check: r14_client_hints_presence,
    },
    Rule {
        id: "R15_NAVIGATOR_CONSTANTS",
        description: "navigator constants (name, vendor, productSub, oscpu, appVersion) must match the family",
        check: r15_navigator_constants,
    },
    Rule {
        id: "R16_PERF_NOW_RESOLUTION",
        description: "performance.now resolution should match the family (warning only)",
        check: r16_perf_now_resolution,
    },
    Rule {
        id: "R17_PLATFORM_VERSION",
        description: "client_hints.platform_version must be the value Chrome reports for os.version/os.build (Linux: empty)",
        check: r17_platform_version,
    },
    Rule {
        id: "R18_ARCH_GPU",
        description: "architecture, GPU and wow64 must fit together (Apple GPU iff arm Mac, wow64 only on Windows)",
        check: r18_arch_gpu,
    },
    Rule {
        id: "R19_GPU_BACKEND",
        description: "WebGL renderer backend, vendor wrapper, limits and Mesa kernel must fit the OS",
        check: r19_gpu_backend,
    },
    Rule {
        id: "R20_SCREEN_GEOMETRY",
        description: "screen work area, window sizes and position must fit the OS and display server",
        check: r20_screen_geometry,
    },
    Rule {
        id: "R21_FONTS_OS",
        description: "fonts must fit the OS, Windows release and distro; default fonts must be installed",
        check: r21_fonts_os,
    },
    Rule {
        id: "R22_VOICES_OS",
        description: "speech voices must belong to the OS (Microsoft voices on Windows, Apple voices on macOS)",
        check: r22_voices_os,
    },
    Rule {
        id: "R23_AUDIO_OS",
        description: "audio sample rate and latencies should be usual for the OS (warning only)",
        check: r23_audio_os,
    },
    Rule {
        id: "R24_PREFERENCES_OS",
        description: "preferences must fit the OS (forced colors only on Windows, scrollbar width, deep color gamut)",
        check: r24_preferences_os,
    },
    Rule {
        id: "R25_HARDWARE",
        description: "device_memory must be a value Chrome reports; Apple chips have fixed thread counts",
        check: r25_hardware,
    },
    Rule {
        id: "R26_MACOS_VERSION",
        description: "macOS version must run on the Mac (Intel Macs end at macOS 26; Apple chips have a first version)",
        check: r26_macos_version,
    },
];

/// Runs every rule on `profile`.
pub fn validate(profile: &PersonaProfile) -> ValidationResult {
    let mut result = ValidationResult::default();
    for rule in &RULES {
        (rule.check)(profile, &mut result);
    }
    result
}

/// Parses a persona (v1 or v2) and validates it.
pub fn validate_json(json: &str) -> Result<ValidationResult, crate::PersonaError> {
    Ok(validate(&PersonaProfile::from_json(json)?))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Vendor name with Chrome's `Google Inc. (X)` wrapper removed.
fn gpu_vendor_core(vendor: &str) -> &str {
    vendor
        .strip_prefix("Google Inc. (")
        .and_then(|v| v.strip_suffix(')'))
        .unwrap_or(vendor)
}

fn is_nvidia(vendor: &str) -> bool {
    matches!(gpu_vendor_core(vendor), "NVIDIA Corporation" | "NVIDIA")
}

fn is_amd(vendor: &str) -> bool {
    matches!(gpu_vendor_core(vendor), "AMD" | "ATI Technologies Inc.")
}

fn is_intel(vendor: &str) -> bool {
    matches!(gpu_vendor_core(vendor), "Intel Inc." | "Intel")
}

fn is_apple(vendor: &str) -> bool {
    matches!(gpu_vendor_core(vendor), "Apple" | "Apple Inc.")
}

fn leading_number(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn family_ua_token(family: BrowserFamily) -> &'static str {
    match family {
        BrowserFamily::Chrome => "Chrome/",
        BrowserFamily::Firefox => "Firefox/",
    }
}

fn is_hex32(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// ---------------------------------------------------------------------------
// R01-R10: vendored
// ---------------------------------------------------------------------------

fn r01_ua_platform(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R01_UA_PLATFORM";
    let ua = &p.browser.user_agent;
    let platform = &p.os.platform;
    let fields = ("browser.user_agent", ua.as_str());
    let other = ("os.platform", platform.as_str());

    if ua.contains("Windows") && platform != "Win32" {
        res.violate(
            ID,
            "User-Agent contains 'Windows' but platform is not 'Win32'",
            fields,
            other,
        );
    }
    if ua.contains("Mac OS X") && platform != "MacIntel" && platform != "MacPPC" {
        res.violate(
            ID,
            "User-Agent contains 'Mac OS X' but platform is not 'MacIntel' or 'MacPPC'",
            fields,
            other,
        );
    }
    if ua.contains("Linux") && !platform.contains("Linux") {
        res.violate(
            ID,
            "User-Agent contains 'Linux' but platform does not contain 'Linux'",
            fields,
            other,
        );
    }
}

fn r02_macos_no_nvidia(p: &PersonaProfile, res: &mut ValidationResult) {
    if p.os.name == "macOS" {
        let vendor = &p.hardware.gpu.vendor_unmasked;
        let renderer = &p.hardware.gpu.renderer_unmasked;
        if vendor.contains("NVIDIA") || renderer.contains("NVIDIA") || renderer.contains("GeForce")
        {
            res.violate(
                "R02_MACOS_NO_NVIDIA",
                "macOS cannot have NVIDIA GPU \u{2014} NVIDIA dropped macOS driver support after Mojave (10.14)",
                ("os.name", &p.os.name),
                ("hardware.gpu.vendor_unmasked", vendor),
            );
        }
    }
}

/// R03's test: the vendor names a GPU family the renderer does not.
fn vendor_renderer_mismatch(vendor: &str, renderer: &str) -> bool {
    if is_nvidia(vendor) {
        !renderer.starts_with("NVIDIA") && !renderer.contains("ANGLE (NVIDIA")
    } else if is_amd(vendor) {
        !renderer.contains("AMD") && !renderer.contains("Radeon") && !renderer.contains("ATI")
    } else if is_intel(vendor) {
        !renderer.contains("Intel")
    } else if is_apple(vendor) {
        !renderer.contains("Apple")
    } else {
        false
    }
}

fn r03_gpu_vendor_renderer(p: &PersonaProfile, res: &mut ValidationResult) {
    let vendor = &p.hardware.gpu.vendor_unmasked;
    let renderer = &p.hardware.gpu.renderer_unmasked;

    if vendor_renderer_mismatch(vendor, renderer) {
        res.violate(
            "R03_GPU_VENDOR_RENDERER",
            format!("GPU vendor '{vendor}' is inconsistent with renderer '{renderer}'"),
            ("hardware.gpu.vendor_unmasked", vendor),
            ("hardware.gpu.renderer_unmasked", renderer),
        );
    }
}

fn r04_webgl_extensions(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R04_WEBGL_EXTENSIONS";
    let vendor = &p.hardware.gpu.vendor_unmasked;
    let renderer = &p.hardware.gpu.renderer_unmasked;
    let os = p.os.name.as_str();
    let has = |name: &str| p.webgl.extensions.iter().any(|e| e == name);

    if is_nvidia(vendor) && os == "Windows" && !has("WEBGL_compressed_texture_s3tc") {
        res.violate(
            ID,
            "NVIDIA GPU on Windows must support WEBGL_compressed_texture_s3tc",
            ("hardware.gpu.vendor_unmasked", vendor),
            ("webgl.extensions", "WEBGL_compressed_texture_s3tc missing"),
        );
    }

    // EXT_disjoint_timer_query is disabled on Metal by Apple.
    let is_apple_silicon = is_apple(vendor)
        && (renderer.contains("M1") || renderer.contains("M2") || renderer.contains("M3"));
    if is_apple_silicon && has("EXT_disjoint_timer_query") {
        res.violate(
            ID,
            "EXT_disjoint_timer_query is not available on Apple Silicon Metal backend",
            ("hardware.gpu.renderer_unmasked", renderer),
            ("webgl.extensions", "EXT_disjoint_timer_query present"),
        );
    }

    if is_amd(vendor) && os == "macOS" && !has("WEBGL_compressed_texture_s3tc") {
        res.violate(
            ID,
            "AMD GPU on macOS must support WEBGL_compressed_texture_s3tc",
            ("hardware.gpu.vendor_unmasked", vendor),
            ("webgl.extensions", "WEBGL_compressed_texture_s3tc missing"),
        );
    }
}

fn r05_font_os(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R05_FONT_OS";
    let has = |name: &str| p.fonts.available.iter().any(|f| f == name);
    let os = p.os.name.as_str();

    match os {
        "Windows" => {
            let candidates = [
                "Arial",
                "Calibri",
                "Consolas",
                "Segoe UI",
                "Times New Roman",
            ];
            let present = candidates.iter().filter(|f| has(f)).count();
            if present < 3 {
                res.violate(
                    ID,
                    format!("Windows font list must include at least 3 of [Arial, Calibri, Consolas, Segoe UI, Times New Roman]; found {present}"),
                    ("os.name", os),
                    ("fonts.available", &format!("{present} Windows core fonts present")),
                );
            }
        }
        "macOS" => {
            if has("Segoe UI") {
                res.violate(
                    ID,
                    "macOS font list must NOT include 'Segoe UI' (Windows-only font)",
                    ("os.name", os),
                    ("fonts.available", "Segoe UI present"),
                );
            }
            if !(has("Helvetica Neue")
                || has("-apple-system")
                || has("Helvetica")
                || has("San Francisco"))
            {
                res.violate(
                    ID,
                    "macOS font list must include 'Helvetica Neue', '-apple-system', or 'Helvetica'",
                    ("os.name", os),
                    ("fonts.available", "No macOS system font found"),
                );
            }
        }
        "Linux"
            if !(has("Liberation Sans")
                || has("DejaVu Sans")
                || has("Noto Sans")
                || has("Ubuntu")) =>
        {
            res.violate(
                ID,
                "Linux font list should include Liberation Sans, DejaVu Sans, Noto Sans, or Ubuntu",
                ("os.name", os),
                ("fonts.available", "No common Linux font found"),
            );
        }
        _ => {}
    }
}

/// Firefox only. Chrome's TLS profile is checked by R13.
fn r06_tls_profile(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R06_TLS_PROFILE";
    if p.browser.family != BrowserFamily::Firefox {
        return;
    }
    let tls_id = &p.tls.profile_id;
    let version = &p.browser.version;
    let os = p.os.name.as_str();

    let major = leading_number(version).unwrap_or(0);
    let expected_prefix = format!("firefox_{major}");
    if !tls_id.starts_with(&expected_prefix) {
        res.violate(
            ID,
            format!("TLS profile '{tls_id}' does not match Firefox version '{version}' (expected prefix '{expected_prefix}')"),
            ("browser.version", version),
            ("tls.profile_id", tls_id),
        );
    }

    let os_mismatch = match os {
        "Windows" => tls_id.contains("macos") || tls_id.contains("linux"),
        "macOS" => tls_id.contains("win") || tls_id.contains("linux"),
        "Linux" => tls_id.contains("win") || tls_id.contains("macos"),
        _ => false,
    };
    if os_mismatch {
        res.violate(
            ID,
            format!("TLS profile '{tls_id}' OS suffix conflicts with declared OS '{os}'"),
            ("os.name", os),
            ("tls.profile_id", tls_id),
        );
    }
}

fn r07_screen_resolution(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R07_SCREEN_RESOLUTION";
    let s = &p.screen;

    if s.width < 800 || s.width > 7680 {
        res.violate_one(
            ID,
            format!(
                "Screen width {} is outside realistic range [800, 7680]",
                s.width
            ),
            ("screen.width", &s.width.to_string()),
        );
    }
    if s.height < 600 || s.height > 4320 {
        res.violate_one(
            ID,
            format!(
                "Screen height {} is outside realistic range [600, 4320]",
                s.height
            ),
            ("screen.height", &s.height.to_string()),
        );
    }

    let valid_ratios = [1.0f64, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];
    let ratio = s.device_pixel_ratio;
    if !valid_ratios.iter().any(|&r| (r - ratio).abs() < 0.001) {
        res.violate_one(
            ID,
            format!("devicePixelRatio {ratio} is not a recognized value (valid: 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0)"),
            ("screen.device_pixel_ratio", &ratio.to_string()),
        );
    }

    let pixel_budget = f64::from(s.width) * f64::from(s.height) * ratio * ratio;
    if pixel_budget > 33_177_600.0 {
        res.warn(
            ID,
            "Screen resolution exceeds 8K equivalent pixel budget",
            "screen",
            &format!("{}x{} @ {}x", s.width, s.height, ratio),
            format!("Pixel budget {pixel_budget:.0} > 33177600 (8K threshold)"),
        );
    }
}

fn r08_hardware_concurrency(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R08_HARDWARE_CONCURRENCY";
    let hc = p.hardware.hardware_concurrency;

    let valid_counts: &[u32] = &[
        1, 2, 4, 6, 8, 10, 11, 12, 14, 16, 18, 20, 22, 24, 28, 32, 64,
    ];
    if !valid_counts.contains(&hc) {
        res.violate_one(
            ID,
            format!("hardware_concurrency {hc} is not a realistic thread count (valid: {valid_counts:?})"),
            ("hardware.hardware_concurrency", &hc.to_string()),
        );
    }
    if hc > 32 {
        res.warn(
            ID,
            "hardware_concurrency > 32 is unusual for consumer hardware",
            "hardware.hardware_concurrency",
            &hc.to_string(),
            "Only workstations / server-grade CPUs exceed 32 threads",
        );
    }
}

fn r09_timezone_locale(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R09_TIMEZONE_LOCALE";
    let lang = p.locale.language.as_str();
    let tz = p.locale.timezone.as_str();
    let field = "locale.timezone";

    match lang {
        "en-US" if tz.starts_with("Europe/") => res.warn(
            ID,
            "en-US locale with European timezone is unusual (possible VPN user)",
            field,
            tz,
            format!("en-US locale typically has America/* timezone, found: {tz}"),
        ),
        "en-GB" if tz != "Europe/London" && !tz.starts_with("Europe/") => res.warn(
            ID,
            "en-GB locale should typically use Europe/London timezone",
            field,
            tz,
            format!("en-GB locale expected Europe/London or Europe/*, found: {tz}"),
        ),
        "ja" | "ja-JP" if !tz.contains("Tokyo") && !tz.contains("Asia/") => res.warn(
            ID,
            "Japanese locale should use Asia/* timezone",
            field,
            tz,
            format!("ja locale expected Asia/*, found: {tz}"),
        ),
        "zh-CN" if !tz.contains("Shanghai") && !tz.contains("Asia/") => res.warn(
            ID,
            "zh-CN locale should use Asia/* timezone",
            field,
            tz,
            format!("zh-CN locale expected Asia/*, found: {tz}"),
        ),
        _ => {}
    }
}

fn r10_seeds_format(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R10_SEEDS_FORMAT";
    let seeds = &p.fingerprint_seeds;
    let all = [
        ("canvas_seed", &seeds.canvas_seed),
        ("webgl_noise_seed", &seeds.webgl_noise_seed),
        ("audio_seed", &seeds.audio_seed),
    ];

    for (name, value) in all {
        if !is_hex32(value) {
            res.violate_one(
                ID,
                format!("Seed '{name}' is not a valid 32-character lowercase hex string"),
                (&format!("fingerprint_seeds.{name}"), value),
            );
        }
    }
    for (i, (name_a, a)) in all.iter().enumerate() {
        for (name_b, b) in &all[i + 1..] {
            if a == b {
                res.violate(
                    ID,
                    format!("{name_a} and {name_b} must be different"),
                    (&format!("fingerprint_seeds.{name_a}"), a),
                    (&format!("fingerprint_seeds.{name_b}"), b),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// R11-R16: new
// ---------------------------------------------------------------------------

/// Chrome only. Brands, order, GREASE brand and version for the UA major.
fn r11_uach_brands(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R11_UACH_BRANDS";
    if p.browser.family != BrowserFamily::Chrome {
        return;
    }
    // Missing client_hints is R14's finding.
    let Some(h) = &p.client_hints else { return };
    let Some(major) = ua_major(&p.browser.user_agent, "Chrome/") else {
        res.violate_one(
            ID,
            "User-Agent has no 'Chrome/<major>' token, so the client_hints brands cannot be checked",
            ("browser.user_agent", &p.browser.user_agent),
        );
        return;
    };

    let actual = format_brand_list(&h.brands);
    let expected_list = chrome_brands(major);
    let expected = format_brand_list(&expected_list);
    if h.brands != expected_list {
        let (grease, grease_v) = grease_brand(major);
        let grease_entry = h
            .brands
            .iter()
            .find(|b| b.brand.starts_with("Not") && b.brand.ends_with("Brand"));
        let description = match grease_entry {
            None => format!(
                "client_hints.brands has no GREASE brand; Chrome {major} uses \"{grease}\";v=\"{grease_v}\""
            ),
            Some(g) if g.brand != grease || g.version != grease_v => format!(
                "GREASE brand {:?} v={:?} is wrong for Chrome {major}; Chromium produces \"{grease}\";v=\"{grease_v}\"",
                g.brand, g.version
            ),
            Some(_) => {
                let mut a: Vec<&BrandVersion> = h.brands.iter().collect();
                let mut e: Vec<&BrandVersion> = expected_list.iter().collect();
                a.sort_by(|x, y| x.brand.cmp(&y.brand));
                e.sort_by(|x, y| x.brand.cmp(&y.brand));
                if a == e {
                    format!("client_hints.brands is in the wrong order for Chrome {major}")
                } else {
                    format!(
                        "client_hints.brands does not match Chromium's brand list for Chrome {major}"
                    )
                }
            }
        };
        res.violate(
            ID,
            description,
            ("client_hints.brands", &actual),
            (&format!("expected brands for Chrome {major}"), &expected),
        );
    }

    // Full version list: same brands in the same order, real brands at the
    // browser's full version, GREASE at "<n>.0.0.0".
    let expected_full = chrome_full_version_list(major, &p.browser.version);
    if h.full_version_list != expected_full {
        res.violate(
            ID,
            format!("client_hints.full_version_list does not match Chrome {major} brands with full version {}", p.browser.version),
            ("client_hints.full_version_list", &format_brand_list(&h.full_version_list)),
            ("expected full_version_list", &format_brand_list(&expected_full)),
        );
    }
}

/// The OS named by a Chrome UA string, in `userAgentData.platform` spelling.
fn ua_os(ua: &str) -> Option<&'static str> {
    if ua.contains("Android") {
        Some("Android")
    } else if ua.contains("CrOS") {
        Some("Chrome OS")
    } else if ua.contains("Windows") {
        Some("Windows")
    } else if ua.contains("Macintosh") || ua.contains("Mac OS X") {
        Some("macOS")
    } else if ua.contains("Linux") || ua.contains("X11") {
        Some("Linux")
    } else {
        None
    }
}

/// Chrome only. `userAgentData.platform` ↔ UA OS ↔ os.name; plus mobile,
/// architecture (platform version is R17).
fn r12_uach_platform(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R12_UACH_PLATFORM";
    if p.browser.family != BrowserFamily::Chrome {
        return;
    }
    let Some(h) = &p.client_hints else { return };
    let ua = &p.browser.user_agent;

    match ua_os(ua) {
        Some(os) if os != h.platform => res.violate(
            ID,
            format!(
                "client_hints.platform '{}' does not match the OS in the User-Agent ('{os}')",
                h.platform
            ),
            ("client_hints.platform", &h.platform),
            ("browser.user_agent", ua),
        ),
        None => res.violate_one(
            ID,
            "User-Agent names no known OS",
            ("browser.user_agent", ua),
        ),
        _ => {}
    }
    if h.platform != p.os.name {
        res.violate(
            ID,
            format!(
                "client_hints.platform '{}' does not match os.name '{}'",
                h.platform, p.os.name
            ),
            ("client_hints.platform", &h.platform),
            ("os.name", &p.os.name),
        );
    }
    if h.mobile && !ua.contains("Mobile") {
        res.violate(
            ID,
            "client_hints.mobile is true but the User-Agent is a desktop one",
            ("client_hints.mobile", "true"),
            ("browser.user_agent", ua),
        );
    }
    if !h.mobile && ua.contains("Mobile") {
        res.violate(
            ID,
            "client_hints.mobile is false but the User-Agent says Mobile",
            ("client_hints.mobile", "false"),
            ("browser.user_agent", ua),
        );
    }

    let expected_arch = match p.os.architecture.as_str() {
        "x86_64" => Some(("x86", "64")),
        "x86" => Some(("x86", "32")),
        "arm64" => Some(("arm", "64")),
        _ => None,
    };
    // Bitness is the OS's: 32-bit Chrome on 64-bit Windows still says 64
    // (with wow64).
    if let Some((arch, bits)) = expected_arch
        && (h.architecture != arch || h.bitness != bits)
    {
        res.violate(
            ID,
            format!(
                "client_hints architecture/bitness {} / {}-bit differ from os.architecture '{}' ({arch} / {bits}-bit)",
                h.architecture, h.bitness, p.os.architecture
            ),
            ("client_hints.architecture", &format!("{} / {}", h.architecture, h.bitness)),
            ("os.architecture", &p.os.architecture),
        );
    }
}

/// Both families: TLS profile family prefix, and Chrome's version in it. Also
/// UA major ↔ `browser.version`.
fn r13_tls_family_version(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R13_TLS_FAMILY_VERSION";
    let family = p.browser.family;
    let tls_id = &p.tls.profile_id;
    let ua = &p.browser.user_agent;

    let Some(major) = ua_major(ua, family_ua_token(family)) else {
        res.violate_one(
            ID,
            format!(
                "User-Agent has no '{}<major>' token for family {family}",
                family_ua_token(family)
            ),
            ("browser.user_agent", ua),
        );
        return;
    };

    match leading_number(&p.browser.version) {
        Some(v) if v == major => {}
        _ => res.violate(
            ID,
            format!(
                "browser.version '{}' does not match the User-Agent major {major}",
                p.browser.version
            ),
            ("browser.version", &p.browser.version),
            ("browser.user_agent", ua),
        ),
    }

    let mut parts = tls_id.split('_');
    let tls_family = parts.next().unwrap_or("");
    if tls_family != family.as_str() {
        res.violate(
            ID,
            format!("TLS profile '{tls_id}' belongs to family '{tls_family}' but the persona is {family}"),
            ("browser.family", family.as_str()),
            ("tls.profile_id", tls_id),
        );
        return;
    }
    // Firefox versions and OS suffix are R06's job.
    if family == BrowserFamily::Chrome
        && parts.next().and_then(|m| m.parse::<u32>().ok()) != Some(major)
    {
        res.violate(
            ID,
            format!("TLS profile '{tls_id}' does not match Chrome version '{major}' (expected prefix 'chrome_{major}')"),
            ("browser.user_agent", ua),
            ("tls.profile_id", tls_id),
        );
    }
}

fn r14_client_hints_presence(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R14_CLIENT_HINTS_PRESENCE";
    match (p.browser.family, p.client_hints.is_some()) {
        (BrowserFamily::Chrome, false) => res.violate(
            ID,
            "family=chrome requires client_hints (sec-ch-ua* headers and navigator.userAgentData)",
            ("browser.family", "chrome"),
            ("client_hints", "missing"),
        ),
        (BrowserFamily::Firefox, true) => res.violate(
            ID,
            "family=firefox must not have client_hints (Firefox sends no sec-ch-ua* and has no userAgentData)",
            ("browser.family", "firefox"),
            ("client_hints", "present"),
        ),
        _ => {}
    }
}

fn r15_navigator_constants(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R15_NAVIGATOR_CONSTANTS";
    let b = &p.browser;
    let (name, vendor, product_sub) = match b.family {
        BrowserFamily::Chrome => ("Chrome", "Google Inc.", "20030107"),
        BrowserFamily::Firefox => ("Firefox", "", "20100101"),
    };
    let family = b.family.as_str();
    let mut check = |field: &str, actual: &str, expected: &str| {
        if actual != expected {
            res.violate(
                ID,
                format!("{field} is '{actual}' but a {family} persona has '{expected}'"),
                ("browser.family", family),
                (field, actual),
            );
        }
    };
    check("browser.name", &b.name, name);
    check("browser.vendor", &b.vendor, vendor);
    check("browser.product_sub", &b.product_sub, product_sub);

    if b.family == BrowserFamily::Chrome {
        // Chrome's appVersion is the UA without the leading "Mozilla/".
        let expected = b
            .user_agent
            .strip_prefix("Mozilla/")
            .unwrap_or(&b.user_agent);
        if b.app_version != expected {
            res.violate(
                ID,
                "browser.app_version must equal the User-Agent without the leading 'Mozilla/' in Chrome",
                ("browser.app_version", &b.app_version),
                ("browser.user_agent", &b.user_agent),
            );
        }
        if b.oscpu.is_some() {
            res.violate(
                ID,
                "Chrome has no navigator.oscpu",
                ("browser.family", family),
                ("browser.oscpu", b.oscpu.as_deref().unwrap_or("")),
            );
        }
        if b.build_id.is_some() {
            res.violate(
                ID,
                "Chrome has no navigator.buildID",
                ("browser.family", family),
                ("browser.build_id", b.build_id.as_deref().unwrap_or("")),
            );
        }
    }
}

fn r16_perf_now_resolution(p: &PersonaProfile, res: &mut ValidationResult) {
    let Some(us) = p
        .timing
        .as_ref()
        .and_then(|t| t.performance_now_resolution_us)
    else {
        return;
    };
    let (ok, expected) = match p.browser.family {
        BrowserFamily::Chrome => (us == 100, "100"),
        // 1 ms is the default; 20 us is Firefox with resistFingerprinting off.
        BrowserFamily::Firefox => (us == 1000 || us == 20, "1000 (or 20 with RFP off)"),
    };
    if !ok {
        res.warn(
            "R16_PERF_NOW_RESOLUTION",
            format!(
                "performance.now resolution {us} us is unusual for {}",
                p.browser.family
            ),
            "timing.performance_now_resolution_us",
            &us.to_string(),
            format!("expected {expected} us"),
        );
    }
}

// ---------------------------------------------------------------------------
// R17-R26: OS rules (M7, ADR 0015). Chrome personas only.
// ---------------------------------------------------------------------------

fn is_chrome(p: &PersonaProfile) -> bool {
    p.browser.family == BrowserFamily::Chrome
}

fn has_font(p: &PersonaProfile, family: &str) -> bool {
    p.fonts
        .available
        .iter()
        .any(|f| f.eq_ignore_ascii_case(family))
}

/// A WebGL parameter as numbers (one number or a two-number range).
fn gl_param(p: &PersonaProfile, name: &str) -> Option<Vec<f64>> {
    match p.webgl.parameters.get(name)? {
        serde_json::Value::Number(n) => Some(vec![n.as_f64()?]),
        serde_json::Value::Array(a) => a.iter().map(serde_json::Value::as_f64).collect(),
        _ => None,
    }
}

/// macOS majors Chrome 153 runs on: 12 Monterey to 15 Sequoia, then 26
/// Tahoe and 27.
const MAC_MAJORS: [u32; 6] = [12, 13, 14, 15, 26, 27];

fn r17_platform_version(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R17_PLATFORM_VERSION";
    if !is_chrome(p) {
        return;
    }
    let Some(h) = &p.client_hints else { return };
    let pv = h.platform_version.as_str();
    let field = ("client_hints.platform_version", pv);
    let three = pv.split('.').count() == 3;
    match p.os_kind() {
        OsKind::Linux => {
            if !pv.is_empty() {
                res.violate(
                    ID,
                    format!("Chrome on Linux reports an empty platformVersion, not '{pv}'"),
                    field,
                    ("os.name", &p.os.name),
                );
            }
        }
        OsKind::Windows => {
            let Some((major, minor, patch)) = version3(pv).filter(|_| three) else {
                res.violate_one(
                    ID,
                    format!("Windows platformVersion '{pv}' is not N.0.0"),
                    field,
                );
                return;
            };
            if (minor, patch) != (0, 0) {
                res.violate_one(
                    ID,
                    format!("Windows platformVersion '{pv}' is not N.0.0"),
                    field,
                );
            }
            let release = p.windows_major();
            let build = windows_build_number(&p.os.build);
            match release {
                Some(10) if !(1..=10).contains(&major) => res.violate(
                    ID,
                    format!("Windows 10 reports platformVersion 1.0.0 to 10.0.0, not '{pv}'"),
                    field,
                    ("os.version", &p.os.version),
                ),
                Some(11) if major < 13 => res.violate(
                    ID,
                    format!("Windows 11 reports platformVersion 13.0.0 or higher, not '{pv}'"),
                    field,
                    ("os.version", &p.os.version),
                ),
                Some(10 | 11) => {}
                _ => res.violate_one(
                    ID,
                    "os.version must name Windows 10 or 11 (Chrome 153 runs on nothing older)",
                    ("os.version", &p.os.version),
                ),
            }
            if let Some(b) = build {
                if let Some(expected) = windows_build_platform_major(b)
                    && expected != major
                {
                    res.violate(
                        ID,
                        format!(
                            "Windows build {b} reports platformVersion {expected}.0.0, not '{pv}'"
                        ),
                        field,
                        ("os.build", &p.os.build),
                    );
                }
                if matches!(release, Some(10 | 11)) && (b >= 22000) != (release == Some(11)) {
                    res.violate(
                        ID,
                        format!(
                            "os.build {} is a Windows {} build but os.version is '{}'",
                            p.os.build,
                            if b >= 22000 { 11 } else { 10 },
                            p.os.version
                        ),
                        ("os.build", &p.os.build),
                        ("os.version", &p.os.version),
                    );
                }
            }
        }
        OsKind::MacOs => {
            let Some(v) = version3(pv).filter(|_| three) else {
                res.violate_one(
                    ID,
                    format!("macOS platformVersion '{pv}' is not N.N.N"),
                    field,
                );
                return;
            };
            if !MAC_MAJORS.contains(&v.0) {
                res.violate_one(
                    ID,
                    format!(
                        "macOS {} does not exist or cannot run Chrome 153 (12-15, 26, 27)",
                        v.0
                    ),
                    field,
                );
            }
            if version3(&p.os.version) != Some(v) {
                res.violate(
                    ID,
                    format!(
                        "macOS platformVersion '{pv}' is not os.version '{}'",
                        p.os.version
                    ),
                    field,
                    ("os.version", &p.os.version),
                );
            }
        }
        OsKind::Other => {}
    }
}

fn r18_arch_gpu(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R18_ARCH_GPU";
    if !is_chrome(p) {
        return;
    }
    let r = &p.hardware.gpu.renderer_unmasked;
    let arch = p.os.architecture.as_str();
    let apple_gpu = apple_chip(r).is_some()
        || r.contains("Apple GPU")
        || is_apple(&p.hardware.gpu.vendor_unmasked);
    let gpu = ("hardware.gpu.renderer_unmasked", r.as_str());
    let a = ("os.architecture", arch);
    match p.os_kind() {
        OsKind::MacOs if arch == "arm64" && !apple_gpu => {
            res.violate(ID, "an Apple Silicon Mac (arm64) has an Apple GPU", a, gpu)
        }
        OsKind::MacOs if arch != "arm64" && apple_gpu => {
            res.violate(ID, "an Apple GPU needs an arm64 Mac", a, gpu)
        }
        OsKind::MacOs => {}
        _ if apple_gpu => res.violate(
            ID,
            "Apple GPUs exist only in Macs",
            ("os.name", &p.os.name),
            gpu,
        ),
        OsKind::Windows => {
            let qualcomm = r.contains("Qualcomm") || r.contains("Adreno");
            if (arch == "arm64") != qualcomm {
                res.violate(
                    ID,
                    "Windows on arm64 has a Qualcomm GPU, and only it does",
                    a,
                    gpu,
                );
            }
        }
        _ => {}
    }
    if let Some(h) = &p.client_hints
        && h.wow64
        && !(p.os_kind() == OsKind::Windows && arch == "x86_64")
    {
        res.violate(
            ID,
            "wow64 is only true for 32-bit Chrome on 64-bit Windows",
            ("client_hints.wow64", "true"),
            ("os.name", &p.os.name),
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlBackend {
    D3d11,
    D3d9,
    Metal,
    OpenGl,
    Vulkan,
    Software,
    Unknown,
}

fn gl_backend(r: &str) -> GlBackend {
    if r.contains("SwiftShader") || r.contains("llvmpipe") {
        GlBackend::Software
    } else if r.contains("Direct3D11") || r.contains("D3D11") {
        GlBackend::D3d11
    } else if r.contains("Direct3D9") {
        GlBackend::D3d9
    } else if r.contains("Metal Renderer") {
        GlBackend::Metal
    } else if r.contains("Vulkan") {
        GlBackend::Vulkan
    } else if r.contains("OpenGL") {
        // Includes macOS's old `OpenGL 4.1 Metal - 88.1` (GL on Metal).
        GlBackend::OpenGl
    } else if r.contains("Metal") {
        GlBackend::Metal
    } else {
        GlBackend::Unknown
    }
}

/// The kernel release a radeonsi/Mesa renderer string embeds:
/// `(radeonsi, navi23, LLVM 19.1.7, DRM 3.61, 6.14.5-300.fc42.x86_64)`.
fn mesa_kernel(r: &str) -> Option<&str> {
    let i = r.find("DRM ")?;
    let rest = &r[i..];
    let after = &rest[rest.find(", ")? + 2..];
    let end = after.find(')')?;
    Some(after[..end].trim())
}

fn r19_gpu_backend(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R19_GPU_BACKEND";
    if !is_chrome(p) {
        return;
    }
    let r = p.hardware.gpu.renderer_unmasked.as_str();
    let v = p.hardware.gpu.vendor_unmasked.as_str();
    let gpu = ("hardware.gpu.renderer_unmasked", r);
    let os = ("os.name", p.os.name.as_str());
    let b = gl_backend(r);
    let fits = match p.os_kind() {
        OsKind::Windows => b == GlBackend::D3d11,
        OsKind::MacOs => b == GlBackend::Metal,
        OsKind::Linux => matches!(b, GlBackend::OpenGl | GlBackend::Vulkan),
        OsKind::Other => true,
    };
    if !fits {
        let want = match p.os_kind() {
            OsKind::Windows => "ANGLE Direct3D11",
            OsKind::MacOs => "ANGLE Metal",
            _ => "ANGLE OpenGL or Vulkan",
        };
        res.violate(
            ID,
            format!("Chrome on {} renders WebGL with {want}", p.os.name),
            os,
            gpu,
        );
    }

    // `Google Inc. (X)` + `ANGLE (X, ...)`. A vendor of another GPU family
    // is R03's finding.
    match r
        .strip_prefix("ANGLE (")
        .and_then(|rest| rest.split(',').next())
    {
        Some(_) if vendor_renderer_mismatch(v, r) => {}
        Some(inner) => {
            let expected = format!("Google Inc. ({inner})");
            if v != expected {
                res.violate(
                    ID,
                    format!("unmasked vendor must be '{expected}' for this ANGLE renderer"),
                    ("hardware.gpu.vendor_unmasked", v),
                    gpu,
                );
            }
        }
        None => res.violate_one(
            ID,
            "Chrome's unmasked renderer is 'ANGLE (vendor, ...)'",
            gpu,
        ),
    }

    // Limits that give the backend away.
    let viewport = gl_param(p, "MAX_VIEWPORT_DIMS").and_then(|v| v.first().copied());
    let line = gl_param(p, "ALIASED_LINE_WIDTH_RANGE").and_then(|v| v.last().copied());
    let point = gl_param(p, "ALIASED_POINT_SIZE_RANGE").and_then(|v| v.last().copied());
    let (want_vp, want_line, want_point): (&[f64], Option<f64>, Option<f64>) = match b {
        GlBackend::D3d11 => (&[32767.0], Some(1.0), Some(1024.0)),
        GlBackend::Metal => (&[16384.0], Some(1.0), Some(511.0)),
        GlBackend::OpenGl => (&[16384.0, 32768.0], None, None),
        _ => (&[], None, None),
    };
    let mut bad = Vec::new();
    if let Some(vp) = viewport
        && !want_vp.is_empty()
        && !want_vp.contains(&vp)
    {
        bad.push(format!("MAX_VIEWPORT_DIMS {vp} (want {want_vp:?})"));
    }
    if let (Some(l), Some(w)) = (line, want_line)
        && l != w
    {
        bad.push(format!("line width max {l} (want {w})"));
    }
    if let (Some(pt), Some(w)) = (point, want_point)
        && pt != w
    {
        bad.push(format!("point size max {pt} (want {w})"));
    }
    if !bad.is_empty() {
        res.violate(
            ID,
            format!(
                "WebGL limits do not fit the {b:?} backend: {}",
                bad.join(", ")
            ),
            ("webgl.parameters", &bad.join(", ")),
            gpu,
        );
    }

    // Mesa radeonsi strings carry the kernel release.
    if let Some(k) = mesa_kernel(r) {
        match p.os.kernel.as_deref() {
            Some(os_k) if os_k == k => {}
            other => res.violate(
                ID,
                format!("the renderer names kernel '{k}' but os.kernel is {other:?}"),
                ("os.kernel", other.unwrap_or("")),
                gpu,
            ),
        }
    }
}

fn r20_screen_geometry(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R20_SCREEN_GEOMETRY";
    if !is_chrome(p) {
        return;
    }
    let s = &p.screen;
    let show = |a: u32, b: u32| format!("{a}x{b}");
    let screen = show(s.width, s.height);
    let avail = format!(
        "{} at {},{}",
        show(s.avail_width, s.avail_height),
        s.avail_left,
        s.avail_top
    );
    let fa = ("screen.avail_*", avail.as_str());
    let fs = ("screen.width/height", screen.as_str());
    if s.avail_left + s.avail_width > s.width || s.avail_top + s.avail_height > s.height {
        res.violate(ID, "the work area extends past the screen", fa, fs);
    }
    let outer = show(s.outer_width, s.outer_height);
    let inner = show(s.inner_width, s.inner_height);
    let fo = ("screen.outer_*", outer.as_str());
    if s.outer_width < s.inner_width || s.outer_height < s.inner_height {
        res.violate(
            ID,
            "the window is smaller than its viewport",
            fo,
            ("screen.inner_*", &inner),
        );
    }
    if s.outer_width > s.avail_width || s.outer_height > s.avail_height {
        res.violate(ID, "the window is larger than the work area", fo, fa);
    }
    let wayland = p.display_server() == Some(DisplayServer::Wayland);
    let dw = s.outer_width.saturating_sub(s.inner_width);
    let maximized = s.maximized.unwrap_or(s.outer_width == s.avail_width);
    let dw_ok = dw == 0 || (dw == 16 && p.os_kind() == OsKind::Windows && !maximized);
    if !dw_ok {
        res.violate(
            ID,
            format!(
                "outer width - inner width is {dw}; Chrome has 0 (16 for a restored Windows window)"
            ),
            fo,
            ("screen.inner_*", &inner),
        );
    }
    if s.maximized == Some(true)
        && !wayland
        && (s.outer_width != s.avail_width || s.outer_height != s.avail_height)
    {
        res.violate(ID, "a maximized window fills the work area", fo, fa);
    }
    let band = s.height.saturating_sub(s.avail_height + s.avail_top);
    match p.os_kind() {
        OsKind::Windows => {
            // Taskbar at the bottom: 40 px (Windows 10) or 48 px (Windows 11)
            // at 100%, scaled by the DPR; 0 when it hides.
            let dpr = s.device_pixel_ratio.max(0.5);
            let fits = |px: f64| (f64::from(band) - px / dpr).abs() <= 1.0;
            let ok = s.avail_top == 0
                && s.avail_left == 0
                && s.avail_width == s.width
                && (band == 0 || fits(40.0) || fits(48.0));
            if !ok {
                res.violate(
                    ID,
                    format!("Windows taskbar band {band} px at DPR {dpr}: want 40 or 48 px / DPR at the bottom"),
                    fa,
                    fs,
                );
            }
        }
        OsKind::MacOs => {
            // Menu bar: 24-25 px, 30-39 with a notch.
            if !(23..=25).contains(&s.avail_top) && !(30..=39).contains(&s.avail_top) {
                res.violate(
                    ID,
                    format!(
                        "macOS menu bar {} px: want 23-25 (30-39 with a notch)",
                        s.avail_top
                    ),
                    fa,
                    fs,
                );
            }
            if s.avail_left > 160 {
                res.violate(ID, "a left Dock is at most 160 px wide", fa, fs);
            }
        }
        OsKind::Linux if wayland => {
            if (s.avail_width, s.avail_height, s.avail_top, s.avail_left)
                != (s.width, s.height, 0, 0)
            {
                res.violate(
                    ID,
                    "on Wayland Chrome sees no work area: screen.avail* equals the screen",
                    fa,
                    fs,
                );
            }
            if p.window_position() != (0, 0) {
                let pos = format!("{:?}", p.window_position());
                res.violate_one(
                    ID,
                    "on Wayland Chrome has no global window position: screenX/Y are 0",
                    ("screen.window_x/y", &pos),
                );
            }
        }
        OsKind::Linux => {
            if s.avail_top > 64 || s.avail_left > 128 || band > 128 {
                res.violate(
                    ID,
                    "X11 panels and docks take at most 64 px at the top, 128 px left or bottom",
                    fa,
                    fs,
                );
            }
        }
        OsKind::Other => {}
    }
}

/// Fonts that only one OS ships (the bench's `fonts_os_markers`).
const WINDOWS_MARKERS: &[&str] = &[
    "Segoe UI",
    "Segoe UI Variable Text",
    "Segoe UI Variable Display",
    "Segoe UI Variable Small",
    "Segoe Fluent Icons",
    "Consolas",
    "Segoe UI Emoji",
];
const WINDOWS11_FONTS: &[&str] = &[
    "Segoe UI Variable Text",
    "Segoe UI Variable Display",
    "Segoe UI Variable Small",
    "Segoe Fluent Icons",
];
const MAC_MARKERS: &[&str] = &[
    "Helvetica Neue",
    "Menlo",
    "Apple Color Emoji",
    "Lucida Grande",
    "Geneva",
    "SF Pro",
];
const LINUX_MARKERS: &[&str] = &[
    "DejaVu Sans",
    "DejaVu Sans Mono",
    "DejaVu Serif",
    "Cantarell",
    "Ubuntu",
    "Ubuntu Sans",
    "Ubuntu Mono",
    "Adwaita Sans",
    "Adwaita Mono",
    "Noto Color Emoji",
];

fn r21_fonts_os(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R21_FONTS_OS";
    if !is_chrome(p) {
        return;
    }
    let os = p.os_kind();
    let foreign: Vec<&str> = match os {
        OsKind::Windows => MAC_MARKERS.iter().chain(LINUX_MARKERS).copied().collect(),
        // Segoe UI on macOS is R05's; Office for Mac brings Consolas.
        OsKind::MacOs => LINUX_MARKERS
            .iter()
            .chain(WINDOWS11_FONTS)
            .copied()
            .collect(),
        OsKind::Linux => WINDOWS_MARKERS.iter().chain(MAC_MARKERS).copied().collect(),
        OsKind::Other => Vec::new(),
    };
    let found: Vec<&str> = foreign.into_iter().filter(|f| has_font(p, f)).collect();
    if !found.is_empty() {
        res.violate(
            ID,
            format!(
                "{} persona lists fonts of another OS: {}",
                p.os.name,
                found.join(", ")
            ),
            ("os.name", &p.os.name),
            ("fonts.available", &found.join(", ")),
        );
    }
    if let Some(release) = p.windows_major() {
        let w11: Vec<&str> = WINDOWS11_FONTS
            .iter()
            .copied()
            .filter(|f| has_font(p, f))
            .collect();
        if release >= 11
            && !(has_font(p, "Segoe UI Variable Text") && has_font(p, "Segoe Fluent Icons"))
        {
            res.violate(
                ID,
                "Windows 11 ships Segoe UI Variable and Segoe Fluent Icons",
                ("os.version", &p.os.version),
                ("fonts.available", &w11.join(", ")),
            );
        }
        if release == 10 && !w11.is_empty() {
            res.violate(
                ID,
                format!("Windows 10 has no {}", w11.join(", ")),
                ("os.version", &p.os.version),
                ("fonts.available", &w11.join(", ")),
            );
        }
    }
    match p.linux_distro() {
        Some((Distro::Ubuntu, _)) if !(has_font(p, "Ubuntu") || has_font(p, "Ubuntu Sans")) => res
            .violate(
                ID,
                "Ubuntu ships the Ubuntu font family",
                ("os.distro", p.os.distro.as_deref().unwrap_or("")),
                ("fonts.available", "no Ubuntu / Ubuntu Sans"),
            ),
        Some((Distro::Fedora, _)) if has_font(p, "Ubuntu") || has_font(p, "Ubuntu Sans") => res
            .warn(
                ID,
                "Ubuntu fonts on Fedora are unusual",
                "fonts.available",
                "Ubuntu",
                "Fedora does not install the Ubuntu family",
            ),
        _ => {}
    }

    // Every default font must resolve to an installed one.
    let d = p.font_defaults();
    let aliases = p.font_aliases();
    let installed = |fam: &str| {
        has_font(p, fam)
            || aliases.iter().any(|(a, _)| a.eq_ignore_ascii_case(fam))
            || (os == OsKind::MacOs && fam == "system-ui")
    };
    for (name, fam) in [
        ("standard", &d.standard),
        ("serif", &d.serif),
        ("sans_serif", &d.sans_serif),
        ("monospace", &d.monospace),
        ("cursive", &d.cursive),
        ("fantasy", &d.fantasy),
        ("system_ui", &d.system_ui),
        ("emoji", &d.emoji),
        ("system_font", &d.system_font),
    ] {
        if !installed(fam) {
            res.violate(
                ID,
                format!("default font {name} '{fam}' is not installed"),
                (&format!("fonts.defaults.{name}"), fam),
                ("fonts.available", "missing"),
            );
        }
    }
    let want_ui = match os {
        OsKind::Windows => Some("Segoe UI"),
        OsKind::MacOs => Some("system-ui"),
        _ => None,
    };
    if let Some(w) = want_ui
        && d.system_ui != w
    {
        res.violate(
            ID,
            format!("system-ui on {} is {w}", p.os.name),
            ("fonts.defaults.system_ui", &d.system_ui),
            ("os.name", &p.os.name),
        );
    }
}

/// macOS voice names (`Eddy (English (US))` counts as `Eddy`).
const APPLE_VOICES: &[&str] = &[
    "Albert",
    "Alex",
    "Alice",
    "Alva",
    "Amélie",
    "Amira",
    "Anna",
    "Bad News",
    "Bahh",
    "Bells",
    "Boing",
    "Bubbles",
    "Carmit",
    "Cellos",
    "Damayanti",
    "Daniel",
    "Daria",
    "Eddy",
    "Ellen",
    "Flo",
    "Fred",
    "Good News",
    "Grandma",
    "Grandpa",
    "Ioana",
    "Jacques",
    "Jester",
    "Joana",
    "Junior",
    "Kanya",
    "Karen",
    "Kathy",
    "Kyoko",
    "Lana",
    "Laura",
    "Lekha",
    "Lesya",
    "Linh",
    "Luciana",
    "Majed",
    "Meijia",
    "Melina",
    "Milena",
    "Moira",
    "Montse",
    "Mónica",
    "Monica",
    "Nora",
    "Organ",
    "Paulina",
    "Ralph",
    "Reed",
    "Rishi",
    "Rocko",
    "Samantha",
    "Sandy",
    "Sara",
    "Satu",
    "Shelley",
    "Sinji",
    "Superstar",
    "Tessa",
    "Thomas",
    "Tingting",
    "Trinoids",
    "Tünde",
    "Whisper",
    "Wobble",
    "Xander",
    "Yelda",
    "Yuna",
    "Zarvox",
    "Zosia",
    "Zuzana",
];

fn r22_voices_os(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R22_VOICES_OS";
    if !is_chrome(p) {
        return;
    }
    let os = p.os_kind();
    let voices = &p.speech_voices;
    let name_of = |n: &str| n.split(" (").next().unwrap_or(n).to_owned();
    let microsoft: Vec<&str> = voices
        .iter()
        .map(|v| v.name.as_str())
        .filter(|n| n.starts_with("Microsoft "))
        .collect();
    let apple: Vec<&str> = voices
        .iter()
        .map(|v| v.name.as_str())
        .filter(|n| APPLE_VOICES.contains(&name_of(n).as_str()))
        .collect();
    let f = ("os.name", p.os.name.as_str());
    if os != OsKind::Windows && !microsoft.is_empty() {
        res.violate(
            ID,
            format!(
                "Microsoft voices exist only on Windows: {}",
                microsoft.join(", ")
            ),
            ("speech_voices", &microsoft.join(", ")),
            f,
        );
    }
    if os != OsKind::MacOs && !apple.is_empty() {
        let few: Vec<&str> = apple.iter().take(3).copied().collect();
        res.violate(
            ID,
            format!("Apple voices exist only on macOS: {}", few.join(", ")),
            ("speech_voices", &few.join(", ")),
            f,
        );
    }
    let local_google: Vec<&str> = voices
        .iter()
        .filter(|v| v.name.starts_with("Google ") && v.local_service)
        .map(|v| v.name.as_str())
        .collect();
    if !local_google.is_empty() {
        res.violate_one(
            ID,
            "Google voices are network voices (local_service false)",
            ("speech_voices", &local_google.join(", ")),
        );
    }
    if voices.iter().filter(|v| v.default).count() > 1 {
        res.violate_one(
            ID,
            "at most one voice is the default",
            ("speech_voices", "default"),
        );
    }
    if matches!(os, OsKind::Windows | OsKind::MacOs) {
        let own = if os == OsKind::Windows {
            microsoft.len()
        } else {
            apple.len()
        };
        if voices.is_empty() {
            res.warn(
                ID,
                "Chrome on Windows and macOS lists the OS voices",
                "speech_voices",
                "[]",
                "an empty list reads as voices that never loaded",
            );
        } else if own == 0 {
            res.violate(
                ID,
                format!("{} lists its own local voices", p.os.name),
                ("speech_voices", "no OS voices"),
                f,
            );
        }
    }
}

fn r23_audio_os(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R23_AUDIO_OS";
    if !is_chrome(p) {
        return;
    }
    let a = p.audio_defaults();
    if a.sample_rate != 44_100 && a.sample_rate != 48_000 {
        res.warn(
            ID,
            "audio sample rate is usually 44100 or 48000",
            "audio.sample_rate",
            &a.sample_rate.to_string(),
            "other rates come from pro audio interfaces",
        );
    }
    if p.os_kind() == OsKind::Windows && a.sample_rate != 48_000 {
        res.warn(
            ID,
            "Windows output devices default to 48000 Hz",
            "audio.sample_rate",
            &a.sample_rate.to_string(),
            "",
        );
    }
    if !(0.001..=0.1).contains(&a.base_latency) {
        res.warn(
            ID,
            "baseLatency is usually between 1 and 100 ms",
            "audio.base_latency",
            &a.base_latency.to_string(),
            "",
        );
    }
    if a.output_latency < a.base_latency {
        res.warn(
            ID,
            "outputLatency is at least baseLatency",
            "audio.output_latency",
            &a.output_latency.to_string(),
            format!("baseLatency {}", a.base_latency),
        );
    }
}

fn r24_preferences_os(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R24_PREFERENCES_OS";
    if !is_chrome(p) {
        return;
    }
    let pr = p.preferences();
    let os = p.os_kind();
    if pr.forced_colors && os != OsKind::Windows {
        res.violate(
            ID,
            "forced-colors: active exists only on Windows (high contrast)",
            ("preferences.forced_colors", "true"),
            ("os.name", &p.os.name),
        );
    }
    let sb = p.scrollbar_width();
    let allowed: &[u32] = match os {
        // Windows 11 (Fluent, overlay when idle) is not captured: 0 and
        // 15-17 pass there.
        OsKind::Windows if p.windows_major() >= Some(11) => &[0, 15, 16, 17],
        OsKind::Windows => &[16, 17],
        OsKind::MacOs => &[0, 15],
        OsKind::Linux => &[14, 15, 16],
        OsKind::Other => &[],
    };
    if !allowed.is_empty() && !allowed.contains(&sb) {
        res.violate(
            ID,
            format!(
                "scrollbar width {sb} px on {} (want one of {allowed:?})",
                p.os.name
            ),
            ("preferences.scrollbar_width", &sb.to_string()),
            ("os.name", &p.os.name),
        );
    }
    let s = &p.screen;
    if s.color_depth != s.pixel_depth {
        res.violate(
            ID,
            "colorDepth and pixelDepth are the same in Chrome",
            ("screen.color_depth", &s.color_depth.to_string()),
            ("screen.pixel_depth", &s.pixel_depth.to_string()),
        );
    }
    if s.color_depth == 30 && !matches!(pr.color_gamut.as_str(), "p3" | "rec2020") {
        res.violate(
            ID,
            "a 30-bit display is a wide-gamut one (color-gamut p3 or rec2020)",
            ("screen.color_depth", "30"),
            ("preferences.color_gamut", &pr.color_gamut),
        );
    }
}

/// `navigator.deviceMemory` values Chrome 153 reports (power of two;
/// 16 seen on a 15 GB machine, so no cap at 8).
const DEVICE_MEMORY: [f64; 8] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];

fn r25_hardware(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R25_HARDWARE";
    if !is_chrome(p) {
        return;
    }
    let dm = p.hardware.device_memory;
    if !DEVICE_MEMORY.contains(&dm) {
        res.violate_one(
            ID,
            format!("deviceMemory {dm} is not a value Chrome reports {DEVICE_MEMORY:?}"),
            ("hardware.device_memory", &dm.to_string()),
        );
    }
    let hc = p.hardware.hardware_concurrency;
    if let Some(chip) = p.apple_chip() {
        match apple_chip_threads(&chip) {
            Some(t) if !t.contains(&hc) => res.violate(
                ID,
                format!("Apple {chip} has {t:?} threads, not {hc}"),
                ("hardware.hardware_concurrency", &hc.to_string()),
                (
                    "hardware.gpu.renderer_unmasked",
                    &p.hardware.gpu.renderer_unmasked,
                ),
            ),
            Some(_) => {}
            None => res.warn(
                ID,
                format!("unknown Apple chip '{chip}'"),
                "hardware.gpu.renderer_unmasked",
                &p.hardware.gpu.renderer_unmasked,
                "thread count not checked",
            ),
        }
    }
}

fn r26_macos_version(p: &PersonaProfile, res: &mut ValidationResult) {
    const ID: &str = "R26_MACOS_VERSION";
    if !is_chrome(p) || p.os_kind() != OsKind::MacOs {
        return;
    }
    let Some((major, _, _)) = p.macos_version() else {
        res.violate_one(
            ID,
            "os.version must be N.N or N.N.N",
            ("os.version", &p.os.version),
        );
        return;
    };
    if p.os.architecture != "arm64" && major > 26 {
        res.violate(
            ID,
            format!("macOS {major} does not run on Intel Macs (26 Tahoe is the last)"),
            ("os.version", &p.os.version),
            ("os.architecture", &p.os.architecture),
        );
    }
    if let Some(chip) = p.apple_chip()
        && let Some(min) = apple_chip_min_macos(&chip)
        && major < min
    {
        res.violate(
            ID,
            format!("Apple {chip} Macs shipped with macOS {min} or later"),
            ("os.version", &p.os.version),
            (
                "hardware.gpu.renderer_unmasked",
                &p.hardware.gpu.renderer_unmasked,
            ),
        );
    }
}

// ---------------------------------------------------------------------------
// Report (for `lightbrowser validate`)
// ---------------------------------------------------------------------------

/// Renders a validation result as plain text, one finding per block.
pub fn render_report(profile: &PersonaProfile, result: &ValidationResult) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} ({}, schema {}, {})",
        profile.profile_name, profile.browser.family, profile.schema_version, profile.profile_id
    );
    for v in &result.violations {
        let _ = writeln!(out, "  [VIOLATION] [{}] {}", v.rule_id, v.description);
        if !v.field_a.is_empty() {
            let _ = writeln!(out, "    -> {} = {}", v.field_a, v.value_a);
        }
        if !v.field_b.is_empty() {
            let _ = writeln!(out, "    -> {} = {}", v.field_b, v.value_b);
        }
    }
    for w in &result.warnings {
        let _ = writeln!(out, "  [WARNING] [{}] {}", w.rule_id, w.description);
        let _ = writeln!(out, "    -> {} = {}", w.field, w.value);
        if !w.detail.is_empty() {
            let _ = writeln!(out, "       {}", w.detail);
        }
    }
    if result.is_valid() {
        let _ = writeln!(out, "OK: {} warning(s)", result.warnings.len());
    } else {
        let _ = writeln!(
            out,
            "INVALID: {} violation(s), {} warning(s)",
            result.violations.len(),
            result.warnings.len()
        );
    }
    out
}

/// The result of [`validate_file`]: text to print and whether to exit 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidateOutcome {
    /// True when the file parsed and had no violations.
    pub ok: bool,
    /// Human-readable report (also used for load errors).
    pub report: String,
}

/// Backs `lightbrowser validate <profile.json>`: load a v1 or v2 persona,
/// run every rule and return the report. The binary prints `report` and exits
/// 0 when `ok`, else 1.
pub fn validate_file(path: impl AsRef<std::path::Path>) -> ValidateOutcome {
    let path = path.as_ref();
    match PersonaProfile::from_path(path) {
        Ok(profile) => {
            let result = validate(&profile);
            ValidateOutcome {
                ok: result.is_valid(),
                report: format!("{}\n{}", path.display(), render_report(&profile, &result)),
            }
        }
        Err(e) => ValidateOutcome {
            ok: false,
            report: format!("{}\nERROR: {e}\n", path.display()),
        },
    }
}
