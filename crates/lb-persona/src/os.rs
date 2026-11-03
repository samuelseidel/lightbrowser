//! The OS axis of a persona (M7, ADR 0015): which desktop OS, display
//! server and distro it claims, and the OS defaults every layer reads
//! through accessors on [`PersonaProfile`].
//!
//! Each accessor returns the persona's own value when the JSON has one and
//! otherwise the default for its OS (and distro). Values marked "not
//! captured" in the comments come from the M7 surface reference, not from
//! a capture of real Chrome on that OS; Linux values were captured from the
//! local Chrome for Testing 153 (`tests/fixtures/os/`).
//!
//! TCP/IP-level signals (TTL, TCP options) are not modelled: they always
//! come from the host OS.

use serde::{Deserialize, Serialize};

use crate::profile::{BrandVersion, PersonaProfile};

/// The desktop OS a persona claims (from `os.name`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OsKind {
    Windows,
    MacOs,
    Linux,
    /// Anything else (Chrome OS, Android, unknown). No OS defaults.
    Other,
}

impl OsKind {
    /// Parses `os.name` (`Windows`, `macOS`, `Linux`; case-insensitive).
    pub fn from_name(name: &str) -> OsKind {
        let n = name.to_ascii_lowercase();
        if n.contains("windows") {
            OsKind::Windows
        } else if n.contains("mac") {
            OsKind::MacOs
        } else if n.contains("linux") {
            OsKind::Linux
        } else {
            OsKind::Other
        }
    }

    /// `windows`, `macos`, `linux` or `other` (also the `profiles/fonts/`
    /// directory names).
    pub const fn as_str(self) -> &'static str {
        match self {
            OsKind::Windows => "windows",
            OsKind::MacOs => "macos",
            OsKind::Linux => "linux",
            OsKind::Other => "other",
        }
    }
}

/// Linux display server. Wayland hides global window coordinates and the
/// work area from Chrome: `screen.avail*` equals the screen and
/// `screenX/Y` are 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayServer {
    X11,
    Wayland,
}

impl DisplayServer {
    pub const fn as_str(self) -> &'static str {
        match self {
            DisplayServer::X11 => "x11",
            DisplayServer::Wayland => "wayland",
        }
    }
}

/// Linux distribution family, from `os.distro` (`ubuntu-24.04`,
/// `fedora-42`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Distro {
    Ubuntu,
    Fedora,
    Other,
}

/// Default fonts (Chrome's per-OS font settings, the generic families and
/// the system font keywords). Families are CSS names as a page would write
/// them; on Linux they may be fontconfig aliases (see
/// [`PersonaProfile::font_aliases`]).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct FontDefaults {
    /// Chrome's "standard" font (initial `font-family`).
    pub standard: String,
    pub serif: String,
    pub sans_serif: String,
    pub monospace: String,
    pub cursive: String,
    pub fantasy: String,
    /// `system-ui`, `-apple-system`, `BlinkMacSystemFont`. On macOS the
    /// table name `system-ui` stands for `.AppleSystemUIFont` (SF).
    pub system_ui: String,
    pub emoji: String,
    /// Family of the system font keywords (`font: caption`, `menu`,
    /// `message-box`, `status-bar`, `icon`, `small-caption`).
    pub system_font: String,
    /// Size in CSS px of the system font keywords.
    pub system_font_px: f64,
}

/// Wheel model: CSS px scrolled per mouse-wheel notch and the
/// `wheelDelta`-to-`deltaY` ratio (`wheelDeltaY = -deltaY * ratio`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WheelModel {
    pub px_per_notch: f64,
    pub wheel_delta_per_px: f64,
}

/// Realtime `AudioContext` defaults.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioDefaults {
    pub sample_rate: u32,
    pub base_latency: f64,
    pub output_latency: f64,
    pub channels: u32,
}

/// `navigator.gpu.requestAdapter()` answer (`GPUAdapterInfo`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct WebGpuAdapter {
    /// `nvidia`, `intel`, `amd`, `apple`, `qualcomm`.
    pub vendor: String,
    /// e.g. `ampere`, `lovelace`, `gen-12lp`, `rdna-2`, `metal-3`.
    pub architecture: String,
    /// Empty unless unmasked.
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub description: String,
}

