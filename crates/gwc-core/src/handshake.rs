//! The G2 handshake used to verify a hub: what to send, and how to read the
//! reply.
//!
//! The verifier connects as a leaf that will not stay: it sends the opening
//! `GNUTELLA CONNECT/0.6` block, reads the hub's reply, and closes without the
//! third step. A Shareaza hub then just logs a dropped connection
//! (`ShakeNeighbour.cpp:267-278`) and has no ban for it.
//!
//! Citations are to ansani/Shareaza@f24b0ce.

use std::net::SocketAddrV4;

/// Longest reply read before giving up. A Shareaza 503 with 15 `X-Try-Hubs`
/// entries is well under 2 KiB.
pub const MAX_REPLY: usize = 8 * 1024;

/// Most `X-Try-Hubs` entries kept from one reply. Shareaza sends at most
/// `Gnutella2.HostCount`, default 15 and capped at 50 (`Settings.cpp:394`).
pub const MAX_TRY_HUBS: usize = 50;

/// The opening handshake block.
///
/// `User-Agent` must not be empty: Shareaza blocks an empty agent
/// (`Security.cpp:1297-1300`). `Accept: application/x-gnutella2` selects the
/// G2 path (`ShakeNeighbour.cpp:1028-1034`); without it the hub assumes G1
/// (`:937`) and, with G1 disabled, answers `503 Wrong Protocol`.
/// `X-Ultrapeer: False` presents us as a leaf.
#[must_use]
pub fn connect_request() -> String {
    format!(
        "GNUTELLA CONNECT/0.6\r\n\
         User-Agent: {}\r\n\
         Accept: application/x-gnutella2\r\n\
         X-Ultrapeer: False\r\n\
         \r\n",
        crate::product_id()
    )
}

/// What a hub's reply says about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// `200` speaking G2: a hub with room.
    Verified,
    /// `503 Maximum connections reached` or another busy 503: alive, full.
    AliveFull,
    /// Anything else: not a usable hub (a leaf, the wrong protocol, garbage).
    Dead,
}

/// How one handshake attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exchange {
    /// The hub's full header block.
    Reply(Reply),
    /// Refused, reset, or not connectable at all.
    Unreachable,
    /// No complete reply before the deadline.
    Timeout,
    /// Closed before the header block ended: what a banned address gets
    /// (`ShakeNeighbour.cpp:1024-1026`).
    Closed,
    /// Not a Gnutella reply, or longer than [`MAX_REPLY`].
    Invalid,
}

impl Exchange {
    /// Only a reply can vouch for a hub; every other ending is [`Verdict::Dead`].
    #[must_use]
    pub fn verdict(&self) -> Verdict {
        match self {
            Exchange::Reply(r) => r.verdict(),
            _ => Verdict::Dead,
        }
    }

    /// The `X-Try-Hubs` entries of the reply, if there was one.
    #[must_use]
    pub fn try_hubs(&self) -> Vec<(SocketAddrV4, Option<u64>)> {
        match self {
            Exchange::Reply(r) => r.try_hubs(),
            _ => Vec::new(),
        }
    }
}

/// A parsed reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub reason: String,
    /// Header names lowercased, in order; repeated headers kept separately.
    pub headers: Vec<(String, String)>,
}

/// Outcome of feeding the bytes read so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parse {
    /// No blank line yet; read more. If the connection closes in this state
    /// (a banned address gets no status line at all), the hub is dead.
    Incomplete,
    /// The full header block arrived.
    Done(Reply),
    /// Not a Gnutella reply, or longer than [`MAX_REPLY`].
    Invalid,
}

