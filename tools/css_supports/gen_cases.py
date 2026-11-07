#!/usr/bin/env python3
"""Generate a `CSS.supports()` corpus (cases only; `record.py --corpus`
adds Chrome's answers).

For every property of the grammar file: values generated from its
grammar, mutations of them (likely invalid), generic edge values (CSS-wide
keywords, `!important`, empty, wrong types, `var()`), and the keywords
the grammar names. Then hand-written families (shorthands, math, colors,
substitution functions) and `@supports` conditions (`not`/`and`/`or`,
`selector()`, `font-tech()`, `font-format()`, `at-rule()`).

Usage:
    python3 tools/css_supports/gen_cases.py --out corpus.jsonl [--seed 1]
        [--per-prop 6] [--mutations 3] [--edges 8] [--grammar FILE]

Deterministic for a seed. The committed corpus,
`crates/lb-page/tests/css_supports/corpus.jsonl`, was made with `--seed 11`
and the defaults (cases follow the grammar file, so a later run differs),
plus the cases later batches found wrong, appended by hand; all answers
are Chrome's (`record.py --corpus`). Check a new batch with:

    python3 tools/css_supports/gen_cases.py --out /tmp/b.jsonl --seed N
    python3 tools/css_supports/record.py --chrome CHROME153 --corpus /tmp/b.jsonl
    LB_SUPPORTS_CORPUS=/tmp/b.jsonl cargo test -p lb-css --test supports_corpus
"""

import argparse
import json
import os
import random
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)
import grammar  # noqa: E402

GRAMMAR = os.path.join(ROOT, "crates", "lb-css", "src", "supports.grammar")

EDGES = ["inherit", "initial", "unset", "revert", "revert-layer", "INITIAL",
         "initial !important", "inherit!important", "", " ", "foo", "-foo", "1px", "0",
         "1", "-1", "1.5", "50%", "-5%", "red", "#f00", "none", "auto", "normal", "1s",
         "45deg", '"a"', "url(a.png)", "calc(1px)", "calc(1)", "calc(50%)", "var(--x)",
         "var(--x) foo", "env(x)", "attr(x)", "1px 1px", "foo foo", "inherit inherit",
         "initial 1px", "1px,", ",1px", "1px / 2px", "1foo", "1PX", "red !important",
         "foo()", "[a]", "{a}", "1px)", "(1px)", "1px;", "\\66 oo", "--a", "2fr", "1dppx",
         "1px 2px 3px 4px 5px", "default", "0 0", "1 1", "none none", "auto auto",
         "1e1", "+1", ".5", "1.", "1px 1px 1px", "foo, bar", "rgb(1 2 3)", "1hz",
         "linear-gradient(red, blue)", "counter(a)", "attr(x px)", "calc(1px + 1%)",
         "calc(1px * 2)", "calc(1s)", "calc(1deg)", "min(1px, 2%)", "max(1, 2)",
         "clamp(1px, 2px, 3px)", "if(style(--x: 1): 1px; else: 2px)"]

COLORS = ["red", "RED", "transparent", "currentcolor", "currentColor", "#f00", "#f008",
          "#ff000080", "#ff", "#12345", "#ggg", "rgb(1 2 3)", "rgb(1, 2, 3)", "rgb(1,2,3,0.5)",
          "rgb(1 2 3 / 50%)", "rgb(1% 2% 3%)", "rgb(1%, 2, 3)", "rgb(1 2% 3)", "rgb(none 2 3)",
          "rgb(1, none, 3)", "rgba(1 2 3)", "rgba(1, 2, 3)", "rgb(1 2)", "rgb(1 2 3 4)",
          "rgb(1 2 3 / 0.5 / 1)", "rgb(1,2 3)", "rgb(calc(1) 2 3)", "rgb(1px 2 3)",
          "hsl(1 2% 3%)", "hsl(1deg 2% 3%)", "hsl(1 2 3)", "hsl(1, 2%, 3%)", "hsl(1, 2, 3)",
          "hsla(1turn 2% 3% / .5)", "hsl(none none none)", "hwb(1 2% 3%)", "hwb(1, 2%, 3%)",
          "lab(50% 10 20)", "lab(50 10 20 / 0.5)", "lch(50% 10 20)", "lch(50% 10 20deg)",
          "oklab(0.5 0.1 0.1)", "oklch(70% 0.1 200)", "oklch(0.7 0.1 200 / 50%)",
          "oklch(70%, 0.1, 200)", "color(srgb 1 0 0)", "color(display-p3 1 0 0)",
          "color(rec2020 1 0 0 / 0.5)", "color(xyz 1 0 0)", "color(xyz-d50 1 0 0)",
          "color(xyz-d65 1 0 0)", "color(srgb-linear 1 0 0)", "color(a98-rgb 1 0 0)",
          "color(prophoto-rgb 1 0 0)", "color(foo 1 0 0)", "color(--foo 1 0 0)",
          "color(srgb 1 0)", "color(srgb 1 0 0 0)", "color-mix(in srgb, red, blue)",
          "color-mix(in oklch, red 10%, blue)", "color-mix(in oklch longer hue, red, blue 30%)",
          "color-mix(in hsl shorter hue, red, blue)", "color-mix(in srgb, red 0%, blue 0%)",
          "color-mix(red, blue)", "color-mix(in srgb, red)", "color-mix(in srgb, red, blue, green)",
          "color-mix(in foo, red, blue)", "color-mix(in srgb red, blue)",
          "color-mix(in srgb, red 120%, blue)", "color-mix(in lab, red 50%, blue 50%)",
          "color-mix(in xyz, red, blue)", "color-mix(in srgb longer hue, red, blue)",
          "light-dark(red, blue)", "light-dark(red)", "light-dark(red, blue, green)",
          "contrast-color(red)", "contrast-color(red max)", "rgb(from red r g b)",
          "rgb(from red calc(r + 1) g b / alpha)", "rgb(from red r g)", "hsl(from red h s l)",
          "oklch(from red l c h)", "oklch(from red l c calc(h + 10))", "lab(from red l a b / 0.5)",
          "color(from red srgb r g b)", "color(from red display-p3 r g b / alpha)",
          "rgb(from red x y z)", "rgb(from rgb(1 2 3) r g b)", "alpha(from red / 0.5)",
          "hwb(from red h w b)", "lch(from red l c h)", "rgb(from red 1 2 3)",
          "-webkit-link", "-webkit-text", "-webkit-activelink", "-webkit-focus-ring-color",
          "Canvas", "CanvasText", "AccentColor", "ActiveBorder", "ButtonFace", "Mark",
          "infotext", "device-cmyk(0 0 0 1)", "rgb(1 2 3)) ", "rgb(1 2 3", "red blue",
          "rgb(1e1 2 3)", "rgb(-1 2 3)", "rgb(300 2 3)", "hsl(1 -2% 3%)", "oklch(-1 0 0)",
          "lab(50% -200 20)", "rgb(1 2 3 / -1)", "rgb(1 2 3 / 2)", "rgb(1 2 3 / none)",
          "rgb( 1 2 3 )", "RGB(1 2 3)", "rgb(1  2  3)", "rgb(1/*c*/2 3)", "rgb(1 2 3/0.5)",
          "rgb(1 2 3 /0.5)", "rgb(calc(1 + 1) 2 3)", "rgb(calc(1% + 1%) 2 3)",
          "hsl(calc(1deg + 1deg) 2% 3%)", "rgb(sign(1px) 2 3)", "rgb(var(--x) 2 3)",
          "color-mix(in srgb, currentcolor, transparent)",
          "color-mix(in display-p3, red, blue)", "color-mix(in oklab, red, blue 50%)"]

