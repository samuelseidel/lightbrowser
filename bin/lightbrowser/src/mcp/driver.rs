//! The browser behind the tools (chrome build): a DevTools client that
//! talks to `lb_page::CdpHost` over [`lb_cdp::InProcess`], channels inside
//! this process with no listener. Everything a tool does is a CDP command
//! Playwright would send: `Page.navigate`, `Accessibility.getFullAXTree`,
//! `Input.dispatch*Event` (the trusted input pipeline), `Runtime.evaluate`
//! in an isolated world, `Page.captureScreenshot`.
//!
//! One page per server. A reader task answers nothing by itself but keeps
//! the page state (URL, load events, status, dialogs) current while no tool
//! runs, so a dialog never freezes the page between two calls.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use lb_cdp::InProcess;
use lb_page::CdpHost;
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc, oneshot};

use super::keys;
use super::policy::{self, BlockedLog, HostPolicy};
use super::rpc::ToolHost;
use super::snapshot::{self, Refs};
use super::tools::{self, ToolResult};

/// The isolated world every tool evaluates in.
const WORLD: &str = "lightbrowser-mcp";

/// Bytes of node lines in one snapshot part, leaving room under the 100 KB
/// output limit for the header, live values and the continuation note.
const SNAPSHOT_BUDGET: usize = snapshot::MAX_OUTPUT - 4096;

/// Run on an element before typing into it: refuses what cannot take text
/// (the result's `error`), else focuses it and selects its content so the
/// typed text replaces it.
const FOCUS_FOR_TYPING: &str = r#"function() {
  if (!this.isConnected) return {error: 'detached'};
  const tag = this.tagName ? this.tagName.toLowerCase() : '';
  let field = false;
  if (tag === 'input') {
    const t = (this.type || 'text').toLowerCase();
    if (['checkbox', 'radio', 'button', 'submit', 'reset', 'image', 'file', 'range', 'color', 'hidden'].includes(t))
      return {error: 'an <input type=' + t + '> does not take typed text'};
    field = true;
  } else if (tag === 'textarea') {
    field = true;
  } else if (tag === 'select') {
    return {error: 'a <select> cannot be typed into (typing does not choose an option)'};
  } else if (!this.isContentEditable) {
    return {error: 'a <' + tag + '> element is not editable'};
  }
  if (field && this.disabled) return {error: 'the field is disabled'};
  if (field && this.readOnly) return {error: 'the field is read-only'};
  if (this.scrollIntoView) this.scrollIntoView({block: 'center'});
  this.focus();
  if (field) {
    try { this.select(); } catch (e) {}
  } else {
    const sel = this.ownerDocument.defaultView.getSelection();
    if (sel && sel.selectAllChildren) sel.selectAllChildren(this); else this.textContent = '';
  }
  return {};
}"#;

/// Longest live field value shown in a snapshot, in characters.
const MAX_VALUE_CHARS: usize = 2000;

/// Page state kept current by the reader task.
#[derive(Default)]
struct State {
    /// Main frame id (equals the target id).
    target: Option<String>,
    session: Option<String>,
    url: String,
    loader: String,
    /// Lifecycle event names seen per loader id.
    lifecycle: HashMap<String, HashSet<String>>,
    /// Status of the last document response per loader id.
    statuses: HashMap<String, u16>,
    /// Bumped by every main-frame navigation.
    doc_gen: u64,
    /// The main frame is between `frameStartedLoading` and `frameStoppedLoading`.
    loading: bool,
    /// Isolated world of document `doc_gen`: (doc_gen, execution context id).
    world: Option<(u64, i64)>,
    refs: Refs,
    /// Things worth telling the agent (dialogs answered).
    notes: Vec<String>,
    /// URL per `Network` request id of the page, until the request
    /// finishes or fails (to name a request the egress policy refused).
    requests: HashMap<String, String>,
}

/// Most open requests remembered; past it the map starts over (a request
/// that never finishes must not grow it without bound).
const MAX_OPEN_REQUESTS: usize = 4096;

struct Shared {
    next_id: Cell<i64>,
    pending: RefCell<HashMap<i64, oneshot::Sender<Result<Value, String>>>>,
    st: RefCell<State>,
    changed: Notify,
    /// Written by the reader when the transport refuses a request.
    blocked: BlockedLog,
}

