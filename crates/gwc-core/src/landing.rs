//! The landing page for people: what this is, how to add it to Shareaza, and
//! aggregate numbers. One self-contained HTML document: inline CSS, no
//! scripts, no external assets, under [`MAX_BYTES`].
//!
//! It never shows a hub address: hubs are mostly residential connections.
//! The inputs carry only counts, so it cannot.

use std::fmt::Write as _;

use crate::stats::Stats;

/// Upper bound on the rendered page.
pub const MAX_BYTES: usize = 8 * 1024;

/// Client/version pairs listed; the rest are summed as "others".
pub const TOP_CLIENTS: usize = 10;

/// The repository, from the workspace manifest.
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// What the page shows.
#[derive(Debug, Clone, Copy)]
pub struct Page<'a> {
    pub canonical_url: &'a str,
    pub git_sha: &'a str,
    /// `None` when the numbers cannot be read (D1 down, or no cron run yet).
    pub stats: Option<&'a Stats>,
}

/// Render the page.
#[must_use]
pub fn render(page: &Page) -> String {
    let mut h = String::with_capacity(4096);
    h.push_str(HEAD);
    let _ = write!(
        h,
        "<header><h1>gwc-edge — Gnutella2 Web Cache</h1><p class=\"m\">{} · {}</p>\
         <p>Bootstraps Gnutella2 clients: hubs register here; clients ask for recent \
         hubs and for other caches.</p></header>\
         <h2>Add this cache to Shareaza</h2><p><code>{}</code></p>\
         <p class=\"m\">In Shareaza: Discovery → Add, type GWebCache. Keep <b>http://</b>: \
         Shareaza 2.7 cannot use https://.</p>",
        esc(&crate::product_id()),
        esc(page.git_sha),
        esc(page.canonical_url),
    );
    match page.stats {
        Some(s) => numbers(&mut h, s),
        None => {
            h.push_str("<h2>Network</h2><p class=\"m\">Statistics are unavailable right now.</p>");
        }
    }
    let _ = write!(
        h,
        "<footer class=\"m\"><p>Source: <a href=\"{repo}\">{repo}</a> · \
         Machine-readable: <a href=\"/stats\">/stats</a></p></footer></body></html>",
        repo = esc(REPOSITORY),
    );
    h
}

const HEAD: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<meta name=\"robots\" content=\"noindex\"><title>gwc-edge — Gnutella2 Web Cache</title><style>\
:root{color-scheme:light dark;--fg:#1a1a1a;--bg:#fafafa;--mut:#666;--line:#ddd;--acc:#0b57d0}\
@media (prefers-color-scheme: dark){:root{--fg:#e8e8e8;--bg:#161616;--mut:#999;--line:#333;--acc:#8ab4f8}}\
body{font:15px/1.5 system-ui,sans-serif;color:var(--fg);background:var(--bg);max-width:42rem;margin:2rem auto;padding:0 1rem}\
h1{font-size:1.3rem;margin:0}h2{font-size:.8rem;letter-spacing:.06em;text-transform:uppercase;color:var(--mut);margin:1.8rem 0 .4rem}\
code{font-size:1.05rem;padding:.2rem .4rem;border:1px solid var(--line);border-radius:4px;user-select:all}\
table{border-collapse:collapse;width:100%}td{padding:.15rem 0;border-bottom:1px solid var(--line)}\
td.n{text-align:right;font-variant-numeric:tabular-nums}.m{color:var(--mut);font-size:.9rem}a{color:var(--acc)}\
</style></head><body>";

/// The aggregate sections.
fn numbers(h: &mut String, s: &Stats) {
    let row =
        |label: &str, n: u64| format!("<tr><td>{}</td><td class=\"n\">{n}</td></tr>", esc(label));
    let run = &s.cron;
    let as_of = if run.last_run_at == 0 {
        "not yet".to_owned()
    } else {
        format!("as of {}", utc(run.last_run_at))
    };
    let _ = write!(
        h,
        "<h2>Network <span class=\"m\">{as_of}</span></h2><table>{}{}{}</table>",
        row("Verified hubs", s.hubs.verified),
        row("Alive but full", s.hubs.alive_full),
        row("Known caches", s.caches.verified),
    );
    if run.last_run_at > 0 {
        let _ = write!(
            h,
            "<h2>Last cron run <span class=\"m\">{} · {} ms</span></h2>\
             <p>{} candidates · {} verified · {} full · {} dead · {} skipped</p>",
            utc(run.last_run_at),
            run.last_run_ms,
            run.candidates,
            run.verified,
            run.alive_full,
            run.dead,
            run.skipped_budget,
        );
    }
    let r = &s.requests;
    let _ = write!(
        h,
        "<h2>Requests, last {} h <span class=\"m\">estimated, 1 in {} sampled</span></h2><table>{}",
        s.window_hours,
        s.sample_rate,
        row("Total", r.total),
    );
    for c in r.by_client.iter().take(TOP_CLIENTS) {
        h.push_str(&row(&format!("{} {}", c.client, c.version), c.n));
    }
    let others = r.other_clients
        + r.by_client
            .iter()
            .skip(TOP_CLIENTS)
            .map(|c| c.n)
            .sum::<u64>();
    if others > 0 {
        h.push_str(&row("Others", others));
    }
    let _ = write!(
        h,
        "</table><h2>Health, last {} h</h2><p>D1 errors {} · degraded responses {}</p>",
        s.window_hours, s.health.d1_errors_24h, s.health.degraded_responses_24h,
    );
}

