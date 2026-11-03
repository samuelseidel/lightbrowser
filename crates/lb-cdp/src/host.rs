//! The boundary between the CDP server and the browser: [`PageHost`].
//!
//! The server owns everything that is protocol: connections, sessions,
//! target bookkeeping, which domains are enabled, event routing, isolated
//! world policy and persona checks for `Emulation.*`. The host owns pages:
//! documents, realms, JS values, layout, input and network. `lb-page`
//! implements this trait; [`crate::mock::MockHost`] is a small in-crate
//! implementation for tests.
//!
//! Threading: the server and the host run on one thread (a tokio
//! `current_thread` runtime inside a `LocalSet`). Host futures need not be
//! `Send`. Methods take `&self`; use interior mutability.
//!
//! Events: the host reports what happens in a page (navigation, lifecycle,
//! execution contexts, console, network) through the [`EventSink`] it was
//! given. Events sent during a host call reach the client before that call's
//! reply.
//!
//! What Playwright relies on (see [`crate::mock::MockHost`] for a working
//! example):
//! - `create_page` reports the `about:blank` main-world realm
//!   (`ExecutionContextCreated`) and its frame (`FrameNavigated`).
//! - A navigation commit sends, in order: `ExecutionContextDestroyed` for
//!   each old realm, `ExecutionContextsCleared`, `FrameNavigated` (with the
//!   loader id `navigate` returned), `ExecutionContextCreated` for the main
//!   world and for every world named in `add_script_to_evaluate_on_new_document`,
//!   then `Lifecycle` `init`, `DOMContentLoaded`, `load`, and
//!   `FrameStoppedLoading`. The document request uses the loader id as its
//!   request id and `resourceType` `Document`, so `page.goto` gets a response.
//! - Object ids must identify their realm: `call_function_on` with an object
//!   runs in that object's realm.
//! - Errors Playwright matches by text: "Node does not have a layout object",
//!   "Node is detached from document", "Cannot find context with specified id".
//! - Never install anything in the main world for CDP's sake. Bindings go to
//!   isolated worlds only ([`BindingScope`]).

use std::sync::Arc;

use lb_persona::{BrowserFamily, PersonaProfile};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::types::*;

/// A failed host call. Becomes a `-32000` CDP error with this message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostError(pub String);

impl HostError {
    pub fn new(msg: impl Into<String>) -> Self {
        HostError(msg.into())
    }

    /// The standard error for something the host does not support.
    pub fn unsupported(what: &str) -> Self {
        HostError(format!("{what} is not supported"))
    }
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HostError {}

pub type HostResult<T> = Result<T, HostError>;

/// Options for a new browser context (`Target.createBrowserContext`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContextOptions {
    /// The default context, created at startup.
    pub is_default: bool,
    pub proxy_server: Option<String>,
    pub proxy_bypass_list: Option<String>,
}

/// `Runtime.evaluate` request.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EvaluateRequest {
    /// The realm. The server resolves a missing `contextId` to the main
    /// world of the page's main frame.
    pub context_id: ExecutionContextId,
    pub expression: String,
    pub object_group: Option<String>,
    pub return_by_value: bool,
    pub await_promise: bool,
    pub generate_preview: bool,
    pub user_gesture: bool,
    pub silent: bool,
    /// Console command-line API (`$`, `$$`, ...). Hosts should ignore it in
    /// the main world: it would be visible to the page.
    pub include_command_line_api: bool,
}

/// What a function is called on.
#[derive(Clone, Debug, PartialEq)]
pub enum CallTarget {
    /// `this` is this object; the realm is the object's.
    Object(RemoteObjectId),
    /// `this` is `undefined` in this realm.
    Context(ExecutionContextId),
}

/// `Runtime.callFunctionOn` request.
#[derive(Clone, Debug, PartialEq)]
pub struct CallFunctionRequest {
    pub function_declaration: String,
    pub target: CallTarget,
    pub arguments: Vec<CallArgument>,
    pub object_group: Option<String>,
    pub return_by_value: bool,
    pub await_promise: bool,
    pub generate_preview: bool,
    pub user_gesture: bool,
    pub silent: bool,
}

