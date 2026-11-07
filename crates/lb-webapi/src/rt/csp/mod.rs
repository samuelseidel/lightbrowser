//! Content Security Policy enforcement (ADR 0036): the CSP list of each
//! window and worker scope, the checks the load paths call, and what a
//! violation does (Chrome's console message, a `securitypolicyviolation`
//! event in a networking task, a `report-uri` POST). The policy model
//! (parsing, matching, texts) is [`policy`].
//!
//! Load paths call one of [`Window::csp_url`], [`Window::csp_inline`],
//! [`Window::csp_navigation`] (and the engine calls the `eval` and
//! WebAssembly gates set by [`Window::csp_apply_codegen`]). Each returns
//! whether the load may go on and reports every policy's violation. A
//! window with no policies, or one CDP `Page.setBypassCSP` covers, allows
//! everything without work.

pub mod policy;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use lb_dom::{Document, Mutation, NodeId, local_name};
use lb_js::{CallCtx, CodeGenKind, CodeGenPolicy, HostError, HostRef, JsValue};
use url::Url;

use policy::{Blocked, Outcome};
pub use policy::{CspList, Delivery, Dir, Disposition, Inline, UrlCheck};

use super::events::EventExtra;
use super::{Dict, Js, K_OBJ, Obj, Runtime, TaskSource, WINDOW_REF, Window, ctor, getter, illegal};
use crate::host::{ConsoleMessage, Destination, NetRequest};
use crate::registry::Registry;

/// The fields of a `SecurityPolicyViolationEvent`.
#[derive(Clone, Debug, Default)]
pub struct ViolationInit {
    pub document_uri: String,
    pub referrer: String,
    pub blocked_uri: String,
    pub violated_directive: String,
    pub effective_directive: String,
    pub original_policy: String,
    pub disposition: String,
    pub source_file: String,
    pub sample: String,
    pub status_code: u16,
    pub line_number: u32,
    pub column_number: u32,
}

/// Where a violation happened: script URL, 1-based line and column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLoc {
    pub url: String,
    pub line: u32,
    pub column: u32,
}

/// The CSP state of a window or worker scope.
#[derive(Default)]
pub struct CspSide {
    list: RefCell<CspList>,
    /// CDP `Page.setBypassCSP`.
    bypass: Cell<bool>,
    /// The document's HTTP status (`statusCode`; 0 for `about:blank`).
    status: Cell<u16>,
    /// Reports sent (each distinct report goes out once).
    reports: RefCell<HashSet<String>>,
    /// The `nonce` IDL attribute's internal slot, by element.
    nonces: RefCell<HashMap<NodeId, String>>,
    /// The last allowed inline style of elements whose `style` attribute
    /// CSP checked.
    style_kept: RefCell<HashMap<NodeId, Option<String>>>,
    /// Elements whose current `style` attribute CSP blocked.
    style_blocked_attr: RefCell<HashSet<NodeId>>,
    /// `style` values the CSSOM wrote (allowed without a check).
    cssom: RefCell<HashMap<NodeId, String>>,
    /// Where content-attribute event handlers were set (parser line).
    handler_lines: RefCell<HashMap<(HostRef, String), u32>>,
    /// The `<base href>` CSP last blocked (not used as base URL).
    blocked_base: RefCell<Option<String>>,
    /// The number of lines of the document's source.
    last_line: Cell<u64>,
    /// Scripts `document.write` made, with where it was called.
    written: RefCell<HashMap<NodeId, SourceLoc>>,
    /// `<style>` elements CSP blocked, with the text it blocked.
    blocked_styles: RefCell<HashMap<NodeId, String>>,
}

impl Window {
    fn csp_on(&self) -> bool {
        !self.csp.bypass.get() && !self.csp.list.borrow().is_empty()
    }

    /// Take the policies of a document's (or worker's) response:
    /// `Content-Security-Policy` and `-Report-Only` headers, every one of
    /// them, comma-separated policies each. Parse errors go to the
    /// console.
    pub fn csp_deliver(&self, headers: &[(String, String)], status: u16) {
        self.csp.status.set(status);
        let url = self.url();
        let mut errors = Vec::new();
        let mut list = self.csp.list.borrow_mut();
        for (disp, name) in [
            (Disposition::Enforce, "content-security-policy"),
            (Disposition::Report, "content-security-policy-report-only"),
        ] {
            for (k, v) in headers {
                if k.eq_ignore_ascii_case(name) {
                    list.policies.extend(policy::parse(
                        v,
                        disp,
                        Delivery::Header,
                        &url,
                        &mut errors,
                    ));
                }
            }
        }
        drop(list);
        for e in errors {
            self.csp_console("error", e);
        }
    }

    /// The CSP list (to inherit: `about:blank` and `srcdoc` frames,
    /// `blob:` and `data:` documents, `blob:` workers).
    pub fn csp_policies(&self) -> CspList {
        self.csp.list.borrow().clone()
    }

    /// Replace the CSP list (inherited), with `status` as the document's
    /// status code.
    pub fn set_csp_policies(&self, list: CspList, status: u16) {
        *self.csp.list.borrow_mut() = list;
        self.csp.status.set(status);
    }

