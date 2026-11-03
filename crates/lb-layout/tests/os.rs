//! Rendering per OS (M7 WP-C): scrollbars, default fonts, the Linux alias
//! layer and emoji, checked against every reference persona and against
//! the Linux capture of Chrome for Testing 153 (`tests/fixtures/os/`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lb_dom::{Document, NodeId};
use lb_layout::LayoutEngine;
use lb_persona::{OsKind, PersonaProfile};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn reference_personas() -> Vec<(String, Arc<PersonaProfile>)> {
    let dir = root().join("profiles/reference/chrome");
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, Arc::new(PersonaProfile::from_path(&p).unwrap()))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(out.len() >= 8);
    out
}

fn persona(file: &str) -> Arc<PersonaProfile> {
    let p = root().join("profiles/reference/chrome").join(file);
    Arc::new(PersonaProfile::from_path(&p).unwrap())
}

fn id(doc: &Document, s: &str) -> NodeId {
    doc.get_element_by_id(s).unwrap_or_else(|| panic!("#{s}"))
}

fn offset(e: &mut LayoutEngine, doc: &Document, s: &str) -> (f32, f32) {
    e.offset_size(doc, id(doc, s))
}

fn client(e: &mut LayoutEngine, doc: &Document, s: &str) -> (f32, f32) {
    e.client_size(doc, id(doc, s))
}

#[test]
fn scrollbars_per_reference_persona() {
    let tall = r#"<!doctype html><body>
        <div id=scroll style="width:100px;height:100px;overflow:scroll"></div>
        <div id=auto style="width:100px;height:100px;overflow:auto"><div style="height:300px"></div></div>
        <div id=autofit style="width:100px;height:100px;overflow:auto"><div style="height:50px"></div></div>
        <div id=hidden style="width:100px;height:100px;overflow:hidden"><div style="height:300px"></div></div>
        <textarea id=ta></textarea>
        <div style="height:5000px"></div>"#;
    for (name, p) in reference_personas() {
        let sb = p.scrollbar_width() as f32;
        let mut e = LayoutEngine::new(p.clone());
        let (vw, _) = e.viewport();
        let doc = Document::parse_html(tall);
        let html = doc.document_element().unwrap();
        // overflow: scroll always takes the scrollbar's width and height.
        let (ow, oh) = offset(&mut e, &doc, "scroll");
        let (cw, ch) = client(&mut e, &doc, "scroll");
        assert_eq!((ow - cw, oh - ch), (sb, sb), "{name}: overflow:scroll");
        // overflow: auto only when the content overflows (vertical only).
        let (ow, oh) = offset(&mut e, &doc, "auto");
        let (cw, ch) = client(&mut e, &doc, "auto");
        assert_eq!((ow - cw, oh - ch), (sb, 0.0), "{name}: overflowing auto");
        assert_eq!(client(&mut e, &doc, "autofit"), (100.0, 100.0), "{name}");
        assert_eq!(client(&mut e, &doc, "hidden"), (100.0, 100.0), "{name}");
        // The viewport of an overflowing document.
        let (rw, rh) = e.client_size(&doc, html);
        assert_eq!((vw - rw, rh), (sb, e.viewport().1), "{name}: root");
        // Blink sizes a textarea for its scrollbar: 20 columns of the
        // monospace '0', 2px padding and 1px border on each side, plus
        // the scrollbar.
        let (tw, _) = offset(&mut e, &doc, "ta");
        let (tc, _) = client(&mut e, &doc, "ta");
        // (Both round on their own, so they may differ by one.)
        assert!(
            (tw - tc - 2.0).abs() <= 1.0,
            "{name}: textarea has no scrollbar showing"
        );
        let style = e.computed_style(&doc, id(&doc, "ta")).unwrap();
        let zero = e.font_for(&style).measure("0");
        assert!(
            (tw - (20.0 * zero + sb + 6.0)).abs() <= 1.0,
            "{name}: textarea {tw} vs {}",
            20.0 * zero + sb + 6.0
        );
        // Content of the root is laid out next to the scrollbar.
        let body = doc.body().unwrap();
        assert_eq!(e.offset_size(&doc, body).0, vw - sb - 16.0, "{name}: body");

        // A document that fits has no viewport scrollbar; one with
        // `overflow: hidden` on the root has none either.
        for html_src in [
            "<!doctype html><body><p>short</p>",
            "<!doctype html><html style='overflow:hidden'><body><div style='height:5000px'></div>",
            "<!doctype html><body style='overflow:hidden'><div style='height:5000px'></div>",
        ] {
            // One layout engine per document.
            let mut e = LayoutEngine::new(p.clone());
            let doc = Document::parse_html(html_src);
            let html = doc.document_element().unwrap();
            assert_eq!(e.client_size(&doc, html).0, vw, "{name}: {html_src}");
        }
        // `overflow: scroll` on the root always shows one.
        let mut e = LayoutEngine::new(p.clone());
        let doc =
            Document::parse_html("<!doctype html><html style='overflow-y:scroll'><body>x</body>");
        let html = doc.document_element().unwrap();
        assert_eq!(e.client_size(&doc, html).0, vw - sb, "{name}: scroll root");
    }
}

