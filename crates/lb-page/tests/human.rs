//! `LightBrowser.human*` through `CdpHost` on real pages: trusted events in
//! Chrome's order, monotonic timestamps, real-clock timing, typed values,
//! scrolling into view, errors, and determinism per persona seed.
#![cfg(feature = "chrome")]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use common::{Resource, serve};
use lb_cdp::types::{HumanClick, HumanCommand, HumanMove, HumanScroll, HumanType};
use lb_cdp::{ContextOptions, EvaluateRequest, NavigateRequest, PageHost};
use lb_page::{CdpHost, CdpHostOptions};
use lb_persona::PersonaProfile;
use serde_json::{Value, json};

fn host(persona: &str, virtual_time: bool) -> CdpHost {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    let p = PersonaProfile::from_path_checked(root.join("reference/chrome").join(persona))
        .expect("reference persona");
    let mut opts = CdpHostOptions::new(Arc::new(p), root.join("tls"));
    opts.workers = 1;
    opts.virtual_time = virtual_time;
    CdpHost::new(opts).expect("host")
}

const WIN: &str = "chrome153_win10_rtx3060_1080p.json";
const MAC: &str = "chrome153_macos15_apple_m2_retina.json";

async fn eval(h: &CdpHost, t: &str, expr: &str) -> Value {
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
    r.result.value.unwrap_or(Value::Null)
}

const PAGE: &str = "<!DOCTYPE html><title>Human</title><body style='margin:0'>\
    <input id=i style='position:absolute;left:300px;top:200px;width:200px;height:24px'>\
    <button id=b style='position:absolute;left:100px;top:1600px;width:120px;height:40px'>far</button>\
    <button id=h style='display:none'>hidden</button>\
    <div style='height:3000px'></div>\
    <script>window.log = [];\
    for (const t of ['mousemove','mousedown','mouseup','click','keydown','keyup','input','wheel'])\
      addEventListener(t, e => log.push({t: e.type, tr: e.isTrusted, ts: e.timeStamp,\
        x: e.clientX, y: e.clientY, k: e.key, s: e.shiftKey, dy: e.deltaY,\
        tg: e.target.id || e.target.nodeName}), true);\
    </script></body>";

