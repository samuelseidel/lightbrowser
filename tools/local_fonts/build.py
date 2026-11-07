#!/usr/bin/env python3
r"""Builds profiles/local_fonts/<os>_<version>.json (see its README.md).

Inputs (none of them is checked in):
  --mac26    names local Chrome on macOS 26 loaded with local() (record.mjs)
  --fc-list  `fc-list --format '%{file}\\t%{family}\\t%{fullname}\\t%{postscriptname}\\t%{style}\\n'`
             on the same Mac (maps each loaded name to its family)
  --apple    directory with Apple's "Fonts included with macOS" pages:
             HT213266.html (13), 108939.html (14), 120414.html (15), 122869.html (26)
  --ms       directory with Microsoft's font list pages:
             windows_10_font_list.html, windows_11_font_list.html

  --ubuntu24, --ubuntu24-fc, --ubuntu24-pkgs
             the same recording on Ubuntu 24.04 (rtx-dev) and its package files:
             for p in <packages>; do dpkg -L $p | grep -E '\.(ttf|otf|ttc)$' | ...; done
Fedora lists are written from the package names below (no Fedora machine
was available); see README.md.
"""

import argparse
import html
import json
import re
from pathlib import Path

FORMAT = 2


def write(out: Path, ident: str, os_label: str, source: str, families: dict):
    fams = {f: sorted(set(n), key=str.lower) for f, n in sorted(families.items(), key=lambda x: x[0].lower()) if n}
    doc = {"format": FORMAT, "os": os_label, "source": source, "families": fams}
    text = json.dumps(doc, ensure_ascii=False, indent=0, sort_keys=False)
    (out / f"{ident}.json").write_text(text + "\n", encoding="utf-8")
    print(ident, len(fams), "families", sum(len(v) for v in fams.values()), "names")


# ---------------------------------------------------------------- macOS


def mac26_families(loaded: list, fc_rows: list) -> dict:
    """Groups the names Chrome loaded by the family fontconfig reports."""
    by_name = {}
    for r in fc_rows:
        if len(r) < 5:
            continue
        _, fams, full, ps, _ = r[:5]
        fam = fams.split(",")[0]
        for n in full.split(",") + [ps]:
            if n:
                by_name.setdefault(n.lower(), fam)
    # Variable-font instances and FontServices' reserved fonts (PingFang):
    # CoreText lists them, fontconfig does not.
    prefix = [
        ("GB18030", "GB18030 Bitmap"),
        ("ITF Devanagari Marathi", "ITF Devanagari Marathi"),
        ("Kefa III", "Kefa III"),
        ("Noto Sans Syriac", "Noto Sans Syriac"),
        ("NotoSansSyriac", "Noto Sans Syriac"),
        ("PingFang HK", "PingFang HK"),
        ("PingFangHK", "PingFang HK"),
        ("PingFang MO", "PingFang MO"),
        ("PingFangMO", "PingFang MO"),
        ("PingFang SC", "PingFang SC"),
        ("PingFangSC", "PingFang SC"),
        ("PingFang TC", "PingFang TC"),
        ("PingFangTC", "PingFang TC"),
        ("STIX Two Text", "STIX Two Text"),
        ("STIXTwoText", "STIX Two Text"),
        ("SignPainter", "SignPainter"),
        ("Skia", "Skia"),
    ]
    out = {}
    for n in loaded:
        fam = by_name.get(n.lower())
        if fam is None:
            fam = next((f for p, f in prefix if n.startswith(p)), None)
        if fam is None:
            raise SystemExit(f"no family for {n!r}")
        out.setdefault(fam, []).append(n)
    return out


def apple_full_names(page: str) -> set:
    """Lowercase lines of both lists (installed/downloadable and
    document-support) with runs of spaces collapsed."""
    s = Path(page).read_text(encoding="utf-8")
    a = [m.start() for m in re.finditer(r"Installed or downloadable fonts</h2>", s)][-1]
    c = s.find("Learn more</h2>", a)
    lines = set()
    for m in re.findall(r'<li role="listitem" class="gb-list_item"><p class="gb-paragraph">(.*?)</p></li>', s[a:c], re.S):
        lines.add(" ".join(html.unescape(re.sub("<[^>]+>", "", m)).lower().split()))
    return lines


