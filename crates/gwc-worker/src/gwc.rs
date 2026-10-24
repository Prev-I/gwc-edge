//! The GWebCache endpoint: rate limiting, the hostfile read path and the
//! update write path.

use std::rc::Rc;

use gwc_core::config::Config;
use gwc_core::counters::{Key, Kind};
use gwc_core::log::Outcome;
use gwc_core::policy::RateKind;
use gwc_core::query::{self, Net, Query};
use gwc_core::response::{Line, Refusal, get_reply, render, update_reply};
use gwc_core::snapshot::{MAX_SNAPSHOT_AGE, Snapshot};
use gwc_core::update::{self, CachePlan, HubPlan};
use worker::{Env, Result};

use crate::probe::Probe;
use crate::{limiter, memo, store};

/// What answering a GWC request produced, for the response and the log line.
pub struct Answer {
    pub body: String,
    pub key: Key,
    pub outcome: Outcome,
    pub hosts_sent: usize,
    pub caches_sent: usize,
    /// Where the hostfile came from, if one was needed.
    pub snapshot: Option<&'static str>,
    /// For updates: what each part did (`gwc_core::log::RequestLine::update`).
    pub update: Option<String>,
}

/// Answer a GWebCache request. The reply is always protocol-correct: in
/// degraded mode (D1 failed) a get still gets the pong if asked and the self
/// `u|` line, and an update still gets period-then-OK.
pub async fn handle(
    raw_query: &str,
    source: &str,
    https: bool,
    now: u64,
    config: &Config,
    env: &Env,
    probe: &mut Probe,
) -> Answer {
    let q = match query::parse(raw_query) {
        Ok(q) => q,
        Err(e) => {
            return Answer::new(
                &[Line::Error(e.reason())],
                Key::new(Kind::Error, None, https),
                Outcome::Malformed,
                config,
                None,
            );
        }
    };
    let client = Some((q.client.vendor.as_str(), q.client.version.as_str()));
    if gwc_core::policy::is_denied(&q.client.vendor, &config.denylist) {
        let lines = if q.is_update() {
            update_reply(Err(Refusal::ClientNotSupported))
        } else {
            Vec::new()
        };
        return Answer::new(
            &lines,
            Key::new(Kind::Denied, client, https),
            Outcome::Denied,
            config,
            None,
        );
    }
    let key = Key::new(Kind::of(&q), client, https);
    let limited = Key::new(Kind::Limited, client, https);
    // Only what can cost D1 is limited: Gnutella2 file gets and updates.
    let rate_kind = RateKind::of(&q).filter(|_| q.net == Net::Gnutella2);
    if let Some(kind) = rate_kind
        && !limiter::allowed(env, source, kind).await
    {
        // Over the limit: the same successful reply, without data or storage.
        let lines = if q.is_update() {
            update_reply(Ok(()))
        } else {
            get_reply(&q, &[], &[], &config.canonical_url)
        };
        return Answer::new(&lines, limited, Outcome::RateLimited, config, None);
    }
    if q.is_update() {
        let stored = store_update(&q, source, now, config, env, probe).await;
        let outcome = if probe.failed() {
            Outcome::Degraded
        } else {
            Outcome::Ok
        };
        let mut a = Answer::new(&update_reply(Ok(())), key, outcome, config, None);
        a.update = Some(stored);
        return a;
    }
    let (lines, snapshot, degraded) = get_lines(&q, now, config, env, probe).await;
    let outcome = if degraded {
        Outcome::Degraded
    } else {
        Outcome::Ok
    };
    Answer::new(&lines, key, outcome, config, snapshot)
}

impl Answer {
    fn new(
        lines: &[Line],
        key: Key,
        outcome: Outcome,
        config: &Config,
        snapshot: Option<&'static str>,
    ) -> Answer {
        let hosts_sent = lines.iter().filter(|l| matches!(l, Line::Host(_))).count();
        let caches_sent = lines
            .iter()
            .filter(|l| matches!(l, Line::Url(u) if u.url != config.canonical_url.url))
            .count();
        Answer {
            body: render(lines),
            key,
            outcome,
            hosts_sent,
            caches_sent,
            snapshot,
            update: None,
        }
    }
}

