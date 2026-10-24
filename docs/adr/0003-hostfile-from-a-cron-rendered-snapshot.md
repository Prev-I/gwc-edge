# ADR-0003: Gets are served from a cron-rendered snapshot

**Status:** accepted, 2026-10-23.

## Context
The Free plan allows 100,000 requests and 5,000,000 D1 rows read a day: 50 rows
per request at the request ceiling. Querying `hubs` per get would read about 95
rows, so a flood would exhaust reads before requests, and the hostfile itself
would fail.

## Decision
- The cron (every 10 minutes) renders the servable hubs and caches into one
  row, `snapshots.hostfile`, holding timestamps; ages, uptime and the 2-hour
  window are computed when a request is served.
- A get reads that one row, or nothing while its isolate remembers it (60 s).
- If the row is missing or older than 30 minutes (cron stalled), a get reads
  the tables live and writes the snapshot back, guarded on the stale value it
  read, so concurrent isolates write once and later requests read it again.
- `/stats` and the landing page read only cron-rendered documents.

## Consequences
- A get costs at most 1 row read; a stalled cron costs about one table scan per
  30 minutes overall.
- `hubs` needs no secondary index, which halves hub writes.
- New hubs appear in hostfiles up to 10 minutes after the cron sees them.
