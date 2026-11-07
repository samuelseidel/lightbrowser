//! CDP time zone, locale, `acceptLanguage` and geolocation overrides on
//! real pages (ADR 0039), against Chrome 153 (headless shell
//! 153.0.8010.12, Linux) driven by Playwright on the same pages:
//! `tests/locale/chrome.json`, recorded with `tests/locale/chrome.mjs`
//! against `tests/locale/serve.py` (re-record:
//! `python3 serve.py 19173 & CHROME=... node chrome.mjs http://127.0.0.1:19173 > chrome.json`).
//!
//! The host is driven through `lb_cdp::PageHost` in the order Playwright
//! sends the commands (the server's ownership rules are tested in
//! `lb-cdp`'s `locale_overrides_match_chrome`). Workers keep the browser's
//! own `navigator.languages` unless their own target gets an
//! `acceptLanguage` override (Playwright 1.65 sends one to dedicated and
//! service workers, not to shared workers): there the persona's list
//! stands for Chrome's own `["en-US"]`.
#![cfg(feature = "chrome")]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use lb_cdp::{ContextOptions, EvaluateRequest, NavigateRequest, PageHost};
use lb_page::{CdpHost, CdpHostOptions};
use lb_persona::PersonaProfile;
use serde_json::{Value, json};

/// ICU's zone and locale are process-wide: the tests take turns.
static ICU: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn icu_lock() -> std::sync::MutexGuard<'static, ()> {
    ICU.lock().unwrap_or_else(|e| e.into_inner())
}

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/locale")
}

fn chrome() -> Value {
    serde_json::from_str(&std::fs::read_to_string(dir().join("chrome.json")).unwrap()).unwrap()
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
        PyServer {
            port: line.trim().parse().expect("port"),
            child,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

impl Drop for PyServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn persona() -> PersonaProfile {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    PersonaProfile::from_path_checked(
        root.join("reference/chrome/chrome153_win10_rtx3060_1080p.json"),
    )
    .expect("reference persona")
}

/// A host behind a CDP server that only pumps its events: with no client
/// asking to wait, the server lets each worker run at once (ADR 0029).
/// The test drives the host directly.
async fn host_with(p: PersonaProfile) -> Rc<CdpHost> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    let mut opts = CdpHostOptions::new(Arc::new(p), root.join("tls"));
    opts.workers = 1;
    let h = Rc::new(CdpHost::new(opts).expect("host"));
    let cfg = lb_cdp::ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        allow_origins: Vec::new(),
    };
    let server = lb_cdp::Server::bind(cfg, h.clone()).await.expect("server");
    tokio::task::spawn_local(async move {
        let _ = server.run().await;
    });
    h
}

async fn eval(h: &CdpHost, t: &str, expr: &str) -> Value {
    let ctx = h.main_context(t).unwrap();
    let req = EvaluateRequest {
        context_id: ctx,
        expression: expr.into(),
        return_by_value: true,
        await_promise: true,
        ..Default::default()
    };
    let r = tokio::time::timeout(Duration::from_secs(20), h.evaluate(t, req))
        .await
        .unwrap_or_else(|_| panic!("{expr}: no answer in 20 s"))
        .unwrap();
    assert!(
        r.exception_details.is_none(),
        "{expr}: {:?}",
        r.exception_details
    );
    r.result.value.unwrap_or(Value::Null)
}

