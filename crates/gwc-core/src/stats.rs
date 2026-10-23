//! The `/stats` document, built from sampled `request_log` totals.
//!
//! The grouping itself runs in SQL (`GROUP BY` over the window): the number
//! of rows in the window is bounded only by the writes, and pulling each one
//! across the JS boundary would not fit the 10 ms CPU budget. What reaches
//! Rust is at most a few dozen grouped rows, assembled here.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::counters::{Kind, SAMPLE_K, hour};

/// Hours of history `/stats` covers, current hour included.
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
    /// `SUM(n) GROUP BY kind, scheme`: at most 16 rows, folded here.
    pub by_kind_scheme: Vec<(String, String, u64)>,
    /// `SUM(n) GROUP BY client, version ORDER BY 3 DESC, 1, 2 LIMIT MAX_CLIENTS`.
    pub top_clients: Vec<ClientCount>,
}

/// The `/stats` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stats {
    pub product: String,
    pub generated_at: u64,
    pub window_hours: u64,
    /// Counts are estimates: one request in `sample_rate` is recorded.
    pub sample_rate: u32,
    pub live_hubs: usize,
    pub live_caches: usize,
    pub requests: Requests,
}

/// Request counts over the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientCount {
    pub client: String,
    pub version: String,
    pub n: u64,
}

/// Assemble the document. Every known kind and both schemes are listed, at 0
/// if absent; unknown kinds and schemes are ignored. The total counts known
/// kinds only, so `other_clients` is what the client list does not explain.
#[must_use]
pub fn build(totals: Totals, now: u64, live_hubs: usize, live_caches: usize) -> Stats {
    let mut by_kind: BTreeMap<String, u64> = Kind::ALL
        .iter()
        .map(|k| (k.as_str().to_owned(), 0))
        .collect();
    let mut by_scheme: BTreeMap<String, u64> = ["http", "https"]
        .iter()
        .map(|s| ((*s).to_owned(), 0))
        .collect();
    for (kind, scheme, n) in totals.by_kind_scheme {
        // The CHECK constraints make unknown values impossible; skip them
        // anyway, so a total only ever counts what it lists.
        if let (Some(k), Some(s)) = (by_kind.get_mut(&kind), by_scheme.get_mut(&scheme)) {
            *k += n;
            *s += n;
        }
    }
    let total: u64 = by_kind.values().sum();
    let mut by_client = totals.top_clients;
    by_client.truncate(MAX_CLIENTS);
    let listed: u64 = by_client.iter().map(|c| c.n).sum();

    Stats {
        product: crate::product_id(),
        generated_at: now,
        window_hours: WINDOW_HOURS,
        sample_rate: SAMPLE_K,
        live_hubs,
        live_caches,
        requests: Requests {
            total,
            by_kind,
            by_scheme,
            by_client,
            other_clients: total.saturating_sub(listed),
        },
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
        let s = build(Totals::default(), NOW, 3, 1);
        assert_eq!(s.requests.total, 0);
        assert_eq!((s.live_hubs, s.live_caches), (3, 1));
        assert_eq!(s.sample_rate, SAMPLE_K);
        assert_eq!(s.window_hours, 24);
        assert_eq!(s.generated_at, NOW);
        assert!(s.product.starts_with("gwc-edge "));
        assert_eq!(
            s.requests.by_kind.len(),
            Kind::ALL.len(),
            "every kind listed, even at 0"
        );
        assert_eq!(s.requests.by_scheme.len(), 2);
        assert_eq!(s.requests.other_clients, 0);
    }

    #[test]
    fn totals_are_assembled() {
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
        let r = build(t, NOW, 0, 0).requests;
        assert_eq!(r.total, 32, "unknown kinds and schemes are not counted");
        assert_eq!(r.by_kind["ping"], 8);
        assert_eq!(r.by_kind["hostfile"], 16);
        assert_eq!(r.by_kind["update"], 0);
        assert!(!r.by_kind.contains_key("bogus"));
        assert_eq!(r.by_scheme["https"], 8);
        assert!(!r.by_scheme.contains_key("gopher"));
        assert_eq!(r.by_client[0], client("RAZA", "2.7.10.6", 24));
        assert_eq!(r.other_clients, 2, "32 total minus 30 explained");
    }

    #[test]
    fn client_list_is_capped_and_never_overflows_the_total() {
        let t = Totals {
            by_kind_scheme: triples(&[("ping", "http", 10)]),
            top_clients: (0..60)
                .map(|i| client("RAZA", &format!("v{i}"), 1))
                .collect(),
        };
        let r = build(t, NOW, 0, 0).requests;
        assert_eq!(r.by_client.len(), MAX_CLIENTS);
        assert_eq!(r.other_clients, 0, "saturates rather than underflowing");
    }

    #[test]
    fn serialises_to_the_documented_json_shape() {
        let t = Totals {
            by_kind_scheme: triples(&[("ping", "http", 8)]),
            top_clients: vec![client("TEST", "0.1", 8)],
        };
        let json = serde_json::to_value(build(t, NOW, 2, 1)).unwrap();
        assert_eq!(json["live_hubs"], 2);
        assert_eq!(json["requests"]["by_kind"]["ping"], 8);
        assert_eq!(json["requests"]["by_scheme"]["http"], 8);
        assert_eq!(json["requests"]["by_client"][0]["client"], "TEST");
    }
}
