//! Everything that is not the GWC protocol: the landing page, `/stats`,
//! `/robots.txt` and `/healthz`.
//!
//! The landing page and `/stats` go through the Workers Cache API (300 s and
//! 60 s): a miss costs one D1 read of the two cron-rendered documents. A
//! degraded render (D1 failed) is served but never cached.

use std::collections::BTreeMap;

use gwc_core::config::Config;
use gwc_core::landing::{self, Page};
use gwc_core::log::Outcome;
use gwc_core::stats::{CronSummary, Hourly, Stats, compose};
use serde::Serialize;
use worker::{Cache, Headers, Response, Result, console_warn};

use crate::probe::Probe;
use crate::{GIT_SHA, store};

/// Seconds the landing page is cached at the edge.
const LANDING_MAX_AGE: u32 = 300;

/// Seconds `/stats` is cached at the edge.
const STATS_MAX_AGE: u32 = 60;

/// `/healthz` reports the cron stalled once its last run is this old: three
/// missed 10-minute runs.
const CRON_STALLED_AFTER: u64 = 30 * 60;

pub const ROBOTS: &str = "User-agent: *\nDisallow: /\n";

/// Edge cache keys: one per page, independent of the host the request used.
fn cache_key(config: &Config, page: &str) -> String {
    format!("{}?gwc-edge-page={page}", config.canonical_url.url)
}

/// A cached copy, re-wrapped so its headers can say it was a hit.
async fn cache_get(key: &str) -> Option<Response> {
    let mut cached = Cache::default().get(key, true).await.ok()??;
    let headers = cached.headers().clone();
    headers.set("X-GWC-Cache", "hit").ok()?;
    let body = cached.text().await.ok()?;
    Some(Response::ok(body).ok()?.with_headers(headers))
}

async fn cache_put(key: &str, resp: &mut Response) {
    if let Ok(copy) = resp.cloned() {
        let _ = Cache::default().put(key, copy).await;
    }
}

fn headers(content_type: &str, max_age: u32, cache: &str) -> Result<Headers> {
    let h = Headers::new();
    h.set("Content-Type", content_type)?;
    h.set("Cache-Control", &format!("public, max-age={max_age}"))?;
    h.set("X-GWC-Cache", cache)?;
    Ok(h)
}

/// The `/stats` document from the two cron-rendered parts, and whether both
/// parts were there and readable. `None` if the hourly part cannot be had:
/// D1 failed, no cron run yet, or a format this build cannot read.
async fn load_stats(
    config: &Config,
    env: &worker::Env,
    probe: &mut Probe,
) -> (Option<Stats>, bool) {
    let Ok(db) = probe.run("binding", || async { store::db(env) }).await else {
        return (None, false);
    };
    let Ok((hourly, cron)) = probe
        .run("read_stats_parts", || store::read_stats_parts(&db))
        .await
    else {
        return (None, false);
    };
    let hourly: Option<Hourly> = hourly.and_then(|b| parse_part(store::STATS, &b));
    let cron: Option<CronSummary> = cron.and_then(|b| parse_part(store::CRON, &b));
    let complete = hourly.is_some() && cron.is_some();
    let stats = hourly.map(|h| {
        compose(
            h,
            cron.unwrap_or_default(),
            GIT_SHA,
            &config.canonical_url.url,
        )
    });
    (stats, complete)
}

/// Parse a stored part, logging a warning if this build cannot read it.
fn parse_part<T: serde::de::DeserializeOwned>(k: &str, body: &str) -> Option<T> {
    match serde_json::from_str(body) {
        Ok(part) => Some(part),
        Err(e) => {
            console_warn!(
                "{}",
                serde_json::json!({"level": "warn", "event": "snapshot_unparsable", "snapshot": k, "error": e.to_string()})
            );
            None
        }
    }
}

fn outcome(probe: &Probe) -> Outcome {
    if probe.failed() {
        Outcome::Degraded
    } else {
        Outcome::Ok
    }
}

