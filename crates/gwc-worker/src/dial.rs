//! Outbound connections for the verifying cron: hub handshakes over TCP, and
//! cache pings over TCP or `fetch()` (ADR-0012).
//!
//! Every exchange runs under one deadline that covers connect, write and
//! read: the Sockets API has no connect timeout, and an unreachable address
//! otherwise hangs for as long as the runtime allows. Any failure, at any
//! stage, is a failed check; refused and unreachable are not told apart.

use std::cell::Cell;
use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::rc::Rc;
use std::task::Poll;
use std::time::Duration;

use futures_util::StreamExt;
use gwc_core::advert::{self, Class, UrlScan};
use gwc_core::handshake::{self, Exchange, Parse};
use gwc_core::ping::{self, Listing};
use gwc_core::verify::{CLOSE_TIMEOUT_MS, HANDSHAKE_TIMEOUT_MS, PING_BODY_MAX, PING_TIMEOUT_MS};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use worker::wasm_bindgen_futures::spawn_local;
use worker::{
    Date, Delay, Fetch, Headers, Method, Request, RequestInit, RequestRedirect, SecureTransport,
    Socket,
};

/// Run `f`, or give up after `ms` milliseconds.
pub async fn within<T>(ms: u64, f: impl Future<Output = T>) -> Option<T> {
    let mut f = pin!(f);
    let mut deadline = pin!(Delay::from(Duration::from_millis(ms)));
    poll_fn(|cx| {
        if let Poll::Ready(v) = f.as_mut().poll(cx) {
            return Poll::Ready(Some(v));
        }
        if deadline.as_mut().poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        Poll::Pending
    })
    .await
}

/// Why a socket exchange produced no usable bytes.
enum Failure {
    Unreachable,
    Timeout,
}

/// What a socket exchange left behind: the close, if it timed out. The
/// runtime may count the connection as open until it completes, so the cron
/// stops counting on that slot, and learns when it ends (ADR-0019).
#[derive(Default)]
pub struct Leak(pub Option<PendingClose>);

/// A socket close still in progress, running as a task of its own so that
/// the moment it completes is recorded when it happens.
pub struct PendingClose {
    /// When the socket's connect was called.
    pub connected_ms: u64,
    /// How the close ended; `None` while it is pending.
    pub closed: Rc<Cell<Option<Closed>>>,
}

/// A leaked socket's close, completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed {
    pub at_ms: u64,
    /// The close resolved, which it does only once the runtime has released
    /// the connection. After a failed close the runtime may hold the
    /// connection until garbage collection, so its slot stays taken
    /// (ADR-0020).
    pub ok: bool,
}

impl PendingClose {
    /// Whether the socket may still hold a connection slot.
    #[must_use]
    pub fn holds_slot(&self) -> bool {
        !self.closed.get().is_some_and(|c| c.ok)
    }
}

/// Connect to `host:port`, send `request`, and read until `done` says the
/// bytes so far are enough, the peer closes, or `max` bytes arrived; all
/// within `ms` milliseconds. Returns the bytes read, possibly incomplete.
async fn talk(
    host: &str,
    port: u16,
    request: &[u8],
    max: usize,
    ms: u64,
    mut done: impl FnMut(&[u8]) -> bool,
) -> (Result<Vec<u8>, Failure>, Leak) {
    let connected_ms = Date::now().as_millis();
    let Ok(mut socket) = Socket::builder()
        .secure_transport(SecureTransport::Off)
        .connect(host, port)
    else {
        return (Err(Failure::Unreachable), Leak::default());
    };
    let outcome = within(ms, async {
        write_all(&mut socket, request).await?;
        let mut received = Vec::with_capacity(1024);
        let mut chunk = [0u8; 2048];
        while received.len() <= max && !done(&received) {
            let n = read(&mut socket, &mut chunk).await?;
            if n == 0 {
                break;
            }
            received.extend_from_slice(&chunk[..n]);
        }
        Ok::<_, std::io::Error>(received)
    })
    .await;
    // The slot is free only once the socket is closed; bounded as well, and
    // handed back when it timed out, so the caller can stop counting on it.
    let mut close = Box::pin(async move { socket.close().await.is_ok() });
    let leak = if within(CLOSE_TIMEOUT_MS, &mut close).await.is_some() {
        Leak(None)
    } else {
        // Finish the close as a task of its own, which notes when and how
        // it ends.
        let closed = Rc::new(Cell::new(None));
        let done = Rc::clone(&closed);
        spawn_local(async move {
            let ok = close.await;
            done.set(Some(Closed {
                at_ms: Date::now().as_millis(),
                ok,
            }));
        });
        Leak(Some(PendingClose {
            connected_ms,
            closed,
        }))
    };
    let result = match outcome {
        None => Err(Failure::Timeout),
        Some(Err(_)) => Err(Failure::Unreachable),
        Some(Ok(bytes)) => Ok(bytes),
    };
    (result, leak)
}

