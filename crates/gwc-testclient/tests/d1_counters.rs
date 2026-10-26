//! The cap counters stay exact when inserts collide: run against the D1 the
//! integration Worker uses (`GWC_STATE_DIR`) and a scratch D1 of its own,
//! through `wrangler d1 execute --local`, which executes the statements on one
//! connection as a D1 batch does. Run by `scripts/integration.sh`.

use gwc_core::update::{COUNT_INSERTED_CACHE, COUNT_INSERTED_HUB, UNCOUNT_REQUEUED_CANDIDATE};
use gwc_testclient::Client;

use gwc_testclient::d1::{query as d1, wrangler};

fn number(dir: &str, sql: &str) -> u64 {
    d1(dir, sql)[0][0]["n"].as_u64().unwrap()
}

fn scratch() -> tempdir::Dir {
    let dir = tempdir::Dir::new();
    let ok = wrangler()
        .args([
            "d1",
            "migrations",
            "apply",
            "DB",
            "--local",
            "--persist-to",
            dir.path(),
        ])
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "migrations apply");
    dir
}

/// A temporary directory removed on drop, without a new dependency.
mod tempdir {
    pub struct Dir(std::path::PathBuf);
    impl Dir {
        pub fn new() -> Dir {
            let p = std::env::temp_dir().join(format!(
                "gwc-d1-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
        pub fn path(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[test]
#[ignore = "needs wrangler: scripts/integration.sh"]
fn a_duplicate_insert_counts_nothing() {
    let dir = scratch();
    let hub = "INSERT INTO hubs (ip, port, status, source, first_seen, last_seen) \
               VALUES ('8.8.4.4', 6346, 'unverified', 'update', 1, 1) ON CONFLICT (ip) DO NOTHING";
    let cache = "INSERT INTO caches (url, host, status, first_seen) \
                 VALUES ('http://dup.example.net/', 'dup.example.net', 'pending', 1) \
                 ON CONFLICT (url) DO NOTHING";
    for _ in 0..2 {
        d1(dir.path(), &format!("{hub}; {COUNT_INSERTED_HUB}"));
        d1(dir.path(), &format!("{cache}; {COUNT_INSERTED_CACHE}"));
    }
    assert_eq!(
        number(dir.path(), "SELECT n FROM counts WHERE k = 'hubs_live'"),
        1
    );
    assert_eq!(
        number(dir.path(), "SELECT n FROM counts WHERE k = 'caches'"),
        1
    );

    // A distinct row still counts.
    d1(
        dir.path(),
        &format!(
            "{}; {COUNT_INSERTED_HUB}",
            hub.replace("8.8.4.4", "1.1.1.1")
        ),
    );
    assert_eq!(
        number(dir.path(), "SELECT n FROM counts WHERE k = 'hubs_live'"),
        2
    );
}

#[test]
#[ignore = "needs wrangler: scripts/integration.sh"]
fn a_requeued_candidate_leaves_its_count_once() {
    let dir = scratch();
    d1(
        dir.path(),
        "INSERT INTO hubs (ip, port, status, source, first_seen, last_seen) \
         VALUES ('9.9.9.9', 6346, 'unverified', 'try_hubs', 1, 1); \
         UPDATE counts SET n = 1 WHERE k = 'hubs_unverified'",
    );
    let uncount = UNCOUNT_REQUEUED_CANDIDATE.replace("?1", "'9.9.9.9'");
    let requeue = "UPDATE hubs SET source = 'update' WHERE ip = '9.9.9.9' AND status <> 'dead'";
    // Two requests that both read the row as a candidate: only the first
    // batch finds it still one.
    for _ in 0..2 {
        d1(dir.path(), &format!("{uncount}; {requeue}"));
    }
    assert_eq!(
        number(
            dir.path(),
            "SELECT n FROM counts WHERE k = 'hubs_unverified'"
        ),
        0
    );
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn concurrent_identical_submissions_keep_the_counter_exact() {
    // The Worker's own batches, racing: the same new cache URL submitted at
    // once from several addresses, so more than one request may find it
    // absent and insert it. Between cron runs the counter must still equal
    // the table.
    let state = std::env::var("GWC_STATE_DIR").expect("GWC_STATE_DIR");
    let base = std::env::var("GWC_BASE_URL").expect("GWC_BASE_URL");
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let base = base.clone();
            std::thread::spawn(move || {
                let r = Client::new(&base).request(
                    "GET",
                    "?url=http%3A%2F%2Frace.example.net%2Fgwc%2F&update=1&net=gnutella2&client=RAZA2.7.10.6",
                    &[("CF-Connecting-IP", &format!("2001:db8:7ace:{i:x}::1"))],
                );
                assert_eq!(r.status, 200);
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let rows = d1(
        &state,
        "SELECT (SELECT n FROM counts WHERE k = 'caches') AS counted, \
                (SELECT COUNT(*) FROM caches) AS stored, \
                (SELECT COUNT(*) FROM caches WHERE url = 'http://race.example.net/gwc/') AS raced",
    );
    let row = &rows[0][0];
    assert_eq!(row["raced"], 1, "{row}");
    assert_eq!(row["counted"], row["stored"], "{row}");
}
