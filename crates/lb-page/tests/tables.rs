//! Table DOM APIs (`HTMLTableElement`, `HTMLTableSectionElement`,
//! `HTMLTableRowElement`): live collections and the insert/delete
//! algorithms of the HTML spec, checked in a real page.
#![cfg(feature = "chrome")]

mod common;

use common::{Resource, browser, context, open, serve};
use lb_js::JsValue;
use lb_page::WaitUntil;

/// Runs `script` (which fills `fails`) on a page with a sample table and
/// returns the failures.
async fn run(script: &str) -> Vec<String> {
    let html = format!(
        "<!DOCTYPE html><body><table id=t><caption>c</caption><colgroup></colgroup>\
         <tfoot><tr id=f1><td>f</td></tr></tfoot>\
         <tbody id=b1><tr id=r1><td>1</td><th>1h</th></tr><tr id=r2><td>2</td></tr></tbody>\
         <thead><tr id=h1><th>h</th></tr></thead>\
         <tbody id=b2><tr id=r3><td>3</td></tr></tbody></table>\
         <script>var fails = [];\
         function eq(a, b, m) {{ if (a !== b) fails.push(m + ': ' + a + ' !== ' + b); }}\
         function ids(c) {{ return Array.prototype.map.call(c, function (e) {{ return e.id || e.localName; }}).join(','); }}\
         function throws(f, name, m) {{ try {{ f(); fails.push(m + ': no throw'); }} catch (e) {{ eq(e.name, name, m); }} }}\
         var t = document.getElementById('t');\
         {script}\
         document.body.setAttribute('data-fails', JSON.stringify(fails));</script></body>"
    );
    let s = serve(vec![("/", Resource::html(&html))]).await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
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
async fn table_collections_are_live_and_ordered() {
    let fails = run(r#"
        eq(String(t.rows), '[object HTMLCollection]', 'rows type');
        eq(ids(t.rows), 'h1,r1,r2,r3,f1', 'rows order: thead, bodies, tfoot');
        eq(t.rows, t.rows, 'rows is cached');
        eq(ids(t.tBodies), 'b1,b2', 'tBodies');
        eq(t.caption.textContent, 'c', 'caption');
        eq(t.tHead.id, '', 'tHead');
        eq(t.tHead.localName, 'thead', 'tHead name');
        eq(t.tFoot.localName, 'tfoot', 'tFoot');
        var rows = t.rows, bodies = t.tBodies;
        var tr = document.createElement('tr'); tr.id = 'x';
        t.appendChild(tr);
        eq(ids(rows), 'h1,r1,r2,r3,x,f1', 'live after appending a direct tr');
        t.appendChild(document.createElement('tbody'));
        eq(bodies.length, 3, 'tBodies live');
        eq(rows.namedItem('r2').id, 'r2', 'namedItem');
        var b1 = document.getElementById('b1');
        eq(String(b1.rows), '[object HTMLCollection]', 'section rows type');
        eq(ids(b1.rows), 'r1,r2', 'section rows');
        var r1 = document.getElementById('r1');
        eq(ids(r1.cells), 'td,th', 'cells');
        eq(r1.rowIndex, 1, 'rowIndex');
        eq(r1.sectionRowIndex, 0, 'sectionRowIndex');
        eq(document.getElementById('f1').rowIndex, 5, 'tfoot rowIndex');
        eq(document.getElementById('x').sectionRowIndex, 4, 'direct tr sectionRowIndex');
        var lone = document.createElement('tr');
        eq(lone.rowIndex, -1, 'detached rowIndex');
        eq(lone.sectionRowIndex, -1, 'detached sectionRowIndex');
        eq(typeof HTMLTableElement.prototype.insertRow, 'function', 'insertRow exists');
        t.cellPadding = '4'; eq(t.getAttribute('cellpadding'), '4', 'cellPadding reflects');
        r1.vAlign = 'top'; eq(r1.getAttribute('valign'), 'top', 'vAlign reflects');
        eq(b1.chOff, '', 'chOff default');
        eq(Object.getOwnPropertyDescriptor(HTMLTableElement.prototype, 'rows').get.call(t) === t.rows, true, 'getter');
        throws(function () { Object.getOwnPropertyDescriptor(HTMLTableElement.prototype, 'rows').get.call(document.body); }, 'TypeError', 'illegal this');
    "#)
    .await;
    assert!(fails.is_empty(), "{fails:#?}");
}

#[tokio::test]
async fn table_insert_and_delete() {
    let fails = run(r#"
        var r = t.insertRow(0);
        eq(r.localName, 'tr', 'insertRow returns tr');
        eq(r.parentNode.localName, 'thead', 'insertRow(0) goes before the first row');
        eq(t.rows[0], r, 'first row');
        var last = t.insertRow();
        eq(last.parentNode.localName, 'tfoot', 'insertRow() appends to the last row parent');
        eq(t.rows[t.rows.length - 1], last, 'appended last');
        var mid = t.insertRow(3);
        eq(t.rows[3], mid, 'insertRow(3)');
        throws(function () { t.insertRow(-2); }, 'IndexSizeError', 'insertRow(-2)');
        throws(function () { t.insertRow(t.rows.length + 1); }, 'IndexSizeError', 'insertRow(len+1)');
        var n = t.rows.length;
        t.deleteRow(-1);
        eq(t.rows.length, n - 1, 'deleteRow(-1)');
        t.deleteRow(0);
        eq(t.rows[0].id, 'h1', 'deleteRow(0)');
        throws(function () { t.deleteRow(t.rows.length); }, 'IndexSizeError', 'deleteRow(len)');
        throws(function () { t.deleteRow(-2); }, 'IndexSizeError', 'deleteRow(-2)');
        throws(function () { t.deleteRow(); }, 'TypeError', 'deleteRow()');

        var e = document.createElement('table');
        var er = e.insertRow();
        eq(e.innerHTML, '<tbody><tr></tr></tbody>', 'insertRow on empty table creates tbody');
        e.deleteRow(-1);
        eq(e.innerHTML, '<tbody></tbody>', 'deleteRow(-1)');
        e.deleteRow(-1);
        eq(e.innerHTML, '<tbody></tbody>', 'deleteRow(-1) on no rows is a no-op');
        e.insertRow();
        eq(e.innerHTML, '<tbody><tr></tr></tbody>', 'insertRow uses the last tbody');

        var f = document.createElement('table');
        eq(f.createTFoot().localName, 'tfoot', 'createTFoot');
        eq(f.createTFoot(), f.tFoot, 'createTFoot returns existing');
        eq(f.createCaption(), f.firstChild, 'createCaption first');
        f.appendChild(document.createElement('colgroup'));
        f.createTHead();
        eq(f.innerHTML, '<caption></caption><thead></thead><tfoot></tfoot><colgroup></colgroup>', 'createTHead after caption');
        f.createTBody();
        f.createTBody();
        eq(f.tBodies.length, 2, 'createTBody');
        eq(f.lastChild.localName, 'tbody', 'createTBody appends when no tbody follows');
        f.deleteCaption(); f.deleteTHead(); f.deleteTFoot();
        eq(f.innerHTML, '<colgroup></colgroup><tbody></tbody><tbody></tbody>', 'delete helpers');
        var h = document.createElement('thead');
        f.tHead = h;
        eq(f.firstChild.nextSibling, h, 'tHead setter goes after colgroup');
        f.tHead = null;
        eq(f.tHead, null, 'tHead = null removes');
        throws(function () { f.tHead = document.createElement('tbody'); }, 'HierarchyRequestError', 'tHead = tbody');
        throws(function () { f.tFoot = document.createElement('div'); }, 'TypeError', 'tFoot = div');
        var ft = document.createElement('tfoot');
        f.tFoot = ft;
        eq(f.lastChild, ft, 'tFoot setter appends');
        var c = document.createElement('caption');
        f.caption = c;
        eq(f.firstChild, c, 'caption setter inserts first');
        f.caption = null;
        eq(f.caption, null, 'caption = null');
    "#)
    .await;
    assert!(fails.is_empty(), "{fails:#?}");
}

#[tokio::test]
async fn section_and_row_insert_and_delete() {
    let fails = run(r#"
        var b = document.getElementById('b1');
        var r = b.insertRow(1);
        eq(ids(b.rows), 'r1,tr,r2', 'section insertRow(1)');
        b.insertRow();
        eq(b.rows.length, 4, 'section insertRow()');
        throws(function () { b.insertRow(9); }, 'IndexSizeError', 'section insertRow(9)');
        b.deleteRow(-1);
        b.deleteRow(1);
        eq(ids(b.rows), 'r1,r2', 'section deleteRow');
        throws(function () { b.deleteRow(2); }, 'IndexSizeError', 'section deleteRow(2)');
        var row = document.getElementById('r2');
        var td = row.insertCell(0);
        eq(td.localName, 'td', 'insertCell makes td');
        eq(row.cells[0], td, 'insertCell(0)');
        row.insertCell();
        eq(row.cells.length, 3, 'insertCell()');
        throws(function () { row.insertCell(5); }, 'IndexSizeError', 'insertCell(5)');
        throws(function () { row.insertCell(-2); }, 'IndexSizeError', 'insertCell(-2)');
        row.deleteCell(-1);
        row.deleteCell(0);
        eq(row.cells.length, 1, 'deleteCell');
        throws(function () { row.deleteCell(1); }, 'IndexSizeError', 'deleteCell(1)');
        row.deleteCell(-1);
        row.deleteCell(-1);
        eq(row.cells.length, 0, 'deleteCell(-1) on no cells is a no-op');
    "#)
    .await;
    assert!(fails.is_empty(), "{fails:#?}");
}
