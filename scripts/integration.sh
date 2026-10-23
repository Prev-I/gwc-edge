#!/usr/bin/env bash
# End-to-end tests: start the Worker under `wrangler dev` with a fresh,
# isolated local D1, seed it, run the cron once, then run the Rust test
# client's integration tests (crates/gwc-testclient/tests) against it.
#
# Usage: scripts/integration.sh      (needs mise tools on PATH)
# Env:   GWC_PORT  port for wrangler dev (default 18789)
set -euo pipefail

cd "$(dirname "$0")/.."
port="${GWC_PORT:-18789}"
base="http://127.0.0.1:$port/"
# The advertised URL, passed explicitly so the tests do not depend on the
# value a deployer puts in wrangler.jsonc.
self_url="http://gwc.example.org/"
state="$(mktemp -d)"   # isolated D1 state: never touches .wrangler/state
log="$(mktemp)"
pid=""
trap '[ -n "$pid" ] && { kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; }; rm -rf "$log" "$state"' EXIT
curl_opts=(--silent --connect-timeout 2 --max-time 5)

fail() {
  echo "integration: $1" >&2
  echo "--- wrangler dev log ---" >&2
  cat "$log" >&2
  exit 1
}

if curl "${curl_opts[@]}" -o /dev/null "$base"; then
  echo "port $port is already in use; set GWC_PORT" >&2
  exit 1
fi

# Compile the test client first: the seed timestamps below are relative to
# now, and the tests check ages, so nothing slow may run between seeding and
# testing except starting the Worker.
cargo test -p gwc-testclient --test read_path --no-run >/dev/null \
  || { echo "integration: test client does not compile" >&2; exit 1; }

wrangler d1 migrations apply DB --local --persist-to "$state" >/dev/null

now="$(date +%s)"
wrangler d1 execute DB --local --persist-to "$state" --command "
  INSERT INTO hubs (ip, port, status, source, vendor, version, leaves, max_leaves, up_since, first_seen, last_seen) VALUES
    ('1.1.1.1', 6347, 'alive_full', 'update', NULL, NULL, NULL, NULL, NULL, $now, $((now - 30))),
    ('8.8.4.4', 6346, 'verified',   'update', 'RAZA', '2.7.10.6', 120, 300, $((now - 9000)), $((now - 9000)), $((now - 120))),
    ('9.9.9.9', 6346, 'unverified', 'update', NULL, NULL, NULL, NULL, NULL, $now, $now),
    ('8.8.8.8', 6346, 'verified',   'update', NULL, NULL, NULL, NULL, NULL, $((now - 9000)), $((now - 8000)));
  INSERT INTO caches (url, host, status, first_seen, last_verified) VALUES
    ('http://dkac.trillinux.org/dkac/dkac.php', 'dkac.trillinux.org', 'verified', $now, $((now - 600))),
    ('http://pending.example.org/', 'pending.example.org', 'pending', $now, NULL);
" >/dev/null

# Start the Worker in a fresh process, so its isolate remembers nothing.
# --test-scheduled exposes /__scheduled, to run the cron on demand.
start_worker() {
  wrangler dev --port "$port" --ip 127.0.0.1 --persist-to "$state" --test-scheduled \
    --var "SELF_URL:$self_url" >>"$log" 2>&1 &
  pid=$!
  local code=000 deadline=$((SECONDS + 180))
  while ((SECONDS < deadline)); do
    code="$(curl "${curl_opts[@]}" -o /dev/null -w '%{http_code}' "${base}healthz" || true)"
    [ "$code" != 000 ] && break
    kill -0 "$pid" 2>/dev/null || fail "wrangler dev exited early"
    sleep 1
  done
  [ "$code" = 200 ] || fail "/healthz answered $code"
  kill -0 "$pid" 2>/dev/null || fail "/healthz answered, but not from the wrangler just started"
}

# Stop it and wait until the port is free, so the next start cannot be
# answered by a leftover process and its remembered state.
stop_worker() {
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  pid=""
  local deadline=$((SECONDS + 30))
  while curl "${curl_opts[@]}" -o /dev/null "${base}healthz"; do
    ((SECONDS < deadline)) || fail "port $port still answering after stop"
    sleep 1
  done
}

start_worker

# Before the first cron run there is no stored snapshot: the hostfile must
# come from the live fallback, which writes the snapshot back.
body="$(curl "${curl_opts[@]}" "${base}?get=1&hostfile=1&net=gnutella2&ping=1&client=RAZA2.7.10.6")" \
  || fail "hostfile request failed"
grep -q '^h|8.8.4.4:6346|' <<<"$body" || fail "no hosts from the live fallback before the first cron run"
health="$(curl "${curl_opts[@]}" "${base}healthz")" || fail "healthz request failed"
grep -q '"hostfile":' <<<"$health" || fail "live fallback did not write the snapshot back: $health"

curl "${curl_opts[@]}" "${base}__scheduled?cron=*/10+*+*+*+*" >/dev/null || fail "cron trigger failed"
# /__scheduled answers 200 whatever the cron did: prove it ran. Only the
# cron writes the stats document.
health="$(curl "${curl_opts[@]}" "${base}healthz")" || fail "healthz request failed"
grep -q '"stats":' <<<"$health" || fail "cron did not render stats: $health"
grep -q '"event":"cron_failed"' "$log" && fail "cron logged a failure"

# A fresh isolate, so the tests read the stored snapshot rather than the copy
# the live fallback left in memory.
stop_worker
start_worker

GWC_BASE_URL="$base" GWC_SELF_URL="$self_url" GWC_SEED_AT="$now" \
  cargo test -p gwc-testclient --test read_path -- --ignored \
  || fail "integration tests failed"
echo "integration: ok"
