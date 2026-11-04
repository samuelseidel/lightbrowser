//! Script elements: classification, execution (`document.currentScript`,
//! load/error events), scripts inserted by script, and
//! `document.write` / `open` / `close`.
//!
//! Parser-inserted scripts are driven by the page layer (lb-page), which
//! uses the helpers here; scripts inserted by script are prepared here.

use std::rc::Rc;

use lb_dom::{Document, HtmlParser, NodeFlags, NodeId, ParseStep, local_name};
use lb_js::ScriptOrigin;
use url::Url;

use super::events::fire_event;
use super::{Js, Window};
use crate::host::{Destination, NetRequest, decode_text};

/// Set on scripts whose `async` IDL attribute was set by script (clears
/// the "force async" default of script-created scripts).
pub const NOT_FORCE_ASYNC: NodeFlags = NodeFlags::USER_0;

/// How a `<script>` element is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptKind {
    Classic,
    Module,
    /// Not JavaScript (`type="text/template"`, JSON, ...): not run.
    Data,
}

/// Classify a script element by its `type` / `language` attributes.
pub fn script_kind(d: &Document, n: NodeId) -> ScriptKind {
    let ty = d
        .get_attribute(n, "type")
        .map(|t| t.trim().to_ascii_lowercase());
    match ty.as_deref() {
        None | Some("") => match d.get_attribute(n, "language") {
            Some(l)
                if !l.is_empty() && !is_js_mime(&format!("text/{}", l.to_ascii_lowercase())) =>
            {
                ScriptKind::Data
            }
            _ => ScriptKind::Classic,
        },
        Some("module") => ScriptKind::Module,
        Some(t) if is_js_mime(t) => ScriptKind::Classic,
        _ => ScriptKind::Data,
    }
}

