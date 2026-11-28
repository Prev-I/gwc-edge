//! Advertising this cache to verified caches (ADR-0021).
//!
//! The pure rules: which caches get an advertisement, how a peer's urlfile is
//! scanned for this cache, how its reply is classified, and the request.

use crate::cache_url::CacheUrl;
use crate::ping::{self, Via};

/// Seconds between two advertisements to the same host: 30 days.
pub const INTERVAL: u64 = 30 * 24 * 3600;

/// Seconds allowed for one advertisement round trip.
pub const TIMEOUT_MS: u64 = 20_000;
/// Bytes of a reply body that are classified.
pub const BODY_MAX: usize = 2 * 1024;
/// Bytes of a peer's urlfile that are scanned.
pub const READ_MAX: usize = 8 * 1024;

/// Whether to advertise to `host` now: the feature is on, the cache is
/// verified, it is not this cache, and its last advertisement is at least
/// [`INTERVAL`] ago (or never). `host` is the cache's bare host.
#[must_use]
pub fn due(
    advertise: bool,
    verified: bool,
    host: &str,
    advertised: Option<u64>,
    self_url: &CacheUrl,
    now: u64,
) -> bool {
    advertise
        && verified
        && !host.eq_ignore_ascii_case(&self_url.host)
        && advertised.is_none_or(|t| now >= t && now - t >= INTERVAL)
}

/// Whether the cache that answered with this pong name is never advertised
/// to: Skulls (ADR-0021).
#[must_use]
pub fn excluded(pong: &str) -> bool {
    pong.to_ascii_lowercase().contains("skulls")
}

/// Scans a peer's urlfile, one line at a time, for this cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlScan {
    peer: String,
    me: String,
    listed: bool,
    third: bool,
}

impl UrlScan {
    #[must_use]
    pub fn new(peer_host: &str, self_url: &CacheUrl) -> UrlScan {
        UrlScan {
            peer: peer_host.to_ascii_lowercase(),
            me: self_url.host.to_ascii_lowercase(),
            listed: false,
            third: false,
        }
    }

    /// Take one line. Only `u|<url>|...` lines count; the host of the url,
    /// in any case, is compared with the peer's and with this cache's.
    pub fn line(&mut self, line: &[u8]) {
        let Some(rest) = line
            .strip_prefix(b"u|")
            .or_else(|| line.strip_prefix(b"U|"))
        else {
            return;
        };
        let url = rest.split(|&b| b == b'|').next().unwrap_or_default();
        let after = if url.len() >= 7 && url[..7].eq_ignore_ascii_case(b"http://") {
            &url[7..]
        } else if url.len() >= 8 && url[..8].eq_ignore_ascii_case(b"https://") {
            &url[8..]
        } else {
            return;
        };
        let end = after
            .iter()
            .position(|&b| b == b'/' || b == b':')
            .unwrap_or(after.len());
        let host = &after[..end];
        if host.is_empty() {
            return;
        }
        if host.eq_ignore_ascii_case(self.me.as_bytes()) {
            self.listed = true;
        } else if !host.eq_ignore_ascii_case(self.peer.as_bytes()) {
            self.third = true;
        }
    }

    /// Whether a line named this cache's host.
    #[must_use]
    pub fn listed(&self) -> bool {
        self.listed
    }

    /// Whether this cache is known to be missing: the body was read whole,
    /// it lists a cache other than the peer, and not this one.
    #[must_use]
    pub fn absent(&self, whole: bool) -> bool {
        whole && self.third && !self.listed
    }
}

/// How a peer received an advertisement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Ok,
    Warned,
    Failed,
}

impl Class {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Ok => "ok",
            Class::Warned => "warned",
            Class::Failed => "failed",
        }
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Classify a reply: the first body line containing `OK` decides, `Warned`
/// if `WARNING` is on that line too; anything else, or a status other than
/// 200, is `Failed`. Case-sensitive, as Shareaza reads it.
#[must_use]
pub fn classify(status: Option<u16>, body: &[u8]) -> Class {
    if status != Some(200) {
        return Class::Failed;
    }
    match body
        .split(|&b| b == b'\r' || b == b'\n')
        .find(|l| contains(l, b"OK"))
    {
        Some(l) if contains(l, b"WARNING") => Class::Warned,
        Some(_) => Class::Ok,
        None => Class::Failed,
    }
}

/// The query of an advertisement for this cache, in the update form
/// (Shareaza wiki, "Pinging Caches"; ADR-0021).
#[must_use]
pub fn query(self_url: &CacheUrl) -> String {
    let enc = |s: &str| -> String { form_urlencoded::byte_serialize(s.as_bytes()).collect() };
    format!(
        "update=1&url={}&net=gnutella2&cache=1&client=TEST&version={}",
        enc(&self_url.url),
        enc(&crate::product_id())
    )
}

