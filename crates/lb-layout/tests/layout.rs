//! Layout, geometry, hit testing and accessibility tests.

use std::sync::Arc;

use lb_css::MediaEnv;
use lb_dom::{Document, NodeId};
use lb_layout::{LayoutEngine, Rect, ScrollAlign};
use lb_persona::PersonaProfile;

fn engine() -> LayoutEngine {
    LayoutEngine::with_env(MediaEnv {
        viewport_width: 800.0,
        viewport_height: 600.0,
        ..MediaEnv::default()
    })
}

fn id(doc: &Document, s: &str) -> NodeId {
    doc.get_element_by_id(s).unwrap_or_else(|| panic!("#{s}"))
}

fn rect(e: &mut LayoutEngine, doc: &Document, s: &str) -> Rect {
    e.bounding_client_rect(doc, id(doc, s))
}

#[test]
fn flex_row_of_three_divs() {
    let doc = Document::parse_html(
        r#"<body style="margin:0"><div id=row style="display:flex; gap:10px; padding:5px">
            <div id=a style="width:100px;height:20px"></div>
            <div id=b style="width:50px;height:30px"></div>
            <div id=c style="flex:1;height:10px"></div></div>"#,
    );
    let mut e = engine();
    let a = rect(&mut e, &doc, "a");
    let b = rect(&mut e, &doc, "b");
    let c = rect(&mut e, &doc, "c");
    assert_eq!((a.x, a.y, a.width), (5.0, 5.0, 100.0));
    assert_eq!(b.x, 115.0);
    assert_eq!(c.x, 175.0);
    assert_eq!(c.width, 800.0 - 5.0 - 175.0);
    // Stretch: all items get the line's height (30px).
    assert_eq!(a.height, 20.0);
    let row = rect(&mut e, &doc, "row");
    assert_eq!(row.height, 40.0);
}

#[test]
fn block_stacking_and_margins() {
    let doc = Document::parse_html(
        r#"<body><div id=a style="height:50px;margin-bottom:20px"></div>
           <div id=b style="height:10px;margin-top:30px;margin-left:15px;padding:4px;border:1px solid"></div>
           <p id=p>para</p></body>"#,
    );
    let mut e = engine();
    let a = rect(&mut e, &doc, "a");
    assert_eq!((a.x, a.y, a.width, a.height), (8.0, 8.0, 784.0, 50.0));
    let b = rect(&mut e, &doc, "b");
    // Vertical margins collapse to the larger one (30px).
    assert_eq!(b.y, 8.0 + 50.0 + 30.0);
    assert_eq!(b.x, 23.0);
    assert_eq!(b.width, 784.0 - 15.0);
    assert_eq!(b.height, 10.0 + 8.0 + 2.0);
    let p = rect(&mut e, &doc, "p");
    // p has a 16px top margin, collapsed with b's 0 bottom margin.
    assert_eq!(p.y, b.bottom() + 16.0);
    // offset* are relative to the document (body is offsetParent).
    let pid = id(&doc, "p");
    assert_eq!(e.offset_parent(&doc, pid), doc.body());
    assert_eq!(e.offset_position(&doc, pid), (8.0, p.y.round()));
    assert_eq!(e.offset_size(&doc, id(&doc, "b")), (769.0, 20.0));
    assert_eq!(e.client_size(&doc, id(&doc, "b")), (767.0, 18.0));
    assert_eq!(
        e.resolved_style_value(&doc, id(&doc, "a"), "width"),
        "784px"
    );
    assert_eq!(e.resolved_style_value(&doc, pid, "margin-top"), "16px");
}

#[test]
fn text_width_from_font_tables() {
    let doc = Document::parse_html(
        r#"<body style="margin:0"><span id=s style="font: 16px Arial">Hello world</span>
           <div id=d style="font: 16px Arial; width: 60px">Hello world</div>"#,
    );
    let mut e = engine();
    // Arial advances (units per em 2048): "Hello world" = 10130 units.
    let expected = 10130.0 * 16.0 / 2048.0;
    let s = rect(&mut e, &doc, "s");
    assert!(
        (s.width - expected).abs() < 0.01,
        "{} vs {expected}",
        s.width
    );
    // Arial 16px: ascent 14 + descent 3 + line gap 1.
    assert_eq!(s.height, 18.0);
    // Wraps at the space in a narrow block.
    let d = rect(&mut e, &doc, "d");
    assert_eq!(d.height, 36.0);
    // The table is exposed for font probes.
    let db = e.fonts();
    assert!(db.get("Arial").is_some());
}

