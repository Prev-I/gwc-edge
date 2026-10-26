//! The `/stats` document (schema 1), and the two cron-rendered parts it is
//! composed from:
//!
//! - [`Hourly`], stored as `snapshots.stats`: request counts and health over
//!   the last 24 h, grouped in SQL by the cron (`GROUP BY` over the window).
//!   The window holds as many rows as writes allow, so the grouping stays in
//!   SQLite: only a few dozen rows reach the Worker.
//! - [`CronSummary`], stored as `snapshots.cron` on every run: hub and cache
//!   counts by status, and what the last run did.
//!
//! `/stats` and the landing page read both rows in one batch.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::counters::{Kind, SAMPLE_K, hour};

/// Version of the `/stats` JSON shape.
pub const SCHEMA: u32 = 1;

/// Hours of history covered, current hour included.
pub const WINDOW_HOURS: u64 = 24;

/// Most client/version pairs listed; the rest are folded into `other_clients`.
pub const MAX_CLIENTS: usize = 50;

/// The `[first, last]` hour buckets of the window ending at `now`.
#[must_use]
pub fn window(now: u64) -> (u64, u64) {
    let last = hour(now);
    (last.saturating_sub(WINDOW_HOURS - 1), last)
}

/// Grouped sums over the window, as the SQL queries return them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Totals {
    /// `SUM(n) GROUP BY kind, scheme`: at most 20 rows, folded here.
    pub by_kind_scheme: Vec<(String, String, u64)>,
    /// `SUM(n) GROUP BY client, version ORDER BY 3 DESC, 1, 2 LIMIT MAX_CLIENTS`,
    /// over request kinds only.
    pub top_clients: Vec<ClientCount>,
}

/// Request counts over the window.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Requests {
    pub total: u64,
    pub by_kind: BTreeMap<String, u64>,
    pub by_scheme: BTreeMap<String, u64>,
    /// Most frequent first, at most [`MAX_CLIENTS`].
    pub by_client: Vec<ClientCount>,
    /// Requests from client/version pairs beyond [`MAX_CLIENTS`].
    pub other_clients: u64,
}

/// Requests from one client/version pair.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientCount {
    pub client: String,
    pub version: String,
    pub n: u64,
}

/// Health events over the window (exact counts, see `crate::health`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Health {
    pub d1_errors_24h: u64,
    pub degraded_responses_24h: u64,
}

/// The hourly part, `snapshots.stats`. Every stored part parses with
/// missing fields defaulted, so a document an older build wrote survives a
/// deploy that adds one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hourly {
    pub generated_at: u64,
    pub window_hours: u64,
    /// Request counts are estimates: one request in `sample_rate` is recorded.
    pub sample_rate: u32,
    pub requests: Requests,
    pub health: Health,
}

/// Hub counts. Statuses count only hubs seen within the 2 h listing window,
/// so `verified` and `alive_full` are what clients can be given now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HubCounts {
    /// What a hostfile lists now: `verified` plus `alive_full`.
    pub served: u64,
    pub verified: u64,
    pub alive_full: u64,
    pub unverified: u64,
    pub dead: u64,
    /// Every row in `hubs`, expired ones included.
    pub total: u64,
}

/// Cache counts by status.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheCounts {
    pub verified: u64,
    pub pending: u64,
    pub failed: u64,
}

