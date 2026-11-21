#!/usr/bin/env python3
"""Carry hot-symbols.txt over to another Rust toolchain, keeping its order.

Rust v0 symbol names carry a hash per crate (`Cs...`) that depends on the
rustc build. A list made with one toolchain matches no Rust function of a
binary built with another, and lld drops unmatched names silently
(`--no-warn-symbol-ordering`), so Rust code is left unordered while C++ code
still is. This rewrites every Rust name: it demangles it without the crate
hashes (binutils `c++filt`) and replaces it with the names of the same
functions in BINARY, an unstripped build made with the new toolchain
(several when a demangled name is ambiguous). C and C++ names are kept as
they are. No valgrind run is needed; regenerate with gen.py when the code
itself changed a lot.

usage: remap.py --binary target/release/lightbrowser [--list bin/lightbrowser/hot-symbols.txt] [--out PATH]
Build BINARY with:
    cargo build --release -p lightbrowser --config 'profile.release.package.lightbrowser.strip=false'
"""

import argparse
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


CRATE_HASH = re.compile(r"\[[0-9a-f]{1,16}\]")


def demangle(names: list[str]) -> list[str]:
    """c++filt, one name per line, in order, with the crate hashes it prints
    (`std[bcb96dfbba0ab899]::...`) removed."""
    out = subprocess.run(
        ["c++filt"], input="\n".join(names) + "\n", capture_output=True, text=True, check=True
    ).stdout.splitlines()
    if len(out) != len(names):
        sys.exit("c++filt changed the line count")
    return [CRATE_HASH.sub("", n) for n in out]


def binary_rust_symbols(binary: str) -> list[str]:
    out = subprocess.run(
        ["nm", "--defined-only", "--format=posix", binary], capture_output=True, text=True, check=True
    ).stdout
    names = []
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[1] in "tTwW" and parts[0].startswith("_R"):
            names.append(parts[0])
    if not names:
        sys.exit(f"{binary}: no Rust symbols (stripped? build with strip=false)")
    return names


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--binary", required=True, help="unstripped release build made with the new toolchain")
    ap.add_argument("--list", default=str(ROOT / "bin/lightbrowser/hot-symbols.txt"))
    ap.add_argument("--out", help="output path (default: rewrite --list)")
    args = ap.parse_args()

    lines = Path(args.list).read_text().splitlines()
    header = [l for l in lines if l.startswith("#")]
    body = [l for l in lines if l and not l.startswith("#")]
    old_rust = [l for l in body if l.startswith("_R")]

    have = binary_rust_symbols(args.binary)
    by_name: dict[str, list[str]] = defaultdict(list)
    for mangled, plain in zip(have, demangle(have)):
        by_name[plain].append(mangled)
    have_set = set(have)
    wanted = dict(zip(old_rust, demangle(old_rust)))

    out, seen = [], set()
    matched = unchanged = missing = 0
    for line in body:
        if not line.startswith("_R"):
            names = [line]
        elif line in have_set:
            names, unchanged = [line], unchanged + 1
        else:
            names = by_name.get(wanted[line], [])
            if names:
                matched += 1
            else:
                missing += 1
        for n in names:
            if n not in seen:
                seen.add(n)
                out.append(n)

    rustc = subprocess.run(["rustc", "-vV"], capture_output=True, text=True).stdout.splitlines()
    release = next((l.split(": ", 1)[1] for l in rustc if l.startswith("release:")), "?")
    commit = next((l.split(": ", 1)[1] for l in rustc if l.startswith("commit-hash:")), "?")
    stamp = f"# rustc {release} {commit[:9]}"
    header = [h for h in header if not h.startswith("# rustc ")]
    Path(args.out or args.list).write_text("\n".join([stamp, *header, *out]) + "\n")
    print(
        f"{len(old_rust)} Rust names: {unchanged} unchanged, {matched} remapped, {missing} not in the binary; "
        f"{len(out)} lines written ({stamp[2:]})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
