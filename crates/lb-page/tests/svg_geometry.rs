//! SVG geometry (`getBBox`, `getCTM`, `getTotalLength`,
//! `getPointAtLength`, `SVGTextContentElement` metrics) in a real page.
//! Expectations were recorded in local Chrome 156 (headless, loopback):
//! shape geometry exactly, text with `--force-device-scale-factor=2` on
//! macOS, whose Arial advances and rounded metrics our macOS tables
//! reproduce.
#![cfg(feature = "chrome")]

mod common;

use std::sync::Arc;

use common::{Resource, browser, context, open, serve};
use lb_js::JsValue;
use lb_net::ClientHelloProfile;
use lb_page::{BrowserContext, ContextOptions, WaitUntil};
use lb_persona::{Persona, PersonaProfile};

const SHAPES: &str = r##"<svg id=s width=400 height=300 xmlns="http://www.w3.org/2000/svg">
<rect id=r x=10 y=20 width=30 height=40 />
<rect id=rneg x=10 y=20 width=-5 height=40 />
<circle id=c cx=50 cy=60 r=10 />
<circle id=cneg cx=5 cy=5 r=-1 />
<ellipse id=e cx=50 cy=60 rx=10 ry=5 />
<line id=l x1=5 y1=50 x2=1 y2=7 />
<polyline id=pl points="0,0 10,5 3,30" />
<polygon id=pg points="0,0 10,5 3,30 -4" />
<path id=p d="M10 10 C 20 20, 40 20, 50 10" />
<path id=p2 d="M 10 80 Q 52.5 10, 95 80 T 180 80" />
<path id=p3 d="M 10 315 L 110 215 A 30 50 0 0 1 162.55 162.45 L 172.55 152.45 A 30 50 -45 0 1 215.1 109.9 L 315 10" />
<path id=p4 d="m10 10h20v30zM5 5l1 1" />
<path id=p5 d="M 20 20 S 40 0 60 20 s 20 20 40 0" />
<path id=pa d="M 10 10 A 20 30 0 1 1 50 50" />
<path id=pclose d="M 0 0 L 10 0 L 10 10 Z L 20 20" />
<path id=pbad d="M 10 10 L 20 20 X 50 50" />
<path id=pempty d="" />
<g id=g transform="translate(5,7)"><rect x=1 y=2 width=3 height=4 transform="scale(2) rotate(45)"/><circle cx=100 cy=100 r=1 /></g>
<g id=gempty></g>
<g id=gz><rect x=50 y=50 width=0 height=10 /><circle cx=1 cy=1 r=1 /></g>
<g id=gpercent><rect x="10%" y="10%" width="50%" height="1em" style="font-size:20px"/></g>
<g id=gt transform="rotate(30 10 10) skewX(10) matrix(1 0 0 1 3 4)"><rect width=10 height=10 /></g>
<g id=gc><svg x=10 y=20 width=100 height=50 viewBox="0 0 10 10"><rect x=1 y=1 width=2 height=2 /></svg></g>
<svg id=ns x=10 y=20 width=100 height=50 viewBox="0 0 10 10"><rect id=nr x=1 y=1 width=2 height=2 /></svg>
<rect id=rcss x=1 y=1 width=2 height=2 style="x:5px;width:10px" />
<rect id=rr x=0 y=0 width=30 height=20 rx=5 />
<rect id=rr2 x=0 y=0 width=30 height=20 ry=50 />
<rect id=dn x=1 y=1 width=5 height=5 style="display:none"/>
<g style="display:none"><rect id=dn2 x=1 y=1 width=5 height=5 /></g>
<defs><rect id=dr width=10 height=10 /><path id=dp d="M0 0L10 0"/></defs>
<use id=u href="#r" x=100 y=100 />
<use id=us href="#gt" transform="translate(100)" />
<polyline id=pl2 points="0,0 3,4 3,10" />
<polygon id=pg2 points="0,0 3,4 3,10" />
</svg>"##;