/// How to send the advertisement to the cache at `url`.
#[must_use]
pub fn via(url: &str, self_url: &CacheUrl) -> Via {
    ping::via_query(url, &query(self_url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_url::normalize;
    use crate::ping::Via;

    const NOW: u64 = 1_800_000_000;

    fn me() -> CacheUrl {
        normalize("http://gwc.example.org/").unwrap()
    }

    #[test]
    fn due_needs_flag_verified_other_host_and_30_days() {
        let me = me();
        assert!(due(true, true, "peer.example.net", None, &me, NOW));
        assert!(!due(false, true, "peer.example.net", None, &me, NOW));
        assert!(!due(true, false, "peer.example.net", None, &me, NOW));
        assert!(!due(true, true, "gwc.example.org", None, &me, NOW));
        assert!(!due(true, true, "GWC.example.org", None, &me, NOW));
        let recent = Some(NOW - INTERVAL + 1);
        assert!(!due(true, true, "peer.example.net", recent, &me, NOW));
        let old = Some(NOW - INTERVAL);
        assert!(due(true, true, "peer.example.net", old, &me, NOW));
        let future = Some(NOW + 5);
        assert!(!due(true, true, "peer.example.net", future, &me, NOW));
    }

    #[test]
    fn skulls_is_excluded_by_pong_name() {
        assert!(excluded("Skulls 0.3.6"));
        assert!(excluded("skulls 0.3.6-catflap"));
        assert!(excluded("SKULLS"));
        assert!(!excluded("Beacon Cache II 0.8.0.1"));
        assert!(!excluded("Cachechu 1.6"));
    }

    #[test]
    fn a_url_line_naming_this_host_in_any_form_is_listed() {
        for l in [
            "u|http://gwc.example.org/|5",
            "U|HTTPS://GWC.EXAMPLE.ORG:8080/x|5",
            "u|http://Gwc.Example.Org/other.php",
        ] {
            let mut s = UrlScan::new("peer.example.net", &me());
            s.line(l.as_bytes());
            assert!(s.listed(), "{l}");
        }
    }

    #[test]
    fn absence_needs_a_whole_body_and_a_third_cache() {
        let mut s = UrlScan::new("peer.example.net", &me());
        assert!(!s.absent(true));
        s.line(b"u|http://peer.example.net/gwc/|5");
        assert!(!s.absent(true));
        s.line(b"u|http://third.example.net/|5");
        assert!(s.absent(true));
        assert!(!s.absent(false));
        s.line(b"u|http://gwc.example.org/|5");
        assert!(!s.absent(true));
    }

    #[test]
    fn lines_without_u_kind_are_ignored() {
        let mut s = UrlScan::new("peer.example.net", &me());
        s.line(b"i|pong|x|gnutella2");
        s.line(b"h|1.2.3.4:6346|5");
        s.line(b"garbage|http://third.example.net/");
        s.line(b"u");
        s.line(b"u|ftp://third.example.net/");
        s.line(&[0xff, 0xfe]);
        assert!(!s.absent(true));
        assert!(!s.listed());
    }

    #[test]
    fn classify_reads_body_lines_of_a_200_only() {
        let c = |st, b: &[u8]| classify(st, b);
        assert_eq!(c(Some(200), b"i|update|OK\r\n"), Class::Ok);
        assert_eq!(
            c(Some(200), b"I|update|OK|WARNING|Blocked URL\n"),
            Class::Warned
        );
        assert_eq!(
            c(Some(200), b"ERROR: Client returned too early\n"),
            Class::Failed
        );
        assert_eq!(c(Some(404), b"OK\n"), Class::Failed);
        assert_eq!(c(None, b""), Class::Failed);
        assert_eq!(
            c(Some(200), b"I|update|WARNING|Rejected URL\n"),
            Class::Failed
        );
        assert_eq!(c(Some(200), b"x\r\ni|update|OK\r\n"), Class::Ok);
        assert_eq!(Class::Ok.as_str(), "ok");
        assert_eq!(Class::Warned.as_str(), "warned");
        assert_eq!(Class::Failed.as_str(), "failed");
    }

    #[test]
    fn the_request_names_this_cache() {
        let me = me();
        let q = query(&me);
        assert!(q.starts_with(
            "update=1&url=http%3A%2F%2Fgwc.example.org%2F&net=gnutella2&cache=1&client=TEST&version=gwc-edge"
        ));
        match via("http://peer.example.net/gwc.php", &me) {
            Via::Fetch(u) => assert!(u.ends_with(&q)),
            v @ Via::Socket { .. } => panic!("{v:?}"),
        }
        assert!(matches!(
            via("http://1.2.3.4:8080/", &me),
            Via::Socket { port: 8080, .. }
        ));
    }

    #[test]
    fn migration_last_advertised_check() {
        let sql = include_str!("../../../migrations/0003_last_advertised.sql");
        assert!(sql.contains(
            "last_advertised INTEGER CHECK (last_advertised IS NULL OR last_advertised >= 0)"
        ));
    }
}
