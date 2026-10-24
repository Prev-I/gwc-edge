//! The hostfile snapshot: the servable hubs and caches, rendered by cron into
//! one D1 row so that every get costs one row read whatever the load.
//!
//! The snapshot holds timestamps, not ages: ages, freshness and uptime are
//! computed when a request is served, so a snapshot up to `MAX_SNAPSHOT_AGE`
//! old still yields exact ages.
//!
//! Format, one record per line, `|`-separated, empty field = unknown:
//!
//! ```text
//! gwc-edge-snapshot 1 <rendered_at>
//! h|<ip:port>|<last_seen>|<leaves>|<vendor>|<up_since>|<max_leaves>
//! u|<url>|<last_verified>
//! ```

use std::fmt::Write as _;
use std::net::SocketAddrV4;

use crate::response::{HostEntry, UrlEntry};

/// `snapshots.k` values: the documents the cron renders.
pub mod keys {
    /// [`super::Snapshot`], every cron run.
    pub const HOSTFILE: &str = "hostfile";
    /// `crate::stats::Hourly` as JSON, hourly.
    pub const STATS: &str = "stats";
    /// `crate::stats::CronSummary` as JSON, every cron run.
    pub const CRON: &str = "cron";
    /// Every key, in the order the CHECK constraint lists them.
    pub const ALL: [&str; 3] = [HOSTFILE, STATS, CRON];
}

/// A hub drops out of the hostfile this long after it was last seen alive
/// (successful update or verification).
pub const LIST_WINDOW: u64 = 2 * 60 * 60;

/// A snapshot older than this is not trusted: the cron that renders it has
/// stalled, and the hostfile is built from a live read instead.
pub const MAX_SNAPSHOT_AGE: u64 = 30 * 60;

/// Tolerated clock skew: a `last_seen` this far in the future counts as now.
const SKEW: u64 = 5 * 60;

const HEADER: &str = "gwc-edge-snapshot 1";

/// A hub as stored in the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapHub {
    pub addr: SocketAddrV4,
    pub last_seen: u64,
    pub leaves: Option<u32>,
    pub vendor: Option<String>,
    /// When the hub came up (`last update time - reported uptime`).
    pub up_since: Option<u64>,
    pub max_leaves: Option<u32>,
}

/// A cache as stored in the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapCache {
    pub url: String,
    pub last_verified: u64,
}

/// Servable hubs and caches at `rendered_at`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub rendered_at: u64,
    pub hubs: Vec<SnapHub>,
    pub caches: Vec<SnapCache>,
}

impl Snapshot {
    /// Serialise for the `snapshots.body` column.
    #[must_use]
    pub fn render(&self) -> String {
        let opt = |v: Option<String>| v.unwrap_or_default();
        let mut out = format!("{HEADER} {}\n", self.rendered_at);
        for h in &self.hubs {
            let _ = writeln!(
                out,
                "h|{}|{}|{}|{}|{}|{}",
                h.addr,
                h.last_seen,
                opt(h.leaves.map(|n| n.to_string())),
                // Only a clean code survives the round trip; anything else
                // (the schema bounds only its length) is dropped, not the hub.
                opt(h
                    .vendor
                    .clone()
                    .filter(|v| v.len() == 4 && v.bytes().all(|b| b.is_ascii_alphanumeric()))),
                opt(h.up_since.map(|n| n.to_string())),
                opt(h.max_leaves.map(|n| n.to_string())),
            );
        }
        for c in &self.caches {
            let _ = writeln!(out, "u|{}|{}", c.url, c.last_verified);
        }
        out
    }

    /// Parse a stored body. `None` if it is not a snapshot this version
    /// wrote; malformed records are skipped rather than failing the whole.
    #[must_use]
    pub fn parse(body: &str) -> Option<Snapshot> {
        let mut lines = body.lines();
        let rendered_at = lines
            .next()?
            .strip_prefix(HEADER)?
            .strip_prefix(' ')?
            .parse()
            .ok()?;
        let mut snap = Snapshot {
            rendered_at,
            ..Snapshot::default()
        };
        for line in lines {
            let f: Vec<&str> = line.split('|').collect();
            match f.as_slice() {
                ["h", addr, last_seen, leaves, vendor, up_since, max_leaves] => {
                    let (Ok(addr), Ok(last_seen)) =
                        (crate::addr::parse_hub(addr), last_seen.parse())
                    else {
                        continue;
                    };
                    snap.hubs.push(SnapHub {
                        addr,
                        last_seen,
                        leaves: leaves.parse().ok(),
                        vendor: Some((*vendor).to_owned()).filter(|v| !v.is_empty()),
                        up_since: up_since.parse().ok(),
                        max_leaves: max_leaves.parse().ok(),
                    });
                }
                ["u", url, last_verified] => {
                    if let Ok(last_verified) = last_verified.parse() {
                        snap.caches.push(SnapCache {
                            url: (*url).to_owned(),
                            last_verified,
                        });
                    }
                }
                _ => {}
            }
        }
        Some(snap)
    }

