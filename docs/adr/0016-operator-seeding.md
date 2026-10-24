# ADR-0016: Operator seeding from another cache

**Status:** accepted, 2026-10-24 (after v0.1.0). Amends ADR-0012: its `new`
queue class also holds seeded hubs, and its `Reregister` also sets the
source to `update`.

## Context
A new cache knows no hubs. Until hubs register with it or show up in
`X-Try-Hubs`, which needs a listed hub to begin with, it hands clients
nothing but its own URL. The project brief asks for a one-shot script that
seeds the cache from `dkac`, the one other G2 cache known to be alive.

The rules so far allow exactly two ways for a hub to enter `hubs` other than
through its own address:
- **`X-Try-Hubs` candidates**, found by the cron (ADR-0006, ADR-0011);
- **none other.**

ADR-0011's escalation had named operator-inserted seeds as "the honest
route" to a non-empty hostfile at launch. It left the form to the operator.

## Decision
- **The tool.** `scripts/seed.sh fetch` runs the `gwc_seed` tool, which asks
  the source cache for its G2 hostfile and urlfile, once each. It keeps
  what this cache's own rules accept and writes SQL to a file.
  - Hubs: public IPv4, port 1024 or above, one per address (its freshest
    entry), at most 100, the freshest.
  - Caches: canonical `http://`, never this cache. The source itself is
    included, since its own urlfile does not list it.
  - `seed.sh apply` stores exactly that file in the deployed database.
- **Nothing seeded is trusted.**
  - Hubs go in as `unverified` with source `seed`, and caches as `pending`.
    They are served only after the cron's handshake or ping, as for
    everything else (ADR-0011, ADR-0012).
  - A seeded hub that fails its first check is `dead` (ADR-0012's table for
    source `seed`).
- **Caps are enforced inside each statement**, against the table as it is
  when the SQL runs:
  - 500 live hubs; seeds stay outside the 150 cap of `X-Try-Hubs`
    candidates;
  - 50 caches, 3 per host;
  - `ON CONFLICT DO NOTHING` for rows already known;
  - the counters are reconciled at the end.
- **Seeds join the `new` class** of the cron's queue (ADR-0012), with
  `last_seen = now`.
  - The source's word is not a sighting.
  - An older `last_seen` would also put seeds ahead of every real
    registration in that class, which is ordered oldest first.
- **A `dead` row that registers itself becomes source `update`** (amends
  ADR-0012's `Reregister`). The row now stands for a registration, so if
  its next check fails, its address stays out of the logs (ADR-0012,
  "Logs").

## Consequences
- **A fresh deploy can list real hubs within its first hour.** That is how
  long the cron takes to check a full seed of 100 hubs, at 10 or more a run.
  While a full seed is queued, a hub registering itself may wait up to about
  an hour instead of 10–20 minutes.
- **`fetch` fails, writing nothing, when the source offers no hub.** An
  error page or a rate-limit reply cannot pass for a seed.
- **A short window remains.** A seeded hub that registers itself within 30
  minutes of the seed is coalesced (ADR-0011) and keeps source `seed` until
  its next registration. Its address came from the source's public list.
