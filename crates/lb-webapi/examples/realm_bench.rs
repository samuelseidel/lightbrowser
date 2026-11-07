//! Realm creation timings (M6, ADR 0013). Run in release:
//! `cargo run --release -p lb-webapi --example realm_bench`.
//!
//! With the full `rt` registry (the page layer's), prints engine creation,
//! binding definition, first and later window realm times as medians,
//! without and with the startup snapshot, plus the snapshot creation time
//! and size.

use std::sync::Arc;
use std::time::Instant;

use lb_js::{Engine, EngineOptions};
use lb_persona::{BrowserFamily, Persona};
use lb_webapi::rt::Runtime;
use lb_webapi::{Bindings, Registry, WindowRealmOptions};

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn run(label: &str, persona: &Arc<Persona>, snapshot: Option<&'static [u8]>) {
    let runs = 7;
    let realms = 20;
    let (mut eng, mut def, mut first, mut next) = (vec![], vec![], vec![], vec![]);
    let mut status = String::new();
    for _ in 0..runs {
        let t0 = Instant::now();
        let mut e = match snapshot {
            Some(b) => Engine::with_snapshot(persona.clone(), EngineOptions::default(), b),
            None => Engine::new(persona.clone(), EngineOptions::default()),
        }
        .unwrap();
        let t1 = Instant::now();
        let mut reg = Registry::new();
        let rt = Runtime::new(&mut reg);
        let b = Bindings::define(&mut e, &reg).unwrap();
        rt.set_bindings(b.clone());
        let t2 = Instant::now();
        // A Linux persona's realm: the OS gate deletion runs (M7).
        let linux = Some(lb_persona::OsApis {
            share: false,
            bluetooth: false,
        });
        let r = b
            .new_window_realm(
                &mut e,
                WindowRealmOptions {
                    os_apis: linux,
                    ..Default::default()
                },
            )
            .unwrap();
        let t3 = Instant::now();
        e.drop_realm(r);
        let mut times = vec![];
        for i in 0..realms {
            let t = Instant::now();
            let r = b
                .new_window_realm(
                    &mut e,
                    WindowRealmOptions {
                        insecure: i % 2 == 1,
                        os_apis: linux,
                        ..Default::default()
                    },
                )
                .unwrap();
            times.push(ms(t.elapsed()));
            e.drop_realm(r);
        }
        status = format!("{:?}", e.snapshot_status());
        eng.push(ms(t1 - t0));
        def.push(ms(t2 - t1));
        first.push(ms(t3 - t2));
        next.push(median(times));
    }
    println!(
        "{label}: engine {:.2} ms, registry+define {:.2} ms, first realm {:.2} ms, next realm {:.2} ms\n  {status}",
        median(eng),
        median(def),
        median(first),
        median(next),
    );
}

fn status(key: &str) -> f64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<f64>().ok())
        .map_or(0.0, |kb| kb / 1024.0)
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("peak") {
        // RSS cost of making the snapshot in memory, in a fresh process.
        lb_js::init_v8("");
        let before = (status("VmRSS:"), status("VmHWM:"));
        let blob = lb_webapi::snapshot::startup_snapshot(BrowserFamily::Chrome).unwrap();
        println!(
            "snapshot {:.2} MB: RSS {:.1} -> {:.1} MB, peak {:.1} -> {:.1} MB",
            blob.len() as f64 / 1e6,
            before.0,
            status("VmRSS:"),
            before.1,
            status("VmHWM:")
        );
        return;
    }
    println!(
        "medians of 7 engines x 20 realms; debug={}",
        cfg!(debug_assertions)
    );
    let persona = Arc::new(Persona::default_chrome());
    run("no snapshot", &persona, None);
    let t = Instant::now();
    let blob = lb_webapi::snapshot::startup_snapshot(BrowserFamily::Chrome).unwrap();
    println!(
        "snapshot: created in {:.1} ms, {:.2} MB",
        ms(t.elapsed()),
        blob.len() as f64 / 1e6
    );
    run("snapshot", &persona, Some(blob));
    heap(&persona, None);
    heap(&persona, Some(blob));
}

/// V8 heap use of an engine with its bindings, and per extra window realm
/// (after a full GC).
fn heap(persona: &Arc<Persona>, snapshot: Option<&'static [u8]>) {
    let mut e = match snapshot {
        Some(b) => Engine::with_snapshot(persona.clone(), EngineOptions::default(), b),
        None => Engine::new(persona.clone(), EngineOptions::default()),
    }
    .unwrap();
    let mut reg = Registry::new();
    let rt = Runtime::new(&mut reg);
    let b = Bindings::define(&mut e, &reg).unwrap();
    rt.set_bindings(b.clone());
    e.low_memory_notification();
    let base = e.heap_stats();
    let n = 10;
    let realms: Vec<_> = (0..n)
        .map(|_| {
            b.new_window_realm(&mut e, WindowRealmOptions::default())
                .unwrap()
        })
        .collect();
    e.low_memory_notification();
    let after = e.heap_stats();
    println!(
        "heap ({}): engine+bindings used {:.1} MB (total {:.1} MB); per window realm {:.2} MB used",
        if snapshot.is_some() {
            "snapshot"
        } else {
            "no snapshot"
        },
        base.used as f64 / 1e6,
        base.total as f64 / 1e6,
        (after.used - base.used) as f64 / 1e6 / n as f64
    );
    drop(realms);
}
