//! Shadow trees: shadow roots, slot assignment and the flat tree.
//!
//! A shadow root is an arena node of its host's document with
//! [`ShadowRootData`]; its `parent` link stays empty, so light-tree walks
//! ([`Document::descendants`], `querySelector`) never enter it, as in the
//! DOM. Slot assignment is computed on demand from the tree ("find a slot",
//! "find slottables"); only manual assignments (`slot.assign()`) are
//! stored. The page layer compares assignments before and after a change to
//! fire `slotchange`.

use html5ever::{local_name, ns};

use crate::document::Document;
use crate::node::{NodeData, NodeId};

/// `ShadowRootMode`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ShadowRootMode {
    Open,
    Closed,
}

/// `SlotAssignmentMode`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SlotAssignment {
    Named,
    Manual,
}

/// The state of one shadow root.
#[derive(Clone, Debug)]
pub struct ShadowRootData {
    pub host: NodeId,
    pub mode: ShadowRootMode,
    pub delegates_focus: bool,
    pub slot_assignment: SlotAssignment,
    pub clonable: bool,
    pub serializable: bool,
    /// Made by `<template shadowrootmode>` (and not yet claimed by
    /// `attachShadow`).
    pub declarative: bool,
    /// `ElementInternals.shadowRoot` may return it.
    pub available_to_internals: bool,
    /// `customElementRegistry` was given as null (scoped registries).
    pub keep_custom_element_registry_null: bool,
}

/// `ShadowRootInit` as the binding layer passes it.
#[derive(Clone, Debug)]
pub struct ShadowRootInit {
    pub mode: ShadowRootMode,
    pub delegates_focus: bool,
    pub slot_assignment: SlotAssignment,
    pub clonable: bool,
    pub serializable: bool,
}

/// Why `attachShadow` failed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AttachShadowError {
    /// Not an HTML element that may host a shadow root.
    NotSupportedElement,
    /// Already a host (and the root is not a declarative one to reuse).
    AlreadyHost,
}

/// Local names of HTML elements `attachShadow` accepts, besides valid
/// custom element names.
const SHADOW_HOSTS: &[&str] = &[
    "article",
    "aside",
    "blockquote",
    "body",
    "div",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "main",
    "nav",
    "p",
    "section",
    "span",
];

/// "Valid custom element name" (HTML).
pub fn is_valid_custom_element_name(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "annotation-xml",
        "color-profile",
        "font-face",
        "font-face-src",
        "font-face-uri",
        "font-face-format",
        "font-face-name",
        "missing-glyph",
    ];
    let b = name.as_bytes();
    if b.is_empty() || !b[0].is_ascii_lowercase() || !name.contains('-') {
        return false;
    }
    if RESERVED.contains(&name) {
        return false;
    }
    name.chars().skip(1).all(|c| {
        matches!(c, '-' | '.' | '0'..='9' | '_' | 'a'..='z' | '\u{B7}')
            || ('\u{C0}'..='\u{D6}').contains(&c)
            || ('\u{D8}'..='\u{F6}').contains(&c)
            || ('\u{F8}'..='\u{37D}').contains(&c)
            || ('\u{37F}'..='\u{1FFF}').contains(&c)
            || ('\u{200C}'..='\u{200D}').contains(&c)
            || ('\u{203F}'..='\u{2040}').contains(&c)
            || ('\u{2070}'..='\u{218F}').contains(&c)
            || ('\u{2C00}'..='\u{2FEF}').contains(&c)
            || ('\u{3001}'..='\u{D7FF}').contains(&c)
            || ('\u{F900}'..='\u{FDCF}').contains(&c)
            || ('\u{FDF0}'..='\u{FFFD}').contains(&c)
            || ('\u{10000}'..='\u{EFFFF}').contains(&c)
    })
}

impl Document {
    /// True if `id` is a shadow root.
    #[inline]
    pub fn is_shadow_root(&self, id: NodeId) -> bool {
        matches!(self.node(id).data, NodeData::ShadowRoot(_))
    }

