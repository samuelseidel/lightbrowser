#!/usr/bin/env python3
"""Generate the fixed bench corpus under bench/corpus/ (deterministic).

About 50 static pages (articles, product lists, tables, forms, docs, threads)
that share a stylesheet and a few scripts and each add their own inline
script, stylesheet and images. Every page marks itself when its scripts have
built the DOM: `<html data-bench-rendered="N">`, with N listed in
`corpus/manifest.json`. The runner checks that mark for every page, so a
browser that skips scripts cannot look fast by accident.

Same seed, same bytes: no clock, no `random` module (its helpers changed
between Python versions), a tiny xorshift generator instead. The image
encoder is a minimal PNG writer on zlib, so the output also depends on the
zlib version only if compression levels change; the manifest records a hash
of the corpus and the runner reports it.

    python3 bench/gen_corpus.py            # write bench/corpus/
    python3 bench/gen_corpus.py --check    # exit 1 if the tree differs
"""

from __future__ import annotations

import argparse
import hashlib
import json
import struct
import sys
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
CORPUS = HERE / "corpus"
PAGES = 50
SEED = 0x1B5EED

WORDS = (
    "network browser page script render layout style cache request response header cookie "
    "session token window frame document element node event listener timer queue module "
    "parser engine buffer stream socket protocol handshake cipher record packet client server "
    "origin policy storage index query result filter sort group table column value metric "
    "latency memory budget profile sample report summary detail section chapter figure caption "
    "release version update change review commit branch merge deploy rollout monitor alert "
    "quiet bright narrow broad steady rapid gentle formal common simple careful useful stable "
    "garden market harbor valley bridge window lantern copper meadow pebble anchor ribbon "
    "orchard summit canyon harvest compass journal"
).split()

TOPICS = ["Networking", "Rendering", "Storage", "Security", "Tooling", "Testing", "Runtime", "Design"]
KINDS = ["article", "listing", "table", "form", "docs", "thread"]


class Rng:
    """xorshift64*, stable everywhere."""

    def __init__(self, seed: int) -> None:
        self.s = (seed or 1) & 0xFFFFFFFFFFFFFFFF

    def next(self) -> int:
        x = self.s
        x ^= x >> 12
        x ^= (x << 25) & 0xFFFFFFFFFFFFFFFF
        x ^= x >> 27
        self.s = x
        return (x * 0x2545F4914F6CDD1D) & 0xFFFFFFFFFFFFFFFF

    def below(self, n: int) -> int:
        return (self.next() >> 11) % n

    def pick(self, seq):
        return seq[self.below(len(seq))]

    def words(self, lo: int, hi: int) -> str:
        return " ".join(self.pick(WORDS) for _ in range(lo + self.below(hi - lo + 1)))

    def sentence(self) -> str:
        s = self.words(7, 16)
        return s[0].upper() + s[1:] + "."

    def paragraph(self) -> str:
        return " ".join(self.sentence() for _ in range(3 + self.below(4)))


# --- images -----------------------------------------------------------------


