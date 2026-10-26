//! The verifying cron, end to end (ADR-0012). `scripts/integration.sh` runs a
//! `fake_peer` and maps these targets to it through `TEST_DIAL`:
//!
//! | target | answers | expected |
//! |---|---|---|
//! | 4.2.5.1 | `503 Maximum connections reached`, `X-Try-Hubs` | `alive_full`, listed; offers 4.2.3.1 |
//! | 4.2.6.1 | `503 Shielded leaf node` | `dead` |
//! | 4.2.7.1 | nothing listening | `dead` (unreachable) |
//! | 4.2.8.1 | never answers | `dead` (timeout) |
//! | 4.2.9.1 | closes mid-header | `dead` (closed) |
//! | 4.2.10.1 | `200` G2 | a `dead` seed that registers again is listed |
//! | ping.example.net, dkac.trillinux.org | a pong, fetched | `verified` |
//! | sock.example.net:6348 | a pong, over a socket | `verified` |
//! | redirect.example.net | a 302 to dkac | `failed`: redirects are not followed |
//! | linger.example.net:6349, lingerfetch.example.net | a pong, then the connection held open | `verified`, without waiting for the close |
//! | halfpong.example.net:6350 | half a pong line, then the connection held open | `failed` |
//!
//! Every other target is blocked, which counts as a failed check. Runs after
//! the read-path tests and before the write-path ones (name order).

use gwc_testclient::logs::{is_request_line, json_since, len, settle};
use gwc_testclient::{Client, Reply};

fn client() -> Client {
    Client::new(&std::env::var("GWC_BASE_URL").expect("GWC_BASE_URL (run scripts/integration.sh)"))
}

fn log_file() -> String {
    std::env::var("GWC_LOG_FILE").expect("GWC_LOG_FILE")
}

/// Run the cron once and wait for its summary line.
fn cron(c: &Client, log: &str) {
    let from = len(log);
    assert_eq!(
        c.get("/cdn-cgi/local/scheduled?cron=*/10+*+*+*+*").status,
        200
    );
    settle(log, from, 1, |v| v["kind"] == "cron");
}

fn listed(r: &Reply, prefix: &str, value: &str) -> bool {
    r.with_prefix(prefix)
        .iter()
        .any(|l| l.split('|').nth(1) == Some(value))
}

fn ok_reply(r: &Reply) {
    assert_eq!(
        r.lines(),
        ["i|update|period|3600", "i|update|OK"],
        "{}",
        r.body
    );
}

fn hub_lines(log: &str, from: usize) -> Vec<serde_json::Value> {
    json_since(log, from)
        .into_iter()
        .filter(|v| v["kind"] == "cron_hub")
        .collect()
}

