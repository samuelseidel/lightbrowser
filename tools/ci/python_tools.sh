#!/usr/bin/env bash
# The Python tooling tests, all on loopback:
#
#   python3 -m unittest discover -s tests/detect-bench      detection bench: scoring and server
#   python3 -m unittest discover -s bench                   crawl bench tooling
#   (cd sdk/python && python3 -m pytest tests)              Python SDK, unit and integration
#
# The two unittest suites need only the standard library. The SDK suite needs
# pytest and Python Playwright >= 1.63 (pip install pytest 'playwright>=1.63');
# its integration tests launch the debug binary and SKIP when there is none,
# which would hide a broken build, so the binary is required here.
# Every suite runs even if an earlier one failed; the exit status is 1 if any did.
#
# Usage: tools/ci/python_tools.sh
#   PYTHON=/path/to/python    interpreter with pytest and Playwright (default python3)
#   LIGHTBROWSER_BIN=...      binary for the SDK tests (default target/debug/lightbrowser)
#   SDK_UNIT_ONLY=1           run only the SDK tests that need no binary
# Used by .github/workflows/ci.yml, job "python-tools".
set -uo pipefail

cd "$(dirname "$0")/../.."
repo=$PWD
py="${PYTHON:-python3}"
case "$py" in
  /*) ;;
  */*) py="$PWD/$py" ;; # a relative path to an interpreter, e.g. .venv/bin/python; the script changes directory
esac
bin="${LIGHTBROWSER_BIN:-$repo/target/debug/lightbrowser}"
export PYTHONDONTWRITEBYTECODE=1
failed=0

run() {
  echo "+ $*"
  "$@" || { echo "FAILED: $*" >&2; failed=$((failed + 1)); }
}

run "$py" -m unittest discover -s tests/detect-bench
run "$py" -m unittest discover -s bench

if [ "${SDK_UNIT_ONLY:-0}" = 1 ]; then
  sdk_args=(-m "not integration")
elif [ -x "$bin" ]; then
  export LIGHTBROWSER_BIN="$bin"
  sdk_args=()
else
  echo "error: $bin is not an executable; build it (cargo build --locked -p lightbrowser) or set SDK_UNIT_ONLY=1" >&2
  failed=$((failed + 1))
  sdk_args=(-m "not integration")
fi

# -ra lists every skip with its reason, so a silent skip shows in the log.
cd sdk/python
run "$py" -m pytest -ra tests ${sdk_args[@]+"${sdk_args[@]}"}

echo "python tools: $failed suite(s) failed"
[ "$failed" -eq 0 ]