/// `Runtime.getProperties` request.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GetPropertiesRequest {
    pub object_id: RemoteObjectId,
    pub own_properties: bool,
    pub accessor_properties_only: bool,
    pub generate_preview: bool,
    pub non_indexed_properties_only: bool,
}

/// Which realms a `Runtime.addBinding` binding is installed in. Bindings are
/// never installed in a main world: that would be visible to the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingScope {
    /// Every isolated world of the page, now and after navigations.
    AllIsolatedWorlds,
    /// Isolated worlds with this name.
    World(String),
    /// One isolated realm.
    Context(ExecutionContextId),
}

/// `Page.navigate` request.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NavigateRequest {
    pub url: String,
    pub referrer: Option<String>,
    pub referrer_policy: Option<String>,
    /// Defaults to the main frame.
    pub frame_id: Option<FrameId>,
    pub transition_type: Option<String>,
}

/// One accepted `Emulation.setDeviceMetricsOverride`, already checked
/// against the persona.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewportOverride {
    pub width: u32,
    pub height: u32,
}

/// Something that happened in a page.
#[derive(Clone, Debug, PartialEq)]
pub enum PageEvent {
    FrameAttached {
        frame_id: FrameId,
        parent_frame_id: FrameId,
    },
    FrameDetached {
        frame_id: FrameId,
    },
    FrameStartedLoading {
        frame_id: FrameId,
    },
    FrameStoppedLoading {
        frame_id: FrameId,
    },
    /// A new document committed in the frame.
    FrameNavigated {
        frame: Frame,
    },
    NavigatedWithinDocument {
        frame_id: FrameId,
        url: String,
    },
    /// `init`, `DOMContentLoaded`, `load`, `networkAlmostIdle`,
    /// `networkIdle`, ... The server also sends
    /// `Page.domContentEventFired` / `Page.loadEventFired` for the main frame.
    Lifecycle {
        frame_id: FrameId,
        loader_id: LoaderId,
        name: String,
        /// Monotonic seconds.
        timestamp: f64,
    },
    /// The page title changed (updates `TargetInfo.title`).
    TitleChanged {
        title: String,
    },
    ExecutionContextCreated {
        context: ExecutionContext,
    },
    ExecutionContextDestroyed {
        context_id: ExecutionContextId,
    },
    /// All realms of the page went away (before a new document commits).
    ExecutionContextsCleared,
    ConsoleApiCalled {
        /// `log`, `debug`, `info`, `error`, `warning`, `dir`, `table`, ...
        kind: String,
        args: Vec<RemoteObject>,
        context_id: ExecutionContextId,
        /// Milliseconds since the epoch.
        timestamp: f64,
        stack_trace: Option<StackTrace>,
    },
    ExceptionThrown {
        /// Milliseconds since the epoch.
        timestamp: f64,
        details: ExceptionDetails,
    },
    /// A binding added with `Runtime.addBinding` was called.
    BindingCalled {
        name: String,
        payload: String,
        context_id: ExecutionContextId,
    },
    JavascriptDialogOpening {
        url: String,
        message: String,
        /// `alert`, `confirm`, `prompt`, `beforeunload`.
        kind: String,
        default_prompt: Option<String>,
    },
    RequestWillBeSent(Box<RequestWillBeSent>),
    ResponseReceived(Box<ResponseReceived>),
    LoadingFinished(LoadingFinished),
    LoadingFailed(LoadingFailed),
    /// A request matched the interception patterns and waits for
    /// `continue_request`, `fulfill_request` or `fail_request`.
    RequestPaused(Box<RequestPaused>),
    /// Any other event, sent as-is to sessions of the page that enabled
    /// `method`'s domain.
    Custom {
        method: String,
        params: Value,
    },
}

/// A page event and the page it belongs to.
#[derive(Clone, Debug, PartialEq)]
pub struct HostEvent {
    pub target_id: TargetId,
    pub event: PageEvent,
}

