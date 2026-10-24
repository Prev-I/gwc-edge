//! GWebCache v2 response lines, and the rules for which lines a request gets.
//!
//! Every rule here exists because of how Shareaza parses a response; the
//! citations are to `DiscoveryServices.cpp` at ansani/Shareaza@f24b0ce. Two
//! facts drive most of them:
//!
//! - Shareaza removes a cache after two consecutive failures
//!   (`Settings.cpp:344` `FailureLimit` = 2, enforced at `:2414`). Removal
//!   deletes the whole entry, G1 and G2 alike (`:2153-2156`).
//! - A failure is any reply it does not recognise as success, and the bar for
//!   success is narrow (see [`get_reply`] and [`update_reply`]).
//!
//! The tests judge every reply through `shareaza_model`, a test-only model of
//! Shareaza's two parsers, so they pin Shareaza's verdict, not just our output.

use std::net::SocketAddrV4;

use crate::cache_url::CacheUrl;
use crate::query::{Net, Query};

/// Networks advertised in the pong. Shareaza fails a G2 query whose pong does
/// not list `gnutella2` (`:1652-1667`).
pub const NETWORKS: &str = "gnutella2";

/// Seconds advertised in `i|update|period|`. Shareaza keeps it per cache
/// (`:1927-1934`) and defaults it to `Discovery.DefaultUpdate`, one hour
/// (`Settings.cpp:343`, `:2139`). Each hub is also gated globally to one
/// update per `DefaultUpdate` (`:863`), so a smaller value would only raise
/// this cache's share of updates; the default asks for nothing unusual.
pub const UPDATE_PERIOD: u32 = 3600;

/// Ages are clamped to one year, as Shareaza does (`:1487`).
pub const MAX_AGE: u64 = 60 * 60 * 24 * 365;

/// One hub in a hostfile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    pub addr: SocketAddrV4,
    /// Seconds since the hub was last seen alive.
    pub age: u64,
    pub leaves: Option<u32>,
    pub vendor: Option<String>,
    /// Seconds the hub has been up.
    pub uptime: Option<u32>,
    pub max_leaves: Option<u32>,
}

/// One cache in a urlfile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlEntry {
    pub url: String,
    /// Seconds since the cache was last verified.
    pub age: u64,
}

/// Why an update is refused, rendered as `i|update|WARNING|<reason>`.
///
/// Shareaza counts any update reply without `OK` as a failure (`:1898`,
/// `:1942`). Updates we merely decline to store (a NAT address, a low port, a
/// third-party cache URL we do not keep, a G1 update from a dual-mode hub)
/// therefore get `OK` like any other; a refusal is reserved for clients we
/// want to drop us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The client's vendor code is denylisted.
    ClientNotSupported,
}

impl Refusal {
    /// Reason text. Must never contain `OK`, `error`, `early` or `bad url`: the
    /// first reads as success (`:1898`), the next two as failures
    /// (`:1905-1918`), and the last removes the cache on a get (`:1695-1705`).
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Refusal::ClientNotSupported => "client not supported",
        }
    }
}

/// A response line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// `i|pong|gwc-edge <version>|gnutella2`
    Pong,
    /// `h|<ip>:<port>|<age>[|<cluster>|<leaves>|<vendor>|<uptime>|<leaf limit>]`
    Host(HostEntry),
    /// `u|<url>|<age>`
    Url(UrlEntry),
    /// `i|update|period|<seconds>`
    UpdatePeriod(u32),
    /// `i|update|OK`
    UpdateOk,
    /// `i|update|WARNING|<reason>`
    UpdateWarning(Refusal),
    /// `i|error|<reason>`
    Error(&'static str),
}

impl Line {
    /// The line without its terminator.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Line::Pong => format!("i|pong|{}|{NETWORKS}", crate::product_id()),
            Line::Host(h) => h.render(),
            Line::Url(u) => format!("u|{}|{}", u.url, u.age.min(MAX_AGE)),
            Line::UpdatePeriod(secs) => format!("i|update|period|{secs}"),
            Line::UpdateOk => "i|update|OK".into(),
            Line::UpdateWarning(w) => format!("i|update|WARNING|{}", w.reason()),
            Line::Error(reason) => format!("i|error|{reason}"),
        }
    }
}

/// Upper bound (exclusive) for leaf counts Shareaza accepts (`:1498`, `:1539`).
const LEAF_LIMIT: u32 = 2048;

