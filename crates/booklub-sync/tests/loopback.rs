//! A computer and phones in one process, talking over loopback through the real server, TLS,
//! and pairing (plan step 5).

use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use booklub_core::model::Book;
use booklub_core::query;
use booklub_core::store::Store;
use booklub_sync::client::{self, Target};
use booklub_sync::server::Server;
use booklub_sync::{ApplyReport, Delta, DeviceKey, Event, Host, PairedPeer, SyncError, Vector};
use tempfile::TempDir;

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

struct TestHost {
    name: String,
    store: Mutex<Store>,
    images: PathBuf,
    confirm: AtomicBool,
    events: Mutex<Vec<Event>>,
    _dir: TempDir,
}

impl TestHost {
    fn new(name: &str) -> Arc<Self> {
        let dir = TempDir::new().unwrap();
        let store = Store::open(&dir.path().join("library.sqlite3"), &dir.path().join("backups")).unwrap();
        let images = dir.path().join("backgrounds");
        fs::create_dir_all(&images).unwrap();
        Arc::new(Self { name: name.into(), store: Mutex::new(store), images, confirm: AtomicBool::new(true), events: Mutex::new(Vec::new()), _dir: dir })
    }

    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap()
    }

    fn books(&self) -> Vec<Book> {
        self.store().load_all().unwrap().books
    }

    fn book(&self, id: &str) -> Option<Book> {
        self.books().into_iter().find(|book| book.id == id)
    }

    fn add(&self, title: &str) -> Book {
        let order = query::next_position(&self.books());
        let book = Book { title: title.into(), ..Book::new(order) };
        self.store().save_book(&book).unwrap();
        book
    }

    fn edit(&self, id: &str, change: impl FnOnce(&mut Book)) {
        let mut book = self.book(id).unwrap();
        change(&mut book);
        self.store().save_book(&book).unwrap();
    }

    fn tombstones(&self) -> usize {
        self.store().delta_since(&Vector::new()).unwrap().tombstones.len()
    }

    fn peers(&self) -> Vec<PairedPeer> {
        self.store().peers().unwrap()
    }

    fn referenced(&self) -> Vec<String> {
        let library = self.store().load_all().unwrap();
        let mut names: Vec<String> = library.books.iter().filter_map(|book| book.background_file.clone()).collect();
        names.extend(library.categories.iter().filter_map(|c| c.background.as_ref().and_then(|b| b.file_name()).map(String::from)));
        names.sort();
        names.dedup();
        names
    }
}

impl Host for TestHost {
    fn peer_id(&self) -> String {
        self.store().peer_id().to_string()
    }
    fn device_name(&self) -> String {
        self.name.clone()
    }
    fn peers(&self) -> Result<Vec<PairedPeer>, String> {
        self.store().peers().map_err(|e| e.to_string())
    }
    fn save_peer(&self, peer: &PairedPeer) -> Result<(), String> {
        self.store().save_peer(peer).map_err(|e| e.to_string())
    }
    fn vector(&self) -> Result<Vector, String> {
        self.store().sync_vector().map_err(|e| e.to_string())
    }
    fn delta_since(&self, theirs: &Vector) -> Result<Delta, String> {
        self.store().delta_since(theirs).map_err(|e| e.to_string())
    }
    fn apply(&self, delta: &Delta) -> Result<ApplyReport, String> {
        self.store().apply_delta(delta).map_err(|e| e.to_string())
    }
    fn reset_for_pairing(&self) -> Result<(), String> {
        self.store().reset_for_pairing().map_err(|e| e.to_string())
    }
    fn missing_images(&self) -> Vec<String> {
        self.referenced().into_iter().filter(|name| !self.images.join(name).exists()).collect()
    }
    fn read_image(&self, name: &str) -> Option<Vec<u8>> {
        fs::read(self.images.join(name)).ok()
    }
    fn store_image(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        fs::write(self.images.join(name), bytes).map_err(|e| e.to_string())
    }
    fn session_finished(&self, peer: &str, acked: &Vector) -> Result<(), String> {
        let store = self.store();
        if let Some(mut record) = store.peers().map_err(|e| e.to_string())?.into_iter().find(|p| p.id == peer) {
            record.last_session = Some("now".into());
            record.acked = acked.clone();
            store.save_peer(&record).map_err(|e| e.to_string())?;
        }
        store.collect_tombstones().map_err(|e| e.to_string())?;
        drop(store);
        let keep = self.referenced();
        for entry in fs::read_dir(&self.images).unwrap().flatten() {
            if !keep.contains(&entry.file_name().to_string_lossy().to_string()) {
                let _ = fs::remove_file(entry.path());
            }
        }
        Ok(())
    }
    fn confirm_pairing(&self, _other_name: &str) -> bool {
        self.confirm.load(Ordering::SeqCst)
    }
    fn notify(&self, event: Event) {
        self.events.lock().unwrap().push(event);
    }
}

