//! CSS background images: Chrome requests the `url()` images of rendered
//! elements when it computes style for a frame, and those requests delay
//! the window's `load` event. LightBrowser has no frames, so the page layer
//! runs [`Window::load_background_images`] at `DOMContentLoaded`, after
//! each style sheet loads and before a screenshot. The bytes are kept per
//! URL (in memory) and decoded only when a screenshot paints them.

use std::collections::HashMap;
use std::rc::Rc;

use lb_paint::Pixmap;
use url::Url;

use super::{ReadyState, TaskSource, Window};
use crate::host::{Destination, NetRequest, NetResponse};

/// One background image URL.
pub(crate) enum BgImg {
    Loading,
    Loaded {
        bytes: bytes::Bytes,
        content_type: Option<String>,
        decoded: Option<Option<Rc<Pixmap>>>,
    },
    Failed,
}

/// Background images of a window, by absolute URL.
#[derive(Default)]
pub(crate) struct BgImages {
    pub(crate) map: HashMap<String, BgImg>,
    /// A loaded sheet or `<style>` mentions `url(`.
    pub(crate) hint: bool,
}

impl Window {
    /// Start loading the background images of rendered elements. Returns
    /// true while any of them is still loading.
    pub fn load_background_images(self: &Rc<Self>) -> bool {
        let hint = self.bg.borrow().hint
            || self.with_doc(|d| {
                d.descendants(lb_dom::Document::DOCUMENT).any(|n| {
                    d.get_attribute(n, "style")
                        .is_some_and(|s| s.to_ascii_lowercase().contains("url("))
                })
            });
        if !hint {
            return false;
        }
        let base = self.base_url();
        let urls = self.with_layout(0, |l, d| {
            l.styles().set_base_url(Some(base));
            l.styles().background_urls(d)
        });
        let mut loading = false;
        for u in urls {
            let state = self
                .bg
                .borrow()
                .map
                .get(&u)
                .map(|s| matches!(s, BgImg::Loading));
            match state {
                Some(true) => loading = true,
                Some(false) => {}
                None => {
                    if let Ok(url) = Url::parse(&u) {
                        start(self, u, url);
                        loading = true;
                    }
                }
            }
        }
        loading
    }

    /// Decoded pixels of a background image (premultiplied), if loaded.
    pub fn background_image(&self, url: &str) -> Option<Rc<Pixmap>> {
        let mut bg = self.bg.borrow_mut();
        let BgImg::Loaded {
            bytes,
            content_type,
            decoded,
        } = bg.map.get_mut(url)?
        else {
            return None;
        };
        decoded
            .get_or_insert_with(|| {
                lb_paint::decode::decode(bytes, content_type.as_deref())
                    .ok()
                    .map(Rc::new)
            })
            .clone()
    }
}

fn start(w: &Rc<Window>, key: String, url: Url) {
    w.bg.borrow_mut().map.insert(key.clone(), BgImg::Loading);
    // Loads started before `load` delay it, as in Chrome.
    let delays = w.ready_state() != ReadyState::Complete;
    if delays {
        w.pending_loads.set(w.pending_loads.get() + 1);
    }
    let done = move |w: &Rc<Window>, r: Result<(bytes::Bytes, Option<String>), ()>| {
        let st = match r {
            Ok((bytes, content_type)) => BgImg::Loaded {
                bytes,
                content_type,
                decoded: None,
            },
            Err(()) => BgImg::Failed,
        };
        w.bg.borrow_mut().map.insert(key, st);
        if delays {
            w.pending_loads.set(w.pending_loads.get().saturating_sub(1));
        }
        w.wake();
    };
    if url.scheme() == "data" {
        let r = super::image::data_url(&url)
            .map(|(b, m)| (b.into(), Some(m)))
            .ok_or(());
        w.queue_task(TaskSource::Networking, Box::new(move |_, w| done(w, r)));
        return;
    }
    if !matches!(url.scheme(), "http" | "https") {
        w.queue_task(
            TaskSource::Networking,
            Box::new(move |_, w| done(w, Err(()))),
        );
        return;
    }
    let req = NetRequest::get(url, Destination::Image, Some(w.url()));
    super::net::start_request(
        w,
        req,
        Box::new(move |_js, w, r: Result<NetResponse, String>| {
            let r = r
                .ok()
                .filter(|r| (200..300).contains(&r.status))
                .map(|r| {
                    let ct = r.header("content-type").map(str::to_owned);
                    (r.body, ct)
                })
                .ok_or(());
            done(w, r)
        }),
    );
}
