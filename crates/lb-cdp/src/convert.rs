//! Helpers that turn LightBrowser's DOM, layout and accessibility data into
//! CDP types. Hosts built on `lb-dom` / `lb-layout` (the mock today,
//! `lb-page` next) use them so the CDP shapes live in one place.

use lb_dom::{Document, NodeId, NodeType, QuirksMode};
use lb_layout::{AxTree, Checked};
use serde_json::json;

use crate::types::{AxNode, AxProperty, AxValue, BackendNodeId, BoxModel, DomNode, Quad};

/// Where the document lives, for the document node's fields.
#[derive(Clone, Debug, Default)]
pub struct DocInfo<'a> {
    pub url: &'a str,
    pub frame_id: &'a str,
    /// `pierce`: describe shadow roots' children too.
    pub pierce: bool,
}

/// Chrome's DOM agent hides text nodes that are only whitespace.
fn skipped(doc: &Document, id: NodeId) -> bool {
    doc.node_type(id) == NodeType::Text
        && doc
            .character_data(id)
            .is_some_and(|t| t.chars().all(char::is_whitespace))
}

fn visible_children(doc: &Document, id: NodeId) -> Vec<NodeId> {
    doc.children(id).filter(|c| !skipped(doc, *c)).collect()
}

/// `DOM.Node` for `id` with `depth` levels of children (`-1` = all).
/// `ids` maps arena ids to backend node ids.
pub fn dom_node(
    doc: &Document,
    id: NodeId,
    depth: i32,
    ids: &dyn Fn(NodeId) -> BackendNodeId,
    info: &DocInfo<'_>,
) -> DomNode {
    let backend = ids(id);
    let ty = doc.node_type(id);
    let kids = visible_children(doc, id);
    let mut n = DomNode {
        node_id: backend,
        backend_node_id: backend,
        parent_id: doc.parent(id).map(ids),
        node_type: ty as u32,
        node_name: doc.node_name(id),
        local_name: doc
            .local_name(id)
            .map(|l| l.to_string())
            .unwrap_or_default(),
        node_value: match ty {
            NodeType::Text | NodeType::Comment | NodeType::ProcessingInstruction => {
                doc.character_data(id).unwrap_or("").to_string()
            }
            _ => String::new(),
        },
        ..Default::default()
    };
    match ty {
        NodeType::Element => {
            let mut attrs = Vec::new();
            for a in doc.attributes(id) {
                let name = match &a.name.prefix {
                    Some(p) => format!("{p}:{}", a.name.local),
                    None => a.name.local.to_string(),
                };
                attrs.push(name);
                attrs.push(a.value.to_string());
            }
            n.attributes = Some(attrs);
            n.child_node_count = Some(kids.len() as u32);
        }
        NodeType::Document => {
            n.child_node_count = Some(kids.len() as u32);
            n.document_url = Some(info.url.to_string());
            n.base_url = Some(info.url.to_string());
            n.xml_version = Some(String::new());
            n.compatibility_mode = Some(
                match doc.quirks_mode() {
                    QuirksMode::Quirks => "QuirksMode",
                    QuirksMode::LimitedQuirks => "LimitedQuirksMode",
                    QuirksMode::NoQuirks => "NoQuirksMode",
                }
                .into(),
            );
            n.frame_id = Some(info.frame_id.to_string());
        }
        NodeType::DocumentType => {
            if let Some(d) = doc.doctype(id) {
                n.public_id = Some(d.public_id.to_string());
                n.system_id = Some(d.system_id.to_string());
            }
        }
        NodeType::DocumentFragment => {
            n.child_node_count = Some(kids.len() as u32);
            if let Some(sr) = doc.shadow_root_data(id) {
                n.parent_id = None;
                n.shadow_root_type = Some(
                    match sr.mode {
                        lb_dom::ShadowRootMode::Open => "open",
                        lb_dom::ShadowRootMode::Closed => "closed",
                    }
                    .into(),
                );
            }
        }
        _ => {}
    }
    let backend_node = |x: NodeId| crate::types::BackendNode {
        node_type: doc.node_type(x) as u32,
        node_name: doc.node_name(x),
        backend_node_id: ids(x),
    };
    // Chrome lists a host's shadow roots at every depth; their children
    // only when piercing.
    let in_shadow_root = doc.is_shadow_root(id);
    let describe_kids = depth != 0 && (!in_shadow_root || info.pierce);
    if let Some(sr) = doc.shadow_root(id) {
        let d = if describe_kids && info.pierce {
            depth - 1
        } else {
            0
        };
        n.shadow_roots = Some(vec![dom_node(doc, sr, d, ids, info)]);
    }
    if ty == NodeType::Element || ty == NodeType::Text {
        if let Some(slot) = doc.find_slot(id, false) {
            n.assigned_slot = Some(backend_node(slot));
        }
        if doc.is_slot(id) && doc.is_shadow_root(doc.root(id)) && (info.pierce || depth != 0) {
            let nodes = doc.assigned_nodes(id);
            if !nodes.is_empty() {
                n.distributed_nodes = Some(nodes.into_iter().map(backend_node).collect());
            }
        }
    }
    if describe_kids && !kids.is_empty() {
        n.children = Some(
            kids.into_iter()
                .map(|c| dom_node(doc, c, depth - 1, ids, info))
                .collect(),
        );
    }
    n
}

