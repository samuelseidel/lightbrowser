//! Consistency rules for personas.
//!
//! R01-R10 are vendored from stealth-browser's `consistency/src/lib.rs`
//! (same ids and messages). R11-R16 are new and cover Chrome and the v2
//! fields. Differences from the vendored engine:
//!
//! - Rules always run. The `adversarial` flag only documents that a file is
//!   meant to be invalid; it no longer makes the engine skip the checks.
//! - R03/R04 understand Chrome's `Google Inc. (NVIDIA)` style unmasked vendor
//!   as well as Firefox's `NVIDIA Corporation`.
//! - R06 applies to Firefox personas only; Chrome is covered by R13.

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

const RULES: [Rule; 16] = [
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

fn r03_gpu_vendor_renderer(p: &PersonaProfile, res: &mut ValidationResult) {
    let vendor = &p.hardware.gpu.vendor_unmasked;
    let renderer = &p.hardware.gpu.renderer_unmasked;

    let mismatch = if is_nvidia(vendor) {
        !renderer.starts_with("NVIDIA") && !renderer.contains("ANGLE (NVIDIA")
    } else if is_amd(vendor) {
        !renderer.contains("AMD") && !renderer.contains("Radeon") && !renderer.contains("ATI")
    } else if is_intel(vendor) {
        !renderer.contains("Intel")
    } else if is_apple(vendor) {
        !renderer.contains("Apple")
    } else {
        false
    };

    if mismatch {
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

    let valid_counts: &[u32] = &[1, 2, 4, 6, 8, 10, 12, 16, 20, 24, 28, 32, 64];
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

    let renderer = &p.hardware.gpu.renderer_unmasked;
    if (renderer.contains("Apple M1") || renderer.contains("Apple M2")) && hc != 8 {
        res.warn(
            ID,
            "Apple M1/M2 chips have 8 cores (4 performance + 4 efficiency); hardware_concurrency should be 8",
            "hardware.hardware_concurrency",
            &hc.to_string(),
            format!("Expected 8 for Apple Silicon, got {hc}"),
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
/// architecture and platform version sanity.
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
    if let Some((arch, bits)) = expected_arch
        && (h.architecture != arch || h.bitness != bits)
    {
        res.warn(
            ID,
            "client_hints architecture/bitness differ from os.architecture",
            "client_hints.architecture",
            &format!("{} / {}-bit", h.architecture, h.bitness),
            format!(
                "os.architecture '{}' implies {arch} / {bits}-bit",
                p.os.architecture
            ),
        );
    }

    // Platform version: Windows 11 reports 13+ (Windows 10 reports 10), macOS
    // reports the real macOS major.
    let hint_major = leading_number(&h.platform_version);
    let os_major = leading_number(&p.os.version);
    match (p.os.name.as_str(), hint_major, os_major) {
        ("Windows", Some(hm), Some(om)) if (om >= 11) != (hm >= 13) => res.warn(
            ID,
            "client_hints.platform_version disagrees with os.version (Windows 11 reports 13.0.0 or higher, Windows 10 reports 10.0.0)",
            "client_hints.platform_version",
            &h.platform_version,
            format!("os.version is '{}'", p.os.version),
        ),
        ("macOS", Some(hm), Some(om)) if hm != om => res.warn(
            ID,
            "client_hints.platform_version major differs from os.version",
            "client_hints.platform_version",
            &h.platform_version,
            format!("os.version is '{}'", p.os.version),
        ),
        _ => {}
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
