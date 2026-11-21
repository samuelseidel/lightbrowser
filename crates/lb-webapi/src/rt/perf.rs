//! Performance Timeline (ADR 0046): `performance.getEntries*`, the
//! navigation, resource, paint, mark, measure and visibility-state entry
//! types, `PerformanceObserver` and the resource timing buffer, as in
//! Chrome 153 (every shape here was recorded from it, see
//! `lb-page/tests/perf/`).
//!
//! Entries are made only from data the loader already has: a resource
//! entry from the [`NetTiming`] its response carries
//! ([`track`] wraps the future of every page request), the navigation
//! entry from the [`NavInfo`] the page layer gives the window and from the
//! points the event loop marks ([`Window::nav_mark`]). A page that never
//! reads the timeline pays one small struct per request; entry objects
//! exist only once script asks for them.
//!
//! Times are milliseconds on the window's clock, coarsened like
//! `performance.now()` ([`coarse_time`]), so values never decrease along
//! the fetch sequence.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use lb_js::{Arg, CallCtx, Handle, HostError, HostRef, JsValue, ValueKind};
use url::Url;

use super::events::coarse_now;
use super::{
    Dict, K_OBJ, Obj, Runtime, TaskSource, Window, coarse_time, ctor, getter, illegal, method,
    need_args, this_obj,
};
use crate::host::{LocalFuture, NetRequest, NetResponse, NetTiming};
use crate::registry::{ImplKind, Registry};

/// Chrome's default resource timing buffer size.
const BUFFER_DEFAULT: usize = 250;

/// Process-wide counter behind `navigationId`.
static NAV_IDS: AtomicU32 = AtomicU32::new(1);

const WINDOW_TYPES: [&str; 15] = [
    "element",
    "event",
    "first-input",
    "interaction-contentful-paint",
    "largest-contentful-paint",
    "layout-shift",
    "long-animation-frame",
    "longtask",
    "mark",
    "measure",
    "navigation",
    "paint",
    "resource",
    "soft-navigation",
    "visibility-state",
];
const WORKER_TYPES: [&str; 3] = ["mark", "measure", "resource"];

/// `PerformanceTiming` attribute names: not usable as mark names and
/// resolved by `measure()` against the navigation entry.
const TIMING_ATTRS: [&str; 21] = [
    "navigationStart",
    "unloadEventStart",
    "unloadEventEnd",
    "redirectStart",
    "redirectEnd",
    "fetchStart",
    "domainLookupStart",
    "domainLookupEnd",
    "connectStart",
    "connectEnd",
    "secureConnectionStart",
    "requestStart",
    "responseStart",
    "responseEnd",
    "domLoading",
    "domInteractive",
    "domContentLoadedEventStart",
    "domContentLoadedEventEnd",
    "domComplete",
    "loadEventStart",
    "loadEventEnd",
];

/// The 36 event types of `performance.eventCounts`, in Chrome's order.
const EVENT_COUNT_TYPES: [&str; 36] = [
    "pointerdown",
    "touchend",
    "input",
    "keydown",
    "mouseleave",
    "mouseenter",
    "drop",
    "beforeinput",
    "pointerenter",
    "dragend",
    "pointercancel",
    "compositionupdate",
    "mousedown",
    "dragleave",
    "dragover",
    "mouseup",
    "pointerover",
    "lostpointercapture",
    "mouseover",
    "gotpointercapture",
    "dblclick",
    "keyup",
    "keypress",
    "pointerup",
    "compositionstart",
    "auxclick",
    "dragstart",
    "touchstart",
    "compositionend",
    "pointerout",
    "dragenter",
    "touchcancel",
    "click",
    "contextmenu",
    "mouseout",
    "pointerleave",
];

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Mark,
    Measure,
    Resource,
    Navigation,
    Paint,
    Visibility,
}

impl Kind {
    fn type_name(self) -> &'static str {
        match self {
            Kind::Mark => "mark",
            Kind::Measure => "measure",
            Kind::Resource => "resource",
            Kind::Navigation => "navigation",
            Kind::Paint => "paint",
            Kind::Visibility => "visibility-state",
        }
    }

    fn iface(self) -> &'static str {
        match self {
            Kind::Mark => "PerformanceMark",
            Kind::Measure => "PerformanceMeasure",
            Kind::Resource => "PerformanceResourceTiming",
            Kind::Navigation => "PerformanceNavigationTiming",
            Kind::Paint => "PerformancePaintTiming",
            Kind::Visibility => "VisibilityStateEntry",
        }
    }

    fn of_type(name: &str) -> Option<Kind> {
        Some(match name {
            "mark" => Kind::Mark,
            "measure" => Kind::Measure,
            "resource" => Kind::Resource,
            "navigation" => Kind::Navigation,
            "paint" => Kind::Paint,
            "visibility-state" => Kind::Visibility,
            _ => return None,
        })
    }
}

/// One `Server-Timing` metric.
#[derive(Clone, Debug)]
pub(crate) struct ServerMetric {
    name: String,
    duration: f64,
    description: String,
}

/// The fields of resource and navigation entries (all times in window
/// milliseconds, 0 when not available).
#[derive(Clone, Debug, Default)]
pub(crate) struct ResData {
    initiator: &'static str,
    protocol: String,
    blocking: bool,
    content_type: String,
    content_encoding: String,
    status: u16,
    worker_start: f64,
    redirect_start: f64,
    redirect_end: f64,
    fetch_start: f64,
    dns_start: f64,
    dns_end: f64,
    connect_start: f64,
    secure_start: f64,
    connect_end: f64,
    request_start: f64,
    response_start: f64,
    response_end: f64,
    transfer: u64,
    encoded: u64,
    decoded: u64,
    server_timing: Vec<ServerMetric>,
    /// Navigation: how the document was reached.
    nav_type: &'static str,
    redirect_count: u32,
}

pub(crate) struct Entry {
    kind: Kind,
    name: String,
    start: f64,
    duration: f64,
    nav_id: u32,
    detail: Option<lb_js::Serialized>,
    detail_value: Cell<Option<Handle>>,
    res: Option<ResData>,
    /// The entry's object in the window's table, once script saw it.
    obj: Cell<Option<u32>>,
    /// `PerformanceServerTiming` objects, once script asked.
    server_objs: RefCell<Vec<u32>>,
}

impl Entry {
    fn new(kind: Kind, name: String, start: f64, duration: f64, nav_id: u32) -> Entry {
        Entry {
            kind,
            name,
            start,
            duration,
            nav_id,
            detail: None,
            detail_value: Cell::new(None),
            res: None,
            obj: Cell::new(None),
            server_objs: RefCell::new(Vec::new()),
        }
    }
}

/// What the page layer knows about a document's own load (the navigation
/// entry and `performance.timing`).
#[derive(Clone, Debug)]
pub struct NavInfo {
    /// The network's measurements of the document request (`None` for
    /// `about:blank`, `srcdoc` and responses a CDP client fulfilled).
    pub timing: Option<Box<NetTiming>>,
    /// `navigate`, `reload` or `back_forward`.
    pub nav_type: &'static str,
    pub status: u16,
    /// Response headers (for `contentType`, `Server-Timing` and the
    /// redirect checks).
    pub headers: Vec<(String, String)>,
    /// Body size as received and decoded.
    pub encoded: u64,
    pub decoded: u64,
}

/// Points of the document's own timeline, in window milliseconds.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NavMarks {
    pub dom_loading: Option<f64>,
    pub dom_interactive: Option<f64>,
    pub dcl_start: Option<f64>,
    pub dcl_end: Option<f64>,
    pub dom_complete: Option<f64>,
    pub load_start: Option<f64>,
    pub load_end: Option<f64>,
}

/// A point of the document load the event loop reports.
#[derive(Clone, Copy, Debug)]
pub enum NavMark {
    DomContentLoadedStart,
    DomContentLoadedEnd,
    LoadStart,
    LoadEnd,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ObserveMode {
    None,
    EntryTypes,
    Type,
}

pub(crate) struct Observer {
    callback: Handle,
    types: Vec<Kind>,
    mode: ObserveMode,
    queue: Vec<Rc<Entry>>,
    task_queued: bool,
    /// The next callback's options carry `droppedEntriesCount` (set by
    /// each `observe()`).
    dropped_pending: bool,
}

/// What a performance host object is.
pub(crate) enum PerfObj {
    Entry(Rc<Entry>),
    Observer(Observer),
    EntryList(Vec<Rc<Entry>>),
    ServerTiming(ServerMetric),
    EventCounts,
}

impl PerfObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            PerfObj::Entry(e) => e.kind.iface(),
            PerfObj::Observer(_) => "PerformanceObserver",
            PerfObj::EntryList(_) => "PerformanceObserverEntryList",
            PerfObj::ServerTiming(_) => "PerformanceServerTiming",
            PerfObj::EventCounts => "EventCounts",
        }
    }
}

/// A window's performance timeline.
pub(crate) struct PerfState {
    resources: Vec<Rc<Entry>>,
    secondary: Vec<Rc<Entry>>,
    limit: usize,
    full_pending: bool,
    marks: Vec<Rc<Entry>>,
    measures: Vec<Rc<Entry>>,
    paint: Vec<Rc<Entry>>,
    nav: Option<Rc<Entry>>,
    vis: Option<Rc<Entry>>,
    /// Observers (object ids) with entry types, in `observe()` order.
    observers: Vec<u32>,
    nav_info: Option<NavInfo>,
    nav_res: Option<ResData>,
    pub(crate) marks_nav: NavMarks,
    nav_id: u32,
    supported: Option<Handle>,
    /// A first paint is due at this window time; paint entries exist.
    paint_due: Option<f64>,
    painted: bool,
}

