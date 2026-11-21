//! IndexedDB as in Chrome 153 (ADR 0028): factory, keys and key ranges,
//! key paths and generators, indexes, cursors, transactions (activation,
//! auto-commit, abort, errors), version changes and `blocked`, values,
//! errors, workers.
//! Each case in `cases/` is served on loopback by `serve.py` (the same
//! server Chrome was recorded against, started per test on a free port;
//! `<!--LOG-->` becomes `prelude.html`), and its log must equal what
//! Chrome 153 (Chrome for Testing 153.0.8010.12) printed, in
//! `chrome.json`. Re-record with `python3 serve.py 18981 &` and
//! `node chrome.mjs http://127.0.0.1:18981 $(ls cases) > chrome.json`.
//! Included from `pages.rs`.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use lb_page::WaitUntil;

use super::common::{browser, context, open};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/idb")
}

/// `serve.py` on a free loopback port, stopped on drop.
pub struct PyServer {
    child: Child,
    pub port: u16,
}

impl PyServer {
    pub fn start() -> PyServer {
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
    let url = format!("http://127.0.0.1:{}/{case}/", srv.port);
    let page = open(&b, &ctx, &url, WaitUntil::Load).await;
    let t0 = Instant::now();
    let got = loop {
        let log = match page.evaluate("L.join('\\n')").await.unwrap() {
            lb_js::JsValue::String(s) => s,
            v => panic!("{v:?}"),
        };
        if log.ends_with("\nend") || log == "end" || t0.elapsed() > Duration::from_secs(10) {
            break log;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    // Logs may name the server's port (Chrome's was recorded on 18981).
    let got = got.replace(&format!("127.0.0.1:{}", srv.port), "127.0.0.1:PORT");
    let want = chrome(case).replace("127.0.0.1:18981", "127.0.0.1:PORT");
    assert_eq!(got, want, "{case}:\n{got}");
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
    idb_basic,
    idb_cursor,
    idb_errors,
    idb_index,
    idb_keypath,
    idb_keys,
    idb_txn,
    idb_values,
    idb_version,
    idb_worker,
);