struct Computer {
    host: Arc<TestHost>,
    server: Server,
}

impl Computer {
    fn start() -> Self {
        Self::start_with(TestHost::new("Desk"), DeviceKey::generate())
    }

    fn start_with(host: Arc<TestHost>, key: DeviceKey) -> Self {
        let server = Server::start_on(host.clone(), key, LOOPBACK, 0).unwrap();
        Self { host, server }
    }

    fn address(&self) -> SocketAddr {
        SocketAddr::new(LOOPBACK, self.server.port())
    }

    fn code(&self) -> String {
        self.server.open_pairing(vec![LOOPBACK]).code
    }
}

struct Phone {
    host: Arc<TestHost>,
    key: DeviceKey,
}

impl Phone {
    fn new(name: &str) -> Self {
        Self { host: TestHost::new(name), key: DeviceKey::generate() }
    }

    fn pair_by_code(&self, computer: &Computer, code: &str) -> Result<PairedPeer, SyncError> {
        client::pair(self.host.as_ref(), &self.key, &Target::Code { address: computer.address(), code: code.into() }).map(|(peer, _)| peer)
    }

    fn sync(&self, computer: &Computer) -> Result<booklub_sync::Summary, SyncError> {
        let record = self.host.peers().into_iter().next().expect("paired");
        client::sync(self.host.as_ref(), &self.key, &record, &[computer.address()])
    }
}

fn paired(computer: &Computer, name: &str) -> Phone {
    let phone = Phone::new(name);
    phone.pair_by_code(computer, &computer.code()).unwrap();
    phone
}

#[test]
fn pairing_by_typed_code_delivers_the_whole_library_with_images() {
    let computer = Computer::start();
    let mut book = computer.host.add("Ѓердан");
    book.background_file = Some("cover-1.jpg".into());
    computer.host.store().save_book(&book).unwrap();
    fs::write(computer.host.images.join("cover-1.jpg"), b"jpeg bytes").unwrap();
    // The phone's own starter book disappears at pairing (UR-02).
    let phone = Phone::new("Galaxy");
    phone.host.add("Phone-only book");

    let peer = phone.pair_by_code(&computer, &computer.code()).unwrap();
    assert_eq!(peer.name, "Desk");
    // Stored with the port, which a later session needs (REQ-22a).
    assert_eq!(peer.addresses, [computer.address().to_string()]);
    assert_eq!(peer.fingerprint, computer.server.fingerprint());
    assert_eq!(phone.host.store().load_all().unwrap(), computer.host.store().load_all().unwrap());
    assert_eq!(fs::read(phone.host.images.join("cover-1.jpg")).unwrap(), b"jpeg bytes");
    // Each side stored the other (REQ-20e).
    let phones = computer.host.peers();
    assert_eq!(phones.len(), 1);
    assert_eq!(phones[0].name, "Galaxy");
    assert_eq!(phones[0].fingerprint, phone.key.fingerprint);
    assert!(phone.host.events.lock().unwrap().iter().any(|e| matches!(e, Event::Books { received: 1, total: 1 })));
}

