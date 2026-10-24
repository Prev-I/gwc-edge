//! The write path, end to end: what an update stores, what it never stores,
//! rate limits, and the full Shareaza bootstrap session. Run by
//! `scripts/integration.sh` against the seeded cache, after the read-path
//! tests (test binaries run in name order).
//!
//! Hubs and caches here use addresses no other test touches. `/__scheduled`
//! runs the cron on demand (`wrangler dev --test-scheduled`). A self-registered
//! hub is stored `unverified` and listed only after a successful handshake
//! (ADR-0011). `TEST_DIAL` maps 4.2.2.1 to a fake hub that accepts; every
//! other hub here is blocked, so it fails its check (`tests/verify.rs`).

use gwc_testclient::logs::{is_request_line, len, settle};
use gwc_testclient::{Client, Reply};

fn client() -> Client {
    Client::new(&std::env::var("GWC_BASE_URL").expect("GWC_BASE_URL (run scripts/integration.sh)"))
}

fn self_line() -> String {
    format!(
        "u|{}|0",
        std::env::var("GWC_CANONICAL_URL").expect("GWC_CANONICAL_URL")
    )
}

/// The current `/stats`, after a cron run, so earlier tests' writes are
/// already counted: a baseline to compare against.
fn fresh_stats(c: &Client) -> serde_json::Value {
    cron(c);
    stats(c)
}

/// Run the cron once, as its trigger would.
fn cron(c: &Client) {
    assert_eq!(c.get("/__scheduled?cron=*/10+*+*+*+*").status, 200);
}

fn stats(c: &Client) -> serde_json::Value {
    let r = c.get("/stats");
    assert_eq!(r.status, 200, "{}", r.body);
    serde_json::from_str(&r.body).unwrap()
}

fn listed(r: &Reply, hub: &str) -> Option<String> {
    r.with_prefix("h|")
        .into_iter()
        .find(|l| l.split('|').nth(1) == Some(hub))
        .map(str::to_owned)
}

