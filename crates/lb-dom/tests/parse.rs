//! Parsing, tree shape and serialization.

mod common;

use common::{dump, dump_children, fixture, parse_chunks};
use lb_dom::{Document, NodeFlags, NodeType, QuirksMode};

fn parse(html: &str) -> Document {
    Document::parse_html(html)
}

/// Cases from html5lib-tests tree-construction, in its dump format.
#[test]
fn html5lib_tree_construction_cases() {
    let cases: &[(&str, &str)] = &[
        (
            "<p>One<p>Two",
            "| <html>\n|   <head>\n|   <body>\n|     <p>\n|       \"One\"\n|     <p>\n|       \"Two\"\n",
        ),
        (
            // Adoption agency.
            "<a><p>X<a>Y</a>Z</p></a>",
            "| <html>\n|   <head>\n|   <body>\n|     <a>\n|     <p>\n|       <a>\n|         \"X\"\n|       <a>\n|         \"Y\"\n|       \"Z\"\n",
        ),
        (
            "<b><p>Bold </b> Not bold</p> Also not bold.",
            "| <html>\n|   <head>\n|   <body>\n|     <b>\n|     <p>\n|       <b>\n|         \"Bold \"\n|       \" Not bold\"\n|     \" Also not bold.\"\n",
        ),
        (
            // Foster parenting.
            "<table><tr><td>a</td></tr>b</table>",
            "| <html>\n|   <head>\n|   <body>\n|     \"b\"\n|     <table>\n|       <tbody>\n|         <tr>\n|           <td>\n|             \"a\"\n",
        ),
        (
            "<!DOCTYPE html><template><div>a</div></template>",
            "| <!DOCTYPE html>\n| <html>\n|   <head>\n|     <template>\n|       content\n|         <div>\n|           \"a\"\n|   <body>\n",
        ),
        (
            "<svg><use xlink:href='#a'/></svg><math><mi>x</mi></math>",
            "| <html>\n|   <head>\n|   <body>\n|     <svg svg>\n|       <svg use>\n|         xlink href=\"#a\"\n|     <math math>\n|       <math mi>\n|         \"x\"\n",
        ),
        (
            "<!--c--><html><body x=1><body y=2>",
            "| <!-- c -->\n| <html>\n|   <head>\n|   <body>\n|     x=\"1\"\n|     y=\"2\"\n",
        ),
    ];
    for (input, expected) in cases {
        let doc = parse(input);
        assert_eq!(dump(&doc), *expected, "input: {input}");
    }
}

#[test]
fn quirks_mode_from_doctype() {
    assert_eq!(parse("<p>x").quirks_mode(), QuirksMode::Quirks);
    assert_eq!(
        parse("<!DOCTYPE html><p>x").quirks_mode(),
        QuirksMode::NoQuirks
    );
}

