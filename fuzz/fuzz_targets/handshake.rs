//! A hub's reply to the verifier's handshake: arbitrary bytes from the network.
#![no_main]

use gwc_core::handshake::{self, MAX_TRY_HUBS, Parse};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Parse::Done(reply) = handshake::parse_reply(data) {
        let _ = reply.verdict();
        let hubs = reply.try_hubs();
        assert!(hubs.len() <= MAX_TRY_HUBS);
        for (addr, _) in hubs {
            assert!(addr.port() >= 1024);
            assert!(gwc_core::addr::is_public_unicast(*addr.ip()));
        }
    }
});
