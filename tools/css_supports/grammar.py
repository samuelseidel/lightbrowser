"""CSS value definition syntax: parse a grammar file and generate values.

The grammar file is `crates/lb-css/src/supports.grammar` (the data
lb-css's `CSS.supports()` matches against). Lines are `name = syntax`
(a property), `<name> = syntax` (a type) or comments (`#`). The syntax is
CSS Values 4 value definition syntax: keywords, `<type>`, `<type [a,b]>`,
`<'property'>`, `name( ... )`, `[ ]`, juxtaposition, `&&`, `||`, `|`,
`?`, `*`, `+`, `#`, `{m}`, `{m,n}`, `{m,}`, `!`, quoted and bare
punctuation (`,`, `/`, `:`, `'['`).

`gen_cases.py` uses `generate()` to make valid-looking values and
`mutate()` to make likely-invalid ones; Chrome decides which are valid.
"""

import random
import re

INF = float("inf")


class Node:
    """kind: kw, lit, type, prop, func, paren, seq, all, any, one, mult."""

    __slots__ = ("kind", "val", "kids", "lo", "hi", "comma", "req", "rng")

    def __init__(self, kind, val=None, kids=None):
        self.kind, self.val, self.kids = kind, val, kids or []
        self.lo = self.hi = 1
        self.comma = self.req = False
        self.rng = None

    def __repr__(self):
        return "%s(%r,%r)" % (self.kind, self.val, self.kids)


TOKEN = re.compile(
    r"\s+|(?P<type><[^>]*>)|(?P<func>-?[A-Za-z_][\w-]*\()|(?P<kw>-?[A-Za-z_][\w-]*)"
    r"|(?P<num>\d+)|(?P<q>'[^']*')|(?P<op>\|\||&&|\^\^|[\[\]|?*+#!{},/:;=()<>%.@-])")


def tokenize(s):
    out, i = [], 0
    while i < len(s):
        m = TOKEN.match(s, i)
        if not m:
            raise ValueError("bad grammar at %r" % s[i:])
        i = m.end()
        if m.lastgroup:
            out.append((m.lastgroup, m.group(m.lastgroup)))
    return out


class P:
    def __init__(self, toks):
        self.t, self.i = toks, 0

    def peek(self, k=0):
        return self.t[self.i + k] if self.i + k < len(self.t) else (None, None)

    def take(self):
        self.i += 1
        return self.t[self.i - 1]

    def one(self):
        kids = [self.any()]
        while self.peek() == ("op", "|"):
            self.take()
            kids.append(self.any())
        return kids[0] if len(kids) == 1 else Node("one", kids=kids)

    def any(self):
        # `^^` (Blink's greedy `||`) generates like `||`.
        kids = [self.all()]
        while self.peek() in (("op", "||"), ("op", "^^")):
            self.take()
            kids.append(self.all())
        return kids[0] if len(kids) == 1 else Node("any", kids=kids)

    def all(self):
        kids = [self.seq()]
        while self.peek() == ("op", "&&"):
            self.take()
            kids.append(self.seq())
        return kids[0] if len(kids) == 1 else Node("all", kids=kids)

    def seq(self):
        kids = []
        while True:
            k, v = self.peek()
            if k is None or (k == "op" and v in ("|", "||", "^^", "&&", "]", ")")):
                break
            kids.append(self.mult(self.term()))
        if not kids:
            raise ValueError("empty group")
        return kids[0] if len(kids) == 1 else Node("seq", kids=kids)

    def term(self):
        k, v = self.take()
        if k == "op" and v == "[":
            n = self.one()
            assert self.take() == ("op", "]")
            return Node("seq", kids=[n]) if n.kind != "seq" else n
        if k == "op" and v == "(":
            n = self.one() if self.peek() != ("op", ")") else None
            assert self.take() == ("op", ")")
            return Node("paren", kids=[n] if n else [])
        if k == "func":
            name = v[:-1]
            n = self.one() if self.peek() != ("op", ")") else None
            assert self.take() == ("op", ")"), (name, self.t[self.i - 1:])
            return Node("func", name, [n] if n else [])
        if k == "type":
            inner = v[1:-1].strip()
            if inner.startswith("'"):
                return Node("prop", inner.strip("'"))
            inner = inner.split("~")[0].strip()  # `<custom-ident ~none>`: exclusions
            m = re.match(r"([\w()-]+)\s*(?:\[([^\]]*)\])?$", inner)
            n = Node("type", m.group(1))
            if m.group(2):
                a, b = [x.strip() for x in m.group(2).split(",")]
                n.rng = (num(a), num(b))
            return n
        if k == "kw":
            return Node("kw", v)
        if k == "q":
            return Node("lit", v[1:-1])
        if k == "op" or k == "num":
            return Node("lit", v)
        raise ValueError("unexpected %r" % ((k, v),))

    def mult(self, n):
        while True:
            k, v = self.peek()
            if k != "op":
                return n
            if v in ("?", "*", "+", "#", "{", "!"):
                self.take()
            else:
                return n
            m = Node("mult", kids=[n])
            if v == "?":
                m.lo, m.hi = 0, 1
            elif v == "*":
                m.lo, m.hi = 0, INF
            elif v == "+":
                m.lo, m.hi = 1, INF
            elif v == "!":
                n.req = True
                continue
            elif v == "#":
                m.lo, m.hi, m.comma = 1, INF, True
                if self.peek() == ("op", "{"):
                    self.take()
                    m.lo, m.hi = self.braces()
            elif v == "{":
                m.lo, m.hi = self.braces()
            n = m

    def braces(self):
        lo = int(self.take()[1])
        hi = lo
        if self.peek() == ("op", ","):
            self.take()
            hi = INF if self.peek() == ("op", "}") else int(self.take()[1])
        assert self.take() == ("op", "}")
        return lo, hi