    /// Inherit `from`'s policies (an `about:blank` or `srcdoc` document).
    pub fn csp_inherit(&self, from: &Window) {
        self.set_csp_policies(from.csp_policies(), 0);
        self.csp.bypass.set(from.csp.bypass.get());
    }

    /// CDP `Page.setBypassCSP`: nothing is checked.
    pub fn set_csp_bypass(&self, on: bool) {
        self.csp.bypass.set(on);
    }

    pub fn csp_bypass(&self) -> bool {
        self.csp.bypass.get()
    }

    /// The combined `sandbox` of the enforced policies (`None`: none).
    pub fn csp_sandbox(&self) -> Option<policy::Sandbox> {
        if self.csp.bypass.get() {
            return None;
        }
        self.csp.list.borrow().sandbox()
    }

    /// A CSP `sandbox` without `allow-same-origin` applies: the document's
    /// origin is opaque.
    pub fn csp_opaque_origin(&self) -> bool {
        self.csp_sandbox()
            .is_some_and(|s| !s.allows("allow-same-origin"))
    }

    /// Storage and cookies of an opaque-origin sandboxed document throw
    /// Chrome's `SecurityError` (`verb` is `read` or `set`).
    pub(crate) fn csp_sandbox_check(
        &self,
        verb: &str,
        prop: &str,
        iface: &str,
    ) -> Result<(), HostError> {
        if !self.csp_opaque_origin() {
            return Ok(());
        }
        let prep = if verb == "set" { "on" } else { "from" };
        Err(self.rt.err(
            "SecurityError",
            format!("Failed to {verb} the '{prop}' property {prep} '{iface}': The document is sandboxed and lacks the 'allow-same-origin' flag."),
        ))
    }

    /// `upgrade-insecure-requests`: the URL to request instead.
    pub fn csp_upgrade(&self, url: &Url) -> Option<Url> {
        if !self.csp_on() || !self.csp.list.borrow().upgrade_insecure() {
            return None;
        }
        policy::upgrade(url)
    }

    /// Set the `eval` / WebAssembly gates of the window's realm from its
    /// policies (after they changed).
    pub fn csp_apply_codegen(self: &Rc<Self>, ops: &mut dyn lb_js::RealmOps) {
        let realm = self.realm();
        let (eval_error, wasm_error) = if self.csp_on() {
            let l = self.csp.list.borrow();
            (l.eval_disabled_message(), l.wasm_disabled_message())
        } else {
            (None, None)
        };
        let policy = if eval_error.is_none() && wasm_error.is_none() {
            None
        } else {
            let weak: Weak<Window> = Rc::downgrade(self);
            Some(CodeGenPolicy {
                eval_error,
                wasm_error,
                check: Rc::new(move |r| {
                    let Some(w) = weak.upgrade() else { return true };
                    let loc = r
                        .location
                        .map(|(url, line, column)| SourceLoc { url, line, column });
                    match r.kind {
                        CodeGenKind::Eval => w.csp_eval(r.source, true, loc),
                        CodeGenKind::Wasm => w.csp_wasm(loc),
                    }
                }),
            })
        };
        let _ = ops.set_code_generation(realm, policy);
    }

    /// Where a check happens: the script on top of the stack, else the
    /// parser's position in the document (Blink's
    /// `CaptureSourceLocation`).
    pub fn csp_here(&self, js: Option<&mut dyn Js>) -> Option<SourceLoc> {
        if let Some(js) = js
            && let Some((url, line, column)) = js.script_location()
            && line > 0
        {
            return Some(SourceLoc { url, line, column });
        }
        self.csp_parser_loc()
    }

    /// The document URL and the parser's current line, while parsing.
    pub fn csp_parser_loc(&self) -> Option<SourceLoc> {
        let line = self
            .parser
            .try_borrow()
            .ok()?
            .as_ref()
            .map(|p| p.current_line())?;
        (line > 0).then(|| SourceLoc {
            url: self.url().to_string(),
            line: line as u32,
            column: 0,
        })
    }

    /// The document URL and the line where the parser created `n` (a
    /// `<base>`, an element with a `style` attribute, a `<script>`).
    pub fn csp_element_loc(&self, n: NodeId) -> Option<SourceLoc> {
        let p = self.parser.try_borrow().ok()?;
        let p = p.as_ref()?;
        let line = p.element_line(n).or_else(|| p.script_line(n))?;
        Some(SourceLoc {
            url: self.url().to_string(),
            line: line as u32,
            column: 0,
        })
    }

    /// Where an image load of `n` happens, as Blink's `SourceLocation`
    /// has it (no script runs then): nothing for images made by script,
    /// the parser's line for parser-made ones.
    pub fn csp_image_loc(&self, n: NodeId) -> Option<SourceLoc> {
        let parser_made =
            self.with_doc(|d| d.flags(n).contains(lb_dom::NodeFlags::PARSER_INSERTED));
        if !parser_made {
            return None;
        }
        self.csp_parser_loc()
    }

    /// The document's source has `lines` lines (set before parsing).
    pub fn csp_set_doc_lines(&self, lines: usize) {
        self.csp.last_line.set(lines as u64);
    }

