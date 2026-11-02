//! querySelector / matches / closest.

mod common;

use common::fixture;
use lb_dom::{Document, DomError, NodeFlags, NodeId};

fn ids(doc: &Document, nodes: &[NodeId]) -> Vec<String> {
    nodes
        .iter()
        .map(|&n| doc.element_id(n).unwrap_or("-").to_owned())
        .collect()
}

fn qsa_ids(doc: &Document, sel: &str) -> Vec<String> {
    ids(
        doc,
        &doc.query_selector_all(Document::DOCUMENT, sel).unwrap(),
    )
}

fn count(doc: &Document, sel: &str) -> usize {
    doc.query_selector_all(Document::DOCUMENT, sel)
        .unwrap_or_else(|e| panic!("{sel}: {e}"))
        .len()
}

const LIST: &str = "<!DOCTYPE html><body>\
    <ul id=list>\
      <li id=l1 class='a b'>1</li><li id=l2 class=B>2</li><li id=l3 lang=en-GB>3</li>\
      <li id=l4 data-k='x-y z'></li><li id=l5><!-- c --></li>\
    </ul>\
    <div id=d1><p id=p1>x</p><span id=s1></span><p id=p2></p></div>\
    <a id=a1 href=/x>x</a><a id=a2>y</a>\
    <svg id=svg1><rect id=r1 /></svg>\
    </body>";

#[test]
fn basic_selectors() {
    let doc = Document::parse_html(LIST);
    assert_eq!(qsa_ids(&doc, "li"), ["l1", "l2", "l3", "l4", "l5"]);
    assert_eq!(qsa_ids(&doc, "LI.a"), ["l1"]);
    assert_eq!(
        qsa_ids(&doc, "#list > li:first-child, #d1 p"),
        ["l1", "p1", "p2"]
    );
    assert_eq!(qsa_ids(&doc, ".b"), ["l1"], "class match is case-sensitive");
    assert_eq!(qsa_ids(&doc, "[data-k~=z]"), ["l4"]);
    assert_eq!(qsa_ids(&doc, "[data-k|=x]"), ["l4"]);
    assert_eq!(qsa_ids(&doc, "[data-k^='x-'][data-k$=z]"), ["l4"]);
    assert_eq!(
        qsa_ids(&doc, "[DATA-K*=y]"),
        ["l4"],
        "attr names are case-insensitive in HTML"
    );
    assert_eq!(qsa_ids(&doc, "[class=b i]"), ["l2"]);
    assert_eq!(qsa_ids(&doc, "li:nth-child(2n+1)"), ["l1", "l3", "l5"]);
    assert_eq!(qsa_ids(&doc, "li:nth-last-child(1)"), ["l5"]);
    assert_eq!(qsa_ids(&doc, "li:nth-child(odd of .a, [lang])"), ["l1"]);
    assert_eq!(qsa_ids(&doc, "#d1 > :last-child"), ["p2"]);
    assert_eq!(qsa_ids(&doc, "#d1 p:first-of-type"), ["p1"]);
    assert_eq!(qsa_ids(&doc, "#d1 :only-of-type"), ["s1"]);
    assert_eq!(
        qsa_ids(&doc, "li:empty, p:empty, span:empty"),
        ["l4", "l5", "s1", "p2"]
    );
    assert_eq!(qsa_ids(&doc, "li:not(.a, #l2):not(:empty)"), ["l3"]);
    assert_eq!(qsa_ids(&doc, ":is(#p1, #s1) + *"), ["s1", "p2"]);
    assert_eq!(qsa_ids(&doc, ":where(#p1) ~ p"), ["p2"]);
    assert_eq!(qsa_ids(&doc, "div:has(> span)"), ["d1"]);
    assert_eq!(qsa_ids(&doc, ":root"), ["-"]);
    assert_eq!(qsa_ids(&doc, ":lang(en)"), ["l3"]);
    assert_eq!(qsa_ids(&doc, "a:link"), ["a1"]);
    assert_eq!(qsa_ids(&doc, "a:any-link, a:-webkit-any-link"), ["a1"]);
    assert_eq!(qsa_ids(&doc, "a:visited"), Vec::<String>::new());
    // SVG elements: case-sensitive local names.
    assert_eq!(qsa_ids(&doc, "svg rect"), ["r1"]);
    assert_eq!(qsa_ids(&doc, "svg > *"), ["r1"]);
}

