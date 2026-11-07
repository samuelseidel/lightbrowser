//! Small web platform items as in Chrome 153 (ADR 0047): the history rate
//! limit, pointer events (coalesced and predicted lists, `pointerrawupdate`,
//! `sourceCapabilities`), `navigator.storage` and `speechSynthesis.speak()`.
//! Each case in `cases/` is served on loopback by `serve.py` (the server
//! Chrome was recorded against, started per test on a free port;
//! `<!--LOG-->` becomes `prelude.html`). Its log (`L`) and the console
//! messages Chrome's own frame made must equal what Chrome 153 (Chrome for
//! Testing 153.0.8010.12) printed, in `chrome.json`. A case with
//! `steps.json` is driven by those steps through `CdpHost` (the CDP
//! commands Chrome got), the others just load. Re-record with
//! `python3 serve.py 18991 &` and
//! `node chrome.mjs http://127.0.0.1:18991 $(ls cases) > chrome.json`
//! (Playwright 1.65; `CHROME=` and `PLAYWRIGHT_MJS=` name the binaries).
//! Included from `pages.rs`.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lb_cdp::types::{KeyEvent, MouseEvent};
use lb_cdp::{ContextOptions, EvaluateRequest, NavigateRequest, PageHost};
use lb_page::{CdpHost, CdpHostOptions, PageObserver, WaitUntil};
use lb_persona::PersonaProfile;

use super::common::{browser, context};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/webplat")
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

/// Chrome's recording for `key`.
fn chrome(key: &str) -> String {
    let all: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir().join("chrome.json")).unwrap()).unwrap();
    all[key]
        .as_str()
        .unwrap_or_else(|| panic!("no recording for {key}"))
        .to_owned()
}

/// Console messages of all frames, as `level: text` lines.
struct Console(Mutex<Vec<String>>);

impl PageObserver for Console {
    fn console(&self, m: &lb_webapi::host::ConsoleMessage) {
        self.0
            .lock()
            .unwrap()
            .push(format!("{}: {}", m.level, m.text));
    }
}

