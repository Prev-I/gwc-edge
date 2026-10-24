# ADR-0009: One structured log line per request

**Status:** accepted, 2026-10-23.

## Context
Workers Logs keeps 200,000 events a day for 7 days on the Free plan. It is the
debugging channel; D1 (`request_log`, `/stats`) is the durable record.

## Decision
- Exactly one JSON line per request, on every path: `kind`, `client`,
  `version`, `scheme`, `outcome` (`ok`, `rate_limited`, `denied`, `degraded`,
  `malformed`), `hosts_sent`, `caches_sent`, `d1_ms`, `cf_colo`, `snapshot`
  when a hostfile was needed, and `update` (what each part of an update did,
  as labels) for updates. Non-protocol routes use log-only kinds (`page`,
  `stats`, `healthz`, `robots`, `redirect`, `method_not_allowed`, `not_found`,
  `config_error`, `internal_error`).
- No line carries the requester's address.
- D1 errors get an extra line at error level with the statement's name, never
  the statement's text (the error string is D1's own and may quote a
  fragment). Other events that are not requests (`rows_dropped`,
  `snapshot_unparsable`, `count_rows_written`, `internal_error`, `cron_failed`)
  are warn or error lines keyed by `event`, never `kind`, and carry no
  `outcome`. The cron writes one line per run.
- `outcome: degraded` means the cache could not give its full reply: a D1
  failure, a copy remembered after one, a configuration error or an internal
  error. Each such reply counts in `degraded_responses_24h`.
- `head_sampling_rate` 1 and invocation logs on.

## Consequences
- A panic writes no request line: `console_error_panic_hook` logs it, and wasm
  has no unwinding to catch it. Neither do the two practically unreachable
  failures before routing (an unparsable request URL or `Host` header).
- About two events per request (the invocation and the line): the Free
  allowance covers about 100,000 requests a day, the request ceiling itself.
  Beyond it, logs are dropped, not requests.