fn ok_reply(r: &Reply) {
    assert_eq!(r.status, 200);
    assert_eq!(
        r.lines(),
        ["i|update|period|3600", "i|update|OK"],
        "{}",
        r.body
    );
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn shareaza_session_lists_the_hub_after_its_handshake() {
    // The definition of done: one hub, one address, as a real Shareaza hub
    // talks to the cache: ping → get → update → get. The update stores the
    // hub `unverified` (ADR-0011); the cron's handshake, answered by the fake
    // hub, lists it.
    let log = std::env::var("GWC_LOG_FILE").expect("GWC_LOG_FILE");
    let c = client();
    let hub = "4.2.2.1:6346";
    let me = [("CF-Connecting-IP", "4.2.2.1")];
    let hostfile = "?get=1&hostfile=1&net=gnutella2&ping=1&pv=4&client=RAZA2.7.10.6\
        &getleaves=1&getnetworks=1&getclusters=0&getvendors=1&getuptime=1";
    let from = len(&log);

    // 1. ping
    let ping = c.request("GET", "?ping=1&client=RAZA2.7.10.6", &me);
    assert!(ping.lines()[0].starts_with("i|pong|gwc-edge "));

    // 2. get: the hub is not known
    let before = c.request("GET", hostfile, &me);
    assert_eq!(listed(&before, hub), None);
    assert_eq!(before.lines().last().copied(), Some(self_line().as_str()));

    // 3. update, from the hub's own address, submitting another cache
    ok_reply(&c.update_from(
        "4.2.2.1",
        hub,
        12,
        7300,
        300,
        Some("http://session.example.net/gwc/"),
    ));

    // 4. not served before the cron has checked it (asked by another client:
    //    the hub's own gets are limited to 2 a minute)
    let waiting = c.hostfile();
    assert_eq!(listed(&waiting, hub), None, "not listed before a handshake");

    // 5. the cron's handshake succeeds; the next get lists the hub, with the
    //    fields its update reported
    cron(&c);
    let after = c.request("GET", hostfile, &me);
    let line = listed(&after, hub).expect("listed after its handshake");
    let fields: Vec<&str> = line.split('|').collect();
    assert_eq!(fields[3..], ["", "12", "RAZA", fields[6], "300"], "{line}");
    assert_eq!(after.lines().last().copied(), Some(self_line().as_str()));

    let lines = settle(&log, from, 5, is_request_line);
    let kinds: Vec<&str> = lines.iter().map(|l| l["kind"].as_str().unwrap()).collect();
    assert_eq!(
        kinds[..5],
        ["ping", "hostfile", "update", "hostfile", "hostfile"]
    );
    assert_eq!(lines[2]["update"], "ip:inserted url:inserted");
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn an_update_from_another_address_is_answered_ok_and_never_stored() {
    let c = client();
    let total = fresh_stats(&c)["hubs"]["total"].as_u64().unwrap();
    ok_reply(&c.update_from("4.2.2.9", "4.2.2.2:6346", 1, 7300, 300, None));
    ok_reply(&c.update("4.2.2.2:6346", 1, 7300, 300, None));
    // Positive control: one real self-registration in the same window, so a
    // cron that did not run cannot make this test pass.
    ok_reply(&c.update_from("4.2.2.7", "4.2.2.7:6346", 1, 7300, 300, None));
    cron(&c);
    assert_eq!(
        stats(&c)["hubs"]["total"],
        total + 1,
        "only the self-registration; not even as unverified"
    );
    assert_eq!(listed(&c.hostfile(), "4.2.2.2:6346"), None);
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn private_low_port_and_g1_updates_are_answered_ok_and_not_stored() {
    let c = client();
    let total = fresh_stats(&c)["hubs"]["total"].as_u64().unwrap();
    ok_reply(&c.update_from("192.168.7.7", "192.168.7.7:6346", 1, 7300, 300, None));
    ok_reply(&c.update_from("4.2.2.3", "4.2.2.3:80", 1, 7300, 300, None));
    let g1 = c.request(
        "GET",
        "?ip=4.2.2.4:6346&update=1&net=gnutella&client=RAZA&version=2.7.10.6",
        &[("CF-Connecting-IP", "4.2.2.4")],
    );
    ok_reply(&g1);
    ok_reply(&c.update_from("4.2.2.8", "4.2.2.8:6346", 1, 7300, 300, None)); // positive control
    cron(&c);
    assert_eq!(stats(&c)["hubs"]["total"], total + 1);
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn submitted_caches_are_pending_unlisted_and_capped_per_domain() {
    let c = client();
    // Stored, whatever the cron's pings (blocked here) made of them since.
    let stored = |s: &serde_json::Value| {
        ["pending", "failed", "verified"]
            .iter()
            .map(|k| s["caches"][k].as_u64().unwrap())
            .sum::<u64>()
    };
    let before = stored(&fresh_stats(&c));
    for path in ["a", "b", "c", "d"] {
        ok_reply(&c.submit(&format!("http://caps.example.net/{path}/")));
    }
    ok_reply(&c.submit("https://tls-only.example.net/gwc/"));
    ok_reply(&c.submit(&std::env::var("GWC_CANONICAL_URL").unwrap()));
    cron(&c);
    assert_eq!(
        stored(&stats(&c)),
        before + 3,
        "three per domain; https and this cache itself not stored"
    );
    let urls = c.urlfile();
    assert!(
        urls.with_prefix("u|http://caps.example.net").is_empty(),
        "pending caches are not served"
    );
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn a_denylisted_hub_gets_a_warning_and_is_not_stored() {
    let c = client();
    let r = c.request(
        "GET",
        "?ip=4.2.2.6:6346&update=1&net=gnutella2&client=CRUX1.0",
        &[("CF-Connecting-IP", "4.2.2.6")],
    );
    assert_eq!(r.lines(), ["i|update|WARNING|client not supported"]);
    cron(&c);
    assert_eq!(listed(&c.hostfile(), "4.2.2.6:6346"), None);
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn gets_are_limited_per_source_and_kind_and_still_succeed() {
    let log = std::env::var("GWC_LOG_FILE").expect("GWC_LOG_FILE");
    let c = client();
    let source = [("CF-Connecting-IP", "2001:db8:ffff:1::1")];
    let hostfile = "?get=1&hostfile=1&net=gnutella2&ping=1&pv=4&client=RAZA2.7.10.6";
    let urlfile = "?get=1&urlfile=1&net=gnutella2&ping=1&pv=4&client=RAZA2.7.10.6";
    let from = len(&log);

    let first = c.request("GET", hostfile, &source);
    let _second = c.request("GET", hostfile, &source);
    let third = c.request("GET", hostfile, &source);
    let other_kind = c.request("GET", urlfile, &source);

    assert_ne!(
        first.with_prefix("h|").len(),
        0,
        "the first get carries hubs"
    );
    let limited = third.lines();
    assert_eq!(
        limited.len(),
        2,
        "pong and the cache's own line, no data: {limited:?}"
    );
    assert!(limited[0].starts_with("i|pong|"));
    assert_eq!(limited[1], self_line());
    assert!(
        !other_kind.with_prefix("u|http://dkac").is_empty(),
        "urlfile has its own limit: Shareaza sends it right after the hostfile"
    );

    let lines = settle(&log, from, 4, is_request_line);
    let kinds: Vec<&str> = lines.iter().map(|l| l["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["hostfile", "hostfile", "limited", "urlfile"]);
    assert_eq!(lines[2]["outcome"], "rate_limited");
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn a_second_update_within_a_minute_is_answered_ok_and_not_processed() {
    let log = std::env::var("GWC_LOG_FILE").expect("GWC_LOG_FILE");
    let c = client();
    let from = len(&log);
    ok_reply(&c.update_from("4.2.2.5", "4.2.2.5:6346", 1, 7300, 300, None));
    ok_reply(&c.update_from("4.2.2.5", "4.2.2.5:6346", 2, 7300, 300, None));
    let lines = settle(&log, from, 2, is_request_line);
    let kinds: Vec<&str> = lines.iter().map(|l| l["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["update", "limited"]);
    assert_eq!(lines[1]["outcome"], "rate_limited");
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn seeded_hubs_coalesce_or_back_off_as_planned() {
    let log = std::env::var("GWC_LOG_FILE").expect("GWC_LOG_FILE");
    let c = client();
    let from = len(&log);
    // 8.8.4.4 (verified, seen 120 s before seeding): refreshed within 30
    // minutes on the same port, so nothing is written.
    ok_reply(&c.update_from("8.8.4.4", "8.8.4.4:6346", 12, 7300, 300, None));
    // 9.9.9.9 failed its first check in the script's cron run; 9.9.9.10 was
    // seeded dead, checked a minute before. Both register themselves.
    ok_reply(&c.update_from("9.9.9.10", "9.9.9.10:6346", 1, 7300, 300, None));
    ok_reply(&c.update_from("9.9.9.9", "9.9.9.9:6346", 1, 7300, 300, None));
    let lines = settle(&log, from, 3, is_request_line);
    let results: Vec<&str> = lines
        .iter()
        .map(|l| l["update"].as_str().unwrap_or_default())
        .collect();
    // 8.8.4.4 and 9.9.9.9 were seen within 30 minutes: nothing written.
    // 9.9.9.10 has failed 3 checks, the last a minute before seeding: it waits
    // out its backoff, 40 minutes (ADR-0012).
    assert_eq!(results, ["ip:coalesced", "ip:backoff", "ip:coalesced"]);

    cron(&c);
    let hosts = c.hostfile();
    let age_8844: u64 = listed(&hosts, "8.8.4.4:6346")
        .expect("still listed")
        .split('|')
        .nth(2)
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        age_8844 >= 120,
        "the coalesced refresh did not reset its age: {age_8844}"
    );
    assert_eq!(listed(&hosts, "9.9.9.10:6346"), None, "dead, not served");
    assert_eq!(listed(&hosts, "9.9.9.9:6346"), None, "dead, not served");
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn pings_and_g1_gets_are_never_limited() {
    let log = std::env::var("GWC_LOG_FILE").expect("GWC_LOG_FILE");
    let c = client();
    let me = [("CF-Connecting-IP", "2001:db8:ffff:2::1")];
    let from = len(&log);
    for _ in 0..3 {
        assert!(
            c.request("GET", "?ping=1&client=RAZA2.7.10.6", &me).lines()[0].starts_with("i|pong|")
        );
        let g1 = c.request(
            "GET",
            "?get=1&hostfile=1&net=gnutella&client=RAZA&version=2.7.10.6",
            &me,
        );
        assert_eq!(g1.lines(), [self_line().as_str()]);
    }
    let lines = settle(&log, from, 6, is_request_line);
    assert!(lines.iter().all(|l| l["outcome"] == "ok"), "{lines:#?}");
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn zz_no_log_line_carries_a_requester_address() {
    // Runs last (tests run in name order): every source address used by the
    // tests, set through CF-Connecting-IP, must be absent from both logs.
    // A cron line may name a hub it checked when that hub is a listing
    // (addendum C), so only that field is left out of the check.
    let logs = [
        std::env::var("GWC_LOG_FILE").expect("GWC_LOG_FILE"),
        std::env::var("GWC_DEGRADED_LOG_FILE").expect("GWC_DEGRADED_LOG_FILE"),
    ];
    for log in &logs {
        for mut v in gwc_testclient::logs::json_since(log, 0) {
            if matches!(v["kind"].as_str(), Some("cron_hub" | "cron_trace"))
                && let Some(line) = v.as_object_mut()
            {
                line.remove("ip");
            }
            let text = v.to_string();
            for needle in [
                "2001:db8",
                "4.2.2.",
                "8.8.4.4",
                "9.9.9.",
                "192.168.7.7",
                "127.0.0.1",
            ] {
                assert!(!text.contains(needle), "{needle} in {log}: {text}");
            }
        }
    }
}