/// Converts `lb_layout::BoxModel` (CSS px) to `DOM.BoxModel` quads.
pub fn box_model(m: &lb_layout::BoxModel) -> BoxModel {
    let q = |r: &lb_layout::Rect| -> Quad { rect(r).quad() };
    BoxModel {
        content: q(&m.content),
        padding: q(&m.padding),
        border: q(&m.border),
        margin: q(&m.margin),
        width: f64::from(m.border.width).round(),
        height: f64::from(m.border.height).round(),
    }
}

/// `lb_layout::Rect` as a CDP rect.
pub fn rect(r: &lb_layout::Rect) -> crate::types::Rect {
    crate::types::Rect {
        x: f64::from(r.x),
        y: f64::from(r.y),
        width: f64::from(r.width),
        height: f64::from(r.height),
    }
}

/// `Accessibility.AXNode`s for a tree, root first. Node ids are the tree
/// index plus one, as strings.
pub fn ax_nodes(
    tree: &AxTree,
    ids: &dyn Fn(NodeId) -> BackendNodeId,
    frame_id: &str,
) -> Vec<AxNode> {
    let sid = |i: usize| (i + 1).to_string();
    tree.nodes
        .iter()
        .map(|n| {
            let s = &n.states;
            let mut props = Vec::new();
            let mut push = |name: &str, kind: &str, v: serde_json::Value| {
                props.push(AxProperty {
                    name: name.into(),
                    value: AxValue::new(kind, v),
                })
            };
            if s.focusable {
                push("focusable", "booleanOrUndefined", json!(true));
            }
            if s.disabled {
                push("disabled", "boolean", json!(true));
            }
            if let Some(c) = s.checked {
                let v = match c {
                    Checked::True => "true",
                    Checked::False => "false",
                    Checked::Mixed => "mixed",
                };
                push("checked", "tristate", json!(v));
            }
            if let Some(e) = s.expanded {
                push("expanded", "booleanOrUndefined", json!(e));
            }
            if let Some(v) = s.selected {
                push("selected", "booleanOrUndefined", json!(v));
            }
            if let Some(v) = s.pressed {
                push(
                    "pressed",
                    "tristate",
                    json!(if v { "true" } else { "false" }),
                );
            }
            if s.required {
                push("required", "boolean", json!(true));
            }
            if s.readonly {
                push("readonly", "boolean", json!(true));
            }
            if s.multiline {
                push("multiline", "boolean", json!(true));
            }
            if let Some(l) = s.level {
                push("level", "integer", json!(l));
            }
            AxNode {
                node_id: sid(n.id),
                ignored: n.ignored,
                role: Some(AxValue::new("role", json!(n.role))),
                name: Some(AxValue::new("computedString", json!(n.name))),
                description: (!n.description.is_empty())
                    .then(|| AxValue::new("computedString", json!(n.description))),
                value: n.value.as_ref().map(|v| AxValue::new("string", json!(v))),
                properties: props,
                parent_id: n.parent.map(sid),
                child_ids: n.children.iter().map(|c| sid(*c)).collect(),
                backend_dom_node_id: n.dom.map(ids),
                frame_id: n.parent.is_none().then(|| frame_id.to_string()),
            }
        })
        .collect()
}

