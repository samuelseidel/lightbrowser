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

static mut ICU: (usize, usize) = (0, 0);
static TOUCHED: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());
static mut PAGES: [u8; 4096] = [0; 4096];

extern "C" fn on_segv(_sig: i32, info: *mut libc::siginfo_t, _ctx: *mut libc::c_void) {
    unsafe {
        let addr = (*info).si_addr() as usize;
        let (start, len) = ICU;
        if addr >= start && addr < start + len {
            let page = (addr - start) / 4096;
            PAGES[page] = 1;
            libc::mprotect((start + page * 4096) as *mut libc::c_void, 4096, libc::PROT_READ);
            return;
        }
        libc::signal(libc::SIGSEGV, libc::SIG_DFL);
    }
}

fn icu_trace_start() {
    let d = lb_js::icu_data::data();
    let start = (d.as_ptr() as usize).div_ceil(4096) * 4096;
    let end = (d.as_ptr() as usize + d.len()) / 4096 * 4096;
    unsafe {
        ICU = (start, end - start);
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_segv as usize;
        sa.sa_flags = libc::SA_SIGINFO;
        libc::sigaction(libc::SIGSEGV, &sa, std::ptr::null_mut());
        libc::mprotect(start as *mut libc::c_void, end - start, libc::PROT_NONE);
    }
    let _ = &TOUCHED;
}

fn icu_trace_report() {
    unsafe {
        let (_, len) = ICU;
        let n = len / 4096;
        let touched: Vec<usize> = (0..n).filter(|&i| PAGES[i] == 1).collect();
        eprintln!("ICU pages touched: {} of {} ({:.1} KB)", touched.len(), n, touched.len() as f64 * 4.0);
        let mut runs = Vec::new();
        let mut i = 0;
        while i < touched.len() {
            let s = touched[i];
            let mut e = s;
            while i + 1 < touched.len() && touched[i + 1] == e + 1 { i += 1; e += 1; }
            runs.push(format!("{}-{}", s * 4096, (e + 1) * 4096));
            i += 1;
        }
        eprintln!("runs: {}", runs.join(" "));
    }
}

fn main() {
    if std::env::var("PROBE_ICU").is_ok() {
        icu_trace_start();
    }
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
    if std::env::var("PROBE_ICU").is_ok() {
        icu_trace_report();
    }
}