/// Chrome's widths and heights at 72px of the capture string, from the
/// Linux capture with Ubuntu-like fontconfig (`tools/os_truth`).
#[test]
fn linux_fonts_match_capture() {
    let fixture: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root().join("tests/fixtures/os/linux_x11_cft153_ubuntu24fonts.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let fonts = &fixture["result"]["fonts"];
    let dims = |v: &serde_json::Value| -> (f32, f32) {
        (v[0].as_f64().unwrap() as f32, v[1].as_f64().unwrap() as f32)
    };
    // Families the Ubuntu and Fedora personas have (or alias), plus the
    // generic keywords whose default font they install.
    let families = [
        "Arial",
        "Helvetica",
        "Arimo",
        "Liberation Sans",
        "Times New Roman",
        "Times",
        "Tinos",
        "Liberation Serif",
        "Courier New",
        "Courier",
        "Cousine",
        "Liberation Mono",
    ];
    let ubuntu_only = ["DejaVu Sans", "DejaVu Serif", "DejaVu Sans Mono"];
    let generics = [
        "serif",
        "sans-serif",
        "cursive",
        "fantasy",
        "math",
        "fangsong",
        "ui-serif",
        "ui-sans-serif",
        "ui-monospace",
        "ui-rounded",
        "initial",
    ];
    for file in [
        "chrome153_ubuntu24_intel_uhd630_x11_1080p.json",
        "chrome153_fedora42_amd_rx6600_wayland_1440p.json",
    ] {
        let p = persona(file);
        let ubuntu = file.contains("ubuntu");
        let mut cases: Vec<(String, (f32, f32))> = Vec::new();
        for f in families
            .iter()
            .chain(if ubuntu { &ubuntu_only[..] } else { &[] })
        {
            cases.push((format!("\"{f}\""), dims(&fonts["detectedDims"][*f])));
        }
        for g in generics {
            cases.push((g.to_owned(), dims(&fonts["generic"][g])));
        }
        if ubuntu {
            // DejaVu Sans Mono is Ubuntu's monospace (Noto Sans Mono on
            // Fedora, not in the capture).
            cases.push(("monospace".into(), dims(&fonts["generic"]["monospace"])));
        }
        for (ff, want) in cases {
            let doc = Document::parse_html(&format!(
                "<!doctype html><body><span id=s style='position:absolute;left:-9999px;top:0;\
                 font-size:72px;white-space:nowrap;font-family:{ff}'>mmmmmmmmmmlli1WQ@#é</span>"
            ));
            let mut e = LayoutEngine::new(p.clone());
            let got = offset(&mut e, &doc, "s");
            assert_eq!(got, want, "{file}: font-family {ff}");
        }
    }
}

#[test]
fn generic_fonts_follow_persona_defaults() {
    for (name, p) in reference_personas() {
        let d = p.font_defaults();
        let os = p.os_kind();
        let width = |_: &mut (), ff: &str| {
            let mut e = LayoutEngine::new(p.clone());
            let doc = Document::parse_html(&format!(
                "<!doctype html><body><span id=s style='font-size:72px;white-space:nowrap;\
                 font-family:{ff}'>mmmmmmmmmmlli1WQ@#é</span>"
            ));
            offset(&mut e, &doc, "s")
        };
        let mut e = ();
        let q = |f: &str| format!("\"{f}\"");
        // sans-serif and serif are the persona's defaults (through the
        // Linux aliases: Arial is Liberation Sans).
        assert_eq!(
            width(&mut e, "sans-serif"),
            width(&mut e, &q(&d.sans_serif)),
            "{name}"
        );
        assert_eq!(
            width(&mut e, "serif"),
            width(&mut e, &q(&d.serif)),
            "{name}"
        );
        assert_eq!(
            width(&mut e, "monospace"),
            width(&mut e, &q(&d.monospace)),
            "{name}"
        );
        assert_ne!(width(&mut e, "monospace"), width(&mut e, "serif"), "{name}");
        match os {
            OsKind::Windows => {
                assert_eq!(
                    width(&mut e, "system-ui"),
                    width(&mut e, "\"Segoe UI\""),
                    "{name}"
                );
                assert_eq!(
                    width(&mut e, "monospace"),
                    width(&mut e, "Consolas"),
                    "{name}"
                );
            }
            OsKind::MacOs => {
                assert_eq!(
                    width(&mut e, "sans-serif"),
                    width(&mut e, "Helvetica"),
                    "{name}"
                );
                assert_eq!(
                    width(&mut e, "-apple-system"),
                    width(&mut e, "system-ui"),
                    "{name}"
                );
            }
            OsKind::Linux => {
                assert_eq!(
                    width(&mut e, "system-ui"),
                    width(&mut e, &q(&d.system_ui)),
                    "{name}"
                );
                assert_ne!(width(&mut e, "system-ui"), width(&mut e, "Arial"), "{name}");
                // Arial and Helvetica are Liberation Sans; Segoe UI is
                // not installed and not aliased.
                assert_eq!(
                    width(&mut e, "Arial, monospace"),
                    width(&mut e, "\"Liberation Sans\"")
                );
                assert_eq!(
                    width(&mut e, "Helvetica, monospace"),
                    width(&mut e, "Arial")
                );
                assert_eq!(
                    width(&mut e, "\"Segoe UI\", monospace"),
                    width(&mut e, "monospace"),
                    "{name}"
                );
                // ui-* keywords are macOS only: the standard font.
                assert_eq!(
                    width(&mut e, "ui-monospace"),
                    width(&mut e, "serif"),
                    "{name}"
                );
            }
            OsKind::Other => {}
        }
    }
}

#[test]
fn emoji_advance_and_line_height_per_os() {
    for (name, p) in reference_personas() {
        let emoji = p.font_defaults().emoji;
        let want_w: f32 = match emoji.as_str() {
            "Noto Color Emoji" => 125.0,
            "Apple Color Emoji" => 125.0,
            "Segoe UI Emoji" => 130.0,
            other => panic!("{name}: emoji font {other}"),
        };
        let mut e = LayoutEngine::new(p.clone());
        let doc = Document::parse_html(
            "<!doctype html><body><span id=s style='font-size:100px;white-space:nowrap'>\u{1F600}</span>\
             <span id=t style='font-size:100px;white-space:nowrap'>A</span>",
        );
        let (w, h) = offset(&mut e, &doc, "s");
        let (_, th) = offset(&mut e, &doc, "t");
        assert_eq!(w, want_w, "{name}");
        assert!(h >= th, "{name}: emoji line {h} < text line {th}");
        if p.os_kind() == OsKind::Linux {
            // Captured: [125, 117] (Noto Color Emoji 1900/500 at 2048 upem).
            assert_eq!((w, h), (125.0, 117.0), "{name}");
        }
    }
}

#[test]
fn system_font_keywords() {
    for (name, p) in reference_personas() {
        let d = p.font_defaults();
        let mut e = LayoutEngine::new(p.clone());
        let doc = Document::parse_html(
            "<!doctype html><body><p id=c style='font: caption'>x</p>\
             <p id=m style='font: bold 20px serif; font: menu'>x</p>\
             <p id=s style='font: -webkit-small-control'>x</p><input id=i>",
        );
        for el in ["c", "m"] {
            let n = id(&doc, el);
            assert_eq!(
                e.resolved_style_value(&doc, n, "font-size"),
                lb_css::fmt_px(d.system_font_px as f32),
                "{name}"
            );
            assert_eq!(
                e.resolved_style_value(&doc, n, "font-weight"),
                "400",
                "{name}"
            );
            let fam = e.resolved_style_value(&doc, n, "font-family");
            assert_eq!(fam.trim_matches('"'), d.system_font, "{name}");
        }
        let s = id(&doc, "s");
        let i = id(&doc, "i");
        assert_eq!(
            e.resolved_style_value(&doc, s, "font-size"),
            "13.3333px",
            "{name}"
        );
        assert_eq!(
            e.resolved_style_value(&doc, s, "font-family"),
            e.resolved_style_value(&doc, i, "font-family"),
            "{name}"
        );
    }
    // Captured on Linux: every keyword is Arial 16px.
    let p = persona("chrome153_ubuntu24_intel_uhd630_x11_1080p.json");
    let d = p.font_defaults();
    assert_eq!((d.system_font.as_str(), d.system_font_px), ("Arial", 16.0));
}

/// The Linux capture's scrollbar probe: `overflow: scroll` div, root of a
/// tall page and a default textarea.
#[test]
fn linux_scrollbars_match_capture() {
    let fixture: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("tests/fixtures/os/linux_x11_cft153.json")).unwrap(),
    )
    .unwrap();
    let sb = &fixture["result"]["scrollbar"];
    let n = |v: &serde_json::Value| v.as_f64().unwrap() as f32;
    let p = persona("chrome153_ubuntu24_intel_uhd630_x11_1080p.json");
    let mut e = LayoutEngine::new(p);
    let doc = Document::parse_html(
        "<!doctype html><body><div id=d style='position:absolute;left:-9999px;width:100px;\
         height:100px;overflow:scroll'></div><textarea id=ta></textarea>\
         <div style='height:3000px'></div>",
    );
    let (ow, oh) = offset(&mut e, &doc, "d");
    let (cw, ch) = client(&mut e, &doc, "d");
    assert_eq!((ow - cw, oh - ch), (n(&sb["div"]), n(&sb["divH"])));
    let html = doc.document_element().unwrap();
    let (vw, vh) = e.viewport();
    let (rw, rh) = e.client_size(&doc, html);
    assert_eq!((vw - rw, vh - rh), (n(&sb["root"]), n(&sb["rootH"])));
    let ta = &sb["textarea"];
    let (tw, th) = offset(&mut e, &doc, "ta");
    assert_eq!((tw, th), (n(&ta["offsetWidth"]), n(&ta["offsetHeight"])));
    assert_eq!(client(&mut e, &doc, "ta").0, n(&ta["clientWidth"]));
}

