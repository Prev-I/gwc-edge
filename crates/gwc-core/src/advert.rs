//! Advertising this cache to verified caches (ADR-0021).
//!
//! The pure rules: which caches get an advertisement, how a peer's urlfile is
//! scanned for this cache, how its reply is classified, and the request.

use crate::cache_url::CacheUrl;
use crate::ping::{self, Via};

/// Seconds between two advertisements to the same host: 30 days.
pub const INTERVAL: u64 = 30 * 24 * 3600;

/// Milliseconds allowed for one advertisement round trip.
pub const TIMEOUT_MS: u64 = 20_000;
/// Bytes of a reply body that are classified.
pub const BODY_MAX: usize = 2 * 1024;
/// Bytes read from the socket for one advertisement reply, headers included.
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

/// Whether the cache whose pong name starts with `Skulls`, in any case, is
/// never advertised to (ADR-0021).
#[must_use]
pub fn excluded(pong: &str) -> bool {
    pong.as_bytes()
        .get(..6)
        .is_some_and(|p| p.eq_ignore_ascii_case(b"skulls"))
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

/// Watches an advertisement reply as it arrives, so the read can stop as
/// soon as its [`Class`] is known. Feed it the whole buffer each time, as
/// [`ping::Reader`]: cursors that only move forward keep the total work
/// linear in the reply's length, however it is trickled (the cron has 10 ms
/// of CPU).
///
/// The class is the one [`classify`] gives the first [`BODY_MAX`] body bytes
/// of the complete reply. It is decided at once by a status line that is not
/// HTTP/1.x (at once), or by a status other than 200 (once the header block
/// has ended); otherwise by the first complete body
/// line containing `OK`, or once the body reaches [`BODY_MAX`] bytes or its
/// `Content-Length` (the last line then counts as complete), or by
/// [`ReplyWatch::close`]. Undecided, it is `Failed`: a timeout or a cut read
/// cannot vouch for the peer.
#[derive(Debug, Clone, Default)]
pub struct ReplyWatch {
    /// Where the body starts, once the header block is found, and where
    /// the classified part of it ends (`BODY_MAX`, or the `Content-Length`
    /// if shorter), absolute.
    body: Option<(usize, usize)>,
    /// How far the header search has got.
    searched: usize,
    /// Whether the status line has been checked.
    status_seen: bool,
    /// Start of the first body line not yet taken, and how far the line-end
    /// search has got (both absolute).
    line_start: usize,
    scanned: usize,
    decided: Option<Class>,
    /// Bytes looked at so far: the measure of the work done.
    examined: usize,
}

/// The class a body line decides, if it contains `OK`.
fn line_class(line: &[u8]) -> Option<Class> {
    contains(line, b"OK").then(|| {
        if contains(line, b"WARNING") {
            Class::Warned
        } else {
            Class::Ok
        }
    })
}

impl ReplyWatch {
    /// A watch for a raw HTTP/1.x reply, header block included (a socket).
    #[must_use]
    pub fn socket() -> ReplyWatch {
        ReplyWatch::default()
    }

    /// A watch for a body alone, whose status is already known (`fetch()`,
    /// which delimits the body itself).
    #[must_use]
    pub fn body(status: u16) -> ReplyWatch {
        ReplyWatch {
            body: Some((0, BODY_MAX)),
            status_seen: true,
            decided: (status != 200).then_some(Class::Failed),
            ..ReplyWatch::default()
        }
    }

    /// Read on from where the last call stopped.
    pub fn feed(&mut self, bytes: &[u8]) {
        if self.decided.is_some() {
            return;
        }
        let Some((start, cap)) = self.body.or_else(|| self.head(bytes)) else {
            return;
        };
        if self.decided.is_some() {
            return;
        }
        let end = bytes.len().min(cap);
        let from = self.scanned.max(start);
        self.examined += end.saturating_sub(from);
        for i in from..end {
            if bytes[i] == b'\r' || bytes[i] == b'\n' {
                let line = &bytes[self.line_start.max(start)..i];
                self.examined += line.len();
                self.line_start = i + 1;
                if let Some(class) = line_class(line) {
                    self.scanned = i + 1;
                    self.decided = Some(class);
                    return;
                }
            }
        }
        self.scanned = self.scanned.max(end);
        // The classified part is all here: its last line is complete.
        if bytes.len() >= cap {
            self.tail(bytes, cap);
        }
    }

    /// The reply is over (the peer closed, or the body stream ended): its
    /// last line counts as complete.
    pub fn close(&mut self, bytes: &[u8]) {
        self.feed(bytes);
        if self.decided.is_none() {
            match self.body {
                Some((_, cap)) => self.tail(bytes, bytes.len().min(cap)),
                None => self.decided = Some(Class::Failed),
            }
        }
    }

    /// Whether reading can stop: the class is known.
    #[must_use]
    pub fn decided(&self) -> bool {
        self.decided.is_some()
    }

    /// The class: the decided one, or `Failed`.
    #[must_use]
    pub fn class(&self) -> Class {
        self.decided.unwrap_or(Class::Failed)
    }

    /// Decide by the last line, up to `end`, known complete.
    fn tail(&mut self, bytes: &[u8], end: usize) {
        let Some((start, _)) = self.body else { return };
        let line = &bytes[self.line_start.max(start).min(end)..end];
        self.examined += line.len();
        self.decided = Some(line_class(line).unwrap_or(Class::Failed));
    }

    /// Look for the header block, deciding `Failed` at the first sign that
    /// the reply is not HTTP/1.x or not a 200. Returns the body's bounds
    /// once found.
    fn head(&mut self, bytes: &[u8]) -> Option<(usize, usize)> {
        const PREFIX: &[u8] = b"HTTP/1.";
        let n = bytes.len().min(PREFIX.len());
        if bytes[..n] != PREFIX[..n] {
            self.decided = Some(Class::Failed);
            return None;
        }
        let from = self.searched.saturating_sub(3);
        self.examined += bytes.len().saturating_sub(from);
        if !self.status_seen
            && let Some(nl) = bytes[from..].iter().position(|&b| b == b'\n')
        {
            self.status_seen = true;
            self.examined += from + nl;
            if ping::status_and_length(&bytes[..=from + nl]).is_none() {
                self.decided = Some(Class::Failed);
                return None;
            }
        }
        let Some(start) = ping::header_end(bytes, from) else {
            self.searched = bytes.len();
            return None;
        };
        self.examined += start;
        let Some((200, length, _)) = ping::status_and_length(&bytes[..start]) else {
            self.decided = Some(Class::Failed);
            return None;
        };
        let cap = start + length.map_or(BODY_MAX, |l| l.min(BODY_MAX));
        self.body = Some((start, cap));
        self.line_start = start;
        self.scanned = start;
        Some((start, cap))
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
        assert!(!excluded("NotSkulls 1.0"));
        assert!(!excluded("Beacon Cache II (skulls-compatible)"));
        assert!(!excluded("Skul"));
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

    const OK_200: &str = "HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\n";

    /// Feed `reply` one byte at a time, as a socket might deliver it.
    fn trickle(reply: &[u8]) -> ReplyWatch {
        let mut watch = ReplyWatch::socket();
        for end in 1..=reply.len() {
            watch.feed(&reply[..end]);
        }
        watch
    }

    #[test]
    fn a_trickled_reply_is_watched_in_linear_work() {
        // A peer padding its headers, then a body with no OK, one byte at a
        // time up to the socket's read limit: each feed looks at what is new.
        let mut reply = String::from("HTTP/1.0 200 OK\r\n");
        reply += &"X-Pad: y\r\n".repeat(400);
        reply += "\r\n";
        reply += &"i|update|nothing\r\n".repeat(READ_MAX / 18);
        let bytes = &reply.as_bytes()[..READ_MAX];
        let mut watch = ReplyWatch::socket();
        for end in 1..=bytes.len() {
            watch.feed(&bytes[..end]);
        }
        assert!(watch.examined <= 3 * READ_MAX, "{}", watch.examined);
        assert!(watch.decided(), "{BODY_MAX} body bytes decide");
        assert_eq!(watch.class(), Class::Failed);
    }

    #[test]
    fn a_reached_content_length_ends_the_last_line() {
        let reply = "HTTP/1.0 200 OK\r\nContent-Length: 11\r\n\r\ni|update|OK";
        let watch = trickle(reply.as_bytes());
        assert!(watch.decided());
        assert_eq!(watch.class(), Class::Ok);
        // Short of its length, the line may still be growing.
        let mut watch = ReplyWatch::socket();
        watch.feed(&reply.as_bytes()[..reply.len() - 1]);
        assert!(!watch.decided());
        assert_eq!(watch.class(), Class::Failed, "incomplete");
    }

    #[test]
    fn a_reply_that_is_not_http_is_decided_at_once() {
        let mut watch = ReplyWatch::socket();
        watch.feed(b"G");
        assert!(watch.decided());
        assert_eq!(watch.class(), Class::Failed);
        // A status line that starts right but is not one, before the
        // header block ends.
        let mut watch = ReplyWatch::socket();
        watch.feed(b"HTTP/1.0 2x0 nope\r\nX-A: b");
        assert!(watch.decided());
        assert_eq!(watch.class(), Class::Failed);
        // A good status line is not decided early.
        let mut watch = ReplyWatch::socket();
        watch.feed(b"HTTP/1.0 200 OK\r\nX-A: b");
        assert!(!watch.decided());
        let mut watch = ReplyWatch::socket();
        watch.feed(b"HTTP/1.");
        assert!(!watch.decided());
    }

    #[test]
    fn a_status_other_than_200_fails_when_the_headers_end() {
        let mut watch = ReplyWatch::socket();
        watch.feed(b"HTTP/1.0 404 Not Found\r\n\r\n");
        assert!(watch.decided());
        assert_eq!(watch.class(), Class::Failed);
        let watch = ReplyWatch::body(302);
        assert!(watch.decided());
        assert_eq!(watch.class(), Class::Failed);
        assert!(!ReplyWatch::body(200).decided());
    }

    #[test]
    fn ok_past_the_classified_bytes_fails() {
        let reply = format!("{OK_200}{}\nOK\n", "x".repeat(BODY_MAX));
        let watch = trickle(reply.as_bytes());
        assert!(watch.decided());
        assert_eq!(watch.class(), Class::Failed);
        // The fetch path, with the body alone.
        let body = format!("{}\ni|update|OK\n", "x".repeat(BODY_MAX - 4));
        let mut watch = ReplyWatch::body(200);
        watch.feed(body.as_bytes());
        assert!(watch.decided());
        assert_eq!(watch.class(), Class::Failed);
    }

    #[test]
    fn a_complete_ok_line_decides_and_warning_on_it_warns() {
        let mut watch = ReplyWatch::body(200);
        watch.feed(b"i|update|WARNING|Rejected|OK");
        assert!(!watch.decided(), "the line may still grow");
        watch.feed(b"i|update|WARNING|Rejected|OK\r");
        assert!(watch.decided());
        assert_eq!(watch.class(), Class::Warned);
        // A later WARNING line changes nothing.
        let reply = format!("{OK_200}i|update|OK\r\ni|WARNING|x\r\n");
        let watch = trickle(reply.as_bytes());
        assert_eq!(watch.class(), Class::Ok);
    }

    #[test]
    fn a_closed_reply_ends_its_last_line() {
        let reply = format!("{OK_200}i|update|OK");
        let mut watch = trickle(reply.as_bytes());
        assert!(!watch.decided());
        watch.close(reply.as_bytes());
        assert!(watch.decided());
        assert_eq!(watch.class(), Class::Ok);
        // Closed before the header block ended.
        let mut watch = ReplyWatch::socket();
        watch.close(b"HTTP/1.0 200 OK\r\n");
        assert_eq!(watch.class(), Class::Failed);
    }

    #[test]
    fn the_watch_agrees_with_classify_on_complete_replies() {
        let bodies = [
            "",
            "i|update|OK\r\n",
            "i|update|OK",
            "i|update|WARNING|x|OK\n",
            "i|pong|x\ni|update|ERROR\n",
            "\r\n\r\nOK",
            "i|nothing\nWARNING\nOK WARNING\n",
            "ok\nOk\n",
        ];
        let long = format!("{}OK\n", "y".repeat(BODY_MAX - 2));
        let longer = format!("{}OK\n", "y".repeat(BODY_MAX - 1));
        for body in bodies
            .iter()
            .copied()
            .chain([long.as_str(), longer.as_str()])
        {
            for status in ["200 OK", "500 Oops"] {
                let reply = format!("HTTP/1.0 {status}\r\n\r\n{body}");
                let (code, b) = ping::http_reply(reply.as_bytes()).unwrap();
                let want = classify(Some(code), &b[..b.len().min(BODY_MAX)]);
                let mut whole = ReplyWatch::socket();
                whole.feed(reply.as_bytes());
                whole.close(reply.as_bytes());
                assert_eq!(whole.class(), want, "{reply:?}");
                let mut slow = trickle(reply.as_bytes());
                slow.close(reply.as_bytes());
                assert_eq!(slow.class(), want, "trickled {reply:?}");
                let mut fetched = ReplyWatch::body(code);
                fetched.feed(body.as_bytes());
                fetched.close(body.as_bytes());
                assert_eq!(fetched.class(), want, "fetched {body:?}");
            }
        }
    }

    #[test]
    fn migration_last_advertised_check() {
        let sql = include_str!("../../../migrations/0003_last_advertised.sql");
        assert!(sql.contains(
            "last_advertised INTEGER CHECK (last_advertised IS NULL OR last_advertised >= 0)"
        ));
    }
}