#[test]
fn realistic_page_shape() {
    let doc = parse(&fixture("realistic.html"));
    let html = doc.document_element().unwrap();
    assert_eq!(doc.tag_name(html).unwrap(), "HTML");
    assert_eq!(doc.get_attribute(html, "LANG"), Some("en-US"));
    assert!(doc.doctype_node().is_some());
    assert_eq!(doc.title(), "Acme Store & Co");

    let head = doc.head().unwrap();
    let body = doc.body().unwrap();
    assert_eq!(doc.parent(head), Some(html));
    let head_tags: Vec<_> = doc
        .element_children(head)
        .map(|e| doc.local_name(e).unwrap().to_string())
        .collect();
    assert_eq!(
        head_tags,
        ["meta", "title", "link", "style", "script", "script"]
    );

    // Raw text elements keep their contents as one text node.
    let style = doc.query_selector(head, "style").unwrap().unwrap();
    assert!(
        doc.text_content(style)
            .unwrap()
            .contains("</div> inside style")
    );
    let script = doc.query_selector(head, "script").unwrap().unwrap();
    let src = doc.text_content(script).unwrap();
    assert!(src.contains("\"</div>\"") && src.contains("<!-- not a comment"));
    assert_eq!(doc.child_count(script), 1);

    // Unclosed <li>s are siblings.
    let lis = doc.query_selector_all(body, "#nav li").unwrap();
    assert_eq!(lis.len(), 3);
    assert!(lis.iter().all(|&li| doc.parent(li) == doc.parent(lis[0])));

    // Misnested <b><i></b></i>: the adoption agency splits <i>.
    let p = doc.query_selector(body, "body > p").unwrap().unwrap();
    assert_eq!(
        doc.inner_html(p),
        "Intro <b>bold <i>both</i></b><i> italic</i> tail\n"
    );
    // <div> closes the open <p>.
    let content = doc.get_element_by_id("content").unwrap();
    assert_eq!(doc.prev_element_sibling(content), Some(p));

    // Nested tables, implied tbody, foster-parented text before the table.
    let inner = doc.get_element_by_id("inner").unwrap();
    let outer = doc.get_element_by_id("outer").unwrap();
    let td = doc.closest(inner, "td").unwrap().unwrap();
    assert_eq!(doc.closest(td, "table").unwrap(), Some(outer));
    assert_eq!(
        doc.query_selector_all(outer, ":scope > tbody > tr")
            .unwrap()
            .len(),
        2
    );
    let before_table = doc.prev_sibling(outer).unwrap();
    assert!(
        doc.character_data(before_table)
            .unwrap()
            .contains("stray text")
    );

    // Template contents are a separate fragment, not children.
    let tpl = doc.get_element_by_id("tpl").unwrap();
    assert_eq!(doc.child_count(tpl), 0);
    let contents = doc.template_contents(tpl).unwrap();
    assert_eq!(doc.node_type(contents), NodeType::DocumentFragment);
    assert!(
        doc.query_selector(contents, "li.item > span")
            .unwrap()
            .is_some()
    );
    assert!(doc.query_selector(body, ".item").unwrap().is_none());
    assert_eq!(
        doc.outer_html(tpl),
        "<template id=\"tpl\"><li class=\"item\"><span>templated</span></li></template>"
    );

    // Duplicate attributes: first wins. Names are lowercased.
    let dup = doc.query_selector(body, "[dup]").unwrap().unwrap();
    assert_eq!(doc.get_attribute(dup, "dup"), Some("1"));
    assert_eq!(doc.get_attribute(dup, "data-x"), Some("Y"));
    assert_eq!(doc.attributes(dup).len(), 2);

    // Entities.
    let ent = doc
        .query_selector(content, ":scope > p:nth-of-type(1)")
        .unwrap()
        .unwrap();
    assert_eq!(
        doc.text_content(ent).unwrap(),
        "Entities: <tag> \u{a0}\u{a9} AB"
    );
    assert_eq!(doc.inner_html(ent), "Entities: &lt;tag&gt; &nbsp;© AB");

    // Foreign content and the HTML integration point.
    let fo_p = doc
        .query_selector(body, "foreignObject > p")
        .unwrap()
        .unwrap();
    assert!(doc.element(fo_p).unwrap().is_html());

    // Parser-inserted flag on parser-created elements.
    assert!(doc.flags(body).contains(NodeFlags::PARSER_INSERTED));

    // textarea drops the first newline.
    let ta = doc.query_selector(body, "textarea").unwrap().unwrap();
    assert_eq!(doc.text_content(ta).unwrap(), "hello");
}

#[test]
fn serialization_round_trip() {
    let doc = parse(&fixture("realistic.html"));
    let once = doc.to_html();
    assert!(once.starts_with("<!DOCTYPE html><html lang=\"en-US\"><head>"));
    let doc2 = parse(&once);
    // Same tree after a round trip, and a stable serialization.
    assert_eq!(dump(&doc), dump(&doc2));
    assert_eq!(once, doc2.to_html());
}

#[test]
fn serialization_rules() {
    let doc = parse(
        "<!DOCTYPE html><body><p a='x\"&amp;<'>a &amp; b < c &nbsp;</p>\
         <script>if (a < b && c) {}</script><br><img src=x><svg><path d='M0'/></svg>\
         <!--note--><pre>\n\nx</pre></body>",
    );
    let body = doc.body().unwrap();
    assert_eq!(
        doc.inner_html(body),
        "<p a=\"x&quot;&amp;&lt;\">a &amp; b &lt; c &nbsp;</p>\
         <script>if (a < b && c) {}</script><br><img src=\"x\"><svg><path d=\"M0\"></path></svg>\
         <!--note--><pre>\nx</pre>"
    );
    // innerHTML of a script is raw text.
    let script = doc.query_selector(body, "script").unwrap().unwrap();
    assert_eq!(doc.inner_html(script), "if (a < b && c) {}");
    assert_eq!(
        doc.outer_html(body).len(),
        doc.inner_html(body).len() + "<body></body>".len()
    );
}

#[test]
fn deep_tree_does_not_overflow() {
    // html5ever scope checks are O(depth) per tag, so keep this moderate.
    let depth = 5_000;
    let html = "<div>".repeat(depth) + "x";
    let doc = parse(&html);
    let s = doc.to_html();
    assert!(s.contains('x'));
    let n = doc.descendants(Document::DOCUMENT).count();
    assert!(n > 500, "{n}");
}

