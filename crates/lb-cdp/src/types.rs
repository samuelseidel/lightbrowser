//! Protocol data types shared by the server and [`crate::PageHost`]
//! implementations. Field names follow the CDP JSON (camelCase); optional
//! fields are left out when `None`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `Target.TargetID`. The main frame of a page has the same id.
pub type TargetId = String;
/// `Browser.BrowserContextID`.
pub type BrowserContextId = String;
/// `Page.FrameId`.
pub type FrameId = String;
/// `Network.LoaderId`.
pub type LoaderId = String;
/// `Network.RequestId` (for documents it equals the loader id).
pub type RequestId = String;
/// `Runtime.RemoteObjectId`.
pub type RemoteObjectId = String;
/// `Runtime.ExecutionContextId`.
pub type ExecutionContextId = i64;
/// `DOM.BackendNodeId`. LightBrowser also uses it as the `DOM.NodeId`.
pub type BackendNodeId = i64;

/// Which JS world a realm belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum World {
    /// The page's own world (`auxData.isDefault = true`).
    Main,
    /// An isolated world with this name (Playwright's utility world).
    Isolated(String),
}

impl World {
    pub fn is_main(&self) -> bool {
        matches!(self, World::Main)
    }
}

/// `Runtime.ExecutionContextDescription`, plus the world it belongs to.
#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionContext {
    pub id: ExecutionContextId,
    pub frame_id: FrameId,
    pub world: World,
    /// e.g. `http://127.0.0.1:8765`, or `://` for `about:blank`.
    pub origin: String,
    /// Globally unique id (`uniqueId`).
    pub unique_id: String,
}

impl ExecutionContext {
    /// The CDP JSON (`context` of `Runtime.executionContextCreated`).
    pub fn to_json(&self) -> Value {
        let (name, aux_type, is_default) = match &self.world {
            World::Main => ("", "default", true),
            World::Isolated(n) => (n.as_str(), "isolated", false),
        };
        serde_json::json!({
            "id": self.id,
            "origin": self.origin,
            "name": name,
            "uniqueId": self.unique_id,
            "auxData": {
                "isDefault": is_default,
                "type": aux_type,
                "frameId": self.frame_id,
            }
        })
    }
}

/// `Runtime.RemoteObject`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteObject {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unserializable_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<RemoteObjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<ObjectPreview>,
}

/// `Runtime.ObjectPreview`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectPreview {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub overflow: bool,
    pub properties: Vec<PropertyPreview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<EntryPreview>>,
}

/// `Runtime.PropertyPreview`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyPreview {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_preview: Option<Box<ObjectPreview>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
}

/// `Runtime.EntryPreview` (Map/Set entries).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryPreview {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<ObjectPreview>,
    pub value: ObjectPreview,
}

/// `Runtime.CallFrame`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallFrame {
    pub function_name: String,
    pub script_id: String,
    pub url: String,
    pub line_number: i64,
    pub column_number: i64,
}

/// `Runtime.StackTrace`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackTrace {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub call_frames: Vec<CallFrame>,
}

/// `Runtime.ExceptionDetails`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExceptionDetails {
    pub exception_id: i64,
    pub text: String,
    pub line_number: i64,
    pub column_number: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_trace: Option<StackTrace>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception: Option<RemoteObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_context_id: Option<ExecutionContextId>,
}

/// `Runtime.CallArgument`: a value, an unserializable value or an object id.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallArgument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unserializable_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_id: Option<RemoteObjectId>,
}

/// Result of `Runtime.evaluate` / `callFunctionOn`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalResult {
    pub result: RemoteObject,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception_details: Option<ExceptionDetails>,
}

/// `Runtime.PropertyDescriptor`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyDescriptor {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<RemoteObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub get: Option<RemoteObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set: Option<RemoteObject>,
    pub configurable: bool,
    pub enumerable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub was_thrown: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_own: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<RemoteObject>,
}

/// `Runtime.InternalPropertyDescriptor`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InternalPropertyDescriptor {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<RemoteObject>,
}

/// Result of `Runtime.getProperties`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Properties {
    pub result: Vec<PropertyDescriptor>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub internal_properties: Vec<InternalPropertyDescriptor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception_details: Option<ExceptionDetails>,
}

/// `Page.Frame`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    pub id: FrameId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<FrameId>,
    pub loader_id: LoaderId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url_fragment: Option<String>,
    pub domain_and_registry: String,
    pub security_origin: String,
    pub mime_type: String,
    pub secure_context_type: String,
    pub cross_origin_isolated_context_type: String,
    pub gated_api_features: Vec<String>,
}