    /// State of shadow root `id`.
    pub fn shadow_root_data(&self, id: NodeId) -> Option<&ShadowRootData> {
        match &self.node(id).data {
            NodeData::ShadowRoot(d) => Some(d),
            _ => None,
        }
    }

    /// Mutable state of shadow root `id`.
    pub fn shadow_root_data_mut(&mut self, id: NodeId) -> Option<&mut ShadowRootData> {
        match &mut self.nodes[id.index()].data {
            NodeData::ShadowRoot(d) => Some(d),
            _ => None,
        }
    }

    /// The shadow root hosted by element `id` (open or closed).
    #[inline]
    pub fn shadow_root(&self, id: NodeId) -> Option<NodeId> {
        self.element(id)?.shadow_root
    }

    /// The host of shadow root `id`.
    #[inline]
    pub fn shadow_host(&self, id: NodeId) -> Option<NodeId> {
        self.shadow_root_data(id).map(|d| d.host)
    }

    /// Parent, or the host for a shadow root (the "get the parent" of
    /// shadow-including trees, without slots).
    pub fn parent_or_host(&self, id: NodeId) -> Option<NodeId> {
        self.parent(id).or_else(|| self.shadow_host(id))
    }

    /// The shadow root `id` is in, if any.
    pub fn containing_shadow_root(&self, id: NodeId) -> Option<NodeId> {
        let r = self.root(id);
        self.is_shadow_root(r).then_some(r)
    }

    /// Shadow-including root: the root, crossing shadow roots to hosts.
    pub fn shadow_including_root(&self, id: NodeId) -> NodeId {
        let mut r = self.root(id);
        while let Some(h) = self.shadow_host(r) {
            r = self.root(h);
        }
        r
    }

    /// True if `a` is a shadow-including inclusive ancestor of `b`.
    pub fn shadow_including_contains(&self, a: NodeId, b: NodeId) -> bool {
        let mut n = Some(b);
        while let Some(x) = n {
            if x == a {
                return true;
            }
            n = self.parent_or_host(x);
        }
        false
    }

    /// `element.attachShadow(init)` minus custom element checks (the
    /// binding layer does those).
    pub fn attach_shadow(
        &mut self,
        host: NodeId,
        init: &ShadowRootInit,
    ) -> Result<NodeId, AttachShadowError> {
        let Some(e) = self.element(host) else {
            return Err(AttachShadowError::NotSupportedElement);
        };
        if e.name.ns != ns!(html) {
            return Err(AttachShadowError::NotSupportedElement);
        }
        let local = &*e.name.local;
        if !SHADOW_HOSTS.contains(&local) && !is_valid_custom_element_name(local) {
            return Err(AttachShadowError::NotSupportedElement);
        }
        if let Some(sr) = e.shadow_root {
            let d = self.shadow_root_data(sr).expect("shadow root");
            if d.declarative && d.mode == init.mode {
                // Claim the declarative root: its children go.
                while let Some(c) = self.first_child(sr) {
                    self.detach(c);
                }
                let d = self.shadow_root_data_mut(sr).expect("shadow root");
                d.declarative = false;
                return Ok(sr);
            }
            return Err(AttachShadowError::AlreadyHost);
        }
        Ok(self.make_shadow_root(host, init, false))
    }

    /// Create and link a shadow root for `host` (no checks).
    pub(crate) fn make_shadow_root(
        &mut self,
        host: NodeId,
        init: &ShadowRootInit,
        declarative: bool,
    ) -> NodeId {
        let sr = self.push_node(NodeData::ShadowRoot(Box::new(ShadowRootData {
            host,
            mode: init.mode,
            delegates_focus: init.delegates_focus,
            slot_assignment: init.slot_assignment,
            clonable: init.clonable,
            serializable: init.serializable,
            declarative,
            available_to_internals: false,
            keep_custom_element_registry_null: false,
        })));
        if let Some(e) = self.element_mut(host) {
            e.shadow_root = Some(sr);
        }
        self.bump_generation();
        sr
    }

