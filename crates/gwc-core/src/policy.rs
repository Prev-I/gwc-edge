//! Client policy: the vendor denylist and rate-limit keys.

use std::net::IpAddr;

use crate::query::Query;

/// Vendor codes denied by default. `CRUX` is CruxP2P, a known scam client.
pub const DEFAULT_DENYLIST: &[&str] = &["CRUX"];

/// Parse a comma-separated denylist (e.g. the `CLIENT_DENYLIST` variable):
/// entries are trimmed and uppercased, empty entries dropped.
#[must_use]
pub fn parse_denylist(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|v| v.trim().to_ascii_uppercase())
        .filter(|v| !v.is_empty())
        .collect()
}

/// Whether a vendor code is denied.
#[must_use]
pub fn is_denied(vendor: &str, denylist: &[String]) -> bool {
    denylist.iter().any(|d| d.eq_ignore_ascii_case(vendor))
}

/// What a request is rate-limited as. Hostfile and urlfile gets are limited
/// separately because Shareaza sends a urlfile get immediately after a
/// successful hostfile get to the same cache (`DiscoveryServices.cpp:1340-1352`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateKind {
    Hostfile,
    Urlfile,
    Update,
}

impl RateKind {
    /// The kind a parsed query counts as. A ping alone is not limited: it
    /// costs no D1 access.
    #[must_use]
    pub fn of(q: &Query) -> Option<RateKind> {
        if q.is_update() {
            Some(RateKind::Update)
        } else if q.hostfile {
            Some(RateKind::Hostfile)
        } else if q.urlfile {
            Some(RateKind::Urlfile)
        } else {
            None
        }
    }

    fn tag(self) -> &'static str {
        match self {
            RateKind::Hostfile => "h",
            RateKind::Urlfile => "u",
            RateKind::Update => "U",
        }
    }
}

/// Rate-limit key for a source address. IPv6 sources are grouped by /64, the
/// smallest block one subscriber is normally given, so rotating addresses
/// within it does not multiply the allowance. An unparsable source shares one
/// key, so it cannot bypass the limit.
#[must_use]
pub fn rate_key(source: &str, kind: RateKind) -> String {
    let who = match source.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.to_string(),
        Ok(IpAddr::V6(v6)) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                v4.to_string()
            } else {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        }
        Err(_) => "unknown".to_owned(),
    };
    format!("{}|{who}", kind.tag())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse;

    #[test]
    fn default_denylist_has_crux() {
        let list = parse_denylist(&DEFAULT_DENYLIST.join(","));
        assert!(is_denied("CRUX", &list));
        assert!(is_denied("crux", &list));
        assert!(!is_denied("RAZA", &list));
    }

    #[test]
    fn denylist_parsing_trims_and_uppercases() {
        assert_eq!(parse_denylist(" crux, foxy ,,"), ["CRUX", "FOXY"]);
        assert_eq!(parse_denylist(""), Vec::<String>::new());
    }

    #[test]
    fn rate_kind_of_each_query_shape() {
        let kind = |q: &str| RateKind::of(&parse(q).unwrap());
        assert_eq!(
            kind("get=1&hostfile=1&ping=1&client=RAZA"),
            Some(RateKind::Hostfile)
        );
        assert_eq!(
            kind("get=1&urlfile=1&ping=1&client=RAZA"),
            Some(RateKind::Urlfile)
        );
        assert_eq!(
            kind("ip=8.8.4.4:6346&update=1&client=RAZA"),
            Some(RateKind::Update)
        );
        assert_eq!(kind("ping=1&client=RAZA"), None);
    }

    #[test]
    fn hostfile_then_urlfile_from_one_client_use_different_keys() {
        assert_ne!(
            rate_key("8.8.4.4", RateKind::Hostfile),
            rate_key("8.8.4.4", RateKind::Urlfile)
        );
    }

    #[test]
    fn ipv6_is_grouped_by_64() {
        let a = rate_key("2001:db8:1:2:aaaa::1", RateKind::Hostfile);
        let b = rate_key("2001:db8:1:2:bbbb::2", RateKind::Hostfile);
        let c = rate_key("2001:db8:1:3::1", RateKind::Hostfile);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a, "h|2001:db8:1:2::/64");
    }

    #[test]
    fn mapped_ipv4_and_garbage() {
        assert_eq!(rate_key("::ffff:8.8.4.4", RateKind::Update), "U|8.8.4.4");
        assert_eq!(rate_key("not an ip", RateKind::Update), "U|unknown");
    }
}
