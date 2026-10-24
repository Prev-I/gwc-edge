# ADR-0012: The verifying cron

**Status:** accepted, 2026-10-24, after an expert review. Amends ADR-0006
(the hub cap counts live rows only) and ADR-0011 (a `dead` hub is not
requeued as `unverified` by its own update, and a requeue never resets
`failures`). Settles the two questions ADR-0011 left open.

## Context
Since ADR-0011, a hub is listed only after a successful G2 handshake, and
only the cron can perform one. This record decides how the cron runs:
- which hubs and caches it checks;
- what each result does to the stored row;
- how rows that keep failing are bounded;
- how it stays within the Free plan.

Two questions were left open:
- **Does a requeue reset `failures`?**
- **Can an attacker keep the 500-hub cap full?** An attacker holding about
  500 public IPv4 addresses could keep every slot taken by re-registering
  each address every 30 minutes, so that new real hubs got `Full`.

Platform facts:
- **One cron invocation:** 10 ms of CPU; 15 minutes of wall-clock time; time
  spent waiting on I/O is not CPU.
- **Limits:** 50 subrequests and 6 simultaneous connections. TCP sockets
  count toward the connections while they connect. Whether they also count
  as subrequests is not documented.
- **Writes:** D1 allows 100,000 written rows a day. Going over fails every
  query until 00:00 UTC.
- **Sockets:** `connect()` has no connect timeout and cannot reach Cloudflare
  or private addresses. Under `wrangler dev`, a closed local port failed at
  once, and an unroutable address hung for more than 10 s. Both were
  observed locally; neither is guaranteed in production.
- **`fetch()`:** it cannot reach a host given as an IP address. A
  non-standard port has been reported as silently dropped in production.
- **Measuring CPU:** `Date.now()` advances only across I/O, so a run cannot
  measure its own CPU.

## Decision

### Hubs: states and the cap
- **The 500 cap counts live rows only**, those not `dead`, through
  `counts.hubs_live`. `dead` rows are bounded by the cron (below).
- **Requeue never resets `failures`.** Only a successful handshake does.
  `failures` is the backoff exponent; letting a self-update reset it would
  hand that lever back to the attacker.
- **A `dead` hub registering again stays `dead`.** Its update is written
  (`Reregister`: port, fields, `last_seen = now`) only if both hold:
  - its backoff has passed: `now >= last_checked + backoff(failures)`, where
    `backoff(f) = min(10 min · 2^(f−1), 24 h)`;
  - it is not already queued: `last_seen <= last_checked`.

  Otherwise nothing is written: `BackingOff` or `Queued`. The cron promotes
  a row from `dead` straight to `verified` or `alive_full`. No request ever
  deletes a row, and the only counter changes a request makes are
  `hubs_live + 1` on an insert and `hubs_unverified − 1` when an
  `X-Try-Hubs` candidate registers itself.
- **What a check does.** Every check sets `last_checked = now`. Only the
  cron writes that column.

  | Before | Success (`200` G2, or a busy `503`) | Failure (anything else, including a timeout) |
  |---|---|---|
  | `unverified`, self-registered or seeded | `verified`/`alive_full`, `failures = 0`, `last_seen = now` | `dead`, `failures + 1` |
  | `unverified` `X-Try-Hubs` candidate | the same | deleted (ADR-0006) |
  | `verified`/`alive_full` | the same | `failures + 1`; `dead` at 2 consecutive failures |
  | `dead` | the same | `failures + 1` |

  A listed hub survives one failure, so a timeout against a loaded hub does
  not delist it; it is retried 10 minutes later.

### Hubs: the queue
Each run checks at most 16 hubs, in waves of up to 5. Each handshake gets
8 s in total, covering connect, write and read. Closing the socket gets 1 s
more, because the connection slot is free only once the socket is closed.

A close that times out may leave the connection counted as open. Each such
socket shrinks the next waves by one, and when no slot is left, the remaining
hubs wait for the next run (`leaked_sockets`, `skipped_budget`). The
alternative was to go on dialling: the runtime would queue the extra
connections, and listed hubs would then time out and collect false
failures.

The 16 places are shared between four classes, each with a reserved number:

| Class | Rows | Order | Reserved |
|---|---|---|---|
| new | `unverified` self-registrations | `last_seen` oldest first | 10 |
| recheck | listed rows checked 90 min ago or more, or 10 min after a failure | `last_checked` oldest first | 3 |
| returning | `dead` rows that registered again since their last check | `last_seen` oldest first | 2 |
| candidate | `X-Try-Hubs` candidates | newest first | 1 |

- Places a class does not use pass to the next classes in order.
- A row that keeps re-registering moves its `last_seen` forward, which puts
  it at the back of its class. A quiet new hub stays at the front.
- Silent `dead` rows are never dialled again, which spares a victim's
  address from repeated connections.

### `X-Try-Hubs`
- **Which replies count.** Entries are taken only from replies judged
  `verified` or `alive_full`, at most 50 per reply.
- **Which entries are kept.** Entries older than 24 h are dropped. Each
  address is kept once, with its newest time, and the list is sorted newest
  first.
- **How many are stored.** At most `min(150 − hubs_unverified,
  500 − hubs_live)`, with `ON CONFLICT DO NOTHING`.
- **Checks.** A candidate is checked once: a failure deletes it, and a
  candidate that failed in a run is not taken back from that run's replies.
  A later run's replies may offer it again, and it is then a new candidate.

### `dead` rows
- **Expiry.** A `dead` row that has been neither seen nor checked for 48 h
  is deleted, at most 100 per run. A failing host can refresh `last_seen` at
  most once per backoff period (at most 24 h), so a row whose owner keeps
  registering is never purged. That avoids an insert loop: it costs about one
  write and one handshake a day per address.