    /// Where a background image load happens: Chrome's first style recalc
    /// that asks for them runs when parsing ends, at the document's last
    /// line (until the document is complete).
    pub fn csp_style_loc(&self) -> Option<SourceLoc> {
        let line = self.csp.last_line.get();
        (line > 0 && self.ready_state() != super::ReadyState::Complete).then(|| SourceLoc {
            url: self.url().to_string(),
            line: line as u32,
            column: 0,
        })
    }

    /// `document.write` at `loc` made the nodes from id `first` on: its
    /// scripts report violations there.
    pub(crate) fn csp_note_written(&self, first: usize, loc: SourceLoc) {
        if !self.csp_on() {
            return;
        }
        let scripts: Vec<NodeId> = self.with_doc(|d| {
            (first..d.node_count())
                .map(|i| NodeId(i as u32))
                .filter(|&n| d.is_html_element_named(n, &local_name!("script")))
                .collect()
        });
        let mut m = self.csp.written.borrow_mut();
        for n in scripts {
            m.insert(n, loc.clone());
        }
    }

    /// Where a script element `document.write` made was written.
    pub fn csp_written_loc(&self, n: NodeId) -> Option<SourceLoc> {
        self.csp.written.borrow_mut().remove(&n)
    }

    /// Is a fetch of `url` for `dir` allowed? Reports violations.
    pub fn csp_url(
        self: &Rc<Self>,
        dir: Dir,
        url: &Url,
        check: &UrlCheck<'_>,
        loc: Option<SourceLoc>,
    ) -> bool {
        if !self.csp_on() {
            return true;
        }
        let out = self.csp.list.borrow().check_url(dir, url, check);
        self.csp_report(out, None, loc)
    }

    /// Inline content: a script, an event handler attribute, a
    /// `javascript:` URL, a style element or attribute.
    #[allow(clippy::too_many_arguments)]
    pub fn csp_inline(
        self: &Rc<Self>,
        kind: Inline,
        element: Option<NodeId>,
        content: &str,
        nonce: &str,
        parser_inserted: bool,
        loc: Option<SourceLoc>,
    ) -> bool {
        if !self.csp_on() {
            return true;
        }
        let out = self
            .csp
            .list
            .borrow()
            .check_inline(kind, content, nonce, parser_inserted);
        self.csp_report(out, element, loc)
    }

    /// `eval` and friends; `will_throw`: the caller throws `EvalError`.
    pub fn csp_eval(
        self: &Rc<Self>,
        content: &str,
        will_throw: bool,
        loc: Option<SourceLoc>,
    ) -> bool {
        if !self.csp_on() {
            return true;
        }
        let out = self.csp.list.borrow().check_eval(content, will_throw);
        self.csp_report(out, None, loc)
    }

    /// A string passed to `setTimeout` / `setInterval` (Blink checks it
    /// when the timer is made; a blocked one returns 0).
    pub(crate) fn csp_string_timer(
        self: &Rc<Self>,
        cx: &mut CallCtx<'_, '_, '_>,
        code: &str,
    ) -> bool {
        if !self.csp_on() {
            return true;
        }
        let loc = self.csp_here(Some(cx));
        self.csp_eval(code, false, loc)
    }

    fn csp_wasm(self: &Rc<Self>, loc: Option<SourceLoc>) -> bool {
        if !self.csp_on() {
            return true;
        }
        let out = self.csp.list.borrow().check_wasm();
        self.csp_report(out, None, loc)
    }

    /// A navigation the browser process checks: an iframe's (`frame-src`)
    /// or a form submission (`form-action`).
    pub fn csp_navigation(self: &Rc<Self>, dir: Dir, url: &Url, loc: Option<SourceLoc>) -> bool {
        if !self.csp_on() {
            return true;
        }
        let me = self.url();
        // Chrome's message names only the origin of a cross-origin frame.
        let shown = if dir == Dir::FrameSrc && url.origin() != me.origin() {
            format!("{}/", url.origin().ascii_serialization())
        } else {
            url.to_string()
        };
        let out = self
            .csp
            .list
            .borrow()
            .check_navigation(dir, url, &shown, false);
        self.csp_report(out, None, loc)
    }

    /// Log a CSP message (a dedicated worker's also reaches its creator's
    /// console, as in Chrome).
    pub(crate) fn csp_console(&self, level: &str, text: String) {
        let msg = ConsoleMessage {
            level: level.into(),
            text,
            url: None,
            line: None,
            from_worker: false,
        };
        if self.is_worker() {
            super::worker::forward_console(self, msg.clone());
        }
        self.log(msg);
    }