def apple_covers(lines: set, names: list) -> set:
    """Recorded full names that one of Apple's lines names (the line is the
    name, optionally followed by a version)."""
    names = sorted({n.lower() for n in names}, key=len, reverse=True)
    out = set()
    ver = re.compile(r"(version|unknown|\(|v?\d)")
    for line in lines:
        for n in names:
            if line == n or (line.startswith(n + " ") and ver.match(line[len(n) + 1 :])):
                out.add(n)
                break
    return out


def mac_older(fams26: dict, apple: Path) -> dict:
    """macOS 13-15 from the recorded macOS 26 set: drop the families Apple's
    list for that version lacks, add the ones Apple later replaced."""
    names = [n for v in fams26.values() for n in v]
    pages = {13: "HT213266.html", 14: "108939.html", 15: "120414.html", 26: "122869.html"}
    cov = {v: apple_covers(apple_full_names(apple / p), names) for v, p in pages.items()}
    out = {}
    for v in (13, 14, 15):
        gone = sorted(cov[26] - cov[v])
        fams = {}
        for f, ns in fams26.items():
            # A family goes when Apple's list for v names none of its faces
            # that the macOS 26 list names.
            listed26 = [n for n in ns if n.lower() in cov[26]]
            # Apple's 14 and 15 lists name it "Noto Sans Mongolian" (no style).
            renamed = f == "Noto Sans Mongolian"
            if listed26 and not renamed and all(n.lower() in gone for n in listed26):
                continue
            fams[f] = list(ns)
        # Faces Apple replaced by macOS 26 (from the older lists).
        fams["Kefa"] = ["Kefa Regular", "Kefa-Regular", "Kefa Bold", "Kefa-Bold"]
        if v < 15:
            # Static Noto Sans Syriac before the variable one (macOS 15).
            fams["Noto Sans Syriac"] = ["Noto Sans Syriac Regular", "NotoSansSyriac-Regular"]
        out[v] = (fams, gone)
    return out


# ---------------------------------------------------------------- Windows