#[test]
fn element_from_point_picks_topmost() {
    let doc = Document::parse_html(
        r#"<body style="margin:0">
          <div id=under style="position:absolute;left:0;top:0;width:200px;height:200px;z-index:2"></div>
          <div id=over style="position:absolute;left:50px;top:50px;width:100px;height:100px;z-index:1"></div>
          <div id=late style="position:absolute;left:300px;top:0;width:100px;height:100px"></div>
          <div id=later style="position:absolute;left:350px;top:0;width:100px;height:100px"></div>
          <div id=ghost style="position:absolute;left:500px;top:0;width:100px;height:100px;pointer-events:none"></div>
          <div id=flow style="margin-top:300px;height:40px"><span id=sp>text</span></div>"#,
    );
    let mut e = engine();
    // z-index 2 beats z-index 1 even though it comes first.
    assert_eq!(
        e.element_from_point(&doc, 75.0, 75.0),
        Some(id(&doc, "under"))
    );
    // Same z: later in tree order wins.
    assert_eq!(
        e.element_from_point(&doc, 360.0, 10.0),
        Some(id(&doc, "later"))
    );
    assert_eq!(
        e.element_from_point(&doc, 310.0, 10.0),
        Some(id(&doc, "late"))
    );
    // pointer-events: none is transparent to hit testing.
    assert_eq!(
        e.element_from_point(&doc, 550.0, 10.0),
        doc.document_element()
    );
    // Text hits return the parent element.
    assert_eq!(e.element_from_point(&doc, 2.0, 305.0), Some(id(&doc, "sp")));
    assert_eq!(
        e.element_from_point(&doc, 700.0, 305.0),
        Some(id(&doc, "flow"))
    );
    let all = e.elements_from_point(&doc, 2.0, 305.0);
    assert_eq!(
        &all[..3],
        &[id(&doc, "sp"), id(&doc, "flow"), doc.body().unwrap()]
    );
    assert_eq!(e.element_from_point(&doc, 900.0, 10.0), None);
}