#[test]
fn fragment_parsing_inner_html() {
    let mut doc =
        parse("<!DOCTYPE html><body><div id=d><span>old</span></div><table id=t></table>");
    let d = doc.get_element_by_id("d").unwrap();
    doc.set_inner_html(
        d,
        "<b>new</b> <i>text<script>x()</script></i><td>no td</td>",
    );
    assert_eq!(
        doc.inner_html(d),
        "<b>new</b> <i>text<script>x()</script></i>no td"
    );
    let script = doc.query_selector(d, "script").unwrap().unwrap();
    assert!(doc.flags(script).contains(NodeFlags::ALREADY_STARTED));
    assert!(!doc.flags(script).contains(NodeFlags::PARSER_INSERTED));

    // Context matters: inside a <table>, <tr> is allowed.
    let t = doc.get_element_by_id("t").unwrap();
    doc.set_inner_html(t, "<tr><td>a</td></tr>");
    assert_eq!(doc.inner_html(t), "<tbody><tr><td>a</td></tr></tbody>");

    // Raw-text context.
    let frag = doc.parse_fragment(script, "<b>not a tag</b>");
    assert_eq!(dump_children(&doc, frag), "| \"<b>not a tag</b>\"\n");

    // Template innerHTML replaces the contents.
    let tpl = doc.create_element_by_name("template").unwrap();
    doc.set_inner_html(tpl, "<p>in template</p>");
    assert_eq!(doc.child_count(tpl), 0);
    assert_eq!(doc.inner_html(tpl), "<p>in template</p>");

    // outerHTML setter.
    let b = doc.query_selector(d, "b").unwrap().unwrap();
    doc.set_outer_html(b, "<em>x</em><em>y</em>").unwrap();
    assert!(doc.inner_html(d).starts_with("<em>x</em><em>y</em> <i>"));
}

#[test]
fn chunked_input_matches_one_shot() {
    let page = fixture("realistic.html");
    let (whole, whole_scripts) = parse_chunks(&[&page]);
    let expected = dump(&whole);
    // Split at many byte positions (on char boundaries).
    let mut positions: Vec<usize> = (0..page.len()).step_by(7).collect();
    positions.extend([1, 2, page.len() - 1]);
    for p in positions {
        if !page.is_char_boundary(p) {
            continue;
        }
        let (a, b) = page.split_at(p);
        let (doc, scripts) = parse_chunks(&[a, b]);
        assert_eq!(dump(&doc), expected, "split at {p}");
        assert_eq!(scripts.len(), whole_scripts.len(), "split at {p}");
    }
}

/// Deterministic tag soup: exercises adoption agency, foster parenting,
/// foreign content and templates in odd combinations, in document and
/// fragment parsing. Must not panic (e.g. on a sink `RefCell` conflict).
#[test]
fn tag_soup_does_not_panic() {
    const PIECES: &[&str] = &[
        "<a>",
        "</a>",
        "<b>",
        "</b>",
        "<i>",
        "</i>",
        "<p>",
        "</p>",
        "<table>",
        "</table>",
        "<tr>",
        "<td>",
        "</td>",
        "<th>",
        "<caption>",
        "<colgroup>",
        "<col>",
        "<tbody>",
        "<template>",
        "</template>",
        "<svg>",
        "</svg>",
        "<math>",
        "<mi>",
        "<foreignObject>",
        "<desc>",
        "<select>",
        "<option>",
        "<optgroup>",
        "</select>",
        "<form>",
        "</form>",
        "<li>",
        "<ul>",
        "</ul>",
        "<div>",
        "</div>",
        "<frameset>",
        "<body>",
        "<html>",
        "<head>",
        "<title>",
        "</title>",
        "<textarea>",
        "</textarea>",
        "<script>",
        "</script>",
        "<style>",
        "</style>",
        "<noscript>",
        "<plaintext>",
        "<br>",
        "</br>",
        "<img>",
        "<hr>",
        "<button>",
        "<nobr>",
        "<marquee>",
        "<object>",
        "<applet>",
        "<input>",
        "<!--x-->",
        "<!DOCTYPE html>",
        "text",
        " ",
        "&amp;",
        "&",
        "<",
        "\0",
        "<h1>",
        "<h2>",
        "</h1>",
        "<dd>",
        "<dt>",
        "<ruby>",
        "<rt>",
        "<rp>",
        "<image>",
        "<isindex>",
        "<xmp>",
        "<iframe>",
        "<noembed>",
        "<annotation-xml encoding=text/html>",
    ];
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..300 {
        let len = (next() % 60) as usize;
        let html: String = (0..len)
            .map(|_| PIECES[(next() % PIECES.len() as u64) as usize])
            .collect();
        let doc = parse(&html);
        let s1 = doc.to_html();
        // Fragment parsing in a few contexts too.
        let mut doc2 = parse(
            "<!DOCTYPE html><body><div id=d></div><table><tbody id=tb></tbody></table><select id=s></select>",
        );
        for id in ["d", "tb", "s"] {
            let ctx = doc2.get_element_by_id(id).unwrap();
            doc2.set_inner_html(ctx, &html);
            let _ = doc2.inner_html(ctx);
        }
        let _ = parse(&s1).to_html();
    }
}
