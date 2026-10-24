#!/usr/bin/env bash
# Deploy gwc-edge to Cloudflare (ADR-0015). Used by the deploy workflow and by
# hand (README, "Deploying"). The committed wrangler.jsonc stays a template;
# this renders wrangler.deploy.json with the D1 database id, the canonical URL
# and its Custom Domain route, then:
#   1. finds the D1 database by name, or creates it;
#   2. applies the migrations, before the code that needs them goes live;
#   3. deploys;
#   4. checks the result as Shareaza sees it (scripts/smoke.sh --prod).
#
# Usage: GWC_CANONICAL_URL=http://gwc.example.org/ scripts/deploy.sh
#        GWC_CANONICAL_URL=... scripts/deploy.sh --dry-run   (render and bundle only)
# Any other argument is refused, so a mistyped --dry-run never deploys.
# Env:   GWC_CANONICAL_URL      required: the URL clients will store
#        CLOUDFLARE_API_TOKEN, CLOUDFLARE_ACCOUNT_ID
#                               in CI; locally, `wrangler login` works too
#        GWC_SKIP_SMOKE=1       skip step 4
set -euo pipefail

dry_run=0
case "$#:${1:-}" in
  0:) ;;
  1:--dry-run) dry_run=1 ;;
  *) echo "usage: GWC_CANONICAL_URL=<url> scripts/deploy.sh [--dry-run]" >&2; exit 2 ;;
esac

cd "$(dirname "$0")/.."
# shellcheck source=scripts/lib.sh
. scripts/lib.sh
canonical="${GWC_CANONICAL_URL:?set GWC_CANONICAL_URL to the public http:// URL of the cache}"
out="wrangler.deploy.json"
name="$(d1_name)"

# Never reuse an earlier render (a dry run's placeholder id, say).
render() {
  rm -f "$out"
  node scripts/render-config.mjs wrangler.jsonc "$out" \
    --canonical-url "$canonical" --database-id "$1"
  [ -s "$out" ] || { echo "deploy: $out was not rendered" >&2; exit 1; }
}

if ((dry_run)); then
  render 00000000-0000-0000-0000-000000000000
  wrangler deploy --dry-run -c "$out" --outdir target/wrangler-dist
  echo "deploy: dry run ok ($out)"
  exit 0
fi

# 1. The database, by name: created on the first deploy only.
id="$(d1_id "$name")"
if [ -z "$id" ]; then
  echo "deploy: creating D1 database $name"
  # wrangler may offer to write the new id into wrangler.jsonc, which must
  # stay a template (ADR-0015): put it back as it was, whatever happens.
  template="$(mktemp)"
  cp wrangler.jsonc "$template"
  trap 'cmp -s "$template" wrangler.jsonc || cp "$template" wrangler.jsonc; rm -f "$template"' EXIT
  wrangler d1 create "$name" >/dev/null </dev/null
  id="$(d1_id "$name")"
fi
[ -n "$id" ] || { echo "deploy: no D1 database $name" >&2; exit 1; }
render "$id"

# 2. Migrations first: a new build may read a column the old schema lacks.
wrangler d1 migrations apply DB --remote -c "$out"

# 3. The Worker, its cron trigger and its Custom Domain.
wrangler deploy -c "$out"

# 4. As Shareaza sees it. A fresh Custom Domain can take a minute to answer.
# Until the cron's first run (within 10 minutes of the first deploy, however
# many attempts that took), /healthz reports it as never run: accepted here.
# The daily prod-smoke check stays strict and catches a cron that never runs.
if [ "${GWC_SKIP_SMOKE:-}" != 1 ]; then
  for attempt in 1 2 3 4 5 6; do
    rc=0
    SMOKE_ALLOW_NO_CRON=1 scripts/smoke.sh --prod "$canonical" || rc=$?
    ((rc == 0)) && exit 0
    ((rc == 2)) && exit 2   # a usage error: retrying cannot help
    ((attempt < 6)) || break
    echo "deploy: smoke attempt $attempt failed; retrying in 20 s" >&2
    sleep 20
  done
  echo "deploy: deployed, but the production checks fail (README, \"Zone settings\")" >&2
  exit 1
fi
