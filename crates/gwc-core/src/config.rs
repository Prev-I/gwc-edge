//! Runtime configuration, read from the Worker's environment variables.

use std::net::SocketAddrV4;

use crate::cache_url::{CacheUrl, normalize};
use crate::policy::{DEFAULT_DENYLIST, parse_denylist};

/// Hosts or caches per reply when not configured.
pub const DEFAULT_COUNT: usize = 20;

/// Largest configurable number of hosts or caches per reply.
pub const MAX_COUNT: usize = 50;

/// Validated configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// This cache's canonical public URL, as clients store it.
    pub canonical_url: CacheUrl,
    /// Hosts per hostfile reply, `1..=MAX_COUNT`.
    pub hostfile_count: usize,
    /// Caches per urlfile reply, `1..=MAX_COUNT`.
    pub urlfile_count: usize,
    /// Denied vendor codes, uppercased.
    pub denylist: Vec<String>,
    /// `GWC_TRACE=1`: the cron logs each hub's reply headers.
    pub trace: bool,
    /// `TEST_DIAL`, integration tests only (ADR-0012): where the cron dials
    /// instead of each hub IP or cache host. Loopback only; when set, a
    /// target it does not map is not dialled at all.
    pub test_dial: Option<Vec<(String, SocketAddrV4)>>,
}

/// Where the cron connects for a hub or a cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dial {
    /// The target itself.
    Direct,
    /// A loopback address from `TEST_DIAL`.
    Mapped(SocketAddrV4),
    /// `TEST_DIAL` is set and does not map the target: the check fails.
    Blocked,
}

impl Config {
    /// Where to dial for `target`, a hub IP or a cache host.
    #[must_use]
    pub fn dial(&self, target: &str) -> Dial {
        match &self.test_dial {
            None => Dial::Direct,
            Some(map) => map
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(target))
                .map_or(Dial::Blocked, |(_, a)| Dial::Mapped(*a)),
        }
    }
}

/// Why the configuration is unusable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// `CANONICAL_URL` is missing.
    MissingCanonicalUrl,
    /// `CANONICAL_URL` is not a URL Shareaza can use (it must be plain `http://`).
    BadCanonicalUrl(String),
    /// `TEST_DIAL` is malformed or maps to an address outside 127.0.0.0/8.
    BadTestDial,
}

impl Config {
    /// Read the configuration through `var`, which returns a variable's value.
    ///
    /// | Variable | Default |
    /// |---|---|
    /// | `CANONICAL_URL` | required |
    /// | `HOSTFILE_COUNT` | 20, clamped to 1..=50 |
    /// | `URLFILE_COUNT` | 20, clamped to 1..=50 |
    /// | `CLIENT_DENYLIST` | `CRUX` |
    /// | `GWC_TRACE` | off; `1` turns it on |
    /// | `TEST_DIAL` | unset; `target=127.x.y.z:port,...` (tests only) |
    ///
    /// # Errors
    ///
    /// See [`ConfigError`]. Unparsable counts fall back to the default.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
        let raw = var("CANONICAL_URL")
            .filter(|v| !v.trim().is_empty())
            .ok_or(ConfigError::MissingCanonicalUrl)?;
        let canonical = normalize(&raw).map_err(|_| ConfigError::BadCanonicalUrl(raw.clone()))?;
        let count = |name| {
            var(name)
                .and_then(|v| v.trim().parse::<usize>().ok())
                .map_or(DEFAULT_COUNT, |n| n.clamp(1, MAX_COUNT))
        };
        Ok(Config {
            canonical_url: canonical,
            hostfile_count: count("HOSTFILE_COUNT"),
            urlfile_count: count("URLFILE_COUNT"),
            denylist: var("CLIENT_DENYLIST").map_or_else(
                || parse_denylist(&DEFAULT_DENYLIST.join(",")),
                |v| parse_denylist(&v),
            ),
            trace: var("GWC_TRACE").is_some_and(|v| v.trim() == "1"),
            test_dial: var("TEST_DIAL")
                .filter(|v| !v.trim().is_empty())
                .map(|v| parse_test_dial(&v))
                .transpose()?,
        })
    }
}