#[test]
fn layout_runs_only_when_geometry_is_requested() {
    let mut doc = Document::parse_html(r#"<div id=a style="height:10px"></div><div id=b></div>"#);
    let mut e = engine();
    assert_eq!(e.stats().layouts, 0);
    let a = id(&doc, "a");
    // Style queries alone do not lay out.
    assert_eq!(e.resolved_style_value(&doc, a, "display"), "block");
    e.styles().property_value(&doc, a, "height");
    assert_eq!(e.stats().layouts, 0);
    assert_eq!(e.box_count(), 0);
    let r1 = e.bounding_client_rect(&doc, a);
    assert_eq!(e.stats().layouts, 1);
    // Cached while nothing changes.
    e.bounding_client_rect(&doc, a);
    e.element_from_point(&doc, 10.0, 10.0);
    assert_eq!(e.stats().layouts, 1);
    // Mutations dirty it, but nothing runs until asked again.
    doc.set_attribute(a, "style", "height:20px").unwrap();
    assert_eq!(e.stats().layouts, 1);
    let r2 = e.bounding_client_rect(&doc, a);
    assert_eq!(e.stats().layouts, 2);
    assert_eq!((r1.height, r2.height), (10.0, 20.0));
    e.drop_layout();
    assert_eq!(e.box_count(), 0);
}

#[test]
fn visibility_scroll_and_display_none() {
    let doc = Document::parse_html(
        r#"<!doctype html><body style="margin:0">
          <div id=hidden style="display:none"><span id=inner>x</span></div>
          <div id=invis style="visibility:hidden;height:10px"></div>
          <div id=empty></div>
          <div id=box style="height:100px;overflow:auto">
            <div style="height:300px"></div><div id=deep style="height:20px">deep</div>
          </div>
          <div style="height:2000px"></div>
          <button id=far>Far</button>"#,
    );
    let mut e = engine();
    assert!(!e.is_visible(&doc, id(&doc, "hidden")));
    assert!(!e.is_visible(&doc, id(&doc, "inner")));
    assert_eq!(rect(&mut e, &doc, "inner"), Rect::ZERO);
    assert!(!e.is_visible(&doc, id(&doc, "invis")));
    assert!(!e.is_visible(&doc, id(&doc, "empty")));
    assert!(e.is_visible(&doc, id(&doc, "box")));
    let bx = id(&doc, "box");
    assert_eq!(e.scroll_size(&doc, bx), (800.0, 320.0));
    // Scroll an inner element into view: the container scrolls.
    let deep = id(&doc, "deep");
    e.scroll_into_view(&doc, deep, ScrollAlign::Start, true);
    assert_eq!(e.scroll_position(&doc, bx), (0.0, 220.0));
    let r = rect(&mut e, &doc, "deep");
    assert_eq!(r.y, 10.0 + 80.0);
    // The far button needs the viewport to scroll.
    let far = id(&doc, "far");
    let before = e.bounding_client_rect(&doc, far);
    assert!(before.y > 600.0);
    e.scroll_into_view(&doc, far, ScrollAlign::Center, true);
    let after = e.bounding_client_rect(&doc, far);
    assert!(after.y > 0.0 && after.bottom() <= 600.0, "{after:?}");
    assert_eq!(e.window_scroll(&doc).1, before.y - after.y);
    let root = doc.document_element().unwrap();
    assert_eq!(e.scroll_position(&doc, root), e.window_scroll(&doc));
    let (_, dh) = e.document_size(&doc);
    assert!(dh > 2000.0);
    // Clamped at the end of the document.
    e.scroll_window_to(&doc, 0.0, 1e9);
    assert_eq!(e.window_scroll(&doc).1, dh - 600.0);
}

#[test]
fn form_controls_get_chrome_sizes() {
    let doc = Document::parse_html(
        r#"<input id=t><input id=c type=checkbox><button id=b>Submit</button>
           <input id=s type=submit><img id=i width=40 height=30><canvas id=cv></canvas>"#,
    );
    let mut e = engine();
    let t = rect(&mut e, &doc, "t");
    assert_eq!((t.width, t.height), (153.0, 21.0));
    let c = rect(&mut e, &doc, "c");
    assert_eq!((c.width, c.height), (13.0, 13.0));
    let b = rect(&mut e, &doc, "b");
    let s = rect(&mut e, &doc, "s");
    assert_eq!(b.height, 21.0);
    assert!((b.width - s.width).abs() < 0.01, "{b:?} {s:?}");
    let i = rect(&mut e, &doc, "i");
    assert_eq!((i.width, i.height), (40.0, 30.0));
    let cv = rect(&mut e, &doc, "cv");
    assert_eq!((cv.width, cv.height), (300.0, 150.0));
    // Inline controls sit on one line, left to right.
    assert!(c.x > t.right() && b.x > c.right());
}

#[test]
fn accessibility_tree_of_a_small_form() {
    let doc = Document::parse_html(
        r#"<title>Shop</title><h1>Sign in</h1>
        <form>
          <label for=user>User name</label><input id=user>
          <label>Password <input id=pw type=password></label>
          <input type=checkbox id=keep checked aria-label="Keep me signed in">
          <button id=go>Log <b>in</b></button>
          <input type=submit disabled>
          <a href="/help" id=help>Need help?</a>
          <div hidden><button>Hidden</button></div>
          <img src=x alt="Logo"><span aria-hidden=true>secret</span>
          <span id=lbl>Search site</span><input aria-labelledby=lbl type=search>
        </form>"#,
    );
    let mut e = engine();
    let ax = e.accessibility_tree(&doc);
    assert_eq!(ax.root().role, "RootWebArea");
    assert_eq!(ax.root().name, "Shop");
    let h = ax.query(Some("heading"), Some("Sign in"));
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].states.level, Some(1));
    let user = ax.query(Some("textbox"), Some("User name"));
    assert_eq!(user.len(), 1);
    assert_eq!(user[0].dom, Some(id(&doc, "user")));
    assert_eq!(ax.query(Some("textbox"), Some("Password")).len(), 1);
    let keep = ax.query(Some("checkbox"), Some("Keep me signed in"));
    assert_eq!(keep[0].states.checked, Some(lb_layout::Checked::True));
    assert_eq!(ax.query(Some("button"), Some("Log in")).len(), 1);
    let submit = ax.query(Some("button"), Some("Submit"));
    assert!(submit[0].states.disabled);
    assert_eq!(ax.query(Some("link"), Some("Need help?")).len(), 1);
    assert!(ax.query(Some("button"), Some("Hidden")).is_empty());
    assert_eq!(ax.query(Some("image"), Some("Logo")).len(), 1);
    assert_eq!(ax.query(Some("searchbox"), Some("Search site")).len(), 1);
    assert!(ax.query(None, Some("secret")).is_empty());
    // Document order.
    let roles: Vec<&str> = ax
        .query(None, None)
        .iter()
        .map(|n| n.role.as_str())
        .filter(|r| matches!(*r, "heading" | "textbox" | "checkbox" | "button" | "link"))
        .collect();
    assert_eq!(
        roles,
        [
            "heading", "textbox", "textbox", "checkbox", "button", "button", "link"
        ]
    );
}