- **Cap.** Beyond 1,000 `dead` rows, the longest-unchecked are deleted, at
  most 100 per run.
- This replaces the brief's "purge after 24 h", which was about visibility.
  The 2 h listing window already covers visibility.

### Caches
- **How many and which.** At most 2 pings per run: `pending` caches first,
  then `failed` ones after 1 h, then `verified` ones after 6 h.
- **How the ping is sent.**
  - A DNS name on port 80 is pinged with `fetch()`.
  - Anything else (an IP-literal host, or another port) gets plain HTTP/1.0
    over `connect()`.
  - Neither follows redirects. Each hop would be another subrequest, and a URL
    that only redirects is not the cache it names. Following them on one path
    only would also make the verdict depend on the URL's shape.

  The request is `?ping=1&net=gnutella2&client=TEST&version=gwc-edge+<version>`.
- **What counts as a pong.** HTTP `200` and an `i|pong|…` line whose
  networks field, when present, lists `gnutella2`.
- **Result.** A pong makes the cache `verified`; only `verified` caches are
  served. Three failures in a row delete it.

### Budget
- **The guard is a count, not a time.** Every socket, fetch and D1
  statement counts as one subrequest against a ceiling of 45.
- **Reserved tail.** The statements the run must still issue (results,
  snapshots, reconcile, and the two Cache API purges) are reserved from the
  start.
- **What happens when it runs out.** Hubs that do not fit are counted as
  `skipped_budget` and wait for the next run.
- **Visibility.** The run's count is logged as `subrequests`.

### Applying results
All results of a run are applied in one D1 batch. Its guards are evaluated
when the batch runs, so a request racing the cron wins:
- a verdict applies only if the row still has the port that was dialled;
- a failed candidate is deleted only if it is still an `unverified`
  candidate.

The snapshot batch runs afterwards, so the hostfile lists the hubs verified
in the same run. If the queue read or the results batch fails, the error is
logged by statement name, the summary says `verification_failed`, and the
snapshots are still rendered.

The request path guards its own writes in the same way. A refresh applies
only to a row still listed, and a requeue only to a row not `dead`. So a
request that read a row before the cron made it `dead` cannot revive it.

### Logs
- **One line per hub checked** (`cron_hub`). Its address is logged only when
  the hub is a listing: listed before, listed by this check, an
  `X-Try-Hubs` candidate, or an operator's seed.
- **Addresses left out.** A self-registration that has never passed a check
  is only a requester's address, so its line has `ip: null`, and with
  `GWC_TRACE=1` it gets no `cron_trace` line, whose headers could carry the
  address (`Listen-IP`). Addendum C allows hub addresses in
  logs; this keeps the stricter rule that requesters never appear.

### Test seam
`TEST_DIAL` maps hub IPs and cache hosts to loopback addresses, as
`target=127.x.y.z:port,…`:
- **Only loopback is accepted.** Any other address is a configuration error.
- **It fails closed.** When it is set, a target it does not map is not
  dialled, and counts as a failed check.
- **It is reported.** It shows on `/healthz` (`test_dial`) and in every cron
  summary.
- **It is never shipped.** A unit test asserts that the shipped
  `wrangler.jsonc` does not define it.

A misdeployed `TEST_DIAL` can therefore only make every check fail, since
loopback is unreachable from production sockets. It can never list a wrong
host. `scripts/integration.sh` runs a `fake_peer` binary behind it.

## Consequences
- **Worst-case writes stay under about 30,000 a day:**
  - about 4,000 for the real network;
  - about 1,000 more at steady state with 500 attacker addresses that never
    answer;
  - about 15,000 with 500 addresses answering a busy `503`, the case
    ADR-0011 already accepted.
- **Reads:** at full occupancy (500 live and 1,000 dead hubs), 2.4 to 2.8
  million rows a day of the 5 million allowed, mostly the cron's 11 to 13
  scans of `hubs` per run. This excludes the request path's live fallback,
  which only runs when the cron has stalled.
- **New hubs are listed later.** A new self-registered hub is listed 10–20
  minutes after its first update, as before. A hub that failed its first
  check waits for its next update, up to about 70 minutes.
- **Known gap.** While attacker rows await their first check, they hold live
  slots. An attacker with enough fresh public IPv4 addresses (about 1,600 or
  more) can keep the live cap full, so new real hubs get `Full`. Listed hubs
  are unaffected. Such an attacker can more cheaply exhaust the 100,000
  requests a day. If it is ever observed, the mitigation is a per-/16 cap on
  self-registrations awaiting a check; it would need an index, so it is
  deferred.
- **Busy fake hubs.** A fake hub answering a busy `503` is listed, and its
  `X-Try-Hubs` are trusted. The diversity cap bounds its share of a reply.
  Candidates are never served before their own handshake. Reflection is at
  most one connection per candidate, about 430 a day in total.
- **Caches pinged over a socket follow no redirects.** A cache on a non-80
  port that only works through a redirect is dropped, where Shareaza would
  follow it.
- **Promotions respect the live cap.** A `dead` hub that passes its check
  becomes live, so returning hubs are picked only while the run has room
  under 500 for them, before any dialling. One that does not fit stays in its
  queue, unpenalised, until there is room. Candidates get the room the
  returning hubs leave. Both use the counters read at the start of the run,
  so self-registrations arriving during the run can still push the count a
  few rows over, until they die or the cap frees up.
- **Accounting is not documented.** Whether sockets and batched D1
  statements count as subrequests is not documented. The ledger assumes they
  do, and the first production runs' `subrequests` show the real figure (16
  handshakes leave room).