MATH = ["calc(1px)", "calc(1px + 2px)", "calc(1px+2px)", "calc(1px + 2%)", "calc(1px + 1)",
        "calc(1px - -2px)", "calc(1px -2px)", "calc(1px * 2)", "calc(2 * 1px)", "calc(1px * 1px)",
        "calc(1px / 2)", "calc(1px / 1px)", "calc(1px / 1px * 1px)", "calc(1px / 0)",
        "calc((1px + 2%) * 2)", "calc(((1px)))", "calc()", "calc(1px +)", "calc(+ 1px)",
        "calc(1em + 1rem + 1vw + 1vh + 1ch + 1ex)", "calc(1px + 1s)", "calc(1px + 1deg)",
        "calc(infinity * 1px)", "calc(-infinity * 1px)", "calc(NaN * 1px)", "calc(e * 1px)",
        "calc(pi * 1px)", "calc(foo * 1px)", "calc(1px * e)", "calc(0)", "calc(0px)",
        "calc(1)", "calc(50%)", "calc(1%)", "calc(1s)", "calc(1deg)", "calc(1fr)",
        "calc(1x)", "calc(1dppx)", "calc(1hz)", "-webkit-calc(1px + 2px)", "CALC(1px)",
        "min(1px, 2%)", "min(1px)", "min()", "min(1px, 2px, 3px)", "min(1px, 1)",
        "max(1px, 2%)", "max(1, 2)", "max(1px 2px)", "clamp(1px, 2px, 3px)",
        "clamp(1px, 2%, 3em)", "clamp(1px, 2px)", "clamp(none, 2px, 3px)",
        "clamp(1px, 2px, none)", "round(1.5px, 1px)", "round(up, 1.5px, 1px)",
        "round(down, 1.5, 1)", "round(nearest, 1px, 1px)", "round(to-zero, 1px, 1px)",
        "round(foo, 1px, 1px)", "round(1.5px)", "round(1.5)", "mod(5px, 2px)",
        "mod(5, 2)", "rem(5px, 2px)", "rem(5px, 2)", "abs(-1px)", "abs(-1)",
        "calc(sign(1px) * 1px)", "sign(1px)", "sign(-1)", "calc(sin(1deg) * 1px)",
        "sin(1deg)", "sin(1)", "cos(1rad)", "tan(1turn)", "asin(1)", "acos(0.5)",
        "atan(1)", "atan2(1px, 2px)", "atan2(1, 2)", "atan2(1px, 2)", "pow(2, 3)",
        "pow(2px, 3)", "sqrt(4)", "sqrt(4px)", "hypot(1px, 2px)", "hypot(1px, 2)",
        "hypot(3)", "log(2)", "log(2, 10)", "log(2px)", "exp(1)", "exp(1px)",
        "calc(progress(1px, 0px, 10px) * 1px)", "progress(1px, 0px, 10px)",
        "progress(1px from 0px to 10px)", "calc(sibling-index() * 1px)", "sibling-index()",
        "sibling-count()", "calc(sibling-count() * 1%)", "random(1px, 2px)",
        "calc-size(auto, size)", "calc-size(auto, size * 2)", "calc-size(fit-content, size + 1px)",
        "calc-size(1px, size)", "calc-size(any, size)", "calc(1px * 2 / 3 + 4px - 5px)",
        "calc(1px + (2px * (3 + 4)))", "calc(1px + calc(2px))", "calc(min(1px, 2%) + 1px)",
        "calc( 1px )", "calc(1px\n+\n2px)", "calc(1px/*a*/+/*b*/2px)", "calc(2 * 3)",
        "calc(1.5)", "calc(-1)", "calc(1e3px)", "calc(1px + 0)", "calc(0 + 1px)",
        "calc(1em * 0)", "calc(1 / 2px)", "calc(1px / 2px * 3)", "calc(1px * 2 * 1px / 1px)"]

SUBST = ["var(--x)", "var(--x,)", "var(--x, 1px)", "var(--x,1px)", "var(x)", "var()",
         "var(--x) var(--y)", "1px var(--x)", "var(--x) !", "var(--x)!important",
         "var(--x, var(--y))", "var(--x, var(y))", "var(--x, ))", "var(--x, [)",
         "var(--x, {a})", "var(--x, ;)", "var(--x, !)", "calc(var(--x) * 2)",
         "rgb(var(--x))", "foo(var(--x))", "VAR(--x)", "var( --x )", "var(--x y)",
         "var(--x, , )", "var(-x)", "var(--)", "var(--x) var(--x) foo bar",
         "env(foo)", "env(safe-area-inset-top)", "env(safe-area-inset-top, 1px)",
         "env(1)", "env()", "env(--x)", "env(x 1)", "env(x, )", "env(x y)", "env(a, b, c)",
         "attr(x)", "attr(x px)", "attr(x type(<length>))", "attr(x, 1px)", "attr()",
         "attr(1)", "attr(x string)", "attr(x %)", "attr(x type(*))", "attr(x raw-string)",
         "attr(data-x type(<color>), red)", "attr(x foo)", "attr(--x)", "attr(x|y)",
         "attr(*|x)", "if(style(--x: 1): 1px; else: 2px)", "if(style(--x: 1): 1px)",
         "if(else: 1px)", "if()", "if(foo)", "if(media(width > 1px): 1px)",
         "if(supports(display: grid): 1px; else: 2px)", "if(style(--x): 1px;)",
         "inherit(--x)", "inherit(--x, 1px)", "random(1px, 2px)", "--foo()", "--foo(1px)",
         "first-valid(1px)", "toggle(1px, 2px)"]

