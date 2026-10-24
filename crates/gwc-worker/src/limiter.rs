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

/// Whether this request may proceed. Fail-open: a missing or failing binding
/// allows it, with one warning per isolate (not per request, which would
/// double the log volume). The warning names the binding only: the error
/// text is not logged, since the key it was called with holds the address.
pub async fn allowed(env: &Env, source: &str, kind: RateKind) -> bool {
    let binding = match kind {
        RateKind::Update => UPDATE_LIMITER,
        RateKind::Hostfile | RateKind::Urlfile => GET_LIMITER,
    };
    let outcome = match env.rate_limiter(binding) {
        Ok(limiter) => limiter.limit(rate_key(source, kind)).await,
        Err(e) => Err(e),
    };
    if let Ok(o) = outcome {
        return o.success;
    }
    if !WARNED.with(|w| w.replace(true)) {
        console_warn!(
            "{}",
            serde_json::json!({"level": "warn", "event": "rate_limiter_unavailable", "binding": binding})
        );
    }
    true
}
