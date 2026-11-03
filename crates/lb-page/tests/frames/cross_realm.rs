//! Cross-realm DOM calls, node adoption across frames and the legacy
//! factory functions, compared with Chrome 153 (Chrome for Testing
//! 153.0.8010.12 on the same pages, loopback). Included from
//! `frames/mod.rs`.

use lb_page::WaitUntil;

use super::super::common::{Resource, Server, browser, context, open, serve};
use super::{eval, s};

const CHILD: &str =
    "<!doctype html><html><head><title>child</title></head><body><p id=c>child</p></body></html>";

/// A page with three same-origin child frames (`f1`, `f3`, `f4`).
async fn frames_page() -> (Server, lb_page::Page, lb_page::Browser) {
    let srv = serve(vec![
        (
            "/",
            Resource::html(
                "<!doctype html><html><head><title>probe</title></head><body><pre id=o></pre>\
                 <iframe id=f1 src=/child.html></iframe><iframe id=f3 src=/child.html></iframe>\
                 <iframe id=f4 src=/child.html></iframe></body></html>",
            ),
        ),
        ("/child.html", Resource::html(CHILD)),
    ])
    .await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &srv.url("/"), WaitUntil::Load).await;
    (srv, page, b)
}

#[tokio::test]
async fn cross_realm_node_calls_match_chrome() {
    let (_srv, page, _b) = frames_page().await;
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
    let (_srv, page, _b) = frames_page().await;
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
    let (_srv, page, _b) = frames_page().await;
    let got = s(eval(&page, include_str!("factories_probe.js")).await);
    let want = r#"c1 = Image:true|0|Image|function Image() { [native code] }|true|false|{"w":false,"e":false,"c":false}|length;name;prototype|throw:TypeError:Failed to construct 'Image': Please use the 'new' operator, this DOM object constructor cannot be called as a function.|true|true|true|true|ok:true|ok:"true/true/true"|{"writable":true,"enumerable":false,"configurable":true}|throw:TypeError:Failed to construct 'Image': Please use the 'new' operator, this DOM object constructor cannot be called as a function.|ok:"<img width=\"1\" height=\"2\">"
   Audio:true|0|Audio|function Audio() { [native code] }|true|false|{"w":false,"e":false,"c":false}|length;name;prototype|throw:TypeError:Failed to construct 'Audio': Please use the 'new' operator, this DOM object constructor cannot be called as a function.|true|true|true|true|ok:true|ok:"true/true/true"|{"writable":true,"enumerable":false,"configurable":true}|throw:TypeError:Failed to construct 'Audio': Please use the 'new' operator, this DOM object constructor cannot be called as a function.|ok:"<audio preload=\"auto\" src=\"1\"></audio>"
   Option:true|0|Option|function Option() { [native code] }|true|false|{"w":false,"e":false,"c":false}|length;name;prototype|throw:TypeError:Failed to construct 'Option': Please use the 'new' operator, this DOM object constructor cannot be called as a function.|true|true|true|true|ok:true|ok:"true/true/true"|{"writable":true,"enumerable":false,"configurable":true}|throw:TypeError:Failed to construct 'Option': Please use the 'new' operator, this DOM object constructor cannot be called as a function.|ok:"<option value=\"2\" selected=\"\">1</option>"
c2 = 5,7,<img width="5">,<option value="v" selected="">t</option>,<audio preload="auto" src="a.mp3"></audio>,false,true
c3 frame Image = true,true,true
c4 keys = Option,Image,Audio,-413"#;
    assert_eq!(got, want);
}

#[tokio::test]
async fn removed_frame_window_matches_chrome() {
    let (_srv, page, _b) = frames_page().await;
    let got = s(eval(&page, include_str!("detached_probe.js")).await);
    let want = r#"Blob ok:function
URL ok:U
HTMLElement ok:U
Node ok:U
Image ok:U
Option ok:U
Audio ok:U
webkitURL ok:U
DOMException ok:U
TextEncoder ok:U
XMLHttpRequest ok:U
Event ok:U
Object ok:function
Array ok:function
Function ok:function
eval ok:function
Math ok:object
JSON ok:object
Promise ok:function
WebAssembly ok:object
Intl ok:object
console ok:object
Atomics ok:object
escape ok:function
Iterator ok:function
SharedArrayBuffer ok:U
navigator ok:object
document ok:object
location ok:object
parent ok:N
top ok:N
opener ok:N
frameElement ok:N
closed ok:true
length ok:0
window ok:object
self ok:object
frames ok:object
globalThis ok:object
name ok:""
history ok:object
screen ok:object
setTimeout ok:function
alert ok:function
fetch ok:function
performance ok:object
chrome ok:object
localStorage ok:N
sessionStorage ok:N
innerWidth ok:0
outerWidth ok:0
devicePixelRatio ok:0
origin ok:"ORIGIN"
isSecureContext ok:true
onload ok:N
event ok:U
scrollX ok:0
screenX ok:0
status ok:""
clientInformation ok:object
crossOriginIsolated ok:false
pre-touched true ok:1
own true true {"writable":true,"enumerable":false,"configurable":true}
doc true ok:"BODY" ok:"P" ok:2 ok:1 ok:"undefined" ok:"complete" ok:N ok:"hidden" ok:true ok:false
nav.userAgent ok:""
nav.appVersion ok:""
nav.vendor ok:"Google Inc."
nav.cookieEnabled ok:false
nav.onLine ok:true
nav.product ok:"Gecko"
nav.appName ok:"Netscape"
nav.maxTouchPoints ok:0
nav.same true
scr.width ok:0
scr.height ok:0
scr.availWidth ok:0
scr.availHeight ok:0
scr.availLeft ok:0
scr.availTop ok:0
scr.colorDepth ok:24
scr.pixelDepth ok:24
scr.orientation ok:N
scr.isExtended ok:false
hist.length throw:SecurityError:Failed to read the 'length' property from 'History': May not use a History object associated with a Document that is not fully active
hist.state throw:SecurityError:Failed to read the 'state' property from 'History': May not use a History object associated with a Document that is not fully active
hist.scrollRestoration throw:SecurityError:Failed to read the 'scrollRestoration' property from 'History': May not use a History object associated with a Document that is not fully active
loc ok:"ORIGIN/child.html" true
cs ok:"0|string|||"
mm ok:"false|(min-width: 1px)"
timers ok:0 ok:0 ok:0 ok:0 ok:U
geometry ok:0 ok:0 ok:0 ok:0 ok:0 ok:0
misc ok:"a" ok:"YQ==" ok:U ok:"ORIGIN" ok:true
touched function function undefined undefined"#;
    let (got, want): (Vec<&str>, Vec<&str>) = (got.lines().collect(), want.lines().collect());
    for (i, w) in want.iter().enumerate() {
        assert_eq!(got.get(i).copied(), Some(*w), "line {i}");
    }
    assert_eq!(got.len(), want.len(), "{got:?}");
    // The page keeps working, and a later frame is live again.
    let r = s(eval(
        &page,
        "(() => { const f = document.createElement('iframe'); document.body.appendChild(f); \
         return [typeof f.contentWindow.URL, f.contentWindow.closed, f.contentWindow.parent === window].join(); })()",
    )
    .await);
    assert_eq!(r, "function,false,true");
}