SELECTORS = """a | a b | a > b | a + b | a ~ b | a >> b | a || b | > a | a > | * | *|a | |a | ns|a
| a, b | .c | #i | [x] | [x=y] | [x="y"] | [x~=y] | [x|=y] | [x^=y] | [x$=y] | [x*=y] | [x=y i]
| [x=y s] | [x=y z] | [*|x] | [|x] | [x y] | [] | :hover | :HOVER | :hover() | :foo | ::foo
| ::before | :before | ::BEFORE | ::after::before | ::before:hover | ::marker | ::slotted(a)
| ::slotted(a b) | ::part(a) | ::part(a b) | ::part() | ::highlight(a) | ::highlight() | :host
| :host(a) | :host(a b) | :host-context(a) | :is(a, b) | :is() | :is(a, :foo) | :where(a)
| :not(a) | :not(a, b) | :not(a b) | :not() | :has(a) | :has(> a) | :has(+ a) | :has(~ a)
| :has(a, b) | :has(:has(a)) | :has(::before) | :has(:is(::before)) | :nth-child(2n+1)
| :nth-child(odd) | :nth-child(even) | :nth-child(2n + 1 of a) | :nth-child(2n+1 of a, b)
| :nth-child(foo) | :nth-child() | :nth-child(+ 2n) | :nth-child(- n) | :nth-child(-n+3)
| :nth-of-type(2n of a) | :nth-of-type(3) | :nth-last-child(2) | :nth-last-of-type(n)
| :lang(en) | :lang("en") | :lang(en, fr) | :lang() | :lang(*-CH) | :dir(ltr) | :dir(foo)
| :dir() | :state(x) | :state() | :focus-visible | :focus-within | :user-valid | :user-invalid
| :popover-open | :modal | :open | :closed | :fullscreen | :picture-in-picture | :playing
| :paused | :seeking | :buffering | :stalled | :muted | :volume-locked | :autofill
| :-webkit-autofill | :placeholder-shown | :read-only | :read-write | :defined | :blank
| :local-link | :target-within | :current | :current(a) | :past | :future | :any-link
| :-webkit-any(a, b) | :-webkit-any() | :-moz-any(a) | :matches(a) | :scope | :root | :empty
| :first-child | :only-child | :checked | :indeterminate | :default | :valid | :invalid
| :in-range | :out-of-range | :required | :optional | :heading | :heading(1) | :has-slotted
| :active-view-transition | :active-view-transition-type(a) | :active-view-transition-type()
| :target-current | :target-before | :target-after | :interest-source | :interest-target
| :xr-overlay | :window-inactive | :horizontal | :vertical | :start | :end | :increment
| ::selection | ::placeholder | ::file-selector-button | ::backdrop | ::cue | ::cue(a)
| ::cue-region | ::first-letter | ::first-line | ::spelling-error | ::grammar-error
| ::target-text | ::search-text | ::details-content | ::view-transition | ::view-transition-group(a)
| ::view-transition-group(*) | ::view-transition-group() | ::view-transition-old(a)
| ::view-transition-new(a) | ::view-transition-image-pair(a) | ::view-transition-group(a.b)
| ::view-transition-group(.b) | ::scroll-marker | ::scroll-marker-group | ::scroll-button(up)
| ::scroll-button(*) | ::scroll-button(foo) | ::column | ::checkmark | ::picker-icon
| ::picker(select) | ::picker(foo) | ::picker() | ::-webkit-scrollbar | ::-webkit-scrollbar-thumb
| ::-webkit-foo | ::-moz-selection | ::-webkit-input-placeholder | :-webkit-full-screen
| ::marker:hover | ::before::marker | ::after::marker | ::placeholder:hover
| a::before | a:hover::before | a::before b | & | & a | a & | &.c | a:is(&) | :is(a)::before
| a::before.c | a::before[x] | a::before#i | a::part(x):hover | a::slotted(b):hover
| :host::part(a) | ::slotted(a)::before | :root::view-transition-group(a) | a\\:b | \\61
| a/**/b | a  b | a\tb | a\nb | a> b | a >b | a,b,c | a,, b | a, | ,a | a.c.d#e[f]:hover::before
| *.c | *#i | *:hover | a:has(b:has(c)) | :not(:has(a)) | :is(:has(a)) | :where(:has(a))
| :has(:not(:has(a))) | :nth-child(1 of :has(a)) | :host(:has(a)) | a:hover:focus:active
| :is(a, ::before) | :where(::before) | :not(::before) | :is(::before) | :-internal-foo
| :-internal-autofill-selected | :-webkit-drag | input::-webkit-inner-spin-button
| :focus-visible:hover | :hover:not(.c) | :not(:not(a)) | :lang(\\*-CH)"""