/// Parse the hub's reply from the bytes received so far.
///
/// Lines end at `\n` with an optional `\r` (`Buffer.cpp:343-374`). A line
/// starting with whitespace continues the previous header. Shareaza re-delivers
/// such a line under the last header name (`Connection.cpp:1154-1169`), which
/// for `X-Try-Hubs` replaces the value (`ShakeNeighbour.cpp:903`); we append
/// it instead, which keeps more entries and never fewer.
#[must_use]
pub fn parse_reply(bytes: &[u8]) -> Parse {
    let Some(end) = header_block_end(bytes) else {
        return if bytes.len() > MAX_REPLY {
            Parse::Invalid
        } else {
            Parse::Incomplete
        };
    };
    if end > MAX_REPLY {
        return Parse::Invalid;
    }
    // Shareaza decodes UTF-8, falling back to the ANSI code page
    // (`Buffer.cpp:362`, `Strings.cpp:234-258`); a lossy decode stands in.
    let text = String::from_utf8_lossy(&bytes[..end]);
    let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));

    let Some((status, reason)) = lines.next().and_then(status_line) else {
        return Parse::Invalid;
    };

    let mut headers: Vec<(String, String)> = Vec::new();
    // Whether the previous line was a header we kept, so a continuation line
    // is not glued onto one we ignored.
    let mut kept_previous = false;
    for line in lines.take_while(|l| !l.is_empty()) {
        if line.starts_with([' ', '\t']) {
            if kept_previous && let Some((_, value)) = headers.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }
        kept_previous = false;
        let Some(colon) = line.find(':') else {
            continue;
        };
        // Connection.cpp:1171-1172: the name is 2..=63 characters.
        if !(2..64).contains(&colon) {
            continue;
        }
        headers.push((
            line[..colon].trim().to_ascii_lowercase(),
            line[colon + 1..].trim().to_owned(),
        ));
        kept_previous = true;
    }
    Parse::Done(Reply {
        status,
        reason: reason.to_owned(),
        headers,
    })
}

/// Index just past the first empty line (`\n\n` or `\n\r\n`), if any.
fn header_block_end(bytes: &[u8]) -> Option<usize> {
    let mut line_start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            let line = &bytes[line_start..i];
            if line.is_empty() || line == b"\r" {
                return Some(i + 1);
            }
            line_start = i + 1;
        }
    }
    None
}

/// `GNUTELLA/0.6 <code> <reason>` → code and reason.
fn status_line(line: &str) -> Option<(u16, &str)> {
    let rest = line.strip_prefix("GNUTELLA/0.6 ")?;
    let (code, reason) = rest.split_once(' ').unwrap_or((rest, ""));
    if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((code.parse().ok()?, reason.trim()))
}

impl Reply {
    /// First value of a header, by case-insensitive name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Classify the hub.
    ///
    /// - `200` with a G2 `Content-Type` (`application/x-gnutella2` or
    ///   `application/x-shareaza`) is a hub, unless `X-Ultrapeer` (or its alias
    ///   `X-Hub`) is present with any value other than `True`.
    /// - `503` is a busy hub, except for Shareaza's reasons that mean "not a
    ///   hub": `Shielded leaf node` (`ShakeNeighbour.cpp:1190`), `Need an
    ///   Ultrapeer` (`:1238-1241`), `Ultrapeer disabled` (`:1230-1234`) and
    ///   `Wrong Protocol` (`:1044`).
    #[must_use]
    pub fn verdict(&self) -> Verdict {
        match self.status {
            200 => {
                // ShakeNeighbour.cpp:837-839: either content type means G2.
                let g2 = self.header("content-type").is_some_and(|t| {
                    let t = t.to_ascii_lowercase();
                    t.contains("application/x-gnutella2") || t.contains("application/x-shareaza")
                });
                // :796-800: `X-Hub` is an alias, and any value but "True" is a
                // leaf. A missing header is not held against the hub.
                let leaf = self
                    .header("x-ultrapeer")
                    .or_else(|| self.header("x-hub"))
                    .is_some_and(|v| !v.eq_ignore_ascii_case("true"));
                if g2 && !leaf {
                    Verdict::Verified
                } else {
                    Verdict::Dead
                }
            }
            503 => {
                let reason = self.reason.to_ascii_lowercase();
                let not_a_hub = [
                    "shielded leaf node",
                    "need an ultrapeer",
                    "ultrapeer disabled",
                    "wrong protocol",
                ]
                .iter()
                .any(|r| reason.contains(r));
                if not_a_hub {
                    Verdict::Dead
                } else {
                    Verdict::AliveFull
                }
            }
            _ => Verdict::Dead,
        }
    }

