//! The verifying cron's rules (ADR-0012): which hubs and caches to check in a
//! run, and what each check's result does to the stored row.
//!
//! - A hub is listed only after a handshake (ADR-0011). A check always sets
//!   `last_checked`; only a successful one resets `failures` or moves
//!   `last_seen`.
//! - `dead` rows hold no slot in the 500-hub cap. They are checked again only
//!   after their owner registers again, and no sooner than [`backoff`] allows,
//!   so a host that keeps failing costs about one write and one handshake a day.
//! - Everything per run is bounded by counts, not time: the Workers runtime
//!   cannot measure CPU from inside, and `Date.now()` stands still between I/O.

use std::collections::HashMap;
use std::net::SocketAddrV4;

use crate::handshake::Verdict;

/// First wait before a failed host is checked again; doubles per failure.
pub const BACKOFF_BASE: u64 = 10 * 60;

/// Longest wait between checks of a failing host.
pub const BACKOFF_MAX: u64 = 24 * 3600;

/// A listed hub is checked again this long after its last check, however
/// often it refreshes itself.
pub const RECHECK: u64 = 90 * 60;

/// A listed hub that failed once is retried this long after.
pub const RETRY: u64 = 10 * 60;

/// Consecutive failures that delist a listed hub: one bad check (a timeout
/// against a loaded hub) is not enough.
pub const SERVABLE_FAILURES: u64 = 2;

/// A `dead` row neither seen nor checked for this long is deleted.
pub const DEAD_TTL: u64 = 48 * 3600;

/// Most `dead` rows kept; beyond it, the longest unchecked go first.
pub const DEAD_MAX: u64 = 1_000;

/// Most rows one run deletes per rule.
pub const PURGE_BATCH: u64 = 100;

/// Most handshakes per run.
pub const HANDSHAKES_PER_RUN: usize = 16;

/// How long a run waits, once its work is done, for the sockets whose close
/// timed out, to learn how long they hold a connection (ADR-0019). Wall
/// time, not CPU: the run is only waiting.
pub const LEAK_WAIT_MS: u64 = 30_000;

/// What became of a run's leaked sockets (ADR-0019): for each one that
/// closed before the wait ended, how long its connection stayed open, from
/// the connect call to the end of the close; how many of those closes failed
/// (their slot was never counted free again, ADR-0020); and how many were
/// still open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeakReport {
    pub open_ms: Vec<u64>,
    pub close_errors: u64,
    pub unsettled: u64,
    pub waited_ms: u64,
}

impl LeakReport {
    /// The `cron_leaks` log line, open times sorted.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut open_ms = self.open_ms.clone();
        open_ms.sort_unstable();
        serde_json::json!({
            "kind": "cron_leaks",
            "leaked": open_ms.len() as u64 + self.unsettled,
            "settled": open_ms.len(),
            "open_ms": open_ms,
            "close_errors": self.close_errors,
            "unsettled": self.unsettled,
            "waited_ms": self.waited_ms,
        })
        .to_string()
    }
}

/// Handshakes in flight at once: the runtime allows 6 open connections.
pub const HANDSHAKE_CONCURRENCY: usize = 5;

/// Connections free for the next dial, out of `limit`, while `open_leaks`
/// leaked sockets are still closing: each holds a slot until its close
/// completes (ADR-0020).
#[must_use]
pub fn slots(limit: usize, open_leaks: usize) -> usize {
    limit.saturating_sub(open_leaks)
}

/// How long the run waits for a slot when leaked sockets hold them all,
/// before leaving the rest for the next run (ADR-0020). Production showed a
/// leaked connection open about 19.5 s from its connect call, so a slot
/// comes back about 10 s after its leak is known.
pub const SLOT_WAIT_MS: u64 = 15_000;

/// One handshake, connect to last byte.
pub const HANDSHAKE_TIMEOUT_MS: u64 = 8_000;

/// How long closing a socket may take before it counts as leaked. The
/// runtime closes a socket only once its connection attempt has settled
/// (ADR-0019), so closing one whose connect is still pending, to an address
/// that drops it, waits for that.
pub const CLOSE_TIMEOUT_MS: u64 = 1_000;

/// Most cache pings per run.
pub const PINGS_PER_RUN: usize = 2;

/// One cache ping, connect to last byte.
pub const PING_TIMEOUT_MS: u64 = 10_000;

