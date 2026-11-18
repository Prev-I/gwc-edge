#!/usr/bin/env bash
# Fail if the built Worker module is larger than its budget (ADR-0026).
# Every fresh isolate compiles the whole module, and with sparse traffic
# nearly every request starts one, so the module's size is paid in CPU time
# by almost every request. Raise the budget only on purpose, in the same
# pull request as the growth, saying why.
#
# Usage: scripts/wasm-size.sh [path/to/index_bg.wasm]   (after a build)
set -euo pipefail

# 667,062 bytes at 0.5.0 without the url crate, plus about 8 % headroom.
max_bytes=720000

# An argument is relative to the caller's directory; the default to the repo.
wasm="${1:-$(dirname "$0")/../crates/gwc-worker/build/index_bg.wasm}"
[ -f "$wasm" ] || { echo "wasm-size: $wasm not found: build first" >&2; exit 2; }
size=$(wc -c < "$wasm" | tr -d ' ')
if [ "$size" -gt "$max_bytes" ]; then
  echo "wasm-size: $wasm is $size bytes, over the $max_bytes budget (ADR-0026)" >&2
  exit 1
fi
echo "wasm-size: $size bytes, budget $max_bytes"
