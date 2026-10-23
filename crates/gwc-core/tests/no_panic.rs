//! Parsers face arbitrary input from the network: none may panic, whatever
//! the bytes. A cheap stand-in for fuzzing that runs on every `cargo test`.

use gwc_core::{addr, cache_url, handshake, query};
use proptest::prelude::*;

proptest! {
    #[test]
    fn query_parse_never_panics(raw in ".{0,400}") {
        let _ = query::parse(&raw);
    }

    #[test]
    fn query_parse_never_panics_on_shaped_input(
        pairs in proptest::collection::vec(("[a-z._]{1,10}", "[ -~]{0,40}"), 0..12)
    ) {
        let raw: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let _ = query::parse(&raw.join("&"));
    }

    #[test]
    fn addr_parse_never_panics(raw in ".{0,64}") {
        let _ = addr::parse_hub(&raw);
        let _ = addr::source_matches(&raw, std::net::Ipv4Addr::LOCALHOST);
    }

    #[test]
    fn cache_url_never_panics(raw in ".{0,300}") {
        let _ = cache_url::normalize(&raw);
    }

    #[test]
    fn normalised_urls_are_stable(
        raw in "http://[a-z0-9.-]{1,30}(:[0-9]{1,5})?(/[a-zA-Z0-9._/-]{0,40})?"
    ) {
        if let Ok(u) = cache_url::normalize(&raw) {
            prop_assert_eq!(cache_url::normalize(&u.url), Ok(u.clone()));
        }
    }

    #[test]
    fn handshake_parse_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        if let handshake::Parse::Done(reply) = handshake::parse_reply(&bytes) {
            let _ = reply.verdict();
            let _ = reply.try_hubs();
        }
    }

    #[test]
    fn handshake_parse_never_panics_on_shaped_input(
        status in "GNUTELLA/0\\.6 [0-9]{3}[ -~]{0,30}",
        headers in proptest::collection::vec("[ -~]{0,80}", 0..10),
    ) {
        let raw = format!("{status}\r\n{}\r\n\r\n", headers.join("\r\n"));
        if let handshake::Parse::Done(reply) = handshake::parse_reply(raw.as_bytes()) {
            let _ = reply.verdict();
            let _ = reply.try_hubs();
        }
    }
}