    /// The declarative shadow root steps of the HTML parser: attach a root
    /// to `host` from a `<template shadowrootmode>` start tag. Returns the
    /// root, or `None` when the host cannot have one (the template then
    /// stays a plain template).
    pub(crate) fn attach_declarative(
        &mut self,
        host: NodeId,
        attrs: &[html5ever::Attribute],
    ) -> Option<NodeId> {
        let get = |n: &str| {
            attrs
                .iter()
                .find(|a| a.name.ns == ns!() && &*a.name.local == n)
                .map(|a| a.value.to_ascii_lowercase())
        };
        let mode = match get("shadowrootmode").as_deref() {
            Some("open") => ShadowRootMode::Open,
            Some("closed") => ShadowRootMode::Closed,
            _ => return None,
        };
        let init = ShadowRootInit {
            mode,
            delegates_focus: get("shadowrootdelegatesfocus").is_some(),
            slot_assignment: SlotAssignment::Named,
            clonable: get("shadowrootclonable").is_some(),
            serializable: get("shadowrootserializable").is_some(),
        };
        let e = self.element(host)?;
        if e.name.ns != ns!(html) {
            return None;
        }
        let local = &*e.name.local;
        if !SHADOW_HOSTS.contains(&local) && !is_valid_custom_element_name(local) {
            return None;
        }
        if e.shadow_root.is_some() {
            return None;
        }
        Some(self.make_shadow_root(host, &init, true))
    }

    // ----- slots -----

    /// True for an HTML `<slot>`.
    #[inline]
    pub fn is_slot(&self, id: NodeId) -> bool {
        self.is_html_element_named(id, &local_name!("slot"))
    }

    /// A slot's name (`name` attribute, or "").
    pub fn slot_name(&self, slot: NodeId) -> &str {
        self.get_attribute(slot, "name").unwrap_or("")
    }

    /// True for nodes that can be slotted (elements and text).
    #[inline]
    pub fn is_slottable(&self, id: NodeId) -> bool {
        matches!(self.node(id).data, NodeData::Element(_) | NodeData::Text(_))
    }

    /// Slots of a shadow tree in tree order.
    pub fn slots_in(&self, root: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.descendants(root).filter(move |&n| self.is_slot(n))
    }

    /// "Find a slot" for `slottable` (`assignedSlot` when `open_only`).
    pub fn find_slot(&self, slottable: NodeId, open_only: bool) -> Option<NodeId> {
        if !self.is_slottable(slottable) {
            return None;
        }
        let parent = self.parent(slottable)?;
        let sr = self.shadow_root(parent)?;
        let d = self.shadow_root_data(sr)?;
        if open_only && d.mode != ShadowRootMode::Open {
            return None;
        }
        if d.slot_assignment == SlotAssignment::Manual {
            let slot = *self.manual_slot_of.get(&slottable)?;
            return (self.root(slot) == sr).then_some(slot);
        }
        let name = match self.element(slottable) {
            Some(e) => e.attr("slot").unwrap_or(""),
            None => "",
        };
        self.slots_in(sr).find(|&s| self.slot_name(s) == name)
    }

    /// "Find slottables": the nodes assigned to `slot`.
    pub fn assigned_nodes(&self, slot: NodeId) -> Vec<NodeId> {
        let root = self.root(slot);
        let Some(d) = self.shadow_root_data(root) else {
            return Vec::new();
        };
        let host = d.host;
        if d.slot_assignment == SlotAssignment::Manual {
            // Manually assigned nodes whose parent is the host and for
            // which this is the first such slot.
            return self
                .manual_slots
                .get(&slot)
                .map(|v| {
                    v.iter()
                        .copied()
                        .filter(|&n| {
                            self.parent(n) == Some(host)
                                && self.manual_slot_of.get(&n) == Some(&slot)
                        })
                        .collect()
                })
                .unwrap_or_default();
        }
        // Named: only the first slot of a name gets nodes.
        let name = self.slot_name(slot);
        if self.slots_in(root).find(|&s| self.slot_name(s) == name) != Some(slot) {
            return Vec::new();
        }
        self.children(host)
            .filter(|&c| {
                self.is_slottable(c)
                    && match self.element(c) {
                        Some(e) => e.attr("slot").unwrap_or(""),
                        None => "",
                    } == name
            })
            .collect()
    }

