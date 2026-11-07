#!/usr/bin/env python3
"""Writes NAME.json for every case from hand-written metadata and the
hand-written NAME.md / NAME.links files next to it.

The metadata below (title, headings, `text`, `rel`) was written by hand
from the extraction rules in docs/adr/0042-extract-dumps.md; `text` follows
the older `--dump text` rules (lb-webapi text.rs), which this change does
not alter. Only the JSON layout comes from Python (json.dumps, indent 2,
keys in insertion order), which is independent of the Rust writer.

`{HOST}` stands for 127.0.0.1:PORT of the test's loopback server.
Run:  python3 gen_json.py
"""
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
HOST = "{HOST}"

# name: (title, lang, description, canonical, headings, rel by link index, text)
CASES = {
    "headings": (
        "Headings & paragraphs", "en", "A test page", f"http://{HOST}/canon/headings",
        [(1, "Main heading"), (2, "Second level"), (3, "Third level"), (4, "Fourth"), (5, "Fifth"), (6, "Sixth")],
        {},
        "Main heading\nFirst paragraph with inline span text, continued on a second line.\nSecond level\nCafé & crème <tag> © 2026\nThird level\nFourth\nFifth\nSixth\nBare text in a div",
    ),
    "lists": (
        "Lists", None, None, None, [(1, "Lists")], {},
        "Lists\nApples\nBananas\nCavendish\nPlantain\nGreen\nRipe\nCherries\nThree\nTen\nEleven\nnested one\nBetween\nFirst list\nSecond list, adjacent\nLoose one\nLoose para one\nLoose para two\nShown item",
    ),
    "links_base": (
        "Links", None, None, None, [(1, "Links")], {1: "noopener nofollow"},
        "Links\nSee the intro, the API reference and a CDN file.\nsame page js link js 2 mail us paren no href\nthe intro other text absolute path empty href\nspace in href",
    ),
    "images": (
        "Images", None, None, None, [(1, "Images")], {},
        "Images\nand .",
    ),
    "inline": (
        "Inline", None, None, None, [(1, "Inline")], {},
        "Inline\nSome emphasis, strong, bold, italic and deleted struck text.\nNested bold and italic inside, spaced out, and empty.\nCode: x = 1, a `tick` b, `edge`, Ctrl+C.\nLine one\nline two\nline three\n\nafter a double break\nHeading-like\nnext\nUnderscores snake_case_name and _lead and trail_ and **stars** and a_b.\nx yzw v",
    ),
    "pre": (
        "Code blocks", None, None, None, [(1, "Code blocks")], {},
        "Code blocks\nfn main() {\nprintln!(\"hi\");\n}\nplain\nindented bold\nline with\nbreak\n```\nfence inside\n```\nprint(1)\nint x;\n\nx",
    ),
    "quote": (
        "Quotes", None, None, None, [(1, "Quotes")], {},
        "Quotes\nFirst quoted paragraph.\nSecond with a link.\nNested quote\nquoted item\nAfter the rule\nBare text quote\ncode in quote",
    ),
    "table": (
        "Tables", None, None, None, [(1, "Tables")], {},
        "Tables\nName\tQty\tPrice\nApple | green\t3\t$1.20\nPear\t12\ta|b\nWide cell\nShort",
    ),
    "table_variants": (
        "Table variants", None, None, None, [(1, "Table variants")], {},
        "Table variants\nSales 2026\na\tb\nc\td\nsum\te\nH1\tH2\nx\ty\nonly",
    ),
    "hidden": (
        "Hidden", None, None, None, [(1, "Hidden")], {},
        "Hidden\nVisible one\nInline none\nClass none\nGhost text visible child\nBefore hidden inlineafter\nghost para\nClosed dialog Open dialog\nPrint only\nhidden link\nLast visible",
    ),
    "whitespace": (
        "Whitespace", None, None, None, [(1, "Whitespace test")], {},
        "Whitespace test\nLeading and multiple spaces and tabs\nnon breaking  spaces and a space\na b cd e\ntext in div span more\n\none\n\ntwo",
    ),
    "escapes": (
        "Escapes", None, None, None, [(1, "Escapes")], {},
        "Escapes\n*star* _under_ `tick` [brackets] <html> back\\slash ~~tilde~~ a~b\n# not a heading\n- not a list\n+ plus\n1. not ordered\n2) nor this\n> not a quote\n---\n===\n-5 degrees\nAT&T &amp; &#35; &copy; x&y\n#hashtag and 1.5 and 3.\nPipes | stay | in text\na * b",
    ),
    "skipped": (
        "Skipped", None, None, None, [(1, "Skipped")], {},
        "Skipped\nReal text\nsvg title\nsvg textx canvas fallback video fallbackaudio fallback textarea text Click me Label text\nEnd",
    ),
    "shadow": (
        "Shadow", None, None, None, [(1, "Shadow"), (2, "In shadow")], {},
        "Shadow\nslotted text unassigned light child\nnamed slot\nAfter",
    ),
    "blocks_in_inline": (
        "Link blocks", None, None, None, [(1, "Link blocks"), (2, "Section head")], {},
        "Link blocks\nCard title\nCard description\nCustom element paragraph\nSpan\nwith a div\ninside\nSection head\nArticle text\nTerm\nDefinition\nSummary\nDetail text\nCaption text",
    ),
    "no_h1": (
        "Title with *stars* & more", None, None, None, [(2, "Only h2")], {},
        "Only h2\nAnother hidden h1\nBody",
    ),
    "scripted": (
        "Scripted title", None, None, None, [(1, "Built by script")], {},
        "Built by script\nalpha\nbeta",
    ),
    "plain": (
        "", None, None, None, [], {},
        "line one\nline two <b>not markup</b>",
    ),
}


def read(name):
    with open(os.path.join(HERE, name), encoding="utf-8") as f:
        return f.read()


for name, (title, lang, desc, canon, headings, rels, text) in CASES.items():
    links = []
    for i, line in enumerate(l for l in read(name + ".links").split("\n") if l):
        url, text_ = line.split("\t", 1)
        links.append({"url": url, "text": text_, "rel": rels.get(i, "")})
    ext = "txt" if os.path.exists(os.path.join(HERE, name + ".txt")) else "html"
    url = f"http://{HOST}/{name}.{ext}"
    obj = {
        "url": url,
        "final_url": url,
        "status": 200,
        "title": title,
        "lang": lang,
        "description": desc,
        "canonical": canon,
        "headings": [{"level": l, "text": t} for l, t in headings],
        "links": links,
        "text": text,
        "markdown": read(name + ".md"),
    }
    with open(os.path.join(HERE, name + ".json"), "w", encoding="utf-8") as f:
        f.write(json.dumps(obj, indent=2, ensure_ascii=False) + "\n")