    /// Hubs offered in `X-Try-Hubs`, with their last-seen time (Unix seconds)
    /// when given. Shareaza writes `ip:port YYYY-MM-DDThh:mmZ` entries joined
    /// by `,` (`ShakeNeighbour.cpp:563-592`, `HostCache.cpp:1862-1865`).
    /// Malformed or non-public entries are skipped; at most [`MAX_TRY_HUBS`].
    ///
    /// Shareaza's reader, `TimeFromString` (`Shareaza.cpp:1851-1889`), takes
    /// only the minute form and normalises impossible dates through `mktime`.
    /// We also take `:ss`, and drop impossible dates instead: both only affect
    /// entries Shareaza itself never writes.
    #[must_use]
    pub fn try_hubs(&self) -> Vec<(SocketAddrV4, Option<u64>)> {
        self.headers
            .iter()
            .filter(|(n, _)| n == "x-try-hubs")
            .flat_map(|(_, v)| v.split(','))
            .filter_map(try_hub_entry)
            .take(MAX_TRY_HUBS)
            .collect()
    }
}

/// `ip:port [timestamp]`. As in Shareaza (`HostCache.cpp:385-401`), a
/// timestamp that is present but unreadable drops the entry.
fn try_hub_entry(entry: &str) -> Option<(SocketAddrV4, Option<u64>)> {
    let entry = entry.trim();
    let (addr, time) = match entry.split_once(' ') {
        Some((addr, time)) => (addr, Some(parse_time(time.trim())?)),
        None => (entry, None),
    };
    Some((crate::addr::parse_hub(addr).ok()?, time))
}

