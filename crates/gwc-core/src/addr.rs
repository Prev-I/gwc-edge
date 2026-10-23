//! Validation of hub addresses claimed in `update=1&ip=<ip>:<port>`.
//!
//! Hubs are IPv4 only: Shareaza parses `h|` lines with `inet_addr`
//! (`DiscoveryServices.cpp:1481-1482`), which has no IPv6 form.

use std::net::{IpAddr, Ipv4Addr, SocketAddrV4};

/// Lowest port accepted for a hub. Below it are privileged ports a hub has no
/// reason to use, and a cache must not be turned into a pointer at them.
pub const MIN_PORT: u16 = 1024;

/// Why a claimed hub address was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddrError {
    /// Not `<dotted-quad>:<port>`.
    Malformed,
    /// Private, loopback, link-local, CGNAT, documentation, benchmark,
    /// multicast, reserved or broadcast.
    NotPublic,
    /// Outside `MIN_PORT..=65535`.
    BadPort,
}

/// Parse and validate `<ip>:<port>` as a publicly routable hub address.
///
/// # Errors
///
/// See [`AddrError`].
pub fn parse_hub(raw: &str) -> Result<SocketAddrV4, AddrError> {
    let (ip, port) = raw.split_once(':').ok_or(AddrError::Malformed)?;
    // Digits only: `u16::from_str` would accept a leading `+`.
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AddrError::Malformed);
    }
    let port: u16 = port.parse().map_err(|_| AddrError::Malformed)?;
    // `Ipv4Addr::from_str` takes only canonical dotted quads: no leading
    // zeros (which `inet_addr` would read as octal), no short forms.
    let ip: Ipv4Addr = ip.parse().map_err(|_| AddrError::Malformed)?;
    if !is_public_unicast(ip) {
        return Err(AddrError::NotPublic);
    }
    if port < MIN_PORT {
        return Err(AddrError::BadPort);
    }
    Ok(SocketAddrV4::new(ip, port))
}

/// Whether `ip` is a public unicast address.
#[must_use]
pub fn is_public_unicast(ip: Ipv4Addr) -> bool {
    !NON_PUBLIC
        .iter()
        .any(|&(net, len)| u32::from(ip) >> (32 - len) == u32::from(net) >> (32 - len))
}

/// Every IPv4 range that is not public unicast, as `(network, prefix length)`.
/// From the IANA special-purpose registry, plus multicast and reserved space.
const NON_PUBLIC: [(Ipv4Addr, u32); 15] = [
    (Ipv4Addr::UNSPECIFIED, 8),           // "this network"
    (Ipv4Addr::new(10, 0, 0, 0), 8),      // RFC 1918
    (Ipv4Addr::new(100, 64, 0, 0), 10),   // RFC 6598 shared address space (CGNAT)
    (Ipv4Addr::new(127, 0, 0, 0), 8),     // loopback
    (Ipv4Addr::new(169, 254, 0, 0), 16),  // link-local
    (Ipv4Addr::new(172, 16, 0, 0), 12),   // RFC 1918
    (Ipv4Addr::new(192, 0, 0, 0), 24),    // IETF protocol assignments
    (Ipv4Addr::new(192, 0, 2, 0), 24),    // TEST-NET-1
    (Ipv4Addr::new(192, 88, 99, 0), 24),  // deprecated 6to4 relay anycast
    (Ipv4Addr::new(192, 168, 0, 0), 16),  // RFC 1918
    (Ipv4Addr::new(198, 18, 0, 0), 15),   // benchmarking
    (Ipv4Addr::new(198, 51, 100, 0), 24), // TEST-NET-2
    (Ipv4Addr::new(203, 0, 113, 0), 24),  // TEST-NET-3
    (Ipv4Addr::new(224, 0, 0, 0), 4),     // multicast
    (Ipv4Addr::new(240, 0, 0, 0), 4),     // reserved, and broadcast
];

