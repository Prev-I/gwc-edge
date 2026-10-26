-- Migration number: 0002  dead caches (ADR-0022)
--
-- A cache URL the cron deleted after verify::CACHE_FAILURES failed pings is
-- remembered here for verify::DEAD_TTL (48 h), and a submission or a seed of
-- it is declined meanwhile. A row blocks while died >= now - DEAD_TTL; the
-- cron's expiry is housekeeping, not the rule.
-- Written by the cron only, at most verify::PINGS_PER_RUN rows a run, so at
-- most 576 rows and no cap. No secondary index: the expiry scans the table
-- (rows read, the cheap budget) rather than doubling every write.
-- COLLATE NOCASE matches update::plan_cache, which compares URLs
-- case-insensitively; caches.url stays BINARY, so the cron's insert upserts.
-- The url bounds mirror gwc_core::cache_url, as caches.url does.
CREATE TABLE dead_caches (
  url   TEXT    NOT NULL COLLATE NOCASE PRIMARY KEY CHECK (length(url) BETWEEN 12 AND 255),
  died  INTEGER NOT NULL CHECK (died >= 0) -- when the cron deleted the cache
) STRICT, WITHOUT ROWID;