/// Where a host sends [`HostEvent`]s. Cheap to clone; `Send`.
#[derive(Clone, Debug)]
pub struct EventSink {
    pub(crate) tx: mpsc::UnboundedSender<crate::server::Outgoing>,
}

impl EventSink {
    /// Reports `event` for page `target_id`. Dropped silently once the
    /// server has stopped.
    pub fn emit(&self, target_id: &str, event: PageEvent) {
        let _ = self
            .tx
            .send(crate::server::Outgoing::Host(Box::new(HostEvent {
                target_id: target_id.to_string(),
                event,
            })));
    }
}

/// The browser behind the CDP server. Every method has a default that
/// returns "not supported", so a host can grow one domain at a time.
///
/// Ids: the server picks browser-context and target ids and passes them in.
/// The main frame of a page has the target's id. Frame ids of child frames,
/// loader ids, execution-context ids, object ids and backend node ids are
/// the host's.
#[allow(async_fn_in_trait)] // Futures are deliberately not Send (single-thread host).
pub trait PageHost {
    /// The engine family compiled into this build.
    fn family(&self) -> BrowserFamily;

    /// Called once by the server before anything else.
    fn set_event_sink(&self, sink: EventSink);

    /// The persona of a browser context. Every fingerprint answer and every
    /// `Emulation.*` check uses it.
    fn persona(&self, context: &str) -> Arc<PersonaProfile>;

    /// Replaces a context's persona (`LightBrowser.setPersona`). The server
    /// has already checked the family and the consistency rules, and that
    /// the context has no pages.
    fn set_persona(&self, _context: &str, _persona: Arc<PersonaProfile>) -> HostResult<()> {
        Err(HostError::unsupported("LightBrowser.setPersona"))
    }

    fn create_context(&self, id: &str, opts: ContextOptions) -> HostResult<()>;

    /// Drops the context's cookie jar, storage and cache. Its pages are
    /// already closed.
    fn dispose_context(&self, id: &str) -> HostResult<()>;

    /// Creates a page showing `about:blank` in `context`. The main-world
    /// realm should be reported with `ExecutionContextCreated` and the
    /// initial frame with `FrameNavigated`.
    async fn create_page(&self, context: &str, target_id: &str) -> HostResult<()>;

    /// Closes a page and frees its document and realms.
    fn close_page(&self, target_id: &str) -> HostResult<()>;

    // ---- Page ----

    /// Starts a navigation. Resolves once the new document has committed
    /// (or failed); load events follow as [`PageEvent`]s.
    async fn navigate(&self, target_id: &str, req: NavigateRequest) -> HostResult<NavigateResult>;

    async fn reload(&self, target_id: &str, ignore_cache: bool) -> HostResult<()>;

    fn frame_tree(&self, target_id: &str) -> HostResult<FrameTree>;

    /// `LightBrowser.fingerprintReport`: the live fingerprint surface of the
    /// page's current document compared with its persona (JSON report with
    /// `ok`, `summary` and `checks`). Collected in an isolated world only.
    async fn fingerprint_report(&self, _target_id: &str) -> HostResult<Value> {
        Err(HostError::unsupported("LightBrowser.fingerprintReport"))
    }

    /// Creates an isolated world in `frame_id` and returns its realm. The
    /// realm is also reported with `ExecutionContextCreated`.
    fn create_isolated_world(
        &self,
        _target_id: &str,
        _frame_id: &str,
        _world_name: &str,
        _grant_universal_access: bool,
    ) -> HostResult<ExecutionContextId> {
        Err(HostError::unsupported("Page.createIsolatedWorld"))
    }

    /// Registers a script to run in every new document, before its own
    /// scripts. With `world_name`, the host creates that isolated world in
    /// every new document (even for an empty script) and runs the script
    /// there; that is how Playwright keeps its utility world.
    fn add_script_to_evaluate_on_new_document(
        &self,
        _target_id: &str,
        _source: &str,
        _world_name: Option<&str>,
        _run_immediately: bool,
    ) -> HostResult<String> {
        Err(HostError::unsupported(
            "Page.addScriptToEvaluateOnNewDocument",
        ))
    }

