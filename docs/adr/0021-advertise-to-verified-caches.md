# ADR-0021: The cron advertises this cache to the caches it verifies

**Status:** accepted, 2026-11-28 (pull request #47). Proposed 2026-10-26;
revisited 2026-11-14; revised 2026-11-28, after a review and an expert
review: advertise only where this cache is shown to be missing, behind a
deployment flag that is off by default, never to Skulls. Amends ADR-0012
("Caches", "Budget"), ADR-0015 (what the render adds) and ADR-0023
(Consequences: the cron's cost).

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

## Decision (proposed, revised 2026-11-28)
The 2026-10-26 proposal re-advertised every verified cache once a week, and
only after field test stages 2 and 3 had passed (they did on 2026-11-09). That
is replaced by the following; "Revised, 2026-11-28" below says why. The
2026-11-14 section below refers to the earlier text, which is kept in the
history of this file.

### The flag
- **`ADVERTISE` is `off` or `on`.** It is read by `gwc_core::config`.
  Anything other than `on`, including an unset or misspelt value, means
  `off`, as anything other than `1` leaves `GWC_TRACE` off. A later record
  can add values safely, since older builds read them as `off`.
- **It is deployment configuration, not a committed setting** (ADR-0015).
  - The committed `wrangler.jsonc` keeps it `off`, and a core test pins
    that, like the one for `TEST_DIAL`.
  - The deployed value comes from `GWC_ADVERTISE`:
    - the `deploy` workflow reads it as a repository variable;
    - `deploy.sh` passes it on as an argument;
    - `render-config.mjs` validates it and writes it into the rendered
      config, `off` when unset.
  - A hand deploy without the variable exported therefore renders `off`.
    `/healthz` shows it.
  - So a fork that deploys `main` sends nothing to third-party caches unless
    its own operator sets the variable.
  - Changing it is a repository-variable change and a deploy. It leaves no
    commit, so the effective mode is shown instead: in the `cron` line and in
    `/healthz`.

### Which caches
A cache is advertised to only when all of these hold:
- **The flag is `on`.**
- **The row was `verified` when the cron's queue read it.** A `pending` or
  `failed` cache is not advertised to, even if this ping passes.
- **Its host is not this cache's host.**
- **Its pong name does not start with `Skulls`**, without regard to case.
  Skulls is never advertised to: see "Revised, 2026-11-28" and the open
  questions.
- **No row of its host was advertised to in the last 30 days.**
  - The interval is read in the queue statement the cron already runs, as
    the newest `last_advertised` among the rows of that host (index
    `caches_host`).
  - So same-host spellings of one cache, such as dkac's and jayl's with and
    without a final `/`, share one timer.
  - Aliases on different hosts do not share it.
  - A row deleted after failed pings and resubmitted starts with no timer.
    ADR-0022 bounds that to about once in 2.5 days for a cache that keeps
    dying.
- **This ping's reply shows the cache does not list this one:**
  - the body was read whole (below);
  - it holds at least one `u|` line, with or without capitals, that names
    a third cache: neither the pinged cache's host nor this cache's.
    - A reply with no list therefore never counts as absence. Examples are
      `I|NO-URLS`, or a rate-limited or empty gwc-edge, which still sends
      its own self line.
    - The same goes for a cache that lists only itself.
  - no `u|` line names this cache's host.
    - The test is the host alone. It is deliberately broader than
      `update::is_self`, which compares host and port of normalised
      `http://` URLs. So a variant of this cache's URL (`https://`, another
      port or path, other capitals) counts as listed.
    - Only lines that contain the host are parsed, so the scan stays
      linear.
- **What this does not prove.** A cache may return only part of its list.
  The Skulls installs seen on 2026-11-28 returned 10 or 11 URLs and keep up
  to 50 (`skulls.php:1260-1263`). "Not in the reply" therefore means "not
  shown", not "not listed". That is why Skulls is excluded, and why the
  30-day interval stays.

### How the ping changes
Today the ping reader stops at the first pong line (`ping::Reader`; the
socket path through `talk`'s `done`, the fetch path on the verdict).
- **For a cache that is not eligible, nothing changes**: the ping stops at
  the pong, as now. In particular, nothing changes while the flag is `off`.
- **Eligibility is decided before the ping, except for the software.**
  - The queue read settles everything but the software: the flag, the
    status, the host and the 30 days.
  - The software is known only at the pong line: a pong naming Skulls ends
    the read there, as now.
- **For an eligible cache, the reader goes on after the pong**, inside the
  ping's existing 10 s and `PING_BODY_MAX`.
  - **The verdict is fixed at the pong line.** Nothing later (a deadline, an
    error, the size cut) can turn a pong into a failure. Otherwise a cache
    that keeps the connection open would fail its pings, and after 3 be
    deleted and remembered in `dead_caches` (ADR-0022).
  - **It scans every body line**, before and after the pong.
  - **It stops once it has seen both the pong and a `u|` line naming this
    cache's host**, in either order, never before the pong. The outcome
    ("listed") is settled then, so a cache that lists this one costs no more
    than a few lines' extra read at each recheck. "Read whole" matters only
    for an absence verdict.
  - **"Read whole" means:**
    - on `fetch()`, the stream ended;
    - on a socket, `Content-Length` was reached, or the peer closed with no
      `Transfer-Encoding` header.
  - A body cut because it ran past `PING_BODY_MAX`, or ended by the
    deadline, is not whole. A peer that keeps the connection open with no
    `Content-Length` is never read whole, so it is never advertised to.
  - Such a peer is never stamped either, so it stays eligible. Every 6-hour
    recheck then holds a connection slot until the ping's 10 s run out. That
    is accepted. The `cron_cache` line has no timing today, so the cost
    shows only in the run's wall time.

### The advertisement
- **At most one per run.**
  - **Where it runs:** in the cron's verification step, after the hub checks
    and the run's cache pings (zero to two), and before the results batch.
    It is the run's last outbound request to another host. Its counters
    therefore reach the stored `cron` document.
  - **Which cache:** when both pinged caches qualify, the first in queue
    order is advertised to. The other waits for its next due ping.
  - **Its cost:** two ledger units (the stamp and the request) and a free
    connection slot at that moment.
    - Both units are checked together before either is taken, so a run
      with one unit left takes none.
    - It does not wait for a slot.
    - If either is short, it is skipped and counted as `advert_skipped`.
  - **Not reserved:** it is not in the full-run assertion (`cron.rs`), which
    stands at 44 of 45. A full run therefore skips it, and the hubs keep
    their priority. AGENTS.md's rule that every cron statement is reserved
    in `cron::TAIL` or `store::*_STATEMENTS` gets this one exception, to be
    written there.
- **The stamp comes first.** `UPDATE caches SET last_advertised = ? WHERE
  host = ?` runs before the request, as its own statement, logged by name if
  it fails. If the stamp fails, nothing is sent.
  - This costs one statement and up to 3 written rows (the per-host cap) per
    advertisement.
  - It means a lost results batch, or a run killed after the send, cannot
    lead to the same cache being advertised to again at the next run.
  - A stamp followed by a failed or skipped send loses 30 days for that
    cache, which is acceptable for a reserve.
  - The results batch is unchanged.
- **The request**:

  ```
  GET <url>?update=1&url=<CANONICAL_URL, encoded>&net=gnutella2&cache=1&client=TEST&version=gwc-edge+<version>
  ```

  - `client=TEST` and `version=<name>` are what the Shareaza wiki asks of a
    cache talking to a cache, and are what this cache's pings already send.
  - `cache=1` is how Skulls recognises another cache (`skulls.php:1649`); the
    others ignore it.
- **It goes the way the ping went**: `fetch()` for a DNS name on port 80,
  otherwise HTTP/1.0 over `connect()`.
  - It follows no redirects. It reads at most 2 KiB of the reply's body. On a
    socket, the headers are bounded too, by a cap on all bytes received,
    as `talk` does for the ping.
  - It waits up to 20 s rather than a ping's 10 s, because a cache may check
    the submitted URL before it replies.
    - Cachechu tests one cache inside the request, with up to three
      sequential 10 s GETs (`index.php:313-324`).
    - G1-G2 WebCache pings the submitted URL before it answers
      (`docs/operations.md`, `GCII`).
  - A reply slower than 20 s is logged `failed`, which costs only the
    metric. A timed-out `fetch()` is not aborted (ADR-0020, Consequences), so
    it can hold a connection a little longer.
- **The reply is classified, and the class is logged; it is never stored.**
  - Only the body lines of a `200` reply are classified, not the status line,
    since a socket's `HTTP/1.x 200 OK` contains `OK`.
  - The classes are `ok`, `warned` and `failed`:
    - `ok`: a line contains `OK` (`DS:1898-1904`) and no `WARNING`;
    - `warned`: a line contains both;
    - `failed`: anything else, including a timeout.
  - `ok` means "not refused", not "listed". Cachechu, for example, answers
    `OK` for any URL it has not tested yet (`index.php:405-407`).
  - The outcome never changes the cache's status: a cache that declines
    submissions is still a good cache to list.
  - Whether the advertisement worked is not checked: the next look comes
    with the next advertisement, 30 days later at the earliest.

### Logging
- **The `cron` line gains:**
  - the effective mode;
  - `caches_advertised`;
  - one counter per class;
  - `advert_skipped`, kept apart from `skipped_budget`.

  These also land in the stored `cron` document, and so in `/stats`.
- **One `cron_advert` line per advertisement:** the URL, the pong name, the
  class and the time taken.

## Open before implementation (2026-10-26)
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
- Indirect evidence on both, and a cost found since: see "Revisited,
  2026-11-14" below.
- **Since the 2026-11-28 revision**, the header question no longer blocks:
  a cache that refused the cron would already fail its pings. The blocklist
  question matters only for Skulls, which is never advertised to.

## Open after the 2026-11-28 revision
These stay open. None blocks implementing the revised decision with the
flag `off`.
- **Skulls.** Could it ever be advertised to safely? Two things would have
  to be known: whether its reply cap is configurable, and how its
  resubmission path treats a listed URL. A reply provably holding its whole
  list would make the absence check sound, but with Skulls' cap that would
  rarely happen.
- **Cachechu's update ban.** Cachechu allows one update per address per
  `BanTime` (55 to 60 min), and runs the ban before anything else
  (`index.php:179-191`). Workers may share outbound addresses (ADR-0023), so
  an advertisement can meet a ban set by another Worker's request. It is
  then logged `failed` and waits 30 days. The ban covers updates, and this
  cache's pings carry none, so they are unaffected.
- **What the unexamined caches do on resubmission.** Would G1-G2 WebCache
  (closed source), Beacon Cache II, perlgcache or PHPGnuCacheII delete a
  listing they hold but do not show? This is one extra throw per 30 days on
  top of the hubs'.
- **Should the cron measure who lists this cache?** It would be the only
  observable trigger for turning the flag on.
- **Cachechu's strict validation of this cache's urlfile** (Consequences).
- **The echo request** for the cron's headers and source address.

## Revisited, 2026-11-14
The record stays *proposed*. The owner decides after field test stage 5 (a
week of observation, not before 2026-11-17), using what that stage shows.

- **The gate is met.** Field test stages 2 and 3 passed on 2026-11-09: stage 2
  with the hub match inferred, stage 3 with the hub listed from `X-Try-Hubs`
  before its first update (`docs/field-test-plan.md`, Results).
- **The case for it is weaker than when it was proposed.**
  - Stage 4 found the production instance already listed by dkac, without any
    announcement (`docs/field-test-plan.md`, Results).
  - A Skulls cache pinged it at about 14:00 UTC on 2026-11-09. Skulls pings a
    URL only once something submits it (`WriteCacheFile`, `skulls.php:1204`,
    `:1244`), most likely a Shareaza hub's `url=`. A Beacon Cache II pinged it
    too.
  - On 2026-11-14 `/stats` showed 15 caches verified by this one. That is the
    audience an advertisement would reach, not how many of them list this
    cache already.
  - What advertising would still add is keeping the entry fresh where lists
    are short: Skulls keeps 50 caches and pushes out the oldest for each new
    one (`skulls.php:1260-1263`). Stage 5 should show whether the listings hold
    without it.
- **Against a default Skulls, re-advertising can remove the listing.** This
  changed the 2026-10-26 Decision's premise that readvertising refreshes
  the entry.
  - On 2026-11-14 the production name resolved to two Cloudflare proxy
    addresses. One was inside `104.16.0.0/12`, which Skulls' default blocklist
    lists (`ext/gwc-blocklist.dat:737`); the other was outside it.
  - Skulls resolves a submitted URL with `gethostbyname()`, which returns one
    address (`ResolveDNS`, `skulls.php:1076-1081`). Which one depends on the
    order its resolver returns them in.
  - **A new URL that resolves into the blocklist** is refused as `Blocked URL`
    and recorded as failed (`skulls.php:1233-1242`, `AddFailedUrl`).
  - **A listed URL resubmitted after `RECHECK_CACHES` hours** (24 by default,
    `vars.php:32`; never under 12) is resolved again. If the
    address is listed, Skulls records it as failed and **deletes the entry**
    (`skulls.php:1185-1202`, `ReplaceCache(…, null)`).
  - **A failed URL is then refused for 2 days** before any other check
    (`CheckFailedUrl`, `skulls.php:708-727`, `:1158`), whichever address DNS
    gives. That covers Shareaza hubs' `url=` too.
  - So each weekly re-advertisement to a default Skulls either refreshes the
    entry or deletes it and blocks the URL there for 2 days. The reply is
    `I|update|OK|WARNING|Blocked URL` (`skulls.php:2059-2060`), logged
    `warned`.
  - The same coin applies when a Shareaza hub submits this cache; advertising
    would add a throw a week per Skulls.
  - The Skulls ping at 14:00 shows that, at that moment, that Skulls either
    resolved the name outside the listed range or runs without the default
    blocklist: the ping comes after the blocklist check (`:1238-1244`).
  - This cache cannot change the addresses. They are the Cloudflare proxy's,
    and a Worker on a Custom Domain is always proxied (ADR-0014); on the Free
    plan there is no choice of addresses.
  - If the record is accepted, it should therefore not re-advertise to
    Skulls, or not advertise to Skulls at all. The pong names the cache's
    software (`I|pong|Skulls <version>|…`, `skulls.php:484`), so the cron can
    tell.
- **The first two open questions have indirect evidence.**
  - Among the 15 verified caches is a Skulls install on a DNS name on port 80,
    so the cron reaches it with `fetch()`, sending `client=TEST`
    (`ping::via`, `ping::query`). That install is hosted outside Cloudflare,
    which is the case the Cloudflare documentation quoted above covers.
  - It answers the cron's pings with a pong, so neither
    `404 ERROR: Invalid identification` nor `403`.
  - Skulls accepts a `CF-Connecting-IP` header only when the install sets
    `USING_CLOUDFLARE_CDN` and the connection comes from a Cloudflare address;
    otherwise it answers 404 (`skulls.php:1802-1817`). So, unless that install
    sets `USING_CLOUDFLARE_CDN`, the cron's `fetch()` carries no such header.
    Either way, the address Skulls attributes to the request is not on its
    blocklist.
  - This is not conclusive: another Skulls may run with other settings or a
    newer blocklist. The echo request above would settle it.
- **If it is accepted**, the work is the one listed under Consequences, "On
  acceptance", plus the Skulls exclusion above. Concretely:
  - migration `0003` adds `last_advertised`;
  - the cron sends at most one advertisement a run, within the ledger and the
    connection slots;
  - the reply is classified, and the counts go to the `cron` line;
  - `docs/protocol.md`, ADR-0012's budget and `docs/operations.md` gain the
    advertisement;
  - the fake peer answers it.

## Revised, 2026-11-28
The owner wants the cache to spread without an operator, but not at the
price of the Skulls blocklist. The revision keeps advertising and drops what
made it risky. A first draft of this revision went to a review and an expert
review; the decision above includes their changes.

- **Where the cache stood on 2026-11-28.** 8 of the 14 Gnutella2 caches that
  could be queried listed it in their urlfile replies:
  - all five Beacon Cache II installs, dkac, G1-G2 WebCache, and one of the
    three Skulls installs;
  - the entries were between 10 and 55 hours old. That suggests hubs keep
    resubmitting it; G1-G2 WebCache also resets an entry when it re-verifies
    it.
  - It was absent from:
    - a Cachechu and a perlgcache;
    - two Skulls installs: one deleted by this cache's cron on 2026-11-26
      and not in `caches`, the other added to `caches` on 2026-11-27;
    - PHPGnuCacheII, whose pong this cache refuses, so it is not in
      `caches`;
    - an empty Cachechu, not in `caches`.
  - Two more caches could not be resolved from the observer's network.
- **Why only where shown missing.** The 2026-11-14 finding was that
  re-advertising to a default Skulls can delete a listing. The absence check
  means a cache whose reply shows this one is never sent anything. It does
  not cover a listing the reply hides.
- **Why Skulls is never advertised to.** Skulls returns part of its list, and
  re-resolves a listed URL only when it is resubmitted.
  - While hubs keep resubmitting, an advertisement adds one throw to theirs.
  - If they stop, which is the case this mechanism is a reserve for, an
    advertisement would be the only event able to delete a stable listing.
  - The first draft's `on-including-skulls` value is therefore dropped. A
    later record can add it, if the open question on Skulls is answered.
- **Why a deployment flag.** It makes the behaviour each operator's choice,
  off unless set. A committed value would pass to every fork that deploys
  `main` as soon as it was turned on.
- **Why the stamp comes first.** If the interval were written by the results
  batch, a lost batch would leave the cache due again at the next run, 10
  minutes later. It would then be advertised to every run for as long as the
  failure lasted.
- **What it would do today.** With `on`, the Cachechu and the perlgcache
  above would be eligible, if their replies are read whole and name a third
  cache. The gain is modest. The mechanism is a reserve in case
  the hubs stop spreading the cache.

## Consequences
- **While `ADVERTISE` is `off`, nothing changes** except the new column,
  which stays empty, and the mode shown in the `cron` line and `/healthz`.
- **When it is `on`, caches this one verifies, and whose reply does not show
  it, learn about it without an operator.**
  - Each such cache then checks this cache on its own schedule. G1-G2
    WebCache does so at once, inside our request. Cachechu does so when one
    of its incoming updates picks this cache, at most daily.
  - That load is negligible.
- **Cost of an advertisement:** two ledger units, one D1 statement and up to
  3 written rows, and one outbound request. There is at most one per run, and
  none in a full run.
- **Some caches test this one more strictly than Shareaza does.**
  - Cachechu fails a cache if any line of its `get=1` reply is not a
    well-formed `I|`, `H|` or `U|` line (`index.php:340-362`).
  - Its `U|` pattern accepts only DNS names ending in letters, and paths in
    `[A-Za-z0-9/.~_-]` (`index.php:36`).
  - So one IP-literal cache in this cache's urlfile, or one path outside
    that alphabet, makes Cachechu mark this cache `BAD`.
  - That exposure already exists whenever anyone submits this cache there.
    An advertisement adds one more submission, at most once in 30 days, and
    only where the cache is not shown.
- **Configuration:**
  - `ADVERTISE` joins the table in `gwc_core::config`, and is `off` in the
    committed `wrangler.jsonc`, pinned by a test.
  - `render-config.mjs`, `deploy.sh` and the `deploy` workflow pass
    `GWC_ADVERTISE` through.
  - The README's deploy section names the variable.
- **Schema:** a new migration (ADR-0015) adds one nullable column,
  `last_advertised INTEGER CHECK (last_advertised IS NULL OR last_advertised >= 0)`.
  The cron's cache queue reads it in the statement it already runs.
- **On acceptance:**
  - `docs/protocol.md` ("Cache verification") gains the advertisement.
  - `docs/operations.md` gains the flag, the `cron` counters and the
    `cron_advert` line.
  - ADR-0012 ("Budget") notes that the full-run assertion leaves one unit,
    which the advertisement uses only when free.
  - ADR-0023's "the cron's cost does not change" no longer holds for
    eligible pings. ADR-0015 gains `ADVERTISE` among what the render adds.
  - `AGENTS.md` records the advertisement's exception to the `cron::TAIL`
    rule.
  - `/healthz` shows the effective mode.
  - `Ledger` gains a method that takes several units at once, or none, so
    the two units are not taken with two `try_take` calls.
  - `scripts/integration.sh` starts `wrangler dev` with
    `--var ADVERTISE:on`, as it does for `TEST_DIAL`, so the fake peer's
    advertisement path runs.
  - Migration `0003` gets its line in `migrations/SHA256SUMS` and a core test
    of its CHECK. It must first be applied locally
    (`wrangler d1 migrations apply DB --local`), since `ADD COLUMN` with a
    CHECK on a STRICT table has not been tried on D1.
  - The test client's fake peer answers the advertisement, and lists or
    omits this cache in its urlfile so that both paths are tested.
  - The reader's tests cover these cases:
    - the verdict latched at the pong;
    - a stop on a line listing this cache, but never before the pong;
    - a cut body;
    - a peer that keeps the connection open;
    - a reply with no `u|` line.
