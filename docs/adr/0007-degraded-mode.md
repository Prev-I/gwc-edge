# ADR-0007: Degraded mode: the protocol minimum, no further D1 access

**Status:** accepted, 2026-10-23.

## Context
D1 fails on errors, timeouts and, for the rest of the UTC day, on an exhausted
quota. Shareaza counts any non-200, and any get that adds nothing, as a
failure, and drops the cache after two.

## Decision
- Every D1 call made while answering a request goes through a per-request
  probe. The first failure is logged at error level by statement name and
  counted; every later D1 call in that request is skipped. The cron, and the
  best-effort writes after a response (sampled counter, health flush), log
  their own failures and are not counted as request errors.
- The reply is then the protocol minimum: a get gets the pong if requested and
  `u|<canonical URL>|0` (plus any hubs the isolate still remembers); an update
  gets `i|update|period|3600` and `i|update|OK`. Never a 5xx, never an empty
  body for an allowed client.
- D1 errors and degraded replies are counted in isolate memory and flushed to
  `request_log` at most once a minute, in one atomic batch, after a reply that
  was not degraded, so `/stats` shows them (`health`) without reading logs.
- A copy of the hostfile produced while a D1 call failed is remembered only
  for a minute, and replies served from it are degraded too.

## Consequences
- During an outage, clients keep the cache and lose only hubs.
- Health counts are exact but best-effort: an isolate evicted before its next
  flush loses its counts. The per-request log lines (`outcome: degraded`) are
  the complete record for 7 days.
