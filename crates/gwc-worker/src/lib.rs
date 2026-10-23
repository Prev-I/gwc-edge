//! Thin Workers adapter: routing, D1 access and the cron entry point.
//! Protocol decisions live in `gwc_core`.

mod gwc;
mod memo;
mod store;

use std::collections::BTreeMap;

use gwc_core::config::Config;
use serde::Serialize;
use worker::{
    Context, Env, Request, Response, Result, ScheduleContext, ScheduledEvent, console_log, event,
};

/// Current Unix time in seconds.
fn now() -> u64 {
    worker::Date::now().as_millis() / 1000
}

/// One structured log line: `{"event": ..., <fields>}`.
fn log(event: &str, fields: &[(&str, &str)]) {
    let mut obj = serde_json::Map::new();
    obj.insert("event".into(), event.into());
    for (k, v) in fields {
        obj.insert((*k).into(), (*v).into());
    }
    console_log!("{}", serde_json::Value::Object(obj));
}

fn config(env: &Env) -> std::result::Result<Config, String> {
    Config::from_vars(|name| env.var(name).ok().map(|v| v.to_string()))
        .map_err(|e| format!("configuration error: {e:?}"))
}

#[event(start)]
fn start() {
    console_error_panic_hook::set_once();
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    let url = req.url()?;
    match url.path() {
        "/healthz" => healthz(&env).await,
        "/stats" => gwc::stats(&env).await,
        path => {
            let config = match config(&env) {
                Ok(c) => c,
                Err(e) => {
                    log("config_error", &[("error", &e)]);
                    return Response::error(e, 500);
                }
            };
            // The cache answers at its own path, and at the root. Shareaza
            // compares URLs case-insensitively (DiscoveryServices.cpp:386),
            // so the path match is too.
            let own_path = config.self_url.url["http://".len()..]
                .find('/')
                .map_or("/", |i| &config.self_url.url["http://".len() + i..]);
            if path != "/" && !path.eq_ignore_ascii_case(own_path) {
                return Response::error("Not Found", 404);
            }
            let https = url.scheme() == "https";
            gwc::handle(url.query().unwrap_or(""), https, &config, &env, &ctx).await
        }
    }
}

#[event(scheduled)]
async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    if let Err(e) = cron(&env).await {
        log("cron_failed", &[("error", &e.to_string())]);
    }
}

/// Render the hostfile snapshot every run, and the stats document about
/// hourly. Hub verification joins in phase 5.
async fn cron(env: &Env) -> Result<()> {
    let now = now();
    let db = store::db(env)?;
    let snap = store::live_snapshot(&db, now).await?;
    let hostfile = snap.render();

    let stats_due = store::snapshot_times(&db)
        .await?
        .into_iter()
        .find(|(k, _)| k == store::STATS)
        // A little under an hour, so a 10-minute cron renders it every hour.
        .is_none_or(|(_, at)| now.saturating_sub(at) >= 55 * 60);
    // Both documents land together, so `/healthz` never sees a hostfile
    // written by the cron without the stats it rendered alongside.
    if stats_due {
        let json = gwc::render_stats(&db, now, &snap).await?;
        store::write_snapshots(
            &db,
            now,
            &[(store::HOSTFILE, &hostfile), (store::STATS, &json)],
        )
        .await?;
    } else {
        store::write_snapshots(&db, now, &[(store::HOSTFILE, &hostfile)]).await?;
    }
    log(
        "cron",
        &[
            ("hubs", &snap.live_hubs(now).to_string()),
            ("caches", &snap.caches.len().to_string()),
            ("stats_rendered", &stats_due.to_string()),
        ],
    );
    Ok(())
}

/// `/healthz` reports `degraded` once the hourly stats render is this late.
const CRON_STALLED_AFTER: u64 = 2 * 60 * 60;

#[derive(Serialize)]
struct Health {
    status: &'static str,
    product: String,
    database: &'static str,
    config: String,
    cron: &'static str,
    /// Seconds since each snapshot was rendered.
    snapshot_age: BTreeMap<String, u64>,
}

/// Liveness, configuration, D1, and snapshot freshness. Reads at most 2 rows.
async fn healthz(env: &Env) -> Result<Response> {
    let now = now();
    let config = match config(env) {
        Ok(_) => "ok".to_owned(),
        Err(e) => e,
    };
    let (database, snapshot_age) = match store::db(env) {
        Ok(db) => match store::snapshot_times(&db).await {
            Ok(times) => (
                "ok",
                times
                    .into_iter()
                    .map(|(k, at)| (k, now.saturating_sub(at)))
                    .collect(),
            ),
            Err(e) => {
                log("healthz_d1_error", &[("error", &e.to_string())]);
                ("error", BTreeMap::default())
            }
        },
        Err(e) => {
            log("healthz_d1_unbound", &[("error", &e.to_string())]);
            ("unbound", BTreeMap::default())
        }
    };
    // Only the cron writes `stats` (hourly, together with the hostfile), so
    // its age is the cron's heartbeat; the hostfile alone can be written back
    // by requests and would hide a stall.
    //   never_run: no stats yet but a hostfile exists, i.e. requests have
    //              been served without a successful cron. Briefly true after a
    //              fresh deploy that gets traffic before its first cron run.
    //   stalled:   the last full cron run (stats included) is over 2 h old,
    //              or the hostfile itself is stale.
    let stats_age = snapshot_age.get(store::STATS).copied();
    let hostfile_age = snapshot_age.get(store::HOSTFILE).copied();
    let cron = match (stats_age, hostfile_age) {
        (None, None) => "pending",
        (None, Some(_)) => "never_run",
        (Some(s), h)
            if s > CRON_STALLED_AFTER
                || h.is_some_and(|h| h > gwc_core::snapshot::MAX_SNAPSHOT_AGE) =>
        {
            "stalled"
        }
        _ => "ok",
    };
    let ok = database == "ok" && config == "ok" && matches!(cron, "ok" | "pending");
    let health = Health {
        status: if ok { "ok" } else { "degraded" },
        product: gwc_core::product_id(),
        database,
        config,
        cron,
        snapshot_age,
    };
    Ok(Response::from_json(&health)?.with_status(if ok { 200 } else { 503 }))
}
