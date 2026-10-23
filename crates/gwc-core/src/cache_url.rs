//! Validation and normalisation of GWebCache URLs (`update=1&url=...`).
//!
//! A URL is kept only if Shareaza can use it, since Shareaza is nearly every
//! client: it reads `u|` lines only when they start with `http://`
//! (`DiscoveryServices.cpp:1573`), and it rejects a cache URL shorter than 12
//! characters or without a `.` followed later by a `/` (`:297-345`).

use std::net::Ipv4Addr;

/// Longest URL accepted, after normalisation.
pub const MAX_LEN: usize = 255;

/// A normalised cache URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheUrl {
    /// `http://<host>[:port]/<path>`, scheme and host lowercased.
    pub url: String,
    /// Lowercased host without the port, for the per-domain cap.
    pub host: String,
}

/// Why a submitted cache URL is not kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlError {
    /// `https://`. Well-formed, but Shareaza 2.7 ignores it in `u|` lines, so
    /// it is not stored; the update is still answered `OK`.
    Https,
    /// Anything else that cannot be a usable cache URL.
    Invalid,
}

/// Normalise a submitted URL the way Shareaza does before comparing
/// (`:127-132`: trim, strip trailing `.` and `/`, restore one `/` if there was
/// one), then validate it.
///
/// # Errors
///
/// See [`UrlError`].
pub fn normalize(raw: &str) -> Result<CacheUrl, UrlError> {
    let trimmed = raw.trim();
    // Printable ASCII only, and nothing that would break a `u|` line, carry
    // credentials, or collide with the query Shareaza appends (`:1413`).
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_graphic() && !matches!(b, b'|' | b'?' | b'#' | b'@' | b'\\'))
    {
        return Err(UrlError::Invalid);
    }

    let lower = trimmed.to_ascii_lowercase();
    let rest = if lower.starts_with("http://") {
        &trimmed[7..]
    } else if lower.starts_with("https://") {
        return Err(UrlError::Https);
    } else {
        return Err(UrlError::Invalid);
    };

    let (authority, path) = rest.find('/').map_or((rest, ""), |i| rest.split_at(i));
    let authority = authority.to_ascii_lowercase();
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority.as_str(), None),
    };
    if !valid_host(host) {
        return Err(UrlError::Invalid);
    }
    // One spelling per port, so a cache cannot be stored twice: no leading
    // zeros, and the default port omitted.
    let port = match port {
        None => None,
        Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => {
            match p.parse::<u16>() {
                Ok(0) | Err(_) => return Err(UrlError::Invalid),
                Ok(80) => None,
                Ok(p) => Some(p),
            }
        }
        Some(_) => return Err(UrlError::Invalid),
    };
    let authority = match port {
        Some(p) => format!("{host}:{p}"),
        None => host.to_owned(),
    };

    // Shareaza's own clean-up (:127-132), applied to the path; a missing path
    // becomes `/`, without which Shareaza rejects the URL (:340-341).
    let had_slash = path.ends_with('/');
    let mut path = path.trim_end_matches(['.', '/']).to_owned();
    if had_slash || path.is_empty() {
        path.push('/');
    }

    let url = format!("http://{authority}{path}");
    if url.len() < 12 || url.len() > MAX_LEN {
        return Err(UrlError::Invalid);
    }
    Ok(CacheUrl {
        url,
        host: host.to_owned(),
    })
}

