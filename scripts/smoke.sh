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
log="$(mktemp)"

# A foreign server on the port would answer the probes below and the test
# would check the wrong service, so refuse to start rather than guess.
if curl -s -o /dev/null "http://127.0.0.1:$port/" 2>/dev/null; then
  echo "port $port is already in use; set SMOKE_PORT" >&2
  exit 1
fi

wrangler d1 migrations apply DB --local >/dev/null

wrangler dev --port "$port" --ip 127.0.0.1 >"$log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; rm -f "$log"' EXIT

# The first run compiles the wasm, which takes far longer than a warm start.
for _ in $(seq 1 180); do
  if curl -fsS -o /dev/null "http://127.0.0.1:$port/healthz" 2>/dev/null; then
    break
  fi
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "wrangler dev exited early:" >&2
    cat "$log" >&2
    exit 1
  fi
  sleep 1
done

body="$(curl -fsS "http://127.0.0.1:$port/healthz")" || {
  echo "/healthz did not answer 200:" >&2
  cat "$log" >&2
  exit 1
}
echo "/healthz -> $body"

case "$body" in
  *'"status":"ok"'*'"database":"ok"'*) echo "smoke: ok" ;;
  *) echo "smoke: unexpected body" >&2; exit 1 ;;
esac