/// A persona listing a foreign font (Segoe UI on Linux, the adversarial
/// `linux_segoe_fonts`) gets it detected with that font's table.
#[test]
fn foreign_fonts_are_detectable() {
    let p = Arc::new(
        PersonaProfile::from_path(
            root().join("profiles/generated/adversarial/chrome153_linux_segoe_fonts.json"),
        )
        .unwrap(),
    );
    assert!(p.fonts.available.iter().any(|f| f == "Segoe UI"));
    let width = |ff: &str| {
        let mut e = LayoutEngine::new(p.clone());
        let doc = Document::parse_html(&format!(
            "<!doctype html><body><span id=s style='font-size:72px;white-space:nowrap;\
             font-family:{ff}'>mmmmmmmmmmlli1WQ@#</span>"
        ));
        offset(&mut e, &doc, "s")
    };
    assert_ne!(width("\"Segoe UI\", monospace"), width("monospace"));
    // A consistent Linux persona does not have it.
    let u = persona("chrome153_ubuntu24_intel_uhd630_x11_1080p.json");
    let mut e = LayoutEngine::new(u);
    assert!(e.fonts().get("Segoe UI").is_none());
    let doc = Document::parse_html("<p>x</p>");
    let _ = e.client_size(&doc, doc.document_element().unwrap());
}