    fn remove_script_to_evaluate_on_new_document(
        &self,
        _target_id: &str,
        _identifier: &str,
    ) -> HostResult<()> {
        Ok(())
    }

    /// Optional; the default is the error Playwright expects to handle.
    fn capture_screenshot(&self, _target_id: &str, _params: &Value) -> HostResult<String> {
        Err(HostError::unsupported("Page.captureScreenshot"))
    }

    fn handle_javascript_dialog(
        &self,
        _target_id: &str,
        _accept: bool,
        _prompt_text: Option<&str>,
    ) -> HostResult<()> {
        Err(HostError::new("No dialog is showing"))
    }

    // ---- Runtime ----

    async fn evaluate(&self, target_id: &str, req: EvaluateRequest) -> HostResult<EvalResult>;

    async fn call_function_on(
        &self,
        target_id: &str,
        req: CallFunctionRequest,
    ) -> HostResult<EvalResult>;

    fn get_properties(
        &self,
        _target_id: &str,
        _req: GetPropertiesRequest,
    ) -> HostResult<Properties> {
        Err(HostError::unsupported("Runtime.getProperties"))
    }

    fn release_object(&self, _target_id: &str, _object_id: &str) -> HostResult<()> {
        Ok(())
    }

    fn release_object_group(&self, _target_id: &str, _group: &str) -> HostResult<()> {
        Ok(())
    }

    /// Installs a binding function. Calls are reported with `BindingCalled`.
    fn add_binding(&self, _target_id: &str, _name: &str, _scope: BindingScope) -> HostResult<()> {
        Err(HostError::unsupported("Runtime.addBinding"))
    }

    // ---- DOM ----

    /// The document node with `depth` levels of children (`-1` = all).
    fn get_document(&self, _target_id: &str, _depth: i32) -> HostResult<DomNode> {
        Err(HostError::unsupported("DOM.getDocument"))
    }

    fn describe_node(&self, _target_id: &str, _node: &NodeRef, _depth: i32) -> HostResult<DomNode> {
        Err(HostError::unsupported("DOM.describeNode"))
    }

    /// A remote object for a node, in `context` (default: main world).
    fn resolve_node(
        &self,
        _target_id: &str,
        _node: BackendNodeId,
        _context: Option<ExecutionContextId>,
        _object_group: Option<&str>,
    ) -> HostResult<RemoteObject> {
        Err(HostError::unsupported("DOM.resolveNode"))
    }

    /// The node behind a remote object.
    fn request_node(&self, _target_id: &str, _object_id: &str) -> HostResult<BackendNodeId> {
        Err(HostError::unsupported("DOM.requestNode"))
    }

    fn query_selector(
        &self,
        _target_id: &str,
        _node: BackendNodeId,
        _selector: &str,
    ) -> HostResult<Option<BackendNodeId>> {
        Err(HostError::unsupported("DOM.querySelector"))
    }

    fn query_selector_all(
        &self,
        _target_id: &str,
        _node: BackendNodeId,
        _selector: &str,
    ) -> HostResult<Vec<BackendNodeId>> {
        Err(HostError::unsupported("DOM.querySelectorAll"))
    }

    fn outer_html(&self, _target_id: &str, _node: &NodeRef) -> HostResult<String> {
        Err(HostError::unsupported("DOM.getOuterHTML"))
    }

    /// Errors with "Could not compute box model." when the node has no box.
    fn box_model(&self, _target_id: &str, _node: &NodeRef) -> HostResult<BoxModel> {
        Err(HostError::unsupported("DOM.getBoxModel"))
    }

    fn content_quads(&self, _target_id: &str, _node: &NodeRef) -> HostResult<Vec<Quad>> {
        Err(HostError::unsupported("DOM.getContentQuads"))
    }

