# ADR-0022: A deleted cache is remembered for 48 hours

**Status:** proposed, 2026-10-26. Would amend ADR-0012 ("Caches").

## Context
- **Three failed pings in a row delete a cache** (`verify::after_ping`,
  `CACHE_FAILURES`), and nothing remembers it afterwards
  (`store::apply_cron`, `DELETE FROM caches WHERE url = ?1`).
- **A deleted URL can come back on its next submission.** It is inserted as
  a new `pending` row (`store::insert_cache`; `ON CONFLICT (url) DO NOTHING`
  applies only while the row exists).
- **`pending` rows are pinged first** (`store::queue_sql`), and a run pings at
  most 2 caches (`verify::PINGS_PER_RUN`).

So a dead cache that keeps being submitted costs 3 pings each time round,
spread over about 2 hours, and each time it goes to the front of the queue,
ahead of re-checks of the caches this one serves. Submissions are
rate-limited per address (1 a minute), not per URL.

Hubs have had this protection since ADR-0012: a `dead` hub row is kept for
48 hours, so a failing host cannot loop through inserts. Skulls 0.3.6
(<https://sourceforge.net/projects/skulls/>, SVN trunk r942) does the same for
caches. A URL whose ping failed is recorded (`AddFailedUrl`,
`skulls.php:729-736`) and refused for 2 days (`CheckFailedUrl`, `:708-727`).
Every submission is checked against it (`:1158`).

### Who resubmits, and why that cuts both ways
- **Shareaza hubs submit only caches that work for them**: at least one
  access and no failures (`DS:1192`, `DS:1298`).
- So when a hub keeps resubmitting a URL this cache deleted, the likeliest
  reason is that this cache's ping fails a cache that works for Shareaza.
- Known cases:
  - Cachechu answers this cache's bare ping in GWC1 form, which the ping
    does not accept (ADR-0021, "This cache cannot verify Cachechu today");
  - a cache on another port that works only through a redirect, which the
    socket ping does not follow (ADR-0012, Consequences).
- For those, a 48-hour block lengthens a false negative instead of saving
  pings on a dead cache.
- Really dead URLs can come from users' submit mode, which does not filter
  the URL submitted (`WndDiscovery.cpp:361`, `DS:1306-1308`), and from
  seeding. How often submit mode is used is unknown.

So this record is worth its cost only once the cron's known false negatives
are fixed, and it should be decided after the Cachechu ping.

## Decision (proposed)
- **When the cron deletes a cache after its third failed ping, it records the
  URL** in a new table:

  ```sql
  CREATE TABLE dead_caches (
    url   TEXT    NOT NULL PRIMARY KEY COLLATE NOCASE CHECK (length(url) BETWEEN 12 AND 255),
    died  INTEGER NOT NULL CHECK (died >= 0)
  ) STRICT, WITHOUT ROWID;
  ```

  - The records are written by one `INSERT … SELECT … FROM json_each(?1)
    WHERE true ON CONFLICT (url) DO UPDATE SET died = excluded.died`, in the
    same results batch as the deletes. That is the pattern of the
    `X-Try-Hubs` candidates insert.
  - The `ON CONFLICT` clause is required. `caches.url` is case-sensitive,
    so `caches` can hold two URLs that differ only in case (seeding
    deduplicates by exact string), and an operator may re-insert a recorded
    URL. Without the clause, a duplicate fails the whole results batch, hub
    verdicts included, and the same rows head the queue again next run.
  - `COLLATE NOCASE` matches `update::plan_cache`, which compares URLs
    case-insensitively. Without it, a resubmission that differs only in
    path case would slip past the record.
- **A submission or a seed for a URL in `dead_caches` is declined.**
  - It gets the reply every update gets (`i|update|OK`, decision A in
    `docs/protocol.md`), and the request's log line gets a new plan reason,
    `dead`.
  - The lookup joins the per-host read the submission already makes
    (`read_caches_for_host`). It is a `UNION ALL` arm on the primary key,
    with a column that tells its rows apart, so `plan_cache` neither counts
    a dead URL as `Known` nor counts it towards the 3-per-host cap. It adds
    no D1 statement and reads at most one more row.
  - Seeding (`gwc_core::seed`) generates its own SQL, so each generated
    `INSERT` gains `AND NOT EXISTS (SELECT 1 FROM dead_caches WHERE url = …)`.
- **A record expires 48 hours after `died`**, the hubs' `DEAD_TTL`.
  - Expired records are deleted in `apply_cron`, next to the purge of dead
    hubs, and the statement counts in `store::APPLY_STATEMENTS`.
  - The table needs no cap. Only the cron deletes caches: at most 2 a run,
    144 runs a day, kept 2 days, so at most 576 rows. Each deletion takes
    3 pings, so the sustained figure is about 192.
- **Only deletions made by the cron create a record.** An operator removing
  a row by hand does not, and can lift a record by deleting it.
- **Alternative rejected: keep the row in `caches` with a `dead` status.**
  - The `status` `CHECK` admits only `pending`, `verified` and `failed`.
    Changing it means rebuilding a live STRICT table in a frozen-migration
    scheme (ADR-0015).
  - It would also have to keep `dead` rows out of the 50-cache and
    3-per-host caps, and out of their counter.
  - A separate table changes none of that.

## Consequences
- **A dead cache costs at most 3 pings every 48 hours**, however often it is
  submitted, and stays out of the front of the queue in between.
- **A cache that was down for a while and comes back** is accepted again 48
  hours after its deletion, at its next submission. Shareaza hubs resubmit
  working caches routinely, so it returns without help.
- **A cache this cache's ping wrongly fails** is held out 48 hours longer
  than today (see "Who resubmits").
- **Costs:**
  - **Rows written:** one per deletion and one per expiry. The table is
    `WITHOUT ROWID` and has no secondary index.
  - **Statements:** two more in the results batch, for the insert and the
    expiry.
  - **Rows read:** the expiry scans the table on every run, since `died` is
    not indexed. That is at most 576 rows a run, about 83,000 a day, and
    about 28,000 a day at the sustained size. Rows
    read are not the scarce budget, while an index on `died` would double
    every write.
- **Schema:** `dead_caches` in a new migration (ADR-0015), plus its line in
  `SHA256SUMS`.
- **On acceptance**, `docs/protocol.md` ("Cache verification", "What
  gwc-edge stores") and `docs/operations.md` (log reasons) change with it.
