# ADR-0001: D1, not KV, holds all state

**Status:** accepted, 2026-10-23.

## Context
The cache stores hubs, caches, counters and rendered documents, on the
Cloudflare Workers Free plan. Workers KV allows 1,000 writes a day on Free;
D1 allows 100,000 rows written and 5,000,000 rows read a day, with SQL,
transactions (`batch`) and conditional writes. Hub updates alone are expected
at 1,000–2,000 a day.

## Decision
All state lives in one D1 database. KV is not used.

## Consequences
- Every write is budgeted in rows; `crates/gwc-worker/src/store.rs` holds every
  statement and its cost, and migration 0001 states the rules.
- Exceeding a daily limit makes D1 fail every query until 00:00 UTC (enforced
  since 2026-09-01): see ADR-0003, ADR-0004 and ADR-0007 for how that is kept
  from happening and survived when it does.
