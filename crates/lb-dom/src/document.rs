//! The document arena and the tree, attribute and text APIs.

use std::borrow::Cow;
use std::fmt;

use html5ever::tendril::StrTendril;
use html5ever::tree_builder::QuirksMode;
use html5ever::{Attribute, LocalName, Namespace, Prefix, QualName, local_name, ns};

use crate::mutation::{Mutation, MutationHook};
use crate::node::{
    Doctype, ElementData, NONE, Node, NodeData, NodeFlags, NodeId, NodeType, ProcessingInstruction,
    link, qualified_name, qualified_name_eq, unlink,
};

/// DOM errors. [`DomError::name`] gives the `DOMException` name the binding
/// layer should throw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DomError {
    HierarchyRequest,
    NotFound,
    InvalidCharacter,
    Namespace,
    NoModificationAllowed,
    /// Bad selector or markup; the string says what failed.
    Syntax(String),
}

impl DomError {
    /// `DOMException.name` for this error.
    pub fn name(&self) -> &'static str {
        match self {
            DomError::HierarchyRequest => "HierarchyRequestError",
            DomError::NotFound => "NotFoundError",
            DomError::InvalidCharacter => "InvalidCharacterError",
            DomError::Namespace => "NamespaceError",
            DomError::NoModificationAllowed => "NoModificationAllowedError",
            DomError::Syntax(_) => "SyntaxError",
        }
    }
}

impl fmt::Display for DomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DomError::Syntax(msg) => write!(f, "SyntaxError: {msg}"),
            other => f.write_str(other.name()),
        }
    }
}

impl std::error::Error for DomError {}

/// One document and every node it owns.
///
/// All nodes live in one `Vec`; dropping the document frees them in one
/// step. Nodes are never freed individually, so a [`NodeId`] stays valid
/// (possibly detached) until the document is dropped. Template contents live
/// in the same arena.
///
/// `Document` is `!Send`: it lives on its page's worker thread.
pub struct Document {
    pub(crate) nodes: Vec<Node>,
    pub(crate) elements: Vec<ElementData>,
    quirks_mode: QuirksMode,
    is_html: bool,
    generation: u64,
    hook: Option<Box<dyn MutationHook>>,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Document")
            .field("nodes", &self.nodes.len())
            .field("elements", &self.elements.len())
            .field("is_html", &self.is_html)
            .field("quirks_mode", &self.quirks_mode)
            .finish()
    }
}

/// Snapshot of a node used by `clone_node` / `import_node`.
enum ShallowCopy {
    Document,
    Doctype(Doctype),
    Element(QualName, Vec<Attribute>),
    Text(StrTendril),
    Comment(StrTendril),
    Pi(ProcessingInstruction),
    Fragment,
}

impl Document {
    /// The document node is always the first arena slot.
    pub const DOCUMENT: NodeId = NodeId(0);

    /// A new, empty HTML document.
    pub fn new() -> Self {
        Self::with_kind(true)
    }

    /// A new, empty document; `is_html` false means an XML document
    /// (case-sensitive names, no HTML-specific lowercasing).
    pub fn with_kind(is_html: bool) -> Self {
        let mut doc = Document {
            nodes: Vec::new(),
            elements: Vec::new(),
            quirks_mode: QuirksMode::NoQuirks,
            is_html,
            generation: 0,
            hook: None,
        };
        doc.nodes.push(Node::new(NodeData::Document));
        doc
    }

    /// Placeholder with no nodes, swapped in while a parser borrows the real
    /// document. Never handed out.
    pub(crate) fn placeholder() -> Self {
        Document {
            nodes: Vec::new(),
            elements: Vec::new(),
            quirks_mode: QuirksMode::NoQuirks,
            is_html: true,
            generation: 0,
            hook: None,
        }
    }

    /// Parse a whole HTML string in one go. Scripts are not run; the parser
    /// simply continues past them.
    pub fn parse_html(html: &str) -> Self {
        let mut doc = Document::new();
        let mut parser = crate::HtmlParser::new();
        parser.push_str(html);
        parser.end();
        while let crate::ParseStep::Script(_) = parser.run(&mut doc) {}
        doc
    }

    // ----- document-level state -----

    /// True for HTML documents (affects name case handling).
    pub fn is_html(&self) -> bool {
        self.is_html
    }

    pub fn quirks_mode(&self) -> QuirksMode {
        self.quirks_mode
    }

    pub fn set_quirks_mode(&mut self, mode: QuirksMode) {
        self.quirks_mode = mode;
    }

    /// Bumped on every child-list or attribute change anywhere in the
    /// document. Live collections cache results keyed by this value.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Number of arena slots, including detached nodes.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Install (or clear) the mutation hook. Returns the previous one.
    pub fn set_mutation_hook(
        &mut self,
        hook: Option<Box<dyn MutationHook>>,
    ) -> Option<Box<dyn MutationHook>> {
        std::mem::replace(&mut self.hook, hook)
    }

    #[inline]
    fn emit(&mut self, m: impl FnOnce() -> Mutation) {
        if let Some(h) = self.hook.as_mut() {
            h.on_mutation(&m());
        }
    }

    // ----- node creation -----

