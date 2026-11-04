//! Dynamic `import()` in windows and worker scopes (ADR 0025).
//!
//! V8 asks the engine's hook ([`hook`]) for each `import(specifier)`. The
//! specifier is resolved as in HTML (`/`, `./`, `../` or an absolute URL;
//! anything else is a "bare" specifier and rejects). A module graph already
//! in the window's module map (which static `import`s, module scripts and
//! earlier `import()`s fill) is linked and evaluated in a microtask, as in
//! Chrome; otherwise the graph is fetched (scripts' destination, CORS mode,
//! `blob:` and `data:` locally, strict JavaScript MIME type check), put in
//! the map as a whole, and the import settles in a networking task. A
//! failed fetch rejects with Chrome's `Failed to fetch dynamically imported
//! module: <url>` for the top URL.

use std::collections::HashMap;
use std::rc::{Rc, Weak};

use lb_js::{DynamicImport, DynamicImportHook, RealmId, Resolver};
use url::Url;

use super::{RtInner, Runtime, TaskSource, Window};
use crate::host::{ConsoleMessage, Destination, NetRequest, NetResponse};

/// A module specifier that does not resolve (bare or invalid).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadSpecifier;

/// HTML's "resolve a module specifier" without import maps.
pub fn resolve_specifier(spec: &str, base: &Url) -> Result<Url, BadSpecifier> {
    if spec.starts_with('/') || spec.starts_with("./") || spec.starts_with("../") {
        return base.join(spec).map_err(|_| BadSpecifier);
    }
    Url::parse(spec).map_err(|_| BadSpecifier)
}

/// The loader's message for a specifier it cannot resolve (static
/// imports; Chrome's text).
pub fn bare_message(spec: &str) -> String {
    format!(
        "Failed to resolve module specifier \"{spec}\". Relative references must start with either \"/\", \"./\", or \"../\"."
    )
}

/// The JavaScript MIME type essences (HTML), which module scripts must
/// have.
fn is_js_mime(ct: &str) -> bool {
    let e = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(
        e.as_str(),
        "text/javascript"
            | "application/javascript"
            | "application/ecmascript"
            | "application/x-ecmascript"
            | "application/x-javascript"
            | "text/ecmascript"
            | "text/javascript1.0"
            | "text/javascript1.1"
            | "text/javascript1.2"
            | "text/javascript1.3"
            | "text/javascript1.4"
            | "text/javascript1.5"
            | "text/jscript"
            | "text/livescript"
            | "text/x-ecmascript"
            | "text/x-javascript"
    )
}

/// The engine hook for `import()` (one per engine; windows by realm).
pub(super) fn hook(rt: Weak<RtInner>) -> DynamicImportHook {
    Rc::new(
        move |realm: RealmId, resolver: Resolver, spec: &str, referrer: &str| {
            let Some(rt) = rt.upgrade() else {
                return DynamicImport::Failed(format!(
                    "Failed to resolve module specifier '{spec}'"
                ));
            };
            let Some(w) = Runtime(rt).window(realm) else {
                return DynamicImport::Failed(format!(
                    "Failed to resolve module specifier '{spec}'"
                ));
            };
            // A module's own URL; a classic script resolves against the
            // document's base URL.
            let base = match Url::parse(referrer) {
                Ok(u) if w.module_map.borrow().contains_key(u.as_str()) => u,
                _ => w.base_url(),
            };
            let Ok(url) = resolve_specifier(spec, &base) else {
                return DynamicImport::Failed(format!(
                    "Failed to resolve module specifier '{spec}'"
                ));
            };
            if w.module_map.borrow().contains_key(url.as_str()) {
                return DynamicImport::Ready(url.to_string());
            }
            start(&w, realm, resolver, url);
            DynamicImport::Pending
        },
    )
}

/// Fetch the graph at `url`, then settle the import in a networking task.
fn start(w: &Rc<Window>, realm: RealmId, resolver: Resolver, url: Url) {
    w.in_flight.set(w.in_flight.get() + 1);
    let weak = Rc::downgrade(w);
    w.net.spawn(Box::pin(async move {
        let r = load_graph(&weak, &url).await;
        let Some(w) = weak.upgrade() else { return };
        w.in_flight.set(w.in_flight.get().saturating_sub(1));
        if w.is_closed() {
            return;
        }
        let outcome = match r {
            Ok(mods) => {
                let mut map = w.module_map.borrow_mut();
                for (u, src) in mods {
                    map.entry(u).or_insert(src);
                }
                Ok(url.to_string())
            }
            Err(()) => Err(format!(
                "Failed to fetch dynamically imported module: {url}"
            )),
        };
        w.queue_task(
            TaskSource::Networking,
            Box::new(move |js, _w| {
                js.realm_ops()
                    .finish_dynamic_import(realm, resolver, outcome);
            }),
        );
    }));
}

/// The modules of the graph at `top` that the module map lacks, or `Err`
/// if one cannot be fetched, has a non-JavaScript MIME type or an
/// unresolvable import.
async fn load_graph(w: &Weak<Window>, top: &Url) -> Result<Vec<(String, String)>, ()> {
    let mut got: HashMap<String, String> = HashMap::new();
    let mut order = Vec::new();
    let mut queue = vec![top.clone()];
    while let Some(u) = queue.pop() {
        let key = u.to_string();
        let known = {
            let Some(w) = w.upgrade() else { return Err(()) };
            w.module_map.borrow().contains_key(&key)
        };
        if known || got.contains_key(&key) {
            continue;
        }
        if got.len() > 512 {
            return Err(());
        }
        let resp = fetch_module(w, &u).await?;
        let ct = resp.header("content-type").unwrap_or("").to_owned();
        if !is_js_mime(&ct) {
            if let Some(w) = w.upgrade() {
                let essence = ct.split(';').next().unwrap_or("").trim().to_owned();
                w.log(ConsoleMessage {
                    level: "error".into(),
                    text: format!("Failed to load module script: Expected a JavaScript-or-Wasm module script but the server responded with a MIME type of \"{essence}\". Strict MIME type checking is enforced for module scripts per HTML spec."),
                    url: None,
                    line: None,
                });
            }
            return Err(());
        }
        let src = super::script::script_source(&resp.body, Some(&ct));
        for spec in super::script::import_specifiers(&src) {
            queue.push(resolve_specifier(&spec, &u).map_err(|_| ())?);
        }
        order.push(key.clone());
        got.insert(key, src);
    }
    Ok(order
        .into_iter()
        .filter_map(|k| got.remove(&k).map(|s| (k, s)))
        .collect())
}

async fn fetch_module(w: &Weak<Window>, u: &Url) -> Result<NetResponse, ()> {
    let fut = {
        let Some(w) = w.upgrade() else { return Err(()) };
        if let Some(r) = super::net::local_response("GET", u, &[]) {
            return r.map_err(|_| ());
        }
        let mut req = NetRequest::get(u.clone(), Destination::Script, Some(w.url()));
        req.mode = "cors";
        w.net_fetch(req)
    };
    let resp = fut.await.map_err(|_| ())?;
    let origin = w.upgrade().map(|w| w.url().origin()).ok_or(())?;
    if !(200..300).contains(&resp.status) || !super::net::cors_allows(&origin, &resp) {
        return Err(());
    }
    Ok(resp)
}
