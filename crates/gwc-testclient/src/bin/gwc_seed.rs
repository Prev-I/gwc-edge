//! Seed a new cache from another one (`docs/field-test-plan.md`): ask it for
//! its G2 hosts and caches, once each, and print SQL that stores what this
//! cache's own rules accept, for the verifying cron to check. Nothing it
//! writes is served before that check (`gwc_core::seed`).
//!
//! Usage: `gwc_seed --self <canonical URL> [--from <cache URL>] > seed.sql`
//! `scripts/seed.sh` runs it and applies the SQL. It fails, printing no SQL,
//! when the source offers no hub at all: an error page or a refusal must not
//! look like a seed.

use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gwc_core::cache_url::normalize;
use gwc_core::seed::{MAX_SEED_HUBS, Seed};

/// The one other cache known to be alive.
const DKAC: &str = "http://dkac.trillinux.org/dkac/dkac.php";

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
    seed(&self_url, &from).map_err(Failure::Run)
}

fn parse_args(args: Vec<String>) -> Result<(gwc_core::cache_url::CacheUrl, String), String> {
    let (mut from, mut self_url) = (DKAC.to_owned(), None);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{arg} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--from" => from = value()?,
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
    if !from.starts_with("http://") || from.contains(['?', '#']) {
        return Err(format!("--from must be a plain http:// URL: {from}"));
    }
    Ok((self_url, from))
}

fn seed(self_url: &gwc_core::cache_url::CacheUrl, from: &str) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .user_agent(gwc_core::product_id())
        .build()
        .into();
    // As caches ask each other: `client=TEST`, this cache's name as version.
    let version = format!("{} (seed)", gwc_core::product_id());
    let mut seed = Seed::default();
    for file in ["hostfile", "urlfile"] {
        let body = agent
            .get(from)
            .query("get", "1")
            .query(file, "1")
            .query("net", "gnutella2")
            .query("client", "TEST")
            .query("version", &version)
            .call()
            .map_err(|e| format!("{from} ({file}): {e}"))?
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("{from} ({file}): {e}"))?;
        seed.read(&body, self_url);
    }
    // The source itself is a cache too, and its own urlfile does not list it.
    if let Ok(source) = normalize(from) {
        seed.add_cache(source, self_url);
    }
    if seed.hubs.is_empty() {
        return Err(format!(
            "{from} offered no usable hub; nothing seeded (is it answering G2 gets?)"
        ));
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    eprintln!(
        "gwc_seed: {} hubs (at most {MAX_SEED_HUBS} kept) and {} caches offered by {from}; \
         the caps may store fewer, and only the cron's checks list them",
        seed.hubs.len(),
        seed.caches.len(),
    );
    print!("{}", seed.sql(now));
    Ok(())
}