CONDITIONS = ["(display: grid)", "display: grid", "(display: grid) and (color: red)",
              "(display: grid) or (color: foo)", "not (display: foo)", "not(display: foo)",
              "not (display: grid) and (color: red)", "(not (display: foo)) and (color: red)",
              "(display: grid) and (color: red) or (width: 1px)", "((display: grid))",
              "(((display: grid)))", "(display:grid)", "( display : grid )", "(DISPLAY: GRID)",
              "(display: grid !important)", "(display: grid!important)", "(display: grid !IMPORTANT)",
              "(display: grid ! important)", "(display: grid !important !important)",
              "(display: )", "(display:)", "(display)", "()", "", " ", "foo", "foo()", "(foo)",
              "(foo: bar)", "(--x: 1)", "(--x:)", "(--x: )", "(--x: {a})", "(--x: [)",
              "(--x)", "(color: red) and", "and (color: red)", "(color: red) and(color: red)",
              "(color: red)and (color: red)", "(color: red) AND (color: red)",
              "(color: red) or not (color: red)", "not not (color: red)", "not (not (color: red))",
              "(color: red) and foo(bar)", "(color: red) or foo(bar)", "foo(bar) or (color: red)",
              "not foo(bar)", "[color: red]", "{color: red}", "(color: red;)",
              "(color: red; width: 1px)", "(color: red))", "((color: red)", "(color: red",
              "(color: rgb(1 2 3))", "(color: var(--x))", "(width: calc(1px + 2%))",
              "font-tech(color-COLRv1)", "font-tech(color-colrv0)", "font-tech(variations)",
              "font-tech(palettes)", "font-tech(incremental)", "font-tech(features-opentype)",
              "font-tech(features-aat)", "font-tech(features-graphite)", "font-tech(color-svg)",
              "font-tech(color-sbix)", "font-tech(color-cbdt)", "font-tech(foo)", "font-tech()",
              "font-tech(variations, palettes)", "font-tech(variations palettes)",
              "font-tech(\"variations\")", "FONT-TECH(variations)", "font-tech( variations )",
              "font-format(woff2)", "font-format(woff)", "font-format(truetype)",
              "font-format(opentype)", "font-format(collection)", "font-format(embedded-opentype)",
              "font-format(svg)", "font-format(foo)", "font-format(\"woff2\")", "font-format()",
              "font-format(WOFF2)", "font-tech(variations) and font-format(woff2)",
              "not font-tech(foo)", "(font-tech(variations))", "at-rule(@container)",
              "at-rule(@media)", "at-rule(@supports)", "at-rule(@foo)", "at-rule(container)",
              "at-rule(@layer)", "at-rule(@property)", "at-rule(@scope)", "at-rule(@starting-style)",
              "at-rule(@position-try)", "at-rule(@view-transition)", "at-rule(@font-face)",
              "at-rule(@keyframes)", "at-rule(@-webkit-keyframes)", "at-rule(@page)",
              "at-rule(@counter-style)", "at-rule(@import)", "at-rule(@namespace)",
              "at-rule(@charset)", "at-rule(@function)", "at-rule(@mixin)", "at-rule(@apply)",
              "at-rule(@custom-media)", "at-rule(@font-feature-values)", "at-rule(@swash)",
              "at-rule(@top-left)", "at-rule(@route)", "at-rule(@container; x)",
              "at-rule(@media, @supports)", "at-rule()", "AT-RULE(@MEDIA)", "selector()",
              "selector(a) and (color: red)", "selector(a) or selector(::foo)",
              "not selector(::foo)", "(selector(a))", "SELECTOR(a)", "selector( a )",
              "selector(a b) and selector(a > b)", "font-tech(variations) or selector(a)",
              "(display: grid) and (not (display: foo))", "named-feature(foo)",
              "env(foo)", "media(width > 1px)", "style(--x: 1)", "supports(display: grid)",
              "(width: 1px) and ((color: red) or (color: foo))", "/* c */ (color: red)",
              "(color: red) /* c */", "(color:/**/red)", "(color: red)\n and\t(width: 1px)",
              "(color red)", "(color: red red)", "(1px: red)", "(\"color\": red)",
              "(color: red) and (color: red) and (color: red) and (color: red)",
              "((color: red) and (width: 1px)) or (foo: bar)", "(color: red) or ((foo))",
              "(color: red) or (foo bar)", "(color: red) or [foo]", "(color: red) or {foo}"]


