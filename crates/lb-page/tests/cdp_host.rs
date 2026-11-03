//! `CdpHost` driven through `lb_cdp::PageHost` directly (no WebSocket):
//! navigation, Runtime, DOM, Input and cookies on real pages.
#![cfg(feature = "chrome")]

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{Resource, serve};
use lb_cdp::types::{CallArgument, CookieParam, KeyEvent, MouseEvent, NodeRef};
use lb_cdp::{
    CallFunctionRequest, CallTarget, ContextOptions, EvaluateRequest, NavigateRequest, PageHost,
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
        let doc = h.get_document("T", -1).unwrap();
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
