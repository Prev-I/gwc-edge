-- Migration number: 0001  initial schema
--
-- Amended in place before any remote database existed (no database_id in
-- wrangler.jsonc). From the first remote apply on, every change is 0002+.
--
-- Budget rules (Workers Free + D1 Free: 100k requests, 100k rows written and
-- 5M rows read per day), see docs/adr/ once written:
--   * The request path writes nothing unless a read in the same invocation
--     showed a row will change; the exceptions are the sampled counter and the
--     health flush, both after the response (AGENTS.md).
--   * Gets read one row: the cron-rendered `snapshots.hostfile`. Only if it is
--     missing or stale does a request scan `hubs` and `caches`, and it then
--     writes the snapshot back, so the next requests read it again.
--   * Secondary indexes only where a frequent read needs one: every index on a
--     written column doubles a write. `caches_host` serves URL submissions;
--     the request-path scans above are bounded by the caps (500 hubs).
-- All timestamps are Unix seconds (UTC). Bounds mirror gwc-core's validation,
-- so a value core accepts can never fail a CHECK; and every integer the Worker
-- reads back is non-negative, so a value a CHECK accepts can always be decoded.

-- G2 hubs. PRIMARY KEY (ip) is also the "one hub per IP" rule.
CREATE TABLE hubs (
  ip            TEXT    NOT NULL PRIMARY KEY CHECK (length(ip) BETWEEN 7 AND 15),
  port          INTEGER NOT NULL CHECK (port BETWEEN 1024 AND 65535),
  status        TEXT    NOT NULL CHECK (status IN ('unverified', 'verified', 'alive_full', 'dead')),
  source        TEXT    NOT NULL CHECK (source IN ('update', 'try_hubs', 'seed')),
  vendor        TEXT    CHECK (vendor IS NULL OR length(vendor) = 4),
  version       TEXT    CHECK (version IS NULL OR length(version) BETWEEN 1 AND 16),
  leaves        INTEGER CHECK (leaves IS NULL OR leaves BETWEEN 0 AND 65535),
  max_leaves    INTEGER CHECK (max_leaves IS NULL OR max_leaves BETWEEN 0 AND 65535),
  up_since      INTEGER CHECK (up_since IS NULL OR up_since >= 0), -- last update time minus reported uptime
  first_seen    INTEGER NOT NULL CHECK (first_seen >= 0),
  last_seen     INTEGER NOT NULL CHECK (last_seen >= 0), -- last accepted update or successful verification; for a dead row, the last registration accepted after its backoff
  last_checked  INTEGER CHECK (last_checked IS NULL OR last_checked >= 0), -- last handshake attempt, any outcome; written by the cron only
  failures      INTEGER NOT NULL DEFAULT 0 CHECK (failures >= 0) -- consecutive failed handshakes; reset only by a successful one
) STRICT, WITHOUT ROWID;

-- Other GWebCaches. At most 50 rows, at most 3 per host.
CREATE TABLE caches (
  url            TEXT    NOT NULL PRIMARY KEY CHECK (length(url) BETWEEN 12 AND 255),
  host           TEXT    NOT NULL CHECK (length(host) BETWEEN 1 AND 253), -- for the per-domain cap
  status         TEXT    NOT NULL CHECK (status IN ('pending', 'verified', 'failed')),
  pong           TEXT    CHECK (pong IS NULL OR length(pong) <= 64),
  submitted_by   TEXT    CHECK (submitted_by IS NULL OR length(submitted_by) = 4),
  first_seen     INTEGER NOT NULL CHECK (first_seen >= 0),
  last_verified  INTEGER CHECK (last_verified IS NULL OR last_verified >= 0),
  last_checked   INTEGER CHECK (last_checked IS NULL OR last_checked >= 0),
  failures       INTEGER NOT NULL DEFAULT 0 CHECK (failures >= 0)
) STRICT, WITHOUT ROWID;

-- The per-host lookup of every URL submission; caches rows are written
-- rarely (an insert, a status change), so the index costs little.
CREATE INDEX caches_host ON caches (host);

-- Client/version pairs, derived hourly by cron from request_log.
CREATE TABLE clients_seen (
  client      TEXT    NOT NULL CHECK (length(client) BETWEEN 1 AND 4),
  version     TEXT    NOT NULL CHECK (length(version) BETWEEN 1 AND 16),
  first_seen  INTEGER NOT NULL CHECK (first_seen >= 0),
  last_seen   INTEGER NOT NULL CHECK (last_seen >= 0),
  PRIMARY KEY (client, version)
) STRICT, WITHOUT ROWID;

-- Hourly request counters, sampled: one request in 8 adds 8
-- (gwc_core::counters). `kind` is the closed set gwc_core::counters::Kind.
-- The health kinds `d1_error` and `degraded` are exact counts, flushed from
-- isolate memory (gwc_core::health), and are not requests.
CREATE TABLE request_log (
  hour     INTEGER NOT NULL CHECK (hour >= 0), -- unix seconds / 3600
  kind     TEXT    NOT NULL CHECK (kind IN ('ping', 'hostfile', 'urlfile', 'update', 'other_net', 'limited', 'denied', 'error', 'd1_error', 'degraded')),
  client   TEXT    NOT NULL CHECK (length(client) BETWEEN 1 AND 4),  -- '-' when absent
  version  TEXT    NOT NULL CHECK (length(version) BETWEEN 1 AND 16), -- '-' when absent
  scheme   TEXT    NOT NULL CHECK (scheme IN ('http', 'https')),
  n        INTEGER NOT NULL DEFAULT 0 CHECK (n >= 0),
  PRIMARY KEY (hour, kind, client, version, scheme)
) STRICT, WITHOUT ROWID;

-- Row counts for the caps, so a cap check reads one row instead of scanning.
-- Incremented in the same batch as each insert; reconciled by cron.
--   hubs_live:       hubs not 'dead' (the 500 cap; dead rows are bounded by the cron)
--   hubs_unverified: 'unverified' X-Try-Hubs candidates (the 150 cap)
--   caches:          every cache row (the 50 cap)
CREATE TABLE counts (
  k  TEXT    NOT NULL PRIMARY KEY CHECK (k IN ('hubs_live', 'hubs_unverified', 'caches')),
  n  INTEGER NOT NULL DEFAULT 0 CHECK (n >= 0)
) STRICT, WITHOUT ROWID;

INSERT INTO counts (k, n) VALUES ('hubs_live', 0), ('hubs_unverified', 0), ('caches', 0);

-- Documents rendered by cron; the request path reads one row.
--   hostfile: gwc_core::snapshot::Snapshot, every cron run
--   cron:     gwc_core::stats::CronSummary as JSON, every cron run
--   stats:    gwc_core::stats::Hourly as JSON, hourly
CREATE TABLE snapshots (
  k            TEXT    NOT NULL PRIMARY KEY CHECK (k IN ('hostfile', 'stats', 'cron')),
  rendered_at  INTEGER NOT NULL CHECK (rendered_at >= 0),
  body         TEXT    NOT NULL
) STRICT, WITHOUT ROWID;