    /// Log, fire and send every violation of `out`; returns
    /// `out.allowed`.
    fn csp_report(
        self: &Rc<Self>,
        out: Outcome,
        element: Option<NodeId>,
        loc: Option<SourceLoc>,
    ) -> bool {
        let doc_url = self.url();
        let origin = doc_url.origin();
        for v in out.violations {
            if let Some(text) = v.console {
                let level = if v.disposition == Disposition::Report {
                    "info"
                } else {
                    "error"
                };
                self.csp_console(level, text);
            }
            if let Some(note) = v.note {
                self.csp_console("error", note);
            }
            let blocked_uri = match &v.blocked {
                Blocked::Url(u) => policy::strip_for_report(&origin, u, v.directive),
                Blocked::Inline => "inline".into(),
                Blocked::Eval => "eval".into(),
                Blocked::WasmEval => "wasm-eval".into(),
            };
            let mut init = ViolationInit {
                document_uri: policy::strip_for_report(&origin, &doc_url, Dir::DefaultSrc),
                referrer: self.referrer.clone(),
                blocked_uri,
                violated_directive: v.directive.name().into(),
                effective_directive: v.directive.name().into(),
                original_policy: v.header.clone(),
                disposition: v.disposition.as_str().into(),
                sample: v.sample.clone(),
                status_code: self.csp.status.get(),
                ..Default::default()
            };
            if let Some(l) = loc.as_ref().filter(|l| l.line > 0) {
                init.source_file = match Url::parse(&l.url) {
                    Ok(mut u) => {
                        u.set_query(None);
                        policy::strip_for_report(&origin, &u, v.directive)
                    }
                    Err(_) => l.url.clone(),
                };
                init.line_number = l.line;
                init.column_number = l.column;
            }
            if !v.endpoints.is_empty() && !v.use_reporting_api {
                self.csp_send_report(&init, &v.endpoints);
            }
            let target_el = if v.at_element { element } else { None };
            let w = self.clone();
            self.queue_task(
                TaskSource::Networking,
                Box::new(move |js, _| dispatch_violation(js, &w, target_el, init)),
            );
        }
        out.allowed
    }

    /// POST a `report-uri` report (`application/csp-report`), once per
    /// distinct report.
    fn csp_send_report(&self, v: &ViolationInit, endpoints: &[String]) {
        let s = |x: &str| serde_json::to_string(x).unwrap_or_default();
        let mut body = format!(
            "{{\"csp-report\":{{\"document-uri\":{},\"referrer\":{},\"violated-directive\":{},\"effective-directive\":{},\"original-policy\":{},\"disposition\":{},\"blocked-uri\":{}",
            s(&v.document_uri),
            s(&v.referrer),
            s(&v.violated_directive),
            s(&v.effective_directive),
            s(&v.original_policy),
            s(&v.disposition),
            s(&v.blocked_uri)
        );
        if v.line_number != 0 {
            body.push_str(&format!(",\"line-number\":{}", v.line_number));
        }
        if v.column_number != 0 {
            body.push_str(&format!(",\"column-number\":{}", v.column_number));
        }
        if !v.source_file.is_empty() {
            body.push_str(&format!(",\"source-file\":{}", s(&v.source_file)));
        }
        body.push_str(&format!(
            ",\"status-code\":{},\"script-sample\":{}}}}}",
            v.status_code,
            s(&v.sample)
        ));
        if !self.csp.reports.borrow_mut().insert(body.clone()) {
            return;
        }
        for e in endpoints {
            let Ok(url) = Url::parse(e) else { continue };
            let req = NetRequest {
                method: "POST".into(),
                url,
                headers: vec![("Content-Type".into(), "application/csp-report".into())],
                body: body.clone().into_bytes(),
                destination: Destination::Empty,
                initiator: Some(self.url()),
                mode: "no-cors",
                user_activated: false,
                storage_key: Some(self.storage_key()),
                redirect_check: None,
            };
            let fut = self.net.fetch(req);
            self.net.spawn(Box::pin(async move {
                let _ = fut.await;
            }));
        }
    }

    /// The element's nonce (the `nonce` IDL attribute's slot, else the
    /// content attribute).
    pub fn csp_nonce(&self, n: NodeId) -> String {
        if let Some(v) = self.csp.nonces.borrow().get(&n) {
            return v.clone();
        }
        self.with_doc(|d| d.get_attribute(n, "nonce").unwrap_or("").to_owned())
    }

    /// The URL to use as the document's base URL for `href` of the first
    /// `<base>`: `None` when `base-uri` blocked it.
    pub(crate) fn csp_base_allowed(&self, href: &str) -> bool {
        self.csp.blocked_base.borrow().as_deref() != Some(href)
    }

    /// Note where a content-attribute event handler was set (its fallback
    /// location when it compiles with no script running).
    pub(crate) fn csp_note_handler(&self, t: HostRef, name: &str) {
        if !self.csp_on() {
            return;
        }
        if let Some(l) = self.csp_parser_loc() {
            self.csp
                .handler_lines
                .borrow_mut()
                .insert((t, name.to_owned()), l.line);
        }
    }

    /// May the content-attribute handler `name` of `t` compile?
    pub(crate) fn csp_handler(
        self: &Rc<Self>,
        js: &mut dyn Js,
        t: HostRef,
        name: &str,
        src: &str,
    ) -> bool {
        if !self.csp_on() {
            return true;
        }
        let el = self.decode_node(t).filter(|(s, _)| *s == 0).map(|(_, n)| n);
        let loc = match js.script_location() {
            Some((url, line, column)) if line > 0 => Some(SourceLoc { url, line, column }),
            _ => self
                .csp
                .handler_lines
                .borrow()
                .get(&(t, name.to_owned()))
                .map(|&line| SourceLoc {
                    url: self.url().to_string(),
                    line,
                    column: 0,
                }),
        };
        self.csp_inline(Inline::ScriptAttribute, el, src, "", false, loc)
    }

    /// The CSSOM wrote `text` into `n`'s `style` attribute.
    pub(crate) fn csp_cssom_wrote(&self, n: NodeId, text: &str) {
        if self.csp_on() {
            self.csp.cssom.borrow_mut().insert(n, text.to_owned());
        }
    }
}

