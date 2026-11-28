//! Pinging another cache (ADR-0012): how to reach it, and what counts as a
//! pong.
//!
//! `fetch()` cannot reach a host given as an IP address, and a non-standard
//! port is not reliably honoured, so only a DNS name on port 80 is fetched.
//! Every other cache is pinged with plain HTTP/1.0 over a TCP socket, which
//! does not follow redirects.

use std::net::Ipv4Addr;

use crate::advert::{self, UrlScan};

/// How to send a ping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    /// `fetch()` this URL.
    Fetch(String),
    /// Connect to `host:port` and send `request`.
    Socket {
        host: String,
        port: u16,
        request: String,
    },
}

/// The query a ping sends, as caches ping each other: `client=TEST` and the
/// cache's name as the version (Shareaza wiki, "Pinging Caches"). It carries
/// a urlfile get, as Shareaza's own G2 requests do, because some caches
/// answer a ping without a get in GWC1 form (ADR-0023).
#[must_use]
pub fn query() -> String {
    let name: String = form_urlencoded::byte_serialize(crate::product_id().as_bytes()).collect();
    format!("ping=1&get=1&urlfile=1&net=gnutella2&client=TEST&version={name}")
}

/// How to ping the cache at `url`, a URL normalised by `cache_url`.
#[must_use]
pub fn via(url: &str) -> Via {
    via_query(url, &query())
}

/// As [`via`], sending `query` instead of the ping query.
#[must_use]
pub fn via_query(url: &str, query: &str) -> Via {
    let (host, port, request) = socket_request_with(url, query);
    if port == 80 && host.parse::<Ipv4Addr>().is_err() {
        return Via::Fetch(format!("{url}?{query}"));
    }
    Via::Socket {
        host,
        port,
        request,
    }
}

/// The host, port and HTTP/1.0 request that ping `url` over a socket.
#[must_use]
pub fn socket_request(url: &str) -> (String, u16, String) {
    socket_request_with(url, &query())
}

fn socket_request_with(url: &str, query: &str) -> (String, u16, String) {
    let rest = url.strip_prefix("http://").unwrap_or(url);
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().unwrap_or(80)),
        None => (authority, 80),
    };
    let request = format!(
        "GET /{path}?{} HTTP/1.0\r\n\
         Host: {authority}\r\n\
         User-Agent: {}\r\n\
         Connection: close\r\n\
         \r\n",
        query,
        crate::product_id()
    );
    (host.to_owned(), port, request)
}

/// `url` with its host and port replaced by `at`, plus the ping query: for
/// `TEST_DIAL`, which aims a fetched ping at a loopback test peer.
#[must_use]
pub fn fetch_url_at(url: &str, at: std::net::SocketAddrV4) -> String {
    let rest = url.strip_prefix("http://").unwrap_or(url);
    let path = rest.split_once('/').map_or("", |(_, p)| p);
    format!("http://{at}/{path}?{}", query())
}

/// Status and body of a complete HTTP/1.x response, if `bytes` holds its
/// header block.
#[must_use]
pub fn http_reply(bytes: &[u8]) -> Option<(u16, &[u8])> {
    let end = header_end(bytes, 0)?;
    let (status, ..) = status_and_length(&bytes[..end])?;
    Some((status, &bytes[end..]))
}

/// Index just past the header block (`\r\n\r\n` or `\n\n`, whichever comes
/// first), searching from `from`, which may lag the buffer's end by up to 3
/// bytes so a terminator split across reads is found.
fn header_end(bytes: &[u8], from: usize) -> Option<usize> {
    let crlf = bytes[from..]
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| from + i + 4);
    let lf = bytes[from..]
        .windows(2)
        .position(|w| w == b"\n\n")
        .map(|i| from + i + 2);
    match (crlf, lf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The status code and `Content-Length` of a header block, and whether it has
/// a `Transfer-Encoding` header, or `None` if its first line is not an
/// HTTP/1.x status line.
fn status_and_length(head: &[u8]) -> Option<(u16, Option<usize>, bool)> {
    let text = std::str::from_utf8(head).ok()?;
    let mut lines = text.lines();
    let code = lines.next()?.strip_prefix("HTTP/1.")?.get(2..5)?;
    if !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut length = None;
    let mut chunked = false;
    for (name, value) in lines.filter_map(|l| l.split_once(':')) {
        let name = name.trim();
        if length.is_none() && name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().ok();
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = true;
        }
    }
    Some((code.parse().ok()?, length, chunked))
}

/// What a ping reply says, once it can be judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// A pong for Gnutella2, with the cache's name.
    Pong(String),
    NoPong,
}

impl Verdict {
    /// The cache's name, if it answered a pong.
    #[must_use]
    pub fn pong(self) -> Option<String> {
        match self {
            Verdict::Pong(name) => Some(name),
            Verdict::NoPong => None,
        }
    }

    fn of(pong: Option<String>) -> Verdict {
        pong.map_or(Verdict::NoPong, Verdict::Pong)
    }
}

