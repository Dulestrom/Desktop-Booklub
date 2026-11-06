//! The home-network rule (BR-01, REQ-22b, REQ-22c): sync talks only to private, link-local,
//! and loopback addresses, never to the internet.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

pub fn is_home_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => is_home_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_home_v4(v4),
            None => is_home_v6(v6),
        },
    }
}

fn is_home_v4(address: Ipv4Addr) -> bool {
    address.is_private() || address.is_link_local() || address.is_loopback()
}

fn is_home_v6(address: Ipv6Addr) -> bool {
    let first = address.segments()[0];
    address.is_loopback()
        // fe80::/10, link-local
        || first & 0xffc0 == 0xfe80
        // fc00::/7, unique local
        || first & 0xfe00 == 0xfc00
}

fn is_v6_link_local(address: IpAddr) -> bool {
    matches!(address, IpAddr::V6(v6) if v6.segments()[0] & 0xffc0 == 0xfe80)
}

pub fn check(address: SocketAddr) -> Result<(), crate::SyncError> {
    if is_home_address(address.ip()) {
        Ok(())
    } else {
        Err(crate::SyncError::NotHomeNetwork(address.ip().to_string()))
    }
}

/// This device's home-network addresses, for the QR code (REQ-20a). Loopback is left out,
/// because a phone can never reach it, and so is IPv6 link-local, which needs an interface
/// name the phone can't know. IPv4 comes first because every home router has it.
pub fn local_addresses() -> Vec<IpAddr> {
    let mut found: Vec<IpAddr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|interface| interface.ip())
        .filter(|ip| is_home_address(*ip) && !ip.is_loopback() && !is_v6_link_local(*ip))
        .collect();
    found.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    found.dedup();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_qr_code_leaves_out_addresses_a_phone_cant_use() {
        for address in local_addresses() {
            assert!(!address.is_loopback() && !is_v6_link_local(address), "{address}");
        }
        assert!(is_v6_link_local("fe80::1".parse().unwrap()) && !is_v6_link_local("fd00::1".parse().unwrap()));
    }

    #[test]
    fn only_home_network_addresses_pass() {
        for home in ["192.168.1.20", "10.0.0.5", "172.16.4.1", "172.31.255.1", "169.254.3.3", "127.0.0.1", "::1", "fe80::1", "fd12:3456::1", "::ffff:192.168.1.2"] {
            assert!(is_home_address(home.parse().unwrap()), "{home}");
        }
        for public in ["8.8.8.8", "1.1.1.1", "172.32.0.1", "100.64.0.1", "2001:4860:4860::8888", "::ffff:8.8.8.8", "0.0.0.0"] {
            assert!(!is_home_address(public.parse().unwrap()), "{public}");
        }
    }
}