    /// Whether the snapshot is too old to serve from.
    #[must_use]
    pub fn is_stale(&self, now: u64) -> bool {
        now.saturating_sub(self.rendered_at) > MAX_SNAPSHOT_AGE
    }

    /// Hubs to serve at `now`: seen within [`LIST_WINDOW`], newest first, at
    /// most `n`.
    #[must_use]
    pub fn hosts(&self, now: u64, n: usize) -> Vec<HostEntry> {
        let mut live: Vec<&SnapHub> = self.hubs.iter().filter(|h| is_live(h, now)).collect();
        live.sort_by(|a, b| b.last_seen.cmp(&a.last_seen).then(a.addr.cmp(&b.addr)));
        live.into_iter()
            .take(n)
            .map(|h| HostEntry {
                addr: h.addr,
                age: now.saturating_sub(h.last_seen),
                leaves: h.leaves,
                vendor: h.vendor.clone(),
                uptime: h
                    .up_since
                    .filter(|&t| t <= now)
                    .and_then(|t| u32::try_from(now - t).ok()),
                max_leaves: h.max_leaves,
            })
            .collect()
    }

    /// Caches to serve at `now`, most recently verified first, at most `n`.
    #[must_use]
    pub fn caches(&self, now: u64, n: usize) -> Vec<UrlEntry> {
        let mut caches: Vec<&SnapCache> = self.caches.iter().collect();
        caches.sort_by(|a, b| {
            b.last_verified
                .cmp(&a.last_verified)
                .then(a.url.cmp(&b.url))
        });
        caches
            .into_iter()
            .take(n)
            .map(|c| UrlEntry {
                url: c.url.clone(),
                age: now.saturating_sub(c.last_verified),
            })
            .collect()
    }

    /// Number of hubs that would be served at `now`, for `/stats`.
    #[must_use]
    pub fn live_hubs(&self, now: u64) -> usize {
        self.hubs.iter().filter(|h| is_live(h, now)).count()
    }
}

