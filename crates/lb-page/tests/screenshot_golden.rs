//! `Page.captureScreenshot` against Chrome 153 (Chrome for Testing
//! 153.0.8010.12) goldens: CSS background images (PNG, JPEG, WebP, GIF),
//! gradients, background size / position / repeat / origin / clip / layers,
//! border radius, box shadows and the root background on the canvas.
//! The pages in `screenshots/pages` place fixed-size boxes absolutely (no
//! text), so geometry does not depend on our approximate inline layout.
//! Goldens: `node screenshots/chrome_shots.mjs http://127.0.0.1:PORT
//! screenshots/chrome` with the pages served on loopback.
#![cfg(feature = "chrome")]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{Resource, serve_fn};
use lb_cdp::{ContextOptions, EvaluateRequest, NavigateRequest, PageHost};
use lb_page::{CdpHost, CdpHostOptions};
use lb_persona::PersonaProfile;
use serde_json::json;

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/screenshots")
}

fn host() -> CdpHost {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    // A DPR 1 persona, like Chrome's default screenshots.
    let p = PersonaProfile::from_path_checked(
        root.join("reference/chrome/chrome153_win10_rtx3060_1080p.json"),
    )
    .expect("reference persona");
    let mut opts = CdpHostOptions::new(Arc::new(p), root.join("tls"));
    opts.workers = 1;
    CdpHost::new(opts).expect("host")
}

fn route(path: &str) -> Option<Resource> {
    let rel = path.trim_start_matches('/');
    let file = dir().join("pages").join(rel);
    let body = std::fs::read(&file).ok()?;
    let ct = match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    };
    Some(Resource::new(ct, body))
}

/// RGBA pixels of a PNG.
fn rgba(png: &[u8]) -> (u32, u32, Vec<[u8; 4]>) {
    let p = lb_paint::decode::decode(png, Some("image/png")).expect("png");
    let px = p
        .pixels()
        .iter()
        .map(|c| {
            let c = c.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    (p.width(), p.height(), px)
}

/// (mean max-channel difference, share of pixels differing by more than 48).
fn diff(a: &[u8], b: &[u8]) -> (f64, f64) {
    let (aw, ah, ap) = rgba(a);
    let (bw, bh, bp) = rgba(b);
    assert_eq!((aw, ah), (bw, bh), "size");
    let d: Vec<u8> = ap
        .iter()
        .zip(&bp)
        .map(|(x, y)| (0..3).map(|i| x[i].abs_diff(y[i])).max().unwrap_or(0))
        .collect();
    let mean = d.iter().map(|&v| f64::from(v)).sum::<f64>() / d.len() as f64;
    let big = d.iter().filter(|&&v| v > 48).count() as f64 / d.len() as f64;
    (mean, big)
}

#[test]
fn backgrounds_radius_and_shadows_match_chrome() {
    let _ = lb_cdp::block_on_local(async {
        let s = serve_fn(Arc::new(route)).await;
        let h = host();
        h.create_context("C", ContextOptions::default()).unwrap();
        let mut report = Vec::new();
        for (i, name) in ["images", "gradients", "radius_shadow", "root"]
            .iter()
            .enumerate()
        {
            let t = format!("T{i}");
            h.create_page("C", &t).await.unwrap();
            h.navigate(
                &t,
                NavigateRequest {
                    url: s.url(&format!("/{name}.html")),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            // Wait for `load` (background images delay it).
            let ctx = h.main_context(&t).unwrap();
            h.evaluate(
                &t,
                EvaluateRequest {
                    context_id: ctx,
                    expression: "new Promise(r => document.readyState == 'complete' ? r(1) : addEventListener('load', () => r(1)))".into(),
                    return_by_value: true,
                    await_promise: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            let b64 = h
                .capture_screenshot(
                    &t,
                    &json!({"format": "png", "clip": {"x": 0, "y": 0, "width": 400, "height": 300, "scale": 1}}),
                )
                .unwrap();
            let ours = lb_paint::encode::base64_decode(&b64).unwrap();
            let golden = std::fs::read(dir().join(format!("chrome/{name}.png"))).unwrap();
            let (mean, big) = diff(&ours, &golden);
            report.push(format!("{name}: mean {mean:.2}, >48: {:.2}%", big * 100.0));
            // Edges (anti-aliasing, JPEG decoding, blur) differ a little.
            assert!(mean < 1.5 && big < 0.015, "{name}: {}", report.join("; "));
        }
        eprintln!("{}", report.join("\n"));
    });
}
