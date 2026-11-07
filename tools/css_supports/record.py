#!/usr/bin/env python3
"""Record Chrome's `CSS.supports()` answers (loopback only).

Serves a generated page on 127.0.0.1, loads it in a local headless Chrome
(`--dump-dom`) and reads the answers back from the page.

Modes:

    # The corpus (`gen_cases.py`): rewrite it with Chrome's answers.
    python3 tools/css_supports/record.py --chrome PATH \\
        --corpus crates/lb-page/tests/css_supports/corpus.jsonl

    # The hand-written cases: {two: [[name, value], ...], one: [[text], ...]}.
    python3 tools/css_supports/record.py --chrome PATH \\
        --cases crates/lb-page/tests/css_supports/cases.json \\
        --json crates/lb-page/tests/css_supports/chrome.json

    # Chrome's property names (`CSSStyleDeclaration` names that take
    # `initial`, plus the hidden `-epub-` aliases) against the grammar
    # file's: prints the names only one side has.
    python3 tools/css_supports/record.py --chrome PATH --props

The persona is Chrome 153, so record with Chrome (for Testing) 153, e.g.
Playwright's `chromium_headless_shell-1243/.../chrome-headless-shell`.
`--port` picks the loopback port (default: any free one).
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
GRAMMAR = os.path.join(ROOT, "crates", "lb-css", "src", "supports.grammar")
sys.path.insert(0, HERE)

PROPS_PAGE = """<!doctype html><pre id=out></pre><script>
const s = document.body.style, names = new Set();
for (const k of Object.getOwnPropertyNames(s)) names.add(k);
for (let p = Object.getPrototypeOf(s); p && p !== Object.prototype; p = Object.getPrototypeOf(p))
  for (const k of Object.getOwnPropertyNames(p)) names.add(k);
const dash = k => (k.startsWith('webkit') ? '-' : '') + k.replace(/[A-Z]/g, c => '-' + c.toLowerCase());
const props = new Set();
for (const k of names) { const d = k.includes('-') ? k : dash(k); if (CSS.supports(d, 'initial')) props.add(d); }
for (const k of %(extra)s) if (CSS.supports(k, 'initial')) props.add(k);
document.getElementById('out').textContent = JSON.stringify({ua: navigator.userAgent, props: [...props].sort()});
</script>"""

# Aliases Chrome parses but does not expose on CSSStyleDeclaration.
HIDDEN = ["-epub-caption-side", "-epub-text-combine", "-epub-text-emphasis",
          "-epub-text-emphasis-color", "-epub-text-emphasis-style", "-epub-text-orientation",
          "-epub-text-transform", "-epub-word-break", "-epub-writing-mode"]

CORPUS_PAGE = """<!doctype html><pre id=out></pre><script>
const T = %(cases)s;
const r = T.map(c => c.length == 2 ? CSS.supports(c[0], c[1]) : CSS.supports(c[0]));
document.getElementById('out').textContent = JSON.stringify({ua: navigator.userAgent, r});
</script>"""

CASES_PAGE = """<!doctype html><pre id=out></pre><script>
const T = %(cases)s;
const two = T.two.map(([n, v]) => [n, v, CSS.supports(n, v)]);
const one = T.one.map(([t]) => [t, CSS.supports(t)]);
document.getElementById('out').textContent = JSON.stringify({ua: navigator.userAgent, two, one});
</script>"""

PORT = 0


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

    srv = http.server.HTTPServer(("127.0.0.1", PORT), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    url = "http://127.0.0.1:%d/" % srv.server_address[1]
    # Linux hosts without unprivileged user namespaces have no sandbox.
    extra = ["--no-sandbox"] if sys.platform.startswith("linux") else []
    with tempfile.TemporaryDirectory() as d:
        dom = subprocess.run(
            [chrome, "--headless", "--use-mock-keychain", "--no-first-run",
             "--disable-background-networking", "--no-proxy-server",
             "--user-data-dir=" + d, *extra, "--dump-dom", url],
            capture_output=True, text=True, timeout=600).stdout
    srv.shutdown()
    srv.server_close()
    m = re.search(r'<pre id="out">(.*?)</pre>', dom, re.S)
    if not m:
        sys.exit("no result in the dumped DOM")
    return json.loads(html.unescape(m.group(1)))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--chrome", required=True)
    ap.add_argument("--json", help="output file (--cases), or another output for --corpus")
    ap.add_argument("--cases", help="record Chrome's answers for a cases file "
                    "({two: [[name, value], ...], one: [[text], ...]}) to --json")
    ap.add_argument("--corpus", help="record Chrome's answers for a JSON-lines corpus "
                    "(`gen_cases.py`: [name, value] or [condition] per line, an optional "
                    "answer last) and rewrite it with them")
    ap.add_argument("--props", action="store_true",
                    help="compare Chrome's property names with the grammar file's")
    ap.add_argument("--port", type=int, default=0, help="loopback port (default: any)")
    a = ap.parse_args()
    global PORT
    PORT = a.port
    if a.corpus:
        lines = [json.loads(l) for l in open(a.corpus, encoding="utf-8") if l.strip()]
        cases = [c for c in lines if isinstance(c, list)]
        # Strip earlier answers: [name, value, bool] / [condition, bool].
        cases = [c[:-1] if c and isinstance(c[-1], bool) else c for c in cases]
        out = run_page(a.chrome, CORPUS_PAGE % {"cases": json.dumps(cases)})
        with open(a.json or a.corpus, "w", encoding="utf-8") as f:
            f.write(json.dumps({"ua": out["ua"]}) + "\n")
            for c, r in zip(cases, out["r"]):
                f.write(json.dumps(c + [r], ensure_ascii=False) + "\n")
        print("%d cases, %d supported" % (len(cases), sum(out["r"])))
    elif a.cases:
        cases = json.load(open(a.cases))
        out = run_page(a.chrome, CASES_PAGE % {"cases": json.dumps(cases)})
        json.dump(out, open(a.json, "w"), indent=0)
    elif a.props:
        import grammar
        out = run_page(a.chrome, PROPS_PAGE % {"extra": json.dumps(HIDDEN)})
        chrome = set(out["props"])
        ours = set(grammar.load(GRAMMAR)[0])
        print(out["ua"])
        print("%d properties in Chrome, %d in the grammar" % (len(chrome), len(ours)))
        for n in sorted(chrome - ours):
            print("missing from the grammar:", n)
        for n in sorted(ours - chrome):
            print("not in Chrome:", n)
    else:
        ap.error("one of --corpus, --cases or --props is required")


if __name__ == "__main__":
    main()
