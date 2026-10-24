//! What an update stores. Every update a legitimate client sends is answered
//! `OK` (ADR-0002); this module decides, part by part, what is written.
//!
//! - `ip=`: stored only from the hub's own address (`CF-Connecting-IP` equal
//!   to the claimed IP, ADR-0006), only for Gnutella2, only if public with a
//!   port of 1024 or more. It is stored `unverified` and served only after
//!   the cron's first successful handshake (ADR-0011): the address proves the
//!   IP, only a handshake proves a G2 hub listens on the port. The update
//!   path creates or demotes rows; only the cron promotes them.
//! - `url=`: a third cache, stored as `pending` until the cron has pinged it.
//! - Writes are bounded by caps read from the `counts` table, and any row
//!   refreshed less than [`COALESCE`] ago is not written, whatever its status
//!   or port.

use std::net::{Ipv4Addr, SocketAddrV4};

use crate::addr::{self, AddrError};
use crate::cache_url::{self, CacheUrl, UrlError};
use crate::query::{Net, Query};

/// A hub refreshing sooner than this after its last refresh writes nothing.
/// Shareaza updates a cache at most hourly, so every real refresh still lands.
pub const COALESCE: u64 = 30 * 60;

/// Most hub rows kept.
pub const MAX_HUBS: u64 = 500;

/// Most `unverified` hub rows (from `X-Try-Hubs`), within [`MAX_HUBS`].
pub const MAX_UNVERIFIED: u64 = 150;

/// Most cache rows kept.
pub const MAX_CACHES: u64 = 50;

/// Most cache URLs kept per host.
pub const MAX_PER_DOMAIN: usize = 3;

/// A hub as it registers itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubClaim {
    pub addr: SocketAddrV4,
    pub vendor: Option<String>,
    pub version: Option<String>,
    /// `x.leaves`, if within the schema's bounds.
    pub leaves: Option<u32>,
    /// `x.max`, if within the schema's bounds.
    pub max_leaves: Option<u32>,
    /// When the hub came up: now minus its reported `uptime`.
    pub up_since: Option<u64>,
}

/// Why an `ip=` is not stored. The reply is `OK` regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpDecline {
    NotGnutella2,
    Malformed,
    NotPublic,
    BadPort,
    /// The request did not come from the claimed address.
    SourceMismatch,
}

/// Why a `url=` is not stored. The reply is `OK` regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlDecline {
    NotGnutella2,
    Https,
    Invalid,
    /// This cache's own URL.
    IsSelf,
}

impl IpDecline {
    /// Log label; never carries the address.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            IpDecline::NotGnutella2 => "not_g2",
            IpDecline::Malformed => "malformed",
            IpDecline::NotPublic => "not_public",
            IpDecline::BadPort => "bad_port",
            IpDecline::SourceMismatch => "source_mismatch",
        }
    }
}

impl UrlDecline {
    /// Log label; never carries the URL.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            UrlDecline::NotGnutella2 => "not_g2",
            UrlDecline::Https => "https",
            UrlDecline::Invalid => "invalid",
            UrlDecline::IsSelf => "self",
        }
    }
}

/// The `ip=` part of an update, if any: what to store, or why not.
#[must_use]
pub fn ip_claim(q: &Query, source: &str, now: u64) -> Option<Result<HubClaim, IpDecline>> {
    let raw = q.ip.as_deref()?;
    Some(claim_hub(q, raw, source, now))
}