/// Runs `script` (which fills `fails`) after `markup` and returns the
/// failures. `mac` uses the macOS reference persona (DPR 2).
async fn run(markup: &str, script: &str, mac: bool) -> Vec<String> {
    let html = format!(
        "<!DOCTYPE html><meta charset=utf-8><body style='margin:0'>{markup}\
         <script>var fails = [];\
         function eq(a, b, m) {{ if (a !== b) fails.push(m + ': ' + a + ' !== ' + b); }}\
         function near(a, b, m, tol) {{ if (!(Math.abs(a - b) <= (tol || 1e-4))) fails.push(m + ': ' + a + ' !~ ' + b); }}\
         function arr(a, b, m, tol) {{ if (typeof a === 'string') {{ fails.push(m + ': ' + a); return; }} \
           for (var i = 0; i < b.length; i++) tol === 0 ? eq(a[i], b[i], m + '[' + i + ']') : near(a[i], b[i], m + '[' + i + ']', tol); }}\
         function err(f) {{ try {{ f(); return 'no throw'; }} catch (e) {{ return e.name + ': ' + e.message; }} }}\
         function $(id) {{ return document.getElementById(id); }}\
         function box(id) {{ var b = (typeof id === 'string' ? $(id) : id).getBBox(); return [b.x, b.y, b.width, b.height]; }}\
         function pt(p) {{ return [p.x, p.y]; }}\
         function rc(r) {{ return [r.x, r.y, r.width, r.height]; }}\
         function mx(m) {{ return [m.a, m.b, m.c, m.d, m.e, m.f]; }}\
         try {{ {script} }} catch (e) {{ fails.push('threw: ' + e); }}\
         document.body.setAttribute('data-fails', JSON.stringify(fails));</script></body>"
    );
    let s = serve(vec![("/", Resource::html(&html))]).await;
    let (b, ctx) = (browser(), if mac { mac_context() } else { context(false) });
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

fn mac_context() -> BrowserContext {
    let persona = Arc::new(Persona::default_chrome());
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../profiles");
    let tls = Arc::new(
        ClientHelloProfile::load(std::path::Path::new(&format!(
            "{dir}/tls/{}.json",
            persona.tls_profile_id
        )))
        .unwrap(),
    );
    let profile = PersonaProfile::from_path(format!(
        "{dir}/reference/chrome/chrome153_macos14_intel_irisplus_1440x900.json"
    ))
    .unwrap();
    BrowserContext::new(ContextOptions {
        persona,
        profile: Some(Arc::new(profile)),
        tls_profile: tls,
        accept_invalid_certs: false,
        virtual_time: false,
    })
    .unwrap()
}

#[tokio::test]
async fn shape_boxes_match_chrome() {
    let fails = run(
        SHAPES,
        r#"
        arr(box('r'), [10, 20, 30, 40], 'rect', 0);
        arr(box('rneg'), [10, 20, 0, 40], 'negative width', 0);
        arr(box('c'), [40, 50, 20, 20], 'circle', 0);
        arr(box('cneg'), [5, 5, 0, 0], 'negative r', 0);
        arr(box('e'), [40, 55, 20, 10], 'ellipse', 0);
        arr(box('l'), [1, 7, 4, 43], 'line', 0);
        arr(box('pl'), [0, 0, 10, 30], 'polyline', 0);
        arr(box('pg'), [0, 0, 10, 30], 'polygon with an odd coordinate', 0);
        arr(box('p'), [10, 10, 40, 7.5], 'cubic extrema', 0);
        arr(box('p2'), [10, 45, 170, 70], 'quadratic and T', 0);
        arr(box('p3'), [10, 10, 305, 305], 'arcs', 0);
        arr(box('p4'), [5, 5, 25, 35], 'relative h/v/z and a second subpath', 0);
        arr(box('p5'), [20, 11.11111068725586, 80, 23.88888931274414], 'S/s reflection', 0);
        arr(box('pa'), [10, -6.055508613586426, 44.03700637817383, 56.05550765991211], 'large arc', 2e-5);
        arr(box('pclose'), [0, 0, 20, 20], 'line after close', 0);
        arr(box('pbad'), [10, 10, 10, 10], 'path data error', 0);
        arr(box('pempty'), [0, 0, 0, 0], 'empty path', 0);
        arr(box('g'), [-7.071067810058594, 4.242640495300293, 108.0710678100586, 96.75736236572266], 'g with transformed children', 1e-6);
        arr(box('gempty'), [0, 0, 0, 0], 'empty g', 0);
        arr(box('gz'), [0, 0, 2, 2], 'empty shapes skipped', 0);
        arr(box('gpercent'), [40, 30, 200, 20], 'percent and em lengths', 0);
        arr(box('gt'), [0, 0, 10, 10], 'own transform not applied', 0);
        arr(box('gc'), [40, 25, 10, 10], 'nested svg viewport', 0);
        arr(box('ns'), [1, 1, 2, 2], 'nested svg own box', 0);
        arr(box('rcss'), [5, 1, 10, 2], 'inline geometry properties', 0);
        arr(box('rr'), [0, 0, 30, 20], 'rounded rect', 0);
        arr(box('dn'), [0, 0, 0, 0], 'display none', 0);
        arr(box('dn2'), [1, 1, 5, 5], 'inside a display:none g', 0);
        arr(box('dr'), [0, 0, 10, 10], 'inside defs', 0);
        arr(box('u'), [110, 120, 30, 40], 'use x/y', 0);
        arr(box('us'), [4.0756731033325195, 1.6565015316009521, 12.133217811584473, 14.541889190673828], 'use of a transformed g', 1e-5);
        var det = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
        det.setAttribute('width', 5);
        arr(box(det), [0, 0, 0, 0], 'detached', 0);
        var b = $('r').getBBox();
        eq(String(b), '[object SVGRect]', 'SVGRect');
        b.x = 3.3;
        eq(b.x, 3.299999952316284, 'float setter');
        eq($('r').getBBox() === b, false, 'new object per call');
        eq(err(function () { b.y = NaN; }), "TypeError: Failed to set the 'y' property on 'SVGRect': The provided float value is non-finite.", 'non-finite');
        eq(err(function () { SVGGraphicsElement.prototype.getBBox.call(document.body); }), 'TypeError: Illegal invocation', 'brand check');
        eq(err(function () { new SVGRect(); }), "TypeError: Failed to construct 'SVGRect': Illegal constructor", 'no constructor');
        eq(Object.getOwnPropertyNames(SVGRect.prototype).join(), 'x,y,width,height,constructor', 'SVGRect members');
        var names = ['ellipse', 'line', 'polyline', 'polygon', 'tspan', 'use', 'defs', 'foreignObject', 'linearGradient', 'textPath', 'image', 'a', 'switch', 'symbol'];
        eq(names.map(function (n) { return Object.prototype.toString.call(document.createElementNS('http://www.w3.org/2000/svg', n)); }).join(),
           '[object SVGEllipseElement],[object SVGLineElement],[object SVGPolylineElement],[object SVGPolygonElement],[object SVGTSpanElement],[object SVGUseElement],[object SVGDefsElement],[object SVGForeignObjectElement],[object SVGLinearGradientElement],[object SVGTextPathElement],[object SVGImageElement],[object SVGAElement],[object SVGSwitchElement],[object SVGSymbolElement]',
           'element interfaces');
    "#,
        false,
    )
    .await;
    assert!(fails.is_empty(), "{fails:#?}");
}

#[tokio::test]
async fn lengths_and_points_match_chrome() {
    let fails = run(
        SHAPES,
        r#"
        function len(id) { return $(id).getTotalLength(); }
        // Chrome's curve lengths differ from PathTraversalState in the last
        // digits (see svg.rs); boxes and line lengths are exact.
        near(len('p'), 43.808631896972656, 'cubic', 1e-3);
        near(len('p3'), 555.2012939453125, 'arcs', 0.05);
        eq(len('r'), 140, 'rect');
        eq(len('c'), 62.42697525024414, 'circle (Skia conics)');
        eq(len('e'), 47.958152770996094, 'ellipse');
        eq(len('rr'), 90.61468505859375, 'rounded rect');
        eq(len('rr2'), 78.8204345703125, 'radii clamped');
        eq(len('l'), Math.fround(Math.hypot(4, 43)), 'line');
        arr(pt($('pa').getPointAtLength(30)), [32.861114501953125, -5.806947231292725], 'point on arc', 1e-3);
        eq(len('pl2'), 11, 'polyline');
        eq(len('pg2'), 21.4403076171875, 'polygon closes');
        near(len('pa'), 95.35272216796875, 'large arc', 1e-2);
        near(len('pclose'), 62.42640686035156, 'close and inject move', 1e-5);
        eq(len('dn'), 20, 'display none still has geometry');
        eq(len('dp'), 10, 'inside defs');
        eq(len('pempty'), 0, 'empty');
        arr(pt($('p').getPointAtLength(10)), [18.359792709350586, 15.358988761901855], 'point on cubic', 1e-3);
        arr(pt($('p').getPointAtLength(-5)), [10, 10], 'clamped low', 0);
        arr(pt($('p').getPointAtLength(1e9)), [50, 10], 'clamped high', 1e-4);
        arr(pt($('pclose').getPointAtLength(25)), [6.464466094970703, 6.464466094970703], 'after close', 1e-5);
        arr(pt($('rr').getPointAtLength(3)), [8, 0], 'rounded rect start', 0);
        arr(pt($('rr').getPointAtLength(7)), [12, 0], 'rounded rect top', 0);
        eq(String($('p').getPointAtLength(1)), '[object SVGPoint]', 'SVGPoint');
        eq(err(function () { $('pempty').getPointAtLength(1); }), "InvalidStateError: Failed to execute 'getPointAtLength' on 'SVGGeometryElement': The element's path is empty.", 'empty path');
        eq(err(function () { $('p').getPointAtLength(); }), "TypeError: Failed to execute 'getPointAtLength' on 'SVGGeometryElement': 1 argument required, but only 0 present.", 'arity');
        var det = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
        eq(err(function () { det.getTotalLength(); }), "InvalidStateError: Failed to execute 'getTotalLength' on 'SVGGeometryElement': This element is non-rendered element.", 'detached');
    "#,
        false,
    )
    .await;
    assert!(fails.is_empty(), "{fails:#?}");
}

#[tokio::test]
async fn ctm_matches_chrome() {
    let fails = run(
        SHAPES,
        r#"
        arr(mx($('g').getCTM()), [1, 0, 0, 1, 5, 7], 'g', 0);
        arr(mx($('g').firstChild.getCTM()), [1.4142135623730951, 1.4142135623730951, -1.4142135623730951, 1.4142135623730951, 5, 7], 'nested transforms', 1e-12);
        arr(mx($('gt').getCTM()), [0.8660254037844387, 0.49999999999999994, -0.3472963553338606, 0.9541888941386711, 7.548636752173485, 1.656501538710299], 'rotate about a point, skew, matrix', 1e-12);
        arr(mx($('nr').getCTM()), [5, 0, 0, 5, 35, 20], 'up to the nearest viewport', 0);
        arr(mx($('ns').getCTM()), [5, 0, 0, 5, 35, 20], 'svg includes its viewport', 0);
        arr(mx($('nr').getScreenCTM()), [5, 0, 0, 5, 35, 20], 'screen', 0);
        arr(mx($('s').getScreenCTM()), [1, 0, 0, 1, 0, 0], 'outer svg', 0);
        var m = $('g').getCTM();
        eq(String(m), '[object SVGMatrix]', 'SVGMatrix');
        arr(mx(m.translate(1, 2)), [1, 0, 0, 1, 6, 9], 'translate', 0);
        arr(mx(m.scale(2).inverse()), [0.5, 0, 0, 0.5, -2.5, -3.5], 'inverse', 0);
        arr(mx(m.flipX()), [-1, 0, 0, 1, 5, 7], 'flipX', 0);
        arr(mx(m.multiply(m)), [1, 0, 0, 1, 10, 14], 'multiply', 0);
        var p = $('s').createSVGPoint(); p.x = 1; p.y = 1;
        arr(pt(p.matrixTransform(m)), [6, 8], 'matrixTransform', 0);
        eq(err(function () { m.scale(0).inverse(); }), "InvalidStateError: Failed to execute 'inverse' on 'SVGMatrix': The matrix is not invertible.", 'singular');
        eq(err(function () { m.multiply({}); }), "TypeError: Failed to execute 'multiply' on 'SVGMatrix': parameter 1 is not of type 'SVGMatrix'.", 'multiply type');
        arr(rc($('s').createSVGRect()), [0, 0, 0, 0], 'createSVGRect', 0);
    "#,
        false,
    )
    .await;
    assert!(fails.is_empty(), "{fails:#?}");
}

const TEXT: &str = r#"<svg width=400 height=300>
<text id=t x=10 y=50 style="font: 16px Arial">Hello world</text>
<text id=tb x=0 y=50 style="font: bold 16px Arial">abc</text>
<text id=tdy x=0 y=50 dy="5 -3" dx="1 2" style="font: 16px Arial">abc</text>
<text id=trot x=0 y=50 rotate="10 20" style="font: 16px Arial">abc</text>
<text id=tw x=0 y=50 style="font: 16px Arial">   a
   b   </text>
<text id=tsp x=0 y=50 style="font: 16px Arial">a <tspan> b</tspan> </text>
<text id=tam x=100 y=50 text-anchor=middle style="font: 16px Arial">abc</text>
<text id=tae x=100 y=50 style="font: 16px Arial;text-anchor:end">abc</text>
<text id=tls x=0 y=50 style="font: 16px Arial;letter-spacing:2px">abc</text>
<text id=txs x="10 20 30" y="5 6" style="font: 16px Arial">abcd</text>
<text id=tfs x=0 y=50 font-size=30 font-family=Arial>abc</text>
<text id=te x=0 y=50 style="font: 16px Arial">😀a</text>
<text id=temp x=10 y=80></text>
</svg>"#;

#[tokio::test]
async fn text_metrics_match_chrome() {
    let fails = run(
        TEXT,
        r#"
        var t = $('t');
        eq(t.getNumberOfChars(), 11, 'chars');
        eq(t.getComputedTextLength(), 79.140625, 'computed length');
        arr(box('t'), [10, 35.5, 79.140625, 18], 'text box', 0);
        eq(t.getSubStringLength(1, 2), 12.453125, 'substring');
        eq(t.getSubStringLength(10, 100), 8.8984375, 'substring clamped');
        eq(t.getSubStringLength(1, -1), t.getSubStringLength(1, 10), 'nchars as unsigned');
        arr(pt(t.getStartPositionOfChar(1)), [21.5546875, 50], 'start', 0);
        arr(pt(t.getEndPositionOfChar(1)), [30.453125, 50], 'end', 0);
        arr(rc(t.getExtentOfChar(1)), [21.5546875, 35.5, 8.8984375, 18], 'extent', 0);
        eq(t.getRotationOfChar(1), 0, 'rotation');
        eq(t.getCharNumAtPosition({x: 12, y: 45}), 0, 'hit');
        eq(t.getCharNumAtPosition({x: 0, y: 0}), -1, 'miss');
        eq(t.getCharNumAtPosition(), -1, 'default point');
        eq(t.selectSubString(0, 1), undefined, 'selectSubString');
        var pre = "IndexSizeError: Failed to execute '";
        eq(err(function () { t.getSubStringLength(20, 1); }), pre + "getSubStringLength' on 'SVGTextContentElement': The charnum provided (20) is greater than the maximum bound (11).", 'past end');
        eq(err(function () { t.getSubStringLength(11, 0); }), pre + "getSubStringLength' on 'SVGTextContentElement': The charnum provided (11) is greater than or equal to the maximum bound (11).", 'at end');
        eq(err(function () { t.getEndPositionOfChar(-1); }), pre + "getEndPositionOfChar' on 'SVGTextContentElement': The charnum provided (4294967295) is greater than the maximum bound (11).", 'unsigned charnum');
        eq(err(function () { t.selectSubString(99, 1); }), pre + "selectSubString' on 'SVGTextContentElement': The charnum provided (99) is greater than the maximum bound (11).", 'selectSubString bound');
        eq(err(function () { t.getSubStringLength(1); }), "TypeError: Failed to execute 'getSubStringLength' on 'SVGTextContentElement': 2 arguments required, but only 1 present.", 'arity');
        eq(err(function () { t.getCharNumAtPosition(5); }), "TypeError: Failed to execute 'getCharNumAtPosition' on 'SVGTextContentElement': The provided value is not of type 'DOMPointInit'.", 'point type');

        arr(box('tb'), [0, 35.5, 27.5703125, 18], 'bold', 0);
        eq($('tb').getComputedTextLength(), 27.5703125, 'bold length');
        arr(box('tdy'), [1, 37.5, 27.796875, 21], 'dx/dy lists', 0);
        arr(pt($('tdy').getStartPositionOfChar(1)), [11.8984375, 52], 'dx/dy position', 0);
        eq($('tdy').getComputedTextLength(), 25.796875, 'dx not in the length');
        arr(box('trot'), [-0.6077685952186584, 35.72028732299805, 30.88147735595703, 20.612083435058594], 'rotate box', 1e-5);
        arr(pt($('trot').getEndPositionOfChar(1)), [17.26023292541504, 53.0434455871582], 'rotated end', 1e-5);
        arr(rc($('trot').getExtentOfChar(1)), [7.701366901397705, 36.37445831298828, 14.518159866333008, 19.95791244506836], 'rotated extent', 1e-5);
        eq($('trot').getRotationOfChar(2), 20, 'last rotate repeats');
        eq($('tw').getNumberOfChars(), 3, 'whitespace collapsed');
        eq($('tsp').getNumberOfChars(), 3, 'collapsed across tspan');
        arr(box('tam'), [87.1015625, 35.5, 25.796875, 18], 'text-anchor attribute', 0);
        arr(box('tae'), [74.203125, 35.5, 25.796875, 18], 'text-anchor style', 0);
        eq($('tls').getComputedTextLength(), 31.796875, 'letter-spacing');
        arr(box('txs'), [10, -9.5, 36.8984375, 19], 'x/y lists', 0);
        arr(rc($('txs').getExtentOfChar(1)), [20, -8.5, 8.8984375, 18], 'x/y list extent', 0);
        arr(pt($('txs').getStartPositionOfChar(1)), [20, 6], 'x/y list position', 0);
        eq($('tfs').getComputedTextLength(), 48.369140625, 'font presentation attributes');
        eq($('te').getNumberOfChars(), 3, 'UTF-16 units');
        eq($('te').getSubStringLength(1, 1), $('te').getSubStringLength(0, 1), 'low surrogate maps to its glyph');
        eq($('temp').getNumberOfChars(), 0, 'empty text');
        arr(box('temp'), [0, 0, 0, 0], 'empty text box', 0);
        var dt = document.createElementNS('http://www.w3.org/2000/svg', 'text');
        dt.textContent = 'abc';
        eq(dt.getNumberOfChars(), 0, 'detached text');
        eq(dt.getComputedTextLength(), 0, 'detached length');
    "#,
        true,
    )
    .await;
    assert!(fails.is_empty(), "{fails:#?}");
}

#[tokio::test]
async fn text_widths_agree_with_canvas() {
    // The same metric tables as canvas measureText (CreepJS compares them).
    let fails = run(
        r#"<svg width=600 height=300><text id=t x=0 y=100 style="font: 40px Arial">Quick fox</text></svg>"#,
        r#"
        var c = document.createElement('canvas').getContext('2d');
        c.font = '40px Arial';
        near($('t').getComputedTextLength(), c.measureText('Quick fox').width, 'svg vs canvas', 0.02);
    "#,
        true,
    )
    .await;
    assert!(fails.is_empty(), "{fails:#?}");
}
