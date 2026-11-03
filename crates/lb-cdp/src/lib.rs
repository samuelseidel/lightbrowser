//! Chrome DevTools Protocol server, so stock Playwright and Puppeteer drive
//! LightBrowser through `connectOverCDP` / `connect`.
//!
//! - [`Server`]: loopback TCP listener with the DevTools HTTP endpoints
//!   (`/json/version`, `/json/list`, `/json/protocol`, `/json/new`) and the
//!   WebSocket at `/devtools/browser/<id>`. Flat sessions (`sessionId` on
//!   every message), as Playwright uses after
//!   `Target.setAutoAttach({flatten: true})`.
//! - [`PageHost`]: the trait the browser implements (pages, realms, DOM,
//!   input, network). The server keeps all protocol state: targets,
//!   sessions, enabled domains, execution contexts, event routing.
//! - Isolated worlds: `Page.createIsolatedWorld` and world-scoped
//!   `addScriptToEvaluateOnNewDocument` give Playwright its utility world.
//!   `Runtime.addBinding` is only ever installed in isolated worlds, so the
//!   page's main world never sees automation bindings.
//! - `Emulation.*` is checked against the context's persona ([`emulation`]):
//!   matching values are accepted, contradicting ones are rejected.
//! - `LightBrowser.*`: `setPersona` works; `fingerprintReport` asks the
//!   host for the page's report ([`PageHost::fingerprint_report`]; on a
//!   browser session it takes `targetId`); `human*` answer "not
//!   implemented yet".
//! - [`remote`]: V8-style `RemoteObject`s and limited previews from a
//!   [`remote::JsValue`] snapshot. [`convert`]: `lb-dom` / `lb-layout` data
//!   as CDP types.
//! - [`mock::MockHost`]: a small host on `lb-dom` + `lb-layout` without a JS
//!   engine, for tests and for the firefox build's `serve` (no JS engine
//!   there yet). The chrome build serves real pages with
//!   `lb_page::CdpHost`.
//!
//! The method list is in [`methods::COMMANDS`]; anything else answers
//! `{code: -32601, message: "'X.y' wasn't found"}`.
//!
//! ```no_run
//! # use std::rc::Rc;
//! # fn persona() -> lb_persona::PersonaProfile { unimplemented!() }
//! let host = Rc::new(lb_cdp::mock::MockHost::new(persona()));
//! lb_cdp::block_on_local(async move {
//!     let server = lb_cdp::Server::bind(lb_cdp::ServerConfig::default(), host).await?;
//!     println!("{}", server.browser_ws_url());
//!     server.run().await
//! })
//! .unwrap()
//! .unwrap();
//! ```

mod browser;
pub mod convert;
pub mod emulation;
pub mod host;
pub mod ids;
pub mod methods;
pub mod mock;
pub mod protocol;
pub mod remote;
mod server;
pub mod types;

pub use browser::query_ax;
pub use host::{
    BindingScope, CallFunctionRequest, CallTarget, ContextOptions, EvaluateRequest, EventSink,
    GetPropertiesRequest, HostError, HostEvent, HostResult, NavigateRequest, PageEvent, PageHost,
    ViewportOverride,
};
pub use protocol::CdpError;
pub use server::{Server, ServerConfig, block_on_local};
