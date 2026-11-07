//! The accessibility snapshot as indented text: one node per line with
//! role, name, state flags and, for interactive and landmark nodes, a
//! stable ref (`e12`) that `browser_click` and `browser_type` take.
//!
//! The nodes come from `Accessibility.getFullAXTree`, which lb-cdp builds
//! from `lb_layout::AxTree` (`convert::ax_nodes`), so the roles, names and
//! states are the ones Playwright's `getByRole` sees. Ignored nodes (hidden
//! subtrees, role-less generics) are left out and their children move up.
//! Refs are kept per document: the same DOM node keeps its ref across
//! snapshots until the page navigates ([`Refs::reset`]).

use std::collections::HashMap;

use serde_json::Value;

/// Output above this many bytes is cut (`truncate`).
pub const MAX_OUTPUT: usize = 100 * 1024;

const INTERACTIVE: &[&str] = &[
    "button",
    "link",
    "textbox",
    "searchbox",
    "combobox",
    "checkbox",
    "radio",
    "slider",
    "spinbutton",
    "switch",
    "listbox",
    "option",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "tab",
    "treeitem",
    "DisclosureTriangle",
    "ColorWell",
    "Date",
    "DateTime",
    "Time",
];

const LANDMARKS: &[&str] = &[
    "banner",
    "navigation",
    "main",
    "complementary",
    "contentinfo",
    "region",
    "form",
    "search",
    "dialog",
];

/// Ref to DOM node (CDP backend node id) for the current document.
#[derive(Debug, Default)]
pub struct Refs {
    by_ref: HashMap<String, i64>,
    by_node: HashMap<i64, String>,
    next: u32,
}

impl Refs {
    /// Forget every ref (the page navigated).
    pub fn reset(&mut self) {
        self.by_ref.clear();
        self.by_node.clear();
        self.next = 0;
    }

    /// The ref of `node`, made on first use.
    fn assign(&mut self, node: i64) -> String {
        if let Some(r) = self.by_node.get(&node) {
            return r.clone();
        }
        self.next += 1;
        let r = format!("e{}", self.next);
        self.by_ref.insert(r.clone(), node);
        self.by_node.insert(node, r.clone());
        r
    }

    /// The DOM node a ref names.
    pub fn resolve(&self, r: &str) -> Option<i64> {
        self.by_ref.get(r.trim()).copied()
    }
}

fn text_of(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.get("value"))
        .map(|x| match x {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}

fn prop<'a>(node: &'a Value, name: &str) -> Option<&'a Value> {
    node.get("properties")?
        .as_array()?
        .iter()
        .find(|p| p.get("name").and_then(Value::as_str) == Some(name))?
        .get("value")?
        .get("value")
}

fn quote(s: &str) -> String {
    // A JSON string is a fine quoted form and keeps newlines out of the line.
    Value::String(s.to_string()).to_string()
}

fn flags(node: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(l) = prop(node, "level").and_then(Value::as_i64) {
        out.push(format!("level={l}"));
    }
    if prop(node, "disabled").and_then(Value::as_bool) == Some(true) {
        out.push("disabled".into());
    }
    match prop(node, "checked").and_then(Value::as_str) {
        Some("true") => out.push("checked".into()),
        Some("false") => out.push("unchecked".into()),
        Some("mixed") => out.push("checked=mixed".into()),
        _ => {}
    }
    match prop(node, "expanded").and_then(Value::as_bool) {
        Some(true) => out.push("expanded".into()),
        Some(false) => out.push("collapsed".into()),
        _ => {}
    }
    if prop(node, "selected").and_then(Value::as_bool) == Some(true) {
        out.push("selected".into());
    }
    match prop(node, "pressed").and_then(Value::as_str) {
        Some("true") => out.push("pressed".into()),
        Some("mixed") => out.push("pressed=mixed".into()),
        _ => {}
    }
    for (p, f) in [
        ("required", "required"),
        ("readonly", "readonly"),
        ("multiline", "multiline"),
    ] {
        if prop(node, p).and_then(Value::as_bool) == Some(true) {
            out.push(f.into());
        }
    }
    out
}

