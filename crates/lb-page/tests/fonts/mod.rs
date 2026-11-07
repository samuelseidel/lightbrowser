//! `FontFace` and `local()` faces per persona OS release. Included from
//! `pages.rs`.
//!
//! Chrome recordings (`chrome.mjs`, loopback): CreepJS's font probe
//! (`creep.html`) and the `FontFace` cases (`cases.html`: family
//! serialization, `src` parsing, `local()` names), by Chrome 153 headless
//! shell
//! - on macOS 26.4.1 (`chrome_macos26.json`),
//! - on Ubuntu 24.04.4 with ubuntu-desktop-minimal (`chrome_ubuntu24.json`,
//!   rtx-dev).
//!
//! The macOS 26 and Ubuntu 24.04 personas must give exactly that. Windows,
//! Fedora and macOS 14/15 cannot be recorded here: their results follow
//! from CreepJS's own per-version lists (`creep.html`) and the face lists
//! in `profiles/local_fonts/` (see its README for where each comes from).

use std::sync::Arc;

use lb_js::JsValue;
use lb_page::WaitUntil;
use lb_persona::PersonaProfile;

use super::common::{Resource, browser, open, serve};

const MAC26: (&str, &str) = (
    "chrome153_macos26_apple_m4_1512x982.json",
    include_str!("chrome_macos26.json"),
);
const UBUNTU24: (&str, &str) = (
    "chrome153_ubuntu24_intel_uhd630_x11_1080p.json",
    include_str!("chrome_ubuntu24.json"),
);

fn persona(file: &str) -> Arc<PersonaProfile> {
    let profiles = concat!(env!("CARGO_MANIFEST_DIR"), "/../../profiles");
    Arc::new(
        PersonaProfile::from_path_checked(format!("{profiles}/reference/chrome/{file}")).unwrap(),
    )
}

