-- Migration number: 0001  initial schema
--
-- All timestamps are Unix seconds (UTC). Row-write budget on the Free plan is
-- 100,000/day, and every index on a written column doubles a write, so indexes
-- exist only where a read would otherwise scan a whole table.

-- G2 hubs. PRIMARY KEY (ip) is also the "one hub per IP" rule.
CREATE TABLE hubs (
  ip            TEXT    PRIMARY KEY,
  port          INTEGER NOT NULL CHECK (port BETWEEN 1024 AND 65535),
  status        TEXT    NOT NULL CHECK (status IN ('unverified', 'verified', 'alive_full', 'dead')),
  source        TEXT    NOT NULL CHECK (source IN ('update', 'try_hubs', 'seed')),
  vendor        TEXT,             -- 4-char code, from client= or the handshake User-Agent
  version       TEXT,
  leaves        INTEGER,          -- x.leaves as reported by the hub
  max_leaves    INTEGER,          -- x.max
  uptime        INTEGER,          -- uptime, seconds
  first_seen    INTEGER NOT NULL,
  last_seen     INTEGER NOT NULL, -- max(last accepted update, last successful verification)
  last_checked  INTEGER,          -- last handshake attempt, any outcome
  failures      INTEGER NOT NULL DEFAULT 0
);

-- The hostfile query reads newest-first by last_seen; without this index every
-- get would scan the whole table.
CREATE INDEX hubs_last_seen ON hubs (last_seen);

-- Other GWebCaches. Capped at ~50 rows, so scans are cheap and no index is kept.
CREATE TABLE caches (
  url            TEXT    PRIMARY KEY,  -- normalised http:// URL
  host           TEXT    NOT NULL,     -- lowercase host, for the per-domain cap
  status         TEXT    NOT NULL CHECK (status IN ('pending', 'verified', 'failed')),
  pong           TEXT,                 -- product string from its i|pong
  submitted_by   TEXT,                 -- client code of the submitter
  first_seen     INTEGER NOT NULL,
  last_verified  INTEGER,
  last_checked   INTEGER,
  failures       INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;

-- Client/version pairs. last_seen is refreshed at most hourly to save writes.
CREATE TABLE clients_seen (
  client      TEXT    NOT NULL,
  version     TEXT    NOT NULL,
  first_seen  INTEGER NOT NULL,
  last_seen   INTEGER NOT NULL,
  PRIMARY KEY (client, version)
) WITHOUT ROWID;

-- Hourly counters: one UPSERT (n = n + 1) per request, never a row per request.
CREATE TABLE request_log (
  hour     INTEGER NOT NULL,  -- unix seconds / 3600
  kind     TEXT    NOT NULL,  -- request kind, e.g. get_hosts, update_ip, rate_limited
  client   TEXT    NOT NULL,
  version  TEXT    NOT NULL,
  scheme   TEXT    NOT NULL CHECK (scheme IN ('http', 'https')),
  n        INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (hour, kind, client, version, scheme)
) WITHOUT ROWID;

-- Per-source-IP rate limiting. One atomic statement checks and records:
-- INSERT ... ON CONFLICT DO UPDATE ... WHERE now - last_at >= window RETURNING.
CREATE TABLE rate_limits (
  ip       TEXT    NOT NULL,
  action   TEXT    NOT NULL CHECK (action IN ('get', 'update')),
  last_at  INTEGER NOT NULL,
  PRIMARY KEY (ip, action)
) WITHOUT ROWID;
