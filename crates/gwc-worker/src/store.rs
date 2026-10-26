//! D1 access. Every statement is here, so the row-read and row-write cost of
//! each request path can be audited in one place.
//!
//! Numeric columns are read as `f64` and converted row by row: a typed `u64`
//! field would make one out-of-range value fail the whole result set, and with
//! it the cron and the hostfile. A bad row is dropped instead.

use gwc_core::cache_url::CacheUrl;
use gwc_core::counters::Key;
use gwc_core::snapshot::{LIST_WINDOW, SnapCache, SnapHub, Snapshot};
use gwc_core::stats::{ClientCount, MAX_CLIENTS, Totals};
use gwc_core::update::{
    COUNT_INSERTED_CACHE, COUNT_INSERTED_HUB, HubClaim, HubRow, UNCOUNT_REQUEUED_CANDIDATE,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use worker::wasm_bindgen::JsValue;
use worker::{D1Database, D1PreparedStatement, D1Result, Env, Result, console_warn};

/// D1 binding name, as declared in `wrangler.jsonc`.
pub const DB: &str = "DB";

pub use gwc_core::snapshot::keys::{CRON, HOSTFILE, STATS};

pub fn db(env: &Env) -> Result<D1Database> {
    env.d1(DB)
}

// Timestamps fit an f64 exactly; D1 stores integral numbers as INTEGER.
#[allow(clippy::cast_precision_loss)]
fn num(n: u64) -> JsValue {
    JsValue::from_f64(n as f64)
}

/// A non-negative integral number that fits `T`, as read from D1.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn int<T: TryFrom<u64>>(x: f64) -> Option<T> {
    let exact = x.is_finite() && x >= 0.0 && x.fract() == 0.0 && x <= 9_007_199_254_740_991.0;
    exact.then(|| T::try_from(x as u64).ok()).flatten()
}

/// Run a query and keep the rows that convert, logging how many did not.
async fn rows<R: DeserializeOwned, T>(
    stmt: D1PreparedStatement,
    what: &str,
    convert: impl Fn(R) -> Option<T>,
) -> Result<Vec<T>> {
    keep(&stmt.all().await?, what, convert)
}

/// The rows of a result that convert, logging how many did not.
fn keep<R: DeserializeOwned, T>(
    result: &D1Result,
    what: &str,
    convert: impl Fn(R) -> Option<T>,
) -> Result<Vec<T>> {
    let raw: Vec<R> = result.results()?;
    let total = raw.len();
    let kept: Vec<T> = raw.into_iter().filter_map(convert).collect();
    if kept.len() < total {
        console_warn!(
            "{}",
            serde_json::json!({
                "level": "warn",
                "event": "rows_dropped",
                "statement": what,
                "dropped": total - kept.len(),
            })
        );
    }
    Ok(kept)
}

#[derive(Deserialize)]
struct SnapshotRow {
    rendered_at: f64,
    body: String,
}

/// One stored snapshot body and when it was rendered. Reads 1 row.
pub async fn read_snapshot(db: &D1Database, k: &str) -> Result<Option<(u64, String)>> {
    let row: Option<SnapshotRow> = db
        .prepare("SELECT rendered_at, body FROM snapshots WHERE k = ?1")
        .bind(&[k.into()])?
        .first(None)
        .await?;
    Ok(row.and_then(|r| Some((int(r.rendered_at)?, r.body))))
}

/// Store snapshots in one batch, so readers never see one updated without
/// the other. Writes 1 row each.
pub async fn write_snapshots(
    db: &D1Database,
    now: u64,
    docs: &[(&str, &str)],
    extra: Vec<D1PreparedStatement>,
) -> Result<()> {
    let mut stmts = docs
        .iter()
        .map(|(k, body)| {
            db.prepare(
                "INSERT INTO snapshots (k, rendered_at, body) VALUES (?1, ?2, ?3) \
                 ON CONFLICT (k) DO UPDATE SET rendered_at = excluded.rendered_at, body = excluded.body",
            )
            .bind(&[(*k).into(), num(now), (*body).into()])
        })
        .collect::<Result<Vec<_>>>()?;
    stmts.extend(extra);
    db.batch(stmts).await?;
    Ok(())
}

