//! The cron run (every 10 minutes): render the hostfile snapshot and the
//! cron summary every run, and the hourly stats document once an hour, all
//! written in one batch. Hub verification joins in phase 5.

use gwc_core::snapshot::{LIST_WINDOW, Snapshot};
use gwc_core::stats::{self, CronRun, CronSummary, Hourly};
use worker::{D1Database, Date, Env, Result, console_log};

use crate::store;

/// Render the hourly stats when the last render is at least this old: a
/// little under an hour, so a 10-minute cron renders it every hour.
const STATS_EVERY: u64 = 55 * 60;

pub async fn run(env: &Env) -> Result<()> {
    let started_ms = Date::now().as_millis();
    let now = started_ms / 1000;
    let db = store::db(env)?;
    let snap = store::live_snapshot(&db, now).await?;
    let (hubs_current, hubs_total, caches) =
        store::status_counts(&db, now.saturating_sub(LIST_WINDOW)).await?;
    let hourly = if stats_due(&db, now).await? {
        Some(render_hourly(&db, now).await?)
    } else {
        None
    };

    let summary = CronSummary {
        hubs: stats::hub_counts(&hubs_current, snap.live_hubs(now) as u64, hubs_total),
        caches: stats::cache_counts(&caches),
        run: CronRun {
            last_run_at: now,
            last_run_ms: Date::now().as_millis().saturating_sub(started_ms),
            ..CronRun::default()
        },
    };
    let hostfile = snap.render();
    let cron =
        serde_json::to_string(&summary).map_err(|e| worker::Error::RustError(e.to_string()))?;
    let mut docs = vec![
        (store::HOSTFILE, hostfile.as_str()),
        (store::CRON, cron.as_str()),
    ];
    if let Some(json) = &hourly {
        docs.push((store::STATS, json.as_str()));
    }
    store::write_snapshots(&db, now, &docs).await?;

    log_run(&snap, now, &summary, hourly.is_some());
    Ok(())
}

/// Due when there is no stats document, it cannot be read (another build's
/// format), or it is older than [`STATS_EVERY`].
async fn stats_due(db: &D1Database, now: u64) -> Result<bool> {
    let stats_doc = store::read_snapshot(db, store::STATS).await?;
    Ok(stats_doc
        .and_then(|(_, body)| serde_json::from_str::<Hourly>(&body).ok())
        .is_none_or(|h| now.saturating_sub(h.generated_at) >= STATS_EVERY))
}

async fn render_hourly(db: &D1Database, now: u64) -> Result<String> {
    let (first, last) = stats::window(now);
    let totals = store::totals(db, first, last).await?;
    serde_json::to_string(&stats::hourly(totals, now))
        .map_err(|e| worker::Error::RustError(e.to_string()))
}

/// One line per run. Verification fields stay 0 until phase 5.
fn log_run(snap: &Snapshot, now: u64, summary: &CronSummary, stats_rendered: bool) {
    let line = serde_json::json!({
        "kind": "cron",
        "candidates": summary.run.candidates,
        "verified": summary.run.verified,
        "alive_full": summary.run.alive_full,
        "dead": summary.run.dead,
        "skipped_budget": summary.run.skipped_budget,
        "ms": summary.run.last_run_ms,
        "hubs_served": snap.live_hubs(now),
        "caches": snap.caches.len(),
        "stats_rendered": stats_rendered,
    });
    console_log!("{line}");
}
