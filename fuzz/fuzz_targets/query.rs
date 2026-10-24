//! The query parser and everything a request's query reaches without D1:
//! the reply it would get, and what an update would claim.
#![no_main]

use gwc_core::cache_url::normalize;
use gwc_core::{query, response, update};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(q) = query::parse(raw) else {
        return;
    };
    assert_eq!(q.client.vendor.len(), 4);
    assert!(
        q.client
            .vendor
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    );
    assert!(q.client.version.len() <= query::MAX_VERSION_LEN);

    let canonical = normalize("http://gwc.example.org/").unwrap();
    let body = response::render(&response::get_reply(&q, &[], &[], &canonical));
    // Shareaza splits on CR and LF: no line may carry either, and every line
    // is one of ours.
    for line in body.split_terminator('\n') {
        assert!(!line.contains('\r'), "{line:?}");
        assert!(
            ["h|", "u|", "i|"].iter().any(|p| line.starts_with(p)),
            "{line:?}"
        );
    }
    assert!(body.is_ascii());

    if let Some(Ok(claim)) = update::ip_claim(&q, &claim_source(&q), 1_792_744_200) {
        assert!(claim.addr.port() >= 1024);
        assert!(gwc_core::addr::is_public_unicast(*claim.addr.ip()));
    }
    if let Some(Ok(url)) = update::url_claim(&q, &canonical) {
        assert!(url.url.starts_with("http://"));
    }
});

/// The claimed IP itself, so the source check passes and the rest is reached.
fn claim_source(q: &query::Query) -> String {
    q.ip.as_deref()
        .and_then(|ip| ip.split(':').next())
        .unwrap_or_default()
        .to_owned()
}