#[test]
fn pairing_by_qr_code_pins_the_computer() {
    let computer = Computer::start();
    computer.host.add("Dune");
    let offer = computer.server.open_pairing(vec![LOOPBACK]);
    let phone = Phone::new("S25");
    let invitation = booklub_sync::Invitation::parse(&offer.uri).unwrap();
    let (peer, summary) = client::pair(phone.host.as_ref(), &phone.key, &Target::Invitation(invitation)).unwrap();
    assert_eq!(peer.id, computer.host.peer_id());
    assert!(summary.units_received > 0);
    assert_eq!(phone.host.books().len(), 1);
    // A QR code naming a different certificate is refused at the TLS handshake (REQ-22d).
    let offer = computer.server.open_pairing(vec![LOOPBACK]);
    let mut forged = booklub_sync::Invitation::parse(&offer.uri).unwrap();
    forged.fingerprint = "00".repeat(32);
    let other = Phone::new("A54");
    assert!(matches!(client::pair(other.host.as_ref(), &other.key, &Target::Invitation(forged)), Err(SyncError::IdentityChanged)));
}

#[test]
fn edits_flow_both_ways_and_one_phone_reaches_another_through_the_computer() {
    let computer = Computer::start();
    let book = computer.host.add("Drive");
    let (a54, s25) = (paired(&computer, "A54"), paired(&computer, "S25"));
    a54.host.edit(&book.id, |b| b.current_page = 40);
    computer.host.edit(&book.id, |b| b.title = "Drive (2nd ed.)".into());
    a54.sync(&computer).unwrap();
    let on_computer = computer.host.book(&book.id).unwrap();
    assert_eq!((on_computer.title.as_str(), on_computer.current_page), ("Drive (2nd ed.)", 40));
    assert_eq!(a54.host.book(&book.id).unwrap(), on_computer);
    // REQ-20j: the A54's page reaches the S25 at its next session.
    s25.sync(&computer).unwrap();
    assert_eq!(s25.host.book(&book.id).unwrap(), on_computer);
    // A book added on a phone arrives too, with its image.
    let mut added = s25.host.add("From the phone");
    added.background_file = Some("phone-photo.jpg".into());
    s25.host.store().save_book(&added).unwrap();
    fs::write(s25.host.images.join("phone-photo.jpg"), b"thumb").unwrap();
    s25.sync(&computer).unwrap();
    a54.sync(&computer).unwrap();
    assert_eq!(a54.host.book(&added.id).unwrap().title, "From the phone");
    assert_eq!(fs::read(computer.host.images.join("phone-photo.jpg")).unwrap(), b"thumb");
    assert_eq!(fs::read(a54.host.images.join("phone-photo.jpg")).unwrap(), b"thumb");
}

#[test]
fn a_deletion_beats_an_edit_and_its_tombstone_goes_once_every_phone_has_it() {
    let computer = Computer::start();
    let book = computer.host.add("Doomed");
    let (a54, s25) = (paired(&computer, "A54"), paired(&computer, "S25"));
    s25.host.edit(&book.id, |b| b.notes = "edited later".into());
    let doomed = computer.host.book(&book.id).unwrap();
    computer.host.store().delete_book(&doomed).unwrap();
    assert_eq!(computer.host.tombstones(), 1);
    s25.sync(&computer).unwrap();
    assert!(s25.host.book(&book.id).is_none() && computer.host.book(&book.id).is_none());
    // The A54 hasn't seen the deletion, so the computer keeps the tombstone (REQ-32e).
    assert_eq!(computer.host.tombstones(), 1);
    a54.sync(&computer).unwrap();
    assert!(a54.host.book(&book.id).is_none());
    assert_eq!(computer.host.tombstones(), 0);
}

#[test]
fn a_wrong_code_counts_down_and_five_misses_close_pairing() {
    let computer = Computer::start();
    let code = computer.code();
    let wrong = if code.starts_with('0') { "1".to_string() + &code[1..] } else { "0".to_string() + &code[1..] };
    let phone = Phone::new("Guesser");
    for remaining in (0..5).rev() {
        match phone.pair_by_code(&computer, &wrong) {
            Err(SyncError::WrongCode { remaining: left }) => assert_eq!(left, remaining),
            other => panic!("expected a wrong-code refusal, got {other:?}"),
        }
    }
    assert!(!computer.server.pairing_open());
    // REQ-20g: even the right code no longer works.
    assert!(matches!(phone.pair_by_code(&computer, &code), Err(SyncError::PairingClosed)));
    assert!(computer.host.peers().is_empty());
}

