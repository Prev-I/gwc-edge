# ADR-0010: The write path

**Status:** accepted, 2026-10-24; the "listed at once" and "on the same
port" clauses are superseded by ADR-0011.

## Context
Updates carry a hub registration (`ip=`), a third cache (`url=`), or both.
Every one is answered `OK` (ADR-0002), so storage is a separate decision, and
it must fit the write budget (ADR-0001) and the caps (ADR-0006).

## Decision
- **Each part is judged on its own** (`gwc_core::update`); the reply never
  depends on it.
- **A hub that registers itself is listed at once**, as `verified`, source
  `update`: it proved its address (`CF-Connecting-IP` equals the claimed IP)
  and Shareaza sends updates only in hub mode. The cron re-checks it by
  handshake as it ages. A self-registration also revives a `dead` or
  `unverified` row of the same IP.
- **One write per hub per 30 minutes at most:** a refresh on the same port
  less than 30 minutes after the last one writes nothing. Shareaza updates a
  cache at most hourly, so every real refresh still lands.
- **Updates are rate-limited to 1 per source per 60 s** by the binding
  (ADR-0005); the brief's "1 per 10 minutes" exceeds the binding's longest
  period, and the 30-minute coalescing bounds the writes anyway. A limited
  update is answered `OK` and not processed.
- **A submitted cache is stored `pending`**, at most 3 per host and 50 in
  all, and is not served until the cron has pinged it.
- **Caps read the `counts` row**, incremented in the same batch as each
  insert; the cron sets it to the true table counts on every run, so a lost
  or concurrent increment lasts at most one cron period.
- **The cron forgets its isolate's hostfile copy and purges its data centre's
  cached pages** after writing, so new data shows at once where it can.

## Consequences
- Per update, worst case: 6 rows read (hub row; its counter if new; up to 3
  cache rows of one host through the `caches_host` index; the cache counter)
  and 5 written (hub insert plus its counter; cache insert, its `caches_host`
  index entry and its counter).
  A declined part costs nothing: it is decided before any D1 access.
- **IPv6.** A dual-stack hub that reaches the cache over IPv6 sends its IPv4
  in `ip=`; the addresses differ, so it is never listed (ADR-0006). How often
  this happens is visible in the logs (`update: "ip:source_mismatch"`).
  Disabling IPv6 for the cache's hostname may avoid it; that is a deployment
  setting to verify before relying on it.
- A new hub is served once the cron has handshaken it, 10–20 minutes after
  its first update (ADR-0011), plus up to 60 s of another isolate's memory.
- The integration test of the Shareaza session runs the cron explicitly
  between the update and the get.
