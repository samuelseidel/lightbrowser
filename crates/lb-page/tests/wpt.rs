//! WPT subset runner: serves `tests/wpt` on loopback, loads every test in
//! `MANIFEST.txt` in a page, and collects the testharness results that
//! `resources/testharnessreport.js` prints to the console.
#![cfg(feature = "chrome")]

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Resource, browser, context, serve_fn};

/// Passing subtests at the time of writing; the run fails below this.
const BASELINE_PASSES: usize = 10400;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/wpt")
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("xml") => "application/xml",
        Some("xhtml") => "application/xhtml+xml",
        Some("css") => "text/css",
        Some("json") => "application/json",
        _ => "text/plain",
    }
}

/// `.any.html` wrapper for a `.any.js` test (window scope).
fn any_wrapper(js_path: &str, src: &str) -> String {
    let mut scripts = String::new();
    for line in src.lines() {
        if let Some(s) = line.trim().strip_prefix("// META: script=") {
            scripts.push_str(&format!("<script src=\"{}\"></script>\n", s.trim()));
        }
    }
    let file = js_path.rsplit('/').next().unwrap_or(js_path);
    format!(
        "<!doctype html>\n<meta charset=utf-8>\n<script src=\"/resources/testharness.js\"></script>\n\
         <script src=\"/resources/testharnessreport.js\"></script>\n{scripts}<div id=log></div>\n<script src=\"{file}\"></script>\n"
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wpt_subset() {
    let base = root();
    let base2 = base.clone();
    let server = serve_fn(Arc::new(move |path: &str| {
        let rel = path.trim_start_matches('/');
        if let Some(js) = rel.strip_suffix(".any.html") {
            let js = format!("{js}.any.js");
            let src = std::fs::read_to_string(base2.join(&js)).ok()?;
            return Some(Resource::html(&any_wrapper(&js, &src)));
        }
        if rel.contains("..") {
            return None;
        }
        let bytes = std::fs::read(base2.join(rel)).ok()?;
        Some(Resource::new(content_type(rel), bytes))
    }))
    .await;
    let manifest = std::fs::read_to_string(base.join("MANIFEST.txt")).unwrap();
    let tests: Vec<String> = manifest
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            l.strip_suffix(".any.js")
                .map_or_else(|| l.to_owned(), |s| format!("{s}.any.html"))
        })
        .filter(|l| std::env::var("WPT_FILTER").map_or(true, |f| l.contains(&f)))
        .collect();

    let b = browser();
    let ctx = context(true);
    let (mut pass, mut total, mut files_ok, mut harness_errors) = (0usize, 0usize, 0usize, 0usize);
    let mut lines = Vec::new();
    for t in &tests {
        if std::env::var_os("WPT_VERBOSE").is_some() {
            eprintln!("running {t}");
        }
        let page = b.new_page(&ctx).await.unwrap();
        let url = server.url(&format!("/{t}"));
        let nav = page.navigate(&url).await;
        let mut result: Option<serde_json::Value> = None;
        if nav.is_ok() {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(30) {
                let msgs = page.console_messages().await.unwrap_or_default();
                if let Some(m) = msgs.iter().find(|m| m.text.starts_with("WPT_RESULT ")) {
                    result = serde_json::from_str(&m.text["WPT_RESULT ".len()..]).ok();
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        let (p, n, status) = match &result {
            Some(r) => {
                let subs = r["tests"].as_array().cloned().unwrap_or_default();
                let p = subs.iter().filter(|s| s["status"] == 0).count();
                (p, subs.len(), r["status"].as_u64().unwrap_or(1))
            }
            None => (0, 0, 2),
        };
        pass += p;
        total += n;
        if status != 0 {
            harness_errors += 1;
        }
        if n > 0 && p == n && status == 0 {
            files_ok += 1;
        }
        let note = match (&result, status) {
            (None, _) => " (no result)".to_owned(),
            (Some(r), 1) => format!(" (harness error: {})", r["message"].as_str().unwrap_or("")),
            (Some(_), 2) => " (harness timeout)".to_owned(),
            _ => String::new(),
        };
        lines.push(format!("{p:4}/{n:<4} {t}{note}"));
        if std::env::var_os("WPT_VERBOSE").is_some()
            && let Some(r) = &result
        {
            for s in r["tests"].as_array().into_iter().flatten() {
                if s["status"] != 0 {
                    lines.push(format!("          FAIL {}: {}", s["name"], s["message"]));
                }
            }
        }
        let _ = page.close().await;
    }
    for l in &lines {
        println!("{l}");
    }
    println!(
        "WPT subset: {pass}/{total} subtests pass, {files_ok}/{} files fully pass, {harness_errors} harness errors/timeouts",
        tests.len()
    );
    assert!(
        pass >= BASELINE_PASSES,
        "WPT passes dropped: {pass} < {BASELINE_PASSES}"
    );
}
