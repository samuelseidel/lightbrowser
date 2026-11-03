//! Accessibility tree: roles (implicit HTML roles and `role`), accessible
//! names (a subset of accname 1.2), states, in document order. Backs CDP
//! `Accessibility.getFullAXTree` / `queryAXTree` (Playwright `getByRole`).
//!
//! Role names follow Chrome's CDP output (`RootWebArea`, `StaticText`,
//! `generic`, `heading`, `textbox`, ...).

use std::collections::HashSet;

use lb_css::{Display, StyleEngine, Visibility};
use lb_dom::{Document, NodeFlags, NodeId, NodeType, ns};

/// `aria-checked` style tri-state.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Checked {
    False,
    True,
    Mixed,
}

/// States and properties of an accessibility node.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AxStates {
    pub disabled: bool,
    pub checked: Option<Checked>,
    pub expanded: Option<bool>,
    pub selected: Option<bool>,
    pub pressed: Option<bool>,
    pub required: bool,
    pub readonly: bool,
    pub focusable: bool,
    pub multiline: bool,
    /// Heading level (1-6, or `aria-level`).
    pub level: Option<u32>,
}

/// One node of the accessibility tree.
#[derive(Clone, Debug, PartialEq)]
pub struct AxNode {
    /// Index in [`AxTree::nodes`]; stable for one build.
    pub id: usize,
    /// DOM node (`None` never happens today; kept for anonymous nodes).
    pub dom: Option<NodeId>,
    pub role: String,
    pub name: String,
    pub description: String,
    pub value: Option<String>,
    pub states: AxStates,
    /// Not exposed to assistive tech: hidden, or a role-less generic.
    pub ignored: bool,
    /// Hidden (`display: none`, `visibility: hidden`, `aria-hidden`).
    pub hidden: bool,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}

/// The tree, root first, in document order.
#[derive(Clone, Debug, Default)]
pub struct AxTree {
    pub nodes: Vec<AxNode>,
}

impl AxTree {
    /// Builds the tree for `doc` (hidden subtrees are kept but marked).
    pub fn build(doc: &Document, styles: &mut StyleEngine) -> AxTree {
        let mut b = AxBuilder {
            doc,
            styles,
            nodes: Vec::new(),
        };
        let root = b.push(AxNode {
            id: 0,
            dom: Some(Document::DOCUMENT),
            role: "RootWebArea".into(),
            name: doc.title(),
            description: String::new(),
            value: None,
            states: AxStates {
                focusable: true,
                ..Default::default()
            },
            ignored: false,
            hidden: false,
            parent: None,
            children: Vec::new(),
        });
        if let Some(html) = doc.document_element() {
            b.walk(html, root, false);
        }
        AxTree { nodes: b.nodes }
    }

    pub fn root(&self) -> &AxNode {
        &self.nodes[0]
    }

    /// The node for a DOM node.
    pub fn node_for(&self, dom: NodeId) -> Option<&AxNode> {
        self.nodes.iter().find(|n| n.dom == Some(dom))
    }

    /// Non-ignored nodes matching `role` and `name` (exact, after trimming)
    /// in document order, as `Accessibility.queryAXTree` does.
    pub fn query(&self, role: Option<&str>, name: Option<&str>) -> Vec<&AxNode> {
        self.nodes
            .iter()
            .filter(|n| !n.ignored)
            .filter(|n| role.is_none_or(|r| n.role == r))
            .filter(|n| name.is_none_or(|nm| n.name == nm.trim()))
            .collect()
    }

    /// Like [`AxTree::query`] restricted to the subtree of `scope`.
    pub fn query_in(&self, scope: usize, role: Option<&str>, name: Option<&str>) -> Vec<&AxNode> {
        let mut ids = Vec::new();
        let mut stack = vec![scope];
        while let Some(i) = stack.pop() {
            ids.push(i);
            stack.extend(self.nodes[i].children.iter().rev());
        }
        ids.sort_unstable();
        ids.into_iter()
            .map(|i| &self.nodes[i])
            .filter(|n| !n.ignored)
            .filter(|n| role.is_none_or(|r| n.role == r))
            .filter(|n| name.is_none_or(|nm| n.name == nm.trim()))
            .collect()
    }
}