/// Store a snapshot built on the request path, only if the stored one is
/// still the stale copy this invocation read (`stale_at`), or still absent.
/// Concurrent isolates therefore write it once, and the next requests read
/// it instead of scanning. Returns whether this call wrote. Writes 0 or 1 row.
pub async fn write_back_snapshot(
    db: &D1Database,
    k: &str,
    now: u64,
    body: &str,
    stale_at: Option<u64>,
) -> Result<bool> {
    let result = match stale_at {
        Some(at) => {
            db.prepare(
                "UPDATE snapshots SET rendered_at = ?2, body = ?3 WHERE k = ?1 AND rendered_at = ?4",
            )
            .bind(&[k.into(), num(now), body.into(), num(at)])?
            .run()
            .await?
        }
        None => {
            db.prepare(
                "INSERT INTO snapshots (k, rendered_at, body) VALUES (?1, ?2, ?3) \
                 ON CONFLICT (k) DO NOTHING",
            )
            .bind(&[k.into(), num(now), body.into()])?
            .run()
            .await?
        }
    };
    Ok(result.meta()?.and_then(|m| m.changes).unwrap_or(0) > 0)
}

/// The two documents `/stats` and the landing page are built from, `stats`
/// and `cron`, in one query. Reads at most 2 rows.
pub async fn read_stats_parts(db: &D1Database) -> Result<(Option<String>, Option<String>)> {
    #[derive(Deserialize)]
    struct R {
        k: String,
        body: String,
    }
    let parts = rows(
        db.prepare("SELECT k, body FROM snapshots WHERE k IN ('stats', 'cron')"),
        "read_stats_parts",
        |r: R| Some((r.k, r.body)),
    )
    .await?;
    let find = |k: &str| {
        parts
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, b)| b.clone())
    };
    Ok((find(STATS), find(CRON)))
}

/// Row counts for `/stats`, in one query and one scan per table (cron only).
/// Servable hubs (`verified`, `alive_full`) count only inside the listing
/// window; `unverified` and `dead` count whatever their age, since they are
/// by definition not recently seen alive. Returns (hubs by status, all hubs,
/// caches by status).
pub async fn status_counts(
    db: &D1Database,
    window_start: u64,
) -> Result<(Vec<(String, u64)>, u64, Vec<(String, u64)>)> {
    #[derive(Deserialize)]
    struct R {
        t: String,
        status: String,
        current: f64,
        n: f64,
    }
    let rows = rows(
        db.prepare(
            "SELECT 'hubs' AS t, status, \
                    CASE WHEN status IN ('verified', 'alive_full') THEN last_seen >= ?1 ELSE 1 END \
                    AS current, COUNT(*) AS n FROM hubs GROUP BY 1, 2, 3 \
             UNION ALL SELECT 'caches' AS t, status, 1 AS current, COUNT(*) AS n FROM caches \
             GROUP BY 1, 2, 3",
        )
        .bind(&[num(window_start)])?,
        "status_counts",
        |r: R| Some((r.t, r.status, r.current > 0.0, int::<u64>(r.n)?)),
    )
    .await?;
    let hubs_current = rows
        .iter()
        .filter(|(t, _, current, _)| t == "hubs" && *current)
        .map(|(_, s, _, n)| (s.clone(), *n))
        .collect();
    let hubs_total = rows
        .iter()
        .filter(|(t, ..)| t == "hubs")
        .map(|(.., n)| n)
        .sum();
    let caches = rows
        .iter()
        .filter(|(t, ..)| t == "caches")
        .map(|(_, s, _, n)| (s.clone(), *n))
        .collect();
    Ok((hubs_current, hubs_total, caches))
}

/// Add several counts in one batch, atomically: all or none. Writes 1 row each.
pub async fn count_batch(db: &D1Database, hour: u64, counts: &[(Key, u32)]) -> Result<()> {
    let stmts = counts
        .iter()
        .map(|(key, weight)| count_statement(db, hour, key, *weight))
        .collect::<Result<Vec<_>>>()?;
    db.batch(stmts).await?;
    Ok(())
}

/// When each snapshot was rendered. Reads at most 3 rows.
pub async fn snapshot_times(db: &D1Database) -> Result<Vec<(String, u64)>> {
    #[derive(Deserialize)]
    struct R {
        k: String,
        rendered_at: f64,
    }
    rows(
        db.prepare("SELECT k, rendered_at FROM snapshots"),
        "snapshot_times",
        |r: R| Some((r.k, int(r.rendered_at)?)),
    )
    .await
}

#[derive(Deserialize)]
struct LiveHubRow {
    ip: String,
    port: f64,
    last_seen: f64,
    leaves: Option<f64>,
    vendor: Option<String>,
    up_since: Option<f64>,
    max_leaves: Option<f64>,
}

