//! The cron run (every 10 minutes, ADR-0012):
//! 1. read the hub and cache queues and the cap counters (one batch);
//! 2. handshake with up to 16 hubs, 5 at a time, then ping up to 2 caches;
//! 3. apply every result, store new `X-Try-Hubs` candidates, and purge old
//!    `dead` rows (one batch);
//! 4. render the hostfile snapshot and the cron summary every run, and the
//!    hourly stats once an hour, reconciling the cap counters (one batch),
//!    after step 3 so the hostfile lists the hubs just verified.
//!
//! The budget is counted, not timed: every socket, fetch and D1 statement is
//! one subrequest against `verify::SUBREQUEST_CEILING`.

use gwc_core::config::{Config, Dial};
use gwc_core::handshake::{Exchange, Verdict};
use gwc_core::log::d1_error_line;
use gwc_core::ping::{self, Via};
use gwc_core::snapshot::{LIST_WINDOW, Snapshot};
use gwc_core::stats::{self, CronRun, CronSummary, Hourly};
use gwc_core::update::{MAX_HUBS, MAX_UNVERIFIED};
use gwc_core::verify::{
    self, CacheAction, Checked, HANDSHAKE_CONCURRENCY, HANDSHAKES_PER_RUN, HubAction, Ledger,
};
use worker::{D1Database, Date, Env, Result, console_error, console_log};

use crate::dial;
use crate::store::{self, CachePing, CronWrites, HubVerdict, QueuedHub};

/// Render the hourly stats when the last render is at least this old: a
/// little under an hour, so a 10-minute cron renders it every hour.
const STATS_EVERY: u64 = 55 * 60;

/// Subrequests the run spends after verifying, kept back from the start:
/// [`store::APPLY_STATEMENTS`], then the snapshot phase's D1 statements (2
/// for the live snapshot, 1 status count, 1 stats check, 2 for the hourly
/// totals, at most 3 documents and 3 reconciles) and the 2 Cache API purges.
const TAIL: u32 = store::APPLY_STATEMENTS + 2 + 1 + 1 + 2 + 3 + 3 + 2;

