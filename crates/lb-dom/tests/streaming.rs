//! Streaming driver: yields at scripts, resumes, document.write, bytes.

mod common;

use common::{dump, fixture, parse_chunks};
use lb_dom::{Document, HtmlParser, NodeFlags, ParseStep};

#[test]
fn yields_at_each_script_in_order() {
    let page = fixture("realistic.html");
    let (doc, scripts) = parse_chunks(&[&page]);
    // Two in <head>, the document.write one, the JSON-LD one.
    assert_eq!(scripts.len(), 4);
    let all = doc
        .query_selector_all(Document::DOCUMENT, "script")
        .unwrap();
    assert_eq!(scripts, all);
    assert!(
        scripts
            .iter()
            .all(|&s| doc.flags(s).contains(NodeFlags::PARSER_INSERTED))
    );
}

#[test]
fn script_sees_only_preceding_tree() {
    let mut doc = Document::new();
    let mut parser = HtmlParser::new();
    parser.push_str("<!DOCTYPE html><p id=a>A</p><script>one</script><p id=b>B</p>");
    let step = parser.run(&mut doc);
    let ParseStep::Script(s) = step else {
        panic!("expected a script, got {step:?}");
    };
    // At the yield the script is complete and in the tree; later markup is not.
    assert_eq!(doc.text_content(s).unwrap(), "one");
    assert!(doc.is_connected(s));
    assert!(doc.get_element_by_id("a").is_some());
    assert!(doc.get_element_by_id("b").is_none());
    assert_eq!(parser.blocked_on(), Some(s));
    assert_eq!(parser.script_line(s), Some(1));

    assert_eq!(parser.run(&mut doc), ParseStep::NeedInput);
    assert!(parser.blocked_on().is_none());
    assert!(doc.get_element_by_id("b").is_some());
    parser.end();
    assert_eq!(parser.run(&mut doc), ParseStep::Done);
    assert_eq!(parser.run(&mut doc), ParseStep::Done);
}

#[test]
fn script_split_across_chunks() {
    let mut doc = Document::new();
    let mut parser = HtmlParser::new();
    assert_eq!(
        parser.feed_str(&mut doc, "<script>var x = '</scr"),
        ParseStep::NeedInput
    );
    let step = parser.feed_str(&mut doc, "ipt>';</script>after");
    let ParseStep::Script(s) = step else {
        panic!("{step:?}")
    };
    assert_eq!(doc.text_content(s).unwrap(), "var x = '");
    // The rest of the chunk is parsed on resume.
    assert_eq!(parser.run(&mut doc), ParseStep::NeedInput);
    assert!(
        doc.text_content(doc.body().unwrap())
            .unwrap()
            .contains("';after")
    );
}

#[test]
fn document_write_inserts_at_insertion_point() {
    let mut doc = Document::new();
    let mut parser = HtmlParser::new();
    parser.push_str("<body><script>w()</script><p id=after>after</p>");
    parser.end();
    let ParseStep::Script(s) = parser.run(&mut doc) else {
        panic!()
    };
    // Two writes in one script keep their order and are parsed at once.
    assert!(parser.write(&mut doc, "<div id=w1>one</div>"));
    assert!(
        doc.get_element_by_id("w1").is_some(),
        "visible to the running script"
    );
    assert!(parser.write(&mut doc, "<div id=w2>two</div>"));
    assert!(doc.get_element_by_id("after").is_none());
    assert_eq!(parser.run(&mut doc), ParseStep::Done);

    let body = doc.body().unwrap();
    let ids: Vec<_> = doc
        .element_children(body)
        .map(|e| doc.element_id(e).unwrap_or("-").to_owned())
        .collect();
    assert_eq!(ids, ["-", "w1", "w2", "after"]);
    assert_eq!(doc.element_children(body).next(), Some(s));
}

#[test]
fn document_write_of_script_and_partial_markup() {
    let mut doc = Document::new();
    let mut parser = HtmlParser::new();
    parser.push_str("<body><script>outer</script><p id=after>after</p>");
    parser.end();
    let ParseStep::Script(outer) = parser.run(&mut doc) else {
        panic!()
    };
    // A written script becomes the pending script, run after the outer one.
    assert!(parser.write(&mut doc, "<script>inner</script><sp"));
    // Text written after a pending script waits; a split tag joins up.
    assert!(parser.write(&mut doc, "an id=s>"));
    assert!(doc.get_element_by_id("s").is_none());
    let ParseStep::Script(inner) = parser.run(&mut doc) else {
        panic!()
    };
    assert_ne!(inner, outer);
    assert_eq!(doc.text_content(inner).unwrap(), "inner");
    assert_eq!(parser.run(&mut doc), ParseStep::Done);

    let span = doc.get_element_by_id("s").unwrap();
    assert_eq!(doc.prev_element_sibling(span), Some(inner));
    let after = doc.get_element_by_id("after").unwrap();
    assert_eq!(doc.parent(after), Some(span));
}