impl LiveHubRow {
    /// Rows passed core validation on the way in; re-check anyway, so a bad
    /// row is dropped rather than served.
    fn into_snap(self) -> Option<SnapHub> {
        let port: u16 = int(self.port)?;
        Some(SnapHub {
            addr: gwc_core::addr::parse_hub(&format!("{}:{port}", self.ip)).ok()?,
            last_seen: int(self.last_seen)?,
            leaves: self.leaves.and_then(int),
            vendor: self.vendor,
            up_since: self.up_since.and_then(int),
            max_leaves: self.max_leaves.and_then(int),
        })
    }
}

#[derive(Deserialize)]
struct CacheRow {
    url: String,
    last_verified: f64,
}

impl CacheRow {
    fn into_snap(self) -> Option<SnapCache> {
        let url = gwc_core::cache_url::normalize(&self.url).ok()?.url;
        Some(SnapCache {
            url,
            last_verified: int(self.last_verified)?,
        })
    }
}

/// Build the hostfile snapshot from the live tables. Scans every hub row
/// (at most 500) and every cache row (at most 50): cron only, or a request
/// when the stored snapshot is missing or stale, at most once per
/// `memo::LIVE_TTL` per isolate.
pub async fn live_snapshot(db: &D1Database, now: u64) -> Result<Snapshot> {
    let hubs = rows(
        db.prepare(
            "SELECT ip, port, last_seen, leaves, vendor, up_since, max_leaves FROM hubs \
             WHERE status IN ('verified', 'alive_full') AND last_seen >= ?1",
        )
        .bind(&[num(now.saturating_sub(LIST_WINDOW))])?,
        "live_hubs",
        LiveHubRow::into_snap,
    )
    .await?;
    let caches = rows(
        db.prepare(
            "SELECT url, last_verified FROM caches \
             WHERE status = 'verified' AND last_verified IS NOT NULL",
        ),
        "live_caches",
        CacheRow::into_snap,
    )
    .await?;
    Ok(Snapshot {
        rendered_at: now,
        hubs,
        caches,
    })
}

fn count_statement(
    db: &D1Database,
    hour: u64,
    key: &Key,
    weight: u32,
) -> Result<D1PreparedStatement> {
    db.prepare(
        "INSERT INTO request_log (hour, kind, client, version, scheme, n) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT (hour, kind, client, version, scheme) DO UPDATE SET n = n + excluded.n",
    )
    .bind(&[
        num(hour),
        key.kind.as_str().into(),
        key.client.as_str().into(),
        key.version.as_str().into(),
        key.scheme.into(),
        num(u64::from(weight)),
    ])
}

/// Add a sampled request to its hourly counter. Expected to write 1 row;
/// returns what D1 reports, because whether an UPSERT bills 1 or 2 rows is not
/// documented and decides the sampling rate.
pub async fn count(db: &D1Database, hour: u64, key: &Key, weight: u32) -> Result<Option<usize>> {
    let result = count_statement(db, hour, key, weight)?.run().await?;
    Ok(result.meta()?.and_then(|m| m.rows_written))
}

/// Grouped request totals over hours `first..=last`. Two primary-key range
/// scans of the window in one batch (one round trip, one consistent read);
/// the grouping runs in SQLite, so at most about 66 rows reach the Worker
/// whatever the window holds.
pub async fn totals(db: &D1Database, first: u64, last: u64) -> Result<Totals> {
    #[derive(Deserialize)]
    struct KindScheme {
        kind: String,
        scheme: String,
        n: f64,
    }
    #[derive(Deserialize)]
    struct ClientSum {
        client: String,
        version: String,
        n: f64,
    }
    let window = [num(first), num(last)];
    let results = db
        .batch(vec![
            db.prepare(
                "SELECT kind, scheme, SUM(n) AS n FROM request_log \
                 WHERE hour BETWEEN ?1 AND ?2 GROUP BY kind, scheme",
            )
            .bind(&window)?,
            db.prepare(format!(
                "SELECT client, version, SUM(n) AS n FROM request_log \
                 WHERE hour BETWEEN ?1 AND ?2 AND kind NOT IN ('d1_error', 'degraded') \
                 GROUP BY client, version \
                 ORDER BY n DESC, client, version LIMIT {MAX_CLIENTS}"
            ))
            .bind(&window)?,
        ])
        .await?;
    let [kinds, clients] = results.as_slice() else {
        return Err(worker::Error::RustError(
            "totals: expected 2 results".into(),
        ));
    };
    Ok(Totals {
        by_kind_scheme: keep(kinds, "totals_kind_scheme", |r: KindScheme| {
            Some((r.kind, r.scheme, int(r.n)?))
        })?,
        top_clients: keep(clients, "totals_clients", |c: ClientSum| {
            Some(ClientCount {
                client: c.client,
                version: c.version,
                n: int(c.n)?,
            })
        })?,
    })
}