impl Default for PerfState {
    fn default() -> Self {
        PerfState {
            resources: Vec::new(),
            secondary: Vec::new(),
            limit: BUFFER_DEFAULT,
            full_pending: false,
            marks: Vec::new(),
            measures: Vec::new(),
            paint: Vec::new(),
            nav: None,
            vis: None,
            observers: Vec::new(),
            nav_info: None,
            nav_res: None,
            marks_nav: NavMarks::default(),
            nav_id: NAV_IDS.fetch_add(1, Ordering::Relaxed),
            supported: None,
            paint_due: None,
            painted: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

/// Window milliseconds of an [`Instant`] in the past, coarsened.
fn ms_of(w: &Window, now_ms: f64, now: Instant, at: Instant) -> f64 {
    let back = now.saturating_duration_since(at).as_secs_f64() * 1000.0;
    coarse_time(w, (now_ms - back).max(0.0))
}

impl Window {
    /// Give the window the facts of its document's load (page layer, right
    /// after [`Window::new`]).
    pub fn set_nav_info(&self, info: NavInfo) {
        let mut li = self.load_info.borrow_mut();
        if let Some(t) = &info.timing {
            li.connection_info = Some(t.protocol.to_owned());
        }
        li.navigation_type = Some(
            match info.nav_type {
                "reload" => "Reload",
                "back_forward" => "BackForward",
                _ => "Other",
            }
            .to_owned(),
        );
        drop(li);
        let now = self.clock.now();
        let mut st = self.perf.borrow_mut();
        st.marks_nav.dom_loading = Some(coarse_time(self, now));
        st.nav_info = Some(info);
    }

    /// The document reached a point of its load.
    pub fn nav_mark(&self, mark: NavMark) {
        let now = coarse_now(self);
        let mut st = self.perf.borrow_mut();
        let m = &mut st.marks_nav;
        let slot = match mark {
            NavMark::DomContentLoadedStart => &mut m.dcl_start,
            NavMark::DomContentLoadedEnd => &mut m.dcl_end,
            NavMark::LoadStart => &mut m.load_start,
            NavMark::LoadEnd => &mut m.load_end,
        };
        slot.get_or_insert(now);
    }

    /// `readyState` changed (called by [`Window::set_ready_state`]).
    pub(crate) fn perf_ready_state(&self, s: super::ReadyState) {
        let now = coarse_now(self);
        let mut st = self.perf.borrow_mut();
        match s {
            super::ReadyState::Interactive => {
                st.marks_nav.dom_interactive.get_or_insert(now);
            }
            super::ReadyState::Complete => {
                st.marks_nav.dom_interactive.get_or_insert(now);
                st.marks_nav.dom_complete.get_or_insert(now);
            }
            super::ReadyState::Loading => {}
        }
        drop(st);
        if s != super::ReadyState::Loading {
            self.arm_paint();
        }
    }

    /// When a first paint is due (window milliseconds), if one is.
    pub fn paint_due(&self) -> Option<f64> {
        self.perf.borrow().paint_due
    }

    /// Schedule the first paint at the next frame when the document has
    /// something to paint and has not painted yet.
    pub(crate) fn arm_paint(&self) {
        {
            let st = self.perf.borrow();
            if st.painted || st.paint_due.is_some() || self.is_worker() {
                return;
            }
        }
        if !self.has_paintable_content() {
            return;
        }
        let frame = 1000.0 / 60.0;
        let now = self.clock.now();
        let due = ((now / frame).floor() + 1.0) * frame;
        self.perf.borrow_mut().paint_due = Some(due);
        self.wake();
    }

    /// Text, an image, a canvas or an SVG in the body.
    fn has_paintable_content(&self) -> bool {
        use lb_dom::{NodeType, local_name};
        self.with_doc(|d| {
            let Some(body) = d.body() else { return false };
            d.descendants(body).any(|n| match d.node_type(n) {
                NodeType::Text => {
                    d.character_data(n)
                        .is_some_and(|t| t.chars().any(|c| !c.is_whitespace()))
                        && d.parent(n).is_none_or(|p| {
                            !d.is_html_element_named(p, &local_name!("script"))
                                && !d.is_html_element_named(p, &local_name!("style"))
                        })
                }
                NodeType::Element => {
                    d.is_html_element_named(n, &local_name!("img"))
                        || d.is_html_element_named(n, &local_name!("canvas"))
                        || d.is_html_element_named(n, &local_name!("video"))
                        || d.local_name(n).is_some_and(|l| &**l == "svg")
                }
                _ => false,
            })
        })
    }

    /// The frame the first paint was due at has come: record
    /// `first-paint` and `first-contentful-paint` (the engine does not
    /// paint; ADR 0046).
    pub fn run_paint(self: &Rc<Self>) {
        let due = {
            let mut st = self.perf.borrow_mut();
            st.paint_due.take().is_some() && !st.painted
        };
        if !due || self.is_closed() {
            return;
        }
        if !self.has_paintable_content() {
            return;
        }
        let now = coarse_now(self);
        let nav_id = {
            let mut st = self.perf.borrow_mut();
            st.painted = true;
            st.nav_id
        };
        self.load_info.borrow_mut().first_paint_ms = Some(now);
        for name in ["first-paint", "first-contentful-paint"] {
            let e = Rc::new(Entry::new(Kind::Paint, name.into(), now, 0.0, nav_id));
            self.perf.borrow_mut().paint.push(e.clone());
            notify(self, &e);
        }
    }
}

// ---------------------------------------------------------------------------
// Resource entries
// ---------------------------------------------------------------------------

/// What [`track`] remembers of a request until its response arrives.
pub(crate) struct Pending {
    url: Url,
    initiator: &'static str,
    blocking: bool,
    mode: &'static str,
    started: Instant,
    controlled: bool,
    /// Service worker scripts come from the browser's script cache: sizes
    /// and transfer read 0.
    cached_script: bool,
}

fn initiator_of(req: &NetRequest) -> &'static str {
    use crate::host::Destination;
    if !req.initiator_type.is_empty() {
        return req.initiator_type;
    }
    match req.destination {
        Destination::Script => "script",
        Destination::Style => "link",
        Destination::Image => "img",
        Destination::Iframe => "iframe",
        Destination::Empty => "fetch",
        Destination::Document => "navigation",
    }
}

impl Pending {
    pub(crate) fn of(req: &NetRequest, controlled: bool) -> Option<Pending> {
        if !matches!(req.url.scheme(), "http" | "https")
            || req.destination == crate::host::Destination::Document
        {
            return None;
        }
        Some(Pending {
            url: req.url.clone(),
            initiator: initiator_of(req),
            blocking: req.render_blocking,
            mode: if req.timing_cors { "cors" } else { req.mode },
            started: Instant::now(),
            controlled,
            cached_script: false,
        })
    }

    /// A script `importScripts` loads in a service worker.
    pub(crate) fn cached_script(mut self) -> Pending {
        self.cached_script = true;
        self
    }
}

/// Wrap the future of a request so its resource entry is made when the
/// response is complete (or the request failed).
pub(crate) fn track(
    w: &Rc<Window>,
    pending: Option<Pending>,
    fut: LocalFuture<Result<NetResponse, String>>,
) -> LocalFuture<Result<NetResponse, String>> {
    let Some(p) = pending else {
        return fut;
    };
    let weak = Rc::downgrade(w);
    Box::pin(async move {
        let r = fut.await;
        if let Some(w) = weak.upgrade() {
            finish(&w, p, r.as_ref());
        }
        r
    })
}

fn header_values<'a>(headers: &'a [(String, String)], name: &str) -> impl Iterator<Item = &'a str> {
    let name = name.to_owned();
    headers
        .iter()
        .filter(move |(k, _)| k.eq_ignore_ascii_case(&name))
        .map(|(_, v)| v.as_str())
}

/// `Timing-Allow-Origin` admits `origin` (`*` or a listed origin).
fn tao_allows(values: &[String], origin: &str) -> bool {
    values
        .iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .any(|v| v == "*" || v == origin)
}

fn parse_server_timing(headers: &[(String, String)]) -> Vec<ServerMetric> {
    let mut out = Vec::new();
    for v in header_values(headers, "server-timing") {
        // Split on commas outside quotes.
        let mut parts = Vec::new();
        let (mut cur, mut quoted) = (String::new(), false);
        for c in v.chars() {
            match c {
                '"' => {
                    quoted = !quoted;
                    cur.push(c);
                }
                ',' if !quoted => parts.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            }
        }
        parts.push(cur);
        for part in parts {
            let mut segs = Vec::new();
            let (mut cur, mut quoted) = (String::new(), false);
            for c in part.chars() {
                match c {
                    '"' => {
                        quoted = !quoted;
                        cur.push(c);
                    }
                    ';' if !quoted => segs.push(std::mem::take(&mut cur)),
                    _ => cur.push(c),
                }
            }
            segs.push(cur);
            let name = segs[0].trim();
            if name.is_empty() {
                continue;
            }
            let mut m = ServerMetric {
                name: name.to_owned(),
                duration: 0.0,
                description: String::new(),
            };
            for p in &segs[1..] {
                let Some((k, val)) = p.split_once('=') else {
                    continue;
                };
                let val = val.trim().trim_matches('"');
                match k.trim() {
                    "dur" => m.duration = val.parse().unwrap_or(0.0),
                    "desc" => m.description = val.to_owned(),
                    _ => {}
                }
            }
            out.push(m);
        }
    }
    out
}

fn mime_of(headers: &[(String, String)]) -> String {
    header_values(headers, "content-type")
        .next()
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .unwrap_or_default()
}

fn measure(
    w: &Window,
    t: &NetTiming,
    now_ms: f64,
    now: Instant,
    allowed: bool,
) -> (f64, f64, ResData) {
    let ms = |i: Instant| ms_of(w, now_ms, now, i);
    let (start, redirect_start, redirect_end) = match t.redirect {
        Some((s, e)) => (ms(s), ms(s), ms(e)),
        None => (ms(t.start), 0.0, 0.0),
    };
    let fetch_start = if t.redirect.is_some() {
        ms(t.fetch_start).max(redirect_end)
    } else {
        start
    };
    let mut d = ResData {
        fetch_start,
        redirect_start,
        redirect_end,
        ..ResData::default()
    };
    let (dns_start, dns_end, connect_start, secure, connect_end) = match &t.conn {
        Some(c) => {
            let dns_start = fetch_start;
            let dns_end = ms(c.dns_end).max(dns_start);
            let connect_start = ms(c.connect_start).max(dns_end);
            let secure = c.secure_start.map(|s| ms(s).max(connect_start));
            let connect_end = ms(c.connect_end).max(secure.unwrap_or(connect_start));
            (dns_start, dns_end, connect_start, secure, connect_end)
        }
        None => (fetch_start, fetch_start, fetch_start, None, fetch_start),
    };
    let request_start = ms(t.request_start).max(connect_end);
    let response_start = ms(t.response_start).max(request_start);
    let response_end = ms(t.response_end).max(response_start);
    d.response_end = response_end;
    if allowed {
        d.dns_start = dns_start;
        d.dns_end = dns_end;
        d.connect_start = connect_start;
        d.secure_start = secure.unwrap_or(0.0);
        d.connect_end = connect_end;
        d.request_start = request_start;
        d.response_start = response_start;
        d.protocol = t.protocol.to_owned();
        d.encoded = t.encoded_body_size;
        d.decoded = t.decoded_body_size;
        d.transfer = t.encoded_body_size + 300;
    } else {
        d.redirect_start = 0.0;
        d.redirect_end = 0.0;
        d.fetch_start = start;
    }
    (start, response_end, d)
}

