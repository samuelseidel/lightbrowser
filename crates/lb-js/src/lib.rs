//! JavaScript engine boundary.
//!
//! Everything above this crate (DOM bindings, Web APIs, event loop, CDP)
//! talks to [`JsEngine`]. Two backends implement it:
//! - `chrome` feature: V8 via the `v8` crate (M2)
//! - `firefox` feature: SpiderMonkey via Servo's `mozjs` crate (M4b)
//!
//! A binary links exactly one backend, so the engine always matches the
//! browser family the persona claims.

use lb_persona::BrowserFamily;

#[cfg(all(feature = "chrome", feature = "firefox"))]
compile_error!("enable only one of the `chrome` / `firefox` features per binary");

/// Family compiled into this binary, if any.
pub const COMPILED_FAMILY: Option<BrowserFamily> = if cfg!(feature = "chrome") {
    Some(BrowserFamily::Chrome)
} else if cfg!(feature = "firefox") {
    Some(BrowserFamily::Firefox)
} else {
    None
};

/// Result of evaluating a script, engine-neutral.
#[derive(Debug, Clone, PartialEq)]
pub enum JsValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    /// Anything else, serialised to JSON by the engine.
    Json(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsError {
    pub message: String,
    pub stack: Option<String>,
}

impl std::fmt::Display for JsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for JsError {}

/// One isolated JS world (a V8 context / SpiderMonkey realm) per frame,
/// plus isolated worlds for CDP utility scripts.
pub trait Realm {
    fn evaluate(&mut self, source: &str, url: &str) -> Result<JsValue, JsError>;
    /// Run queued promise jobs until empty.
    fn perform_microtask_checkpoint(&mut self);
}

/// An engine instance (V8 isolate / SpiderMonkey runtime). Single-threaded:
/// pinned to the worker thread that created it.
pub trait JsEngine {
    type Realm: Realm;

    fn family(&self) -> BrowserFamily;
    fn new_realm(&mut self) -> Self::Realm;
    /// Soft heap cap in bytes; the engine collects or aborts the page above it.
    fn set_heap_limit(&mut self, bytes: usize);
    /// Hint from the event loop when the page is idle.
    fn low_memory_notification(&mut self);
}
