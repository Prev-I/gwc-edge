//! Request counters for `/stats`: a closed set of request kinds, bounded keys,
//! and 1-in-[`SAMPLE_K`] sampling.
//!
//! Exact per-request counting costs one D1 row write per request, which a
//! flood turns into the whole daily write quota. A sampled request writes
//! `n = n + SAMPLE_K`, so counts are unbiased estimates and writes are bounded
//! at `requests / SAMPLE_K` whatever keys a client invents.

use crate::query::{Net, Query};

/// One request in this many is counted, with weight `SAMPLE_K`.
pub const SAMPLE_K: u32 = 8;

/// What a request was. This is the closed set the `request_log.kind` CHECK
/// constraint mirrors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Ping,
    Hostfile,
    Urlfile,
    Update,
    /// A get or update for a network other than Gnutella2.
    OtherNet,
    /// Answered from the rate limiter, without data.
    Limited,
    /// The client's vendor code is denylisted.
    Denied,
    /// The query could not be parsed.
    Error,
    /// Health event, not a request: a D1 call failed.
    D1Error,
    /// Health event, not a request: a reply was served in degraded mode.
    Degraded,
}

impl Kind {
    /// Every kind, in the order the CHECK constraint lists them.
    pub const ALL: [Kind; 10] = [
        Kind::Ping,
        Kind::Hostfile,
        Kind::Urlfile,
        Kind::Update,
        Kind::OtherNet,
        Kind::Limited,
        Kind::Denied,
        Kind::Error,
        Kind::D1Error,
        Kind::Degraded,
    ];

    /// The kinds that are requests, as `/stats` counts them.
    pub const REQUESTS: [Kind; 8] = [
        Kind::Ping,
        Kind::Hostfile,
        Kind::Urlfile,
        Kind::Update,
        Kind::OtherNet,
        Kind::Limited,
        Kind::Denied,
        Kind::Error,
    ];

    /// Health events, recorded exactly (not sampled) by `crate::health`.
    pub const HEALTH: [Kind; 2] = [Kind::D1Error, Kind::Degraded];

    /// Whether this kind counts as a request.
    #[must_use]
    pub fn is_request(self) -> bool {
        !Kind::HEALTH.contains(&self)
    }

    /// Column value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Ping => "ping",
            Kind::Hostfile => "hostfile",
            Kind::Urlfile => "urlfile",
            Kind::Update => "update",
            Kind::OtherNet => "other_net",
            Kind::Limited => "limited",
            Kind::Denied => "denied",
            Kind::Error => "error",
            Kind::D1Error => "d1_error",
            Kind::Degraded => "degraded",
        }
    }

    /// Inverse of [`Kind::as_str`].
    #[must_use]
    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// The kind of a parsed, allowed, not rate-limited query.
    #[must_use]
    pub fn of(q: &Query) -> Kind {
        let lone_ping = !(q.is_update() || q.hostfile || q.urlfile);
        if lone_ping {
            // Answered with the pong on any network (`response::get_reply`).
            Kind::Ping
        } else if q.net != Net::Gnutella2 {
            Kind::OtherNet
        } else if q.is_update() {
            Kind::Update
        } else if q.hostfile {
            Kind::Hostfile
        } else {
            Kind::Urlfile
        }
    }
}

/// A `request_log` primary key, minus the hour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub kind: Kind,
    /// Vendor code, or `-` when the query had none.
    pub client: String,
    /// Version, or `-` when empty or absent.
    pub version: String,
    /// `http` or `https`.
    pub scheme: &'static str,
}

impl Key {
    /// Build a key. `client`/`version` come from [`crate::query::Client`]
    /// (already bounded and sanitised) or are `None` for unparsable queries.
    #[must_use]
    pub fn new(kind: Kind, client: Option<(&str, &str)>, https: bool) -> Key {
        let (client, version) = client.unwrap_or(("-", "-"));
        let or_dash = |s: &str| {
            if s.is_empty() {
                "-".to_owned()
            } else {
                s.to_owned()
            }
        };
        Key {
            kind,
            client: or_dash(client),
            version: or_dash(version),
            scheme: if https { "https" } else { "http" },
        }
    }
}

/// The hour bucket a timestamp falls in.
#[must_use]
pub fn hour(now: u64) -> u64 {
    now / 3600
}

