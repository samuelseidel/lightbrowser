//! Persona generator: samples consistent desktop Chrome personas from the
//! catalog in `profiles/catalog/` (OS releases, CPUs and Apple chips, GPUs
//! and WebGL per backend, screens, fonts, voices, audio).
//!
//! The same catalog, OS, seed and index always give the same persona.
//! Every persona is validated; one that breaks a rule is refused (an error,
//! never written). Run it with
//! `cargo run -p lb-persona --example gen_personas -- --os linux --count 5 --seed 1 --out profiles/generated/linux`.
//!
//! Catalog values for Windows and macOS are the M7 surface reference, not
//! captures; each generated persona says so in `provenance`.

use std::path::Path;

use serde_json::{Value, json};

use crate::PersonaProfile;
use crate::consistency::{self, render_report};
use crate::os::{OsKind, version3};
use crate::sfc32::Sfc32;
use crate::uach::{chrome_brands, chrome_full_version_list};

/// The catalog files, parsed.
pub struct Catalog {
    os: Value,
    cpu: Value,
    gpu: Value,
    screen: Value,
    fonts: Value,
    voices: Value,
    audio: Value,
}

impl Catalog {
    /// Reads `{os,cpu,gpu,screen,fonts,voices,audio}.json` from `dir`.
    pub fn load(dir: &Path) -> Result<Catalog, String> {
        let read = |name: &str| -> Result<Value, String> {
            let path = dir.join(format!("{name}.json"));
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
        };
        Ok(Catalog {
            os: read("os")?,
            cpu: read("cpu")?,
            gpu: read("gpu")?,
            screen: read("screen")?,
            fonts: read("fonts")?,
            voices: read("voices")?,
            audio: read("audio")?,
        })
    }
}

/// Deterministic choices from SFC32 (seeded with the run seed and index).
struct Rng(Sfc32);

impl Rng {
    fn new(seed: u64, index: u64) -> Rng {
        let mut b = [0u8; 16];
        b[..8].copy_from_slice(&seed.to_le_bytes());
        b[8..].copy_from_slice(&index.to_le_bytes());
        b[15] ^= 0x5a;
        let mut s = Sfc32::new(b);
        for _ in 0..12 {
            s.next_u32();
        }
        Rng(s)
    }

    fn below(&mut self, n: usize) -> usize {
        if n <= 1 {
            0
        } else {
            (self.0.next_u32() as usize) % n
        }
    }

    /// One item, weighted by its `weight` (default 1).
    fn pick<'a>(&mut self, items: &[&'a Value]) -> Option<&'a Value> {
        let w = |v: &Value| v.get("weight").and_then(Value::as_u64).unwrap_or(1).max(1);
        let total: u64 = items.iter().map(|v| w(v)).sum();
        if total == 0 {
            return None;
        }
        let mut r = u64::from(self.0.next_u32()) % total;
        for v in items {
            if r < w(v) {
                return Some(v);
            }
            r -= w(v);
        }
        None
    }

    /// One value from `[[value, weight], ...]`.
    fn pick_pair(&mut self, pairs: &Value) -> Option<Value> {
        let list = pairs.as_array()?;
        let total: u64 = list.iter().filter_map(|p| p.get(1)?.as_u64()).sum();
        let mut r = u64::from(self.0.next_u32()) % total.max(1);
        for p in list {
            let w = p.get(1)?.as_u64()?;
            if r < w {
                return p.get(0).cloned();
            }
            r -= w;
        }
        None
    }

    /// One plain value from an array.
    fn pick_plain(&mut self, list: &Value) -> Option<Value> {
        let a = list.as_array()?;
        a.get(self.below(a.len())).cloned()
    }

    fn hex(&mut self, digits: usize) -> String {
        let mut s = String::with_capacity(digits + 8);
        while s.len() < digits {
            s.push_str(&format!("{:08x}", self.0.next_u32()));
        }
        s.truncate(digits);
        s
    }

    fn chance(&mut self, percent: u32) -> bool {
        self.0.next_u32() % 100 < percent
    }
}