/// One batch of DOM mutations (see [`on_mutation`]); an inserted
/// subtree is looked at once even when its descendants have their own
/// insertion records in the batch.
pub(crate) fn on_mutations(js: &mut dyn Js, w: &Rc<Window>, batch: &[(u32, Mutation)]) {
    if w.csp.bypass.get() {
        return;
    }
    let mut walked: HashSet<NodeId> = HashSet::new();
    let mut styled: HashSet<NodeId> = HashSet::new();
    for (slot, m) in batch {
        if *slot == 0 {
            on_mutation(js, w, m, &mut walked, &mut styled);
        }
    }
}

/// Mutations CSP looks at (main document): `<base>` changes (`base-uri`),
/// `<meta http-equiv>` insertions, `style` attributes (`style-src-attr`).
fn on_mutation(
    js: &mut dyn Js,
    w: &Rc<Window>,
    m: &Mutation,
    walked: &mut HashSet<NodeId>,
    styled_done: &mut HashSet<NodeId>,
) {
    let on = w.csp_on();
    match m {
        Mutation::ChildInserted { node, .. } => {
            if walked.contains(node) {
                return;
            }
            let (metas, styled, base) = w.with_doc(|d| {
                let mut metas = Vec::new();
                let mut styled = Vec::new();
                let mut base = false;
                if !d.is_connected(*node) {
                    return (metas, styled, base);
                }
                for n in d.inclusive_descendants(*node) {
                    walked.insert(n);
                    let Some(e) = d.element(n) else { continue };
                    if !e.is_html() {
                        continue;
                    }
                    if e.name.local == local_name!("meta") {
                        metas.push(n);
                    } else if on && e.name.local == local_name!("base") {
                        base = true;
                    }
                    if on && d.get_attribute(n, "style").is_some() {
                        styled.push(n);
                    }
                }
                (metas, styled, base)
            });
            hide_nonces(w, *node);
            for n in metas {
                meta_inserted(js, w, n);
            }
            if base {
                process_base(js, w);
            }
            for n in styled {
                if styled_done.insert(n) {
                    style_attribute(js, w, n, true);
                }
            }
        }
        Mutation::ChildRemoved { node, .. } => {
            let base = w.with_doc(|d| d.is_html_element_named(*node, &local_name!("base")));
            if base {
                process_base(js, w);
            }
        }
        Mutation::Attribute { element, name, .. } => {
            if !name.ns.is_empty() {
                return;
            }
            let (is_base, is_meta) = w.with_doc(|d| {
                (
                    d.is_html_element_named(*element, &local_name!("base")),
                    d.is_html_element_named(*element, &local_name!("meta")),
                )
            });
            if &*name.local == "href" && is_base {
                process_base(js, w);
            } else if &*name.local == "style" {
                // The element's current value, once per batch.
                if styled_done.insert(*element) {
                    style_attribute(js, w, *element, false);
                }
            } else if &*name.local == "nonce" {
                // The content attribute was set: it is the nonce now.
                let v = w.with_doc(|d| d.get_attribute(*element, "nonce").map(str::to_owned));
                match v {
                    Some(v) if !v.is_empty() => {
                        w.csp.nonces.borrow_mut().insert(*element, v);
                    }
                    _ => {}
                }
            }
            let _ = is_meta;
        }
        Mutation::CharacterData { .. } => {}
    }
}

/// Nonce hiding: once an element with a nonce is connected to a
/// document whose CSP came in a header, its `nonce` content attribute
/// reads empty and the nonce lives in the IDL slot (Chrome).
pub(crate) fn hide_nonces(w: &Rc<Window>, node: NodeId) {
    if !w.csp_on() || !w.csp.list.borrow().header_delivered() {
        return;
    }
    let found: Vec<(NodeId, String)> = w.with_doc(|d| {
        if !d.is_connected(node) {
            return Vec::new();
        }
        d.inclusive_descendants(node)
            .filter_map(|n| {
                let v = d.get_attribute(n, "nonce")?;
                (!v.is_empty() && d.element(n).is_some_and(|e| e.is_html()))
                    .then(|| (n, v.to_owned()))
            })
            .collect()
    });
    for (n, v) in found {
        w.csp.nonces.borrow_mut().insert(n, v);
        // Chrome sets the attribute to "" while inserting.
        let _ = w.with_doc_mut(|d| d.set_attribute(n, "nonce", ""));
    }
}