def num(s):
    s = s.replace("∞", "inf")
    m = re.match(r"[-+]?(inf|[\d.]+)", s)
    return float(m.group(0))


def parse(syntax):
    p = P(tokenize(syntax))
    n = p.one()
    if p.i != len(p.t):
        raise ValueError("trailing grammar: %r" % (p.t[p.i:],))
    return n


def load(path):
    """Return (properties, types): name -> syntax text."""
    props, types = {}, {}
    for line in open(path, encoding="utf-8"):
        line = line.rstrip("\n")
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        name, _, syn = line.partition(" = ")
        name = name.strip()
        if name.startswith("<"):
            types[name[1:-1]] = syn
        else:
            props[name] = syn
    return props, types


# ---------------------------------------------------------------------------
# Generation
# ---------------------------------------------------------------------------

SAMPLES = {
    "length": ["1px", "0", "2.5em", "10vw", "3rem", "1in", "4ch", "1cqi", "5vmin", "1q",
               "calc(1px + 2em)", "-2px", "100px", "1lh", "2svh"],
    "percentage": ["50%", "0%", "100%", "12.5%", "calc(10% * 2)", "-5%"],
    "number": ["1", "0", "0.5", "2", "1.5", "-1", "calc(1 + 2)", "3"],
    "integer": ["1", "2", "0", "3", "-1", "calc(2)", "10"],
    "angle": ["45deg", "1rad", "0.5turn", "100grad", "0deg", "calc(1deg + 1rad)", "-90deg"],
    "time": ["1s", "200ms", "0s", "0.5s", "calc(1s + 10ms)"],
    "frequency": ["1hz", "2khz"],
    "resolution": ["2dppx", "96dpi", "1x", "2x"],
    "flex": ["1fr", "0.5fr", "2fr"],
    "string": ['"a"', "'b c'", '""', '"x"'],
    "url": ["url(a.png)", 'url("a.png")', "url('b.svg#c')"],
    "ident": ["foo", "bar", "a-b"],
    "custom-ident": ["foo", "bar-baz", "_x", "a1"],
    "dashed-ident": ["--a", "--foo-bar"],
    "hex-color": ["#f00", "#ff0000", "#ff000080", "#f008", "#ABCDEF"],
    "named-color": ["red", "blue", "rebeccapurple", "transparent", "Black"],
    "declaration-value": ["foo bar", "1px", "a, b"],
    "any-value": ["foo", "1px 2px"],
    "family-name": ["Arial", '"Times New Roman"', "Helvetica Neue", "'Font A'"],
    "generic-family": ["serif", "sans-serif", "monospace", "cursive", "fantasy", "system-ui"],
    "dimension": ["1px", "2em", "3s"],
    "zero": ["0"],
    "custom-property-name": ["--x"],
    "length-lit": ["1px", "0", "2em"],
    "path-data": ['"M0 0 L1 1"', '"M 0 0 h 10 v 10 z"', '""', '"M0 0 A1 1 0 1 1 2 2"', '"L0 0"'],
    "calc-size()": ["calc-size(auto, size)", "calc-size(fit-content, size + 1px)",
                    "calc-size(1px, size * 2)", "calc-size(any, size)"],
}