# PostScript names (name ID 6) of the default-install faces. Microsoft's
# lists give family, full name and file only; these are the fonts' own
# names (the ones shipped with macOS were checked with fc-list). Faces left
# out (variable-font instances, some CJK fonts) are matched by full name.
WIN_PS = {
    "Arial": "ArialMT", "Arial Italic": "Arial-ItalicMT", "Arial Bold": "Arial-BoldMT",
    "Arial Bold Italic": "Arial-BoldItalicMT", "Arial Black": "Arial-Black", "Bahnschrift": "Bahnschrift",
    "Calibri Light": "Calibri-Light", "Calibri Light Italic": "Calibri-LightItalic", "Calibri": "Calibri",
    "Calibri Italic": "Calibri-Italic", "Calibri Bold": "Calibri-Bold", "Calibri Bold Italic": "Calibri-BoldItalic",
    "Cambria": "Cambria", "Cambria Italic": "Cambria-Italic", "Cambria Bold": "Cambria-Bold",
    "Cambria Bold Italic": "Cambria-BoldItalic", "Cambria Math": "CambriaMath",
    "Candara Light": "Candara-Light", "Candara Light Italic": "Candara-LightItalic", "Candara": "Candara",
    "Candara Italic": "Candara-Italic", "Candara Bold": "Candara-Bold", "Candara Bold Italic": "Candara-BoldItalic",
    "Comic Sans MS": "ComicSansMS", "Comic Sans MS Italic": "ComicSansMS-Italic", "Comic Sans MS Bold": "ComicSansMS-Bold",
    "Comic Sans MS Bold Italic": "ComicSansMS-BoldItalic", "Consolas": "Consolas", "Consolas Italic": "Consolas-Italic",
    "Consolas Bold": "Consolas-Bold", "Consolas Bold Italic": "Consolas-BoldItalic", "Constantia": "Constantia",
    "Constantia Italic": "Constantia-Italic", "Constantia Bold": "Constantia-Bold", "Constantia Bold Italic": "Constantia-BoldItalic",
    "Corbel Light": "Corbel-Light", "Corbel Light Italic": "Corbel-LightItalic", "Corbel": "Corbel",
    "Corbel Italic": "Corbel-Italic", "Corbel Bold": "Corbel-Bold", "Corbel Bold Italic": "Corbel-BoldItalic",
    "Courier New": "CourierNewPSMT", "Courier New Italic": "CourierNewPS-ItalicMT", "Courier New Bold": "CourierNewPS-BoldMT",
    "Courier New Bold Italic": "CourierNewPS-BoldItalicMT", "Ebrima": "Ebrima", "Ebrima Bold": "Ebrima-Bold",
    "Franklin Gothic Medium": "FranklinGothic-Medium", "Franklin Gothic Medium Italic": "FranklinGothic-MediumItalic",
    "Gabriola": "Gabriola", "Gadugi": "Gadugi", "Gadugi Bold": "Gadugi-Bold", "Georgia": "Georgia",
    "Georgia Italic": "Georgia-Italic", "Georgia Bold": "Georgia-Bold", "Georgia Bold Italic": "Georgia-BoldItalic",
    "HoloLens MDL2 Assets": "HoloLensMDL2Assets", "Impact": "Impact", "Ink Free": "InkFree",
    "Javanese Text": "JavaneseText", "Leelawadee UI": "LeelawadeeUI", "Leelawadee UI Semilight": "LeelawadeeUI-Semilight",
    "Leelawadee UI Bold": "LeelawadeeUI-Bold", "Lucida Console": "LucidaConsole", "Lucida Sans Unicode": "LucidaSansUnicode",
    "Marlett": "Marlett", "Microsoft Himalaya": "MicrosoftHimalaya", "Microsoft New Tai Lue": "MicrosoftNewTaiLue",
    "Microsoft New Tai Lue Bold": "MicrosoftNewTaiLue-Bold", "Microsoft PhagsPa": "MicrosoftPhagsPa",
    "Microsoft PhagsPa Bold": "MicrosoftPhagsPa-Bold", "Microsoft Sans Serif": "MicrosoftSansSerif",
    "Microsoft Tai Le": "MicrosoftTaiLe", "Microsoft Tai Le Bold": "MicrosoftTaiLe-Bold", "Microsoft Yi Baiti": "MicrosoftYiBaiti",
    "MingLiU-ExtB": "MingLiU-ExtB", "PMingLiU-ExtB": "PMingLiU-ExtB", "MingLiU_HKSCS-ExtB": "MingLiU_HKSCS-ExtB",
    "Mongolian Baiti": "MongolianBaiti", "MS Gothic": "MS-Gothic", "MS PGothic": "MS-PGothic", "MS UI Gothic": "MS-UIGothic",
    "MV Boli": "MVBoli", "Myanmar Text": "MyanmarText", "Myanmar Text Bold": "MyanmarText-Bold",
    "Nirmala UI Semilight": "NirmalaUI-Semilight", "Nirmala UI": "NirmalaUI", "Nirmala UI Bold": "NirmalaUI-Bold",
    "Palatino Linotype": "PalatinoLinotype-Roman", "Palatino Linotype Italic": "PalatinoLinotype-Italic",
    "Palatino Linotype Bold": "PalatinoLinotype-Bold", "Palatino Linotype Bold Italic": "PalatinoLinotype-BoldItalic",
    "Segoe MDL2 Assets": "SegoeMDL2Assets", "Segoe Print": "SegoePrint", "Segoe Print Bold": "SegoePrint-Bold",
    "Segoe Script": "SegoeScript", "Segoe Script Bold": "SegoeScript-Bold", "Segoe UI Light": "SegoeUI-Light",
    "Segoe UI Light Italic": "SegoeUI-LightItalic", "Segoe UI Semilight": "SegoeUI-Semilight",
    "Segoe UI Semilight Italic": "SegoeUI-SemilightItalic", "Segoe UI": "SegoeUI", "Segoe UI Italic": "SegoeUI-Italic",
    "Segoe UI Semibold": "SegoeUI-Semibold", "Segoe UI Semibold Italic": "SegoeUI-SemiboldItalic", "Segoe UI Bold": "SegoeUI-Bold",
    "Segoe UI Bold Italic": "SegoeUI-BoldItalic", "Segoe UI Black": "SegoeUI-Black", "Segoe UI Black Italic": "SegoeUI-BlackItalic",
    "Segoe UI Historic": "SegoeUIHistoric", "Segoe UI Emoji": "SegoeUIEmoji", "Segoe UI Symbol": "SegoeUISymbol",
    "SimSun": "SimSun", "NSimSun": "NSimSun", "SimSun-ExtB": "SimSun-ExtB", "Sylfaen": "Sylfaen", "Symbol": "SymbolMT",
    "Tahoma": "Tahoma", "Tahoma Bold": "Tahoma-Bold", "Times New Roman": "TimesNewRomanPSMT",
    "Times New Roman Italic": "TimesNewRomanPS-ItalicMT", "Times New Roman Bold": "TimesNewRomanPS-BoldMT",
    "Times New Roman Bold Italic": "TimesNewRomanPS-BoldItalicMT", "Trebuchet MS": "TrebuchetMS",
    "Trebuchet MS Italic": "TrebuchetMS-Italic", "Trebuchet MS Bold": "TrebuchetMS-Bold", "Trebuchet MS Bold Italic": "Trebuchet-BoldItalic",
    "Verdana": "Verdana", "Verdana Italic": "Verdana-Italic", "Verdana Bold": "Verdana-Bold", "Verdana Bold Italic": "Verdana-BoldItalic",
    "Webdings": "Webdings", "Wingdings": "Wingdings-Regular", "Yu Gothic Light": "YuGothic-Light", "Yu Gothic Regular": "YuGothic-Regular",
    "Yu Gothic Medium": "YuGothic-Medium", "Yu Gothic Bold": "YuGothic-Bold", "Yu Gothic UI Light": "YuGothicUI-Light",
    "Yu Gothic UI Semilight": "YuGothicUI-Semilight", "Yu Gothic UI Regular": "YuGothicUI-Regular",
    "Yu Gothic UI Semibold": "YuGothicUI-Semibold", "Yu Gothic UI Bold": "YuGothicUI-Bold", "Segoe Fluent Icons": "SegoeFluentIcons",
}
def win_family(ms_family: str, name: str) -> str:
    """The family CSS matches the face by (DirectWrite WWS family)."""
    for pre in ("Sitka Small", "Sitka Text", "Sitka Subheading", "Sitka Heading", "Sitka Display", "Sitka Banner",
                "Microsoft JhengHei UI", "Microsoft YaHei UI", "Yu Gothic UI", "Segoe UI Variable Display",
                "Segoe UI Variable Small", "Segoe UI Variable Text"):
        if name.startswith(pre):
            return pre
    if ms_family in ("MS Gothic", "SimSun", "MingLiU-ExtB"):
        return name
    return ms_family