async fn goto(h: &CdpHost, t: &str, url: String) {
    let nav = h
        .navigate(
            t,
            NavigateRequest {
                url,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(nav.error_text.is_none(), "{:?}", nav.error_text);
    // Let the page's own scripts and frames load.
    tokio::time::sleep(Duration::from_millis(300)).await;
}

/// Every realm of `serve.py`'s index page, as `chrome.mjs` collects them.
async fn all(h: &CdpHost, t: &str) -> Value {
    let mut out = serde_json::Map::new();
    for (k, e) in [
        ("main", "probe()"),
        ("frame", "framed()"),
        ("blank", "blankFrame()"),
        ("worker", "ask(dw)"),
        ("newWorker", "newWorker()"),
        ("shared", "askShared()"),
        ("sw", "askSW()"),
        ("acceptLanguage", "acceptLanguage()"),
    ] {
        out.insert(k.into(), eval(h, t, e).await);
    }
    Value::Object(out)
}

/// `got` equals Chrome's probe `want`; `lang`/`langs` are `languages`
/// when given (realms no `acceptLanguage` override reaches).
fn same_probe(what: &str, got: &Value, want: &Value, languages: Option<&[String]>) {
    let mut want = want.clone();
    if let Some(l) = languages {
        want["lang"] = json!(l[0]);
        want["langs"] = json!(l);
    }
    assert_eq!(got, &want, "{what}");
}

// Playwright's newContext({timezoneId: 'Asia/Tokyo', locale: 'de-DE'}):
// the overrides come before the page loads; the window, its frames, every
// worker kind and the service worker see them.
#[test]
fn context_override_reaches_every_realm() {
    let _icu = icu_lock();
    let want = chrome();
    let p = persona();
    let own = p.locale.languages.clone();
    lb_cdp::block_on_local(async {
        let srv = PyServer::start();
        let h = host_with(p).await;
        h.create_context("C", ContextOptions::default()).unwrap();
        h.create_page("C", "T").await.unwrap();
        assert!(
            h.set_time_zone_override("C", "T", Some("Asia/Tokyo"))
                .unwrap()
        );
        h.set_locale_override("C", "T", Some("de-DE")).unwrap();
        h.set_accept_language_override("T", Some("de-DE")).unwrap();
        goto(&h, "T", srv.url("/")).await;
        let got = all(&h, "T").await;
        let w = &want["context"];
        for k in ["main", "frame", "blank"] {
            same_probe(k, &got[k], &w[k], None);
        }
        // No worker target got an override here (no CDP session).
        for k in ["worker", "newWorker", "shared", "sw"] {
            same_probe(k, &got[k], &w[k], Some(&own));
        }
        assert_eq!(got["acceptLanguage"], w["acceptLanguage"]);
        // The fingerprint report follows the overrides (no mismatch).
        let r = h.fingerprint_report("T").await.unwrap();
        for name in ["intl.timeZone", "intl.locale", "navigator.languages"] {
            let c = r["checks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"] == name)
                .unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(c["status"], "ok", "{c}");
        }
        h.close_page("T").unwrap();
    })
    .unwrap();
}

// The operator path (`serve --timezone Asia/Tokyo --locale de-DE`, the
// SDK's `timezone=`/`locale=`): the persona itself carries them, so every
// realm, even a fresh isolate's first one, shows Chrome's values for that
// zone and locale, and navigator.languages / Accept-Language are the
// persona's new list everywhere.
#[test]
fn persona_override_matches_chrome() {
    let _icu = icu_lock();
    let want = chrome();
    let p = persona()
        .with_locale_override(&lb_persona::LocaleOverride {
            timezone: Some("Asia/Tokyo".into()),
            locale: Some("de-DE".into()),
            languages: None,
        })
        .unwrap();
    let langs = p.locale.languages.clone();
    let header = p.browser.accept_language.clone();
    lb_cdp::block_on_local(async {
        let srv = PyServer::start();
        let h = host_with(p).await;
        h.create_context("C", ContextOptions::default()).unwrap();
        h.create_page("C", "T").await.unwrap();
        goto(&h, "T", srv.url("/")).await;
        let got = all(&h, "T").await;
        for k in [
            "main",
            "frame",
            "blank",
            "worker",
            "newWorker",
            "shared",
            "sw",
        ] {
            same_probe(k, &got[k], &want["context"][k], Some(&langs));
        }
        assert_eq!(got["acceptLanguage"], json!(header));
        let r = h.fingerprint_report("T").await.unwrap();
        assert_eq!(r["ok"], true, "{}", r["summary"]);
        h.close_page("T").unwrap();
    })
    .unwrap();
}

// Raw CDP after the page and its workers run: every realm changes at once;
// a reload keeps it; zone ids and locale tags resolve as in Chrome.
#[test]
fn late_override_reaches_running_realms() {
    let _icu = icu_lock();
    let want = chrome();
    let p = persona();
    let own = p.locale.languages.clone();
    lb_cdp::block_on_local(async {
        let srv = PyServer::start();
        let h = host_with(p).await;
        h.create_context("C", ContextOptions::default()).unwrap();
        h.create_page("C", "T").await.unwrap();
        goto(&h, "T", srv.url("/")).await;
        eval(&h, "T", "Promise.all([ask(dw), askShared(), askSW()])").await;
        assert!(h.set_time_zone_override("C", "T", Some("America/Sao_Paulo")).unwrap());
        h.set_locale_override("C", "T", Some("fr-FR")).unwrap();
        let got = all(&h, "T").await;
        for k in ["main", "frame", "blank", "worker", "newWorker", "shared", "sw"] {
            same_probe(k, &got[k], &want["late"][k], Some(&own));
        }
        h.reload("T", false).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        for (k, e) in [("main", "probe()"), ("worker", "ask(dw)")] {
            same_probe(k, &eval(&h, "T", e).await, &want["reload"][k], Some(&own));
        }
        for (id, w) in want["ids"].as_object().unwrap() {
            assert!(h.set_time_zone_override("C", "T", Some(id)).unwrap(), "{id}");
            let got = eval(&h, "T", "(() => { const x = probe(); return [x.tz, x.off, x.str]; })()").await;
            assert_eq!(got, w["v"], "zone {id}");
        }
        // The zone chrome.mjs set last (the map above is sorted).
        assert!(h.set_time_zone_override("C", "T", Some("Factory")).unwrap());
        for (tag, w) in want["locales"].as_object().unwrap() {
            assert!(lb_cdp::emulation::locale_is_valid(tag), "{tag}");
            let icu = lb_cdp::emulation::icu_locale_tag(tag);
            h.set_locale_override("C", "T", Some(&icu)).unwrap();
            let got = eval(
                &h,
                "T",
                "(() => { const x = probe(); return [x.loc, x.nloc, x.ls, x.num, x.lang]; })()",
            )
            .await;
            let mut w = w["v"].clone();
            w[4] = json!(own[0]);
            assert_eq!(got, w, "locale {tag}");
        }
        h.set_locale_override("C", "T", None).unwrap();
        let got = eval(
            &h,
            "T",
            "[probe().loc === Intl.DateTimeFormat().resolvedOptions().locale, probe().tz]",
        )
        .await;
        assert_eq!(got, want["cleared"]);
        // acceptLanguage: the window's navigator.languages (a new array on
        // every read) and the header; workers keep their own.
        h.set_accept_language_override("T", Some("fr-CH, fr;q=0.9,en_GB"))
            .unwrap();
        let a = &want["acceptLanguage"];
        assert_eq!(
            eval(&h, "T", "[navigator.language, navigator.languages, navigator.languages === navigator.languages]").await,
            a["langs"]
        );
        assert_eq!(eval(&h, "T", "newWorker().then(x => x.langs)").await, json!(own));
        assert_eq!(eval(&h, "T", "acceptLanguage()").await, a["header"]);
        h.close_page("T").unwrap();
    })
    .unwrap();
}

fn geo(params: Value) -> Option<lb_cdp::GeolocationOverride> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as f64;
    Some(lb_cdp::emulation::geolocation(&params, now).unwrap())
}

