# ADR-0005: Rate limiting uses the Workers Rate Limiting binding

**Status:** accepted, 2026-10-23; implemented with the write path.

## Context
The brief asks for per-source limits (1 get per 30 s, 1 update per 10 min).
Storing them in D1 costs a row write per request; the Cache API is per data
centre, not atomic, and absent on `*.workers.dev`.

## Decision
- Gets are limited with the Workers Rate Limiting binding (`env.rate_limiter`,
  available in `worker` 0.8.7), 2 per 60 s, keyed by source and request kind
  (hostfile and urlfile separately, because Shareaza sends both back to back),
  IPv6 grouped by /64. Fail-open: if the binding is unavailable, allow.
- Updates from a hub's own address are coalesced through the hub row: one
  write per hub per 30 minutes at most.
- No `rate_limits` table.

## Consequences
- Rate limiting costs no D1 writes.
- Limits are approximate and per data centre; that is enough for abuse
  control, and over-limit clients still get a successful reply (ADR-0002).