impl Frame {
    /// A frame with Chrome's defaults for `url`: origin, secure-context type
    /// and registrable domain derived from the URL.
    pub fn new(id: FrameId, parent_id: Option<FrameId>, loader_id: LoaderId, url: &str) -> Frame {
        let parsed = url::Url::parse(url).ok();
        let (origin, secure, domain) = match &parsed {
            Some(u) if u.scheme() == "about" || u.scheme() == "data" => {
                ("://".to_string(), "InsecureScheme", String::new())
            }
            Some(u) => {
                let origin = u.origin().ascii_serialization();
                let host = u.host_str().unwrap_or("");
                let loopback = matches!(host, "localhost" | "127.0.0.1" | "[::1]")
                    || host.ends_with(".localhost");
                let secure = if u.scheme() == "https" || u.scheme() == "wss" {
                    "Secure"
                } else if loopback {
                    "SecureLocalhost"
                } else {
                    "InsecureScheme"
                };
                let domain = if host.parse::<std::net::IpAddr>().is_ok()
                    || host.starts_with('[')
                    || host == "localhost"
                {
                    String::new()
                } else {
                    // Last two labels; no public-suffix list here.
                    let labels: Vec<&str> = host.split('.').collect();
                    labels[labels.len().saturating_sub(2)..].join(".")
                };
                (origin, secure, domain)
            }
            None => ("://".to_string(), "InsecureScheme", String::new()),
        };
        let (url_no_frag, fragment) = match url.split_once('#') {
            Some((u, f)) => (u.to_string(), Some(format!("#{f}"))),
            None => (url.to_string(), None),
        };
        Frame {
            id,
            parent_id,
            loader_id,
            name: None,
            url: url_no_frag,
            url_fragment: fragment,
            domain_and_registry: domain,
            security_origin: origin,
            mime_type: "text/html".into(),
            secure_context_type: secure.into(),
            cross_origin_isolated_context_type: "NotIsolated".into(),
            gated_api_features: Vec::new(),
        }
    }
}

/// `Page.FrameTree`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameTree {
    pub frame: Frame,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub child_frames: Vec<FrameTree>,
}

/// Result of a navigation request (`Page.navigate`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigateResult {
    pub frame_id: FrameId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loader_id: Option<LoaderId>,
    /// Set when the navigation failed, e.g. `net::ERR_CONNECTION_REFUSED`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_text: Option<String>,
}

/// `DOM.Node`. `node_id` equals `backend_node_id` in LightBrowser.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomNode {
    pub node_id: BackendNodeId,
    pub backend_node_id: BackendNodeId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<BackendNodeId>,
    pub node_type: u32,
    pub node_name: String,
    pub local_name: String,
    pub node_value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_node_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<DomNode>>,
    /// Flat `[name1, value1, name2, value2, ...]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Vec<String>>,
    #[serde(rename = "documentURL", skip_serializing_if = "Option::is_none")]
    pub document_url: Option<String>,
    #[serde(rename = "baseURL", skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xml_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatibility_mode: Option<String>,
    /// Set on the document node and on `<iframe>` owners.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<FrameId>,
}

/// How a DOM command names its node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeRef {
    /// `nodeId` or `backendNodeId` (the same thing here).
    Backend(BackendNodeId),
    /// A `Runtime.RemoteObjectId` that refers to a node.
    Object(RemoteObjectId),
}

/// Eight numbers: x1,y1 .. x4,y4 clockwise from top-left.
pub type Quad = [f64; 8];

/// `DOM.Rect`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn quad(&self) -> Quad {
        let (x, y, r, b) = (self.x, self.y, self.x + self.width, self.y + self.height);
        [x, y, r, y, r, b, x, b]
    }
}

/// `DOM.BoxModel`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoxModel {
    pub content: Quad,
    pub padding: Quad,
    pub border: Quad,
    pub margin: Quad,
    pub width: f64,
    pub height: f64,
}

/// `Network.Cookie`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    /// Seconds since the epoch; `-1` for session cookies.
    pub expires: f64,
    pub size: i64,
    pub http_only: bool,
    pub secure: bool,
    pub session: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
    pub priority: String,
    pub same_party: bool,
    pub source_scheme: String,
    pub source_port: i64,
}

/// `Network.CookieParam` (`Network.setCookies`, `Storage.setCookies`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieParam {
    pub name: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secure: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
}

/// `Network.deleteCookies` params.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteCookies {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// `Input.dispatchMouseEvent` params.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MouseEvent {
    /// `mousePressed`, `mouseReleased`, `mouseMoved` or `mouseWheel`.
    #[serde(rename = "type")]
    pub kind: String,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub modifiers: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<f64>,
    /// `none`, `left`, `middle`, `right`, `back`, `forward`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buttons: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer_type: Option<String>,
}

/// `Input.dispatchKeyEvent` params.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyEvent {
    /// `keyDown`, `keyUp`, `rawKeyDown` or `char`.
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub modifiers: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unmodified_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows_virtual_key_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_virtual_key_code: Option<i64>,
    #[serde(default)]
    pub auto_repeat: bool,
    #[serde(default)]
    pub is_keypad: bool,
    #[serde(default)]
    pub is_system_key: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
}

/// `Network.Request`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetRequest {
    pub url: String,
    pub method: String,
    pub headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_post_data: Option<bool>,
    pub initial_priority: String,
    pub referrer_policy: String,
}

