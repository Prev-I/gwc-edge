//! The GWebCache endpoint and the hostfile read path.

use std::rc::Rc;

use gwc_core::config::Config;
use gwc_core::counters::{Key, Kind};
use gwc_core::log::Outcome;
use gwc_core::query::{self, Net, Query};
use gwc_core::response::{Line, Refusal, get_reply, render, update_reply};
use gwc_core::snapshot::{MAX_SNAPSHOT_AGE, Snapshot};
use worker::{Env, Result};

use crate::probe::Probe;
use crate::{memo, store};

/// What answering a GWC request produced, for the response and the log line.
pub struct Answer {
    pub body: String,
    pub key: Key,
    pub outcome: Outcome,
    pub hosts_sent: usize,
    pub caches_sent: usize,
    /// Where the hostfile came from, if one was needed.
    pub snapshot: Option<&'static str>,
}

/// Answer a GWebCache request. The reply is always protocol-correct: in
/// degraded mode (D1 failed) a get still gets the pong if asked and the self
/// `u|` line, and an update still gets period-then-OK.
pub async fn handle(
    raw_query: &str,
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
    if q.is_update() {
        // Storage arrives with the write path (phase 4); every update a
        // legitimate client sends is answered OK regardless.
        return Answer::new(&update_reply(Ok(())), key, Outcome::Ok, config, None);
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
