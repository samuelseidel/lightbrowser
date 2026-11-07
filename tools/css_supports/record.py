#!/usr/bin/env python3
"""Record Chrome's `CSS.supports` data and write `lb-css/src/supports_data.rs`.

Loopback only. Serves a generated page on 127.0.0.1, loads it in a local
headless Chrome (`--dump-dom`) and reads back, for every property Chrome
knows (the names of `CSSStyleDeclaration` that pass
`CSS.supports(name, 'initial')`):

- which value classes it accepts (`CLASSES`: a custom ident, a dashed
  ident, a length, zero, a number, ...),
- which keywords of `vocab.txt` it accepts,
- which functions of `funcs.txt` it accepts (one sample call each).

Usage:
    python3 tools/css_supports/record.py --chrome PATH [--json OUT.json]
        [--from-json IN.json]
    python3 tools/css_supports/record.py --chrome PATH \
        --cases crates/lb-page/tests/css_supports/cases.json \
        --json crates/lb-page/tests/css_supports/chrome.json

The persona is Chrome 153, so record with Chrome (for Testing) 153, e.g.
Playwright's `chromium_headless_shell-1243/.../chrome-headless-shell`.
`--from-json` regenerates the Rust file from an earlier recording.
"""

import argparse
import html
import http.server
import json
import os
import re
import subprocess
import sys
import tempfile
import threading

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
OUT_RS = os.path.join(ROOT, "crates", "lb-css", "src", "supports_data.rs")

# Order matters: bit i of a property's mask is CLASSES[i].
CLASSES = ["foo", "--a", "1px", "0", "1", "50%", "#f00", "1s", "45deg", '"abcd"',
           "1fr", "2dppx", "[a]", "10hz", "1 / 2", "a,b"]

# Named and system colors: stored as one bit (`COLOR_BIT`), not per keyword.
COLOR_KEYWORDS = set("""red blue green black white gray grey canvas canvastext
linktext visitedtext activetext buttonface buttontext buttonborder field
fieldtext highlight highlighttext selecteditem selecteditemtext mark marktext
graytext accentcolor accentcolortext activeborder activecaption appworkspace
background buttonhighlight buttonshadow captiontext inactiveborder
inactivecaption inactivecaptiontext infobackground infotext menutext scrollbar
threeddarkshadow threedface threedhighlight threedlightshadow threedshadow
window windowframe windowtext -webkit-link transparent currentcolor
fuchsia""".split())
WIDE = {"inherit", "initial", "unset", "revert", "revert-layer"}

PAGE = """<!doctype html><pre id=out></pre><script>
const V = %(vocab)s, F = %(funcs)s, C = %(classes)s;
const WIDE = ['inherit', 'initial', 'unset', 'revert', 'revert-layer'];
const s = document.body.style, names = new Set();
for (const k of Object.getOwnPropertyNames(s)) names.add(k);
for (let p = Object.getPrototypeOf(s); p && p !== Object.prototype; p = Object.getPrototypeOf(p))
  for (const k of Object.getOwnPropertyNames(p)) names.add(k);
const dash = k => (k.startsWith('webkit') ? '-' : '') + k.replace(/[A-Z]/g, c => '-' + c.toLowerCase());
const props = new Set();
for (const k of names) { const d = k.includes('-') ? k : dash(k); if (CSS.supports(d, 'initial')) props.add(d); }
const out = {};
for (const p of [...props].sort()) {
  // A component is accepted alone, doubled, or next to lengths, an
  // ident or a string (`box-shadow: 1px 1px red`, `counter-reset: a 1`,
  // `font-feature-settings: "liga" 1`, `cursor: url(a.cur), auto`).
  const CTX = [v => v, v => v + ' ' + v, v => '1px 1px ' + v, v => v + ' 1px 1px',
    v => '1px ' + v, v => v + ' 1px', v => 'a ' + v, v => v + ' a', v => '"abcd" ' + v,
    v => v + ' 1', v => v + ', auto'];
  const ok = v => CTX.some(c => CSS.supports(p, c(v)));
  let m = 0; C.forEach((v, i) => { if (ok(v)) m |= 1 << i; });
  if (ok('red')) m |= 1 << C.length;
  const k = []; V.forEach((v, i) => { if (!WIDE.includes(v) && ok(v)) k.push(i); });
  // Keywords valid only next to another one (`text-box: trim-both cap`):
  // pair each with the accepted ones, twice, so chains of three are found.
  for (let round = 0; round < 2 && k.length <= 60; round++) {
    const have = k.map(i => V[i]);
    V.forEach((v, i) => {
      if (k.includes(i) || WIDE.includes(v)) return;
      if (have.some(h => !WIDE.includes(h) && (CSS.supports(p, h + ' ' + v) || CSS.supports(p, v + ' ' + h)))) k.push(i);
    });
  }
  const f = []; F.forEach((v, i) => { if (ok(v[1])) f.push(i); });
  out[p] = [m, k, f];
}
document.getElementById('out').textContent = JSON.stringify({ua: navigator.userAgent, vocab: V, funcs: F, classes: C, props: out});
</script>"""


