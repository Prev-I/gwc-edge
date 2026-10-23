//! Pure GWebCache v2 logic: query parsing, response formatting, address
//! validation, handshake parsing and expiry math.
//!
//! Nothing here depends on the Workers runtime, so all of it is tested natively.

/// Product name advertised in `i|pong` and on `/healthz`.
pub const PRODUCT: &str = "gwc-edge";

/// Version advertised in `i|pong` and on `/healthz`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Product identifier, `<name> <version>`, as it appears after `i|pong|`.
#[must_use]
pub fn product_id() -> String {
    format!("{PRODUCT} {VERSION}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_id_is_name_space_version() {
        assert_eq!(
            product_id(),
            format!("gwc-edge {}", env!("CARGO_PKG_VERSION"))
        );
    }
}