fn arr<'a>(v: &'a Value, key: &str) -> Vec<&'a Value> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn n(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn err(what: &str) -> String {
    format!("catalog: {what}")
}

/// Generates `count` personas for `os` (Windows, macOS or Linux).
pub fn generate(
    cat: &Catalog,
    os: OsKind,
    count: usize,
    seed: u64,
) -> Result<Vec<PersonaProfile>, String> {
    (0..count as u64)
        .map(|i| generate_one(cat, os, seed, i))
        .collect()
}

/// One persona. Errors if the catalog cannot produce one for `os` or if the
/// result breaks a consistency rule (the report is in the error).
pub fn generate_one(
    cat: &Catalog,
    os: OsKind,
    seed: u64,
    index: u64,
) -> Result<PersonaProfile, String> {
    let mut rng = Rng::new(seed, index);
    let kind = os.as_str();
    let releases: Vec<&Value> = arr(&cat.os, "releases")
        .into_iter()
        .filter(|r| s(r, "kind") == kind)
        .collect();
    let release = rng
        .pick(&releases)
        .ok_or_else(|| err(&format!("no release for {kind}")))?;
    let mut os_block = release["os"].clone();
    let chrome = &cat.os["chrome"];
    let version = s(chrome, "version").to_owned();
    let major = chrome["major"].as_u64().unwrap_or(153) as u32;
    let ui = &cat.screen["chrome_ui_px"];

    // Machine: GPU, threads, memory, screen.
    let m = match os {
        OsKind::Windows => windows_machine(cat, release, &mut rng)?,
        OsKind::MacOs => mac_machine(cat, release, &mut rng)?,
        OsKind::Linux => linux_machine(cat, release, &mut rng)?,
        OsKind::Other => return Err("generator supports windows, macos and linux".into()),
    };
    if os == OsKind::MacOs {
        os_block["architecture"] = json!(m.arch);
    }
    let ui_px = ui.get(kind).and_then(Value::as_u64).unwrap_or(100) as u32;
    let inner_h = m.outer.1.saturating_sub(ui_px);

    let platform_version = match os {
        OsKind::MacOs => {
            let (a, b, c) =
                version3(s(&os_block, "version")).ok_or_else(|| err("macOS version"))?;
            format!("{a}.{b}.{c}")
        }
        _ => s(release, "platform_version").to_owned(),
    };
    let ua_os = s(&cat.os["ua_os"], kind);
    let ua = format!(
        "Mozilla/5.0 ({ua_os}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{major}.0.0.0 Safari/537.36"
    );
    let locales = arr(&cat.os, "locales");
    let locale = rng.pick(&locales).ok_or_else(|| err("locales"))?;
    let tz = rng
        .pick_plain(&locale["timezones"])
        .ok_or_else(|| err("timezones"))?;

    let backend = &cat.gpu["backends"][m.backend.as_str()];
    if backend.is_null() {
        return Err(err(&format!("no backend {}", m.backend)));
    }
    let fonts = cat.fonts["sets"][s(release, "fonts")].clone();
    if !fonts.is_array() {
        return Err(err(&format!("no font set {}", s(release, "fonts"))));
    }
    let voices = cat.voices[kind].clone();
    let audio_key = match (os, m.arch.as_str()) {
        (OsKind::MacOs, "arm64") => "macos_arm",
        (OsKind::MacOs, _) => "macos_intel",
        _ => kind,
    };
    let audio_opts = arr(&cat.audio, audio_key);
    let mut audio = rng.pick(&audio_opts).ok_or_else(|| err("audio"))?.clone();
    if let Some(o) = audio.as_object_mut() {
        o.remove("weight");
    }
    let devices_key = format!("{kind}_{}", if m.laptop { "laptop" } else { "desktop" });
    let media_devices = cat.audio["media_devices"][devices_key.as_str()].clone();

    // UUID v4 shape (the schema's pattern).
    let profile_id = format!(
        "{}-{}-4{}-{}{}-{}",
        rng.hex(8),
        rng.hex(4),
        rng.hex(3),
        ["8", "9", "a", "b"][rng.below(4)],
        rng.hex(3),
        rng.hex(12)
    );
    let seeds = [rng.hex(32), rng.hex(32), rng.hex(32)];
    let behavior_profile =
        ["casual_user", "casual_user", "power_user", "slow_reader"][rng.below(4)];
    let dark = rng.chance(25);

    let mut screen = json!({
        "width": m.screen.0, "height": m.screen.1,
        "avail_width": m.avail.0, "avail_height": m.avail.1,
        "color_depth": m.color_depth, "pixel_depth": m.color_depth,
        "device_pixel_ratio": m.dpr,
        "inner_width": m.outer.0, "inner_height": inner_h,
        "outer_width": m.outer.0, "outer_height": m.outer.1,
        "avail_top": m.avail_origin.1, "avail_left": m.avail_origin.0,
        "maximized": true,
    });
    if os == OsKind::Linux && s(&os_block, "display_server") == "wayland" {
        screen["window_x"] = json!(0);
        screen["window_y"] = json!(0);
    }
    let os_label = match os {
        OsKind::Windows => format!("Windows {}", s(&os_block, "version")),
        OsKind::MacOs => format!("macOS {}", s(&os_block, "version")),
        _ => format!(
            "{} ({})",
            s(&os_block, "version"),
            if s(&os_block, "display_server") == "wayland" {
                "Wayland"
            } else {
                "X11"
            }
        ),
    };
    let mut v = json!({
        "schema_version": "2.0",
        "profile_id": profile_id,
        "profile_name": format!("Chrome {major} on {os_label} - {} - {}x{} (generated)", m.gpu_label, m.screen.0, m.screen.1),
        "created_at": "2026-11-03T00:00:00Z",
        "tags": ["chrome", format!("chrome{major}"), kind, s(release, "id"), "generated"],
        "adversarial": false,
        "os": os_block,
        "browser": {
            "family": "chrome", "name": "Chrome", "version": version,
            "user_agent": ua, "accept_language": s(locale, "accept_language"),
            "do_not_track": null, "cookie_enabled": true, "java_enabled": false,
            "pdf_viewer_enabled": true, "product": "Gecko", "product_sub": "20030107",
            "vendor": "Google Inc.", "vendor_sub": "", "app_name": "Netscape",
            "app_code_name": "Mozilla",
            "app_version": ua.strip_prefix("Mozilla/").unwrap_or(&ua),
        },
        "client_hints": {
            "brands": chrome_brands(major).iter().map(|b| json!({"brand": b.brand, "version": b.version})).collect::<Vec<_>>(),
            "full_version_list": chrome_full_version_list(major, &version).iter().map(|b| json!({"brand": b.brand, "version": b.version})).collect::<Vec<_>>(),
            "platform": match os { OsKind::Windows => "Windows", OsKind::MacOs => "macOS", _ => "Linux" },
            "platform_version": platform_version,
            "architecture": if m.arch == "arm64" { "arm" } else { "x86" },
            "bitness": "64", "model": "", "mobile": false, "wow64": false,
            "full_version": version,
        },
        "hardware": {
            "hardware_concurrency": m.threads,
            "device_memory": m.device_memory,
            "max_touch_points": 0,
            "gpu": {
                "vendor": "WebKit", "renderer": "WebKit WebGL",
                "vendor_unmasked": m.vendor, "renderer_unmasked": m.renderer,
            },
        },
        "screen": screen,
        "locale": {
            "language": s(locale, "language"), "languages": locale["languages"].clone(),
            "timezone": tz[0].clone(), "timezone_offset": tz[1].clone(),
        },
        "network": {"connection_type": "4g", "downlink": 10, "rtt": 50, "save_data": false},
        "fingerprint_seeds": {"canvas_seed": seeds[0], "webgl_noise_seed": seeds[1], "audio_seed": seeds[2]},
        "webgl": {
            "version": s(&cat.gpu, "version"),
            "shading_language_version": s(&cat.gpu, "shading_language_version"),
            "extensions": backend["extensions"].clone(),
            "parameters": backend["parameters"].clone(),
            "shader_precision": cat.gpu["shader_precision"].clone(),
        },
        "fonts": {"available": fonts},
        "tls": {
            "profile_id": s(chrome, "tls_profile_id"), "ja3": s(chrome, "ja3"),
            "ja3s_note": s(chrome, "ja3s_note"),
        },
        "timing": {"performance_now_resolution_us": 100},
        "behavior": {"profile": behavior_profile},
        "audio": audio,
        "permissions": {"geolocation": "prompt", "notifications": "prompt", "camera": "prompt", "microphone": "prompt"},
        "preferences": {
            "color_scheme": if dark { "dark" } else { "light" },
            "color_gamut": m.gamut,
            "high_dynamic_range": m.hdr,
            "scrollbar_width": m.scrollbar,
        },
        "speech_voices": voices,
        "provenance": {
            "captured": false,
            "source": format!("generated from profiles/catalog by gen_personas (os {kind}, seed {seed}, index {index})"),
            "notes": ["Windows and macOS values are the M7 surface reference, not captures; Linux platformVersion, scrollbars, wheel and font defaults follow the local Chrome for Testing 153 capture (tests/fixtures/os/)."],
        },
    });
    if !media_devices.is_null() {
        v["media_devices"] = media_devices;
    }
    if let Some(pd) = m.pointing_device {
        v["behavior"]["pointing_device"] = json!(pd);
    }
    if m.laptop {
        let percent = 35 + rng.below(60) as u32;
        let level = f64::from(percent) / 100.0;
        let charging = rng.chance(40);
        v["battery"] = if charging {
            json!({"level": level, "charging": true, "charging_time": f64::from(72 * (100 - percent))})
        } else {
            json!({"level": level, "charging": false, "discharging_time": f64::from(360 * percent)})
        };
    }

    let profile = PersonaProfile::from_json(&v.to_string()).map_err(|e| e.to_string())?;
    let result = consistency::validate(&profile);
    if !result.is_valid() {
        return Err(format!(
            "generated persona breaks the rules (catalog bug):\n{}",
            render_report(&profile, &result)
        ));
    }
    Ok(profile)
}

