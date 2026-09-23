#!/usr/bin/env bash
# Build the analyzer, run it on every MIR test, and diff against expected output.
set -euo pipefail
cd "$(dirname "$0")"
cargo build --quiet
BIN=./target/debug/guardstate
SUMMARY=tests_mir/can_sleep.txt
pass=0; fail=0
for t in tests_mir/t*.mir; do
  base=$(basename "$t" .mir)
  exp="tests_mir/expected/$base.txt"
  got=$("$BIN" "$t" --summary "$SUMMARY")
  if diff -u "$exp" <(printf '%s\n' "$got") > /dev/null; then
    echo "PASS  $base"; pass=$((pass+1))
  else
    echo "FAIL  $base"; diff -u "$exp" <(printf '%s\n' "$got") || true; fail=$((fail+1))
  fi
done
echo "-----------------------------"
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
