#!/usr/bin/env bash
# Start the Worker under `wrangler dev` with a fresh, isolated local D1, apply
# migrations, and check that /healthz answers 200 with a working database.
#
# With --prod, check a deployed cache instead, as Shareaza sees it: the four
# request shapes it sends, with its headers (no Accept, no Accept-Encoding).
# Each reply must be HTTP 200 with a reason phrase, text/plain, not
# compressed, and every get must end with the cache's own `u|<url>|0` line.
# That catches zone settings that break Shareaza (an HTTPS redirect, a
# challenge, compression) as well as the cache itself (README, "Zone
# settings"). Nothing is stored: the update comes from another address, and
# the submitted URL is the cache's own.
#
# The local run ends with the same --prod checks against the local Worker,
# reached through curl's --connect-to, so CI exercises them too.
#
# Usage: scripts/smoke.sh                  (needs mise tools on PATH)
#        scripts/smoke.sh --prod <canonical URL>
# Env:   SMOKE_PORT        port for wrangler dev (default 18787: 8787,
#                          wrangler's own default, is often taken by other
#                          local tools)
#        SMOKE_CONNECT_TO  --prod only: curl --connect-to value, to reach the
#                          canonical host at another address
#        SMOKE_ALLOW_NO_CRON=1
#                          --prod only, right after a deploy: accept /healthz
#                          reporting the cron as not run yet (a new database)
# Needs bash 4 or later (macOS: install one, e.g. with mise or Homebrew).
set -euo pipefail

cd "$(dirname "$0")/.."