// ---------------------------------------------------------------- write path

/// The stored row for a hub IP, for `update::plan_hub`. Reads 1 row.
pub async fn read_hub(db: &D1Database, ip: &str) -> Result<Option<HubRow>> {
    #[derive(Deserialize)]
    struct R {
        port: f64,
        status: String,
        source: String,
        last_seen: f64,
        last_checked: Option<f64>,
        failures: f64,
    }
    let row: Option<R> = db
        .prepare(
            "SELECT port, status, source, last_seen, last_checked, failures FROM hubs WHERE ip = ?1",
        )
        .bind(&[ip.into()])?
        .first(None)
        .await?;
    Ok(row.and_then(|r| {
        Some(HubRow {
            port: int(r.port)?,
            status: r.status,
            source: r.source,
            last_seen: int(r.last_seen)?,
            last_checked: r.last_checked.and_then(int),
            failures: int(r.failures)?,
        })
    }))
}

/// One cap counter (`hubs_live`, `hubs_unverified` or `caches`). Reads 1 row.
pub async fn read_counter(db: &D1Database, k: &str) -> Result<u64> {
    #[derive(Deserialize)]
    struct R {
        n: f64,
    }
    let row: Option<R> = db
        .prepare("SELECT n FROM counts WHERE k = ?1")
        .bind(&[k.into()])?
        .first(None)
        .await?;
    Ok(row.and_then(|r| int(r.n)).unwrap_or(0))
}

/// What a URL submission is planned against (`update::plan_cache`).
#[derive(Debug, Default)]
pub struct HostCaches {
    /// The cache URLs stored for the submitted URL's host.
    pub same_host: Vec<String>,
    /// When the cron deleted the submitted URL, if it is recorded in
    /// `dead_caches` (ADR-0022); `plan_cache` applies `DEAD_TTL`.
    pub died: Option<u64>,
}

/// The cache URLs stored for `host`, and the `dead_caches` record for `url`,
/// in one statement. Reads at most 4 rows: 3 through the `caches_host`
/// index, 1 by the `dead_caches` primary key, which is `COLLATE NOCASE` like
/// `plan_cache`'s comparison.
pub async fn read_caches_for_host(db: &D1Database, host: &str, url: &str) -> Result<HostCaches> {
    #[derive(Deserialize)]
    struct R {
        url: String,
        died: Option<f64>,
    }
    let found: Vec<R> = rows(
        db.prepare(
            "SELECT url, NULL AS died FROM caches WHERE host = ?1 \
             UNION ALL SELECT url, died FROM dead_caches WHERE url = ?2",
        )
        .bind(&[host.into(), url.into()])?,
        "read_caches_for_host",
        Some,
    )
    .await?;
    let mut out = HostCaches::default();
    for r in found {
        match r.died {
            None => out.same_host.push(r.url),
            Some(d) => out.died = int(d),
        }
    }
    Ok(out)
}

fn opt_num(n: Option<u64>) -> JsValue {
    n.map_or(JsValue::NULL, num)
}

fn opt_str(s: Option<&str>) -> JsValue {
    s.map_or(JsValue::NULL, JsValue::from)
}

/// Insert a hub that registered itself, `unverified` until the cron's first
/// successful handshake (ADR-0011), and count it, in one batch. Writes 2
/// rows. The counter adds `changes()`, the rows the insert just changed: a
/// concurrent insert of the same IP makes the insert a no-op and the counter
/// add 0, so a lost race never inflates the cap. The cron still reconciles
/// `counts` from the table on every run.
pub async fn insert_hub(db: &D1Database, claim: &HubClaim, now: u64) -> Result<()> {
    let ip = claim.addr.ip().to_string();
    db.batch(vec![
        db.prepare(
            "INSERT INTO hubs (ip, port, status, source, vendor, version, leaves, max_leaves, \
             up_since, first_seen, last_seen) \
             VALUES (?1, ?2, 'unverified', 'update', ?3, ?4, ?5, ?6, ?7, ?8, ?8) \
             ON CONFLICT (ip) DO NOTHING",
        )
        .bind(&hub_values(&ip, claim, now))?,
        db.prepare(COUNT_INSERTED_HUB),
    ])
    .await?;
    Ok(())
}

/// Refresh a servable hub from its own update on the same port: fields and
/// `last_seen`; status and failures are the cron's to change. Writes 1 row,
/// or none if the cron made it `dead` since it was read.
pub async fn refresh_hub(db: &D1Database, claim: &HubClaim, now: u64) -> Result<()> {
    let ip = claim.addr.ip().to_string();
    db.prepare(
        "UPDATE hubs SET port = ?2, vendor = ?3, version = ?4, leaves = ?5, max_leaves = ?6, \
         up_since = ?7, last_seen = ?8 WHERE ip = ?1 AND status IN ('verified', 'alive_full')",
    )
    .bind(&hub_values(&ip, claim, now))?
    .run()
    .await?;
    Ok(())
}