fn claim_hub(q: &Query, raw: &str, source: &str, now: u64) -> Result<HubClaim, IpDecline> {
    if q.net != Net::Gnutella2 {
        return Err(IpDecline::NotGnutella2);
    }
    let hub = addr::parse_hub(raw).map_err(|e| match e {
        AddrError::Malformed => IpDecline::Malformed,
        AddrError::NotPublic => IpDecline::NotPublic,
        AddrError::BadPort => IpDecline::BadPort,
    })?;
    let ip: Ipv4Addr = *hub.ip();
    if !addr::source_matches(source, ip) {
        return Err(IpDecline::SourceMismatch);
    }
    // The schema bounds leaf counts to 0..=65535; anything else is dropped.
    let bounded = |n: Option<u32>| n.filter(|&n| n <= 65_535);
    Ok(HubClaim {
        addr: hub,
        vendor: Some(q.client.vendor.clone()),
        version: Some(q.client.version.clone()).filter(|v| !v.is_empty()),
        leaves: bounded(q.leaves),
        max_leaves: bounded(q.max_leaves),
        up_since: q.uptime.and_then(|u| now.checked_sub(u64::from(u))),
    })
}

/// The `url=` part of an update, if any: what to store, or why not.
#[must_use]
pub fn url_claim(q: &Query, canonical: &CacheUrl) -> Option<Result<CacheUrl, UrlDecline>> {
    let raw = q.url.as_deref()?;
    if q.net != Net::Gnutella2 {
        return Some(Err(UrlDecline::NotGnutella2));
    }
    Some(match cache_url::normalize(raw) {
        // Every path on this cache's host and port reaches this cache: the
        // GWC protocol is answered on `/` and on the canonical path.
        Ok(u) if authority(&u.url).eq_ignore_ascii_case(authority(&canonical.url)) => {
            Err(UrlDecline::IsSelf)
        }
        Ok(u) => Ok(u),
        Err(UrlError::Https) => Err(UrlDecline::Https),
        Err(UrlError::Invalid) => Err(UrlDecline::Invalid),
    })
}

/// `host[:port]` of a normalised `http://` URL.
fn authority(url: &str) -> &str {
    let rest = url.strip_prefix("http://").unwrap_or(url);
    rest.split('/').next().unwrap_or(rest)
}

/// The stored row for the claimed IP, as far as planning needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubRow {
    pub port: u16,
    pub status: String,
    pub source: String,
    pub last_seen: u64,
}

/// What to do with a hub claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubPlan {
    /// A new row, `unverified`, source `update`, and `counts.hubs + 1`.
    Insert,
    /// A servable hub on the same port: fields and `last_seen = now`; status
    /// and failures unchanged.
    Refresh,
    /// A servable hub on a new port: the new port, status `unverified`, until
    /// the cron has checked it.
    Demote,
    /// An `unverified` or `dead` row: the claim's port and fields, status
    /// `unverified`, source `update`, queued for the cron. `was_try_hubs`
    /// also means `counts.hubs_unverified - 1`: it leaves the candidates' cap.
    Requeue { was_try_hubs: bool },
    /// Refreshed less than [`COALESCE`] ago: nothing to write.
    Coalesced,
    /// No room for a new hub.
    Full,
}

impl HubPlan {
    /// Log label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            HubPlan::Insert => "inserted",
            HubPlan::Refresh => "refreshed",
            HubPlan::Demote => "demoted",
            HubPlan::Requeue { .. } => "requeued",
            HubPlan::Coalesced => "coalesced",
            HubPlan::Full => "full",
        }
    }
}

/// Plan a hub claim against its stored row (if any) and the hub count.
#[must_use]
pub fn plan_hub(existing: Option<&HubRow>, claim: &HubClaim, now: u64, hubs: u64) -> HubPlan {
    let Some(row) = existing else {
        return if hubs >= MAX_HUBS {
            HubPlan::Full
        } else {
            HubPlan::Insert
        };
    };
    if now.saturating_sub(row.last_seen) < COALESCE {
        return HubPlan::Coalesced;
    }
    match row.status.as_str() {
        "verified" | "alive_full" if row.port == claim.addr.port() => HubPlan::Refresh,
        "verified" | "alive_full" => HubPlan::Demote,
        // Only an `unverified` candidate is in the X-Try-Hubs cap; a `dead`
        // one already left it.
        _ => HubPlan::Requeue {
            was_try_hubs: row.status == "unverified" && row.source == "try_hubs",
        },
    }
}