impl HostEntry {
    /// Every optional field is emitted only if Shareaza would accept it: one
    /// out-of-range field fails the whole response (`:1491-1546`).
    fn render(&self) -> String {
        let year = u32::try_from(MAX_AGE).unwrap_or(u32::MAX);
        let leaves = |n: Option<u32>| n.filter(|&n| n < LEAF_LIMIT).map(|n| n.to_string());
        let fields = [
            String::new(), // cluster: Shareaza asks getclusters=0 and skips it (:1490)
            leaves(self.leaves).unwrap_or_default(),
            self.vendor
                .as_deref()
                .filter(|v| is_quotable_vendor(v))
                .unwrap_or_default()
                .to_owned(),
            self.uptime
                .filter(|&u| u > 60 && u < year)
                .map(|u| u.to_string())
                .unwrap_or_default(),
            leaves(self.max_leaves).unwrap_or_default(),
        ];
        let used = fields
            .iter()
            .rposition(|f| !f.is_empty())
            .map_or(0, |i| i + 1);
        let mut line = format!("h|{}|{}", self.addr, self.age.min(MAX_AGE));
        for f in &fields[..used] {
            line.push('|');
            line.push_str(f);
        }
        line
    }
}

/// Four ASCII alphanumerics, and not one Shareaza blocks: a blocked vendor
/// aborts the response (`:1509-1514`, `Security.cpp:1313-1321` blocks "foxy").
fn is_quotable_vendor(v: &str) -> bool {
    v.len() == 4 && v.bytes().all(|b| b.is_ascii_alphanumeric()) && !v.eq_ignore_ascii_case("foxy")
}

/// Render a full body: every line terminated by `\n`, including the last.
#[must_use]
pub fn render(lines: &[Line]) -> String {
    let mut body = String::new();
    for line in lines {
        body.push_str(&line.render());
        body.push('\n');
    }
    body
}

/// Lines for a get (and/or ping) from a client that is not denylisted.
/// Denylisted clients get an empty body, which Shareaza counts as a failure
/// (`:1794-1802`) and, after two, drops this cache: the intended outcome.
///
/// - Every hostfile or urlfile request ends with `u|<self_url>|0`. Shareaza
///   counts a `u|` line as success when the URL is already in its list
///   (`:138-140`, `:1573-1578`), and the cache it queried always is. Without
///   it an empty hostfile would get this cache removed after two queries.
/// - Non-G2 networks get only that line: no hosts (we have none for G1) and
///   no pong, which a G1 query treats as a failure (`:1641-1646`). An empty
///   reply would get this cache removed for G2 as well, since removal deletes
///   the whole entry.
/// - The pong is sent only when asked for: on G2, or for a ping without a get.
/// - A rate-limited get is answered by calling this with no hosts and no
///   caches: Shareaza still counts it a success, and no D1 row is read.
/// - `self_url` is not repeated if it is also among `caches`.
#[must_use]
pub fn get_reply(
    q: &Query,
    hosts: &[HostEntry],
    caches: &[UrlEntry],
    self_url: &CacheUrl,
) -> Vec<Line> {
    let g2 = q.net == Net::Gnutella2;
    let files = q.hostfile || q.urlfile;
    let mut lines = Vec::new();
    // A pong on a G1 *get* is a failure to Shareaza (:1641-1646); a ping
    // without a get, which Shareaza never sends, gets one on any network so
    // no allowed client ever receives an empty body.
    if q.ping && (g2 || !files) {
        lines.push(Line::Pong);
    }
    if g2 && q.hostfile {
        lines.extend(hosts.iter().cloned().map(Line::Host));
    }
    if g2 && q.urlfile {
        lines.extend(
            caches
                .iter()
                .filter(|c| !c.url.eq_ignore_ascii_case(&self_url.url))
                .cloned()
                .map(Line::Url),
        );
    }
    if files {
        lines.push(Line::Url(UrlEntry {
            url: self_url.url.clone(),
            age: 0,
        }));
    }
    lines
}