/// Seen within the window, and not implausibly far in the future.
fn is_live(h: &SnapHub, now: u64) -> bool {
    h.last_seen <= now + SKEW && now.saturating_sub(h.last_seen) < LIST_WINDOW
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    const NOW: u64 = 1_792_744_200;

    fn hub(last_byte: u8, last_seen: u64) -> SnapHub {
        SnapHub {
            addr: SocketAddrV4::new(Ipv4Addr::new(8, 8, 4, last_byte), 6346),
            last_seen,
            leaves: None,
            vendor: None,
            up_since: None,
            max_leaves: None,
        }
    }

    fn sample() -> Snapshot {
        Snapshot {
            rendered_at: NOW - 60,
            hubs: vec![
                SnapHub {
                    leaves: Some(120),
                    vendor: Some("RAZA".into()),
                    up_since: Some(NOW - 7300),
                    max_leaves: Some(300),
                    ..hub(1, NOW - 100)
                },
                hub(2, NOW - 50),
            ],
            caches: vec![SnapCache {
                url: "http://dkac.trillinux.org/dkac/dkac.php".into(),
                last_verified: NOW - 600,
            }],
        }
    }

    #[test]
    fn migration_check_lists_exactly_the_snapshot_keys() {
        let sql = include_str!("../../../migrations/0001_initial.sql");
        let list: Vec<String> = keys::ALL.iter().map(|k| format!("'{k}'")).collect();
        assert!(
            sql.contains(&format!("CHECK (k IN ({}))", list.join(", "))),
            "snapshots.k CHECK out of sync with snapshot::keys::ALL"
        );
    }

    #[test]
    fn render_parse_round_trip() {
        let s = sample();
        assert_eq!(Snapshot::parse(&s.render()), Some(s));
        let empty = Snapshot {
            rendered_at: NOW,
            ..Snapshot::default()
        };
        assert_eq!(Snapshot::parse(&empty.render()), Some(empty));
    }

    #[test]
    fn rendered_format_is_stable() {
        assert_eq!(
            sample().render(),
            format!(
                "gwc-edge-snapshot 1 {}\n\
                 h|8.8.4.1:6346|{}|120|RAZA|{}|300\n\
                 h|8.8.4.2:6346|{}||||\n\
                 u|http://dkac.trillinux.org/dkac/dkac.php|{}\n",
                NOW - 60,
                NOW - 100,
                NOW - 7300,
                NOW - 50,
                NOW - 600
            )
        );
    }

    #[test]
    fn a_bad_vendor_is_dropped_not_the_hub() {
        // The schema only bounds vendor length; a '|' or space must not take
        // the whole hub out of the snapshot.
        let s = Snapshot {
            rendered_at: NOW,
            hubs: vec![SnapHub {
                vendor: Some("R|ZA".into()),
                ..hub(1, NOW)
            }],
            caches: vec![],
        };
        let parsed = Snapshot::parse(&s.render()).unwrap();
        assert_eq!(parsed.hubs, [hub(1, NOW)]);
    }

    #[test]
    fn foreign_or_garbled_bodies() {
        assert_eq!(Snapshot::parse(""), None);
        assert_eq!(
            Snapshot::parse("gwc-edge-snapshot 2 5\n"),
            None,
            "future version"
        );
        assert_eq!(Snapshot::parse("gwc-edge-snapshot 1 x\n"), None);
        let s = Snapshot::parse(&format!(
            "gwc-edge-snapshot 1 {NOW}\nh|10.0.0.1:6346|{NOW}||||\nh|junk\nx|y\nu|http://a.example.org/\nh|8.8.4.4:6346|{NOW}||||\n"
        ))
        .unwrap();
        assert_eq!(
            s.hubs,
            [hub(4, NOW)],
            "private, short and unknown records skipped"
        );
        assert_eq!(s.caches, []);
    }

    #[test]
    fn staleness() {
        let mut s = sample();
        s.rendered_at = NOW - MAX_SNAPSHOT_AGE;
        assert!(!s.is_stale(NOW));
        s.rendered_at = NOW - MAX_SNAPSHOT_AGE - 1;
        assert!(s.is_stale(NOW));
        s.rendered_at = NOW + 3600;
        assert!(!s.is_stale(NOW), "a future snapshot is not stale");
    }

    #[test]
    fn hosts_are_fresh_newest_first_and_capped() {
        let s = Snapshot {
            rendered_at: NOW,
            hubs: vec![
                hub(1, NOW - 300),
                hub(2, NOW - LIST_WINDOW), // just expired
                hub(3, NOW - 10),
                hub(4, NOW - LIST_WINDOW + 1), // just fresh
                hub(5, NOW + 60),              // clock skew: counts as now
            ],
            caches: vec![],
        };
        let ages: Vec<(u8, u64)> = s
            .hosts(NOW, 10)
            .iter()
            .map(|h| (h.addr.ip().octets()[3], h.age))
            .collect();
        assert_eq!(ages, [(5, 0), (3, 10), (1, 300), (4, LIST_WINDOW - 1)]);
        assert_eq!(s.hosts(NOW, 2).len(), 2);
        assert_eq!(s.live_hubs(NOW), 4);
    }

    #[test]
    fn far_future_hubs_are_not_served() {
        let s = Snapshot {
            rendered_at: NOW,
            hubs: vec![hub(1, NOW + SKEW + 1)],
            caches: vec![],
        };
        assert_eq!(s.hosts(NOW, 10), []);
    }

    #[test]
    fn host_fields_and_uptime_at_serve_time() {
        let h = &sample().hosts(NOW, 10)[1];
        assert_eq!(h.addr.ip().octets()[3], 1);
        assert_eq!(h.age, 100);
        assert_eq!(h.leaves, Some(120));
        assert_eq!(h.vendor.as_deref(), Some("RAZA"));
        assert_eq!(h.uptime, Some(7300), "uptime grows with the clock");
        assert_eq!(h.max_leaves, Some(300));
        let later = &sample().hosts(NOW + 60, 10)[1];
        assert_eq!(later.uptime, Some(7360));
    }

    #[test]
    fn caches_newest_first_and_capped() {
        let s = Snapshot {
            rendered_at: NOW,
            hubs: vec![],
            caches: vec![
                SnapCache {
                    url: "http://a.example.org/".into(),
                    last_verified: NOW - 500,
                },
                SnapCache {
                    url: "http://b.example.org/".into(),
                    last_verified: NOW - 5,
                },
            ],
        };
        let got: Vec<(String, u64)> = s
            .caches(NOW, 10)
            .into_iter()
            .map(|c| (c.url, c.age))
            .collect();
        assert_eq!(
            got,
            [
                ("http://b.example.org/".into(), 5),
                ("http://a.example.org/".into(), 500)
            ]
        );
        assert_eq!(s.caches(NOW, 1).len(), 1);
    }
}
