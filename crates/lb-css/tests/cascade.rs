//! Cascade, inheritance, media and serialization tests over a real DOM.

use lb_css::{MediaEnv, StyleEngine, computed_value};
use lb_dom::{Document, NodeFlags};
use lb_persona::PersonaProfile;

fn persona(name: &str) -> PersonaProfile {
    let path = format!(
        "{}/../../profiles/reference/chrome/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    PersonaProfile::from_path(path).expect("persona")
}

fn win() -> PersonaProfile {
    persona("chrome153_win10_rtx3060_1080p")
}

fn by_id(doc: &Document, id: &str) -> lb_dom::NodeId {
    doc.get_element_by_id(id).unwrap_or_else(|| panic!("#{id}"))
}

fn val(e: &mut StyleEngine, doc: &Document, id: &str, prop: &str) -> String {
    e.property_value(doc, by_id(doc, id), prop)
}

#[test]
fn specificity_order_and_important() {
    let doc = Document::parse_html(
        r#"<style>
            #a { color: red }
            .c { color: blue }
            div { color: green }
            .c.d { background-color: rgb(1, 2, 3) }
            .d { background-color: rgb(9, 9, 9) !important }
            p { color: lime } p { color: navy }
            span { width: 10px !important }
        </style>
        <div id=a class="c d"></div>
        <p id=p></p>
        <span id=s style="width: 50px; color: rgb(4, 5, 6) !important"></span>
        <div id=i class=c style="color: rgb(7, 7, 7)"></div>"#,
    );
    let mut e = StyleEngine::from_persona(&win());
    assert_eq!(val(&mut e, &doc, "a", "color"), "rgb(255, 0, 0)");
    assert_eq!(val(&mut e, &doc, "a", "background-color"), "rgb(9, 9, 9)");
    assert_eq!(val(&mut e, &doc, "p", "color"), "rgb(0, 0, 128)");
    // Author !important beats a normal inline declaration.
    assert_eq!(val(&mut e, &doc, "s", "width"), "10px");
    assert_eq!(val(&mut e, &doc, "s", "color"), "rgb(4, 5, 6)");
    assert_eq!(val(&mut e, &doc, "i", "color"), "rgb(7, 7, 7)");
}

#[test]
fn inheritance_and_ua_defaults() {
    let doc = Document::parse_html(
        r#"<body><div id=d style="color: rgb(10, 20, 30); font-size: 20px; width: 50%">
             <p id=p>x<em id=em>y</em></p><h1 id=h1>t</h1><code id=code>c</code>
             <span id=sp style="font-size: 2em; line-height: 1.5; color: inherit"></span>
           </div><a id=link href="/x">l</a><input id=in><button id=btn>b</button>"#,
    );
    let mut e = StyleEngine::from_persona(&win());
    // Inherited: color, font-size. Not inherited: width.
    assert_eq!(val(&mut e, &doc, "p", "color"), "rgb(10, 20, 30)");
    assert_eq!(val(&mut e, &doc, "p", "font-size"), "20px");
    assert_eq!(val(&mut e, &doc, "p", "width"), "auto");
    assert_eq!(val(&mut e, &doc, "d", "width"), "50%");
    // UA sheet.
    assert_eq!(val(&mut e, &doc, "p", "display"), "block");
    assert_eq!(val(&mut e, &doc, "p", "margin-top"), "20px");
    assert_eq!(val(&mut e, &doc, "em", "font-style"), "italic");
    assert_eq!(val(&mut e, &doc, "em", "display"), "inline");
    assert_eq!(val(&mut e, &doc, "h1", "font-size"), "40px");
    assert_eq!(val(&mut e, &doc, "h1", "font-weight"), "700");
    assert_eq!(val(&mut e, &doc, "h1", "margin-top"), "26.8px");
    assert_eq!(val(&mut e, &doc, "sp", "font-size"), "40px");
    assert_eq!(val(&mut e, &doc, "sp", "line-height"), "60px");
    // Monospace keyword size: Chrome's 13px default.
    let doc2 = Document::parse_html("<code id=c>x</code><pre id=p>y</pre>");
    assert_eq!(val(&mut e, &doc2, "c", "font-size"), "13px");
    assert_eq!(val(&mut e, &doc2, "p", "font-family"), "monospace");
    let body = doc.body().unwrap();
    assert_eq!(e.property_value(&doc, body, "margin"), "8px");
    assert_eq!(
        e.property_value(&doc, body, "font-family"),
        "\"Times New Roman\""
    );
    assert_eq!(
        e.property_value(&doc, body, "background-color"),
        "rgba(0, 0, 0, 0)"
    );
    assert_eq!(val(&mut e, &doc, "link", "color"), "rgb(0, 0, 238)");
    assert_eq!(val(&mut e, &doc, "link", "cursor"), "pointer");
    assert_eq!(val(&mut e, &doc, "in", "display"), "inline-block");
    assert_eq!(val(&mut e, &doc, "in", "font-size"), "13.3333px");
    assert_eq!(val(&mut e, &doc, "in", "font-family"), "Arial");
    assert_eq!(val(&mut e, &doc, "in", "border-top-width"), "2px");
    assert_eq!(val(&mut e, &doc, "btn", "padding-left"), "6px");
    assert_eq!(
        val(&mut e, &doc, "btn", "background-color"),
        "rgb(239, 239, 239)"
    );
    // Head content is display:none.
    let head = doc.head().unwrap();
    assert_eq!(e.property_value(&doc, head, "display"), "none");
}

#[test]
fn media_queries_follow_persona_screen() {
    let html = r#"<style>
        #a { width: 1px }
        @media (min-width: 1400px) { #a { width: 2px } }
        @media (min-width: 1900px) { #a { width: 3px } }
        @media (-webkit-min-device-pixel-ratio: 2) { #b { display: none } }
        @media print { #b { color: red } }
        #v { width: 50vw; height: 10vh }
    </style><div id=a></div><div id=b></div><div id=v></div>"#;
    let doc = Document::parse_html(html);
    let mut w = StyleEngine::from_persona(&win());
    assert_eq!(val(&mut w, &doc, "a", "width"), "3px");
    assert_eq!(val(&mut w, &doc, "b", "display"), "block");
    assert_eq!(val(&mut w, &doc, "v", "width"), "960px");
    let mac = persona("chrome153_macos15_apple_m2_retina");
    let mut m = StyleEngine::from_persona(&mac);
    assert_eq!(val(&mut m, &doc, "a", "width"), "2px");
    assert_eq!(val(&mut m, &doc, "b", "display"), "none");
    assert_eq!(val(&mut m, &doc, "v", "height"), "81.2px");
    assert!(m.match_media("(max-width: 1470px)").matches);
    assert!(!m.match_media("(min-width: 1471px)").matches);
    assert_eq!(
        m.match_media("screen and (min-resolution:2dppx)").media,
        "screen and (min-resolution: 2dppx)"
    );
    // Resizing the viewport re-evaluates.
    w.set_viewport(1000.0, 800.0);
    assert_eq!(val(&mut w, &doc, "a", "width"), "1px");
    // macOS form controls use system-ui.
    let d2 = Document::parse_html("<input id=i>");
    assert_eq!(val(&mut m, &d2, "i", "font-family"), "system-ui");
}

#[test]
fn styles_are_lazy_and_invalidate_on_mutation() {
    let mut doc = Document::parse_html(
        r#"<style>.x { display: none } div:hover { color: red }</style>
           <div id=a></div><div id=b></div><div id=c></div>"#,
    );
    let mut e = StyleEngine::new(MediaEnv::default());
    assert_eq!(e.stats().computed, 0);
    let a = by_id(&doc, "a");
    assert_eq!(e.property_value(&doc, a, "display"), "block");
    // a + body + html.
    let after_first = e.stats().computed;
    assert_eq!(after_first, 3);
    // Cached: no recomputation.
    e.property_value(&doc, a, "display");
    assert_eq!(e.stats().computed, after_first);
    doc.set_attribute(a, "class", "x").unwrap();
    assert_eq!(e.property_value(&doc, a, "display"), "none");
    // Hover state flags invalidate too.
    doc.set_flags(a, NodeFlags::HOVER, true);
    assert_eq!(e.property_value(&doc, a, "color"), "rgb(255, 0, 0)");
    // Editing the <style> text re-parses it.
    let style = doc
        .query_selector(lb_dom::Document::DOCUMENT, "style")
        .unwrap()
        .unwrap();
    let text = doc.first_child(style).unwrap();
    doc.set_character_data(text, ".x { display: flex }");
    assert_eq!(e.property_value(&doc, a, "display"), "flex");
    // Detached elements have no computed style.
    doc.detach(a);
    assert_eq!(e.property_value(&doc, a, "display"), "");
}

#[test]
fn vars_calc_and_box_values() {
    let doc = Document::parse_html(
        r#"<style>
          :root { --pad: 4px; --w: calc(100% - 2 * var(--pad)) }
          #a { padding: var(--pad) calc(var(--pad) * 3); width: var(--w); margin: 0 auto;
               border: 3px solid currentColor; color: rgb(1 2 3); display: flex; gap: 1em 2px;
               flex-flow: column wrap; justify-content: space-between; transform: translateX(10px) }
          #b { flex: 1; position: absolute; float: left; display: inline; z-index: 5;
               overflow-x: hidden; opacity: 0.5; visibility: hidden; pointer-events: none }
          #g { display: grid; grid-template-columns: 100px repeat(2, 1fr) minmax(10px, auto); grid-column: 2 / span 2 }
          #bad { width: var(--missing); height: -5px }
        </style><div id=a><div id=b></div></div><div id=g></div><div id=bad></div>"#,
    );
    let mut e = StyleEngine::new(MediaEnv::default());
    assert_eq!(val(&mut e, &doc, "a", "padding"), "4px 12px");
    assert_eq!(val(&mut e, &doc, "a", "width"), "calc(100% - 8px)");
    assert_eq!(val(&mut e, &doc, "a", "margin-left"), "auto");
    assert_eq!(val(&mut e, &doc, "a", "border-left-color"), "rgb(1, 2, 3)");
    assert_eq!(val(&mut e, &doc, "a", "border-top-width"), "3px");
    assert_eq!(val(&mut e, &doc, "a", "row-gap"), "16px");
    assert_eq!(val(&mut e, &doc, "a", "column-gap"), "2px");
    assert_eq!(val(&mut e, &doc, "a", "flex-direction"), "column");
    assert_eq!(val(&mut e, &doc, "a", "justify-content"), "space-between");
    assert_eq!(
        val(&mut e, &doc, "a", "transform"),
        "matrix(1, 0, 0, 1, 10, 0)"
    );
    // Absolute: float none, inline blockified; flex item.
    assert_eq!(val(&mut e, &doc, "b", "display"), "block");
    assert_eq!(val(&mut e, &doc, "b", "float"), "none");
    assert_eq!(val(&mut e, &doc, "b", "flex-basis"), "0%");
    assert_eq!(val(&mut e, &doc, "b", "z-index"), "5");
    assert_eq!(val(&mut e, &doc, "b", "overflow-y"), "auto");
    assert_eq!(val(&mut e, &doc, "b", "opacity"), "0.5");
    assert_eq!(val(&mut e, &doc, "b", "visibility"), "hidden");
    assert_eq!(val(&mut e, &doc, "b", "pointer-events"), "none");
    assert_eq!(
        val(&mut e, &doc, "g", "grid-template-columns"),
        "100px repeat(2, 1fr) minmax(10px, auto)"
    );
    assert_eq!(val(&mut e, &doc, "g", "grid-column-end"), "span 2");
    assert_eq!(val(&mut e, &doc, "bad", "width"), "auto");
    assert_eq!(val(&mut e, &doc, "bad", "height"), "auto");
    let s = e.computed_style(&doc, by_id(&doc, "a")).unwrap();
    assert_eq!(computed_value(&s, "--pad").as_deref(), Some("4px"));
}