#[test]
fn a_code_works_once_and_expires() {
    let computer = Computer::start();
    let code = computer.code();
    Phone::new("First").pair_by_code(&computer, &code).unwrap();
    // REQ-20c.
    assert!(matches!(Phone::new("Second").pair_by_code(&computer, &code), Err(SyncError::PairingClosed)));
    // REQ-20b, with a short lifetime standing in for 5 minutes.
    let short = computer.server.open_pairing_for(vec![LOOPBACK], Duration::from_millis(50)).code;
    std::thread::sleep(Duration::from_millis(80));
    assert!(matches!(Phone::new("Late").pair_by_code(&computer, &short), Err(SyncError::PairingClosed)));
    assert_eq!(computer.host.peers().len(), 1);
}

#[test]
fn either_side_can_decline_the_pairing() {
    let computer = Computer::start();
    computer.host.confirm.store(false, Ordering::SeqCst);
    let phone = Phone::new("Unwanted");
    assert!(matches!(phone.pair_by_code(&computer, &computer.code()), Err(SyncError::Declined)));
    assert!(computer.host.peers().is_empty() && phone.host.peers().is_empty());
    computer.host.confirm.store(true, Ordering::SeqCst);
    phone.host.confirm.store(false, Ordering::SeqCst);
    assert!(matches!(phone.pair_by_code(&computer, &computer.code()), Err(SyncError::Declined)));
    assert!(computer.host.peers().is_empty() && phone.host.peers().is_empty());
}

#[test]
fn an_unpaired_key_is_refused() {
    // QA-12: a freshly generated key, presenting a paired phone's record, gets nothing.
    let computer = Computer::start();
    computer.host.add("Private");
    let phone = paired(&computer, "A54");
    let stranger = Phone::new("Stranger");
    let record = phone.host.peers()[0].clone();
    stranger.host.save_peer(&record).unwrap();
    assert!(matches!(stranger.sync(&computer), Err(SyncError::NotPaired)));
    // The same phone ID with a different key is an identity change (REQ-22d).
    let impostor = Phone { host: phone.host.clone(), key: DeviceKey::generate() };
    assert!(matches!(impostor.sync(&computer), Err(SyncError::IdentityChanged)));
    assert!(stranger.host.books().iter().all(|book| book.title != "Private"));
}

#[test]
fn a_removed_phone_is_refused() {
    let computer = Computer::start();
    let phone = paired(&computer, "A54");
    phone.sync(&computer).unwrap();
    let id = computer.host.peers()[0].id.clone();
    computer.host.store().remove_peer(&id).unwrap();
    // REQ-21b.
    assert!(matches!(phone.sync(&computer), Err(SyncError::NotPaired)));
}

#[test]
fn a_computer_with_a_new_key_is_refused() {
    let host = TestHost::new("Desk");
    let computer = Computer::start_with(host.clone(), DeviceKey::generate());
    let phone = paired(&computer, "A54");
    drop(computer);
    // Same library, same peer ID, different certificate: REQ-22d.
    let reinstalled = Computer::start_with(host, DeviceKey::generate());
    assert!(matches!(phone.sync(&reinstalled), Err(SyncError::IdentityChanged)));
}

#[test]
fn nothing_is_sent_to_an_address_outside_the_home_network() {
    let phone = Phone::new("A54");
    let started = Instant::now();
    let public: SocketAddr = "8.8.8.8:47120".parse().unwrap();
    let result = client::pair(phone.host.as_ref(), &phone.key, &Target::Code { address: public, code: "K7Q49XMP".into() });
    assert!(matches!(result, Err(SyncError::NotHomeNetwork(_))));
    let record = PairedPeer { id: "x".into(), name: "x".into(), fingerprint: "00".repeat(32), ..Default::default() };
    assert!(matches!(client::sync(phone.host.as_ref(), &phone.key, &record, &[public]), Err(SyncError::NotHomeNetwork(_))));
    // Refused before any connection attempt, so it returns at once.
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test]
fn an_unreachable_computer_is_reported_as_such() {
    let phone = Phone::new("A54");
    let record = PairedPeer { id: "x".into(), name: "x".into(), fingerprint: "00".repeat(32), ..Default::default() };
    // Nothing listens on port 9 of loopback.
    let closed = SocketAddr::new(LOOPBACK, 9);
    assert!(matches!(client::sync(phone.host.as_ref(), &phone.key, &record, &[closed]), Err(SyncError::Unreachable)));
}
