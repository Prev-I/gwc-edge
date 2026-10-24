//! The structured log lines: exactly one per request, plus one per D1 error.
//!
//! Workers Logs keeps 200,000 events a day for 7 days on the Free plan; it is
//! the debugging channel, not a record. No line carries the requester's IP.

use serde::Serialize;

/// How a request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    RateLimited,
    Denied,
    /// D1 failed; the reply is the protocol minimum, with no further D1 access.
    Degraded,
    /// The query had GWC parameters but could not be parsed.
    Malformed,
}

/// `kind` values for requests that are not GWC requests. They exist only in
/// logs, never in `request_log`.
pub mod route_kind {
    pub const PAGE: &str = "page";
    pub const STATS: &str = "stats";
    pub const HEALTHZ: &str = "healthz";
    pub const ROBOTS: &str = "robots";
    pub const METHOD_NOT_ALLOWED: &str = "method_not_allowed";
    pub const NOT_FOUND: &str = "not_found";
    pub const REDIRECT: &str = "redirect";
    pub const CONFIG_ERROR: &str = "config_error";
    pub const INTERNAL_ERROR: &str = "internal_error";
}

/// The one line logged per request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequestLine<'a> {
    /// A `request_log.kind`, or one of [`route_kind`].
    pub kind: &'a str,
    /// Vendor code, `-` if none.
    pub client: &'a str,
    /// Version, `-` if none.
    pub version: &'a str,
    /// `http` or `https`.
    pub scheme: &'a str,
    pub outcome: Outcome,
    pub hosts_sent: usize,
    pub caches_sent: usize,
    /// Milliseconds spent waiting on D1 in this request.
    pub d1_ms: u64,
    /// Cloudflare data centre code, `-` if unknown.
    pub cf_colo: &'a str,
    /// Where the hostfile came from, when a get needed one:
    /// `memo`, `stored`, `live`, `stale` or `fallback`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<&'a str>,
}

impl RequestLine<'_> {
    /// The line as one compact JSON object.
    #[must_use]
    pub fn to_json(&self) -> String {
        // Serialising plain strings and integers cannot fail.
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// A failed D1 call: the statement's name (never its text) and the error.
#[must_use]
pub fn d1_error_line(statement: &str, error: &str) -> String {
    serde_json::json!({
        "level": "error",
        "event": "d1_error",
        "statement": statement,
        "error": error,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn line() -> RequestLine<'static> {
        RequestLine {
            kind: "hostfile",
            client: "RAZA",
            version: "2.7.10.6",
            scheme: "http",
            outcome: Outcome::Ok,
            hosts_sent: 20,
            caches_sent: 0,
            d1_ms: 3,
            cf_colo: "MXP",
            snapshot: Some("stored"),
        }
    }

    fn keys(json: &str) -> BTreeSet<String> {
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        v.as_object().unwrap().keys().cloned().collect()
    }

    #[test]
    fn request_line_has_exactly_these_keys() {
        let expected: BTreeSet<String> = [
            "kind",
            "client",
            "version",
            "scheme",
            "outcome",
            "hosts_sent",
            "caches_sent",
            "d1_ms",
            "cf_colo",
            "snapshot",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(keys(&line().to_json()), expected);
        let no_snapshot = RequestLine {
            snapshot: None,
            ..line()
        };
        assert!(!keys(&no_snapshot.to_json()).contains("snapshot"));
    }

    #[test]
    fn request_line_is_one_line_of_json() {
        let json = line().to_json();
        assert!(!json.contains('\n'));
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["outcome"], "ok");
        assert_eq!(v["hosts_sent"], 20);
    }

    #[test]
    fn outcome_strings() {
        let s = |o: Outcome| serde_json::to_value(o).unwrap();
        assert_eq!(s(Outcome::Ok), "ok");
        assert_eq!(s(Outcome::RateLimited), "rate_limited");
        assert_eq!(s(Outcome::Denied), "denied");
        assert_eq!(s(Outcome::Degraded), "degraded");
        assert_eq!(s(Outcome::Malformed), "malformed");
    }

    #[test]
    fn d1_error_line_names_the_statement_at_error_level() {
        let v: serde_json::Value =
            serde_json::from_str(&d1_error_line("read_snapshot", "D1_ERROR: boom")).unwrap();
        assert_eq!(v["level"], "error");
        assert_eq!(v["event"], "d1_error");
        assert_eq!(v["statement"], "read_snapshot");
        assert_eq!(v["error"], "D1_ERROR: boom");
    }
}
