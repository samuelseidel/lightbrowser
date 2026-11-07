//! DOM ranges (`Range`, `StaticRange`, `Document.createRange`): boundary
//! points, comparisons, live updates, the content algorithms and
//! geometry, checked in a real page. Expected values and error texts were
//! recorded in Chrome 156 on the same markup.
#![cfg(feature = "chrome")]

mod common;

use common::{Resource, browser, context, open, serve};
use lb_js::JsValue;
use lb_page::WaitUntil;

/// Runs `script` (which fills `fails`) on a sample page and returns the
/// failures.
async fn run(script: &str) -> Vec<String> {
    let html = format!(
        "<!DOCTYPE html><html><head></head><body><p id=p>Hello <b id=b>bold</b> world</p>\
         <div id=w style=\"width:60px\">one two three four</div><div id=e></div>\
         <script>var fails = [];\
         function eq(a, b, m) {{ if (a !== b) fails.push(m + ': ' + a + ' !== ' + b); }}\
         function bp(r) {{ return [r.startContainer.nodeName, r.startOffset, r.endContainer.nodeName, r.endOffset].join(); }}\
         function err(f, m) {{ try {{ f(); return 'no throw'; }} catch (e) {{ return e.name + ': ' + e.message; }} }}\
         var p = document.getElementById('p'), b = document.getElementById('b'), txt = p.firstChild;\
         try {{ {script} }} catch (e) {{ fails.push('threw: ' + e); }}\
         document.body.setAttribute('data-fails', JSON.stringify(fails));</script></body></html>"
    );
    let s = serve(vec![("/", Resource::html(&html))]).await;
    let (br, ctx) = (browser(), context(false));
    let page = open(&br, &ctx, &s.url("/"), WaitUntil::Load).await;
    match page
        .evaluate("document.body.getAttribute('data-fails')")
        .await
        .unwrap()
    {
        JsValue::String(s) => serde_json::from_str(&s).unwrap(),
        v => panic!("script did not finish: {v:?}"),
    }
}