class Gen:
    def __init__(self, props, types, rnd=None):
        self.props, self.types = props, types
        self.cache = {}
        self.r = rnd or random.Random(1)

    def node(self, key, syn):
        if key not in self.cache:
            try:
                self.cache[key] = parse(syn) if syn and syn != "?" else None
            except Exception:
                self.cache[key] = None
        return self.cache[key]

    def prop(self, name, depth=0):
        return self.gen(self.node(("p", name), self.props.get(name)), depth)

    def gen(self, n, depth=0):
        """A list of component strings, or None when the grammar is unknown."""
        if n is None:
            return None
        r = self.r
        k = n.kind
        if k == "kw":
            return [n.val]
        if k == "lit":
            return [n.val]
        if k == "prop":
            if depth > 6:
                return None
            return self.prop(n.val, depth + 1)
        if k == "type":
            if n.val in self.types:
                if depth > 6:
                    return None
                return self.gen(self.node(("t", n.val), self.types[n.val]), depth + 1)
            s = SAMPLES.get(n.val)
            if not s:
                return None
            v = r.choice(s)
            if n.rng and v.lstrip("calc(").startswith("-"):
                v = s[0]
            return [v]
        if k == "func":
            inner = self.gen(n.kids[0], depth + 1) if n.kids else []
            if inner is None:
                return None
            return [n.val + "(" + join(inner) + ")"]
        if k == "paren":
            inner = self.gen(n.kids[0], depth + 1) if n.kids else []
            return None if inner is None else ["(" + join(inner) + ")"]
        if k == "seq":
            out = []
            for c in n.kids:
                g = self.gen(c, depth)
                if g is None:
                    return None
                out += g
            return out
        if k == "one":
            kids = list(n.kids)
            r.shuffle(kids)
            for c in kids:
                g = self.gen(c, depth + 1)
                if g is not None:
                    return g
            return None
        if k in ("any", "all"):
            kids = list(n.kids)
            r.shuffle(kids)
            if k == "any":
                kids = kids[: min(r.randint(1, len(kids)), r.randint(1, 3))]
            out = []
            for c in kids:
                g = self.gen(c, depth + 1)
                if g is None:
                    if k == "all":
                        return None
                    continue
                out += g
            return out or None
        if k == "mult":
            lo = n.lo
            hi = min(n.hi, lo + 2)
            if n.kids[0].req and lo == 0:
                lo = 1
            cnt = r.randint(lo, int(hi))
            out = []
            for i in range(cnt):
                g = self.gen(n.kids[0], depth + 1)
                if g is None:
                    return None if i < n.lo else out
                if n.comma and i:
                    out.append(",")
                out += g
            return out
        return None


def join(parts):
    s = ""
    for p in parts:
        if p == ",":
            s += ","
        elif s and not s.endswith("("):
            s += " " + p
        else:
            s += p
    # Commas next to omitted parts are omitted too.
    s = s.replace(" ,", ",")
    while True:
        t = re.sub(r",\s*,", ",", s)
        t = re.sub(r"^\s*,\s*|\s*,\s*$", "", t)
        t = re.sub(r"\(\s*,\s*", "(", t)
        t = re.sub(r"\s*,\s*\)", ")", t)
        if t == s:
            return s
        s = t


def mutate(value, rnd):
    """A likely-invalid neighbour of `value`."""
    parts = value.split(" ")
    ops = ["drop", "dup", "swap", "junk", "unit", "comma", "neg", "trail"]
    op = rnd.choice(ops)
    if op == "drop" and len(parts) > 1:
        del parts[rnd.randrange(len(parts))]
    elif op == "dup":
        i = rnd.randrange(len(parts))
        parts.insert(i, parts[i])
    elif op == "swap" and len(parts) > 1:
        i = rnd.randrange(len(parts) - 1)
        parts[i], parts[i + 1] = parts[i + 1], parts[i]
    elif op == "junk":
        parts.insert(rnd.randrange(len(parts) + 1), rnd.choice(["foo", "1px", "red", "0", "1", "auto", "none", "/", ","]))
    elif op == "unit":
        v = re.sub(r"(\d)(px|em|%|s|deg|fr)?\b", lambda m: m.group(1) + rnd.choice(["s", "deg", "px", "", "%", "x"]), value, count=1)
        return v
    elif op == "comma":
        return value.replace(" ", ", ", 1) if " " in value else value + ", " + value
    elif op == "neg":
        return re.sub(r"(?<![\w.-])(\d)", r"-\1", value, count=1)
    elif op == "trail":
        return value + rnd.choice([" ,", " /", " 1px", " foo", " none", " auto"])
    return " ".join(parts)