/// Loads `html` with the persona and returns `window.R` once set.
async fn run(file: &str, html: &str) -> String {
    let srv = serve(vec![("/", Resource::html(html))]).await;
    let p = persona(file);
    let tls = concat!(env!("CARGO_MANIFEST_DIR"), "/../../profiles/tls");
    let ctx = lb_page::BrowserContext::new(
        lb_page::context_options_for(&p, std::path::Path::new(tls), false, true).unwrap(),
    )
    .unwrap();
    let b = browser();
    let page = open(&b, &ctx, &srv.url("/"), WaitUntil::Load).await;
    for _ in 0..200 {
        if let JsValue::String(s) = page.evaluate("window.R").await.unwrap() {
            return s;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("{file}: window.R not set");
}

/// CreepJS's font probe gives what Chrome gave on the persona's OS: on
/// macOS 26 the 18 macOS faces load, on Ubuntu 24.04 DejaVu Sans,
/// Liberation Mono, Noto Color Emoji and four Noto script faces;
/// `FontFace.family` reads back quoted, so CreepJS's own OS guess is
/// `undefined` (it read "macOS Mavericks" before: unquoted families and
/// only 2 faces).
#[tokio::test]
async fn creepjs_probe_matches_chrome() {
    for (file, rec) in [MAC26, UBUNTU24] {
        let chrome: serde_json::Value = serde_json::from_str(rec).unwrap();
        let got: serde_json::Value =
            serde_json::from_str(&run(file, include_str!("creep.html")).await).unwrap();
        assert_eq!(got, chrome["creep"], "{file}");
    }
}

/// Family serialization, `src` parsing and `local()` names, line by line
/// as Chrome 153 on macOS 26 and on Ubuntu 24.04.
#[tokio::test]
async fn font_face_cases_match_chrome() {
    for (file, rec) in [MAC26, UBUNTU24] {
        let chrome: serde_json::Value = serde_json::from_str(rec).unwrap();
        let want: Vec<&str> = chrome["cases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let got = run(file, include_str!("cases.html")).await;
        let got: Vec<&str> = got.split('\n').collect();
        for (g, w) in got.iter().zip(&want) {
            assert_eq!(g, w, "{file}");
        }
        assert_eq!(got.len(), want.len(), "{file}");
    }
}

/// Every reference persona: CreepJS's per-version face lists, applied to
/// the names that load, give the persona's OS release (macOS 13 and later
/// are all "Ventura", CreepJS's newest; Linux has no versions in CreepJS
/// and is classified by Noto Color Emoji). CreepJS's own result stays
/// `undefined` everywhere, as in Chrome (quoted families).
#[tokio::test]
async fn creepjs_versions_follow_the_persona_os() {
    let cases = [
        ("chrome153_win10_rtx3060_1080p.json", Some("Windows 10")),
        (
            "chrome153_win11_irisxe_laptop_1536x864.json",
            Some("Windows 11"),
        ),
        ("chrome153_win11_rtx4060_1080p.json", Some("Windows 11")),
        (
            "chrome153_macos14_intel_irisplus_1440x900.json",
            Some("macOS Ventura"),
        ),
        (
            "chrome153_macos15_apple_m2_retina.json",
            Some("macOS Ventura"),
        ),
        (
            "chrome153_macos26_apple_m4_1512x982.json",
            Some("macOS Ventura"),
        ),
        ("chrome153_ubuntu24_intel_uhd630_x11_1080p.json", None),
        ("chrome153_fedora42_amd_rx6600_wayland_1440p.json", None),
    ];
    for (file, want) in cases {
        let r: serde_json::Value =
            serde_json::from_str(&run(file, include_str!("creep.html")).await).unwrap();
        assert_eq!(r["byNameVersion"].as_str(), want, "{file}: {r}");
        assert!(r["platformVersion"].is_null(), "{file}: {r}");
        let by_name: Vec<&str> = r["byName"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let linux = file.contains("ubuntu") || file.contains("fedora");
        assert_eq!(by_name.contains(&"Noto Color Emoji"), linux, "{file}");
        // CreepJS's "Like Windows" test (Lucida Console) only on Windows.
        assert_eq!(
            by_name.contains(&"Lucida Console"),
            file.contains("win"),
            "{file}"
        );
    }
}

/// `local()` per release: faces new in a release load only from it on;
/// variable fonts on Linux load by instance name only ("Ubuntu Sans
/// Regular", not "Ubuntu Sans", as recorded on Ubuntu 24.04).
#[tokio::test]
async fn local_faces_follow_the_release() {
    let names = [
        "Segoe Fluent Icons",
        "Segoe UI Variable Text Regular",
        "Cascadia Code Regular",
        "HoloLens MDL2 Assets",
        "Sitka Banner",
        "SitkaBanner",
        "Calibri Light",
        "Calibri-Light",
        "Consolas",
        "Kefa III Regular Regular",
        "Kefa Regular",
        "Noto Sans Syriac Regular Bold",
        "Noto Sans Syriac Regular",
        "PingFang MO Regular",
        "Menlo Regular",
        "Ubuntu Sans",
        "Ubuntu Sans Regular",
        "Ubuntu Mono",
        "Ubuntu Mono Regular",
        "DejaVu Sans Mono Oblique",
        "FreeSans",
        "Noto Sans",
        "Noto Sans Regular",
        "Noto Sans CJK SC",
        "Adwaita Sans Regular",
        "Cantarell Regular",
        "LiberationMono",
        "C059-Roman",
        "system-ui",
    ];
    let probe = format!(
        "<body><script>window.R = null; const names = {};\
         Promise.all(names.map(n => new FontFace('f', `local(\"${{n}}\")`).load().then(() => n, () => null)))\
           .then(v => {{ R = v.filter(Boolean).join(); }});</script></body>",
        serde_json::to_string(&names).unwrap()
    );
    let cases = [
        (
            "chrome153_win10_rtx3060_1080p.json",
            "HoloLens MDL2 Assets,Sitka Banner,SitkaBanner,Calibri Light,Calibri-Light,Consolas",
        ),
        (
            "chrome153_win11_rtx4060_1080p.json",
            "Segoe Fluent Icons,Segoe UI Variable Text Regular,Cascadia Code Regular,\
             HoloLens MDL2 Assets,Sitka Banner,Calibri Light,Calibri-Light,Consolas",
        ),
        (
            "chrome153_macos14_intel_irisplus_1440x900.json",
            "Kefa Regular,Noto Sans Syriac Regular,Menlo Regular",
        ),
        (
            "chrome153_macos15_apple_m2_retina.json",
            "Kefa Regular,Noto Sans Syriac Regular Bold,PingFang MO Regular,Menlo Regular",
        ),
        (
            "chrome153_macos26_apple_m4_1512x982.json",
            "Kefa III Regular Regular,Noto Sans Syriac Regular Bold,PingFang MO Regular,\
             Menlo Regular",
        ),
        (
            "chrome153_ubuntu24_intel_uhd630_x11_1080p.json",
            "Noto Sans Syriac Regular,Ubuntu Sans Regular,Ubuntu Mono Regular,\
             DejaVu Sans Mono Oblique,Noto Sans Regular,Noto Sans CJK SC,LiberationMono,\
             C059-Roman,system-ui",
        ),
        (
            "chrome153_fedora42_amd_rx6600_wayland_1440p.json",
            "Noto Sans Regular,Adwaita Sans Regular,LiberationMono,C059-Roman,system-ui",
        ),
    ];
    for (file, want) in cases {
        assert_eq!(run(file, &probe).await, want, "{file}");
    }
}