def shorthand_values():
    """Hand-written values for shorthands (valid and invalid)."""
    S = {
        "font": ["12px Arial", "bold 12px Arial", "italic bold 12px/1.5 Arial, sans-serif",
                 "bold", "12px", "Arial", "12px/1.5", "small-caps 12px serif",
                 "normal normal normal 12px serif", "normal normal normal normal 12px serif",
                 "normal normal normal normal normal 12px serif", "condensed 12px serif",
                 "ultra-condensed 12px serif", "50% 12px serif", "oblique 10deg 12px serif",
                 "caption", "menu", "icon", "message-box", "small-caption", "status-bar",
                 "-webkit-small-control", "caption 12px", "12px/normal serif", "12px / 1 serif",
                 "12px/ serif", "100 12px serif", "1000 12px serif", "1001 12px serif",
                 "0 12px serif", "small serif", "larger serif", "12px serif,", "12px , serif",
                 "12px \"Font\"", "12px 'a' b", "12px a b c", "12px 1a", "12px inherit",
                 "12px serif inherit", "12px initial", "12px default", "12px a, default",
                 "small-caps small-caps 12px serif", "bold bold 12px serif", "italic italic 12px x",
                 "calc(12px) serif", "12px/calc(1 + 1) serif", "12px/1px serif", "12px/50% serif",
                 "math 12px serif", "12px system-ui", "-12px serif", "12px -webkit-body",
                 "12px emoji", "12px ui-monospace", "12px generic(fangsong)", "petite-caps 12px x",
                 "small-caps bold italic condensed 12px x", "12px \"a\" \"b\""],
        "background": ["red", "url(a.png)", "none", "red url(a.png) no-repeat",
                       "url(a.png) center / cover", "url(a.png) 10px 20px / 50% auto repeat-x",
                       "url(a.png), red", "red, url(a.png)", "url(a.png), url(b.png) red",
                       "center", "center / 10px", "/ 10px", "10px / 10px", "fixed", "border-box",
                       "border-box padding-box", "border-box padding-box content-box",
                       "linear-gradient(red, blue)", "red red", "repeat repeat", "repeat-x repeat",
                       "left top", "top left", "left 10px top 20px", "10px left", "bottom 10px right",
                       "center center", "left left", "url(a.png) text", "text", "local scroll",
                       "none, none", "none,", ", none", "red 1px", "1px 2px 3px", "1px 2px 3px 4px",
                       "left 10px top", "left top 10px", "url(a.png) url(b.png)", "border-area",
                       "-webkit-gradient(linear, left top, left bottom, from(red), to(blue))",
                       "image-set(\"a.png\" 1x)", "cross-fade(url(a.png), url(b.png))",
                       "url(a.png) center / 10px 20px 30px", "x-start", "repeat-block"],
        "border": ["1px solid red", "solid", "1px", "red", "thin dotted", "medium double blue",
                   "solid solid", "1px 2px solid", "red red", "none", "hidden", "1px solid red 1px",
                   "calc(1px) solid", "-1px solid", "thick groove currentcolor", "0", "0 none",
                   "solid 1px red", "red 1px solid", "1px solid #f00", "1em inset", "foo"],
        "grid": ["none", "auto-flow / 1fr", "auto-flow dense / 1fr 1fr", "1fr / auto-flow",
                 "1fr / auto-flow dense", "\"a\" 10px / 1fr", "[a] \"a\" 10px [b] / auto",
                 "auto / auto", "repeat(2, 1fr) / 1fr", "dense auto-flow / 1fr", "1fr",
                 "auto-flow 10px / 1fr", "subgrid / subgrid", "masonry / 1fr", "1fr / 1fr / 1fr",
                 "auto-flow / auto-flow", "\"a b\" \"c d\"", "row / 1fr", "1fr 1fr / 1fr 1fr"],
        "grid-template": ["none", "1fr / 1fr", "\"a\"", "\"a b\" \"c d\"", "\"a\" 10px",
                          "[a] \"a\" 10px [b]", "\"a\" / 1fr", "[a] \"a\" [b] \"c\" / 1fr 1fr",
                          "repeat(2, 10px) / repeat(auto-fill, minmax(10px, 1fr))", "subgrid",
                          "subgrid / 1fr", "1fr / subgrid [a]", "\"a\" repeat(2, 1fr)",
                          "auto 1fr / auto", "1fr", "/ 1fr", "1fr /", "\"a\" / repeat(2, 1fr)",
                          "\"a.\" \". b\"", "\"a\" \"b\" / [x] 1fr [y]", "[a b] 1fr / [c] 2fr",
                          "masonry / 1fr", "\"\""],
        "grid-template-columns": ["1fr", "1fr 2fr", "repeat(3, 1fr)", "repeat(auto-fill, 100px)",
                                  "repeat(auto-fit, minmax(100px, 1fr))", "[a] 1fr [b]", "subgrid",
                                  "subgrid [a] [b]", "subgrid repeat(2, [a])", "masonry", "none",
                                  "minmax(10px, auto)", "fit-content(100px)", "repeat(2, [a] 1fr)",
                                  "repeat(0, 1fr)", "repeat(-1, 1fr)", "repeat(2)", "-1fr",
                                  "minmax(1fr, 1fr)", "minmax(auto, 1fr)", "1fr repeat(auto-fill, 10px)",
                                  "repeat(auto-fill, 10px) repeat(auto-fill, 10px)",
                                  "repeat(auto-fill, 1fr)", "[a b c] 10px", "[] 10px", "[1] 10px",
                                  "[a] [b] 10px", "auto auto", "min-content max-content",
                                  "10px [a]", "calc(1px + 1fr)", "repeat(2, 1fr 2fr)", "foo"],
        "grid-area": ["a", "1", "1 / 2", "1 / 2 / 3 / 4", "1 / 2 / 3 / 4 / 5", "span 2",
                      "span a", "span 2 a", "a 2", "2 span", "span", "auto", "auto / auto",
                      "-1", "0", "span 0", "span -1", "a / b", "1 2", "span 1 / span 2"],
        "grid-row": ["1", "1 / 3", "span 2", "1 / span 2", "a / b", "auto", "1 / 2 / 3", "0"],
        "animation": ["foo 1s", "1s foo", "foo 1s ease-in 2s infinite alternate both paused",
                      "none", "foo", "1s", "1s 2s", "1s 2s 3s", "foo 1s, bar 2s", "foo,", "infinite",
                      "foo 1s steps(2) 3", "foo 1s linear(0, 1)", "foo 1s cubic-bezier(0, 0, 1, 1)",
                      "foo 1s cubic-bezier(0, 0, 1)", "foo 1s cubic-bezier(2, 0, 1, 1)",
                      "foo 1s scroll()", "foo 1s view()", "foo auto", "foo -1s", "foo 1s -1s",
                      "foo 1s 1.5", "foo 1s -1", "\"foo\" 1s", "none none", "ease ease",
                      "foo bar", "foo 1s running paused", "foo 1s forwards backwards",
                      "foo 1s --tl", "foo 1s normal", "normal foo", "inherit 1s", "default 1s"],
        "transition": ["all 1s", "opacity 1s ease", "1s", "none", "all 1s, color 2s",
                       "all 1s ease-in 2s", "all 1s 2s 3s", "all, none", "none, all",
                       "all 1s allow-discrete", "allow-discrete", "normal", "foo 1s", "-1s",
                       "1s -1s", "all ease ease", "all all", "color background 1s",
                       "1s step-end", "1s steps(2, jump-none)", "1s steps(1, jump-none)",
                       "1s steps(2, start)", "1s steps(0)", "1s linear(0, 0.5 50%, 1)",
                       "--x 1s", "inherit 1s", "initial", "1s, ", "1s,,1s", "all 1s ease-in-out"],
        "text-shadow": ["1px 1px red", "red 1px 1px", "1px 1px 2px red", "1px 1px", "1px",
                        "red", "none", "1px 1px 2px 3px red", "1px 1px, 2px 2px blue",
                        "inset 1px 1px", "1px 1px -2px", "red 1px 1px red", "1px red 1px",
                        "1px 1px 2px", "1px 1px calc(2px)", "1px 1px 1s", "none, 1px 1px"],
        "box-shadow": ["1px 1px red", "inset 1px 1px red", "1px 1px 2px 3px red", "1px 1px",
                       "red", "inset", "none", "1px 1px 2px 3px 4px", "1px 1px inset",
                       "1px 1px -2px", "1px 1px 2px -3px", "red 1px 1px inset", "inset red 1px 1px",
                       "1px inset 1px", "1px 1px, inset 2px 2px blue", "inset inset 1px 1px",
                       "1px", "1px 1px 1px 1px 1px", "none, 1px 1px", "1px 1px red red"],
        "mask": ["none", "url(a.svg)", "url(a.svg) no-repeat", "url(a.svg) center / contain",
                 "linear-gradient(red, transparent)", "url(a.svg) alpha", "url(a.svg) luminance",
                 "url(a.svg) add", "url(a.svg) exclude", "url(a.svg) border-box content-box",
                 "url(a.svg) no-clip", "url(a.svg), url(b.svg)", "alpha alpha", "foo",
                 "url(a.svg) fill-box", "url(a.svg) view-box", "1px", "url(a.svg) 1px 2px / 3px"],
        "inset": ["0", "1px", "1px 2px", "1px 2px 3px", "1px 2px 3px 4px", "1px 2px 3px 4px 5px",
                  "auto", "auto 1px", "10%", "anchor(top)", "anchor(--a top)", "anchor(foo)",
                  "anchor(top, 1px)", "anchor-size(width)", "-1px", "1px auto 2% 0", "red"],
        "place-items": ["center", "center start", "start end", "baseline", "first baseline",
                        "last baseline center", "normal", "stretch", "center center center",
                        "legacy", "legacy left", "anchor-center", "safe center", "unsafe end",
                        "left", "right", "self-start", "flex-end", "foo", "center legacy"],
        "place-content": ["center", "center start", "space-between", "space-between center",
                          "stretch", "baseline", "first baseline", "left", "left right",
                          "safe center unsafe end", "normal", "start", "foo", "center 1px"],
        "place-self": ["auto", "center", "auto center", "start end", "anchor-center", "stretch",
                       "baseline", "last baseline", "left", "foo", "center center center"],
        "margin": ["0", "1px", "1px 2px", "1px 2px 3px", "1px 2px 3px 4px", "1px 2px 3px 4px 5px",
                   "auto", "-1px", "10%", "1px auto", "anchor-size(width)", "foo", "1px,2px"],
        "padding": ["0", "1px", "1px 2px 3px 4px", "-1px", "auto", "10%", "1px 2px 3px 4px 5px"],
        "border-radius": ["1px", "1px 2px", "1px / 2px", "1px 2px 3px 4px / 5px 6px 7px 8px",
                          "1px / 2px / 3px", "50%", "-1px", "1px /", "/ 1px",
                          "1px 2px 3px 4px 5px"],
        "border-image": ["url(a.png) 30", "url(a.png) 30 round", "url(a.png) 30 / 10px",
                         "url(a.png) 30 / 10px / 5px", "url(a.png) 30 fill", "none",
                         "url(a.png) 30 stretch repeat", "url(a.png) / 10px", "30 url(a.png)",
                         "url(a.png) 30% 20 10 5", "linear-gradient(red, blue) 30"],
        "flex": ["1", "1 1", "1 1 0", "1 1 auto", "auto", "none", "initial", "1 1 1 1",
                 "0 0 100px", "100px", "1 100px", "100px 1", "-1", "1 -1", "content", "1 content"],
        "flex-flow": ["row", "row wrap", "wrap row", "column wrap-reverse", "row row", "wrap wrap"],
        "columns": ["auto", "100px", "3", "100px 3", "3 100px", "auto auto", "auto 3",
                    "3 3", "100px 100px", "-1", "0", "100px 3 4"],
        "outline": ["1px solid red", "auto", "solid", "red", "1px", "auto 1px", "auto red",
                    "invert", "1px solid red red", "hidden"],
        "list-style": ["disc", "none", "inside", "url(a.png)", "disc inside url(a.png)",
                       "none none", "none disc", "\"-\"", "symbols(cyclic \"*\")", "foo",
                       "square outside"],
        "text-decoration": ["underline", "underline red", "underline dotted red 1px",
                            "underline overline", "none", "none underline", "red", "1px",
                            "from-font", "underline wavy", "blink", "spelling-error",
                            "underline underline"],
        "offset": ["none", "path(\"M0 0 L1 1\")", "ray(45deg)", "10px 20px", "auto",
                   "path(\"M0 0\") 10px", "path(\"M0 0\") 10px auto 45deg",
                   "ray(45deg closest-side contain)", "url(#a)", "circle()", "10px / auto"],
        "overflow": ["hidden", "hidden scroll", "clip", "overlay", "auto auto", "visible visible visible"],
        "gap": ["1px", "1px 2px", "normal", "normal 1px", "1px 2px 3px", "-1px", "10%"],
        "aspect-ratio": ["16 / 9", "16/9", "auto", "auto 16/9", "16 / 9 auto", "1", "0",
                         "-1", "1 / 0", "16 / ", "/ 9", "auto auto"],
        "container": ["a", "a / size", "a b / inline-size", "none", "/ size", "a / foo",
                      "a / scroll-state", "a / size scroll-state", "none / size"],
        "contain-intrinsic-size": ["10px", "auto 10px", "10px 20px", "auto 10px auto 20px",
                                   "none", "auto none", "auto", "-1px"],
        "scroll-timeline": ["--a", "--a block", "--a x", "none", "--a, --b y", "a"],
        "view-timeline": ["--a", "--a block", "--a 10px 20px", "--a auto", "--a x auto"],
        "animation-range": ["normal", "cover", "entry 10%", "entry 10% exit 90%", "10% 90%",
                            "contain 0% contain 100%", "foo"],
        "position-try": ["none", "flip-block", "--a", "most-width --a", "flip-block, --a",
                         "top", "normal flip-block"],
        "text-emphasis": ["dot", "filled dot", "open circle red", "\"x\"", "red", "none"],
        "column-rule": ["1px solid red", "solid", "thin", "red"],
        "-webkit-text-stroke": ["1px red", "1px", "red", "thin", "-1px"],
        "-webkit-box-reflect": ["below", "above 10px", "left 1px linear-gradient(red, blue)",
                                "none", "below 10px url(a.png) 30 round"],
        "font-variant": ["small-caps", "normal", "none", "common-ligatures small-caps",
                         "historical-forms", "stylistic(a)", "oldstyle-nums tabular-nums",
                         "small-caps small-caps", "full-width ruby", "sub", "emoji", "unicode"],
        "font-synthesis": ["none", "weight", "style", "weight style small-caps position", "foo"],
        "text-wrap": ["wrap", "nowrap", "balance", "pretty", "stable", "wrap balance", "auto"],
        "white-space": ["normal", "pre", "nowrap", "pre-wrap", "break-spaces", "pre-line",
                        "collapse", "preserve nowrap", "wrap collapse", "preserve-breaks"],
        "transform": ["none", "rotate(45deg)", "rotate(0)", "translate(1px, 2px)",
                      "translate(1px 2px)", "scale(2) rotate(1deg)", "matrix(1,0,0,1,0,0)",
                      "matrix(1, 0, 0, 1, 0)", "translateX(10%)", "translateZ(10%)",
                      "translate3d(1px, 2px, 3px)", "perspective(10px)", "perspective(none)",
                      "skew(10deg, 20deg)", "rotate3d(1, 1, 1, 45deg)", "scale(50%)",
                      "rotate(45)", "foo(1)", "none rotate(1deg)", "rotate(1deg),scale(2)",
                      "scaleX(2) scaleY(2)", "matrix3d(1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1)"],
        "filter": ["blur(2px)", "blur()", "blur(-1px)", "brightness(50%)", "drop-shadow(1px 1px red)",
                   "url(#a)", "grayscale(1) sepia(1)", "hue-rotate(0)", "hue-rotate(45deg)",
                   "opacity(2)", "saturate(-1)", "none", "contrast()", "invert(1) url(a.svg#f)"],
        "clip-path": ["none", "circle(50%)", "circle()", "ellipse(10px 20px at 1px 2px)",
                      "inset(10px round 5px)", "polygon(0 0, 1px 1px, 0 1px)",
                      "polygon(evenodd, 0 0, 1px 1px)", "path(\"M0 0\")", "url(#a)",
                      "border-box", "circle(50%) border-box", "xywh(0 0 10px 10px)",
                      "rect(0 10px 10px 0)", "shape(from 0 0, line to 10px 10px)", "foo"],
        "content": ["\"a\"", "none", "normal", "counter(a)", "counters(a, \".\")", "attr(x)",
                    "open-quote", "url(a.png)", "\"a\" / \"alt\"", "\"a\" counter(b) \"c\"",
                    "linear-gradient(red, blue)", "foo", "1px", "image-set(\"a.png\" 1x)"],
        "cursor": ["pointer", "url(a.cur), auto", "url(a.cur) 1 2, pointer", "url(a.cur)",
                   "foo", "-webkit-grab", "-webkit-zoom-in", "image-set(\"a.png\" 1x), auto"],
        "font-family": ["Arial", "Arial, sans-serif", "\"Times New Roman\"", "Times New Roman",
                        "1a", "a 1", "serif, ", ", serif", "inherit", "a, inherit", "default",
                        "a, default", "initial a", "-webkit-body", "system-ui", "math",
                        "generic(kai)", "a, \"b\", c d e", "\\31 a", "unset, a"],
        "font-feature-settings": ["normal", "\"liga\"", "\"liga\" 1", "\"liga\" on", "\"liga\" off",
                                  "\"lig\"", "\"ligaa\"", "liga", "\"liga\" 1, \"kern\" 0",
                                  "\"liga\" -1", "\"liga\" 1.5", "\"liga\", \"kern\""],
        "font-variation-settings": ["normal", "\"wght\" 400", "\"wght\" 400, \"wdth\" 100",
                                    "\"wght\"", "\"wgh\" 1", "\"wght\" 1.5", "wght 400"],
        "will-change": ["auto", "transform", "transform, opacity", "auto, transform", "foo",
                        "contents", "scroll-position", "will-change", "initial", "1px"],
        "counter-reset": ["a", "a 1", "a 1 b 2", "none", "a -1", "a 1.5", "reversed(a)",
                          "reversed(a) 2", "1", "a a", "none a"],
        "quotes": ["auto", "none", "\"a\" \"b\"", "\"a\"", "\"a\" \"b\" \"c\" \"d\"", "match-parent"],
        "shape-outside": ["none", "circle(50%)", "url(a.png)", "margin-box", "inset(1px) margin-box",
                          "linear-gradient(red, blue)"],
        "transition-timing-function": ["ease", "linear", "steps(2, end)", "steps(2, jump-both)",
                                       "cubic-bezier(0.1, -1, 0.2, 2)", "linear(0, 1)",
                                       "linear(0 0%, 1 100%)", "linear(0, 0.5 25% 75%, 1)",
                                       "linear(0)", "linear()", "step-start, ease"],
        "image-rendering": ["auto", "pixelated", "crisp-edges", "-webkit-optimize-contrast", "smooth"],
        "line-height": ["1", "1.5", "normal", "1px", "50%", "-1", "calc(1 + 1px)", "calc(1 + 1)"],
        "z-index": ["1", "-1", "auto", "1.5", "calc(1.5)", "1e1", "calc(1px / 1px)"],
        "opacity": ["0.5", "50%", "calc(50%)", "-1", "2", "1px", "calc(0.5 + 10%)"],
        "rotate": ["45deg", "0", "x 45deg", "1 0 0 45deg", "45deg x", "none", "0deg"],
        "scale": ["1", "1 2", "1 2 3", "50%", "none", "1 2 3 4"],
        "translate": ["1px", "1px 2px", "1px 2px 3px", "10%", "1px 2px 3%", "none"],
        "anchor-name": ["--a", "--a, --b", "none", "a", "--a --b"],
        "position-area": ["top", "top left", "span-all", "center center", "top bottom",
                          "block-start inline-end", "x-start", "span-x-start span-y-end", "foo"],
        "text-box": ["normal", "trim-both cap alphabetic", "trim-start", "cap", "ex text", "auto"],
        "dynamic-range-limit": ["standard", "high", "constrained", "no-limit",
                                "dynamic-range-limit-mix(standard 50%, high 50%)"],
        "display": ["grid", "inline grid", "block flow-root", "list-item block", "flow list-item",
                    "inline list-item flow", "contents", "ruby", "ruby-text", "math", "-webkit-box",
                    "-webkit-inline-box", "inline-flex", "grid-lanes", "masonry", "inline-masonry",
                    "block block", "table-cell", "foo", "none none", "run-in", "flex flex"],
    }
    return S


