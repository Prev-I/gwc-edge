//! The cron run (every 10 minutes, ADR-0012):
//! 1. read the hub and cache queues and the cap counters (one batch);
//! 2. handshake with up to 16 hubs, 5 at a time, then ping up to 2 caches;
//!    then, with `ADVERTISE=on`, advertise this cache to at most one of them
//!    whose reply showed it does not list this one (ADR-0021);
//! 3. apply every result, store new `X-Try-Hubs` candidates, purge old
//!    `dead` rows, record the caches deleted after failed pings in
//!    `dead_caches` and expire old records (ADR-0022) (one batch);
//! 4. render the hostfile snapshot and the cron summary every run, and the
//!    hourly stats once an hour, reconciling the cap counters (one batch),
//!    after step 3 so the hostfile lists the hubs just verified;
//! 5. wait a little for the sockets whose close timed out, and log how long
//!    they took (ADR-0019).
//!
//! The budget is counted, not timed: every socket, fetch and D1 statement is
//! one subrequest against `verify::SUBREQUEST_CEILING`.

use gwc_core::advert::{self, UrlScan};
use gwc_core::config::{Config, Dial};
use gwc_core::handshake::{Exchange, Verdict};
use gwc_core::log::d1_error_line;
use gwc_core::ping::{self, Via};
use gwc_core::snapshot::{LIST_WINDOW, Snapshot};
use gwc_core::stats::{self, CronRun, CronSummary, Hourly};
use gwc_core::update::{MAX_HUBS, MAX_UNVERIFIED};
use gwc_core::verify::{
    self, CacheAction, Checked, HANDSHAKE_CONCURRENCY, HANDSHAKES_PER_RUN, HubAction, LEAK_WAIT_MS,
    LeakReport, Ledger, SLOT_WAIT_MS,
};
use std::time::Duration;
use worker::{D1Database, Date, Delay, Env, Result, console_error, console_log};

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

// A full run fits the ledger: the queue read, every handshake and ping, and
// the tail. A statement added to the run without room for it would
// otherwise skip pings silently (`skipped_budget`).
//
// The one exception is the advertisement (ADR-0021, "The advertisement"):
// its two units (the stamp and the request) are not reserved here, so a
// full run skips it (`advert_skipped`) and the hubs keep their priority.
#[allow(clippy::cast_possible_truncation)] // small constants
const _: () = assert!(
    store::QUEUE_STATEMENTS
        + gwc_core::verify::HANDSHAKES_PER_RUN as u32
        + gwc_core::verify::PINGS_PER_RUN as u32
        + TAIL
        <= gwc_core::verify::SUBREQUEST_CEILING,
    "a full cron run exceeds the subrequest ceiling"
);