struct AxBuilder<'a> {
    doc: &'a Document,
    styles: &'a mut StyleEngine,
    nodes: Vec<AxNode>,
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl AxBuilder<'_> {
    fn push(&mut self, mut n: AxNode) -> usize {
        let id = self.nodes.len();
        n.id = id;
        if let Some(p) = n.parent {
            self.nodes[p].children.push(id);
        }
        self.nodes.push(n);
        id
    }

    fn local(&self, el: NodeId) -> String {
        self.doc
            .element(el)
            .filter(|e| e.name.ns == ns!(html))
            .map(|e| e.name.local.to_string())
            .unwrap_or_default()
    }

    fn attr(&self, el: NodeId, name: &str) -> Option<&str> {
        self.doc.get_attribute(el, name)
    }

    fn is_hidden(&mut self, el: NodeId) -> bool {
        if self
            .attr(el, "aria-hidden")
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
        {
            return true;
        }
        match self.styles.computed_style(self.doc, el) {
            Some(s) => s.display == Display::None || s.visibility != Visibility::Visible,
            None => true,
        }
    }

    fn walk(&mut self, el: NodeId, parent: usize, parent_hidden: bool) {
        let hidden = parent_hidden || self.is_hidden(el);
        let role = self.role(el);
        let ignored = hidden
            || matches!(role.as_str(), "generic" | "none" | "presentation")
                && self.attr(el, "aria-label").is_none()
                && !self.focusable(el);
        let name = if hidden {
            String::new()
        } else {
            self.name(el, &role)
        };
        let description = if hidden {
            String::new()
        } else {
            self.description(el, &name)
        };
        let states = self.states(el, &role);
        let value = self.value(el, &role);
        let id = self.push(AxNode {
            id: 0,
            dom: Some(el),
            role,
            name,
            description,
            value,
            states,
            ignored,
            hidden,
            parent: Some(parent),
            children: Vec::new(),
        });
        let local = self.local(el);
        if matches!(local.as_str(), "script" | "style" | "template" | "head") {
            return;
        }
        let kids: Vec<NodeId> = self.doc.children(el).collect();
        for c in kids {
            match self.doc.node_type(c) {
                NodeType::Element => self.walk(c, id, hidden),
                NodeType::Text => {
                    let text = collapse_ws(self.doc.character_data(c).unwrap_or(""));
                    if text.is_empty() {
                        continue;
                    }
                    self.push(AxNode {
                        id: 0,
                        dom: Some(c),
                        role: "StaticText".into(),
                        name: text,
                        description: String::new(),
                        value: None,
                        states: AxStates::default(),
                        ignored: hidden,
                        hidden,
                        parent: Some(id),
                        children: Vec::new(),
                    });
                }
                _ => {}
            }
        }
    }

    fn focusable(&self, el: NodeId) -> bool {
        let local = self.local(el);
        if self.attr(el, "tabindex").is_some() {
            return true;
        }
        if self.attr(el, "disabled").is_some() {
            return false;
        }
        match local.as_str() {
            "a" | "area" => self.attr(el, "href").is_some(),
            "button" | "select" | "textarea" | "iframe" | "summary" => true,
            "input" => self
                .attr(el, "type")
                .is_none_or(|t| !t.eq_ignore_ascii_case("hidden")),
            _ => self
                .attr(el, "contenteditable")
                .is_some_and(|v| !v.eq_ignore_ascii_case("false")),
        }
    }

    /// Explicit `role` (first known token) or the implicit HTML role.
    fn role(&self, el: NodeId) -> String {
        if let Some(r) = self.attr(el, "role") {
            for tok in r.split_ascii_whitespace() {
                let t = tok.to_ascii_lowercase();
                if KNOWN_ROLES.contains(&t.as_str()) {
                    return match t.as_str() {
                        "presentation" => "none".into(),
                        "img" => "image".into(),
                        _ => t,
                    };
                }
            }
        }
        let local = self.local(el);
        let has = |n: &str| self.attr(el, n).is_some();
        let in_sectioning = || {
            self.doc.ancestors(el).any(|a| {
                matches!(
                    self.local(a).as_str(),
                    "article" | "aside" | "main" | "nav" | "section"
                )
            })
        };
        let r = match local.as_str() {
            "a" | "area" => {
                if has("href") {
                    "link"
                } else {
                    "generic"
                }
            }
            "article" => "article",
            "aside" => "complementary",
            "blockquote" => "blockquote",
            "body" => "generic",
            "button" => "button",
            "caption" => "caption",
            "code" => "code",
            "datalist" => "listbox",
            "dd" => "definition",
            "del" => "deletion",
            "details" => "group",
            "dfn" => "term",
            "dialog" => "dialog",
            "dt" => "term",
            "em" => "emphasis",
            "fieldset" => "group",
            "figure" => "figure",
            "footer" => {
                if in_sectioning() {
                    "generic"
                } else {
                    "contentinfo"
                }
            }
            "form" => "form",
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading",
            "header" => {
                if in_sectioning() {
                    "generic"
                } else {
                    "banner"
                }
            }
            "hr" => "separator",
            "html" => "generic",
            "iframe" => "Iframe",
            "img" => {
                if self.attr(el, "alt").is_some_and(str::is_empty) {
                    "none"
                } else {
                    "image"
                }
            }
            "input" => {
                let ty = self.attr(el, "type").unwrap_or("text").to_ascii_lowercase();
                match ty.as_str() {
                    "button" | "submit" | "reset" | "image" => "button",
                    "checkbox" => "checkbox",
                    "radio" => "radio",
                    "range" => "slider",
                    "number" => "spinbutton",
                    "search" => {
                        if has("list") {
                            "combobox"
                        } else {
                            "searchbox"
                        }
                    }
                    "hidden" => "none",
                    "color" => "ColorWell",
                    "date" | "datetime-local" | "month" | "time" | "week" => "Date",
                    "file" => "button",
                    _ => {
                        if has("list") {
                            "combobox"
                        } else {
                            "textbox"
                        }
                    }
                }
            }
            "ins" => "insertion",
            "label" => "LabelText",
            "legend" => "legend",
            "li" => "listitem",
            "main" => "main",
            "mark" => "mark",
            "math" => "math",
            "menu" | "ol" | "ul" => "list",
            "meter" => "meter",
            "nav" => "navigation",
            "optgroup" => "group",
            "option" => "option",
            "output" => "status",
            "p" => "paragraph",
            "pre" => "generic",
            "progress" => "progressbar",
            "search" => "search",
            "section" => {
                if has("aria-label") || has("aria-labelledby") || has("title") {
                    "region"
                } else {
                    "generic"
                }
            }
            "select" => {
                if has("multiple")
                    || self
                        .attr(el, "size")
                        .and_then(|s| s.parse::<u32>().ok())
                        .is_some_and(|s| s > 1)
                {
                    "listbox"
                } else {
                    "combobox"
                }
            }
            "strong" => "strong",
            "sub" => "subscript",
            "sup" => "superscript",
            "summary" => "DisclosureTriangle",
            "svg" => "image",
            "table" => "table",
            "tbody" | "thead" | "tfoot" => "rowgroup",
            "td" => "cell",
            "textarea" => "textbox",
            "th" => {
                if self
                    .attr(el, "scope")
                    .is_some_and(|s| s.eq_ignore_ascii_case("row"))
                {
                    "rowheader"
                } else {
                    "columnheader"
                }
            }
            "time" => "time",
            "tr" => "row",
            "br" => "LineBreak",
            _ => "generic",
        };
        r.into()
    }

    /// Accessible name.
    fn name(&mut self, el: NodeId, role: &str) -> String {
        let mut visited = HashSet::new();
        self.name_inner(el, role, &mut visited, false)
    }

    fn name_inner(
        &mut self,
        el: NodeId,
        role: &str,
        visited: &mut HashSet<NodeId>,
        in_traversal: bool,
    ) -> String {
        if !visited.insert(el) {
            return String::new();
        }
        // 2B: aria-labelledby (not when already following a reference).
        if !in_traversal && let Some(ids) = self.attr(el, "aria-labelledby").map(str::to_owned) {
            let parts: Vec<String> = ids
                .split_ascii_whitespace()
                .filter_map(|id| self.doc.get_element_by_id(id))
                .map(|r| {
                    let rr = self.role(r);
                    self.name_inner(r, &rr, visited, true)
                })
                .filter(|s| !s.is_empty())
                .collect();
            if !parts.is_empty() {
                return collapse_ws(&parts.join(" "));
            }
        }
        // 2C: aria-label.
        if let Some(l) = self.attr(el, "aria-label") {
            let l = collapse_ws(l);
            if !l.is_empty() {
                return l;
            }
        }
        // 2D: native attributes and elements.
        let local = self.local(el);
        let native = self.native_name(el, &local);
        if let Some(n) = native.filter(|n| !n.is_empty()) {
            return n;
        }
        // 2F: name from content.
        if in_traversal || NAME_FROM_CONTENT.contains(&role) {
            let t = self.text_alternative(el, visited);
            if !t.is_empty() {
                return t;
            }
        }
        // 2I: tooltip.
        if let Some(t) = self.attr(el, "title") {
            return collapse_ws(t);
        }
        if matches!(local.as_str(), "input" | "textarea")
            && let Some(p) = self.attr(el, "placeholder")
        {
            return collapse_ws(p);
        }
        String::new()
    }

    fn native_name(&mut self, el: NodeId, local: &str) -> Option<String> {
        match local {
            "input" | "textarea" | "select" | "meter" | "progress" | "output" => {
                let ty = self.attr(el, "type").unwrap_or("").to_ascii_lowercase();
                if local == "input" {
                    match ty.as_str() {
                        "button" | "submit" | "reset" => {
                            return Some(match self.attr(el, "value") {
                                Some(v) => v.to_owned(),
                                None => match ty.as_str() {
                                    "submit" => "Submit".into(),
                                    "reset" => "Reset".into(),
                                    _ => String::new(),
                                },
                            });
                        }
                        "image" => {
                            return self
                                .attr(el, "alt")
                                .or(self.attr(el, "value"))
                                .map(str::to_owned)
                                .or_else(|| Some("Submit".into()));
                        }
                        _ => {}
                    }
                }
                let labels = self.labels(el);
                if labels.is_empty() {
                    return None;
                }
                let mut visited = HashSet::from([el]);
                let parts: Vec<String> = labels
                    .into_iter()
                    .map(|l| self.text_alternative(l, &mut visited))
                    .filter(|s| !s.is_empty())
                    .collect();
                Some(collapse_ws(&parts.join(" ")))
            }
            "img" | "area" => self.attr(el, "alt").map(collapse_ws),
            "fieldset" => self.first_child_named(el, "legend").map(|l| {
                let mut v = HashSet::new();
                self.text_alternative(l, &mut v)
            }),
            "figure" => self.first_child_named(el, "figcaption").map(|l| {
                let mut v = HashSet::new();
                self.text_alternative(l, &mut v)
            }),
            "table" => self.first_child_named(el, "caption").map(|l| {
                let mut v = HashSet::new();
                self.text_alternative(l, &mut v)
            }),
            "svg" => self
                .first_child_named_any_ns(el, "title")
                .and_then(|t| self.doc.text_content(t))
                .map(|t| collapse_ws(&t)),
            _ => None,
        }
    }

    fn first_child_named(&self, el: NodeId, name: &str) -> Option<NodeId> {
        self.doc
            .element_children(el)
            .find(|&c| self.local(c) == name)
    }

    fn first_child_named_any_ns(&self, el: NodeId, name: &str) -> Option<NodeId> {
        self.doc
            .element_children(el)
            .find(|&c| self.doc.local_name(c).is_some_and(|l| &**l == name))
    }

    /// `<label for=id>` elements plus an ancestor `<label>`.
    fn labels(&self, el: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        if let Some(id) = self.doc.element_id(el).filter(|s| !s.is_empty()) {
            for n in self.doc.descendants(Document::DOCUMENT) {
                if self.local(n) == "label" && self.attr(n, "for") == Some(id) {
                    out.push(n);
                }
            }
        }
        if let Some(anc) = self
            .doc
            .ancestors(el)
            .find(|&a| self.doc.is_element(a) && self.local(a) == "label")
            && self.attr(anc, "for").is_none()
            && !out.contains(&anc)
        {
            out.push(anc);
        }
        out
    }

    /// Text alternative of a subtree (accname 2F): text nodes and the
    /// names of embedded controls, skipping hidden nodes.
    fn text_alternative(&mut self, el: NodeId, visited: &mut HashSet<NodeId>) -> String {
        let mut out = String::new();
        let kids: Vec<NodeId> = self.doc.children(el).collect();
        for c in kids {
            match self.doc.node_type(c) {
                NodeType::Text => out.push_str(self.doc.character_data(c).unwrap_or("")),
                NodeType::Element => {
                    if self.is_hidden(c) || visited.contains(&c) {
                        continue;
                    }
                    let local = self.local(c);
                    if matches!(local.as_str(), "script" | "style" | "template") {
                        continue;
                    }
                    let block = self
                        .styles
                        .computed_style(self.doc, c)
                        .is_some_and(|s| !s.display.is_inline_level());
                    let role = self.role(c);
                    let part = match local.as_str() {
                        // Embedded controls contribute their value.
                        "input" if !matches!(role.as_str(), "button" | "checkbox" | "radio") => {
                            self.attr(c, "value").unwrap_or("").to_owned()
                        }
                        "select" => self
                            .doc
                            .descendants(c)
                            .find(|&o| {
                                self.local(o) == "option" && self.attr(o, "selected").is_some()
                            })
                            .or_else(|| {
                                self.doc.descendants(c).find(|&o| self.local(o) == "option")
                            })
                            .and_then(|o| self.doc.text_content(o))
                            .unwrap_or_default(),
                        "textarea" => self.doc.text_content(c).unwrap_or_default(),
                        _ => self.name_inner(c, &role, visited, true),
                    };
                    if block {
                        out.push(' ');
                    }
                    out.push_str(&part);
                    if block {
                        out.push(' ');
                    }
                }
                _ => {}
            }
        }
        collapse_ws(&out)
    }

    fn description(&mut self, el: NodeId, name: &str) -> String {
        if let Some(ids) = self.attr(el, "aria-describedby").map(str::to_owned) {
            let mut visited = HashSet::new();
            let parts: Vec<String> = ids
                .split_ascii_whitespace()
                .filter_map(|id| self.doc.get_element_by_id(id))
                .map(|r| self.text_alternative(r, &mut visited))
                .filter(|s| !s.is_empty())
                .collect();
            if !parts.is_empty() {
                return parts.join(" ");
            }
        }
        if let Some(d) = self.attr(el, "aria-description") {
            return collapse_ws(d);
        }
        match self.attr(el, "title") {
            Some(t) if collapse_ws(t) != name => collapse_ws(t),
            _ => String::new(),
        }
    }

    fn states(&self, el: NodeId, role: &str) -> AxStates {
        let local = self.local(el);
        let attr_true = |n: &str| {
            self.attr(el, n)
                .is_some_and(|v| v.eq_ignore_ascii_case("true"))
        };
        let form_control = matches!(
            local.as_str(),
            "button" | "input" | "select" | "textarea" | "option" | "optgroup" | "fieldset"
        );
        let disabled_by_fieldset = form_control
            && self.doc.ancestors(el).any(|a| {
                self.doc.is_element(a)
                    && self.local(a) == "fieldset"
                    && self.attr(a, "disabled").is_some()
            });
        let mut s = AxStates {
            disabled: attr_true("aria-disabled")
                || (form_control && self.attr(el, "disabled").is_some())
                || disabled_by_fieldset,
            required: attr_true("aria-required") || self.attr(el, "required").is_some(),
            readonly: attr_true("aria-readonly") || self.attr(el, "readonly").is_some(),
            focusable: self.focusable(el),
            multiline: local == "textarea" || attr_true("aria-multiline"),
            ..Default::default()
        };
        let tri = |v: &str| match v.to_ascii_lowercase().as_str() {
            "true" => Some(Checked::True),
            "false" => Some(Checked::False),
            "mixed" => Some(Checked::Mixed),
            _ => None,
        };
        if matches!(
            role,
            "checkbox" | "radio" | "switch" | "menuitemcheckbox" | "menuitemradio"
        ) {
            s.checked = if local == "input" {
                let flags = self.doc.flags(el);
                let on = if flags.contains(NodeFlags::CHECKED_DIRTY) {
                    flags.contains(NodeFlags::CHECKED)
                } else {
                    self.attr(el, "checked").is_some()
                };
                Some(if on { Checked::True } else { Checked::False })
            } else {
                Some(
                    self.attr(el, "aria-checked")
                        .and_then(tri)
                        .unwrap_or(Checked::False),
                )
            };
        }
        if let Some(p) = self.attr(el, "aria-pressed") {
            s.pressed = tri(p).map(|c| c == Checked::True);
        }
        if let Some(e) = self.attr(el, "aria-expanded") {
            s.expanded = Some(e.eq_ignore_ascii_case("true"));
        } else if local == "details" {
            s.expanded = Some(self.attr(el, "open").is_some());
        } else if local == "summary"
            && let Some(d) = self.doc.parent_element(el)
            && self.local(d) == "details"
        {
            s.expanded = Some(self.attr(d, "open").is_some());
        }
        if let Some(v) = self.attr(el, "aria-selected") {
            s.selected = Some(v.eq_ignore_ascii_case("true"));
        } else if local == "option" {
            s.selected = Some(self.attr(el, "selected").is_some());
        }
        if role == "heading" {
            s.level = self
                .attr(el, "aria-level")
                .and_then(|l| l.parse().ok())
                .or_else(|| local.strip_prefix('h').and_then(|n| n.parse().ok()))
                .or(Some(2));
        }
        s
    }

    fn value(&self, el: NodeId, role: &str) -> Option<String> {
        let local = self.local(el);
        match role {
            "textbox" | "searchbox" | "combobox" | "spinbutton" | "slider" => {
                if local == "textarea" {
                    return self.doc.text_content(el);
                }
                if local == "select" {
                    return self
                        .doc
                        .descendants(el)
                        .find(|&o| self.local(o) == "option" && self.attr(o, "selected").is_some())
                        .or_else(|| {
                            self.doc
                                .descendants(el)
                                .find(|&o| self.local(o) == "option")
                        })
                        .and_then(|o| self.doc.text_content(o))
                        .map(|t| collapse_ws(&t));
                }
                Some(
                    self.attr(el, "aria-valuenow")
                        .or(self.attr(el, "value"))
                        .unwrap_or("")
                        .to_owned(),
                )
            }
            "progressbar" | "meter" => self.attr(el, "value").map(str::to_owned),
            _ => None,
        }
    }
}

