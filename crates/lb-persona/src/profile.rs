//! The full persona profile: serde types for `persona_v2.schema.json`, plus a
//! loader that also accepts GeckoPhantom `persona_v1` files.
//!
//! [`Persona`] stays the small network-facing summary (headers and TLS
//! profile id). [`PersonaProfile`] is the whole thing: navigator, screen, GPU,
//! fonts, seeds and so on. Contexts should hold an `Arc<PersonaProfile>` and
//! hand out [`PersonaProfile::to_network`] where only the summary is needed.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::consistency::{self, ValidationResult};
use crate::os::WebGpuAdapter;
use crate::{BrowserFamily, Persona};

/// Schema version written by v2 files.
pub const SCHEMA_V2: &str = "2.0";

/// Why a persona could not be loaded.
#[derive(Debug)]
pub enum PersonaError {
    /// The file could not be read.
    Io(std::io::Error),
    /// The JSON did not match the schema's shape.
    Parse(serde_json::Error),
    /// `schema_version` is neither `1.x` nor `2.x`.
    UnsupportedVersion(String),
    /// The profile parsed but broke one or more consistency rules.
    Inconsistent(ValidationResult),
}

impl fmt::Display for PersonaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "cannot read persona: {e}"),
            Self::Parse(e) => write!(f, "invalid persona JSON: {e}"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported persona schema_version {v:?}"),
            Self::Inconsistent(r) => {
                write!(f, "persona is inconsistent: ")?;
                for (i, v) in r.violations.iter().enumerate() {
                    if i > 0 {
                        f.write_str("; ")?;
                    }
                    write!(f, "[{}] {}", v.rule_id, v.description)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for PersonaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Parse(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for PersonaError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for PersonaError {
    fn from(e: serde_json::Error) -> Self {
        Self::Parse(e)
    }
}

/// A complete persona (schema v1 or v2). Field names match the JSON.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct PersonaProfile {
    pub schema_version: String,
    pub profile_id: String,
    pub profile_name: String,
    pub created_at: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Marks a file that is meant to be invalid (negative tests). Rules still
    /// run on it; the flag only documents intent.
    #[serde(default)]
    pub adversarial: bool,
    pub os: OsInfo,
    pub browser: BrowserInfo,
    /// User-Agent Client Hints. Chrome only (rule R14).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_hints: Option<ClientHints>,
    pub hardware: HardwareInfo,
    pub screen: ScreenInfo,
    pub locale: LocaleInfo,
    pub network: NetworkInfo,
    pub fingerprint_seeds: FingerprintSeeds,
    pub webgl: WebGlInfo,
    pub fonts: FontInfo,
    pub tls: TlsInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<TimingInfo>,
    #[serde(default)]
    pub behavior: BehaviorInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub speech_voices: Vec<SpeechVoice>,
    /// `navigator.permissions.query` results by permission name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub permissions: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_devices: Option<MediaDevices>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery: Option<BatteryInfo>,
    /// User preferences behind media queries (`prefers-color-scheme`, ...)
    /// and the scrollbar width. Optional; defaults are a light desktop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferences: Option<Preferences>,
    /// WebGPU adapter. Absent: derived from the GPU on Windows and macOS,
    /// none on Linux ([`PersonaProfile::webgpu`]). `null`: no adapter.
    #[serde(
        default,
        deserialize_with = "explicit_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub webgpu: Option<Option<WebGpuAdapter>>,
    /// Hardware video decode answers ([`PersonaProfile::codecs`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<MediaInfo>,
    /// OS-gated APIs ([`PersonaProfile::os_apis`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apis: Option<ApisInfo>,
    /// Where the values come from (captured or reference). Documentation
    /// only; no layer reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

/// Keeps an explicit JSON `null` apart from an absent field.
fn explicit_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// `media`: codec answers. Absent fields take the OS default.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct MediaInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hevc: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h264_power_efficient: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub av1_hw: Option<bool>,
}

/// `apis`: OS-gated APIs. Absent fields take the OS default.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct ApisInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bluetooth: Option<bool>,
}

/// `provenance`: whether the persona's OS values were captured from a real
/// device or built from the surface reference.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct Provenance {
    pub captured: bool,
    /// What the values come from.
    pub source: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct OsInfo {
    /// `Windows`, `macOS` or `Linux`.
    pub name: String,
    pub version: String,
    pub build: String,
    /// `navigator.platform`.
    pub platform: String,
    /// `x86_64`, `arm64` or `x86`.
    pub architecture: String,
    /// Linux: kernel release (`uname -r`). Mesa renderer strings embed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    /// Linux: distro id and version, e.g. `ubuntu-24.04`, `fedora-42`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distro: Option<String>,
    /// Linux: `x11` (default) or `wayland`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_server: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct BrowserInfo {
    /// Absent in v1 files, which are Firefox.
    #[serde(default = "default_family")]
    pub family: BrowserFamily,
    pub name: String,
    pub version: String,
    /// Firefox only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    pub user_agent: String,
    pub accept_language: String,
    #[serde(default)]
    pub do_not_track: Option<String>,
    pub cookie_enabled: bool,
    pub java_enabled: bool,
    pub pdf_viewer_enabled: bool,
    pub product: String,
    pub product_sub: String,
    pub vendor: String,
    pub vendor_sub: String,
    /// Firefox only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oscpu: Option<String>,
    pub app_name: String,
    pub app_code_name: String,
    pub app_version: String,
}

fn default_family() -> BrowserFamily {
    BrowserFamily::Firefox
}

/// One `{brand, version}` entry of a UA-CH brand list.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct BrandVersion {
    pub brand: String,
    pub version: String,
}

impl BrandVersion {
    pub fn new(brand: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            brand: brand.into(),
            version: version.into(),
        }
    }
}