/// Lines for an update.
///
/// `Ok(())` covers everything a legitimate client can send: stored, merged,
/// rate-limited and declined updates alike are `i|update|OK`, preceded by the
/// update period. The period line comes first because Shareaza stops reading
/// at the first line containing `OK` (`:1898-1904`).
#[must_use]
pub fn update_reply(outcome: Result<(), Refusal>) -> Vec<Line> {
    match outcome {
        Ok(()) => vec![Line::UpdatePeriod(UPDATE_PERIOD), Line::UpdateOk],
        Err(r) => vec![Line::UpdateWarning(r)],
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use proptest::prelude::*;

    use super::*;
    use crate::cache_url::normalize;
    use crate::query::parse;
    use crate::shareaza_model::{self as shareaza, Outcome};

    fn self_url() -> CacheUrl {
        normalize("http://gwc.example.org/").unwrap()
    }

    fn host(age: u64) -> HostEntry {
        HostEntry {
            addr: SocketAddrV4::new(Ipv4Addr::new(8, 8, 4, 4), 6346),
            age,
            leaves: None,
            vendor: None,
            uptime: None,
            max_leaves: None,
        }
    }

    fn cache(url: &str, age: u64) -> UrlEntry {
        UrlEntry {
            url: url.into(),
            age,
        }
    }

    fn rendered(lines: &[Line]) -> Vec<String> {
        lines.iter().map(Line::render).collect()
    }

    /// Shareaza's verdict on our reply to `query`.
    fn verdict(query: &str, hosts: &[HostEntry], caches: &[UrlEntry]) -> Outcome {
        let q = parse(query).unwrap();
        let body = render(&get_reply(&q, hosts, caches, &self_url()));
        shareaza::get(&body, q.net == Net::Gnutella2, &[&self_url().url])
    }

    const G2_HOSTS: &str = "get=1&hostfile=1&net=gnutella2&ping=1&pv=4&client=RAZA2.7.10.6";
    const G2_URLS: &str = "get=1&urlfile=1&net=gnutella2&ping=1&pv=4&client=RAZA2.7.10.6";
    const G1_HOSTS: &str = "get=1&hostfile=1&net=gnutella&client=RAZA&version=2.7.10.6";
    const G1_URLS: &str = "get=1&urlfile=1&net=gnutella&client=RAZA&version=2.7.10.6";

    // --- Shareaza's verdict on every kind of get reply ------------------------

    #[test]
    fn g2_hostfile_with_hubs_succeeds() {
        assert_eq!(
            verdict(G2_HOSTS, &[host(10), host(20)], &[]),
            Outcome::Success
        );
    }

    #[test]
    fn g2_hostfile_without_hubs_still_succeeds() {
        // Decision B: without the self u| line this would be a failure.
        assert_eq!(verdict(G2_HOSTS, &[], &[]), Outcome::Success);
    }

    #[test]
    fn g2_urlfile_succeeds() {
        let caches = [cache("http://dkac.trillinux.org/dkac/dkac.php", 60)];
        assert_eq!(verdict(G2_URLS, &[], &caches), Outcome::Success);
        assert_eq!(verdict(G2_URLS, &[], &[]), Outcome::Success);
    }

    #[test]
    fn g1_gets_succeed_without_g1_data() {
        // Removal would delete the G2 entry too (:2153-2156), so G1 gets are
        // kept successful with the self line alone.
        assert_eq!(verdict(G1_HOSTS, &[host(1)], &[]), Outcome::Success);
        assert_eq!(verdict(G1_URLS, &[], &[]), Outcome::Success);
        let q = parse(G1_HOSTS).unwrap();
        assert_eq!(
            rendered(&get_reply(&q, &[host(1)], &[], &self_url())),
            ["u|http://gwc.example.org/|0"]
        );
    }

    #[test]
    fn rate_limited_get_succeeds_without_data() {
        // A rate-limited get is answered with no hosts and no caches.
        assert_eq!(verdict(G2_HOSTS, &[], &[]), Outcome::Success);
        assert_eq!(verdict(G2_URLS, &[], &[]), Outcome::Success);
    }

    #[test]
    fn self_line_only_helps_if_it_is_the_address_the_client_stored() {
        // An alias (another host, scheme or port) is not "known" (:369-392),
        // and with a full cache list a new URL is refused (:258-268).
        let q = parse(G2_HOSTS).unwrap();
        let body = render(&get_reply(&q, &[], &[], &self_url()));
        assert_eq!(
            shareaza::get(&body, true, &["http://gwc-edge.example.workers.dev/"]),
            Outcome::Failure
        );
    }

    #[test]
    fn denylisted_empty_body_fails() {
        assert_eq!(shareaza::get("", true, &[]), Outcome::Failure);
    }

    proptest! {
        #[test]
        fn any_host_data_yields_a_successful_reply(
            ages in proptest::collection::vec(any::<u64>(), 0..30),
            leaves in proptest::option::of(any::<u32>()),
            max_leaves in proptest::option::of(any::<u32>()),
            uptime in proptest::option::of(any::<u32>()),
            vendor in proptest::option::of("[ -~]{0,6}"),
        ) {
            let hosts: Vec<HostEntry> = ages
                .iter()
                .map(|&age| HostEntry {
                    leaves,
                    max_leaves,
                    uptime,
                    vendor: vendor.clone(),
                    ..host(age)
                })
                .collect();
            prop_assert_eq!(verdict(G2_HOSTS, &hosts, &[]), Outcome::Success);
        }
    }

    // --- line formats -----------------------------------------------------------

    #[test]
    fn pong_line() {
        assert_eq!(
            Line::Pong.render(),
            format!("i|pong|gwc-edge {}|gnutella2", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn minimal_host_line() {
        assert_eq!(Line::Host(host(42)).render(), "h|8.8.4.4:6346|42");
    }

    #[test]
    fn extended_host_line_follows_shareaza_field_order() {
        // "h|Host:Port|Age|Cluster|CurrentLeaves|VendorCode|Uptime|LeafLimit" (:1474)
        let h = HostEntry {
            leaves: Some(120),
            vendor: Some("RAZA".into()),
            uptime: Some(7300),
            max_leaves: Some(300),
            ..host(42)
        };
        assert_eq!(
            Line::Host(h).render(),
            "h|8.8.4.4:6346|42||120|RAZA|7300|300"
        );
    }

    #[test]
    fn trailing_unknown_fields_are_omitted() {
        let h = HostEntry {
            leaves: Some(5),
            ..host(1)
        };
        assert_eq!(Line::Host(h).render(), "h|8.8.4.4:6346|1||5");
    }

    #[test]
    fn out_of_range_host_fields_are_left_empty() {
        // Shareaza fails the *whole* response on one bad field: leaves and
        // leaf limit must be 0..2048 (:1498, :1539), uptime 61..1 year
        // (:1524), and a blocked vendor aborts it (:1509-1514).
        let h = HostEntry {
            leaves: Some(2048),
            vendor: Some("FOXY".into()),
            uptime: Some(60),
            max_leaves: Some(5000),
            ..host(7)
        };
        assert_eq!(Line::Host(h).render(), "h|8.8.4.4:6346|7");
    }

    #[test]
    fn vendor_must_be_four_alphanumerics() {
        for bad in ["RAZ", "RAZAX", "RA|A", "RA A", "foxy"] {
            let h = HostEntry {
                vendor: Some(bad.into()),
                ..host(1)
            };
            assert_eq!(Line::Host(h).render(), "h|8.8.4.4:6346|1", "{bad:?}");
        }
    }

    #[test]
    fn uptime_bounds_are_exclusive() {
        let render = |uptime| {
            Line::Host(HostEntry {
                uptime: Some(uptime),
                ..host(1)
            })
            .render()
        };
        assert_eq!(render(61), "h|8.8.4.4:6346|1||||61");
        assert_eq!(render(60), "h|8.8.4.4:6346|1");
        let year = u32::try_from(MAX_AGE).unwrap();
        assert_eq!(
            render(year - 1),
            format!("h|8.8.4.4:6346|1||||{}", year - 1)
        );
        assert_eq!(render(year), "h|8.8.4.4:6346|1");
    }

    #[test]
    fn age_is_clamped_to_a_year() {
        assert_eq!(
            Line::Host(host(u64::MAX)).render(),
            format!("h|8.8.4.4:6346|{MAX_AGE}")
        );
    }

    #[test]
    fn url_line() {
        let u = cache("http://dkac.trillinux.org/dkac/dkac.php", 300);
        assert_eq!(
            Line::Url(u).render(),
            "u|http://dkac.trillinux.org/dkac/dkac.php|300"
        );
    }

    #[test]
    fn update_and_error_lines() {
        assert_eq!(Line::UpdateOk.render(), "i|update|OK");
        assert_eq!(Line::UpdatePeriod(3600).render(), "i|update|period|3600");
        assert_eq!(
            Line::UpdateWarning(Refusal::ClientNotSupported).render(),
            "i|update|WARNING|client not supported"
        );
        assert_eq!(Line::Error("no command").render(), "i|error|no command");
    }

    #[test]
    fn body_is_lf_terminated_including_the_last_line() {
        // Shareaza splits on \r or \n and trims (:1455-1459); plain LF is
        // what the live dkac cache sends.
        let body = render(&[Line::Pong, Line::UpdateOk]);
        assert_eq!(body, format!("{}\ni|update|OK\n", Line::Pong.render()));
        assert!(!body.contains('\r'));
        assert_eq!(render(&[]), "");
    }

    // --- which lines a get receives ---------------------------------------------

    #[test]
    fn shareaza_hostfile_get_gets_pong_hosts_then_self() {
        let q = parse(G2_HOSTS).unwrap();
        let lines = get_reply(&q, &[host(10), host(20)], &[], &self_url());
        assert_eq!(
            rendered(&lines),
            [
                Line::Pong.render(),
                "h|8.8.4.4:6346|10".into(),
                "h|8.8.4.4:6346|20".into(),
                "u|http://gwc.example.org/|0".into(),
            ]
        );
    }

    #[test]
    fn urlfile_get_lists_caches_and_self_once() {
        let q = parse(G2_URLS).unwrap();
        let caches = [
            cache("http://dkac.trillinux.org/dkac/dkac.php", 60),
            cache("HTTP://GWC.EXAMPLE.ORG/", 0),
        ];
        let lines = get_reply(&q, &[host(1)], &caches, &self_url());
        assert_eq!(
            rendered(&lines),
            [
                Line::Pong.render(),
                "u|http://dkac.trillinux.org/dkac/dkac.php|60".into(),
                "u|http://gwc.example.org/|0".into(),
            ],
            "no hosts on a urlfile request, and self listed once"
        );
    }

    #[test]
    fn hosts_come_before_caches_when_both_are_asked() {
        let q = parse("get=1&hostfile=1&urlfile=1&client=TEST").unwrap();
        let caches = [cache("http://other.example.net/gwc/", 5)];
        let lines = get_reply(&q, &[host(1)], &caches, &self_url());
        assert_eq!(
            rendered(&lines),
            [
                "h|8.8.4.4:6346|1".to_string(),
                "u|http://other.example.net/gwc/|5".into(),
                "u|http://gwc.example.org/|0".into(),
            ]
        );
    }

    #[test]
    fn no_pong_unless_asked_on_g2() {
        let q = parse("get=1&hostfile=1&net=gnutella2&client=RAZA").unwrap();
        assert!(!get_reply(&q, &[host(1)], &[], &self_url()).contains(&Line::Pong));
        let g1_ping = parse("get=1&hostfile=1&ping=1&net=gnutella&client=RAZA").unwrap();
        assert!(!get_reply(&g1_ping, &[], &[], &self_url()).contains(&Line::Pong));
    }

    #[test]
    fn ping_without_get_gets_a_pong_on_any_network() {
        // Never an empty body for an allowed client (addendum A2). Shareaza
        // never sends a G1 ping, and a G1 *get* still gets no pong.
        let q = parse("ping=1&net=gnutella&client=TEST&version=0.1").unwrap();
        assert_eq!(get_reply(&q, &[host(1)], &[], &self_url()), [Line::Pong]);
    }

    #[test]
    fn plain_ping_is_just_a_pong() {
        let q = parse("ping=1&client=TEST&version=0.1").unwrap();
        assert_eq!(get_reply(&q, &[host(1)], &[], &self_url()), [Line::Pong]);
    }

    // --- which lines an update receives -----------------------------------------

    #[test]
    fn accepted_update_succeeds_with_period_first() {
        let lines = update_reply(Ok(()));
        assert_eq!(lines, [Line::UpdatePeriod(UPDATE_PERIOD), Line::UpdateOk]);
        assert_eq!(shareaza::update(&render(&lines)), Outcome::Success);
    }

    #[test]
    fn update_period_never_shortens_shareaza_default() {
        // Discovery.DefaultUpdate is 60*60 (Settings.cpp:343).
        const { assert!(UPDATE_PERIOD >= 3600) };
    }

    #[test]
    fn refused_update_fails_but_never_removes() {
        let body = render(&update_reply(Err(Refusal::ClientNotSupported)));
        assert_eq!(shareaza::update(&body), Outcome::Failure, "{body}");
        // The same reason must not remove us if it ever reached a get parser.
        assert_ne!(shareaza::get(&body, true, &[]), Outcome::Removed, "{body}");
        assert!(!body.contains("OK"));
    }
}
