//! WebGL rendering through real pages (ADR 0030): the scenes of
//! `lb-gl/tests/chrome/scenes.html` stay close to Chrome for Testing 153's
//! SwiftShader output (`chrome_scenes.json`, recorded with `scenes.mjs`),
//! and a gradient fingerprint is stable per persona seed and differs
//! between the reference personas.
#![cfg(feature = "chrome")]

mod common;

use std::sync::Arc;

use common::{Resource, browser, open, serve};
use lb_js::JsValue;
use lb_net::ClientHelloProfile;
use lb_page::{BrowserContext, ContextOptions, WaitUntil};
use lb_persona::{Persona, PersonaProfile};

const SCENES: &str = include_str!("../../lb-gl/tests/chrome/scenes.html");
const CHROME: &str = include_str!("../../lb-gl/tests/chrome/chrome_scenes.json");
const API: &str = include_str!("../../lb-gl/tests/chrome/api.html");
const CHROME_API: &str = include_str!("../../lb-gl/tests/chrome/chrome_api.json");

fn ctx(persona_file: &str, seed: Option<&str>) -> BrowserContext {
    let persona = Arc::new(Persona::default_chrome());
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../profiles");
    let tls = Arc::new(
        ClientHelloProfile::load(std::path::Path::new(&format!(
            "{dir}/tls/{}.json",
            persona.tls_profile_id
        )))
        .unwrap(),
    );
    let mut p =
        PersonaProfile::from_path(format!("{dir}/reference/chrome/{persona_file}")).unwrap();
    if let Some(s) = seed {
        p.fingerprint_seeds.webgl_noise_seed = s.to_owned();
    }
    BrowserContext::new(ContextOptions {
        persona,
        profile: Some(Arc::new(p)),
        tls_profile: tls,
        accept_invalid_certs: false,
        virtual_time: true,
    })
    .unwrap()
}

async fn scenes(persona_file: &str, seed: Option<&str>) -> serde_json::Value {
    run_page(SCENES, persona_file, seed).await
}

async fn run_page(html: &str, persona_file: &str, seed: Option<&str>) -> serde_json::Value {
    let s = serve(vec![("/", Resource::html(html))]).await;
    let b = browser();
    let c = ctx(persona_file, seed);
    let page = open(&b, &c, &s.url("/"), WaitUntil::Load).await;
    match page.evaluate("JSON.stringify(window.OUT)").await.unwrap() {
        JsValue::String(s) => serde_json::from_str(&s).unwrap(),
        other => panic!("{other:?}"),
    }
}

fn pixels(v: &serde_json::Value) -> Vec<u8> {
    let s = v["b64"].as_str().expect("b64").as_bytes();
    let val = |c: u8| -> u32 {
        match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            _ => 63,
        }
    };
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    for q in s.chunks(4) {
        let pad = q.iter().filter(|c| **c == b'=').count();
        let n = q.iter().fold(0u32, |acc, c| {
            (acc << 6) | if *c == b'=' { 0 } else { val(*c) }
        });
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..3 - pad]);
    }
    out
}