/// `2026-10-23 14:20 UTC`.
fn utc(t: u64) -> String {
    // Howard Hinnant's civil_from_days, for t >= 0.
    let (days, secs) = (t / 86_400, t % 86_400);
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        secs / 3600,
        secs % 3600 / 60
    )
}

/// Escape text for HTML element content and attribute values.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::stats::{CronRun, CronSummary, HubCounts, Totals, compose, hourly};

    const NOW: u64 = 1_792_744_200; // 2026-10-23 08:30:00 UTC

    fn stats(clients: usize) -> Stats {
        let mut h = hourly(Totals::default(), NOW - 600);
        h.requests.total = 9872;
        h.requests.by_client = (0..clients)
            .map(|i| crate::stats::ClientCount {
                client: "RAZA".into(),
                version: format!("2.7.10.{i}-abcdefg"),
                n: 1000 - i as u64,
            })
            .collect();
        h.health.d1_errors_24h = 3;
        let cron = CronSummary {
            hubs: HubCounts {
                served: 35,
                verified: 31,
                alive_full: 4,
                unverified: 3,
                dead: 12,
                total: 50,
            },
            run: CronRun {
                last_run_at: NOW - 120,
                last_run_ms: 412,
                candidates: 20,
                verified: 17,
                alive_full: 2,
                dead: 1,
                skipped_budget: 0,
            },
            ..CronSummary::default()
        };
        compose(h, cron, "a1b2c3d", "http://gwc.example.org/")
    }

    fn page(s: Option<&Stats>) -> String {
        render(&Page {
            canonical_url: "http://gwc.example.org/",
            git_sha: "a1b2c3d",
            stats: s,
        })
    }

    #[test]
    fn under_8kb_with_maximum_data() {
        let s = stats(crate::stats::MAX_CLIENTS);
        let html = page(Some(&s));
        assert!(html.len() < MAX_BYTES, "{} bytes", html.len());
    }

    #[test]
    fn self_contained_no_script_no_external_assets() {
        let html = page(Some(&stats(5))).to_ascii_lowercase();
        assert!(html.starts_with("<!doctype html>"));
        for banned in ["<script", "src=", "<link", "@import", "url("] {
            assert!(!html.contains(banned), "{banned}");
        }
        assert!(html.contains("<style>"));
    }

    #[test]
    fn has_light_and_dark_schemes() {
        assert!(page(None).contains("prefers-color-scheme: dark"));
    }

    #[test]
    fn shows_canonical_url_and_the_http_note() {
        let html = page(None);
        assert!(html.contains("<code>http://gwc.example.org/</code>"));
        assert!(html.contains("Discovery"));
        assert!(
            html.contains("https://"),
            "the note names https:// as the wrong scheme"
        );
    }

    #[test]
    fn shows_version_and_git_sha_and_links() {
        let html = page(None);
        assert!(html.contains(&crate::product_id()));
        assert!(html.contains("a1b2c3d"));
        assert!(html.contains(&format!("href=\"{REPOSITORY}\"")));
        assert!(html.contains("href=\"/stats\""));
    }

    #[test]
    fn shows_the_numbers() {
        let html = page(Some(&stats(12)));
        for text in [
            ">31<",
            ">4<",
            ">9872<",
            "412 ms",
            "2026-10-23 08:28 UTC",
            "20 candidates",
        ] {
            assert!(html.contains(text), "{text}");
        }
        assert!(html.contains("2.7.10.0-abcdefg"));
        assert!(
            !html.contains("2.7.10.10-abcdefg"),
            "only the top {TOP_CLIENTS}"
        );
        assert!(html.contains("D1 errors"));
    }

    #[test]
    fn degraded_shows_unavailable() {
        let html = page(None);
        assert!(html.contains("unavailable"));
        assert!(html.len() < MAX_BYTES);
    }

    #[test]
    fn escapes_text() {
        assert_eq!(
            esc(r#"<a href="x">&'"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
        let mut s = stats(1);
        s.requests.by_client[0].client = "<b>".into();
        assert!(page(Some(&s)).contains("&lt;b&gt;"));
    }

    #[test]
    fn utc_formatting() {
        assert_eq!(utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc(NOW), "2026-10-23 08:30 UTC");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00 UTC");
    }

    proptest! {
        #[test]
        fn never_contains_an_ipv4_address(
            counts in proptest::collection::vec(any::<u32>(), 12),
            version in "[A-Za-z0-9_-]{1,16}",
        ) {
            // Versions here have no dots, so any dotted quad would have come
            // from the template itself.
            let mut s = stats(1);
            s.hubs.served = counts[0].into();
            s.hubs.verified = counts[1].into();
            s.hubs.alive_full = counts[2].into();
            s.caches.verified = counts[3].into();
            s.cron.last_run_ms = counts[4].into();
            s.cron.candidates = counts[5].into();
            s.requests.total = counts[6].into();
            s.health.d1_errors_24h = counts[7].into();
            s.requests.by_client[0].version = version;
            let html = page(Some(&s));
            let dotted_quad = html.split(|c: char| !(c.is_ascii_digit() || c == '.'))
                .any(|t| t.split('.').filter(|p| !p.is_empty()).count() == 4);
            prop_assert!(!dotted_quad);
        }
    }
}
