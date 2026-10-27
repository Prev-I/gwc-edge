# ADR-0021: The cron advertises this cache to the caches it verifies

**Status:** proposed, 2026-10-26. Would amend ADR-0012 ("Caches").

## Context
Other GWebCaches learn about this one in only two ways today:
- **A Shareaza hub submits it.** A hub's `url=` is a cache picked at random
  from the hub's own list of working caches (`DS:1298`, `DS:1833-1856`), so a
  hub submits this cache only after it has learned it from somewhere else.
- **An operator announces it by hand** (`docs/field-test-plan.md`, stage 4).

Older caches advertise themselves:
- **Cachechu 1.6** (<https://github.com/kjotoole/cachechu>, commit `c0934a8`)
  tests one known cache each time it receives an update. When that cache's
  ping and get return anything, it sends `update=1&url=<itself>` there
  (`index.php:319-325`). It does so before it validates those replies.
- **Skulls 0.3.6** (<https://sourceforge.net/projects/skulls/>, SVN trunk
  r942) marks every request it makes with `cache=1` (`skulls.php:834`). It
  adds its own URL in an `X-GWC-URL` header only when its operator sets
  `CACHE_URL`, which is empty by default (`vars.php:11`, `skulls.php:775`,
  `:865`).

The cron already pings every cache it lists. A pong proves three things: the
cache answers, it serves `gnutella2`, and the cron can reach it within budget.
That makes it the natural moment to tell the cache about this one.

### This cache cannot verify Cachechu today
*Resolved by ADR-0023 (0.4.0): the ping now carries a urlfile get, so
Cachechu installs that list `gnutella2` first, the default, are verified.
This subsection records the state when the record was proposed.*

The cron's ping is a bare `ping=1&net=gnutella2&client=TEST&version=…`
(`ping::query`).
- Cachechu treats a request with neither `get` nor `update` as GWC1, and
  answers `PONG Cachechu 1.6` (`index.php:126-130`, `:206-213`).
- `ping::pong_line` accepts only an `i|pong|…` line.
- So every Cachechu install is marked `failed`, and deleted after 3 pings,
  although it answers Shareaza, whose G2 gets always carry `get=1`
  (`DS:1413-1434`).
- Skulls works around exactly this: on a `PONG Cachechu` it pings again with
  `update=1` added (`skulls.php:977-980`).

Fixing the ping changes what counts as verification (`docs/protocol.md`,
"Cache verification"), so it is a decision of its own, not part of this
record. Until it is made, no Cachechu cache would receive an advertisement.

## Decision (proposed)
- **After a ping that returns a pong, the cron may send that cache one
  update** naming this cache:

  ```
  GET <url>?update=1&url=<CANONICAL_URL, encoded>&net=gnutella2&cache=1&client=TEST&version=gwc-edge+<version>
  ```

  - `client=TEST` and `version=<name>` are what the Shareaza wiki asks of a
    cache talking to a cache, and are what this cache's pings already send.
  - `cache=1` is how Skulls recognises another cache (`skulls.php:1649`).
    Cachechu and this cache ignore it.
- **It goes the way the ping went**: `fetch()` for a DNS name on port 80,
  otherwise HTTP/1.0 over `connect()`.
  - It follows no redirects, and reads at most 16 KiB
    (`verify::PING_BODY_MAX`).
  - It waits up to 20 s rather than a ping's 10 s, because a cache may
    check something before it replies.
    - Skulls pings the submitted URL synchronously, with 8 s to connect and
      10 s to read (`vars.php:33-34`), and sometimes pings it twice
      (`skulls.php:1022-1040`).
    - Cachechu tests one cache inside the request, with up to three
      sequential 10 s GETs (`index.php:313-324`). That cache may be this
      one.
    - A reply slower than 20 s is logged `failed`, which costs only the
      metric. A timed-out `fetch()` is not aborted (ADR-0020,
      Consequences), so it can hold a connection a little longer.
- **At most once per cache every 7 days**, tracked in a new `caches` column,
  `last_advertised`.
  - The column is written by the `UPDATE` that already records the pong. An
    advertisement therefore adds no D1 statement and no written row: that
    `UPDATE` rewrites the row anyway, and the only secondary index on
    `caches` is on `host`.
  - If the results batch fails after an advertisement was sent, the
    timestamp is lost and the cache is advertised again at its next due
    ping. That is accepted.
  - Readvertising is needed. Skulls keeps 50 caches and pushes out the
    oldest entry for each new one (`skulls.php:1260-1263`); an entry moves
    back to the newest end only when it is resubmitted at least 24 h after
    its last check (`:1185-1226`). Cachechu re-tests a URL at most daily, when an
    incoming update picks it (`index.php:154`, `:281`), and drops it after
    two bad tests. 7 days is a
    judgement, not a figure either cache gives.
- **At most one advertisement per run.** It costs one subrequest from the
  `Ledger` and one connection slot, and it is skipped (left for the next run)
  when either is short. Hub checks keep their priority.
- **The reply is classified, and the class is logged; it is never stored.**
  - The classes are `ok`, `warned` and `failed`:
    - `ok`: a line contains `OK` (`DS:1898-1904`) and no `WARNING`;
    - `warned`: a line contains both;
    - `failed`: anything else, including a timeout.
  - Skulls reports most refusals as `warned`
    (`I|update|OK|WARNING|Blocked URL`, `…|Ping of … failed`,
    `skulls.php:2059-2067`). One of its refusals is `failed`
    (`I|update|WARNING|Network of URL not supported`, `:2068-2069`). One of
    its `ok` replies is not an acceptance (`I|update|OK|CONGESTION|…`,
    `:2070-2071`).
  - Cachechu's refusals and its ban are `failed`
    (`I|update|WARNING|Rejected URL`, `index.php:409`;
    `ERROR: Client returned too early`, `:190`). It answers `OK` for any URL
    it has not tested yet (`index.php:405-407`).
  - So `ok` means "not refused", not "listed".
  - The cron's summary counts `caches_advertised` and one counter per class.
    The outcome never changes the cache's status: a cache that declines
    submissions is still a good cache to list.
  - A failed advertisement is retried after the same 7 days, not sooner.
    The period exists so that this cache does not hammer other caches.
- **Not before field test stages 2 and 3 pass** (a real Shareaza as leaf,
  then as hub). Advertising spreads this URL to every client of those
  caches, so it must first be proven to answer Shareaza correctly.

## Open before implementation
- **The Cachechu ping** above: decided by ADR-0023.
- **Does the cron's `fetch()` carry `CF-Connecting-IP`?** The Cloudflare
  headers reference says that, for Worker subrequests to an origin outside
  Cloudflare, "the Worker script controls the client IP headers"
  (developers.cloudflare.com/fundamentals/reference/http-headers/). The cron
  has no incoming request whose headers it could forward, so the header is
  most likely absent, but the page never says so. If it were present:
  - Skulls, as installed by default, would answer any such request with
    `404 ERROR: Invalid identification` (`skulls.php:1801-1827`). That covers
    today's `fetch()` pings as well, but not socket pings, which carry only
    the bytes this cache writes.
  - Cachechu would answer an `update=1&url=…` without `get` in GWC1 form
    (`OK`) and still process it (`index.php:121-130`). It drops an update
    only when it is combined with a get.
- **Is the cron's source address on Skulls' default blocklist?** The
  blocklist is on by default (`USE_GWC_BLOCKLIST`, `vars.php:44`) and lists
  Cloudflare's `104.16.0.0/12` (`ext/gwc-blocklist.dat:737`). Two effects:
  - Requests with `client=TEST` or `cache=1` from a listed address get
    `403` (`skulls.php:1834-1849`). That covers pings and advertisements
    alike.
  - Separately, a submitted URL whose name resolves into a listed range is
    refused, and remembered as failed for 2 days (`skulls.php:1233-1242`).
    That would include this cache's own URL if it resolves into that range.
- One request from the deployed cron to an echo endpoint answers the first
  two questions: the headers it sends, and the address it comes from.

## Consequences
- **Caches this one verifies learn about it without an operator.** Each such
  cache then checks this cache on its own schedule: Skulls at once, inside
  our request; Cachechu when one of its incoming updates picks this cache,
  at most daily. That load
  is negligible.
- **Some caches test this one more strictly than Shareaza does.**
  - Cachechu fails a cache if any line of its `get=1` reply is not a
    well-formed `I|`, `H|` or `U|` line (`index.php:340-362`).
  - Its `U|` pattern accepts only DNS names ending in letters, and paths in
    `[A-Za-z0-9/.~_-]` (`index.php:36`).
  - So one IP-literal cache in this cache's urlfile, or one path outside
    that alphabet, makes Cachechu mark this cache `BAD`.
  - That exposure already exists whenever anyone submits this cache there;
    advertising makes it routine.
- **Schema:** a new migration (ADR-0015) adds one nullable column,
  `last_advertised INTEGER CHECK (last_advertised IS NULL OR last_advertised >= 0)`.
  The cron's cache queue reads it in the statement it already runs.
- **On acceptance**, `docs/protocol.md` ("Cache verification") and ADR-0012's
  budget section gain the advertisement, and the test client's fake peer
  answers it.
