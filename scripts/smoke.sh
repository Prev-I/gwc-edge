#!/usr/bin/env bash
# Start the Worker under `wrangler dev` with a local D1, apply migrations, and
# check that /healthz answers 200 with a working database.
#
# Usage: scripts/smoke.sh            (run from anywhere; needs mise tools on PATH)
# Env:   SMOKE_PORT  port for wrangler dev (default 18787: 8787, wrangler's own
#                    default, is often taken by other local tools)
set -euo pipefail

cd "$(dirname "$0")/.."
port="${SMOKE_PORT:-18787}"
url="http://127.0.0.1:$port/healthz"
log="$(mktemp)"
# Every probe is bounded, so a server that accepts and then stalls cannot hang
# the script (or a CI job) indefinitely.
curl_opts=(--silent --connect-timeout 2 --max-time 5)

# A foreign server on the port would answer the probes below and the test
# would check the wrong service, so refuse to start rather than guess.
if curl "${curl_opts[@]}" -o /dev/null "http://127.0.0.1:$port/"; then
  echo "port $port is already in use; set SMOKE_PORT" >&2
  exit 1
fi

wrangler d1 migrations apply DB --local >/dev/null

wrangler dev --port "$port" --ip 127.0.0.1 >"$log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; rm -f "$log"' EXIT

fail() {
  echo "smoke: $1" >&2
  echo "--- wrangler dev log ---" >&2
  cat "$log" >&2
  exit 1
}

# Wait for any HTTP answer, not just a 200: a running but degraded Worker
# should fail now, not after the whole wait. The first run compiles the wasm,
# which takes far longer than a warm start.
code=000
deadline=$((SECONDS + 180))
while ((SECONDS < deadline)); do
  code="$(curl "${curl_opts[@]}" -o /dev/null -w '%{http_code}' "$url" || true)"
  [ "$code" != 000 ] && break
  kill -0 "$pid" 2>/dev/null || fail "wrangler dev exited early"
  sleep 1
done
[ "$code" != 000 ] || fail "no answer from $url within 180 s"

body="$(curl "${curl_opts[@]}" -w '\n%{http_code}' "$url")" || fail "/healthz request failed"
code="${body##*$'\n'}"
body="${body%$'\n'*}"
echo "/healthz -> $code $body"

[ "$code" = 200 ] || fail "/healthz answered $code, expected 200"
for field in '"status":"ok"' '"database":"ok"'; do
  [[ "$body" == *"$field"* ]] || fail "/healthz body lacks $field"
done
echo "smoke: ok"