/// What the per-OS machine pickers return.
struct Machine {
    arch: String,
    backend: String,
    vendor: String,
    renderer: String,
    gpu_label: String,
    threads: u64,
    device_memory: f64,
    screen: (u32, u32),
    dpr: f64,
    avail: (u32, u32),
    avail_origin: (u32, u32),
    outer: (u32, u32),
    color_depth: u32,
    gamut: String,
    hdr: bool,
    scrollbar: u32,
    laptop: bool,
    pointing_device: Option<&'static str>,
}

/// `ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 (0x00002504) ...)` ->
/// `NVIDIA GeForce RTX 3060`.
fn gpu_label(renderer: &str) -> String {
    let inner = renderer
        .strip_prefix("ANGLE (")
        .and_then(|r| r.split_once(", "))
        .map_or(renderer, |(_, rest)| rest);
    let inner = inner
        .strip_prefix("ANGLE Metal Renderer: ")
        .unwrap_or(inner);
    let inner = inner.strip_prefix("Mesa ").unwrap_or(inner);
    let end = [inner.find(" ("), inner.find(',')]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(inner.len());
    inner[..end].to_owned()
}

fn pick_form(rng: &mut Rng) -> &'static str {
    if rng.chance(55) { "desktop" } else { "laptop" }
}

fn windows_machine(cat: &Catalog, release: &Value, rng: &mut Rng) -> Result<Machine, String> {
    let form = pick_form(rng);
    let gpus: Vec<&Value> = arr(&cat.gpu, "windows")
        .into_iter()
        .filter(|g| s(g, "form") == form)
        .collect();
    let gpu = rng.pick(&gpus).ok_or_else(|| err("windows gpu"))?;
    let screens: Vec<&Value> = arr(&cat.screen, s(release, "screens"))
        .into_iter()
        .filter(|x| s(x, "form") == form)
        .collect();
    let sc = rng.pick(&screens).ok_or_else(|| err("windows screen"))?;
    let (w, h, dpr) = (n(sc, "width") as u32, n(sc, "height") as u32, n(sc, "dpr"));
    let taskbar = n(release, "taskbar_px");
    let avail_h = ((f64::from(h) * dpr - taskbar) / dpr).round() as u32;
    let cpu = &cat.cpu[form];
    let renderer = s(gpu, "renderer").to_owned();
    Ok(Machine {
        arch: "x86_64".into(),
        backend: "d3d11".into(),
        vendor: format!("Google Inc. ({})", s(gpu, "vendor")),
        gpu_label: gpu_label(&renderer),
        renderer,
        threads: rng
            .pick_pair(&cpu["threads"])
            .and_then(|v| v.as_u64())
            .ok_or_else(|| err("threads"))?,
        device_memory: rng
            .pick_pair(&cpu["device_memory"])
            .and_then(|v| v.as_f64())
            .ok_or_else(|| err("memory"))?,
        screen: (w, h),
        dpr,
        avail: (w, avail_h),
        avail_origin: (0, 0),
        outer: (w, avail_h),
        color_depth: 24,
        gamut: "srgb".into(),
        hdr: false,
        scrollbar: 17,
        laptop: form == "laptop",
        pointing_device: None,
    })
}