/// Hardware video decode answers behind `canPlayType`,
/// `MediaSource.isTypeSupported` and `mediaCapabilities.decodingInfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Codecs {
    /// HEVC (`hvc1`/`hev1`) playable: hardware decode on Windows/macOS;
    /// `""` on Linux by default (captured).
    pub hevc: bool,
    /// `decodingInfo` H.264 1080p `powerEfficient`.
    pub h264_power_efficient: bool,
    /// AV1 decoded in hardware (`powerEfficient` for AV1).
    pub av1_hw: bool,
}

/// OS-gated web APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OsApis {
    /// `navigator.share` / `canShare`: Windows and macOS, not Linux.
    pub share: bool,
    /// `navigator.bluetooth` and the `Bluetooth*` interfaces: Windows and
    /// macOS, not Linux.
    pub bluetooth: bool,
}

/// High-entropy UA-CH values (`getHighEntropyValues`, the
/// `Sec-CH-UA-*` headers sent after `Accept-CH`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighEntropyHints {
    pub platform: String,
    /// Windows `10.0.0`/`15.0.0`..., macOS `15.1.0`, Linux `""`.
    pub platform_version: String,
    pub architecture: String,
    pub bitness: String,
    pub model: String,
    pub wow64: bool,
    pub full_version_list: Vec<BrandVersion>,
    pub full_version: String,
    /// `["Desktop"]` for desktop personas.
    pub form_factors: Vec<String>,
}

/// Blink's `AlternateFamilyName` pairs (every OS): when a family is not
/// installed Chrome retries with its partner.
const BLINK_ALTERNATES: &[(&str, &str)] = &[
    ("Arial", "Helvetica"),
    ("Helvetica", "Arial"),
    ("Courier", "Courier New"),
    ("Courier New", "Courier"),
    ("Times", "Times New Roman"),
    ("Times New Roman", "Times"),
];

/// fontconfig's metric-compatible aliases on Linux (captured: Arial,
/// Helvetica, Arimo -> Liberation Sans and so on, Calibri -> Carlito,
/// Cambria -> Caladea).
const LINUX_METRIC_ALIASES: &[(&str, &str)] = &[
    ("Arial", "Liberation Sans"),
    ("Helvetica", "Liberation Sans"),
    ("Arimo", "Liberation Sans"),
    ("Times New Roman", "Liberation Serif"),
    ("Times", "Liberation Serif"),
    ("Tinos", "Liberation Serif"),
    ("Courier New", "Liberation Mono"),
    ("Courier", "Liberation Mono"),
    ("Cousine", "Liberation Mono"),
    ("Calibri", "Carlito"),
    ("Cambria", "Caladea"),
];

/// Logical CPU count for an Apple chip name (`M2 Pro` ...). Every
/// configuration Apple sells. `None` for unknown chips.
pub fn apple_chip_threads(chip: &str) -> Option<&'static [u32]> {
    Some(match chip {
        "M1" => &[8],
        "M1 Pro" => &[8, 10],
        "M1 Max" => &[10],
        "M1 Ultra" => &[20],
        "M2" => &[8],
        "M2 Pro" => &[10, 12],
        "M2 Max" => &[12],
        "M2 Ultra" => &[24],
        "M3" => &[8],
        "M3 Pro" => &[11, 12],
        "M3 Max" => &[14, 16],
        "M3 Ultra" => &[28, 32],
        "M4" => &[10],
        "M4 Pro" => &[12, 14],
        "M4 Max" => &[14, 16],
        "M5" => &[10],
        _ => return None,
    })
}

/// The first macOS major an Apple chip family shipped with.
pub fn apple_chip_min_macos(chip: &str) -> Option<u32> {
    let family = chip.split_whitespace().next()?;
    Some(match family {
        "M1" => 11,
        "M2" => 12,
        "M3" => 14,
        "M4" => 15,
        "M5" => 26,
        _ => return None,
    })
}

/// `Apple M2 Pro` from an ANGLE Metal renderer string, as `M2 Pro`.
pub fn apple_chip(renderer: &str) -> Option<String> {
    let i = renderer.find("Apple M")?;
    let rest = &renderer[i + "Apple ".len()..];
    let end = rest.find([',', ')']).unwrap_or(rest.len());
    Some(rest[..end].trim().to_owned())
}

/// Windows build (`10.0.19045`) to Chrome's `platformVersion` major
/// (Chromium `GetPlatformVersion`: the UniversalApiContract version).
/// `None` for builds not in the table.
pub fn windows_build_platform_major(build: u32) -> Option<u32> {
    Some(match build {
        17763 => 7,
        18362 | 18363 => 8,
        19041..=19045 => 10,
        22000 => 14,
        22621 | 22631 => 15,
        26100 | 26200 => 19,
        _ => return None,
    })
}

