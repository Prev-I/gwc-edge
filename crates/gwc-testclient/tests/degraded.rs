//! Degraded mode, end to end: the same Worker with no D1 binding and no Rate
//! Limiting bindings (`wrangler.no-d1.jsonc`), so every D1 call fails and
//! every limiter call fails open. Every protocol reply must still be the
//! protocol minimum, never a 5xx, and never empty for an allowed client.
//! Run by `scripts/integration.sh`.

use gwc_testclient::Client;
use gwc_testclient::logs::{is_request_line, len, settle};

fn client() -> Client {
    Client::new(
        &std::env::var("GWC_DEGRADED_URL").expect("GWC_DEGRADED_URL (run scripts/integration.sh)"),
    )
}

fn self_line() -> String {
    format!(
        "u|{}|0",
        std::env::var("GWC_CANONICAL_URL").expect("GWC_CANONICAL_URL")
    )
}

#[test]
#[ignore = "needs a running degraded cache: scripts/integration.sh"]
fn hostfile_is_pong_and_self() {
    let r = client().hostfile();
    assert_eq!(r.status, 200);
    let lines = r.lines();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].starts_with("i|pong|gwc-edge "));
    assert_eq!(lines[1], self_line());
}

#[test]
#[ignore = "needs a running degraded cache: scripts/integration.sh"]
fn urlfile_is_pong_and_self() {
    let lines_owned = client().urlfile();
    let lines = lines_owned.lines();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].starts_with("i|pong|"));
    assert_eq!(lines[1], self_line());
}

#[test]
#[ignore = "needs a running degraded cache: scripts/integration.sh"]
fn an_unavailable_rate_limiter_lets_requests_through_and_says_so() {
    // Fail-open (ADR-0005): three gets from one source, past the 2-a-minute
    // limit, all answered in full, each logged as unchecked; the isolate's
    // first such request also logs one warning naming the binding.
    let log = std::env::var("GWC_DEGRADED_LOG_FILE").expect("GWC_DEGRADED_LOG_FILE");
    let c = client();
    let me = [("CF-Connecting-IP", "2001:db8:1111:1::1")];
    let from = len(&log);
    for _ in 0..3 {
        let r = c.request(
            "GET",
            "?get=1&hostfile=1&net=gnutella2&ping=1&client=RAZA2.7.10.6",
            &me,
        );
        assert_eq!(r.lines().len(), 2, "not rate limited: {}", r.body);
    }
    let lines = settle(&log, from, 3, is_request_line);
    assert_eq!(lines.len(), 3);
    for l in &lines {
        assert_eq!(l["kind"], "hostfile", "{l}");
        assert_eq!(l["rate_limit"], "unavailable", "{l}");
    }
    let warnings: Vec<_> = gwc_testclient::logs::json_since(&log, 0)
        .into_iter()
        .filter(|v| v["event"] == "rate_limiter_unavailable")
        .collect();
    assert!(!warnings.is_empty(), "the binding failure is logged");
    for w in &warnings {
        assert_eq!(w["level"], "warn");
        assert!(w["binding"].is_string(), "{w}");
        assert!(!w.to_string().contains("2001:db8"), "{w}");
    }

    // Requests that are never rate limited are not marked.
    let from = len(&log);
    let _ = c.get("?ping=1&client=TEST&version=0.1");
    let ping = settle(&log, from, 1, is_request_line);
    assert!(ping[0].get("rate_limit").is_none(), "{}", ping[0]);
}

#[test]
#[ignore = "needs a running degraded cache: scripts/integration.sh"]
fn g1_is_self_only() {
    assert_eq!(client().g1_hostfile().lines(), [self_line().as_str()]);
}

#[test]
#[ignore = "needs a running degraded cache: scripts/integration.sh"]
fn update_is_period_then_ok() {
    // From the hub's own address, so the claim really reaches (failing) D1.
    let log = std::env::var("GWC_DEGRADED_LOG_FILE").expect("GWC_DEGRADED_LOG_FILE");
    let from = len(&log);
    let r = client().update_from("8.8.4.4", "8.8.4.4:6346", 12, 7300, 300, None);
    assert_eq!(r.status, 200);
    assert_eq!(r.lines(), ["i|update|period|3600", "i|update|OK"]);
    let lines = settle(&log, from, 1, is_request_line);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["kind"], "update");
    assert_eq!(lines[0]["outcome"], "degraded");
    assert_eq!(lines[0]["update"], "ip:error");
}

#[test]
#[ignore = "needs a running degraded cache: scripts/integration.sh"]
fn denied_is_still_empty() {
    let r = client().get("?get=1&hostfile=1&net=gnutella2&ping=1&client=CRUX1.0");
    assert_eq!(r.status, 200);
    assert_eq!(r.body, "");
}

#[test]
#[ignore = "needs a running degraded cache: scripts/integration.sh"]
fn never_a_5xx_on_the_protocol_path() {
    let c = client();
    for suffix in [
        "?get=1&hostfile=1&net=gnutella2&ping=1&pv=4&client=RAZA2.7.10.6",
        "?get=1&urlfile=1&net=gnutella2&ping=1&pv=4&client=RAZA2.7.10.6",
        "?get=1&hostfile=1&net=gnutella&client=RAZA&version=2.7.10.6",
        "?ip=8.8.4.4:6346&x.leaves=1&uptime=7300&x.max=300&update=1&net=gnutella2&client=RAZA2.7.10.6",
        "?url=http%3A%2F%2Fgwc.example.net%2F&update=1&net=gnutella2&client=RAZA2.7.10.6",
        "?ping=1&client=TEST&version=0.1",
        "?ping=1&net=gnutella&client=TEST&version=0.1",
        "?get=1&hostfile=1",
    ] {
        let r = c.get(suffix);
        assert_eq!(r.status, 200, "{suffix}");
        assert!(
            !r.body.is_empty(),
            "{suffix}: an allowed client never gets an empty body"
        );
    }
}

#[test]
#[ignore = "needs a running degraded cache: scripts/integration.sh"]
fn landing_page_is_200_and_says_unavailable() {
    let host = std::env::var("GWC_CANONICAL_URL").unwrap();
    let host = host.trim_start_matches("http://").trim_end_matches('/');
    let r = client().request("GET", "/", &[("Host", host)]);
    assert_eq!(r.status, 200);
    assert!(r.body.contains("unavailable"));
    assert_eq!(r.header("x-gwc-cache"), Some("miss"));
    let again = client().request("GET", "/", &[("Host", host)]);
    assert_eq!(
        again.header("x-gwc-cache"),
        Some("miss"),
        "a degraded page is never cached"
    );
}

#[test]
#[ignore = "needs a running degraded cache: scripts/integration.sh"]
fn stats_is_503_and_never_cached() {
    // A cached reply would be a 200; the second 503 shows nothing was stored.
    for _ in 0..2 {
        assert_eq!(client().get("/stats").status, 503);
    }
}

#[test]
#[ignore = "needs a running degraded cache: scripts/integration.sh"]
fn healthz_reports_the_database_error() {
    let r = client().get("/healthz");
    assert_eq!(r.status, 503);
    let json: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(json["database"], "error");
    assert_eq!(json["status"], "degraded");
}