pub async fn run(env: &Env) -> Result<()> {
    let started_ms = Date::now().as_millis();
    let now = started_ms / 1000;
    let db = store::db(env)?;
    let config = crate::config(env);
    let mut ledger = Ledger::new(TAIL);
    let mut run = CronRun {
        test_dial: config.as_ref().is_ok_and(|c| c.test_dial.is_some()),
        advertise: if config.as_ref().is_ok_and(|c| c.advertise) {
            "on"
        } else {
            "off"
        }
        .to_owned(),
        ..CronRun::default()
    };
    let mut leaks = Vec::new();

    // Verification needs a valid configuration (TEST_DIAL above all). It
    // never stops the snapshots: a D1 error here is logged, and the run goes
    // on to render them.
    if let Ok(config) = &config {
        ledger.spend(store::QUEUE_STATEMENTS);
        let applied = match store::cron_queue(&db, now).await {
            Ok(queue) => {
                let (writes, candidate) =
                    verify(&queue, now, config, &mut ledger, &mut run, &mut leaks).await;
                // The run's last outbound request, before the results batch,
                // so its counters reach the stored `cron` document.
                if let Some(candidate) = candidate {
                    advertise(
                        &db,
                        &candidate,
                        config,
                        now,
                        &mut ledger,
                        &mut run,
                        &mut leaks,
                    )
                    .await;
                }
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
    report_leaks(leaks).await;
    Ok(())
}

/// How often the wait for leaked sockets looks at them.
const LEAK_POLL_MS: u64 = 250;

/// Wait, up to [`LEAK_WAIT_MS`], for the sockets whose close timed out, and
/// log how long each connection stayed open, from its connect call to the
/// end of its close (ADR-0019). Each close records its own end as it
/// happens; this only waits. After every write, so the wait delays nothing
/// but the end of the invocation.
async fn report_leaks(leaks: Vec<dial::PendingClose>) {
    if leaks.is_empty() {
        return;
    }
    let started = Date::now().as_millis();
    let elapsed = || Date::now().as_millis().saturating_sub(started);
    while elapsed() < LEAK_WAIT_MS && leaks.iter().any(|p| p.closed.get().is_none()) {
        Delay::from(Duration::from_millis(LEAK_POLL_MS)).await;
    }
    let closed: Vec<(u64, dial::Closed)> = leaks
        .iter()
        .filter_map(|p| Some((p.connected_ms, p.closed.get()?)))
        .collect();
    let report = LeakReport {
        open_ms: closed
            .iter()
            .map(|(connected, c)| c.at_ms.saturating_sub(*connected))
            .collect(),
        close_errors: closed.iter().filter(|(_, c)| !c.ok).count() as u64,
        unsettled: (leaks.len() - closed.len()) as u64,
        waited_ms: elapsed(),
    };
    console_log!("{}", report.to_json());
}

/// Where to dial for a target, or `None` when `TEST_DIAL` blocks it.
fn target(config: &Config, key: &str, host: &str, port: u16) -> Option<(String, u16)> {
    match config.dial(key) {
        Dial::Direct => Some((host.to_owned(), port)),
        Dial::Mapped(a) => Some((a.ip().to_string(), a.port())),
        Dial::Blocked => None,
    }
}

/// A pinged cache whose reply showed it does not list this one (ADR-0021).
struct Candidate {
    url: String,
    host: String,
    pong: String,
}

/// Check the picked hubs and ping the due caches; decide every write. Also
/// returns the first pinged cache, in queue order, that qualifies for the
/// advertisement.
async fn verify(
    queue: &store::CronQueue,
    now: u64,
    config: &Config,
    ledger: &mut Ledger,
    run: &mut CronRun,
    leaks: &mut Vec<dial::PendingClose>,
) -> (CronWrites, Option<Candidate>) {
    let mut writes = CronWrites::default();
    // Room under the live cap. A `dead` hub that passes its check becomes
    // live, so returning hubs are picked only while there is room for them,
    // before dialling: one that cannot be promoted waits in its queue,
    // unpenalised (ADR-0012). Candidates get what they leave.
    let mut room = MAX_HUBS.saturating_sub(queue.hubs_live);
    let (mut offered, returning) =
        check_hubs(queue, room, config, ledger, run, leaks, &mut writes).await;
    room = room.saturating_sub(returning);
    // A candidate that failed in this run is not offered back in the same run.
    offered.retain(|(a, _)| !writes.deletes.contains(&a.ip().to_string()));
    let room = MAX_UNVERIFIED
        .saturating_sub(queue.hubs_unverified)
        .min(room);
    writes.candidates =
        verify::candidates(offered, now, usize::try_from(room).unwrap_or(usize::MAX));
    // After the handshakes, so their sockets are closed: 6 connections at most.
    let mut candidate = None;
    for cache in &queue.caches {
        if let Some((ping, qualifies)) = ping_cache(cache, now, config, ledger, run, leaks).await {
            writes.caches.push(ping);
            candidate = candidate.or(qualifies);
        }
    }
    (writes, candidate)
}

/// Handshake with the picked hubs in waves of [`HANDSHAKE_CONCURRENCY`].
/// A socket whose close timed out holds a connection slot until its close
/// completes, so it shrinks the waves meanwhile; with no slot left after
/// [`SLOT_WAIT_MS`], the rest wait for the next run (ADR-0020). Returning
/// (`dead`) hubs are picked only up to `room`.
/// Returns the `X-Try-Hubs` entries offered by hubs that passed, and how many
/// returning hubs were picked.
async fn check_hubs(
    queue: &store::CronQueue,
    room: u64,
    config: &Config,
    ledger: &mut Ledger,
    run: &mut CronRun,
    leaks: &mut Vec<dial::PendingClose>,
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
        let slots = free_slots(leaks, HANDSHAKE_CONCURRENCY, run).await;
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
                    None => (class, hub, None, dial::Leak::default()),
                }
            });
        for (class, hub, exchange, leak) in futures_util::future::join_all(wave).await {
            note_leak(run, leaks, leak);
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
/// as a failed ping and costs nothing. When the cache is due an
/// advertisement, the ping scans its urlfile too, and the cache is returned
/// as a [`Candidate`] if its pong is not Skulls' and its reply, read whole,
/// does not list this one (ADR-0021).
async fn ping_cache(
    cache: &store::QueuedCache,
    now: u64,
    config: &Config,
    ledger: &mut Ledger,
    run: &mut CronRun,
    leaks: &mut Vec<dial::PendingClose>,
) -> Option<(CachePing, Option<Candidate>)> {
    let (host, _, request) = ping::socket_request(&cache.url);
    let due = advert::due(
        config.advertise,
        cache.verified,
        &cache.host,
        cache.advertised,
        &config.canonical_url,
        now,
    );
    let scan = due.then(|| UrlScan::new(&cache.host, &config.canonical_url));
    let dial = config.dial(&host);
    // The ping may use the sixth connection, unless leaked sockets still
    // closing hold every one.
    if dial != Dial::Blocked
        && (free_slots(leaks, HANDSHAKE_CONCURRENCY + 1, run).await == 0 || !ledger.try_take())
    {
        run.skipped_budget += 1;
        return None;
    }
    let (outcome, leak) = match (dial, ping::via(&cache.url)) {
        (Dial::Blocked, _) => (
            dial::PingResult {
                pong: None,
                listing: ping::Listing::Unknown,
            },
            dial::Leak::default(),
        ),
        // TEST_DIAL: the same method, aimed at the mapped loopback address.
        (Dial::Mapped(a), Via::Fetch(_)) => (
            dial::ping_fetch(&ping::fetch_url_at(&cache.url, a), scan).await,
            dial::Leak::default(),
        ),
        (Dial::Mapped(a), Via::Socket { .. }) => {
            dial::ping_socket(&a.ip().to_string(), a.port(), &request, scan).await
        }
        (Dial::Direct, Via::Fetch(url)) => {
            (dial::ping_fetch(&url, scan).await, dial::Leak::default())
        }
        (
            Dial::Direct,
            Via::Socket {
                host,
                port,
                request,
            },
        ) => dial::ping_socket(&host, port, &request, scan).await,
    };
    note_leak(run, leaks, leak);
    run.caches_pinged += 1;
    let candidate = match (&outcome.pong, outcome.listing) {
        (Some(name), ping::Listing::Absent) if due && !advert::excluded(name) => Some(Candidate {
            url: cache.url.clone(),
            host: cache.host.clone(),
            pong: name.clone(),
        }),
        _ => None,
    };
    let action = verify::after_ping(cache.failures, outcome.pong);
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
    Some((
        CachePing {
            url: cache.url.clone(),
            action,
        },
        candidate,
    ))
}

/// Advertise this cache to `candidate` (ADR-0021, "The advertisement"):
/// only with a free connection slot now (no wait) and both ledger units, the
/// stamp's and the request's; otherwise it is skipped. The stamp comes
/// first: if it fails, nothing is sent. The reply's class is counted and
/// logged, never stored.
async fn advertise(
    db: &D1Database,
    candidate: &Candidate,
    config: &Config,
    now: u64,
    ledger: &mut Ledger,
    run: &mut CronRun,
    leaks: &mut Vec<dial::PendingClose>,
) {
    let (host, _, _) = ping::socket_request(&candidate.url);
    // A candidate answered its ping, so `TEST_DIAL` did not block it.
    let at = match config.dial(&host) {
        Dial::Direct => None,
        Dial::Mapped(a) => Some(a),
        Dial::Blocked => {
            run.advert_skipped += 1;
            return;
        }
    };
    if verify::slots(HANDSHAKE_CONCURRENCY + 1, open_leaks(leaks)) == 0 || !ledger.try_take_n(2) {
        run.advert_skipped += 1;
        return;
    }
    // A failed stamp still spends both units, by design: both are checked
    // together before either is taken.
    if let Err(e) = store::stamp_advertised(db, &candidate.host, now).await {
        console_error!("{}", d1_error_line("stamp_advertised", &e.to_string()));
        run.advert_skipped += 1;
        return;
    }
    let started = Date::now().as_millis();
    // The way the ping went; with `TEST_DIAL`, aimed at the mapped address.
    let class = match (at, advert::via(&candidate.url, &config.canonical_url)) {
        (Some(a), Via::Fetch(url)) => dial::advertise_fetch(&ping::fetch_at(&url, a)).await,
        (None, Via::Fetch(url)) => dial::advertise_fetch(&url).await,
        (
            at,
            Via::Socket {
                host,
                port,
                request,
            },
        ) => {
            let (host, port) = at.map_or((host, port), |a| (a.ip().to_string(), a.port()));
            let (class, leak) = dial::advertise_socket(&host, port, &request).await;
            note_leak(run, leaks, leak);
            class
        }
    };
    run.caches_advertised += 1;
    match class {
        advert::Class::Ok => run.adverts_ok += 1,
        advert::Class::Warned => run.adverts_warned += 1,
        advert::Class::Failed => run.adverts_failed += 1,
    }
    console_log!(
        "{}",
        serde_json::json!({
            "kind": "cron_advert",
            "url": candidate.url,
            "pong": candidate.pong,
            "class": class.as_str(),
            "ms": Date::now().as_millis().saturating_sub(started),
        })
    );
}

/// Count a socket whose close timed out, and keep its close for
/// [`report_leaks`].
fn note_leak(run: &mut CronRun, leaks: &mut Vec<dial::PendingClose>, leak: dial::Leak) {
    if let Some(pending) = leak.0 {
        run.leaked_sockets += 1;
        leaks.push(pending);
    }
}

/// Leaked sockets that may still hold a connection slot: their close has
/// not completed, or it failed (ADR-0020).
fn open_leaks(leaks: &[dial::PendingClose]) -> usize {
    leaks.iter().filter(|p| p.holds_slot()).count()
}

/// Connections free out of `limit`. When leaked sockets hold them all, wait
/// up to [`SLOT_WAIT_MS`] for one of their closes to complete (ADR-0020).
/// The wait costs wall time only, and is added to `slot_wait_ms`.
async fn free_slots(leaks: &[dial::PendingClose], limit: usize, run: &mut CronRun) -> usize {
    let started = Date::now().as_millis();
    loop {
        let free = verify::slots(limit, open_leaks(leaks));
        let waited = Date::now().as_millis().saturating_sub(started);
        if free > 0 || waited >= SLOT_WAIT_MS {
            run.slot_wait_ms += waited;
            return free;
        }
        Delay::from(Duration::from_millis(LEAK_POLL_MS)).await;
    }
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
        "slot_wait_ms": r.slot_wait_ms,
        "verification_failed": r.verification_failed,
        "subrequests": r.subrequests,
        "test_dial": r.test_dial,
        "advertise": r.advertise,
        "caches_advertised": r.caches_advertised,
        "adverts_ok": r.adverts_ok,
        "adverts_warned": r.adverts_warned,
        "adverts_failed": r.adverts_failed,
        "advert_skipped": r.advert_skipped,
        "ms": r.last_run_ms,
        "hubs_served": snap.live_hubs(now),
        "caches": snap.caches.len(),
        "stats_rendered": stats_rendered,
    });
    console_log!("{line}");
}