/// `Network.Response`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetResponse {
    pub url: String,
    pub status: i64,
    pub status_text: String,
    pub headers: BTreeMap<String, String>,
    pub mime_type: String,
    pub charset: String,
    pub connection_reused: bool,
    pub connection_id: i64,
    pub from_disk_cache: bool,
    pub from_service_worker: bool,
    pub from_prefetch_cache: bool,
    pub encoded_data_length: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_ip_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_port: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    pub security_state: String,
}

/// `Network.requestWillBeSent` params.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestWillBeSent {
    pub request_id: RequestId,
    pub loader_id: LoaderId,
    #[serde(rename = "documentURL")]
    pub document_url: String,
    pub request: NetRequest,
    /// Monotonic seconds.
    pub timestamp: f64,
    /// Seconds since the epoch.
    pub wall_time: f64,
    pub initiator: Value,
    pub redirect_has_extra_info: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_response: Option<NetResponse>,
    /// `Document`, `Script`, `XHR`, `Fetch`, `Image`, ...
    #[serde(rename = "type")]
    pub resource_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<FrameId>,
    pub has_user_gesture: bool,
}

/// `Network.responseReceived` params.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseReceived {
    pub request_id: RequestId,
    pub loader_id: LoaderId,
    pub timestamp: f64,
    #[serde(rename = "type")]
    pub resource_type: String,
    pub response: NetResponse,
    pub has_extra_info: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<FrameId>,
}

/// `Network.loadingFinished` params.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadingFinished {
    pub request_id: RequestId,
    pub timestamp: f64,
    pub encoded_data_length: f64,
}

/// `Network.loadingFailed` params.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadingFailed {
    pub request_id: RequestId,
    pub timestamp: f64,
    #[serde(rename = "type")]
    pub resource_type: String,
    pub error_text: String,
    pub canceled: bool,
}

/// `Fetch.requestPaused` params.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPaused {
    /// Interception id, used by `Fetch.continueRequest` and friends.
    pub request_id: String,
    pub request: NetRequest,
    pub frame_id: FrameId,
    pub resource_type: String,
    /// The `Network.RequestId` of the same request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_id: Option<RequestId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_status_code: Option<i64>,
}

/// `Fetch.RequestPattern`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPattern {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_stage: Option<String>,
}

/// `Fetch.HeaderEntry`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HeaderEntry {
    pub name: String,
    pub value: String,
}

/// `Fetch.continueRequest` params.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinueRequest {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// Base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<Vec<HeaderEntry>>,
}

/// `Fetch.fulfillRequest` params.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FulfillRequest {
    pub request_id: String,
    pub response_code: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<Vec<HeaderEntry>>,
    /// Base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_phrase: Option<String>,
}

/// One `Accessibility.AXNode` property (`{name, value: AXValue}`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AxProperty {
    pub name: String,
    pub value: AxValue,
}

/// `Accessibility.AXValue`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AxValue {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

impl AxValue {
    pub fn new(kind: &str, value: Value) -> AxValue {
        AxValue {
            kind: kind.into(),
            value: Some(value),
        }
    }
}

/// `Accessibility.AXNode`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AxNode {
    pub node_id: String,
    pub ignored: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<AxValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<AxValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<AxValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<AxValue>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub properties: Vec<AxProperty>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub child_ids: Vec<String>,
    #[serde(rename = "backendDOMNodeId", skip_serializing_if = "Option::is_none")]
    pub backend_dom_node_id: Option<BackendNodeId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<FrameId>,
}

/// Console call type for `Runtime.consoleAPICalled` (`log`, `info`,
/// `warning`, `error`, `debug`, `table`, ...).
pub type ConsoleType = String;

/// `Target.TargetInfo`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetInfo {
    pub target_id: TargetId,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub url: String,
    pub attached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opener_id: Option<TargetId>,
    pub can_access_opener: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_context_id: Option<BrowserContextId>,
}

/// `LightBrowser.humanMove` params.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanMove {
    pub x: f64,
    pub y: f64,
}

/// `LightBrowser.humanClick` params: a CSS `selector` (resolved in the
/// main document) or a viewport point `x`, `y`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanClick {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    /// `left` (default), `middle` or `right`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button: Option<String>,
    /// Default 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click_count: Option<u32>,
}

/// `LightBrowser.humanType` params.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanType {
    pub text: String,
    /// Clicked first (with `humanClick`) when given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
}

/// `LightBrowser.humanScroll` params.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanScroll {
    #[serde(default)]
    pub delta_y: f64,
    #[serde(default)]
    pub delta_x: f64,
    /// Pointer position for the wheel events (the pointer moves there
    /// first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    /// Scroll until this element is in view (the deltas are then ignored).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
}

/// One `LightBrowser.human*` command.
#[derive(Clone, Debug, PartialEq)]
pub enum HumanCommand {
    Move(HumanMove),
    Click(HumanClick),
    Type(HumanType),
    Scroll(HumanScroll),
}