def png(w: int, h: int, rng: Rng, kind: int) -> bytes:
    """A small RGB PNG: gradients with a few blocks, so it compresses like a photo-ish graphic."""
    base = [rng.below(200) + 30 for _ in range(3)]
    rows = []
    blocks = [(rng.below(w), rng.below(h), 8 + rng.below(w // 2), 8 + rng.below(h // 2), rng.below(256)) for _ in range(6)]
    for y in range(h):
        row = bytearray([0])
        for x in range(w):
            if kind == 0:
                r = (base[0] + x * 2) & 255
                g = (base[1] + y * 2) & 255
                b = (base[2] + (x + y)) & 255
            else:
                r = (base[0] + (x * y >> 4)) & 255
                g = (base[1] + (x ^ y)) & 255
                b = (base[2] + x) & 255
            for bx, by, bw, bh, c in blocks:
                if bx <= x < bx + bw and by <= y < by + bh:
                    r, g, b = (r + c) & 255, (g ^ c) & 255, (b + c // 2) & 255
            row += bytes((r, g, b))
        rows.append(bytes(row))
    raw = b"".join(rows)

    def chunk(tag: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


# --- shared assets ----------------------------------------------------------

SITE_CSS = """\
:root{--fg:#1d2330;--bg:#fafaf7;--accent:#1f6f8b;--muted:#6b7280;--line:#dcdcd4}
*{box-sizing:border-box}
html{font-family:system-ui,"Segoe UI",Roboto,sans-serif;color:var(--fg);background:var(--bg)}
body{margin:0;line-height:1.55}
header.site{display:flex;align-items:center;justify-content:space-between;padding:12px 24px;border-bottom:1px solid var(--line);background:#fff}
header.site .brand{font-weight:700;color:var(--accent);text-decoration:none}
nav.main a{margin-left:16px;color:var(--fg);text-decoration:none}
nav.main a:hover{color:var(--accent)}
main{max-width:960px;margin:0 auto;padding:24px}
h1{font-size:2rem;margin:0 0 .5rem}
h2{font-size:1.4rem;margin:1.6rem 0 .5rem;border-bottom:1px solid var(--line);padding-bottom:.2rem}
h3{font-size:1.1rem;margin:1.2rem 0 .4rem}
p{margin:.6rem 0}
.meta{color:var(--muted);font-size:.9rem}
.toc{background:#fff;border:1px solid var(--line);padding:12px 16px;margin:16px 0}
.toc ol{margin:0;padding-left:20px}
figure{margin:16px 0;padding:0}
figure img{max-width:100%;height:auto;display:block;border:1px solid var(--line)}
figcaption{font-size:.85rem;color:var(--muted)}
.grid{display:grid;grid-template-columns:repeat(3,1fr);gap:16px}
.card{background:#fff;border:1px solid var(--line);padding:12px;border-radius:4px}
.card img{width:100%;height:auto;display:block}
.card .price{font-weight:700;color:var(--accent)}
.card.sale{border-color:#b45309}
table{border-collapse:collapse;width:100%;margin:12px 0}
th,td{border:1px solid var(--line);padding:6px 10px;text-align:left}
th{background:#f0f0ea}
tr.odd td{background:#f8f8f3}
form.f label{display:block;margin:10px 0 2px;font-weight:600}
form.f input,form.f select,form.f textarea{width:100%;padding:6px 8px;border:1px solid var(--line);font:inherit}
form.f button{margin-top:12px;padding:8px 16px;background:var(--accent);color:#fff;border:0;border-radius:3px}
pre,code{font-family:ui-monospace,Menlo,Consolas,monospace;font-size:.9rem}
pre{background:#f0f0ea;padding:10px;overflow:auto}
.comment{border-left:3px solid var(--line);padding:4px 12px;margin:10px 0}
.comment .by{font-weight:600}
.comment.depth1{margin-left:24px}
.comment.depth2{margin-left:48px}
footer.site{border-top:1px solid var(--line);padding:16px 24px;color:var(--muted);font-size:.85rem}
.hidden{display:none}
@media (max-width:700px){.grid{grid-template-columns:1fr}nav.main a{margin-left:8px}}
"""

LIB_JS = """\
/* shared helpers, loaded synchronously in <head> */
(function (g) {
  'use strict';
  function el(tag, attrs, children) {
    var n = document.createElement(tag);
    if (attrs) for (var k in attrs) {
      if (k === 'text') n.textContent = attrs[k];
      else if (k === 'class') n.className = attrs[k];
      else n.setAttribute(k, attrs[k]);
    }
    if (children) for (var i = 0; i < children.length; i++) n.appendChild(children[i]);
    return n;
  }
  function slug(s) { return String(s).toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, ''); }
  function money(n) { return '$' + (Math.round(n * 100) / 100).toFixed(2); }
  function mark(n) { document.documentElement.setAttribute('data-bench-rendered', String(n)); }
  g.bench = { el: el, slug: slug, money: money, mark: mark, ready: [] };
})(window);
"""

APP_JS = """\
/* deferred: runs before DOMContentLoaded */
(function () {
  'use strict';
  var links = document.querySelectorAll('nav.main a');
  for (var i = 0; i < links.length; i++) {
    links[i].addEventListener('click', function (e) { e.preventDefault(); });
    if (links[i].getAttribute('href') === location.pathname) links[i].classList.add('current');
  }
  var rows = document.querySelectorAll('table tbody tr');
  for (var j = 0; j < rows.length; j++) if (j % 2) rows[j].classList.add('odd');
  var imgs = document.images, n = 0;
  for (var k = 0; k < imgs.length; k++) if (imgs[k].getAttribute('alt')) n++;
  document.documentElement.setAttribute('data-bench-images', String(n));
  var y = document.getElementById('year');
  if (y) y.textContent = '2026';
  var h = document.body.getBoundingClientRect().height;
  document.documentElement.setAttribute('data-bench-laid-out', h >= 0 ? '1' : '0');
})();
"""

TINY_JS = """\
/* async: a small deferred-work script */
setTimeout(function () { document.documentElement.setAttribute('data-bench-timer', '1'); }, 5);
"""


# --- pages ------------------------------------------------------------------


def nav(i: int) -> str:
    prev_i, next_i = (i - 1) % PAGES, (i + 1) % PAGES
    return (
        '<header class="site"><a class="brand" href="/index.html">Corpus</a>'
        '<nav class="main">'
        f'<a href="/pages/p{prev_i:02d}.html">Previous</a>'
        f'<a href="/pages/p{next_i:02d}.html">Next</a>'
        '<a href="/index.html">Index</a></nav></header>'
    )


def head(i: int, title: str) -> str:
    return (
        '<!doctype html>\n<html lang="en"><head><meta charset="utf-8">'
        '<meta name="viewport" content="width=device-width,initial-scale=1">'
        f"<title>{title}</title>"
        '<link rel="stylesheet" href="/css/site.css">'
        f'<link rel="stylesheet" href="/css/p{i:02d}.css">'
        '<script src="/js/lib.js"></script>'
        "</head><body>\n"
    )


def tail(i: int) -> str:
    return (
        '<footer class="site">Static bench page, generated. <span id="year">----</span></footer>\n'
        '<script src="/js/app.js" defer></script>'
        '<script src="/js/tiny.js" async></script>'
        "</body></html>\n"
    )


def page_css(i: int, rng: Rng) -> str:
    hue = rng.below(360)
    return (
        f".hero-{i:02d}{{background:hsl({hue},40%,94%);padding:16px;border-left:6px solid hsl({hue},50%,40%)}}\n"
        f".tag-{i:02d}{{color:hsl({hue},60%,30%);font-weight:600}}\n"
        f"main.k{i % 6} h1{{letter-spacing:{(i % 3) * 0.01:.2f}em}}\n"
    )


def js_str(s: str) -> str:
    return json.dumps(s)


def build_article(i: int, rng: Rng, imgs: list[str]) -> tuple[str, int]:
    title = f"{rng.pick(TOPICS)}: {rng.words(3, 5)}"
    nsec = 5 + rng.below(4)
    body = []
    for s in range(nsec):
        body.append(f"<h2>{rng.words(2, 4).title()}</h2>")
        for _ in range(2 + rng.below(3)):
            body.append(f"<p>{rng.paragraph()}</p>")
        if s % 2 == 0:
            body.append(
                f'<figure><img src="{imgs[s % len(imgs)]}" width="320" height="200" alt="{rng.words(2, 4)}">'
                f"<figcaption>{rng.sentence()}</figcaption></figure>"
            )
        if s == 1:
            body.append(f"<pre><code>{rng.words(8, 14)};\n{rng.words(6, 10)};</code></pre>")
    script = (
        "<script>(function(){var hs=document.querySelectorAll('main h2'),toc=bench.el('ol');"
        "for(var i=0;i<hs.length;i++){hs[i].id=bench.slug(hs[i].textContent);"
        "var li=bench.el('li'),a=bench.el('a',{href:'#'+hs[i].id,text:hs[i].textContent});li.appendChild(a);toc.appendChild(li);}"
        "var box=document.getElementById('toc');box.appendChild(toc);"
        "var words=document.querySelectorAll('main p'),total=0;"
        "for(var j=0;j<words.length;j++)total+=words[j].textContent.split(/\\s+/).length;"
        "document.getElementById('wc').textContent=total+' words';"
        "bench.mark(hs.length);})();</script>"
    )
    html = (
        head(i, title)
        + nav(i)
        + f'<main class="k{i % 6}"><div class="hero-{i:02d}"><h1>{title}</h1>'
        f'<p class="meta">By <span class="tag-{i:02d}">{rng.words(2, 2).title()}</span> &middot; <span id="wc"></span></p></div>'
        '<div class="toc" id="toc"><strong>Contents</strong></div>\n'
        + "\n".join(body)
        + "</main>\n"
        + script
        + tail(i)
    )
    return html, nsec


def build_listing(i: int, rng: Rng, imgs: list[str]) -> tuple[str, int]:
    n = 12 + rng.below(9)
    items = [
        {"id": k, "name": rng.words(2, 3).title(), "price": 5 + rng.below(9000) / 100, "sale": rng.below(4) == 0,
         "img": imgs[k % len(imgs)], "blurb": rng.sentence()}
        for k in range(n)
    ]
    title = f"Shop: {rng.words(2, 3)}"
    script = (
        f"<script>(function(){{var items={json.dumps(items, separators=(',', ':'))};"
        "var grid=document.getElementById('grid'),sum=0;"
        "items.sort(function(a,b){return a.price-b.price;});"
        "items.forEach(function(it){var c=bench.el('div',{'class':'card'+(it.sale?' sale':'')},["
        "bench.el('img',{src:it.img,width:'200',height:'120',alt:it.name}),"
        "bench.el('h3',{text:it.name}),bench.el('p',{text:it.blurb}),"
        "bench.el('p',{'class':'price',text:bench.money(it.price)})]);"
        "c.addEventListener('click',function(){c.classList.toggle('sel');});grid.appendChild(c);sum+=it.price;});"
        "document.getElementById('total').textContent=bench.money(sum);"
        f"bench.mark(grid.children.length);}})();</script>"
    )
    html = (
        head(i, title)
        + nav(i)
        + f'<main class="k{i % 6}"><div class="hero-{i:02d}"><h1>{title}</h1>'
        '<p class="meta">Catalogue total <span id="total"></span></p></div>'
        '<div class="grid" id="grid"></div></main>\n'
        + script
        + tail(i)
    )
    return html, n


def build_table(i: int, rng: Rng, imgs: list[str]) -> tuple[str, int]:
    nrows = 30 + rng.below(25)
    cols = ["Host", "Requests", "Bytes", "Errors", "Median ms"]
    data = [[rng.words(1, 1) + str(k), rng.below(5000), rng.below(900000), rng.below(40), rng.below(900)] for k in range(nrows)]
    title = f"Report: {rng.words(2, 3)}"
    script = (
        f"<script>(function(){{var rows={json.dumps(data, separators=(',', ':'))},cols={json.dumps(cols)};"
        "var t=document.getElementById('t'),th=bench.el('thead'),tr=bench.el('tr');"
        "cols.forEach(function(c){tr.appendChild(bench.el('th',{text:c}));});th.appendChild(tr);t.appendChild(th);"
        "var tb=bench.el('tbody'),req=0,bytes=0;"
        "rows.forEach(function(r){var row=bench.el('tr');r.forEach(function(v){row.appendChild(bench.el('td',{text:String(v)}));});"
        "tb.appendChild(row);req+=r[1];bytes+=r[2];});t.appendChild(tb);"
        "document.getElementById('sum').textContent=req+' requests, '+bytes+' bytes';"
        "bench.mark(tb.children.length);})();</script>"
    )
    html = (
        head(i, title)
        + nav(i)
        + f'<main class="k{i % 6}"><div class="hero-{i:02d}"><h1>{title}</h1>'
        f'<p class="meta" id="sum"></p></div><table id="t"></table>'
        f'<figure><img src="{imgs[0]}" width="320" height="200" alt="{rng.words(2, 3)}"><figcaption>{rng.sentence()}</figcaption></figure>'
        "</main>\n"
        + script
        + tail(i)
    )
    return html, nrows


def build_form(i: int, rng: Rng, imgs: list[str]) -> tuple[str, int]:
    title = f"Settings: {rng.words(2, 3)}"
    nfields = 8 + rng.below(6)
    fields = "".join(
        f'<label for="f{k}">{rng.words(1, 2).title()}</label>'
        + (
            f'<input id="f{k}" name="f{k}" type="text" value="{rng.words(1, 1)}">'
            if k % 3 == 0
            else f'<select id="f{k}" name="f{k}"><option>{rng.words(1, 1)}</option><option>{rng.words(1, 1)}</option></select>'
            if k % 3 == 1
            else f'<textarea id="f{k}" name="f{k}" rows="2">{rng.sentence()}</textarea>'
        )
        for k in range(nfields)
    )
    script = (
        "<script>(function(){var f=document.getElementById('form'),els=f.elements,n=0;"
        "for(var i=0;i<els.length;i++){var e=els[i];if(e.name){n++;"
        "e.addEventListener('input',function(){f.setAttribute('data-dirty','1');});}}"
        "f.addEventListener('submit',function(ev){ev.preventDefault();});"
        "document.getElementById('count').textContent=n+' fields';"
        "bench.mark(n);})();</script>"
    )
    html = (
        head(i, title)
        + nav(i)
        + f'<main class="k{i % 6}"><div class="hero-{i:02d}"><h1>{title}</h1><p class="meta" id="count"></p></div>'
        f'<form class="f" id="form" action="/pages/p{i:02d}.html">{fields}<button type="submit">Save</button></form></main>\n'
        + script
        + tail(i)
    )
    return html, nfields


def build_docs(i: int, rng: Rng, imgs: list[str]) -> tuple[str, int]:
    title = f"Reference: {rng.words(2, 3)}"
    nsec = 6 + rng.below(4)
    parts = []
    for s in range(nsec):
        parts.append(f"<section class='api'><h3>{rng.pick(WORDS)}.{rng.pick(WORDS)}({rng.pick(WORDS)})</h3>")
        parts.append(f"<p>{rng.paragraph()}</p>")
        parts.append(f"<pre><code>let {rng.pick(WORDS)} = {rng.pick(WORDS)}.{rng.pick(WORDS)}({rng.below(100)});</code></pre></section>")
    script = (
        "<script>(function(){var secs=document.querySelectorAll('section.api'),idx=bench.el('ul');"
        "for(var i=0;i<secs.length;i++){var h=secs[i].querySelector('h3');"
        "var li=bench.el('li',{text:h.textContent});idx.appendChild(li);"
        "if(i%2===1)secs[i].classList.add('alt');}"
        "document.getElementById('idx').appendChild(idx);"
        "var q=document.querySelectorAll('pre > code');"
        "bench.mark(secs.length+q.length*0);})();</script>"
    )
    html = (
        head(i, title)
        + nav(i)
        + f'<main class="k{i % 6}"><div class="hero-{i:02d}"><h1>{title}</h1></div>'
        '<div class="toc" id="idx"><strong>API</strong></div>' + "\n".join(parts) + "</main>\n"
        + script
        + tail(i)
    )
    return html, nsec


def build_thread(i: int, rng: Rng, imgs: list[str]) -> tuple[str, int]:
    title = f"Thread: {rng.words(3, 5)}"
    n = 18 + rng.below(14)
    comments = [{"by": rng.words(1, 1) + str(rng.below(99)), "depth": rng.below(3), "text": rng.paragraph()} for _ in range(n)]
    script = (
        f"<script>(function(){{var cs={json.dumps(comments, separators=(',', ':'))};"
        "var root=document.getElementById('thread'),by={};"
        "cs.forEach(function(c){var d=bench.el('div',{'class':'comment depth'+c.depth},["
        "bench.el('div',{'class':'by',text:c.by}),bench.el('p',{text:c.text})]);root.appendChild(d);by[c.by]=(by[c.by]||0)+1;});"
        "document.getElementById('authors').textContent=Object.keys(by).length+' authors';"
        "bench.mark(root.querySelectorAll('.comment').length);})();</script>"
    )
    html = (
        head(i, title)
        + nav(i)
        + f'<main class="k{i % 6}"><div class="hero-{i:02d}"><h1>{title}</h1><p class="meta" id="authors"></p></div>'
        f'<p>{rng.paragraph()}</p><div id="thread"></div></main>\n'
        + script
        + tail(i)
    )
    return html, n


BUILDERS = {"article": build_article, "listing": build_listing, "table": build_table, "form": build_form,
            "docs": build_docs, "thread": build_thread}


def digest(files: dict[str, bytes]) -> str:
    """Hash of names and contents (manifest.json excluded), the corpus identity in bench results."""
    h = hashlib.sha256()
    for name in sorted(files):
        if name != "manifest.json":
            h.update(name.encode() + b"\0" + hashlib.sha256(files[name]).digest())
    return h.hexdigest()


def disk_digest(root: Path = CORPUS) -> str:
    """`digest` of the files currently on disk."""
    return digest({str(p.relative_to(root)): p.read_bytes() for p in root.rglob("*") if p.is_file()})


def build() -> dict[str, bytes]:
    files: dict[str, bytes] = {
        "css/site.css": SITE_CSS.encode(),
        "js/lib.js": LIB_JS.encode(),
        "js/app.js": APP_JS.encode(),
        "js/tiny.js": TINY_JS.encode(),
    }
    irng = Rng(SEED ^ 0xABCDEF)
    shared = []
    for k in range(10):
        name = f"img/s{k:02d}.png"
        files[name] = png(160, 100, irng, k % 2)
        shared.append("/" + name)
    pages = []
    for i in range(PAGES):
        rng = Rng(SEED + i * 7919)
        kind = KINDS[i % len(KINDS)]
        own = f"img/p{i:02d}.png"
        files[own] = png(96, 64, rng, i % 2)
        imgs = ["/" + own] + [shared[(i + j) % len(shared)] for j in range(1, 5)]
        files[f"css/p{i:02d}.css"] = page_css(i, rng).encode()
        html, expected = BUILDERS[kind](i, rng, imgs)
        path = f"pages/p{i:02d}.html"
        files[path] = html.encode()
        title = html.split("<title>", 1)[1].split("</title>", 1)[0]
        pages.append({"path": "/" + path, "kind": kind, "title": title, "expected_rendered": expected})
    links = "".join(f'<li><a href="{p["path"]}">{p["title"]}</a></li>' for p in pages)
    files["index.html"] = (
        '<!doctype html>\n<html lang="en"><head><meta charset="utf-8"><title>Bench corpus</title>'
        '<link rel="stylesheet" href="/css/site.css"></head><body><main><h1>Bench corpus</h1><ul>'
        + links + "</ul></main></body></html>\n"
    ).encode()
    manifest = {
        "seed": SEED,
        "pages": pages,
        "files": len(files) + 1,
        "bytes": sum(len(v) for v in files.values()),
        "sha256": digest(files),
    }
    files["manifest.json"] = (json.dumps(manifest, indent=1) + "\n").encode()
    return files


def write(files: dict[str, bytes], root: Path = CORPUS) -> None:
    for name, data in files.items():
        p = root / name
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)


def tree_matches(files: dict[str, bytes], root: Path = CORPUS) -> bool:
    have = {str(p.relative_to(root)) for p in root.rglob("*") if p.is_file()} if root.is_dir() else set()
    if have != set(files):
        return False
    return all((root / n).read_bytes() == d for n, d in files.items())


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check", action="store_true", help="exit 1 if bench/corpus differs from the generator output")
    args = ap.parse_args()
    files = build()
    if args.check:
        ok = tree_matches(files)
        print("corpus up to date" if ok else "corpus differs from generator output")
        return 0 if ok else 1
    write(files)
    total = sum(len(v) for v in files.values())
    print(f"wrote {len(files)} files, {total / 1e6:.2f} MB to {CORPUS}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