/// Most bytes of a ping reply read.
pub const PING_BODY_MAX: usize = 16 * 1024;

/// Consecutive failed pings that delete a cache.
pub const CACHE_FAILURES: u64 = 3;

/// A verified cache is pinged again this long after its last ping.
pub const CACHE_RECHECK: u64 = 6 * 3600;

/// A failed cache is pinged again this long after its last ping.
pub const CACHE_RETRY: u64 = 3600;

/// `X-Try-Hubs` entries older than this are not taken.
pub const CANDIDATE_MAX_AGE: u64 = 24 * 3600;

/// How far in the future an `X-Try-Hubs` timestamp may be.
pub const CANDIDATE_SKEW: u64 = 300;

/// Subrequests a run allows itself, of the 50 the Free plan gives: every
/// socket, fetch and D1 statement counts one.
pub const SUBREQUEST_CEILING: u32 = 45;

/// How long a host that has failed `failures` checks in a row waits before
/// its next one: 10 min, doubling, at most a day.
#[must_use]
pub fn backoff(failures: u64) -> u64 {
    match failures {
        0 => 0,
        f => BACKOFF_BASE
            .saturating_mul(1u64 << (f - 1).min(20))
            .min(BACKOFF_MAX),
    }
}

/// The hub queue's classes, in fill order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Self-registered and never listed since: oldest registration first, so
    /// a row that keeps re-registering moves to the back.
    New,
    /// Listed and due: [`RECHECK`] after the last check, or [`RETRY`] after a
    /// failure.
    Recheck,
    /// `dead`, but registered again after its backoff.
    Returning,
    /// An `X-Try-Hubs` candidate, newest first.
    Candidate,
}

impl Class {
    pub const ALL: [Class; 4] = [
        Class::New,
        Class::Recheck,
        Class::Returning,
        Class::Candidate,
    ];

    /// Handshakes reserved for the class in each run, of
    /// [`HANDSHAKES_PER_RUN`]: a flood in one class cannot starve the others.
    #[must_use]
    pub fn reserved(self) -> usize {
        match self {
            Class::New => 10,
            Class::Recheck => 3,
            Class::Returning => 2,
            Class::Candidate => 1,
        }
    }

    /// Log label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Class::New => "new",
            Class::Recheck => "recheck",
            Class::Returning => "returning",
            Class::Candidate => "candidate",
        }
    }
}

/// Pick up to `n` items from the four class queues (each already in its own
/// order): each class's reservation first, then the unused share in class
/// order.
#[must_use]
pub fn pick<T>(queues: [Vec<T>; 4], n: usize) -> Vec<(Class, T)> {
    let mut rest: Vec<std::vec::IntoIter<T>> = queues.into_iter().map(Vec::into_iter).collect();
    let mut picked = Vec::with_capacity(n);
    for (class, queue) in Class::ALL.into_iter().zip(rest.iter_mut()) {
        let room = n.saturating_sub(picked.len()).min(class.reserved());
        picked.extend(queue.by_ref().take(room).map(|t| (class, t)));
    }
    for (class, queue) in Class::ALL.into_iter().zip(rest.iter_mut()) {
        let room = n.saturating_sub(picked.len());
        picked.extend(queue.by_ref().take(room).map(|t| (class, t)));
    }
    picked
}

/// A hub row as the cron judges it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked<'a> {
    pub status: &'a str,
    pub source: &'a str,
    pub failures: u64,
}

/// What a check does to a hub row. `last_checked = now` in every case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubAction {
    Set {
        status: &'static str,
        failures: u64,
        /// A successful check: `last_seen = now`.
        seen: bool,
    },
    /// A failed `X-Try-Hubs` candidate (ADR-0006).
    Delete,
}

/// Apply a check's verdict to a hub row.
#[must_use]
pub fn after_check(row: &Checked<'_>, verdict: Verdict) -> HubAction {
    let ok = |status| HubAction::Set {
        status,
        failures: 0,
        seen: true,
    };
    let failed = |status| HubAction::Set {
        status,
        failures: row.failures.saturating_add(1),
        seen: false,
    };
    match (verdict, row.status) {
        (Verdict::Verified, _) => ok("verified"),
        (Verdict::AliveFull, _) => ok("alive_full"),
        (Verdict::Dead, "unverified") if row.source == "try_hubs" => HubAction::Delete,
        (Verdict::Dead, "verified") if row.failures + 1 < SERVABLE_FAILURES => failed("verified"),
        (Verdict::Dead, "alive_full") if row.failures + 1 < SERVABLE_FAILURES => {
            failed("alive_full")
        }
        (Verdict::Dead, _) => failed("dead"),
    }
}