#[test]
fn document_write_without_insertion_point() {
    let mut doc = Document::new();
    let mut parser = HtmlParser::new();
    assert_eq!(parser.feed_str(&mut doc, "<p>x"), ParseStep::NeedInput);
    assert!(!parser.write(&mut doc, "<b>ignored</b>"));
    parser.end();
    assert_eq!(parser.run(&mut doc), ParseStep::Done);
    assert!(!parser.write(&mut doc, "<b>ignored</b>"));
    assert_eq!(doc.query_selector(Document::DOCUMENT, "b").unwrap(), None);
}

#[test]
fn utf8_bytes_split_mid_character() {
    let html = "<p>héllo — wörld 🎉</p>";
    let bytes = html.as_bytes();
    for split in 0..bytes.len() {
        let mut doc = Document::new();
        let mut parser = HtmlParser::new();
        parser.feed_bytes(&mut doc, &bytes[..split]);
        parser.feed_bytes(&mut doc, &bytes[split..]);
        parser.end();
        assert_eq!(parser.run(&mut doc), ParseStep::Done);
        let p = doc
            .query_selector(Document::DOCUMENT, "p")
            .unwrap()
            .unwrap();
        assert_eq!(
            doc.text_content(p).unwrap(),
            "héllo — wörld 🎉",
            "split {split}"
        );
    }
    // Invalid and truncated input become U+FFFD.
    let mut doc = Document::new();
    let mut parser = HtmlParser::new();
    parser.feed_bytes(&mut doc, b"<p>a\xffb\xe2\x82");
    parser.end();
    parser.run(&mut doc);
    let p = doc
        .query_selector(Document::DOCUMENT, "p")
        .unwrap()
        .unwrap();
    assert_eq!(doc.text_content(p).unwrap(), "a\u{FFFD}b\u{FFFD}");
}

#[test]
fn encoding_hint_reported() {
    let mut doc = Document::new();
    let mut parser = HtmlParser::new();
    parser.push_str("<head><meta charset=windows-1252><title>x</title>");
    parser.end();
    assert_eq!(parser.run(&mut doc), ParseStep::Done);
    assert_eq!(parser.encoding_hint(), Some("windows-1252"));
    assert_eq!(doc.title(), "x");
}

#[test]
fn many_small_chunks() {
    let page = fixture("realistic.html");
    let one_shot = dump(&parse_chunks(&[&page]).0);
    let chunks: Vec<&str> = page
        .char_indices()
        .map(|(i, c)| &page[i..i + c.len_utf8()])
        .collect();
    let (doc, scripts) = parse_chunks(&chunks);
    assert_eq!(dump(&doc), one_shot);
    assert_eq!(scripts.len(), 4);
}

#[test]
fn pauses_after_links_and_style_end_tags_when_asked() {
    let mut doc = Document::new();
    let mut parser = HtmlParser::with_options(lb_dom::ParserOptions {
        pause_at_style_sheets: true,
        ..Default::default()
    });
    parser.push_str(
        "<!DOCTYPE html><head><link rel=stylesheet href=a.css><style>p{}</style></head>\
         <body><p id=a>A</p><link rel=stylesheet href=b.css><p id=b>B</p><svg><style>x</style></svg>",
    );
    parser.end();
    let mut seen = Vec::new();
    loop {
        match parser.run(&mut doc) {
            ParseStep::StyleSheet(n) => {
                assert!(parser.blocked_on().is_none());
                let name = doc.element(n).unwrap().name.local.to_string();
                let b_parsed = doc.get_element_by_id("b").is_some();
                seen.push(format!("{name}:{b_parsed}"));
            }
            ParseStep::Done => break,
            s => panic!("{s:?}"),
        }
    }
    // The body link pauses before `#b` is parsed; SVG `<style>` is not HTML.
    assert_eq!(seen, ["link:false", "style:false", "link:false"]);
    assert_eq!(parser.take_closed_styles().len(), 1);
    // Without the option the parser runs through.
    let mut doc = Document::new();
    let mut parser = HtmlParser::new();
    parser.push_str("<link rel=stylesheet href=a.css><style>p{}</style>");
    parser.end();
    assert_eq!(parser.run(&mut doc), ParseStep::Done);
}