/// A DNS name with at least one dot, or a public IPv4 address.
///
/// A host is numeric when its last label is all digits, or when every label
/// is decimal or `0x`-hex: that is when `inet_addr` and WinINet read it as an
/// address. A numeric host must then be a canonical dotted quad. WinINet would
/// otherwise read `127.1`, `0177.0.0.1` or `0x7f.0.0.1` as loopback, and every
/// client we listed it to would query its own machine or LAN.
fn valid_host(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    let decimal = |l: &str| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit());
    let hex = |l: &str| {
        l.strip_prefix("0x")
            .is_some_and(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    let numeric =
        labels.last().is_some_and(|l| decimal(l)) || labels.iter().all(|l| decimal(l) || hex(l));
    if numeric {
        return host
            .parse::<Ipv4Addr>()
            .is_ok_and(crate::addr::is_public_unicast);
    }
    labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(raw: &str) -> String {
        normalize(raw)
            .unwrap_or_else(|e| panic!("{raw:?}: {e:?}"))
            .url
    }

    #[test]
    fn keeps_a_typical_cache_url() {
        let u = normalize("http://dkac.trillinux.org/dkac/dkac.php").unwrap();
        assert_eq!(u.url, "http://dkac.trillinux.org/dkac/dkac.php");
        assert_eq!(u.host, "dkac.trillinux.org");
    }

    #[test]
    fn lowercases_scheme_and_host_but_not_path() {
        let u = normalize("HTTP://GWC.Example.ORG/Skulls.php").unwrap();
        assert_eq!(u.url, "http://gwc.example.org/Skulls.php");
        assert_eq!(u.host, "gwc.example.org");
    }

    #[test]
    fn keeps_port_in_url_but_not_in_host() {
        let u = normalize("http://cache.example.net:3558/").unwrap();
        assert_eq!(u.url, "http://cache.example.net:3558/");
        assert_eq!(u.host, "cache.example.net");
    }

    #[test]
    fn trailing_dots_and_slashes_are_normalised_like_shareaza() {
        // :127-132
        assert_eq!(
            ok("http://gwc.example.org/gwc./"),
            "http://gwc.example.org/gwc/"
        );
        assert_eq!(
            ok("http://gwc.example.org/gwc.php."),
            "http://gwc.example.org/gwc.php"
        );
        assert_eq!(ok("http://gwc.example.org//"), "http://gwc.example.org/");
        assert_eq!(ok("http://gwc.example.org/."), "http://gwc.example.org/");
        assert_eq!(ok("  http://gwc.example.org/  "), "http://gwc.example.org/");
    }

    #[test]
    fn bare_host_gets_a_root_path() {
        // Shareaza would reject "http://host.com": no '/' after the dot (:340).
        assert_eq!(ok("http://gwc.example.org"), "http://gwc.example.org/");
    }

    #[test]
    fn https_is_recognised_but_not_kept() {
        assert_eq!(normalize("https://gwc.example.org/"), Err(UrlError::Https));
    }

    #[test]
    fn rejects_what_shareaza_rejects_or_cannot_use() {
        for raw in [
            "",
            "gwc.example.org/",
            "ftp://gwc.example.org/",
            "http://",
            "http://localhost/gwc", // no dot: Shareaza needs a '.' then a '/' (:336-341)
            "http://a.b/",          // 11 characters (:300)
            "http://gwc.example.org/?x=1", // Shareaza appends "?get=1", giving two '?' (:1413)
            "http://gwc.example.org/#top",
            "http://user@gwc.example.org/",
            "http://gwc.example.org/a|b", // would break the u| line
            "http://gwc.example.org/a b",
            "http://gwc.exämple.org/", // Shareaza truncates at non-ASCII (:1845-1853)
            "http://[2001:db8::1]/gwc/",
            "http://gwc.example.org:0/",
            "http://gwc.example.org:70000/",
            "http://gwc.example.org:/",
            "http://gwc..example.org/",
            "http://-gwc.example.org/",
            "http://gwc.example.org\u{7f}/",
        ] {
            assert_eq!(normalize(raw), Err(UrlError::Invalid), "{raw:?}");
        }
    }

    #[test]
    fn rejects_non_public_ip_hosts() {
        for raw in [
            "http://127.0.0.1/gwc.php",
            "http://10.0.0.1:8080/gwc.php",
            "http://192.168.1.10/gwc/",
        ] {
            assert_eq!(normalize(raw), Err(UrlError::Invalid), "{raw:?}");
        }
        assert_eq!(
            ok("http://8.8.4.4:8080/gwc.php"),
            "http://8.8.4.4:8080/gwc.php"
        );
    }

    #[test]
    fn rejects_numeric_hosts_that_are_not_canonical_ipv4() {
        // WinINet resolves short, octal and hex forms to loopback or LAN
        // addresses; Shareaza's own check lets them through (:297-345).
        for raw in [
            "http://127.1/gwc.php",
            "http://10.1/x/",
            "http://192.168.1/x/",
            "http://0177.0.0.1/x/",
            "http://0x7f.0.0.1/x/",
            "http://1.2.3.0x4/x/",
            "http://8.8.4.04/x/",
        ] {
            assert_eq!(normalize(raw), Err(UrlError::Invalid), "{raw:?}");
        }
        assert_eq!(ok("http://gwc1.example.org/"), "http://gwc1.example.org/");
        // A 0x prefix alone does not make a name numeric.
        assert_eq!(ok("http://0xacab.org/"), "http://0xacab.org/");
        assert_eq!(ok("http://0x00sec.org/gwc/"), "http://0x00sec.org/gwc/");
        assert_eq!(ok("http://1.example.org/"), "http://1.example.org/");
    }

    #[test]
    fn ports_are_canonical_and_default_port_dropped() {
        assert_eq!(ok("http://gwc.example.org:080/"), "http://gwc.example.org/");
        assert_eq!(ok("http://gwc.example.org:80/"), "http://gwc.example.org/");
        assert_eq!(
            ok("http://gwc.example.org:03558/"),
            "http://gwc.example.org:3558/"
        );
    }

    #[test]
    fn rejects_overlong_urls() {
        let long = format!("http://gwc.example.org/{}", "a".repeat(MAX_LEN));
        assert_eq!(normalize(&long), Err(UrlError::Invalid));
        let fits = format!("http://gwc.example.org/{}", "a".repeat(MAX_LEN - 23));
        assert_eq!(normalize(&fits).unwrap().url.len(), MAX_LEN);
    }

    proptest::proptest! {
        #[test]
        fn normalised_urls_pass_shareaza_validity_check(
            raw in "(?i:http)://[a-z0-9._-]{1,30}(:[0-9]{1,5})?(/[a-zA-Z0-9._/~-]{0,40})?"
        ) {
            // Mirror of CheckWebCacheValid (:297-345) and of the u| filter
            // (:1573) on our output.
            if let Ok(u) = normalize(&raw) {
                proptest::prop_assert!(u.url.len() >= 12);
                proptest::prop_assert!(u.url.starts_with("http://"));
                let rest = &u.url[7..];
                let dot = rest.find('.').expect("a dot");
                proptest::prop_assert!(rest[dot..].contains('/'), "{}", u.url);
                proptest::prop_assert_eq!(normalize(&u.url), Ok(u.clone()), "idempotent");
            }
        }
    }
}