/// Whether this request is counted, given a uniform random `u32`.
///
/// A threshold rather than a modulus: it uses the high bits, which stay
/// uniform when the value comes from a scaled float such as `Math.random()`.
#[must_use]
pub fn sampled(random: u32) -> bool {
    u64::from(random) * u64::from(SAMPLE_K) < 1 << 32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse;

    #[test]
    fn kinds_round_trip_and_are_distinct() {
        let names: Vec<&str> = Kind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            names,
            [
                "ping",
                "hostfile",
                "urlfile",
                "update",
                "other_net",
                "limited",
                "denied",
                "error",
                "d1_error",
                "degraded"
            ]
        );
        for k in Kind::ALL {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
        }
        assert_eq!(Kind::parse("stats"), None);
    }

    #[test]
    fn health_kinds_are_not_requests() {
        assert_eq!(Kind::REQUESTS.len() + Kind::HEALTH.len(), Kind::ALL.len());
        for k in Kind::HEALTH {
            assert!(!k.is_request(), "{k:?}");
            assert!(!Kind::REQUESTS.contains(&k));
        }
        for k in Kind::REQUESTS {
            assert!(k.is_request(), "{k:?}");
        }
    }

    #[test]
    fn kind_of_queries() {
        let kind = |q: &str| Kind::of(&parse(q).unwrap());
        assert_eq!(
            kind("get=1&hostfile=1&ping=1&net=gnutella2&client=RAZA2.7"),
            Kind::Hostfile
        );
        assert_eq!(
            kind("get=1&urlfile=1&ping=1&net=gnutella2&client=RAZA2.7"),
            Kind::Urlfile
        );
        assert_eq!(
            kind("get=1&net=gnutella2&client=RAZA"),
            Kind::Hostfile,
            "both files count as hostfile"
        );
        assert_eq!(kind("ping=1&net=gnutella2&client=TEST"), Kind::Ping);
        assert_eq!(
            kind("ping=1&client=TEST&version=0.1"),
            Kind::Ping,
            "a lone ping is answered on any network, so it counts as a ping"
        );
        assert_eq!(
            kind("ip=8.8.4.4:6346&update=1&net=gnutella2&client=RAZA"),
            Kind::Update
        );
        assert_eq!(
            kind("get=1&hostfile=1&client=RAZA"),
            Kind::OtherNet,
            "no net= means Gnutella"
        );
        assert_eq!(
            kind("get=1&hostfile=1&net=gnutella&client=RAZA"),
            Kind::OtherNet
        );
        assert_eq!(
            kind("ip=8.8.4.4:6346&net=gnutella&client=RAZA"),
            Kind::OtherNet
        );
    }

    #[test]
    fn keys_fill_placeholders_and_scheme() {
        let k = Key::new(Kind::Hostfile, Some(("RAZA", "2.7.10.6")), false);
        assert_eq!(
            k,
            Key {
                kind: Kind::Hostfile,
                client: "RAZA".into(),
                version: "2.7.10.6".into(),
                scheme: "http"
            }
        );
        let k = Key::new(Kind::Error, None, true);
        assert_eq!(
            (k.client.as_str(), k.version.as_str(), k.scheme),
            ("-", "-", "https")
        );
        let k = Key::new(Kind::Ping, Some(("RAZA", "")), false);
        assert_eq!(k.version, "-");
    }

    #[test]
    fn migration_check_constraint_lists_exactly_these_kinds() {
        // A kind core can emit must never fail the CHECK on insert.
        let sql = include_str!("../../../migrations/0001_initial.sql");
        let list: Vec<String> = Kind::ALL
            .iter()
            .map(|k| format!("'{}'", k.as_str()))
            .collect();
        assert!(
            sql.contains(&format!("CHECK (kind IN ({}))", list.join(", "))),
            "request_log.kind CHECK out of sync with Kind::ALL"
        );
    }

    #[test]
    fn hour_buckets() {
        assert_eq!(hour(0), 0);
        assert_eq!(hour(3599), 0);
        assert_eq!(hour(3600), 1);
    }

    #[test]
    fn one_in_k_is_sampled() {
        // Exactly 1/K of a uniform sweep over the u32 range is sampled.
        let step = 1u32 << 16;
        let hits = (0..=u32::MAX / step).filter(|i| sampled(i * step)).count();
        assert_eq!(hits, (1usize << 16) / SAMPLE_K as usize);
        assert!(sampled(0));
        assert!(!sampled(u32::MAX));
    }
}