async fn get_lines(
    q: &Query,
    now: u64,
    config: &Config,
    env: &Env,
    probe: &mut Probe,
) -> (Vec<Line>, Option<&'static str>, bool) {
    if q.net == Net::Gnutella2 && (q.hostfile || q.urlfile) {
        let (snap, source, degraded) = hostfile(env, now, probe).await;
        let lines = get_reply(
            q,
            &snap.hosts(now, config.hostfile_count),
            &snap.caches(now, config.urlfile_count),
            &config.canonical_url,
        );
        (lines, Some(source), degraded)
    } else {
        (get_reply(q, &[], &[], &config.canonical_url), None, false)
    }
}

/// The hostfile snapshot to serve, where it came from, and whether the reply
/// is degraded (D1 failed now, or when this copy was remembered). Cheapest
/// source first:
///
/// 1. `memo`: this isolate's copy, while fresh (no read);
/// 2. `stored`: the stored snapshot (1 row), unless stale;
/// 3. `live`: a live read of `hubs` and `caches` (cron stalled), written back
///    if the stored row is still the stale one just read, so the next
///    requests in every isolate read it instead of scanning;
/// 4. `stale`: the stale stored snapshot, if the live read failed;
/// 5. `fallback`: the last copy in memory, or an empty snapshot.
///
/// A copy made while a D1 call failed is kept only `memo::ERROR_TTL`, and
/// replies served from it stay degraded until D1 is tried again.
///
/// A get never fails: the reply still carries the self line.
pub async fn hostfile(
    env: &Env,
    now: u64,
    probe: &mut Probe,
) -> (Rc<Snapshot>, &'static str, bool) {
    if let Some((snap, degraded)) = memo::hostfile(now) {
        return (snap, "memo", degraded);
    }
    memo::lease_hostfile(now);
    if let Ok((snap, ttl, source)) = load_hostfile(env, now, probe).await {
        // A copy produced while a D1 call failed (write-back, or the stale
        // path) is kept only until D1 is tried again.
        let degraded = probe.failed();
        let ttl = if degraded { memo::ERROR_TTL } else { ttl };
        (
            memo::set_hostfile(now, ttl, degraded, Rc::new(snap)),
            source,
            degraded,
        )
    } else {
        let fallback = memo::last_hostfile().unwrap_or_else(|| Rc::new(Snapshot::default()));
        (
            memo::set_hostfile(now, memo::ERROR_TTL, true, fallback),
            "fallback",
            true,
        )
    }
}

/// Steps 2 to 4 of [`hostfile`]: the snapshot, how long to keep it, its source.
async fn load_hostfile(
    env: &Env,
    now: u64,
    probe: &mut Probe,
) -> Result<(Snapshot, u64, &'static str)> {
    let db = probe.run("binding", || async { store::db(env) }).await?;
    let row = probe
        .run("read_snapshot", || {
            store::read_snapshot(&db, store::HOSTFILE)
        })
        .await?;
    let stored_at = row.as_ref().map(|(at, _)| *at);
    let mut stored = row.and_then(|(_, body)| Snapshot::parse(&body));
    if let Some(snap) = stored.take_if(|s| !s.is_stale(now)) {
        return Ok((snap, memo::TTL, "stored"));
    }
    match probe
        .run("live_snapshot", || store::live_snapshot(&db, now))
        .await
    {
        Ok(live) => {
            // Only replace a row that is absent or stale by its own timestamp:
            // a fresh row this build cannot parse (another version during a
            // rollout) is left alone, not fought over.
            let replaceable = stored_at.is_none_or(|at| now.saturating_sub(at) > MAX_SNAPSHOT_AGE);
            if replaceable {
                let body = live.render();
                let _ = probe
                    .try_run("write_back_snapshot", || {
                        store::write_back_snapshot(&db, store::HOSTFILE, now, &body, stored_at)
                    })
                    .await;
            }
            Ok((live, memo::LIVE_TTL, "live"))
        }
        Err(e) => match stored {
            Some(snap) => Ok((snap, memo::LIVE_TTL, "stale")),
            None => Err(e),
        },
    }
}

