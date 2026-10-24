//! Pinging another cache (ADR-0012): how to reach it, and what counts as a
//! pong.
//!
//! `fetch()` cannot reach a host given as an IP address, and a non-standard
//! port is not reliably honoured, so only a DNS name on port 80 is fetched.
//! Every other cache is pinged with plain HTTP/1.0 over a TCP socket, which
//! does not follow redirects.

use std::net::Ipv4Addr;

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
/// cache's name as the version (Shareaza wiki, "Pinging Caches").
#[must_use]
pub fn query() -> String {
    let name: String = form_urlencoded::byte_serialize(crate::product_id().as_bytes()).collect();
    format!("ping=1&net=gnutella2&client=TEST&version={name}")
}

/// How to ping the cache at `url`, a URL normalised by `cache_url`.
#[must_use]
pub fn via(url: &str) -> Via {
    let (host, port, request) = socket_request(url);
    if port == 80 && host.parse::<Ipv4Addr>().is_err() {
        return Via::Fetch(format!("{url}?{}", query()));
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
        query(),
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
    let end = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| bytes.windows(2).position(|w| w == b"\n\n").map(|i| i + 2))?;
    let head = std::str::from_utf8(&bytes[..end]).ok()?;
    let status = head.lines().next()?.strip_prefix("HTTP/1.")?;
    let code = status.get(2..5)?;
    if !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((code.parse().ok()?, &bytes[end..]))
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
    let text = String::from_utf8_lossy(body);
    text.split(['\r', '\n']).find_map(|line| {
        let fields: Vec<&str> = line.split('|').map(str::trim).collect();
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
    })
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
        assert!(query().starts_with("ping=1&net=gnutella2&client=TEST&version=gwc-edge"));
        assert!(!query().contains(' '));
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
}
