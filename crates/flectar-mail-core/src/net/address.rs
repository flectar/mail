//! Which addresses an outgoing request may reach. Remote images and
//! unsubscribe links come from mail, so they must never reach this machine
//! or a local network.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether `address` is on the public internet. An IPv6 address that
/// stands for an IPv4 one counts as that one.
pub fn public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => public_ipv4(address),
        IpAddr::V6(address) => public_ipv6(address),
    }
}

fn public_ipv4(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    !(address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_multicast()
        || address.is_unspecified()
        || address.is_broadcast()
        || address.is_documentation()
        || octets[0] == 0
        // Shared address space (RFC 6598).
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        // IETF protocol assignments (RFC 6890).
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        // Benchmarking (RFC 2544).
        || (octets[0] == 198 && matches!(octets[1], 18 | 19))
        || octets[0] >= 240)
}

fn public_ipv6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    if address.is_loopback()
        || address.is_unique_local()
        || address.is_unicast_link_local()
        || address.is_multicast()
        || address.is_unspecified()
        // Documentation (RFC 3849).
        || segments[..2] == [0x2001, 0x0db8]
        // Site-local, deprecated but still routed locally (RFC 3879).
        || segments[0] & 0xffc0 == 0xfec0
        // Discard only (RFC 6666).
        || segments[..4] == [0x0100, 0, 0, 0]
        // Teredo (RFC 4380) and the local-use NAT64 prefix (RFC 8215), which
        // can lead to any IPv4 address.
        || segments[..2] == [0x2001, 0x0000]
        || segments[..3] == [0x0064, 0xff9b, 0x0001]
    {
        return false;
    }
    // IPv4-mapped and IPv4-compatible addresses, the NAT64 well-known prefix
    // (RFC 6052) and 6to4 (RFC 3056) each carry an IPv4 address, which a
    // translator or relay then reaches.
    let carried = match segments {
        [0, 0, 0, 0, 0, 0 | 0xffff, high, low] | [0x0064, 0xff9b, 0, 0, 0, 0, high, low] => {
            Some((high, low))
        }
        [0x2002, high, low, ..] => Some((high, low)),
        _ => None,
    };
    carried.is_none_or(|(high, low)| {
        public_ipv4(Ipv4Addr::from((u32::from(high) << 16) | u32::from(low)))
    })
}

#[cfg(test)]
mod tests {
    use super::public_ip;

    #[test]
    fn local_addresses_are_never_public() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "fec0::1",
            "2001:db8::1",
            "100::1",
            "::ffff:127.0.0.1",
            "::127.0.0.1",
            "64:ff9b::127.0.0.1",
            "64:ff9b::10.0.0.1",
            "64:ff9b:1::10.0.0.1",
            "2002:7f00:0001::1",
            "2002:c0a8:0101::1",
            "2001:0000:4136:e378:8000:63bf:3fff:fdd2",
        ] {
            assert!(!public_ip(address.parse().unwrap()), "{address}");
        }
    }

    #[test]
    fn internet_addresses_are_public() {
        for address in [
            "93.184.215.14",
            "2606:4700::1111",
            "::ffff:93.184.215.14",
            // An IPv6-only network reaches an IPv4 site through NAT64.
            "64:ff9b::93.184.215.14",
            "2002:5db8:d70e::1",
        ] {
            assert!(public_ip(address.parse().unwrap()), "{address}");
        }
    }
}