def ms_rows(page: Path) -> list:
    s = page.read_text(encoding="utf-8")
    s = re.sub(r"<(script|style)[^>]*>.*?</\1>", "", s, flags=re.S)
    t = re.sub(r"</(tr|p|h\d|li)>", "\n", s)
    t = re.sub(r"</t[dh]>", "\t", t)
    t = html.unescape(re.sub("<[^>]+>", "", t))
    i = t.find("Font Name\t")
    j = t.find("Fonts included in Feature On Demand", i)
    rows, fam = [], None
    for line in t[i:j].split("\n")[1:]:
        c = [x.strip() for x in line.split("\t")]
        if len(c) < 4 or not c[2]:
            continue
        if c[0]:
            fam = c[0]
        name, file = c[1].rstrip("*").strip(), c[2]
        if not name and not file.lower().endswith((".ttf", ".ttc")):
            name = file  # one row of the Windows 11 page has its cells shifted
        rows.append((fam, name))
    return rows


def windows(ms: Path) -> dict:
    out = {}
    for v in (10, 11):
        fams = {}
        for fam, name in ms_rows(ms / f"windows_{v}_font_list.html"):
            f = win_family(fam, name)
            fams.setdefault(f, []).append(name)
            if v == 10 and name.startswith("Sitka "):
                # Windows 10's static Sitka TTCs.
                parts = name.split(" ", 2)
                style = parts[2] if len(parts) > 2 else ""
                ps = "Sitka" + parts[1] + ("-" + style.replace(" ", "") if style else "")
                fams[f].append(ps)
            elif name in WIN_PS:
                fams[f].append(WIN_PS[name])
        out[v] = fams
    return out


# ---------------------------------------------------------------- Linux

