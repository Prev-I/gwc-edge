//! Seeding a new cache from another one (`docs/field-test-plan.md`): read
//! its hostfile and urlfile replies, keep what this cache's own rules accept,
//! and write SQL that stores it for the verifying cron.
//!
//! ADR-0016. Nothing seeded is served before the cron has checked it. Hubs go in as
//! `unverified` with source `seed`, so the cron's first handshake decides
//! (ADR-0011, ADR-0012); they share the `new` queue with self-registrations,
//! so a full seed of 100 hubs takes about an hour of cron runs. Caches go in
//! as `pending` until a ping answers. The caps are enforced inside each
//! statement, against the table as it is when the SQL runs.

use std::collections::HashSet;
use std::fmt::Write;
use std::net::{Ipv4Addr, SocketAddrV4};

use crate::cache_url::{CacheUrl, normalize};
use crate::update::{MAX_CACHES, MAX_HUBS, MAX_PER_DOMAIN, is_self, reconcile_sql};
use crate::verify::dead_cache_floor;

/// Most hubs taken from one seeding.
pub const MAX_SEED_HUBS: usize = 100;

/// What a cache's replies offered that this cache would store.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Seed {
    /// Hubs, with their age in seconds as the other cache reported it.
    pub hubs: Vec<(SocketAddrV4, u64)>,
    pub caches: Vec<CacheUrl>,
    /// Addresses and URLs already taken, so a long reply stays linear.
    seen_ips: HashSet<Ipv4Addr>,
    seen_urls: HashSet<String>,
}

impl Seed {
    /// Read a GWebCache v2 reply. Lines are split on CR or LF and fields on
    /// `|`, trimmed, the first field matched case-insensitively, as Shareaza
    /// does. Hosts must pass the hub-address rules (public IPv4, port 1024 or
    /// more); URLs the cache-URL rules. `self_url` is never kept. Of an
    /// address listed twice, the freshest entry is kept; anything else is
    /// ignored.
    pub fn read(&mut self, body: &str, self_url: &CacheUrl) {
        for line in body.split(['\r', '\n']) {
            let fields: Vec<&str> = line.split('|').map(str::trim).collect();
            match fields.as_slice() {
                [kind, addr, age, ..] if kind.eq_ignore_ascii_case("h") => {
                    if let (Ok(addr), Ok(age)) = (crate::addr::parse_hub(addr), age.parse::<u64>())
                    {
                        self.add_hub(addr, age);
                    }
                }
                [kind, url, ..] if kind.eq_ignore_ascii_case("u") => {
                    if let Ok(url) = normalize(url) {
                        self.add_cache(url, self_url);
                    }
                }
                _ => {}
            }
        }
    }

    fn add_hub(&mut self, addr: SocketAddrV4, age: u64) {
        if self.seen_ips.insert(*addr.ip()) {
            self.hubs.push((addr, age));
        } else if let Some(known) = self
            .hubs
            .iter_mut()
            .find(|(a, old)| a.ip() == addr.ip() && *old > age)
        {
            *known = (addr, age);
        }
    }

    /// Keep a cache URL unless it is this cache or already kept: the source
    /// cache itself, say, which its own urlfile does not list.
    pub fn add_cache(&mut self, url: CacheUrl, self_url: &CacheUrl) {
        if !is_self(&url, self_url) && self.seen_urls.insert(url.url.clone()) {
            self.caches.push(url);
        }
    }

    /// SQL storing the seed: one statement per row, each with its cap in its
    /// `WHERE`, `ON CONFLICT DO NOTHING` for rows already known, then the
    /// cap counters reconciled. At most [`MAX_SEED_HUBS`] hubs, the freshest
    /// ones, and [`MAX_PER_DOMAIN`] caches per host. Seeded hubs get
    /// `last_seen = now`: the other cache's word is not a sighting, and an
    /// older `last_seen` would queue them ahead of real registrations.
    #[must_use]
    pub fn sql(&self, now: u64) -> String {
        let mut hubs = self.hubs.clone();
        hubs.sort_by_key(|(addr, age)| (*age, *addr));
        hubs.truncate(MAX_SEED_HUBS);

        let mut out = String::new();
        for (addr, _) in hubs {
            let _ = writeln!(
                out,
                "INSERT INTO hubs (ip, port, status, source, first_seen, last_seen) \
                 SELECT '{}', {}, 'unverified', 'seed', {now}, {now} \
                 WHERE (SELECT COUNT(*) FROM hubs WHERE status <> 'dead') < {MAX_HUBS} \
                 ON CONFLICT (ip) DO NOTHING;",
                addr.ip(),
                addr.port(),
            );
        }
        let mut per_host: Vec<&str> = Vec::new();
        for cache in &self.caches {
            if per_host.iter().filter(|h| **h == cache.host).count() >= MAX_PER_DOMAIN {
                continue;
            }
            per_host.push(&cache.host);
            let _ = writeln!(
                out,
                "INSERT INTO caches (url, host, status, first_seen) \
                 SELECT '{url}', '{host}', 'pending', {now} \
                 WHERE (SELECT COUNT(*) FROM caches) < {MAX_CACHES} \
                 AND (SELECT COUNT(*) FROM caches WHERE host = '{host}') < {MAX_PER_DOMAIN} \
                 AND NOT EXISTS (SELECT 1 FROM dead_caches \
                 WHERE url = '{url}' AND died >= {live}) \
                 ON CONFLICT (url) DO NOTHING;",
                url = quote(&cache.url),
                host = quote(&cache.host),
                live = dead_cache_floor(now),
            );
        }
        for statement in reconcile_sql() {
            out.push_str(&statement);
            out.push_str(";\n");
        }
        out
    }
}

