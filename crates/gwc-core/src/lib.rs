//! Pure GWebCache v2 logic: query parsing, response formatting, address
//! validation, handshake parsing and expiry math.
//!
//! Nothing here depends on the Workers runtime, so all of it is tested natively.
//!
//! Shareaza is the compatibility oracle. Citations of the form
//! `DiscoveryServices.cpp:N` (or a bare `:N` inside a module that names the
//! file) refer to <https://github.com/ansani/Shareaza> at commit
//! `f24b0ce3753067e02fb6ee7dbf82d8c214c291d2`, under `shareaza/`.

pub mod addr;
pub mod cache_url;
pub mod handshake;
pub mod policy;
pub mod query;
pub mod response;

#[cfg(test)]
mod shareaza_model;

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
