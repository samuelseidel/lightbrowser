//! html5ever `TreeSink` over the arena.
//!
//! The sink owns the [`Document`] only while the parser runs: the driver
//! swaps the caller's document in before feeding and back out afterwards.

use std::borrow::Cow;
use std::cell::{Cell, Ref, RefCell};
use std::collections::HashMap;

use html5ever::tendril::StrTendril;
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{Attribute, QualName, local_name, ns};

use crate::document::Document;
use crate::node::{Doctype, NodeFlags, NodeId};

pub(crate) struct Sink {
    pub(crate) doc: RefCell<Document>,
    /// The node html5ever treats as "the document": the real document node,
    /// or a scratch fragment for fragment parsing.
    root: NodeId,
    /// Document parser (true) or fragment parser (false).
    document_parser: bool,
    /// `<template shadowrootmode>` attaches shadow roots (document parser
    /// and `setHTMLUnsafe`).
    pub(crate) allow_shadow: bool,
    line: Cell<u64>,
    pub(crate) script_lines: RefCell<HashMap<NodeId, u64>>,
    pub(crate) errors: Cell<u32>,
    /// Last HTML `<link>` / `<style>` element created (for style sheet
    /// pauses, see [`crate::ParserOptions::pause_at_style_sheets`]).
    pub(crate) last_link: Cell<Option<NodeId>>,
    pub(crate) last_style: Cell<Option<NodeId>>,
    /// HTML `<style>` elements the parser closed, for the page layer.
    pub(crate) closed_styles: RefCell<Vec<NodeId>>,
    /// Elements the document parser made that may be custom elements
    /// (valid custom element name or an `is` attribute), for the page
    /// layer to construct.
    pub(crate) customs: RefCell<Vec<NodeId>>,
    /// Last such element (a pause point, see `PauseSink`).
    pub(crate) last_custom: Cell<Option<NodeId>>,
}

impl Sink {
    pub(crate) fn new(doc: Document, root: NodeId, document_parser: bool) -> Self {
        Sink {
            doc: RefCell::new(doc),
            root,
            document_parser,
            allow_shadow: document_parser,
            line: Cell::new(1),
            script_lines: RefCell::new(HashMap::new()),
            errors: Cell::new(0),
            last_link: Cell::new(None),
            last_style: Cell::new(None),
            closed_styles: RefCell::new(Vec::new()),
            customs: RefCell::new(Vec::new()),
            last_custom: Cell::new(None),
        }
    }
}

impl TreeSink for Sink {
    type Handle = NodeId;
    type Output = ();
    // A shared borrow of the arena: html5ever only holds it within one
    // expression, never across another sink call.
    type ElemName<'a>
        = Ref<'a, QualName>
    where
        Self: 'a;

    fn finish(self) {}

    fn parse_error(&self, _msg: Cow<'static, str>) {
        self.errors.set(self.errors.get().saturating_add(1));
    }

    fn get_document(&self) -> NodeId {
        self.root
    }

    fn elem_name<'a>(&'a self, target: &'a NodeId) -> Ref<'a, QualName> {
        Ref::map(self.doc.borrow(), |doc| {
            &doc.element(*target).expect("not an element").name
        })
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> NodeId {
        let is_script = name.ns == ns!(html) && name.local == local_name!("script");
        let html = name.ns == ns!(html);
        let (is_link, is_style) = (
            html && name.local == local_name!("link"),
            html && name.local == local_name!("style"),
        );
        let mut doc = self.doc.borrow_mut();
        let id = doc.create_element(name, attrs);
        if flags.mathml_annotation_xml_integration_point
            && let Some(e) = doc.element_mut(id)
        {
            e.mathml_annotation_xml_integration_point = true;
        }
        if self.document_parser && doc.custom_state(id) == crate::CustomState::Undefined {
            self.customs.borrow_mut().push(id);
            self.last_custom.set(Some(id));
        }
        if self.document_parser {
            doc.set_flags(id, NodeFlags::PARSER_INSERTED, true);
            if is_script {
                self.script_lines.borrow_mut().insert(id, self.line.get());
            }
        }
        if is_link {
            self.last_link.set(Some(id));
        } else if is_style {
            self.last_style.set(Some(id));
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

    fn pop(&self, node: &NodeId) {
        if self.document_parser
            && self
                .doc
                .borrow()
                .is_html_element_named(*node, &local_name!("style"))
        {
            self.closed_styles.borrow_mut().push(*node);
        }
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

    fn allow_declarative_shadow_roots(&self, _intended_parent: &NodeId) -> bool {
        self.allow_shadow
    }

    fn attach_declarative_shadow(
        &self,
        location: &NodeId,
        template: &NodeId,
        attrs: &[Attribute],
    ) -> bool {
        let mut doc = self.doc.borrow_mut();
        let Some(sr) = doc.attach_declarative(*location, attrs) else {
            return false;
        };
        if let Some(e) = doc.element_mut(*template) {
            e.template_contents = Some(sr);
        }
        true
    }
}
