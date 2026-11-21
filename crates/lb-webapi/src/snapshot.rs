//! The process-wide V8 startup snapshot of the bindings (M6, ADR 0013).
//!
//! Made once per process, in memory, the first time an engine asks for
//! it (call it before creating any engine: V8 shares one read-only heap
//! between all isolates, so the snapshot is made alone and every later
//! engine comes from it): the full `rt` registry is defined on a
//! snapshot-creator engine and one window realm is serialized. Engines
//! restored from it (`lb_js::Engine::with_snapshot`) replay their binding definitions
//! against it and deserialize window realms instead of building them.
//! Nothing is written to disk. The blob holds no persona data, so one blob
//! serves every persona of the family.

use std::sync::OnceLock;

use lb_persona::BrowserFamily;

use crate::{Bindings, Registry};

/// The startup snapshot for `family`, created on first call. `Err` says
/// why there is none (callers then build engines without one).
pub fn startup_snapshot(family: BrowserFamily) -> Result<&'static [u8], &'static str> {
    static CHROME: OnceLock<Result<&'static [u8], String>> = OnceLock::new();
    match family {
        BrowserFamily::Chrome => CHROME
            .get_or_init(|| create(family))
            .as_deref()
            .map_err(|e| e.as_str()),
        BrowserFamily::Firefox => Err("no snapshot for the firefox family"),
    }
}

/// Create the process's snapshot blob with the page layer's registry
/// (`rt::Runtime::new`). About 110 ms. Only once per process and before
/// any other engine exists; use [`startup_snapshot`].
fn create(family: BrowserFamily) -> Result<&'static [u8], String> {
    let mut reg = Registry::new();
    let _rt = crate::rt::Runtime::new(&mut reg);
    Bindings::create_snapshot(family, &reg).map_err(|e| e.to_string())
}