/// What to do with a cache claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePlan {
    /// A new `pending` row, and `counts.caches + 1`.
    Insert,
    /// Already stored.
    Known,
    /// No room for a new cache.
    Full,
    /// The host already has [`MAX_PER_DOMAIN`] URLs.
    DomainFull,
}

impl CachePlan {
    /// Log label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            CachePlan::Insert => "inserted",
            CachePlan::Known => "known",
            CachePlan::Full => "full",
            CachePlan::DomainFull => "domain_full",
        }
    }
}

/// Plan a cache claim against the URLs stored for its host and the cache count.
#[must_use]
pub fn plan_cache(url: &CacheUrl, same_host: &[String], caches: u64) -> CachePlan {
    if same_host.iter().any(|u| u.eq_ignore_ascii_case(&url.url)) {
        CachePlan::Known
    } else if caches >= MAX_CACHES {
        CachePlan::Full
    } else if same_host.len() >= MAX_PER_DOMAIN {
        CachePlan::DomainFull
    } else {
        CachePlan::Insert
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_url::normalize;
    use crate::query::parse;

    const NOW: u64 = 1_792_744_200;

    fn update(ip: &str) -> Query {
        parse(&format!(
            "ip={ip}&x.leaves=12&uptime=7300&x.max=300&update=1&net=gnutella2&client=RAZA2.7.10.6"
        ))
        .unwrap()
    }

    fn claim() -> HubClaim {
        ip_claim(&update("8.8.4.4:6346"), "8.8.4.4", NOW)
            .unwrap()
            .unwrap()
    }

    fn canonical() -> CacheUrl {
        normalize("http://gwc.example.org/").unwrap()
    }

    #[test]
    fn a_hub_registering_itself_is_claimed_with_its_fields() {
        assert_eq!(
            claim(),
            HubClaim {
                addr: "8.8.4.4:6346".parse().unwrap(),
                vendor: Some("RAZA".into()),
                version: Some("2.7.10.6".into()),
                leaves: Some(12),
                max_leaves: Some(300),
                up_since: Some(NOW - 7300),
            }
        );
    }

    #[test]
    fn mapped_ipv6_source_counts_as_the_same_address() {
        assert!(
            ip_claim(&update("8.8.4.4:6346"), "::ffff:8.8.4.4", NOW)
                .unwrap()
                .is_ok()
        );
    }

    #[test]
    fn another_source_is_never_stored() {
        // ADR-0006: not even as unverified.
        for source in ["8.8.8.8", "", "2001:db8::1", "garbage"] {
            assert_eq!(
                ip_claim(&update("8.8.4.4:6346"), source, NOW),
                Some(Err(IpDecline::SourceMismatch)),
                "{source:?}"
            );
        }
    }

    #[test]
    fn bad_addresses_are_declined_before_the_source_check() {
        let d = |ip: &str| ip_claim(&update(ip), ip.split(':').next().unwrap(), NOW);
        assert_eq!(d("192.168.1.2:6346"), Some(Err(IpDecline::NotPublic)));
        assert_eq!(d("8.8.4.4:80"), Some(Err(IpDecline::BadPort)));
        assert_eq!(d("nonsense"), Some(Err(IpDecline::Malformed)));
    }

    #[test]
    fn g1_updates_are_declined() {
        let q =
            parse("ip=8.8.4.4:6346&update=1&net=gnutella&client=RAZA&version=2.7.10.6").unwrap();
        assert_eq!(
            ip_claim(&q, "8.8.4.4", NOW),
            Some(Err(IpDecline::NotGnutella2))
        );
    }

    #[test]
    fn no_ip_part_no_claim() {
        let q = parse("url=http%3A%2F%2Fgwc.example.net%2F&update=1&client=RAZA").unwrap();
        assert_eq!(ip_claim(&q, "8.8.4.4", NOW), None);
    }

    #[test]
    fn out_of_schema_counters_are_dropped_not_fatal() {
        let q = parse(
            "ip=8.8.4.4:6346&x.leaves=70000&x.max=99999&uptime=4000000000&update=1&client=RAZA",
        )
        .unwrap();
        let c = ip_claim(&q, "8.8.4.4", NOW).unwrap().unwrap();
        assert_eq!(
            (c.leaves, c.max_leaves),
            (None, None),
            "beyond the schema's 0..=65535"
        );
        assert_eq!(c.up_since, None, "an uptime longer than the epoch");
        assert_eq!(c.version, None, "empty version is not stored");
    }

    #[test]
    fn url_claims() {
        let q = |url: &str| {
            parse(&format!(
                "url={url}&update=1&net=gnutella2&client=RAZA2.7.10.6"
            ))
            .unwrap()
        };
        assert_eq!(
            url_claim(
                &q("http%3A%2F%2FGWC.example.NET%2Fskulls.php"),
                &canonical()
            ),
            Some(Ok(normalize("http://gwc.example.net/skulls.php").unwrap()))
        );
        assert_eq!(
            url_claim(&q("https%3A%2F%2Fgwc.example.net%2F"), &canonical()),
            Some(Err(UrlDecline::Https))
        );
        assert_eq!(
            url_claim(&q("ftp%3A%2F%2Fx"), &canonical()),
            Some(Err(UrlDecline::Invalid))
        );
        assert_eq!(
            url_claim(&q("http%3A%2F%2Fgwc.example.org"), &canonical()),
            Some(Err(UrlDecline::IsSelf)),
            "this cache, however it is spelled"
        );
        assert_eq!(
            url_claim(
                &q("http%3A%2F%2Fgwc.example.org%2Fanother%2Fpath.php"),
                &canonical()
            ),
            Some(Err(UrlDecline::IsSelf)),
            "any path on this cache's host and port answers as this cache"
        );
        assert!(
            url_claim(&q("http%3A%2F%2Fgwc.example.org%3A8080%2F"), &canonical())
                .unwrap()
                .is_ok(),
            "another port is another service"
        );
        let g1 =
            parse("url=http%3A%2F%2Fgwc.example.net%2F&update=1&net=gnutella&client=RAZA").unwrap();
        assert_eq!(
            url_claim(&g1, &canonical()),
            Some(Err(UrlDecline::NotGnutella2))
        );
        assert_eq!(url_claim(&update("8.8.4.4:6346"), &canonical()), None);
    }

    fn row(port: u16, status: &str, source: &str, last_seen: u64) -> HubRow {
        HubRow {
            port,
            status: status.into(),
            source: source.into(),
            last_seen,
        }
    }

    #[test]
    fn new_hubs_are_inserted_unverified_until_the_cap() {
        // ADR-0011: the update path never lists a hub; the cron promotes it
        // after a successful handshake.
        assert_eq!(plan_hub(None, &claim(), NOW, 0), HubPlan::Insert);
        assert_eq!(plan_hub(None, &claim(), NOW, MAX_HUBS - 1), HubPlan::Insert);
        assert_eq!(plan_hub(None, &claim(), NOW, MAX_HUBS), HubPlan::Full);
    }

    #[test]
    fn any_refresh_within_the_window_is_coalesced_whatever_the_status_or_port() {
        let c = claim();
        for status in ["verified", "alive_full", "unverified", "dead"] {
            for port in [6346, 6347] {
                assert_eq!(
                    plan_hub(Some(&row(port, status, "update", NOW - 60)), &c, NOW, 10),
                    HubPlan::Coalesced,
                    "{status} on port {port}"
                );
            }
        }
        assert_eq!(
            plan_hub(
                Some(&row(6346, "verified", "update", NOW - COALESCE + 1)),
                &c,
                NOW,
                10
            ),
            HubPlan::Coalesced
        );
    }

    #[test]
    fn a_servable_hub_on_the_same_port_is_refreshed_and_keeps_its_status() {
        let c = claim();
        for status in ["verified", "alive_full"] {
            assert_eq!(
                plan_hub(
                    Some(&row(6346, status, "update", NOW - COALESCE)),
                    &c,
                    NOW,
                    10
                ),
                HubPlan::Refresh,
                "{status}"
            );
        }
    }

    #[test]
    fn a_servable_hub_on_a_new_port_is_demoted() {
        assert_eq!(
            plan_hub(
                Some(&row(6347, "verified", "update", NOW - COALESCE)),
                &claim(),
                NOW,
                10
            ),
            HubPlan::Demote,
            "unlisted until the cron has checked the new port"
        );
    }

    #[test]
    fn a_dead_or_unverified_hub_is_requeued_for_the_cron() {
        let c = claim();
        assert_eq!(
            plan_hub(
                Some(&row(6346, "dead", "update", NOW - COALESCE)),
                &c,
                NOW,
                MAX_HUBS
            ),
            HubPlan::Requeue {
                was_try_hubs: false
            },
            "existing rows are requeued even at the cap"
        );
        assert_eq!(
            plan_hub(
                Some(&row(6347, "unverified", "try_hubs", NOW - COALESCE)),
                &c,
                NOW,
                10
            ),
            HubPlan::Requeue { was_try_hubs: true },
            "a candidate that registers itself leaves the X-Try-Hubs cap"
        );
        assert_eq!(
            plan_hub(
                Some(&row(6346, "dead", "try_hubs", NOW - COALESCE)),
                &c,
                NOW,
                10
            ),
            HubPlan::Requeue {
                was_try_hubs: false
            },
            "a dead candidate is no longer in that cap"
        );
        assert_eq!(
            plan_hub(
                Some(&row(6346, "unverified", "update", NOW - COALESCE)),
                &c,
                NOW,
                10
            ),
            HubPlan::Requeue {
                was_try_hubs: false
            }
        );
    }

    #[test]
    fn outcome_labels_are_stable_and_ip_free() {
        assert_eq!(IpDecline::SourceMismatch.as_str(), "source_mismatch");
        assert_eq!(IpDecline::NotPublic.as_str(), "not_public");
        assert_eq!(UrlDecline::IsSelf.as_str(), "self");
        assert_eq!(HubPlan::Refresh.as_str(), "refreshed");
        assert_eq!(HubPlan::Demote.as_str(), "demoted");
        assert_eq!(HubPlan::Requeue { was_try_hubs: true }.as_str(), "requeued");
        assert_eq!(HubPlan::Coalesced.as_str(), "coalesced");
        assert_eq!(CachePlan::DomainFull.as_str(), "domain_full");
        for label in [
            IpDecline::NotGnutella2.as_str(),
            IpDecline::Malformed.as_str(),
            IpDecline::BadPort.as_str(),
            UrlDecline::Https.as_str(),
            UrlDecline::Invalid.as_str(),
            UrlDecline::NotGnutella2.as_str(),
            HubPlan::Insert.as_str(),
            HubPlan::Full.as_str(),
            CachePlan::Insert.as_str(),
            CachePlan::Known.as_str(),
            CachePlan::Full.as_str(),
        ] {
            assert!(
                label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "{label}"
            );
        }
    }

    fn urls(u: &[&str]) -> Vec<String> {
        u.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn cache_plans() {
        let u = normalize("http://gwc.example.net/b/").unwrap();
        assert_eq!(plan_cache(&u, &[], 0), CachePlan::Insert);
        assert_eq!(
            plan_cache(&u, &urls(&["http://gwc.example.net/b/"]), 10),
            CachePlan::Known
        );
        assert_eq!(plan_cache(&u, &[], MAX_CACHES), CachePlan::Full);
        let three = urls(&[
            "http://gwc.example.net/a/",
            "http://gwc.example.net/c/",
            "http://gwc.example.net/d/",
        ]);
        assert_eq!(plan_cache(&u, &three, 10), CachePlan::DomainFull);
        assert_eq!(plan_cache(&u, &three[..2], 10), CachePlan::Insert);
    }
}
