//! Rate limiting with the Workers Rate Limiting binding (ADR-0005): no D1
//! writes, approximate and per data centre, fail-open.
//!
//! Only requests that cost D1 are limited: Gnutella2 hostfile and urlfile
//! gets (keyed separately, since Shareaza sends both back to back) and
//! updates. Over the limit, a get is answered without data and an update is
//! answered `OK` without being processed: never a reply Shareaza counts as a
//! failure (ADR-0002).

use std::cell::Cell;

use gwc_core::policy::{RateKind, rate_key};
use worker::{Env, console_warn};

thread_local! {
    /// Whether this isolate has already warned about a failing binding.
    static WARNED: Cell<bool> = const { Cell::new(false) };
}

/// Gets per source and kind: 2 per 60 s (the binding's periods are 10 or 60 s).
pub const GET_LIMITER: &str = "GET_LIMITER";

/// Updates per source: 1 per 60 s. The brief's "1 per 10 minutes" is longer
/// than the binding allows; writes are coalesced to one per hub per 30 minutes
/// anyway (`gwc_core::update::COALESCE`).
pub const UPDATE_LIMITER: &str = "UPDATE_LIMITER";

/// What the limiter said about a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Allowed,
    Limited,
    /// The binding is missing or failed: the request is let through
    /// (fail-open), and its log line says so (`rate_limit: "unavailable"`).
    Unchecked,
}

/// Whether this request may proceed. Fail-open: a missing or failing binding
/// allows it. Every such request is marked in its own log line, and the
/// first one in an isolate also logs one warning naming the binding: not
/// one per request, which would double the log volume. Neither carries the
/// error text, since the key the binding was called with holds the address.
pub async fn admit(env: &Env, source: &str, kind: RateKind) -> Admission {
    let binding = match kind {
        RateKind::Update => UPDATE_LIMITER,
        RateKind::Hostfile | RateKind::Urlfile => GET_LIMITER,
    };
    let outcome = match env.rate_limiter(binding) {
        Ok(limiter) => limiter.limit(rate_key(source, kind)).await,
        Err(e) => Err(e),
    };
    if let Ok(o) = outcome {
        return if o.success {
            Admission::Allowed
        } else {
            Admission::Limited
        };
    }
    if !WARNED.with(|w| w.replace(true)) {
        console_warn!(
            "{}",
            serde_json::json!({"level": "warn", "event": "rate_limiter_unavailable", "binding": binding})
        );
    }
    Admission::Unchecked
}
