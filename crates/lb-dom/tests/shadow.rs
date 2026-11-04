//! Shadow roots, slot assignment, the flat tree, cloning and declarative
//! shadow DOM.

use lb_dom::{
    Document, ShadowRootInit, ShadowRootMode, ShadowSerialization, SlotAssignment,
    is_valid_custom_element_name,
};

fn init(mode: ShadowRootMode) -> ShadowRootInit {
    ShadowRootInit {
        mode,
        delegates_focus: false,
        slot_assignment: SlotAssignment::Named,
        clonable: false,
        serializable: false,
    }
}

#[test]
fn attach_and_connectedness() {
    let mut d = Document::parse_html("<div id=h><span slot=a>A</span>text</div><img id=i>");
    let h = d.get_element_by_id("h").unwrap();
    let img = d.get_element_by_id("i").unwrap();
    assert!(d.attach_shadow(img, &init(ShadowRootMode::Open)).is_err());
    let sr = d.attach_shadow(h, &init(ShadowRootMode::Open)).unwrap();
    assert!(d.attach_shadow(h, &init(ShadowRootMode::Open)).is_err());
    assert_eq!(d.shadow_root(h), Some(sr));
    assert_eq!(d.shadow_host(sr), Some(h));
    assert_eq!(d.parent(sr), None);
    let p = d.create_element_by_name("p").unwrap();
    d.append_child(sr, p).unwrap();
    assert!(d.is_connected(p));
    assert_eq!(d.root(p), sr);
    assert_eq!(d.shadow_including_root(p), Document::DOCUMENT);
    // The host cannot go inside its own shadow tree.
    assert!(d.append_child(p, h).is_err());
    // querySelector does not enter shadow trees.
    assert_eq!(d.query_selector(Document::DOCUMENT, "p").unwrap(), None);
    assert_eq!(d.query_selector(sr, "p").unwrap(), Some(p));
}

#[test]
fn named_and_manual_slots() {
    let mut d = Document::parse_html("<div id=h><span slot=a>A</span>t<b>B</b></div>");
    let h = d.get_element_by_id("h").unwrap();
    let kids: Vec<_> = d.children(h).collect();
    let sr = d.attach_shadow(h, &init(ShadowRootMode::Open)).unwrap();
    d.set_inner_html(sr, "<slot name=a></slot><slot></slot><slot name=a></slot>");
    let slots: Vec<_> = d.slots_in(sr).collect();
    assert_eq!(d.assigned_nodes(slots[0]), vec![kids[0]]);
    assert_eq!(d.assigned_nodes(slots[1]), vec![kids[1], kids[2]]);
    assert!(d.assigned_nodes(slots[2]).is_empty());
    assert_eq!(d.find_slot(kids[2], true), Some(slots[1]));
    assert_eq!(d.flat_children(h), slots.clone());
    assert_eq!(d.flat_children(slots[1]), vec![kids[1], kids[2]]);
    assert_eq!(d.flat_parent(kids[0]), Some(slots[0]));

    let mut d = Document::parse_html("<div id=h><i>1</i><i>2</i></div>");
    let h = d.get_element_by_id("h").unwrap();
    let kids: Vec<_> = d.children(h).collect();
    let mut i = init(ShadowRootMode::Closed);
    i.slot_assignment = SlotAssignment::Manual;
    let sr = d.attach_shadow(h, &i).unwrap();
    d.set_inner_html(sr, "<slot></slot><slot></slot>");
    let slots: Vec<_> = d.slots_in(sr).collect();
    assert!(d.assigned_nodes(slots[0]).is_empty());
    d.assign_slot(slots[0], &[kids[1], kids[0]]);
    assert_eq!(d.assigned_nodes(slots[0]), vec![kids[1], kids[0]]);
    d.assign_slot(slots[1], &[kids[0]]);
    assert_eq!(d.assigned_nodes(slots[0]), vec![kids[1]]);
    assert_eq!(d.find_slot(kids[0], false), Some(slots[1]));
    assert_eq!(d.find_slot(kids[0], true), None);
}

#[test]
fn declarative_shadow_and_get_html() {
    let d = Document::parse_html(
        "<div id=h><template shadowrootmode=open shadowrootserializable><slot></slot>\
         </template>x</div><template id=t shadowrootmode=bogus></template>",
    );
    let h = d.get_element_by_id("h").unwrap();
    let sr = d.shadow_root(h).expect("declarative root");
    assert_eq!(d.inner_html(sr), "<slot></slot>");
    assert_eq!(d.inner_html(h), "x");
    assert_eq!(
        d.get_html(
            h,
            ShadowSerialization {
                serializable: true,
                roots: &[]
            }
        ),
        "<template shadowrootmode=\"open\" shadowrootserializable=\"\"><slot></slot></template>x"
    );
    assert!(d.get_element_by_id("t").is_some());
}

#[test]
fn clone_copies_clonable_roots_only() {
    let mut d = Document::parse_html("<div id=a>x</div><div id=b>y</div>");
    let a = d.get_element_by_id("a").unwrap();
    let b = d.get_element_by_id("b").unwrap();
    let mut i = init(ShadowRootMode::Open);
    i.clonable = true;
    let sa = d.attach_shadow(a, &i).unwrap();
    d.set_inner_html(sa, "<em>in</em>");
    let sb = d.attach_shadow(b, &init(ShadowRootMode::Open)).unwrap();
    d.set_inner_html(sb, "<em>in</em>");
    let ca = d.clone_node(a, false);
    let csr = d.shadow_root(ca).expect("clonable root cloned");
    assert_eq!(d.inner_html(csr), "<em>in</em>");
    assert_eq!(d.child_count(ca), 0);
    let cb = d.clone_node(b, true);
    assert!(d.shadow_root(cb).is_none());
}

#[test]
fn custom_element_names() {
    for ok in ["a-b", "x-\u{e9}", "my-element-1"] {
        assert!(is_valid_custom_element_name(ok), "{ok}");
    }
    for bad in ["ab", "A-b", "1-a", "font-face", "a-B", "-a"] {
        assert!(!is_valid_custom_element_name(bad), "{bad}");
    }
}