/// Make the resource entry of a finished request.
pub(crate) fn finish(w: &Rc<Window>, p: Pending, r: Result<&NetResponse, &String>) {
    let now_ms = w.clock.now();
    let now = Instant::now();
    let ms = |i: Instant| ms_of(w, now_ms, now, i);
    let origin_url = w.url();
    let origin = origin_url.origin();
    let origin_s = origin.ascii_serialization();
    let mut data = ResData {
        initiator: p.initiator,
        blocking: p.blocking,
        ..ResData::default()
    };
    let start;
    let end;
    match r {
        Err(_) => {
            start = ms(p.started);
            end = ms(now).max(start);
            data.fetch_start = start;
            data.response_end = end;
        }
        Ok(resp) => {
            // Which hops need `Timing-Allow-Origin`.
            let (mut cross, mut allowed) = (false, true);
            let hops: Vec<(Url, Vec<String>)> = match &resp.timing {
                Some(t) => t.redirects.clone(),
                None => Vec::new(),
            };
            for (u, tao) in &hops {
                if u.origin() != origin {
                    cross = true;
                    allowed &= tao_allows(tao, &origin_s);
                }
            }
            if resp.url.origin() != origin || p.url.origin() != origin {
                cross = true;
                let tao: Vec<String> = header_values(&resp.headers, "timing-allow-origin")
                    .map(str::to_owned)
                    .collect();
                if resp.url.origin() != origin {
                    allowed &= tao_allows(&tao, &origin_s);
                }
            }
            let opaque = cross && p.mode == "no-cors";
            let cors_failed = cross
                && p.mode == "cors"
                && !super::net::cors_ok(&origin, resp)
                && p.initiator != "other";
            let m = match &resp.timing {
                Some(t) => measure(w, t, now_ms, now, allowed && !cors_failed),
                None => {
                    // Not from the network stack (a CDP-fulfilled answer, a
                    // service worker's): sizes from the body, the steps
                    // happen at the moment of arrival.
                    let s = ms(p.started);
                    let e = ms(now).max(s);
                    let ok = !cors_failed;
                    let d = ResData {
                        fetch_start: s,
                        dns_start: if ok { s } else { 0.0 },
                        dns_end: if ok { s } else { 0.0 },
                        connect_start: if ok { s } else { 0.0 },
                        connect_end: if ok { s } else { 0.0 },
                        request_start: if ok { e } else { 0.0 },
                        response_start: if ok { e } else { 0.0 },
                        response_end: e,
                        encoded: if ok { resp.body.len() as u64 } else { 0 },
                        decoded: if ok { resp.body.len() as u64 } else { 0 },
                        transfer: if ok { resp.body.len() as u64 + 300 } else { 0 },
                        ..ResData::default()
                    };
                    (s, e, d)
                }
            };
            let (s, e, mut d) = m;
            start = s;
            end = e;
            d.initiator = p.initiator;
            d.blocking = p.blocking;
            if !opaque && !cors_failed {
                d.status = resp.status;
                d.content_type = mime_of(&resp.headers);
                d.content_encoding = header_values(&resp.headers, "content-encoding")
                    .next()
                    .unwrap_or("")
                    .to_owned();
            }
            if d.transfer != 0 || d.encoded != 0 || !d.protocol.is_empty() {
                d.server_timing = parse_server_timing(&resp.headers);
            }
            if p.cached_script {
                d.encoded = 0;
                d.transfer = 0;
                d.decoded = resp.body.len() as u64;
            }
            if p.controlled {
                d.worker_start = start;
            }
            data = d;
        }
    }
    let nav_id = w.perf.borrow().nav_id;
    let mut e = Entry::new(
        Kind::Resource,
        p.url.to_string(),
        start,
        end - start,
        nav_id,
    );
    e.res = Some(data);
    add_resource(w, Rc::new(e));
}

/// Add an entry to the resource timing buffer (spec "add a
/// PerformanceResourceTiming entry"): observers first, then the buffer,
/// the secondary buffer and the buffer full event.
fn add_resource(w: &Rc<Window>, e: Rc<Entry>) {
    notify(w, &e);
    let mut st = w.perf.borrow_mut();
    if st.resources.len() < st.limit && !st.full_pending {
        insert_sorted(&mut st.resources, e);
        return;
    }
    let queue = !st.full_pending;
    st.full_pending = true;
    st.secondary.push(e);
    drop(st);
    if queue {
        w.queue_task(
            TaskSource::Networking,
            Box::new(|js, w| fire_buffer_full(js, w)),
        );
    }
}

/// The resource buffer is ordered by start time (after equal ones).
fn insert_sorted(v: &mut Vec<Rc<Entry>>, e: Rc<Entry>) {
    let at = v.partition_point(|x| x.start <= e.start);
    v.insert(at, e);
}

/// Spec "fire a buffer full event".
fn fire_buffer_full(js: &mut dyn super::Js, w: &Rc<Window>) {
    loop {
        let before = w.perf.borrow().secondary.len();
        if before == 0 {
            break;
        }
        let can_add = {
            let st = w.perf.borrow();
            st.resources.len() < st.limit
        };
        if !can_add && let Some(target) = performance_ref(w) {
            super::events::fire_event(js, w, target, "resourcetimingbufferfull", false, false);
        }
        {
            let mut st = w.perf.borrow_mut();
            while !st.secondary.is_empty() && st.resources.len() < st.limit {
                let e = st.secondary.remove(0);
                insert_sorted(&mut st.resources, e);
            }
        }
        let after = w.perf.borrow().secondary.len();
        if before <= after {
            w.perf.borrow_mut().secondary.clear();
            break;
        }
    }
    w.perf.borrow_mut().full_pending = false;
}

/// The `performance` object, if script has made it.
fn performance_ref(w: &Window) -> Option<HostRef> {
    w.cache
        .borrow()
        .get(&(0, 0, "performance", String::new()))
        .map(|&id| HostRef::new(K_OBJ, id))
}

// ---------------------------------------------------------------------------
// Navigation entry
// ---------------------------------------------------------------------------

fn nav_data(w: &Window) -> ResData {
    if let Some(d) = w.perf.borrow().nav_res.clone() {
        return d;
    }
    let info = w.perf.borrow().nav_info.clone();
    let dom_loading = w.perf.borrow().marks_nav.dom_loading.unwrap_or(0.0);
    let d = match info {
        Some(info) => {
            let url = w.history.borrow().entries.first().map(|e| e.url.clone());
            let url = url.unwrap_or_else(|| w.url());
            let origin = url.origin();
            let origin_s = origin.ascii_serialization();
            let data = match &info.timing {
                Some(t) => {
                    let mut ok = true;
                    for (u, tao) in &t.redirects {
                        if u.origin() != origin && !tao_allows(tao, &origin_s) {
                            ok = false;
                        }
                    }
                    // The start of the navigation is the window's time
                    // origin: the clock was started at the fetch.
                    let t0 = t.start;
                    let ms = |i: Instant| {
                        coarse_time(w, i.saturating_duration_since(t0).as_secs_f64() * 1000.0)
                    };
                    let (rs, re) = match t.redirect {
                        Some((s, e)) if ok => (ms(s), ms(e)),
                        _ => (0.0, 0.0),
                    };
                    // Chrome's fetch starts a little after the navigation.
                    let fetch_start = if t.redirect.is_some() {
                        ms(t.fetch_start).max(re)
                    } else {
                        ms(t.fetch_start)
                    }
                    .max(0.1);
                    let (dns_start, dns_end, connect_start, secure, connect_end) = match &t.conn {
                        Some(c) => {
                            let dns_end = ms(c.dns_end).max(fetch_start);
                            let connect_start = ms(c.connect_start).max(dns_end);
                            let secure = c.secure_start.map(|s| ms(s).max(connect_start));
                            let connect_end =
                                ms(c.connect_end).max(secure.unwrap_or(connect_start));
                            (fetch_start, dns_end, connect_start, secure, connect_end)
                        }
                        None => (fetch_start, fetch_start, fetch_start, None, fetch_start),
                    };
                    let request_start = ms(t.request_start).max(connect_end);
                    let response_start = ms(t.response_start).max(request_start);
                    let response_end = ms(t.response_end).max(response_start);
                    ResData {
                        protocol: t.protocol.to_owned(),
                        redirect_start: rs,
                        redirect_end: re,
                        fetch_start,
                        dns_start,
                        dns_end,
                        connect_start,
                        secure_start: secure.unwrap_or(0.0),
                        connect_end,
                        request_start,
                        response_start,
                        response_end,
                        transfer: info.encoded + 300,
                        encoded: info.encoded,
                        decoded: info.decoded,
                        redirect_count: if ok { t.redirects.len() as u32 } else { 0 },
                        ..ResData::default()
                    }
                }
                None => ResData {
                    response_start: dom_loading.min(1.0),
                    response_end: dom_loading.min(1.0),
                    ..ResData::default()
                },
            };
            ResData {
                initiator: "navigation",
                content_type: mime_of(&info.headers),
                content_encoding: header_values(&info.headers, "content-encoding")
                    .next()
                    .unwrap_or("")
                    .to_owned(),
                status: info.status,
                server_timing: parse_server_timing(&info.headers),
                nav_type: info.nav_type,
                ..data
            }
        }
        None => ResData {
            initiator: "navigation",
            nav_type: "navigate",
            status: 0,
            ..ResData::default()
        },
    };
    w.perf.borrow_mut().nav_res = Some(d.clone());
    d
}

