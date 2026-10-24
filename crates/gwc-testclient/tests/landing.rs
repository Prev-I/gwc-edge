//! Everything that is not the protocol: landing page, robots, methods, alias
//! hosts and the edge cache. Run by `scripts/integration.sh` after a cron run,
//! against the cache it seeded.

use gwc_testclient::{Client, Reply};

fn client() -> Client {
    Client::new(&std::env::var("GWC_BASE_URL").expect("GWC_BASE_URL (run scripts/integration.sh)"))
}

/// The canonical host, as a browser on the real domain would send it.
fn canonical_host() -> String {
    let url = std::env::var("GWC_CANONICAL_URL").expect("GWC_CANONICAL_URL");
    url.trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned()
}

fn browser(method: &str, suffix: &str) -> Reply {
    let host = canonical_host();
    client().request(method, suffix, &[("Host", &host), ("Accept", "text/html")])
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn root_without_params_is_the_landing_page() {
    let r = browser("GET", "/");
    assert_eq!(r.status, 200);
    assert_eq!(r.content_type.as_deref(), Some("text/html; charset=utf-8"));
    assert_eq!(r.cache_control.as_deref(), Some("public, max-age=300"));
    assert!(r.body.len() < 8 * 1024, "{} bytes", r.body.len());
    let lower = r.body.to_ascii_lowercase();
    assert!(lower.starts_with("<!doctype html>"));
    assert!(!lower.contains("<script"));
    assert!(
        r.body
            .contains(&std::env::var("GWC_CANONICAL_URL").unwrap())
    );
    assert!(r.body.contains("Verified hubs"));
    assert!(
        !r.body.contains("8.8.4.4") && !r.body.contains("1.1.1.1"),
        "no hub addresses"
    );
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn landing_page_second_hit_is_served_from_the_edge_cache() {
    let _ = browser("GET", "/");
    let r = browser("GET", "/");
    assert_eq!(r.header("x-gwc-cache"), Some("hit"));
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn stats_second_hit_is_served_from_the_edge_cache() {
    let _ = client().get("/stats");
    let r = client().get("/stats");
    assert_eq!(r.status, 200);
    assert_eq!(r.cache_control.as_deref(), Some("public, max-age=60"));
    assert_eq!(r.header("x-gwc-cache"), Some("hit"));
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn a_browser_with_gwc_params_gets_the_protocol() {
    let r = browser(
        "GET",
        "/?get=1&hostfile=1&net=gnutella2&ping=1&client=RAZA2.7.10.6",
    );
    assert_eq!(r.status, 200);
    assert_eq!(r.content_type.as_deref(), Some("text/plain; charset=utf-8"));
    assert!(r.lines()[0].starts_with("i|pong|"));
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn head_matches_get_without_a_body() {
    for suffix in [
        "/",
        "/?ping=1&client=TEST&version=0.1",
        "/stats",
        "/robots.txt",
    ] {
        let get = browser("GET", suffix);
        let head = browser("HEAD", suffix);
        assert_eq!(head.status, get.status, "{suffix}");
        assert_eq!(head.content_type, get.content_type, "{suffix}");
        assert_eq!(head.body, "", "{suffix}");
    }
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn other_methods_are_405_with_an_empty_body() {
    for method in ["POST", "PUT", "DELETE", "OPTIONS", "PATCH"] {
        let r = browser(method, "/?get=1&hostfile=1&client=RAZA2.7.10.6");
        assert_eq!(r.status, 405, "{method}");
        assert_eq!(r.body, "", "{method}");
        assert_eq!(r.header("allow"), Some("GET, HEAD"), "{method}");
    }
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn robots_txt_disallows_everything() {
    let r = client().get("/robots.txt");
    assert_eq!(r.status, 200);
    assert_eq!(r.body, "User-agent: *\nDisallow: /\n");
}

#[test]
#[ignore = "needs a running cache: scripts/integration.sh"]
fn an_alias_host_redirects_the_page_but_not_the_protocol() {
    let alias = [("Host", "alias.example.net")];
    let page = client().request("GET", "/", &alias);
    assert_eq!(page.status, 301);
    assert_eq!(
        page.header("location"),
        std::env::var("GWC_CANONICAL_URL").ok().as_deref()
    );

    let gwc = client().request(
        "GET",
        "/?get=1&urlfile=1&net=gnutella2&ping=1&client=RAZA2.7.10.6",
        &alias,
    );
    assert_eq!(gwc.status, 200, "never a redirect on the protocol path");
    let self_line = format!("u|{}|0", std::env::var("GWC_CANONICAL_URL").unwrap());
    assert_eq!(
        gwc.lines().last().copied(),
        Some(self_line.as_str()),
        "the canonical URL, not the alias"
    );
}