/// A `dead` hub registering again after its backoff: the claim's port and
/// fields and `last_seen = now`, which queues it for the cron's next check.
/// Status and failures stay: only a handshake revives it (ADR-0012). The
/// source becomes `update` (ADR-0016): the row now stands for a
/// registration, so a failed check keeps its address out of the logs.
/// Writes 1 row.
pub async fn reregister_hub(db: &D1Database, claim: &HubClaim, now: u64) -> Result<()> {
    let ip = claim.addr.ip().to_string();
    db.prepare(
        "UPDATE hubs SET port = ?2, source = 'update', vendor = ?3, version = ?4, leaves = ?5, \
         max_leaves = ?6, up_since = ?7, last_seen = ?8 WHERE ip = ?1 AND status = 'dead'",
    )
    .bind(&hub_values(&ip, claim, now))?
    .run()
    .await?;
    Ok(())
}

/// Put a hub back in the cron's queue from its own update: a servable hub on
/// a new port (demoted), or an `unverified` row (requeued). Status
/// `unverified`, source `update`, the claim's port and fields. Writes 1 row,
/// plus 1 if it was an `X-Try-Hubs` candidate (it leaves that cap). A row the
/// cron made `dead` since it was read is left alone: only a handshake revives
/// a dead row (ADR-0012).
pub async fn requeue_hub(
    db: &D1Database,
    claim: &HubClaim,
    now: u64,
    was_try_hubs: bool,
) -> Result<()> {
    let ip = claim.addr.ip().to_string();
    let mut stmts = Vec::with_capacity(2);
    if was_try_hubs {
        // First, while the row is still a candidate: it leaves that count
        // only if no concurrent request or cron run changed it meanwhile.
        stmts.push(
            db.prepare(UNCOUNT_REQUEUED_CANDIDATE)
                .bind(&[ip.as_str().into()])?,
        );
    }
    stmts.push(
        db.prepare(
            "UPDATE hubs SET port = ?2, status = 'unverified', source = 'update', vendor = ?3, \
             version = ?4, leaves = ?5, max_leaves = ?6, up_since = ?7, last_seen = ?8 \
             WHERE ip = ?1 AND status <> 'dead'",
        )
        .bind(&hub_values(&ip, claim, now))?,
    );
    db.batch(stmts).await?;
    Ok(())
}

/// `?1..=?8` for the hub statements above.
fn hub_values(ip: &str, claim: &HubClaim, now: u64) -> [JsValue; 8] {
    [
        ip.into(),
        num(u64::from(claim.addr.port())),
        opt_str(claim.vendor.as_deref()),
        opt_str(claim.version.as_deref()),
        opt_num(claim.leaves.map(u64::from)),
        opt_num(claim.max_leaves.map(u64::from)),
        opt_num(claim.up_since),
        num(now),
    ]
}

/// Store a submitted cache as `pending`, and count it, in one batch. Writes 2
/// rows; like [`insert_hub`], the counter adds only the rows the insert
/// changed.
pub async fn insert_cache(
    db: &D1Database,
    url: &CacheUrl,
    submitted_by: &str,
    now: u64,
) -> Result<()> {
    db.batch(vec![
        db.prepare(
            "INSERT INTO caches (url, host, status, submitted_by, first_seen) \
             VALUES (?1, ?2, 'pending', ?3, ?4) ON CONFLICT (url) DO NOTHING",
        )
        .bind(&[
            url.url.as_str().into(),
            url.host.as_str().into(),
            submitted_by.into(),
            num(now),
        ])?,
        db.prepare(COUNT_INSERTED_CACHE),
    ])
    .await?;
    Ok(())
}

/// `UPDATE counts` statements setting each cap counter to its true value,
/// counted inside the cron's batch so an insert racing the cron is never
/// erased, and written only when it differs (usually 0 rows written). Each
/// table is scanned once (`UPDATE … FROM`). `hubs_live` counts rows that are
/// not `dead` (ADR-0012); `hubs_unverified` counts only `X-Try-Hubs`
/// candidates: the 150 cap is theirs (ADR-0011).
pub fn reconcile_counts(db: &D1Database) -> Vec<D1PreparedStatement> {
    gwc_core::update::reconcile_sql()
        .iter()
        .map(|sql| db.prepare(sql))
        .collect()
}

