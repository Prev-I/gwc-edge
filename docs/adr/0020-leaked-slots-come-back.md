# ADR-0020: A leaked socket's slot comes back when its close completes

**Status:** accepted, 2026-10-26. Amends ADR-0019, which kept ADR-0012's
rule that a leaked socket holds a slot for the rest of the run.

## Context
ADR-0019 explained leaked sockets and added the `cron_leaks` line to learn
how long they stay open. The first three production runs that logged it
(2026-10-25, 22:20 to 22:40 UTC) answered:
- every leaked socket closed within the run's wait;
- its connection stayed open 19,280 to 19,507 ms from the connect call
  (`open_ms`, 4 sockets).

So the platform gives up on a connection that is never answered after about
19.5 seconds. A leak is known about 9 seconds after the connect call (the
8-second handshake deadline and the 1-second close timeout), and its slot
comes back about 10 seconds later. A wave of handshakes takes up to
9 seconds, so the slot would be free again within the same run, one or two
waves later.

Until now, a leaked socket cost its slot for the whole run: after 5 leaks,
the remaining hubs waited 10 minutes for the next run (`skipped_budget`).

## Decision
- **A leaked socket holds a slot only until its close succeeds.** Each
  close records when and how it completes (ADR-0019). Before each wave of
  handshakes, and before each cache ping, the run counts only the leaked
  sockets that may still hold a slot (`verify::slots`).
  - workerd releases the connection before it resolves a close
    (`src/workerd/api/sockets.c++:976-981` at `fe3eae4`).
  - A close that fails takes another path, which may keep the connection
    until garbage collection. Its slot stays taken for the rest of the run,
    as before this record, and `cron_leaks` counts it in `close_errors`.
- **When they hold every slot, the run waits for one.** It polls every
  250 ms for up to `SLOT_WAIT_MS` (15 seconds), then dials as many as are
  free. If none came back, the rest wait for the next run, as before.
  - The wait starts once a leak is known, about 9 seconds after the
    connect call. 15 seconds more covers the measured 19.5 seconds from
    the connect call with a margin.
  - A core test ties the constant to that measurement.
- **The wait is visible.** The `cron` line and `/stats` carry
  `slot_wait_ms`, the run's total time spent waiting for slots.
- **Unchanged:**
  - the subrequest ledger, charged before dialling;
  - the 5 handshakes in flight;
  - the sixth connection kept for a cache ping;
  - the pick order;
  - the `cron_leaks` line, apart from its new `close_errors`.

## Consequences
- **Hubs that time out cost their followers seconds, not a run.** A run with
  many unanswered candidates still checks everything it picked.
- **Runs with leaks take longer.** In the worst case, all 16 handshakes and
  both pings leak and every wave waits for slots, and a run lasts about
  90 seconds of wall time. The limit is 15 minutes, and CPU use does not
  change. The field test's `ms` bound is raised to match.
- **`skipped_budget` should now stay at 0** unless:
  - the subrequest ceiling is reached; or
  - the platform's connect timeout grows past the wait; or
  - closes fail.

  A `skipped_budget` above 0 with `slot_wait_ms` of at least 15,000 points
  to the last two: compare `cron_leaks` `open_ms` with 19,500, and check
  its `close_errors`. The field test's week-long check uses this
  criterion.
- **Reclaiming relies on the platform freeing the connection when the close
  resolves.** That is what workerd's source shows, but the production limit
  itself cannot be inspected. A sign that it is wrong: a `timeout` in a
  wave dialled right after `slot_wait_ms` grew, where a seventh
  connection would have queued.
- **A close that fails within the close timeout is not a leak.** Such a
  socket, usually one whose connection was refused, counts as closed, as
  before this record. Production gave no sign that it holds a slot. At
  22:30 UTC on 2026-10-25, wave 2 dialled 3 sockets next to 2 open leaks
  and 2 refused connections from wave 1. The leak in wave 2 stayed open
  19,507 ms, not the roughly 29 s a queued seventh connection would show.
- **`fetch()` pings are not tracked.** A `fetch()` ping that hits its
  deadline is not aborted, and may hold a connection that `verify::slots`
  does not count. At most one runs at a time, after the handshakes, so it
  can only overlap the second ping. This predates this record and is
  left as is.
- **If the platform changes its timeout,** `SLOT_WAIT_MS` and its test must
  follow the new measurement.