pub struct Driver {
    cdp: Rc<InProcess<CdpHost>>,
    /// The same host, for what is not a CDP command (`browser_extract`
    /// markdown, ADR 0052).
    host: Rc<CdpHost>,
    shared: Rc<Shared>,
    timeout: Duration,
    blocked: BlockedLog,
    policy: HostPolicy,
}

impl Driver {
    /// Starts the browser on `host`. Must run inside `lb_cdp::block_on_local`.
    pub fn new(
        host: CdpHost,
        timeout: Duration,
        policy: HostPolicy,
        blocked: BlockedLog,
    ) -> Result<Driver, String> {
        let host = Rc::new(host);
        let (cdp, rx) = InProcess::connect(host.clone()).map_err(|e| e.to_string())?;
        let cdp = Rc::new(cdp);
        let shared = Rc::new(Shared {
            next_id: Cell::new(1),
            pending: RefCell::new(HashMap::new()),
            st: RefCell::new(State::default()),
            changed: Notify::new(),
            blocked: blocked.clone(),
        });
        tokio::task::spawn_local(reader(cdp.clone(), shared.clone(), rx));
        Ok(Driver {
            cdp,
            host,
            shared,
            timeout,
            blocked,
            policy,
        })
    }

    /// Closes the browser connection (stdin closed).
    pub fn shutdown(&self) {
        self.cdp.close();
    }

    // ---- CDP plumbing ----

