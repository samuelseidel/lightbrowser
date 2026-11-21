//! The fingerprint-report CI gate: for every Chrome reference persona the
//! live surface (on the CLI's default `about:blank` page and on a loopback
//! http page) matches the persona. Included from `pages.rs`.

use std::sync::Arc;
use std::time::Duration;

use lb_page::{BrowserContext, WaitUntil};
use lb_persona::PersonaProfile;

use super::common::{Resource, browser, serve};

fn reference_personas() -> Vec<std::path::PathBuf> {
    let dir = format!(
        "{}/../../profiles/reference/chrome",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut v: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn reference_chrome_personas_match_their_live_surface() {
    let tls =
        std::path::PathBuf::from(format!("{}/../../profiles/tls", env!("CARGO_MANIFEST_DIR")));
    let srv = serve(vec![(
        "/",
        Resource::html("<!DOCTYPE html><title>report</title><p>hello</p>"),
    )])
    .await;
    let personas = reference_personas();
    assert!(!personas.is_empty());
    for path in personas {
        let profile = Arc::new(PersonaProfile::from_path_checked(&path).unwrap());
        let opts = lb_page::context_options_for(&profile, &tls, false, false).unwrap();
        let ctx = BrowserContext::new(opts).unwrap();
        let b = browser();
        for url in ["about:blank".to_string(), srv.url("/")] {
            let page = b.new_page(&ctx).await.unwrap();
            page.goto(&url, WaitUntil::Load, Duration::from_secs(20))
                .await
                .unwrap();
            let r = page.fingerprint_report().await.unwrap();
            let bad: Vec<&serde_json::Value> = r["checks"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| c["status"] == "mismatch")
                .collect();
            assert!(
                bad.is_empty() && r["ok"] == true,
                "{} on {url}: {bad:#?}",
                path.display()
            );
            assert_eq!(r["persona"], profile.profile_id.as_str());
            assert!(
                r["summary"]["ok"].as_u64().unwrap() > 80,
                "{:#}",
                r["summary"]
            );
            page.close().await.unwrap();
        }
    }
}