// ------------------------------------------------------------- verifying cron

/// A hub the cron may check.
#[derive(Debug, Clone)]
pub struct QueuedHub {
    pub ip: String,
    pub port: u16,
    pub status: String,
    pub source: String,
    pub failures: u64,
}

/// A cache the cron may ping.
#[derive(Debug, Clone)]
pub struct QueuedCache {
    pub url: String,
    pub failures: u64,
}

/// What the cron reads before dialling.
pub struct CronQueue {
    /// One queue per `verify::Class`, each in its own order.
    pub hubs: [Vec<QueuedHub>; 4],
    pub caches: Vec<QueuedCache>,
    pub hubs_live: u64,
    pub hubs_unverified: u64,
}

/// Statements [`cron_queue`] issues.
pub const QUEUE_STATEMENTS: u32 = 3;

/// The hub queue (four classes, in `verify::Class` order) and the cache queue.
fn queue_sql() -> (String, String) {
    use gwc_core::verify::{
        CACHE_RECHECK, CACHE_RETRY, HANDSHAKES_PER_RUN, PINGS_PER_RUN, RECHECK, RETRY,
    };
    // `k` carries each class's own order to the outer ORDER BY, which is
    // what guarantees it: a UNION ALL keeps no order by itself.
    let class = |n: usize, filter: &str, key: &str| {
        format!(
            "SELECT * FROM (SELECT {n} AS class, {key} AS k, ip, port, status, source, failures \
             FROM hubs WHERE {filter} ORDER BY k LIMIT {HANDSHAKES_PER_RUN})"
        )
    };
    let hubs = [
        class(0, "status = 'unverified' AND source <> 'try_hubs'", "last_seen"),
        class(
            1,
            &format!(
                "status IN ('verified', 'alive_full') AND (last_checked IS NULL \
                 OR last_checked <= ?1 - {RECHECK} OR (failures >= 1 AND last_checked <= ?1 - {RETRY}))"
            ),
            "COALESCE(last_checked, -1)",
        ),
        class(2, "status = 'dead' AND last_seen > COALESCE(last_checked, 0)", "last_seen"),
        class(3, "status = 'unverified' AND source = 'try_hubs'", "-last_seen"),
    ]
    .join(" UNION ALL ")
        + " ORDER BY class, k";
    let caches = format!(
        "SELECT url, failures FROM caches \
         WHERE status = 'pending' \
         OR (status = 'failed' AND COALESCE(last_checked, 0) <= ?1 - {CACHE_RETRY}) \
         OR (status = 'verified' AND COALESCE(last_checked, 0) <= ?1 - {CACHE_RECHECK}) \
         ORDER BY CASE status WHEN 'pending' THEN 0 WHEN 'failed' THEN 1 ELSE 2 END, \
         COALESCE(last_checked, first_seen) ASC LIMIT {PINGS_PER_RUN}"
    );
    (hubs, caches)
}

/// The cron's queues and cap counters, in one batch of
/// [`QUEUE_STATEMENTS`] statements.
///
/// Reads: four scans of `hubs` (at most 500 live rows plus
/// `verify::DEAD_MAX` dead ones; no index, which would cost a written row on
/// every check), returning at most `HANDSHAKES_PER_RUN` rows per class; one
/// scan of `caches` (at most 50 rows); the 3 `counts` rows.
///
/// A whole run, at full occupancy (500 live and 1,000 dead hubs), scans
/// `hubs` 11 to 13 times: these four, the purge, the eviction (count and
/// ordered scan), the live snapshot, the status counts and two reconciles,
/// plus the purge and eviction deletes if SQLite scans for them. That is
/// 16,500 to 19,500 rows a run, 2.4 to 2.8 million a day of the 5 million
/// allowed, not counting the request path's live fallback; the real network
/// (35 hubs) reads a few hundred rows a run.
pub async fn cron_queue(db: &D1Database, now: u64) -> Result<CronQueue> {
    #[derive(Deserialize)]
    struct H {
        class: f64,
        ip: String,
        port: f64,
        status: String,
        source: String,
        failures: f64,
    }
    #[derive(Deserialize)]
    struct C {
        url: String,
        failures: f64,
    }
    #[derive(Deserialize)]
    struct N {
        k: String,
        n: f64,
    }
    let (hubs, caches) = queue_sql();
    let results = db
        .batch(vec![
            db.prepare(hubs).bind(&[num(now)])?,
            db.prepare(caches).bind(&[num(now)])?,
            db.prepare("SELECT k, n FROM counts"),
        ])
        .await?;
    let [hub_rows, cache_rows, count_rows] = results.as_slice() else {
        return Err(worker::Error::RustError("cron_queue: batch shape".into()));
    };

    let mut queues: [Vec<QueuedHub>; 4] = Default::default();
    for (class, hub) in keep(hub_rows, "cron_queue_hubs", |h: H| {
        Some((
            int::<usize>(h.class)?,
            QueuedHub {
                ip: h.ip,
                port: int(h.port)?,
                status: h.status,
                source: h.source,
                failures: int(h.failures)?,
            },
        ))
    })? {
        if let Some(q) = queues.get_mut(class) {
            q.push(hub);
        }
    }

    let caches = keep(cache_rows, "cron_queue_caches", |c: C| {
        Some(QueuedCache {
            url: c.url,
            failures: int(c.failures)?,
        })
    })?;

    let counts = keep(count_rows, "cron_queue_counts", |c: N| {
        Some((c.k, int::<u64>(c.n)?))
    })?;
    let count = |k: &str| {
        counts
            .iter()
            .find(|(name, _)| name == k)
            .map_or(0, |(_, n)| *n)
    };
    Ok(CronQueue {
        hubs: queues,
        caches,
        hubs_live: count("hubs_live"),
        hubs_unverified: count("hubs_unverified"),
    })
}

