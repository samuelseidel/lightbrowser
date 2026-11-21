#!/usr/bin/env bash
# MVP acceptance: one Playwright connectOverCDP crawl, zero scored flags.
#
# Starts the in-repo detection bench and `lightbrowser serve` on loopback,
# then runs T-010 with --input human. A flag is a rule in the session's
# `fired` list (weight > 0). Weight-0 rules are printed as info and do not
# fail the run. See docs/MVP.md.
#
#   tests/detect-bench/mvp.sh
#   LIGHTBROWSER_BIN=./target/release/lightbrowser tests/detect-bench/mvp.sh
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

bin=${LIGHTBROWSER_BIN:-"$root/target/debug/lightbrowser"}
persona=${MVP_PERSONA:-"$root/profiles/reference/chrome/chrome153_win10_rtx3060_1080p.json"}
if [[ ! -x "$bin" ]]; then
  echo "mvp: binary not found: $bin (build with: cargo build -p lightbrowser)" >&2
  exit 1
fi
if [[ ! -f "$persona" ]]; then
  echo "mvp: persona not found: $persona" >&2
  exit 1
fi
if ! node -e "require('playwright')" >/dev/null 2>&1; then
  export NODE_PATH="$root/tests/playwright/node_modules${NODE_PATH:+:$NODE_PATH}"
fi
if ! node -e "require('playwright')" >/dev/null 2>&1; then
  echo "mvp: playwright is not installed (npm install --prefix tests/playwright)" >&2
  exit 1
fi

results=$(mktemp -d)
cleanup() {
  if [[ -n "${lb_pid:-}" ]]; then kill "$lb_pid" 2>/dev/null || true; wait "$lb_pid" 2>/dev/null || true; fi
  if [[ -n "${bench_pid:-}" ]]; then kill "$bench_pid" 2>/dev/null || true; wait "$bench_pid" 2>/dev/null || true; fi
  rm -rf "$results"
}
trap cleanup EXIT

python3 tests/detect-bench/server.py --host 127.0.0.1 --port 0 --quiet \
  --results-dir "$results" >"$results/bench.log" 2>&1 &
bench_pid=$!
base=""
for _ in $(seq 1 50); do
  if grep -q 'detect-bench on ' "$results/bench.log" 2>/dev/null; then
    base=$(sed -n 's/.*detect-bench on \(http:\/\/[^ ]*\)\/shop.*/\1/p' "$results/bench.log" | head -1)
    break
  fi
  if ! kill -0 "$bench_pid" 2>/dev/null; then
    echo "mvp: detect-bench exited" >&2
    cat "$results/bench.log" >&2 || true
    exit 1
  fi
  sleep 0.1
done
if [[ -z "$base" ]]; then
  echo "mvp: detect-bench did not print its URL" >&2
  cat "$results/bench.log" >&2 || true
  exit 1
fi

"$bin" serve --port 0 --egress loopback --persona "$persona" \
  >"$results/lb.log" 2>&1 &
lb_pid=$!
ws=""
for _ in $(seq 1 100); do
  if grep -q 'DevTools listening on ' "$results/lb.log" 2>/dev/null; then
    ws=$(sed -n 's/.*DevTools listening on \(ws:\/\/[^ ]*\).*/\1/p' "$results/lb.log" | head -1)
    break
  fi
  if ! kill -0 "$lb_pid" 2>/dev/null; then
    echo "mvp: lightbrowser exited" >&2
    cat "$results/lb.log" >&2 || true
    exit 1
  fi
  sleep 0.1
done
if [[ -z "$ws" ]]; then
  echo "mvp: lightbrowser did not print a DevTools URL" >&2
  cat "$results/lb.log" >&2 || true
  exit 1
fi

echo "mvp: bench $base"
echo "mvp: cdp   $ws"
set +e
node tests/detect-bench/run.js --base "$base" --scenario T-010 --cdp "$ws" \
  --input human --seed 1 --expect 'fp<=0&beh<=0' | tee "$results/run.txt"
code=${PIPESTATUS[0]}
set -e
echo "mvp: run.js exit $code"
if [[ "$code" -ne 0 ]]; then
  echo "mvp: scored flags are not zero (see the fired rules above)" >&2
  exit "$code"
fi
echo "mvp: zero scored flags"