    fn push(&mut self, data: NodeData) -> NodeId {
        let id = u32::try_from(self.nodes.len())
            .ok()
            .filter(|&i| i != NONE)
            .expect("DOM arena full");
        self.nodes.push(Node::new(data));
        NodeId(id)
    }

    /// Create a detached element with the given name and attributes.
    /// A `<template>` gets its content fragment here.
    pub fn create_element(&mut self, name: QualName, attrs: Vec<Attribute>) -> NodeId {
        let is_template = name.ns == ns!(html) && name.local == local_name!("template");
        let idx = u32::try_from(self.elements.len()).expect("element table full");
        self.elements.push(ElementData::new(name, attrs));
        let id = self.push(NodeData::Element(idx));
        if is_template {
            let contents = self.push(NodeData::DocumentFragment);
            self.elements[idx as usize].template_contents = Some(contents);
        }
        id
    }

    /// `document.createElement(localName)`: validates the name, lowercases it
    /// in HTML documents and puts it in the HTML namespace.
    pub fn create_element_by_name(&mut self, local: &str) -> Result<NodeId, DomError> {
        if !is_valid_element_name(local) {
            return Err(DomError::InvalidCharacter);
        }
        let local = if self.is_html {
            Cow::Owned(local.to_ascii_lowercase())
        } else {
            Cow::Borrowed(local)
        };
        let name = QualName::new(None, ns!(html), LocalName::from(&*local));
        Ok(self.create_element(name, Vec::new()))
    }

    /// `document.createElementNS(namespace, qualifiedName)`.
    pub fn create_element_ns(
        &mut self,
        namespace: Namespace,
        qualified: &str,
    ) -> Result<NodeId, DomError> {
        let name = validate_and_extract(namespace, qualified, is_valid_element_name)?;
        Ok(self.create_element(name, Vec::new()))
    }

    pub fn create_text(&mut self, data: &str) -> NodeId {
        self.push(NodeData::Text(StrTendril::from_slice(data)))
    }

    pub(crate) fn create_text_tendril(&mut self, data: StrTendril) -> NodeId {
        self.push(NodeData::Text(data))
    }

    pub fn create_comment(&mut self, data: &str) -> NodeId {
        self.push(NodeData::Comment(StrTendril::from_slice(data)))
    }

    pub(crate) fn create_comment_tendril(&mut self, data: StrTendril) -> NodeId {
        self.push(NodeData::Comment(data))
    }

    pub fn create_document_fragment(&mut self) -> NodeId {
        self.push(NodeData::DocumentFragment)
    }

    pub fn create_processing_instruction(&mut self, target: &str, data: &str) -> NodeId {
        self.create_pi_tendril(StrTendril::from_slice(target), StrTendril::from_slice(data))
    }

    pub(crate) fn create_pi_tendril(&mut self, target: StrTendril, data: StrTendril) -> NodeId {
        self.push(NodeData::ProcessingInstruction(Box::new(
            ProcessingInstruction { target, data },
        )))
    }

    pub fn create_doctype(&mut self, name: &str, public_id: &str, system_id: &str) -> NodeId {
        self.create_doctype_tendril(Doctype {
            name: name.into(),
            public_id: public_id.into(),
            system_id: system_id.into(),
        })
    }

    pub(crate) fn create_doctype_tendril(&mut self, doctype: Doctype) -> NodeId {
        self.push(NodeData::Doctype(Box::new(doctype)))
    }

    // ----- node inspection -----