/// User-Agent Client Hints (`navigator.userAgentData`, `sec-ch-ua*`).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ClientHints {
    /// `sec-ch-ua` / `userAgentData.brands`, in header order, major versions.
    pub brands: Vec<BrandVersion>,
    /// Same brands and order with full versions.
    pub full_version_list: Vec<BrandVersion>,
    /// `Windows`, `macOS`, `Linux`, ... (no quotes).
    pub platform: String,
    pub platform_version: String,
    /// `x86` or `arm`.
    pub architecture: String,
    /// `64` or `32`.
    pub bitness: String,
    pub model: String,
    pub mobile: bool,
    #[serde(default)]
    pub wow64: bool,
    /// `formFactors` / `Sec-CH-UA-Form-Factors`. Absent: `["Desktop"]`
    /// (`["Mobile"]` when `mobile`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form_factors: Option<Vec<String>>,
    /// Deprecated `uaFullVersion`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_version: Option<String>,
}

impl ClientHints {
    /// The `sec-ch-ua` header value, e.g.
    /// `"Google Chrome";v="153", "Not_A Brand";v="8", "Chromium";v="153"`.
    pub fn sec_ch_ua(&self) -> String {
        format_brand_list(&self.brands)
    }

    /// The `sec-ch-ua-full-version-list` header value.
    pub fn sec_ch_ua_full_version_list(&self) -> String {
        format_brand_list(&self.full_version_list)
    }
}

/// Formats a brand list as a structured-header list (`"Brand";v="1", ...`).
pub fn format_brand_list(list: &[BrandVersion]) -> String {
    let mut out = String::new();
    for (i, b) in list.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&format!("\"{}\";v=\"{}\"", b.brand, b.version));
    }
    out
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct HardwareInfo {
    pub hardware_concurrency: u32,
    pub device_memory: f64,
    pub max_touch_points: u32,
    pub gpu: GpuInfo,
    /// `performance.memory.jsHeapSizeLimit` in bytes (Chrome only).
    /// Optional; see [`HardwareInfo::js_heap_size_limit`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub js_heap_size_limit: Option<u64>,
}

