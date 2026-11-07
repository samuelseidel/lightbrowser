//! Performance Timeline as in Chrome 153 (ADR 0046): Navigation, Resource,
//! Paint and User Timing entries, `PerformanceObserver`, and the timelines
//! of workers. Each case in `cases/` is served on loopback by `serve.py`
//! (two ports, two origins; the page gets the second as `?x=PORT`;
//! `<!--LOG-->` becomes `prelude.html`) and its log must equal what Chrome
//! 153 printed for the same page, in `chrome.json`. The logs hold names,
//! key sets and relations between values, never absolute times. Re-record
//! with `python3 serve.py 18991 18992 &` and
//! `node chrome.mjs http://127.0.0.1:18991 18992 $(ls cases) > chrome.json`
//! (Chrome's recording ran on ports 19281 and 19282). Included from
//! `pages.rs`.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use lb_page::WaitUntil;

use super::common::{browser, context, open};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/perf")
}

/// `serve.py` on two free loopback ports, stopped on drop.
pub struct PyServer {
    child: Child,
    pub ports: (u16, u16),
}

impl PyServer {
    pub fn start() -> PyServer {
        let mut child = Command::new("python3")
            .arg(dir().join("serve.py"))
            .args(["0", "0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("python3 serve.py");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let mut it = line.split_whitespace().map(|p| p.parse().expect("port"));
        let ports = (it.next().unwrap(), it.next().unwrap());
        PyServer { child, ports }
    }
}

impl Drop for PyServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Chrome's log for `case`.
fn chrome(case: &str) -> String {
    let all: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir().join("chrome.json")).unwrap()).unwrap();
    all[case].as_str().unwrap_or_default().to_owned()
}

/// The ports of this run replaced by the ones Chrome's recording used.
fn normalize(log: &str, ports: (u16, u16)) -> String {
    let mut out = log.to_owned();
    for (p, rec) in [(ports.0, 19281), (ports.1, 19282)] {
        for pre in [":", "%3A", "x="] {
            out = out.replace(&format!("{pre}{p}"), &format!("{pre}{rec}"));
        }
    }
    out
}

async fn run(case: &str) {
    let srv = PyServer::start();
    let (b, ctx) = (browser(), context(false));
    let url = format!("http://127.0.0.1:{}/{case}/?x={}", srv.ports.0, srv.ports.1);
    let page = open(&b, &ctx, &url, WaitUntil::Load).await;
    let t0 = Instant::now();
    let got = loop {
        // The page may be navigating (reload, history): no log then.
        let log = match page.evaluate("L.join('\\n')").await {
            Ok(lb_js::JsValue::String(s)) => s,
            _ => String::new(),
        };
        if log.ends_with("\nend") || log == "end" || t0.elapsed() > Duration::from_secs(15) {
            break log;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let got = normalize(&got, srv.ports);
    let want = chrome(case);
    if got != want {
        // The lines that differ, Chrome's first.
        let (g, w): (Vec<_>, Vec<_>) = (got.lines().collect(), want.lines().collect());
        let mut diff = String::new();
        for i in 0..g.len().max(w.len()) {
            let (a, b) = (
                g.get(i).copied().unwrap_or("<none>"),
                w.get(i).copied().unwrap_or("<none>"),
            );
            if a != b {
                diff.push_str(&format!("line {i}\n  chrome: {b}\n  lb:     {a}\n"));
            }
        }
        panic!("{case} differs from Chrome:\n{diff}");
    }
}

macro_rules! cases {
    ($($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() {
                run(stringify!($name)).await;
            }
        )*
    };
}

cases!(
    nav_entry,
    nav_reload,
    nav_redirect,
    observer,
    paint,
    res_buffer,
    res_conn,
    res_redirect,
    res_sizes,
    res_types,
    timeline,
    user_timing,
    worker_dedicated,
    worker_service,
    worker_shared,
);
