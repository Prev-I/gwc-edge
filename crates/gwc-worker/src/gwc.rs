//! The GWebCache endpoint, the snapshot read path, and `/stats`.

use std::rc::Rc;

use gwc_core::config::Config;
use gwc_core::counters::{self, Key, Kind, SAMPLE_K};
use gwc_core::query::{self, Net, Query};
use gwc_core::response::{Line, Refusal, get_reply, render, update_reply};
use gwc_core::snapshot::Snapshot;
use gwc_core::stats;
use worker::{Context, Env, Headers, Response, Result};

use crate::{log, memo, now, store};

/// Answer a GWebCache request. Always HTTP 200: Shareaza fails any other
/// status (`HttpRequest.cpp:142-145`).
pub async fn handle(
    raw_query: &str,
    https: bool,
    config: &Config,
    env: &Env,
    ctx: &Context,
) -> Result<Response> {
    let now = now();
    let (lines, key) = match query::parse(raw_query) {
        Err(e) => (
            vec![Line::Error(e.reason())],
            Key::new(Kind::Error, None, https),
        ),
        Ok(q) => {
            let client = Some((q.client.vendor.as_str(), q.client.version.as_str()));
            if gwc_core::policy::is_denied(&q.client.vendor, &config.denylist) {
                let lines = if q.is_update() {
                    update_reply(Err(Refusal::ClientNotSupported))
                } else {
                    Vec::new()
                };
                (lines, Key::new(Kind::Denied, client, https))
            } else if q.is_update() {
                // Storage arrives with the write path (phase 4); every update
                // a legitimate client sends is answered OK regardless.
                (update_reply(Ok(())), Key::new(Kind::of(&q), client, https))
            } else {
                let lines = get_lines(&q, now, config, env).await;
                (lines, Key::new(Kind::of(&q), client, https))
            }
        }
    };
    count(env, ctx, now, key);
    text(&render(&lines))
}

async fn get_lines(q: &Query, now: u64, config: &Config, env: &Env) -> Vec<Line> {
    if q.net == Net::Gnutella2 && (q.hostfile || q.urlfile) {
        let snap = hostfile(env, now).await;
        get_reply(
            q,
            &snap.hosts(now, config.hostfile_count),
            &snap.caches(now, config.urlfile_count),
            &config.self_url,
        )
    } else {
        get_reply(q, &[], &[], &config.self_url)
    }
}

/// The hostfile snapshot to serve, cheapest source first:
///
/// 1. this isolate's copy, while fresh (no read);
/// 2. the stored snapshot (1 row), unless stale;
/// 3. a live read of `hubs` and `caches` (cron stalled). It is written back
///    if the stored row is still the stale one just read, so the next
///    requests, in every isolate, read it instead of scanning: about one scan
///    per `MAX_SNAPSHOT_AGE` overall while the cron is down;
/// 4. the stale stored snapshot, if the live read failed: ages are computed
///    at serve time, so it is still exact;
/// 5. the last copy in memory, then an empty snapshot, retried after
///    `memo::ERROR_TTL`.
///
/// A get never fails: the reply still carries the self line.
pub async fn hostfile(env: &Env, now: u64) -> Rc<Snapshot> {
    if let Some(snap) = memo::hostfile(now) {
        return snap;
    }
    memo::lease_hostfile(now);
    match load_hostfile(env, now).await {
        Ok((snap, ttl)) => memo::set_hostfile(now, ttl, Rc::new(snap)),
        Err(e) => {
            log("hostfile_unavailable", &[("error", &e.to_string())]);
            let fallback = memo::last_hostfile().unwrap_or_else(|| Rc::new(Snapshot::default()));
            memo::set_hostfile(now, memo::ERROR_TTL, fallback)
        }
    }
}

