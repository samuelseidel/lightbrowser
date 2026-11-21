//! Node storage: ids, the compact node struct and per-kind data.

use bitflags::bitflags;
use html5ever::tendril::StrTendril;
use html5ever::{Attribute, QualName, ns};

/// Index of a node in its document's arena.
///
/// Ids are only meaningful for the [`crate::Document`] that created them.
/// Nodes are never freed individually, so an id stays valid for the
/// document's whole lifetime.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct NodeId(pub u32);

impl NodeId {
    /// Arena index as `usize`.
    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// "No node" marker for the packed links.
pub(crate) const NONE: u32 = u32::MAX;

#[inline]
pub(crate) fn link(id: Option<NodeId>) -> u32 {
    id.map_or(NONE, |n| n.0)
}

#[inline]
pub(crate) fn unlink(raw: u32) -> Option<NodeId> {
    (raw != NONE).then_some(NodeId(raw))
}

bitflags! {
    /// Per-node flags. The DOM sets the parser bits; the page and binding
    /// layers own the rest. Selector matching reads the state bits
    /// (`:hover`, `:focus`, `:checked`, ...), so the page layer must keep
    /// them current (e.g. `HOVER` on the hovered element and its ancestors).
    #[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
    pub struct NodeFlags: u32 {
        /// Created by the document parser (not by script or fragment parsing).
        const PARSER_INSERTED = 1 << 0;
        /// A `<script>` that must not run again ("already started").
        const ALREADY_STARTED = 1 << 1;
        /// At least one event listener is registered on this node.
        const HAS_LISTENERS = 1 << 2;
        /// A JS wrapper object exists for this node.
        const HAS_WRAPPER = 1 << 3;
        /// Style for this element (or a descendant) needs recomputing.
        const STYLE_DIRTY = 1 << 4;
        /// A `<template>`'s content fragment.
        const TEMPLATE_CONTENTS = 1 << 5;
        /// A form-associated custom element.
        const FORM_ASSOCIATED = 1 << 6;
        /// A form-associated custom element whose validity is not valid
        /// (`:invalid`).
        const CUSTOM_INVALID = 1 << 7;
        /// `:hover`.
        const HOVER = 1 << 8;
        /// `:active`.
        const ACTIVE = 1 << 9;
        /// `:focus`.
        const FOCUS = 1 << 10;
        /// `:focus-visible`.
        const FOCUS_VISIBLE = 1 << 11;
        /// `:focus-within`.
        const FOCUS_WITHIN = 1 << 12;
        /// `:target`.
        const TARGET = 1 << 13;
        /// Checkedness set by script/user; used when `CHECKED_DIRTY` is set,
        /// otherwise `:checked` reads the `checked`/`selected` attribute.
        const CHECKED = 1 << 14;
        /// See `CHECKED`.
        const CHECKED_DIRTY = 1 << 15;
        /// All selector-visible state bits (`HOVER` ..= `CHECKED_DIRTY`).
        /// Changing any of them bumps [`crate::Document::render_generation`].
        const STATE_MASK = 0xff << 8;
        /// Free for the binding/page layers.
        const USER_0 = 1 << 24;
        /// Free for the binding/page layers.
        const USER_1 = 1 << 25;
        /// Free for the binding/page layers.
        const USER_2 = 1 << 26;
        /// Free for the binding/page layers.
        const USER_3 = 1 << 27;
    }
}

/// DOM `nodeType` values.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(u16)]
pub enum NodeType {
    Element = 1,
    Text = 3,
    ProcessingInstruction = 7,
    Comment = 8,
    Document = 9,
    DocumentType = 10,
    DocumentFragment = 11,
}

/// One arena slot. Tree links are packed `u32` ids; element data lives in a
/// side table so the node stays small (see the `size_of` test).
pub(crate) struct Node {
    pub(crate) parent: u32,
    pub(crate) first_child: u32,
    pub(crate) last_child: u32,
    pub(crate) prev_sibling: u32,
    pub(crate) next_sibling: u32,
    pub(crate) flags: NodeFlags,
    pub(crate) data: NodeData,
}

impl Node {
    pub(crate) fn new(data: NodeData) -> Self {
        Node {
            parent: NONE,
            first_child: NONE,
            last_child: NONE,
            prev_sibling: NONE,
            next_sibling: NONE,
            flags: NodeFlags::empty(),
            data,
        }
    }
}

/// Kind-specific node payload.
pub(crate) enum NodeData {
    Document,
    Doctype(Box<Doctype>),
    /// Index into `Document::elements`.
    Element(u32),
    Text(StrTendril),
    Comment(StrTendril),
    ProcessingInstruction(Box<ProcessingInstruction>),
    DocumentFragment,
    /// A shadow root (a `DocumentFragment` to scripts). Its `parent` link
    /// stays empty: the host is in [`crate::ShadowRootData::host`].
    ShadowRoot(Box<crate::shadow::ShadowRootData>),
}

/// `<!DOCTYPE>` data.
#[derive(Clone, Debug, Default)]
pub struct Doctype {
    pub name: StrTendril,
    pub public_id: StrTendril,
    pub system_id: StrTendril,
}

/// Processing instruction data (only created by XML-ish input or script).
#[derive(Clone, Debug)]
pub struct ProcessingInstruction {
    pub target: StrTendril,
    pub data: StrTendril,
}

/// An element's custom element state (HTML "custom element state").
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum CustomState {
    #[default]
    Uncustomized,
    Undefined,
    Failed,
    Precustomized,
    Custom,
}

impl CustomState {
    /// `:defined` matches.
    pub fn is_defined(self) -> bool {
        matches!(self, CustomState::Uncustomized | CustomState::Custom)
    }
}

/// Element payload, stored in the document's element side table.
#[derive(Clone, Debug)]
pub struct ElementData {
    /// Namespace, prefix and local name (interned atoms).
    pub name: QualName,
    /// Attributes in insertion order.
    pub attrs: Vec<Attribute>,
    /// Content fragment of a `<template>`.
    pub template_contents: Option<NodeId>,
    /// Shadow root hosted by this element.
    pub shadow_root: Option<NodeId>,
    /// Custom element state (HTML).
    pub custom: CustomState,
    /// The `is` value of a customized built-in.
    pub is_value: Option<html5ever::LocalName>,
    /// MathML `annotation-xml` HTML integration point (parser detail).
    pub mathml_annotation_xml_integration_point: bool,
}

impl ElementData {
    pub(crate) fn new(name: QualName, attrs: Vec<Attribute>) -> Self {
        ElementData {
            name,
            attrs,
            template_contents: None,
            shadow_root: None,
            custom: CustomState::Uncustomized,
            is_value: None,
            mathml_annotation_xml_integration_point: false,
        }
    }

    /// True for elements in the HTML namespace.
    #[inline]
    pub fn is_html(&self) -> bool {
        self.name.ns == ns!(html)
    }

    /// Value of the no-namespace attribute `local`, if present.
    pub fn attr(&self, local: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.name.ns == ns!() && &*a.name.local == local)
            .map(|a| &*a.value)
    }
}

/// Qualified name of an element or attribute as a string (`prefix:local`).
pub(crate) fn qualified_name(name: &QualName) -> String {
    match &name.prefix {
        Some(p) => format!("{}:{}", &**p, &*name.local),
        None => name.local.to_string(),
    }
}

/// True if `name` written as `prefix:local` equals `q`.
pub(crate) fn qualified_name_eq(name: &QualName, q: &str) -> bool {
    match &name.prefix {
        None => &*name.local == q,
        Some(p) => {
            let (p2, l2) = match q.split_once(':') {
                Some(x) => x,
                None => return false,
            };
            &**p == p2 && &*name.local == l2
        }
    }
}
