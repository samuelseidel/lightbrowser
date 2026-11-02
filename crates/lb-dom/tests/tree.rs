//! Tree, attribute and text APIs, mutation hooks, node size.

use std::cell::RefCell;
use std::rc::Rc;

use lb_dom::{Document, DomError, Mutation, NodeFlags, NodeId, NodeType, QualName, local_name, ns};

fn el(doc: &mut Document, name: &str) -> NodeId {
    doc.create_element_by_name(name).unwrap()
}

#[test]
fn node_struct_is_compact() {
    let size = lb_dom::node_struct_size();
    println!("Node: {size} bytes");
    assert!(size <= 48, "Node grew to {size} bytes");
    assert_eq!(std::mem::size_of::<NodeId>(), 4);
}

#[test]
fn build_and_navigate() {
    let mut doc = Document::new();
    let html = el(&mut doc, "HTML");
    doc.append_child(Document::DOCUMENT, html).unwrap();
    let body = el(&mut doc, "body");
    doc.append_child(html, body).unwrap();
    let a = el(&mut doc, "a");
    let b = el(&mut doc, "b");
    let c = el(&mut doc, "c");
    doc.append_child(body, a).unwrap();
    doc.append_child(body, c).unwrap();
    doc.insert_before(body, b, Some(c)).unwrap();
    let t = doc.create_text("txt");
    doc.append_child(b, t).unwrap();

    assert_eq!(doc.local_name(html).unwrap(), &local_name!("html"));
    assert_eq!(doc.tag_name(html).unwrap(), "HTML");
    assert_eq!(doc.children(body).collect::<Vec<_>>(), [a, b, c]);
    assert_eq!(doc.first_child(body), Some(a));
    assert_eq!(doc.last_child(body), Some(c));
    assert_eq!(doc.next_sibling(a), Some(b));
    assert_eq!(doc.prev_sibling(c), Some(b));
    assert_eq!(doc.parent(t), Some(b));
    assert_eq!(doc.parent_element(html), None);
    assert_eq!(
        doc.descendants(Document::DOCUMENT).collect::<Vec<_>>(),
        [html, body, a, b, t, c]
    );
    assert_eq!(doc.inclusive_descendants(b).collect::<Vec<_>>(), [b, t]);
    assert_eq!(
        doc.ancestors(t).collect::<Vec<_>>(),
        [b, body, html, Document::DOCUMENT]
    );
    assert!(doc.is_connected(t));
    assert!(doc.contains(body, t));
    assert!(!doc.contains(t, body));
    assert_eq!(doc.body(), Some(body));
    assert_eq!(doc.owner_document(t), Some(Document::DOCUMENT));
    assert_eq!(doc.owner_document(Document::DOCUMENT), None);
    assert_eq!(doc.node_name(t), "#text");

    // Move b to the end; remove a; replace c with a new node.
    doc.append_child(body, b).unwrap();
    assert_eq!(doc.children(body).collect::<Vec<_>>(), [a, c, b]);
    doc.remove_child(body, a).unwrap();
    assert!(!doc.is_connected(a));
    assert_eq!(doc.parent(a), None);
    let d = el(&mut doc, "d");
    assert_eq!(doc.replace_child(body, d, c).unwrap(), c);
    assert_eq!(doc.children(body).collect::<Vec<_>>(), [d, b]);
    // insertBefore(node, node) keeps the position.
    doc.insert_before(body, d, Some(d)).unwrap();
    assert_eq!(doc.children(body).collect::<Vec<_>>(), [d, b]);
}

#[test]
fn hierarchy_errors() {
    let mut doc = Document::new();
    let html = el(&mut doc, "html");
    doc.append_child(Document::DOCUMENT, html).unwrap();
    let div = el(&mut doc, "div");
    doc.append_child(html, div).unwrap();
    let t = doc.create_text("x");

    assert_eq!(doc.append_child(div, html), Err(DomError::HierarchyRequest));
    assert_eq!(doc.append_child(div, div), Err(DomError::HierarchyRequest));
    assert_eq!(doc.append_child(t, div), Err(DomError::HierarchyRequest));
    assert_eq!(
        doc.append_child(div, Document::DOCUMENT),
        Err(DomError::HierarchyRequest)
    );
    assert_eq!(
        doc.append_child(Document::DOCUMENT, t),
        Err(DomError::HierarchyRequest)
    );
    let second = el(&mut doc, "body");
    assert_eq!(
        doc.append_child(Document::DOCUMENT, second),
        Err(DomError::HierarchyRequest)
    );
    let dt = doc.create_doctype("html", "", "");
    assert_eq!(doc.append_child(div, dt), Err(DomError::HierarchyRequest));
    assert_eq!(
        doc.append_child(Document::DOCUMENT, dt),
        Err(DomError::HierarchyRequest)
    );
    doc.insert_before(Document::DOCUMENT, dt, Some(html))
        .unwrap();
    let other = el(&mut doc, "p");
    assert_eq!(
        doc.insert_before(div, t, Some(other)),
        Err(DomError::NotFound)
    );
    assert_eq!(doc.remove_child(div, other), Err(DomError::NotFound));
    // Replacing the document element with another element is fine.
    doc.replace_child(Document::DOCUMENT, second, html).unwrap();
    assert_eq!(doc.document_element(), Some(second));
}

