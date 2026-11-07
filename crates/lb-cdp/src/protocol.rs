//! CDP message framing: incoming commands, responses, errors and events.
//!
//! Messages are JSON text frames. A command is
//! `{"id": N, "method": "Domain.name", "params": {...}, "sessionId": "..."}`;
//! the reply carries the same `id` (and `sessionId`) with either `result` or
//! `error: {code, message}`. Events have `method` and `params` but no `id`.

use serde::Deserialize;
use serde_json::{Map, Value, json};

/// JSON-RPC error codes Chrome uses.
pub mod code {
    /// Malformed JSON.
    pub const PARSE_ERROR: i64 = -32700;
    /// Well-formed JSON that is not a command.
    pub const INVALID_REQUEST: i64 = -32600;
    /// Unknown method: `'Foo.bar' wasn't found`.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Params that do not match the method's schema.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Any failure while running a known method.
    pub const SERVER_ERROR: i64 = -32000;
    /// `sessionId` that is not attached on this connection.
    pub const SESSION_NOT_FOUND: i64 = -32001;
}

/// An error reply. `data` is optional detail (Chrome uses it for param errors).
#[derive(Clone, Debug, PartialEq)]
pub struct CdpError {
    pub code: i64,
    pub message: String,
    pub data: Option<String>,
}

impl CdpError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        CdpError {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// `-32000` with a message: the usual error for a known method.
    pub fn server(message: impl Into<String>) -> Self {
        Self::new(code::SERVER_ERROR, message)
    }

    /// Chrome's reply for a method it does not implement.
    pub fn method_not_found(method: &str) -> Self {
        Self::new(code::METHOD_NOT_FOUND, format!("'{method}' wasn't found"))
    }

    /// Params that failed to deserialize; `detail` goes in `data`.
    pub fn invalid_params(detail: impl Into<String>) -> Self {
        CdpError {
            code: code::INVALID_PARAMS,
            message: "Invalid parameters".into(),
            data: Some(detail.into()),
        }
    }

    pub fn session_not_found() -> Self {
        Self::new(code::SESSION_NOT_FOUND, "Session with given id not found.")
    }

    pub fn to_json(&self) -> Value {
        let mut e = json!({ "code": self.code, "message": self.message });
        if let Some(d) = &self.data {
            e["data"] = Value::String(d.clone());
        }
        e
    }
}

impl std::fmt::Display for CdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for CdpError {}

/// One incoming command.
#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    pub id: i64,
    pub method: String,
    pub params: Value,
    pub session_id: Option<String>,
}

#[derive(Deserialize)]
struct RawCommand {
    id: Option<Value>,
    method: Option<Value>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(rename = "sessionId", default)]
    session_id: Option<String>,
}

/// Why a text frame could not be turned into a [`Command`]. Carries the id
/// and session when they could be read, so the error can still be replied to.
#[derive(Clone, Debug, PartialEq)]
pub struct ParseFailure {
    pub id: Option<i64>,
    pub session_id: Option<String>,
    pub error: CdpError,
}

impl Command {
    /// Parses one text frame. Missing `params` become `{}`.
    pub fn parse(text: &str) -> Result<Command, ParseFailure> {
        let raw: RawCommand = serde_json::from_str(text).map_err(|_| ParseFailure {
            id: None,
            session_id: None,
            error: CdpError::new(code::PARSE_ERROR, "Message must be a valid JSON"),
        })?;
        let fail = |id, msg: &str| ParseFailure {
            id,
            session_id: raw.session_id.clone(),
            error: CdpError::new(code::INVALID_REQUEST, msg),
        };
        let id = match raw.id.as_ref().and_then(Value::as_i64) {
            Some(id) => id,
            None => return Err(fail(None, "Message must have integer 'id' property")),
        };
        let method = match raw.method {
            Some(Value::String(m)) => m,
            _ => return Err(fail(Some(id), "Message must have string 'method' property")),
        };
        let params = match raw.params {
            None | Some(Value::Null) => Value::Object(Map::new()),
            Some(p @ Value::Object(_)) => p,
            Some(_) => {
                return Err(fail(
                    Some(id),
                    "Message has property other than 'id', 'method', 'sessionId', 'params'",
                ));
            }
        };
        Ok(Command {
            id,
            method,
            params,
            session_id: raw.session_id,
        })
    }

    /// `Domain` part of the method.
    pub fn domain(&self) -> &str {
        self.method.split_once('.').map_or("", |(d, _)| d)
    }
}

/// Serializes a successful reply.
pub fn response(id: i64, session_id: Option<&str>, result: Value) -> String {
    let mut m = Map::new();
    m.insert("id".into(), id.into());
    m.insert("result".into(), result);
    if let Some(s) = session_id {
        m.insert("sessionId".into(), s.into());
    }
    Value::Object(m).to_string()
}

/// Serializes an error reply. `id` is omitted when the request had none.
pub fn error_response(id: Option<i64>, session_id: Option<&str>, err: &CdpError) -> String {
    let mut m = Map::new();
    if let Some(id) = id {
        m.insert("id".into(), id.into());
    }
    m.insert("error".into(), err.to_json());
    if let Some(s) = session_id {
        m.insert("sessionId".into(), s.into());
    }
    Value::Object(m).to_string()
}

/// Serializes an event.
pub fn event(method: &str, params: Value, session_id: Option<&str>) -> String {
    let mut m = Map::new();
    m.insert("method".into(), method.into());
    m.insert("params".into(), params);
    if let Some(s) = session_id {
        m.insert("sessionId".into(), s.into());
    }
    Value::Object(m).to_string()
}

/// Deserializes `params` into `T`, mapping failures to `-32602`.
pub fn params<T: serde::de::DeserializeOwned>(params: &Value) -> Result<T, CdpError> {
    T::deserialize(params).map_err(|e| CdpError::invalid_params(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands_and_rejects_bad_frames() {
        let c = Command::parse(r#"{"id":3,"method":"Page.enable","sessionId":"S"}"#).unwrap();
        assert_eq!(c.id, 3);
        assert_eq!(c.domain(), "Page");
        assert_eq!(c.params, json!({}));
        assert_eq!(c.session_id.as_deref(), Some("S"));

        let e = Command::parse("{").unwrap_err();
        assert_eq!(e.error.code, code::PARSE_ERROR);
        let e = Command::parse(r#"{"method":"X.y"}"#).unwrap_err();
        assert_eq!(e.error.code, code::INVALID_REQUEST);
        assert_eq!(e.id, None);
        let e = Command::parse(r#"{"id":1}"#).unwrap_err();
        assert_eq!(e.id, Some(1));
        let e = Command::parse(r#"{"id":1,"method":"X.y","params":[1]}"#).unwrap_err();
        assert_eq!(e.error.code, code::INVALID_REQUEST);
    }

    #[test]
    fn serializes_replies_and_events() {
        let r: Value = serde_json::from_str(&response(1, Some("S"), json!({"a": 1}))).unwrap();
        assert_eq!(r, json!({"id": 1, "result": {"a": 1}, "sessionId": "S"}));
        let r: Value = serde_json::from_str(&error_response(
            Some(2),
            None,
            &CdpError::method_not_found("Foo.bar"),
        ))
        .unwrap();
        assert_eq!(
            r,
            json!({"id": 2, "error": {"code": -32601, "message": "'Foo.bar' wasn't found"}})
        );
        let r: Value =
            serde_json::from_str(&event("Page.loadEventFired", json!({}), None)).unwrap();
        assert_eq!(r, json!({"method": "Page.loadEventFired", "params": {}}));
    }
}
