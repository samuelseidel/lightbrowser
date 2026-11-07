//! The tool list: names, descriptions and JSON Schemas (`tools/list`), and
//! the argument checks `tools/call` makes before a tool runs.

use serde_json::{Value, json};

/// One tool's result: text, or an image, or an error text.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Text(String),
    Image { data: String, mime: &'static str },
}

/// A finished call: content blocks plus the `isError` flag.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub content: Vec<Output>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn text(s: impl Into<String>) -> ToolResult {
        ToolResult {
            content: vec![Output::Text(s.into())],
            is_error: false,
        }
    }

    pub fn error(s: impl Into<String>) -> ToolResult {
        ToolResult {
            content: vec![Output::Text(s.into())],
            is_error: true,
        }
    }

    pub fn image(data: String) -> ToolResult {
        ToolResult {
            content: vec![Output::Image {
                data,
                mime: "image/png",
            }],
            is_error: false,
        }
    }

    pub fn to_json(&self) -> Value {
        let content: Vec<Value> = self
            .content
            .iter()
            .map(|c| match c {
                Output::Text(t) => json!({"type": "text", "text": t}),
                Output::Image { data, mime } => {
                    json!({"type": "image", "data": data, "mimeType": mime})
                }
            })
            .collect();
        json!({"content": content, "isError": self.is_error})
    }
}

/// Every tool, as `tools/list` returns them.
pub fn list() -> Value {
    let obj = |props: Value, required: &[&str]| {
        json!({
            "type": "object",
            "properties": props,
            "required": required,
            "additionalProperties": false,
        })
    };
    json!([
        {
            "name": "browser_navigate",
            "description": "Open a URL in the browser and wait for it to load. Returns the page title, the final URL after redirects and the HTTP status. Only loopback, localhost, *.localhost and *.test are allowed unless the server was started with --allow-host.",
            "inputSchema": obj(json!({
                "url": {"type": "string", "description": "The URL to open (http, https or about:blank)."},
                "wait_until": {
                    "type": "string",
                    "enum": ["load", "domcontentloaded", "networkidle"],
                    "description": "When the navigation counts as done (default load)."
                }
            }), &["url"]),
        },
        {
            "name": "browser_snapshot",
            "description": "Return the page's accessibility tree as indented text: one node per line with role, name, state flags and a ref such as [ref=e12] on interactive and landmark nodes. Pass those refs to browser_click and browser_type. Refs stay valid until the next navigation. A long page is returned in parts of about 100 KB; the end of a part says which start_line continues it. Content of iframes is not included.",
            "inputSchema": obj(json!({
                "start_line": {"type": "integer", "minimum": 1, "description": "First node line to return (default 1); use the value a cut-off snapshot names to read the rest."}
            }), &[]),
        },
        {
            "name": "browser_click",
            "description": "Click an element by its snapshot ref with a trusted mouse click at the centre of the element (scrolled into view first).",
            "inputSchema": obj(json!({
                "ref": {"type": "string", "description": "Element ref from browser_snapshot, such as e12."}
            }), &["ref"]),
        },
        {
            "name": "browser_type",
            "description": "Focus a text field, textarea or contenteditable element by its snapshot ref and type text into it with trusted key events, replacing its current content. Fails for elements that cannot take text (disabled, read-only, links, buttons, checkboxes, selects). Optionally press Enter afterwards.",
            "inputSchema": obj(json!({
                "ref": {"type": "string", "description": "Element ref from browser_snapshot, such as e3."},
                "text": {"type": "string", "description": "The text to type."},
                "submit": {"type": "boolean", "description": "Press Enter after typing (default false)."}
            }), &["ref", "text"]),
        },
        {
            "name": "browser_press_key",
            "description": "Press a key or key combination on the focused element, for example Enter, Tab, ArrowDown, Escape, a or Control+a.",
            "inputSchema": obj(json!({
                "key": {"type": "string", "description": "Key name or combination such as Enter, Shift+Tab, Control+a."}
            }), &["key"]),
        },
        {
            "name": "browser_evaluate",
            "description": "Evaluate a JavaScript expression in the page (an isolated world: it sees the DOM but not the page's own globals) and return the JSON result. Promises are awaited.",
            "inputSchema": obj(json!({
                "expression": {"type": "string", "description": "JavaScript expression, for example document.title."}
            }), &["expression"]),
        },
        {
            "name": "browser_screenshot",
            "description": "Take a PNG screenshot of the page viewport and return it as an image.",
            "inputSchema": obj(json!({}), &[]),
        },
        {
            "name": "browser_extract",
            "description": "Return the page content: its visible text, or its HTML after scripts ran.",
            "inputSchema": obj(json!({
                // EXTRACT-HOOK: the extract package adds "markdown" here and in
                // `Driver::extract`; until it is merged the formats are text and html.
                "format": {"type": "string", "enum": ["text", "html"], "description": "text or html."}
            }), &["format"]),
        },
        {
            "name": "browser_close",
            "description": "Close the page and forget its state. The next tool call opens a fresh blank page.",
            "inputSchema": obj(json!({}), &[]),
        },
    ])
}

