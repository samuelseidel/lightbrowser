# Local font faces

Face names that `FontFace` / `@font-face` `local()` resolves on a default
install of an OS release. `lb-layout` (`src/local_fonts.rs`) bundles them
(one zstd frame, about 15 KB for all files; a list is unpacked and parsed
on its first `local()` lookup only) and picks one by the persona's `os.name`,
`os.version`, Windows build and Linux `os.distro`; `lb-webapi`
`rt/fonts.rs` resolves `local()` with it.

Chrome resolves `local()` by unique face name, never by family name alone
(`local("Menlo")` fails on macOS, `local("Menlo Regular")` and
`local("Menlo-Regular")` load), without case, in all languages on Windows
and Linux and in English only on macOS:

- macOS: CoreText (full and PostScript names, also the reserved
  FontServices fonts such as PingFang and variable-font instances).
- Windows: `IDWriteFontSet::GetMatchingFonts`, PostScript name then full
  name (Chromium `DWriteFontProxyImpl::MatchUniqueFont`).
- Linux: fontconfig `FC_POSTSCRIPT_NAME` then `FC_FULLNAME`, sfnt files
  only (Chromium `FontConfigLocalMatching`). Localized full names match
  ("FreeSans Fett"); a variable font matches by its named instances only
  (`local("Ubuntu Sans")` fails, `local("Ubuntu Sans Regular")` loads);
  `local("system-ui")` loads (recorded on Ubuntu 24.04).

## Format (`format: 2`)

```json
{"format": 2, "os": "macOS 26", "source": "...",
 "families": {"Menlo": ["Menlo Bold", "Menlo-Bold", "Menlo Regular", "Menlo-Regular"]}}
```

`families` maps the family CSS matches the faces by (the persona's
`fonts.available` names) to the face names (full and PostScript). A
persona family that a list lacks (an extra installed font) resolves by its
family name.

## Files and where they come from

