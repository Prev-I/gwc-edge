//! Per-isolate memory of the last snapshot and stats document.
//!
//! An isolate serves many requests; remembering the parsed snapshot turns
//! most gets into zero D1 reads, keeps the last good copy to fall back on,
//! and keeps concurrent requests from each going to D1 at once.

use std::cell::RefCell;
use std::rc::Rc;

use gwc_core::snapshot::Snapshot;

/// How long a copy read from the stored snapshot is served.
pub const TTL: u64 = 60;

/// How long a copy built by a live read is served: one cron period. A live
/// read scans every hub and cache, so an isolate does it at most this often
/// while the cron is stalled.
pub const LIVE_TTL: u64 = 10 * 60;

/// How long a copy is served after D1 failed, before D1 is tried again.
pub const ERROR_TTL: u64 = 60;

/// How long a refresh in progress holds off other requests in this isolate.
const LEASE: u64 = 15;

struct Entry<T> {
    fetched_at: u64,
    ttl: u64,
    value: Rc<T>,
}

thread_local! {
    static HOSTFILE: RefCell<Option<Entry<Snapshot>>> = const { RefCell::new(None) };
    static STATS: RefCell<Option<Entry<String>>> = const { RefCell::new(None) };
}

fn fresh<T>(slot: &RefCell<Option<Entry<T>>>, now: u64) -> Option<Rc<T>> {
    slot.borrow()
        .as_ref()
        .filter(|e| now.saturating_sub(e.fetched_at) < e.ttl)
        .map(|e| Rc::clone(&e.value))
}

fn last<T>(slot: &RefCell<Option<Entry<T>>>) -> Option<Rc<T>> {
    slot.borrow().as_ref().map(|e| Rc::clone(&e.value))
}

/// Mark the current copy fresh for [`LEASE`] seconds before refreshing, so
/// requests arriving meanwhile serve it instead of refreshing too.
fn lease<T>(slot: &RefCell<Option<Entry<T>>>, now: u64) {
    if let Some(e) = slot.borrow_mut().as_mut() {
        e.fetched_at = now;
        e.ttl = LEASE;
    }
}

fn put<T>(slot: &RefCell<Option<Entry<T>>>, now: u64, ttl: u64, value: Rc<T>) -> Rc<T> {
    *slot.borrow_mut() = Some(Entry {
        fetched_at: now,
        ttl,
        value: Rc::clone(&value),
    });
    value
}

pub fn hostfile(now: u64) -> Option<Rc<Snapshot>> {
    HOSTFILE.with(|s| fresh(s, now))
}

pub fn last_hostfile() -> Option<Rc<Snapshot>> {
    HOSTFILE.with(last)
}

pub fn lease_hostfile(now: u64) {
    HOSTFILE.with(|s| lease(s, now));
}

pub fn set_hostfile(now: u64, ttl: u64, snap: Rc<Snapshot>) -> Rc<Snapshot> {
    HOSTFILE.with(|s| put(s, now, ttl, snap))
}

pub fn stats(now: u64) -> Option<Rc<String>> {
    STATS.with(|s| fresh(s, now))
}

pub fn last_stats() -> Option<Rc<String>> {
    STATS.with(last)
}

pub fn lease_stats(now: u64) {
    STATS.with(|s| lease(s, now));
}

pub fn set_stats(now: u64, ttl: u64, json: Rc<String>) -> Rc<String> {
    STATS.with(|s| put(s, now, ttl, json))
}