/// `GET /` without GWC parameters, on the canonical host.
pub async fn landing(
    config: &Config,
    env: &worker::Env,
    probe: &mut Probe,
) -> Result<(Response, Outcome)> {
    let key = cache_key(config, "landing");
    if let Some(hit) = cache_get(&key).await {
        return Ok((hit, Outcome::Ok));
    }
    let (stats, complete) = load_stats(config, env, probe).await;
    let html = landing::render(&Page {
        canonical_url: &config.canonical_url.url,
        git_sha: GIT_SHA,
        stats: stats.as_ref(),
    });
    let mut resp = Response::ok(html)?.with_headers(headers(
        "text/html; charset=utf-8",
        LANDING_MAX_AGE,
        "miss",
    )?);
    // Only a page with every number is cached, so the numbers appear as soon
    // as they exist, and a gap is never served for five minutes.
    if complete {
        cache_put(&key, &mut resp).await;
    }
    Ok((resp, outcome(probe)))
}

/// `GET /stats`: 503 until the first cron run, or while D1 fails.
pub async fn stats(
    config: &Config,
    env: &worker::Env,
    probe: &mut Probe,
) -> Result<(Response, Outcome)> {
    let key = cache_key(config, "stats");
    if let Some(hit) = cache_get(&key).await {
        return Ok((hit, Outcome::Ok));
    }
    let (doc, complete) = load_stats(config, env, probe).await;
    let Some(doc) = doc else {
        let msg = if probe.failed() {
            "stats unavailable"
        } else {
            "stats not rendered yet"
        };
        return Ok((Response::error(msg, 503)?, outcome(probe)));
    };
    let json = serde_json::to_string(&doc).map_err(|e| worker::Error::RustError(e.to_string()))?;
    let mut resp =
        Response::ok(json)?.with_headers(headers("application/json", STATS_MAX_AGE, "miss")?);
    if complete {
        cache_put(&key, &mut resp).await;
    }
    Ok((resp, Outcome::Ok))
}

/// `GET /robots.txt`: keep crawlers away from a cache meant for clients.
pub fn robots() -> Result<Response> {
    let h = Headers::new();
    h.set("Content-Type", "text/plain; charset=utf-8")?;
    h.set("Cache-Control", "public, max-age=86400")?;
    Ok(Response::ok(ROBOTS)?.with_headers(h))
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    product: String,
    git_sha: &'static str,
    database: &'static str,
    config: String,
    cron: &'static str,
    /// Seconds since each snapshot was rendered.
    snapshot_age: BTreeMap<String, u64>,
}

/// `/healthz`: configuration, D1, and whether the cron is running. Reads at
/// most 3 rows. A missing or invalid `CANONICAL_URL` answers 500.
pub async fn healthz(
    config: std::result::Result<&Config, &str>,
    env: &worker::Env,
    now: u64,
    probe: &mut Probe,
) -> Result<(Response, Outcome)> {
    let config_status = match config {
        Ok(_) => "ok".to_owned(),
        Err(e) => e.to_owned(),
    };
    let times = match probe.run("binding", || async { store::db(env) }).await {
        Ok(db) => probe
            .run("snapshot_times", || store::snapshot_times(&db))
            .await
            .ok(),
        Err(_) => None,
    };
    let database = if times.is_some() { "ok" } else { "error" };
    let snapshot_age: BTreeMap<String, u64> = times
        .unwrap_or_default()
        .into_iter()
        .map(|(k, at)| (k, now.saturating_sub(at)))
        .collect();
    // Only the cron writes `cron`, every run, so its age is the cron's
    // heartbeat; the hostfile alone can be written back by requests.
    //   pending:   nothing yet (fresh deploy, no traffic, no cron run);
    //   never_run: a hostfile but no cron row, i.e. requests were served
    //              without a completed cron run (briefly true after a deploy);
    //   stalled:   the last completed cron run is over 30 minutes old.
    let cron = match (
        snapshot_age.get(store::CRON),
        snapshot_age.get(store::HOSTFILE),
    ) {
        (None, None) => "pending",
        (None, Some(_)) => "never_run",
        (Some(&age), _) if age > CRON_STALLED_AFTER => "stalled",
        _ => "ok",
    };
    let ok = database == "ok" && config.is_ok() && matches!(cron, "ok" | "pending");
    let health = Health {
        status: if ok { "ok" } else { "degraded" },
        product: gwc_core::product_id(),
        git_sha: GIT_SHA,
        database,
        config: config_status,
        cron,
        snapshot_age,
    };
    let status = match (config.is_ok(), ok) {
        (false, _) => 500,
        (true, false) => 503,
        (true, true) => 200,
    };
    Ok((
        Response::from_json(&health)?.with_status(status),
        outcome(probe),
    ))
}
