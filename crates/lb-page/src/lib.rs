//! Pages: browser, contexts, workers, the HTML event loop and navigation.
//!
//! # Model (ARCHITECTURE.md "Process and threading model")
//!
//! - [`Browser`] (chrome): N worker threads. Each runs a tokio
//!   `current_thread` runtime with a `LocalSet` and owns one `lb_js`
//!   engine, created on first use (the V8 platform is initialised once per
//!   process by lb-js). Bindings are defined once per engine.
//! - [`BrowserContext`]: persona, TLS profile, one shared cookie jar, the
//!   `localStorage` partition and a pool of network contexts. Many
//!   contexts can share a browser; contexts are engine-neutral.
//! - [`Page`] (chrome): a handle (`Send`) to a page pinned to one worker.
//!   Each page has one main frame: a [`lb_webapi::rt::Window`] with its own
//!   realm and document arena. Navigating away drops both in one step.
//!   Nested frames are a skeleton (iframes get no browsing context yet).
//! - The event loop (one local task per page) runs tasks by source
//!   priority, due timers (4 ms clamp after 5 nested levels, like Chrome),
//!   `requestAnimationFrame` at 60 Hz, a microtask checkpoint after each
//!   callback, and gives V8 a low-memory notification when the page goes
//!   idle. With [`ContextOptions::virtual_time`] the clock jumps to the
//!   next timer when nothing else is pending.
//! - Navigation: lb-net fetch (persona headers, cookies, redirects),
//!   encoding sniffing (BOM, header, `<meta charset>`), the streaming
//!   parser with classic scripts (inline, external, `defer`, `async`),
//!   module scripts (static imports prefetched), `document.write`,
//!   `DOMContentLoaded`, stylesheets for lb-css, `load`. Lifecycle events
//!   go to a [`PageObserver`] (shaped like CDP `Page.lifecycleEvent`).
//!
//! The firefox build has [`BrowserContext`] and [`load_document`] (fetch
//! and parse without running scripts) until SpiderMonkey lands (M4b).

mod context;

#[cfg(feature = "chrome")]
mod browser;
#[cfg(feature = "chrome")]
mod nav;
#[cfg(feature = "chrome")]
mod page;

pub use context::{BrowserContext, ContextOptions, PageError};
pub use lb_webapi::host::{ConsoleMessage, Destination, NetRequest, NetResponse, decode_text};

#[cfg(feature = "chrome")]
pub use browser::{Browser, BrowserOptions};
#[cfg(feature = "chrome")]
pub use page::{Page, PageHandle};

use std::time::Duration;

/// When a navigation counts as done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitUntil {
    /// The response arrived and the new document exists.
    Commit,
    DomContentLoaded,
    Load,
    /// `load` fired and no request was in flight for 500 ms.
    NetworkIdle,
    /// `load` fired, then this much more time passed.
    Time(Duration),
}

impl std::str::FromStr for WaitUntil {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "commit" => WaitUntil::Commit,
            "domcontentloaded" | "DOMContentLoaded" => WaitUntil::DomContentLoaded,
            "load" => WaitUntil::Load,
            "networkidle" | "networkidle0" => WaitUntil::NetworkIdle,
            ms => WaitUntil::Time(Duration::from_millis(ms.trim_end_matches("ms").parse().map_err(
                |_| format!("expected load, networkidle, domcontentloaded, commit or a number of ms, got {s:?}"),
            )?)),
        })
    }
}

/// A navigation's committed response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavResult {
    pub status: u16,
    /// URL after redirects.
    pub url: String,
    /// Loader id (increments per navigation of the page).
    pub loader_id: u64,
    /// Response headers in arrival order.
    pub headers: Vec<(String, String)>,
}

/// Page events for the CDP layer. Called on the page's worker thread.
pub trait PageObserver: Send + Sync {
    /// `init`, `commit`, `DOMContentLoaded`, `load`, `networkAlmostIdle`,
    /// `networkIdle` (CDP `Page.lifecycleEvent` names).
    fn lifecycle(&self, _name: &str, _loader_id: u64, _url: &str) {}
    /// Console messages and uncaught exceptions.
    fn console(&self, _msg: &ConsoleMessage) {}
}

/// Fetch `url` and parse it into a document without running scripts
/// (both families). Returns the response and the document.
pub async fn load_document(
    ctx: &BrowserContext,
    url: url::Url,
) -> Result<(NetResponse, lb_dom::Document), PageError> {
    let req = NetRequest::get(url, Destination::Document, None);
    let resp = ctx.fetch(req).await.map_err(PageError::Net)?;
    let ct = resp.header("content-type").map(str::to_owned);
    let html = ct.as_deref().is_none_or(is_html_type);
    let (text, _) = decode_text(&resp.body, ct.as_deref(), html);
    let mut doc = lb_dom::Document::new();
    let mut parser = lb_dom::HtmlParser::with_options(lb_dom::ParserOptions {
        scripting_enabled: false,
    });
    parser.push_str(&if html { text } else { text_document(&text) });
    parser.end();
    while let lb_dom::ParseStep::Script(_) = parser.run(&mut doc) {}
    Ok((resp, doc))
}

pub(crate) fn is_html_type(ct: &str) -> bool {
    let mime = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    mime.is_empty() || mime == "text/html" || mime == "application/xhtml+xml"
}

/// How Chrome shows a non-HTML text response: a `<pre>` with the text.
pub(crate) fn text_document(text: &str) -> String {
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<html><head></head><body><pre style=\"word-wrap: break-word; white-space: pre-wrap;\">{escaped}</pre></body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_until_parses() {
        assert_eq!("load".parse::<WaitUntil>(), Ok(WaitUntil::Load));
        assert_eq!(
            "networkidle".parse::<WaitUntil>(),
            Ok(WaitUntil::NetworkIdle)
        );
        assert_eq!(
            "250".parse::<WaitUntil>(),
            Ok(WaitUntil::Time(Duration::from_millis(250)))
        );
        assert!("soon".parse::<WaitUntil>().is_err());
    }
}
