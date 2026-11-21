#!/usr/bin/env bash
# Fails when the release binary is not under the size budget.
#
# The budget is "< 50 MB, stripped" (ADR 0013, revision 4; docs/ARCHITECTURE.md
# "Goals and budgets"). MB is decimal, as in bench/run.py (BUDGET_BINARY_MB =
# 50.0, size = st_size / 1e6): the limit is 50,000,000 bytes, not 50 MiB. The
# number is read from bench/run.py so that this check and the bench cannot
# disagree. A release build strips the binary (`strip = true` in Cargo.toml).
#
# Usage: tools/ci/check_binary_size.sh [path/to/lightbrowser]
# The binary defaults to target/release/lightbrowser
# (cargo build --release --locked -p lightbrowser, the build the budget is
# measured on). Used by .github/workflows/ci.yml, job "release".
set -euo pipefail

cd "$(dirname "$0")/../.."
bin="${1:-target/release/lightbrowser}"

budget_mb=$(sed -n 's/^BUDGET_BINARY_MB = \([0-9][0-9.]*\).*/\1/p' bench/run.py)
if [ -z "$budget_mb" ]; then
  echo "error: BUDGET_BINARY_MB not found in bench/run.py" >&2
  exit 2
fi
limit=$(awk -v mb="$budget_mb" 'BEGIN { printf "%d", mb * 1000000 }')

if [ ! -f "$bin" ]; then
  echo "error: $bin not found (cargo build --release --locked -p lightbrowser)" >&2
  exit 2
fi
size=$(wc -c < "$bin" | tr -d ' ')

mb=$(awk -v b="$size" 'BEGIN { printf "%.2f", b / 1000000 }')
if [ "$size" -lt "$limit" ]; then
  echo "ok: $bin is $size bytes ($mb MB), under the budget of $budget_mb MB ($limit bytes)"
else
  echo "FAIL: $bin is $size bytes ($mb MB), the budget is under $budget_mb MB ($limit bytes; ADR 0013 revision 4)" >&2
  exit 1
fi
