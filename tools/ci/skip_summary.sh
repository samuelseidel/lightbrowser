#!/bin/sh
# Count the skipped tests in a cargo test log, per reason.
#
#   cargo test --workspace 2>&1 | tee test.log
#   tools/ci/skip_summary.sh test.log        # or: ... | tools/ci/skip_summary.sh
#
# A test that needs root, iptables, IPv6 or a default route prints
#   LB-SKIP <binary>::<test>: <reason>
# and returns, and a skipped test counts as passed (crates/lb-net/tests/common).
# The line goes to the real stderr, so it is in a plain `cargo test` log
# without --nocapture. This script is the other half: it shows what did not
# run. It always exits 0; set LB_REQUIRE_NO_SKIPS=1 for the run to make a skip
# fail the test instead.
set -eu

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
    sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
fi

awk '
    {
        at = index($0, "LB-SKIP ")
        if (at == 0) next
        rest = substr($0, at + 8)
        sub(/\r$/, "", rest)
        # A skip that LB_REQUIRE_NO_SKIPS=1 turned into a failure carries a
        # suffix; it is the same reason.
        sub(/ \(LB_REQUIRE_NO_SKIPS=1[^)]*\)$/, "", rest)
        sep = index(rest, ": ")
        if (sep == 0) { test = "?"; reason = rest }
        else { test = substr(rest, 1, sep - 1); reason = substr(rest, sep + 2) }
        total++
        if (!(reason in count)) { order[++n] = reason }
        count[reason]++
        if (!((reason SUBSEP test) in seen)) {
            seen[reason SUBSEP test] = 1
            tests[reason] = tests[reason] "\n      " test
        }
    }
    END {
        if (total == 0) { print "LB-SKIP summary: no skipped tests"; exit }
        printf "LB-SKIP summary: %d skip(s), %d reason(s)\n", total, n
        # Most frequent reason first.
        for (i = 1; i <= n; i++) {
            best = i
            for (j = i + 1; j <= n; j++) if (count[order[j]] > count[order[best]]) best = j
            tmp = order[i]; order[i] = order[best]; order[best] = tmp
        }
        for (i = 1; i <= n; i++) printf "%5d  %s%s\n", count[order[i]], order[i], tests[order[i]]
    }
' "$@"