/// `YYYY-MM-DDThh:mmZ` or `YYYY-MM-DDThh:mm:ssZ` (UTC) → Unix seconds.
fn parse_time(s: &str) -> Option<u64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.splitn(3, '-').map(|p| p.parse::<u32>().ok());
    let (year, month, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':').map(|p| p.parse::<u32>().ok());
    let (hour, minute) = (t.next()??, t.next()??);
    let second = t.next().unwrap_or(Some(0))?;
    if t.next().is_some() || !(1970..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    if day == 0 || day > days_in_month(year, month) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + u64::from(hour * 3600 + minute * 60 + second))
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`, restricted to years >= 1970).
fn days_from_civil(year: u32, month: u32, day: u32) -> u64 {
    let y = u64::from(if month <= 2 { year - 1 } else { year });
    let era = y / 400;
    let yoe = y - era * 400;
    let m = u64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + u64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    fn done(raw: &str) -> Reply {
        match parse_reply(raw.as_bytes()) {
            Parse::Done(r) => r,
            other => panic!("{other:?}"),
        }
    }

    /// A Shareaza hub accepting a G2 leaf, header order as in
    /// `ShakeNeighbour.cpp:1245-1257` and `:317-438`.
    const SHAREAZA_200: &str = "GNUTELLA/0.6 200 OK\r\n\
        User-Agent: Shareaza 2.7.10.6\r\n\
        Accept: application/x-gnutella2\r\n\
        Content-Type: application/x-gnutella2\r\n\
        X-Ultrapeer: True\r\n\
        X-Ultrapeer-Needed: False\r\n\
        Listen-IP: 8.8.4.4:6346\r\n\
        Remote-IP: 1.1.1.1:41234\r\n\
        \r\n";

    /// A full Shareaza hub, `ShakeNeighbour.cpp:1220-1226` via
    /// `SendHostHeaders` (`:544-629`).
    const SHAREAZA_503_FULL: &str = "GNUTELLA/0.6 503 Maximum connections reached\r\n\
        User-Agent: Shareaza 2.7.10.6\r\n\
        Accept: application/x-gnutella2\r\n\
        Content-Type: application/x-gnutella2\r\n\
        X-Ultrapeer: True\r\n\
        X-Try-Hubs: 9.9.9.9:6346 2026-10-23T08:30Z,1.0.0.1:6347 2026-10-22T23:59Z\r\n\
        \r\n";

    #[test]
    fn only_a_reply_can_vouch_for_a_hub() {
        assert_eq!(
            Exchange::Reply(done(SHAREAZA_200)).verdict(),
            Verdict::Verified
        );
        assert_eq!(
            Exchange::Reply(done(SHAREAZA_503_FULL)).verdict(),
            Verdict::AliveFull
        );
        for failed in [
            Exchange::Unreachable,
            Exchange::Timeout,
            Exchange::Closed,
            Exchange::Invalid,
        ] {
            assert_eq!(failed.verdict(), Verdict::Dead, "{failed:?}");
            assert_eq!(failed.try_hubs(), []);
        }
        assert_eq!(Exchange::Reply(done(SHAREAZA_503_FULL)).try_hubs().len(), 2);
    }

    #[test]
    fn connect_request_is_a_g2_leaf_handshake() {
        let req = connect_request();
        assert!(req.starts_with("GNUTELLA CONNECT/0.6\r\n"));
        assert!(req.ends_with("\r\n\r\n"));
        assert_eq!(req.matches("\r\n\r\n").count(), 1, "one header block");
        assert!(req.contains("\r\nAccept: application/x-gnutella2\r\n"));
        assert!(req.contains("\r\nX-Ultrapeer: False\r\n"));
        let ua = req
            .lines()
            .find_map(|l| l.strip_prefix("User-Agent: "))
            .expect("a User-Agent");
        assert_eq!(ua.trim_end(), crate::product_id());
    }

    #[test]
    fn shareaza_200_is_verified() {
        let r = done(SHAREAZA_200);
        assert_eq!((r.status, r.reason.as_str()), (200, "OK"));
        assert_eq!(r.header("content-type"), Some("application/x-gnutella2"));
        assert_eq!(r.verdict(), Verdict::Verified);
        assert!(r.try_hubs().is_empty(), "no X-Try-Hubs on 200 (:1251-1252)");
    }

    #[test]
    fn shareaza_503_full_is_alive_full_with_try_hubs() {
        let r = done(SHAREAZA_503_FULL);
        assert_eq!(r.status, 503);
        assert_eq!(r.verdict(), Verdict::AliveFull);
        assert_eq!(
            r.try_hubs(),
            [
                (
                    SocketAddrV4::new(Ipv4Addr::new(9, 9, 9, 9), 6346),
                    Some(1_792_744_200) // 2026-10-23T08:30:00Z
                ),
                (
                    SocketAddrV4::new(Ipv4Addr::new(1, 0, 0, 1), 6347),
                    Some(1_792_713_540) // 2026-10-22T23:59:00Z
                ),
            ]
        );
    }

    #[test]
    fn not_a_hub_503_reasons_are_dead() {
        for reason in [
            "Shielded leaf node",
            "Need an Ultrapeer",
            "Ultrapeer disabled",
            "Wrong Protocol",
        ] {
            let r = done(&format!(
                "GNUTELLA/0.6 503 {reason}\r\nUser-Agent: Shareaza 2.7.10.6\r\n\r\n"
            ));
            assert_eq!(r.verdict(), Verdict::Dead, "{reason}");
        }
    }

    #[test]
    fn wrong_protocol_reply_ends_right_after_the_status_line() {
        // :1044 passes a message ending in \r\n and SendHostHeaders adds
        // another (:553-554), so the header block is empty.
        let r =
            done("GNUTELLA/0.6 503 Wrong Protocol\r\n\r\nUser-Agent: Shareaza 2.7.10.6\r\n\r\n");
        assert_eq!(r.headers, Vec::<(String, String)>::new());
        assert_eq!(r.verdict(), Verdict::Dead);
    }

    #[test]
    fn other_busy_503_is_alive_full() {
        let r = done("GNUTELLA/0.6 503 Busy\r\n\r\n");
        assert_eq!(r.verdict(), Verdict::AliveFull);
    }

    #[test]
    fn g1_or_leaf_200_is_dead() {
        let g1 =
            done("GNUTELLA/0.6 200 OK\r\nContent-Type: application/x-gnutella-packets\r\n\r\n");
        assert_eq!(g1.verdict(), Verdict::Dead);
        let no_type = done("GNUTELLA/0.6 200 OK\r\nX-Ultrapeer: True\r\n\r\n");
        assert_eq!(no_type.verdict(), Verdict::Dead);
        let leaf = done(
            "GNUTELLA/0.6 200 OK\r\nContent-Type: application/x-gnutella2\r\nX-Ultrapeer: False\r\n\r\n",
        );
        assert_eq!(leaf.verdict(), Verdict::Dead);
    }

    #[test]
    fn x_ultrapeer_other_than_true_is_a_leaf() {
        // Shareaza treats any value other than "True" as false.
        let r = done(
            "GNUTELLA/0.6 200 OK\r\nContent-Type: application/x-gnutella2\r\nX-Ultrapeer: maybe\r\n\r\n",
        );
        assert_eq!(r.verdict(), Verdict::Dead);
    }

    #[test]
    fn x_hub_alias_and_shareaza_content_type_are_accepted() {
        // ShakeNeighbour.cpp:796-800 (X-Hub), :837-839 (x-shareaza content type).
        let r = done(
            "GNUTELLA/0.6 200 OK\r\nContent-Type: application/x-shareaza\r\nX-Hub: True\r\n\r\n",
        );
        assert_eq!(r.verdict(), Verdict::Verified);
        let leaf = done(
            "GNUTELLA/0.6 200 OK\r\nContent-Type: application/x-gnutella2\r\nX-Hub: False\r\n\r\n",
        );
        assert_eq!(leaf.verdict(), Verdict::Dead);
    }

    #[test]
    fn non_utf8_header_bytes_are_decoded_lossily() {
        // Buffer.cpp:362 falls back to ANSI; one Latin-1 byte must not kill a hub.
        let mut raw = b"GNUTELLA/0.6 200 OK\r\nUser-Agent: Shareaza \xe9\r\n".to_vec();
        raw.extend_from_slice(b"Content-Type: application/x-gnutella2\r\n\r\n");
        match parse_reply(&raw) {
            Parse::Done(r) => assert_eq!(r.verdict(), Verdict::Verified),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn missing_x_ultrapeer_on_200_still_counts_as_hub() {
        let r = done("GNUTELLA/0.6 200 OK\r\ncontent-type: Application/X-Gnutella2\r\n\r\n");
        assert_eq!(r.verdict(), Verdict::Verified);
    }

    #[test]
    fn bare_lf_line_endings_are_accepted() {
        let r = done("GNUTELLA/0.6 200 OK\nContent-Type: application/x-gnutella2\n\n");
        assert_eq!(r.verdict(), Verdict::Verified);
    }

    #[test]
    fn partial_reply_is_incomplete() {
        assert_eq!(parse_reply(b""), Parse::Incomplete);
        assert_eq!(
            parse_reply(b"GNUTELLA/0.6 200 OK\r\nContent-"),
            Parse::Incomplete
        );
        assert_eq!(parse_reply(b"GNUTELLA/0.6 200 OK\r\n"), Parse::Incomplete);
    }

    #[test]
    fn garbage_and_overlong_replies_are_invalid() {
        assert_eq!(
            parse_reply(b"HTTP/1.1 400 Bad Request\r\n\r\n"),
            Parse::Invalid
        );
        assert_eq!(parse_reply(b"GNUTELLA/0.6 abc OK\r\n\r\n"), Parse::Invalid);
        assert_eq!(
            parse_reply(b"\xff\xfe\r\n\r\n"),
            Parse::Invalid,
            "not a status line"
        );
        let long = format!("GNUTELLA/0.6 200 OK\r\nX: {}", "a".repeat(MAX_REPLY));
        assert_eq!(parse_reply(long.as_bytes()), Parse::Invalid);
    }

    #[test]
    fn bytes_after_the_header_block_are_ignored() {
        // A hub may start sending G2 packets right after the block.
        let mut bytes = SHAREAZA_200.as_bytes().to_vec();
        bytes.extend_from_slice(&[0x00, 0xff, 0x13]);
        assert!(matches!(parse_reply(&bytes), Parse::Done(_)));
    }

    #[test]
    fn continuation_lines_extend_the_previous_header() {
        let r = done(
            "GNUTELLA/0.6 503 Maximum connections reached\r\n\
             X-Try-Hubs: 9.9.9.9:6346 2026-10-23T08:30Z,\r\n \
             1.0.0.1:6347 2026-10-22T23:59Z\r\n\r\n",
        );
        assert_eq!(r.try_hubs().len(), 2);
    }

    #[test]
    fn header_names_shorter_than_two_or_longer_than_63_are_ignored() {
        // Connection.cpp:1171-1172
        let long_name = "X".repeat(64);
        let r = done(&format!(
            "GNUTELLA/0.6 200 OK\r\nA: 1\r\n{long_name}: 2\r\nno colon here\r\nContent-Type: application/x-gnutella2\r\n\r\n"
        ));
        assert_eq!(
            r.headers,
            [("content-type".into(), "application/x-gnutella2".into())]
        );
    }

    #[test]
    fn try_hubs_skips_bad_entries_and_accepts_missing_time() {
        let r = done(
            "GNUTELLA/0.6 503 Maximum connections reached\r\n\
             X-Try-Hubs: 9.9.9.9:6346,10.0.0.1:6346 2026-10-23T08:30Z, junk ,\
             1.0.0.1:6347 not-a-date,8.8.8.8:80 2026-10-23T08:30Z,1.1.1.1:6346 2026-13-40T99:99Z\r\n\r\n",
        );
        assert_eq!(
            r.try_hubs(),
            [(SocketAddrV4::new(Ipv4Addr::new(9, 9, 9, 9), 6346), None)],
            "private, junk, bad-date, low-port and impossible-date entries dropped"
        );
    }

    #[test]
    fn timestamps_follow_the_calendar() {
        assert_eq!(parse_time("1970-01-01T00:00Z"), Some(0));
        assert_eq!(parse_time("2026-10-23T08:30:15Z"), Some(1_792_744_215));
        assert_eq!(parse_time("2024-02-29T00:00Z"), Some(1_709_164_800));
        assert_eq!(parse_time("2026-02-29T00:00Z"), None, "not a leap year");
        assert_eq!(parse_time("2000-02-29T00:00Z"), Some(951_782_400));
        assert_eq!(parse_time("1900-02-28T00:00Z"), None, "before 1970");
        assert_eq!(parse_time("2026-04-31T00:00Z"), None);
        assert_eq!(parse_time("2026-10-23T08:30"), None, "no Z");
        assert_eq!(parse_time("2026-10-23T08:30:00:00Z"), None);
        assert_eq!(parse_time("2026-10-23T24:00Z"), None);
        assert_eq!(parse_time("2026-10-23 08:30Z"), None);
    }

    #[test]
    fn try_hubs_is_capped() {
        let entries: Vec<String> = (1..=60)
            .map(|i| format!("9.9.{i}.1:6346 2026-10-23T08:30Z"))
            .collect();
        let r = done(&format!(
            "GNUTELLA/0.6 503 Maximum connections reached\r\nX-Try-Hubs: {}\r\n\r\n",
            entries.join(",")
        ));
        assert_eq!(r.try_hubs().len(), MAX_TRY_HUBS);
    }
}
