//! Logging, end to end: one JSON line per request on every path, no
//! requester address, and D1 errors at error level by statement name. Reads
//! the `wrangler dev` output files `scripts/integration.sh` captures; each
//! test looks only at the lines written after it started, so other tests'
//! requests cannot satisfy it.

use std::time::{Duration, Instant};

use gwc_testclient::Client;

/// The JSON objects in a `wrangler dev` log from byte `from` on, colour codes
/// and prefixes removed.
fn json_since(path: &str, from: usize) -> Vec<serde_json::Value> {
    let raw = std::fs::read(path).unwrap_or_default();
    String::from_utf8_lossy(raw.get(from..).unwrap_or_default())
        .lines()
        .filter_map(|l| {
            let start = l.find('{')?;
            let end = l.rfind('}')?;
            serde_json::from_str(&l[start..=end]).ok()
        })
        .collect()
}

fn len(path: &str) -> usize {
    std::fs::metadata(path).map_or(0, |m| usize::try_from(m.len()).unwrap_or(0))
}

/// Lines matching `pred` written since `from`: wait until `want` appear, then
/// a little longer, so a late duplicate is counted too.
fn settle(
    path: &str,
    from: usize,
    want: usize,
    pred: impl Fn(&serde_json::Value) -> bool,
) -> Vec<serde_json::Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while json_since(path, from).iter().filter(|v| pred(v)).count() < want
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(500));
    json_since(path, from)
        .into_iter()
        .filter(|v| pred(v))
        .collect()
}

/// A request line: the only lines with an `outcome`.
fn is_request_line(v: &serde_json::Value) -> bool {
    v.get("outcome").is_some()
}

/// A request to send: method, path or query, extra headers.
struct Req {
    method: &'static str,
    suffix: String,
    headers: Vec<(&'static str, String)>,
}

fn req(method: &'static str, suffix: impl Into<String>) -> Req {
    Req {
        method,
        suffix: suffix.into(),
        headers: Vec::new(),
    }
}

/// One request of every kind, each expected to log exactly one line.
fn every_path(host: &str) -> Vec<(Req, &'static str)> {
    let v = "logcheck";
    let with_host = |suffix: &str, h: &str| Req {
        headers: vec![("Host", h.to_owned())],
        ..req("GET", suffix)
    };
    vec![
        (
            req("GET", format!("?ping=1&client=TEST&version={v}")),
            "ping",
        ),
        (
            req(
                "GET",
                format!("?get=1&hostfile=1&net=gnutella2&ping=1&client=TEST&version={v}"),
            ),
            "hostfile",
        ),
        (
            req(
                "GET",
                format!("?get=1&urlfile=1&net=gnutella2&ping=1&client=TEST&version={v}"),
            ),
            "urlfile",
        ),
        (
            req(
                "GET",
                format!("?get=1&hostfile=1&net=gnutella&client=TEST&version={v}"),
            ),
            "other_net",
        ),
        (
            req(
                "GET",
                format!("?ip=8.8.4.4:6346&update=1&net=gnutella2&client=TEST&version={v}"),
            ),
            "update",
        ),
        (
            req(
                "GET",
                format!("?get=1&hostfile=1&net=gnutella2&client=CRUX&version={v}"),
            ),
            "denied",
        ),
        (req("GET", "?get=1"), "error"),
        (with_host("/", host), "page"),
        (req("GET", "/stats"), "stats"),
        (req("GET", "/healthz"), "healthz"),
        (req("GET", "/robots.txt"), "robots"),
        (with_host("/", "alias.example.net"), "redirect"),
        (req("POST", "/"), "method_not_allowed"),
        (
            req("HEAD", format!("?ping=1&client=TEST&version={v}")),
            "ping",
        ),
        (req("GET", "/favicon.ico"), "not_found"),
    ]
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn one_json_line_per_request_on_every_path() {
    let log = std::env::var("GWC_LOG_FILE").expect("GWC_LOG_FILE");
    let c = Client::new(&std::env::var("GWC_BASE_URL").expect("GWC_BASE_URL"));
    let canonical = std::env::var("GWC_CANONICAL_URL").expect("GWC_CANONICAL_URL");
    let host = canonical
        .trim_start_matches("http://")
        .trim_end_matches('/');
    let from = len(&log);

    let requests = every_path(host);
    for (r, _) in &requests {
        let headers: Vec<(&str, &str)> = r.headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let _ = c.request(r.method, &r.suffix, &headers);
    }

    let lines = settle(&log, from, requests.len(), is_request_line);
    let kinds: Vec<&str> = lines.iter().map(|l| l["kind"].as_str().unwrap()).collect();
    let expected: Vec<&str> = requests.iter().map(|(_, k)| *k).collect();
    assert_eq!(kinds, expected, "exactly one line per request, in order");
    for (l, kind) in lines.iter().zip(&expected) {
        let want = match *kind {
            "denied" => "denied",
            "error" => "malformed",
            _ => "ok",
        };
        assert_eq!(l["outcome"], want, "{l}");
        for field in [
            "kind",
            "client",
            "version",
            "scheme",
            "outcome",
            "hosts_sent",
            "caches_sent",
            "d1_ms",
            "cf_colo",
        ] {
            assert!(l.get(field).is_some(), "{field} missing in {l}");
        }
    }
    assert_eq!(lines[1]["hosts_sent"], 2, "the two live seeded hubs");
    let source = lines[1]["snapshot"].as_str().unwrap_or_default();
    assert!(
        ["memo", "stored"].contains(&source),
        "a healthy read: {source}"
    );
    assert_eq!(
        lines[2]["caches_sent"], 1,
        "dkac, the cache's own line not counted"
    );
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn no_line_carries_the_requester_address() {
    let log = std::env::var("GWC_LOG_FILE").expect("GWC_LOG_FILE");
    for v in json_since(&log, 0) {
        assert!(!v.to_string().contains("127.0.0.1"), "{v}");
    }
}

#[test]
#[ignore = "needs a running degraded cache: scripts/integration.sh"]
fn a_failed_d1_call_is_one_error_line_and_no_further_access() {
    let log = std::env::var("GWC_DEGRADED_LOG_FILE").expect("GWC_DEGRADED_LOG_FILE");
    let c = Client::new(&std::env::var("GWC_DEGRADED_URL").expect("GWC_DEGRADED_URL"));
    let from = len(&log);
    // /healthz would make two D1 calls; the first fails, the second must not
    // be attempted.
    let _ = c.get("/healthz");
    let errors = settle(&log, from, 1, |v| v["event"] == "d1_error");
    assert_eq!(
        errors.len(),
        1,
        "one failure, then no further D1 access: {errors:#?}"
    );
    assert_eq!(errors[0]["level"], "error");
    assert_eq!(
        errors[0]["statement"], "binding",
        "a statement name, never SQL"
    );
    let requests = settle(&log, from, 1, is_request_line);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["kind"], "healthz");
    assert_eq!(requests[0]["outcome"], "degraded");
}