/// Steps 2 to 4 of [`hostfile`]; returns the snapshot and how long to keep it.
async fn load_hostfile(env: &Env, now: u64) -> Result<(Snapshot, u64)> {
    let db = store::db(env)?;
    let row = store::read_snapshot(&db, store::HOSTFILE).await?;
    let stored_at = row.as_ref().map(|(at, _)| *at);
    let mut stored = match &row {
        None => {
            log("hostfile_missing", &[]);
            None
        }
        Some((_, body)) => {
            let parsed = Snapshot::parse(body);
            if parsed.is_none() {
                log("hostfile_unparsable", &[]);
            }
            parsed
        }
    };
    if let Some(snap) = stored.take_if(|s| !s.is_stale(now)) {
        return Ok((snap, memo::TTL));
    }
    if let Some(snap) = &stored {
        log(
            "hostfile_stale",
            &[("rendered_at", &snap.rendered_at.to_string())],
        );
    }
    match store::live_snapshot(&db, now).await {
        Ok(live) => {
            // Only replace a row that is absent or stale by its own timestamp:
            // a fresh row this build cannot parse (another version during a
            // rollout) is left alone, not fought over.
            let replaceable = stored_at
                .is_none_or(|at| now.saturating_sub(at) > gwc_core::snapshot::MAX_SNAPSHOT_AGE);
            if replaceable {
                match store::write_back_snapshot(
                    &db,
                    store::HOSTFILE,
                    now,
                    &live.render(),
                    stored_at,
                )
                .await
                {
                    Ok(wrote) => log("hostfile_written_back", &[("wrote", &wrote.to_string())]),
                    Err(e) => log("hostfile_write_back_failed", &[("error", &e.to_string())]),
                }
            }
            Ok((live, memo::LIVE_TTL))
        }
        Err(e) => match stored {
            Some(snap) => {
                log("hostfile_live_failed", &[("error", &e.to_string())]);
                Ok((snap, memo::LIVE_TTL))
            }
            None => Err(e),
        },
    }
}

/// Record a request in its hourly counter, one time in [`SAMPLE_K`], after
/// the response is sent. Best-effort: a failed write is logged, never felt.
fn count(env: &Env, ctx: &Context, now: u64, key: Key) {
    // Math.random() in [0, 1) scaled to the u32 range; sampled() reads the high bits.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let random = (worker::js_sys::Math::random() * 4_294_967_296.0) as u32;
    if !counters::sampled(random) {
        return;
    }
    let env = env.clone();
    ctx.wait_until(async move {
        let result = match store::db(&env) {
            Ok(db) => store::count(&db, counters::hour(now), &key, SAMPLE_K).await,
            Err(e) => Err(e),
        };
        match result {
            Ok(Some(1)) => {}
            // Budget sizing assumes 1 row per counter write; say so if not.
            Ok(rows) => log("count_rows_written", &[("rows", &format!("{rows:?}"))]),
            Err(e) => log("count_failed", &[("error", &e.to_string())]),
        }
    });
}

/// `GET /stats`: the cron-rendered document, however old (it carries its own
/// `generated_at`). It is never rendered on request: a render scans the
/// whole counter window, and per-isolate renders would multiply that. Before
/// the first cron run it answers 503. Remembered like the hostfile, so a
/// failing D1 is asked at most once per `memo::ERROR_TTL`.
pub async fn stats(env: &Env) -> Result<Response> {
    let now = now();
    let json = if let Some(json) = memo::stats(now) {
        json
    } else {
        memo::lease_stats(now);
        let loaded = match store::db(env) {
            Ok(db) => store::read_snapshot(&db, store::STATS).await,
            Err(e) => Err(e),
        };
        match loaded {
            Ok(Some((_, body))) => memo::set_stats(now, memo::TTL, Rc::new(body)),
            Ok(None) => return Response::error("stats not rendered yet", 503),
            Err(e) => {
                log("stats_unavailable", &[("error", &e.to_string())]);
                match memo::last_stats() {
                    Some(json) => memo::set_stats(now, memo::ERROR_TTL, json),
                    None => return Response::error("stats unavailable", 503),
                }
            }
        }
    };
    let headers = Headers::new();
    headers.set("Content-Type", "application/json")?;
    headers.set("Cache-Control", "no-store")?;
    Ok(Response::ok(json.as_str())?.with_headers(headers))
}

/// Build the `/stats` JSON from the grouped counter totals of the window.
pub async fn render_stats(db: &worker::D1Database, now: u64, snap: &Snapshot) -> Result<String> {
    let (first, last) = stats::window(now);
    let totals = store::totals(db, first, last).await?;
    let doc = stats::build(totals, now, snap.live_hubs(now), snap.caches.len());
    serde_json::to_string(&doc).map_err(|e| worker::Error::RustError(e.to_string()))
}

/// A `text/plain` GWebCache body. `no-transform` stops compression, which
/// Shareaza does not decode on this path (`HttpRequest.cpp:169-186`).
fn text(body: &str) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", "text/plain; charset=utf-8")?;
    headers.set("Cache-Control", "no-store, no-transform")?;
    Ok(Response::ok(body)?.with_headers(headers))
}
