//! Test-only model of how Shareaza judges a cache's reply, so tests can assert
//! Shareaza's verdict rather than the shape of our output.
//!
//! It mirrors `CDiscoveryServices::RunWebCacheGet` (`DiscoveryServices.cpp:1399-1803`)
//! and `RunWebCacheUpdate` (`:1808-1942`) at ansani/Shareaza@f24b0ce, line for
//! line where it matters. Simplifications, each conservative for our purposes:
//!
//! - `inet_addr` is approximated by canonical dotted-quad parsing; we never
//!   emit any other form.
//! - `HostCache.Add` is assumed to accept every well-formed `h|` entry.
//! - `Add(url)` is modelled pessimistically: a URL counts only if it matches
//!   one the client already holds (`known_urls`), as `GetByAddress` would
//!   (`:117-140`, `:369-392`). A new URL is assumed refused, as when the client
//!   already holds `Discovery.CacheCount` caches (`:258-268`). Decision B must
//!   hold even then.
//! - Plain-IP and plain-URL lines (G1 data) are reported as [`Outcome::G1Data`],
//!   because they flip the cache to G1-only (`:1771-1785`); we never want them.

use std::net::Ipv4Addr;

/// Shareaza's verdict on one reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// `OnSuccess`: failure count reset.
    Success,
    /// `OnFailure`: one step closer to removal (2 by default).
    Failure,
    /// Removed from the client's list at once.
    Removed,
    /// Parsed as G1 data, which reflags the cache as G1-only.
    G1Data,
}

/// Shareaza's line split: up to the first `\r` or `\n`, then skip exactly one
/// character, then trim (`:1455-1460`). Empty lines are skipped.
fn lines(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        let end = rest.find(['\r', '\n']).unwrap_or(rest.len());
        let line = &rest[..end];
        rest = rest.get(end + 1..).unwrap_or("");
        let line = line.trim_matches(['\r', '\n', ' ', '\t']);
        if !line.is_empty() {
            out.push(line.to_owned());
        }
    }
    out
}

/// Shareaza's field split (`:1462-1469`): a trailing `|` adds nothing,
/// `a||b` keeps the empty middle field.
fn parts(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut tmp = line;
    while !tmp.is_empty() {
        let end = tmp.find('|').unwrap_or(tmp.len());
        out.push(tmp[..end].trim().to_owned());
        tmp = tmp.get(end + 1..).unwrap_or("");
    }
    out
}