fn mac_machine(cat: &Catalog, release: &Value, rng: &mut Rng) -> Result<Machine, String> {
    let (major, _, _) =
        version3(s(&release["os"], "version")).ok_or_else(|| err("macOS version"))?;
    let chips: Vec<&Value> = arr(&cat.cpu, "apple")
        .into_iter()
        .filter(|c| n(c, "min_macos") as u32 <= major)
        .collect();
    let intel: Vec<&Value> = arr(&cat.cpu, "intel_mac")
        .into_iter()
        .filter(|c| n(c, "max_macos") as u32 >= major)
        .collect();
    let use_intel = !intel.is_empty() && (chips.is_empty() || rng.chance(20));
    let (arch, vendor, gpu, model_key, threads, mem) = if use_intel {
        let m = rng.pick(&intel).ok_or_else(|| err("intel mac"))?;
        (
            "x86_64",
            "Intel",
            s(m, "gpu").to_owned(),
            s(m, "screen").to_owned(),
            rng.pick_plain(&m["threads"]),
            rng.pick_plain(&m["device_memory"]),
        )
    } else {
        let c = rng.pick(&chips).ok_or_else(|| err("apple chip"))?;
        let model = rng
            .pick_plain(&c["models"])
            .ok_or_else(|| err("mac model"))?;
        (
            "arm64",
            "Apple",
            format!("Apple {}", s(c, "chip")),
            model.as_str().unwrap_or("").to_owned(),
            rng.pick_plain(&c["threads"]),
            rng.pick_plain(&c["device_memory"]),
        )
    };
    let sc = &cat.screen["macos"][model_key.as_str()];
    if sc.is_null() {
        return Err(err(&format!("no mac screen {model_key}")));
    }
    let (w, h, dpr) = (n(sc, "width") as u32, n(sc, "height") as u32, n(sc, "dpr"));
    let menu = n(sc, "menu_bar") as u32;
    let laptop = sc.get("laptop").and_then(Value::as_bool).unwrap_or(true);
    let renderer = s(&cat.gpu, "macos_renderer")
        .replace("{vendor}", vendor)
        .replace("{gpu}", &gpu);
    Ok(Machine {
        arch: arch.into(),
        backend: "metal".into(),
        vendor: format!("Google Inc. ({vendor})"),
        gpu_label: gpu,
        renderer,
        threads: threads
            .and_then(|v| v.as_u64())
            .ok_or_else(|| err("threads"))?,
        device_memory: mem.and_then(|v| v.as_f64()).ok_or_else(|| err("memory"))?,
        screen: (w, h),
        dpr,
        avail: (w, h - menu),
        avail_origin: (0, menu),
        outer: (w, h - menu),
        color_depth: n(sc, "color_depth") as u32,
        gamut: s(sc, "gamut").to_owned(),
        hdr: sc.get("hdr").and_then(Value::as_bool).unwrap_or(false),
        // Overlay scrollbars with a trackpad; legacy 15 px with a mouse.
        scrollbar: if laptop { 0 } else { 15 },
        laptop,
        pointing_device: if laptop { None } else { Some("mouse") },
    })
}