#[test]
fn persona_viewport_and_fonts() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../profiles/reference/chrome/chrome153_win10_rtx3060_1080p.json"
    );
    let persona = Arc::new(PersonaProfile::from_path(path).unwrap());
    let (w, h) = (
        persona.screen.inner_width as f32,
        persona.screen.inner_height as f32,
    );
    let mut e = LayoutEngine::new(persona);
    assert_eq!(e.viewport(), (w, h));
    let doc = Document::parse_html(
        r#"<body style="margin:0"><span id=a style="font-family: 'Comic Neue', Arial">Hi</span>
           <span id=b style="font-family: NotInstalled, monospace; font-size:16px">Hi</span>"#,
    );
    // A font missing from the persona falls through to the next family.
    let a = rect(&mut e, &doc, "a");
    let arial = (1479.0 + 455.0) * 16.0 / 2048.0;
    assert!((a.width - arial).abs() < 0.01);
    let b = rect(&mut e, &doc, "b");
    // Chrome's Windows monospace font is Consolas (a derived table: Courier
    // New's 1229 units per character, scaled).
    let consolas = e.fonts().get("Consolas").unwrap().clone();
    assert!(consolas.approximate);
    let adv = consolas
        .face(400.0, false)
        .advance('H', consolas.units_per_em);
    assert!(adv < 1229.0);
    assert!((b.width - 2.0 * adv * 16.0 / consolas.units_per_em).abs() < 0.01);
}

#[test]
fn inline_runs_grid_and_calc() {
    let doc = Document::parse_html(
        r#"<!doctype html><body style="margin:0; font: 16px Arial">
          <p id=p style="margin:0">Click <a id=a href=#>here</a> now</p>
          <div id=c style="text-align:center"><span id=cs>Hi</span></div>
          <div id=g style="display:grid; grid-template-columns: 100px 1fr 2fr; width: 400px">
            <div id=g1>a</div><div id=g2>b</div><div id=g3>c</div><div id=g4>d</div></div>
          <div id=w style="width: calc(50% - 20px); height: 1em; position: relative; left: 10px"></div>"#,
    );
    let mut e = engine();
    let a = rect(&mut e, &doc, "a");
    let click = (1479.0 + 455.0 + 455.0 + 1024.0 + 1024.0 + 569.0) * 16.0 / 2048.0;
    assert!((a.x - click).abs() < 0.01, "{a:?}");
    assert_eq!(a.y, 0.0);
    assert_eq!(
        e.element_from_point(&doc, a.x + 1.0, 5.0),
        Some(id(&doc, "a"))
    );
    let cs = rect(&mut e, &doc, "cs");
    assert!((cs.x + cs.width / 2.0 - 400.0).abs() < 0.01, "{cs:?}");
    let g2 = rect(&mut e, &doc, "g2");
    let g3 = rect(&mut e, &doc, "g3");
    let g4 = rect(&mut e, &doc, "g4");
    assert_eq!(g2.x, 100.0);
    assert_eq!(g2.width, 100.0);
    assert_eq!(g3.x, 200.0);
    assert_eq!(g4.x, 0.0);
    assert_eq!(g4.y, g2.bottom());
    let w = rect(&mut e, &doc, "w");
    assert_eq!((w.x, w.width, w.height), (10.0, 380.0, 16.0));
}