fn is_js_mime(t: &str) -> bool {
    matches!(
        t,
        "application/ecmascript"
            | "application/javascript"
            | "application/x-ecmascript"
            | "application/x-javascript"
            | "text/ecmascript"
            | "text/javascript"
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

/// Inline source of a script element.
pub fn inline_text(w: &Window, n: NodeId) -> String {
    w.with_doc(|d| d.child_text_content(n))
}

/// The resolved `src` URL, `Some(None)` when `src` is present but invalid.
pub fn script_src(w: &Window, n: NodeId) -> Option<Option<Url>> {
    let src = w.with_doc(|d| d.get_attribute(n, "src").map(str::to_owned))?;
    Some(if src.trim().is_empty() {
        None
    } else {
        w.resolve_url(&src)
    })
}

/// Whether a script element is `async` / `defer` (attributes).
pub fn script_flags(w: &Window, n: NodeId) -> (bool, bool) {
    w.with_doc(|d| (d.has_attribute(n, "async"), d.has_attribute(n, "defer")))
}

/// Run a classic script with `document.currentScript` set; exceptions are
/// reported. `line` is the 0-based line offset of inline scripts.
pub fn execute_classic(
    js: &mut dyn Js,
    w: &Rc<Window>,
    node: Option<NodeId>,
    source: &str,
    url: &str,
    line: u32,
) {
    let saved = w.current_script.replace(node);
    let r = js.eval(source, ScriptOrigin::new(url).at(line, 0));
    w.current_script.set(saved);
    if let Err(e) = r {
        w.report_exception(js, &e);
    }
    w.after_mutation(js);
}

/// Fire `load` at a script element.
pub fn fire_load(js: &mut dyn Js, w: &Rc<Window>, n: NodeId) {
    fire_event(js, w, w.node_ref(0, n), "load", false, false);
}

/// Fire `error` at a script element.
pub fn fire_error(js: &mut dyn Js, w: &Rc<Window>, n: NodeId) {
    fire_event(js, w, w.node_ref(0, n), "error", false, false);
}

/// Decode a fetched script body (`charset` attribute / header, else
/// UTF-8).
pub fn script_source(body: &[u8], content_type: Option<&str>) -> String {
    let ct = content_type
        .filter(|c| crate::host::charset_param(c).is_some())
        .map(str::to_owned)
        .unwrap_or_else(|| "text/javascript;charset=utf-8".into());
    decode_text(body, Some(&ct), false).0
}

/// Prepare a script inserted by script (not by the parser): inline
/// classic scripts run now; external ones load and run when fetched
/// (async), delaying the `load` event.
pub(crate) fn prepare_dynamic(js: &mut dyn Js, w: &Rc<Window>, n: NodeId) {
    let kind = w.with_doc(|d| {
        if d.flags(n).contains(NodeFlags::ALREADY_STARTED) {
            return None;
        }
        Some(script_kind(d, n))
    });
    let Some(kind) = kind else { return };
    if kind == ScriptKind::Data {
        return;
    }
    // A script with neither src nor text stays "not started" until it
    // gets one (spec); everything else is started now.
    let has_src = script_src(w, n).is_some();
    let text = inline_text(w, n);
    if !has_src && text.is_empty() {
        return;
    }
    w.with_doc_mut(|d| d.set_flags(n, NodeFlags::ALREADY_STARTED, true));
    if kind == ScriptKind::Module {
        w.warn("module scripts inserted by script are not supported yet; skipped");
        return;
    }
    match script_src(w, n) {
        None => {
            let url = w.url().to_string();
            execute_classic(js, w, Some(n), &text, &url, 0);
        }
        Some(None) => {
            let w2 = w.clone();
            w.queue_task(
                super::TaskSource::DomManipulation,
                Box::new(move |js, _| fire_error(js, &w2, n)),
            );
        }
        Some(Some(url)) => {
            w.pending_loads.set(w.pending_loads.get() + 1);
            let req = NetRequest::get(url.clone(), Destination::Script, Some(w.url()));
            super::net::start_request(
                w,
                req,
                Box::new(move |js, w, r| {
                    match r {
                        Ok(resp) if (200..300).contains(&resp.status) => {
                            let src = script_source(&resp.body, resp.header("content-type"));
                            execute_classic(js, w, Some(n), &src, resp.url.as_str(), 0);
                            fire_load(js, w, n);
                        }
                        _ => fire_error(js, w, n),
                    }
                    w.pending_loads.set(w.pending_loads.get().saturating_sub(1));
                    w.wake();
                }),
            );
        }
    }
}

/// `document.write(text)`.
pub(crate) fn document_write(js: &mut dyn Js, w: &Rc<Window>, text: &str) {
    let wrote = {
        let mut p = w.parser.borrow_mut();
        match p.as_mut() {
            Some(parser) => {
                let doc = w.doc_rc(0);
                let r = parser.write(&mut doc.borrow_mut(), text);
                Some(r)
            }
            None => None,
        }
    };
    match wrote {
        Some(true) => {}
        Some(false) => {
            w.warn("Failed to execute 'write' on 'Document': It isn't possible to write into a document from an asynchronously-loaded external script unless it is explicitly opened.");
        }
        None => {
            // No active parser: implicit document.open(), write, and leave
            // the document open until close().
            document_open(w);
            {
                let mut p = w.parser.borrow_mut();
                if let Some(parser) = p.as_mut() {
                    parser.push_str(text);
                }
            }
            run_script_parser(js, w, false);
        }
    }
    w.after_mutation(js);
}

/// `document.open()` on a loaded document: empty it and start a parser.
pub(crate) fn document_open(w: &Window) {
    w.with_doc_mut(|d| d.remove_all_children(Document::DOCUMENT));
    *w.parser.borrow_mut() = Some(HtmlParser::new());
    w.opened.set(true);
}

/// `document.close()`: finish a parser started by `document.open()`.
pub(crate) fn document_close(js: &mut dyn Js, w: &Rc<Window>) {
    if !w.opened.get() {
        return;
    }
    if let Some(p) = w.parser.borrow_mut().as_mut() {
        p.end();
    }
    run_script_parser(js, w, true);
    *w.parser.borrow_mut() = None;
    w.opened.set(false);
    w.after_mutation(js);
}

/// Drive a script-created parser; inline classic scripts in the written
/// markup run (external ones are fetched like dynamic scripts).
fn run_script_parser(js: &mut dyn Js, w: &Rc<Window>, to_end: bool) {
    loop {
        let step = {
            let mut p = w.parser.borrow_mut();
            let Some(parser) = p.as_mut() else { return };
            let doc = w.doc_rc(0);
            parser.run(&mut doc.borrow_mut())
        };
        match step {
            ParseStep::Script(n) => {
                w.after_mutation(js);
                let is_script = w.with_doc(|d| d.is_html_element_named(n, &local_name!("script")));
                if is_script {
                    w.with_doc_mut(|d| d.set_flags(n, NodeFlags::PARSER_INSERTED, false));
                    prepare_dynamic(js, w, n);
                }
            }
            ParseStep::StyleSheet(_) => {}
            ParseStep::CustomElement(_) => w.after_mutation(js),
            ParseStep::NeedInput | ParseStep::Done => {
                let _ = to_end;
                return;
            }
        }
    }
}

/// Static import specifiers of a module (`import ... from "x"`,
/// `import "x"`, `export ... from "x"`). A scan, not a parser.
pub fn import_specifiers(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = src.as_bytes();
    let mut i = 0;
    let quoted_after = |mut j: usize| -> Option<(String, usize)> {
        while j < b.len() && (b[j] as char).is_whitespace() {
            j += 1;
        }
        let q = *b.get(j)?;
        if q != b'"' && q != b'\'' {
            return None;
        }
        let end = src[j + 1..].find(q as char)? + j + 1;
        Some((src[j + 1..end].to_owned(), end + 1))
    };
    while i < b.len() {
        let rest = &src[i..];
        let word_start =
            i == 0 || !(b[i - 1] as char).is_alphanumeric() && b[i - 1] != b'_' && b[i - 1] != b'.';
        let kw = if !word_start {
            0
        } else if rest.starts_with("from") {
            4
        } else if rest.starts_with("import") {
            6
        } else {
            0
        };
        if kw > 0
            && let Some((s, end)) = quoted_after(i + kw)
        {
            out.push(s);
            i = end;
            continue;
        }
        i += 1;
    }
    out
}
