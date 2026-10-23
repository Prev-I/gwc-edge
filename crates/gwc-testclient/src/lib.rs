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
    pub body: String,
}

impl Reply {
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
pub struct Client {
    base: String,
    agent: ureq::Agent,
}

impl Client {
    /// `base` is the cache URL as stored, e.g. `http://127.0.0.1:18787/`.
    #[must_use]
    pub fn new(base: &str) -> Client {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .user_agent(USER_AGENT)
            .build()
            .into();
        Client {
            base: base.to_owned(),
            agent,
        }
    }

    /// GET `<base><suffix>` where `suffix` is a path or `?query`.
    ///
    /// # Panics
    ///
    /// If the request cannot be made at all.
    #[must_use]
    pub fn get(&self, suffix: &str) -> Reply {
        let url = format!("{}{}", self.base, suffix.trim_start_matches('/'));
        let mut resp = self
            .agent
            .get(&url)
            .call()
            .unwrap_or_else(|e| panic!("GET {url}: {e}"));
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let (content_type, cache_control) = (header("content-type"), header("cache-control"));
        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .read_to_string()
            .unwrap_or_else(|e| panic!("body of {url}: {e}"));
        Reply {
            status,
            content_type,
            cache_control,
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
    #[must_use]
    pub fn update(
        &self,
        ip_port: &str,
        leaves: u32,
        uptime: u32,
        max: u32,
        url: Option<&str>,
    ) -> Reply {
        let url = url.map_or_else(String::new, |u| format!("&url={}", encode(u)));
        self.get(&format!(
            "?ip={ip_port}&x.leaves={leaves}&uptime={uptime}&x.max={max}{url}&update=1&net=gnutella2&client={CLIENT}"
        ))
    }
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
            body: "a\r\n\r\n b \nc".into(),
        };
        assert_eq!(r.lines(), ["a", "b", "c"]);
    }
}