/// A `<meta http-equiv=Content-Security-Policy>` was inserted.
fn meta_inserted(js: &mut dyn Js, w: &Rc<Window>, n: NodeId) {
    let (equiv, content, in_head) = w.with_doc(|d| {
        let equiv = d
            .get_attribute(n, "http-equiv")
            .map(|v| v.trim().to_ascii_lowercase());
        let content = d.get_attribute(n, "content").map(str::to_owned);
        let in_head = d
            .parent(n)
            .is_some_and(|p| d.is_html_element_named(p, &local_name!("head")));
        (equiv, content, in_head)
    });
    let (Some(equiv), Some(content)) = (equiv, content) else {
        return;
    };
    let report_only = match equiv.as_str() {
        "content-security-policy" => false,
        "content-security-policy-report-only" => true,
        _ => return,
    };
    if report_only {
        w.csp_console("error", format!("The report-only Content Security Policy '{content}' was delivered via a <meta> element, which is disallowed. The policy has been ignored."));
        return;
    }
    if !in_head {
        w.csp_console("error", format!("The Content Security Policy '{content}' was delivered via a <meta> element outside the document's <head>, which is disallowed. The policy has been ignored."));
        return;
    }
    let url = w.url();
    let mut errors = Vec::new();
    let ps = policy::parse(
        &content,
        Disposition::Enforce,
        Delivery::Meta,
        &url,
        &mut errors,
    );
    for e in errors {
        w.csp_console("error", e);
    }
    if ps.is_empty() {
        return;
    }
    w.csp.list.borrow_mut().policies.extend(ps);
    w.csp_apply_codegen(js.realm_ops());
}

/// Blink's `Document::ProcessBaseElement`: the first `<base href>` sets
/// the base URL unless `base-uri` blocks it (reported each time).
fn process_base(js: &mut dyn Js, w: &Rc<Window>) {
    if !w.csp_on() {
        return;
    }
    let href = w.with_doc(|d| {
        d.descendants(Document::DOCUMENT)
            .find(|&n| {
                d.is_html_element_named(n, &local_name!("base"))
                    && d.get_attribute(n, "href").is_some()
            })
            .and_then(|n| d.get_attribute(n, "href").map(str::to_owned))
    });
    let Some(href) = href else {
        *w.csp.blocked_base.borrow_mut() = None;
        return;
    };
    let Ok(url) = w.url().join(&href) else { return };
    let el = w.with_doc(|d| {
        d.descendants(Document::DOCUMENT).find(|&n| {
            d.is_html_element_named(n, &local_name!("base")) && d.get_attribute(n, "href").is_some()
        })
    });
    let loc = match js.script_location() {
        Some((url, line, column)) if line > 0 => Some(SourceLoc { url, line, column }),
        _ => el
            .and_then(|n| w.csp_element_loc(n))
            .or_else(|| w.csp_parser_loc()),
    };
    let ok = w.csp_url(Dir::BaseUri, &url, &UrlCheck::default(), loc);
    *w.csp.blocked_base.borrow_mut() = (!ok).then_some(href);
}

/// A `style` attribute was set (or came with an inserted element):
/// `style-src-attr`. A blocked one leaves the element its last allowed
/// inline style (none if it never had one).
fn style_attribute(js: &mut dyn Js, w: &Rc<Window>, n: NodeId, inserted: bool) {
    if !w.csp_on() {
        return;
    }
    let value = w.with_doc(|d| d.get_attribute(n, "style").map(str::to_owned));
    // Written through the CSSOM: not checked, and the last allowed style
    // even when a later write in this batch replaced it.
    let cssom = w.csp.cssom.borrow_mut().remove(&n);
    if let Some(c) = &cssom
        && value.as_ref() != Some(c)
    {
        w.csp.style_kept.borrow_mut().insert(n, Some(c.clone()));
    }
    let allowed = match &value {
        None => true,
        Some(v) if cssom.as_ref() == Some(v) => true,
        Some(v) => {
            let loc = if inserted {
                w.csp_element_loc(n).or_else(|| w.csp_here(Some(js)))
            } else {
                w.csp_here(Some(js))
            };
            w.csp_inline(Inline::StyleAttribute, Some(n), v, "", true, loc)
        }
    };
    if allowed {
        match value {
            Some(v) => {
                w.csp.style_kept.borrow_mut().insert(n, Some(v));
            }
            None => {
                w.csp.style_kept.borrow_mut().remove(&n);
            }
        }
        if w.csp.style_blocked_attr.borrow_mut().remove(&n) {
            set_style_override(w, n, None);
        }
        return;
    }
    let kept = w.csp.style_kept.borrow().get(&n).cloned().flatten();
    w.csp.style_blocked_attr.borrow_mut().insert(n);
    set_style_override(w, n, Some(kept));
}

fn set_style_override(w: &Window, n: NodeId, v: Option<Option<String>>) {
    w.with_layout(0, |engine, _| {
        engine.styles().set_inline_style_override(n, v)
    });
}

/// May a frame of `parent` show a document whose response came with
/// `headers` at `url` (its `frame-ancestors`)? A refused one is logged in
/// the page's console (no event: the document is not shown). CDP bypass
/// of the page lifts it, as in Chrome.
pub fn frame_ancestors_allow(parent: &Rc<Window>, headers: &[(String, String)], url: &Url) -> bool {
    if parent.csp.bypass.get()
        || !headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("content-security-policy") && v.to_ascii_lowercase().contains("frame-ancestors"))
    {
        return true;
    }
    let mut errors = Vec::new();
    let mut list = CspList::default();
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("content-security-policy") {
            list.policies.extend(policy::parse(v, Disposition::Enforce, Delivery::Header, url, &mut errors));
        } else if k.eq_ignore_ascii_case("content-security-policy-report-only") {
            list.policies.extend(policy::parse(v, Disposition::Report, Delivery::Header, url, &mut errors));
        }
    }
    let mut ancestors = Vec::new();
    let mut cur = Some(parent.clone());
    while let Some(w) = cur {
        ancestors.push(super::frames::effective_url(&w));
        cur = super::frames::parent(&w);
    }
    let out = list.check_ancestors(&ancestors);
    for v in &out.violations {
        if let Some(t) = &v.console {
            parent.csp_console("error", t.clone());
        }
    }
    out.allowed
}