/// A value for a single-quoted SQL literal. Cache URLs may contain `'`.
fn quote(s: &str) -> String {
    s.replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::DEAD_TTL;

    const NOW: u64 = 1_792_744_200;

    /// A reply as dkac would give it, with what this cache must refuse.
    const REPLY: &str = "i|pong|DKAC/Enticing-Enumon|gnutella-gnutella2\r\n\
        h|8.8.4.4:6346|120|||RAZA|9000|300\r\n\
        H|1.1.1.1:6347|30\r\n\
        h|8.8.4.4:6347|10\r\n\
        h|1.1.1.1:6347|900\r\n\
        h|192.168.1.2:6346|5\r\n\
        h|9.9.9.9:80|5\r\n\
        h|garbage|5\r\n\
        h|4.2.2.1:6346|old\r\n\
        u|http://other.example.net/gwc/|60\r\n\
        U|HTTP://Other.Example.net/gwc/|60\r\n\
        u|https://tls.example.net/|60\r\n\
        u|http://gwc.example.org/elsewhere|60\r\n\
        u|http://quote.example.net/it's/|60\r\n\
        i|update|OK\r\n";

    fn canonical() -> CacheUrl {
        normalize("http://gwc.example.org/").unwrap()
    }

    fn read(body: &str) -> Seed {
        let mut seed = Seed::default();
        seed.read(body, &canonical());
        seed
    }

    #[test]
    fn only_what_this_cache_would_store_is_kept() {
        let seed = read(REPLY);
        assert_eq!(
            seed.hubs,
            [
                ("8.8.4.4:6347".parse().unwrap(), 10),
                ("1.1.1.1:6347".parse().unwrap(), 30)
            ],
            "one per IP, its freshest entry; public, port >= 1024"
        );
        let urls: Vec<&str> = seed.caches.iter().map(|c| c.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "http://other.example.net/gwc/",
                "http://quote.example.net/it's/"
            ],
            "normalised, deduplicated, no https, never this cache"
        );
    }

    #[test]
    fn the_sql_stores_rows_for_the_cron_within_the_caps() {
        let sql = read(REPLY).sql(NOW);
        let lines: Vec<&str> = sql.lines().collect();
        assert_eq!(lines.len(), 2 + 2 + 3, "hubs, caches, reconciles");
        assert!(
            lines[0]
                .contains("SELECT '8.8.4.4', 6347, 'unverified', 'seed', 1792744200, 1792744200"),
            "freshest first, seen now: {}",
            lines[0]
        );
        assert!(lines[1].contains("'1.1.1.1', 6347"));
        for hub in &lines[..2] {
            assert!(hub.contains("WHERE (SELECT COUNT(*) FROM hubs WHERE status <> 'dead') < 500"));
            assert!(hub.ends_with("ON CONFLICT (ip) DO NOTHING;"));
        }
        assert!(
            lines[3].contains("'http://quote.example.net/it''s/'"),
            "quoted: {}",
            lines[3]
        );
        assert!(lines[2].contains("(SELECT COUNT(*) FROM caches) < 50"));
        assert!(lines[2].contains("WHERE host = 'other.example.net') < 3"));
        assert!(lines[4].starts_with("UPDATE counts SET n = t.c"));
    }

    #[test]
    fn a_cache_the_cron_deleted_lately_is_not_seeded() {
        // ADR-0022: the same block as a submission, read the same way.
        let sql = read(REPLY).sql(NOW);
        let guard = format!(
            "AND NOT EXISTS (SELECT 1 FROM dead_caches \
             WHERE url = 'http://other.example.net/gwc/' AND died >= {})",
            NOW - DEAD_TTL
        );
        let cache = sql
            .lines()
            .find(|l| l.contains("'http://other.example.net/gwc/'"))
            .expect("the cache is seeded");
        assert!(cache.contains(&guard), "{cache}");
        assert!(cache.ends_with("ON CONFLICT (url) DO NOTHING;"), "{cache}");
    }

    #[test]
    fn a_seed_takes_at_most_its_share() {
        let many: String = (0..150)
            .map(|i| format!("h|8.{}.{}.1:6346|{i}\n", i / 100 + 1, i % 100))
            .chain((0..5).map(|i| format!("u|http://one.example.net/{i}/|1\n")))
            .collect();
        let sql = read(&many).sql(NOW);
        assert_eq!(sql.matches("INSERT INTO hubs").count(), MAX_SEED_HUBS);
        assert_eq!(
            sql.matches("INSERT INTO caches").count(),
            MAX_PER_DOMAIN,
            "three per host"
        );
        assert!(!sql.contains("'8.2.49.1'"), "the stalest are left out");
    }

    #[test]
    fn the_source_cache_can_be_added_but_never_this_one() {
        let mut seed = Seed::default();
        let dkac = normalize("http://dkac.trillinux.org/dkac/dkac.php").unwrap();
        seed.add_cache(dkac.clone(), &canonical());
        seed.add_cache(
            normalize("http://gwc.example.org/other/").unwrap(),
            &canonical(),
        );
        seed.read(
            "u|http://dkac.trillinux.org/dkac/dkac.php|5\n",
            &canonical(),
        );
        assert_eq!(seed.caches, [dkac]);
    }

    #[test]
    fn an_empty_or_error_reply_seeds_nothing() {
        for body in [
            "",
            "ERROR No network\n",
            "i|pong|x\n",
            "<html>blocked</html>",
        ] {
            let seed = read(body);
            assert_eq!((seed.hubs.len(), seed.caches.len()), (0, 0), "{body}");
            assert_eq!(seed.sql(NOW).matches("INSERT").count(), 0);
        }
    }
}
