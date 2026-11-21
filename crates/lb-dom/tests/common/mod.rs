//! Shared test helpers.
#![allow(dead_code)]

use std::fmt::Write;

use lb_dom::{Document, HtmlParser, NodeId, NodeType, ParseStep, ns};

/// Tree dump in the html5lib-tests `#document` format.
pub fn dump(doc: &Document) -> String {
    let mut out = String::new();
    for c in doc.children(Document::DOCUMENT) {
        dump_node(doc, c, 0, &mut out);
    }
    out
}

/// Dump of a fragment's children in the same format.
pub fn dump_children(doc: &Document, parent: NodeId) -> String {
    let mut out = String::new();
    for c in doc.children(parent) {
        dump_node(doc, c, 0, &mut out);
    }
    out
}

fn indent(depth: usize) -> String {
    format!("| {}", "  ".repeat(depth))
}

fn dump_node(doc: &Document, id: NodeId, depth: usize, out: &mut String) {
    let pad = indent(depth);
    match doc.node_type(id) {
        NodeType::DocumentType => {
            let d = doc.doctype(id).unwrap();
            if d.public_id.is_empty() && d.system_id.is_empty() {
                writeln!(out, "{pad}<!DOCTYPE {}>", &*d.name).unwrap();
            } else {
                writeln!(
                    out,
                    "{pad}<!DOCTYPE {} \"{}\" \"{}\">",
                    &*d.name, &*d.public_id, &*d.system_id
                )
                .unwrap();
            }
        }
        NodeType::Text => writeln!(out, "{pad}\"{}\"", doc.character_data(id).unwrap()).unwrap(),
        NodeType::Comment => {
            writeln!(out, "{pad}<!-- {} -->", doc.character_data(id).unwrap()).unwrap()
        }
        NodeType::Element => {
            let e = doc.element(id).unwrap();
            let prefix = if e.name.ns == ns!(svg) {
                "svg "
            } else if e.name.ns == ns!(mathml) {
                "math "
            } else {
                ""
            };
            writeln!(out, "{pad}<{prefix}{}>", &*e.name.local).unwrap();
            let mut attrs: Vec<(String, String)> = e
                .attrs
                .iter()
                .map(|a| {
                    let n = if a.name.ns == ns!(xlink) {
                        format!("xlink {}", &*a.name.local)
                    } else if a.name.ns == ns!(xml) {
                        format!("xml {}", &*a.name.local)
                    } else if a.name.ns == ns!(xmlns) {
                        format!("xmlns {}", &*a.name.local)
                    } else {
                        a.name.local.to_string()
                    };
                    (n, a.value.to_string())
                })
                .collect();
            attrs.sort();
            let apad = indent(depth + 1);
            for (n, v) in attrs {
                writeln!(out, "{apad}{n}=\"{v}\"").unwrap();
            }
            if let Some(tc) = doc.template_contents(id) {
                writeln!(out, "{apad}content").unwrap();
                for c in doc.children(tc) {
                    dump_node(doc, c, depth + 2, out);
                }
            }
            for c in doc.children(id) {
                dump_node(doc, c, depth + 1, out);
            }
        }
        NodeType::ProcessingInstruction => {
            let p = doc.processing_instruction(id).unwrap();
            writeln!(out, "{pad}<?{} {}>", &*p.target, &*p.data).unwrap();
        }
        NodeType::Document | NodeType::DocumentFragment => {}
    }
}

/// Parse `chunks` in order, continuing past every script. Returns the
/// document and the scripts in the order the parser yielded them.
pub fn parse_chunks(chunks: &[&str]) -> (Document, Vec<NodeId>) {
    let mut doc = Document::new();
    let mut parser = HtmlParser::new();
    let mut scripts = Vec::new();
    for c in chunks {
        let mut step = parser.feed_str(&mut doc, c);
        while let ParseStep::Script(s) = step {
            scripts.push(s);
            step = parser.run(&mut doc);
        }
        assert_eq!(step, ParseStep::NeedInput);
    }
    parser.end();
    loop {
        match parser.run(&mut doc) {
            ParseStep::Script(s) => scripts.push(s),
            ParseStep::Done => break,
            ParseStep::NeedInput => panic!("NeedInput after end"),
            ParseStep::StyleSheet(_) => panic!("style sheet pauses are off"),
            ParseStep::CustomElement(_) => panic!("custom element pauses are off"),
        }
    }
    (doc, scripts)
}

pub fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(path).unwrap()
}
