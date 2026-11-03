//! `XMLSerializer` (Playwright's `page.content()` uses it for the doctype).
//! HTML elements serialize as their HTML markup with the XHTML namespace
//! on the outermost element, which is what Chrome gives for the common
//! cases; void elements keep HTML syntax.

use lb_dom::{Document, NodeType};
use lb_js::HostRef;

use super::{K_OBJ, Obj, Runtime, arg_node, ctor, method, need_args};
use crate::registry::Registry;

fn doctype_markup(d: &Document, n: lb_dom::NodeId) -> String {
    let Some(dt) = d.doctype(n) else {
        return String::new();
    };
    let mut s = format!("<!DOCTYPE {}", dt.name);
    if !dt.public_id.is_empty() {
        s.push_str(&format!(" PUBLIC \"{}\"", dt.public_id));
    } else if !dt.system_id.is_empty() {
        s.push_str(" SYSTEM");
    }
    if !dt.system_id.is_empty() {
        s.push_str(&format!(" \"{}\"", dt.system_id));
    }
    s.push('>');
    s
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    ctor(reg, rt, "XMLSerializer", |cx, w| {
        let id = w.new_obj(Obj::XmlSerializer);
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    method(reg, rt, "XMLSerializer", "serializeToString", |cx, w| {
        need_args(cx, 1, "serializeToString", "XMLSerializer")?;
        let (slot, n) = arg_node(cx, 0, "serializeToString", "XMLSerializer")?;
        let s = w.doc_slot(slot, |d| match d.node_type(n) {
            NodeType::DocumentType => doctype_markup(d, n),
            NodeType::Document => {
                let mut out = String::new();
                for c in d.children(n) {
                    match d.node_type(c) {
                        NodeType::DocumentType => out.push_str(&doctype_markup(d, c)),
                        NodeType::Element => out.push_str(&with_xmlns(&d.outer_html(c))),
                        _ => out.push_str(&d.outer_html(c)),
                    }
                }
                out
            }
            NodeType::Element => with_xmlns(&d.outer_html(n)),
            _ => d.outer_html(n),
        });
        cx.ret(s)
    });
}

/// Add the XHTML namespace to the first start tag.
fn with_xmlns(html: &str) -> String {
    if !html.starts_with('<') || html[..html.find('>').unwrap_or(0)].contains("xmlns=") {
        return html.to_string();
    }
    let end = html[1..]
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .map_or(html.len(), |i| i + 1);
    format!(
        "{} xmlns=\"http://www.w3.org/1999/xhtml\"{}",
        &html[..end],
        &html[end..]
    )
}