async fn open(h: &CdpHost, url: &str) {
    h.create_context("C", ContextOptions::default()).unwrap();
    h.create_page("C", "T").await.unwrap();
    h.navigate(
        "T",
        NavigateRequest {
            url: url.into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

async fn log(h: &CdpHost) -> Vec<Value> {
    eval(h, "T", "log").await.as_array().cloned().unwrap()
}

fn of<'a>(log: &'a [Value], t: &str) -> Vec<&'a Value> {
    log.iter().filter(|e| e["t"] == t).collect()
}

#[test]
fn human_click_type_scroll_on_the_real_clock() {
    lb_cdp::block_on_local(async {
        let s = serve(vec![("/", Resource::html(PAGE))]).await;
        let h = host(WIN, false);
        open(&h, &s.url("/")).await;

        // humanMove ends exactly at the point.
        h.human_input("T", HumanCommand::Move(HumanMove { x: 640.0, y: 300.0 }))
            .await
            .unwrap();
        let l = log(&h).await;
        let moves = of(&l, "mousemove");
        assert!(moves.len() >= 3, "{} moves", moves.len());
        let last = moves.last().unwrap();
        assert_eq!(
            (last["x"].as_f64(), last["y"].as_f64()),
            (Some(640.0), Some(300.0))
        );

        // humanType with a selector clicks the field first, then types.
        let t0 = Instant::now();
        h.human_input(
            "T",
            HumanCommand::Type(HumanType {
                text: "Hello World".into(),
                selector: Some("#i".into()),
            }),
        )
        .await
        .unwrap();
        let typing_ms = t0.elapsed().as_millis();
        assert_eq!(
            eval(&h, "T", "document.getElementById('i').value").await,
            json!("Hello World")
        );
        assert_eq!(eval(&h, "T", "document.activeElement.id").await, json!("i"));
        // 11 characters at ~55 WPM plus think time and the click: seconds,
        // not milliseconds.
        assert!(typing_ms > 1500, "typing took {typing_ms} ms");

        // humanClick on an element below the fold: wheel input first.
        let r = h
            .human_input(
                "T",
                HumanCommand::Click(HumanClick {
                    selector: Some("#b".into()),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        assert!(eval(&h, "T", "scrollY").await.as_f64().unwrap() > 500.0);
        let rect = eval(
            &h,
            "T",
            "(r => [r.left, r.top, r.right, r.bottom])(document.getElementById('b').getBoundingClientRect())",
        )
        .await;
        let (x, y) = (r["x"].as_f64().unwrap(), r["y"].as_f64().unwrap());
        let rect: Vec<f64> = rect
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        assert!(
            x >= rect[0] && x <= rect[2] && y >= rect[1] && y <= rect[3],
            "{x},{y} vs {rect:?}"
        );

        let l = log(&h).await;
        // Every event is trusted; timestamps never go back.
        assert!(l.iter().all(|e| e["tr"] == true));
        let ts: Vec<f64> = l.iter().map(|e| e["ts"].as_f64().unwrap()).collect();
        assert!(ts.windows(2).all(|w| w[1] >= w[0]), "{ts:?}");

        // Two clicks (the field, the button), each: moves, down, up, click.
        let downs = of(&l, "mousedown");
        let ups = of(&l, "mouseup");
        let clicks = of(&l, "click");
        assert_eq!((downs.len(), ups.len(), clicks.len()), (2, 2, 2));
        assert_eq!(clicks[0]["tg"], "i");
        assert_eq!(clicks[1]["tg"], "b");
        assert_eq!(
            (clicks[1]["x"].as_f64(), clicks[1]["y"].as_f64()),
            (Some(x), Some(y))
        );
        for k in 0..2 {
            let hold = ups[k]["ts"].as_f64().unwrap() - downs[k]["ts"].as_f64().unwrap();
            assert!((30.0..400.0).contains(&hold), "hold {hold}");
            let pos = |e: &Value| l.iter().position(|x| x == e).unwrap();
            let (d, u, c) = (pos(downs[k]), pos(ups[k]), pos(clicks[k]));
            assert!(d < u && u < c);
            let before = l[..d].iter().filter(|e| e["t"] == "mousemove").count();
            assert!(before >= 3);
        }

        // Typing: Shift around capitals, key events before each input event.
        let keys = of(&l, "keydown");
        let shift = keys.iter().filter(|k| k["k"] == "Shift").count();
        assert!(shift >= 2, "{shift} Shift presses");
        let cap = keys.iter().find(|k| k["k"] == "H").unwrap();
        assert_eq!(cap["s"], true);
        let small = keys.iter().find(|k| k["k"] == "e").unwrap();
        assert_eq!(small["s"], false);
        for (i, e) in l.iter().enumerate() {
            if e["t"] == "input" {
                assert_eq!(l[i - 1]["t"], "keydown", "input without its keydown");
            }
        }
        assert_eq!(of(&l, "keyup").len(), keys.len());
        // Wheel events came before the second press and scrolled the page.
        let wheels = of(&l, "wheel");
        assert!(!wheels.is_empty());
        assert!(
            wheels
                .iter()
                .all(|w| w["dy"].as_f64().unwrap().abs() == 100.0)
        );

        // humanScroll by delta.
        let y0 = eval(&h, "T", "scrollY").await.as_f64().unwrap();
        h.human_input(
            "T",
            HumanCommand::Scroll(HumanScroll {
                delta_y: -300.0,
                ..Default::default()
            }),
        )
        .await
        .unwrap();
        let y1 = eval(&h, "T", "scrollY").await.as_f64().unwrap();
        assert!(y1 < y0, "{y0} -> {y1}");

        // Errors.
        let e = h
            .human_input(
                "T",
                HumanCommand::Click(HumanClick {
                    selector: Some("#nope".into()),
                    ..Default::default()
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(e.to_string(), "No node found for selector: #nope");
        let e = h
            .human_input(
                "T",
                HumanCommand::Click(HumanClick {
                    selector: Some("#h".into()),
                    ..Default::default()
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(e.to_string(), "Node is not visible");
    })
    .unwrap();
}

/// The input of one session under virtual time: (type, x, y, key).
async fn session(persona: &str) -> (Vec<(String, f64, f64, String)>, f64) {
    let s = serve(vec![("/", Resource::html(PAGE))]).await;
    let h = host(persona, true);
    open(&h, &s.url("/")).await;
    let t0 = Instant::now();
    h.human_input(
        "T",
        HumanCommand::Type(HumanType {
            text: "Quick test".into(),
            selector: Some("#i".into()),
        }),
    )
    .await
    .unwrap();
    h.human_input(
        "T",
        HumanCommand::Scroll(HumanScroll {
            delta_y: 700.0,
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    let real = t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(
        eval(&h, "T", "document.getElementById('i').value").await,
        json!("Quick test")
    );
    let l = log(&h).await;
    let ts: Vec<f64> = l.iter().map(|e| e["ts"].as_f64().unwrap()).collect();
    assert!(ts.windows(2).all(|w| w[1] >= w[0]));
    // The virtual clock moved by the length of the stream (seconds) while
    // the call took far less real time.
    let span = ts.last().unwrap() - ts.first().unwrap();
    assert!(span > 1000.0, "virtual span {span}");
    assert!(real < span, "real {real} ms vs virtual {span} ms");
    let out = l
        .iter()
        .map(|e| {
            (
                e["t"].as_str().unwrap().to_string(),
                e["x"].as_f64().unwrap_or(0.0),
                e["y"].as_f64().unwrap_or(0.0) + e["dy"].as_f64().unwrap_or(0.0),
                e["k"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    (out, span)
}

#[test]
fn same_persona_same_stream_virtual_time() {
    lb_cdp::block_on_local(async {
        let (a, sa) = session(WIN).await;
        let (b, sb) = session(WIN).await;
        assert_eq!(a, b);
        assert!((sa - sb).abs() < 1.0, "{sa} vs {sb}");
        // Another persona (another seed, trackpad scrolling) differs.
        let (c, _) = session(MAC).await;
        assert_ne!(a, c);
        let wheel_dy: Vec<f64> = c.iter().filter(|e| e.0 == "wheel").map(|e| e.2).collect();
        assert!(wheel_dy.len() > 5);
    })
    .unwrap();
}