    #[inline]
    pub(crate) fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    #[inline]
    fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.index()]
    }

    pub fn node_type(&self, id: NodeId) -> NodeType {
        match self.node(id).data {
            NodeData::Document => NodeType::Document,
            NodeData::Doctype(_) => NodeType::DocumentType,
            NodeData::Element(_) => NodeType::Element,
            NodeData::Text(_) => NodeType::Text,
            NodeData::Comment(_) => NodeType::Comment,
            NodeData::ProcessingInstruction(_) => NodeType::ProcessingInstruction,
            NodeData::DocumentFragment => NodeType::DocumentFragment,
        }
    }

    #[inline]
    pub fn is_element(&self, id: NodeId) -> bool {
        matches!(self.node(id).data, NodeData::Element(_))
    }

    #[inline]
    pub fn is_text(&self, id: NodeId) -> bool {
        matches!(self.node(id).data, NodeData::Text(_))
    }

    /// Element payload, or `None` for non-elements.
    #[inline]
    pub fn element(&self, id: NodeId) -> Option<&ElementData> {
        match self.node(id).data {
            NodeData::Element(i) => Some(&self.elements[i as usize]),
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn element_mut(&mut self, id: NodeId) -> Option<&mut ElementData> {
        match self.nodes[id.index()].data {
            NodeData::Element(i) => Some(&mut self.elements[i as usize]),
            _ => None,
        }
    }

    pub fn doctype(&self, id: NodeId) -> Option<&Doctype> {
        match &self.node(id).data {
            NodeData::Doctype(d) => Some(d),
            _ => None,
        }
    }

    pub fn processing_instruction(&self, id: NodeId) -> Option<&ProcessingInstruction> {
        match &self.node(id).data {
            NodeData::ProcessingInstruction(p) => Some(p),
            _ => None,
        }
    }

    /// Element's local name.
    pub fn local_name(&self, id: NodeId) -> Option<&LocalName> {
        self.element(id).map(|e| &e.name.local)
    }

    /// Element's qualified name (namespace, prefix, local).
    pub fn qual_name(&self, id: NodeId) -> Option<&QualName> {
        self.element(id).map(|e| &e.name)
    }

    /// True if `id` is an HTML element with this local name.
    pub fn is_html_element_named(&self, id: NodeId, local: &LocalName) -> bool {
        self.element(id)
            .is_some_and(|e| e.name.ns == ns!(html) && e.name.local == *local)
    }

    /// `Element.tagName`: qualified name, uppercased for HTML elements in
    /// HTML documents.
    pub fn tag_name(&self, id: NodeId) -> Option<String> {
        let e = self.element(id)?;
        let mut q = qualified_name(&e.name);
        if self.is_html && e.is_html() {
            q.make_ascii_uppercase();
        }
        Some(q)
    }

    /// `Node.nodeName`.
    pub fn node_name(&self, id: NodeId) -> String {
        match &self.node(id).data {
            NodeData::Document => "#document".into(),
            NodeData::Doctype(d) => d.name.to_string(),
            NodeData::Element(_) => self.tag_name(id).unwrap_or_default(),
            NodeData::Text(_) => "#text".into(),
            NodeData::Comment(_) => "#comment".into(),
            NodeData::ProcessingInstruction(p) => p.target.to_string(),
            NodeData::DocumentFragment => "#document-fragment".into(),
        }
    }

    pub fn flags(&self, id: NodeId) -> NodeFlags {
        self.node(id).flags
    }

    pub fn set_flags(&mut self, id: NodeId, flags: NodeFlags, on: bool) {
        self.node_mut(id).flags.set(flags, on);
    }

    /// `ownerDocument`: `None` for the document itself.
    pub fn owner_document(&self, id: NodeId) -> Option<NodeId> {
        (id != Self::DOCUMENT).then_some(Self::DOCUMENT)
    }

    // ----- tree navigation -----

    #[inline]
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        unlink(self.node(id).parent)
    }

    #[inline]
    pub fn first_child(&self, id: NodeId) -> Option<NodeId> {
        unlink(self.node(id).first_child)
    }

    #[inline]
    pub fn last_child(&self, id: NodeId) -> Option<NodeId> {
        unlink(self.node(id).last_child)
    }

    #[inline]
    pub fn prev_sibling(&self, id: NodeId) -> Option<NodeId> {
        unlink(self.node(id).prev_sibling)
    }

    #[inline]
    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        unlink(self.node(id).next_sibling)
    }

    pub fn parent_element(&self, id: NodeId) -> Option<NodeId> {
        self.parent(id).filter(|&p| self.is_element(p))
    }

    pub fn first_element_child(&self, id: NodeId) -> Option<NodeId> {
        self.children(id).find(|&c| self.is_element(c))
    }

    pub fn last_element_child(&self, id: NodeId) -> Option<NodeId> {
        let mut c = self.last_child(id);
        while let Some(n) = c {
            if self.is_element(n) {
                return Some(n);
            }
            c = self.prev_sibling(n);
        }
        None
    }

    pub fn next_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let mut c = self.next_sibling(id);
        while let Some(n) = c {
            if self.is_element(n) {
                return Some(n);
            }
            c = self.next_sibling(n);
        }
        None
    }

    pub fn prev_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let mut c = self.prev_sibling(id);
        while let Some(n) = c {
            if self.is_element(n) {
                return Some(n);
            }
            c = self.prev_sibling(n);
        }
        None
    }

    /// Children in order.
    pub fn children(&self, id: NodeId) -> Children<'_> {
        Children {
            doc: self,
            next: self.node(id).first_child,
        }
    }

    /// Element children in order.
    pub fn element_children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.children(id).filter(|&c| self.is_element(c))
    }

    pub fn child_count(&self, id: NodeId) -> usize {
        self.children(id).count()
    }

    /// Descendants in tree order, excluding `id`. Template contents are not
    /// entered (they are a separate tree).
    pub fn descendants(&self, id: NodeId) -> Descendants<'_> {
        Descendants {
            doc: self,
            root: id,
            next: self.first_child(id),
        }
    }

    /// `id` followed by its descendants in tree order.
    pub fn inclusive_descendants(&self, id: NodeId) -> Descendants<'_> {
        Descendants {
            doc: self,
            root: id,
            next: Some(id),
        }
    }

    /// Ancestors from the parent upward.
    pub fn ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(self.parent(id), move |&n| self.parent(n))
    }

    /// Topmost ancestor (the node itself if detached and parentless).
    pub fn root(&self, id: NodeId) -> NodeId {
        self.ancestors(id).last().unwrap_or(id)
    }

    /// True when the node is in the document tree.
    pub fn is_connected(&self, id: NodeId) -> bool {
        self.root(id) == Self::DOCUMENT
    }

    /// `a.contains(b)`: `b` is `a` or a descendant of `a`.
    pub fn contains(&self, a: NodeId, b: NodeId) -> bool {
        a == b || self.ancestors(b).any(|n| n == a)
    }

    // ----- document shortcuts -----

    pub fn document_element(&self) -> Option<NodeId> {
        self.first_element_child(Self::DOCUMENT)
    }

    pub fn doctype_node(&self) -> Option<NodeId> {
        self.children(Self::DOCUMENT)
            .find(|&c| matches!(self.node(c).data, NodeData::Doctype(_)))
    }

    pub fn head(&self) -> Option<NodeId> {
        let html = self.document_element()?;
        self.element_children(html)
            .find(|&c| self.is_html_element_named(c, &local_name!("head")))
    }

    pub fn body(&self) -> Option<NodeId> {
        let html = self.document_element()?;
        self.element_children(html).find(|&c| {
            self.is_html_element_named(c, &local_name!("body"))
                || self.is_html_element_named(c, &local_name!("frameset"))
        })
    }

    /// `document.title` for HTML documents: the first `<title>`'s text with
    /// whitespace stripped and collapsed.
    pub fn title(&self) -> String {
        let Some(t) = self
            .descendants(Self::DOCUMENT)
            .find(|&n| self.is_html_element_named(n, &local_name!("title")))
        else {
            return String::new();
        };
        let raw = self.child_text_content(t);
        raw.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// First element in tree order under `root` whose `id` equals `value`.
    pub fn get_element_by_id_in(&self, root: NodeId, value: &str) -> Option<NodeId> {
        if value.is_empty() {
            return None;
        }
        self.descendants(root)
            .find(|&n| self.element(n).and_then(|e| e.attr("id")) == Some(value))
    }

    /// `document.getElementById`.
    pub fn get_element_by_id(&self, value: &str) -> Option<NodeId> {
        self.get_element_by_id_in(Self::DOCUMENT, value)
    }

    // ----- raw linking (no checks, no events) -----

    /// Unlink `id` from its parent and siblings.
    fn unlink_raw(&mut self, id: NodeId) {
        let n = self.node(id);
        let (parent, prev, next) = (n.parent, n.prev_sibling, n.next_sibling);
        if parent == NONE {
            return;
        }
        if prev != NONE {
            self.nodes[prev as usize].next_sibling = next;
        } else {
            self.nodes[parent as usize].first_child = next;
        }
        if next != NONE {
            self.nodes[next as usize].prev_sibling = prev;
        } else {
            self.nodes[parent as usize].last_child = prev;
        }
        let n = self.node_mut(id);
        n.parent = NONE;
        n.prev_sibling = NONE;
        n.next_sibling = NONE;
    }

    /// Link a parentless `id` into `parent` before `before` (or at the end).
    fn link_raw(&mut self, parent: NodeId, id: NodeId, before: Option<NodeId>) {
        debug_assert_eq!(self.node(id).parent, NONE);
        let prev = match before {
            Some(b) => {
                debug_assert_eq!(self.parent(b), Some(parent));
                let prev = self.node(b).prev_sibling;
                self.node_mut(b).prev_sibling = id.0;
                prev
            }
            None => {
                let prev = self.node(parent).last_child;
                self.node_mut(parent).last_child = id.0;
                prev
            }
        };
        if prev != NONE {
            self.nodes[prev as usize].next_sibling = id.0;
        } else {
            self.node_mut(parent).first_child = id.0;
        }
        let n = self.node_mut(id);
        n.parent = parent.0;
        n.prev_sibling = prev;
        n.next_sibling = link(before);
    }

    // ----- mutation with events, no validity checks -----

    /// Remove `id` from its parent (if any), firing the hook.
    pub fn detach(&mut self, id: NodeId) {
        let Some(parent) = self.parent(id) else {
            return;
        };
        let prev = self.prev_sibling(id);
        let next = self.next_sibling(id);
        self.unlink_raw(id);
        self.generation += 1;
        self.emit(|| Mutation::ChildRemoved {
            parent,
            node: id,
            prev_sibling: prev,
            next_sibling: next,
        });
    }

    /// Insert `id` into `parent` before `before`, moving it out of any
    /// previous parent. No validity checks: the parser and internal callers
    /// use this; script-facing calls go through [`Document::insert_before`].
    pub(crate) fn insert_unchecked(&mut self, parent: NodeId, id: NodeId, before: Option<NodeId>) {
        self.detach(id);
        self.link_raw(parent, id, before);
        self.generation += 1;
        let prev = self.prev_sibling(id);
        self.emit(|| Mutation::ChildInserted {
            parent,
            node: id,
            prev_sibling: prev,
            next_sibling: before,
        });
    }

    /// Unlink and return all children of a scratch node without events.
    pub(crate) fn take_children_silently(&mut self, parent: NodeId) -> Vec<NodeId> {
        let kids: Vec<NodeId> = self.children(parent).collect();
        for &k in &kids {
            self.unlink_raw(k);
        }
        kids
    }

    /// Move all children of `from` to the end of `to`.
    pub(crate) fn reparent_children(&mut self, from: NodeId, to: NodeId) {
        while let Some(c) = self.first_child(from) {
            self.insert_unchecked(to, c, None);
        }
    }

    /// Append `data` as text before `before` (or at the end) of `parent`,
    /// merging into an adjacent text node like the HTML parser does.
    pub(crate) fn insert_text_unchecked(
        &mut self,
        parent: NodeId,
        data: StrTendril,
        before: Option<NodeId>,
    ) {
        let prev = match before {
            Some(b) => self.prev_sibling(b),
            None => self.last_child(parent),
        };
        if let Some(p) = prev
            && let NodeData::Text(t) = &mut self.node_mut(p).data
        {
            t.push_tendril(&data);
            return;
        }
        let t = self.create_text_tendril(data);
        self.insert_unchecked(parent, t, before);
    }

    // ----- script-facing mutation (DOM semantics) -----

    /// `parent.appendChild(node)`.
    pub fn append_child(&mut self, parent: NodeId, node: NodeId) -> Result<NodeId, DomError> {
        self.insert_before(parent, node, None)
    }

    /// `parent.insertBefore(node, child)`.
    pub fn insert_before(
        &mut self,
        parent: NodeId,
        node: NodeId,
        child: Option<NodeId>,
    ) -> Result<NodeId, DomError> {
        self.check_insert(parent, node, child, None)?;
        let child = if child == Some(node) {
            self.next_sibling(node)
        } else {
            child
        };
        self.insert_validated(parent, node, child);
        Ok(node)
    }

    /// `parent.removeChild(child)`.
    pub fn remove_child(&mut self, parent: NodeId, child: NodeId) -> Result<NodeId, DomError> {
        if self.parent(child) != Some(parent) {
            return Err(DomError::NotFound);
        }
        self.detach(child);
        Ok(child)
    }

    /// `parent.replaceChild(node, child)`. Returns `child`.
    pub fn replace_child(
        &mut self,
        parent: NodeId,
        node: NodeId,
        child: NodeId,
    ) -> Result<NodeId, DomError> {
        self.check_insert(parent, node, Some(child), Some(child))?;
        let mut reference = self.next_sibling(child);
        if reference == Some(node) {
            reference = self.next_sibling(node);
        }
        if node != child {
            self.detach(child);
        }
        self.insert_validated(parent, node, reference);
        Ok(child)
    }

    /// Remove all children of `id`.
    pub fn remove_all_children(&mut self, id: NodeId) {
        while let Some(c) = self.first_child(id) {
            self.detach(c);
        }
    }

    fn insert_validated(&mut self, parent: NodeId, node: NodeId, before: Option<NodeId>) {
        if matches!(self.node(node).data, NodeData::DocumentFragment) {
            let kids: Vec<NodeId> = self.children(node).collect();
            for k in kids {
                self.insert_unchecked(parent, k, before);
            }
        } else {
            self.insert_unchecked(parent, node, before);
        }
    }

    /// "Ensure pre-insertion validity" / replace validity from the DOM spec.
    /// `replacing` is the child being replaced, if any.
    fn check_insert(
        &self,
        parent: NodeId,
        node: NodeId,
        child: Option<NodeId>,
        replacing: Option<NodeId>,
    ) -> Result<(), DomError> {
        use NodeType as T;
        let pt = self.node_type(parent);
        if !matches!(pt, T::Document | T::DocumentFragment | T::Element) {
            return Err(DomError::HierarchyRequest);
        }
        if self.contains(node, parent) {
            return Err(DomError::HierarchyRequest);
        }
        if let Some(c) = child
            && self.parent(c) != Some(parent)
        {
            return Err(DomError::NotFound);
        }
        let nt = self.node_type(node);
        match nt {
            T::Document => return Err(DomError::HierarchyRequest),
            T::Text if pt == T::Document => return Err(DomError::HierarchyRequest),
            T::DocumentType if pt != T::Document => return Err(DomError::HierarchyRequest),
            _ => {}
        }
        if pt != T::Document {
            return Ok(());
        }
        // Document parent: at most one element and one doctype, doctype first.
        let other_elements = self
            .children(parent)
            .filter(|&c| Some(c) != replacing && self.is_element(c))
            .count();
        let doctype_after = |c: NodeId| {
            std::iter::successors(self.next_sibling(c), |&n| self.next_sibling(n))
                .any(|n| self.node_type(n) == T::DocumentType)
        };
        let element_before = |c: NodeId| {
            std::iter::successors(self.prev_sibling(c), |&n| self.prev_sibling(n))
                .any(|n| self.is_element(n))
        };
        // An element may go in if there is no other element and no doctype
        // would end up after it.
        let element_ok = || {
            if other_elements > 0 {
                return false;
            }
            match child {
                None => true,
                Some(c) => {
                    let child_is_doctype = self.node_type(c) == T::DocumentType;
                    !(replacing.is_none() && child_is_doctype) && !doctype_after(c)
                }
            }
        };
        match nt {
            T::DocumentFragment => {
                let mut elems = 0;
                for c in self.children(node) {
                    match self.node_type(c) {
                        T::Element => elems += 1,
                        T::Text => return Err(DomError::HierarchyRequest),
                        _ => {}
                    }
                }
                if elems > 1 || (elems == 1 && !element_ok()) {
                    return Err(DomError::HierarchyRequest);
                }
            }
            T::Element => {
                if !element_ok() {
                    return Err(DomError::HierarchyRequest);
                }
            }
            T::DocumentType => {
                let other_doctype = self
                    .children(parent)
                    .any(|c| Some(c) != replacing && self.node_type(c) == T::DocumentType);
                let bad = other_doctype
                    || match child {
                        Some(c) => element_before(c),
                        None => other_elements > 0,
                    };
                if bad {
                    return Err(DomError::HierarchyRequest);
                }
            }
            _ => {}
        }
        Ok(())
    }

    // ----- text -----

    /// Data of a Text, Comment or ProcessingInstruction node.
    pub fn character_data(&self, id: NodeId) -> Option<&str> {
        match &self.node(id).data {
            NodeData::Text(t) | NodeData::Comment(t) => Some(t),
            NodeData::ProcessingInstruction(p) => Some(&p.data),
            _ => None,
        }
    }

    /// Replace the data of a Text, Comment or ProcessingInstruction node.
    /// Returns false for other node kinds.
    pub fn set_character_data(&mut self, id: NodeId, value: &str) -> bool {
        let new = StrTendril::from_slice(value);
        let old = match &mut self.node_mut(id).data {
            NodeData::Text(t) | NodeData::Comment(t) => std::mem::replace(t, new),
            NodeData::ProcessingInstruction(p) => std::mem::replace(&mut p.data, new),
            _ => return false,
        };
        self.emit(|| Mutation::CharacterData {
            node: id,
            old_value: old,
        });
        true
    }

    /// `Node.textContent` getter. `None` for documents and doctypes.
    pub fn text_content(&self, id: NodeId) -> Option<String> {
        match &self.node(id).data {
            NodeData::Document | NodeData::Doctype(_) => None,
            NodeData::Element(_) | NodeData::DocumentFragment => {
                let mut s = String::new();
                for n in self.descendants(id) {
                    if let NodeData::Text(t) = &self.node(n).data {
                        s.push_str(t);
                    }
                }
                Some(s)
            }
            _ => self.character_data(id).map(str::to_owned),
        }
    }

    /// Concatenated data of the direct Text children.
    pub fn child_text_content(&self, id: NodeId) -> String {
        let mut s = String::new();
        for c in self.children(id) {
            if let NodeData::Text(t) = &self.node(c).data {
                s.push_str(t);
            }
        }
        s
    }

    /// `Node.textContent` setter. `None` and `""` both clear an element.
    pub fn set_text_content(&mut self, id: NodeId, value: Option<&str>) {
        match self.node_type(id) {
            NodeType::Document | NodeType::DocumentType => {}
            NodeType::Element | NodeType::DocumentFragment => {
                self.remove_all_children(id);
                if let Some(v) = value.filter(|v| !v.is_empty()) {
                    let t = self.create_text(v);
                    self.insert_unchecked(id, t, None);
                }
            }
            _ => {
                self.set_character_data(id, value.unwrap_or(""));
            }
        }
    }

    // ----- attributes -----

    /// Attributes in order; empty for non-elements.
    pub fn attributes(&self, id: NodeId) -> &[Attribute] {
        self.element(id).map_or(&[], |e| &e.attrs)
    }

    /// Lowercase `name` when matching against an HTML element in an HTML
    /// document, as `getAttribute` and friends do.
    fn attr_lookup_name<'a>(&self, e: &ElementData, name: &'a str) -> Cow<'a, str> {
        if self.is_html && e.is_html() && name.bytes().any(|b| b.is_ascii_uppercase()) {
            Cow::Owned(name.to_ascii_lowercase())
        } else {
            Cow::Borrowed(name)
        }
    }

    fn find_attr(&self, id: NodeId, qualified: &str) -> Option<usize> {
        let e = self.element(id)?;
        let q = self.attr_lookup_name(e, qualified);
        e.attrs.iter().position(|a| qualified_name_eq(&a.name, &q))
    }

    /// `getAttribute(qualifiedName)`.
    pub fn get_attribute(&self, id: NodeId, qualified: &str) -> Option<&str> {
        let i = self.find_attr(id, qualified)?;
        Some(&self.element(id)?.attrs[i].value)
    }

    /// `getAttributeNS(namespace, localName)`.
    pub fn get_attribute_ns(&self, id: NodeId, namespace: &Namespace, local: &str) -> Option<&str> {
        self.element(id)?
            .attrs
            .iter()
            .find(|a| a.name.ns == *namespace && &*a.name.local == local)
            .map(|a| &*a.value)
    }

    pub fn has_attribute(&self, id: NodeId, qualified: &str) -> bool {
        self.find_attr(id, qualified).is_some()
    }

    /// `setAttribute(qualifiedName, value)`.
    pub fn set_attribute(
        &mut self,
        id: NodeId,
        qualified: &str,
        value: &str,
    ) -> Result<(), DomError> {
        if !is_valid_attribute_name(qualified) {
            return Err(DomError::InvalidCharacter);
        }
        let Some(e) = self.element(id) else {
            return Ok(());
        };
        match self.find_attr(id, qualified) {
            Some(i) => self.replace_attr_value(id, i, value),
            None => {
                let local = self.attr_lookup_name(e, qualified);
                let name = QualName::new(None, ns!(), LocalName::from(&*local));
                self.append_attr(id, name, value);
            }
        }
        Ok(())
    }

    /// `setAttributeNS(namespace, qualifiedName, value)`.
    pub fn set_attribute_ns(
        &mut self,
        id: NodeId,
        namespace: Namespace,
        qualified: &str,
        value: &str,
    ) -> Result<(), DomError> {
        let name = validate_and_extract(namespace, qualified, is_valid_attribute_name)?;
        let Some(e) = self.element(id) else {
            return Ok(());
        };
        let found = e
            .attrs
            .iter()
            .position(|a| a.name.ns == name.ns && a.name.local == name.local);
        match found {
            Some(i) => self.replace_attr_value(id, i, value),
            None => self.append_attr(id, name, value),
        }
        Ok(())
    }

    fn replace_attr_value(&mut self, id: NodeId, i: usize, value: &str) {
        let e = self.element_mut(id).expect("element");
        let attr = &mut e.attrs[i];
        let old = std::mem::replace(&mut attr.value, StrTendril::from_slice(value));
        let name = attr.name.clone();
        self.generation += 1;
        self.emit(|| Mutation::Attribute {
            element: id,
            name,
            old_value: Some(old),
        });
    }

    fn append_attr(&mut self, id: NodeId, name: QualName, value: &str) {
        let e = self.element_mut(id).expect("element");
        e.attrs.push(Attribute {
            name: name.clone(),
            value: StrTendril::from_slice(value),
        });
        self.generation += 1;
        self.emit(|| Mutation::Attribute {
            element: id,
            name,
            old_value: None,
        });
    }

    fn remove_attr_at(&mut self, id: NodeId, i: usize) {
        let e = self.element_mut(id).expect("element");
        let a = e.attrs.remove(i);
        self.generation += 1;
        self.emit(|| Mutation::Attribute {
            element: id,
            name: a.name,
            old_value: Some(a.value),
        });
    }

    /// `removeAttribute(qualifiedName)`. Returns true if one was removed.
    pub fn remove_attribute(&mut self, id: NodeId, qualified: &str) -> bool {
        match self.find_attr(id, qualified) {
            Some(i) => {
                self.remove_attr_at(id, i);
                true
            }
            None => false,
        }
    }

    /// `removeAttributeNS(namespace, localName)`.
    pub fn remove_attribute_ns(&mut self, id: NodeId, namespace: &Namespace, local: &str) -> bool {
        let found = self.element(id).and_then(|e| {
            e.attrs
                .iter()
                .position(|a| a.name.ns == *namespace && &*a.name.local == local)
        });
        match found {
            Some(i) => {
                self.remove_attr_at(id, i);
                true
            }
            None => false,
        }
    }

    /// Append attributes whose names are not present yet (parser use).
    pub(crate) fn add_attrs_if_missing(&mut self, id: NodeId, attrs: Vec<Attribute>) {
        for a in attrs {
            let present = self
                .element(id)
                .is_some_and(|e| e.attrs.iter().any(|x| x.name == a.name));
            if !present {
                let e = self.element_mut(id).expect("element");
                e.attrs.push(a.clone());
                self.generation += 1;
                self.emit(|| Mutation::Attribute {
                    element: id,
                    name: a.name,
                    old_value: None,
                });
            }
        }
    }

    /// The `id` attribute.
    pub fn element_id(&self, id: NodeId) -> Option<&str> {
        self.element(id)?.attr("id")
    }

    /// The `class` attribute, or `""`.
    pub fn class_name(&self, id: NodeId) -> &str {
        self.element(id).and_then(|e| e.attr("class")).unwrap_or("")
    }

    /// Class tokens (split on ASCII whitespace).
    pub fn classes(&self, id: NodeId) -> impl Iterator<Item = &str> {
        self.class_name(id).split_ascii_whitespace()
    }

    /// Case-sensitive class membership.
    pub fn has_class(&self, id: NodeId, class: &str) -> bool {
        self.classes(id).any(|c| c == class)
    }

    /// `<template>` content fragment.
    pub fn template_contents(&self, id: NodeId) -> Option<NodeId> {
        self.element(id)?.template_contents
    }

    // ----- cloning -----

    fn shallow_copy(&self, id: NodeId) -> ShallowCopy {
        match &self.node(id).data {
            NodeData::Document => ShallowCopy::Document,
            NodeData::Doctype(d) => ShallowCopy::Doctype((**d).clone()),
            NodeData::Element(i) => {
                let e = &self.elements[*i as usize];
                ShallowCopy::Element(e.name.clone(), e.attrs.clone())
            }
            NodeData::Text(t) => ShallowCopy::Text(t.clone()),
            NodeData::Comment(t) => ShallowCopy::Comment(t.clone()),
            NodeData::ProcessingInstruction(p) => ShallowCopy::Pi((**p).clone()),
            NodeData::DocumentFragment => ShallowCopy::Fragment,
        }
    }

    /// `node.cloneNode(deep)`. Cloning the document node gives a fragment
    /// holding copies of its children (a second arena is not created).
    pub fn clone_node(&mut self, id: NodeId, deep: bool) -> NodeId {
        // Two passes: snapshot under a shared borrow, then build.
        let plan = Self::plan_copy(self, id, deep);
        self.build_copy(plan)
    }

    /// `document.importNode(node, deep)`: copy a node from another document
    /// into this arena.
    pub fn import_node(&mut self, src: &Document, id: NodeId, deep: bool) -> NodeId {
        let plan = Self::plan_copy(src, id, deep);
        self.build_copy(plan)
    }

    /// Pre-order list of (copy, parent index in the plan, goes into the
    /// parent's template contents).
    fn plan_copy(src: &Document, id: NodeId, deep: bool) -> Vec<(ShallowCopy, usize, bool)> {
        let mut plan = vec![(src.shallow_copy(id), usize::MAX, false)];
        if !deep {
            return plan;
        }
        // Stack of (source node, parent plan index, into template contents).
        let mut stack: Vec<(NodeId, usize, bool)> = Vec::new();
        let push_children = |stack: &mut Vec<_>, of: NodeId, idx: usize| {
            let kids: Vec<NodeId> = src.children(of).collect();
            for k in kids.into_iter().rev() {
                stack.push((k, idx, false));
            }
            if let Some(tc) = src.template_contents(of) {
                let kids: Vec<NodeId> = src.children(tc).collect();
                for k in kids.into_iter().rev() {
                    stack.push((k, idx, true));
                }
            }
        };
        push_children(&mut stack, id, 0);
        while let Some((n, parent, into_tc)) = stack.pop() {
            let idx = plan.len();
            plan.push((src.shallow_copy(n), parent, into_tc));
            push_children(&mut stack, n, idx);
        }
        plan
    }

    fn build_copy(&mut self, plan: Vec<(ShallowCopy, usize, bool)>) -> NodeId {
        let mut ids: Vec<NodeId> = Vec::with_capacity(plan.len());
        for (copy, parent, into_tc) in plan {
            let id = match copy {
                ShallowCopy::Document | ShallowCopy::Fragment => self.create_document_fragment(),
                ShallowCopy::Doctype(d) => self.create_doctype_tendril(d),
                ShallowCopy::Element(name, attrs) => self.create_element(name, attrs),
                ShallowCopy::Text(t) => self.create_text_tendril(t),
                ShallowCopy::Comment(t) => self.create_comment_tendril(t),
                ShallowCopy::Pi(p) => self.create_pi_tendril(p.target, p.data),
            };
            if parent != usize::MAX {
                let mut p = ids[parent];
                if into_tc {
                    p = self.template_contents(p).expect("template contents");
                }
                // Fresh nodes: no observer can see them, so no events.
                self.link_raw(p, id, None);
            }
            ids.push(id);
        }
        self.generation += 1;
        ids[0]
    }
}