/// Store what an update carries, part by part (`gwc_core::update`), and
/// say what each part did, for the log line (labels only, never an address
/// or URL). The reply does not depend on it: a D1 failure only makes the
/// request degraded.
async fn store_update(
    q: &Query,
    source: &str,
    now: u64,
    config: &Config,
    env: &Env,
    probe: &mut Probe,
) -> String {
    let hub = update::ip_claim(q, source, now);
    let cache = update::url_claim(q, &config.canonical_url);
    let mut parts: Vec<String> = Vec::new();
    let label = |part: &str, what: &str| format!("{part}:{what}");

    let db = if matches!(hub, Some(Ok(_))) || matches!(cache, Some(Ok(_))) {
        probe.run("binding", || async { store::db(env) }).await.ok()
    } else {
        None // nothing storable: no D1 access at all
    };

    match &hub {
        None => {}
        Some(Err(d)) => parts.push(label("ip", d.as_str())),
        Some(Ok(claim)) => {
            let what = match &db {
                Some(db) => store_hub(db, claim, now, probe).await,
                None => "error",
            };
            parts.push(label("ip", what));
        }
    }
    match &cache {
        None => {}
        Some(Err(d)) => parts.push(label("url", d.as_str())),
        Some(Ok(url)) => {
            let what = match &db {
                Some(db) => store_cache(db, url, &q.client.vendor, now, probe).await,
                None => "error",
            };
            parts.push(label("url", what));
        }
    }
    parts.join(" ")
}

async fn store_hub(
    db: &worker::D1Database,
    claim: &update::HubClaim,
    now: u64,
    probe: &mut Probe,
) -> &'static str {
    let ip = claim.addr.ip().to_string();
    let Ok(existing) = probe.run("read_hub", || store::read_hub(db, &ip)).await else {
        return "error";
    };
    let live = if existing.is_none() {
        match probe
            .run("read_counter", || store::read_counter(db, "hubs_live"))
            .await
        {
            Ok(n) => n,
            Err(_) => return "error",
        }
    } else {
        0
    };
    let plan = update::plan_hub(existing.as_ref(), claim, now, live);
    let written = match plan {
        HubPlan::Insert => {
            probe
                .run("insert_hub", || store::insert_hub(db, claim, now))
                .await
        }
        HubPlan::Refresh => {
            probe
                .run("refresh_hub", || store::refresh_hub(db, claim, now))
                .await
        }
        HubPlan::Demote => {
            probe
                .run("requeue_hub", || store::requeue_hub(db, claim, now, false))
                .await
        }
        HubPlan::Requeue { was_try_hubs } => {
            probe
                .run("requeue_hub", || {
                    store::requeue_hub(db, claim, now, was_try_hubs)
                })
                .await
        }
        HubPlan::Reregister => {
            probe
                .run("reregister_hub", || store::reregister_hub(db, claim, now))
                .await
        }
        HubPlan::Coalesced | HubPlan::Full | HubPlan::BackingOff | HubPlan::Queued => Ok(()),
    };
    if written.is_ok() {
        plan.as_str()
    } else {
        "error"
    }
}

async fn store_cache(
    db: &worker::D1Database,
    url: &gwc_core::cache_url::CacheUrl,
    submitted_by: &str,
    now: u64,
    probe: &mut Probe,
) -> &'static str {
    let Ok(same_host) = probe
        .run("read_caches_for_host", || {
            store::read_caches_for_host(db, &url.host)
        })
        .await
    else {
        return "error";
    };
    let Ok(caches) = probe
        .run("read_counter", || store::read_counter(db, "caches"))
        .await
    else {
        return "error";
    };
    let plan = update::plan_cache(url, &same_host, caches);
    if plan == CachePlan::Insert
        && probe
            .run("insert_cache", || {
                store::insert_cache(db, url, submitted_by, now)
            })
            .await
            .is_err()
    {
        return "error";
    }
    plan.as_str()
}
