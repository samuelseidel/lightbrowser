#!/bin/sh
# Record which page code Chrome runs during console calls, with no DevTools
# client attached (Playwright would attach one): load each case with
# `chrome-headless-shell --dump-dom` on loopback and print its hit list.
# Usage: CHROME=/path/to/chrome-headless-shell sh record.sh > chrome.json
set -e
cd "$(dirname "$0")/cases"
python3 -m http.server 18995 --bind 127.0.0.1 >/dev/null 2>&1 &
srv=$!
trap 'kill $srv' EXIT
sleep 1
printf '{\n'
sep=''
for f in *.html; do
  hits=$("$CHROME" --no-sandbox --user-data-dir="$(mktemp -d)" --disable-background-networking \
    --dump-dom "http://127.0.0.1:18995/$f" 2>/dev/null | sed -n 's/.*HITS=\(\[[^]]*\]\).*/\1/p' | head -1)
  printf '%s "%s": %s' "$sep" "${f%.html}" "$hits"
  sep=$(printf ',\n')
done
printf '\n}\n'
