//! Runtime configuration, read from the Worker's environment variables.

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
    pub self_url: CacheUrl,
    /// Hosts per hostfile reply, `1..=MAX_COUNT`.
    pub hostfile_count: usize,
    /// Caches per urlfile reply, `1..=MAX_COUNT`.
    pub urlfile_count: usize,
    /// Denied vendor codes, uppercased.
    pub denylist: Vec<String>,
}

/// Why the configuration is unusable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// `SELF_URL` is missing.
    MissingSelfUrl,
    /// `SELF_URL` is not a URL Shareaza can use (it must be plain `http://`).
    BadSelfUrl(String),
}

impl Config {
    /// Read the configuration through `var`, which returns a variable's value.
    ///
    /// | Variable | Default |
    /// |---|---|
    /// | `SELF_URL` | required |
    /// | `HOSTFILE_COUNT` | 20, clamped to 1..=50 |
    /// | `URLFILE_COUNT` | 20, clamped to 1..=50 |
    /// | `CLIENT_DENYLIST` | `CRUX` |
    ///
    /// # Errors
    ///
    /// See [`ConfigError`]. Unparsable counts fall back to the default.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
        let raw = var("SELF_URL")
            .filter(|v| !v.trim().is_empty())
            .ok_or(ConfigError::MissingSelfUrl)?;
        let self_url = normalize(&raw).map_err(|_| ConfigError::BadSelfUrl(raw.clone()))?;
        let count = |name| {
            var(name)
                .and_then(|v| v.trim().parse::<usize>().ok())
                .map_or(DEFAULT_COUNT, |n| n.clamp(1, MAX_COUNT))
        };
        Ok(Config {
            self_url,
            hostfile_count: count("HOSTFILE_COUNT"),
            urlfile_count: count("URLFILE_COUNT"),
            denylist: var("CLIENT_DENYLIST").map_or_else(
                || parse_denylist(&DEFAULT_DENYLIST.join(",")),
                |v| parse_denylist(&v),
            ),
        })
    }
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
        let c = Config::from_vars(vars(&[("SELF_URL", "http://gwc.example.org/")])).unwrap();
        assert_eq!(c.self_url.url, "http://gwc.example.org/");
        assert_eq!((c.hostfile_count, c.urlfile_count), (20, 20));
        assert_eq!(c.denylist, ["CRUX"]);
    }

    #[test]
    fn counts_are_clamped_and_garbage_ignored() {
        let c = Config::from_vars(vars(&[
            ("SELF_URL", "http://gwc.example.org/"),
            ("HOSTFILE_COUNT", "500"),
            ("URLFILE_COUNT", "0"),
        ]))
        .unwrap();
        assert_eq!((c.hostfile_count, c.urlfile_count), (MAX_COUNT, 1));
        let c = Config::from_vars(vars(&[
            ("SELF_URL", "http://gwc.example.org/"),
            ("HOSTFILE_COUNT", "lots"),
            ("URLFILE_COUNT", " 7 "),
        ]))
        .unwrap();
        assert_eq!((c.hostfile_count, c.urlfile_count), (DEFAULT_COUNT, 7));
    }

    #[test]
    fn denylist_can_be_replaced_or_emptied() {
        let c = Config::from_vars(vars(&[
            ("SELF_URL", "http://gwc.example.org/"),
            ("CLIENT_DENYLIST", "crux, foxy"),
        ]))
        .unwrap();
        assert_eq!(c.denylist, ["CRUX", "FOXY"]);
        let c = Config::from_vars(vars(&[
            ("SELF_URL", "http://gwc.example.org/"),
            ("CLIENT_DENYLIST", ""),
        ]))
        .unwrap();
        assert_eq!(c.denylist, Vec::<String>::new());
    }

    #[test]
    fn the_shipped_wrangler_config_is_valid() {
        // A bad SELF_URL fails every request; catch it before a deploy does.
        let cfg = include_str!("../../../wrangler.jsonc");
        let value = cfg
            .split("\"SELF_URL\": \"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("SELF_URL in wrangler.jsonc");
        let c = Config::from_vars(vars(&[("SELF_URL", value)])).unwrap();
        assert_eq!(
            c.self_url.url, value,
            "SELF_URL must already be in normalised form"
        );
    }

    #[test]
    fn self_url_is_required_and_must_be_plain_http() {
        assert_eq!(
            Config::from_vars(vars(&[])),
            Err(ConfigError::MissingSelfUrl)
        );
        assert_eq!(
            Config::from_vars(vars(&[("SELF_URL", "https://gwc.example.org/")])),
            Err(ConfigError::BadSelfUrl("https://gwc.example.org/".into()))
        );
        let c = Config::from_vars(vars(&[("SELF_URL", "HTTP://GWC.Example.org")])).unwrap();
        assert_eq!(c.self_url.url, "http://gwc.example.org/", "normalised");
    }
}
