#!/usr/bin/env python3
"""Renderer header-group orders with client hints, from an Accept-CH capture.

    python3 tools/header_capture/derive_groups.py \
        profiles/headers/captures/chrome_153_linux_accept_ch.jsonl

Prints the `renderer_group.observed_hints` entries for
`profiles/headers/chrome_153.json`: for every subresource request that
carried high-entropy hints, the hint set, the page headers (plus `origin`)
and the observed order of the group (Blink's hash-table order, ADR 0011).
Fails if one (hints, extra) set was seen with two different orders.
"""
import json
import sys

PERSONA = {"user-agent", "accept-language", "sec-ch-ua", "sec-ch-ua-mobile", "sec-ch-ua-platform"}
SKIP = {"host", "connection", "content-length"}
# Headers the capture page set itself, by request path (accept_ch.js).
PAGE_ACCEPT = {"/api?combo3"}


def main(path):
    seen = {}
    for line in open(path):
        d = json.loads(line)
        if "_meta" in d:
            continue
        names = [n.lower() for n, _ in d["headers"] if not n.startswith(":") and n.lower() not in SKIP]
        hints = sorted(n for n in names if n.startswith("sec-ch-ua-") and n not in PERSONA)
        if not hints or "upgrade-insecure-requests" in names:  # navigations have no group
            continue
        target = d["request"].split()[1]
        end = names.index("sec-fetch-site" if target in PAGE_ACCEPT else "accept")
        group = names[:end]
        extra = sorted(n for n in group if n not in PERSONA and n not in hints)
        key = (tuple(hints), tuple(extra))
        if key in seen and seen[key] != group:
            sys.exit(f"two orders for {key}: {seen[key]} vs {group}")
        seen[key] = group
    out = [{"hints": list(h), "extra": list(e), "order": o} for (h, e), o in sorted(seen.items())]
    print(json.dumps(out, indent=2))


if __name__ == "__main__":
    main(sys.argv[1])
