#!/usr/bin/env bash
# Fuzz gwc-core's parsers with cargo-fuzz, on the dated nightly pinned in
# mise.toml. Corpora live in fuzz/corpus/<target> and are committed; new
# inputs found during a run are added there, minimised with `--min`.
#
# Usage: scripts/fuzz.sh [seconds-per-target] [target...]
#        scripts/fuzz.sh --replay        run every committed input once (CI)
#        scripts/fuzz.sh --min           minimise the corpora in place
set -euo pipefail

cd "$(dirname "$0")/.."
nightly="$(grep -o 'nightly-[0-9-]*' mise.toml | head -1)"
all=(query handshake cache_url ping_reply)
cd fuzz

case "${1:-}" in
  --replay)
    for t in "${all[@]}"; do
      cargo "+$nightly" fuzz run "$t" "corpus/$t" -- -runs=0
    done
    ;;
  --min)
    for t in "${all[@]}"; do
      cargo "+$nightly" fuzz cmin "$t" "corpus/$t"
    done
    ;;
  *)
    seconds="${1:-60}"
    [[ "$seconds" =~ ^[0-9]+$ ]] || { echo "usage: scripts/fuzz.sh [seconds] [target...] | --replay | --min" >&2; exit 2; }
    shift || true
    targets=("$@")
    [ ${#targets[@]} -gt 0 ] || targets=("${all[@]}")
    for t in "${targets[@]}"; do
      echo "fuzz: $t for ${seconds}s"
      cargo "+$nightly" fuzz run "$t" "corpus/$t" -- -max_total_time="$seconds" -max_len=4096
    done
    ;;
esac