/// Reads a ping reply as it arrives, and judges it as early as it can.
///
/// Feed it the whole buffer received so far each time it grows. Each byte is
/// examined a bounded number of times and each complete line parsed once, so
/// a peer that sends one byte at a time costs linear work, not quadratic (the
/// cron has 10 ms of CPU). The reply is judged:
/// - at once, when the status line is malformed or the status is not 200;
/// - on the first complete pong line (ended by CR or LF), without waiting for
///   the peer to close: many caches keep the connection open;
/// - when the body reaches its `Content-Length` and a line end follows its
///   last line; if anything else follows, the length cut the line short and
///   there is no pong;
/// - otherwise when the peer closes (see [`Reader::close`]), or at the
///   deadline (see [`Reader::at_deadline`]).
///
/// Half a pong line is never taken for one.
///
/// With a scan ([`Reader::scanning`], ADR-0021) it reads on after the pong:
/// every complete body line goes to the scan, before and after the pong,
/// until [`Reader::settled`]. The verdict stays the one fixed at the pong
/// line. The scan sees each line once: lines are cut by one cursor
/// (`line_start`, `scanned`) that only moves forward across feeds. A last
/// line with no line end reaches the scan only when the body is known to
/// be over: at its `Content-Length`, or in [`Reader::close`]. So call
/// `close` before asking for [`Reader::listing`] at `Closed` or `StreamEnd`.
#[derive(Debug, Clone, Default)]
pub struct Reader {
    /// Where the header block ends, once found, with the status and length.
    body: Option<Body>,
    /// How far the header search has got.
    searched: usize,
    /// Start of the first line not yet parsed, and how far the line-end
    /// search has got (both absolute).
    line_start: usize,
    scanned: usize,
    verdict: Option<Verdict>,
    /// The verdict if nothing more arrives: a body that has reached its
    /// `Content-Length` with nothing after it yet.
    provisional: Option<Verdict>,
    /// Bytes looked at so far, header search and line parsing included: the
    /// measure of the work done, which stays linear in the reply's length.
    examined: usize,
    /// The urlfile scan, if this ping also looks for this cache.
    scan: Option<UrlScan>,
    /// Whether the body has reached its `Content-Length`, which delimits it
    /// (no `Transfer-Encoding`), and the length does not cut the last line
    /// short (see `advance`): the body is whole, and its last line complete.
    /// Bytes that arrive later cannot undo it.
    reached: bool,
    /// Whether the last line, with no line end, went to the scan.
    tail_scanned: bool,
}

#[derive(Debug, Clone, Copy)]
struct Body {
    start: usize,
    status: u16,
    length: Option<usize>,
    /// A `Transfer-Encoding` header was present: the body is never known
    /// to be whole.
    chunked: bool,
}

/// How a ping read ended, for [`Reader::listing`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The peer closed the socket.
    Closed,
    /// `fetch()`'s body stream ended.
    StreamEnd,
    /// The body ran past the size limit.
    Cut,
    /// The deadline passed, or the connection failed.
    Deadline,
}

/// Whether the pinged cache lists this one (ADR-0021).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listing {
    Listed,
    /// Read whole, listing another cache and not this one.
    Absent,
    Unknown,
}

impl Reader {
    /// A reader for a raw HTTP/1.x reply, header block included (a socket).
    #[must_use]
    pub fn new() -> Reader {
        Reader::default()
    }

    /// A reader for a body alone, whose status is already known (`fetch()`).
    /// `fetch()` delimits the body itself, so no length is needed.
    #[must_use]
    pub fn body(status: u16) -> Reader {
        Reader {
            body: Some(Body {
                start: 0,
                status,
                length: None,
                chunked: false,
            }),
            ..Reader::default()
        }
    }

    /// The same reader, also scanning the urlfile for this cache.
    #[must_use]
    pub fn scanning(mut self, scan: UrlScan) -> Reader {
        self.scan = Some(scan);
        self
    }

    /// Whether reading can stop. Without a scan, once there is a verdict.
    /// With one, once the verdict is no pong, or a pong that is excluded
    /// from advertisement (Skulls), whose urlfile listed this cache, or
    /// whose body has reached its `Content-Length` (reading on can change
    /// nothing).
    #[must_use]
    pub fn settled(&self) -> bool {
        match (&self.verdict, &self.scan) {
            (None, _) => false,
            (Some(Verdict::NoPong), _) | (Some(Verdict::Pong(_)), None) => true,
            (Some(Verdict::Pong(name)), Some(scan)) => {
                self.reached || scan.listed() || advert::excluded(name)
            }
        }
    }

    /// The verdict fixed so far, if any.
    #[must_use]
    pub fn verdict(&self) -> Option<Verdict> {
        self.verdict.clone()
    }

    /// Whether the pinged cache lists this one, once the read ended by
    /// `end`. Absence needs the body read whole: `fetch()`'s stream ended,
    /// or its `Content-Length` was reached however the read ended, or the
    /// peer closed with neither `Content-Length` nor `Transfer-Encoding`.
    /// Always unknown without a scan, without a pong, or for an excluded
    /// pong.
    #[must_use]
    pub fn listing(&self, end: End) -> Listing {
        let Some(scan) = &self.scan else {
            return Listing::Unknown;
        };
        match self.verdict.as_ref().or(self.provisional.as_ref()) {
            Some(Verdict::Pong(name)) if !advert::excluded(name) => {
                if scan.listed() {
                    Listing::Listed
                } else if scan.absent(self.whole(end)) {
                    Listing::Absent
                } else {
                    Listing::Unknown
                }
            }
            _ => Listing::Unknown,
        }
    }

    fn whole(&self, end: End) -> bool {
        self.reached
            || match end {
                End::StreamEnd => true,
                End::Closed => self.body.is_some_and(|b| b.length.is_none() && !b.chunked),
                End::Cut | End::Deadline => false,
            }
    }