fn ensure_nav(w: &Window) -> Option<Rc<Entry>> {
    if w.is_worker() {
        return None;
    }
    if let Some(n) = w.perf.borrow().nav.clone() {
        return Some(n);
    }
    let d = nav_data(w);
    let url = w
        .history
        .borrow()
        .entries
        .first()
        .map(|e| e.url.to_string())
        .unwrap_or_else(|| w.url().to_string());
    let nav_id = w.perf.borrow().nav_id;
    let mut e = Entry::new(Kind::Navigation, url, 0.0, 0.0, nav_id);
    e.res = Some(d);
    let e = Rc::new(e);
    w.perf.borrow_mut().nav = Some(e.clone());
    Some(e)
}

fn ensure_vis(w: &Window) -> Option<Rc<Entry>> {
    if w.is_worker() {
        return None;
    }
    if let Some(v) = w.perf.borrow().vis.clone() {
        return Some(v);
    }
    let nav_id = w.perf.borrow().nav_id;
    let e = Rc::new(Entry::new(
        Kind::Visibility,
        "visible".into(),
        0.0,
        0.0,
        nav_id,
    ));
    w.perf.borrow_mut().vis = Some(e.clone());
    Some(e)
}

/// A navigation timing value (the attributes `PerformanceNavigationTiming`
/// adds, and the `PerformanceTiming` ones) in window milliseconds; `None`
/// when the point was not reached (or would reveal cross-origin timing).
fn nav_point(w: &Window, name: &str) -> Option<f64> {
    let d = nav_data(w);
    let m = w.perf.borrow().marks_nav;
    // The network steps exist when the document came over the network.
    let net = !d.protocol.is_empty();
    let some_if = |c: bool, v: f64| c.then_some(v);
    match name {
        "navigationStart" => Some(0.0),
        "redirectStart" => some_if(d.redirect_count > 0, d.redirect_start),
        "redirectEnd" => some_if(d.redirect_count > 0, d.redirect_end),
        "fetchStart" => Some(d.fetch_start),
        "domainLookupStart" => some_if(net, d.dns_start),
        "domainLookupEnd" => some_if(net, d.dns_end),
        "connectStart" => some_if(net, d.connect_start),
        "connectEnd" => some_if(net, d.connect_end),
        "secureConnectionStart" => some_if(d.secure_start > 0.0, d.secure_start),
        "requestStart" => some_if(net, d.request_start),
        "responseStart" => some_if(net, d.response_start),
        "responseEnd" => Some(d.response_end),
        "domLoading" => m.dom_loading.map(|l| l.max(d.response_end)),
        "domInteractive" => m.dom_interactive,
        "domContentLoadedEventStart" => m.dcl_start,
        "domContentLoadedEventEnd" => m.dcl_end,
        "domComplete" => m.dom_complete,
        "loadEventStart" => m.load_start,
        "loadEventEnd" => m.load_end,
        _ => None,
    }
}

/// Legacy `performance.timing` value (milliseconds since the epoch, 0 when
/// the point was not reached).
pub(crate) fn legacy_timing(w: &Window, field: &str) -> f64 {
    if field == "navigationStart" {
        return w.clock.time_origin.floor();
    }
    nav_point(w, field).map_or(0.0, |ms| (w.clock.time_origin + ms).floor())
}

/// Legacy `performance.navigation` type and redirect count.
pub(crate) fn legacy_navigation(w: &Window) -> (u32, u32) {
    let d = nav_data(w);
    let t = match d.nav_type {
        "reload" => 1,
        "back_forward" => 2,
        _ => 0,
    };
    (t, d.redirect_count)
}

// ---------------------------------------------------------------------------
// Observers and notifications
// ---------------------------------------------------------------------------

/// Tell the observers of the entry's type.
fn notify(w: &Rc<Window>, e: &Rc<Entry>) {
    let ids = w.perf.borrow().observers.clone();
    for id in ids {
        let queue = w
            .with_obj(id, |o| match o {
                Obj::Perf(p) => match &mut **p {
                    PerfObj::Observer(ob) if ob.types.contains(&e.kind) => {
                        ob.queue.push(e.clone());
                        !std::mem::replace(&mut ob.task_queued, true)
                    }
                    _ => false,
                },
                _ => false,
            })
            .unwrap_or(false);
        if queue {
            queue_delivery(w, id);
        }
    }
}

fn queue_delivery(w: &Rc<Window>, id: u32) {
    w.queue_task(
        TaskSource::Networking,
        Box::new(move |js, w| deliver(js, w, id)),
    );
}

/// Run an observer's callback with its queued entries.
fn deliver(js: &mut dyn super::Js, w: &Rc<Window>, id: u32) {
    let taken = w
        .with_obj(id, |o| match o {
            Obj::Perf(p) => match &mut **p {
                PerfObj::Observer(ob) => {
                    ob.task_queued = false;
                    if ob.queue.is_empty() {
                        None
                    } else {
                        let dropped = std::mem::take(&mut ob.dropped_pending);
                        Some((ob.callback, std::mem::take(&mut ob.queue), dropped))
                    }
                }
                _ => None,
            },
            _ => None,
        })
        .flatten();
    let Some((cb, mut entries, dropped)) = taken else {
        return;
    };
    sort_entries(&mut entries);
    let list = w.new_obj(Obj::Perf(Box::new(PerfObj::EntryList(entries))));
    let me = w.obj_arg(id);
    let opts = Arg::Json(if dropped {
        "{\"droppedEntriesCount\":0}".into()
    } else {
        "{}".into()
    });
    if let Err(e) = js.call(cb, me.clone(), &[w.obj_arg(list), me, opts]) {
        w.report_exception(js, &e);
    }
}

