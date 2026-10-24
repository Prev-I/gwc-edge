//! A GWebCache client that sends exactly what Shareaza 2.7.10.6 sends, for
//! integration tests against `wrangler dev`.
//!
//! The query strings are Shareaza's (`DiscoveryServices.cpp:1413-1434` for
//! gets, `:1824-1870` for updates, at ansani/Shareaza@f24b0ce); only the
//! values vary. Nothing here depends on `gwc-core`, so the tests check the
//! cache from the outside.

use std::time::Duration;

/// Shareaza's User-Agent (`<client name> <version>`, `Network.cpp:238-242`).
pub const USER_AGENT: &str = "Shareaza 2.7.10.6";

/// Shareaza's vendor code and version, glued as it sends them on G2.
pub const CLIENT: &str = "RAZA2.7.10.6";

/// A response, as Shareaza's parser sees it.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub content_type: Option<String>,
    pub cache_control: Option<String>,
    /// Every header, names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: String,
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

    /// Non-empty lines, trimmed the way Shareaza trims them.
    #[must_use]
    pub fn lines(&self) -> Vec<&str> {
        self.body
            .split(['\r', '\n'])
            .map(|l| l.trim_matches([' ', '\t']))
            .filter(|l| !l.is_empty())
            .collect()
    }

    /// Lines starting with `prefix`.
    #[must_use]
    pub fn with_prefix(&self, prefix: &str) -> Vec<&str> {
        self.lines()
            .into_iter()
            .filter(|l| l.starts_with(prefix))
            .collect()
    }
}

/// A client bound to one cache URL.
///
/// Unless a request sets `CF-Connecting-IP` itself, each one carries a
/// different source address (an IPv6 /64 from the documentation range), so
/// the cache's per-source rate limits never throttle unrelated tests. A test
/// that needs a hub's own address sets the header.
pub struct Client {
    base: String,
    agent: ureq::Agent,
    next_source: std::cell::Cell<u32>,
}

impl Client {
    /// `base` is the cache URL as stored, e.g. `http://127.0.0.1:18787/`.
    #[must_use]
    pub fn new(base: &str) -> Client {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // A 301 must be seen, not followed: the tests check it.
            .max_redirects(0)
            // A cron run answers only when it is done, and a stalled hub
            // takes it 8 s (`verify::HANDSHAKE_TIMEOUT_MS`).
            .timeout_global(Some(Duration::from_secs(30)))
            .user_agent(USER_AGENT)
            .build()
            .into();
        // A different starting point per client, so clients in different
        // test binaries do not reuse each other's sources.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos())
            ^ std::process::id().rotate_left(16);
        Client {
            base: base.to_owned(),
            agent,
            next_source: std::cell::Cell::new(seed),
        }
    }

    /// GET `<base><suffix>` where `suffix` is a path or `?query`.
    ///
    /// # Panics
    ///
    /// If the request cannot be made at all.
    #[must_use]
    pub fn get(&self, suffix: &str) -> Reply {
        self.request("GET", suffix, &[])
    }

    /// Any method, with extra headers (e.g. `Host`, `Accept`), as a browser
    /// or another tool would send it.
    ///
    /// # Panics
    ///
    /// If the request cannot be made at all, or the method is unsupported.
    #[must_use]
    pub fn request(&self, method: &str, suffix: &str, headers: &[(&str, &str)]) -> Reply {
        let url = format!("{}{}", self.base, suffix.trim_start_matches('/'));
        let mut req = ureq::http::Request::builder().method(method).uri(&url);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        if !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("CF-Connecting-IP"))
        {
            let n = self.next_source.get();
            self.next_source.set(n.wrapping_add(1));
            req = req.header(
                "CF-Connecting-IP",
                format!("2001:db8:{:x}:{:x}::1", n >> 16, n & 0xffff),
            );
        }
        let req = req
            .body(())
            .unwrap_or_else(|e| panic!("{method} {url}: {e}"));
        let mut resp = self
            .agent
            .run(req)
            .unwrap_or_else(|e| panic!("{method} {url}: {e}"));
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_ascii_lowercase(),
                    v.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        let find = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        let (content_type, cache_control) = (find("content-type"), find("cache-control"));
        let status = resp.status().as_u16();
        let body = if method == "HEAD" {
            String::new()
        } else {
            resp.body_mut()
                .read_to_string()
                .unwrap_or_else(|e| panic!("body of {url}: {e}"))
        };
        Reply {
            status,
            content_type,
            cache_control,
            headers,
            body,
        }
    }

    /// Shareaza's G2 hostfile query, `ping=1` included.
    #[must_use]
    pub fn hostfile(&self) -> Reply {
        self.get(&format!(
            "?get=1&hostfile=1&net=gnutella2&ping=1&pv=4&client={CLIENT}{GET_TAIL}"
        ))
    }

    /// Shareaza's G2 urlfile query.
    #[must_use]
    pub fn urlfile(&self) -> Reply {
        self.get(&format!(
            "?get=1&urlfile=1&net=gnutella2&ping=1&pv=4&client={CLIENT}{GET_TAIL}"
        ))
    }

    /// Shareaza's G1 hostfile query (spec-1 `version=`, no ping).
    #[must_use]
    pub fn g1_hostfile(&self) -> Reply {
        self.get(&format!(
            "?get=1&hostfile=1&net=gnutella&client=RAZA&version=2.7.10.6{GET_TAIL}"
        ))
    }

    /// Shareaza's hub update: `ip=` first, counters, optional submitted URL.
    /// Sent from a fresh source address, not the hub's own.
    #[must_use]
    pub fn update(
        &self,
        ip_port: &str,
        leaves: u32,
        uptime: u32,
        max: u32,
        url: Option<&str>,
    ) -> Reply {
        self.request("GET", &update_query(ip_port, leaves, uptime, max, url), &[])
    }

    /// The same update, sent from `source`: the hub's own address when
    /// `source` is the IP of `ip_port`.
    #[must_use]
    pub fn update_from(
        &self,
        source: &str,
        ip_port: &str,
        leaves: u32,
        uptime: u32,
        max: u32,
        url: Option<&str>,
    ) -> Reply {
        self.request(
            "GET",
            &update_query(ip_port, leaves, uptime, max, url),
            &[("CF-Connecting-IP", source)],
        )
    }

    /// Shareaza's submit: a cache URL alone (`DiscoveryServices.cpp:1837-1841`).
    #[must_use]
    pub fn submit(&self, url: &str) -> Reply {
        self.get(&format!(
            "?url={}&update=1&net=gnutella2&client={CLIENT}",
            encode(url)
        ))
    }
}

