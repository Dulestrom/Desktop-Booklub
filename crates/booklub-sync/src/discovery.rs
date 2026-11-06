//! Finding each other on the home network with DNS-SD (REQ-22). Multicast DNS never leaves the
//! local link. The computer announces `_booklub._tcp`; a phone looks for its paired computer by
//! peer ID and falls back to the addresses stored at pairing (REQ-22a).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::network;

pub const SERVICE: &str = "_booklub._tcp.local.";
/// REQ-22a.
pub const SEARCH_TIME: Duration = Duration::from_secs(10);

/// Announces this device while it lives. The instance name is the peer ID, not the device's
/// name, so nothing personal goes out on the network.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertiser {
    pub fn start(peer_id: &str, port: u16) -> Result<Self, mdns_sd::Error> {
        let daemon = ServiceDaemon::new()?;
        let host = format!("booklub-{}.local.", peer_id.chars().filter(char::is_ascii_alphanumeric).take(12).collect::<String>());
        let properties = [("id", peer_id), ("v", "1")];
        let info = ServiceInfo::new(SERVICE, peer_id, &host, "", port, &properties[..])?.enable_addr_auto();
        let fullname = info.get_fullname().to_string();
        daemon.register(info)?;
        Ok(Self { daemon, fullname })
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// Looks for the computer with `peer_id` for up to `limit`, returning its home-network addresses
/// as soon as it is found, or none.
pub fn find(peer_id: &str, limit: Duration) -> Vec<SocketAddr> {
    let Ok(daemon) = ServiceDaemon::new() else { return Vec::new() };
    let mut found = Vec::new();
    if let Ok(events) = daemon.browse(SERVICE) {
        let deadline = Instant::now() + limit;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match events.recv_timeout(left) {
                Ok(ServiceEvent::ServiceResolved(service)) if service.get_property_val_str("id") == Some(peer_id) => {
                    let port = service.get_port();
                    found = service
                        .get_addresses()
                        .iter()
                        .map(|ip| SocketAddr::new(ip.to_ip_addr(), port))
                        .filter(|address| network::is_home_address(address.ip()))
                        .collect();
                    found.sort_by_key(|address| (address.is_ipv6(), *address));
                    if !found.is_empty() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let _ = daemon.stop_browse(SERVICE);
    }
    let _ = daemon.shutdown();
    found
}