    /// The verdict, once the bytes so far settle it; `None` while more bytes
    /// could change it. With a scan, also scans the lines that have arrived.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<Verdict> {
        if !self.settled() {
            self.advance(bytes);
        }
        self.verdict.clone()
    }

    /// The verdict once the peer has closed: whatever arrived is the whole
    /// reply, and its last line counts as complete, unless the body is
    /// shorter than its `Content-Length`. (A body of exactly that length,
    /// then a close, ends its last line too.) That last line goes to the
    /// scan, and the verdict is fixed.
    pub fn close(&mut self, bytes: &[u8]) -> Option<String> {
        let fed = self.feed(bytes);
        let last = self.body.and_then(|body| {
            let end = match body.length {
                Some(n) if bytes.len() < body.start + n => return None,
                Some(n) => body.start + n,
                None => bytes.len(),
            };
            Some(&bytes[self.line_start.min(end)..end])
        });
        // With a length, `advance` has handed the last line over already,
        // if the length did not cut it short.
        if let Some(line) = last
            && self.body.is_some_and(|b| b.length.is_none())
            && !self.settled()
        {
            self.scan_tail(line);
        }
        if let Some(verdict) = fed {
            return verdict.pong();
        }
        let verdict = Verdict::of(pong_line(last?));
        self.verdict = Some(verdict.clone());
        verdict.pong()
    }

    /// Hand the last line, known complete with no line end, to the scan,
    /// once.
    fn scan_tail(&mut self, line: &[u8]) {
        if let Some(scan) = self.scan.as_mut()
            && !self.tail_scanned
            && !line.is_empty()
        {
            self.examined += line.len();
            scan.line(line);
            self.tail_scanned = true;
        }
    }

    /// The verdict at the deadline, when the peer neither closed nor sent
    /// enough to settle the reply: nothing more can arrive, so a body that
    /// reached its `Content-Length` counts as complete. Anything else is no
    /// pong: without a length, the last line may still have been arriving.
    #[must_use]
    pub fn at_deadline(&self) -> Option<String> {
        self.verdict
            .clone()
            .or_else(|| self.provisional.clone())
            .and_then(Verdict::pong)
    }

    /// Bytes looked at so far (see the field).
    #[must_use]
    pub fn examined(&self) -> usize {
        self.examined
    }

    /// Read on from where the last call stopped, fixing the verdict when the
    /// bytes settle it. Stops as soon as the reader is settled.
    fn advance(&mut self, bytes: &[u8]) {
        let body = if let Some(body) = self.body {
            body
        } else {
            let from = self.searched.saturating_sub(3);
            self.examined += bytes.len().saturating_sub(from);
            let Some(end) = header_end(bytes, from) else {
                self.searched = bytes.len();
                return;
            };
            let Some((status, length, chunked)) = status_and_length(&bytes[..end]) else {
                self.verdict = Some(Verdict::NoPong);
                return;
            };
            let body = Body {
                start: end,
                status,
                length,
                chunked,
            };
            self.body = Some(body);
            self.line_start = end;
            self.scanned = end;
            body
        };
        if body.status != 200 {
            self.verdict = Some(Verdict::NoPong);
            return;
        }
        let limit = body
            .length
            .map_or(bytes.len(), |n| bytes.len().min(body.start + n));
        while self.scanned < limit {
            let i = self.scanned;
            self.scanned += 1;
            self.examined += 1;
            if matches!(bytes[i], b'\r' | b'\n') {
                let line = &bytes[self.line_start..i];
                self.examined += line.len();
                self.line_start = i + 1;
                if let Some(scan) = self.scan.as_mut() {
                    scan.line(line);
                }
                if self.verdict.is_none()
                    && let Some(name) = pong_line(line)
                {
                    self.verdict = Some(Verdict::Pong(name));
                }
                if self.settled() {
                    return;
                }
            }
        }
        let Some(n) = body.length else {
            return;
        };
        let end = body.start + n;
        if bytes.len() < end {
            return;
        }
        let last = &bytes[self.line_start.min(end)..end];
        // The body ends at the length, unless `Transfer-Encoding` overrides
        // it, or the length cuts the last line short (anything but a line
        // end follows a non-empty last line). Bytes past a length that ends
        // on a line boundary cut nothing. A settled reader reads no further,
        // so what arrives after the length is first reached cannot undo it:
        // a read that ends exactly at a wrong length takes a cut last line
        // as complete, accepted (ADR-0021, controller ruling).
        let cut = !last.is_empty() && bytes.get(end).is_some_and(|b| !matches!(b, b'\r' | b'\n'));
        if !self.reached && !body.chunked && !cut {
            self.reached = true;
            self.scan_tail(last);
        }
        if self.verdict.is_some() {
            return;
        }
        if last.is_empty() {
            // Every line examined, no pong among them.
            self.verdict = Some(Verdict::NoPong);
            return;
        }
        // For the verdict, it is complete if a line end follows it, cut
        // short if anything else does, undecided until more arrives or the
        // peer closes.
        let verdict = Verdict::of(pong_line(last));
        match bytes.get(end) {
            Some(b'\r' | b'\n') => self.verdict = Some(verdict),
            Some(_) => self.verdict = Some(Verdict::NoPong),
            None => self.provisional = Some(verdict),
        }
    }
}

/// Longest pong name stored.
pub const MAX_PONG: usize = 64;

/// The cache's name, if `body` holds a pong for Gnutella2: a line whose first
/// two fields are `i` and `pong`, and whose networks field, when present,
/// lists `gnutella2` (split on `-`).
#[must_use]
pub fn pong(status: u16, body: &[u8]) -> Option<String> {
    if status != 200 {
        return None;
    }
    body.split(|b| matches!(b, b'\r' | b'\n'))
        .find_map(pong_line)
}

