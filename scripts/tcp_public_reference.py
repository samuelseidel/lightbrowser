#!/usr/bin/env python3
"""Rebuild profiles/reference/tcp_public.json from public data.

Source: the OS fingerprint database published in NikolaiT/zardaxt
(database/newCleaned.json): real client SYNs, each labelled with the OS its
User-Agent claims. The file is under an all-rights-reserved licence, so it is
downloaded to a temporary directory and never committed. Only the aggregate
shares computed here (facts about the traffic) are written to the repository.

  python3 scripts/tcp_public_reference.py            # download, then write
  python3 scripts/tcp_public_reference.py --db FILE  # use a local copy

Needs network access to raw.githubusercontent.com (or --db). Read-only on the
network; it fetches one file.
"""
import argparse
import collections
import datetime
import json
import pathlib
import sys
import tempfile
import urllib.request

URL = "https://raw.githubusercontent.com/NikolaiT/zardaxt/master/database/newCleaned.json"
OUT = pathlib.Path(__file__).resolve().parent.parent / "profiles/reference/tcp_public.json"
CLASSES = {"windows": "Windows", "macos": "Mac OS", "linux": "Linux"}


def shares(entries, key, top=12):
    c = collections.Counter()
    for e in entries:
        c[key(e)] += e["n"]
    total = sum(c.values())
    return {str(k): round(v / total, 4) for k, v in c.most_common(top)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", help="local copy of newCleaned.json")
    ap.add_argument("--out", default=str(OUT))
    args = ap.parse_args()
    if args.db:
        raw = pathlib.Path(args.db).read_bytes()
    else:
        with urllib.request.urlopen(URL, timeout=60) as r:  # noqa: S310 (fixed https URL)
            raw = r.read()
    db = json.loads(raw)
    out = {
        "source": {
            "name": "NikolaiT/zardaxt database/newCleaned.json",
            "url": URL,
            "retrieved": datetime.date.today().isoformat(),
            "entries": len(db),
            "licence_note": "The source is all-rights-reserved, so only the aggregate shares below are kept; the file itself is not copied.",
            "method": "share of sessions (weighted by each fingerprint's n) over every fingerprint of the class",
            "caveat": "Labels come from the User-Agent, so classes include traffic behind proxies and VPNs. 'linux' includes Android and Chrome OS.",
            "regenerate": "python3 scripts/tcp_public_reference.py",
        },
        "classes": {},
    }
    for ours, theirs in CLASSES.items():
        es = [e for e in db if e["os"] == theirs]
        zero = sum(e["n"] for e in es if e["ip_id"] == 0)
        ecn = sum(e["n"] for e in es if e["tcp_flags"] & 0xC2 == 0xC2)
        total = sum(e["n"] for e in es)
        out["classes"][ours] = {
            "label": theirs,
            "sessions": total,
            "option_order": shares(es, lambda e: e["tcp_options_ordered"]),
            "window": shares(es, lambda e: e["tcp_window_size"]),
            "window_scale": shares(es, lambda e: e["tcp_window_scaling"]),
            "ttl": shares(es, lambda e: e["ip_ttl"]),
            "ip_id_zero": round(zero / total, 4),
            "ecn_on_syn": round(ecn / total, 4),
        }
    pathlib.Path(args.out).write_text(json.dumps(out, indent=2) + "\n")
    print(f"wrote {args.out}: {len(db)} fingerprints", file=sys.stderr)


if __name__ == "__main__":
    main()