/// Handshake with the hub at `host:port`.
pub async fn handshake(host: &str, port: u16) -> (Exchange, Leak) {
    let request = handshake::connect_request();
    let (bytes, leak) = talk(
        host,
        port,
        request.as_bytes(),
        handshake::MAX_REPLY,
        HANDSHAKE_TIMEOUT_MS,
        {
            // Linear in the reply's length, however it is trickled.
            let mut watch = handshake::ReplyWatch::default();
            move |b: &[u8]| watch.settled(b)
        },
    )
    .await;
    let exchange = match bytes {
        Err(Failure::Timeout) => Exchange::Timeout,
        Err(Failure::Unreachable) => Exchange::Unreachable,
        Ok(bytes) => match handshake::parse_reply(&bytes) {
            Parse::Done(reply) => Exchange::Reply(reply),
            Parse::Invalid => Exchange::Invalid,
            // The peer closed before the header block ended.
            Parse::Incomplete => Exchange::Closed,
        },
    };
    (exchange, leak)
}

/// What a ping found: the cache's pong name, if it answered one, and
/// whether its urlfile lists this cache (ADR-0021).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PingResult {
    pub pong: Option<String>,
    pub listing: Listing,
}

/// Ping a cache over a socket: plain HTTP/1.0, no redirects. Returns the
/// cache's pong name, if it answered one. Reading stops as soon as the reply
/// is settled (`ping::Reader::settled`): without a scan, once it is judged,
/// so a cache that answers and keeps the connection open is not left to the
/// deadline; with one, also once the urlfile has been read far enough to
/// tell whether it lists this cache (ADR-0021).
pub async fn ping_socket(
    host: &str,
    port: u16,
    request: &str,
    scan: Option<UrlScan>,
) -> (PingResult, Leak) {
    let mut reader = ping::Reader::new();
    if let Some(scan) = scan {
        reader = reader.scanning(scan);
    }
    let (bytes, leak) = talk(
        host,
        port,
        request.as_bytes(),
        PING_BODY_MAX,
        PING_TIMEOUT_MS,
        |b: &[u8]| {
            reader.feed(b);
            reader.settled()
        },
    )
    .await;
    // Each arm's pong is the verdict latched at the pong line, when there
    // is one: `feed`, `close` and `at_deadline` all return it first.
    let (pong, end) = match bytes {
        // Stopped at the size limit: `done` never saw the last chunk. Judge
        // what arrived, that chunk included, but never as a closed reply.
        Ok(bytes) if bytes.len() > PING_BODY_MAX => (
            reader.feed(&bytes).and_then(ping::Verdict::pong),
            ping::End::Cut,
        ),
        // Settled while reading. The listing of a settled reader does not
        // depend on how the read ended; `Cut` claims nothing about it.
        Ok(_) if reader.settled() => (
            reader.verdict().and_then(ping::Verdict::pong),
            ping::End::Cut,
        ),
        // The peer closed: whatever arrived is the whole reply, and `close`
        // hands its last line to the scan.
        Ok(bytes) => (reader.close(&bytes), ping::End::Closed),
        // The deadline, or the connection failed mid-reply: a body that had
        // reached its Content-Length still counts.
        Err(_) => (reader.at_deadline(), ping::End::Deadline),
    };
    let listing = reader.listing(end);
    (PingResult { pong, listing }, leak)
}

/// How a `fetch()` ping's read stopped.
enum FetchEnd {
    /// The reader settled.
    Settled,
    /// The body ran past [`PING_BODY_MAX`].
    Cut,
    /// The body stream failed.
    Failed,
    /// The body stream ended.
    StreamEnd,
}

/// A GET for `url` with the product-id User-Agent that does not follow
/// redirects: each hop would be another subrequest, and a URL that only
/// redirects is not the cache it names.
fn get_request(url: &str) -> Option<Request> {
    let headers = Headers::new();
    headers.set("User-Agent", &gwc_core::product_id()).ok()?;
    let mut init = RequestInit::new();
    init.with_method(Method::Get)
        .with_headers(headers)
        .with_redirect(RequestRedirect::Manual);
    Request::new_with_init(url, &init).ok()
}