/// Give a request of `w` a redirect check (its fetch directive against
/// the window's policies) when a policy restricts it. Returns the
/// directive and the request URL, for [`redirect_refused`].
pub(crate) fn guard_redirects(w: &Window, req: &mut NetRequest) -> Option<(Dir, Url)> {
    if !w.csp_on() || req.redirect_check.is_some() {
        return None;
    }
    let dir = match req.destination {
        Destination::Image => Dir::ImgSrc,
        Destination::Style => Dir::StyleSrcElem,
        Destination::Empty => Dir::ConnectSrc,
        // Nonces and 'strict-dynamic' depend on the element: not checked.
        Destination::Script if !w.csp.list.borrow().element_dependent(Dir::ScriptSrcElem) => {
            Dir::ScriptSrcElem
        }
        _ => return None,
    };
    let list = w.csp.list.borrow().clone();
    if !list.restricts(dir) {
        return None;
    }
    req.redirect_check = Some(crate::host::RedirectCheck(std::sync::Arc::new(
        move |u: &Url| {
            list.check_url(
                dir,
                u,
                &UrlCheck {
                    original: Some(u),
                    ..Default::default()
                },
            )
            .allowed
        },
    )));
    Some((dir, req.url.clone()))
}

/// A request of `w` to `original` failed with `err`: when its redirect
/// check refused a hop, report the violation (the refused URL in the
/// message, the original as `blockedURI`).
pub(crate) fn redirect_refused(w: &Rc<Window>, dir: Dir, original: &Url, err: &str) {
    let Some(rest) = err.strip_prefix(crate::host::CSP_BLOCKED) else {
        return;
    };
    let Ok(to) = Url::parse(rest.trim()) else {
        return;
    };
    w.csp_url(
        dir,
        &to,
        &UrlCheck {
            original: Some(original),
            ..Default::default()
        },
        None,
    );
}

/// The `url()`s of a `FontFace` source string, checked against
/// `font-src` (the faces are not fetched either way; a blocked one is
/// reported).
pub(crate) fn check_font_src(cx: &mut CallCtx<'_, '_, '_>, w: &Rc<Window>, src: &str) {
    if !w.csp_on() {
        return;
    }
    let mut urls = Vec::new();
    let mut p = cssparser::Parser::new(src);
    while let Ok(t) = p.next_including_whitespace_and_comments() {
        match t.clone() {
            cssparser::Token::UnquotedUrl(u) => urls.push(u.to_string()),
            cssparser::Token::Function(f) if f.eq_ignore_ascii_case("url") => {
                let u: Result<String, cssparser::ParseError<()>> =
                    p.parse_nested_block(|p| Ok(p.expect_string()?.to_string()));
                if let Ok(u) = u {
                    urls.push(u);
                }
            }
            _ => {}
        }
    }
    for u in urls {
        let Some(url) = w.resolve_url(&u) else {
            continue;
        };
        let loc = w.csp_here(Some(&mut *cx));
        w.csp_url(Dir::FontSrc, &url, &UrlCheck::default(), loc);
    }
}

/// May the `<style>` element `n` with `text` apply (`style-src-elem`)?
/// A blocked one fires `error`; the same text is checked once.
pub(crate) fn style_element_allowed(w: &Rc<Window>, n: NodeId, text: &str) -> bool {
    if !w.csp_on() {
        return true;
    }
    if w.csp
        .blocked_styles
        .borrow()
        .get(&n)
        .is_some_and(|t| t == text)
    {
        return false;
    }
    let nonce = w.csp_nonce(n);
    if w.csp_inline(
        Inline::Style,
        Some(n),
        text,
        &nonce,
        true,
        w.csp_parser_loc(),
    ) {
        w.csp.blocked_styles.borrow_mut().remove(&n);
        return true;
    }
    w.csp.blocked_styles.borrow_mut().insert(n, text.to_owned());
    false
}

/// The `<style>` element `n`'s current text was blocked (its sheet is
/// empty and it fires `error`).
pub(crate) fn style_blocked(w: &Window, n: NodeId) -> bool {
    w.csp.blocked_styles.borrow().contains_key(&n)
}

/// Fire `securitypolicyviolation` (in a networking task): at the element
/// for inline violations while it is connected, else at the document, or
/// at a worker's global scope.
fn dispatch_violation(
    js: &mut dyn Js,
    w: &Rc<Window>,
    element: Option<NodeId>,
    init: ViolationInit,
) {
    if w.is_closed() {
        return;
    }
    let target = if w.is_worker() {
        WINDOW_REF
    } else {
        match element.filter(|&n| w.with_doc(|d| d.is_connected(n))) {
            Some(n) => w.node_ref(0, n),
            None => w.node_ref(0, Document::DOCUMENT),
        }
    };
    let ev = super::events::new_event(
        w,
        "SecurityPolicyViolationEvent",
        "securitypolicyviolation",
        true,
        false,
        EventExtra::Csp(Box::new(init)),
    );
    w.with_obj(ev, |o| {
        if let Obj::Event(e) = o {
            e.composed = true;
        }
    });
    super::events::dispatch(js, w, target, ev);
}

