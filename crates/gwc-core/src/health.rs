//! Health counters: D1 errors and degraded replies, counted in isolate memory
//! and flushed to `request_log` (kinds `d1_error`, `degraded`) at most once a
//! minute, so a quota exhaustion shows in `/stats` without reading logs.
//!
//! Counted exactly, not sampled: these are rare, and they matter most when
//! they are rare. A failed flush puts the counts back for the next one; they
//! are lost only if the isolate is evicted first.

/// Least time between two flushes from one isolate.
pub const FLUSH_INTERVAL: u64 = 60;

/// Counts not yet flushed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HealthCounters {
    d1_errors: u64,
    degraded: u64,
    last_flush: u64,
}

/// What a flush must write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flush {
    pub d1_errors: u64,
    pub degraded: u64,
}

impl HealthCounters {
    /// A D1 call failed.
    pub fn d1_error(&mut self) {
        self.d1_errors += 1;
    }

    /// A reply was served in degraded mode.
    pub fn degraded(&mut self) {
        self.degraded += 1;
    }

    /// Take the counts if there are any and a flush is due; they are then
    /// owed by the caller, who gives them back with [`HealthCounters::restore`]
    /// if writing them fails.
    pub fn take_due(&mut self, now: u64) -> Option<Flush> {
        let pending = self.d1_errors > 0 || self.degraded > 0;
        let due = self.last_flush == 0 || now.saturating_sub(self.last_flush) >= FLUSH_INTERVAL;
        if !(pending && due) {
            return None;
        }
        self.last_flush = now;
        Some(Flush {
            d1_errors: std::mem::take(&mut self.d1_errors),
            degraded: std::mem::take(&mut self.degraded),
        })
    }

    /// Give back the counts of a failed flush.
    pub fn restore(&mut self, flush: Flush) {
        self.d1_errors += flush.d1_errors;
        self.degraded += flush.degraded;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 1_792_744_200;

    #[test]
    fn nothing_flushed_when_zero() {
        let mut h = HealthCounters::default();
        assert_eq!(h.take_due(T), None);
    }

    #[test]
    fn counts_are_flushed_and_reset() {
        let mut h = HealthCounters::default();
        h.d1_error();
        h.d1_error();
        h.degraded();
        assert_eq!(
            h.take_due(T),
            Some(Flush {
                d1_errors: 2,
                degraded: 1
            })
        );
        h.degraded();
        assert_eq!(
            h.take_due(T + FLUSH_INTERVAL),
            Some(Flush {
                d1_errors: 0,
                degraded: 1
            })
        );
    }

    #[test]
    fn flushes_at_most_once_a_minute() {
        let mut h = HealthCounters::default();
        h.degraded();
        assert!(h.take_due(T).is_some());
        h.degraded();
        assert_eq!(h.take_due(T + FLUSH_INTERVAL - 1), None, "too soon");
        assert!(h.take_due(T + FLUSH_INTERVAL).is_some());
    }

    #[test]
    fn a_failed_flush_is_restored() {
        let mut h = HealthCounters::default();
        h.d1_error();
        let f = h.take_due(T).unwrap();
        h.d1_error();
        h.restore(f);
        assert_eq!(
            h.take_due(T + FLUSH_INTERVAL),
            Some(Flush {
                d1_errors: 2,
                degraded: 0
            })
        );
    }
}