def keywords(n, out):
    if n is None:
        return
    if n.kind == "kw":
        out.add(n.val)
    for k in n.kids:
        keywords(k, out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--per-prop", type=int, default=6)
    ap.add_argument("--mutations", type=int, default=3)
    ap.add_argument("--edges", type=int, default=8, help="edge values per property (random subset)")
    ap.add_argument("--all-keywords", action="store_true", help="every grammar keyword alone")
    ap.add_argument("--grammar", default=GRAMMAR)
    ap.add_argument("--props", help="only these properties (comma separated)")
    ap.add_argument("--vocab", type=int, default=4,
                    help="keywords of vocab.txt and sample calls of funcs.txt per property, "
                    "alone and next to other components (catches values the grammar lacks)")
    a = ap.parse_args()
    rnd = random.Random(a.seed)
    props, types = grammar.load(a.grammar)
    g = grammar.Gen(props, types, rnd)
    seen, cases = set(), []

    def add(*c):
        if c not in seen:
            seen.add(c)
            cases.append(list(c))

    vocab = sorted(set(open(os.path.join(HERE, "vocab.txt")).read().split()))
    funcs = [l.split("\t")[1] for l in open(os.path.join(HERE, "funcs.txt")).read().strip().split("\n")]
    ctx = [lambda v: v, lambda v: v + " " + v, lambda v: "1px 1px " + v, lambda v: v + " 1px",
           lambda v: "a " + v, lambda v: v + " a", lambda v: '"abcd" ' + v, lambda v: v + " 1",
           lambda v: v + ", auto", lambda v: "auto " + v, lambda v: v + ", " + v]
    names = sorted(props)
    if a.props:
        names = [n for n in names if n in a.props.split(",")]
    for p in names:
        vals = []
        for _ in range(a.per_prop * 3):
            v = g.prop(p)
            if v is not None:
                s = grammar.join(v)
                if s and s not in vals:
                    vals.append(s)
            if len(vals) >= a.per_prop:
                break
        for v in vals:
            add(p, v)
        for _ in range(a.mutations):
            if vals:
                add(p, grammar.mutate(rnd.choice(vals), rnd))
        for v in rnd.sample(EDGES, min(a.edges, len(EDGES))):
            add(p, v)
        kws = set()
        keywords(g.node(("p", p), props.get(p)), kws)
        kws = sorted(kws)
        if not a.all_keywords:
            kws = rnd.sample(kws, min(3, len(kws)))
        for k in kws:
            add(p, k)
        for v in rnd.sample(vocab, a.vocab) + rnd.sample(funcs, max(1, a.vocab // 2)):
            add(p, rnd.choice(ctx)(v))
    if not a.props:
        for p, vals in sorted(shorthand_values().items()):
            for v in vals:
                add(p, v)
        color_props = ["color", "background-color", "border-top-color", "outline-color",
                       "caret-color", "accent-color", "fill", "stroke", "text-decoration-color",
                       "column-rule-color", "-webkit-text-fill-color", "scrollbar-color",
                       "flood-color", "lighting-color", "stop-color", "text-emphasis-color"]
        for v in COLORS:
            add("color", v)
        for p in color_props[1:]:
            for v in rnd.sample(COLORS, 12):
                add(p, v)
        add("scrollbar-color", "red blue")
        add("scrollbar-color", "auto")
        add("scrollbar-color", "red")
        math_props = ["width", "height", "margin-left", "padding-top", "top", "font-size",
                      "line-height", "opacity", "z-index", "rotate", "transition-duration",
                      "flex-grow", "order", "border-top-width", "letter-spacing", "column-count",
                      "tab-size", "font-weight", "aspect-ratio", "image-resolution",
                      "grid-template-columns", "border-radius", "translate", "scale",
                      "animation-iteration-count", "outline-offset", "word-spacing",
                      "text-indent", "flex-basis", "min-width", "max-height", "inset"]
        for v in MATH:
            add("width", v)
        for p in math_props[1:]:
            for v in rnd.sample(MATH, 25):
                add(p, v)
        for p in ["width", "color", "display", "margin", "font", "background", "transform",
                  "grid-template-columns", "z-index", "animation", "--x", "content"]:
            for v in SUBST:
                add(p, v)
        for n in ["display", "DISPLAY", "Display", "-webkit-display", "--x", "--", "-x", "x",
                  "", " display", "display ", "-WEBKIT-TRANSFORM", "webkitTransform",
                  "-webkit-flex", "-moz-transform", "-ms-transform", "-o-transform",
                  "zoom", "-epub-writing-mode", "-webkit-zoom", "font-display", "src",
                  "unicode-range", "size", "page", "syntax", "inherits", "initial-value",
                  "ascent-override", "line-gap-override", "font-named-instance", "navigation",
                  "result", "-internal-visited-color", "all", "--X", "--é", "-\\-x"]:
            add(n, "grid")
            add(n, "initial")
        for v in ["foo", "", " ", "{}", "{a}", "[a]", "(a)", "a;", "a!", "!important", "a !important",
                  "\"", "'a", "url(a", ")", "]", "}", "a)", "1px foo()", "calc(", "--y", "!",
                  "a b c", "\\", "/* c */", "a /* c */ b", "é", "@media", "#", "a{b}"]:
            add("--x", v)
        for s in [x.strip() for x in SELECTORS.split("|")]:
            add("selector(%s)" % s)
        for c in CONDITIONS:
            add(c)
        # Conditions made of declarations from the corpus.
        decls = [c for c in cases if len(c) == 2 and not c[0].startswith("-")]
        for c in rnd.sample(decls, min(300, len(decls))):
            add("(%s: %s)" % (c[0], c[1]))
        for _ in range(100):
            x, y = rnd.sample(decls, 2)
            op = rnd.choice(["and", "or", "and not", "or not"])
            add("(%s: %s) %s (%s: %s)" % (x[0], x[1], op, y[0], y[1]))
    with open(a.out, "w", encoding="utf-8") as f:
        for c in cases:
            f.write(json.dumps(c, ensure_ascii=False) + "\n")
    print("%d cases" % len(cases), file=sys.stderr)


if __name__ == "__main__":
    main()
