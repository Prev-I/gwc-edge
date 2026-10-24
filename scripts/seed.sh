#!/usr/bin/env bash
# One-shot seeding of a new cache from another one, dkac by default
# (docs/field-test-plan.md, stage 1). In two steps, so what you review is
# what gets stored:
#
#   scripts/seed.sh fetch seed.sql   ask the source once for its G2 hosts and
#                                    caches; write the SQL to seed.sql
#   scripts/seed.sh apply seed.sql   store exactly that file in the deployed D1
#   scripts/seed.sh apply-local seed.sql <persist-dir>   ... in a local D1 (tests)
#
# Hubs are stored `unverified` and caches `pending`: nothing is served before
# the cron has checked it, over the next hour of runs for a full seed. Caps
# are enforced in the SQL itself; rows already known are left alone. `fetch`
# fails, writing nothing, when the source offers no hub.
#
# Env: GWC_CANONICAL_URL  required for fetch: this cache, never stored as its
#                         own peer
#      GWC_SEED_FROM      source cache (default dkac)
#      CLOUDFLARE_API_TOKEN / CLOUDFLARE_ACCOUNT_ID, or `wrangler login`, for apply
set -euo pipefail

usage() {
  echo "usage: scripts/seed.sh fetch <file> | apply <file> | apply-local <file> <persist-dir>" >&2
  exit 2
}
case "${1:-}:$#" in
  fetch:2 | apply:2 | apply-local:3) ;;
  *) usage ;;
esac
# Paths are relative to where the script is run from, not the repository.
absolute() { case "$1" in /*) echo "$1" ;; *) echo "$PWD/$1" ;; esac; }
cmd=$1
file="$(absolute "$2")"
dir=""
if [ "$cmd" = apply-local ]; then
  [ -n "$3" ] || usage
  dir="$(absolute "$3")"
fi

cd "$(dirname "$0")/.."
# shellcheck source=scripts/lib.sh
. scripts/lib.sh

case "$cmd" in
  fetch)
    : "${GWC_CANONICAL_URL:?set GWC_CANONICAL_URL to the canonical URL of this cache}"
    args=(--self "$GWC_CANONICAL_URL")
    [ -n "${GWC_SEED_FROM:-}" ] && args+=(--from "$GWC_SEED_FROM")
    tmp="$(mktemp)"
    trap 'rm -f "$tmp"' EXIT
    cargo run -q -p gwc-testclient --bin gwc_seed -- "${args[@]}" >"$tmp"
    mv "$tmp" "$file"
    echo "seed: wrote $file; review it, then: scripts/seed.sh apply $file"
    ;;
  apply-local)
    [ -s "$file" ] || { echo "seed: $file is empty or missing" >&2; exit 1; }
    wrangler d1 execute DB --local --persist-to "$dir" --file "$file" >/dev/null
    echo "seed: stored in the local D1 at $dir"
    ;;
  apply)
    [ -s "$file" ] || { echo "seed: $file is empty or missing" >&2; exit 1; }
    name="$(d1_name)"
    id="$(d1_id "$name")"
    [ -n "$id" ] || { echo "seed: no remote D1 database named $name; deploy first" >&2; exit 1; }
    # A config naming the database by id, like the deploy's, rendered here
    # rather than trusting a wrangler.deploy.json a dry run may have left.
    cfg="./.seed-wrangler-$$.json"
    trap 'rm -f "$cfg"' EXIT
    node scripts/render-config.mjs wrangler.jsonc "$cfg" \
      --canonical-url "${GWC_CANONICAL_URL:-http://placeholder.example.org/}" --database-id "$id"
    # --command, not --file: a remote --file goes through D1's import, which
    # briefly makes the database unavailable to the running Worker.
    wrangler d1 execute DB --remote -c "$cfg" --command "$(cat "$file")"
    echo "seed: stored; the cron checks the hubs and caches over its next runs"
    ;;
esac