#[test]
fn fragments_insert_their_children() {
    let mut doc = Document::new();
    let div = el(&mut doc, "div");
    let frag = doc.create_document_fragment();
    let a = doc.create_text("a");
    let b = el(&mut doc, "b");
    doc.append_child(frag, a).unwrap();
    doc.append_child(frag, b).unwrap();
    doc.append_child(div, frag).unwrap();
    assert_eq!(doc.children(div).collect::<Vec<_>>(), [a, b]);
    assert_eq!(doc.child_count(frag), 0);
}

#[test]
fn text_content() {
    let mut doc = Document::parse_html("<div id=d>a<b>b<!--c-->c</b>d</div>");
    let d = doc.get_element_by_id("d").unwrap();
    assert_eq!(doc.text_content(d).unwrap(), "abcd");
    assert_eq!(doc.text_content(Document::DOCUMENT), None);
    let gen0 = doc.generation();
    doc.set_text_content(d, Some("<new>"));
    assert_eq!(doc.child_count(d), 1);
    assert_eq!(doc.inner_html(d), "&lt;new&gt;");
    assert!(doc.generation() > gen0);
    doc.set_text_content(d, Some(""));
    assert_eq!(doc.first_child(d), None);
    let t = doc.create_text("x");
    doc.set_text_content(t, Some("y"));
    assert_eq!(doc.character_data(t), Some("y"));
}

#[test]
fn attributes() {
    let mut doc = Document::parse_html("<!DOCTYPE html><div id=d class='x  y' b=2 a=1></div>");
    let d = doc.get_element_by_id("d").unwrap();
    let names = |doc: &Document| -> Vec<String> {
        doc.attributes(d)
            .iter()
            .map(|a| a.name.local.to_string())
            .collect()
    };
    assert_eq!(names(&doc), ["id", "class", "b", "a"], "source order kept");
    assert_eq!(doc.get_attribute(d, "B"), Some("2"));
    assert_eq!(doc.element_id(d), Some("d"));
    assert_eq!(doc.classes(d).collect::<Vec<_>>(), ["x", "y"]);
    assert!(doc.has_class(d, "y"));

    let gen0 = doc.generation();
    doc.set_attribute(d, "Data-New", "v").unwrap();
    doc.set_attribute(d, "b", "3").unwrap();
    assert_eq!(names(&doc), ["id", "class", "b", "a", "data-new"]);
    assert_eq!(doc.get_attribute(d, "b"), Some("3"));
    assert!(doc.generation() > gen0);
    assert!(doc.remove_attribute(d, "A"));
    assert!(!doc.remove_attribute(d, "a"));
    assert!(!doc.has_attribute(d, "a"));
    assert_eq!(
        doc.set_attribute(d, "bad name", "x"),
        Err(DomError::InvalidCharacter)
    );
    assert_eq!(
        doc.set_attribute(d, "", "x"),
        Err(DomError::InvalidCharacter)
    );

    let xlink = lb_dom::Namespace::from("http://www.w3.org/1999/xlink");
    doc.set_attribute_ns(d, xlink.clone(), "xlink:href", "#a")
        .unwrap();
    assert_eq!(doc.get_attribute_ns(d, &xlink, "href"), Some("#a"));
    assert_eq!(doc.get_attribute(d, "xlink:href"), Some("#a"));
    assert_eq!(
        doc.set_attribute_ns(d, ns!(), "p:x", "1"),
        Err(DomError::Namespace)
    );
    assert!(doc.outer_html(d).contains("xlink:href=\"#a\""));
}

#[test]
fn element_creation_rules() {
    let mut doc = Document::new();
    let e = doc.create_element_by_name("My-Element").unwrap();
    assert_eq!(&**doc.local_name(e).unwrap(), "my-element");
    assert_eq!(doc.tag_name(e).unwrap(), "MY-ELEMENT");
    assert_eq!(
        doc.create_element_by_name(""),
        Err(DomError::InvalidCharacter)
    );
    assert_eq!(
        doc.create_element_by_name("1a"),
        Err(DomError::InvalidCharacter)
    );
    assert_eq!(
        doc.create_element_by_name("a b"),
        Err(DomError::InvalidCharacter)
    );
    let svg = doc
        .create_element_ns(ns!(svg), "svg:foreignObject")
        .unwrap();
    let q: &QualName = doc.qual_name(svg).unwrap();
    assert_eq!(&*q.local, "foreignObject");
    assert_eq!(doc.tag_name(svg).unwrap(), "svg:foreignObject");
    let tpl = doc.create_element_by_name("template").unwrap();
    assert!(doc.template_contents(tpl).is_some());
}

