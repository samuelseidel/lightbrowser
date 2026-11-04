//! HTML fragment serialization (`innerHTML` / `outerHTML`) via html5ever's
//! serializer, which implements the spec's escaping and void-element rules.

use std::io;

use html5ever::serialize::{Serialize, SerializeOpts, Serializer, TraversalScope, serialize};

use crate::document::Document;
use crate::node::{NodeData, NodeId};

/// A node ready for html5ever's serializer.
pub struct SerializeNode<'a> {
    doc: &'a Document,
    id: NodeId,
    shadows: ShadowSerialization<'a>,
}

/// Which shadow roots `getHTML()` writes as declarative templates.
#[derive(Clone, Copy, Default)]
pub struct ShadowSerialization<'a> {
    /// `serializableShadowRoots`: roots marked serializable.
    pub serializable: bool,
    /// `shadowRoots`: these roots in any case.
    pub roots: &'a [NodeId],
}

impl<'a> SerializeNode<'a> {
    pub fn new(doc: &'a Document, id: NodeId) -> Self {
        SerializeNode {
            doc,
            id,
            shadows: ShadowSerialization::default(),
        }
    }

    fn shadow_to_write(&self, host: NodeId) -> Option<NodeId> {
        let sr = self.doc.shadow_root(host)?;
        let d = self.doc.shadow_root_data(sr)?;
        ((self.shadows.serializable && d.serializable) || self.shadows.roots.contains(&sr))
            .then_some(sr)
    }
}

enum Op {
    Open(NodeId),
    Close(NodeId),
    CloseTemplate,
}

fn template_name() -> html5ever::QualName {
    html5ever::QualName::new(
        None,
        html5ever::ns!(html),
        html5ever::local_name!("template"),
    )
}

impl SerializeNode<'_> {
    /// Push the children of `id` (or its template contents) so the first
    /// child pops first.
    fn push_children(&self, stack: &mut Vec<Op>, id: NodeId) {
        let parent = self.doc.template_contents(id).unwrap_or(id);
        let mut c = self.doc.last_child(parent);
        while let Some(n) = c {
            stack.push(Op::Open(n));
            c = self.doc.prev_sibling(n);
        }
    }
}

impl SerializeNode<'_> {
    /// Write `host`'s shadow root as a `<template shadowrootmode>` start
    /// tag and queue its children, if it is to be written.
    fn open_shadow<S: Serializer>(
        &self,
        s: &mut S,
        stack: &mut Vec<Op>,
        host: NodeId,
    ) -> io::Result<()> {
        let Some(sr) = self.shadow_to_write(host) else {
            return Ok(());
        };
        let d = self.doc.shadow_root_data(sr).expect("shadow root");
        let attr = |n: &str| html5ever::QualName::new(None, html5ever::ns!(), n.into());
        let mut attrs = vec![(
            attr("shadowrootmode"),
            match d.mode {
                crate::ShadowRootMode::Open => "open",
                crate::ShadowRootMode::Closed => "closed",
            },
        )];
        if d.delegates_focus {
            attrs.push((attr("shadowrootdelegatesfocus"), ""));
        }
        if d.serializable {
            attrs.push((attr("shadowrootserializable"), ""));
        }
        if d.clonable {
            attrs.push((attr("shadowrootclonable"), ""));
        }
        s.start_elem(template_name(), attrs.iter().map(|(n, v)| (n, *v)))?;
        stack.push(Op::CloseTemplate);
        self.push_children(stack, sr);
        Ok(())
    }
}

impl Serialize for SerializeNode<'_> {
    fn serialize<S: Serializer>(&self, s: &mut S, scope: TraversalScope) -> io::Result<()> {
        // Explicit stack: deep trees must not overflow the call stack.
        let mut stack = Vec::new();
        match scope {
            TraversalScope::IncludeNode => stack.push(Op::Open(self.id)),
            TraversalScope::ChildrenOnly(_) => {
                self.push_children(&mut stack, self.id);
                self.open_shadow(s, &mut stack, self.id)?;
            }
        }
        while let Some(op) = stack.pop() {
            match op {
                Op::Open(id) => match &self.doc.node(id).data {
                    NodeData::Element(i) => {
                        let e = &self.doc.elements[*i as usize];
                        s.start_elem(e.name.clone(), e.attrs.iter().map(|a| (&a.name, &*a.value)))?;
                        stack.push(Op::Close(id));
                        self.push_children(&mut stack, id);
                        self.open_shadow(s, &mut stack, id)?;
                    }
                    NodeData::Document | NodeData::DocumentFragment | NodeData::ShadowRoot(_) => {
                        self.push_children(&mut stack, id)
                    }
                    NodeData::Doctype(d) => s.write_doctype(&d.name)?,
                    NodeData::Text(t) => s.write_text(t)?,
                    NodeData::Comment(t) => s.write_comment(t)?,
                    NodeData::ProcessingInstruction(p) => {
                        s.write_processing_instruction(&p.target, &p.data)?
                    }
                },
                Op::Close(id) => {
                    let name = self.doc.qual_name(id).expect("element").clone();
                    s.end_elem(name)?;
                }
                Op::CloseTemplate => s.end_elem(template_name())?,
            }
        }
        Ok(())
    }
}

impl Document {
    fn serialize_to_string(&self, id: NodeId, scope: TraversalScope) -> String {
        self.serialize_with(id, scope, ShadowSerialization::default())
    }

    fn serialize_with(
        &self,
        id: NodeId,
        scope: TraversalScope,
        shadows: ShadowSerialization<'_>,
    ) -> String {
        let mut out = Vec::new();
        let opts = SerializeOpts {
            traversal_scope: scope,
            ..Default::default()
        };
        let node = SerializeNode {
            doc: self,
            id,
            shadows,
        };
        serialize(&mut out, &node, opts).expect("writing to a Vec cannot fail");
        String::from_utf8(out).expect("serializer output is UTF-8")
    }

    /// `outerHTML`: the node and its subtree as markup.
    pub fn outer_html(&self, id: NodeId) -> String {
        self.serialize_to_string(id, TraversalScope::IncludeNode)
    }

    /// `innerHTML`: the children of an element (template contents for a
    /// `<template>`), document or fragment.
    pub fn inner_html(&self, id: NodeId) -> String {
        let ctx = self.qual_name(id).cloned();
        self.serialize_to_string(id, TraversalScope::ChildrenOnly(ctx))
    }

    /// `getHTML(options)`: like `innerHTML`, writing the chosen shadow
    /// roots as `<template shadowrootmode>` elements.
    pub fn get_html(&self, id: NodeId, shadows: ShadowSerialization<'_>) -> String {
        let ctx = self.qual_name(id).cloned();
        self.serialize_with(id, TraversalScope::ChildrenOnly(ctx), shadows)
    }

    /// The whole document as markup, doctype included.
    pub fn to_html(&self) -> String {
        self.inner_html(Self::DOCUMENT)
    }
}