// navigator.geolocation with the permission granted (the persona's
// `permissions.geolocation`; Chrome: Playwright's `permissions`), and
// without it.
#[test]
fn geolocation_follows_the_override() {
    let _icu = icu_lock();
    let want = chrome();
    let g = &want["geo"];
    let mut p = persona();
    p.permissions.insert("geolocation".into(), "granted".into());
    lb_cdp::block_on_local(async {
        let srv = PyServer::start();
        let h = host_with(p).await;
        h.create_context("C", ContextOptions::default()).unwrap();
        h.create_page("C", "T").await.unwrap();
        h.set_geolocation_override(
            "T",
            geo(json!({"latitude": 52.52, "longitude": 13.405, "accuracy": 20})),
        )
        .unwrap();
        goto(&h, "T", srv.url("/geo.html")).await;
        assert_eq!(eval(&h, "T", "surface()").await, g["surface"]);
        assert_eq!(eval(&h, "T", "one()").await, g["granted"]);
        for (k, params, opts) in [
            ("full", json!({"latitude": -33.8688, "longitude": 151.2093, "accuracy": 5, "altitude": 58, "altitudeAccuracy": 3, "heading": 90, "speed": 1.5}), ""),
            ("edge", json!({"latitude": 90, "longitude": -180, "accuracy": 0, "heading": 400, "speed": -1, "altitudeAccuracy": -1}), ""),
            ("noAccuracy", json!({"latitude": 1, "longitude": 2}), ""),
            ("empty", json!({}), ""),
            ("timeout0", json!({"latitude": 1, "longitude": 2, "accuracy": 3}), "{timeout: 0}"),
        ] {
            h.set_geolocation_override("T", geo(params)).unwrap();
            assert_eq!(eval(&h, "T", &format!("one({opts})")).await, g[k], "{k}");
        }
        h.set_geolocation_override("T", None).unwrap();
        assert_eq!(eval(&h, "T", "one({timeout: 300})").await, g["cleared"]);
        // A request waiting without an override ends when one is set.
        let (got, ()) = tokio::join!(eval(&h, "T", "one({timeout: 5000})"), async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            h.set_geolocation_override(
                "T",
                geo(json!({"latitude": 7, "longitude": 8, "accuracy": 9})),
            )
            .unwrap();
        });
        assert_eq!(got, g["pendingThenSet"]);
        h.close_page("T").unwrap();
    })
    .unwrap();
    // The persona's `prompt`: nobody answers, Chrome denies.
    lb_cdp::block_on_local(async {
        let srv = PyServer::start();
        let h = host_with(persona()).await;
        h.create_context("C", ContextOptions::default()).unwrap();
        h.create_page("C", "T").await.unwrap();
        h.set_geolocation_override(
            "T",
            geo(json!({"latitude": 52.52, "longitude": 13.405, "accuracy": 20})),
        )
        .unwrap();
        goto(&h, "T", srv.url("/geo.html")).await;
        assert_eq!(eval(&h, "T", "one()").await, g["prompt"]);
        h.close_page("T").unwrap();
    })
    .unwrap();
}