#[test]
fn clone_and_import() {
    let mut doc =
        Document::parse_html("<div id=d a=1><p>x<b>y</b></p><template><i>t</i></template></div>");
    let d = doc.get_element_by_id("d").unwrap();
    let shallow = doc.clone_node(d, false);
    assert_eq!(doc.outer_html(shallow), "<div id=\"d\" a=\"1\"></div>");
    let deep = doc.clone_node(d, true);
    assert_eq!(doc.outer_html(deep), doc.outer_html(d));
    assert_eq!(doc.parent(deep), None);
    // The clone's template has its own contents.
    let t1 = doc.query_selector(d, "template").unwrap().unwrap();
    let t2 = doc.query_selector(deep, "template").unwrap().unwrap();
    assert_ne!(doc.template_contents(t1), doc.template_contents(t2));

    let mut other = Document::new();
    let imported = other.import_node(&doc, d, true);
    assert_eq!(other.outer_html(imported), doc.outer_html(d));
}

#[test]
fn mutation_hook_sees_changes() {
    let log: Rc<RefCell<Vec<Mutation>>> = Rc::default();
    let sink = log.clone();
    let mut doc = Document::parse_html("<div id=d><span>s</span></div>");
    let d = doc.get_element_by_id("d").unwrap();
    let span = doc.first_child(d).unwrap();
    doc.set_mutation_hook(Some(Box::new(move |m: &Mutation| {
        sink.borrow_mut().push(m.clone())
    })));

    let p = el(&mut doc, "p");
    doc.append_child(d, p).unwrap();
    doc.remove_child(d, span).unwrap();
    doc.set_attribute(d, "class", "c").unwrap();
    doc.set_attribute(d, "class", "c2").unwrap();
    let t = doc.create_text("a");
    doc.append_child(p, t).unwrap();
    doc.set_character_data(t, "b");

    let log = log.borrow();
    assert_eq!(log.len(), 6, "{log:#?}");
    assert!(matches!(
        log[0],
        Mutation::ChildInserted { parent, node, prev_sibling: Some(prev), next_sibling: None }
            if parent == d && node == p && prev == span
    ));
    assert!(matches!(
        log[1],
        Mutation::ChildRemoved { parent, node, prev_sibling: None, next_sibling: Some(next) }
            if parent == d && node == span && next == p
    ));
    assert!(matches!(
        &log[2],
        Mutation::Attribute {
            old_value: None,
            ..
        }
    ));
    assert!(matches!(&log[3], Mutation::Attribute { old_value: Some(v), .. } if &**v == "c"));
    assert!(matches!(&log[5], Mutation::CharacterData { old_value, .. } if &**old_value == "a"));
}

#[test]
fn mutation_hook_sees_parser_insertions_but_not_fragment_scratch() {
    let log: Rc<RefCell<Vec<Mutation>>> = Rc::default();
    let sink = log.clone();
    let mut doc = Document::new();
    doc.set_mutation_hook(Some(Box::new(move |m: &Mutation| {
        sink.borrow_mut().push(m.clone())
    })));
    let mut parser = lb_dom::HtmlParser::new();
    parser.push_str("<p>a</p>");
    parser.end();
    parser.run(&mut doc);
    let inserted = log
        .borrow()
        .iter()
        .filter(|m| matches!(m, Mutation::ChildInserted { .. }))
        .count();
    assert_eq!(inserted, 5, "html, head, body, p, text");

    log.borrow_mut().clear();
    let p = doc
        .query_selector(Document::DOCUMENT, "p")
        .unwrap()
        .unwrap();
    doc.set_inner_html(p, "<b>1</b><i>2</i>");
    let log = log.borrow();
    // One removal (old text) and two insertions into <p>; nothing from the
    // scratch fragment tree.
    assert_eq!(log.len(), 3, "{log:#?}");
    assert!(log.iter().all(|m| match m {
        Mutation::ChildInserted { parent, .. } | Mutation::ChildRemoved { parent, .. } =>
            *parent == p,
        _ => false,
    }));
}

#[test]
fn flags_round_trip() {
    let mut doc = Document::new();
    let e = el(&mut doc, "div");
    assert!(doc.flags(e).is_empty());
    doc.set_flags(e, NodeFlags::HAS_LISTENERS | NodeFlags::USER_0, true);
    assert!(doc.flags(e).contains(NodeFlags::HAS_LISTENERS));
    doc.set_flags(e, NodeFlags::USER_0, false);
    assert_eq!(doc.flags(e), NodeFlags::HAS_LISTENERS);
    assert_eq!(doc.node_type(e), NodeType::Element);
}