prod() {
  local url=$1 failed=0 origin connect=()
  origin="$(sed -E 's#^(http://[^/]+).*#\1#' <<<"$url")"
  [ -n "${SMOKE_CONNECT_TO:-}" ] && connect=(--connect-to "$SMOKE_CONNECT_TO")
  # "${connect[@]}" on an empty array trips set -u before bash 4.4.
  connect+=(--silent)
  [[ "$url" == http://*/ || "$url" == http://*/*  ]] \
    || { echo "smoke --prod: expected the canonical http:// URL, got $url" >&2; exit 2; }
  local enc
  enc="$(node -e 'process.stdout.write(encodeURIComponent(process.argv[1]))' "$url")"
  local client="client=RAZA2.7.10.6"
  local tail="&getleaves=1&getnetworks=1&getclusters=0&getvendors=1&getuptime=1"
  local shapes=(
    "hostfile|?get=1&hostfile=1&net=gnutella2&ping=1&pv=4&$client$tail"
    "urlfile|?get=1&urlfile=1&net=gnutella2&ping=1&pv=4&$client$tail"
    "update|?ip=198.51.100.7:6346&x.leaves=0&uptime=7300&x.max=300&update=1&net=gnutella2&$client"
    "submit|?url=$enc&update=1&net=gnutella2&$client"
  )
  for shape in "${shapes[@]}"; do
    local name=${shape%%|*} query=${shape#*|} headers body
    headers="$(mktemp)"
    if ! body="$(curl --silent --http1.1 --connect-timeout 5 --max-time 15 \
        -H 'User-Agent: Shareaza 2.7.10.6' -H 'Accept:' -H 'Accept-Encoding:' \
        "${connect[@]}" -D "$headers" "${url}${query}")"; then
      echo "FAIL $name: request failed"; failed=1; rm -f "$headers"; continue
    fi
    local status ctype
    status="$(head -1 "$headers" | tr -d '\r')"
    ctype="$(grep -i '^content-type:' "$headers" | tr -d '\r' || true)"
    local problems=()
    [[ "$status" =~ ^HTTP/1\.[01]\ 200\ [^[:space:]] ]] || problems+=("status line '$status' (need 200 with a reason phrase)")
    [[ "$(tr '[:upper:]' '[:lower:]' <<<"$ctype")" == *text/plain* ]] || problems+=("content type '${ctype:-none}'")
    grep -qi '^content-encoding:' "$headers" && problems+=("compressed: $(grep -i '^content-encoding:' "$headers" | tr -d '\r')")
    case "$name" in
      hostfile|urlfile)
        [ "$(printf '%s' "$body" | tail -1)" = "u|${url}|0" ] || problems+=("last line is not u|${url}|0")
        ;;
      *)
        grep -qx 'i|update|OK' <<<"$body" || problems+=("no i|update|OK")
        ;;
    esac
    rm -f "$headers"
    if ((${#problems[@]})); then
      failed=1
      echo "FAIL $name: ${problems[*]}"
      echo "  body: $(head -c 300 <<<"$body" | tr '\n' ' ')"
    else
      echo "ok   $name"
    fi
  done
  local health
  health="$(curl --silent --max-time 15 "${connect[@]}" -w '\n%{http_code}' "$origin/healthz" || true)"
  local fresh_ok=0
  # Before the cron's first run /healthz says 503 with cron never_run (or
  # pending) while all else is fine.
  [[ "${SMOKE_ALLOW_NO_CRON:-0}" == 1 && "${health##*$'\n'}" == 503 \
    && "$health" == *'"database":"ok"'* && "$health" == *'"config":"ok"'* \
    && ( "$health" == *'"cron":"never_run"'* || "$health" == *'"cron":"pending"'* ) ]] && fresh_ok=1
  if [[ "$health" == *'"test_dial":false'* ]] && \
     { [[ "${health##*$'\n'}" == 200 && "$health" == *'"status":"ok"'* ]] || ((fresh_ok)); }; then
    echo "ok   healthz$( ((fresh_ok)) && echo ' (the cron has not run yet)')"
  else
    failed=1
    echo "FAIL healthz: ${health//$'\n'/ }"
  fi
  if ((failed)); then
    echo "smoke --prod: FAILED for $url" >&2
    return 1
  fi
  echo "smoke --prod: ok"
}

if [ "${1:-}" = --prod ]; then
  prod "${2:?usage: scripts/smoke.sh --prod <canonical URL>}"
  exit $?
fi
port="${SMOKE_PORT:-18787}"
url="http://127.0.0.1:$port/healthz"
log="$(mktemp)"
state="$(mktemp -d)"   # isolated D1 state: never touches .wrangler/state
# Every probe is bounded, so a server that accepts and then stalls cannot hang
# the script (or a CI job) indefinitely.
curl_opts=(--silent --connect-timeout 2 --max-time 5)

# A foreign server on the port would answer the probes below and the test
# would check the wrong service, so refuse to start rather than guess.
if curl "${curl_opts[@]}" -o /dev/null "http://127.0.0.1:$port/"; then
  echo "port $port is already in use; set SMOKE_PORT" >&2
  exit 1
fi

trap 'rm -rf "$log" "$state"' EXIT
wrangler d1 migrations apply DB --local --persist-to "$state" >/dev/null

wrangler dev --port "$port" --ip 127.0.0.1 --persist-to "$state" --test-scheduled >"$log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; rm -rf "$log" "$state"' EXIT

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

# The production checks, against this Worker: the canonical URL from
# wrangler.jsonc, reached on the local port, after one cron run (a hostfile
# served without one makes /healthz report the cron as never run).
cron_code="$(curl "${curl_opts[@]}" --max-time 30 -o /dev/null -w '%{http_code}' \
  "http://127.0.0.1:$port/cdn-cgi/local/scheduled" || true)"
[ "$cron_code" = 200 ] || fail "cron trigger answered $cron_code"
canonical="$(grep -o '"CANONICAL_URL": "[^"]*"' wrangler.jsonc | cut -d'"' -f4)"
host="$(sed -E 's#^http://([^/:]+).*#\1#' <<<"$canonical")"
SMOKE_CONNECT_TO="$host:80:127.0.0.1:$port" prod "$canonical" || fail "--prod checks failed"
echo "smoke: ok"