fn sort_entries(v: &mut [Rc<Entry>]) {
    v.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

// ---------------------------------------------------------------------------
// Timeline queries
// ---------------------------------------------------------------------------

/// Every entry, as `getEntries()` orders them: grouped by type, then
/// stably sorted by start time.
fn all_entries(w: &Window) -> Vec<Rc<Entry>> {
    let nav = ensure_nav(w);
    let vis = ensure_vis(w);
    let st = w.perf.borrow();
    let mut v: Vec<Rc<Entry>> = Vec::with_capacity(st.resources.len() + 8);
    v.extend(st.resources.iter().cloned());
    v.extend(nav);
    v.extend(st.marks.iter().cloned());
    v.extend(st.measures.iter().cloned());
    v.extend(st.paint.iter().cloned());
    v.extend(vis);
    drop(st);
    sort_entries(&mut v);
    v
}

fn entries_of(w: &Window, kind: Kind) -> Vec<Rc<Entry>> {
    match kind {
        Kind::Navigation => ensure_nav(w).into_iter().collect(),
        Kind::Visibility => ensure_vis(w).into_iter().collect(),
        _ => {
            let st = w.perf.borrow();
            let src = match kind {
                Kind::Mark => &st.marks,
                Kind::Measure => &st.measures,
                Kind::Resource => &st.resources,
                _ => &st.paint,
            };
            let mut v: Vec<_> = src.clone();
            drop(st);
            if matches!(kind, Kind::Mark | Kind::Measure | Kind::Paint) {
                sort_entries(&mut v);
            }
            v
        }
    }
}

fn entry_obj(w: &Window, e: &Rc<Entry>) -> u32 {
    if let Some(id) = e.obj.get() {
        return id;
    }
    let id = w.new_obj(Obj::Perf(Box::new(PerfObj::Entry(e.clone()))));
    e.obj.set(Some(id));
    id
}

fn entries_arg(w: &Window, v: &[Rc<Entry>]) -> Arg {
    Arg::Array(v.iter().map(|e| w.obj_arg(entry_obj(w, e))).collect())
}

fn this_entry(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<Rc<Entry>, HostError> {
    let id = this_obj(cx)?;
    w.with_obj(id, |o| match o {
        Obj::Perf(p) => match &**p {
            PerfObj::Entry(e) => Some(e.clone()),
            _ => None,
        },
        _ => None,
    })
    .flatten()
    .ok_or_else(illegal)
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

/// JavaScript's ToNumber for the values a timestamp option can hold.
fn to_number(cx: &mut CallCtx<'_, '_, '_>, h: Handle) -> f64 {
    match cx.handle_value(h) {
        JsValue::Number(n) => n,
        JsValue::Bool(b) => f64::from(u8::from(b)),
        JsValue::Null => 0.0,
        JsValue::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse().unwrap_or(f64::NAN)
            }
        }
        _ => f64::NAN,
    }
}

fn non_finite(prefix: &str, prop: &str, dict: &str) -> HostError {
    HostError::type_error(format!(
        "{prefix}Failed to read the '{prop}' property from '{dict}': The provided double value is non-finite."
    ))
}

fn fail(op: &str, m: impl std::fmt::Display) -> String {
    format!("Failed to execute '{op}' on 'Performance': {m}")
}

/// Structured-clone `detail` (`None` for undefined).
fn clone_detail(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    h: Option<Handle>,
    prefix: &str,
) -> Result<Option<lb_js::Serialized>, HostError> {
    let Some(h) = h else { return Ok(None) };
    let mut writer = |_: HostRef, class: &str| -> Result<u32, String> {
        Err(format!("{class} object could not be cloned."))
    };
    let r = cx.serialize(h, &[], &mut writer);
    cx.release(h);
    match r {
        Ok(s) => Ok(Some(s)),
        Err(Some(m)) => Err(w.rt.err("DataCloneError", format!("{prefix}{m}"))),
        Err(None) => Err(HostError::Thrown),
    }
}

// ---------------------------------------------------------------------------
// mark and measure
// ---------------------------------------------------------------------------

/// `mark(name, options)` and `new PerformanceMark(name, options)`.
fn make_mark(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Rc<Window>,
    prefix: &str,
    is_ctor: bool,
) -> Result<Rc<Entry>, HostError> {
    if cx.is_empty() {
        return Err(HostError::type_error(format!(
            "{prefix}1 argument required, but only 0 present."
        )));
    }
    let name = cx.arg_string(0)?;
    match cx.arg_kind(1) {
        ValueKind::Undefined | ValueKind::Null | ValueKind::Object | ValueKind::Function => {}
        _ => {
            return Err(HostError::type_error(format!(
                "{prefix}The provided value is not of type 'PerformanceMarkOptions'."
            )));
        }
    }
    let d = Dict::arg(cx, 1);
    let start = match d.get(cx, "startTime")? {
        Some(h) => {
            let n = to_number(cx, h);
            cx.release(h);
            if !n.is_finite() {
                d.release(cx);
                return Err(non_finite(prefix, "startTime", "PerformanceMarkOptions"));
            }
            Some(n)
        }
        None => None,
    };
    let detail = d.get(cx, "detail")?;
    d.release(cx);
    if !w.is_worker() && TIMING_ATTRS.contains(&name.as_str()) && !is_ctor {
        if let Some(h) = detail {
            cx.release(h);
        }
        return Err(w.rt.err(
            "SyntaxError",
            format!(
                "{prefix}'{name}' is part of the PerformanceTiming interface, and cannot be used as a mark name."
            ),
        ));
    }
    if let Some(s) = start
        && s < 0.0
    {
        if let Some(h) = detail {
            cx.release(h);
        }
        return Err(HostError::type_error(format!(
            "{prefix}'{name}' cannot have a negative start time."
        )));
    }
    let detail = clone_detail(cx, w, detail, prefix)?;
    let start = start.unwrap_or_else(|| coarse_now(w));
    let nav_id = w.perf.borrow().nav_id;
    let mut e = Entry::new(Kind::Mark, name, start, 0.0, nav_id);
    e.detail = detail;
    Ok(Rc::new(e))
}

/// A `measure()` start or end: a number, or the name of a mark or of a
/// `PerformanceTiming` attribute.
fn mark_time(
    w: &Window,
    h: Option<Handle>,
    cx: &mut CallCtx<'_, '_, '_>,
    numbers: bool,
) -> Result<Option<f64>, HostError> {
    let Some(h) = h else { return Ok(None) };
    let kind = cx.handle_kind(h);
    if numbers && kind == ValueKind::Number {
        let n = to_number(cx, h);
        cx.release(h);
        return Ok(Some(n));
    }
    let name = cx.handle_string(h);
    cx.release(h);
    let name = name?;
    resolve_mark(w, &name).map(Some)
}

fn resolve_mark(w: &Window, name: &str) -> Result<f64, HostError> {
    let marks = w.perf.borrow().marks.clone();
    if let Some(m) = marks.iter().rev().find(|m| m.name == name) {
        return Ok(m.start);
    }
    if !w.is_worker() && TIMING_ATTRS.contains(&name) {
        // Blink: the legacy attribute (whole milliseconds) minus
        // `navigationStart`, 0 meaning empty.
        let legacy = legacy_timing(w, name);
        let v = (legacy != 0.0).then(|| legacy - legacy_timing(w, "navigationStart"));
        let Some(v) = v else {
            return Err(w.rt.err(
                "InvalidAccessError",
                fail(
                    "measure",
                    format!(
                        "'{name}' is empty: either the event hasn't happened yet, or it would provide cross-origin timing information."
                    ),
                ),
            ));
        };
        return Ok(v);
    }
    Err(w.rt.err(
        "SyntaxError",
        fail("measure", format!("The mark '{name}' does not exist.")),
    ))
}

fn do_measure(cx: &mut CallCtx<'_, '_, '_>, w: &Rc<Window>) -> Result<Rc<Entry>, HostError> {
    need_args(cx, 1, "measure", "Performance")?;
    let name = cx.arg_string(0)?;
    let opts_arg = matches!(
        cx.arg_kind(1),
        ValueKind::Object | ValueKind::Function | ValueKind::Null
    );
    let tf = |m: &str| HostError::type_error(fail("measure", m));
    let (start, end, duration, detail);
    if opts_arg || cx.len() < 2 || cx.arg_kind(1) == ValueKind::Undefined {
        let d = Dict::arg(cx, 1);
        let s = d.get(cx, "start")?;
        let e = d.get(cx, "end")?;
        let du = d.get(cx, "duration")?;
        let de = d.get(cx, "detail")?;
        d.release(cx);
        let non_empty = s.is_some() || e.is_some() || du.is_some() || de.is_some();
        if non_empty {
            if cx.len() > 2 && cx.arg_kind(2) != ValueKind::Undefined {
                for h in [s, e, du, de].into_iter().flatten() {
                    cx.release(h);
                }
                return Err(tf(
                    "If a non-empty PerformanceMeasureOptions object was passed, |end_mark| must not be passed.",
                ));
            }
            if s.is_none() && e.is_none() {
                for h in [du, de].into_iter().flatten() {
                    cx.release(h);
                }
                return Err(tf(
                    "If a non-empty PerformanceMeasureOptions object was passed, at least one of its 'start' or 'end' properties must be present.",
                ));
            }
            if s.is_some() && e.is_some() && du.is_some() {
                for h in [s, e, du, de].into_iter().flatten() {
                    cx.release(h);
                }
                return Err(tf(
                    "If a non-empty PerformanceMeasureOptions object was passed, it must not have all of its 'start', 'duration', and 'end' properties defined",
                ));
            }
            let num = |cx: &mut CallCtx<'_, '_, '_>, h: Option<Handle>, prop: &str| match h {
                Some(h) if cx.handle_kind(h) == ValueKind::Number => {
                    let n = to_number(cx, h);
                    if !n.is_finite() {
                        cx.release(h);
                        return Err(non_finite(
                            "Failed to execute 'measure' on 'Performance': ",
                            prop,
                            "PerformanceMeasureOptions",
                        ));
                    }
                    Ok((Some(h), Some(n)))
                }
                other => Ok((other, None)),
            };
            let (s, sn) = num(cx, s, "start")?;
            let (e, en) = num(cx, e, "end")?;
            let dur = match du {
                Some(h) => {
                    let n = to_number(cx, h);
                    cx.release(h);
                    if !n.is_finite() {
                        return Err(non_finite(
                            "Failed to execute 'measure' on 'Performance': ",
                            "duration",
                            "PerformanceMeasureOptions",
                        ));
                    }
                    Some(n)
                }
                None => None,
            };
            let st = match sn {
                Some(n) => {
                    if let Some(h) = s {
                        cx.release(h);
                    }
                    Some(n)
                }
                None => mark_time(w, s, cx, true)?,
            };
            let en2 = match en {
                Some(n) => {
                    if let Some(h) = e {
                        cx.release(h);
                    }
                    Some(n)
                }
                None => mark_time(w, e, cx, true)?,
            };
            for t in [st, en2].into_iter().flatten() {
                if t < 0.0 {
                    return Err(tf(&format!("'{name}' cannot have a negative time stamp.")));
                }
            }
            start = st;
            end = en2;
            duration = dur;
            detail = clone_detail(cx, w, de, &fail("measure", ""))?;
        } else {
            start = None;
            end = if cx.len() > 2 && cx.arg_kind(2) != ValueKind::Undefined {
                let h = cx.arg_handle(2);
                mark_time(w, Some(h), cx, false)?
            } else {
                None
            };
            duration = None;
            detail = None;
        }
    } else {
        // `measure(name, startMark, endMark)`
        let sh = cx.arg_handle(1);
        let st = mark_time(w, Some(sh), cx, false)?;
        let en = if cx.len() > 2 && cx.arg_kind(2) != ValueKind::Undefined {
            let h = cx.arg_handle(2);
            mark_time(w, Some(h), cx, false)?
        } else {
            None
        };
        start = st;
        end = en;
        duration = None;
        detail = None;
    }
    let now = coarse_now(w);
    let (s, e) = match (start, end, duration) {
        (Some(s), Some(e), _) => (s, e),
        (Some(s), None, Some(d)) => (s, s + d),
        (None, Some(e), Some(d)) => (e - d, e),
        (Some(s), None, None) => (s, now),
        (None, Some(e), None) => (0.0, e),
        _ => (0.0, now),
    };
    let nav_id = w.perf.borrow().nav_id;
    let mut ent = Entry::new(Kind::Measure, name, s, e - s, nav_id);
    ent.detail = detail;
    Ok(Rc::new(ent))
}