/// A check's result for one hub, as `verify::after_check` decided it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HubVerdict {
    pub ip: String,
    pub port: u16,
    pub status: &'static str,
    pub failures: u64,
    /// A successful check: `last_seen = now`.
    pub seen: bool,
}

/// A ping's result for one cache.
pub struct CachePing {
    pub url: String,
    pub action: gwc_core::verify::CacheAction,
}

/// Everything the checks decided, to apply in one batch.
#[derive(Default)]
pub struct CronWrites {
    pub verdicts: Vec<HubVerdict>,
    /// Failed `X-Try-Hubs` candidates.
    pub deletes: Vec<String>,
    /// New candidates and the `last_seen` each gets.
    pub candidates: Vec<(std::net::SocketAddrV4, u64)>,
    pub caches: Vec<CachePing>,
}

/// Rows removed by the cron's own housekeeping.
#[derive(Debug, Default, Clone, Copy)]
pub struct Removed {
    pub purged: usize,
    pub evicted: usize,
    pub candidates_added: usize,
}

/// Most statements [`apply_cron`] issues: hub verdicts, candidate deletes,
/// candidate inserts, the purge, the eviction, one per pinged cache, the
/// `dead_caches` record and its expiry (ADR-0022).
#[allow(clippy::cast_possible_truncation)] // a small constant
pub const APPLY_STATEMENTS: u32 = 7 + gwc_core::verify::PINGS_PER_RUN as u32;

/// The `dead_caches` statements of a results batch (ADR-0022): the record of
/// the caches this run deleted after failed pings, in the same batch as
/// their deletes, and the expiry of old records. `caches.url` is
/// case-sensitive and `dead_caches.url` is not, so two deleted URLs may be
/// one record: the insert upserts rather than failing the batch. The expiry
/// is housekeeping only: `plan_cache` and the seed already ignore an expired
/// record. Writes 1 row per record and per expired row; the expiry scans the
/// table (at most 576 rows).
fn dead_cache_statements(
    db: &D1Database,
    now: u64,
    w: &CronWrites,
) -> Result<Vec<D1PreparedStatement>> {
    use gwc_core::verify::{CacheAction, dead_cache_floor};
    let dead: Vec<&str> = w
        .caches
        .iter()
        .filter(|c| c.action == CacheAction::Delete)
        .map(|c| c.url.as_str())
        .collect();
    let mut stmts = Vec::new();
    if !dead.is_empty() {
        stmts.push(
            db.prepare(
                "INSERT INTO dead_caches (url, died) SELECT value, ?2 FROM json_each(?1) WHERE true \
                 ON CONFLICT (url) DO UPDATE SET died = excluded.died",
            )
            .bind(&[JsValue::from(serde_json::json!(dead).to_string()), num(now)])?,
        );
    }
    stmts.push(
        db.prepare("DELETE FROM dead_caches WHERE died < ?1")
            .bind(&[num(dead_cache_floor(now))])?,
    );
    Ok(stmts)
}