/// A text field whose live value the driver reads: the AX tree only knows
/// the `value` attribute, not what was typed since.
#[derive(Debug, PartialEq)]
pub struct Field {
    /// Index of its line in the rendered lines (no `value=` yet).
    pub line: usize,
    pub node: i64,
    /// The attribute value, if reading the live one fails.
    pub fallback: String,
}

/// ` value="..."` for a line.
pub fn value_suffix(v: &str) -> String {
    format!(" value={}", quote(v))
}

const FIELDS: &[&str] = &["textbox", "searchbox", "combobox", "spinbutton", "slider"];

struct Walk<'a> {
    by_id: HashMap<&'a str, &'a Value>,
    refs: &'a mut Refs,
    lines: Vec<String>,
    fields: Vec<Field>,
}

impl Walk<'_> {
    /// `named`: the name of the nearest shown ancestor, to drop text that
    /// only repeats it.
    fn visit(&mut self, node: &Value, depth: usize, named: &str) {
        let ignored = node.get("ignored").and_then(Value::as_bool) == Some(true);
        let kids = |w: &mut Self, depth: usize, named: &str| {
            let ids: Vec<String> = node
                .get("childIds")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            for id in ids {
                if let Some(child) = w.by_id.get(id.as_str()).copied() {
                    w.visit(child, depth, named);
                }
            }
        };
        if ignored {
            kids(self, depth, named);
            return;
        }
        let role = text_of(node, "role");
        let name = text_of(node, "name");
        if role == "StaticText" {
            if !name.is_empty() && !(named.contains(name.as_str())) {
                self.lines
                    .push(format!("{}- text {}", "  ".repeat(depth), quote(&name)));
            }
            return;
        }
        let mut line = format!("{}- {role}", "  ".repeat(depth));
        if !name.is_empty() {
            line.push(' ');
            line.push_str(&quote(&name));
        }
        for f in flags(node) {
            line.push_str(&format!(" [{f}]"));
        }
        let focusable = prop(node, "focusable").and_then(Value::as_bool) == Some(true);
        let backend = node.get("backendDOMNodeId").and_then(Value::as_i64);
        let wants_ref = depth > 0
            && (INTERACTIVE.contains(&role.as_str())
                // `form` and `region` are landmarks only when named.
                || LANDMARKS.contains(&role.as_str())
                    && (!matches!(role.as_str(), "form" | "region") || !name.is_empty())
                || focusable && role != "RootWebArea");
        if let (true, Some(b)) = (wants_ref, backend) {
            let r = self.refs.assign(b);
            line.push_str(&format!(" [ref={r}]"));
        }
        let value = text_of(node, "value");
        match backend {
            Some(b) if FIELDS.contains(&role.as_str()) => self.fields.push(Field {
                line: self.lines.len(),
                node: b,
                fallback: value,
            }),
            _ if !value.is_empty() => line.push_str(&value_suffix(&value)),
            _ => {}
        }
        self.lines.push(line);
        let next_named = if name.is_empty() { named } else { &name };
        let next_named = next_named.to_owned();
        kids(self, depth + 1, &next_named);
    }
}

/// Renders `Accessibility.getFullAXTree` nodes, root first, and assigns
/// refs. Returns the lines and the text fields to complete with their live
/// values.
pub fn render(nodes: &[Value], refs: &mut Refs) -> (Vec<String>, Vec<Field>) {
    let Some(root) = nodes.first() else {
        return (Vec::new(), Vec::new());
    };
    let by_id = nodes
        .iter()
        .filter_map(|n| Some((n.get("nodeId")?.as_str()?, n)))
        .collect();
    let mut w = Walk {
        by_id,
        refs,
        lines: Vec::new(),
        fields: Vec::new(),
    };
    w.visit(root, 0, "");
    (w.lines, w.fields)
}