/// Ping a cache with `fetch()`. Redirects are not followed, as over a socket.
/// Stops reading once the body is past [`PING_BODY_MAX`] bytes (so at most
/// one chunk more), or as soon as the reply is settled
/// (`ping::Reader::settled`; see [`ping_socket`]). The runtime delimits the
/// body itself, so `Content-Length` is not used: it may count the compressed
/// bytes of a body the runtime has already decoded.
///
/// The reader and the body live outside the deadline, so a deadline after
/// the pong line still yields that pong, and the scan so far.
pub async fn ping_fetch(url: &str, scan: Option<UrlScan>) -> PingResult {
    let mut reader: Option<ping::Reader> = None;
    let mut body = Vec::new();
    let stop = within(PING_TIMEOUT_MS, async {
        let request = get_request(url)?;
        let mut response = Fetch::Request(request).send().await.ok()?;
        let mut fresh = ping::Reader::body(response.status_code());
        if let Some(scan) = scan {
            fresh = fresh.scanning(scan);
        }
        let reader = reader.insert(fresh);
        let mut stream = response.stream().ok()?;
        loop {
            reader.feed(&body);
            if reader.settled() {
                return Some(FetchEnd::Settled);
            }
            if body.len() > PING_BODY_MAX {
                return Some(FetchEnd::Cut);
            }
            match stream.next().await {
                Some(Ok(chunk)) => body.extend_from_slice(&chunk),
                Some(Err(_)) => return Some(FetchEnd::Failed),
                None => return Some(FetchEnd::StreamEnd),
            }
        }
    })
    .await;
    let Some(mut reader) = reader else {
        // No response: no pong, and nothing scanned.
        return PingResult {
            pong: None,
            listing: Listing::Unknown,
        };
    };
    let end = match stop.flatten() {
        // The stream ended: the body is whole, and `close` hands its last
        // line to the scan, then judges it if nothing has yet.
        Some(FetchEnd::StreamEnd) => {
            let pong = reader.close(&body);
            let listing = reader.listing(ping::End::StreamEnd);
            return PingResult { pong, listing };
        }
        // A settled reader's listing does not depend on the end.
        Some(FetchEnd::Settled | FetchEnd::Cut) => ping::End::Cut,
        // The deadline, the stream failing, or the body stream not opening.
        Some(FetchEnd::Failed) | None => ping::End::Deadline,
    };
    // Only the latched verdict counts here: a body that was still arriving
    // is no pong, as before the scan.
    PingResult {
        pong: reader.verdict().and_then(ping::Verdict::pong),
        listing: reader.listing(end),
    }
}

/// Advertise this cache over a socket: `request` is the plain HTTP/1.0 GET
/// for the update. Reads at most [`advert::READ_MAX`] bytes in total, headers
/// included (`talk` may read one chunk more; the excess is not looked at),
/// and stops once the reply's class is decided ([`advert::ReplyWatch`]). A
/// timeout, a failed connection or a cut reply is `Failed` unless already
/// decided.
pub async fn advertise_socket(host: &str, port: u16, request: &str) -> (Class, Leak) {
    let mut watch = advert::ReplyWatch::socket();
    let (bytes, leak) = talk(
        host,
        port,
        request.as_bytes(),
        advert::READ_MAX,
        advert::TIMEOUT_MS,
        |b: &[u8]| {
            watch.feed(b);
            watch.decided()
        },
    )
    .await;
    if let Ok(bytes) = bytes {
        if bytes.len() > advert::READ_MAX {
            // Cut at the limit: `done` never saw the last chunk.
            watch.feed(&bytes[..advert::READ_MAX]);
        } else {
            // The peer closed: whatever arrived is the whole reply.
            watch.close(&bytes);
        }
    }
    (watch.class(), leak)
}

/// Advertise this cache with `fetch()` to `url`, the update URL. No
/// redirects, as for pings. Reads the body until the reply's class is
/// decided ([`advert::ReplyWatch`], at most [`advert::BODY_MAX`] bytes and
/// one chunk) or the body ends. A timeout, or a failed request or body
/// stream, is `Failed` unless already decided.
pub async fn advertise_fetch(url: &str) -> Class {
    within(advert::TIMEOUT_MS, async {
        let request = get_request(url)?;
        let mut response = Fetch::Request(request).send().await.ok()?;
        let mut watch = advert::ReplyWatch::body(response.status_code());
        if watch.decided() {
            return Some(watch.class());
        }
        let mut stream = response.stream().ok()?;
        let mut body = Vec::new();
        loop {
            watch.feed(&body);
            if watch.decided() {
                break;
            }
            match stream.next().await {
                Some(Ok(chunk)) => body.extend_from_slice(&chunk),
                Some(Err(_)) => break,
                None => {
                    watch.close(&body);
                    break;
                }
            }
        }
        Some(watch.class())
    })
    .await
    .flatten()
    .unwrap_or(Class::Failed)
}

async fn write_all(socket: &mut Socket, mut bytes: &[u8]) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let n = poll_fn(|cx| Pin::new(&mut *socket).poll_write(cx, bytes)).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[n..];
    }
    poll_fn(|cx| Pin::new(&mut *socket).poll_flush(cx)).await
}

async fn read(socket: &mut Socket, chunk: &mut [u8]) -> std::io::Result<usize> {
    let mut buf = ReadBuf::new(chunk);
    poll_fn(|cx| Pin::new(&mut *socket).poll_read(cx, &mut buf)).await?;
    Ok(buf.filled().len())
}
