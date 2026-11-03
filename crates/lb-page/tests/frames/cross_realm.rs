//! Cross-realm DOM calls, node adoption across frames and the legacy
//! factory functions, compared with Chrome 153 (Chrome for Testing
//! 153.0.8010.12 on the same pages, loopback). Included from
//! `frames/mod.rs`.

use lb_page::WaitUntil;

use super::super::common::{Resource, Server, browser, context, open, serve};
use super::{eval, s};

const CHILD: &str =
    "<!doctype html><html><head><title>child</title></head><body><p id=c>child</p></body></html>";

/// A page with two same-origin child frames (`f1`, `f3`).
async fn two_frames() -> (Server, lb_page::Page, lb_page::Browser) {
    let srv = serve(vec![
        (
            "/",
            Resource::html(
                "<!doctype html><html><head><title>probe</title></head><body><pre id=o></pre>\
                 <iframe id=f1 src=/child></iframe><iframe id=f3 src=/child></iframe></body></html>",
            ),
        ),
        ("/child", Resource::html(CHILD)),
    ])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &srv.url("/"), WaitUntil::Load).await;
    (srv, page, b)
}

#[tokio::test]
async fn cross_realm_node_calls_match_chrome() {
    let (_srv, page, _b) = two_frames().await;
    let got = s(eval(&page, include_str!("cross_realm_probe.js")).await);
    // Chrome's output, line by line.
    let want = [
        r#"a1 nodeName = ok:"BODY""#,
        r#"a2 appendChild = ok:true,true"#,
        r#"a3 main appendChild frame = ok:true,true,true"#,
        r#"a4 adopt via appendChild = true,true,true,false,true,true"#,
        r#"a5 listeners after adopt = 2"#,
        r#"a6 adoptNode = true,null,true,false"#,
        r#"a7 cloneNode xrealm = ok:true,ok:false"#,
        r#"a8 firstChild id = ok:true"#,
        r#"a9 err realm = true,false,HierarchyRequestError,Failed to execute 'appendChild' on 'Node': The new child element contains the parent."#,
        r#"a10 rect = ok:true,ok:false"#,
        r#"a11 childNodes = ok:true,ok:true"#,
        r#"a12 classList = ok:"true/zz/true""#,
        r#"a13 ownerDocument = ok:true"#,
        r#"a14 createElement xrealm = ok:"true/true/false""#,
        r#"a15 querySelector xrealm = ok:true"#,
        r#"a16 insertBefore foreign = ok:"true/true/true/true""#,
        r#"a17 replaceChildren foreign = ok:"2/true""#,
        r#"a18 importNode = ok:"true/true/true""#,
        r#"a19 append dom parser doc node = ok:"true/true/""#,
        r#"a20 frame innerHTML on top = ok:"true/true""#,
        r#"a21 frame textContent of top body = ok:"string""#,
        r#"a22 compareDocumentPosition = ok:37"#,
        r#"a23 contains = ok:false"#,
        r#"a24 isEqualNode = ok:true"#,
        r#"a25 adopt document = throw:DOMException:Failed to execute 'adoptNode' on 'Document': The node provided is of type '#document', which may not be adopted."#,
        r#"a26 adopt html = ok:"true/""#,
    ];
    let got: Vec<&str> = got.lines().collect();
    for (i, w) in want.iter().enumerate() {
        assert_eq!(got.get(i).copied(), Some(*w), "line {i}");
    }
    assert_eq!(got.len(), want.len(), "{got:?}");
}

#[tokio::test]
async fn cross_realm_fuzz_never_panics() {
    let (_srv, page, _b) = two_frames().await;
    let got = s(eval(&page, include_str!("cross_realm_fuzz.js")).await);
    let v: serde_json::Value = serde_json::from_str(&got).expect(&got);
    assert!(v["calls"].as_u64().unwrap() > 50_000, "{got}");
    assert_eq!(v["n"], 0, "host functions panicked: {got}");
    assert_eq!(v["alive"], "#document", "{got}");
    // The worker still runs scripts afterwards.
    assert_eq!(
        s(eval(&page, "typeof document.createElement('p').remove").await),
        "function"
    );
}

#[tokio::test]
async fn legacy_factory_functions_match_chrome() {
    let (_srv, page, _b) = two_frames().await;
    let got = s(eval(&page, include_str!("factories_probe.js")).await);
    let want = r#"c1 = Image:true|0|Image|function Image() { [native code] }|true|false|{"w":false,"e":false,"c":false}|length;name;prototype|throw:TypeError:Failed to construct 'Image': Please use the 'new' operator, this DOM object constructor cannot be called as a function.|true|true|true|true|ok:true|ok:"true/true/true"|{"writable":true,"enumerable":false,"configurable":true}|throw:TypeError:Failed to construct 'Image': Please use the 'new' operator, this DOM object constructor cannot be called as a function.|ok:"<img width=\"1\" height=\"2\">"
   Audio:true|0|Audio|function Audio() { [native code] }|true|false|{"w":false,"e":false,"c":false}|length;name;prototype|throw:TypeError:Failed to construct 'Audio': Please use the 'new' operator, this DOM object constructor cannot be called as a function.|true|true|true|true|ok:true|ok:"true/true/true"|{"writable":true,"enumerable":false,"configurable":true}|throw:TypeError:Failed to construct 'Audio': Please use the 'new' operator, this DOM object constructor cannot be called as a function.|ok:"<audio preload=\"auto\" src=\"1\"></audio>"
   Option:true|0|Option|function Option() { [native code] }|true|false|{"w":false,"e":false,"c":false}|length;name;prototype|throw:TypeError:Failed to construct 'Option': Please use the 'new' operator, this DOM object constructor cannot be called as a function.|true|true|true|true|ok:true|ok:"true/true/true"|{"writable":true,"enumerable":false,"configurable":true}|throw:TypeError:Failed to construct 'Option': Please use the 'new' operator, this DOM object constructor cannot be called as a function.|ok:"<option value=\"2\" selected=\"\">1</option>"
c2 = 5,7,<img width="5">,<option value="v" selected="">t</option>,<audio preload="auto" src="a.mp3"></audio>,false,true
c3 frame Image = true,true,true
c4 keys = Option,Image,Audio,-413"#;
    assert_eq!(got, want);
}
