#!/usr/bin/env bash
# End-to-end tests: start the Worker under `wrangler dev` with a fresh,
# isolated local D1, seed it, run the cron once, then run the Rust test
# client's integration tests (crates/gwc-testclient/tests) against it. A
# second Worker with no D1 binding (wrangler.no-d1.jsonc) exercises the
# degraded mode.
#
# The verifying cron reaches fake hubs and caches (the `fake_peer` binary)
# through TEST_DIAL, which maps a few test addresses to loopback ports and
# blocks every other target (ADR-0012).
#
# Usage: scripts/integration.sh      (needs mise tools on PATH)
# Env:   GWC_PORT       port for wrangler dev (default 18789); the degraded
#                       Worker uses the next port
#        GWC_FAKE_PORT  first of the fake peers' 8 ports (default GWC_PORT + 10)
set -euo pipefail

cd "$(dirname "$0")/.."
port="${GWC_PORT:-18789}"
base="http://127.0.0.1:$port/"
degraded_port=$((port + 1))
degraded_base="http://127.0.0.1:$degraded_port/"
fake_port="${GWC_FAKE_PORT:-$((port + 10))}"
# What the cron dials instead of each target. Every other hub and cache is
# blocked, which counts as a failed check.
test_dial="4.2.2.1=127.0.0.1:$fake_port"                  # a hub with room
test_dial+=",4.2.5.1=127.0.0.1:$((fake_port + 1))"       # a full hub, with X-Try-Hubs
test_dial+=",4.2.6.1=127.0.0.1:$((fake_port + 2))"       # a leaf
test_dial+=",4.2.7.1=127.0.0.1:$((fake_port + 4))"       # nothing listening
test_dial+=",4.2.8.1=127.0.0.1:$((fake_port + 5))"       # never answers
test_dial+=",4.2.9.1=127.0.0.1:$((fake_port + 6))"       # closes mid-header
test_dial+=",4.2.10.1=127.0.0.1:$fake_port"              # a dead seed that comes back
# Caches that pong: dkac and ping.example.net are fetched (port 80), the
# other is pinged over a socket (port 6348); a redirect is not followed.
test_dial+=",dkac.trillinux.org=127.0.0.1:$((fake_port + 3))"
test_dial+=",ping.example.net=127.0.0.1:$((fake_port + 3))"
test_dial+=",sock.example.net=127.0.0.1:$((fake_port + 3))"
test_dial+=",redirect.example.net=127.0.0.1:$((fake_port + 7))"
# The advertised URL, passed explicitly so the tests do not depend on the
# value a deployer puts in wrangler.jsonc.
self_url="http://gwc.example.org/"
state="$(mktemp -d)"   # isolated D1 state: never touches .wrangler/state
# The degraded Worker's own state: rate-limiter counts would otherwise carry
# over between runs in .wrangler/state.
degraded_state="$(mktemp -d)"
log="$(mktemp)"
degraded_log="$(mktemp)"
pid=""
degraded_pid=""
fake_pid=""
cleanup() {
  for p in "$pid" "$degraded_pid" "$fake_pid"; do
    [ -n "$p" ] && { kill "$p" 2>/dev/null || true; wait "$p" 2>/dev/null || true; }
  done
  rm -rf "$log" "$degraded_log" "$state" "$degraded_state"
}
trap cleanup EXIT
curl_opts=(--silent --connect-timeout 2 --max-time 5)

fail() {
  echo "integration: $1" >&2
  echo "--- wrangler dev log ---" >&2
  cat "$log" >&2
  echo "--- degraded wrangler dev log ---" >&2
  cat "$degraded_log" >&2
  exit 1
}

for b in "$base" "$degraded_base"; do
  if curl "${curl_opts[@]}" -o /dev/null "$b"; then
    echo "$b is already in use; set GWC_PORT" >&2
    exit 1
  fi
done
for p in $(seq "$fake_port" $((fake_port + 7))); do
  if (exec 3<>"/dev/tcp/127.0.0.1/$p") 2>/dev/null; then
    echo "port $p is already in use; set GWC_FAKE_PORT" >&2
    exit 1
  fi
done

# Compile the test client first: the seed timestamps below are relative to
# now, and the tests check ages, so nothing slow may run between seeding and
# testing except starting the Worker.
cargo test -p gwc-testclient --tests --no-run >/dev/null \
  || { echo "integration: test client does not compile" >&2; exit 1; }
cargo build -q -p gwc-testclient --bin fake_peer \
  || { echo "integration: fake_peer does not compile" >&2; exit 1; }
target/debug/fake_peer "$fake_port" >/dev/null 2>&1 &
fake_pid=$!

wrangler d1 migrations apply DB --local --persist-to "$state" >/dev/null