/// Load the case and let it run (the page's own clock).
async fn run_plain(case: &str, virtual_time: bool) -> (String, String) {
    let srv = PyServer::start();
    let (b, ctx) = (browser(), context(virtual_time));
    let page = b.new_page(&ctx).await.unwrap();
    let console = Arc::new(Console(Mutex::new(Vec::new())));
    page.set_observer(Some(console.clone())).await.unwrap();
    let url = format!("http://127.0.0.1:{}/{case}/", srv.port);
    page.goto(&url, WaitUntil::Load, Duration::from_secs(20))
        .await
        .unwrap();
    let t0 = Instant::now();
    let log = loop {
        let log = match page.evaluate("L.join('\\n')").await.unwrap() {
            lb_js::JsValue::String(s) => s,
            v => panic!("{v:?}"),
        };
        if log.ends_with("\nend") || log == "end" || t0.elapsed() > Duration::from_secs(30) {
            break log;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let console = console.0.lock().unwrap().join("\n");
    (log, console)
}

fn host() -> CdpHost {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    let p = PersonaProfile::from_path_checked(
        root.join("reference/chrome/chrome153_win10_rtx3060_1080p.json"),
    )
    .expect("reference persona");
    let mut opts = CdpHostOptions::new(Arc::new(p), root.join("tls"));
    opts.workers = 1;
    CdpHost::new(opts).expect("host")
}

async fn eval(h: &CdpHost, expr: &str) -> serde_json::Value {
    let ctx = h.main_context("T").unwrap();
    let r = h
        .evaluate(
            "T",
            EvaluateRequest {
                context_id: ctx,
                expression: expr.into(),
                return_by_value: true,
                await_promise: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(r.exception_details.is_none(), "{expr}: {:?}", r.exception_details);
    r.result.value.unwrap_or(serde_json::Value::Null)
}

/// Playwright's mouse API as the CDP commands it sends (Playwright 1.65,
/// `crInput.js`): `move` with `steps` goes from the last position in equal
/// strides, one command each; `down`, `up` and `click` use the left button.
fn expand_playwright(steps: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    use serde_json::json;
    let (mut x, mut y, mut down) = (0.0f64, 0.0f64, false);
    let mut out = Vec::new();
    let mouse = |kind: &str, x: f64, y: f64, down: bool| {
        let mut p = json!({"type": kind, "button": if down { "left" } else { "none" },
            "buttons": i32::from(down), "x": x, "y": y, "modifiers": 0});
        if kind == "mouseMoved" {
            p["force"] = json!(if down { 0.5 } else { 0.0 });
        } else {
            p["button"] = json!("left");
            p["clickCount"] = json!(1);
            if kind == "mousePressed" {
                p["buttons"] = json!(1);
                p["force"] = json!(0.5);
            } else {
                p["buttons"] = json!(0);
            }
        }
        json!({"method": "Input.dispatchMouseEvent", "params": p})
    };
    for s in steps {
        let Some(call) = s["pw"].as_str() else {
            out.push(s);
            continue;
        };
        let args = s["args"].as_array().cloned().unwrap_or_default();
        match call {
            "move" | "click" => {
                let (tx, ty) = (args[0].as_f64().unwrap(), args[1].as_f64().unwrap());
                let n = args.get(2).and_then(|o| o["steps"].as_u64()).unwrap_or(1);
                let (fx, fy) = (x, y);
                for i in 1..=n {
                    let f = i as f64 / n as f64;
                    out.push(mouse("mouseMoved", fx + (tx - fx) * f, fy + (ty - fy) * f, down));
                }
                (x, y) = (tx, ty);
                if call == "click" {
                    out.push(mouse("mousePressed", x, y, true));
                    out.push(mouse("mouseReleased", x, y, false));
                }
            }
            "down" => {
                down = true;
                out.push(mouse("mousePressed", x, y, true));
            }
            "up" => {
                down = false;
                out.push(mouse("mouseReleased", x, y, false));
            }
            other => panic!("pw {other}"),
        }
    }
    out
}

/// Load the case, send its `steps.json` the way chrome.mjs does, and read
/// the log.
async fn run_steps(case: &str, steps: &str) -> (String, String) {
    let srv = PyServer::start();
    let h = host();
    h.create_context("C", ContextOptions::default()).unwrap();
    h.create_page("C", "T").await.unwrap();
    h.navigate(
        "T",
        NavigateRequest {
            url: format!("http://127.0.0.1:{}/{case}/", srv.port),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let steps: Vec<serde_json::Value> = serde_json::from_str(steps).unwrap();
    for s in expand_playwright(steps) {
        if let Some(ms) = s["wait"].as_u64() {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        } else if let Some(expr) = s["eval"].as_str() {
            eval(&h, expr).await;
        } else {
            match s["method"].as_str().unwrap() {
                "Input.dispatchMouseEvent" => h
                    .dispatch_mouse_event("T", serde_json::from_value::<MouseEvent>(s["params"].clone()).unwrap())
                    .await
                    .unwrap(),
                "Input.dispatchKeyEvent" => h
                    .dispatch_key_event("T", serde_json::from_value::<KeyEvent>(s["params"].clone()).unwrap())
                    .await
                    .unwrap(),
                m => panic!("step {m}"),
            }
        }
    }
    let t0 = Instant::now();
    let log = loop {
        let v = eval(&h, "L.join('\\n')").await;
        let log = v.as_str().unwrap().to_owned();
        if log.ends_with("\nend") || log == "end" || t0.elapsed() > Duration::from_secs(30) {
            break log;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    (log, String::new())
}

async fn run(case: &str, virtual_time: bool) {
    let steps = dir().join("cases").join(case).join("steps.json");
    let (log, console) = if steps.exists() {
        run_steps(case, &std::fs::read_to_string(&steps).unwrap()).await
    } else {
        run_plain(case, virtual_time).await
    };
    assert_eq!(log, chrome(case), "{case} log:\n{log}");
    if !steps.exists() {
        assert_eq!(
            console,
            chrome(&format!("{case}.console")),
            "{case} console:\n{console}"
        );
    }
}

macro_rules! cases {
    ($virtual_time:expr; $($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() {
                run(stringify!($name), $virtual_time).await;
            }
        )*
    };
}

// Real clock, and the page's own waiting.
cases!(
    false;
    history_budget,
    history_cap,
    history_frame,
    history_loc,
    history_nav,
    history_shared,
);

// Trusted input through CDP (steps.json): `CdpHost` here, the commands
// over a CDP session in Chrome.
cases!(
    false;
    pointer_click,
    pointer_kinds,
    pointer_late,
    pointer_plain,
    pointer_pw,
    pointer_raw,
);

// navigator.storage (the quota policy needs no disk: the numbers are
// compared as relations, see the cases).
cases!(
    false;
    storage_shape,
    storage_usage,
);

// The 10 s window runs on the virtual clock: the page's `setTimeout`s
// jump (Chrome waited for them).
cases!(true; history_window);
