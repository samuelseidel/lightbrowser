//! `Selection` (`getSelection()`): state, the methods that change it,
//! error texts, `containsNode`, `toString` and `selectionchange`, checked
//! in a real page. Expected values were recorded in Chrome 156 on the
//! same markup.
#![cfg(feature = "chrome")]

mod common;

use common::{Resource, browser, context, open, serve};
use lb_js::JsValue;
use lb_page::WaitUntil;

/// Runs `script` (which fills `fails`) on a sample page and returns the
/// failures.
async fn run(script: &str) -> Vec<String> {
    run_then(script, None).await.0
}

/// [`run`], then evaluates `later` after the page's tasks have run.
async fn run_then(script: &str, later: Option<&str>) -> (Vec<String>, Option<JsValue>) {
    let html = format!(
        "<!DOCTYPE html><html><head></head><body><p id=p>Hello <b id=b>bold</b> world</p>\
         <div id=w style=\"width:60px\">one two three four</div><div id=e></div><div id=h style=\"display:none\">hidden</div>\
         <div id=x><p>One   two</p><p>Three <b>four</b></p><div>five<br>six</div><span style=\"display:none\">gone</span>end</div>\
         <script>var fails = [];\
         function eq(a, b, m) {{ if (a !== b) fails.push(m + ': ' + a + ' !== ' + b); }}\
         function bp(r) {{ return [r.startContainer.nodeName, r.startOffset, r.endContainer.nodeName, r.endOffset].join(); }}\
         function st(s) {{ return [s.anchorNode && s.anchorNode.nodeName, s.anchorOffset, s.focusNode && s.focusNode.nodeName, s.focusOffset, s.isCollapsed, s.rangeCount, s.type, s.direction].join(); }}\
         function err(f, m) {{ try {{ f(); return 'no throw'; }} catch (e) {{ return e.name + ': ' + e.message; }} }}\
         var s = getSelection(), p = document.getElementById('p'), b = document.getElementById('b'), txt = p.firstChild;\
         try {{ {script} }} catch (e) {{ fails.push('threw: ' + e); }}\
         document.body.setAttribute('data-fails', JSON.stringify(fails));</script></body></html>"
    );
    let s = serve(vec![("/", Resource::html(&html))]).await;
    let (br, ctx) = (browser(), context(false));
    let page = open(&br, &ctx, &s.url("/"), WaitUntil::Load).await;
    let fails = match page
        .evaluate("document.body.getAttribute('data-fails')")
        .await
        .unwrap()
    {
        JsValue::String(s) => serde_json::from_str(&s).unwrap(),
        v => panic!("script did not finish: {v:?}"),
    };
    let mut after = None;
    if let Some(js) = later {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        after = Some(page.evaluate(js).await.unwrap());
    }
    (fails, after)
}