#[tokio::test]
async fn boundary_points_and_errors() {
    let fails = run(r#"
        var r = document.createRange();
        eq(Object.prototype.toString.call(r), '[object Range]', 'type');
        eq(String(r), '', 'empty toString');
        eq(bp(r) + ',' + r.collapsed + ',' + r.commonAncestorContainer.nodeName, '#document,0,#document,0,true,#document', 'initial');
        var n = new Range(1, 2);
        eq(n.startContainer, document, 'new Range() is in the document');
        eq(Range.START_TO_START + ',' + Range.END_TO_START, '0,3', 'constants');
        eq(Range.length + ' ' + StaticRange.length + ' ' + Range.prototype.setStart.length, '0 1 2', 'lengths');
        var F = "Failed to execute '";
        eq(err(function () { r.setStart(txt, 99); }), "IndexSizeError: " + F + "setStart' on 'Range': The offset 99 is larger than the node's length (6).", 'setStart text');
        eq(err(function () { r.setEnd(p, 9); }), "IndexSizeError: " + F + "setEnd' on 'Range': There is no child at offset 9.", 'setEnd element');
        eq(err(function () { r.setStart(p, -1); }), "IndexSizeError: " + F + "setStart' on 'Range': The offset 4294967295 is invalid.", 'setStart -1');
        eq(err(function () { r.setStart(p, 2147483647); }), "IndexSizeError: " + F + "setStart' on 'Range': There is no child at offset 2147483647.", 'setStart int max');
        eq(err(function () { r.setStart(document.doctype, 0); }), "InvalidNodeTypeError: " + F + "setStart' on 'Range': The node provided is of type 'html'.", 'setStart doctype');
        eq(err(function () { r.setStartBefore(document); }), "InvalidNodeTypeError: " + F + "setStartBefore' on 'Range': the given Node has no parent.", 'setStartBefore document');
        eq(err(function () { r.setStartAfter(document.createElement('a')); }), "InvalidNodeTypeError: " + F + "setStartAfter' on 'Range': the given Node has no parent.", 'setStartAfter detached');
        eq(err(function () { r.selectNode(document); }), "InvalidNodeTypeError: " + F + "selectNode' on 'Range': the given Node has no parent.", 'selectNode document');
        eq(err(function () { r.selectNodeContents(document.doctype); }), "InvalidNodeTypeError: " + F + "selectNodeContents' on 'Range': The node provided is of type 'html'.", 'selectNodeContents doctype');
        eq(err(function () { r.setStart({}, 0); }), "TypeError: " + F + "setStart' on 'Range': parameter 1 is not of type 'Node'.", 'setStart non-node');
        eq(err(function () { r.setStart(p); }), "TypeError: " + F + "setStart' on 'Range': 2 arguments required, but only 1 present.", 'setStart one arg');
        eq(err(function () { Range.prototype.detach.call({}); }), 'TypeError: Illegal invocation', 'illegal this');
        eq(err(function () { new AbstractRange(); }), "TypeError: Failed to construct 'AbstractRange': Illegal constructor", 'AbstractRange ctor');
        r.setEndBefore(document.documentElement);
        eq(bp(r), '#document,0,#document,1', 'setEndBefore html');
        r.setStartBefore(document.doctype);
        eq(bp(r), '#document,0,#document,1', 'setStartBefore doctype');
        var q = document.createRange();
        q.setStart(txt, 3); q.setEnd(txt, 1);
        eq(bp(q), '#text,1,#text,1', 'setEnd before start collapses');
        q.setStart(txt, 3); q.setEnd(txt, 4); q.setStart(document.createElement('a'), 0);
        eq(bp(q), 'A,0,A,0', 'setStart in another tree collapses');
        q.setStart(txt, 1); q.setEnd(txt, 3); q.collapse();
        eq(bp(q), '#text,3,#text,3', 'collapse()');
        q.setEnd(txt, 3); q.setStart(txt, 1); q.collapse(true);
        eq(bp(q), '#text,1,#text,1', 'collapse(true)');
        q.selectNode(txt);
        eq(bp(q), 'P,0,P,1', 'selectNode text');
        q.selectNodeContents(txt);
        eq(bp(q), '#text,0,#text,6', 'selectNodeContents text');
        q.setStart(txt, 1); q.setEnd(b.lastChild, 1);
        eq(q.commonAncestorContainer, p, 'commonAncestorContainer');
        var c = q.cloneRange();
        eq(bp(c) + ' ' + (c !== q), '#text,1,#text,1 true', 'cloneRange');
        q.detach();
        eq(q.collapsed, false, 'detach is a no-op');
        var s = new StaticRange({startContainer: txt, startOffset: 99, endContainer: p, endOffset: 0});
        eq([s.startOffset, s.collapsed, String(s)].join(), '99,false,[object StaticRange]', 'StaticRange');
        s = new StaticRange({startContainer: p, startOffset: -1, endContainer: txt, endOffset: '3'});
        eq([s.startContainer.id, s.startOffset, s.endContainer.nodeName, s.endOffset].join(), 'p,4294967295,#text,3', 'StaticRange offsets');
        eq(Object.getOwnPropertyNames(StaticRange.prototype).join(), 'constructor', 'StaticRange prototype');
        eq(err(function () { new StaticRange({startContainer: document.doctype, startOffset: 0, endContainer: p, endOffset: 0}); }), "InvalidNodeTypeError: Failed to construct 'StaticRange': Neither startContainer nor endContainer can be a DocumentType or Attribute node.", 'StaticRange doctype');
        eq(err(function () { new StaticRange({startContainer: p, startOffset: 0}); }), "TypeError: Failed to construct 'StaticRange': Failed to read the 'endContainer' property from 'StaticRangeInit': Required member is undefined.", 'StaticRange missing');
        eq(err(function () { new StaticRange({startContainer: 5, startOffset: 0, endContainer: p, endOffset: 0}); }), "TypeError: Failed to construct 'StaticRange': Failed to read the 'startContainer' property from 'StaticRangeInit': Failed to convert value to 'Node'.", 'StaticRange bad node');
        eq(err(function () { new StaticRange(); }), "TypeError: Failed to construct 'StaticRange': 1 argument required, but only 0 present.", 'StaticRange no args');
        eq(err(function () { Range.prototype.cloneRange.call(s); }), 'TypeError: Illegal invocation', 'StaticRange is no Range');
    "#)
    .await;
    assert!(fails.is_empty(), "{fails:#?}");
}

#[tokio::test]
async fn comparisons() {
    let fails = run(r#"
        var F = "Failed to execute '";
        var r = document.createRange(); r.setStart(txt, 1); r.setEnd(txt, 3);
        var s = document.createRange(); s.setStart(txt, 2); s.setEnd(p, 3);
        eq([0, 1, 2, 3].map(function (h) { return r.compareBoundaryPoints(h, s); }).join(), '-1,1,-1,-1', 'compareBoundaryPoints');
        eq(r.compareBoundaryPoints(65536, r), 0, 'how is an unsigned short');
        eq(err(function () { r.compareBoundaryPoints(-1, r); }), "NotSupportedError: " + F + "compareBoundaryPoints' on 'Range': The comparison method provided must be one of 'START_TO_START', 'START_TO_END', 'END_TO_END', or 'END_TO_START'.", 'bad how');
        var o = new Range(); o.selectNodeContents(document.createElement('a'));
        eq(err(function () { r.compareBoundaryPoints(0, o); }), "WrongDocumentError: " + F + "compareBoundaryPoints' on 'Range': The source range is in a different document than this range.", 'other tree');
        eq(err(function () { r.compareBoundaryPoints(0, {}); }), "TypeError: " + F + "compareBoundaryPoints' on 'Range': parameter 2 is not of type 'Range'.", 'not a range');
        r.setStart(txt, 1); r.setEnd(b, 1);
        eq([r.comparePoint(txt, 0), r.comparePoint(txt, 1), r.comparePoint(p, 1), r.comparePoint(b.lastChild, 0), r.comparePoint(p, 3), r.comparePoint(document.body, 0)].join(), '-1,0,0,0,1,-1', 'comparePoint');
        eq(err(function () { r.comparePoint(txt, 99); }), "IndexSizeError: " + F + "comparePoint' on 'Range': The offset 99 is larger than the node's length (6).", 'comparePoint offset');
        eq(err(function () { r.comparePoint(p, 7); }), "IndexSizeError: " + F + "comparePoint' on 'Range': There is no child at offset 7.", 'comparePoint child');
        eq(err(function () { r.comparePoint(document.createElement('a'), 0); }), "WrongDocumentError: " + F + "comparePoint' on 'Range': The node provided and the Range are not in the same tree.", 'comparePoint other tree');
        eq(err(function () { r.comparePoint(document.doctype, 0); }), "InvalidNodeTypeError: " + F + "comparePoint' on 'Range': The node provided is of type 'html'.", 'comparePoint doctype');
        eq(r.isPointInRange(document.createElement('a'), 0), false, 'isPointInRange other tree');
        eq(r.isPointInRange(txt, 1) && r.isPointInRange(b, 1) && !r.isPointInRange(txt, 0), true, 'isPointInRange');
        eq(err(function () { r.isPointInRange(txt, 99); }), "IndexSizeError: " + F + "isPointInRange' on 'Range': The offset 99 is larger than the node's length (6).", 'isPointInRange offset');
        r.setStart(txt, 1); r.setEnd(txt, 2);
        eq([r.intersectsNode(txt), r.intersectsNode(b), r.intersectsNode(p), r.intersectsNode(document.documentElement), r.intersectsNode(document), r.intersectsNode(document.createElement('a'))].join(), 'true,false,true,true,true,false', 'intersectsNode');
        eq(err(function () { r.intersectsNode(null); }), "TypeError: " + F + "intersectsNode' on 'Range': parameter 1 is not of type 'Node'.", 'intersectsNode null');
        r.setStart(txt, 2); r.setEnd(p.lastChild, 3);
        eq(r.toString(), 'llo bold wo', 'toString');
        r.selectNode(p);
        eq(r.toString(), 'Hello bold world', 'toString of elements');
    "#)
    .await;
    assert!(fails.is_empty(), "{fails:#?}");
}

#[tokio::test]
async fn live_updates() {
    let fails = run(r#"
        var r = document.createRange();
        r.selectNodeContents(b); p.removeChild(b);
        eq([r.startContainer.id, r.startOffset, r.endContainer.id, r.endOffset].join(), 'p,1,p,1', 'removal of an ancestor');
        p.insertBefore(b, p.childNodes[1]);
        r.setStart(p, 1); r.setEnd(p, 3);
        p.insertBefore(document.createElement('i'), p.firstChild);
        eq(r.startOffset + ',' + r.endOffset, '2,4', 'insertion before');
        p.removeChild(p.firstChild);
        var tx = p.firstChild;
        r.setStart(tx, 4); r.setEnd(p, 2);
        var nt = tx.splitText(2);
        eq([r.startContainer === nt, r.startOffset, r.endOffset].join(), 'true,2,3', 'splitText');
        p.normalize();
        tx = p.firstChild;
        r.setStart(tx, 3); r.setEnd(tx, 5); tx.data = 'Hello ';
        eq(r.startOffset + ',' + r.endOffset, '0,0', 'data setter');
        r.setStart(tx, 1); r.setEnd(tx, 5); tx.deleteData(2, 2);
        eq(r.startOffset + ',' + r.endOffset, '1,3', 'deleteData');
        tx.data = 'Hello ';
        var t = document.createTextNode('abc');
        r.setStart(t, 1); r.setEnd(t, 3); t.appendData('de');
        eq(bp(r), '#text,1,#text,3', 'appendData');
        t = document.createTextNode('abc');
        r.setStart(t, 1); r.setEnd(t, 3); t.insertData(1, 'XY');
        eq(bp(r), '#text,1,#text,5', 'insertData');
        t = document.createTextNode('abcdef');
        r.setStart(t, 2); r.setEnd(t, 5); t.replaceData(1, 2, 'XYZW');
        eq(bp(r), '#text,1,#text,7', 'replaceData');
        var d = document.createElement('div');
        d.appendChild(document.createTextNode('ab')); d.appendChild(document.createTextNode('cd'));
        r.setStart(d.lastChild, 1); r.setEnd(d, 2); d.normalize();
        eq(bp(r) + ' ' + (r.startContainer === d.firstChild), '#text,3,DIV,1 true', 'normalize');
        d = document.createElement('div'); d.innerHTML = '<a>x</a><b>y</b><c>z</c>';
        r.setStart(d.childNodes[1].firstChild, 1); r.setEnd(d, 3); d.removeChild(d.childNodes[1]);
        eq(bp(r), 'DIV,1,DIV,2', 'removal inside');
        d = document.createElement('div'); d.innerHTML = '<a>x</a><b>y</b>';
        r.setStart(d.childNodes[1], 0); r.setEnd(d, 2); d.innerHTML = '<i></i>';
        eq(bp(r), 'DIV,0,DIV,0', 'innerHTML');
        d = document.createElement('div'); d.innerHTML = '<a>x</a><b>y</b>';
        r.setStart(d, 1); r.setEnd(d, 2); d.textContent = 'q';
        eq(bp(r), 'DIV,0,DIV,0', 'textContent');
        d = document.createElement('div'); d.innerHTML = '<a></a>';
        r.setStart(d, 1); r.setEnd(d, 1); d.appendChild(document.createElement('b'));
        eq(bp(r), 'DIV,1,DIV,1', 'append at the boundary');
        d = document.createElement('div'); d.innerHTML = '<a></a><b></b>';
        var s = new StaticRange({startContainer: d, startOffset: 2, endContainer: d, endOffset: 2});
        d.removeChild(d.firstChild);
        eq(s.startOffset + ' ' + s.collapsed, '2 true', 'StaticRange is not live');
    "#)
    .await;
    assert!(fails.is_empty(), "{fails:#?}");
}

#[tokio::test]
async fn content_algorithms() {
    let fails = run(r#"
        var F = "Failed to execute '";
        function html(f) { var x = document.createElement('div'); x.appendChild(f); return x.innerHTML; }
        var r = document.createRange();
        r.setStart(txt, 2); r.setEnd(p.lastChild, 3);
        eq(html(r.cloneContents()), 'llo <b id="b">bold</b> wo', 'cloneContents');
        var d = document.createElement('div'); d.innerHTML = 'abc<span>def</span>ghi';
        r.setStart(d.firstChild, 1); r.setEnd(d.lastChild, 1);
        eq(html(r.extractContents()) + ' | ' + d.innerHTML + ' | ' + bp(r), 'bc<span>def</span>g | ahi | DIV,1,DIV,1', 'extractContents');
        d = document.createElement('div'); d.innerHTML = '<p>ab<b>cd</b></p><p>ef</p><p>g<i>hi</i></p>';
        r.setStart(d.firstChild.lastChild.firstChild, 1); r.setEnd(d.lastChild.lastChild.firstChild, 1);
        eq(html(r.extractContents()) + ' | ' + d.innerHTML + ' ' + bp(r), '<p><b>d</b></p><p>ef</p><p>g<i>h</i></p> | <p>ab<b>c</b></p><p><i>i</i></p> DIV,1,DIV,1', 'extract partial elements');
        d = document.createElement('div'); d.innerHTML = 'ab<i>cd<u>e</u></i>f<s>g</s>h';
        r.setStart(d.firstChild, 1); r.setEnd(d.childNodes[1].firstChild, 1); r.deleteContents();
        eq(d.innerHTML + ' ' + bp(r), 'a<i>d<u>e</u></i>f<s>g</s>h DIV,1,DIV,1', 'deleteContents');
        r.setStart(d.childNodes[1], 0); r.setEnd(d.lastChild, 1); r.deleteContents();
        eq(d.innerHTML + ' ' + bp(r), 'a<i></i> DIV,2,DIV,2', 'deleteContents across');
        d = document.createElement('div'); d.innerHTML = 'a<!--hello-->b';
        r.setStart(d.childNodes[1], 1); r.setEnd(d.childNodes[1], 3);
        var f = r.cloneContents();
        eq(f.firstChild.nodeName + ' ' + f.firstChild.data + ' ' + r.toString(), '#comment el ', 'clone comment data');
        r.setStart(document, 0); r.setEnd(document, 1);
        eq(err(function () { r.extractContents(); }), "HierarchyRequestError: " + F + "extractContents' on 'Range': The Range contains a doctype node.", 'extract doctype');
        eq(err(function () { r.cloneContents(); }), "HierarchyRequestError: " + F + "cloneContents' on 'Range': Nodes of type 'html' may not be inserted inside nodes of type '#document-fragment'.", 'clone doctype');
        d = document.createElement('div'); d.textContent = 'abcdef';
        r.setStart(d.firstChild, 2); r.setEnd(d.firstChild, 4); r.insertNode(document.createElement('i'));
        eq(d.innerHTML + ' ' + bp(r) + ' ' + d.childNodes.length, 'ab<i></i>cdef #text,2,#text,2 3', 'insertNode splits text');
        d = document.createElement('div'); d.innerHTML = '<a></a><b></b>';
        r.setStart(d, 1); r.setEnd(d, 1); r.insertNode(document.createElement('i'));
        eq(d.innerHTML + ' ' + bp(r), '<a></a><i></i><b></b> DIV,1,DIV,2', 'insertNode collapsed');
        d = document.createElement('div'); d.innerHTML = '<a></a><b></b>';
        r.setStart(d, 1); r.setEnd(d, 2);
        f = document.createDocumentFragment(); f.appendChild(document.createElement('x')); f.appendChild(document.createElement('y'));
        r.insertNode(f);
        eq(d.innerHTML + ' ' + bp(r), '<a></a><x></x><y></y><b></b> DIV,1,DIV,4', 'insertNode fragment');
        r.setStart(document.createTextNode('abc'), 1);
        eq(err(function () { r.insertNode(document.createElement('i')); }), "HierarchyRequestError: " + F + "insertNode' on 'Range': This operation would split a text node, but there's no parent into which to insert.", 'insertNode detached text');
        r.setStart(document, 0);
        eq(err(function () { r.insertNode(document.createTextNode('x')); }), "HierarchyRequestError: " + F + "insertNode' on 'Range': Nodes of type '#text' may not be inserted inside nodes of type '#document'.", 'insertNode text into document');
        eq(err(function () { r.insertNode(document.createElement('x')); }), "HierarchyRequestError: " + F + "insertNode' on 'Range': Can't insert an element before a doctype.", 'insertNode element into document');
        r.setStart(b, 0);
        eq(err(function () { r.insertNode(p); }), "HierarchyRequestError: " + F + "insertNode' on 'Range': The new child element contains the parent.", 'insertNode ancestor');
        r.setStart(p, 0);
        eq(err(function () { r.insertNode(p); }), "HierarchyRequestError: " + F + "insertNode' on 'Range': Unable to insert a node into a Range starting from the node itself.", 'insertNode itself');
        var c = document.createComment('x'); p.appendChild(c); r.setStart(c, 0);
        eq(err(function () { r.insertNode(document.createElement('i')); }), "HierarchyRequestError: " + F + "insertNode' on 'Range': Nodes of type 'I' may not be inserted inside nodes of type '#comment'.", 'insertNode into comment');
        p.removeChild(c);
        d = document.createElement('div'); d.innerHTML = 'ab<i>c</i>de';
        r.setStart(d.firstChild, 1); r.setEnd(d.lastChild, 1);
        var sp = document.createElement('span'); sp.textContent = 'old';
        r.surroundContents(sp);
        eq(d.innerHTML + ' ' + bp(r), 'a<span>b<i>c</i>d</span>e DIV,1,DIV,2', 'surroundContents');
        r.setStart(txt, 2); r.setEnd(b.firstChild, 2);
        eq(err(function () { r.surroundContents(document.createElement('i')); }), "InvalidStateError: " + F + "surroundContents' on 'Range': The Range has partially selected a non-Text node.", 'surround partial');
        r.selectNode(b);
        eq(err(function () { r.surroundContents(document.createDocumentFragment()); }), "InvalidNodeTypeError: " + F + "surroundContents' on 'Range': The node provided is of type '#document-fragment'.", 'surround fragment');
        eq(err(function () { r.surroundContents(document.doctype); }), "InvalidNodeTypeError: " + F + "surroundContents' on 'Range': The node provided is of type 'html'.", 'surround doctype');
        f = r.createContextualFragment('<td>x</td><i>y</i>');
        eq(f.childNodes.length + ' ' + f.firstChild.nodeName, '2 #text', 'createContextualFragment');
        r.setStart(txt, 1);
        f = r.createContextualFragment('<li>x');
        eq(f.firstChild.nodeName, 'LI', 'createContextualFragment in text');
        f = new Range().createContextualFragment('<i>y</i><script>window.ran = 1<\/script>');
        eq(Array.prototype.map.call(f.childNodes, function (n) { return n.nodeName; }).join(), 'I,SCRIPT', 'createContextualFragment in document');
        document.getElementById('e').appendChild(f);
        eq(window.ran, 1, 'contextual fragment scripts run');
        document.getElementById('e').textContent = '';
        eq(err(function () { r.createContextualFragment(); }), "TypeError: " + F + "createContextualFragment' on 'Range': 1 argument required, but only 0 present.", 'createContextualFragment no args');
    "#)
    .await;
    assert!(fails.is_empty(), "{fails:#?}");
}

#[tokio::test]
async fn geometry() {
    let fails = run(r#"
        function R(x) { return [x.x, x.y, x.width, x.height].join(); }
        function list(r) { return Array.prototype.map.call(r.getClientRects(), R); }
        var r = document.createRange();
        r.selectNode(b);
        var l = r.getClientRects();
        eq(Object.prototype.toString.call(l) + ' ' + Object.prototype.toString.call(r.getBoundingClientRect()), '[object DOMRectList] [object DOMRect]', 'types');
        // Chrome: the element's border box, then its text.
        eq(l.length, 2, 'selectNode(b): element and text');
        eq(R(l[0]), R(b.getBoundingClientRect()), 'element rect');
        eq(R(r.getBoundingClientRect()), R(b.getBoundingClientRect()), 'bounding rect');
        r.selectNodeContents(p);
        l = list(r);
        // text, b, b's text, text.
        eq(l.length, 4, 'selectNodeContents(p)');
        eq(l[1], R(b.getBoundingClientRect()), 'nested element');
        r.selectNode(p);
        eq(list(r).length, 4, 'selectNode(p): p, then the texts');
        eq(list(r)[0], R(p.getBoundingClientRect()), 'p rect first');
        r.setStart(txt, 1); r.setEnd(txt, 4);
        var whole = document.createRange(); whole.selectNodeContents(txt);
        var a = r.getBoundingClientRect(), w = whole.getBoundingClientRect();
        eq(r.getClientRects().length, 1, 'partial text: one rect');
        eq(a.x > w.x && a.right < w.right && a.width > 0 && a.y === w.y && a.height === w.height, true, 'partial text inside the whole: ' + R(a) + ' / ' + R(w));
        r.setStart(txt, 1); r.setEnd(txt, 1);
        a = r.getBoundingClientRect();
        eq(r.getClientRects().length + ' ' + a.width + ' ' + (a.height > 0), '1 0 true', 'collapsed in text');
        eq(R(new Range().getBoundingClientRect()) + ' ' + new Range().getClientRects().length, '0,0,0,0 0', 'collapsed at document');
        r.selectNodeContents(document.createElement('div'));
        eq(R(r.getBoundingClientRect()) + ' ' + r.getClientRects().length, '0,0,0,0 0', 'detached');
        var e = document.getElementById('e');
        r.selectNode(e);
        eq(list(r).join(';'), R(e.getBoundingClientRect()), 'empty div');
        r.selectNodeContents(e);
        eq(r.getClientRects().length, 0, 'empty div contents');
        var wr = document.getElementById('w');
        r.selectNodeContents(wr);
        l = r.getClientRects();
        eq(l.length > 1, true, 'wrapped text: one rect per line (' + l.length + ')');
        eq(l.length > 1 && l[1].y > l[0].y && l[1].x === l[0].x, true, 'lines stack');
        var bb = r.getBoundingClientRect(), er = wr.getBoundingClientRect();
        eq(bb.x === er.x && bb.y === er.y && bb.width <= er.width && bb.height === er.height, true, 'lines fill the box: ' + R(bb) + ' / ' + R(er));
    "#)
    .await;
    assert!(fails.is_empty(), "{fails:#?}");
}