/// `div#id.a.b`, the description V8 gives an element (`#text` etc. for
/// other nodes).
pub fn node_description(doc: &Document, id: NodeId) -> String {
    match doc.node_type(id) {
        NodeType::Element => {
            let mut s = doc
                .local_name(id)
                .map(|l| l.to_string())
                .unwrap_or_default();
            if let Some(i) = doc.element_id(id).filter(|i| !i.is_empty()) {
                s.push('#');
                s.push_str(i);
            }
            for c in doc.classes(id) {
                s.push('.');
                s.push_str(c);
            }
            s
        }
        _ => doc.node_name(id),
    }
}

/// The JS class of a node (`HTMLDivElement`, `Text`, `HTMLDocument`, ...),
/// for common HTML elements; unknown HTML tags give `HTMLElement`.
pub fn node_class(doc: &Document, id: NodeId) -> String {
    match doc.node_type(id) {
        NodeType::Document => "HTMLDocument".into(),
        NodeType::Text => "Text".into(),
        NodeType::Comment => "Comment".into(),
        NodeType::DocumentType => "DocumentType".into(),
        NodeType::DocumentFragment => "DocumentFragment".into(),
        NodeType::ProcessingInstruction => "ProcessingInstruction".into(),
        NodeType::Element => {
            let tag = doc
                .local_name(id)
                .map(|l| l.to_string())
                .unwrap_or_default();
            let name = match tag.as_str() {
                "a" => "Anchor",
                "body" => "Body",
                "br" => "BR",
                "button" => "Button",
                "div" => "Div",
                "form" => "Form",
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "Heading",
                "head" => "Head",
                "html" => "Html",
                "iframe" => "IFrame",
                "img" => "Image",
                "input" => "Input",
                "label" => "Label",
                "li" => "LI",
                "link" => "Link",
                "meta" => "Meta",
                "ol" => "OList",
                "option" => "Option",
                "p" => "Paragraph",
                "script" => "Script",
                "select" => "Select",
                "span" => "Span",
                "style" => "Style",
                "table" => "Table",
                "textarea" => "TextArea",
                "title" => "Title",
                "ul" => "UList",
                _ => "",
            };
            format!("HTML{name}Element")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dom_node_shapes() {
        let doc = Document::parse_html(
            "<!doctype html><html><head><title>T</title></head><body>\n  <div id=a class='x y'>hi</div>\n</body></html>",
        );
        let ids = |n: NodeId| i64::from(n.0) + 1;
        let info = DocInfo {
            url: "http://127.0.0.1/",
            frame_id: "F",
            pierce: false,
        };
        let root = dom_node(&doc, Document::DOCUMENT, 1, &ids, &info);
        assert_eq!(root.node_type, 9);
        assert_eq!(root.node_name, "#document");
        assert_eq!(root.frame_id.as_deref(), Some("F"));
        assert_eq!(root.compatibility_mode.as_deref(), Some("NoQuirksMode"));
        let kids = root.children.as_ref().unwrap();
        assert_eq!(kids[0].node_type, 10);
        assert_eq!(kids[0].node_name, "html");
        assert_eq!(kids[1].node_name, "HTML");
        assert!(kids[1].children.is_none(), "depth 1 stops at html");

        let full = dom_node(&doc, Document::DOCUMENT, -1, &ids, &info);
        let html = &full.children.as_ref().unwrap()[1];
        let body = &html.children.as_ref().unwrap()[1];
        // Whitespace-only text is hidden, as in Chrome.
        assert_eq!(body.child_node_count, Some(1));
        let div = &body.children.as_ref().unwrap()[0];
        assert_eq!(
            div.attributes.as_ref().unwrap(),
            &["id", "a", "class", "x y"]
        );
        assert_eq!(div.parent_id, Some(body.node_id));

        let div_id = doc.get_element_by_id("a").unwrap();
        assert_eq!(node_description(&doc, div_id), "div#a.x.y");
        assert_eq!(node_class(&doc, div_id), "HTMLDivElement");
    }
}
