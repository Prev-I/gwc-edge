//! D1 access. Every statement is here, so the row-read and row-write cost of
//! each request path can be audited in one place.
//!
//! Numeric columns are read as `f64` and converted row by row: a typed `u64`
//! field would make one out-of-range value fail the whole result set, and with
//! it the cron and the hostfile. A bad row is dropped instead.

use gwc_core::counters::Key;
use gwc_core::snapshot::{LIST_WINDOW, SnapCache, SnapHub, Snapshot};
use gwc_core::stats::{ClientCount, MAX_CLIENTS, Totals};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use worker::wasm_bindgen::JsValue;
use worker::{D1Database, D1PreparedStatement, D1Result, Env, Result};

use crate::log;

/// D1 binding name, as declared in `wrangler.jsonc`.
pub const DB: &str = "DB";

/// `snapshots.k` values.
pub const HOSTFILE: &str = "hostfile";
pub const STATS: &str = "stats";

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
        log(
            "rows_dropped",
            &[
                ("query", what),
                ("dropped", &(total - kept.len()).to_string()),
            ],
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
pub async fn write_snapshots(db: &D1Database, now: u64, docs: &[(&str, &str)]) -> Result<()> {
    let stmts = docs
        .iter()
        .map(|(k, body)| {
            db.prepare(
                "INSERT INTO snapshots (k, rendered_at, body) VALUES (?1, ?2, ?3) \
                 ON CONFLICT (k) DO UPDATE SET rendered_at = excluded.rendered_at, body = excluded.body",
            )
            .bind(&[(*k).into(), num(now), (*body).into()])
        })
        .collect::<Result<Vec<_>>>()?;
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

/// When each snapshot was rendered. Reads at most 2 rows.
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
struct HubRow {
    ip: String,
    port: f64,
    last_seen: f64,
    leaves: Option<f64>,
    vendor: Option<String>,
    up_since: Option<f64>,
    max_leaves: Option<f64>,
}

impl HubRow {
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
        HubRow::into_snap,
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

/// Add a sampled request to its hourly counter. Expected to write 1 row;
/// returns what D1 reports, because whether an UPSERT bills 1 or 2 rows is not
/// documented and decides the sampling rate.
pub async fn count(db: &D1Database, hour: u64, key: &Key, weight: u32) -> Result<Option<usize>> {
    let result = db
        .prepare(
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
        ])?
        .run()
        .await?;
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
                 WHERE hour BETWEEN ?1 AND ?2 GROUP BY client, version \
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