/// `X-Try-Hubs` entries worth storing as candidates, with the `last_seen`
/// each gets: one per address (the newest), none older than
/// [`CANDIDATE_MAX_AGE`], newest first, at most `room`. A timestamp in the
/// future beyond [`CANDIDATE_SKEW`], or none, counts as now.
#[must_use]
pub fn candidates(
    entries: impl IntoIterator<Item = (SocketAddrV4, Option<u64>)>,
    now: u64,
    room: usize,
) -> Vec<(SocketAddrV4, u64)> {
    let mut newest: HashMap<std::net::Ipv4Addr, (SocketAddrV4, u64)> = HashMap::new();
    for (addr, at) in entries {
        let at = at.filter(|&t| t <= now + CANDIDATE_SKEW).unwrap_or(now);
        if now.saturating_sub(at) > CANDIDATE_MAX_AGE {
            continue;
        }
        let e = newest.entry(*addr.ip()).or_insert((addr, at));
        if at > e.1 {
            *e = (addr, at);
        }
    }
    let mut out: Vec<_> = newest.into_values().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out.truncate(room);
    out
}

/// What a ping does to a cache row. `last_checked = now` in every case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheAction {
    /// It answered a pong: `verified`, `failures = 0`, `last_verified = now`.
    Verified { pong: String },
    /// No pong: `failed` (no longer served), `failures + 1`.
    Failed { failures: u64 },
    /// [`CACHE_FAILURES`] pings in a row failed.
    Delete,
}

/// Apply a ping's result to a cache row.
#[must_use]
pub fn after_ping(failures: u64, pong: Option<String>) -> CacheAction {
    match pong {
        Some(pong) => CacheAction::Verified { pong },
        None if failures + 1 >= CACHE_FAILURES => CacheAction::Delete,
        None => CacheAction::Failed {
            failures: failures + 1,
        },
    }
}

/// Counts subrequests against [`SUBREQUEST_CEILING`]. The platform does not
/// say whether sockets or batched D1 statements count, so all of them do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ledger {
    used: u32,
    /// Kept back for the statements the run must still issue.
    tail: u32,
}

impl Ledger {
    #[must_use]
    pub fn new(tail: u32) -> Self {
        Ledger { used: 0, tail }
    }

    /// Record `n` subrequests that are issued whatever the budget says.
    pub fn spend(&mut self, n: u32) {
        self.used = self.used.saturating_add(n);
    }

    /// Record one optional subrequest if the budget allows it.
    pub fn try_take(&mut self) -> bool {
        if self.used + self.tail + 1 > SUBREQUEST_CEILING {
            return false;
        }
        self.used += 1;
        true
    }