fn this_init(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<ViolationInit, HostError> {
    let id = match cx.this_host() {
        Some(h) if h.kind == K_OBJ => h.id,
        _ => return Err(illegal()),
    };
    w.with_obj(id, |o| match o {
        Obj::Event(e) => match &e.extra {
            EventExtra::Csp(c) => Some((**c).clone()),
            _ => Some(ViolationInit::default()),
        },
        _ => None,
    })
    .flatten()
    .ok_or_else(illegal)
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    ctor(reg, rt, "SecurityPolicyViolationEvent", |cx, w| {
        if cx.is_empty() {
            return Err(HostError::type_error(
                "Failed to construct 'SecurityPolicyViolationEvent': 1 argument required, but only 0 present.",
            ));
        }
        let type_ = cx.arg_string(0)?;
        let d = Dict::arg(cx, 1);
        let s = |cx: &mut CallCtx<'_, '_, '_>, k: &str| -> Result<String, HostError> {
            Ok(d.string(cx, k)?.unwrap_or_default())
        };
        let n = |cx: &mut CallCtx<'_, '_, '_>, k: &str| -> Result<f64, HostError> {
            Ok(d.number(cx, k)?.unwrap_or(0.0))
        };
        // Chrome's IDL order of the dictionary members.
        let init = ViolationInit {
            blocked_uri: s(cx, "blockedURI")?,
            column_number: n(cx, "columnNumber")? as u32,
            disposition: s(cx, "disposition")?,
            document_uri: s(cx, "documentURI")?,
            effective_directive: s(cx, "effectiveDirective")?,
            line_number: n(cx, "lineNumber")? as u32,
            original_policy: s(cx, "originalPolicy")?,
            referrer: s(cx, "referrer")?,
            sample: s(cx, "sample")?,
            source_file: s(cx, "sourceFile")?,
            status_code: n(cx, "statusCode")? as u16,
            violated_directive: s(cx, "violatedDirective")?,
        };
        d.release(cx);
        // Blink makes it bubbling and composed whatever the init says.
        let mut e =
            super::events::EventData::new("SecurityPolicyViolationEvent", &type_, true, false);
        e.composed = true;
        e.extra = EventExtra::Csp(Box::new(init));
        e.time = super::events::coarse_now(w);
        let id = w.new_obj(Obj::Event(Box::new(e)));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    macro_rules! field {
        ($name:literal, |$i:ident| $v:expr) => {
            getter(reg, rt, "SecurityPolicyViolationEvent", $name, |cx, w| {
                let $i = this_init(cx, w)?;
                cx.ret($v)
            });
        };
    }
    field!("documentURI", |i| i.document_uri);
    field!("referrer", |i| i.referrer);
    field!("blockedURI", |i| i.blocked_uri);
    field!("violatedDirective", |i| i.violated_directive);
    field!("effectiveDirective", |i| i.effective_directive);
    field!("originalPolicy", |i| i.original_policy);
    field!("disposition", |i| if i.disposition.is_empty() {
        "enforce".to_string()
    } else {
        i.disposition
    });
    field!("sourceFile", |i| i.source_file);
    field!("sample", |i| i.sample);
    field!("statusCode", |i| u32::from(i.status_code));
    field!("lineNumber", |i| i.line_number);
    field!("columnNumber", |i| i.column_number);
    // `HTMLBaseElement.href` (its changes are processed for `base-uri`):
    // the attribute resolved against the document URL.
    getter(reg, rt, "HTMLBaseElement", "href", |cx, w| {
        let (slot, n) = super::this_node(cx, w)?;
        let attr = w.doc_slot(slot, |d| d.get_attribute(n, "href").map(str::to_owned));
        let base = w.url();
        cx.ret(match attr {
            None => base.to_string(),
            Some(a) => base.join(a.trim()).map_or(a, |u| u.to_string()),
        })
    });
    super::setter(reg, rt, "HTMLBaseElement", "href", |cx, w| {
        let (slot, n) = super::this_node(cx, w)?;
        let v = cx.arg_string(0)?;
        let _ = w.doc_slot_mut(slot, |d| d.set_attribute(n, "href", &v));
        w.after_mutation(cx);
        Ok(())
    });
    // The `nonce` IDL attribute: an internal slot, not the content
    // attribute (it is hidden once a header-delivered policy applies).
    for iface in ["HTMLElement", "SVGElement", "MathMLElement"] {
        getter(reg, rt, iface, "nonce", |cx, w| {
            let (slot, n) = super::this_node(cx, w)?;
            if slot != 0 {
                let v = w.doc_slot(slot, |d| {
                    d.get_attribute(n, "nonce").unwrap_or("").to_owned()
                });
                return cx.ret(v);
            }
            cx.ret(w.csp_nonce(n))
        });
        super::setter(reg, rt, iface, "nonce", |cx, w| {
            let (slot, n) = super::this_node(cx, w)?;
            let v = cx.arg_string(0)?;
            if slot == 0 {
                w.csp.nonces.borrow_mut().insert(n, v);
            }
            Ok(())
        });
    }
    let _ = JsValue::Undefined;
}
