//! Read path, end to end, against a running cache seeded by
//! `scripts/integration.sh`:
//!
//! | hub | status | last seen | expected |
//! |---|---|---|---|
//! | 1.1.1.1:6347 | `alive_full` | 30 s ago | served first |
//! | 8.8.4.4:6346 | `verified`, RAZA, 120/300 leaves, up 2.5 h | 120 s ago | served second, extended fields |
//! | 9.9.9.9:6346 | `unverified` | now | not served |
//! | 8.8.8.8:6346 | `verified` | 8000 s ago | expired, not served |
//!
//! Caches: dkac verified 600 s ago (served), `pending.example.org` (not).
//!
//! Run with `scripts/integration.sh`; the tests are ignored by plain
//! `cargo test` because they need the server.

use gwc_testclient::{Client, Reply};

fn client() -> Client {
    Client::new(&std::env::var("GWC_BASE_URL").expect("GWC_BASE_URL (run scripts/integration.sh)"))
}

fn self_line() -> String {
    let url = std::env::var("GWC_SELF_URL").expect("GWC_SELF_URL (run scripts/integration.sh)");
    format!("u|{url}|0")
}

/// Seconds since the script seeded the database: seeded ages and uptimes have
/// grown by this much when the test reads them.
fn elapsed() -> u64 {
    let seeded: u64 = std::env::var("GWC_SEED_AT")
        .expect("GWC_SEED_AT (run scripts/integration.sh)")
        .parse()
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    now.saturating_sub(seeded)
}

/// `actual` is `seeded + elapsed`, within a few seconds of clock granularity.
fn assert_grown(what: &str, actual: u64, seeded: u64) {
    let expected = seeded + elapsed();
    assert!(
        actual + 5 >= expected && actual <= expected + 5,
        "{what}: {actual}, expected about {expected}"
    );
}

/// `h|ip:port|age|...` → (`ip:port`, age, the remaining fields).
fn host(line: &str) -> (String, u64, Vec<String>) {
    let f: Vec<&str> = line.split('|').collect();
    (
        f[1].to_owned(),
        f[2].parse().expect("numeric age"),
        f[3..].iter().map(|s| (*s).to_owned()).collect(),
    )
}

fn assert_plain_200(r: &Reply) {
    assert_eq!(r.status, 200);
    assert_eq!(r.content_type.as_deref(), Some("text/plain; charset=utf-8"));
    let cache_control = r.cache_control.as_deref().unwrap_or_default();
    assert!(cache_control.contains("no-store"), "{cache_control}");
    assert!(cache_control.contains("no-transform"), "{cache_control}");
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn hostfile_serves_live_hubs_newest_first() {
    let r = client().hostfile();
    assert_plain_200(&r);
    let lines = r.lines();
    assert!(lines[0].starts_with("i|pong|gwc-edge "), "{lines:?}");
    assert!(lines[0].ends_with("|gnutella2"));
    assert_eq!(lines.last().copied(), Some(self_line().as_str()));

    let hosts: Vec<_> = r.with_prefix("h|").into_iter().map(host).collect();
    let addrs: Vec<&str> = hosts.iter().map(|(a, _, _)| a.as_str()).collect();
    assert_eq!(
        addrs,
        ["1.1.1.1:6347", "8.8.4.4:6346"],
        "unverified and expired hubs left out"
    );

    let (_, age, extra) = &hosts[1];
    assert_grown("age computed at serve time", *age, 120);
    assert_eq!(extra[0], "", "cluster");
    assert_eq!(extra[1], "120", "leaves");
    assert_eq!(extra[2], "RAZA", "vendor");
    assert_grown(
        "uptime grows from up_since",
        extra[3].parse().unwrap(),
        9000,
    );
    assert_eq!(extra[4], "300", "leaf limit");
    assert!(hosts[0].2.is_empty(), "no extended fields when unknown");
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn urlfile_lists_verified_caches_then_self() {
    let r = client().urlfile();
    assert_plain_200(&r);
    let urls: Vec<&str> = r
        .with_prefix("u|")
        .into_iter()
        .map(|l| l.rsplit_once('|').unwrap().0)
        .collect();
    let self_url = self_line();
    let self_url = self_url.rsplit_once('|').unwrap().0;
    assert_eq!(
        urls,
        ["u|http://dkac.trillinux.org/dkac/dkac.php", self_url]
    );
    assert!(
        r.with_prefix("h|").is_empty(),
        "no hosts on a urlfile request"
    );
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn g1_get_gets_only_the_self_line() {
    let r = client().g1_hostfile();
    assert_plain_200(&r);
    assert_eq!(r.lines(), [self_line().as_str()]);
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn denylisted_client_gets_an_empty_body() {
    let r = client().get("?get=1&hostfile=1&net=gnutella2&ping=1&client=CRUX1.0");
    assert_plain_200(&r);
    assert_eq!(r.body, "");
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn malformed_query_is_a_200_error_line() {
    let r = client().get("?");
    assert_plain_200(&r);
    assert_eq!(r.lines(), ["i|error|client required"]);
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn update_is_period_then_ok() {
    let r = client().update(
        "8.8.4.4:6346",
        12,
        7300,
        300,
        Some("http://gwc.example.net/skulls.php"),
    );
    assert_plain_200(&r);
    assert_eq!(r.lines(), ["i|update|period|3600", "i|update|OK"]);
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn stats_reports_live_counts() {
    let r = client().get("/stats");
    assert_eq!(r.status, 200);
    assert_eq!(r.content_type.as_deref(), Some("application/json"));
    let json: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(json["live_hubs"], 2);
    assert_eq!(json["live_caches"], 1);
    assert_eq!(json["sample_rate"], 8);
    assert!(json["requests"]["by_kind"]["hostfile"].is_u64());
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn healthz_shows_a_fresh_hostfile_snapshot() {
    let r = client().get("/healthz");
    assert_eq!(r.status, 200);
    let json: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(json["status"], "ok");
    let age = json["snapshot_age"]["hostfile"]
        .as_u64()
        .expect("cron has run");
    assert!(age <= elapsed(), "rendered after seeding: {age}");
}
