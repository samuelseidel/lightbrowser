//! html5ever `TreeSink` over the arena.
//!
//! The sink owns the [`Document`] only while the parser runs: the driver
//! swaps the caller's document in before feeding and back out afterwards.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use html5ever::tendril::StrTendril;
use html5ever::tree_builder::{ElemName, ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{Attribute, LocalName, Namespace, QualName, local_name, ns};

use crate::document::Document;
use crate::node::{Doctype, NodeFlags, NodeId};

pub(crate) struct Sink {
    pub(crate) doc: RefCell<Document>,
    /// The node html5ever treats as "the document": the real document node,
    /// or a scratch fragment for fragment parsing.
    root: NodeId,
    /// Document parser (true) or fragment parser (false).
    document_parser: bool,
    line: Cell<u64>,
    pub(crate) script_lines: RefCell<HashMap<NodeId, u64>>,
    pub(crate) errors: Cell<u32>,
}

impl Sink {
    pub(crate) fn new(doc: Document, root: NodeId, document_parser: bool) -> Self {
        Sink {
            doc: RefCell::new(doc),
            root,
            document_parser,
            line: Cell::new(1),
            script_lines: RefCell::new(HashMap::new()),
            errors: Cell::new(0),
        }
    }
}

/// Owned element name handed to html5ever (cloning atoms is cheap and
/// avoids holding a `RefCell` borrow across tree-builder calls).
#[derive(Debug)]
pub(crate) struct ElemNameBuf(QualName);

impl ElemName for ElemNameBuf {
    fn ns(&self) -> &Namespace {
        &self.0.ns
    }

    fn local_name(&self) -> &LocalName {
        &self.0.local
    }
}

impl TreeSink for Sink {
    type Handle = NodeId;
    type Output = ();
    type ElemName<'a>
        = ElemNameBuf
    where
        Self: 'a;

    fn finish(self) {}

    fn parse_error(&self, _msg: Cow<'static, str>) {
        self.errors.set(self.errors.get().saturating_add(1));
    }

    fn get_document(&self) -> NodeId {
        self.root
    }

    fn elem_name<'a>(&'a self, target: &'a NodeId) -> ElemNameBuf {
        let doc = self.doc.borrow();
        ElemNameBuf(doc.element(*target).expect("not an element").name.clone())
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> NodeId {
        let is_script = name.ns == ns!(html) && name.local == local_name!("script");
        let mut doc = self.doc.borrow_mut();
        let id = doc.create_element(name, attrs);
        if flags.mathml_annotation_xml_integration_point
            && let Some(e) = doc.element_mut(id)
        {
            e.mathml_annotation_xml_integration_point = true;
        }
        if self.document_parser {
            doc.set_flags(id, NodeFlags::PARSER_INSERTED, true);
            if is_script {
                self.script_lines.borrow_mut().insert(id, self.line.get());
            }
        }
        id
    }

    fn create_comment(&self, text: StrTendril) -> NodeId {
        self.doc.borrow_mut().create_comment_tendril(text)
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> NodeId {
        self.doc.borrow_mut().create_pi_tendril(target, data)
    }

    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        let mut doc = self.doc.borrow_mut();
        match child {
            NodeOrText::AppendNode(n) => doc.insert_unchecked(*parent, n, None),
            NodeOrText::AppendText(t) => doc.insert_text_unchecked(*parent, t, None),
        }
    }

    fn append_based_on_parent_node(
        &self,
        element: &NodeId,
        prev_element: &NodeId,
        child: NodeOrText<NodeId>,
    ) {
        let has_parent = self.doc.borrow().parent(*element).is_some();
        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        let mut doc = self.doc.borrow_mut();
        let dt = doc.create_doctype_tendril(Doctype {
            name,
            public_id,
            system_id,
        });
        doc.insert_unchecked(self.root, dt, None);
    }

    fn mark_script_already_started(&self, node: &NodeId) {
        self.doc
            .borrow_mut()
            .set_flags(*node, NodeFlags::ALREADY_STARTED, true);
    }

    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        self.doc
            .borrow()
            .template_contents(*target)
            .expect("template without contents")
    }

    fn same_node(&self, x: &NodeId, y: &NodeId) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        if self.document_parser {
            self.doc.borrow_mut().set_quirks_mode(mode);
        }
    }

    fn append_before_sibling(&self, sibling: &NodeId, new_node: NodeOrText<NodeId>) {
        let mut doc = self.doc.borrow_mut();
        let Some(parent) = doc.parent(*sibling) else {
            return;
        };
        match new_node {
            NodeOrText::AppendNode(n) => doc.insert_unchecked(parent, n, Some(*sibling)),
            NodeOrText::AppendText(t) => doc.insert_text_unchecked(parent, t, Some(*sibling)),
        }
    }

    fn add_attrs_if_missing(&self, target: &NodeId, attrs: Vec<Attribute>) {
        self.doc.borrow_mut().add_attrs_if_missing(*target, attrs);
    }

    fn remove_from_parent(&self, target: &NodeId) {
        self.doc.borrow_mut().detach(*target);
    }

    fn reparent_children(&self, node: &NodeId, new_parent: &NodeId) {
        self.doc.borrow_mut().reparent_children(*node, *new_parent);
    }

    fn is_mathml_annotation_xml_integration_point(&self, handle: &NodeId) -> bool {
        self.doc
            .borrow()
            .element(*handle)
            .is_some_and(|e| e.mathml_annotation_xml_integration_point)
    }

    fn set_current_line(&self, line_number: u64) {
        self.line.set(line_number);
    }

    // No shadow DOM yet: `<template shadowrootmode>` stays a plain template.
    fn allow_declarative_shadow_roots(&self, _intended_parent: &NodeId) -> bool {
        false
    }
}