    /// Errors with "Node does not have a layout object" or "Node is
    /// detached from document" like Chrome (Playwright matches the text).
    fn scroll_into_view_if_needed(
        &self,
        _target_id: &str,
        _node: &NodeRef,
        _rect: Option<Rect>,
    ) -> HostResult<()> {
        Err(HostError::unsupported("DOM.scrollIntoViewIfNeeded"))
    }

    fn set_file_input_files(
        &self,
        _target_id: &str,
        _node: &NodeRef,
        _files: Vec<String>,
    ) -> HostResult<()> {
        Err(HostError::unsupported("DOM.setFileInputFiles"))
    }

    // ---- Input ----

    async fn dispatch_mouse_event(&self, _target_id: &str, _ev: MouseEvent) -> HostResult<()> {
        Err(HostError::unsupported("Input.dispatchMouseEvent"))
    }

    async fn dispatch_key_event(&self, _target_id: &str, _ev: KeyEvent) -> HostResult<()> {
        Err(HostError::unsupported("Input.dispatchKeyEvent"))
    }

    async fn insert_text(&self, _target_id: &str, _text: &str) -> HostResult<()> {
        Err(HostError::unsupported("Input.insertText"))
    }

    // ---- Network / Fetch / Storage ----

    fn set_extra_http_headers(
        &self,
        _target_id: &str,
        _headers: Vec<(String, String)>,
    ) -> HostResult<()> {
        Ok(())
    }

    fn set_cache_disabled(&self, _target_id: &str, _disabled: bool) -> HostResult<()> {
        Ok(())
    }

    /// Cookies of a context; `urls` limits them to cookies sent to those URLs.
    fn cookies(&self, _context: &str, _urls: Option<&[String]>) -> HostResult<Vec<Cookie>> {
        Ok(Vec::new())
    }

    fn set_cookies(&self, _context: &str, _cookies: Vec<CookieParam>) -> HostResult<()> {
        Err(HostError::unsupported("Network.setCookies"))
    }

    fn delete_cookies(&self, _context: &str, _filter: DeleteCookies) -> HostResult<()> {
        Ok(())
    }

    fn clear_cookies(&self, _context: &str) -> HostResult<()> {
        Ok(())
    }

    /// Body of a finished response: `(body, base64_encoded)`.
    fn response_body(&self, _target_id: &str, _request_id: &str) -> HostResult<(String, bool)> {
        Err(HostError::new("No resource with given identifier found"))
    }

    /// `Some(patterns)` turns request interception on, `None` off.
    fn set_request_interception(
        &self,
        _target_id: &str,
        _patterns: Option<Vec<RequestPattern>>,
    ) -> HostResult<()> {
        Err(HostError::unsupported("Fetch.enable"))
    }

    fn continue_request(&self, _target_id: &str, _req: ContinueRequest) -> HostResult<()> {
        Err(HostError::new("Invalid InterceptionId."))
    }

    fn fulfill_request(&self, _target_id: &str, _req: FulfillRequest) -> HostResult<()> {
        Err(HostError::new("Invalid InterceptionId."))
    }

    fn fail_request(&self, _target_id: &str, _request_id: &str, _reason: &str) -> HostResult<()> {
        Err(HostError::new("Invalid InterceptionId."))
    }

    /// `Security.setIgnoreCertificateErrors`. Local test servers only.
    fn set_ignore_certificate_errors(
        &self,
        _target_id: Option<&str>,
        _ignore: bool,
    ) -> HostResult<()> {
        Ok(())
    }

    // ---- Emulation (already validated against the persona) ----

    /// A viewport inside the persona's screen. `None` restores the
    /// persona's own inner size.
    fn set_viewport(
        &self,
        _target_id: &str,
        _viewport: Option<ViewportOverride>,
    ) -> HostResult<()> {
        Ok(())
    }

    fn set_focus_emulation(&self, _target_id: &str, _enabled: bool) -> HostResult<()> {
        Ok(())
    }

    // ---- Accessibility ----

    /// The full tree of the main frame, root first.
    fn ax_tree(&self, _target_id: &str) -> HostResult<Vec<AxNode>> {
        Err(HostError::unsupported("Accessibility.getFullAXTree"))
    }
}