fn push_user(w: &Rc<Window>, e: Rc<Entry>) {
    {
        let mut st = w.perf.borrow_mut();
        match e.kind {
            Kind::Mark => st.marks.push(e.clone()),
            _ => st.measures.push(e.clone()),
        }
    }
    notify(w, &e);
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

fn num(n: f64) -> String {
    serde_json::Value::from(n).to_string()
}

fn jstr(s: &str) -> String {
    serde_json::Value::from(s).to_string()
}

fn server_json(v: &[ServerMetric]) -> String {
    let items: Vec<String> = v
        .iter()
        .map(|m| {
            format!(
                "{{\"name\":{},\"duration\":{},\"description\":{}}}",
                jstr(&m.name),
                num(m.duration),
                jstr(&m.description)
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

fn entry_duration(w: &Window, e: &Entry) -> f64 {
    if e.kind == Kind::Navigation {
        w.perf.borrow().marks_nav.load_end.unwrap_or(0.0)
    } else {
        e.duration
    }
}

fn entry_json(w: &Window, e: &Entry) -> String {
    let mut s = format!(
        "{{\"name\":{},\"entryType\":\"{}\",\"startTime\":{},\"duration\":{},\"navigationId\":{}",
        jstr(&e.name),
        e.kind.type_name(),
        num(e.start),
        num(entry_duration(w, e)),
        e.nav_id
    );
    match e.kind {
        Kind::Resource | Kind::Navigation => {
            let nav = e.kind == Kind::Navigation;
            let d = if nav {
                nav_data(w)
            } else {
                e.res.clone().unwrap_or_default()
            };
            s.push_str(&format!(
                ",\"initiatorType\":{},\"deliveryType\":\"\",\"nextHopProtocol\":{},\"renderBlockingStatus\":{},\"contentType\":{},\"contentEncoding\":{},\"workerStart\":{},\"workerRouterEvaluationStart\":0,\"workerCacheLookupStart\":0,\"workerMatchedSourceType\":\"\",\"workerFinalSourceType\":\"\",\"redirectStart\":{},\"redirectEnd\":{},\"fetchStart\":{},\"domainLookupStart\":{},\"domainLookupEnd\":{},\"connectStart\":{},\"secureConnectionStart\":{},\"connectEnd\":{},\"requestStart\":{},\"responseStart\":{},\"firstInterimResponseStart\":0,\"finalResponseHeadersStart\":{},\"responseEnd\":{},\"transferSize\":{},\"encodedBodySize\":{},\"decodedBodySize\":{},\"responseStatus\":{},\"serverTiming\":{}",
                jstr(d.initiator),
                jstr(&d.protocol),
                jstr(if d.blocking { "blocking" } else { "non-blocking" }),
                jstr(&d.content_type),
                jstr(&d.content_encoding),
                num(d.worker_start),
                num(d.redirect_start),
                num(d.redirect_end),
                num(d.fetch_start),
                num(d.dns_start),
                num(d.dns_end),
                num(d.connect_start),
                num(d.secure_start),
                num(d.connect_end),
                num(d.request_start),
                num(d.response_start),
                num(d.response_start),
                num(d.response_end),
                d.transfer,
                d.encoded,
                d.decoded,
                d.status,
                server_json(&d.server_timing),
            ));
            if nav {
                s.push_str(&format!(
                    ",\"unloadEventStart\":0,\"unloadEventEnd\":0,\"domInteractive\":{},\"domContentLoadedEventStart\":{},\"domContentLoadedEventEnd\":{},\"domComplete\":{},\"loadEventStart\":{},\"loadEventEnd\":{},\"type\":{},\"redirectCount\":{},\"activationStart\":0,\"criticalCHRestart\":0,\"notRestoredReasons\":null,\"confidence\":{{\"randomizedTriggerRate\":0.4994798,\"value\":\"low\"}}",
                    num(nav_point(w, "domInteractive").unwrap_or(0.0)),
                    num(nav_point(w, "domContentLoadedEventStart").unwrap_or(0.0)),
                    num(nav_point(w, "domContentLoadedEventEnd").unwrap_or(0.0)),
                    num(nav_point(w, "domComplete").unwrap_or(0.0)),
                    num(nav_point(w, "loadEventStart").unwrap_or(0.0)),
                    num(nav_point(w, "loadEventEnd").unwrap_or(0.0)),
                    jstr(d.nav_type),
                    d.redirect_count,
                ));
            }
        }
        Kind::Paint => {
            s.push_str(&format!(
                ",\"paintTime\":{},\"presentationTime\":{}",
                num(e.start),
                num(e.start)
            ));
        }
        _ => {}
    }
    s.push('}');
    s
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// A getter with a number or string value.
enum V {
    N(f64),
    S(String),
}

impl From<V> for Arg {
    fn from(v: V) -> Arg {
        match v {
            V::N(n) => Arg::Number(n),
            V::S(s) => Arg::String(s),
        }
    }
}

fn res_value(w: &Window, e: &Entry, name: &str) -> V {
    let nav = e.kind == Kind::Navigation;
    let d = if nav {
        nav_data(w)
    } else {
        e.res.clone().unwrap_or_default()
    };
    match name {
        "initiatorType" => V::S(d.initiator.to_owned()),
        "nextHopProtocol" => V::S(d.protocol),
        "deliveryType" => V::S(String::new()),
        "renderBlockingStatus" => V::S(
            if d.blocking {
                "blocking"
            } else {
                "non-blocking"
            }
            .into(),
        ),
        "contentType" => V::S(d.content_type),
        "contentEncoding" => V::S(d.content_encoding),
        "workerMatchedSourceType" | "workerFinalSourceType" => V::S(String::new()),
        "workerStart" => V::N(d.worker_start),
        "redirectStart" => V::N(d.redirect_start),
        "redirectEnd" => V::N(d.redirect_end),
        "fetchStart" => V::N(d.fetch_start),
        "domainLookupStart" => V::N(d.dns_start),
        "domainLookupEnd" => V::N(d.dns_end),
        "connectStart" => V::N(d.connect_start),
        "secureConnectionStart" => V::N(d.secure_start),
        "connectEnd" => V::N(d.connect_end),
        "requestStart" => V::N(d.request_start),
        "responseStart" | "finalResponseHeadersStart" => V::N(d.response_start),
        "responseEnd" => V::N(d.response_end),
        "transferSize" => V::N(d.transfer as f64),
        "encodedBodySize" => V::N(d.encoded as f64),
        "decodedBodySize" => V::N(d.decoded as f64),
        "responseStatus" => V::N(f64::from(d.status)),
        _ => V::N(0.0),
    }
}

/// A frozen array in the calling realm.
fn frozen_array(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    items: Vec<Arg>,
) -> Result<Handle, HostError> {
    let arr = cx.make_handle(Arg::Array(items))?;
    let freeze = w.helpers.borrow().freeze;
    if let Some(f) = freeze {
        let _ = cx.try_call(f, Arg::Undefined, &[Arg::Handle(arr)]);
    }
    Ok(arr)
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    // ----- Performance -----
    getter(reg, rt, "Performance", "eventCounts", |cx, w| {
        this_obj(cx)?;
        let id = w.cached_obj((0, 0, "eventCounts", String::new()), || {
            Obj::Perf(Box::new(PerfObj::EventCounts))
        });
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, "Performance", "getEntries", |cx, w| {
        this_obj(cx)?;
        cx.ret(entries_arg(w, &all_entries(w)))
    });
    method(reg, rt, "Performance", "getEntriesByType", |cx, w| {
        this_obj(cx)?;
        need_args(cx, 1, "getEntriesByType", "Performance")?;
        let t = cx.arg_string(0)?;
        let v = Kind::of_type(&t)
            .map(|k| entries_of(w, k))
            .unwrap_or_default();
        cx.ret(entries_arg(w, &v))
    });
    method(reg, rt, "Performance", "getEntriesByName", |cx, w| {
        this_obj(cx)?;
        need_args(cx, 1, "getEntriesByName", "Performance")?;
        let name = cx.arg_string(0)?;
        let ty = if cx.len() > 1 && cx.arg_kind(1) != ValueKind::Undefined {
            Some(cx.arg_string(1)?)
        } else {
            None
        };
        let v: Vec<Rc<Entry>> = match ty {
            Some(t) => Kind::of_type(&t)
                .map(|k| entries_of(w, k))
                .unwrap_or_default(),
            None => all_entries(w),
        }
        .into_iter()
        .filter(|e| e.name == name)
        .collect();
        cx.ret(entries_arg(w, &v))
    });
    method(reg, rt, "Performance", "mark", |cx, w| {
        this_obj(cx)?;
        let e = make_mark(cx, w, "Failed to execute 'mark' on 'Performance': ", false)?;
        push_user(w, e.clone());
        cx.ret(w.obj_arg(entry_obj(w, &e)))
    });
    method(reg, rt, "Performance", "measure", |cx, w| {
        this_obj(cx)?;
        let e = do_measure(cx, w)?;
        push_user(w, e.clone());
        cx.ret(w.obj_arg(entry_obj(w, &e)))
    });
    method(reg, rt, "Performance", "clearMarks", |cx, w| {
        this_obj(cx)?;
        let name = super::opt_string(cx, 0)?;
        let mut st = w.perf.borrow_mut();
        match name {
            Some(n) => st.marks.retain(|e| e.name != n),
            None => st.marks.clear(),
        }
        Ok(())
    });
    method(reg, rt, "Performance", "clearMeasures", |cx, w| {
        this_obj(cx)?;
        let name = super::opt_string(cx, 0)?;
        let mut st = w.perf.borrow_mut();
        match name {
            Some(n) => st.measures.retain(|e| e.name != n),
            None => st.measures.clear(),
        }
        Ok(())
    });
    method(reg, rt, "Performance", "clearResourceTimings", |cx, w| {
        this_obj(cx)?;
        w.perf.borrow_mut().resources.clear();
        Ok(())
    });
    method(
        reg,
        rt,
        "Performance",
        "setResourceTimingBufferSize",
        |cx, w| {
            this_obj(cx)?;
            need_args(cx, 1, "setResourceTimingBufferSize", "Performance")?;
            let n = cx.arg_u32(0)?;
            w.perf.borrow_mut().limit = n as usize;
            Ok(())
        },
    );
    method(reg, rt, "Performance", "toJSON", |cx, w| {
        this_obj(cx)?;
        let origin = (w.clock.time_origin * 10.0).floor() / 10.0;
        if w.is_worker() {
            return cx.ret(Arg::Json(format!("{{\"timeOrigin\":{}}}", num(origin))));
        }
        // `{timeOrigin, timing, navigation}` with the live `PerformanceTiming`
        // and `PerformanceNavigation` objects (they serialize themselves).
        let obj = cx.make_handle(Arg::Json(format!("{{\"timeOrigin\":{}}}", num(origin))))?;
        let timing =
            super::navigator::single_nav(w, "nav:timing", super::navigator::NavObj::Timing);
        let nav =
            super::navigator::single_nav(w, "nav:navigation", super::navigator::NavObj::Navigation);
        cx.set_property(obj, "timing", timing)?;
        cx.set_property(obj, "navigation", nav)?;
        let r = cx.ret(Arg::Handle(obj));
        cx.release(obj);
        r
    });

    // ----- entries -----
    getter(reg, rt, "PerformanceEntry", "name", |cx, w| {
        let e = this_entry(cx, w)?;
        cx.ret(e.name.clone())
    });
    getter(reg, rt, "PerformanceEntry", "entryType", |cx, w| {
        let e = this_entry(cx, w)?;
        cx.ret(e.kind.type_name())
    });
    getter(reg, rt, "PerformanceEntry", "startTime", |cx, w| {
        let e = this_entry(cx, w)?;
        cx.ret(e.start)
    });
    getter(reg, rt, "PerformanceEntry", "duration", |cx, w| {
        let e = this_entry(cx, w)?;
        cx.ret(entry_duration(w, &e))
    });
    getter(reg, rt, "PerformanceEntry", "navigationId", |cx, w| {
        let e = this_entry(cx, w)?;
        cx.ret(e.nav_id)
    });
    method(reg, rt, "PerformanceEntry", "toJSON", |cx, w| {
        let e = this_entry(cx, w)?;
        cx.ret(Arg::Json(entry_json(w, &e)))
    });
    for iface in ["PerformanceMark", "PerformanceMeasure"] {
        getter(reg, rt, iface, "detail", |cx, w| {
            let e = this_entry(cx, w)?;
            if let Some(h) = e.detail_value.get() {
                return cx.ret(Arg::Handle(h));
            }
            let Some(s) = &e.detail else {
                return cx.ret(Arg::Null);
            };
            let h = cx
                .deserialize(s, &[])
                .map_err(|m| w.rt.err("DataCloneError", m))?;
            e.detail_value.set(Some(h));
            cx.ret(Arg::Handle(h))
        });
    }
    ctor(reg, rt, "PerformanceMark", |cx, w| {
        if !cx.is_construct_call() {
            return Err(HostError::type_error(
                "Failed to construct 'PerformanceMark': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
            ));
        }
        let e = make_mark(cx, w, "Failed to construct 'PerformanceMark': ", true)?;
        let id = entry_obj(w, &e);
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    for name in [
        "initiatorType",
        "nextHopProtocol",
        "deliveryType",
        "workerStart",
        "redirectStart",
        "redirectEnd",
        "fetchStart",
        "domainLookupStart",
        "domainLookupEnd",
        "connectStart",
        "connectEnd",
        "secureConnectionStart",
        "requestStart",
        "responseStart",
        "responseEnd",
        "transferSize",
        "encodedBodySize",
        "decodedBodySize",
        "renderBlockingStatus",
        "responseStatus",
        "contentType",
        "contentEncoding",
        "finalResponseHeadersStart",
        "firstInterimResponseStart",
        "workerRouterEvaluationStart",
        "workerCacheLookupStart",
        "workerMatchedSourceType",
        "workerFinalSourceType",
    ] {
        getter(reg, rt, "PerformanceResourceTiming", name, move |cx, w| {
            let e = this_entry(cx, w)?;
            cx.ret(res_value(w, &e, name))
        });
    }
    getter(
        reg,
        rt,
        "PerformanceResourceTiming",
        "serverTiming",
        |cx, w| {
            let e = this_entry(cx, w)?;
            let metrics = if e.kind == Kind::Navigation {
                nav_data(w).server_timing
            } else {
                e.res
                    .as_ref()
                    .map(|r| r.server_timing.clone())
                    .unwrap_or_default()
            };
            let ids = {
                let mut objs = e.server_objs.borrow_mut();
                if objs.len() != metrics.len() {
                    objs.clear();
                    for m in &metrics {
                        objs.push(w.new_obj(Obj::Perf(Box::new(PerfObj::ServerTiming(m.clone())))));
                    }
                }
                objs.clone()
            };
            let arr = frozen_array(cx, w, ids.into_iter().map(|i| w.obj_arg(i)).collect())?;
            let r = cx.ret(Arg::Handle(arr));
            cx.release(arr);
            r
        },
    );
    for (name, kind) in [("name", 0), ("duration", 1), ("description", 2)] {
        getter(reg, rt, "PerformanceServerTiming", name, move |cx, w| {
            let id = this_obj(cx)?;
            let m = w
                .with_obj(id, |o| match o {
                    Obj::Perf(p) => match &**p {
                        PerfObj::ServerTiming(m) => Some(m.clone()),
                        _ => None,
                    },
                    _ => None,
                })
                .flatten()
                .ok_or_else(illegal)?;
            match kind {
                0 => cx.ret(m.name),
                1 => cx.ret(m.duration),
                _ => cx.ret(m.description),
            }
        });
    }
    method(reg, rt, "PerformanceServerTiming", "toJSON", |cx, w| {
        let id = this_obj(cx)?;
        let m = w
            .with_obj(id, |o| match o {
                Obj::Perf(p) => match &**p {
                    PerfObj::ServerTiming(m) => Some(m.clone()),
                    _ => None,
                },
                _ => None,
            })
            .flatten()
            .ok_or_else(illegal)?;
        cx.ret(Arg::Json(
            server_json(&[m])[1..].trim_end_matches(']').to_owned(),
        ))
    });
    // Navigation timing.
    for name in [
        "unloadEventStart",
        "unloadEventEnd",
        "domInteractive",
        "domContentLoadedEventStart",
        "domContentLoadedEventEnd",
        "domComplete",
        "loadEventStart",
        "loadEventEnd",
    ] {
        getter(
            reg,
            rt,
            "PerformanceNavigationTiming",
            name,
            move |cx, w| {
                let e = this_entry(cx, w)?;
                if e.kind != Kind::Navigation {
                    return Err(illegal());
                }
                cx.ret(nav_point(w, name).unwrap_or(0.0))
            },
        );
    }
    getter(reg, rt, "PerformanceNavigationTiming", "type", |cx, w| {
        this_entry(cx, w)?;
        cx.ret(nav_data(w).nav_type)
    });
    getter(
        reg,
        rt,
        "PerformanceNavigationTiming",
        "redirectCount",
        |cx, w| {
            this_entry(cx, w)?;
            cx.ret(nav_data(w).redirect_count)
        },
    );
    for name in ["activationStart", "criticalCHRestart"] {
        getter(reg, rt, "PerformanceNavigationTiming", name, |cx, w| {
            this_entry(cx, w)?;
            cx.ret(0)
        });
    }
    getter(
        reg,
        rt,
        "PerformanceNavigationTiming",
        "notRestoredReasons",
        |cx, w| {
            this_entry(cx, w)?;
            cx.ret(Arg::Null)
        },
    );
    getter(
        reg,
        rt,
        "PerformanceNavigationTiming",
        "confidence",
        |cx, w| {
            this_entry(cx, w)?;
            cx.ret(Arg::Json(
                "{\"randomizedTriggerRate\":0.4994798,\"value\":\"low\"}".into(),
            ))
        },
    );
    for name in ["paintTime", "presentationTime"] {
        getter(reg, rt, "PerformancePaintTiming", name, |cx, w| {
            let e = this_entry(cx, w)?;
            cx.ret(e.start)
        });
    }

    // ----- PerformanceObserver -----
    ctor(reg, rt, "PerformanceObserver", |cx, w| {
        if !cx.is_construct_call() {
            return Err(HostError::type_error(
                "Failed to construct 'PerformanceObserver': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
            ));
        }
        if cx.is_empty() {
            return Err(HostError::type_error(
                "Failed to construct 'PerformanceObserver': 1 argument required, but only 0 present.",
            ));
        }
        if cx.arg_kind(0) != ValueKind::Function {
            return Err(HostError::type_error(
                "Failed to construct 'PerformanceObserver': parameter 1 is not of type 'Function'.",
            ));
        }
        let cb = cx.arg_handle(0);
        let id = w.new_obj(Obj::Perf(Box::new(PerfObj::Observer(Observer {
            callback: cb,
            types: Vec::new(),
            mode: ObserveMode::None,
            queue: Vec::new(),
            task_queued: false,
            dropped_pending: false,
        }))));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    method(reg, rt, "PerformanceObserver", "observe", |cx, w| {
        observe(cx, w)
    });
    method(reg, rt, "PerformanceObserver", "disconnect", |cx, w| {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| match o {
            Obj::Perf(p) => match &mut **p {
                PerfObj::Observer(ob) => {
                    ob.queue.clear();
                    ob.types.clear();
                    Some(())
                }
                _ => None,
            },
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)?;
        w.perf.borrow_mut().observers.retain(|&i| i != id);
        Ok(())
    });
    method(reg, rt, "PerformanceObserver", "takeRecords", |cx, w| {
        let id = this_obj(cx)?;
        let q = w
            .with_obj(id, |o| match o {
                Obj::Perf(p) => match &mut **p {
                    PerfObj::Observer(ob) => Some(std::mem::take(&mut ob.queue)),
                    _ => None,
                },
                _ => None,
            })
            .flatten()
            .ok_or_else(illegal)?;
        // Chrome 153 returns an Array of entries here (the callback's
        // argument is the EntryList). Entries queued by `mark` in this
        // turn are in it; the callback itself runs as a task.
        let mut entries = q;
        sort_entries(&mut entries);
        cx.ret(entries_arg(w, &entries))
    });
    reg.register(
        "PerformanceObserver",
        "supportedEntryTypes",
        ImplKind::StaticGetter,
        super::wf(rt, |cx, w| {
            let cached = w.perf.borrow().supported;
            if let Some(h) = cached {
                return cx.ret(Arg::Handle(h));
            }
            let types: &[&str] = if w.is_worker() {
                &WORKER_TYPES
            } else {
                &WINDOW_TYPES
            };
            let arr = frozen_array(
                cx,
                w,
                types.iter().map(|t| Arg::String((*t).to_owned())).collect(),
            )?;
            w.perf.borrow_mut().supported = Some(arr);
            cx.ret(Arg::Handle(arr))
        }),
    );

    // ----- PerformanceObserverEntryList -----
    fn list_of(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<Vec<Rc<Entry>>, HostError> {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| match o {
            Obj::Perf(p) => match &**p {
                PerfObj::EntryList(v) => Some(v.clone()),
                _ => None,
            },
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)
    }
    method(
        reg,
        rt,
        "PerformanceObserverEntryList",
        "getEntries",
        |cx, w| {
            let v = list_of(cx, w)?;
            cx.ret(entries_arg(w, &v))
        },
    );
    method(
        reg,
        rt,
        "PerformanceObserverEntryList",
        "getEntriesByType",
        |cx, w| {
            let v = list_of(cx, w)?;
            need_args(cx, 1, "getEntriesByType", "PerformanceObserverEntryList")?;
            let t = cx.arg_string(0)?;
            let v: Vec<_> = v.into_iter().filter(|e| e.kind.type_name() == t).collect();
            cx.ret(entries_arg(w, &v))
        },
    );
    method(
        reg,
        rt,
        "PerformanceObserverEntryList",
        "getEntriesByName",
        |cx, w| {
            let v = list_of(cx, w)?;
            need_args(cx, 1, "getEntriesByName", "PerformanceObserverEntryList")?;
            let name = cx.arg_string(0)?;
            let ty = if cx.len() > 1 && cx.arg_kind(1) != ValueKind::Undefined {
                Some(cx.arg_string(1)?)
            } else {
                None
            };
            let v: Vec<_> = v
                .into_iter()
                .filter(|e| e.name == name && ty.as_ref().is_none_or(|t| e.kind.type_name() == t))
                .collect();
            cx.ret(entries_arg(w, &v))
        },
    );

    // ----- EventCounts (a read-only map, all zero: no Event Timing) -----
    fn ec_this(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(), HostError> {
        let id = this_obj(cx)?;
        w.with_obj(
            id,
            |o| matches!(o, Obj::Perf(p) if matches!(**p, PerfObj::EventCounts)),
        )
        .filter(|b| *b)
        .map(|_| ())
        .ok_or_else(illegal)
    }
    getter(reg, rt, "EventCounts", "size", |cx, w| {
        ec_this(cx, w)?;
        cx.ret(EVENT_COUNT_TYPES.len() as u32)
    });
    method(reg, rt, "EventCounts", "get", |cx, w| {
        ec_this(cx, w)?;
        need_args(cx, 1, "get", "EventCounts")?;
        let k = cx.arg_string(0)?;
        if EVENT_COUNT_TYPES.contains(&k.as_str()) {
            cx.ret(0)
        } else {
            cx.ret(Arg::Undefined)
        }
    });
    method(reg, rt, "EventCounts", "has", |cx, w| {
        ec_this(cx, w)?;
        need_args(cx, 1, "has", "EventCounts")?;
        let k = cx.arg_string(0)?;
        cx.ret(EVENT_COUNT_TYPES.contains(&k.as_str()))
    });
    for (name, kind) in [("keys", 0), ("values", 1), ("entries", 2)] {
        method(reg, rt, "EventCounts", name, move |cx, w| {
            ec_this(cx, w)?;
            let items: Vec<Arg> = EVENT_COUNT_TYPES
                .iter()
                .map(|k| match kind {
                    0 => Arg::String((*k).to_owned()),
                    1 => Arg::Number(0.0),
                    _ => Arg::Array(vec![Arg::String((*k).to_owned()), Arg::Number(0.0)]),
                })
                .collect();
            let arr = cx.make_handle(Arg::Array(items))?;
            let values = w.helpers.borrow().array_values;
            let r = match values {
                Some(f) => {
                    let it = cx
                        .try_call_handle(f, Arg::Handle(arr), &[])
                        .map_err(|e| HostError::Error(e.message))?;
                    let r = cx.ret(Arg::Handle(it));
                    cx.release(it);
                    r
                }
                None => cx.ret(Arg::Handle(arr)),
            };
            cx.release(arr);
            r
        });
    }
    method(reg, rt, "EventCounts", "forEach", |cx, w| {
        ec_this(cx, w)?;
        need_args(cx, 1, "forEach", "EventCounts")?;
        if cx.arg_kind(0) != ValueKind::Function {
            return Err(HostError::type_error(
                "Failed to execute 'forEach' on 'EventCounts': The callback provided as parameter 1 is not a function.",
            ));
        }
        let f = cx.arg_handle(0);
        let this_arg = if cx.len() > 1 {
            Arg::Handle(cx.arg_handle(1))
        } else {
            Arg::Undefined
        };
        let me = cx.this_handle();
        for k in EVENT_COUNT_TYPES {
            cx.call(
                f,
                this_arg.clone(),
                &[Arg::Number(0.0), Arg::String(k.to_owned()), Arg::Handle(me)],
            )
            .map_err(|_| HostError::Thrown)?;
        }
        let _ = w;
        Ok(())
    });
}

/// `PerformanceObserver.observe(options)`.
fn observe(cx: &mut CallCtx<'_, '_, '_>, w: &Rc<Window>) -> Result<(), HostError> {
    let id = this_obj(cx)?;
    let pre = "Failed to execute 'observe' on 'PerformanceObserver': ";
    match cx.arg_kind(0) {
        ValueKind::Undefined | ValueKind::Null | ValueKind::Object | ValueKind::Function => {}
        _ => {
            return Err(HostError::type_error(format!(
                "{pre}The provided value is not of type 'PerformanceObserverInit'."
            )));
        }
    }
    let d = Dict::arg(cx, 0);
    let entry_types = d.get(cx, "entryTypes")?;
    let ty = d.get(cx, "type")?;
    let buffered = d.bool(cx, "buffered")?;
    d.release(cx);
    let supported: &[&str] = if w.is_worker() {
        &WORKER_TYPES
    } else {
        &WINDOW_TYPES
    };
    let err = |m: &str| HostError::type_error(format!("{pre}{m}"));
    if entry_types.is_none() && ty.is_none() {
        return Err(err(
            "An observe() call must include either entryTypes or type arguments.",
        ));
    }
    let mode = w
        .with_obj(id, |o| match o {
            Obj::Perf(p) => match &**p {
                PerfObj::Observer(ob) => Some(ob.mode),
                _ => None,
            },
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)?;
    if let (Some(a), Some(b)) = (entry_types, ty) {
        cx.release(a);
        cx.release(b);
        return Err(err(
            "An observe() call must not include both entryTypes and type arguments.",
        ));
    }
    let (kinds, new_mode, buffered) = if let Some(h) = entry_types {
        if mode == ObserveMode::Type {
            cx.release(h);
            return Err(w.rt.err(
                "InvalidModificationError",
                format!(
                    "{pre}This PerformanceObserver has performed observe({{type:...}}, therefore it cannot perform observe({{entryTypes:...}})"
                ),
            ));
        }
        let names: Vec<String> = match cx.handle_value(h) {
            JsValue::Json(s) => serde_json::from_str::<Vec<serde_json::Value>>(&s)
                .map(|v| {
                    v.iter()
                        .map(|x| match x {
                            serde_json::Value::String(s) => s.clone(),
                            o => o.to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        cx.release(h);
        let mut kinds: Vec<Kind> = Vec::new();
        for n in names {
            if !supported.contains(&n.as_str()) {
                w.warn(format!(
                    "The entry type '{n}' does not exist or isn't supported."
                ));
            } else if let Some(k) = Kind::of_type(&n)
                && !kinds.contains(&k)
            {
                kinds.push(k);
            }
        }
        (kinds, ObserveMode::EntryTypes, false)
    } else {
        let h = ty.unwrap();
        let name = cx.handle_string(h);
        cx.release(h);
        let name = name?;
        if mode == ObserveMode::EntryTypes {
            return Err(w.rt.err(
                "InvalidModificationError",
                format!(
                    "{pre}This observer has performed observe({{entryTypes:...}}, therefore it cannot perform observe({{type:...}})"
                ),
            ));
        }
        let kinds: Vec<Kind> = if supported.contains(&name.as_str()) {
            Kind::of_type(&name).into_iter().collect()
        } else {
            w.warn(format!(
                "The entry type '{name}' does not exist or isn't supported."
            ));
            Vec::new()
        };
        (kinds, ObserveMode::Type, buffered)
    };
    // Apply.
    let mut queue_task = false;
    let mut replay: Vec<Rc<Entry>> = Vec::new();
    if new_mode == ObserveMode::Type && buffered {
        for k in &kinds {
            replay.extend(entries_of(w, *k));
        }
    }
    let active = w
        .with_obj(id, |o| match o {
            Obj::Perf(p) => match &mut **p {
                PerfObj::Observer(ob) => {
                    ob.mode = new_mode;
                    ob.dropped_pending = true;
                    if new_mode == ObserveMode::EntryTypes {
                        ob.types = kinds.clone();
                    } else {
                        for k in &kinds {
                            if !ob.types.contains(k) {
                                ob.types.push(*k);
                            }
                        }
                    }
                    if !replay.is_empty() {
                        ob.queue.extend(replay.iter().cloned());
                        queue_task = !std::mem::replace(&mut ob.task_queued, true);
                    }
                    !ob.types.is_empty()
                }
                _ => false,
            },
            _ => false,
        })
        .unwrap_or(false);
    {
        let mut st = w.perf.borrow_mut();
        if active {
            if !st.observers.contains(&id) {
                st.observers.push(id);
            }
        } else {
            st.observers.retain(|&i| i != id);
        }
    }
    if queue_task {
        queue_delivery(w, id);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Workers: importScripts entries
// ---------------------------------------------------------------------------

impl Window {
    /// A script `importScripts` fetched (the worker's own timeline); a
    /// service worker's comes from the script cache (no transfer).
    pub(crate) fn perf_import(
        self: &Rc<Self>,
        req: &NetRequest,
        started: Instant,
        r: Result<&NetResponse, &String>,
    ) {
        let Some(mut p) = Pending::of(req, false) else {
            return;
        };
        p.initiator = "other";
        p.started = started;
        if self.sw.scope_set() {
            p = p.cached_script();
        }
        finish(self, p, r);
    }

    /// The script of a dedicated worker this window created was fetched
    /// (an `other` entry on the creator's timeline).
    pub fn perf_worker_script(self: &Rc<Self>, url: Url, started: Instant, resp: &NetResponse) {
        let req = NetRequest::get(url, crate::host::Destination::Script, None);
        let Some(mut p) = Pending::of(&req, false) else {
            return;
        };
        p.initiator = "other";
        p.started = started;
        p.mode = "same-origin";
        finish(self, p, Ok(resp));
    }

    /// An iframe's document finished loading (an `iframe` entry on the
    /// parent's timeline).
    pub fn perf_iframe(self: &Rc<Self>, url: Url, started: Instant, resp: &NetResponse) {
        let mut req = NetRequest::get(url, crate::host::Destination::Iframe, None);
        req.mode = "navigate";
        let Some(mut p) = Pending::of(&req, false) else {
            return;
        };
        p.started = started;
        finish(self, p, Ok(resp));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header `serve.py` sends with `st=1` and the entries Chrome 153
    /// made of it (`res_sizes` in `crates/lb-page/tests/perf/chrome.json`).
    #[test]
    fn server_timing_as_chrome_reads_it() {
        let h = vec![(
            "Server-Timing".to_owned(),
            "db;dur=53.2;desc=\"Database\", cache;desc=hit".to_owned(),
        )];
        let m = parse_server_timing(&h);
        assert_eq!(m.len(), 2);
        assert_eq!((m[0].name.as_str(), m[0].duration), ("db", 53.2));
        assert_eq!(m[0].description, "Database");
        assert_eq!((m[1].name.as_str(), m[1].duration), ("cache", 0.0));
        assert_eq!(m[1].description, "hit");
        assert_eq!(
            server_json(&m),
            r#"[{"name":"db","duration":53.2,"description":"Database"},{"name":"cache","duration":0.0,"description":"hit"}]"#
        );
    }

    /// `Timing-Allow-Origin: *` admits any origin, a list admits its
    /// members (the cases of `res_redirect`).
    #[test]
    fn timing_allow_origin_values() {
        let o = "http://127.0.0.1:19281";
        assert!(tao_allows(&["*".into()], o));
        assert!(tao_allows(
            &["http://a.test, http://127.0.0.1:19281".into()],
            o
        ));
        assert!(!tao_allows(&["http://a.test".into()], o));
        assert!(!tao_allows(&[], o));
    }
}