CASES_PAGE = """<!doctype html><pre id=out></pre><script>
const T = %(cases)s;
const two = T.two.map(([n, v]) => [n, v, CSS.supports(n, v)]);
const one = T.one.map(([t]) => [t, CSS.supports(t)]);
document.getElementById('out').textContent = JSON.stringify({ua: navigator.userAgent, two, one});
</script>"""


def run_page(chrome, page):
    """Load `page` from a loopback server in headless `chrome`; return the
    JSON it left in `<pre id=out>`."""

    class H(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            body = page.encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *a):
            pass

    srv = http.server.HTTPServer(("127.0.0.1", 0), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    url = "http://127.0.0.1:%d/" % srv.server_address[1]
    with tempfile.TemporaryDirectory() as d:
        dom = subprocess.run(
            [chrome, "--headless", "--use-mock-keychain", "--no-first-run",
             "--disable-background-networking", "--no-proxy-server",
             "--user-data-dir=" + d, "--dump-dom", url],
            capture_output=True, text=True, timeout=120).stdout
    srv.shutdown()
    m = re.search(r'<pre id="out">(.*?)</pre>', dom, re.S)
    if not m:
        sys.exit("no result in the dumped DOM")
    return json.loads(html.unescape(m.group(1)))


def record(chrome):
    vocab = sorted(set(open(os.path.join(HERE, "vocab.txt")).read().split()))
    funcs = [l.split("\t") for l in open(os.path.join(HERE, "funcs.txt")).read().strip().split("\n")]
    page = PAGE % {"vocab": json.dumps(vocab), "funcs": json.dumps(funcs), "classes": json.dumps(CLASSES)}
    return run_page(chrome, page)


def rust_str(s):
    return json.dumps(s)


def generate(data):
    vocab, funcs = data["vocab"], data["funcs"]
    color_bit = len(CLASSES)
    keywords, kw_sets, kw_index = set(), [], {}
    fn_sets, fn_index = [], {}
    rows = []
    for p in sorted(data["props"]):
        mask, kws, fns = data["props"][p]
        ks = {vocab[i] for i in kws} - WIDE
        if "red" in ks:
            mask |= 1 << color_bit  # bit 16 may also be set by 'red red'
        ks -= COLOR_KEYWORDS
        if mask & 1:
            ks = set()  # any custom ident
        keywords |= ks
        rows.append((p, mask, frozenset(ks), tuple(sorted(funcs[i][0] for i in fns))))
    keywords = sorted(keywords)
    kpos = {k: i for i, k in enumerate(keywords)}
    fnames = sorted({f[0] for f in funcs})
    fpos = {f: i for i, f in enumerate(fnames)}
    out_rows = []
    for p, mask, ks, fs in rows:
        key = tuple(sorted(kpos[k] for k in ks))
        if key not in kw_index:
            kw_index[key] = len(kw_sets)
            kw_sets.append(key)
        fkey = tuple(sorted(fpos[f] for f in fs))
        if fkey not in fn_index:
            fn_index[fkey] = len(fn_sets)
            fn_sets.append(fkey)
        out_rows.append((p, mask, kw_index[key], fn_index[fkey]))
    w = []
    w.append("//! Chrome's `CSS.supports` data, recorded by `tools/css_supports/record.py`")
    w.append("//! from %s." % data.get("ua", "Chrome"))
    w.append("//! Do not edit by hand.")
    w.append("")
    w.append("/// Value classes, bit i of a property's mask (bit %d: named colors)." % color_bit)
    w.append("pub(crate) const CLASSES: &[&str] = &[%s];" % ", ".join(rust_str(c) for c in CLASSES))
    w.append("pub(crate) const COLOR_BIT: u32 = %d;" % color_bit)
    w.append("")
    w.append("/// Keywords some property accepts (sorted).")
    w.append("pub(crate) static KEYWORDS: &[&str] = &[")
    for k in keywords:
        w.append("    %s," % rust_str(k))
    w.append("];")
    w.append("")
    w.append("/// Keyword sets: sorted indices into `KEYWORDS`.")
    w.append("pub(crate) static KW_SETS: &[&[u16]] = &[")
    for s in kw_sets:
        w.append("    &[%s]," % ", ".join(str(i) for i in s))
    w.append("];")
    w.append("")
    w.append("/// Function probes (sorted): `math*` are math functions by result")
    w.append("/// type, `color` the color functions, `gradient` the gradients.")
    w.append("pub(crate) static FUNCS: &[&str] = &[")
    for f in fnames:
        w.append("    %s," % rust_str(f))
    w.append("];")
    w.append("")
    w.append("/// Function sets: sorted indices into `FUNCS`.")
    w.append("pub(crate) static FN_SETS: &[&[u8]] = &[")
    for s in fn_sets:
        w.append("    &[%s]," % ", ".join(str(i) for i in s))
    w.append("];")
    w.append("")
    w.append("/// Every property Chrome knows (sorted): name, class mask, keyword set,")
    w.append("/// function set.")
    w.append("pub(crate) static PROPERTIES: &[(&str, u32, u16, u16)] = &[")
    for p, mask, k, f in out_rows:
        w.append("    (%s, %d, %d, %d)," % (rust_str(p), mask, k, f))
    w.append("];")
    open(OUT_RS, "w").write("\n".join(w) + "\n")
    try:
        subprocess.run(["rustfmt", "--edition", "2024", OUT_RS], check=False)
    except FileNotFoundError:
        print("rustfmt not found: run `rustfmt --edition 2024` on the file")
    print("wrote %s: %d properties, %d keywords, %d keyword sets, %d function sets"
          % (OUT_RS, len(out_rows), len(keywords), len(kw_sets), len(fn_sets)))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--chrome")
    ap.add_argument("--json")
    ap.add_argument("--from-json")
    ap.add_argument("--cases", help="record Chrome's answers for a cases file "
                    "({two: [[name, value], ...], one: [[text], ...]}) to --json")
    a = ap.parse_args()
    if a.cases:
        cases = json.load(open(a.cases))
        out = run_page(a.chrome, CASES_PAGE % {"cases": json.dumps(cases)})
        json.dump(out, open(a.json, "w"), indent=0)
        return
    if a.from_json:
        data = json.load(open(a.from_json))
    else:
        if not a.chrome:
            sys.exit("--chrome or --from-json is required")
        data = record(a.chrome)
    if a.json:
        json.dump(data, open(a.json, "w"))
    generate(data)


if __name__ == "__main__":
    main()