impl HardwareInfo {
    /// `performance.memory.jsHeapSizeLimit`: the persona's value, else
    /// what Chrome 153 reports on a 64-bit desktop (4,395,630,592, seen on
    /// the local Chromium 153 on Linux).
    pub fn js_heap_size_limit(&self) -> u64 {
        self.js_heap_size_limit.unwrap_or(4_395_630_592)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct GpuInfo {
    /// Masked WebGL `VENDOR`.
    pub vendor: String,
    /// Masked WebGL `RENDERER`.
    pub renderer: String,
    /// `UNMASKED_VENDOR_WEBGL`.
    pub vendor_unmasked: String,
    /// `UNMASKED_RENDERER_WEBGL`.
    pub renderer_unmasked: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ScreenInfo {
    pub width: u32,
    pub height: u32,
    pub avail_width: u32,
    pub avail_height: u32,
    pub color_depth: u32,
    pub pixel_depth: u32,
    pub device_pixel_ratio: f64,
    pub inner_width: u32,
    pub inner_height: u32,
    pub outer_width: u32,
    pub outer_height: u32,
    #[serde(default)]
    pub avail_top: u32,
    #[serde(default)]
    pub avail_left: u32,
    /// `screenX`. Absent: see [`PersonaProfile::window_position`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_x: Option<i32>,
    /// `screenY`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_y: Option<i32>,
    /// The window is maximized (outer size equals the work area).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximized: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct LocaleInfo {
    pub language: String,
    pub languages: Vec<String>,
    /// IANA timezone id.
    pub timezone: String,
    /// `getTimezoneOffset()` in minutes.
    #[serde(default)]
    pub timezone_offset: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct NetworkInfo {
    pub connection_type: String,
    pub downlink: f64,
    pub rtt: u32,
    pub save_data: bool,
}

/// 32-char hex seeds. Decode with [`FingerprintSeeds::bytes`] for `Sfc32::new`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct FingerprintSeeds {
    pub canvas_seed: String,
    pub webgl_noise_seed: String,
    pub audio_seed: String,
}

impl FingerprintSeeds {
    /// Decodes a seed string into the 16 bytes `Sfc32::new` takes. `None` if
    /// it is not 32 hex digits.
    pub fn bytes(seed: &str) -> Option<[u8; 16]> {
        if seed.len() != 32 || !seed.is_ascii() {
            return None;
        }
        let mut out = [0u8; 16];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&seed[2 * i..2 * i + 2], 16).ok()?;
        }
        Some(out)
    }

    pub fn canvas_bytes(&self) -> Option<[u8; 16]> {
        Self::bytes(&self.canvas_seed)
    }

    pub fn webgl_bytes(&self) -> Option<[u8; 16]> {
        Self::bytes(&self.webgl_noise_seed)
    }

    pub fn audio_bytes(&self) -> Option<[u8; 16]> {
        Self::bytes(&self.audio_seed)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct WebGlInfo {
    pub version: String,
    pub shading_language_version: String,
    pub extensions: Vec<String>,
    /// `getParameter` results by constant name (numbers or number arrays).
    pub parameters: BTreeMap<String, serde_json::Value>,
    /// `getShaderPrecisionFormat` results keyed
    /// `VERTEX_SHADER.HIGH_FLOAT` and so on.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub shader_precision: BTreeMap<String, ShaderPrecision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct ShaderPrecision {
    pub range_min: i32,
    pub range_max: i32,
    pub precision: i32,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct FontInfo {
    pub available: Vec<String>,
    /// Overrides of the OS default fonts
    /// ([`PersonaProfile::font_defaults`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<FontDefaultsInfo>,
}

/// `fonts.defaults`: every field optional; absent ones take the OS default.
#[derive(Debug, Clone, PartialEq, Default, Deserialize, Serialize)]
pub struct FontDefaultsInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standard: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serif: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sans_serif: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monospace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursive: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fantasy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_ui: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_font: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_font_px: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct TlsInfo {
    /// Id of a file in `profiles/tls/`.
    pub profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ja3: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ja3s_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct TimingInfo {
    /// Granularity of `performance.now()` in microseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub performance_now_resolution_us: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct BehaviorInfo {
    /// lb-behavior preset name: `casual_user`, `power_user`, `slow_reader`
    /// or `mobile_like`.
    #[serde(default = "default_behavior")]
    pub profile: String,
    /// Seed of the generated input (`LightBrowser.human*`): up to 16 hex
    /// digits. Absent: derived from `profile_id` and the fingerprint seeds,
    /// so a persona always produces the same input streams.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    /// `mouse` (notched wheel) or `trackpad` (pixel scrolling with
    /// momentum). Absent: `trackpad` on macOS, `mouse` elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointing_device: Option<String>,
    /// Wheel model ([`PersonaProfile::wheel`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wheel: Option<WheelInfo>,
}

/// `behavior.wheel`: absent fields take the OS default.
#[derive(Debug, Clone, PartialEq, Default, Deserialize, Serialize)]
pub struct WheelInfo {
    /// CSS px scrolled per notch (`deltaY` of one notch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub px_per_notch: Option<f64>,
    /// `-wheelDeltaY / deltaY`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_ratio: Option<f64>,
}

impl Default for BehaviorInfo {
    fn default() -> Self {
        Self {
            profile: default_behavior(),
            seed: None,
            pointing_device: None,
            wheel: None,
        }
    }
}

fn default_behavior() -> String {
    "casual_user".into()
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AudioInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_latency: Option<f64>,
    /// `AudioContext.outputLatency` in seconds. Absent: twice
    /// `base_latency`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_latency: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_channel_count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SpeechVoice {
    pub name: String,
    pub lang: String,
    #[serde(default = "yes")]
    pub local_service: bool,
    #[serde(default)]
    pub default: bool,
}

fn yes() -> bool {
    true
}

/// Media devices. Labels are only exposed once permission is granted;
/// counts default to one device per kind that has a label field.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct MediaDevices {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webcam_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker_label: Option<String>,
    /// Number of microphones. Default: 1 if `mic_label` is set, else 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audioinput: Option<u32>,
    /// Number of cameras. Default: 1 if `webcam_label` is set, else 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub videoinput: Option<u32>,
    /// Number of speakers. Default: 1 if `speaker_label` is set, else 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audiooutput: Option<u32>,
}

impl MediaDevices {
    /// Device counts as `(audioinput, videoinput, audiooutput)`.
    pub fn counts(&self) -> (u32, u32, u32) {
        let n = |c: Option<u32>, l: &Option<String>| c.unwrap_or(u32::from(l.is_some()));
        (
            n(self.audioinput, &self.mic_label),
            n(self.videoinput, &self.webcam_label),
            n(self.audiooutput, &self.speaker_label),
        )
    }
}

/// `navigator.getBattery()` values.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct BatteryInfo {
    pub level: f64,
    pub charging: bool,
    /// Seconds until full; `None` is `Infinity` (0 when full and charging).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charging_time: Option<f64>,
    /// Seconds until empty; `None` is `Infinity`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discharging_time: Option<f64>,
}

impl BatteryInfo {
    /// What Chrome reports on a desktop without a battery: charging, full,
    /// `chargingTime` 0, `dischargingTime` Infinity.
    pub fn desktop() -> BatteryInfo {
        BatteryInfo {
            level: 1.0,
            charging: true,
            charging_time: Some(0.0),
            discharging_time: None,
        }
    }
}

/// User preferences seen through media queries, plus the classic
/// scrollbar width. Every field has a default, so `{}` is a light desktop.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Preferences {
    /// `prefers-color-scheme`: `light` or `dark`.
    #[serde(default = "pref_light")]
    pub color_scheme: String,
    /// `prefers-reduced-motion: reduce`.
    #[serde(default)]
    pub reduced_motion: bool,
    /// `prefers-reduced-transparency: reduce`.
    #[serde(default)]
    pub reduced_transparency: bool,
    /// `prefers-contrast`: `no-preference`, `more`, `less` or `custom`.
    #[serde(default = "pref_no_preference")]
    pub contrast: String,
    /// `forced-colors: active` (Windows high contrast).
    #[serde(default)]
    pub forced_colors: bool,
    /// `color-gamut`: `srgb`, `p3` or `rec2020` (the widest it matches).
    #[serde(default = "pref_srgb")]
    pub color_gamut: String,
    /// `dynamic-range: high`.
    #[serde(default)]
    pub high_dynamic_range: bool,
    /// Width of classic (non-overlay) scrollbars in CSS px. Default: 17 on
    /// Windows, 15 on Linux, 0 on macOS (overlay scrollbars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollbar_width: Option<u32>,
}

impl Default for Preferences {
    fn default() -> Self {
        Preferences {
            color_scheme: pref_light(),
            reduced_motion: false,
            reduced_transparency: false,
            contrast: pref_no_preference(),
            forced_colors: false,
            color_gamut: pref_srgb(),
            high_dynamic_range: false,
            scrollbar_width: None,
        }
    }
}

fn pref_light() -> String {
    "light".into()
}
fn pref_no_preference() -> String {
    "no-preference".into()
}
fn pref_srgb() -> String {
    "srgb".into()
}

impl Preferences {
    /// True for `prefers-color-scheme: dark`.
    pub fn dark(&self) -> bool {
        self.color_scheme.eq_ignore_ascii_case("dark")
    }
}

impl PersonaProfile {
    /// Parses a v1 or v2 persona. Does not run the consistency rules; use
    /// [`PersonaProfile::from_json_checked`] for that.
    ///
    /// v1 files (GeckoPhantom Firefox 128) have no `browser.family` and no
    /// `client_hints`; they load as `family = firefox`. A v1 file that claims
    /// `client_hints` is still returned as-is, and rule R14 flags it.
    pub fn from_json(json: &str) -> Result<Self, PersonaError> {
        // Look at the version first so an unknown schema gives a clear error
        // rather than a confusing missing-field one.
        let raw: serde_json::Value = serde_json::from_str(json)?;
        let version = raw
            .get("schema_version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        if !(version.starts_with("1.") || version.starts_with("2.")) {
            return Err(PersonaError::UnsupportedVersion(version));
        }
        let mut profile: PersonaProfile = serde_json::from_value(raw)?;
        if version.starts_with("1.") {
            // v1 predates the family field; it is always the Firefox shape.
            profile.browser.family = BrowserFamily::Firefox;
        }
        Ok(profile)
    }

    /// Reads and parses a persona file.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, PersonaError> {
        Self::from_json(&std::fs::read_to_string(path)?)
    }

    /// Parses and runs the consistency rules. Violations become
    /// [`PersonaError::Inconsistent`]. Warnings are dropped; call
    /// [`consistency::validate`] to see them.
    pub fn from_json_checked(json: &str) -> Result<Self, PersonaError> {
        let profile = Self::from_json(json)?;
        let result = consistency::validate(&profile);
        if result.is_valid() {
            Ok(profile)
        } else {
            Err(PersonaError::Inconsistent(result))
        }
    }

    /// [`PersonaProfile::from_json_checked`] for a file.
    pub fn from_path_checked(path: impl AsRef<Path>) -> Result<Self, PersonaError> {
        Self::from_json_checked(&std::fs::read_to_string(path)?)
    }

    pub fn family(&self) -> BrowserFamily {
        self.browser.family
    }

    /// The persona's preferences, or the defaults (light, no motion
    /// reduction, sRGB).
    pub fn preferences(&self) -> Preferences {
        self.preferences.clone().unwrap_or_default()
    }

    /// Scrollbar width in CSS px: the persona's value, else 17 on Windows,
    /// 0 on macOS (overlay scrollbars) and 15 elsewhere.
    pub fn scrollbar_width(&self) -> u32 {
        let os = self.os.name.to_ascii_lowercase();
        self.preferences
            .as_ref()
            .and_then(|p| p.scrollbar_width)
            .unwrap_or(if os.contains("windows") {
                17
            } else if os.contains("mac") {
                0
            } else {
                15
            })
    }

    /// `navigator.getBattery()` values: the persona's, else Chrome's
    /// desktop answer ([`BatteryInfo::desktop`]).
    pub fn battery_info(&self) -> BatteryInfo {
        self.battery.clone().unwrap_or_else(BatteryInfo::desktop)
    }

    /// `performance.now()` granularity in microseconds: the persona's value,
    /// else the family default (Chrome 100, Firefox 1000).
    pub fn performance_now_resolution_us(&self) -> u32 {
        self.timing
            .as_ref()
            .and_then(|t| t.performance_now_resolution_us)
            .unwrap_or(match self.browser.family {
                BrowserFamily::Chrome => 100,
                BrowserFamily::Firefox => 1000,
            })
    }

    /// The network-facing summary used by `lb-net`.
    ///
    /// Chrome `sec-ch-ua*` values come from `client_hints`; Firefox gets none.
    pub fn to_network(&self) -> Persona {
        let hints = match self.browser.family {
            BrowserFamily::Chrome => self.client_hints.as_ref(),
            BrowserFamily::Firefox => None,
        };
        Persona {
            family: self.browser.family,
            user_agent: self.browser.user_agent.clone(),
            accept_language: self.browser.accept_language.clone(),
            sec_ch_ua: hints.map(ClientHints::sec_ch_ua),
            sec_ch_ua_platform: hints.map(|h| h.platform.clone()),
            sec_ch_ua_mobile: hints.map(|h| if h.mobile { "?1" } else { "?0" }.to_owned()),
            tls_profile_id: self.tls.profile_id.clone(),
            high_entropy: hints.and_then(|_| self.high_entropy_hints()),
            tcp_profile_id: self.os_kind().default_tcp_profile_id().to_owned(),
        }
    }
}

impl Persona {
    /// Loads a v1 or v2 persona from JSON, checks the consistency rules and
    /// returns the network summary. Use [`PersonaProfile::from_json_checked`]
    /// to keep the full profile.
    pub fn load_json(json: &str) -> Result<Self, PersonaError> {
        PersonaProfile::from_json_checked(json).map(|p| p.to_network())
    }

    /// [`Persona::load_json`] for a file.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, PersonaError> {
        PersonaProfile::from_path_checked(path).map(|p| p.to_network())
    }
}