    async fn send(
        &self,
        method: &str,
        params: Value,
        session: Option<String>,
    ) -> Result<Value, String> {
        let id = self.shared.next_id.get();
        self.shared.next_id.set(id + 1);
        let (tx, rx) = oneshot::channel();
        self.shared.pending.borrow_mut().insert(id, tx);
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.cdp.send(msg.to_string());
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(r)) => r.map_err(|e| format!("{method}: {e}")),
            Ok(Err(_)) => Err(format!("{method}: the browser went away")),
            Err(_) => {
                self.shared.pending.borrow_mut().remove(&id);
                Err(format!(
                    "{method}: timed out after {} s (--timeout)",
                    self.timeout.as_secs()
                ))
            }
        }
    }

    /// A command on the page's session (the page must exist).
    async fn page_cmd(&self, method: &str, params: Value) -> Result<Value, String> {
        let session = self.shared.st.borrow().session.clone();
        match session {
            Some(s) => self.send(method, params, Some(s)).await,
            None => Err("no page is open".into()),
        }
    }

    /// Waits until `cond` holds on the state or `limit` passes; returns
    /// whether it held.
    async fn wait_for(&self, limit: Duration, cond: impl Fn(&State) -> bool) -> bool {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let notified = self.shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if cond(&self.shared.st.borrow()) {
                return true;
            }
            tokio::select! {
                () = &mut notified => {}
                () = tokio::time::sleep_until(deadline) => {
                    return cond(&self.shared.st.borrow());
                }
            }
        }
    }

    /// The page, made on first use. Returns its session id.
    async fn ensure_page(&self) -> Result<(), String> {
        if self.shared.st.borrow().session.is_some() {
            return Ok(());
        }
        let r = self
            .send("Target.createTarget", json!({"url": "about:blank"}), None)
            .await?;
        let target = r["targetId"]
            .as_str()
            .ok_or("Target.createTarget returned no targetId")?
            .to_owned();
        let r = self
            .send(
                "Target.attachToTarget",
                json!({"targetId": target, "flatten": true}),
                None,
            )
            .await?;
        let session = r["sessionId"]
            .as_str()
            .ok_or("Target.attachToTarget returned no sessionId")?
            .to_owned();
        {
            let mut st = self.shared.st.borrow_mut();
            *st = State::default();
            st.target = Some(target);
            st.session = Some(session);
            st.url = "about:blank".into();
        }
        for (m, p) in [
            ("Page.enable", json!({})),
            ("Page.setLifecycleEventsEnabled", json!({"enabled": true})),
            ("Network.enable", json!({})),
        ] {
            self.page_cmd(m, p).await?;
        }
        Ok(())
    }

    /// The isolated world of the current document.
    async fn world(&self) -> Result<i64, String> {
        let (gen_now, target) = {
            let st = self.shared.st.borrow();
            if let Some((g, id)) = st.world
                && g == st.doc_gen
            {
                return Ok(id);
            }
            (st.doc_gen, st.target.clone().ok_or("no page is open")?)
        };
        let r = self
            .page_cmd(
                "Page.createIsolatedWorld",
                json!({"frameId": target, "worldName": WORLD}),
            )
            .await?;
        let id = r["executionContextId"]
            .as_i64()
            .ok_or("Page.createIsolatedWorld returned no execution context")?;
        self.shared.st.borrow_mut().world = Some((gen_now, id));
        Ok(id)
    }

    /// Evaluates `expression` in the isolated world (promises awaited) and
    /// returns the `RemoteObject`.
    async fn eval(&self, expression: &str) -> Result<Value, String> {
        let ctx = self.world().await?;
        let r = self
            .page_cmd(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "contextId": ctx,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;
        if let Some(e) = r.get("exceptionDetails") {
            return Err(exception_text(e));
        }
        Ok(r["result"].clone())
    }

    async fn eval_string(&self, expression: &str) -> Result<String, String> {
        let r = self.eval(expression).await?;
        Ok(r["value"].as_str().unwrap_or_default().to_owned())
    }

    /// Notes and blocked requests since the last call, appended to a text.
    fn finish(&self, mut text: String) -> ToolResult {
        let notes = std::mem::take(&mut self.shared.st.borrow_mut().notes);
        for n in notes {
            text.push_str("\nNote: ");
            text.push_str(&n);
        }
        let blocked = self.blocked.take();
        if !blocked.is_empty() {
            text.push_str("\nNote: ");
            text.push_str(&policy::blocked_note(&blocked));
        }
        ToolResult::text(text)
    }

    // ---- tools ----

    async fn navigate(&self, url: &str, wait_until: &str) -> Result<ToolResult, String> {
        let parsed = url::Url::parse(url).map_err(|e| {
            format!(
                "invalid URL {url:?}: {e} (include the scheme, for example http://127.0.0.1:8000/)"
            )
        })?;
        if !self.policy.allows(&parsed) {
            return Ok(ToolResult::error(policy::blocked_message(&[
                parsed.to_string()
            ])));
        }
        let wait_name = match wait_until {
            "load" => "load",
            "domcontentloaded" => "DOMContentLoaded",
            "networkidle" => "networkIdle",
            other => {
                return Err(format!(
                    "wait_until must be load, domcontentloaded or networkidle, got {other:?}"
                ));
            }
        };
        self.ensure_page().await?;
        self.blocked.clear();
        let r = self
            .page_cmd("Page.navigate", json!({"url": parsed.as_str()}))
            .await?;
        if let Some(err) = r["errorText"].as_str().filter(|e| !e.is_empty()) {
            let blocked = self.blocked.take();
            return Ok(if !blocked.is_empty() {
                ToolResult::error(policy::blocked_message(&blocked))
            } else if err.contains("ERR_BLOCKED_BY_CLIENT") {
                ToolResult::error(policy::blocked_message(&[url.to_owned()]))
            } else {
                ToolResult::error(format!("navigation to {url} failed: {err}"))
            });
        }
        let loader = r["loaderId"].as_str().map(str::to_owned);
        let mut timed_out = false;
        if let Some(l) = &loader {
            let l = l.clone();
            timed_out = !self
                .wait_for(self.timeout, move |s| {
                    s.lifecycle.get(&l).is_some_and(|n| n.contains(wait_name))
                })
                .await;
        }
        let title = self.eval_string("document.title").await.unwrap_or_default();
        // The document's own URL: a `#fragment` navigation starts no document
        // and the host sends no event for it, so the frame URL can be stale.
        let (frame_url, status) = {
            let st = self.shared.st.borrow();
            let status = loader.as_ref().and_then(|l| st.statuses.get(l)).copied();
            (st.url.clone(), status)
        };
        let final_url = self.href().await.unwrap_or(frame_url);
        let mut text = format!("Navigated to: {final_url}\n");
        if final_url != parsed.as_str() && final_url != url {
            text.push_str(&format!("Requested: {url}\n"));
        }
        text.push_str(&format!("Title: {title}\n"));
        match status {
            Some(s) => text.push_str(&format!("Status: {s}")),
            None => text.push_str("Status: none (no network response)"),
        }
        if timed_out {
            text.push_str(&format!(
                "\nNote: {wait_until} did not happen within {} s (--timeout); the page may still be loading",
                self.timeout.as_secs()
            ));
        }
        Ok(self.finish(text))
    }

    async fn snapshot(&self, start_line: usize) -> Result<ToolResult, String> {
        self.ensure_page().await?;
        let r = self
            .page_cmd("Accessibility.getFullAXTree", json!({}))
            .await?;
        let nodes = r["nodes"].as_array().cloned().unwrap_or_default();
        let (url, mut lines, fields) = {
            let mut st = self.shared.st.borrow_mut();
            let (lines, fields) = snapshot::render(&nodes, &mut st.refs);
            (st.url.clone(), lines, fields)
        };
        let url = self.href().await.unwrap_or(url);
        let total = lines.len();
        if start_line > total.max(1) {
            return Err(format!(
                "start_line {start_line} is past the end: the snapshot has {total} lines"
            ));
        }
        // The part of the tree this call returns: the rest is reached with
        // `start_line`, so nothing is out of reach behind the size limit.
        let (from, to) = snapshot::window(&lines, start_line - 1, SNAPSHOT_BUDGET);
        // What was typed lives in the DOM property, not in the AX tree. Only
        // the fields shown are read.
        let shown: Vec<&snapshot::Field> = fields
            .iter()
            .filter(|f| (from..to).contains(&f.line))
            .collect();
        let world = if shown.is_empty() {
            None
        } else {
            self.world().await.ok()
        };
        for f in shown {
            let live = match world {
                Some(w) => self.live_value(f.node, w).await,
                None => None,
            };
            let v = live.unwrap_or_else(|| f.fallback.clone());
            if !v.is_empty() {
                lines[f.line].push_str(&snapshot::value_suffix(&v, MAX_VALUE_CHARS));
            }
        }
        let title = nodes
            .first()
            .and_then(|n| n["name"]["value"].as_str())
            .unwrap_or_default();
        let mut text = format!("Page URL: {url}\nPage Title: {title}\n");
        if from > 0 || to < total {
            text.push_str(&format!(
                "Showing node lines {}-{} of {total}\n",
                from + 1,
                to
            ));
        }
        text.push('\n');
        text.push_str(&lines[from..to].join("\n"));
        if to < total {
            text.push_str(&format!(
                "\n\n[snapshot cut: call browser_snapshot with start_line={} for the rest ({} more lines)]",
                to + 1,
                total - to
            ));
        }
        Ok(self.finish(snapshot::truncate(text)))
    }

    /// The `value` property of a form control, in the isolated world.
    async fn live_value(&self, node: i64, world: i64) -> Option<String> {
        let obj = self
            .page_cmd(
                "DOM.resolveNode",
                json!({"backendNodeId": node, "executionContextId": world}),
            )
            .await
            .ok()?;
        let id = obj["object"]["objectId"].as_str()?;
        let r = self
            .page_cmd(
                "Runtime.callFunctionOn",
                json!({
                    "objectId": id,
                    "functionDeclaration": "function() { return ('value' in this) ? String(this.value) : null; }",
                    "returnByValue": true,
                }),
            )
            .await
            .ok()?;
        r["result"]["value"].as_str().map(str::to_owned)
    }

    fn node_for(&self, r: &str) -> Result<i64, String> {
        self.shared.st.borrow().refs.resolve(r).ok_or_else(|| {
            format!(
                "unknown ref {r:?}: refs come from browser_snapshot and are reset by every navigation; take a new snapshot"
            )
        })
    }

    fn stale(r: &str, e: &str) -> String {
        format!("ref {r} is no longer usable ({e}); the element left the page, take a new snapshot")
    }

    /// Centre of the element's first visible box, in CSS pixels.
    async fn center_of(&self, r: &str, node: i64) -> Result<(f64, f64), String> {
        self.page_cmd("DOM.scrollIntoViewIfNeeded", json!({"backendNodeId": node}))
            .await
            .map_err(|e| Self::stale(r, &e))?;
        let q = self
            .page_cmd("DOM.getContentQuads", json!({"backendNodeId": node}))
            .await
            .map_err(|e| Self::stale(r, &e))?;
        for quad in q["quads"].as_array().into_iter().flatten() {
            let p: Vec<f64> = quad
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_f64).collect())
                .unwrap_or_default();
            if p.len() < 8 {
                continue;
            }
            let (xs, ys) = ([p[0], p[2], p[4], p[6]], [p[1], p[3], p[5], p[7]]);
            let w = xs.iter().cloned().fold(f64::MIN, f64::max)
                - xs.iter().cloned().fold(f64::MAX, f64::min);
            let h = ys.iter().cloned().fold(f64::MIN, f64::max)
                - ys.iter().cloned().fold(f64::MAX, f64::min);
            if w > 0.0 && h > 0.0 {
                return Ok((xs.iter().sum::<f64>() / 4.0, ys.iter().sum::<f64>() / 4.0));
            }
        }
        Err(format!(
            "ref {r} has no visible box (hidden, zero-sized or detached); it cannot be clicked"
        ))
    }

    /// The document's URL (`location.href`), which also follows `#fragment`
    /// and `pushState` navigations.
    async fn href(&self) -> Option<String> {
        self.eval_string("location.href").await.ok()
    }

    /// Where the page is now: the document generation and its URL.
    async fn mark(&self) -> (u64, String) {
        let href = self.href().await.unwrap_or_default();
        (self.shared.st.borrow().doc_gen, href)
    }

    /// After input: wait for a navigation the input started, and say so.
    async fn settle(&self, before: (u64, String)) -> String {
        let gen_before = before.0;
        self.wait_for(Duration::from_millis(150), move |s| {
            s.loading || s.doc_gen != gen_before
        })
        .await;
        if self.shared.st.borrow().loading {
            self.wait_for(self.timeout, |s| !s.loading).await;
        }
        let (gen_now, url) = {
            let st = self.shared.st.borrow();
            (st.doc_gen, st.url.clone())
        };
        if gen_now != before.0 {
            let title = self.eval_string("document.title").await.unwrap_or_default();
            format!(
                "\nThe page navigated to {url} (title {title:?}). Refs were reset: take a new snapshot."
            )
        } else {
            match self.href().await {
                Some(h) if !before.1.is_empty() && h != before.1 => format!(
                    "\nThe URL changed to {h} without loading a new document; refs are still valid."
                ),
                _ => String::new(),
            }
        }
    }

    async fn mouse(
        &self,
        kind: &str,
        x: f64,
        y: f64,
        button: &str,
        buttons: u32,
        clicks: u32,
    ) -> Result<(), String> {
        self.page_cmd(
            "Input.dispatchMouseEvent",
            json!({
                "type": kind, "x": x, "y": y, "button": button,
                "buttons": buttons, "clickCount": clicks, "modifiers": 0,
            }),
        )
        .await
        .map(|_| ())
    }

    async fn click(&self, r: &str) -> Result<ToolResult, String> {
        let node = self.node_for(r)?;
        self.ensure_page().await?;
        let (x, y) = self.center_of(r, node).await?;
        let gen_before = self.mark().await;
        self.mouse("mouseMoved", x, y, "none", 0, 0).await?;
        self.mouse("mousePressed", x, y, "left", 1, 1).await?;
        self.mouse("mouseReleased", x, y, "left", 0, 1).await?;
        let after = self.settle(gen_before).await;
        Ok(self.finish(format!("Clicked {r} at ({x:.0}, {y:.0}).{after}")))
    }

    async fn key_events(&self, events: Vec<Value>) -> Result<(), String> {
        for e in events {
            self.page_cmd("Input.dispatchKeyEvent", e).await?;
        }
        Ok(())
    }

    async fn type_text(&self, r: &str, text: &str, submit: bool) -> Result<ToolResult, String> {
        let node = self.node_for(r)?;
        self.ensure_page().await?;
        let world = self.world().await?;
        let obj = self
            .page_cmd(
                "DOM.resolveNode",
                json!({"backendNodeId": node, "executionContextId": world}),
            )
            .await
            .map_err(|e| Self::stale(r, &e))?;
        let object_id = obj["object"]["objectId"]
            .as_str()
            .ok_or_else(|| Self::stale(r, "no object"))?
            .to_owned();
        let focus = self
            .page_cmd(
                "Runtime.callFunctionOn",
                json!({
                    "objectId": object_id,
                    "functionDeclaration": FOCUS_FOR_TYPING,
                    "returnByValue": true,
                }),
            )
            .await?;
        if let Some(e) = focus.get("exceptionDetails") {
            return Err(format!("could not focus {r}: {}", exception_text(e)));
        }
        match focus["result"]["value"]["error"].as_str() {
            Some("detached") => return Err(Self::stale(r, "it is not in the document")),
            Some(why) => {
                return Err(format!(
                    "cannot type into {r}: {why}. browser_type works on text fields, textareas and contenteditable elements; use browser_click for buttons, links, checkboxes and selects"
                ));
            }
            None => {}
        }
        let gen_before = self.mark().await;
        // A line break is one Enter, whether the text has \n, \r or \r\n.
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        // Characters on the US layout are key presses; the rest is inserted.
        let mut pending = String::new();
        for c in text.chars() {
            match keys::char_events(c) {
                Some(ev) => {
                    if !pending.is_empty() {
                        self.page_cmd(
                            "Input.insertText",
                            json!({"text": std::mem::take(&mut pending)}),
                        )
                        .await?;
                    }
                    self.key_events(ev).await?;
                }
                None => pending.push(c),
            }
        }
        if !pending.is_empty() {
            self.page_cmd("Input.insertText", json!({"text": pending}))
                .await?;
        }
        if submit {
            self.key_events(keys::press_events("Enter")?).await?;
        }
        let after = self.settle(gen_before).await;
        let mut msg = format!(
            "Typed {} characters into {r}{}.",
            text.chars().count(),
            if submit { " and pressed Enter" } else { "" }
        );
        if after.is_empty() {
            let v = self
                .page_cmd(
                    "Runtime.callFunctionOn",
                    json!({
                        "objectId": object_id,
                        "functionDeclaration": "function() { return ('value' in this) ? String(this.value) : (this.isContentEditable ? this.textContent : null); }",
                        "returnByValue": true,
                    }),
                )
                .await
                .ok()
                .and_then(|v| v["result"]["value"].as_str().map(str::to_owned));
            if let Some(v) = v {
                msg.push_str(&format!(" The field now holds {}.", Value::String(v)));
            }
        }
        msg.push_str(&after);
        Ok(self.finish(msg))
    }

    async fn press_key(&self, key: &str) -> Result<ToolResult, String> {
        let events = keys::press_events(key)?;
        self.ensure_page().await?;
        let gen_before = self.mark().await;
        self.key_events(events).await?;
        let after = self.settle(gen_before).await;
        Ok(self.finish(format!("Pressed {key}.{after}")))
    }

    async fn evaluate(&self, expression: &str) -> Result<ToolResult, String> {
        self.ensure_page().await?;
        let r = self.eval(expression).await?;
        let text = if let Some(u) = r["unserializableValue"].as_str() {
            u.to_owned()
        } else if r["type"] == "undefined" {
            "undefined".to_owned()
        } else {
            serde_json::to_string_pretty(&r["value"]).unwrap_or_else(|_| "null".into())
        };
        Ok(self.finish(snapshot::truncate(text)))
    }

    async fn screenshot(&self) -> Result<ToolResult, String> {
        self.ensure_page().await?;
        let r = self
            .page_cmd("Page.captureScreenshot", json!({"format": "png"}))
            .await?;
        let data = r["data"]
            .as_str()
            .ok_or("Page.captureScreenshot returned no data")?
            .to_owned();
        Ok(ToolResult::image(data))
    }

    async fn extract(&self, format: &str) -> Result<ToolResult, String> {
        self.ensure_page().await?;
        if format == "markdown" {
            // `lightbrowser fetch --dump markdown`'s converter on the live
            // document, with the page's computed style deciding what is
            // hidden (ADR 0052).
            let target = self
                .shared
                .st
                .borrow()
                .target
                .clone()
                .ok_or("no page is open")?;
            let e = self.host.extract(&target).await?;
            return Ok(self.finish(snapshot::truncate(e.markdown)));
        }
        let expr = match format {
            "text" => {
                "document.body ? document.body.innerText : (document.documentElement ? document.documentElement.textContent : '')"
            }
            "html" => {
                "(document.doctype ? '<!DOCTYPE ' + document.doctype.name + '>\\n' : '') + (document.documentElement ? document.documentElement.outerHTML : '')"
            }
            other => {
                return Err(format!(
                    "format must be text, html or markdown, got {other:?}"
                ));
            }
        };
        let text = self.eval_string(expr).await?;
        Ok(self.finish(snapshot::truncate(text)))
    }

    async fn close(&self) -> Result<ToolResult, String> {
        let target = self.shared.st.borrow().target.clone();
        let Some(target) = target else {
            return Ok(ToolResult::text("No page was open."));
        };
        let r = self
            .send("Target.closeTarget", json!({"targetId": target}), None)
            .await;
        *self.shared.st.borrow_mut() = State::default();
        self.blocked.clear();
        r?;
        Ok(ToolResult::text(
            "Page closed. The next tool call opens a fresh page.",
        ))
    }
}