# Ubuntu 24.04's default font packages: ubuntu-desktop-minimal's font
# Depends/Recommends (fonts-dejavu-core, fonts-liberation, fonts-noto-cjk,
# fonts-noto-color-emoji, fonts-noto-core, fonts-ubuntu), what they pull in
# (fonts-dejavu-mono, fonts-noto-mono, fonts-liberation-sans-narrow) and
# ghostscript's (fonts-urw-base35, fonts-droid-fallback): the packages the
# desktop installation on rtx-dev installed in one transaction.
UBUNTU24_PKGS = ["fonts-dejavu-core", "fonts-dejavu-mono", "fonts-droid-fallback", "fonts-liberation",
                 "fonts-liberation-sans-narrow", "fonts-noto-cjk", "fonts-noto-color-emoji", "fonts-noto-core",
                 "fonts-noto-mono", "fonts-ubuntu", "fonts-urw-base35"]
# Ubuntu 22.04 (jammy ubuntu-desktop-minimal): fonts-freefont-ttf is a
# Depends, there is no fonts-noto-core and fonts-ubuntu is the static 0.83
# set. Packages of the 24.04 recording whose names 22.04 shares.
UBUNTU22_PKGS = ["fonts-dejavu-core", "fonts-dejavu-mono", "fonts-droid-fallback", "fonts-freefont-ttf",
                 "fonts-liberation", "fonts-liberation-sans-narrow", "fonts-noto-cjk", "fonts-noto-color-emoji",
                 "fonts-noto-mono", "fonts-urw-base35"]
# fonts-ubuntu 0.83 static files (Ubuntu-R.ttf ...): full names (not captured).
UBUNTU_STATIC = {
    "Ubuntu": ["Ubuntu", "Ubuntu Bold", "Ubuntu Italic", "Ubuntu Bold Italic", "Ubuntu Light", "Ubuntu Light Italic",
               "Ubuntu Medium", "Ubuntu Medium Italic", "Ubuntu Thin"],
    "Ubuntu Condensed": ["Ubuntu Condensed"],
    "Ubuntu Mono": ["Ubuntu Mono", "Ubuntu Mono Bold", "Ubuntu Mono Italic", "Ubuntu Mono Bold Italic"],
}
# Chrome 153 on Ubuntu 24.04 loads local("system-ui") (no font has that name).
SYSTEM_UI = {"system-ui": ["system-ui"]}


def ubuntu_recorded(loaded: list, fc_rows: list, pkg_rows: list, pkgs: list, keep=lambda pkg, name: True) -> dict:
    """Names Chrome loaded on Ubuntu 24.04 whose font file belongs to one of
    `pkgs`, grouped by fontconfig family (its first, English, name)."""
    ok = {n.lower() for n in loaded}
    file_pkg = {r[2]: r[0] for r in pkg_rows if len(r) >= 3}
    out = {}
    for r in fc_rows:
        if len(r) < 5:
            continue
        f, fams, full, ps, _ = r[:5]
        pkg = file_pkg.get(f)
        if pkg not in pkgs:
            continue
        fam = fams.split(",")[0]
        for n in full.split(",") + [ps]:
            if n and n.lower() in ok and keep(pkg, n):
                out.setdefault(fam, []).append(n)
    return out


# Fedora: faces of the default Workstation font packages (comps "fonts"
# group and the Workstation's dependencies), not captured: fontconfig full
# and PostScript names as the fonts name them. Variable fonts resolve by
# their named instances only (fontconfig gives the default pattern no full
# name: on Ubuntu 24.04 local("Ubuntu Sans") fails, "Ubuntu Sans Regular"
# loads).


def four(family: str, ps: str, regular_full: str = None, styles=("Bold", "Italic", "Bold Italic"), ps_styles=None,
         regular_ps: str = None):
    ps_styles = ps_styles or [s.replace(" ", "") for s in styles]
    names = [regular_full or family, regular_ps or ps]
    for s, p in zip(styles, ps_styles):
        names += [f"{family} {s}", f"{ps}-{p}"]
    return names


def instances(family: str, ps: str, weights, italic=True):
    """Named instances of a variable font: `Family Weight`, `PS-Weight`."""
    names = []
    for w in weights:
        names += [f"{family} {w}", f"{ps}-{w.replace(' ', '')}"]
        if italic:
            it = "Italic" if w == "Regular" else f"{w} Italic"
            names += [f"{family} {it}", f"{ps}-{it.replace(' ', '')}"]
    return names


