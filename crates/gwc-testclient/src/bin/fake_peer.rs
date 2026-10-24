//! Fake peers for the verifying cron's integration tests: what the cron
//! reaches through `TEST_DIAL` instead of real hubs and caches.
//!
//! Usage: `fake_peer <base-port>`. Listens on 127.0.0.1, one port per
//! behaviour, until killed:
//!
//! | port | answers |
//! |---|---|
//! | base | a Shareaza hub with room: `200 OK`, G2 (verified) |
//! | base + 1 | a full hub: `503 Maximum connections reached` with `X-Try-Hubs` (`alive_full`) |
//! | base + 2 | a leaf: `503 Shielded leaf node` (dead) |
//! | base + 3 | a GWebCache: `i|pong|fake-cache 1.0|gnutella2` over HTTP |
//! | base + 5 | a hub that never answers (the cron times out) |
//! | base + 6 | a hub that closes in the middle of its header block |
//! | base + 7 | an HTTP redirect to a real cache (not followed) |
//! | base + 8 | another GWebCache's hostfile and urlfile, for `gwc_seed` |
//! | base + 9 | a GWebCache pong, then the connection held open for 20 s |
//! | base + 10 | half a pong line, then the connection held open for 20 s |
//!
//! base + 4 is left closed, for a connection that is refused.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Fresh `X-Try-Hubs` entries in the full hub's reply. `4.2.3.1` is offered
/// a few minutes ago, `4.2.3.2` two days ago (too old to take), and the
/// private address is never taken.
const FRESH_CANDIDATE: &str = "4.2.3.1:6346";

/// What a port answers with, given the request's header block.
type Behaviour = fn(&[u8]) -> Vec<u8>;

fn main() {
    let base: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .expect("usage: fake_peer <base-port>");
    let behaviours: [(u16, Behaviour); 10] = [
        (0, |_| hub_ok()),
        (1, |_| hub_full()),
        (2, |_| leaf()),
        (3, |_| cache()),
        (5, |_| stall()),
        (6, |_| cut_short()),
        (7, |_| redirect()),
        (8, other_cache),
        (9, |_| cache()),
        (10, |_| half_pong()),
    ];
    let mut threads = Vec::new();
    for (offset, reply) in behaviours {
        let linger = matches!(offset, 9 | 10);
        let listener = TcpListener::bind(("127.0.0.1", base + offset))
            .unwrap_or_else(|e| panic!("bind {}: {e}", base + offset));
        threads.push(std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || answer(stream, reply, linger));
            }
        }));
    }
    println!("fake_peer: listening on 127.0.0.1:{base}..{}", base + 10);
    for t in threads {
        let _ = t.join();
    }
}

/// Read the request's header block (or give up after 2 s), then reply and
/// close.
fn answer(mut stream: TcpStream, reply: Behaviour, linger: bool) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut got = Vec::new();
    let mut buf = [0u8; 1024];
    while !got.windows(4).any(|w| w == b"\r\n\r\n") && got.len() < 8192 {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
        }
    }
    let _ = stream.write_all(&reply(&got));
    // Some caches keep the connection open after replying: hold it well past
    // the cron's 10 s ping deadline.
    if linger {
        std::thread::sleep(Duration::from_secs(20));
    }
}

fn hub_ok() -> Vec<u8> {
    b"GNUTELLA/0.6 200 OK\r\n\
      User-Agent: Shareaza 2.7.10.6\r\n\
      Accept: application/x-gnutella2\r\n\
      Content-Type: application/x-gnutella2\r\n\
      X-Ultrapeer: True\r\n\
      X-Ultrapeer-Needed: False\r\n\
      \r\n"
        .to_vec()
}

fn hub_full() -> Vec<u8> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format!(
        "GNUTELLA/0.6 503 Maximum connections reached\r\n\
         User-Agent: Shareaza 2.7.10.6\r\n\
         Accept: application/x-gnutella2\r\n\
         Content-Type: application/x-gnutella2\r\n\
         X-Ultrapeer: True\r\n\
         X-Try-Hubs: {FRESH_CANDIDATE} {},4.2.3.2:6346 {},192.168.1.1:6346 {}\r\n\
         \r\n",
        minute(now - 300),
        minute(now - 2 * 86_400),
        minute(now - 300),
    )
    .into_bytes()
}

/// Never answers within the cron's 8 s: sleeps, then sends nothing.
fn stall() -> Vec<u8> {
    std::thread::sleep(Duration::from_secs(20));
    Vec::new()
}

/// A status line and one header, then the connection closes.
fn cut_short() -> Vec<u8> {
    b"GNUTELLA/0.6 200 OK\r\nUser-Agent: Shareaza 2.7.10.6\r\n".to_vec()
}

fn redirect() -> Vec<u8> {
    b"HTTP/1.0 302 Found\r\n\
      Location: http://dkac.trillinux.org/dkac/dkac.php\r\n\
      \r\n"
        .to_vec()
}

/// A GWebCache's G2 hostfile or urlfile reply, as `gwc_seed` asks for them:
/// two hubs and two caches it should take, and entries it must refuse.
fn other_cache(request: &[u8]) -> Vec<u8> {
    let request = String::from_utf8_lossy(request);
    let body = if request.contains("hostfile=1") {
        "i|pong|fake-cache 1.0|gnutella2\r\n\
         h|4.3.1.1:6346|60\r\n\
         h|4.3.2.1:6347|600\r\n\
         h|10.0.0.1:6346|5\r\n\
         h|4.3.3.1:80|5\r\n"
    } else {
        "u|http://seeded.example.net/gwc/|60\r\n\
         u|http://seeded.example.net/other/|60\r\n\
         u|https://tls.example.net/|60\r\n\
         u|http://gwc.example.org/|0\r\n"
    };
    format!("HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\n{body}").into_bytes()
}

/// A reply cut in the middle of its pong line.
fn half_pong() -> Vec<u8> {
    b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\ni|pong|fake-ca".to_vec()
}

fn leaf() -> Vec<u8> {
    b"GNUTELLA/0.6 503 Shielded leaf node\r\n\
      User-Agent: Shareaza 2.7.10.6\r\n\
      \r\n"
        .to_vec()
}

fn cache() -> Vec<u8> {
    b"HTTP/1.0 200 OK\r\n\
      Content-Type: text/plain\r\n\
      \r\n\
      i|pong|fake-cache 1.0|gnutella2\r\n"
        .to_vec()
}

/// `YYYY-MM-DDThh:mmZ`, as Shareaza writes `X-Try-Hubs` times.
fn minute(unix: u64) -> String {
    let days = i64::try_from(unix / 86_400).unwrap_or(0);
    let secs = unix % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60
    )
}

#[cfg(test)]
mod tests {
    use super::minute;

    #[test]
    fn minutes_are_formatted_like_shareaza() {
        assert_eq!(minute(0), "1970-01-01T00:00Z");
        assert_eq!(minute(1_792_744_200), "2026-10-23T08:30Z");
        assert_eq!(minute(951_782_400), "2000-02-29T00:00Z");
    }
}