impl ToolHost for Driver {
    async fn call(&self, name: &str, args: Value) -> ToolResult {
        let r = match name {
            "browser_navigate" => match tools::str_arg(&args, "url") {
                Ok(url) => {
                    let wait = args
                        .get("wait_until")
                        .and_then(Value::as_str)
                        .unwrap_or("load");
                    self.navigate(url, wait).await
                }
                Err(e) => Err(e),
            },
            "browser_snapshot" => match tools::uint_arg(&args, "start_line") {
                Ok(n) => self.snapshot(n.unwrap_or(1)).await,
                Err(e) => Err(e),
            },
            "browser_click" => match tools::str_arg(&args, "ref") {
                Ok(r) => self.click(r).await,
                Err(e) => Err(e),
            },
            "browser_type" => match (
                tools::str_arg(&args, "ref"),
                tools::str_arg(&args, "text"),
                tools::bool_arg(&args, "submit"),
            ) {
                (Ok(r), Ok(t), Ok(s)) => self.type_text(r, t, s).await,
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => Err(e),
            },
            "browser_press_key" => match tools::str_arg(&args, "key") {
                Ok(k) => self.press_key(k).await,
                Err(e) => Err(e),
            },
            "browser_evaluate" => match tools::str_arg(&args, "expression") {
                Ok(e) => self.evaluate(e).await,
                Err(e) => Err(e),
            },
            "browser_screenshot" => self.screenshot().await,
            "browser_extract" => match tools::str_arg(&args, "format") {
                Ok(f) => self.extract(f).await,
                Err(e) => Err(e),
            },
            "browser_close" => self.close().await,
            other => Err(format!("unknown tool {other}")),
        };
        r.unwrap_or_else(ToolResult::error)
    }
}