/// `target=127.x.y.z:port` pairs, comma-separated.
fn parse_test_dial(raw: &str) -> Result<Vec<(String, SocketAddrV4)>, ConfigError> {
    raw.split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|entry| {
            let (target, addr) = entry.split_once('=').ok_or(ConfigError::BadTestDial)?;
            let addr: SocketAddrV4 = addr.trim().parse().map_err(|_| ConfigError::BadTestDial)?;
            let target = target.trim();
            if target.is_empty() || !addr.ip().is_loopback() {
                return Err(ConfigError::BadTestDial);
            }
            Ok((target.to_ascii_lowercase(), addr))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn defaults() {
        let c = Config::from_vars(vars(&[("CANONICAL_URL", "http://gwc.example.org/")])).unwrap();
        assert_eq!(c.canonical_url.url, "http://gwc.example.org/");
        assert_eq!((c.hostfile_count, c.urlfile_count), (20, 20));
        assert_eq!(c.denylist, ["CRUX"]);
    }

    #[test]
    fn counts_are_clamped_and_garbage_ignored() {
        let c = Config::from_vars(vars(&[
            ("CANONICAL_URL", "http://gwc.example.org/"),
            ("HOSTFILE_COUNT", "500"),
            ("URLFILE_COUNT", "0"),
        ]))
        .unwrap();
        assert_eq!((c.hostfile_count, c.urlfile_count), (MAX_COUNT, 1));
        let c = Config::from_vars(vars(&[
            ("CANONICAL_URL", "http://gwc.example.org/"),
            ("HOSTFILE_COUNT", "lots"),
            ("URLFILE_COUNT", " 7 "),
        ]))
        .unwrap();
        assert_eq!((c.hostfile_count, c.urlfile_count), (DEFAULT_COUNT, 7));
    }

    #[test]
    fn denylist_can_be_replaced_or_emptied() {
        let c = Config::from_vars(vars(&[
            ("CANONICAL_URL", "http://gwc.example.org/"),
            ("CLIENT_DENYLIST", "crux, foxy"),
        ]))
        .unwrap();
        assert_eq!(c.denylist, ["CRUX", "FOXY"]);
        let c = Config::from_vars(vars(&[
            ("CANONICAL_URL", "http://gwc.example.org/"),
            ("CLIENT_DENYLIST", ""),
        ]))
        .unwrap();
        assert_eq!(c.denylist, Vec::<String>::new());
    }

    #[test]
    fn the_shipped_wrangler_config_is_valid() {
        // A bad CANONICAL_URL fails every request; catch it before a deploy does.
        let cfg = include_str!("../../../wrangler.jsonc");
        let value = cfg
            .split("\"CANONICAL_URL\": \"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("CANONICAL_URL in wrangler.jsonc");
        let c = Config::from_vars(vars(&[("CANONICAL_URL", value)])).unwrap();
        assert_eq!(
            c.canonical_url.url, value,
            "CANONICAL_URL must already be in normalised form"
        );
    }

    #[test]
    fn test_dial_maps_targets_to_loopback_only_and_blocks_the_rest() {
        let base = [("CANONICAL_URL", "http://gwc.example.org/")];
        let c = Config::from_vars(vars(&base)).unwrap();
        assert_eq!(c.test_dial, None);
        assert_eq!(c.dial("8.8.4.4"), Dial::Direct);
        assert!(!c.trace);

        let c = Config::from_vars(vars(&[
            base[0],
            (
                "TEST_DIAL",
                " 8.8.4.4=127.0.0.1:18801, Cache.Example.ORG=127.0.0.2:18802 ",
            ),
            ("GWC_TRACE", "1"),
        ]))
        .unwrap();
        assert!(c.trace);
        assert_eq!(
            c.dial("8.8.4.4"),
            Dial::Mapped("127.0.0.1:18801".parse().unwrap())
        );
        assert_eq!(
            c.dial("cache.example.org"),
            Dial::Mapped("127.0.0.2:18802".parse().unwrap())
        );
        assert_eq!(c.dial("9.9.9.9"), Dial::Blocked, "fail closed");

        for bad in [
            "8.8.4.4=8.8.8.8:6346",
            "8.8.4.4=10.0.0.1:6346",
            "8.8.4.4",
            "=127.0.0.1:1",
            "8.8.4.4=127.0.0.1",
            "8.8.4.4=localhost:6346",
        ] {
            assert_eq!(
                Config::from_vars(vars(&[base[0], ("TEST_DIAL", bad)])),
                Err(ConfigError::BadTestDial),
                "{bad}"
            );
        }
        assert_eq!(
            Config::from_vars(vars(&[base[0], ("TEST_DIAL", "  ")]))
                .unwrap()
                .test_dial,
            None
        );
    }

    #[test]
    fn the_shipped_wrangler_config_has_no_test_dial() {
        for cfg in [
            include_str!("../../../wrangler.jsonc"),
            include_str!("../../../wrangler.no-d1.jsonc"),
        ] {
            assert!(
                !cfg.contains("\"TEST_DIAL\""),
                "TEST_DIAL is for tests only"
            );
        }
    }

    #[test]
    fn canonical_url_is_required_and_must_be_plain_http() {
        assert_eq!(
            Config::from_vars(vars(&[])),
            Err(ConfigError::MissingCanonicalUrl)
        );
        assert_eq!(
            Config::from_vars(vars(&[("CANONICAL_URL", "https://gwc.example.org/")])),
            Err(ConfigError::BadCanonicalUrl(
                "https://gwc.example.org/".into()
            ))
        );
        let c = Config::from_vars(vars(&[("CANONICAL_URL", "HTTP://GWC.Example.org")])).unwrap();
        assert_eq!(c.canonical_url.url, "http://gwc.example.org/", "normalised");
    }
}
