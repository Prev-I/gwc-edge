//! A cache's answer to an advertisement (ADR-0021), over HTTP: arbitrary
//! bytes, watched as they arrive.
#![no_main]

use gwc_core::advert::{BODY_MAX, ReplyWatch, classify};
use gwc_core::ping;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fed a growing buffer until decided, then closed: never a panic, and
    // linear work however it is split.
    let mut trickled = ReplyWatch::socket();
    for end in 1..=data.len() {
        trickled.feed(&data[..end]);
        if trickled.decided() {
            break;
        }
    }
    trickled.close(data);
    let mut once = ReplyWatch::socket();
    once.close(data);
    let mut body = ReplyWatch::body(200);
    body.close(data);
    assert_eq!(
        body.class(),
        classify(Some(200), &data[..data.len().min(BODY_MAX)])
    );

    // Without a Content-Length, the class is the one `classify` gives the
    // first BODY_MAX body bytes, however the reply was split. A length
    // narrows the classified body on purpose, so those replies are left
    // out (any header naming it, conservatively).
    let Some((status, rest)) = ping::http_reply(data) else {
        return;
    };
    let head = &data[..data.len() - rest.len()];
    if head
        .to_ascii_lowercase()
        .windows(14)
        .any(|w| w == b"content-length")
    {
        return;
    }
    let want = classify(Some(status), &rest[..rest.len().min(BODY_MAX)]);
    assert_eq!(once.class(), want, "at once");
    assert_eq!(trickled.class(), want, "trickled");
});
