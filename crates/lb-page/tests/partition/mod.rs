//! Storage partitioning as in Chrome 153 (ADR 0037): `localStorage`,
//! `sessionStorage` and storage events (rebrowser's storagePartitioning
//! check), IndexedDB, the Cache API, `BroadcastChannel`, shared workers,
//! service workers, `blob:` URLs, cookies with SameSite and CHIPS
//! (`Partitioned`), and the Storage Access API answers, in frame chains of
//! two sites on one port: A = 127.0.0.1, B = localhost (as rebrowser does).
//!
//! Each case in `cases/` has a `steps` file: the pages to open, in order,
//! each in a new page of one browser context (`A index.html` is
//! `http://127.0.0.1:PORT/<case>/index.html`). `serve.py` serves them on
//! loopback (both sites, started per test on a free port; `<!--LOG-->`
//! becomes `prelude.html`, `/<case>/f.html` is `frame.html`), and the
//! joined step logs must equal what Chromium 153 (153.0.8010.12, the full
//! browser, headless, on Linux) printed, in `chrome.json`. Chrome is
//! started directly, not by Playwright's launcher, whose flags turn
//! `ThirdPartyStoragePartitioning` off. The 153 headless shell logs the
//! same except `access` (it has no permission layer: `prompt` at the top
//! level, `requestStorageAccess()` rejected in frames); Chrome 156 on macOS
//! logs the same as Chromium 153. Re-record with `python3 serve.py 19120 &`
//! and `CHROME=<chromium> node chrome.mjs 19120 $(ls cases) > chrome.json`.
//! Included from `pages.rs`.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use lb_page::WaitUntil;

use super::common::{browser, context, open};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/partition")
}

/// `serve.py` on a free loopback port, stopped on drop.
struct PyServer {
    child: Child,
    port: u16,
}

impl PyServer {
    fn start() -> PyServer {
        let mut child = Command::new("python3")
            .arg(dir().join("serve.py"))
            .arg("0")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("python3 serve.py");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port = line.trim().parse().expect("port");
        PyServer { child, port }
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

async fn run(case: &str) {
    let srv = PyServer::start();
    let (b, ctx) = (browser(), context(false));
    let steps = std::fs::read_to_string(dir().join("cases").join(case).join("steps")).unwrap();
    let mut logs = Vec::new();
    for line in steps.lines().filter(|l| !l.trim().is_empty()) {
        let (site, file) = line.split_once(' ').expect("steps line");
        let host = if site == "B" {
            "localhost"
        } else {
            "127.0.0.1"
        };
        let url = format!("http://{host}:{}/{case}/{file}", srv.port);
        let page = open(&b, &ctx, &url, WaitUntil::Load).await;
        let t0 = Instant::now();
        let log = loop {
            let log = match page.evaluate("L.join('\\n')").await.unwrap() {
                lb_js::JsValue::String(s) => s,
                v => panic!("{v:?}"),
            };
            if log.ends_with("\nend") || log == "end" || t0.elapsed() > Duration::from_secs(15) {
                break log;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        logs.push(log);
        page.close().await.unwrap();
    }
    let got = logs.join("\n--\n").replace(&srv.port.to_string(), "PORT");
    assert_eq!(got, chrome(case), "{case}:\n{got}");
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
    access,
    bc,
    blob,
    caches,
    cookies,
    idb,
    ls,
    service_worker,
    shared_worker,
);
