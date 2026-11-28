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
//! | gone.example.net (seeded `failed` twice) | blocked | deleted, and recorded in `dead_caches` |
//! | unlisted.example.net (two rows), unlisted-sock.example.net:6351 | a pong and a urlfile naming a third cache, not this one | `verified`, and advertised to once (ADR-0021) |
//! | unlisted-linger.example.net:6352 | the same with a `Content-Length`, then the connection held open | advertised to, without waiting for the close |
//! | keepopen-sock.example.net:6353, keepopen.example.net | the same with no `Content-Length`, held open past the ping deadline | `verified` at the deadline, never advertised to |
//! | listing.example.net | a urlfile that names this cache | `verified`, never advertised to |
//! | skulls.example.net | a Skulls pong | `verified`, never advertised to |
//!
//! Every other target is blocked, which counts as a failed check. Runs after
//! the read-path tests and before the write-path ones (name order).
//!
//! The advertisement's caches are seeded `verified` and just checked, so no
//! cron run touches them until a test makes them due ([`make_due`]).

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

fn state_dir() -> String {
    std::env::var("GWC_STATE_DIR").expect("GWC_STATE_DIR")
}

/// Age the caches' `last_checked` past the 6 h recheck, so the next cron
/// runs ping them. Interpolates SQL: only literal test URLs.
fn make_due(urls: &[&str]) {
    let past = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 7 * 3600;
    let list = urls
        .iter()
        .map(|u| format!("'{u}'"))
        .collect::<Vec<_>>()
        .join(", ");
    gwc_testclient::d1::query(
        &state_dir(),
        &format!("UPDATE caches SET last_checked = {past} WHERE url IN ({list})"),
    );
}

/// `caches.last_advertised` of the row with this URL. Interpolates SQL:
/// only literal test URLs.
fn last_advertised(url: &str) -> Option<u64> {
    let rows = gwc_testclient::d1::query(
        &state_dir(),
        &format!("SELECT last_advertised FROM caches WHERE url = '{url}'"),
    );
    let row = rows[0]
        .first()
        .unwrap_or_else(|| panic!("no row for {url}"));
    row["last_advertised"].as_u64()
}

fn lines_of(log: &str, from: usize, kind: &str) -> Vec<serde_json::Value> {
    json_since(log, from)
        .into_iter()
        .filter(|v| v["kind"] == kind)
        .collect()
}

/// The `cron_cache` line for `url` since `from`, if it was pinged.
fn ping_line(log: &str, from: usize, url: &str) -> Option<serde_json::Value> {
    lines_of(log, from, "cron_cache")
        .into_iter()
        .rfind(|v| v["url"] == url)
}

/// The last run's part of `/stats`.
fn last_run(c: &Client) -> serde_json::Value {
    let stats: serde_json::Value = serde_json::from_str(&c.get("/stats").body).unwrap();
    stats["cron"].clone()
}