now="$(date +%s)"
wrangler d1 execute DB --local --persist-to "$state" --command "
  -- last_checked and failures keep the cron's queue away from the listed
  -- seeds for 90 minutes, so the tests never depend on how long they take.
  INSERT INTO hubs (ip, port, status, source, vendor, version, leaves, max_leaves, up_since, first_seen, last_seen, last_checked, failures) VALUES
    ('1.1.1.1', 6347, 'alive_full', 'update', NULL, NULL, NULL, NULL, NULL, $now, $((now - 30)), $now, 0),
    ('8.8.4.4', 6346, 'verified',   'update', 'RAZA', '2.7.10.6', 120, 300, $((now - 9000)), $((now - 9000)), $((now - 120)), $now, 0),
    ('9.9.9.9', 6346, 'unverified', 'update', NULL, NULL, NULL, NULL, NULL, $now, $now, NULL, 0),
    ('8.8.8.8', 6346, 'verified',   'update', NULL, NULL, NULL, NULL, NULL, $((now - 9000)), $((now - 8000)), $now, 0),
    ('9.9.9.10', 6346, 'dead',      'update', NULL, NULL, NULL, NULL, NULL, $((now - 90000)), $((now - 86000)), $((now - 60)), 3),
    ('4.2.10.1', 6346, 'dead',      'update', NULL, NULL, NULL, NULL, NULL, $((now - 90000)), $((now - 90000)), $((now - 700)), 1),
    ('4.2.11.1', 6346, 'dead',      'update', NULL, NULL, NULL, NULL, NULL, $((now - 300000)), $((now - 200000)), $((now - 200000)), 4);
  INSERT INTO caches (url, host, status, first_seen, last_verified) VALUES
    ('http://dkac.trillinux.org/dkac/dkac.php', 'dkac.trillinux.org', 'verified', $now, $((now - 600))),
    ('http://pending.example.org/', 'pending.example.org', 'pending', $now, NULL);
" >/dev/null

# Wait until a freshly started wrangler answers /healthz with $2.
await_health() {
  local url=$1 want=$2 child=$3 code=000 deadline=$((SECONDS + 180))
  while ((SECONDS < deadline)); do
    code="$(curl "${curl_opts[@]}" -o /dev/null -w '%{http_code}' "${url}healthz" || true)"
    [ "$code" != 000 ] && break
    kill -0 "$child" 2>/dev/null || fail "wrangler dev ($url) exited early"
    sleep 1
  done
  [ "$code" = "$want" ] || fail "$url/healthz answered $code, expected $want"
  kill -0 "$child" 2>/dev/null || fail "$url answered, but not from the wrangler just started"
}

# Start the Worker in a fresh process, so its isolate remembers nothing.
# --test-scheduled exposes /__scheduled, to run the cron on demand.
start_worker() {
  wrangler dev --port "$port" --ip 127.0.0.1 --persist-to "$state" --test-scheduled \
    --var "CANONICAL_URL:$self_url" --var "TEST_DIAL:$test_dial" >>"$log" 2>&1 &
  pid=$!
  await_health "$base" 200 "$pid"
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

# The degraded Worker first: both Workers build into the same directory, so
# it finishes building before the main one starts, and the main one's later
# restart only reloads it (harmless: it holds no state). It has no D1 binding
# at all, so its /healthz is 503.
wrangler dev -c wrangler.no-d1.jsonc --port "$degraded_port" --ip 127.0.0.1 \
  --persist-to "$degraded_state" --var "CANONICAL_URL:$self_url" >>"$degraded_log" 2>&1 &
degraded_pid=$!
await_health "$degraded_base" 503 "$degraded_pid"

start_worker

# Before the first cron run there are no numbers: the landing page says so
# and is not cached.
canonical_host="${self_url#http://}"
canonical_host="${canonical_host%/}"
for attempt in 1 2; do
  headers="$(curl "${curl_opts[@]}" -o /dev/null -D - -H "Host: $canonical_host" "$base")" \
    || fail "landing request failed"
  grep -qi '^x-gwc-cache: miss' <<<"$headers" \
    || fail "landing page before the first cron run was cached (attempt $attempt)"
done

# Before the first cron run there is no stored snapshot: the hostfile must
# come from the live fallback, which writes the snapshot back.
body="$(curl "${curl_opts[@]}" "${base}?get=1&hostfile=1&net=gnutella2&ping=1&client=RAZA2.7.10.6")" \
  || fail "hostfile request failed"
grep -q '^h|8.8.4.4:6346|' <<<"$body" || fail "no hosts from the live fallback before the first cron run"
health="$(curl "${curl_opts[@]}" "${base}healthz")" || fail "healthz request failed"
grep -q '"hostfile":' <<<"$health" || fail "live fallback did not write the snapshot back: $health"

curl "${curl_opts[@]}" "${base}__scheduled?cron=*/10+*+*+*+*" >/dev/null || fail "cron trigger failed"
# /__scheduled answers 200 whatever the cron did: prove it ran. Only the
# cron writes the stats and cron documents.
health="$(curl "${curl_opts[@]}" "${base}healthz")" || fail "healthz request failed"
grep -q '"stats":' <<<"$health" || fail "cron did not render stats: $health"
grep -q '"cron":"ok"' <<<"$health" || fail "cron heartbeat missing: $health"
grep -q '"test_dial":true' <<<"$health" || fail "healthz does not report TEST_DIAL: $health"
kill -0 "$fake_pid" 2>/dev/null || fail "fake_peer is not running"
grep -q '"event":"cron_failed"' "$log" && fail "cron logged a failure"
grep -q '"verification_failed":true' "$log" && fail "the cron's verification failed"

# A fresh isolate, so the tests read the stored snapshot rather than the copy
# the live fallback left in memory.
stop_worker
start_worker

# The main Worker's restart rebuilt the shared build directory; make sure the
# degraded Worker has finished reloading before its tests run.
await_health "$degraded_base" 503 "$degraded_pid"

# One test thread: the log test counts lines, and the edge-cache tests rely
# on request order.
GWC_BASE_URL="$base" GWC_CANONICAL_URL="$self_url" GWC_SEED_AT="$now" \
GWC_DEGRADED_URL="$degraded_base" GWC_LOG_FILE="$log" GWC_DEGRADED_LOG_FILE="$degraded_log" \
  cargo test -p gwc-testclient --tests -- --ignored --test-threads=1 \
  || fail "integration tests failed"
echo "integration: ok"