/// Iterator over a node's children.
pub struct Children<'a> {
    doc: &'a Document,
    next: u32,
}

impl Iterator for Children<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let id = unlink(self.next)?;
        self.next = self.doc.node(id).next_sibling;
        Some(id)
    }
}

/// Pre-order iterator over a subtree.
pub struct Descendants<'a> {
    doc: &'a Document,
    root: NodeId,
    next: Option<NodeId>,
}

impl Iterator for Descendants<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let cur = self.next?;
        self.next = if let Some(c) = self.doc.first_child(cur) {
            Some(c)
        } else {
            let mut n = cur;
            loop {
                if n == self.root {
                    break None;
                }
                if let Some(s) = self.doc.next_sibling(n) {
                    break Some(s);
                }
                match self.doc.parent(n) {
                    Some(p) => n = p,
                    None => break None,
                }
            }
        };
        Some(cur)
    }
}

/// "Valid element local name" (DOM spec, 2025 relaxed rules).
pub(crate) fn is_valid_element_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first.is_ascii_alphabetic() {
        !name
            .bytes()
            .any(|b| b.is_ascii_whitespace() || matches!(b, 0 | b'/' | b'>'))
    } else if first == ':' || first == '_' || first as u32 >= 0x80 {
        chars.all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':' | '_') || c as u32 >= 0x80
        })
    } else {
        false
    }
}