    #[must_use]
    pub fn used(&self) -> u32 {
        self.used
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_792_744_200;

    #[test]
    fn backoff_doubles_from_ten_minutes_to_a_day() {
        assert_eq!(backoff(0), 0);
        assert_eq!(backoff(1), 600);
        assert_eq!(backoff(2), 1_200);
        assert_eq!(backoff(4), 4_800);
        assert_eq!(backoff(8), 76_800);
        assert_eq!(backoff(9), BACKOFF_MAX);
        assert_eq!(backoff(u64::MAX), BACKOFF_MAX);
    }

    #[test]
    fn reservations_fill_the_run_exactly() {
        let total: usize = Class::ALL.iter().map(|c| c.reserved()).sum();
        assert_eq!(total, HANDSHAKES_PER_RUN);
    }

    fn classes(picked: &[(Class, u32)]) -> Vec<Class> {
        picked.iter().map(|(c, _)| *c).collect()
    }

    #[test]
    fn a_flood_in_one_class_keeps_the_others_their_share() {
        let flood: Vec<u32> = (0..100).collect();
        let picked = pick(
            [flood, vec![200, 201, 202, 203], vec![300], vec![400, 401]],
            16,
        );
        assert_eq!(picked.len(), 16);
        let count = |c| classes(&picked).iter().filter(|&&x| x == c).count();
        assert_eq!(count(Class::Recheck), 3, "reserved share only, first pass");
        assert_eq!(count(Class::Returning), 1);
        assert_eq!(count(Class::Candidate), 1);
        // The unused Returning share flows down in class order.
        assert_eq!(count(Class::New), 11);
        assert_eq!(&picked[..2], &[(Class::New, 0), (Class::New, 1)]);
    }

    #[test]
    fn unused_shares_flow_to_the_next_classes() {
        let picked = pick(
            [vec![], vec![1, 2, 3, 4, 5], vec![], (10..30).collect()],
            16,
        );
        assert_eq!(picked.len(), 16);
        assert_eq!(
            classes(&picked)
                .iter()
                .filter(|&&c| c == Class::Recheck)
                .count(),
            5
        );
        assert_eq!(pick::<u32>([vec![], vec![], vec![], vec![]], 16), []);
        assert_eq!(pick([vec![1], vec![2], vec![3], vec![4]], 2).len(), 2);
    }

    fn row<'a>(status: &'a str, source: &'a str, failures: u64) -> Checked<'a> {
        Checked {
            status,
            source,
            failures,
        }
    }

    #[test]
    fn a_successful_check_lists_any_row_and_clears_its_failures() {
        for (status, source) in [
            ("unverified", "update"),
            ("unverified", "try_hubs"),
            ("verified", "update"),
            ("alive_full", "seed"),
            ("dead", "update"),
        ] {
            assert_eq!(
                after_check(&row(status, source, 5), Verdict::Verified),
                HubAction::Set {
                    status: "verified",
                    failures: 0,
                    seen: true
                },
                "{status}/{source}"
            );
            assert_eq!(
                after_check(&row(status, source, 5), Verdict::AliveFull),
                HubAction::Set {
                    status: "alive_full",
                    failures: 0,
                    seen: true
                }
            );
        }
    }

    #[test]
    fn a_failed_candidate_is_deleted_and_a_failed_registration_is_dead() {
        assert_eq!(
            after_check(&row("unverified", "try_hubs", 0), Verdict::Dead),
            HubAction::Delete
        );
        for source in ["update", "seed"] {
            assert_eq!(
                after_check(&row("unverified", source, 0), Verdict::Dead),
                HubAction::Set {
                    status: "dead",
                    failures: 1,
                    seen: false
                }
            );
        }
        assert_eq!(
            after_check(&row("dead", "try_hubs", 3), Verdict::Dead),
            HubAction::Set {
                status: "dead",
                failures: 4,
                seen: false
            },
            "a dead row stays, whatever its source"
        );
    }

    #[test]
    fn a_listed_hub_survives_one_failure_but_not_two() {
        for status in ["verified", "alive_full"] {
            assert_eq!(
                after_check(&row(status, "update", 0), Verdict::Dead),
                HubAction::Set {
                    status,
                    failures: 1,
                    seen: false
                }
            );
            assert_eq!(
                after_check(&row(status, "update", 1), Verdict::Dead),
                HubAction::Set {
                    status: "dead",
                    failures: 2,
                    seen: false
                }
            );
        }
    }

    fn a(s: &str) -> SocketAddrV4 {
        s.parse().unwrap()
    }

    #[test]
    fn candidates_are_fresh_unique_newest_first_and_bounded() {
        let got = candidates(
            [
                (a("8.8.4.4:6346"), Some(NOW - 600)),
                (a("8.8.4.4:6347"), Some(NOW - 60)), // same IP, newer
                (a("9.9.9.9:6346"), Some(NOW - CANDIDATE_MAX_AGE - 1)), // stale
                (a("1.1.1.1:6346"), None),           // no time: now
                (a("1.0.0.1:6346"), Some(NOW + 3_600)), // far future: now
                (a("4.2.2.2:6346"), Some(NOW - 3_600)),
            ],
            NOW,
            10,
        );
        assert_eq!(
            got,
            [
                (a("1.0.0.1:6346"), NOW),
                (a("1.1.1.1:6346"), NOW),
                (a("8.8.4.4:6347"), NOW - 60),
                (a("4.2.2.2:6346"), NOW - 3_600),
            ]
        );
        assert_eq!(candidates([(a("8.8.4.4:6346"), None)], NOW, 0), []);
        assert_eq!(
            candidates([(a("8.8.4.4:6346"), Some(NOW + CANDIDATE_SKEW))], NOW, 1),
            [(a("8.8.4.4:6346"), NOW + CANDIDATE_SKEW)]
        );
    }

    #[test]
    fn a_cache_is_deleted_at_its_third_failed_ping_in_a_row() {
        assert_eq!(after_ping(0, None), CacheAction::Failed { failures: 1 });
        assert_eq!(after_ping(1, None), CacheAction::Failed { failures: 2 });
        assert_eq!(after_ping(2, None), CacheAction::Delete);
        assert_eq!(
            after_ping(2, Some("dkac 1.0".into())),
            CacheAction::Verified {
                pong: "dkac 1.0".into()
            }
        );
    }

    #[test]
    fn the_ledger_keeps_its_tail() {
        let mut l = Ledger::new(5);
        l.spend(10);
        let mut taken = 0;
        while l.try_take() {
            taken += 1;
        }
        assert_eq!(taken, SUBREQUEST_CEILING - 15);
        assert_eq!(l.used(), SUBREQUEST_CEILING - 5);
        l.spend(5);
        assert_eq!(l.used(), SUBREQUEST_CEILING, "the tail is still spent");
    }

    #[test]
    fn class_labels_are_stable() {
        let labels: Vec<_> = Class::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(labels, ["new", "recheck", "returning", "candidate"]);
    }

    fn leak_line(r: &LeakReport) -> serde_json::Value {
        serde_json::from_str(&r.to_json()).unwrap()
    }

    #[test]
    fn a_leak_report_sorts_open_times_and_counts_both_outcomes() {
        let report = LeakReport {
            open_ms: vec![21_400, 1_250, 9_800],
            close_errors: 1,
            unsettled: 2,
            waited_ms: 30_000,
        };
        assert_eq!(
            leak_line(&report),
            serde_json::json!({
                "kind": "cron_leaks",
                "leaked": 5,
                "settled": 3,
                "open_ms": [1_250, 9_800, 21_400],
                "close_errors": 1,
                "unsettled": 2,
                "waited_ms": 30_000,
            })
        );
    }

    #[test]
    fn a_leak_report_with_nothing_settled_still_names_what_it_waited_for() {
        let report = LeakReport {
            open_ms: vec![],
            close_errors: 0,
            unsettled: 1,
            waited_ms: LEAK_WAIT_MS,
        };
        let line = leak_line(&report);
        assert_eq!(line["leaked"], 1);
        assert_eq!(line["settled"], 0);
        assert_eq!(line["open_ms"], serde_json::json!([]));
        assert_eq!(line["waited_ms"], LEAK_WAIT_MS);
    }

    #[test]
    fn slots_shrink_with_each_leaked_socket_still_open() {
        assert_eq!(slots(HANDSHAKE_CONCURRENCY, 0), 5);
        assert_eq!(slots(HANDSHAKE_CONCURRENCY, 2), 3);
        assert_eq!(slots(HANDSHAKE_CONCURRENCY, 5), 0);
        assert_eq!(slots(HANDSHAKE_CONCURRENCY, 9), 0);
        // A cache ping may use the sixth connection.
        assert_eq!(slots(HANDSHAKE_CONCURRENCY + 1, 5), 1);
    }

    #[test]
    fn the_slot_wait_outlasts_the_connect_timeout_measured_in_production() {
        // On 2026-10-25 every leaked socket's connection stayed open about
        // 19.5 s from its connect call (ADR-0020). A leak is known only
        // after the exchange's deadline and the close timeout, so waiting
        // SLOT_WAIT_MS from then covers the rest, with a margin.
        const MEASURED_CONNECT_TIMEOUT_MS: u64 = 19_500;
        let leak_known_at = HANDSHAKE_TIMEOUT_MS.min(PING_TIMEOUT_MS) + CLOSE_TIMEOUT_MS;
        assert!(leak_known_at + SLOT_WAIT_MS >= MEASURED_CONNECT_TIMEOUT_MS + 2_000);
        // A wait never costs more than a run can spare: 15 minutes of wall
        // time, and no CPU while waiting.
        const { assert!(SLOT_WAIT_MS <= 30_000) };
    }

    #[test]
    fn the_leak_wait_fits_well_inside_the_cron_wall_time() {
        // 15 minutes of wall time per cron invocation; the wait costs no CPU.
        const { assert!(LEAK_WAIT_MS <= 60_000) };
    }
}