WEIGHTS9 = ["Thin", "ExtraLight", "Light", "Regular", "Medium", "SemiBold", "Bold", "ExtraBold", "Black"]
LIBERATION = {
    "Liberation Mono": four("Liberation Mono", "LiberationMono"),
    "Liberation Sans": four("Liberation Sans", "LiberationSans"),
    "Liberation Serif": four("Liberation Serif", "LiberationSerif"),
}
NOTO_EMOJI = {"Noto Color Emoji": ["Noto Color Emoji", "NotoColorEmoji"]}
URW = {
    "C059": ["C059 Roman", "C059-Roman", "C059 Bold", "C059-Bold", "C059 Italic", "C059-Italic", "C059 Bold Italic", "C059-BdIta"],
    "D050000L": ["D050000L"],
    "Nimbus Mono PS": four("Nimbus Mono PS", "NimbusMonoPS", "Nimbus Mono PS", regular_ps="NimbusMonoPS-Regular"),
    "Nimbus Roman": four("Nimbus Roman", "NimbusRoman", "Nimbus Roman", regular_ps="NimbusRoman-Regular"),
    "Nimbus Sans": four("Nimbus Sans", "NimbusSans", "Nimbus Sans", regular_ps="NimbusSans-Regular"),
    "Nimbus Sans Narrow": four("Nimbus Sans Narrow", "NimbusSansNarrow", "Nimbus Sans Narrow",
                               styles=("Bold", "Oblique", "Bold Oblique"), regular_ps="NimbusSansNarrow-Regular"),
    "P052": ["P052 Roman", "P052-Roman", "P052 Bold", "P052-Bold", "P052 Italic", "P052-Italic", "P052 Bold Italic", "P052-BoldItalic"],
    "Standard Symbols PS": ["Standard Symbols PS", "StandardSymbolsPS"],
    "URW Bookman": ["URW Bookman Light", "URWBookman-Light", "URW Bookman Light Italic", "URWBookman-LightItalic",
                    "URW Bookman Demi", "URWBookman-Demi", "URW Bookman Demi Italic", "URWBookman-DemiItalic"],
    "URW Gothic": ["URW Gothic Book", "URWGothic-Book", "URW Gothic Book Oblique", "URWGothic-BookOblique",
                   "URW Gothic Demi", "URWGothic-Demi", "URW Gothic Demi Oblique", "URWGothic-DemiOblique"],
    "Z003": ["Z003 Medium Italic", "Z003-MediumItalic"],
}
NOTO_VF = {
    "Noto Sans": instances("Noto Sans", "NotoSans", WEIGHTS9),
    "Noto Serif": instances("Noto Serif", "NotoSerif", WEIGHTS9),
    "Noto Sans Mono": instances("Noto Sans Mono", "NotoSansMono", WEIGHTS9, italic=False),
}
ADWAITA = {
    "Adwaita Sans": instances("Adwaita Sans", "AdwaitaSans", WEIGHTS9),
    "Adwaita Mono": four("Adwaita Mono", "AdwaitaMono", "Adwaita Mono", regular_ps="AdwaitaMono-Regular"),
}
CANTARELL = {"Cantarell": instances("Cantarell", "Cantarell", ["Thin", "Light", "Regular", "Bold", "ExtraBold"], italic=False)}


def merge(*sets) -> dict:
    out = {}
    for s in sets:
        for f, n in s.items():
            out.setdefault(f, []).extend(n)
    return out


