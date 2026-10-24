# ADR-0004: Request counters are sampled, 1 in 8

**Status:** accepted, 2026-10-23.

## Context
`/stats` reports requests by kind, scheme and client over 24 hours. Counting
every request costs one D1 row written per request, which at the request
ceiling is the whole daily write budget, and a client inventing new
client/version keys makes it unbounded.

## Decision
One request in 8, chosen at random, writes `n = n + 8` into its hourly
`request_log` row, after the response. `clients_seen` is derived from
`request_log` by the cron, never written on the request path. Health events
(`d1_error`, `degraded`) are counted exactly (ADR-0007).

## Consequences
- At most 12,500 counter writes a day at the request ceiling, whatever the keys.
- Counts are unbiased estimates; rare client/version pairs are under-represented.
  `/stats` says so (`sample_rate`).
- Upgrade path if exact counts are ever needed: a Durable Object aggregator.