/// What the last cron run did (ADR-0012).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CronRun {
    pub last_run_at: u64,
    pub last_run_ms: u64,
    /// Hubs picked for a handshake.
    pub picked: u64,
    /// Handshake verdicts.
    pub verified: u64,
    pub alive_full: u64,
    pub dead: u64,
    /// Hubs picked but not dialled: the subrequest budget, or the connection
    /// slots, ran out.
    pub skipped_budget: u64,
    /// Sockets whose close timed out: each holds a connection slot until its
    /// close succeeds, so the run dials fewer at a time meanwhile.
    pub leaked_sockets: u64,
    /// Time spent waiting for a leaked socket's slot to come back (ADR-0020).
    pub slot_wait_ms: u64,
    /// The verification's D1 work failed (logged as `d1_error`); its results
    /// were not stored, though its `cron_hub` lines were logged.
    pub verification_failed: bool,
    /// Failed `X-Try-Hubs` candidates deleted.
    pub deleted: u64,
    /// New `X-Try-Hubs` candidates stored.
    pub candidates_added: u64,
    /// `dead` rows deleted: silent for 48 h, or beyond the dead-row cap.
    pub purged: u64,
    pub evicted: u64,
    /// Cache pings, and how they ended.
    pub caches_pinged: u64,
    pub caches_verified: u64,
    pub caches_failed: u64,
    /// Subrequests counted against the run's budget.
    pub subrequests: u64,
    /// `TEST_DIAL` was set: an integration-test build, never production.
    pub test_dial: bool,
}

/// The per-run part, `snapshots.cron`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CronSummary {
    pub hubs: HubCounts,
    pub caches: CacheCounts,
    pub run: CronRun,
}

/// The `/stats` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stats {
    pub schema: u32,
    pub product: String,
    pub git_sha: String,
    pub canonical_url: String,
    /// When the hourly part was rendered.
    pub generated_at: u64,
    pub window_hours: u64,
    pub sample_rate: u32,
    pub hubs: HubCounts,
    pub caches: CacheCounts,
    pub cron: CronRun,
    pub requests: Requests,
    pub health: Health,
}

/// Build the hourly part from the grouped totals. Request kinds and both
/// schemes are always listed, at 0 if absent; unknown values are ignored.
/// Health kinds are not requests: they go to [`Health`] only.
#[must_use]
pub fn hourly(totals: Totals, now: u64) -> Hourly {
    let mut by_kind: BTreeMap<String, u64> = Kind::REQUESTS
        .iter()
        .map(|k| (k.as_str().to_owned(), 0))
        .collect();
    let mut by_scheme: BTreeMap<String, u64> = ["http", "https"]
        .iter()
        .map(|s| ((*s).to_owned(), 0))
        .collect();
    let mut health = Health::default();
    for (kind, scheme, n) in totals.by_kind_scheme {
        match Kind::parse(&kind) {
            Some(Kind::D1Error) => health.d1_errors_24h += n,
            Some(Kind::Degraded) => health.degraded_responses_24h += n,
            // The CHECK constraints make unknown values impossible; skip them
            // anyway, so a total only ever counts what it lists.
            _ => {
                if let (Some(k), Some(s)) = (by_kind.get_mut(&kind), by_scheme.get_mut(&scheme)) {
                    *k += n;
                    *s += n;
                }
            }
        }
    }
    let total: u64 = by_kind.values().sum();
    let mut by_client = totals.top_clients;
    by_client.truncate(MAX_CLIENTS);
    let listed: u64 = by_client.iter().map(|c| c.n).sum();
    Hourly {
        generated_at: now,
        window_hours: WINDOW_HOURS,
        sample_rate: SAMPLE_K,
        requests: Requests {
            total,
            by_kind,
            by_scheme,
            by_client,
            other_clients: total.saturating_sub(listed),
        },
        health,
    }
}

/// Hub counts from `SELECT status, COUNT(*) ... WHERE last_seen within the
/// window GROUP BY status`, the number served now, and the total row count.
#[must_use]
pub fn hub_counts(by_status: &[(String, u64)], served: u64, total: u64) -> HubCounts {
    let n = |s: &str| {
        by_status
            .iter()
            .filter(|(k, _)| k == s)
            .map(|(_, n)| n)
            .sum()
    };
    HubCounts {
        served,
        verified: n("verified"),
        alive_full: n("alive_full"),
        unverified: n("unverified"),
        dead: n("dead"),
        total,
    }
}

