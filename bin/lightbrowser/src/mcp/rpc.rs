//! JSON-RPC 2.0 and the MCP lifecycle: `initialize` (protocol version
//! negotiation), `notifications/initialized`, `ping`, `tools/list` and
//! `tools/call`. Messages are one JSON text per line; this module turns one
//! incoming line into the line to send back, if any.

use serde_json::{Value, json};

use super::tools::{self, ToolResult};

/// Protocol versions we speak, newest first.
pub const SUPPORTED_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Runs tools. `lightbrowser mcp` implements it with the browser driver;
/// the tests use a stub.
pub trait ToolHost {
    /// Runs the tool `name` (a name from [`tools::names`]) with `args`
    /// (a JSON object). Failures are results with `is_error`.
    async fn call(&self, name: &str, args: Value) -> ToolResult;
}

/// The version to answer an `initialize` with: the client's own when we
/// speak it (the newest of them if it offered several), else our newest.
pub fn negotiate(offered: &Value) -> &'static str {
    let offers: Vec<&str> = match offered {
        Value::String(s) => vec![s.as_str()],
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    SUPPORTED_VERSIONS
        .iter()
        .find(|v| offers.contains(v))
        .copied()
        .unwrap_or(SUPPORTED_VERSIONS[0])
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

fn result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// Handles one line. `None` when nothing is to be sent (a notification, a
/// response from the client, a blank line).
pub async fn handle_line<H: ToolHost>(host: &H, line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let reply = match serde_json::from_str::<Value>(line) {
        Err(e) => Some(error(Value::Null, PARSE_ERROR, format!("parse error: {e}"))),
        Ok(Value::Array(items)) => {
            if items.is_empty() {
                Some(error(Value::Null, INVALID_REQUEST, "empty batch"))
            } else {
                let mut out = Vec::new();
                for item in items {
                    out.extend(handle_value(host, item).await);
                }
                (!out.is_empty()).then_some(Value::Array(out))
            }
        }
        Ok(v) => handle_value(host, v).await,
    };
    reply.map(|v| v.to_string())
}

async fn handle_value<H: ToolHost>(host: &H, msg: Value) -> Option<Value> {
    let Some(obj) = msg.as_object() else {
        return Some(error(
            Value::Null,
            INVALID_REQUEST,
            "a request must be an object",
        ));
    };
    let id = obj.get("id").cloned();
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        // A response to something we never asked, or garbage.
        return match (&id, obj.contains_key("result") || obj.contains_key("error")) {
            (_, true) => None,
            (Some(id), false) => Some(error(id.clone(), INVALID_REQUEST, "missing method")),
            (None, false) => Some(error(Value::Null, INVALID_REQUEST, "missing method")),
        };
    };
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = id else {
        // Notifications (initialized, cancelled, ...) need no answer.
        return None;
    };
    Some(match method {
        "initialize" => match &params {
            Value::Object(p) if p.get("protocolVersion").is_some_and(|v| !v.is_null()) => {
                let version = negotiate(&p["protocolVersion"]);
                result(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {
                            "name": "lightbrowser",
                            "version": env!("CARGO_PKG_VERSION"),
                        },
                        "instructions": "Drives LightBrowser, a headless browser for internal bot-detection validation. Call browser_navigate, then browser_snapshot to get element refs for browser_click and browser_type. Only loopback and test hosts are reachable unless the server was started with --allow-host.",
                    }),
                )
            }
            _ => error(
                id,
                INVALID_PARAMS,
                "initialize needs params with a protocolVersion",
            ),
        },
        "ping" => result(id, json!({})),
        "tools/list" => result(id, json!({"tools": tools::list()})),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(error(id, INVALID_PARAMS, "tools/call needs a tool name"));
            };
            let args = match params.get("arguments") {
                None | Some(Value::Null) => json!({}),
                Some(a @ Value::Object(_)) => a.clone(),
                Some(_) => {
                    return Some(error(id, INVALID_PARAMS, "arguments must be an object"));
                }
            };
            if !tools::names().iter().any(|n| n == name) {
                return Some(error(id, INVALID_PARAMS, format!("unknown tool: {name}")));
            }
            result(id, host.call(name, args).await.to_json())
        }
        other => error(id, METHOD_NOT_FOUND, format!("method not found: {other}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;
    impl ToolHost for Echo {
        async fn call(&self, name: &str, args: Value) -> ToolResult {
            if name == "browser_close" {
                ToolResult::error("nope")
            } else {
                ToolResult::text(format!("{name} {args}"))
            }
        }
    }

    async fn ask(line: &str) -> Value {
        let out = handle_line(&Echo, line).await.expect("a reply");
        assert!(!out.contains('\n'), "one line");
        serde_json::from_str(&out).unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn initialize_negotiates_each_version() {
        for v in SUPPORTED_VERSIONS {
            let r = ask(&format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"{v}","capabilities":{{}},"clientInfo":{{"name":"t","version":"1"}}}}}}"#
            ))
            .await;
            assert_eq!(r["result"]["protocolVersion"], v);
            assert_eq!(r["result"]["serverInfo"]["name"], "lightbrowser");
            assert_eq!(
                r["result"]["serverInfo"]["version"],
                env!("CARGO_PKG_VERSION")
            );
            assert_eq!(r["result"]["capabilities"]["tools"]["listChanged"], false);
            assert_eq!(r["id"], 1);
        }
        let r = ask(r#"{"jsonrpc":"2.0","id":"a","method":"initialize","params":{"protocolVersion":"2030-01-01"}}"#).await;
        assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(r["id"], "a");
        assert_eq!(
            negotiate(&json!(["2030-01-01", "2025-03-26", "2024-11-05"])),
            "2025-03-26"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn errors_use_the_standard_codes() {
        let r = ask(r#"{"jsonrpc":"2.0","id":2,"method":"resources/list"}"#).await;
        assert_eq!(r["error"]["code"], -32601);
        let r = ask(r#"{"jsonrpc":"2.0","id":3,"method":"initialize","params":{}}"#).await;
        assert_eq!(r["error"]["code"], -32602);
        let r = ask(r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{}}"#).await;
        assert_eq!(r["error"]["code"], -32602);
        let r =
            ask(r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"nope"}}"#).await;
        assert_eq!(r["error"]["code"], -32602);
        let r = ask(r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"browser_snapshot","arguments":[1]}}"#).await;
        assert_eq!(r["error"]["code"], -32602);
        let r = ask("{not json").await;
        assert_eq!(r["error"]["code"], -32700);
        assert_eq!(r["id"], Value::Null);
        let r = ask("[]").await;
        assert_eq!(r["error"]["code"], -32600);
        let r = ask("7").await;
        assert_eq!(r["error"]["code"], -32600);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn notifications_and_responses_get_no_reply() {
        assert!(
            handle_line(
                &Echo,
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
            )
            .await
            .is_none()
        );
        assert!(
            handle_line(
                &Echo,
                r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#
            )
            .await
            .is_none()
        );
        assert!(
            handle_line(&Echo, r#"{"jsonrpc":"2.0","method":"whatever"}"#)
                .await
                .is_none()
        );
        assert!(
            handle_line(&Echo, r#"{"jsonrpc":"2.0","id":9,"result":{}}"#)
                .await
                .is_none()
        );
        assert!(handle_line(&Echo, "  ").await.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ping_list_and_call() {
        let r = ask(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).await;
        assert_eq!(r["result"], json!({}));
        let r = ask(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).await;
        assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 9);
        let r = ask(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"browser_navigate","arguments":{"url":"x"}}}"#).await;
        assert_eq!(r["result"]["isError"], false);
        assert!(
            r["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("browser_navigate")
        );
        let r = ask(
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"browser_close"}}"#,
        )
        .await;
        assert_eq!(r["result"]["isError"], true);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn batches_answer_requests_only() {
        let out = handle_line(
            &Echo,
            r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/initialized"}]"#,
        )
        .await
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
    }
}