/// Whether the address a request came from (`CF-Connecting-IP`) is the one
/// the hub claims. Any parse failure, or an IPv6 source, is "no".
#[must_use]
pub fn source_matches(source: &str, claimed: Ipv4Addr) -> bool {
    match source.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4 == claimed,
        Ok(IpAddr::V6(v6)) => v6.to_ipv4_mapped() == Some(claimed),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_public_address_and_port() {
        assert_eq!(
            parse_hub("8.8.4.4:6346"),
            Ok(SocketAddrV4::new(Ipv4Addr::new(8, 8, 4, 4), 6346))
        );
        assert_eq!(parse_hub("1.1.1.1:65535").map(|a| a.port()), Ok(65535));
        assert_eq!(parse_hub("1.1.1.1:1024").map(|a| a.port()), Ok(1024));
    }

    #[test]
    fn rejects_ports_outside_1024_to_65535() {
        assert_eq!(parse_hub("8.8.4.4:1023"), Err(AddrError::BadPort));
        assert_eq!(parse_hub("8.8.4.4:0"), Err(AddrError::BadPort));
        assert_eq!(parse_hub("8.8.4.4:65536"), Err(AddrError::Malformed));
    }

    #[test]
    fn rejects_malformed_addresses() {
        for raw in [
            "",
            "8.8.4.4",
            "8.8.4.4:",
            ":6346",
            "8.8.4:6346",
            "8.8.4.4.4:6346",
            "host.example:6346",
            "[2001:db8::1]:6346",
            "8.8.4.4:6346:1",
            " 8.8.4.4:6346",
            "8.8.4.4:+6346",
            "8.8.4.4:6346 ",
        ] {
            assert_eq!(parse_hub(raw), Err(AddrError::Malformed), "{raw:?}");
        }
    }

    #[test]
    fn rejects_leading_zero_octets() {
        // inet_addr reads a leading 0 as octal ("010" is 8), so such an
        // address would mean something else to Shareaza than to us.
        assert_eq!(parse_hub("010.8.4.4:6346"), Err(AddrError::Malformed));
    }

    #[test]
    fn rejects_every_non_public_range() {
        for ip in [
            "0.1.2.3",         // 0.0.0.0/8 "this network"
            "10.0.0.1",        // RFC 1918
            "100.64.0.1",      // RFC 6598 CGNAT, low end
            "100.127.255.254", // CGNAT, high end
            "127.0.0.1",       // loopback
            "169.254.1.1",     // link-local
            "172.16.0.1",      // RFC 1918
            "172.31.255.254",
            "192.0.0.8",   // IETF protocol assignments
            "192.0.2.1",   // TEST-NET-1
            "192.88.99.1", // deprecated 6to4 relay anycast
            "192.168.1.1", // RFC 1918
            "198.18.0.1",  // benchmarking
            "198.19.255.254",
            "198.51.100.1", // TEST-NET-2
            "203.0.113.1",  // TEST-NET-3
            "224.0.0.1",    // multicast
            "239.255.255.250",
            "240.0.0.1",       // reserved
            "255.255.255.255", // broadcast
        ] {
            let ip: Ipv4Addr = ip.parse().unwrap();
            assert!(!is_public_unicast(ip), "{ip} must not be public");
            assert_eq!(parse_hub(&format!("{ip}:6346")), Err(AddrError::NotPublic));
        }
    }

    #[test]
    fn range_edges_just_outside_are_public() {
        for ip in [
            "1.0.0.1",
            "9.255.255.255",
            "11.0.0.0",
            "100.63.255.255",
            "100.128.0.0",
            "172.15.255.255",
            "172.32.0.0",
            "192.167.255.255",
            "192.169.0.0",
            "198.17.255.255",
            "198.20.0.0",
            "223.255.255.255",
        ] {
            let ip: Ipv4Addr = ip.parse().unwrap();
            assert!(is_public_unicast(ip), "{ip} must be public");
        }
    }

    #[test]
    fn source_must_equal_claimed_ip() {
        let claimed = Ipv4Addr::new(8, 8, 4, 4);
        assert!(source_matches("8.8.4.4", claimed));
        assert!(!source_matches("8.8.8.8", claimed));
        assert!(!source_matches("", claimed));
        assert!(!source_matches("2001:4860:4860::8844", claimed));
        assert!(
            source_matches("::ffff:8.8.4.4", claimed),
            "an IPv4-mapped IPv6 source is the same host"
        );
    }
}