/// The `N` of `10.0.N` (`os.build` of a Windows persona).
pub fn windows_build_number(build: &str) -> Option<u32> {
    build.rsplit('.').next()?.parse().ok()
}

/// `major.minor.patch` with missing parts as 0 (`15.1` -> `(15, 1, 0)`).
pub fn version3(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim();
    let mut it = s.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next().map_or(Some(0), |x| x.parse().ok())?;
    let patch = it.next().map_or(Some(0), |x| x.parse().ok())?;
    if it.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// WebGPU architecture for a GPU renderer string (Chrome's
/// `GPUAdapterInfo.architecture`), by family.
fn webgpu_for_renderer(renderer: &str) -> Option<WebGpuAdapter> {
    let r = renderer;
    let has = |s: &str| r.contains(s);
    let (vendor, arch) = if has("Apple M") || has("Apple GPU") {
        ("apple", "metal-3")
    } else if has("NVIDIA") || has("GeForce") {
        let arch = if has("RTX 50") {
            "blackwell"
        } else if has("RTX 40") {
            "lovelace"
        } else if has("RTX 30") || has("RTX A") {
            "ampere"
        } else if has("RTX 20") || has("GTX 16") {
            "turing"
        } else if has("GTX 10") {
            "pascal"
        } else {
            ""
        };
        ("nvidia", arch)
    } else if has("AMD") || has("Radeon") {
        let arch = if has("RX 9") {
            "rdna-4"
        } else if has("RX 7") || has("780M") || has("760M") {
            "rdna-3"
        } else if has("RX 6") || has("680M") || has("660M") {
            "rdna-2"
        } else if has("RX 5") {
            "rdna-1"
        } else {
            ""
        };
        ("amd", arch)
    } else if has("Intel") {
        let arch = if has("Arc") {
            "xe-hpg"
        } else if has("Iris(R) Xe") || has("Iris Xe") || has("UHD Graphics 7") {
            "gen-12lp"
        } else if has("Iris(TM) Plus") || has("Iris Plus") {
            "gen-11"
        } else if has("UHD Graphics 6") || has("HD Graphics 6") {
            "gen-9"
        } else {
            ""
        };
        ("intel", arch)
    } else if has("Qualcomm") || has("Adreno") {
        ("qualcomm", "")
    } else {
        return None;
    };
    Some(WebGpuAdapter {
        vendor: vendor.into(),
        architecture: arch.into(),
        device: String::new(),
        description: String::new(),
    })
}

/// True for software renderers (no hardware video decode).
fn software_gpu(renderer: &str) -> bool {
    ["SwiftShader", "llvmpipe", "Basic Render", "softpipe"]
        .iter()
        .any(|s| renderer.contains(s))
}

/// AV1 hardware decode by GPU family.
fn av1_hw_decode(renderer: &str) -> bool {
    let has = |s: &str| renderer.contains(s);
    if let Some(chip) = apple_chip(renderer) {
        return !(chip.starts_with("M1") || chip.starts_with("M2"));
    }
    has("RTX 30")
        || has("RTX 40")
        || has("RTX 50")
        || has("RTX A")
        || has("Iris(R) Xe")
        || has("Iris Xe")
        || has("Arc")
        || has("RX 6") && !has("RX 6400") && !has("RX 6500")
        || has("RX 7")
        || has("RX 9")
        || has("780M")
        || has("680M")
}

impl PersonaProfile {
    /// The OS from `os.name`.
    pub fn os_kind(&self) -> OsKind {
        OsKind::from_name(&self.os.name)
    }

    /// Linux only: `os.display_server`, default X11. `None` off Linux.
    pub fn display_server(&self) -> Option<DisplayServer> {
        if self.os_kind() != OsKind::Linux {
            return None;
        }
        Some(
            match self
                .os
                .display_server
                .as_deref()
                .map(str::to_ascii_lowercase)
            {
                Some(s) if s == "wayland" => DisplayServer::Wayland,
                _ => DisplayServer::X11,
            },
        )
    }

    /// Linux only: the distro family and version from `os.distro`
    /// (`ubuntu-24.04` -> `(Ubuntu, "24.04")`).
    pub fn linux_distro(&self) -> Option<(Distro, String)> {
        if self.os_kind() != OsKind::Linux {
            return None;
        }
        let d = self.os.distro.as_deref()?.to_ascii_lowercase();
        let (id, ver) = d.split_once('-').unwrap_or((d.as_str(), ""));
        let kind = match id {
            "ubuntu" => Distro::Ubuntu,
            "fedora" => Distro::Fedora,
            _ => Distro::Other,
        };
        Some((kind, ver.to_owned()))
    }

    /// Windows only: 10 or 11 (the first number of `os.version`).
    pub fn windows_major(&self) -> Option<u32> {
        if self.os_kind() != OsKind::Windows {
            return None;
        }
        let digits: String = self
            .os
            .version
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    }

    /// macOS only: `os.version` as `(major, minor, patch)`.
    pub fn macos_version(&self) -> Option<(u32, u32, u32)> {
        if self.os_kind() != OsKind::MacOs {
            return None;
        }
        version3(&self.os.version)
    }

    /// The Apple chip named by the WebGL renderer (`M4`, `M2 Pro`).
    pub fn apple_chip(&self) -> Option<String> {
        apple_chip(&self.hardware.gpu.renderer_unmasked)
    }

    /// Default fonts: `fonts.defaults` merged over the OS/distro defaults.
    pub fn font_defaults(&self) -> FontDefaults {
        let mut d = self.os_font_defaults();
        if let Some(s) = self.fonts.defaults.as_ref() {
            let set = |dst: &mut String, src: &Option<String>| {
                if let Some(v) = src {
                    dst.clone_from(v);
                }
            };
            set(&mut d.standard, &s.standard);
            set(&mut d.serif, &s.serif);
            set(&mut d.sans_serif, &s.sans_serif);
            set(&mut d.monospace, &s.monospace);
            set(&mut d.cursive, &s.cursive);
            set(&mut d.fantasy, &s.fantasy);
            set(&mut d.system_ui, &s.system_ui);
            set(&mut d.emoji, &s.emoji);
            set(&mut d.system_font, &s.system_font);
            if let Some(px) = s.system_font_px {
                d.system_font_px = px;
            }
        }
        d
    }

    fn os_font_defaults(&self) -> FontDefaults {
        let f = |standard: &str,
                 serif: &str,
                 sans: &str,
                 mono: &str,
                 cursive: &str,
                 fantasy: &str,
                 system_ui: &str,
                 emoji: &str,
                 system_font: &str,
                 px: f64| FontDefaults {
            standard: standard.into(),
            serif: serif.into(),
            sans_serif: sans.into(),
            monospace: mono.into(),
            cursive: cursive.into(),
            fantasy: fantasy.into(),
            system_ui: system_ui.into(),
            emoji: emoji.into(),
            system_font: system_font.into(),
            system_font_px: px,
        };
        match self.os_kind() {
            // Not captured: Chrome's Windows font settings (Consolas is the
            // fixed font) and the 9 pt NONCLIENTMETRICS fonts.
            OsKind::Windows => f(
                "Times New Roman",
                "Times New Roman",
                "Arial",
                "Consolas",
                "Comic Sans MS",
                "Impact",
                "Segoe UI",
                "Segoe UI Emoji",
                "Segoe UI",
                12.0,
            ),
            // Not captured: Chrome's macOS font settings; `system-ui` is SF.
            OsKind::MacOs => f(
                "Times",
                "Times",
                "Helvetica",
                "Courier",
                "Apple Chancery",
                "Papyrus",
                "system-ui",
                "Apple Color Emoji",
                "system-ui",
                13.0,
            ),
            // Captured (CfT 153 with Ubuntu/Fedora-like fontconfig):
            // standard, cursive and fantasy are Times New Roman, sans-serif
            // Arial, monospace fontconfig "Monospace", the keywords Arial
            // 16px; system-ui is the GTK font.
            OsKind::Linux => {
                let (system_ui, mono) = match self.linux_distro() {
                    Some((Distro::Ubuntu, v)) if v.as_str() >= "23.10" => {
                        ("Ubuntu Sans", "DejaVu Sans Mono")
                    }
                    Some((Distro::Ubuntu, _)) => ("Ubuntu", "DejaVu Sans Mono"),
                    Some((Distro::Fedora, v)) => {
                        let major: u32 = v
                            .split('.')
                            .next()
                            .and_then(|m| m.parse().ok())
                            .unwrap_or(0);
                        if major >= 42 {
                            ("Adwaita Sans", "Noto Sans Mono")
                        } else {
                            ("Cantarell", "Noto Sans Mono")
                        }
                    }
                    _ => ("DejaVu Sans", "DejaVu Sans Mono"),
                };
                f(
                    "Times New Roman",
                    "Times New Roman",
                    "Arial",
                    mono,
                    "Times New Roman",
                    "Times New Roman",
                    system_ui,
                    "Noto Color Emoji",
                    "Arial",
                    16.0,
                )
            }
            OsKind::Other => f(
                "Times New Roman",
                "Times New Roman",
                "Arial",
                "Courier New",
                "Comic Sans MS",
                "Impact",
                "Arial",
                "Noto Color Emoji",
                "Arial",
                16.0,
            ),
        }
    }

    /// Family aliases Chrome resolves when a page names a family that is
    /// not installed: `(requested, used)`. Linux: fontconfig's
    /// metric-compatible aliases whose target the persona has (captured),
    /// then Blink's alternates; elsewhere Blink's alternates (Arial and
    /// Helvetica, Times and Times New Roman, Courier and Courier New), kept
    /// only when the partner is installed and the family itself is not.
    pub fn font_aliases(&self) -> Vec<(String, String)> {
        let has = |f: &str| {
            self.fonts
                .available
                .iter()
                .any(|a| a.eq_ignore_ascii_case(f))
        };
        let mut out: Vec<(String, String)> = Vec::new();
        let mut push = |from: &str, to: &str| {
            if !has(from) && has(to) && !out.iter().any(|(a, _)| a == from) {
                out.push((from.into(), to.into()));
            }
        };
        if self.os_kind() == OsKind::Linux {
            for (a, b) in LINUX_METRIC_ALIASES {
                push(a, b);
            }
        }
        for (a, b) in BLINK_ALTERNATES {
            push(a, b);
        }
        out
    }

    /// `(screenX, screenY)` of the window: `screen.window_x/y`, else 0,0 on
    /// Wayland, else the top-left of the work area.
    pub fn window_position(&self) -> (i32, i32) {
        let s = &self.screen;
        if let (Some(x), Some(y)) = (s.window_x, s.window_y) {
            return (x, y);
        }
        if self.display_server() == Some(DisplayServer::Wayland) {
            return (0, 0);
        }
        (
            i32::try_from(s.avail_left).unwrap_or(0),
            i32::try_from(s.avail_top).unwrap_or(0),
        )
    }

    /// Wheel model: `behavior.wheel`, else Windows 100 px and ratio 1.2
    /// (not captured), Linux X11 120 px and ratio 1 (captured), macOS 40
    /// px per tick and ratio 3 (Chromium's `kScrollbarPixelsPerCocoaTick`,
    /// not captured).
    pub fn wheel(&self) -> WheelModel {
        let (px, ratio) = match self.os_kind() {
            OsKind::Windows | OsKind::Other => (100.0, 1.2),
            OsKind::MacOs => (40.0, 3.0),
            OsKind::Linux => (120.0, 1.0),
        };
        let w = self.behavior.wheel.as_ref();
        WheelModel {
            px_per_notch: w.and_then(|w| w.px_per_notch).unwrap_or(px),
            wheel_delta_per_px: w.and_then(|w| w.delta_ratio).unwrap_or(ratio),
        }
    }

    /// Realtime audio defaults: the persona's `audio`, else 48 kHz on
    /// Windows and Linux (PipeWire), 44.1 kHz on macOS; `baseLatency` 0.01;
    /// `outputLatency` twice the base latency; 2 channels.
    pub fn audio_defaults(&self) -> AudioDefaults {
        let a = self.audio.as_ref();
        let rate = match self.os_kind() {
            OsKind::MacOs => 44_100,
            _ => 48_000,
        };
        let base = a.and_then(|a| a.base_latency).unwrap_or(0.01);
        AudioDefaults {
            sample_rate: a.and_then(|a| a.sample_rate).unwrap_or(rate),
            base_latency: base,
            output_latency: a.and_then(|a| a.output_latency).unwrap_or(base * 2.0),
            channels: a.and_then(|a| a.max_channel_count).unwrap_or(2),
        }
    }

    /// WebGPU adapter: the persona's `webgpu` (`null` means no adapter),
    /// else on Windows and macOS the WebGL GPU's vendor and architecture;
    /// `None` on Linux (captured: `requestAdapter()` resolves `null`).
    pub fn webgpu(&self) -> Option<WebGpuAdapter> {
        match &self.webgpu {
            Some(explicit) => explicit.clone(),
            None => match self.os_kind() {
                OsKind::Windows | OsKind::MacOs => {
                    let r = &self.hardware.gpu.renderer_unmasked;
                    if software_gpu(r) {
                        None
                    } else {
                        webgpu_for_renderer(r)
                    }
                }
                _ => None,
            },
        }
    }

    /// Codec answers: the persona's `media`, else hardware decode on
    /// Windows and macOS with a hardware GPU, none on Linux (captured:
    /// HEVC `""`, H.264 not power efficient).
    pub fn codecs(&self) -> Codecs {
        let r = &self.hardware.gpu.renderer_unmasked;
        let hw = matches!(self.os_kind(), OsKind::Windows | OsKind::MacOs) && !software_gpu(r);
        let m = self.media.as_ref();
        Codecs {
            hevc: m.and_then(|m| m.hevc).unwrap_or(hw),
            h264_power_efficient: m.and_then(|m| m.h264_power_efficient).unwrap_or(hw),
            av1_hw: m.and_then(|m| m.av1_hw).unwrap_or(hw && av1_hw_decode(r)),
        }
    }

    /// OS-gated APIs: the persona's `apis`, else present on Windows and
    /// macOS, absent on Linux (captured).
    pub fn os_apis(&self) -> OsApis {
        let on = matches!(self.os_kind(), OsKind::Windows | OsKind::MacOs);
        let a = self.apis.as_ref();
        OsApis {
            share: a.and_then(|a| a.share).unwrap_or(on),
            bluetooth: a.and_then(|a| a.bluetooth).unwrap_or(on),
        }
    }

    /// High-entropy UA-CH values. `None` without `client_hints` (Firefox).
    pub fn high_entropy_hints(&self) -> Option<HighEntropyHints> {
        let h = self.client_hints.as_ref()?;
        Some(HighEntropyHints {
            platform: h.platform.clone(),
            platform_version: h.platform_version.clone(),
            architecture: h.architecture.clone(),
            bitness: h.bitness.clone(),
            model: h.model.clone(),
            wow64: h.wow64,
            full_version_list: h.full_version_list.clone(),
            full_version: h
                .full_version
                .clone()
                .unwrap_or_else(|| self.browser.version.clone()),
            form_factors: h
                .form_factors
                .clone()
                .unwrap_or_else(|| vec![if h.mobile { "Mobile" } else { "Desktop" }.to_owned()]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_kind_names() {
        assert_eq!(OsKind::from_name("Windows"), OsKind::Windows);
        assert_eq!(OsKind::from_name("macOS"), OsKind::MacOs);
        assert_eq!(OsKind::from_name("Linux"), OsKind::Linux);
        assert_eq!(OsKind::from_name("Chrome OS"), OsKind::Other);
    }

    #[test]
    fn apple_chips() {
        assert_eq!(
            apple_chip("ANGLE (Apple, ANGLE Metal Renderer: Apple M2 Pro, Unspecified Version)")
                .as_deref(),
            Some("M2 Pro")
        );
        assert_eq!(apple_chip_threads("M4"), Some(&[10u32][..]));
        assert_eq!(apple_chip_min_macos("M3 Max"), Some(14));
        assert!(apple_chip("ANGLE (Intel, Intel(R) UHD Graphics 630)").is_none());
    }

    #[test]
    fn versions() {
        assert_eq!(version3("15.1"), Some((15, 1, 0)));
        assert_eq!(version3("26.0.1"), Some((26, 0, 1)));
        assert_eq!(version3("x"), None);
        assert_eq!(windows_build_number("10.0.26100"), Some(26100));
        assert_eq!(windows_build_platform_major(19045), Some(10));
        assert_eq!(windows_build_platform_major(26100), Some(19));
    }

    #[test]
    fn webgpu_architectures() {
        let w = webgpu_for_renderer(
            "ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 (0x00002504) Direct3D11 vs_5_0 ps_5_0, D3D11)",
        )
        .unwrap();
        assert_eq!(
            (w.vendor.as_str(), w.architecture.as_str()),
            ("nvidia", "ampere")
        );
        let w = webgpu_for_renderer(
            "ANGLE (Apple, ANGLE Metal Renderer: Apple M4, Unspecified Version)",
        )
        .unwrap();
        assert_eq!(
            (w.vendor.as_str(), w.architecture.as_str()),
            ("apple", "metal-3")
        );
        assert!(av1_hw_decode(
            "ANGLE (Apple, ANGLE Metal Renderer: Apple M4, Unspecified Version)"
        ));
        assert!(!av1_hw_decode(
            "ANGLE (Apple, ANGLE Metal Renderer: Apple M2, Unspecified Version)"
        ));
    }
}
