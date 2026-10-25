# ADR-0019: Sockets whose connect never settles cannot be closed early

**Status:** accepted, 2026-10-25, after stage 1 of the field test. Amends
ADR-0012 (what a leaked socket is) and the field test plan's criterion for
`leaked_sockets`.

## Context
ADR-0012 bounds every socket close at 1 second (`CLOSE_TIMEOUT_MS`). A close
that times out is a *leaked socket*: the runtime may still count its
connection as open, so the rest of the run dials with one slot fewer. The
field test plan then required `leaked_sockets: 0`.

**Production disagreed.** After seeding on 2026-10-25, three cron runs
reported 1, then 5, then 0 leaked sockets. The logs of the next two runs
show where they came from:
- **21:30 UTC:** 8 handshakes, all to `X-Try-Hubs` candidates. 5 timed
  out, and the run reported exactly 5 leaked sockets. The 2 hubs that
  replied and the 1 that refused the connection closed in time.
- **21:40 UTC:** 2 handshakes, both timed out, and 2 leaked sockets.

**The runtime explains it.** workerd's `Socket::close()` waits until the
socket connects, successfully or otherwise, before it closes anything
(`src/workerd/api/sockets.c++:950-953` at `fe3eae4`). Other facts:
- No API aborts a connection attempt that is still pending.
- A host that silently drops the connection request keeps the attempt
  pending until a platform timeout, whose length is not documented.
- Cloudflare's limits page counts a connection against the per-invocation
  limit while it is being established.

**Such hosts are common.** `X-Try-Hubs` names hubs that a hub heard about,
and many of them are stale or firewalled.

## Decision
- **A leaked socket is expected after a handshake or ping whose connection
  never opened.** It is not a defect of the cache. ADR-0012's accounting
  stays: each leaked socket holds a slot for the rest of the run.
  - The pick order dials each class's reserved share first, then the
    rest, in class order: new, recheck, returning, candidate.
  - Most of the hubs left over are therefore `X-Try-Hubs` candidates, and
    the slots lost to leaks mostly delay them.
  - Returning hubs and the one reserved candidate are dialled early,
    though, and they are the likeliest to time out, so their leaks can
    also delay new or listed hubs beyond their reserved share.
- **Measure how long a leaked socket stays open.** A close that timed out
  is not dropped: it goes on as a task of its own, which records when it
  completes.
  - Once every write is done, the run waits up to `LEAK_WAIT_MS`
    (30 seconds of wall time, no CPU) for those closes.
  - It then logs one `cron_leaks` line: for each close that completed, how
    long its connection stayed open, from the connect call (`open_ms`); and
    how many were still open.
  - Nothing about which hubs are checked changes.
- **The field test criterion becomes one that a correct cache can meet.**
  Instead of `leaked_sockets: 0`:
  - `leaked_sockets` is at most the run's handshakes with `result:
    "timeout"` plus its `caches_failed`;
  - `skipped_budget` is above 0 only in a run where `leaked_sockets`
    reached `HANDSHAKE_CONCURRENCY` (5) or `subrequests` reached the
    ceiling (45).

## Consequences
- **`cron_leaks` decides the next step.** `open_ms` approximates the
  platform's connect timeout for an address that drops the connection.
  A socket counts as leaked only after its exchange deadline and the close
  timeout, so `open_ms` is never below about 9,000 for a handshake (8 s
  plus 1 s) or 11,000 for a ping.
  - If it is short compared with a run, a later change can give the slots
    back during the run.
  - If it is longer than a run, there is nothing to give back, and
    the current accounting is already the best available.
- **A cron invocation with leaked sockets lasts up to 30 seconds longer.**
  The limit is 15 minutes of wall time. A close still pending when the
  wait ends is left to the runtime when the invocation finishes.
- **Stage 1 is re-judged under the new criterion** on runs logged with
  `cron_hub` lines. The 21:30 run meets it: 5 leaked sockets against 5
  timeouts, and 2 hubs skipped with all 5 slots leaked. So does the 21:40
  run: 2 leaked sockets against 2 timeouts, none skipped.