fn update_query(ip_port: &str, leaves: u32, uptime: u32, max: u32, url: Option<&str>) -> String {
    let url = url.map_or_else(String::new, |u| format!("&url={}", encode(u)));
    format!(
        "?ip={ip_port}&x.leaves={leaves}&uptime={uptime}&x.max={max}{url}&update=1&net=gnutella2&client={CLIENT}"
    )
}

/// The GWC 2.1 extras Shareaza appends to every get (`:1434`).
const GET_TAIL: &str = "&getleaves=1&getnetworks=1&getclusters=0&getvendors=1&getuptime=1";

/// Percent-encode as Shareaza's `URLEncode` does for URLs (`Strings.cpp`):
/// everything but unreserved characters.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_shareaza() {
        assert_eq!(
            encode("http://gwc.example.org/skulls.php"),
            "http%3A%2F%2Fgwc.example.org%2Fskulls.php"
        );
    }

    #[test]
    fn splits_lines_like_shareaza() {
        let r = Reply {
            status: 200,
            content_type: None,
            cache_control: None,
            headers: vec![("x-a".into(), "1".into())],
            body: "a\r\n\r\n b \nc".into(),
        };
        assert_eq!(r.header("X-A"), Some("1"));
        assert_eq!(r.lines(), ["a", "b", "c"]);
    }
}

/// Reading the `wrangler dev` output `scripts/integration.sh` captures. Each
/// test looks only at what was written after it started.
pub mod logs {
    use std::time::{Duration, Instant};

    /// Current size of a log file: the offset a test starts reading from.
    #[must_use]
    pub fn len(path: &str) -> usize {
        std::fs::metadata(path).map_or(0, |m| usize::try_from(m.len()).unwrap_or(0))
    }

    /// The JSON objects in a log from byte `from` on, colour codes and
    /// prefixes removed.
    #[must_use]
    pub fn json_since(path: &str, from: usize) -> Vec<serde_json::Value> {
        let raw = std::fs::read(path).unwrap_or_default();
        String::from_utf8_lossy(raw.get(from..).unwrap_or_default())
            .lines()
            .filter_map(|l| {
                let start = l.find('{')?;
                let end = l.rfind('}')?;
                serde_json::from_str(&l[start..=end]).ok()
            })
            .collect()
    }

    /// Lines matching `pred` written since `from`: wait until `want` appear,
    /// then a little longer, so a late duplicate is counted too.
    pub fn settle(
        path: &str,
        from: usize,
        want: usize,
        pred: impl Fn(&serde_json::Value) -> bool,
    ) -> Vec<serde_json::Value> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while json_since(path, from).iter().filter(|v| pred(v)).count() < want
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(100));
        }
        std::thread::sleep(Duration::from_millis(500));
        json_since(path, from)
            .into_iter()
            .filter(|v| pred(v))
            .collect()
    }

    /// A request line: the only lines with an `outcome`.
    #[must_use]
    pub fn is_request_line(v: &serde_json::Value) -> bool {
        v.get("outcome").is_some()
    }
}
