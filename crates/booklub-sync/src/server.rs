//! The computer's side: it listens while Booklub is open and sync is on (REQ-22e), and phones
//! connect to it (plan P1).

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rustls::{ServerConfig, ServerConnection, StreamOwned};

use crate::code::{display_code, new_code, new_secret, Invitation, PROTOCOL};
use crate::frame::{receive, send, unexpected, Message, Method, Refusal};
use crate::key::{fingerprint, DeviceKey};
use crate::pairing::{self, Binding, MAX_FAILURES};
use crate::{network, session, Event, Host, PairedPeer, SyncError};

/// REQ-20b.
pub const PAIRING_LIFETIME: Duration = Duration::from_secs(5 * 60);
pub(crate) const TIMEOUT: Duration = Duration::from_secs(30);
/// Long enough for the Reader to read and answer the confirmation (REQ-20d).
pub(crate) const CONFIRM_TIMEOUT: Duration = Duration::from_secs(180);
const PORTS_TRIED: u16 = 20;

/// What the Devices page shows while pairing is open.
#[derive(Debug, Clone)]
pub struct PairingOffer {
    pub invitation: Invitation,
    /// The QR code's content.
    pub uri: String,
    /// The typed code, as "K7Q4 9XMP".
    pub code: String,
    pub expires: Instant,
}

struct Window {
    generation: u64,
    code: String,
    secret: String,
    expires: Instant,
    failures: u8,
}