fn exception_text(details: &Value) -> String {
    let desc = details["exception"]["description"]
        .as_str()
        .or_else(|| details["text"].as_str())
        .unwrap_or("script error");
    format!("JavaScript error: {desc}")
}

/// Routes replies to their callers and keeps the page state current.
async fn reader(
    cdp: Rc<InProcess<CdpHost>>,
    shared: Rc<Shared>,
    mut rx: mpsc::UnboundedReceiver<String>,
) {
    while let Some(text) = rx.recv().await {
        let Ok(msg) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(id) = msg.get("id").and_then(Value::as_i64) {
            if let Some(tx) = shared.pending.borrow_mut().remove(&id) {
                let r = match msg.get("error") {
                    Some(e) => Err(e["message"].as_str().unwrap_or("error").to_owned()),
                    None => Ok(msg["result"].clone()),
                };
                let _ = tx.send(r);
            }
            continue;
        }
        on_event(&cdp, &shared, &msg);
        shared.changed.notify_waiters();
    }
}

fn on_event(cdp: &InProcess<CdpHost>, shared: &Shared, msg: &Value) {
    let method = msg["method"].as_str().unwrap_or_default();
    let p = &msg["params"];
    let mut st = shared.st.borrow_mut();
    let ours = |frame: &Value, st: &State| {
        frame.as_str().is_some() && frame.as_str() == st.target.as_deref()
    };
    match method {
        "Page.lifecycleEvent" => {
            if let (Some(l), Some(n)) = (p["loaderId"].as_str(), p["name"].as_str()) {
                st.lifecycle
                    .entry(l.to_owned())
                    .or_default()
                    .insert(n.to_owned());
            }
        }
        "Page.frameNavigated" => {
            let f = &p["frame"];
            if f.get("parentId").is_none() && ours(&f["id"], &st) {
                st.url = f["url"].as_str().unwrap_or_default().to_owned();
                st.loader = f["loaderId"].as_str().unwrap_or_default().to_owned();
                // Only the current document's entries are ever read again
                // (its own events can arrive before this one, and stay).
                let keep = st.loader.clone();
                st.lifecycle.retain(|l, _| *l == keep);
                st.statuses.retain(|l, _| *l == keep);
                st.doc_gen += 1;
                st.world = None;
                st.refs.reset();
            }
        }
        "Page.navigatedWithinDocument" => {
            if ours(&p["frameId"], &st) {
                st.url = p["url"].as_str().unwrap_or_default().to_owned();
            }
        }
        "Page.frameStartedLoading" => {
            if ours(&p["frameId"], &st) {
                st.loading = true;
            }
        }
        "Page.frameStoppedLoading" => {
            if ours(&p["frameId"], &st) {
                st.loading = false;
            }
        }
        "Network.responseReceived" => {
            if p["type"] == "Document"
                && ours(&p["frameId"], &st)
                && let (Some(l), Some(s)) =
                    (p["loaderId"].as_str(), p["response"]["status"].as_u64())
            {
                st.statuses.insert(l.to_owned(), s as u16);
            }
        }
        "Network.requestWillBeSent" => {
            if let (Some(id), Some(url)) = (p["requestId"].as_str(), p["request"]["url"].as_str()) {
                if st.requests.len() >= MAX_OPEN_REQUESTS {
                    st.requests.clear();
                }
                st.requests.insert(id.to_owned(), url.to_owned());
            }
        }
        "Network.loadingFinished" => {
            if let Some(id) = p["requestId"].as_str() {
                st.requests.remove(id);
            }
        }
        "Network.loadingFailed" => {
            // The egress policy (ADR 0044) refuses with Chrome's
            // `net::ERR_BLOCKED_BY_CLIENT`; this server intercepts nothing
            // (no `Fetch` domain), so nothing else blocks a request.
            let url = p["requestId"]
                .as_str()
                .and_then(|id| st.requests.remove(id));
            if p["errorText"]
                .as_str()
                .is_some_and(|e| e.starts_with("net::ERR_BLOCKED_BY_CLIENT"))
                && let Some(url) = url
            {
                shared.blocked.push(url);
            }
        }
        "Page.javascriptDialogOpening" => {
            // Nobody can answer a dialog in a headless run: accept alerts and
            // leave-page prompts, dismiss confirm and prompt, and say so.
            let kind = p["type"].as_str().unwrap_or("alert").to_owned();
            let accept = matches!(kind.as_str(), "alert" | "beforeunload");
            let message = p["message"].as_str().unwrap_or_default();
            st.notes.push(format!(
                "a JavaScript {kind} dialog ({message:?}) was {}",
                if accept { "accepted" } else { "dismissed" }
            ));
            if let Some(session) = st.session.clone() {
                let id = shared.next_id.get();
                shared.next_id.set(id + 1);
                cdp.send(
                    json!({
                        "id": id,
                        "method": "Page.handleJavaScriptDialog",
                        "params": {"accept": accept},
                        "sessionId": session,
                    })
                    .to_string(),
                );
            }
        }
        "Target.detachedFromTarget" | "Target.targetDestroyed" => {
            let gone = p["targetId"].as_str().or_else(|| p["sessionId"].as_str());
            if gone.is_some() && (gone == st.target.as_deref() || gone == st.session.as_deref()) {
                *st = State::default();
            }
        }
        _ => {}
    }
}
