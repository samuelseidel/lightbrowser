//! Temporary profiling probe (M6 Lightpanda work): times each phase of a
//! `fetch`-like page load and prints RSS and V8 heap stats.
use std::sync::Arc;
use std::time::{Duration, Instant};

fn rss() -> String {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let get = |k: &str| {
        s.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
            / 1024.0
    };
    format!("rss {:.1} hwm {:.1}", get("VmRSS:"), get("VmHWM:"))
}

fn main() {
    let t0 = Instant::now();
    let lap = |what: &str| eprintln!("{:8.1} ms  {what:30} {}", t0.elapsed().as_secs_f64() * 1e3, rss());
    if let Ok(out) = std::env::var("PROBE_DUMP") {
        let t = Instant::now();
        let blob = lb_webapi::snapshot::startup_snapshot(lb_persona::BrowserFamily::Chrome).unwrap();
        eprintln!("snapshot {} bytes in {:.1} ms", blob.len(), t.elapsed().as_secs_f64() * 1e3);
        std::fs::write(out, blob).unwrap();
        return;
    }
    let url = std::env::args().nth(1).expect("url");
    let snapshot = std::env::var("PROBE_SNAPSHOT").is_ok();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    let full = Arc::new(
        lb_persona::PersonaProfile::from_path_checked(&root.join("reference/chrome/chrome153_win10_rtx3060_1080p.json")).unwrap(),
    );
    let net = full.to_network();
    let tls = lb_net::ClientHelloProfile::load(&root.join(format!("tls/{}.json", net.tls_profile_id))).unwrap();
    lap("persona loaded");
    let ctx = lb_page::BrowserContext::new(lb_page::ContextOptions {
        persona: Arc::new(net),
        profile: Some(full),
        tls_profile: Arc::new(tls),
        accept_invalid_certs: false,
        virtual_time: false,
    })
    .unwrap();
    lap("context");
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let browser = lb_page::Browser::new(lb_page::BrowserOptions {
            workers: 1,
            startup_snapshot: snapshot,
            ..Default::default()
        })
        .unwrap();
        lap("browser");
        let page = browser.new_page(&ctx).await.unwrap();
        lap("new_page");
        let st = page.run(|h| h.engine.heap_stats()).await.unwrap();
        eprintln!("          heap used {:.1} MB total {:.1} MB", st.used as f64 / 1e6, st.total as f64 / 1e6);
        let _nav = page.navigate(&url).await.unwrap();
        lap("navigate (committed)");
        page.wait_for(lb_page::WaitUntil::Load, Duration::from_secs(30)).await.unwrap();
        lap("load");
        let html = page.content().await.unwrap();
        lap("content");
        let st = page.run(|h| h.engine.heap_stats()).await.unwrap();
        eprintln!("          heap used {:.1} MB total {:.1} MB; html {} bytes", st.used as f64 / 1e6, st.total as f64 / 1e6, html.len());
        let st = page.run(|h| { h.engine.low_memory_notification(); h.engine.heap_stats() }).await.unwrap();
        eprintln!("          after GC: heap used {:.1} MB total {:.1} MB", st.used as f64 / 1e6, st.total as f64 / 1e6);
        lap("gc");
        let _ = page.close().await;
        lap("closed");
    });
}
