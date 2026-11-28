# ADR-0027: Seeding falls back to, and adds, the known caches

**Status:** accepted, 2026-11-28 (owner's decision). Amends ADR-0016 (the
source, and which caches a seed adds).

## Context
ADR-0016 seeds a new cache from one source, `dkac`, described then as "the
one other G2 cache known to be alive".
- **More caches are known now.** On 2026-11-28 this cache held 15 verified
  rows: 13 Gnutella2 web caches, two of them under two spellings (`dkac` and
  `cache.jayl.de`, with and without a final `/`). They are Beacon Cache II,
  Skulls, Cachechu, perlgcache, Bazooka, DKAC and G1-G2 WebCache installs.
- **`dkac` is a single point of failure.** If it stops answering, `seed.sh
  fetch` fails and a new deploy, a fork's for example, starts with nothing
  but its own URL.
- **What `dkac` lists is not checked.** Its urlfile on 2026-11-28 included
  two caches that could not be reached from the operator's network. With 3
  caches per host, unchecked entries can take the place of caches known to
  answer on the same host (`bj.ddns.net` runs five).

## Decision
- **A list of known caches**, `gwc_core::seed::KNOWN_CACHES`:
  - the 13 caches this cache had verified on 2026-11-28, `dkac` first, one
    spelling each (for `dkac` and `cache.jayl.de`, the one without the final
    `/`);
  - a core test checks that each entry is in canonical form, listed once,
    and that no host exceeds the per-host cap.
- **Without `--from`** (`GWC_SEED_FROM` unset), `gwc_seed`:
  - asks the known caches in order, until one offers a usable hub, never
    this cache itself (`gwc_core::seed::choose`);
  - passes over a source after its hostfile when that offers no hub, so the
    urlfile is asked only of the chosen source;
  - starts the seed with every known cache but this one, so that the
    source's own list comes after them and the per-host cap keeps the
    caches known to answer;
  - then adds the source's hubs and caches, and the source itself, as
    before.
- **With `--from`**, nothing changes: that cache alone is asked, and the
  known caches are not added. An operator who names a source gets exactly
  that source's seed, and so does the integration test.
- **Nothing on the list is trusted.** Known caches go in as `pending`, like
  every seeded cache, and are served only after the cron's ping (ADR-0016,
  ADR-0012). A cache on the list that has died is deleted after 3 failed
  pings, and remembered for 48 hours (ADR-0022).
- `fetch` still fails, writing nothing, when no source offers a hub.

## Consequences
- **A new deploy starts with every cache known to answer**, not only those
  `dkac` happens to list, and can be seeded while any one of the known
  caches answers.
- **The list ages.** It records what answered on 2026-11-28. A dead entry
  costs a new deploy three pings over its first hours, then nothing. It is
  refreshed by hand, through a pull request that changes the list and its
  date.
- **Production is not reseeded.** On 2026-11-28 it already held every cache
  on the list.
  - `bj.ddns.net` also runs a Skulls, which answers again since 2026-11-27.
    It is not on the list: that host's 3 slots are taken by its Beacon
    Cache II, Cachechu and perlgcache.
- **A source can still add a second spelling** of a listed cache, for example
  `dkac` with a final `/`. `normalize` keeps that slash, as Shareaza does, so
  the two are separate rows. That was so before this record, and costs only
  one of that host's slots.