/// Cuts `s` to [`MAX_OUTPUT`] bytes (on a character boundary) with a note.
pub fn truncate(s: String) -> String {
    truncate_at(s, MAX_OUTPUT)
}

pub fn truncate_at(s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let total = s.len();
    format!(
        "{}\n\n[output truncated: showing the first {cut} of {total} bytes (limit {} KB)]",
        &s[..cut],
        max / 1024
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, role: &str, name: &str, backend: i64, kids: &[&str]) -> Value {
        json!({
            "nodeId": id,
            "ignored": false,
            "role": {"type": "role", "value": role},
            "name": {"type": "computedString", "value": name},
            "childIds": kids,
            "backendDOMNodeId": backend,
            "properties": [],
        })
    }

    fn sample() -> Vec<Value> {
        let mut anon = node("3", "generic", "", 30, &["4"]);
        anon["ignored"] = json!(true);
        let mut input = node("5", "textbox", "Name", 50, &[]);
        input["value"] = json!({"type": "string", "value": "Ada"});
        input["properties"] = json!([
            {"name": "required", "value": {"type": "boolean", "value": true}},
            {"name": "focusable", "value": {"type": "booleanOrUndefined", "value": true}},
        ]);
        let mut cb = node("6", "checkbox", "Agree", 60, &[]);
        cb["properties"] =
            json!([{"name": "checked", "value": {"type": "tristate", "value": "true"}}]);
        let mut h = node("2", "heading", "Hello", 20, &["7"]);
        h["properties"] = json!([{"name": "level", "value": {"type": "integer", "value": 1}}]);
        vec![
            node("1", "RootWebArea", "Title", 10, &["2", "3", "8"]),
            h,
            anon,
            node("4", "button", "Go", 40, &["9"]),
            input,
            cb,
            node("7", "StaticText", "Hello", 21, &[]),
            node("8", "navigation", "Menu", 80, &["5", "6"]),
            node("9", "StaticText", "Go", 41, &[]),
        ]
    }

    #[test]
    fn renders_tree_with_refs_and_flags() {
        let mut refs = Refs::default();
        let (lines, fields) = render(&sample(), &mut refs);
        let out = lines.join("\n");
        assert_eq!(
            fields,
            [Field {
                line: 4,
                node: 50,
                fallback: "Ada".into()
            }]
        );
        let expect = [
            "- RootWebArea \"Title\"",
            "  - heading \"Hello\" [level=1]",
            "  - button \"Go\" [ref=e1]",
            "  - navigation \"Menu\" [ref=e2]",
            "    - textbox \"Name\" [required] [ref=e3]",
            "    - checkbox \"Agree\" [checked] [ref=e4]",
        ]
        .join("\n");
        assert_eq!(out, expect);
        assert_eq!(refs.resolve("e1"), Some(40));
        assert_eq!(refs.resolve("e4"), Some(60));
        assert_eq!(refs.resolve("e9"), None);
        // Stable across snapshots; reset forgets.
        let again = render(&sample(), &mut refs).0.join("\n");
        assert_eq!(again, expect);
        refs.reset();
        assert_eq!(refs.resolve("e1"), None);
    }

    #[test]
    fn hidden_subtrees_leave_nothing() {
        let mut hidden = node("2", "button", "", 20, &["3"]);
        hidden["ignored"] = json!(true);
        let mut text = node("3", "StaticText", "secret", 21, &[]);
        text["ignored"] = json!(true);
        let nodes = vec![node("1", "RootWebArea", "T", 10, &["2"]), hidden, text];
        let out = render(&nodes, &mut Refs::default()).0.join("\n");
        assert_eq!(out, "- RootWebArea \"T\"");
    }

    #[test]
    fn truncation_keeps_char_boundaries_and_says_so() {
        let s = "é".repeat(100);
        let t = truncate_at(s, 51);
        assert!(t.contains("output truncated"));
        assert!(t.starts_with(&"é".repeat(25)));
        let short = truncate_at("abc".into(), 10);
        assert_eq!(short, "abc");
    }
}
