//! Storage partitioning as in Chrome 153 (ADR 0037).
//!
//! Every window and worker scope has a [`StorageKey`]: its origin, the
//! top-level site of its page, and the ancestor chain bit (it or one of
//! its ancestor frames is cross-site with the top-level site). Storage is
//! looked up by the key, not the origin: `localStorage`,
//! `sessionStorage`, IndexedDB, the Cache API, service worker
//! registrations, shared workers, `BroadcastChannel` and `blob:` URLs. So
//! a frame of site A inside a frame of site B inside a page of site A has
//! its own storage, apart from the top-level A page's (Chrome's
//! `ThirdPartyStoragePartitioning`, on by default).
//!
//! The key is fixed when the window is linked into its frame tree (the
//! page layer sets it before that for network-loaded frames, to pick
//! their storage areas) and handed to workers the window starts.
//!
//! Also here: `storage` events between windows sharing an area, and
//! `document.hasStorageAccess()`, `hasUnpartitionedCookieAccess()` and
//! `requestStorageAccess()` with Chrome's answers when third-party cookies
//! are allowed (its default).

use std::rc::Rc;

use lb_js::HostError;

use super::{EventExtra, Obj, Runtime, WINDOW_REF, Window, method};
use crate::host::StorageChange;
use crate::registry::Registry;

pub use crate::host::{StorageKey, site};

impl Window {
    /// This window's or worker scope's storage key.
    pub fn storage_key(&self) -> StorageKey {
        if let Some(k) = self.frames.storage_key.borrow().as_ref() {
            return k.clone();
        }
        StorageKey::first_party(&super::frames::effective_url(self))
    }

    /// Fix this window's storage key (the page layer, before the realm is
    /// made; frames get theirs when linked).
    pub fn set_storage_key(&self, key: StorageKey) {
        *self.frames.storage_key.borrow_mut() = Some(key);
    }

    /// [`StorageKey::serialize`] of [`Window::storage_key`].
    pub fn storage_key_string(&self) -> String {
        self.storage_key().serialize()
    }
}

/// Ids of windows watching storage areas (only to tell the writer apart;
/// not fingerprint data).
static WATCH_IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// `w` made its `localStorage` (`local`) or `sessionStorage` object:
/// watch the area for other windows' changes, delivered as `storage`
/// events through `w`'s inbox.
pub(crate) fn watch_storage(w: &Rc<Window>, local: bool) {
    let (area, slot) = if local {
        (w.local_storage.clone(), &w.frames.storage_watch.0)
    } else {
        (w.session_storage.clone(), &w.frames.storage_watch.1)
    };
    if slot.get() != 0 {
        return;
    }
    let id = WATCH_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    slot.set(id);
    let inbox = w.inbox();
    area.watch(
        id,
        Box::new(move |c: &StorageChange| {
            if inbox.is_closed() {
                return false;
            }
            let c = c.clone();
            inbox.send(Box::new(move |js, w| fire_storage_event(js, w, local, c)));
            true
        }),
    );
}

/// A window `storage` listener or handler was added: watch both areas
/// (a document window that is still in its frame).
pub(crate) fn watch_storage_areas(w: &Rc<Window>) {
    if w.is_worker() || super::frames::is_detached(w) {
        return;
    }
    watch_storage(w, true);
    watch_storage(w, false);
}

/// `w` changed its `localStorage` (`local`) or `sessionStorage`.
pub(crate) fn storage_changed(
    w: &Window,
    local: bool,
    key: Option<String>,
    old: Option<String>,
    new: Option<String>,
) {
    let (area, from) = if local {
        (&w.local_storage, w.frames.storage_watch.0.get())
    } else {
        (&w.session_storage, w.frames.storage_watch.1.get())
    };
    area.notify(
        from,
        &StorageChange {
            key,
            old,
            new,
            url: w.url().to_string(),
        },
    );
}

fn fire_storage_event(js: &mut dyn super::Js, w: &Rc<Window>, local: bool, c: StorageChange) {
    if w.is_closed() {
        return;
    }
    let name = if local {
        "localStorage"
    } else {
        "sessionStorage"
    };
    let area = w.cached_obj((0, 0, name, String::new()), || Obj::Storage { local });
    super::fire_event_with(
        js,
        w,
        WINDOW_REF,
        "StorageEvent",
        "storage",
        false,
        false,
        EventExtra::Storage {
            key: c.key,
            old: c.old,
            new: c.new,
            url: c.url,
            area: Some(area),
        },
    );
}

/// The key a frame of `parent` showing `child` gets.
pub(crate) fn frame_key(parent: &Window, child: &Window) -> StorageKey {
    StorageKey::for_frame(&parent.storage_key(), &super::frames::effective_url(child))
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    // Chrome with third-party cookies allowed (its default): every frame
    // has storage (cookie) access, and `requestStorageAccess()` resolves
    // at once, also in cross-site frames without a user gesture (recorded
    // in Chromium 153 and Chrome 156; the 153 headless shell, without the
    // permission layer, rejects in frames).
    for name in ["hasStorageAccess", "hasUnpartitionedCookieAccess"] {
        method(reg, rt, "Document", name, |cx, w| {
            let r = cx.ret_new_promise()?;
            cx.resolve_promise(r, lb_js::Arg::Bool(w.origin_string() != "null"))
        });
    }
    method(reg, rt, "Document", "requestStorageAccess", |cx, w| {
        let r = cx.ret_new_promise()?;
        if w.origin_string() == "null" {
            return cx.reject_promise(r, not_allowed(w));
        }
        cx.resolve_promise(r, lb_js::Arg::Undefined)
    });
}

fn not_allowed(w: &Rc<Window>) -> HostError {
    w.rt.err("NotAllowedError", "requestStorageAccess not allowed")
}