struct Shared {
    host: Arc<dyn Host>,
    key: DeviceKey,
    config: Arc<ServerConfig>,
    window: Mutex<Option<Window>>,
    generation: AtomicU64,
    /// One session at a time, so merges never interleave.
    session: Mutex<()>,
    stop: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct Server {
    shared: Arc<Shared>,
    port: u16,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Listens on every interface, on `preferred` or the next free port (plan P8).
    pub fn start(host: Arc<dyn Host>, key: DeviceKey, preferred: u16) -> io::Result<Self> {
        Self::start_on(host, key, IpAddr::V4(Ipv4Addr::UNSPECIFIED), preferred)
    }

    /// As [`Server::start`], on one address only, as tests on loopback need.
    pub fn start_on(host: Arc<dyn Host>, key: DeviceKey, address: IpAddr, preferred: u16) -> io::Result<Self> {
        let listener = bind(address, preferred)?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let config = crate::tls::server_config(&key).map_err(io::Error::other)?;
        let shared = Arc::new(Shared {
            host,
            key,
            config,
            window: Mutex::new(None),
            generation: AtomicU64::new(0),
            session: Mutex::new(()),
            stop: AtomicBool::new(false),
        });
        let thread = {
            let shared = shared.clone();
            std::thread::Builder::new().name("booklub-sync".into()).spawn(move || accept_loop(&shared, &listener))?
        };
        Ok(Self { shared, port, thread: Some(thread) })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn fingerprint(&self) -> &str {
        &self.shared.key.fingerprint
    }

    /// Opens pairing with a fresh QR secret and typed code, replacing any earlier ones.
    pub fn open_pairing(&self, addresses: Vec<IpAddr>) -> PairingOffer {
        self.open_pairing_for(addresses, PAIRING_LIFETIME)
    }

    pub fn open_pairing_for(&self, addresses: Vec<IpAddr>, lifetime: Duration) -> PairingOffer {
        let (code, secret) = (new_code(), new_secret());
        let expires = Instant::now() + lifetime;
        let generation = self.shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *lock(&self.shared.window) = Some(Window { generation, code: code.clone(), secret: secret.clone(), expires, failures: 0 });
        let invitation = Invitation {
            peer_id: self.shared.host.peer_id(),
            name: self.shared.host.device_name(),
            addresses,
            port: self.port,
            fingerprint: self.shared.key.fingerprint.clone(),
            secret,
        };
        PairingOffer { uri: invitation.to_uri(), invitation, code: display_code(&code), expires }
    }

    pub fn close_pairing(&self) {
        *lock(&self.shared.window) = None;
    }

    /// Whether a code is still usable: not expired, used, or tried too often.
    pub fn pairing_open(&self) -> bool {
        lock(&self.shared.window).as_ref().is_some_and(|window| window.expires > Instant::now())
    }

    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.halt();
    }
}

fn bind(address: IpAddr, preferred: u16) -> io::Result<TcpListener> {
    let mut last = None;
    for port in preferred..preferred.saturating_add(PORTS_TRIED) {
        match TcpListener::bind(SocketAddr::new(address, port)) {
            Ok(listener) => return Ok(listener),
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("no free port")))
}

fn accept_loop(shared: &Arc<Shared>, listener: &TcpListener) {
    while !shared.stop.load(Ordering::SeqCst) {
        match listener.accept() {
            // REQ-22c: anything from outside the home network is closed before a byte is read.
            Ok((stream, address)) if network::is_home_address(address.ip()) => {
                let shared = shared.clone();
                let _ = std::thread::Builder::new().name("booklub-sync-peer".into()).spawn(move || {
                    if let Err(error) = serve(&shared, stream, address) {
                        shared.host.notify(Event::SessionFailed { peer: None, reason: error.to_string() });
                    }
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

type Tls = StreamOwned<ServerConnection, TcpStream>;

fn refuse(stream: &mut Tls, refusal: Refusal) -> SyncError {
    let error = SyncError::from(refusal.clone());
    let _ = send(stream, &Message::Refused { refusal });
    error
}

fn serve(shared: &Shared, tcp: TcpStream, address: SocketAddr) -> Result<(), SyncError> {
    tcp.set_nonblocking(false)?;
    tcp.set_read_timeout(Some(TIMEOUT))?;
    tcp.set_write_timeout(Some(TIMEOUT))?;
    let connection = ServerConnection::new(shared.config.clone()).map_err(|e| SyncError::Io(e.to_string()))?;
    let mut stream = StreamOwned::new(connection, tcp);
    while stream.conn.is_handshaking() {
        stream.conn.complete_io(&mut stream.sock)?;
    }
    let phone_fingerprint = stream.conn.peer_certificates().and_then(|chain| chain.first()).map(|cert| fingerprint(cert)).ok_or(SyncError::NotPaired)?;
    let host = shared.host.as_ref();

    let (phone, phone_name) = match receive(&mut stream)? {
        Message::Hello { protocol, peer, name } => {
            send(&mut stream, &Message::Hello { protocol: PROTOCOL, peer: host.peer_id(), name: host.device_name() })?;
            if protocol != PROTOCOL {
                return Err(refuse(&mut stream, Refusal::Incompatible { protocol: PROTOCOL }));
            }
            (peer, name)
        }
        other => return Err(unexpected(&other)),
    };

    let mut next = receive(&mut stream)?;
    if let Message::Pair { method, spake } = &next {
        let binding = Binding { computer_fingerprint: &shared.key.fingerprint, phone_fingerprint: &phone_fingerprint, computer_id: &host.peer_id(), phone_id: &phone };
        pair(shared, &mut stream, *method, spake, &binding, &phone_name, address)?;
        next = receive(&mut stream)?;
    }
    let Message::Sync { vector } = next else { return Err(unexpected(&next)) };

    // QA-12, REQ-21b, REQ-22d.
    let peers = host.peers().map_err(SyncError::Storage)?;
    match peers.iter().find(|peer| peer.id == phone) {
        None => return Err(refuse(&mut stream, Refusal::NotPaired)),
        Some(peer) if peer.fingerprint != phone_fingerprint => return Err(refuse(&mut stream, Refusal::IdentityChanged)),
        Some(_) => {}
    }
    let _one_at_a_time = lock(&shared.session);
    match session::as_computer(&mut stream, host, &phone, &vector) {
        Ok(summary) => {
            host.notify(Event::SessionDone { peer: phone, summary });
            stream.conn.send_close_notify();
            let _ = stream.conn.complete_io(&mut stream.sock);
            Ok(())
        }
        Err(error) => {
            host.notify(Event::SessionFailed { peer: Some(phone), reason: error.to_string() });
            Ok(())
        }
    }
}

fn pair(shared: &Shared, stream: &mut Tls, method: Method, spake: &str, binding: &Binding, phone_name: &str, address: SocketAddr) -> Result<(), SyncError> {
    let (secret, generation) = match lock(&shared.window).as_ref() {
        Some(window) if window.expires > Instant::now() => (if method == Method::Qr { window.secret.clone() } else { window.code.clone() }, window.generation),
        _ => return Err(refuse(stream, Refusal::PairingClosed)),
    };
    let proved = pairing::as_computer(stream, spake, &secret, binding)?;
    {
        let mut window = lock(&shared.window);
        match window.as_mut() {
            Some(open) if open.generation == generation && open.expires > Instant::now() => {
                if proved {
                    // REQ-20c: a secret works once.
                    *window = None;
                } else {
                    open.failures += 1;
                    let remaining = MAX_FAILURES.saturating_sub(open.failures);
                    if remaining == 0 {
                        // REQ-20g.
                        *window = None;
                        shared.host.notify(Event::PairingClosed);
                    }
                    drop(window);
                    return Err(refuse(stream, Refusal::WrongCode { remaining }));
                }
            }
            _ => {
                drop(window);
                return Err(refuse(stream, Refusal::PairingClosed));
            }
        }
    }
    stream.sock.set_read_timeout(Some(CONFIRM_TIMEOUT))?;
    if !shared.host.confirm_pairing(phone_name) {
        return Err(refuse(stream, Refusal::Declined));
    }
    stream.sock.set_read_timeout(Some(TIMEOUT))?;
    let peer = PairedPeer {
        id: binding.phone_id.to_string(),
        name: phone_name.to_string(),
        fingerprint: binding.phone_fingerprint.to_string(),
        addresses: vec![address.ip().to_string()],
        last_session: None,
        acked: Default::default(),
    };
    shared.host.save_peer(&peer).map_err(SyncError::Storage)?;
    send(stream, &Message::PairAccepted)?;
    shared.host.notify(Event::Paired { peer });
    Ok(())
}