fn linux_machine(cat: &Catalog, release: &Value, rng: &mut Rng) -> Result<Machine, String> {
    let form = pick_form(rng);
    let gpus: Vec<&Value> = arr(&cat.gpu, "linux")
        .into_iter()
        .filter(|g| s(g, "form") == form)
        .collect();
    let gpu = rng.pick(&gpus).ok_or_else(|| err("linux gpu"))?;
    let screens: Vec<&Value> = arr(&cat.screen, "linux")
        .into_iter()
        .filter(|x| s(x, "form") == form)
        .collect();
    let sc = rng.pick(&screens).ok_or_else(|| err("linux screen"))?;
    let (w, h, dpr) = (n(sc, "width") as u32, n(sc, "height") as u32, n(sc, "dpr"));
    let top = n(release, "top_bar_px") as u32;
    let dock = n(release, "dock_px") as u32;
    let wayland = s(&release["os"], "display_server") == "wayland";
    let renderer = s(gpu, "renderer")
        .replace("{llvm}", s(release, "llvm"))
        .replace("{drm}", s(release, "drm"))
        .replace("{kernel}", s(&release["os"], "kernel"));
    let cpu = &cat.cpu[form];
    // X11: the work area excludes the top bar and the dock. Wayland: Chrome
    // sees no work area (avail = screen); the window still sits below them.
    let (avail, origin) = if wayland {
        ((w, h), (0, 0))
    } else {
        ((w - dock, h - top), (dock, top))
    };
    Ok(Machine {
        arch: "x86_64".into(),
        backend: s(gpu, "backend").to_owned(),
        vendor: format!("Google Inc. ({})", s(gpu, "vendor")),
        gpu_label: gpu_label(&renderer),
        renderer,
        threads: rng
            .pick_pair(&cpu["threads"])
            .and_then(|v| v.as_u64())
            .ok_or_else(|| err("threads"))?,
        device_memory: rng
            .pick_pair(&cpu["device_memory"])
            .and_then(|v| v.as_f64())
            .ok_or_else(|| err("memory"))?,
        screen: (w, h),
        dpr,
        avail,
        avail_origin: origin,
        outer: (w - dock, h - top),
        color_depth: 24,
        gamut: "srgb".into(),
        hdr: false,
        scrollbar: 15,
        laptop: form == "laptop",
        pointing_device: None,
    })
}

#[cfg(test)]
mod tests {
    use super::gpu_label;

    #[test]
    fn labels() {
        assert_eq!(
            gpu_label(
                "ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 (0x00002504) Direct3D11 vs_5_0 ps_5_0, D3D11)"
            ),
            "NVIDIA GeForce RTX 3060"
        );
        assert_eq!(
            gpu_label("ANGLE (Intel, Mesa Intel(R) UHD Graphics 630 (CFL GT2), OpenGL 4.6)"),
            "Intel(R) UHD Graphics 630"
        );
        assert_eq!(
            gpu_label(
                "ANGLE (AMD, AMD Radeon RX 6600 (radeonsi, navi23, LLVM 19.1.1, DRM 3.57, 6.8.0-87-generic), OpenGL 4.6)"
            ),
            "AMD Radeon RX 6600"
        );
    }
}
