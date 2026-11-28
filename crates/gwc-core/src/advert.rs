//! Advertising this cache to verified caches (ADR-0021).
//!
//! Later tasks extend this module; for now it holds the advertising interval
//! and the check that migration 0003 matches core's bounds.

/// Seconds between two advertisements to the same host: 30 days.
pub const INTERVAL: u64 = 30 * 24 * 3600;

#[cfg(test)]
mod tests {
    #[test]
    fn migration_last_advertised_check() {
        let sql = include_str!("../../../migrations/0003_last_advertised.sql");
        assert!(sql.contains(
            "last_advertised INTEGER CHECK (last_advertised IS NULL OR last_advertised >= 0)"
        ));
    }
}