#[tokio::test]
async fn scenes_are_close_to_chrome_swiftshader() {
    let chrome: serde_json::Value = serde_json::from_str(CHROME).unwrap();
    let lb = scenes("chrome153_win10_rtx3060_1080p.json", None).await;
    for (name, c) in chrome.as_object().unwrap() {
        let l = &lb[name];
        assert!(l["error"].is_null(), "{name}: {}", l["error"]);
        assert_eq!(l["err"], 0, "{name}: GL error");
        let (a, b) = (pixels(c), pixels(l));
        assert_eq!(a.len(), b.len(), "{name}");
        let sum: u64 = a
            .iter()
            .zip(&b)
            .map(|(x, y)| u64::from(x.abs_diff(*y)))
            .sum();
        let mean = sum as f64 / a.len() as f64;
        let n = a.len() / 4;
        let big = (0..n)
            .filter(|i| (0..4).any(|k| a[i * 4 + k].abs_diff(b[i * 4 + k]) > 48))
            .count();
        let big = big as f64 / n as f64;
        // `shader_math` hashes `sin` of large arguments, whose precision
        // differs between SwiftShader and Rust's libm: only its red channel
        // (an integer iteration count) is compared.
        if name == "shader_math" {
            let red = (0..n).filter(|i| a[i * 4].abs_diff(b[i * 4]) > 2).count();
            assert_eq!(red, 0, "{name}: red channel");
            continue;
        }
        eprintln!(
            "{name}: mean abs {mean:.2}, {:.1}% of pixels off by more than 48",
            big * 100.0
        );
        assert!(mean < 2.5, "{name}: mean abs difference {mean}");
        assert!(
            big < 0.06,
            "{name}: {:.1}% of pixels differ by more than 48",
            big * 100.0
        );
    }
}

#[tokio::test]
async fn gradient_fingerprint_is_stable_per_seed_and_differs_between_personas() {
    let personas = [
        "chrome153_win10_rtx3060_1080p.json",
        "chrome153_win11_irisxe_laptop_1536x864.json",
        "chrome153_win11_rtx4060_1080p.json",
        "chrome153_macos14_intel_irisplus_1440x900.json",
        "chrome153_macos15_apple_m2_retina.json",
        "chrome153_macos26_apple_m4_1512x982.json",
        "chrome153_ubuntu24_intel_uhd630_x11_1080p.json",
        "chrome153_fedora42_amd_rx6600_wayland_1440p.json",
    ];
    let mut hashes = Vec::new();
    for p in personas {
        let r = scenes(p, None).await;
        let h: Vec<String> = ["gradient_aa", "fingerprintjs_like"]
            .iter()
            .map(|k| format!("{:x}", md5_like(&pixels(&r[*k]))))
            .collect();
        hashes.push(h.join(":"));
    }
    let again = scenes(personas[0], None).await;
    let h0: Vec<String> = ["gradient_aa", "fingerprintjs_like"]
        .iter()
        .map(|k| format!("{:x}", md5_like(&pixels(&again[*k]))))
        .collect();
    assert_eq!(hashes[0], h0.join(":"), "same persona, same pixels");
    for i in 0..hashes.len() {
        for j in i + 1..hashes.len() {
            assert_ne!(
                hashes[i], hashes[j],
                "{} and {} render alike",
                personas[i], personas[j]
            );
        }
    }
    // Another seed on the same persona changes the output too.
    let other = scenes(personas[0], Some("0123456789abcdef0123456789abcdef")).await;
    let h1: Vec<String> = ["gradient_aa", "fingerprintjs_like"]
        .iter()
        .map(|k| format!("{:x}", md5_like(&pixels(&other[*k]))))
        .collect();
    assert_ne!(hashes[0], h1.join(":"));
}

/// FNV-1a 64 (a stable hash; no extra dependency).
fn md5_like(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for x in b {
        h ^= u64::from(*x);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Errors, queries, uniform round trips, framebuffer status and
/// readPixels results equal Chrome's (`chrome_api.json`, `api.mjs`).
#[tokio::test]
async fn api_results_equal_chrome() {
    let chrome: serde_json::Value = serde_json::from_str(CHROME_API).unwrap();
    let lb = run_page(API, "chrome153_ubuntu24_intel_uhd630_x11_1080p.json", None).await;
    let mut diffs = Vec::new();
    for (g, vals) in chrome.as_object().unwrap() {
        for (k, v) in vals.as_object().unwrap() {
            if &lb[g][k] != v {
                diffs.push(format!("{g}.{k}: chrome {v}, lightbrowser {}", lb[g][k]));
            }
        }
    }
    assert!(diffs.is_empty(), "{}", diffs.join("\n"));
}
