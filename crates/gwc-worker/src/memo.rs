//! Per-isolate memory: the last hostfile snapshot and the health counters.
//!
//! An isolate serves many requests; remembering the parsed snapshot turns
//! most gets into zero D1 reads, keeps the last good copy to fall back on,
//! and keeps concurrent requests from each going to D1 at once. `/stats` and
//! the landing page use the Cache API instead (see `pages`).

use std::cell::RefCell;
use std::rc::Rc;

use gwc_core::health::HealthCounters;
use gwc_core::snapshot::Snapshot;

/// How long a copy read from the stored snapshot is served.
pub const TTL: u64 = 60;

/// How long a copy built by a live read is served: one cron period.
pub const LIVE_TTL: u64 = 10 * 60;

/// How long a copy is served after D1 failed, before D1 is tried again.
pub const ERROR_TTL: u64 = 60;

/// How long a refresh in progress holds off other requests in this isolate.
const LEASE: u64 = 15;

struct Entry {
    fetched_at: u64,
    ttl: u64,
    /// Set when D1 failed while producing this copy: replies served from it
    /// are degraded.
    degraded: bool,
    value: Rc<Snapshot>,
}

thread_local! {
    static HOSTFILE: RefCell<Option<Entry>> = const { RefCell::new(None) };
    static HEALTH: RefCell<HealthCounters> = RefCell::new(HealthCounters::default());
}

/// The remembered snapshot while fresh, and whether it is a degraded copy.
pub fn hostfile(now: u64) -> Option<(Rc<Snapshot>, bool)> {
    HOSTFILE.with(|s| {
        s.borrow()
            .as_ref()
            .filter(|e| now.saturating_sub(e.fetched_at) < e.ttl)
            .map(|e| (Rc::clone(&e.value), e.degraded))
    })
}

/// The last snapshot remembered, however old.
pub fn last_hostfile() -> Option<Rc<Snapshot>> {
    HOSTFILE.with(|s| s.borrow().as_ref().map(|e| Rc::clone(&e.value)))
}

/// Mark the current copy fresh for a few seconds before refreshing, so
/// requests arriving meanwhile serve it instead of refreshing too.
pub fn lease_hostfile(now: u64) {
    HOSTFILE.with(|s| {
        if let Some(e) = s.borrow_mut().as_mut() {
            e.fetched_at = now;
            e.ttl = LEASE;
        }
    });
}

pub fn set_hostfile(now: u64, ttl: u64, degraded: bool, value: Rc<Snapshot>) -> Rc<Snapshot> {
    HOSTFILE.with(|s| {
        *s.borrow_mut() = Some(Entry {
            fetched_at: now,
            ttl,
            degraded,
            value: Rc::clone(&value),
        });
    });
    value
}

/// Run `f` on this isolate's health counters.
pub fn with_health<R>(f: impl FnOnce(&mut HealthCounters) -> R) -> R {
    HEALTH.with(|h| f(&mut h.borrow_mut()))
}