#[test]
fn scope_matches_and_closest() {
    let doc = Document::parse_html(LIST);
    let d1 = doc.get_element_by_id("d1").unwrap();
    let s1 = doc.get_element_by_id("s1").unwrap();
    // Scoped query: only descendants, :scope is the element.
    assert_eq!(
        ids(&doc, &doc.query_selector_all(d1, ":scope > p").unwrap()),
        ["p1", "p2"]
    );
    assert_eq!(doc.query_selector(d1, "div").unwrap(), None);
    assert_eq!(
        ids(&doc, &doc.query_selector_all(d1, "body p").unwrap()),
        ["p1", "p2"]
    );
    assert!(doc.matches(s1, "#d1 > span").unwrap());
    assert!(!doc.matches(s1, "p").unwrap());
    assert_eq!(doc.closest(s1, "div").unwrap(), Some(d1));
    assert_eq!(doc.closest(s1, "span").unwrap(), Some(s1));
    assert_eq!(doc.closest(s1, "table").unwrap(), None);
    assert!(doc.matches(s1, ":scope").unwrap());
}

#[test]
fn invalid_and_unsupported_selectors() {
    let doc = Document::parse_html(LIST);
    for bad in ["", "li[", "::", ":no-such-class", "a >", "#", "li, ", "!"] {
        match doc.query_selector(Document::DOCUMENT, bad) {
            Err(e @ DomError::Syntax(_)) => assert_eq!(e.name(), "SyntaxError"),
            other => panic!("{bad:?} -> {other:?}"),
        }
    }
    // Pseudo-elements and dynamic states parse but do not match.
    for ok in [
        "p::before",
        "p:before",
        "li::marker",
        "input::-webkit-inner-spin-button",
        "::highlight(x)",
        "a:hover",
        "a:focus-visible",
        "p::after:hover",
        "x-foo:defined",
        ":dir(ltr)",
    ] {
        assert!(
            doc.query_selector(Document::DOCUMENT, ok).is_ok(),
            "{ok} should parse"
        );
    }
    assert_eq!(count(&doc, "p::before"), 0);
    assert_eq!(count(&doc, "a:hover"), 0);
}

#[test]
fn form_state_pseudo_classes() {
    let doc = Document::parse_html(&fixture("realistic.html"));
    let names = |sel: &str| -> Vec<String> {
        doc.query_selector_all(Document::DOCUMENT, sel)
            .unwrap()
            .iter()
            .map(|&n| {
                doc.get_attribute(n, "name")
                    .map(str::to_owned)
                    .or_else(|| doc.text_content(n))
                    .unwrap_or_default()
            })
            .collect()
    };
    assert_eq!(names("input:checked"), ["a"]);
    assert_eq!(names("option:checked"), ["two"]);
    assert_eq!(names("input:disabled"), ["in-fieldset"]);
    assert_eq!(names("option:disabled"), ["three"]);
    assert_eq!(names("button:disabled"), ["Buy"]);
    assert_eq!(names("input:enabled").len(), 5);
    assert_eq!(names(":required"), ["q"]);
    assert_eq!(names("input:read-write"), ["q", "in-legend"]);
    assert_eq!(names("textarea:read-write"), ["t"]);
    assert_eq!(names(":placeholder-shown"), ["q"]);
    assert_eq!(names("input:invalid"), ["q"]);
}

#[test]
fn dynamic_state_from_flags() {
    let mut doc = Document::parse_html(LIST);
    let a1 = doc.get_element_by_id("a1").unwrap();
    assert_eq!(count(&doc, ":hover"), 0);
    doc.set_flags(a1, NodeFlags::HOVER | NodeFlags::FOCUS, true);
    assert_eq!(qsa_ids(&doc, "a:hover:focus"), ["a1"]);
    doc.set_flags(a1, NodeFlags::FOCUS, false);
    assert_eq!(count(&doc, ":focus"), 0);
}

#[test]
fn quirks_mode_ids_and_classes_case_insensitive() {
    let quirks = Document::parse_html("<div id=Foo class=Bar></div>");
    assert_eq!(count(&quirks, "#foo"), 1);
    assert_eq!(count(&quirks, ".bar"), 1);
    let standards = Document::parse_html("<!DOCTYPE html><div id=Foo class=Bar></div>");
    assert_eq!(count(&standards, "#foo"), 0);
    assert_eq!(count(&standards, ".bar"), 0);
    assert_eq!(count(&standards, "DIV#Foo.Bar"), 1);
}

#[test]
fn reusable_selector_list() {
    let doc = Document::parse_html(LIST);
    let list = lb_dom::SelectorList::parse("li.a, p").unwrap();
    assert_eq!(
        ids(&doc, &doc.select_all(Document::DOCUMENT, &list)),
        ["l1", "p1", "p2"]
    );
    assert_eq!(
        doc.select_first(Document::DOCUMENT, &list),
        doc.get_element_by_id("l1")
    );
}
