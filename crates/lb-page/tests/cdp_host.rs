//! `CdpHost` driven through `lb_cdp::PageHost` directly (no WebSocket):
//! navigation, Runtime, DOM, Input and cookies on real pages.
#![cfg(feature = "chrome")]

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{Resource, serve};
use lb_cdp::types::{CallArgument, CookieParam, KeyEvent, MouseEvent, NodeRef};
use lb_cdp::{
    BindingScope, CallFunctionRequest, CallTarget, ContextOptions, EvaluateRequest, EventSink,
    HostEvent, NavigateRequest, PageEvent, PageHost,
};
use lb_page::{CdpHost, CdpHostOptions};
use lb_persona::PersonaProfile;
use serde_json::json;

fn host() -> CdpHost {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    let p = PersonaProfile::from_path_checked(
        root.join("reference/chrome/chrome153_win10_rtx3060_1080p.json"),
    )
    .expect("reference persona");
    let mut opts = CdpHostOptions::new(Arc::new(p), root.join("tls"));
    opts.workers = 1;
    CdpHost::new(opts).expect("host")
}

async fn eval(h: &CdpHost, t: &str, expr: &str) -> serde_json::Value {
    let ctx = h.main_context(t).unwrap();
    let r = h
        .evaluate(
            t,
            EvaluateRequest {
                context_id: ctx,
                expression: expr.into(),
                return_by_value: true,
                await_promise: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        r.exception_details.is_none(),
        "{expr}: {:?}",
        r.exception_details
    );
    r.result.value.unwrap_or(serde_json::Value::Null)
}

const PAGE: &str = "<!DOCTYPE html><title>Host</title><body style='margin:0'>\
    <button id=b style='width:100px;height:40px' onclick=\"this.textContent='done:'+event.isTrusted\">go</button>\
    <input id=i style='width:100px;height:20px'></body>";

// Isolated worlds are reported at once and get their realm on first use:
// bindings added before that are there, the DOM is shared, globals are not.
#[test]
fn isolated_worlds_made_on_first_use() {
    lb_cdp::block_on_local(async {
        let s = serve(vec![("/", Resource::html(PAGE))]).await;
        let h = host();
        h.create_context("C", ContextOptions::default()).unwrap();
        h.create_page("C", "T").await.unwrap();
        h.add_script_to_evaluate_on_new_document("T", "", Some("auto"), false)
            .unwrap();
        h.navigate(
            "T",
            NavigateRequest {
                url: s.url("/"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        eval(&h, "T", "window.mainOnly = 1").await;
        let frame = h.frame_tree("T").unwrap().frame.id;
        let w = h.create_isolated_world("T", &frame, "util", true).unwrap();
        h.add_binding("T", "pwBinding", BindingScope::AllIsolatedWorlds)
            .unwrap();
        let ev = |expr: &str| EvaluateRequest {
            context_id: w,
            expression: expr.into(),
            return_by_value: true,
            ..Default::default()
        };
        let r = h
            .evaluate(
                "T",
                ev("[typeof pwBinding, document.title, typeof mainOnly, \
                    document.getElementById('b').textContent, typeof HTMLDivElement]"),
            )
            .await
            .unwrap();
        assert!(r.exception_details.is_none(), "{:?}", r.exception_details);
        assert_eq!(
            r.result.value,
            Some(json!(["function", "Host", "undefined", "go", "function"]))
        );
        // The same realm on later use.
        h.evaluate("T", ev("globalThis.kept = 7")).await.unwrap();
        let r = h.evaluate("T", ev("kept")).await.unwrap();
        assert_eq!(r.result.value, Some(json!(7)));
        // The main world never sees the isolated world's globals.
        assert_eq!(
            eval(&h, "T", "typeof kept + typeof pwBinding").await,
            json!("undefinedundefined")
        );
        h.close_page("T").unwrap();
    })
    .unwrap();
}

#[test]
fn runtime_dom_input_and_cookies() {
    lb_cdp::block_on_local(async {
        let s = serve(vec![("/", Resource::html(PAGE))]).await;
        let h = host();
        h.create_context("C", ContextOptions::default()).unwrap();
        h.create_page("C", "T").await.unwrap();
        let nav = h
            .navigate(
                "T",
                NavigateRequest {
                    url: s.url("/"),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(nav.error_text.is_none() && nav.loader_id.is_some());
        assert_eq!(h.frame_tree("T").unwrap().frame.url, s.url("/"));
        assert_eq!(eval(&h, "T", "1 + 1").await, json!(2));
        assert_eq!(
            eval(
                &h,
                "T",
                "new Promise(r => setTimeout(() => r({a: [1, 'x']}), 5))"
            )
            .await,
            json!({"a": [1, "x"]})
        );

        // Exceptions carry the thrown value.
        let ctx = h.main_context("T").unwrap();
        let r = h
            .evaluate(
                "T",
                EvaluateRequest {
                    context_id: ctx,
                    expression: "throw new RangeError('nope')".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let d = r.exception_details.expect("exception");
        let e = d.exception.expect("exception object");
        assert_eq!(e.class_name.as_deref(), Some("RangeError"));
        assert!(e.description.unwrap().starts_with("RangeError: nope"));

        // Objects: ids identify the realm and work as call arguments.
        let obj = h
            .evaluate(
                "T",
                EvaluateRequest {
                    context_id: ctx,
                    expression: "document.getElementById('b')".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .result;
        assert_eq!(obj.subtype.as_deref(), Some("node"));
        assert_eq!(obj.description.as_deref(), Some("button#b"));
        let oid = obj.object_id.clone().unwrap();
        let r = h
            .call_function_on(
                "T",
                CallFunctionRequest {
                    function_declaration: "function(x) { return this.id + x.id }".into(),
                    target: CallTarget::Object(oid.clone()),
                    arguments: vec![CallArgument {
                        object_id: Some(oid.clone()),
                        ..Default::default()
                    }],
                    object_group: None,
                    return_by_value: true,
                    await_promise: false,
                    generate_preview: false,
                    user_gesture: false,
                    silent: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(r.result.value, Some(json!("bb")));

        // DOM: the node, its box, and back to an object.
        let node = h.request_node("T", &oid).unwrap();
        let bm = h.box_model("T", &NodeRef::Backend(node)).unwrap();
        assert_eq!((bm.width, bm.height), (100.0, 40.0));
        let doc = h.get_document("T", -1, false).unwrap();
        assert_eq!(doc.node_name, "#document");
        let q = h.query_selector("T", doc.node_id, "#i").unwrap().unwrap();
        assert!(h.box_model("T", &NodeRef::Backend(q)).is_ok());

        // Input: a trusted click at the button's center.
        let (x, y) = (50.0, 20.0);
        for kind in ["mouseMoved", "mousePressed", "mouseReleased"] {
            h.dispatch_mouse_event(
                "T",
                MouseEvent {
                    kind: kind.into(),
                    x,
                    y,
                    button: Some("left".into()),
                    click_count: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
        assert_eq!(
            eval(&h, "T", "document.getElementById('b').textContent").await,
            json!("done:true")
        );
        // Typing into the focused input.
        eval(&h, "T", "document.getElementById('i').focus()").await;
        h.insert_text("T", "hey").await.unwrap();
        h.dispatch_key_event(
            "T",
            KeyEvent {
                kind: "keyDown".into(),
                key: Some("!".into()),
                text: Some("!".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            eval(&h, "T", "document.getElementById('i').value").await,
            json!("hey!")
        );

        // Cookies through the context's jar.
        h.set_cookies(
            "C",
            vec![CookieParam {
                name: "k".into(),
                value: "v".into(),
                url: Some(s.url("/")),
                ..Default::default()
            }],
        )
        .unwrap();
        assert_eq!(eval(&h, "T", "document.cookie").await, json!("k=v"));
        let all = h.cookies("C", None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].domain, "127.0.0.1");
        h.clear_cookies("C").unwrap();
        assert!(h.cookies("C", None).unwrap().is_empty());

        // The AX tree has the button.
        let ax = h.ax_tree("T").unwrap();
        assert!(
            ax.iter()
                .any(|n| n.role.as_ref().and_then(|r| r.value.clone()) == Some(json!("button")))
        );
        h.close_page("T").unwrap();
    })
    .unwrap();
}

#[test]
fn media_query_list_change_on_emulation() {
    lb_cdp::block_on_local(async {
        let s = serve(vec![("/", Resource::html(PAGE))]).await;
        let h = host();
        h.create_context("C", ContextOptions::default()).unwrap();
        h.create_page("C", "T").await.unwrap();
        h.navigate(
            "T",
            NavigateRequest {
                url: s.url("/"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        eval(
            &h,
            "T",
            "window.__n = 0; const m = matchMedia('(prefers-color-scheme: dark)'); \
             m.addEventListener('change', e => { window.__n++; window.__m = e.matches; }); \
             window.__dark = m",
        )
        .await;
        assert_eq!(eval(&h, "T", "window.__dark.matches").await, json!(false));
        h.set_emulated_media(
            "T",
            lb_css::MediaOverrides {
                prefers_dark: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            eval(&h, "T", "[window.__n, window.__m, window.__dark.matches]").await,
            json!([1, true, true])
        );
        h.close_page("T").unwrap();
    })
    .unwrap();
}

#[test]
fn emulate_media_reaches_child_frames() {
    lb_cdp::block_on_local(async {
        let outer = "<!DOCTYPE html><body style='margin:0'>\
            <iframe id=f style='border:0;width:400px;height:200px' src='/child'></iframe>";
        let child = "<!DOCTYPE html><body>x</body>";
        let s = serve(vec![
            ("/", Resource::html(outer)),
            ("/child", Resource::html(child)),
        ])
        .await;
        let h = host();
        h.create_context("C", ContextOptions::default()).unwrap();
        h.create_page("C", "T").await.unwrap();
        h.navigate(
            "T",
            NavigateRequest {
                url: s.url("/"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let ready = std::time::Instant::now();
        loop {
            let ok = eval(
                &h,
                "T",
                "!!document.getElementById('f')?.contentDocument?.body",
            )
            .await;
            if ok == json!(true) {
                break;
            }
            assert!(ready.elapsed() < std::time::Duration::from_secs(5));
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let probe = "document.getElementById('f').contentWindow\
            .matchMedia('(prefers-color-scheme: dark)').matches";
        assert_eq!(eval(&h, "T", probe).await, json!(false));
        h.set_emulated_media(
            "T",
            lb_css::MediaOverrides {
                prefers_dark: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(eval(&h, "T", probe).await, json!(true));
        // Kept for a later navigation in the frame.
        eval(
            &h,
            "T",
            &format!("document.getElementById('f').src = '{}'", s.url("/child")),
        )
        .await;
        let again = std::time::Instant::now();
        loop {
            let ok = eval(&h, "T", probe).await;
            if ok == json!(true) {
                break;
            }
            assert!(again.elapsed() < std::time::Duration::from_secs(5));
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        h.close_page("T").unwrap();
    })
    .unwrap();
}

#[test]
fn click_reaches_button_inside_iframe() {
    lb_cdp::block_on_local(async {
        // Same-origin child doc (avoid srcdoc quote escaping). Button is
        // 80x30 at (0,0) in a marginless body.
        let outer = "<!DOCTYPE html><body style='margin:0'>\
            <iframe id=f style='border:0;width:400px;height:200px' src='/child'></iframe>";
        let child = "<!DOCTYPE html><body style='margin:0'>\
            <button id=b style='width:80px;height:30px' \
            onclick=\"document.body.dataset.c='1'\">go</button></body>";
        let s = serve(vec![
            ("/", Resource::html(outer)),
            ("/child", Resource::html(child)),
        ])
        .await;
        let h = host();
        h.create_context("C", ContextOptions::default()).unwrap();
        h.create_page("C", "T").await.unwrap();
        h.navigate(
            "T",
            NavigateRequest {
                url: s.url("/"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // Poll from Rust: `await_promise` + `setInterval` never advances
        // timers while the evaluate job is waiting on the promise.
        let ready = std::time::Instant::now();
        loop {
            let ok = eval(
                &h,
                "T",
                "!!document.getElementById('f')?.contentDocument?.getElementById('b')",
            )
            .await;
            if ok == json!(true) {
                break;
            }
            assert!(
                ready.elapsed() < std::time::Duration::from_secs(5),
                "iframe button never appeared"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // Child-frame objectId geometry (Playwright frameLocator path).
        let child = h.frame_tree("T").unwrap().child_frames[0].frame.id.clone();
        let ctx = h.create_isolated_world("T", &child, "util", true).unwrap();
        let oid = h
            .evaluate(
                "T",
                EvaluateRequest {
                    context_id: ctx,
                    expression: "document.getElementById('b')".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .result
            .object_id
            .expect("button object id");
        let quads = h.content_quads("T", &NodeRef::Object(oid)).unwrap();
        assert!(!quads.is_empty(), "child-frame content quads");
        // Page-level centre of the button: iframe box + local quads
        // (content_quads synced the child viewport to the iframe).
        let fr = eval(
            &h,
            "T",
            "(() => { const r = document.getElementById('f').getBoundingClientRect();\
             return [r.x, r.y]; })()",
        )
        .await;
        let q = &quads[0];
        let (x, y) = (
            fr[0].as_f64().unwrap() + (q[0] + q[2]) / 2.0,
            fr[1].as_f64().unwrap() + (q[1] + q[5]) / 2.0,
        );
        for kind in ["mouseMoved", "mousePressed", "mouseReleased"] {
            h.dispatch_mouse_event(
                "T",
                MouseEvent {
                    kind: kind.into(),
                    x,
                    y,
                    button: Some("left".into()),
                    click_count: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
        assert_eq!(
            eval(
                &h,
                "T",
                "document.getElementById('f').contentDocument.body.dataset.c"
            )
            .await,
            json!("1")
        );
        h.close_page("T").unwrap();
    })
    .unwrap();
}

#[test]
fn console_reports_isolated_world_context_id() {
    lb_cdp::block_on_local(async {
        let s = serve(vec![("/", Resource::html(PAGE))]).await;
        let h = host();
        let (sink, mut rx) = EventSink::channel();
        h.set_event_sink(sink);
        h.create_context("C", ContextOptions::default()).unwrap();
        h.create_page("C", "T").await.unwrap();
        h.navigate(
            "T",
            NavigateRequest {
                url: s.url("/"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // Drain navigation noise.
        while rx.try_recv().is_ok() {}
        let frame = h.frame_tree("T").unwrap().frame.id;
        let world = h
            .create_isolated_world("T", &frame, "console-test", true)
            .unwrap();
        h.evaluate(
            "T",
            EvaluateRequest {
                context_id: world,
                expression: "console.log('isolated world')".into(),
                return_by_value: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut got = None;
        while let Ok(HostEvent { event, .. }) = rx.try_recv() {
            if let PageEvent::ConsoleApiCalled {
                context_id, args, ..
            } = event
            {
                got = Some((context_id, args));
            }
        }
        let (ctx, args) = got.expect("Runtime.consoleAPICalled");
        assert_eq!(
            args[0].value.as_ref().and_then(|v| v.as_str()),
            Some("isolated world")
        );
        assert_eq!(ctx, world, "console must name the isolated world, not main");
        h.close_page("T").unwrap();
    })
    .unwrap();
}

#[test]
fn print_to_pdf_has_selectable_text() {
    lb_cdp::block_on_local(async {
        let html = "<!DOCTYPE html><title>T</title><body><h1>Report</h1>\
            <p>Paragraph text for the PDF layer.</p></body>";
        let s = serve(vec![("/", Resource::html(html))]).await;
        let h = host();
        h.create_context("C", ContextOptions::default()).unwrap();
        h.create_page("C", "T").await.unwrap();
        h.navigate(
            "T",
            NavigateRequest {
                url: s.url("/"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let b64 = h.print_to_pdf("T", &json!({})).expect("Page.printToPDF");
        let pdf = lb_paint::encode::base64_decode(&b64).expect("pdf base64");
        let latin = String::from_utf8_lossy(&pdf);
        assert!(latin.starts_with("%PDF-1.4"));
        assert!(
            latin.contains("/BaseFont /Helvetica"),
            "expected Helvetica text layer"
        );
        assert!(
            latin.contains("(Report)"),
            "expected selectable heading text: missing (Report)"
        );
        h.close_page("T").unwrap();
    })
    .unwrap();
}

#[test]
fn request_node_reaches_child_frame() {
    lb_cdp::block_on_local(async {
        let outer = "<!DOCTYPE html><body style='margin:0'>\
            <iframe id=f style='border:0;width:400px;height:200px' src='/child'></iframe>";
        let child = "<!DOCTYPE html><body style='margin:0'>\
            <button id=b style='width:80px;height:30px'>go</button></body>";
        let s = serve(vec![
            ("/", Resource::html(outer)),
            ("/child", Resource::html(child)),
        ])
        .await;
        let h = host();
        h.create_context("C", ContextOptions::default()).unwrap();
        h.create_page("C", "T").await.unwrap();
        h.navigate(
            "T",
            NavigateRequest {
                url: s.url("/"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let ready = std::time::Instant::now();
        loop {
            let ok = eval(
                &h,
                "T",
                "!!document.getElementById('f')?.contentDocument?.getElementById('b')",
            )
            .await;
            if ok == json!(true) {
                break;
            }
            assert!(
                ready.elapsed() < std::time::Duration::from_secs(5),
                "iframe button never appeared"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let child = h.frame_tree("T").unwrap().child_frames[0].frame.id.clone();
        let ctx = h.create_isolated_world("T", &child, "util", true).unwrap();
        let oid = h
            .evaluate(
                "T",
                EvaluateRequest {
                    context_id: ctx,
                    expression: "document.getElementById('b')".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .result
            .object_id
            .expect("button object id");
        // Backend ids are document-key encoded: child-frame requestNode
        // must round-trip through resolveNode / getBoxModel.
        let backend = h.request_node("T", &oid).expect("child-frame requestNode");
        let again = h
            .resolve_node("T", backend, Some(ctx), None)
            .expect("resolveNode on child backend id");
        assert_eq!(again.subtype.as_deref(), Some("node"));
        assert_eq!(again.description.as_deref(), Some("button#b"));
        let bm = h
            .box_model("T", &NodeRef::Backend(backend))
            .expect("box model of child-frame backend id");
        assert_eq!((bm.width, bm.height), (80.0, 30.0));
        let owner = h.get_frame_owner("T", &child).expect("frame owner");
        let owner_html = h
            .outer_html("T", &NodeRef::Backend(owner))
            .expect("iframe outerHTML");
        assert!(
            owner_html.contains("<iframe"),
            "frame owner should be the iframe element: {owner_html}"
        );
        h.close_page("T").unwrap();
    })
    .unwrap();
}

#[test]
fn beforeunload_reaches_child_frames() {
    use std::rc::Rc;
    use std::time::Duration;

    lb_cdp::block_on_local(async {
        // Parent has no handler; the child cancels and counts on the parent.
        let outer = "<!DOCTYPE html><body style='margin:0'>\
            <iframe id=f style='border:0;width:400px;height:200px' src='/child'></iframe>";
        let child = "<!DOCTYPE html><body style='margin:0'>\
            <p id=p style='width:100px;height:40px'>x</p>\
            <script>addEventListener('beforeunload', e => {\
              parent.__bu = (parent.__bu || 0) + 1; e.preventDefault(); })</script></body>";
        let s = serve(vec![
            ("/", Resource::html(outer)),
            ("/child", Resource::html(child)),
        ])
        .await;
        let h = Rc::new(host());
        h.create_context("C", ContextOptions::default()).unwrap();
        h.create_page("C", "T").await.unwrap();
        h.navigate(
            "T",
            NavigateRequest {
                url: s.url("/"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let ready = std::time::Instant::now();
        loop {
            let ok = eval(
                &h,
                "T",
                "!!document.getElementById('f')?.contentDocument?.getElementById('p')",
            )
            .await;
            if ok == json!(true) {
                break;
            }
            assert!(
                ready.elapsed() < Duration::from_secs(5),
                "iframe never appeared"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // No gesture / no dialog host answer: the child handler still runs.
        assert!(h.run_before_unload("T").await.unwrap());
        assert_eq!(eval(&h, "T", "window.__bu").await, json!(1));

        // Gesture in the child, then dismiss the dialog: stay on the page.
        let fr = eval(
            &h,
            "T",
            "(() => { const r = document.getElementById('f').getBoundingClientRect();\
             return [r.x, r.y]; })()",
        )
        .await;
        let (x, y) = (
            fr[0].as_f64().unwrap() + 50.0,
            fr[1].as_f64().unwrap() + 20.0,
        );
        for kind in ["mouseMoved", "mousePressed", "mouseReleased"] {
            h.dispatch_mouse_event(
                "T",
                MouseEvent {
                    kind: kind.into(),
                    x,
                    y,
                    button: Some("left".into()),
                    click_count: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
        h.set_dialog_handling("T", true);
        let answer = h.clone();
        tokio::task::spawn_local(async move {
            for _ in 0..200 {
                tokio::time::sleep(Duration::from_millis(10)).await;
                if answer.handle_javascript_dialog("T", false, None).is_ok() {
                    return;
                }
            }
            panic!("beforeunload dialog never opened");
        });
        assert!(
            !h.run_before_unload("T").await.unwrap(),
            "dismissed child beforeunload must keep the page"
        );
        assert_eq!(eval(&h, "T", "window.__bu").await, json!(2));

        // Accept: close proceeds.
        let answer = h.clone();
        tokio::task::spawn_local(async move {
            for _ in 0..200 {
                tokio::time::sleep(Duration::from_millis(10)).await;
                if answer.handle_javascript_dialog("T", true, None).is_ok() {
                    return;
                }
            }
            panic!("beforeunload dialog never opened (accept)");
        });
        assert!(h.run_before_unload("T").await.unwrap());
        assert_eq!(eval(&h, "T", "window.__bu").await, json!(3));
        h.close_page("T").unwrap();
    })
    .unwrap();
}