/// Cache counts from `SELECT status, COUNT(*) ... GROUP BY status`.
#[must_use]
pub fn cache_counts(by_status: &[(String, u64)]) -> CacheCounts {
    let n = |s: &str| {
        by_status
            .iter()
            .filter(|(k, _)| k == s)
            .map(|(_, n)| n)
            .sum()
    };
    CacheCounts {
        verified: n("verified"),
        pending: n("pending"),
        failed: n("failed"),
    }
}

/// The `/stats` document from its two parts.
#[must_use]
pub fn compose(hourly: Hourly, cron: CronSummary, git_sha: &str, canonical_url: &str) -> Stats {
    Stats {
        schema: SCHEMA,
        product: crate::product_id(),
        git_sha: git_sha.to_owned(),
        canonical_url: canonical_url.to_owned(),
        generated_at: hourly.generated_at,
        window_hours: hourly.window_hours,
        sample_rate: hourly.sample_rate,
        hubs: cron.hubs,
        caches: cron.caches,
        cron: cron.run,
        requests: hourly.requests,
        health: hourly.health,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_792_744_200;

    fn triples(p: &[(&str, &str, u64)]) -> Vec<(String, String, u64)> {
        p.iter()
            .map(|(k, s, n)| ((*k).to_owned(), (*s).to_owned(), *n))
            .collect()
    }

    fn pairs(p: &[(&str, u64)]) -> Vec<(String, u64)> {
        p.iter().map(|(k, n)| ((*k).to_owned(), *n)).collect()
    }

    fn client(c: &str, v: &str, n: u64) -> ClientCount {
        ClientCount {
            client: c.into(),
            version: v.into(),
            n,
        }
    }

    #[test]
    fn window_is_24_hour_buckets_ending_now() {
        let (first, last) = window(NOW);
        assert_eq!(last, hour(NOW));
        assert_eq!(last - first + 1, WINDOW_HOURS);
        assert_eq!(window(3600), (0, 1), "no underflow near the epoch");
    }

    #[test]
    fn empty_log() {
        let h = hourly(Totals::default(), NOW);
        assert_eq!(h.requests.total, 0);
        assert_eq!(h.sample_rate, SAMPLE_K);
        assert_eq!(h.window_hours, 24);
        assert_eq!(h.generated_at, NOW);
        assert_eq!(
            h.requests.by_kind.len(),
            Kind::REQUESTS.len(),
            "every request kind, even at 0"
        );
        assert!(!h.requests.by_kind.contains_key("d1_error"));
        assert_eq!(h.requests.by_scheme.len(), 2);
        assert_eq!(h.health, Health::default());
    }

    #[test]
    fn totals_are_folded() {
        let t = Totals {
            by_kind_scheme: triples(&[
                ("hostfile", "http", 16),
                ("urlfile", "http", 8),
                ("ping", "https", 8),
                ("bogus", "http", 99),
                ("ping", "gopher", 5),
            ]),
            top_clients: vec![client("RAZA", "2.7.10.6", 24), client("TEST", "0.1", 6)],
        };
        let r = hourly(t, NOW).requests;
        assert_eq!(r.total, 32, "unknown kinds and schemes are not counted");
        assert_eq!(r.by_kind["hostfile"], 16);
        assert_eq!(r.by_kind["ping"], 8);
        assert_eq!(r.by_kind["update"], 0);
        assert!(!r.by_kind.contains_key("bogus"));
        assert_eq!(r.by_scheme["https"], 8);
        assert!(!r.by_scheme.contains_key("gopher"));
        assert_eq!(r.by_client[0], client("RAZA", "2.7.10.6", 24));
        assert_eq!(r.other_clients, 2, "32 total minus 30 explained");
    }

    #[test]
    fn health_kinds_go_to_health_not_requests() {
        let t = Totals {
            by_kind_scheme: triples(&[
                ("ping", "http", 8),
                ("d1_error", "http", 3),
                ("degraded", "http", 5),
            ]),
            top_clients: vec![],
        };
        let h = hourly(t, NOW);
        assert_eq!(h.requests.total, 8);
        assert_eq!(
            h.requests.by_scheme["http"], 8,
            "health events are not requests"
        );
        assert_eq!(
            h.health,
            Health {
                d1_errors_24h: 3,
                degraded_responses_24h: 5
            }
        );
    }

    #[test]
    fn client_list_is_capped_and_never_overflows_the_total() {
        let t = Totals {
            by_kind_scheme: triples(&[("ping", "http", 10)]),
            top_clients: (0..60)
                .map(|i| client("RAZA", &format!("v{i}"), 1))
                .collect(),
        };
        let r = hourly(t, NOW).requests;
        assert_eq!(r.by_client.len(), MAX_CLIENTS);
        assert_eq!(r.other_clients, 0, "saturates rather than underflowing");
    }

    #[test]
    fn hub_and_cache_counts_by_status() {
        let h = hub_counts(
            &pairs(&[
                ("verified", 31),
                ("alive_full", 4),
                ("unverified", 3),
                ("dead", 12),
                ("x", 9),
            ]),
            35,
            70,
        );
        assert_eq!(
            h,
            HubCounts {
                served: 35,
                verified: 31,
                alive_full: 4,
                unverified: 3,
                dead: 12,
                total: 70,
            }
        );
        let c = cache_counts(&pairs(&[("verified", 2), ("pending", 1)]));
        assert_eq!(
            c,
            CacheCounts {
                verified: 2,
                pending: 1,
                failed: 0
            }
        );
    }

    #[test]
    fn parts_round_trip_through_json() {
        let h = hourly(Totals::default(), NOW);
        let back: Hourly = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
        assert_eq!(back, h);
        let c = CronSummary {
            hubs: HubCounts {
                served: 2,
                ..HubCounts::default()
            },
            ..CronSummary::default()
        };
        let back: CronSummary = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn parts_missing_fields_still_parse() {
        // A document written by an older build must not take /stats down
        // after a deploy that adds a field.
        let h: Hourly = serde_json::from_str(r#"{"generated_at": 5}"#).unwrap();
        assert_eq!(h.generated_at, 5);
        assert_eq!(h.requests.total, 0);
        let c: CronSummary = serde_json::from_str(r#"{"hubs": {"served": 3}}"#).unwrap();
        assert_eq!(c.hubs.served, 3);
        assert_eq!(c.run, CronRun::default());
    }

    #[test]
    fn composed_document_has_the_documented_shape() {
        let t = Totals {
            by_kind_scheme: triples(&[("hostfile", "http", 8), ("degraded", "http", 1)]),
            top_clients: vec![client("RAZA", "2.7.10.6", 8)],
        };
        let cron = CronSummary {
            hubs: hub_counts(&pairs(&[("verified", 31), ("alive_full", 4)]), 35, 40),
            caches: cache_counts(&pairs(&[("verified", 2)])),
            run: CronRun {
                last_run_at: NOW - 60,
                last_run_ms: 412,
                ..CronRun::default()
            },
        };
        let s = compose(hourly(t, NOW), cron, "a1b2c3d", "http://gwc.example.org/");
        let json = serde_json::to_value(&s).unwrap();
        let keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut expected = [
            "schema",
            "product",
            "git_sha",
            "canonical_url",
            "generated_at",
            "window_hours",
            "sample_rate",
            "hubs",
            "caches",
            "cron",
            "requests",
            "health",
        ];
        expected.sort_unstable();
        assert_eq!(keys, expected, "serde_json maps are ordered by key");
        assert_eq!(json["schema"], 1);
        assert!(json["product"].as_str().unwrap().starts_with("gwc-edge "));
        assert_eq!(json["git_sha"], "a1b2c3d");
        assert_eq!(json["hubs"]["served"], 35);
        assert_eq!(json["caches"]["verified"], 2);
        assert_eq!(json["cron"]["last_run_ms"], 412);
        assert_eq!(json["requests"]["by_kind"]["hostfile"], 8);
        assert_eq!(json["health"]["degraded_responses_24h"], 1);
    }
}
