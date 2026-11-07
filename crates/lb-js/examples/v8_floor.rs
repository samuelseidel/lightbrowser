//! Size floor of a binary that links V8 (M6 binary-size evidence, ADR
//! 0013): one isolate, one context, one script. Build with
//! `cargo build --release -p lb-js --example v8_floor` and compare its
//! stripped size with `lightbrowser`.

use std::sync::Arc;

use lb_js::{Engine, EngineOptions, RealmOptions, ScriptOrigin};
use lb_persona::Persona;

fn main() {
    let mut e = Engine::new(
        Arc::new(Persona::default_chrome()),
        EngineOptions::default(),
    )
    .expect("engine");
    let r = e.new_realm(RealmOptions::default()).expect("realm");
    let v = e
        .evaluate(
            r,
            "new Intl.DateTimeFormat('de-DE').format(0) + ' ' + (1.5).toLocaleString('fr')",
            ScriptOrigin::new("x.js"),
        )
        .expect("eval");
    println!("{v:?}");
}
