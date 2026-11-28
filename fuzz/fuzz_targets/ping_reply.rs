//! Another cache's answer to a ping, over HTTP: arbitrary bytes.
#![no_main]

use gwc_core::advert::UrlScan;
use gwc_core::cache_url::CacheUrl;
use gwc_core::ping::{self, End, MAX_PONG, Reader};
use libfuzzer_sys::fuzz_target;

/// The scan an advertisement's ping carries (ADR-0021), fixed.
fn scanning() -> Reader {
    let me = CacheUrl {
        url: "http://gwc.example.org/".into(),
        host: "gwc.example.org".into(),
    };
    Reader::new().scanning(UrlScan::new("peer.example.net", &me))
}

fuzz_target!(|data: &[u8]| {
    if let Some((status, body)) = ping::http_reply(data)
        && let Some(name) = ping::pong(status, body)
    {
        assert!(name.len() <= MAX_PONG);
        assert!(name.chars().all(|c| c.is_ascii_graphic() || c == ' '));
    }

    // The verdict must not depend on how the bytes were split into reads,
    // and a name, whenever there is one, stays within its bounds.
    let mut whole = Reader::new();
    let at_once = whole.feed(data);
    let mut trickled = Reader::new();
    let mut first = None;
    for end in 1..=data.len() {
        if let Some(v) = trickled.feed(&data[..end]) {
            first = Some(v);
            break;
        }
    }
    assert_eq!(at_once, first, "chunking changed the verdict");
    assert!(trickled.examined() <= 8 * data.len().max(1));
    // The close and deadline verdicts must not depend on chunking either.
    let mut fed = Reader::new();
    for end in 1..=data.len() {
        let _ = fed.feed(&data[..end]);
    }
    let closed = Reader::new().close(data);
    assert_eq!(
        fed.clone().close(data),
        closed,
        "chunking changed the close verdict"
    );
    let mut once = Reader::new();
    let _ = once.feed(data);
    assert_eq!(
        fed.at_deadline(),
        once.at_deadline(),
        "chunking changed the deadline verdict"
    );
    if let Some(name) = &closed {
        assert!(name.len() <= MAX_PONG);
    }

    // A scan reads on after the pong, but never changes the verdict: it is
    // fixed at the pong, at every read, at the close and at the deadline
    // (ADR-0021). The listing's own split-invariance is not asserted: a
    // read ending exactly at a Content-Length that cuts a line is an
    // accepted residual (`a_read_ending_at_the_length_settles_before_any_cut_shows`).
    let mut plain = Reader::new();
    let mut scan = scanning();
    for end in 1..=data.len() {
        assert_eq!(
            plain.feed(&data[..end]),
            scan.feed(&data[..end]),
            "a scan changed the verdict at {end}"
        );
    }
    assert!(scan.examined() <= 8 * data.len().max(1));
    assert_eq!(plain.at_deadline(), scan.at_deadline(), "deadline");
    assert_eq!(plain.clone().close(data), scan.clone().close(data), "close");
    let mut scan_once = scanning();
    assert_eq!(scan_once.feed(data), at_once, "a scan changed the verdict");
    assert_eq!(scan_once.at_deadline(), once.at_deadline(), "deadline");
    assert_eq!(scan_once.clone().close(data), closed, "close");
    let mut scan_closed = scanning();
    let _ = scan_closed.close(data);
    for end in [End::Closed, End::StreamEnd, End::Cut, End::Deadline] {
        let _ = scan.listing(end);
        let _ = scan_closed.listing(end);
    }
});