/// "Valid attribute local name" (DOM spec, 2025 relaxed rules).
pub(crate) fn is_valid_attribute_name(name: &str) -> bool {
    !name.is_empty()
        && !name
            .bytes()
            .any(|b| b.is_ascii_whitespace() || matches!(b, 0 | b'/' | b'>' | b'='))
}

/// DOM "validate and extract" for `*NS` methods.
fn validate_and_extract(
    namespace: Namespace,
    qualified: &str,
    valid_local: fn(&str) -> bool,
) -> Result<QualName, DomError> {
    let (prefix, local) = match qualified.split_once(':') {
        Some((p, l)) => (Some(p), l),
        None => (None, qualified),
    };
    if prefix.is_some_and(|p| {
        p.is_empty()
            || p.bytes()
                .any(|b| b.is_ascii_whitespace() || matches!(b, 0 | b'/' | b'>'))
    }) || !valid_local(local)
    {
        return Err(DomError::InvalidCharacter);
    }
    let xmlns = Namespace::from("http://www.w3.org/2000/xmlns/");
    let bad = (prefix.is_some() && namespace == ns!())
        || (prefix == Some("xml") && namespace != ns!(xml))
        || ((qualified == "xmlns" || prefix == Some("xmlns")) && namespace != xmlns)
        || (namespace == xmlns && qualified != "xmlns" && prefix != Some("xmlns"));
    if bad {
        return Err(DomError::Namespace);
    }
    Ok(QualName::new(
        prefix.map(Prefix::from),
        namespace,
        LocalName::from(local),
    ))
}