/// [`pong`] for one line.
fn pong_line(line: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(line);
    let fields: Vec<&str> = text.split('|').map(str::trim).collect();
    let [kind, word, rest @ ..] = fields.as_slice() else {
        return None;
    };
    if !kind.eq_ignore_ascii_case("i") || !word.eq_ignore_ascii_case("pong") {
        return None;
    }
    let nets = rest.get(1).copied().unwrap_or_default();
    if !nets.is_empty()
        && !nets
            .split('-')
            .any(|n| n.trim().eq_ignore_ascii_case("gnutella2"))
    {
        return None;
    }
    let name = rest.first().copied().unwrap_or_default();
    Some(
        name.chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .take(MAX_PONG)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dns_name_on_port_80_is_fetched() {
        assert_eq!(
            via("http://dkac.trillinux.org/dkac/dkac.php"),
            Via::Fetch(format!(
                "http://dkac.trillinux.org/dkac/dkac.php?{}",
                query()
            ))
        );
        assert!(
            query()
                .starts_with("ping=1&get=1&urlfile=1&net=gnutella2&client=TEST&version=gwc-edge")
        );
        assert!(!query().contains(' '));
    }

    #[test]
    fn the_ping_is_a_g2_urlfile_get() {
        // Cachechu answers a request with neither `get` nor `update` in GWC1
        // form, `PONG <name>`, which is no pong here (ADR-0023). As this
        // cache reads it, the ping is a G2 urlfile query with a ping.
        let q = query();
        assert!(q.split('&').any(|p| p == "get=1"), "{q}");
        assert!(!q.contains("gwcs"), "Cachechu reads `gwcs` as GWC1: {q}");
        let parsed = crate::query::parse(&q).expect("the ping parses");
        assert!(parsed.ping && parsed.urlfile && !parsed.hostfile, "{q}");
        assert_eq!(parsed.net, crate::query::Net::Gnutella2);
        assert_eq!(
            crate::policy::RateKind::of(&parsed),
            Some(crate::policy::RateKind::Urlfile)
        );
    }

    #[test]
    fn gwc2_replies_to_the_get_are_judged_at_their_pong() {
        // What Cachechu 1.6 (`index.php:206-213`, `:413-457`) and Skulls
        // 0.3.6 (`skulls.php:474-497`, then `Get()`) send for the ping.
        let cachechu = format!(
            "{OK}I|pong|Cachechu 1.6|gnutella2\nI|networks|gnutella2\n\
             H|4.3.2.1:6346|60\nU|http://gwc.example.net/|60\nI|access|period|1800\n"
        );
        assert_eq!(early(&cachechu), Some(pong_named("Cachechu 1.6")));
        let skulls = format!(
            "{OK}I|pong|Skulls 0.3.6|gnutella2-mute\r\nH|4.3.2.1:6346|60\r\n\
             U|http://gwc.example.net/|60\r\n"
        );
        assert_eq!(early(&skulls), Some(pong_named("Skulls 0.3.6")));
        // Cachechu joins its networks with `|`: listing `gnutella` first
        // fails here as it fails in Shareaza (`shareaza_model`).
        let g1_first = format!("{OK}I|pong|Cachechu 1.6|gnutella|gnutella2\n");
        assert_eq!(at_close(&g1_first), None);
    }

    #[test]
    fn ip_hosts_and_other_ports_go_over_a_socket() {
        let Via::Socket {
            host,
            port,
            request,
        } = via("http://gwc.example.net:8080/cache.php")
        else {
            panic!("socket expected");
        };
        assert_eq!((host.as_str(), port), ("gwc.example.net", 8080));
        assert!(request.starts_with(&format!("GET /cache.php?{} HTTP/1.0\r\n", query())));
        assert!(request.contains("\r\nHost: gwc.example.net:8080\r\n"));
        assert!(request.contains("\r\nUser-Agent: gwc-edge "));
        assert!(request.ends_with("\r\n\r\n"));

        let Via::Socket {
            host,
            port,
            request,
        } = via("http://8.8.4.4/")
        else {
            panic!("socket expected");
        };
        assert_eq!((host.as_str(), port), ("8.8.4.4", 80));
        assert!(request.starts_with("GET /?ping=1"));
        assert!(request.contains("\r\nHost: 8.8.4.4\r\n"));
    }

    #[test]
    fn a_fetched_cache_can_still_be_pinged_over_a_socket() {
        let (host, port, request) = socket_request("http://cache.example.org/gwc/");
        assert_eq!((host.as_str(), port), ("cache.example.org", 80));
        assert!(request.starts_with("GET /gwc/?ping=1"));
        assert!(request.contains("\r\nHost: cache.example.org\r\n"));
    }

    #[test]
    fn a_fetched_ping_can_be_aimed_at_a_test_peer() {
        assert_eq!(
            fetch_url_at(
                "http://cache.example.org/gwc/x.php",
                "127.0.0.1:18803".parse().unwrap()
            ),
            format!("http://127.0.0.1:18803/gwc/x.php?{}", query())
        );
        assert!(
            fetch_url_at("http://cache.example.org", "127.0.0.1:1".parse().unwrap())
                .starts_with("http://127.0.0.1:1/?ping=1")
        );
    }

    #[test]
    fn http_replies_are_split_into_status_and_body() {
        assert_eq!(
            http_reply(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\ni|pong|x\r\n"),
            Some((200, &b"i|pong|x\r\n"[..]))
        );
        assert_eq!(
            http_reply(b"HTTP/1.0 404 Not Found\n\nnope"),
            Some((404, &b"nope"[..]))
        );
        assert_eq!(http_reply(b"HTTP/1.1 200 OK\r\nPartial: yes\r\n"), None);
        assert_eq!(http_reply(b"GNUTELLA/0.6 200 OK\r\n\r\n"), None);
        assert_eq!(http_reply(b"HTTP/1.1 2x0 OK\r\n\r\n"), None);
        assert_eq!(http_reply(b"HTTP/1.1\r\n\r\n"), None);
    }

    const OK: &str = "HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\n";

    /// Feed `reply` one byte at a time, as a peer trickling it would; the
    /// first verdict, and the reader.
    fn trickle(reply: &[u8]) -> (Option<Verdict>, Reader) {
        let mut r = Reader::new();
        for end in 1..=reply.len() {
            if let Some(v) = r.feed(&reply[..end]) {
                return (Some(v), r);
            }
        }
        (None, r)
    }

    fn early(reply: &str) -> Option<Verdict> {
        let whole = Reader::new().feed(reply.as_bytes());
        assert_eq!(
            trickle(reply.as_bytes()).0,
            whole,
            "chunking must not matter"
        );
        whole
    }

    fn at_close(reply: &str) -> Option<String> {
        Reader::new().close(reply.as_bytes())
    }

    fn pong_named(name: &str) -> Verdict {
        Verdict::Pong(name.into())
    }

    const NO_PONG: Option<Verdict> = Some(Verdict::NoPong);

    #[test]
    fn a_complete_pong_line_settles_the_reply_before_the_peer_closes() {
        let reply = format!("{OK}i|pong|fake-cache 1.0|gnutella2\r\n");
        assert_eq!(early(&reply), Some(pong_named("fake-cache 1.0")));
        // A later line, still arriving, does not matter.
        assert_eq!(
            early(&format!("{reply}u|http://other.exa")),
            Some(pong_named("fake-cache 1.0"))
        );
        // A bare LF ends a line, and so does a bare CR.
        assert_eq!(
            early(&format!("{OK}i|pong|x|gnutella2\n")),
            Some(pong_named("x"))
        );
        assert_eq!(early(&format!("{OK}i|pong|y\r")), Some(pong_named("y")));
        // Other lines first.
        assert_eq!(
            early(&format!("{OK}h|1.2.3.4:6346|5\r\ni|pong|z\r\n")),
            Some(pong_named("z"))
        );
    }

    #[test]
    fn a_partial_line_is_never_taken_for_a_pong() {
        for partial in [
            "i|pong|fake-cache 1.0|gnutella2",
            "i|pong|fake-ca",
            "i|po",
            "",
        ] {
            assert_eq!(early(&format!("{OK}{partial}")), None, "{partial:?}");
        }
        // Each prefix of a full reply, header block included, waits for more.
        let full = format!("{OK}i|pong|fake-cache 1.0|gnutella2\r\n");
        for cut in 0..full.len() - 2 {
            assert_eq!(Reader::new().feed(&full.as_bytes()[..cut]), None, "{cut}");
        }
    }

    #[test]
    fn when_the_peer_closes_the_last_line_counts_as_complete() {
        assert_eq!(
            at_close(&format!("{OK}i|pong|old-cache 2.0")),
            Some("old-cache 2.0".into())
        );
        assert_eq!(
            at_close("HTTP/1.0 200 OK\r\nServ"),
            None,
            "cut in the headers"
        );
        assert_eq!(at_close(""), None);
        assert_eq!(at_close(&format!("{OK}ERROR No network\r\n")), None);
    }

    #[test]
    fn content_length_ends_the_last_line_but_not_more() {
        let body = "i|pong|sized 1.0|gnutella2";
        let head = |n: usize| format!("HTTP/1.1 200 OK\r\nContent-Length: {n}\r\n\r\n");
        let exact = format!("{}{body}", head(body.len()));
        assert_eq!(
            early(&exact),
            None,
            "is the line cut? not known before the close"
        );
        assert_eq!(at_close(&exact), Some("sized 1.0".into()));
        // Held open to the deadline: nothing more can come, so it counts.
        let mut kept_open = Reader::new();
        assert_eq!(kept_open.feed(exact.as_bytes()), None);
        assert_eq!(kept_open.at_deadline(), Some("sized 1.0".into()));
        // Without a length the last line may still have been arriving.
        let mut open = Reader::new();
        assert_eq!(open.feed(format!("{OK}i|pong|x").as_bytes()), None);
        assert_eq!(open.at_deadline(), None);
        assert_eq!(
            early(&format!("{}{body}\r\n", head(body.len()))),
            Some(pong_named("sized 1.0")),
            "a line end past the length ends the line"
        );
        // A complete pong line settles before the declared length is in.
        assert_eq!(
            early(&format!("{}{body}\r\n", head(99_999))),
            Some(pong_named("sized 1.0"))
        );
        let short = format!("{}{body}", head(99));
        assert_eq!(early(&short), None);
        assert_eq!(at_close(&short), None, "truncated");
        // A length that cuts the pong line, with the rest of the line in
        // view: no pong, and no G1-only line passing for G2.
        let g1 = "i|pong|name|gnutella\r\n";
        assert_eq!(early(&format!("{}{g1}", head(11))), NO_PONG);
        // Nothing beyond the length is read.
        assert_eq!(early(&format!("{}x\r\n{body}\r\n", head(3))), NO_PONG);
    }

    #[test]
    fn errors_and_garbage_are_judged_at_once() {
        assert_eq!(early("HTTP/1.1 404 Not Found\r\n\r\n"), NO_PONG);
        assert_eq!(early("HTTP/1.1 302 Found\r\nLocation: x\r\n\r\n"), NO_PONG);
        assert_eq!(early("GNUTELLA/0.6 200 OK\r\n\r\n"), NO_PONG);
        assert_eq!(early("<html>\n\n<body>"), NO_PONG);
        // A 200 whose complete lines hold no pong keeps waiting: the verdict
        // is only "no pong" once the peer closes (or the deadline passes).
        let error = format!("{OK}ERROR No network\r\n");
        assert_eq!(early(&error), None);
        assert_eq!(at_close(&error), None);
        assert_eq!(at_close(&format!("{OK}i|pong|g1 only|gnutella\r\n")), None);
    }

    #[test]
    fn lf_only_headers_end_at_the_first_blank_line() {
        // The body's own CRLF CRLF must not be taken for the header end.
        let reply = "HTTP/1.0 200 OK\nX: y\n\ni|pong|lf\r\n\r\nmore";
        assert_eq!(early(reply), Some(pong_named("lf")));
        assert_eq!(http_reply(reply.as_bytes()).map(|(s, _)| s), Some(200));
    }

    #[test]
    fn a_trickled_reply_costs_linear_work() {
        // 16 KiB sent one byte per read, the worst a hostile cache can do
        // within the ping's size limit: headers that never end, or a body of
        // empty lines.
        let n = 16 * 1024;
        let headers = "X".repeat(n);
        let (verdict, r) = trickle(headers.as_bytes());
        assert_eq!(verdict, None);
        assert!(r.examined() <= 4 * n, "{}", r.examined());
        let lines = format!("{OK}{}", "\n".repeat(n));
        let (verdict, r) = trickle(lines.as_bytes());
        assert_eq!(verdict, None);
        assert!(r.examined() <= 4 * lines.len(), "{}", r.examined());
        let long_line = format!("{OK}{}", "i|".repeat(n / 2));
        let (_, r) = trickle(long_line.as_bytes());
        assert!(r.examined() <= 4 * long_line.len(), "{}", r.examined());
    }

    #[test]
    fn a_fetched_body_is_read_the_same_way() {
        let mut r = Reader::body(200);
        assert_eq!(r.feed(b"i|pong|f"), None);
        assert_eq!(
            r.feed(b"i|pong|fetched|gnutella2\n"),
            Some(pong_named("fetched"))
        );
        assert_eq!(Reader::body(500).feed(b""), NO_PONG);
        assert_eq!(Reader::body(200).close(b"i|pong|end"), Some("end".into()));
    }

    #[test]
    fn a_pong_for_gnutella2_is_recognised() {
        assert_eq!(
            pong(
                200,
                b"I|pong|Beacon Cache 0.1.5D|gnutella-gnutella2\r\nh|1.2.3.4:6346|5\r\n"
            ),
            Some("Beacon Cache 0.1.5D".into())
        );
        assert_eq!(
            pong(200, b"i|pong|dkac 1.0\n"),
            Some("dkac 1.0".into()),
            "no networks field: not held against it"
        );
        assert_eq!(pong(200, b"i|pong\n"), Some(String::new()));
        let long = format!("i|pong|{}\u{7}\n", "x".repeat(100));
        assert_eq!(pong(200, long.as_bytes()), Some("x".repeat(MAX_PONG)));
    }

    #[test]
    fn anything_else_is_not_a_pong() {
        assert_eq!(pong(200, b"i|pong|Skulls|gnutella\n"), None, "G1 only");
        assert_eq!(pong(200, b"ERROR No network\n"), None, "dkac without net=");
        assert_eq!(pong(200, b"PONG old cache\n"), None, "GWC1");
        assert_eq!(pong(200, b""), None);
        assert_eq!(pong(500, b"i|pong|x|gnutella2\n"), None, "not a 200");
        assert_eq!(pong(200, b"<html>i|pong</html>\n"), None);
    }

    // Scan mode (ADR-0021): reading on after the pong for this cache's
    // urlfile line.

    fn scan() -> crate::advert::UrlScan {
        let me = crate::cache_url::normalize("http://gwc.example.org/").unwrap();
        crate::advert::UrlScan::new("peer.example.net", &me)
    }

    fn scanning() -> Reader {
        Reader::new().scanning(scan())
    }

    const THIRD: &str = "u|http://third.example.net/|5\r\n";
    const ME: &str = "u|http://gwc.example.org/|5\r\n";

    #[test]
    fn without_a_scan_the_reader_stops_at_the_pong_as_before() {
        let pong = format!("{OK}i|pong|Peer 1|gnutella2\r\n");
        let reply = format!("{pong}{THIRD}");
        let mut r = Reader::new();
        assert!(!r.settled());
        assert_eq!(r.feed(&pong.as_bytes()[..pong.len() - 3]), None);
        assert!(!r.settled());
        assert_eq!(r.feed(pong.as_bytes()), Some(pong_named("Peer 1")));
        assert!(r.settled());
        assert_eq!(r.close(reply.as_bytes()), Some("Peer 1".into()));
        assert_eq!(r.listing(End::Closed), Listing::Unknown);
        assert_eq!(r.verdict(), Some(pong_named("Peer 1")));
    }

    #[test]
    fn a_scan_reads_on_until_this_cache_is_listed() {
        let pong = format!("{OK}i|pong|Peer 1|gnutella2\r\n");
        let third = format!("{pong}{THIRD}");
        let me = format!("{third}{ME}");
        let reply = format!("{me}u|http://later.example.net/|5\r\n");
        let mut r = scanning();
        assert_eq!(r.feed(pong.as_bytes()), Some(pong_named("Peer 1")));
        assert!(!r.settled(), "this cache not seen yet");
        assert_eq!(r.feed(third.as_bytes()), Some(pong_named("Peer 1")));
        assert!(!r.settled());
        r.feed(me.as_bytes());
        assert!(r.settled());
        r.feed(reply.as_bytes());
        assert_eq!(r.listing(End::Deadline), Listing::Listed);
        assert_eq!(r.at_deadline(), Some("Peer 1".into()));
        // Byte by byte, it settles exactly at the end of this cache's line.
        let mut t = scanning();
        for end in 1..=reply.len() {
            t.feed(&reply.as_bytes()[..end]);
            assert_eq!(t.settled(), end >= me.len() - 1, "{end}");
        }
        assert_eq!(t.listing(End::Cut), Listing::Listed);
        // Listed before the pong: settled only at the pong.
        let early = format!("{OK}{ME}h|1.2.3.4:6346|5\r\ni|pong|P|gnutella2\r\n");
        let mut e = scanning();
        e.feed(&early.as_bytes()[..early.len() - 25]);
        assert!(!e.settled(), "never before the pong");
        e.feed(early.as_bytes());
        assert!(e.settled());
        assert_eq!(e.listing(End::Deadline), Listing::Listed);
    }

    #[test]
    fn a_pong_after_the_url_lines_is_still_the_verdict() {
        let reply = format!("{OK}{THIRD}h|1.2.3.4:6346|5\r\ni|pong|Late 1|gnutella2\r\n");
        let mut r = scanning();
        assert_eq!(r.close(reply.as_bytes()), Some("Late 1".into()));
        assert_eq!(r.verdict(), Some(pong_named("Late 1")));
        assert_eq!(r.listing(End::Closed), Listing::Absent);
    }

    #[test]
    fn only_a_whole_body_shows_absence() {
        let reply = format!("{OK}{THIRD}i|pong|Late 1|gnutella2\r\n");
        let mut r = scanning();
        assert_eq!(r.feed(reply.as_bytes()), Some(pong_named("Late 1")));
        assert_eq!(r.listing(End::Cut), Listing::Unknown);
        assert_eq!(r.listing(End::Deadline), Listing::Unknown);
        assert_eq!(r.listing(End::StreamEnd), Listing::Absent);
        // The fetch path: the stream's end is whole.
        let mut f = Reader::body(200).scanning(scan());
        let body = format!("i|pong|Fetched|gnutella2\n{THIRD}");
        assert_eq!(f.close(body.as_bytes()), Some("Fetched".into()));
        assert_eq!(f.listing(End::StreamEnd), Listing::Absent);
        assert_eq!(f.listing(End::Cut), Listing::Unknown);
    }

    #[test]
    fn the_last_line_counts_at_the_close() {
        // The peer closed: its last line is complete without a line end, as
        // for the verdict.
        let pong = format!("{OK}i|pong|P|gnutella2\r\n");
        let mut listed = scanning();
        listed.close(format!("{pong}u|http://gwc.example.org/").as_bytes());
        assert_eq!(listed.listing(End::Closed), Listing::Listed);
        let mut absent = scanning();
        absent.close(format!("{pong}u|http://third.example.net/").as_bytes());
        assert_eq!(absent.listing(End::Closed), Listing::Absent);
        // Without a close the half line is not taken.
        let mut open = scanning();
        open.feed(format!("{pong}u|http://gwc.example.org/").as_bytes());
        assert_eq!(open.listing(End::Deadline), Listing::Unknown);
        assert!(!open.settled());
    }

    #[test]
    fn content_length_reached_is_whole() {
        let body = format!("i|pong|Sized|gnutella2\r\n{THIRD}");
        let head = |n: usize| format!("HTTP/1.1 200 OK\r\nContent-Length: {n}\r\n\r\n");
        let mut exact = scanning();
        assert_eq!(
            exact.close(format!("{}{body}", head(body.len())).as_bytes()),
            Some("Sized".into())
        );
        assert_eq!(exact.listing(End::Closed), Listing::Absent);
        let mut short = scanning();
        assert_eq!(
            short.close(format!("{}{body}", head(body.len() + 10)).as_bytes()),
            Some("Sized".into()),
            "the pong line was complete; the body was not"
        );
        assert_eq!(short.verdict(), Some(pong_named("Sized")));
        assert_eq!(short.listing(End::Closed), Listing::Unknown);
        // The last line ends at the length, with no line end: it is complete.
        let mine = "i|pong|Sized|gnutella2\r\nu|http://gwc.example.org/";
        let mut kept = scanning();
        kept.feed(format!("{}{mine}", head(mine.len())).as_bytes());
        assert!(kept.settled());
        assert_eq!(kept.listing(End::Deadline), Listing::Listed);
        // Nothing past the length is scanned.
        let past = format!("{}{body}{ME}", head(body.len()));
        let mut beyond = scanning();
        beyond.feed(past.as_bytes());
        assert!(beyond.settled(), "the length was reached");
        assert_eq!(beyond.listing(End::Closed), Listing::Absent);
    }

    #[test]
    fn chunked_is_never_whole() {
        let reply = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
             i|pong|Chunky|gnutella2\r\n{THIRD}"
        );
        let mut r = scanning();
        assert_eq!(r.close(reply.as_bytes()), Some("Chunky".into()));
        assert_eq!(r.listing(End::Closed), Listing::Unknown);
    }

    #[test]
    fn the_verdict_is_fixed_at_the_pong() {
        let reply = format!(
            "{OK}i|pong|Fixed|gnutella2\r\n\u{1}<html>|||\r\n{THIRD}i|pong|Other|gnutella2\r\n"
        );
        let mut r = scanning();
        assert_eq!(r.feed(reply.as_bytes()), Some(pong_named("Fixed")));
        assert_eq!(r.at_deadline(), Some("Fixed".into()));
        assert_eq!(r.verdict(), Some(pong_named("Fixed")));
        assert_eq!(r.listing(End::Deadline), Listing::Unknown);
    }

    #[test]
    fn no_pong_settles_a_scan() {
        let mut r = scanning();
        assert_eq!(r.feed(b"HTTP/1.1 404 Not Found\r\n\r\n"), NO_PONG);
        assert!(r.settled());
        assert_eq!(r.listing(End::Closed), Listing::Unknown);
        // A 200 with no pong, closed: no pong, even with this cache listed.
        let mut c = scanning();
        assert_eq!(c.close(format!("{OK}{ME}ERROR\r\n").as_bytes()), None);
        assert_eq!(c.verdict(), NO_PONG);
        assert!(c.settled());
        assert_eq!(c.listing(End::Closed), Listing::Unknown);
    }

    #[test]
    fn a_skulls_pong_settles_a_scan() {
        let pong = format!("{OK}i|pong|Skulls 0.3.6|gnutella2\r\n");
        let reply = format!("{pong}{THIRD}h|1.2.3.4:6346|5\r\n");
        let mut r = scanning();
        assert_eq!(r.feed(pong.as_bytes()), Some(pong_named("Skulls 0.3.6")));
        assert!(r.settled());
        r.close(reply.as_bytes());
        assert_eq!(r.listing(End::Closed), Listing::Unknown);
    }

    #[test]
    fn a_trickled_scan_costs_linear_work() {
        let lines = format!("{OK}i|pong|P|gnutella2\r\n{}", THIRD.repeat(512));
        let mut r = scanning();
        for end in 1..=lines.len() {
            r.feed(&lines.as_bytes()[..end]);
        }
        assert!(!r.settled());
        assert!(r.examined() <= 4 * lines.len(), "{}", r.examined());
    }

    fn sized(body: &str, n: usize) -> String {
        format!("HTTP/1.1 200 OK\r\nContent-Length: {n}\r\n\r\n{body}")
    }

    #[test]
    fn a_reached_length_is_whole_even_at_the_deadline() {
        let body = format!("i|pong|Kept|gnutella2\r\n{THIRD}");
        let reply = sized(&body, body.len());
        let mut r = scanning();
        r.feed(&reply.as_bytes()[..reply.len() - 1]);
        assert!(!r.settled());
        r.feed(reply.as_bytes());
        assert!(r.settled(), "reading on can change nothing");
        assert_eq!(r.listing(End::Deadline), Listing::Absent);
        assert_eq!(r.listing(End::Cut), Listing::Absent);
        assert_eq!(r.listing(End::Closed), Listing::Absent);
        // The last line ends at the length, with no line end.
        let open = "i|pong|Kept|gnutella2\r\nu|http://third.example.net/";
        let mut t = scanning();
        t.feed(sized(open, open.len()).as_bytes());
        assert!(t.settled());
        assert_eq!(t.listing(End::Deadline), Listing::Absent);
    }

    #[test]
    fn a_length_that_cuts_a_line_is_not_whole() {
        let body = format!("i|pong|Cut|gnutella2\r\n{ME}");
        let n = body.find(".org").unwrap();
        let reply = sized(&body, n);
        for closed in [false, true] {
            let mut r = scanning();
            if closed {
                assert_eq!(r.close(reply.as_bytes()), Some("Cut".into()));
            } else {
                r.feed(reply.as_bytes());
            }
            for end in [End::Closed, End::Deadline, End::Cut] {
                assert_eq!(r.listing(end), Listing::Unknown, "{closed} {end:?}");
            }
        }
    }

    #[test]
    fn transfer_encoding_beats_content_length() {
        let body = format!("i|pong|Both|gnutella2\r\n{THIRD}");
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nTransfer-Encoding: chunked\r\n\r\n{body}",
            body.len()
        );
        let mut r = scanning();
        r.feed(reply.as_bytes());
        assert!(!r.settled(), "the length does not delimit the body");
        r.close(reply.as_bytes());
        assert_eq!(r.listing(End::Closed), Listing::Unknown);
        assert_eq!(r.listing(End::Deadline), Listing::Unknown);
    }

    #[test]
    fn a_read_ending_at_the_length_settles_before_any_cut_shows() {
        // A wrong Content-Length cuts this cache's line to `gwc.exa`. In one
        // read, the byte after the length shows the cut: not whole, Unknown
        // (`a_length_that_cuts_a_line_is_not_whole`). A read that ends
        // exactly at the length shows nothing after it, so the length counts
        // as reached and the reader settles there: the cut line is taken as
        // complete, a third cache, and the listing reads Absent. Accepted
        // (ADR-0021, controller ruling): settling at the length wins; it
        // needs a broken peer and an exact split, and costs at most one
        // spurious advertisement per 30 days.
        let body = format!("i|pong|Cut|gnutella2\r\n{ME}");
        let n = body.find(".org").unwrap();
        let reply = sized(&body, n);
        let at_length = reply.len() - (body.len() - n);
        let mut r = scanning();
        assert_eq!(
            r.feed(&reply.as_bytes()[..at_length]),
            Some(pong_named("Cut"))
        );
        assert!(r.settled(), "settled after the first read");
        r.feed(reply.as_bytes());
        assert_eq!(r.listing(End::Deadline), Listing::Absent);
        let mut once = scanning();
        once.feed(reply.as_bytes());
        assert_eq!(once.listing(End::Deadline), Listing::Unknown);
    }
}