FEDORA = {
    "fedora_42": ("Fedora Linux 42 (Workstation Edition)", "adwaita-sans-fonts, adwaita-mono-fonts, liberation-*-fonts 2.1.5, "
                  "google-noto-color-emoji-fonts, google-noto-sans-vf-fonts, google-noto-serif-vf-fonts, "
                  "google-noto-sans-mono-vf-fonts, urw-base35-*-fonts",
                  merge(ADWAITA, LIBERATION, NOTO_EMOJI, NOTO_VF, URW, SYSTEM_UI)),
    "fedora_41": ("Fedora Linux 41 (Workstation Edition)", "abattis-cantarell-vf-fonts, liberation-*-fonts 2.1.5, "
                  "google-noto-color-emoji-fonts, google-noto-sans-vf-fonts, google-noto-serif-vf-fonts, "
                  "google-noto-sans-mono-vf-fonts, urw-base35-*-fonts",
                  merge(CANTARELL, LIBERATION, NOTO_EMOJI, NOTO_VF, URW, SYSTEM_UI)),
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--mac26", required=True)
    ap.add_argument("--fc-list", required=True)
    ap.add_argument("--apple", required=True)
    ap.add_argument("--ms", required=True)
    ap.add_argument("--ubuntu24", required=True, help="names Chrome 153 loaded on Ubuntu 24.04 (record.mjs)")
    ap.add_argument("--ubuntu24-fc", required=True, help="fc-list output there (same format as --fc-list)")
    ap.add_argument("--ubuntu24-pkgs", required=True, help="package<TAB>version<TAB>file of the font packages there")
    ap.add_argument("--out", default="profiles/local_fonts")
    a = ap.parse_args()
    out = Path(a.out)
    loaded = json.loads(Path(a.mac26).read_text())
    rows = [l.rstrip("\n").split("\t") for l in Path(a.fc_list).read_text(encoding="utf-8").splitlines()]
    f26 = mac26_families(loaded, rows)
    write(out, "macos_26", "macOS 26", "Recorded: local Chrome 156 and Chrome 153 headless shell on macOS 26.4.1 (25E253), "
          "default install; every full, PostScript and family name fontconfig lists for the system fonts tried with "
          "FontFace local(), the ones that loaded kept (tools/local_fonts/record.mjs).", f26)
    for v, (fams, gone) in mac_older(f26, Path(a.apple)).items():
        write(out, f"macos_{v}", f"macOS {v}", f"Derived: the macOS 26 recording without the faces Apple's 'Fonts included "
              f"with macOS' list for {v} lacks ({len(gone)} names), plus Kefa (replaced by Kefa III in 26)"
              + (" and the static Noto Sans Syriac (variable since 15)" if v < 15 else "") + ".", fams)
    for v, fams in windows(Path(a.ms)).items():
        write(out, f"windows_{v}", f"Windows {v}", f"Documented: Microsoft's Windows {v} font list (desktop fonts, no "
              "Feature on Demand packages): full names, families as DirectWrite groups them, PostScript names from the "
              "fonts where known (not captured).", fams)
    u_loaded = json.loads(Path(a.ubuntu24).read_text())
    u_fc = [l.rstrip("\n").split("\t") for l in Path(a.ubuntu24_fc).read_text(encoding="utf-8").splitlines()]
    u_pkg = [l.split("\t") for l in Path(a.ubuntu24_pkgs).read_text(encoding="utf-8").splitlines()]
    vers = {r[0]: r[1] for r in u_pkg if len(r) >= 3}
    pk = lambda ps: ", ".join(f"{p} {vers.get(p, '?')}" for p in ps)
    write(out, "ubuntu_24.04", "Ubuntu 24.04 LTS", "Recorded: Chrome 153 headless shell on Ubuntu 24.04.4 (rtx-dev, "
          "ubuntu-desktop-minimal installed); every family, full and PostScript name fc-list gives tried with local(), "
          f"kept when it loaded and its file is in a default package ({pk(UBUNTU24_PKGS)}); plus system-ui.",
          merge(ubuntu_recorded(u_loaded, u_fc, u_pkg, UBUNTU24_PKGS), SYSTEM_UI))
    # 22.04: DejaVu Sans Mono was Book and Bold only (in fonts-dejavu-core).
    keep22 = lambda pkg, n: not (pkg == "fonts-dejavu-mono" and "Oblique" in n)
    write(out, "ubuntu_22.04", "Ubuntu 22.04 LTS", "Derived: the Ubuntu 24.04 recording's names for the packages 22.04 "
          f"also installs ({pk(UBUNTU22_PKGS)}; DejaVu Sans Mono Book and Bold only), plus fonts-ubuntu 0.83's static "
          "faces (documented, not captured) and system-ui. Not listed: jammy's script fonts (fonts-indic, kacst-one, "
          "khmeros-core, lao, lklug-sinhala, opensymbol, sil-abyssinica, sil-padauk, thai-tlwg, tibetan-machine).",
          merge(ubuntu_recorded(u_loaded, u_fc, u_pkg, UBUNTU22_PKGS, keep22), UBUNTU_STATIC, SYSTEM_UI))
    for ident, (label, pkgs, fams) in FEDORA.items():
        write(out, ident, label, f"Documented: faces of the default font packages ({pkgs}); fontconfig full and "
              "PostScript names as the fonts name them, variable fonts by their named instances (not captured), "
              "plus system-ui as recorded on Ubuntu.", fams)

if __name__ == "__main__":
    main()
