//! Web platform APIs as in Chrome 153 (ADR 0025): Web Crypto, `blob:` URLs
//! in `fetch()`/XHR and synchronous XHR, dynamic `import()`, WebSocket.
//! Each case in `cases/` is served on loopback by `serve.py` (the same
//! server Chrome was recorded against, started per test on a free port;
//! `<!--LOG-->` becomes `prelude.html`), and its log must equal what
//! Chrome 153 (Chrome for Testing 153.0.8010.12) printed, in
//! `chrome.json`. Re-record with `python3 serve.py 18961 &` and
//! `node chrome.mjs http://127.0.0.1:18961 $(ls cases | grep -v '\.js$') > chrome.json`.
//! Included from `pages.rs`.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use lb_page::WaitUntil;

use super::common::{browser, context, open};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/apis")
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
    crypto_aes,
    crypto_digest,
    crypto_ec,
    crypto_errors,
    crypto_hmac,
    crypto_kdf,
    crypto_order,
    crypto_random,
    crypto_rsa,
    xhr_blob,
    xhr_sync,
);
