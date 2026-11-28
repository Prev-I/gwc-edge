//! Seed a new cache from another one (`docs/field-test-plan.md`): ask it for
//! its G2 hosts and caches, once each, and print SQL that stores what this
//! cache's own rules accept, for the verifying cron to check. Nothing it
//! writes is served before that check (`gwc_core::seed`).
//!
//! Usage: `gwc_seed --self <canonical URL> [--from <cache URL>] > seed.sql`
//! `scripts/seed.sh` runs it and applies the SQL.
//!
//! Without `--from`, it asks the known caches (`gwc_core::seed::KNOWN_CACHES`)
//! in order until one offers a hub, and adds them all as caches (ADR-0027).
//! With `--from`, it asks that cache alone. It fails, printing no SQL, when
//! no source offers a hub: an error page or a refusal must not look like a
//! seed.

use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gwc_core::cache_url::normalize;
use gwc_core::seed::{MAX_SEED_HUBS, Seed, choose};

const USAGE: &str = "usage: gwc_seed --self <canonical URL> [--from <cache URL>]";

/// Why a run failed: exit 2 for a usage error, 1 for anything else.
enum Failure {
    Usage(String),
    Run(String),
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(e)) => {
            eprintln!("gwc_seed: {e}");
            ExitCode::from(2)
        }
        Err(Failure::Run(e)) => {
            eprintln!("gwc_seed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), Failure> {
    let (self_url, from) = parse_args(args).map_err(Failure::Usage)?;
    seed(&self_url, from.as_deref()).map_err(Failure::Run)
}

fn parse_args(
    args: Vec<String>,
) -> Result<(gwc_core::cache_url::CacheUrl, Option<String>), String> {
    let (mut from, mut self_url) = (None, None);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{arg} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--from" => from = Some(value()?),
            "--self" => {
                let v = value()?;
                self_url = Some(normalize(&v).map_err(|_| format!("bad --self URL: {v}"))?);
            }
            _ => return Err(USAGE.to_owned()),
        }
    }
    let self_url = self_url.ok_or(USAGE)?;
    // The operator names the source; it only has to be plain HTTP. (A test
    // points it at a local fake cache, which the cache-URL rules refuse.)
    if let Some(from) = &from
        && (!from.starts_with("http://") || from.contains(['?', '#']))
    {
        return Err(format!("--from must be a plain http:// URL: {from}"));
    }
    Ok((self_url, from))
}

fn seed(self_url: &gwc_core::cache_url::CacheUrl, named: Option<&str>) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .user_agent(gwc_core::product_id())
        .build()
        .into();
    // As caches ask each other: `client=TEST`, this cache's name as version.
    let version = format!("{} (seed)", gwc_core::product_id());
    let (from, seed) = choose(
        named,
        self_url,
        |from, seed| ask(&agent, from, &version, self_url, seed),
        |note| eprintln!("gwc_seed: {note}"),
    )?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    eprintln!(
        "gwc_seed: {} hubs (at most {MAX_SEED_HUBS} kept) from {from}, and {} caches \
         (the known ones first when no source was named); the caps may store fewer, \
         and only the cron's checks list them",
        seed.hubs.len(),
        seed.caches.len(),
    );
    print!("{}", seed.sql(now));
    Ok(())
}

/// One source's G2 hostfile and urlfile, added to `seed`, plus the source
/// itself: a cache too, and one its own urlfile does not list.
fn ask(
    agent: &ureq::Agent,
    from: &str,
    version: &str,
    self_url: &gwc_core::cache_url::CacheUrl,
    mut seed: Seed,
) -> Result<Seed, String> {
    let hubs_before = seed.hubs.len();
    for file in ["hostfile", "urlfile"] {
        let body = agent
            .get(from)
            .query("get", "1")
            .query(file, "1")
            .query("net", "gnutella2")
            .query("client", "TEST")
            .query("version", version)
            .call()
            .map_err(|e| format!("{from} ({file}): {e}"))?
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("{from} ({file}): {e}"))?;
        seed.read(&body, self_url);
        // No hub, no seed from here: spare the urlfile request.
        if seed.hubs.len() == hubs_before {
            return Ok(seed);
        }
    }
    if let Ok(source) = normalize(from) {
        seed.add_cache(source, self_url);
    }
    Ok(seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn no_source_named_means_the_known_caches() {
        let (_, from) = parse_args(args(&["--self", "http://gwc.example.org/"])).unwrap();
        assert_eq!(from, None);
    }

    #[test]
    fn a_named_source_is_parsed_and_must_be_plain_http() {
        let (_, from) = parse_args(args(&[
            "--from",
            "http://127.0.0.1:9/gwc/",
            "--self",
            "http://gwc.example.org/",
        ]))
        .unwrap();
        assert_eq!(from.as_deref(), Some("http://127.0.0.1:9/gwc/"));
        assert!(
            parse_args(args(&[
                "--from",
                "https://x.example/",
                "--self",
                "http://gwc.example.org/"
            ]))
            .is_err()
        );
    }
}