/// Run the cron until every URL has been pinged since `from` (at most 4
/// runs). Each run sends at most one advertisement, and `/stats` counts it.
fn cron_until_pinged(c: &Client, log: &str, from: usize, urls: &[&str]) {
    for _ in 0..4 {
        let run_from = len(log);
        cron(c, log);
        let sent = lines_of(log, run_from, "cron_advert");
        assert!(sent.len() <= 1, "one advertisement a run at most: {sent:?}");
        let run = last_run(c);
        assert_eq!(run["caches_advertised"], sent.len(), "{run}");
        let ok = sent.iter().filter(|v| v["class"] == "ok").count();
        assert_eq!(run["adverts_ok"], ok, "{run}");
        if urls.iter().all(|u| ping_line(log, from, u).is_some()) {
            return;
        }
    }
    panic!("not all of {urls:?} were pinged");
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
fn a_cache_deleted_after_failed_pings_is_declined_when_submitted_again() {
    // ADR-0022. The seed has failed twice; its third failed ping deletes it
    // and records it, in the same batch. A resubmission is then answered OK
    // and not stored, matched case-insensitively as `plan_cache` matches.
    let (c, log) = (client(), log_file());
    let gone = "http://gone.example.net/Gwc/";
    let deleted = |from| {
        json_since(&log, from)
            .into_iter()
            .any(|v| v["kind"] == "cron_cache" && v["url"] == gone && v["result"] == "deleted")
    };
    // Two pings per run, and pending submissions come first.
    for _ in 0..6 {
        if deleted(0) {
            break;
        }
        cron(&c, &log);
    }
    assert!(deleted(0), "the seeded cache was never deleted");

    let from = len(&log);
    ok_reply(&c.submit("http://gone.example.net/gwc/"));
    let line = settle(&log, from, 1, is_request_line).remove(0);
    assert_eq!(line["update"], "url:dead", "{line}");
    assert!(!listed(&c.urlfile(), "u|", "http://gone.example.net/gwc/"));

    // The cron recorded the URL as it was stored, and its expiry deleted only
    // the record past DEAD_TTL (both seeded by scripts/integration.sh).
    let state = std::env::var("GWC_STATE_DIR").expect("GWC_STATE_DIR");
    let rows = gwc_testclient::d1::query(&state, "SELECT url FROM dead_caches ORDER BY url");
    let urls: Vec<&str> = rows[0].iter().map(|r| r["url"].as_str().unwrap()).collect();
    assert_eq!(
        urls,
        ["http://fresh.example.net/", "http://gone.example.net/Gwc/"]
    );
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

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn a_cache_that_lists_this_one_or_runs_skulls_is_never_advertised_to() {
    // ADR-0021: listing.example.net's urlfile names this cache, and
    // skulls.example.net pongs as Skulls. Both stay verified, and neither is
    // sent anything.
    let (c, log) = (client(), log_file());
    let listing = "http://listing.example.net/";
    let skulls = "http://skulls.example.net/";
    make_due(&[listing, skulls]);
    let from = len(&log);
    cron_until_pinged(&c, &log, from, &[listing, skulls]);
    for url in [listing, skulls] {
        assert_eq!(
            ping_line(&log, from, url).unwrap()["result"],
            "verified",
            "{url}"
        );
        assert!(
            lines_of(&log, 0, "cron_advert")
                .iter()
                .all(|v| v["url"] != url),
            "{url} was advertised to"
        );
        assert_eq!(last_advertised(url), None, "{url}");
    }
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn a_cache_whose_reply_has_a_length_is_advertised_to_while_it_lingers() {
    // ADR-0021: a reply with a Content-Length is whole once that many bytes
    // have arrived, though the peer keeps the connection open for 20 s. The
    // advertisement's reply is the same, and its `i|update|OK` has no line
    // end: only the length completes it. Neither waits for the 10 s ping
    // deadline, let alone the close.
    let (c, log) = (client(), log_file());
    let linger = "http://unlisted-linger.example.net:6352/";
    make_due(&[linger]);
    let from = len(&log);
    cron_until_pinged(&c, &log, from, &[linger]);
    assert_eq!(ping_line(&log, from, linger).unwrap()["result"], "verified");
    let sent: Vec<_> = lines_of(&log, from, "cron_advert")
        .into_iter()
        .filter(|v| v["url"] == linger)
        .collect();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["class"], "ok", "{}", sent[0]);
    assert!(sent[0]["ms"].as_u64().unwrap() < 10_000, "{}", sent[0]);
    let run = lines_of(&log, from, "cron").pop().unwrap();
    assert!(
        run["ms"].as_u64().unwrap() < 10_000,
        "the ping waited for its deadline: {run}"
    );
    assert!(last_advertised(linger).is_some());
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn a_verified_cache_that_keeps_its_connection_open_is_not_advertised_to() {
    // ADR-0021, the dial half: the urlfile names a third cache and not this
    // one, but with no Content-Length the peer holds the connection open
    // past the 10 s ping deadline. The scan keeps reading after the pong, so
    // the deadline ends the read: the pong fixed earlier still stands
    // (`verified`), but the body was never read whole, so nothing is sent.
    // Over a socket and through fetch (port 80).
    let (c, log) = (client(), log_file());
    let socket = "http://keepopen-sock.example.net:6353/";
    let fetched = "http://keepopen.example.net/";
    make_due(&[socket, fetched]);
    let from = len(&log);
    cron_until_pinged(&c, &log, from, &[socket, fetched]);
    for url in [socket, fetched] {
        assert_eq!(
            ping_line(&log, from, url).unwrap()["result"],
            "verified",
            "{url}: the deadline lost the pong"
        );
        assert!(
            lines_of(&log, 0, "cron_advert")
                .iter()
                .all(|v| v["url"] != url),
            "{url} was advertised to: the deadline was taken for a whole body"
        );
        assert_eq!(last_advertised(url), None, "{url}");
    }
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn an_unlisting_cache_is_advertised_to_once() {
    // ADR-0021. Both caches' urlfiles name a third cache and not this one:
    // unlisted.example.net is fetched (port 80), unlisted-sock over a socket.
    let (c, log) = (client(), log_file());
    let fetched = "http://unlisted.example.net/";
    let other = "http://unlisted.example.net/other/";
    let socket = "http://unlisted-sock.example.net:6351/";
    let advert_for = |from: usize, url: &str| -> Vec<serde_json::Value> {
        lines_of(&log, from, "cron_advert")
            .into_iter()
            .filter(|v| v["url"] == url)
            .collect()
    };

    // Due together, both qualify, and the first in queue order is advertised
    // to; the other waits for its next due ping (6 h on).
    let from = len(&log);
    make_due(&[fetched, socket]);
    cron_until_pinged(&c, &log, from, &[fetched, socket]);
    let sent = lines_of(&log, from, "cron_advert");
    assert_eq!(sent.len(), 1, "{sent:?}");
    let first = sent[0]["url"].as_str().unwrap().to_owned();
    let second = if first == fetched { socket } else { fetched };
    // So it is due again now.
    let from = len(&log);
    make_due(&[second]);
    cron_until_pinged(&c, &log, from, &[second]);

    for url in [fetched, socket] {
        let sent = advert_for(0, url);
        assert_eq!(sent.len(), 1, "{url}: {sent:?}");
        assert_eq!(sent[0]["class"], "ok", "{}", sent[0]);
        assert_eq!(sent[0]["pong"], "fake-cache 1.0", "{}", sent[0]);
        assert!(last_advertised(url).is_some(), "{url}");
    }
    // One stamp covers every row of the host.
    assert_eq!(last_advertised(other), last_advertised(fetched));

    // Within 30 days, no row of either host is advertised to again, though
    // all are pinged and still pass.
    let from = len(&log);
    make_due(&[fetched, other, socket]);
    cron_until_pinged(&c, &log, from, &[fetched, other, socket]);
    for url in [fetched, other, socket] {
        assert_eq!(ping_line(&log, from, url).unwrap()["result"], "verified");
        assert!(advert_for(from, url).is_empty(), "{url} advertised again");
    }
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn healthz_and_stats_show_the_mode() {
    let c = client();
    let health: serde_json::Value = serde_json::from_str(&c.get("/healthz").body).unwrap();
    assert_eq!(health["advertise"], "on");
    assert_eq!(last_run(&c)["advertise"], "on");
}