/// Apply a run's results in one batch (ADR-0012). Each guard is evaluated
/// when the batch runs, not when the hub was read, so a request racing the
/// cron wins:
/// - a verdict applies only to the port that was dialled;
/// - a failed candidate is deleted only if it is still an `unverified`
///   candidate (one that registered itself meanwhile stays).
///
/// Writes: 1 row per verdict, deleted candidate, inserted candidate, purged
/// or evicted row and pinged cache, and per `dead_caches` record written or
/// expired. Reads: the verdict and delete statements look rows up by primary
/// key; the purge and the eviction each scan `hubs`, and the expiry scans
/// `dead_caches` ([`dead_cache_statements`]).
pub async fn apply_cron(db: &D1Database, now: u64, w: &CronWrites) -> Result<Removed> {
    use gwc_core::verify::{CacheAction, DEAD_MAX, DEAD_TTL, PURGE_BATCH};
    let json = |v: &serde_json::Value| JsValue::from(v.to_string());
    let mut stmts = Vec::new();
    if !w.verdicts.is_empty() {
        stmts.push(
            db.prepare(
                "UPDATE hubs SET status = j.status, failures = j.failures, last_checked = ?2, \
                 last_seen = CASE WHEN j.seen THEN ?2 ELSE hubs.last_seen END \
                 FROM (SELECT value ->> 'ip' AS ip, CAST(value ->> 'port' AS INTEGER) AS port, \
                 value ->> 'status' AS status, CAST(value ->> 'failures' AS INTEGER) AS failures, \
                 CAST(value ->> 'seen' AS INTEGER) AS seen FROM json_each(?1)) AS j \
                 WHERE hubs.ip = j.ip AND hubs.port = j.port",
            )
            .bind(&[json(&serde_json::json!(w.verdicts)), num(now)])?,
        );
    }
    if !w.deletes.is_empty() {
        stmts.push(
            db.prepare(
                "DELETE FROM hubs WHERE ip IN (SELECT value FROM json_each(?1)) \
                 AND status = 'unverified' AND source = 'try_hubs'",
            )
            .bind(&[json(&serde_json::json!(w.deletes))])?,
        );
    }
    let insert_at = stmts.len();
    if !w.candidates.is_empty() {
        let list: Vec<_> = w
            .candidates
            .iter()
            .map(|(a, seen)| serde_json::json!({"ip": a.ip().to_string(), "port": a.port(), "seen": seen}))
            .collect();
        stmts.push(
            db.prepare(
                "INSERT INTO hubs (ip, port, status, source, first_seen, last_seen) \
                 SELECT value ->> 'ip', CAST(value ->> 'port' AS INTEGER), 'unverified', 'try_hubs', \
                 ?2, CAST(value ->> 'seen' AS INTEGER) FROM json_each(?1) WHERE true \
                 ON CONFLICT (ip) DO NOTHING",
            )
            .bind(&[json(&serde_json::json!(list)), num(now)])?,
        );
    }
    let purge_at = stmts.len();
    stmts.push(
        db.prepare(format!(
            "DELETE FROM hubs WHERE ip IN (SELECT ip FROM hubs WHERE status = 'dead' \
             AND last_seen < ?1 AND COALESCE(last_checked, first_seen) < ?1 LIMIT {PURGE_BATCH})"
        ))
        .bind(&[num(now.saturating_sub(DEAD_TTL))])?,
    );
    stmts.push(db.prepare(format!(
        "DELETE FROM hubs WHERE ip IN (SELECT ip FROM hubs WHERE status = 'dead' \
         ORDER BY COALESCE(last_checked, first_seen) ASC \
         LIMIT min({PURGE_BATCH}, max(0, (SELECT COUNT(*) FROM hubs WHERE status = 'dead') - {DEAD_MAX})))"
    )));
    for c in &w.caches {
        let url: JsValue = c.url.as_str().into();
        stmts.push(match &c.action {
            CacheAction::Verified { pong } => db
                .prepare(
                    "UPDATE caches SET status = 'verified', pong = ?2, failures = 0, \
                     last_verified = ?3, last_checked = ?3 WHERE url = ?1",
                )
                .bind(&[url, pong.as_str().into(), num(now)])?,
            CacheAction::Failed { failures } => db
                .prepare(
                    "UPDATE caches SET status = 'failed', failures = ?2, last_checked = ?3 WHERE url = ?1",
                )
                .bind(&[url, num(*failures), num(now)])?,
            CacheAction::Delete => db.prepare("DELETE FROM caches WHERE url = ?1").bind(&[url])?,
        });
    }
    stmts.extend(dead_cache_statements(db, now, w)?);
    let results = db.batch(stmts).await?;
    let changes = |i: usize| {
        results
            .get(i)
            .and_then(|r| r.meta().ok().flatten())
            .and_then(|m| m.changes)
            .unwrap_or(0)
    };
    Ok(Removed {
        candidates_added: if w.candidates.is_empty() {
            0
        } else {
            changes(insert_at)
        },
        purged: changes(purge_at),
        evicted: changes(purge_at + 1),
    })
}