#[tokio::test]
async fn state_and_errors() {
    let fails = run(r#"
        var F = "Failed to execute '";
        eq([s === getSelection(), s === document.getSelection(), Object.prototype.toString.call(s), String(s)].join(), 'true,true,[object Selection],', 'one object');
        eq(st(s) + ' ' + s.baseNode + ',' + s.extentOffset, ',0,,0,true,0,None,none null,0', 'initial');
        eq(document.implementation.createHTMLDocument('x').getSelection(), null, 'no selection without a window');
        eq(err(function () { new Selection(); }), "TypeError: Failed to construct 'Selection': Illegal constructor", 'constructor');
        eq(err(function () { s.getRangeAt(0); }), "IndexSizeError: " + F + "getRangeAt' on 'Selection': 0 is not a valid index.", 'getRangeAt(0)');
        eq(err(function () { s.getRangeAt(-1); }), "IndexSizeError: " + F + "getRangeAt' on 'Selection': 4294967295 is not a valid index.", 'getRangeAt(-1)');
        s.collapse(txt, 2);
        eq(st(s), '#text,2,#text,2,true,1,Caret,none', 'collapse');
        eq(err(function () { s.collapse(txt, 99); }), "IndexSizeError: " + F + "collapse' on 'Selection': The offset 99 is larger than the node's length (6).", 'collapse offset');
        eq(err(function () { s.collapse(document.doctype, 0); }), "InvalidNodeTypeError: " + F + "collapse' on 'Selection': The node provided is of type 'html'.", 'collapse doctype');
        eq(err(function () { s.collapse(); }), "TypeError: " + F + "collapse' on 'Selection': 1 argument required, but only 0 present.", 'collapse no args');
        s.collapse(null);
        eq(st(s), ',0,,0,true,0,None,none', 'collapse(null)');
        s.collapse(txt, 1); s.collapse(document.implementation.createHTMLDocument('x').body, 0); s.collapse(document.createElement('a'), 0);
        eq(st(s), '#text,1,#text,1,true,1,Caret,none', 'nodes outside the document are ignored');
        s.setPosition(b, 1);
        eq(st(s), 'B,1,B,1,true,1,Caret,none', 'setPosition');
        s.collapse(txt, 1); s.extend(b.firstChild, 2);
        eq(st(s) + ' ' + String(s), '#text,1,#text,2,false,1,Range,forward ello bo', 'extend');
        s.collapse(b.firstChild, 2); s.extend(txt, 1);
        var r = s.getRangeAt(0);
        eq(st(s) + ' ' + bp(r), '#text,2,#text,1,false,1,Range,backward #text,1,#text,2', 'extend backward');
        s.removeAllRanges();
        eq(err(function () { s.extend(txt, 1); }), "InvalidStateError: " + F + "extend' on 'Selection': This Selection object doesn't have any Ranges.", 'extend without a range');
        s.collapse(txt, 1);
        eq(err(function () { s.extend(txt, 99); }), "IndexSizeError: " + F + "extend' on 'Selection': The offset 99 is larger than the node's length (6).", 'extend offset');
        s.extend(document.createElement('a'), 0);
        eq(st(s), '#text,1,#text,1,true,1,Caret,none', 'extend outside is ignored');
        s.extend(txt, 1);
        eq(st(s), '#text,1,#text,1,true,1,Caret,none', 'extend to the anchor');
        eq(err(function () { s.setBaseAndExtent(txt, 1, txt, 99); }), "IndexSizeError: " + F + "setBaseAndExtent' on 'Selection': The offset 99 is larger than the node's length (6).", 'setBaseAndExtent offset');
        eq(err(function () { s.setBaseAndExtent(txt, 1); }), "TypeError: " + F + "setBaseAndExtent' on 'Selection': 4 arguments required, but only 2 present.", 'setBaseAndExtent args');
        s.setBaseAndExtent(b.firstChild, 3, txt, 2);
        eq(st(s) + ' ' + String(s), '#text,3,#text,2,false,1,Range,backward llo bol', 'setBaseAndExtent');
        s.setBaseAndExtent(null, 0, txt, 2);
        eq(st(s), ',0,,0,true,0,None,none', 'setBaseAndExtent(null)');
        s.setBaseAndExtent(txt, 1, null, 0);
        eq(st(s), '#text,1,#text,1,true,1,Caret,none', 'setBaseAndExtent without extent');
        s.selectAllChildren(p);
        eq(st(s) + ' ' + String(s), 'P,0,P,3,false,1,Range,forward Hello bold world', 'selectAllChildren');
        s.selectAllChildren(document.createElement('div'));
        eq(st(s), 'P,0,P,3,false,1,Range,forward', 'selectAllChildren detached is ignored');
        eq(err(function () { s.selectAllChildren(document.doctype); }), "InvalidNodeTypeError: " + F + "selectAllChildren' on 'Selection': The node provided is of type 'html'.", 'selectAllChildren doctype');
        s.setBaseAndExtent(b.firstChild, 3, txt, 2); s.collapseToStart();
        eq(st(s), '#text,2,#text,2,true,1,Caret,none', 'collapseToStart');
        s.setBaseAndExtent(b.firstChild, 3, txt, 2); s.collapseToEnd();
        eq(st(s), '#text,3,#text,3,true,1,Caret,none', 'collapseToEnd');
        s.removeAllRanges();
        eq(err(function () { s.collapseToStart(); }), "InvalidStateError: " + F + "collapseToStart' on 'Selection': there is no selection.", 'collapseToStart without a range');
        s.empty();
        eq(err(function () { s.collapseToEnd(); }), "InvalidStateError: " + F + "collapseToEnd' on 'Selection': there is no selection.", 'collapseToEnd without a range');
        eq(err(function () { Object.getOwnPropertyDescriptor(Selection.prototype, 'type').get.call({}); }), 'TypeError: Illegal invocation', 'illegal this');
        eq([s.modify.length, s.collapse.length, s.extend.length, s.setBaseAndExtent.length, s.containsNode.length].join(), '0,1,1,4,1', 'lengths');
    "#)
    .await;
    assert!(fails.is_empty(), "{fails:#?}");
}

#[tokio::test]
async fn ranges_of_the_selection() {
    let fails = run(r#"
        var F = "Failed to execute '";
        var r = document.createRange(); r.setStart(txt, 1); r.setEnd(txt, 3);
        s.addRange(r);
        eq((s.getRangeAt(0) === r) + ' ' + st(s), 'true #text,1,#text,3,false,1,Range,none', 'addRange keeps the object');
        eq(s.getRangeAt(0) === s.getRangeAt(0), true, 'getRangeAt is stable');
        r.setEnd(b, 1);
        eq(st(s), '#text,1,B,1,false,1,Range,none', 'the range is live');
        var r2 = document.createRange(); r2.selectNode(b); s.addRange(r2);
        eq(st(s) + ' ' + (s.getRangeAt(0) === r2), '#text,1,B,1,false,1,Range,none false', 'a second range is ignored');
        eq(err(function () { s.getRangeAt(1); }), "IndexSizeError: " + F + "getRangeAt' on 'Selection': 1 is not a valid index.", 'getRangeAt(1)');
        eq(err(function () { s.removeRange(document.createRange()); }), "NotFoundError: " + F + "removeRange' on 'Selection': Range not found.", 'removeRange other');
        s.removeRange(s.getRangeAt(0));
        eq(st(s), ',0,,0,true,0,None,none', 'removeRange');
        eq(err(function () { s.addRange({}); }), "TypeError: " + F + "addRange' on 'Selection': parameter 1 is not of type 'Range'.", 'addRange non-range');
        eq(err(function () { s.addRange(null); }), "TypeError: " + F + "addRange' on 'Selection': parameter 1 is not of type 'Range'.", 'addRange null');
        var d = document.createRange(); d.selectNodeContents(document.createElement('div')); s.addRange(d);
        eq(st(s), ',0,,0,true,0,None,none', 'addRange detached is ignored');
        s.collapse(txt, 2);
        var a = s.getRangeAt(0);
        s.extend(txt, 4);
        eq((s.getRangeAt(0) === a) + ' ' + st(s), 'false #text,2,#text,4,false,1,Range,forward', 'extend makes a new range');
        s.extend(txt, 0);
        eq(st(s), '#text,2,#text,0,false,1,Range,backward', 'extend backward');
        s.getRangeAt(0).setEnd(txt, 5);
        eq(st(s), '#text,0,#text,5,false,1,Range,none', 'changing the range drops the direction');
        s.removeAllRanges(); s.addRange(r); s.addRange(r);
        eq(st(s), '#text,1,B,1,false,1,Range,none', 'addRange twice');
        var c = s.getComposedRanges();
        eq(c.length + ' ' + Object.prototype.toString.call(c[0]) + ' ' + c[0].startOffset, '1 [object StaticRange] 1', 'getComposedRanges');
        var t = document.createTextNode('abcdef'); p.appendChild(t);
        s.setBaseAndExtent(t, 1, t, 4); p.removeChild(t);
        eq(st(s), 'P,3,P,3,true,1,Caret,none', 'removal moves the selection');
    "#)
    .await;
    assert!(fails.is_empty(), "{fails:#?}");
}

#[tokio::test]
async fn contents_and_text() {
    let fails = run(r#"
        s.setBaseAndExtent(txt, 2, p.lastChild, 2);
        eq([s.containsNode(b), s.containsNode(txt), s.containsNode(txt, true), s.containsNode(p), s.containsNode(p, true), s.containsNode(document.body, true)].join(), 'true,true,true,false,true,true', 'containsNode');
        eq(err(function () { s.containsNode(null); }), "TypeError: Failed to execute 'containsNode' on 'Selection': parameter 1 is not of type 'Node'.", 'containsNode null');
        eq(s.containsNode(document.createElement('a'), true), false, 'containsNode other tree');
        var x = document.getElementById('x');
        s.selectAllChildren(document.body);
        eq([s.containsNode(p), s.containsNode(p, true), s.containsNode(b), s.containsNode(txt), s.containsNode(document, true)].join(), 'false,true,true,true,false', 'containsNode with the body selected');
        s.removeAllRanges();
        eq(s.containsNode(b, true), false, 'containsNode without a range');
        s.selectAllChildren(x);
        eq(String(s), 'One two\n\nThree four\n\nfive\nsix\nend', 'toString renders');
        eq(s.getRangeAt(0).toString(), 'One   twoThree fourfivesixgoneend', 'Range toString does not');
        var h = document.getElementById('h');
        s.setBaseAndExtent(h.firstChild, 1, h.firstChild, 3);
        eq(st(s) + ' [' + String(s) + ']', '#text,1,#text,3,false,1,Range,forward []', 'hidden text');
        var d = document.createElement('div'); d.innerHTML = 'abc<span>def</span>ghi'; document.body.appendChild(d);
        s.setBaseAndExtent(d.firstChild, 1, d.lastChild, 1); s.deleteFromDocument();
        eq(d.innerHTML + ' ' + st(s), 'ahi DIV,1,DIV,1,true,1,Range,none', 'deleteFromDocument');
        s.collapse(d.firstChild, 1); s.deleteFromDocument();
        eq(d.innerHTML + ' ' + st(s), 'ahi #text,1,#text,1,true,1,Caret,none', 'deleteFromDocument collapsed');
        s.collapse(txt, 1); s.modify('extend', 'forward', 'character');
        eq(st(s), '#text,1,#text,2,false,1,Range,forward', 'modify extend forward character');
        s.collapse(txt, 1); s.modify('extend', 'backward', 'character');
        eq(st(s), '#text,1,#text,0,false,1,Range,backward', 'modify extend backward character');
        s.collapse(txt, 0); s.modify('move', 'forward', 'word');
        eq(st(s), '#text,5,#text,5,true,1,Caret,none', 'modify move forward word');
        s.collapse(txt, 1); s.modify('move', 'left', 'character');
        eq(st(s), '#text,0,#text,0,true,1,Caret,none', 'modify move left');
        s.collapse(txt, 1); s.modify('x', 'y', 'z'); s.modify();
        eq(st(s), '#text,1,#text,1,true,1,Caret,none', 'modify with bad arguments');
        s.removeAllRanges(); s.modify('move', 'forward', 'character');
        eq(st(s), ',0,,0,true,0,None,none', 'modify without a range');
    "#)
    .await;
    assert!(fails.is_empty(), "{fails:#?}");
}

#[tokio::test]
async fn selectionchange_fires_once_per_task() {
    let fails = run_then(r#"
        window.events = [];
        document.addEventListener('selectionchange', function (e) { events.push([e.bubbles, e.cancelable, e.target === document, e.constructor.name].join()); });
        s.collapse(txt, 1); s.extend(txt, 3); s.removeAllRanges();
        eq(events.length, 0, 'not synchronous');
    "#, Some("events.join(';')"))
    .await;
    let (fails, events) = fails;
    assert!(fails.is_empty(), "{fails:#?}");
    // Chrome: one plain `Event` for the three changes, not bubbling.
    assert_eq!(
        events,
        Some(JsValue::String("false,false,true,Event".into()))
    );
}
