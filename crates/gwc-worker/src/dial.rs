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
use gwc_core::handshake::{self, Exchange, Parse};
use gwc_core::ping;
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

/// Ping a cache over a socket: plain HTTP/1.0, no redirects. Returns the
/// cache's pong name, if it answered one. Reading stops as soon as the reply
/// is judged (`ping::Reader`): a cache that answers and keeps the connection
/// open is not left to the deadline.
pub async fn ping_socket(host: &str, port: u16, request: &str) -> (Option<String>, Leak) {
    let mut reader = ping::Reader::new();
    let mut verdict = None;
    let (bytes, leak) = talk(
        host,
        port,
        request.as_bytes(),
        PING_BODY_MAX,
        PING_TIMEOUT_MS,
        |b: &[u8]| {
            verdict = reader.feed(b);
            verdict.is_some()
        },
    )
    .await;
    let pong = match (bytes, verdict) {
        // Settled while reading.
        (Ok(_), Some(verdict)) => verdict.pong(),
        // Stopped at the size limit: judge what arrived, the last chunk
        // included, but never as a closed reply.
        (Ok(bytes), None) if bytes.len() > PING_BODY_MAX => {
            reader.feed(&bytes).and_then(ping::Verdict::pong)
        }
        // The peer closed: whatever arrived is the whole reply.
        (Ok(bytes), None) => reader.close(&bytes),
        // The deadline, or the connection failed mid-reply: a body that had
        // reached its Content-Length still counts.
        (Err(_), _) => reader.at_deadline(),
    };
    (pong, leak)
}

/// Ping a cache with `fetch()`. Redirects are not followed, as over a socket:
/// each hop would be another subrequest, and a URL that only redirects is
/// not the cache it names. Reads at most [`PING_BODY_MAX`] bytes of the body
/// and stops as soon as the reply is judged (`ping::Reader`). The runtime
/// delimits the body itself, so `Content-Length` is not used: it may count
/// the compressed bytes of a body the runtime has already decoded.
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
        let mut reader = ping::Reader::body(response.status_code());
        let mut stream = response.stream().ok()?;
        let mut body = Vec::new();
        loop {
            if let Some(verdict) = reader.feed(&body) {
                return verdict.pong();
            }
            if body.len() > PING_BODY_MAX {
                return None;
            }
            match stream.next().await {
                Some(Ok(chunk)) => body.extend_from_slice(&chunk),
                Some(Err(_)) => return None,
                None => return reader.close(&body),
            }
        }
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
