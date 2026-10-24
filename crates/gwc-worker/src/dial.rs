//! Outbound connections for the verifying cron: hub handshakes over TCP, and
//! cache pings over TCP or `fetch()` (ADR-0012).
//!
//! Every exchange runs under one deadline that covers connect, write and
//! read: the Sockets API has no connect timeout, and an unreachable address
//! otherwise hangs for as long as the runtime allows. Any failure, at any
//! stage, is a failed check; refused and unreachable are not told apart.

use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::task::Poll;
use std::time::Duration;

use futures_util::StreamExt;
use gwc_core::handshake::{self, Exchange, Parse};
use gwc_core::ping;
use gwc_core::verify::{HANDSHAKE_TIMEOUT_MS, PING_BODY_MAX, PING_TIMEOUT_MS};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use worker::{
    Delay, Fetch, Headers, Method, Request, RequestInit, RequestRedirect, SecureTransport, Socket,
};

/// How long closing a socket may take: closing one to an unreachable address
/// can hang too, and the connection slot is only free once it is closed.
const CLOSE_TIMEOUT_MS: u64 = 1_000;

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

/// What a socket exchange left behind: whether its close timed out, in which
/// case the runtime may still count the connection as open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Leak(pub bool);

/// Connect to `host:port`, send `request`, and read until `done` says the
/// bytes so far are enough, the peer closes, or `max` bytes arrived; all
/// within `ms` milliseconds. Returns the bytes read, possibly incomplete.
async fn talk(
    host: &str,
    port: u16,
    request: &[u8],
    max: usize,
    ms: u64,
    done: impl Fn(&[u8]) -> bool,
) -> (Result<Vec<u8>, Failure>, Leak) {
    let Ok(mut socket) = Socket::builder()
        .secure_transport(SecureTransport::Off)
        .connect(host, port)
    else {
        return (Err(Failure::Unreachable), Leak(false));
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
    // reported when it timed out, so the caller can stop counting on it.
    let leak = Leak(within(CLOSE_TIMEOUT_MS, socket.close()).await.is_none());
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
        |b| !matches!(handshake::parse_reply(b), Parse::Incomplete),
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

/// Ping a cache over a socket: plain HTTP/1.0, no redirects. Returns the
/// cache's pong name, if it answered one.
pub async fn ping_socket(host: &str, port: u16, request: &str) -> (Option<String>, Leak) {
    let (bytes, leak) = talk(
        host,
        port,
        request.as_bytes(),
        PING_BODY_MAX,
        PING_TIMEOUT_MS,
        |_| false,
    )
    .await;
    let pong = bytes.ok().and_then(|bytes| {
        let (status, body) = ping::http_reply(&bytes)?;
        ping::pong(status, body)
    });
    (pong, leak)
}

/// Ping a cache with `fetch()`. Redirects are not followed, as over a socket:
/// each hop would be another subrequest, and a URL that only redirects is
/// not the cache it names. Reads at most [`PING_BODY_MAX`] bytes of the body.
pub async fn ping_fetch(url: &str) -> Option<String> {
    within(PING_TIMEOUT_MS, async {
        let headers = Headers::new();
        headers.set("User-Agent", &gwc_core::product_id()).ok()?;
        let mut init = RequestInit::new();
        init.with_method(Method::Get)
            .with_headers(headers)
            .with_redirect(RequestRedirect::Manual);
        let request = Request::new_with_init(url, &init).ok()?;
        let mut response = Fetch::Request(request).send().await.ok()?;
        let status = response.status_code();
        let mut stream = response.stream().ok()?;
        let mut body = Vec::new();
        while body.len() < PING_BODY_MAX {
            match stream.next().await {
                Some(Ok(chunk)) => body.extend_from_slice(&chunk),
                Some(Err(_)) => return None,
                None => break,
            }
        }
        ping::pong(status, &body)
    })
    .await
    .flatten()
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
