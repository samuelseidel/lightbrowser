#!/usr/bin/env bash
# Persona validation gate.
#
#   - every file in profiles/reference/chrome/ must validate: `lightbrowser
#     validate` exits 0;
#   - every file in profiles/generated/adversarial/ must be rejected: exit
#     status 1 and at least one [VIOLATION] in the report. (A file that cannot
#     be read or parsed also exits 1, and must not count as rejected by a rule.)
#     Which rule fires is not checked: one adversarial file breaks several
#     (profiles/README.md).
#
# Usage: tools/ci/validate_personas.sh [path/to/lightbrowser]
# The binary is the argument, else $LIGHTBROWSER_BIN, else target/debug/lightbrowser
# (cargo build --locked -p lightbrowser). Used by .github/workflows/ci.yml, job "rust".
set -euo pipefail

cd "$(dirname "$0")/../.."
bin="${1:-${LIGHTBROWSER_BIN:-target/debug/lightbrowser}}"

# Floors, so that a deleted file or a wrong glob cannot pass an empty loop.
# Raise them when files are added.
MIN_REFERENCE=8
MIN_ADVERSARIAL=13

if [ ! -x "$bin" ]; then
  echo "error: $bin is not an executable (cargo build --locked -p lightbrowser)" >&2
  exit 2
fi

shopt -s nullglob
reference=(profiles/reference/chrome/*.json)
adversarial=(profiles/generated/adversarial/*.json)

if [ "${#reference[@]}" -lt "$MIN_REFERENCE" ]; then
  echo "error: ${#reference[@]} files in profiles/reference/chrome/, expected at least $MIN_REFERENCE" >&2
  exit 2
fi
if [ "${#adversarial[@]}" -lt "$MIN_ADVERSARIAL" ]; then
  echo "error: ${#adversarial[@]} files in profiles/generated/adversarial/, expected at least $MIN_ADVERSARIAL" >&2
  exit 2
fi

failed=0

for f in "${reference[@]}"; do
  if out=$("$bin" validate "$f" 2>&1); then rc=0; else rc=$?; fi
  if [ "$rc" -eq 0 ]; then
    echo "ok    reference    $f"
  else
    echo "FAIL  reference    $f: exit status $rc, expected 0"
    echo "$out" | sed 's/^/        /'
    failed=$((failed + 1))
  fi
done

for f in "${adversarial[@]}"; do
  if out=$("$bin" validate "$f" 2>&1); then rc=0; else rc=$?; fi
  if [ "$rc" -ne 1 ]; then
    echo "FAIL  adversarial  $f: exit status $rc, expected 1"
    echo "$out" | sed 's/^/        /'
    failed=$((failed + 1))
  elif ! printf '%s\n' "$out" | grep -q '\[VIOLATION\]'; then
    echo "FAIL  adversarial  $f: exit status 1 but no rule violation in the report"
    echo "$out" | sed 's/^/        /'
    failed=$((failed + 1))
  else
    echo "ok    adversarial  $f ($(printf '%s\n' "$out" | grep -c '\[VIOLATION\]') violation lines)"
  fi
done

echo "${#reference[@]} reference and ${#adversarial[@]} adversarial personas checked, $failed failed"
[ "$failed" -eq 0 ]