fn hub_line(log: &str, from: usize, ip: &str) -> serde_json::Value {
    hub_lines(log, from)
        .into_iter()
        .rfind(|v| v["ip"] == ip)
        .unwrap_or_else(|| panic!("no cron_hub line for {ip}"))
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn hubs_are_judged_by_their_handshake() {
    let (c, log) = (client(), log_file());
    let from = len(&log);
    for hub in [
        "4.2.5.1", "4.2.6.1", "4.2.7.1", "4.2.8.1", "4.2.9.1", "4.2.10.1",
    ] {
        ok_reply(&c.update_from(hub, &format!("{hub}:6346"), 5, 7300, 300, None));
    }
    let updates = settle(&log, from, 6, is_request_line);
    assert_eq!(
        updates[5]["update"], "ip:reregistered",
        "the dead seed, past its backoff, is queued again"
    );
    cron(&c, &log);

    let full = hub_line(&log, from, "4.2.5.1");
    assert_eq!(
        (&full["class"], &full["result"], &full["now"]),
        (&"new".into(), &"503".into(), &"alive_full".into()),
        "{full}"
    );
    assert_eq!(full["try_hubs"], 2, "the private entry is never parsed");
    let back = hub_line(&log, from, "4.2.10.1");
    assert_eq!(
        (&back["class"], &back["result"], &back["now"]),
        (&"returning".into(), &"200".into(), &"verified".into()),
        "a dead hub is promoted straight to verified: {back}"
    );

    // The failures are self-registrations that were never listed: their
    // addresses are requesters', so the lines leave them out.
    let mut failed: Vec<String> = hub_lines(&log, from)
        .iter()
        .filter(|v| v["class"] == "new" && v["now"] == "dead")
        .map(|v| {
            assert!(v["ip"].is_null(), "{v}");
            v["result"].as_str().unwrap().to_owned()
        })
        .collect();
    failed.sort();
    assert_eq!(failed, ["503", "closed", "timeout", "unreachable"]);

    let hosts = c.hostfile();
    assert!(listed(&hosts, "h|", "4.2.5.1:6346"), "{}", hosts.body);
    assert!(listed(&hosts, "h|", "4.2.10.1:6346"), "{}", hosts.body);
    for gone in ["4.2.6.1", "4.2.7.1", "4.2.8.1", "4.2.9.1"] {
        assert!(!listed(&hosts, "h|", &format!("{gone}:6346")));
    }

    // 4.2.5.1 offered 4.2.3.1 (fresh), 4.2.3.2 (two days old) and a private
    // address. Only the fresh one becomes a candidate, checked on the next
    // run; blocked here, so it fails and is deleted.
    let from = len(&log);
    cron(&c, &log);
    let candidate = hub_line(&log, from, "4.2.3.1");
    assert_eq!(candidate["class"], "candidate");
    assert_eq!(
        (&candidate["result"], &candidate["now"]),
        (&"blocked".into(), &"deleted".into()),
        "a failed candidate is deleted: {candidate}"
    );
    for never in ["4.2.3.2", "192.168.1.1"] {
        assert!(
            hub_lines(&log, 0).iter().all(|v| v["ip"] != never),
            "{never} was never a candidate"
        );
    }
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn a_cache_that_keeps_its_connection_open_is_judged_by_its_reply() {
    // fake_peer answers a pong and then holds the connection for 20 s, past
    // the 10 s ping deadline: the reply alone must settle the ping, over a
    // socket and through fetch alike. Half a pong line, held the same way,
    // is never taken for one.
    let (c, log) = (client(), log_file());
    let socket = "http://linger.example.net:6349/";
    let fetched = "http://lingerfetch.example.net/";
    let half = "http://halfpong.example.net:6350/";
    for url in [socket, fetched, half] {
        ok_reply(&c.submit(url));
    }
    let from = len(&log);
    let pinged = |url: &str| {
        json_since(&log, from)
            .into_iter()
            .rfind(|v| v["kind"] == "cron_cache" && v["url"] == url)
    };
    // Two pings per run; earlier tests' submissions may come first.
    for _ in 0..6 {
        cron(&c, &log);
        if [socket, fetched, half].iter().all(|u| pinged(u).is_some()) {
            break;
        }
    }
    for url in [socket, fetched] {
        assert_eq!(pinged(url).expect("pinged")["result"], "verified", "{url}");
    }
    assert_eq!(pinged(half).expect("pinged")["result"], "failed");
    let urls = c.urlfile();
    assert!(listed(&urls, "u|", socket), "{}", urls.body);
    assert!(listed(&urls, "u|", fetched), "{}", urls.body);
    assert!(!listed(&urls, "u|", half));
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn submitted_caches_are_served_once_they_answer_a_ping() {
    let (c, log) = (client(), log_file());
    let fetched = "http://ping.example.net/gwc/";
    let socket = "http://sock.example.net:6348/gwc/";
    let redirect = "http://redirect.example.net/";
    for url in [fetched, socket, redirect] {
        ok_reply(&c.submit(url));
    }
    assert!(!listed(&c.urlfile(), "u|", fetched), "pending: not served");
    let from = len(&log);
    let pinged = |url: &str| {
        json_since(&log, from)
            .into_iter()
            .rfind(|v| v["kind"] == "cron_cache" && v["url"] == url)
    };
    // Two pings per run.
    for _ in 0..4 {
        cron(&c, &log);
        if [fetched, socket, redirect]
            .iter()
            .all(|u| pinged(u).is_some())
        {
            break;
        }
    }
    for url in [fetched, socket] {
        assert_eq!(pinged(url).expect("pinged")["result"], "verified", "{url}");
    }
    assert_eq!(
        pinged(redirect).expect("pinged")["result"],
        "failed",
        "a redirect is not followed"
    );
    let urls = c.urlfile();
    assert!(listed(&urls, "u|", fetched), "{}", urls.body);
    assert!(listed(&urls, "u|", socket), "{}", urls.body);
    assert!(!listed(&urls, "u|", redirect));
    assert!(
        listed(&urls, "u|", "http://dkac.trillinux.org/dkac/dkac.php"),
        "dkac answered its pings too"
    );
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn stats_report_the_last_run_and_healthz_the_test_seam() {
    let (c, log) = (client(), log_file());
    cron(&c, &log);
    let stats: serde_json::Value = serde_json::from_str(&c.get("/stats").body).unwrap();
    let run = &stats["cron"];
    assert_eq!(run["test_dial"], true);
    assert_eq!(run["verification_failed"], false);
    let subrequests = run["subrequests"].as_u64().unwrap();
    assert!((1..=45).contains(&subrequests), "{subrequests}");
    // Loopback connects open at once, so no close can leak and no wave
    // waits for a slot (ADR-0019, ADR-0020).
    assert_eq!(run["leaked_sockets"], 0);
    assert_eq!(run["slot_wait_ms"], 0);
    let health: serde_json::Value = serde_json::from_str(&c.get("/healthz").body).unwrap();
    assert_eq!(health["test_dial"], true);
}