/// Roles whose name can come from content (ARIA 1.2 "name from contents").
const NAME_FROM_CONTENT: &[&str] = &[
    "button",
    "cell",
    "checkbox",
    "columnheader",
    "gridcell",
    "heading",
    "link",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "radio",
    "row",
    "rowheader",
    "switch",
    "tab",
    "tooltip",
    "treeitem",
    "legend",
    "caption",
    "LabelText",
    "DisclosureTriangle",
    "term",
    "code",
    "emphasis",
    "strong",
    "deletion",
    "insertion",
    "mark",
    "subscript",
    "superscript",
    "time",
];

/// ARIA roles accepted in the `role` attribute.
const KNOWN_ROLES: &[&str] = &[
    "alert",
    "alertdialog",
    "application",
    "article",
    "banner",
    "blockquote",
    "button",
    "caption",
    "cell",
    "checkbox",
    "code",
    "columnheader",
    "combobox",
    "complementary",
    "contentinfo",
    "definition",
    "deletion",
    "dialog",
    "directory",
    "document",
    "emphasis",
    "feed",
    "figure",
    "form",
    "generic",
    "grid",
    "gridcell",
    "group",
    "heading",
    "img",
    "image",
    "insertion",
    "link",
    "list",
    "listbox",
    "listitem",
    "log",
    "main",
    "mark",
    "marquee",
    "math",
    "menu",
    "menubar",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "meter",
    "navigation",
    "none",
    "note",
    "option",
    "paragraph",
    "presentation",
    "progressbar",
    "radio",
    "radiogroup",
    "region",
    "row",
    "rowgroup",
    "rowheader",
    "scrollbar",
    "search",
    "searchbox",
    "separator",
    "slider",
    "spinbutton",
    "status",
    "strong",
    "subscript",
    "superscript",
    "switch",
    "tab",
    "table",
    "tablist",
    "tabpanel",
    "term",
    "textbox",
    "time",
    "timer",
    "toolbar",
    "tooltip",
    "tree",
    "treegrid",
    "treeitem",
];
