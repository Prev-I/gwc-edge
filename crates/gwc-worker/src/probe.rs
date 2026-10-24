//! Every D1 call a request makes goes through a [`Probe`]: it is timed for the
//! request's log line, a failure is logged at error level by statement name
//! and counted as a health event, and after the first failure every further
//! D1 call in the request is skipped. That is degraded mode: the reply falls
//! back to the protocol minimum, with no more D1 access.

use std::future::Future;

use gwc_core::log::d1_error_line;
use worker::{Date, Error, Result, console_error};

use crate::memo;

/// Per-request D1 bookkeeping.
#[derive(Debug, Default)]
pub struct Probe {
    /// Milliseconds spent waiting on D1.
    pub d1_ms: u64,
    failed: bool,
}

impl Probe {
    /// Whether a D1 call has failed in this request (degraded mode).
    pub fn failed(&self) -> bool {
        self.failed
    }

    /// Run a D1 write the reply does not depend on (the snapshot write-back):
    /// timed, and a failure is logged and counted as a D1 error, but it does
    /// not make the reply degraded. Skipped if an earlier call failed.
    pub async fn try_run<T, F>(&mut self, name: &str, op: impl FnOnce() -> F) -> Option<T>
    where
        F: Future<Output = Result<T>>,
    {
        if self.failed {
            return None;
        }
        let start = Date::now().as_millis();
        let result = op().await;
        self.d1_ms += Date::now().as_millis().saturating_sub(start);
        match result {
            Ok(v) => Some(v),
            Err(e) => {
                console_error!("{}", d1_error_line(name, &e.to_string()));
                memo::with_health(gwc_core::health::HealthCounters::d1_error);
                None
            }
        }
    }

    /// Run the D1 operation `name`, unless an earlier one failed.
    pub async fn run<T, F>(&mut self, name: &str, op: impl FnOnce() -> F) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        if self.failed {
            return Err(Error::RustError(format!(
                "{name}: skipped, D1 already failed"
            )));
        }
        let start = Date::now().as_millis();
        let result = op().await;
        self.d1_ms += Date::now().as_millis().saturating_sub(start);
        if let Err(e) = &result {
            self.failed = true;
            console_error!("{}", d1_error_line(name, &e.to_string()));
            memo::with_health(gwc_core::health::HealthCounters::d1_error);
        }
        result
    }
}