| File | Persona releases | Source |
|---|---|---|
| `macos_26.json` | macOS 26 and later | **Recorded**: Chrome 156 and Chrome 153 headless shell on macOS 26.4.1 (25E253), default install (no downloaded fonts; `/Library/Fonts` holds Arial Unicode only). 6411 candidates (every family, full and PostScript name fontconfig lists for the system fonts, plus CoreText's names) tried with `local()`; the 1758 that loaded, same in both versions. |
| `macos_15.json` | macOS 15 | **Derived** from `macos_26.json` with Apple's "Fonts included with macOS" lists ([Ventura](https://support.apple.com/en-us/HT213266), [Sonoma](https://support.apple.com/en-us/108939), [Sequoia](https://support.apple.com/en-us/120414), [Tahoe](https://support.apple.com/en-us/122869)): a family goes when none of its recorded faces is on the release's list (Kefa III), Kefa Regular/Bold (replaced by Kefa III in 26) is added. |
| `macos_14.json`, `macos_13.json` | macOS 14, macOS 13 and older | As 15, also without PingFang MO and the variable Noto Sans Syriac (both new in 15), with the static Noto Sans Syriac Regular. Apple lists no other difference in the faces macOS 26 installs. |
| `windows_10.json` | Windows 10 (build < 22000) | **Documented**: [Microsoft's Windows 10 font list](https://learn.microsoft.com/en-us/typography/fonts/windows_10_font_list), desktop fonts (no Feature on Demand packages): full names, grouped by DirectWrite family (Calibri Light is Calibri, Yu Gothic UI its own); PostScript names from the fonts (the Microsoft fonts macOS ships were checked with fc-list; static Sitka from its naming). |
| `windows_11.json` | Windows 11 (build >= 22000) | **Documented**: [Microsoft's Windows 11 font list](https://learn.microsoft.com/en-us/typography/fonts/windows_11_font_list), same rules; adds Segoe Fluent Icons, Segoe UI Variable, Cascadia Code/Mono and the variable Sitka's Semibold. Variable-font instances list full names only. |
| `ubuntu_24.04.json` | Ubuntu 24.04 and later | **Recorded**: Chrome 153 headless shell on Ubuntu 24.04.4 (rtx-dev, ubuntu-desktop-minimal installed): every family, full and PostScript name `fc-list` gives (1411) tried with `local()`; kept when it loaded and its file belongs to a default package. Default packages: ubuntu-desktop-minimal's font Depends/Recommends ([noble](https://packages.ubuntu.com/noble/ubuntu-desktop-minimal): fonts-dejavu-core, fonts-liberation, fonts-noto-cjk, fonts-noto-color-emoji, fonts-noto-core, fonts-ubuntu), what they pull in (fonts-dejavu-mono, fonts-noto-mono, fonts-liberation-sans-narrow) and ghostscript's (fonts-urw-base35, fonts-droid-fallback): the packages the desktop installation there installed in one transaction (`/var/log/dpkg.log`). Not fonts-freefont-ttf (Playwright's dependencies added it later). Plus `system-ui`. |
| `ubuntu_22.04.json` | Ubuntu before 24.04 | **Derived**: the 24.04 recording's names for the packages jammy also installs ([jammy](https://packages.ubuntu.com/jammy/ubuntu-desktop-minimal): fonts-freefont-ttf is a Depends there, no fonts-noto-core), DejaVu Sans Mono Book and Bold only, plus fonts-ubuntu 0.83's static faces (documented full names, not captured) and `system-ui`. Jammy's script fonts (fonts-indic, kacst-one, khmeros-core, lao, lklug-sinhala, opensymbol, sil-abyssinica, sil-padauk, thai-tlwg, tibetan-machine) are not listed. |
| `fedora_42.json` | Fedora 42 and later | **Documented**: adwaita-sans/mono-fonts, liberation-*-fonts 2.1.5, google-noto-color-emoji-fonts, google-noto-sans/serif/sans-mono-vf-fonts, urw-base35-*-fonts; variable fonts by named instances (Thin to Black, with Italic), as fontconfig lists them on Ubuntu; plus `system-ui`. |
| `fedora_41.json` | Fedora before 42 | As 42 with abattis-cantarell-vf-fonts instead of Adwaita. |

Only default-install fonts are listed. Not captured (no such machine
here): Windows, Fedora, macOS 13-15 and the static Ubuntu 0.83 faces of
22.04. Known gaps there: localized full names (Windows and Fedora match
them too), the exact named instances of Fedora's variable fonts and the
PostScript names of Windows' variable fonts, the script fonts a Linux
desktop adds with its language packs.

The Ubuntu 24.04 persona's `fonts.available` follows this list: Noto
Sans, Noto Serif and Noto Sans Mono (fonts-noto-core, fonts-noto-mono)
are installed and measurable, GNU FreeFont is not.

CreepJS (`src/fonts`) guesses the OS release from which of its per-version
names load; every list here loads that release's names
(`crates/lb-page/tests/fonts/`). Each list also agrees with the persona's
measurable fonts: every `fonts.available` family has faces in its list and
every list family lb-layout has a metric table for is in `fonts.available`
(`cargo test -p lb-layout --test os local_faces`).

## Re-recording and rebuilding

```bash
fc-list --format '%{file}\t%{family}\t%{fullname}\t%{postscriptname}\t%{style}\n' > /tmp/fc.tsv
swiftc -O -o /tmp/ctnames tools/local_fonts/ctnames.swift && /tmp/ctnames > /tmp/ct.txt
PLAYWRIGHT_CORE=/path/to/playwright-core/index.mjs node tools/local_fonts/record.mjs \
    --chrome "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
    --fc-list /tmp/fc.tsv --names /tmp/ct.txt --port 19141 > /tmp/loaded.json
# Apple's pages saved as HT213266.html 108939.html 120414.html 122869.html in APPLE_DIR,
# Microsoft's as windows_10_font_list.html windows_11_font_list.html in MS_DIR
# On Ubuntu 24.04 (rtx-dev): the same recording with fc-list's names only, and
# the package of every font file
fc-list --format '%{file}\t%{family}\t%{fullname}\t%{postscriptname}\t%{style}\n' > $OUT/fc.tsv
node tools/local_fonts/record.mjs --chrome $CHROME153 --fc-list $OUT/fc.tsv --port 19141 > $OUT/loaded.json
for p in fonts-dejavu-core fonts-dejavu-mono fonts-droid-fallback fonts-freefont-ttf fonts-liberation \
    fonts-liberation-sans-narrow fonts-noto-cjk fonts-noto-color-emoji fonts-noto-core fonts-noto-mono \
    fonts-ubuntu fonts-urw-base35; do v=$(dpkg-query -W -f='${Version}' $p)
  dpkg -L $p | grep -E '\.(ttf|otf|ttc|otc)$' | while read f; do printf '%s\t%s\t%s\n' $p $v "$(readlink -f $f)"; done
done > $OUT/pkgfiles.tsv
python3 tools/local_fonts/build.py --mac26 /tmp/loaded.json --fc-list /tmp/fc.tsv \
    --apple APPLE_DIR --ms MS_DIR --ubuntu24 UBUNTU/loaded.json --ubuntu24-fc UBUNTU/fc.tsv \
    --ubuntu24-pkgs UBUNTU/pkgfiles.tsv --out profiles/local_fonts
```

Loopback only; the recording page loads nothing from other hosts.