pub async fn run(env: &Env) -> Result<()> {
    let started_ms = Date::now().as_millis();
    let now = started_ms / 1000;
    let db = store::db(env)?;
    let config = crate::config(env);
    let mut ledger = Ledger::new(TAIL);
    let mut run = CronRun {
        test_dial: config.as_ref().is_ok_and(|c| c.test_dial.is_some()),
        ..CronRun::default()
    };

    // Verification needs a valid configuration (TEST_DIAL above all). It
    // never stops the snapshots: a D1 error here is logged, and the run goes
    // on to render them.
    if let Ok(config) = &config {
        ledger.spend(store::QUEUE_STATEMENTS);
        let applied = match store::cron_queue(&db, now).await {
            Ok(queue) => {
                let writes = verify(&queue, now, config, &mut ledger, &mut run).await;
                store::apply_cron(&db, now, &writes)
                    .await
                    .map_err(|e| ("apply_cron", e))
            }
            Err(e) => Err(("cron_queue", e)),
        };
        match applied {
            Ok(removed) => {
                run.candidates_added = removed.candidates_added as u64;
                run.purged = removed.purged as u64;
                run.evicted = removed.evicted as u64;
            }
            Err((statement, e)) => {
                run.verification_failed = true;
                console_error!("{}", d1_error_line(statement, &e.to_string()));
            }
        }
    }
    ledger.spend(TAIL);
    run.subrequests = u64::from(ledger.used());

    let snap = store::live_snapshot(&db, now).await?;
    let (hubs_current, hubs_total, caches) =
        store::status_counts(&db, now.saturating_sub(LIST_WINDOW)).await?;
    let hourly = if stats_due(&db, now).await? {
        Some(render_hourly(&db, now).await?)
    } else {
        None
    };

    run.last_run_at = now;
    run.last_run_ms = Date::now().as_millis().saturating_sub(started_ms);
    let summary = CronSummary {
        hubs: stats::hub_counts(&hubs_current, snap.live_hubs(now) as u64, hubs_total),
        caches: stats::cache_counts(&caches),
        run,
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
    // The cap counters are reconciled in the same batch, so a lost or
    // concurrent increment never outlives a cron period.
    store::write_snapshots(&db, now, &docs, store::reconcile_counts(&db)).await?;
    // This isolate's copies are now stale. It already holds the new hostfile,
    // so it remembers that (no read, and still a fallback if D1 fails next);
    // other isolates and data centres catch up within their TTLs.
    crate::memo::set_hostfile(now, crate::memo::TTL, false, std::rc::Rc::new(snap.clone()));
    if let Ok(config) = &config {
        crate::pages::purge(config).await;
    }

    log_run(&snap, now, &summary, hourly.is_some());
    Ok(())
}

/// Where to dial for a target, or `None` when `TEST_DIAL` blocks it.
fn target(config: &Config, key: &str, host: &str, port: u16) -> Option<(String, u16)> {
    match config.dial(key) {
        Dial::Direct => Some((host.to_owned(), port)),
        Dial::Mapped(a) => Some((a.ip().to_string(), a.port())),
        Dial::Blocked => None,
    }
}

/// Check the picked hubs and ping the due caches; decide every write.
async fn verify(
    queue: &store::CronQueue,
    now: u64,
    config: &Config,
    ledger: &mut Ledger,
    run: &mut CronRun,
) -> CronWrites {
    let mut writes = CronWrites::default();
    // Room under the live cap. A `dead` hub that passes its check becomes
    // live, so returning hubs are picked only while there is room for them,
    // before dialling: one that cannot be promoted waits in its queue,
    // unpenalised (ADR-0012). Candidates get what they leave.
    let mut room = MAX_HUBS.saturating_sub(queue.hubs_live);
    let (mut offered, returning) = check_hubs(queue, room, config, ledger, run, &mut writes).await;
    room = room.saturating_sub(returning);
    // A candidate that failed in this run is not offered back in the same run.
    offered.retain(|(a, _)| !writes.deletes.contains(&a.ip().to_string()));
    let room = MAX_UNVERIFIED
        .saturating_sub(queue.hubs_unverified)
        .min(room);
    writes.candidates =
        verify::candidates(offered, now, usize::try_from(room).unwrap_or(usize::MAX));
    // After the handshakes, so their sockets are closed: 6 connections at most.
    for cache in &queue.caches {
        if let Some(ping) = ping_cache(cache, config, ledger, run).await {
            writes.caches.push(ping);
        }
    }
    writes
}

/// Handshake with the picked hubs in waves of [`HANDSHAKE_CONCURRENCY`].
/// A socket whose close timed out may still hold a connection slot, so each
/// leaked socket shrinks the next waves; with no slot left, the rest wait for
/// the next run. Returning (`dead`) hubs are picked only up to `room`.
/// Returns the `X-Try-Hubs` entries offered by hubs that passed, and how many
/// returning hubs were picked.
async fn check_hubs(
    queue: &store::CronQueue,
    room: u64,
    config: &Config,
    ledger: &mut Ledger,
    run: &mut CronRun,
    writes: &mut CronWrites,
) -> (Vec<(std::net::SocketAddrV4, Option<u64>)>, u64) {
    let mut queues = queue.hubs.clone();
    queues[2].truncate(usize::try_from(room).unwrap_or(usize::MAX));
    let picked = verify::pick(queues, HANDSHAKES_PER_RUN);
    let returning = picked
        .iter()
        .filter(|(c, _)| *c == verify::Class::Returning)
        .count() as u64;
    let mut jobs = Vec::new();
    for (class, hub) in picked {
        run.picked += 1;
        let dial = target(config, &hub.ip, &hub.ip, hub.port);
        // A blocked target costs nothing; anything dialled is counted first.
        if dial.is_some() && !ledger.try_take() {
            run.skipped_budget += 1;
            continue;
        }
        jobs.push((class, hub, dial));
    }
    let mut results: Vec<(verify::Class, QueuedHub, Option<Exchange>)> = Vec::new();
    let mut jobs = jobs.into_iter().peekable();
    while jobs.peek().is_some() {
        let slots = HANDSHAKE_CONCURRENCY.saturating_sub(leaked(run));
        if slots == 0 {
            run.skipped_budget += jobs.count() as u64;
            break;
        }
        let wave = jobs
            .by_ref()
            .take(slots)
            .map(|(class, hub, dial)| async move {
                match dial {
                    Some((host, port)) => {
                        let (exchange, leak) = dial::handshake(&host, port).await;
                        (class, hub, Some(exchange), leak)
                    }
                    None => (class, hub, None, dial::Leak(false)),
                }
            });
        for (class, hub, exchange, leak) in futures_util::future::join_all(wave).await {
            run.leaked_sockets += u64::from(leak.0);
            results.push((class, hub, exchange));
        }
    }

    let mut offered = Vec::new();
    for (class, hub, exchange) in results {
        let verdict = exchange.as_ref().map_or(Verdict::Dead, Exchange::verdict);
        let action = verify::after_check(
            &Checked {
                status: &hub.status,
                source: &hub.source,
                failures: hub.failures,
            },
            verdict,
        );
        match verdict {
            Verdict::Verified => run.verified += 1,
            Verdict::AliveFull => run.alive_full += 1,
            Verdict::Dead => run.dead += 1,
        }
        // Only a hub's reply vouches for the hubs it names (ADR-0012).
        let named = exchange
            .as_ref()
            .map(Exchange::try_hubs)
            .unwrap_or_default();
        if verdict != Verdict::Dead {
            offered.extend(named.iter().copied());
        }
        log_hub(
            config,
            &hub,
            class,
            exchange.as_ref(),
            verdict,
            &action,
            named.len(),
        );
        match action {
            HubAction::Set {
                status,
                failures,
                seen,
            } => writes.verdicts.push(HubVerdict {
                ip: hub.ip,
                port: hub.port,
                status,
                failures,
                seen,
            }),
            HubAction::Delete => {
                run.deleted += 1;
                writes.deletes.push(hub.ip);
            }
        }
    }
    (offered, returning)
}

/// Ping one cache, unless the budget is spent. A `TEST_DIAL` block counts
/// as a failed ping and costs nothing.
async fn ping_cache(
    cache: &store::QueuedCache,
    config: &Config,
    ledger: &mut Ledger,
    run: &mut CronRun,
) -> Option<CachePing> {
    let (host, _, request) = ping::socket_request(&cache.url);
    let dial = config.dial(&host);
    // A socket that leaked may still hold a slot; keep one free for the ping.
    if dial != Dial::Blocked && (leaked(run) > HANDSHAKE_CONCURRENCY || !ledger.try_take()) {
        run.skipped_budget += 1;
        return None;
    }
    let (pong, leak) = match (dial, ping::via(&cache.url)) {
        (Dial::Blocked, _) => (None, dial::Leak(false)),
        // TEST_DIAL: the same method, aimed at the mapped loopback address.
        (Dial::Mapped(a), Via::Fetch(_)) => (
            dial::ping_fetch(&ping::fetch_url_at(&cache.url, a)).await,
            dial::Leak(false),
        ),
        (Dial::Mapped(a), Via::Socket { .. }) => {
            dial::ping_socket(&a.ip().to_string(), a.port(), &request).await
        }
        (Dial::Direct, Via::Fetch(url)) => (dial::ping_fetch(&url).await, dial::Leak(false)),
        (
            Dial::Direct,
            Via::Socket {
                host,
                port,
                request,
            },
        ) => dial::ping_socket(&host, port, &request).await,
    };
    run.leaked_sockets += u64::from(leak.0);
    run.caches_pinged += 1;
    let action = verify::after_ping(cache.failures, pong);
    let result = match action {
        CacheAction::Verified { .. } => {
            run.caches_verified += 1;
            "verified"
        }
        CacheAction::Failed { .. } => {
            run.caches_failed += 1;
            "failed"
        }
        CacheAction::Delete => {
            run.caches_failed += 1;
            "deleted"
        }
    };
    console_log!(
        "{}",
        serde_json::json!({"kind": "cron_cache", "url": cache.url, "result": result})
    );
    Some(CachePing {
        url: cache.url.clone(),
        action,
    })
}

/// Sockets whose close timed out so far in this run.
fn leaked(run: &CronRun) -> usize {
    usize::try_from(run.leaked_sockets).unwrap_or(usize::MAX)
}

/// The address to log for a checked hub. A listing may be logged (addendum
/// C): a hub that was listed, one this check lists, an `X-Try-Hubs`
/// candidate, or an operator's seed. A self-registration that never passed a
/// check is only a requester's address, so it is left out.
fn loggable_ip(hub: &QueuedHub, verdict: Verdict) -> Option<&str> {
    let listing = matches!(hub.status.as_str(), "verified" | "alive_full")
        || verdict != Verdict::Dead
        || matches!(hub.source.as_str(), "try_hubs" | "seed");
    listing.then_some(hub.ip.as_str())
}

/// One line per hub checked, and with `GWC_TRACE=1` one with its reply.
fn log_hub(
    config: &Config,
    hub: &QueuedHub,
    class: verify::Class,
    exchange: Option<&Exchange>,
    verdict: Verdict,
    action: &HubAction,
    named: usize,
) {
    let ip = loggable_ip(hub, verdict);
    let result = match exchange {
        None => "blocked".to_owned(),
        Some(Exchange::Reply(r)) => r.status.to_string(),
        Some(Exchange::Unreachable) => "unreachable".to_owned(),
        Some(Exchange::Timeout) => "timeout".to_owned(),
        Some(Exchange::Closed) => "closed".to_owned(),
        Some(Exchange::Invalid) => "invalid".to_owned(),
    };
    let action = match action {
        HubAction::Set { status, .. } => *status,
        HubAction::Delete => "deleted",
    };
    console_log!(
        "{}",
        serde_json::json!({
            "kind": "cron_hub",
            "ip": ip,
            "port": hub.port,
            "class": class.as_str(),
            "was": hub.status,
            "result": result,
            "now": action,
            "try_hubs": named,
        })
    );
    // A reply's headers can name the peer's own address (`Listen-IP`), so a
    // hub whose address is not logged gets no trace either.
    if config.trace
        && ip.is_some()
        && let Some(Exchange::Reply(r)) = exchange
    {
        console_log!(
            "{}",
            serde_json::json!({
                "kind": "cron_trace",
                "ip": ip,
                "port": hub.port,
                "status": r.status,
                "reason": r.reason,
                "headers": r.headers,
            })
        );
    }
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

/// One line per run.
fn log_run(snap: &Snapshot, now: u64, summary: &CronSummary, stats_rendered: bool) {
    let r = &summary.run;
    let line = serde_json::json!({
        "kind": "cron",
        "picked": r.picked,
        "verified": r.verified,
        "alive_full": r.alive_full,
        "dead": r.dead,
        "deleted": r.deleted,
        "candidates_added": r.candidates_added,
        "purged": r.purged,
        "evicted": r.evicted,
        "caches_pinged": r.caches_pinged,
        "caches_verified": r.caches_verified,
        "caches_failed": r.caches_failed,
        "skipped_budget": r.skipped_budget,
        "leaked_sockets": r.leaked_sockets,
        "verification_failed": r.verification_failed,
        "subrequests": r.subrequests,
        "test_dial": r.test_dial,
        "ms": r.last_run_ms,
        "hubs_served": snap.live_hubs(now),
        "caches": snap.caches.len(),
        "stats_rendered": stats_rendered,
    });
    console_log!("{line}");
}