    /// "Find flattened slottables" of `slot`.
    pub fn flattened_assigned_nodes(&self, slot: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        self.flatten_into(slot, &mut out);
        out
    }

    fn flatten_into(&self, slot: NodeId, out: &mut Vec<NodeId>) {
        if !self.is_shadow_root(self.root(slot)) {
            return;
        }
        let mut nodes = self.assigned_nodes(slot);
        if nodes.is_empty() {
            nodes = self
                .children(slot)
                .filter(|&c| self.is_slottable(c))
                .collect();
        }
        for n in nodes {
            if self.is_slot(n) && self.is_shadow_root(self.root(n)) {
                self.flatten_into(n, out);
            } else {
                out.push(n);
            }
        }
    }

    /// `slot.assign(...nodes)`: set the manually assigned nodes.
    pub fn assign_slot(&mut self, slot: NodeId, nodes: &[NodeId]) {
        // A node is assigned to at most one slot; repeated nodes keep
        // their first position.
        let mut list: Vec<NodeId> = Vec::with_capacity(nodes.len());
        for &n in nodes {
            if !list.contains(&n) {
                list.push(n);
            }
        }
        if let Some(old) = self.manual_slots.remove(&slot) {
            for n in old {
                if self.manual_slot_of.get(&n) == Some(&slot) {
                    self.manual_slot_of.remove(&n);
                }
            }
        }
        for &n in &list {
            if let Some(prev) = self.manual_slot_of.insert(n, slot)
                && prev != slot
                && let Some(v) = self.manual_slots.get_mut(&prev)
            {
                v.retain(|&x| x != n);
            }
        }
        self.manual_slots.insert(slot, list);
        self.bump_generation();
    }

    /// Children in the flat tree (for style inheritance, layout, hit
    /// testing and the accessibility tree): a host's shadow root's
    /// children; a slot's flattened assigned nodes (or its children when
    /// none are assigned); otherwise the children. Slots stay in the flat
    /// tree as elements (`display: contents` by default).
    pub fn flat_children(&self, id: NodeId) -> Vec<NodeId> {
        if let Some(sr) = self.shadow_root(id) {
            return self.children(sr).collect();
        }
        if self.is_slot(id) && self.is_shadow_root(self.root(id)) {
            let assigned = self.assigned_nodes(id);
            if !assigned.is_empty() {
                return assigned;
            }
        }
        self.children(id).collect()
    }

    /// True if `id` has a flat-tree child list different from its children.
    #[inline]
    pub fn has_flat_children(&self, id: NodeId) -> bool {
        self.shadow_root(id).is_some() || (self.is_slot(id) && self.is_shadow_root(self.root(id)))
    }

    /// Parent in the flat tree: the assigned slot for a slotted node, the
    /// host for a child of a shadow root, `None` for children of a host
    /// that are not assigned (they are not rendered).
    pub fn flat_parent(&self, id: NodeId) -> Option<NodeId> {
        let p = self.parent(id)?;
        if self.shadow_root(p).is_some() {
            return self.find_slot(id, false);
        }
        if let Some(h) = self.shadow_host(p) {
            return Some(h);
        }
        if self.is_slot(p)
            && self.is_shadow_root(self.root(p))
            && !self.assigned_nodes(p).is_empty()
        {
            // Fallback content of a slot that has assigned nodes.
            return None;
        }
        Some(p)
    }

    /// Shadow-including descendants of `id` in shadow-including tree
    /// order (a host's shadow root and its subtree come before the host's
    /// children), excluding `id`.
    pub fn shadow_including_descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = Vec::new();
        let push_kids = |stack: &mut Vec<NodeId>, n: NodeId| {
            let kids: Vec<NodeId> = self.children(n).collect();
            stack.extend(kids.into_iter().rev());
            if let Some(sr) = self.shadow_root(n) {
                stack.push(sr);
            }
        };
        push_kids(&mut stack, id);
        while let Some(n) = stack.pop() {
            out.push(n);
            push_kids(&mut stack, n);
        }
        out
    }
}
