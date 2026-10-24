//! Another cache's answer to a ping, over HTTP: arbitrary bytes.
#![no_main]

use gwc_core::ping::{self, MAX_PONG};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some((status, body)) = ping::http_reply(data)
        && let Some(name) = ping::pong(status, body)
    {
        assert!(name.len() <= MAX_PONG);
        assert!(name.chars().all(|c| c.is_ascii_graphic() || c == ' '));
    }
});
