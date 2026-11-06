//! The phone's side: it pairs with a computer, then connects to it for each session.

use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use rustls::{ClientConnection, StreamOwned};

use crate::code::{normalize_code, Invitation, PROTOCOL};
use crate::frame::{receive, send, unexpected, Message, Method};
use crate::key::{fingerprint, DeviceKey};
use crate::pairing::{self, Binding};
use crate::server::{CONFIRM_TIMEOUT, TIMEOUT};
use crate::{network, session, Event, Host, PairedPeer, Summary, SyncError};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// How the Reader is pairing: by scanning the QR code, or by typing the address and code.
#[derive(Debug, Clone)]
pub enum Target {
    Invitation(Invitation),
    Code { address: SocketAddr, code: String },
}

type Tls = StreamOwned<ClientConnection, TcpStream>;

struct Connected {
    stream: Tls,
    address: SocketAddr,
    computer_fingerprint: String,
    computer_id: String,
    computer_name: String,
}

fn connect_one(address: SocketAddr, key: &DeviceKey, expected: Option<&str>, host: &dyn Host) -> Result<Connected, SyncError> {
    // REQ-22b: checked before any packet leaves.
    network::check(address)?;
    let tcp = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)?;
    tcp.set_read_timeout(Some(TIMEOUT))?;
    tcp.set_write_timeout(Some(TIMEOUT))?;
    let config = crate::tls::client_config(key, expected.map(String::from)).map_err(|e| SyncError::Io(e.to_string()))?;
    let connection = ClientConnection::new(Arc::clone(&config), crate::tls::server_name()).map_err(|e| SyncError::Io(e.to_string()))?;
    let mut stream = StreamOwned::new(connection, tcp);
    while stream.conn.is_handshaking() {
        stream.conn.complete_io(&mut stream.sock)?;
    }
    let computer_fingerprint = stream.conn.peer_certificates().and_then(|chain| chain.first()).map(|cert| fingerprint(cert)).ok_or(SyncError::IdentityChanged)?;
    // REQ-30e.
    send(&mut stream, &Message::Hello { protocol: PROTOCOL, peer: host.peer_id(), name: host.device_name() })?;
    match receive(&mut stream)? {
        Message::Hello { protocol, peer, name } => {
            if protocol != PROTOCOL {
                return Err(SyncError::Incompatible { ours: PROTOCOL, theirs: protocol });
            }
            Ok(Connected { stream, address, computer_fingerprint, computer_id: peer, computer_name: name })
        }
        other => Err(unexpected(&other)),
    }
}

/// Tries each address in turn. A failure that another address can't fix ends the search.
fn connect(addresses: &[SocketAddr], key: &DeviceKey, expected: Option<&str>, host: &dyn Host) -> Result<Connected, SyncError> {
    let mut failure = SyncError::Unreachable;
    for &address in addresses {
        match connect_one(address, key, expected, host) {
            Ok(connected) => return Ok(connected),
            Err(error @ (SyncError::IdentityChanged | SyncError::Incompatible { .. })) => return Err(error),
            Err(error) => failure = error,
        }
    }
    Err(failure)
}

/// Pairs with a computer, then runs the first session, in which this phone takes the
/// computer's whole library (UR-02, REQ-31).
pub fn pair(host: &dyn Host, key: &DeviceKey, target: &Target) -> Result<(PairedPeer, Summary), SyncError> {
    let (addresses, expected, method, secret) = match target {
        Target::Invitation(invitation) => (invitation.socket_addresses(), Some(invitation.fingerprint.as_str()), Method::Qr, invitation.secret.clone()),
        Target::Code { address, code } => (vec![*address], None, Method::Code, normalize_code(code).ok_or(SyncError::WrongCode { remaining: pairing::MAX_FAILURES })?),
    };
    let mut connected = connect(&addresses, key, expected, host)?;
    if let Target::Invitation(invitation) = target {
        if invitation.peer_id != connected.computer_id {
            return Err(SyncError::IdentityChanged);
        }
    }
    // REQ-20d, on the phone.
    if !host.confirm_pairing(&connected.computer_name) {
        return Err(SyncError::Declined);
    }
    let phone_id = host.peer_id();
    let binding = Binding {
        computer_fingerprint: &connected.computer_fingerprint,
        phone_fingerprint: &key.fingerprint,
        computer_id: &connected.computer_id,
        phone_id: &phone_id,
    };
    connected.stream.sock.set_read_timeout(Some(CONFIRM_TIMEOUT))?;
    pairing::as_phone(&mut connected.stream, method, &secret, &binding)?;
    connected.stream.sock.set_read_timeout(Some(TIMEOUT))?;

    // With their ports, so a later session reaches a computer that isn't on the default port.
    let mut addresses: Vec<String> = vec![connected.address.to_string()];
    if let Target::Invitation(invitation) = target {
        addresses.extend(invitation.socket_addresses().iter().map(SocketAddr::to_string));
    }
    let mut seen = std::collections::HashSet::new();
    addresses.retain(|address| seen.insert(address.clone()));
    let computer = PairedPeer {
        id: connected.computer_id.clone(),
        name: connected.computer_name.clone(),
        fingerprint: connected.computer_fingerprint.clone(),
        addresses,
        last_session: None,
        acked: Default::default(),
    };
    host.save_peer(&computer).map_err(SyncError::Storage)?;
    host.reset_for_pairing().map_err(SyncError::Storage)?;
    host.notify(Event::Paired { peer: computer.clone() });
    let summary = session::as_phone(&mut connected.stream, host, &computer.id)?;
    close(&mut connected.stream);
    host.notify(Event::SessionDone { peer: computer.id.clone(), summary: summary.clone() });
    Ok((computer, summary))
}

/// One session with the paired computer, trying `addresses` in order: those found by DNS-SD
/// first, then the ones stored at pairing (REQ-22a).
pub fn sync(host: &dyn Host, key: &DeviceKey, computer: &PairedPeer, addresses: &[SocketAddr]) -> Result<Summary, SyncError> {
    let mut connected = connect(addresses, key, Some(&computer.fingerprint), host)?;
    if connected.computer_id != computer.id {
        return Err(SyncError::IdentityChanged);
    }
    let summary = session::as_phone(&mut connected.stream, host, &computer.id)?;
    close(&mut connected.stream);
    host.notify(Event::SessionDone { peer: computer.id.clone(), summary: summary.clone() });
    Ok(summary)
}

fn close(stream: &mut Tls) {
    stream.conn.send_close_notify();
    let _ = stream.conn.complete_io(&mut stream.sock);
}
