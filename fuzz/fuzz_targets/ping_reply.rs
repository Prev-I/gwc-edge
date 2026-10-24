//! Another cache's answer to a ping, over HTTP: arbitrary bytes.
#![no_main]

use gwc_core::ping::{self, MAX_PONG, Reader};
use libfuzzer_sys::fuzz_target;

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
    if let Some(name) = closed {
        assert!(name.len() <= MAX_PONG);
    }
});