/// `_stscanf( ..., "%i" )` for the values we emit: an optional sign and digits.
fn scan_int(s: &str) -> Option<i64> {
    let digits_end = s
        .char_indices()
        .skip_while(|&(i, c)| i == 0 && (c == '-' || c == '+'))
        .find(|&(_, c)| !c.is_ascii_digit())
        .map_or(s.len(), |(i, _)| i);
    s[..digits_end].parse().ok()
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

fn contains_ci(s: &str, needle: &str) -> bool {
    s.to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

/// Judge a get reply. `g2` is the protocol of the query; `known_urls` are the
/// caches already in the client's list (it always holds the one it queried).
pub fn get(body: &str, g2: bool, known_urls: &[&str]) -> Outcome {
    let (mut hosts, mut caches) = (0, 0);
    for line in lines(body) {
        let p = parts(&line);
        let head = p[0].to_ascii_lowercase();
        match head.as_str() {
            "h" => {
                if p.len() < 3 || !host_ok(&p) {
                    return Outcome::Failure;
                }
                hosts += 1;
            }
            "u" => {
                if p.len() < 2 {
                    return Outcome::Failure;
                }
                let wanted = starts_with_ci(&p[1], "http://")
                    || (g2 && starts_with_ci(&p[1], "ukhl:"))
                    || (!g2 && starts_with_ci(&p[1], "uhc:"));
                if wanted && add_known(&p[1], known_urls) {
                    caches += 1;
                }
            }
            "uhc" | "ukhl" => {
                if p.len() < 2 {
                    return Outcome::Failure;
                }
                if (head == "ukhl") == g2 && add_known(&p[1], known_urls) {
                    caches += 1;
                }
            }
            "i" => {
                if let Some(outcome) = info_line(&p, g2) {
                    return outcome;
                }
            }
            _ if starts_with_ci(&line, "PONG") => {
                if !g2 {
                    return Outcome::Failure;
                }
            }
            _ if contains_ci(&line, "ERROR") => {
                if !contains_ci(&line, "Something Failed") {
                    return Outcome::Failure;
                }
            }
            _ if line.parse::<std::net::SocketAddrV4>().is_ok() || line.contains("://") => {
                return Outcome::G1Data;
            }
            _ => return Outcome::Failure,
        }
    }
    if hosts > 0 || caches > 0 {
        Outcome::Success
    } else {
        Outcome::Failure
    }
}

/// `CDiscoveryServices::Add(LPCTSTR)` up to its "already in list" exit
/// (`:117-140`): the clean-up, then `GetByAddress` (`:369-392`).
fn add_known(raw: &str, known_urls: &[&str]) -> bool {
    let end_slash = raw.ends_with('/');
    let mut addr = raw.trim().trim_end_matches(['.', '/']).to_owned();
    if end_slash {
        addr.push('/');
    }
    if addr.len() < 8 {
        return false;
    }
    known_urls.iter().any(|known| {
        if known.len() > 45 {
            // Long addresses ignore their last two characters (:377-381).
            let n = known.len() - 2;
            addr.get(..n)
                .zip(known.get(..n))
                .is_some_and(|(a, k)| a.eq_ignore_ascii_case(k))
        } else {
            addr.eq_ignore_ascii_case(known)
        }
    })
}

/// `inet_addr` for the dotted quads we emit; it returns `INADDR_NONE` for
/// `255.255.255.255`, which Shareaza then treats as invalid (`:1481-1482`).
fn inet_addr_ok(s: &str) -> bool {
    s.parse::<Ipv4Addr>()
        .is_ok_and(|ip| ip != Ipv4Addr::BROADCAST)
}

/// `h|` field checks, `:1476-1546`.
fn host_ok(p: &[String]) -> bool {
    let Some(colon) = p[1].find(':') else {
        return false;
    };
    let addr_ok = colon > 6
        && inet_addr_ok(&p[1][..colon])
        && scan_int(&p[1][colon + 1..]).is_some_and(|port| port > 0 && port < 65536)
        && scan_int(&p[2]).is_some();
    let field = |i: usize| p.get(i).filter(|f| !f.is_empty());
    let leaves_ok =
        |i| field(i).is_none_or(|f| scan_int(f).is_some_and(|n| (0..2048).contains(&n)));
    let year = 60 * 60 * 24 * 365;
    addr_ok
        && leaves_ok(4)
        && field(5).is_none_or(|v| !contains_ci(&v.chars().take(4).collect::<String>(), "foxy"))
        && field(6).is_none_or(|f| scan_int(f).is_some_and(|u| u > 60 && u < year))
        && leaves_ok(7)
}

/// `i|` lines, `:1631-1749`. `None` means "keep reading".
fn info_line(p: &[String], g2: bool) -> Option<Outcome> {
    if p.len() < 2 {
        return None;
    }
    let cmd = p[1].to_ascii_lowercase();
    let has_net = |list: &str, sep: char| {
        list.split(sep)
            .any(|n| n.eq_ignore_ascii_case(if g2 { "gnutella2" } else { "gnutella" }))
    };
    match cmd.as_str() {
        "pong" => {
            if !g2 {
                return Some(Outcome::Failure);
            }
            if p.len() >= 4 && !has_net(&p[3], '-') {
                return Some(Outcome::Failure);
            }
        }
        "force" if p.len() >= 3 && p[2].eq_ignore_ascii_case("remove") => {
            return Some(Outcome::Removed);
        }
        "update"
            if p.len() >= 4
                && p[2].eq_ignore_ascii_case("warning")
                && p[3].eq_ignore_ascii_case("bad url") =>
        {
            return Some(Outcome::Removed);
        }
        "networks" if p.len() >= 3 && !p[2..].iter().any(|n| has_net(n, '\0')) => {
            return Some(Outcome::Failure);
        }
        "nets" if p.len() >= 3 && !has_net(&p[2], '-') => return Some(Outcome::Failure),
        _ => {}
    }
    None
}

/// Judge an update reply (`:1890-1942`).
pub fn update(body: &str) -> Outcome {
    for line in lines(body) {
        if line.contains("OK") {
            return Outcome::Success;
        }
        if contains_ci(&line, "i|warning|client|early")
            || contains_ci(&line, "i|warning|You came back too early")
            || contains_ci(&line, "WARNING: You came back too early")
            || contains_ci(&line, "ERROR")
        {
            return Outcome::Failure;
        }
        if line == "i|force|remove" {
            return Outcome::Removed;
        }
    }
    Outcome::Failure
}

#[cfg(test)]
mod tests {
    use super::*;

    // The model itself must agree with the source on the cases that matter.

    #[test]
    fn splits_like_shareaza() {
        assert_eq!(lines("a\r\nb\n\nc"), ["a", "b", "c"]);
        assert_eq!(parts("a|"), ["a"]);
        assert_eq!(parts("a||"), ["a", ""]);
        assert_eq!(parts("a||b"), ["a", "", "b"]);
    }

    #[test]
    fn empty_or_info_only_get_is_a_failure() {
        assert_eq!(get("", true, &[]), Outcome::Failure);
        assert_eq!(get("i|pong|x 1|gnutella2\n", true, &[]), Outcome::Failure);
    }

    #[test]
    fn get_keywords() {
        assert_eq!(
            get("u|http://a.example.org/|0\ni|force|remove\n", true, &[]),
            Outcome::Removed
        );
        assert_eq!(
            get("i|update|WARNING|bad url\n", true, &[]),
            Outcome::Removed
        );
        assert_eq!(get("ERROR No network\n", true, &[]), Outcome::Failure);
        assert_eq!(
            get(
                "i|pong|x 1|gnutella2\nu|http://a.example.org/|0\n",
                false,
                &[]
            ),
            Outcome::Failure
        );
        assert_eq!(
            get(
                "i|pong|x 1|gnutella\nu|http://a.example.org/|0\n",
                true,
                &[]
            ),
            Outcome::Failure
        );
        assert_eq!(get("1.2.3.4:6346\n", true, &[]), Outcome::G1Data);
        assert_eq!(get("x|y\n", true, &[]), Outcome::Failure);
        assert_eq!(get("h|8.8.4.4:6346|5||2048\n", true, &[]), Outcome::Failure);
        assert_eq!(get("h|8.8.4.4:6346|5||||60\n", true, &[]), Outcome::Failure);
        assert_eq!(
            get("h|8.8.4.4:6346|5|||FOXY\n", true, &[]),
            Outcome::Failure
        );
        assert_eq!(
            get("h|8.8.4.4:6346|5||12|RAZA|61|300\n", true, &[]),
            Outcome::Success
        );
    }

    #[test]
    fn only_known_urls_count_when_the_list_is_full() {
        let known = ["http://gwc.example.org/"];
        assert_eq!(
            get("u|http://gwc.example.org/|0\n", true, &known),
            Outcome::Success
        );
        assert_eq!(
            get("u|HTTP://GWC.Example.org/|0\n", true, &known),
            Outcome::Success
        );
        // "/." does not end in '/', so no slash is restored after the trim
        // (:127-132) and it no longer matches.
        assert_eq!(
            get("u|http://gwc.example.org/.|0\n", true, &known),
            Outcome::Failure
        );
        assert_eq!(
            get("u|http://gwc.example.org//|0\n", true, &known),
            Outcome::Success
        );
        assert_eq!(
            get("u|http://other.example.org/|0\n", true, &known),
            Outcome::Failure
        );
        assert_eq!(
            get("u|http://gwc.example.org:80/|0\n", true, &known),
            Outcome::Failure
        );
        assert_eq!(
            get("u|http://gwc.example.org|0\n", true, &known),
            Outcome::Failure,
            "slash matters"
        );
    }

    #[test]
    fn long_known_urls_ignore_their_last_two_characters() {
        // DS:377-381
        let long = "http://gwc.example.org/some/long/path/to/skulls.php";
        assert!(long.len() > 45);
        let near = "http://gwc.example.org/some/long/path/to/skulls.pXX";
        assert_eq!(
            get(&format!("u|{near}|0\n"), true, &[long]),
            Outcome::Success
        );
    }

    #[test]
    fn inet_addr_rejects_the_broadcast_address() {
        assert_eq!(
            get("h|255.255.255.255:6346|5\n", true, &[]),
            Outcome::Failure
        );
    }

    #[test]
    fn update_keywords() {
        assert_eq!(update("i|update|OK\n"), Outcome::Success);
        assert_eq!(
            update("i|update|ok\n"),
            Outcome::Failure,
            "OK is case-sensitive"
        );
        assert_eq!(update("i|update|WARNING|whatever\n"), Outcome::Failure);
        assert_eq!(
            update("i|update|WARNING|x\ni|update|OK\n"),
            Outcome::Success
        );
        assert_eq!(update("i|error|x\ni|update|OK\n"), Outcome::Failure);
        assert_eq!(update("i|force|remove\n"), Outcome::Removed);
        assert_eq!(update(""), Outcome::Failure);
    }
}