/// Names of the tools, in list order.
pub fn names() -> Vec<String> {
    list()
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// A required string argument.
pub fn str_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    match args.get(name) {
        Some(Value::String(s)) => Ok(s),
        Some(_) => Err(format!("argument {name:?} must be a string")),
        None => Err(format!("missing required argument {name:?}")),
    }
}

/// An optional positive integer argument.
pub fn uint_arg(args: &Value, name: &str) -> Result<Option<usize>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .filter(|n| *n >= 1)
            .map(|n| Some(n as usize))
            .ok_or_else(|| format!("argument {name:?} must be an integer of at least 1")),
    }
}

/// An optional boolean argument.
pub fn bool_arg(args: &Value, name: &str) -> Result<bool, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("argument {name:?} must be a boolean")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_an_object_schema() {
        let tools = list();
        let tools = tools.as_array().unwrap();
        assert_eq!(tools.len(), 9);
        for t in tools {
            assert!(t["name"].as_str().unwrap().starts_with("browser_"));
            assert!(!t["description"].as_str().unwrap().is_empty());
            let s = &t["inputSchema"];
            assert_eq!(s["type"], "object");
            assert!(s["properties"].is_object());
            for r in s["required"].as_array().unwrap() {
                assert!(s["properties"].get(r.as_str().unwrap()).is_some());
            }
        }
    }

    #[test]
    fn argument_helpers() {
        let a = json!({"s": "x", "b": true, "n": 1});
        assert_eq!(str_arg(&a, "s"), Ok("x"));
        assert!(str_arg(&a, "n").is_err());
        assert!(str_arg(&a, "missing").unwrap_err().contains("missing"));
        assert_eq!(bool_arg(&a, "b"), Ok(true));
        assert_eq!(bool_arg(&a, "none"), Ok(false));
        assert!(bool_arg(&a, "s").is_err());
        let n = json!({"a": 3, "z": 0, "f": 1.5, "s": "2"});
        assert_eq!(uint_arg(&n, "a"), Ok(Some(3)));
        assert_eq!(uint_arg(&n, "none"), Ok(None));
        assert!(uint_arg(&n, "z").is_err());
        assert!(uint_arg(&n, "f").is_err());
        assert!(uint_arg(&n, "s").is_err());
    }

    #[test]
    fn results_serialize() {
        let r = ToolResult::error("bad").to_json();
        assert_eq!(r["isError"], true);
        assert_eq!(r["content"][0]["text"], "bad");
        let i = ToolResult::image("AAAA".into()).to_json();
        assert_eq!(i["content"][0]["type"], "image");
        assert_eq!(i["content"][0]["mimeType"], "image/png");
    }
}
