//! Sync with phones (Android SRS Sections 3.6 to 3.8). While sync is on and the window is open,
//! the `booklub-sync` server listens on a thread of its own. Every store call it makes is
//! carried to the GTK thread, which owns the store, so the library keeps one writer and one
//! clock, and the screen updates as changes arrive.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use adw::prelude::*;
use booklub_core::background;
use booklub_sync::discovery::Advertiser;
use booklub_sync::server::{PairingOffer, Server};
use booklub_sync::{network, ApplyReport, Delta, DeviceKey, Event, Host, PairedPeer, Vector, DEFAULT_PORT};
use gtk::glib;

use crate::app::App;

type Job = Box<dyn FnOnce(&Rc<App>) + Send>;

const CLOSED: &str = "Sync was turned off.";
/// How often a phone's last session is written down while it keeps syncing every few seconds.
const RECORD_EVERY: Duration = Duration::from_secs(60);

/// What the Devices page shows, kept here so it outlives the dialog.
#[derive(Default)]
pub struct SyncState {
    running: RefCell<Option<Running>>,
    pub offer: RefCell<Option<PairingOffer>>,
    /// The latest failure worth showing (REQ-34b), cleared by the next success.
    pub problem: RefCell<Option<String>>,
    /// When each phone last finished a session, even between writes to the store.
    pub last_seen: RefCell<BTreeMap<String, chrono::DateTime<chrono::Local>>>,
    recorded: RefCell<BTreeMap<String, Instant>>,
    listeners: RefCell<Vec<Weak<dyn Fn()>>>,
    /// Lets a test run answer the pairing confirmation.
    pub auto_confirm: Cell<Option<bool>>,
}

struct Running {
    server: Server,
    _advertiser: Option<Advertiser>,
    open: Arc<AtomicBool>,
    addresses: Vec<IpAddr>,
}

impl Drop for Running {
    fn drop(&mut self) {
        // Sessions still running finish their current call and then stop at the next.
        self.open.store(false, Ordering::SeqCst);
    }
}

impl SyncState {
    pub fn is_running(&self) -> bool {
        self.running.borrow().is_some()
    }

    #[cfg(feature = "dev-tools")]
    pub fn port(&self) -> Option<u16> {
        self.running.borrow().as_ref().map(|running| running.server.port())
    }

    pub fn addresses(&self) -> Vec<IpAddr> {
        self.running.borrow().as_ref().map(|running| running.addresses.clone()).unwrap_or_default()
    }

    pub fn pairing_open(&self) -> bool {
        self.running.borrow().as_ref().is_some_and(|running| running.server.pairing_open()) && self.offer.borrow().is_some()
    }

    /// Calls `listener` whenever sync's state changes, for as long as the caller keeps it.
    pub fn listen(&self, listener: &Rc<dyn Fn()>) {
        self.listeners.borrow_mut().push(Rc::downgrade(listener));
    }

    fn changed(&self) {
        let listeners: Vec<Rc<dyn Fn()>> = {
            let mut list = self.listeners.borrow_mut();
            list.retain(|weak| weak.strong_count() > 0);
            list.iter().filter_map(Weak::upgrade).collect()
        };
        for listener in listeners {
            listener();
        }
    }
}

/// Starts listening, if sync is on and it isn't already (REQ-22e).
pub fn start(app: &Rc<App>) -> Result<(), String> {
    if app.sync.is_running() {
        return Ok(());
    }
    let key = DeviceKey::load_or_create(&app.paths.device).map_err(|e| format!("Booklub couldn't create this computer's sync key: {e}"))?;
    let (jobs, receiver) = async_channel::unbounded::<Job>();
    let open = Arc::new(AtomicBool::new(true));
    let peer = app.store.peer_id().to_string();
    let host = Arc::new(DesktopHost {
        jobs,
        open: open.clone(),
        peer: peer.clone(),
        name: device_name(),
        backgrounds: app.paths.backgrounds.clone(),
        sync_thumbnails: app.paths.sync_thumbnails.clone(),
    });
    let server = Server::start(host, key, DEFAULT_PORT).map_err(|e| format!("Booklub couldn't listen for phones: {e}"))?;
    // Without DNS-SD, phones still reach the addresses they stored at pairing (REQ-22a).
    let advertiser = Advertiser::start(&peer, server.port()).ok();
    let weak = Rc::downgrade(app);
    glib::spawn_future_local(async move {
        while let Ok(job) = receiver.recv().await {
            match weak.upgrade() {
                Some(app) => job(&app),
                None => break,
            }
        }
    });
    app.sync.running.replace(Some(Running { server, _advertiser: advertiser, open, addresses: network::local_addresses() }));
    app.sync.problem.replace(None);
    app.sync.changed();
    Ok(())
}

pub fn stop(app: &App) {
    app.sync.offer.replace(None);
    app.sync.running.replace(None);
    app.sync.changed();
}

/// REQ-22e: the Devices switch.
pub fn set_enabled(app: &Rc<App>, enabled: bool) {
    let mut preferences = app.preferences.get();
    preferences.sync_enabled = enabled;
    app.preferences.set(preferences);
    app.save_preferences();
    if enabled {
        if let Err(problem) = start(app) {
            app.sync.problem.replace(Some(problem));
            app.sync.changed();
        }
    } else {
        stop(app);
    }
}

/// Opens pairing with a fresh code (REQ-20, REQ-20f), replacing any earlier one.
pub fn open_pairing(app: &App) -> Option<PairingOffer> {
    let offer = {
        let running = app.sync.running.borrow();
        let running = running.as_ref()?;
        running.server.open_pairing(running.addresses.clone())
    };
    app.sync.offer.replace(Some(offer.clone()));
    app.sync.problem.replace(None);
    app.sync.changed();
    Some(offer)
}

pub fn close_pairing(app: &App) {
    if let Some(running) = app.sync.running.borrow().as_ref() {
        running.server.close_pairing();
    }
    app.sync.offer.replace(None);
    app.sync.changed();
}

/// REQ-21, REQ-21a.
pub fn remove_phone(app: &App, id: &str) -> bool {
    let removed = app.report(app.store.remove_peer(id));
    app.sync.last_seen.borrow_mut().remove(id);
    app.sync.changed();
    removed
}

/// REQ-34a: whether this computer holds anything the phone hasn't acknowledged.
pub fn pending_for(app: &App, phone: &PairedPeer) -> bool {
    let Ok(ours) = app.store.sync_vector() else { return false };
    ours.iter().any(|(origin, stamp)| phone.acked.get(origin).is_none_or(|seen| seen < stamp))
}

fn device_name() -> String {
    let name = glib::host_name().to_string();
    if name.is_empty() { "This computer".to_string() } else { name }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Book,
    Category,
}

/// Every background file the library uses, and which shape its sync thumbnail takes (BR-08).
fn referenced(app: &App) -> BTreeMap<String, Kind> {
    let library = app.library.borrow();
    let mut out: BTreeMap<String, Kind> = library.categories.iter().filter_map(|c| c.background.as_ref()?.file_name().map(|f| (f.to_string(), Kind::Category))).collect();
    for book in &library.books {
        if let Some(file) = &book.background_file {
            out.insert(file.clone(), Kind::Book);
        }
    }
    out
}

fn sync_thumbnail_path(sync_thumbnails: &Path, name: &str, kind: Kind) -> PathBuf {
    match kind {
        Kind::Book => background::thumbnail_path(sync_thumbnails, name),
        Kind::Category => background::wide_thumbnail_path(sync_thumbnails, name),
    }
}

fn remove_sync_thumbnails(sync_thumbnails: &Path, name: &str) {
    let _ = fs::remove_file(background::thumbnail_path(sync_thumbnails, name));
    let _ = fs::remove_file(background::wide_thumbnail_path(sync_thumbnails, name));
}

/// Redraws whatever a sync changed, keeping the Reader's scroll position.
fn after_change(app: &Rc<App>) {
    let adjustment = app.ui().grid_scroller.vadjustment();
    let scroll = adjustment.value();
    crate::window::refresh(app);
    adjustment.set_value(scroll);
    app.ui().inspector.after_sync();
}

struct DesktopHost {
    jobs: async_channel::Sender<Job>,
    open: Arc<AtomicBool>,
    peer: String,
    name: String,
    backgrounds: PathBuf,
    sync_thumbnails: PathBuf,
}

impl DesktopHost {
    /// Runs `work` on the GTK thread and waits for its answer.
    fn on_main<T: Send + 'static>(&self, work: impl FnOnce(&Rc<App>) -> T + Send + 'static) -> Result<T, String> {
        if !self.open.load(Ordering::SeqCst) {
            return Err(CLOSED.into());
        }
        let (reply, answer) = mpsc::channel();
        self.jobs
            .send_blocking(Box::new(move |app: &Rc<App>| {
                let _ = reply.send(work(app));
            }))
            .map_err(|_| CLOSED.to_string())?;
        answer.recv().map_err(|_| CLOSED.to_string())
    }

    /// Hands an event to the GTK thread without waiting.
    fn post(&self, work: impl FnOnce(&Rc<App>) + Send + 'static) {
        let _ = self.jobs.send_blocking(Box::new(work));
    }

    fn kind_of(&self, name: &str) -> Kind {
        let name = name.to_string();
        self.on_main(move |app| referenced(app).get(&name).copied()).ok().flatten().unwrap_or(Kind::Book)
    }
}

impl Host for DesktopHost {
    fn peer_id(&self) -> String {
        self.peer.clone()
    }

    fn device_name(&self) -> String {
        self.name.clone()
    }

    fn peers(&self) -> Result<Vec<PairedPeer>, String> {
        self.on_main(|app| app.store.peers().map_err(|e| e.message))?
    }

    fn save_peer(&self, peer: &PairedPeer) -> Result<(), String> {
        let peer = peer.clone();
        self.on_main(move |app| app.store.save_peer(&peer).map_err(|e| e.message))?
    }

    fn vector(&self) -> Result<Vector, String> {
        self.on_main(|app| app.store.sync_vector().map_err(|e| e.message))?
    }

    fn delta_since(&self, theirs: &Vector) -> Result<Delta, String> {
        let theirs = theirs.clone();
        self.on_main(move |app| app.store.delta_since(&theirs).map_err(|e| e.message))?
    }

    fn apply(&self, delta: &Delta) -> Result<ApplyReport, String> {
        let delta = delta.clone();
        let sync_thumbnails = self.sync_thumbnails.clone();
        self.on_main(move |app| {
            let before = referenced(app);
            let report = app.store.apply_delta(&delta).map_err(|e| e.message)?;
            if report.units_applied + report.deleted > 0 {
                app.reload();
                // REQ-33d: an image the merged library no longer uses goes, with its thumbnails.
                let after = referenced(app);
                for gone in before.keys().filter(|name| !after.contains_key(*name)) {
                    app.thumbs.forget(gone);
                    background::remove(gone, &app.paths.backgrounds, &app.paths.thumbnails);
                    remove_sync_thumbnails(&sync_thumbnails, gone);
                }
                after_change(app);
            }
            Ok(report)
        })?
    }

    /// A computer is never reset by pairing; only phones take the other side's library.
    fn reset_for_pairing(&self) -> Result<(), String> {
        Err("A computer keeps its library when pairing.".into())
    }

    fn missing_images(&self) -> Vec<String> {
        let names = self.on_main(|app| referenced(app).into_keys().collect::<Vec<_>>()).unwrap_or_default();
        names.into_iter().filter(|name| !self.backgrounds.join(name).exists()).collect()
    }

    /// REQ-33e: made once, on this thread, then sent unchanged every time.
    fn read_image(&self, name: &str) -> Option<Vec<u8>> {
        let kind = self.kind_of(name);
        let path = sync_thumbnail_path(&self.sync_thumbnails, name, kind);
        if let Ok(bytes) = fs::read(&path) {
            return Some(bytes);
        }
        let made = match kind {
            Kind::Book => background::ensure_thumbnail(name, &self.backgrounds, &self.sync_thumbnails),
            Kind::Category => background::ensure_wide_thumbnail(name, &self.backgrounds, &self.sync_thumbnails),
        };
        made.ok().and_then(|path| fs::read(path).ok())
    }

    /// REQ-33f: a phone's thumbnail becomes this computer's background, and also its sync
    /// thumbnail, so other phones receive exactly the same bytes.
    fn store_image(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        if background::check(bytes.to_vec()).is_err() {
            // Not an image Booklub accepts; it stays missing rather than stored.
            return Ok(());
        }
        let kind = self.kind_of(name);
        let write = |path: &Path| -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let temporary = path.with_extension("part");
            fs::write(&temporary, bytes)?;
            fs::rename(&temporary, path)
        };
        write(&self.backgrounds.join(name)).map_err(|e| e.to_string())?;
        write(&sync_thumbnail_path(&self.sync_thumbnails, name, kind)).map_err(|e| e.to_string())?;
        let name = name.to_string();
        self.post(move |app| app.thumbs.forget(&name));
        Ok(())
    }

    fn session_finished(&self, peer: &str, acked: &Vector) -> Result<(), String> {
        let (peer, acked) = (peer.to_string(), acked.clone());
        self.on_main(move |app| {
            let now = chrono::Local::now();
            app.sync.last_seen.borrow_mut().insert(peer.clone(), now);
            let Some(mut record) = app.store.peers().map_err(|e| e.message)?.into_iter().find(|p| p.id == peer) else { return Ok(()) };
            let due = app.sync.recorded.borrow().get(&peer).is_none_or(|at| at.elapsed() >= RECORD_EVERY);
            if record.acked != acked || due {
                record.acked = acked;
                record.last_session = Some(now.to_rfc3339());
                app.store.save_peer(&record).map_err(|e| e.message)?;
                app.sync.recorded.borrow_mut().insert(peer, Instant::now());
                // REQ-32e.
                app.store.collect_tombstones().map_err(|e| e.message)?;
            }
            Ok(())
        })?
    }

    /// REQ-20d: asks the Reader, and waits for the answer.
    fn confirm_pairing(&self, other_name: &str) -> bool {
        let name = other_name.to_string();
        let (reply, answer) = mpsc::channel::<bool>();
        self.post(move |app| ask_to_pair(app, &name, reply));
        answer.recv_timeout(Duration::from_secs(170)).unwrap_or(false)
    }

    fn notify(&self, event: Event) {
        self.post(move |app| handle(app, event));
    }
}

fn handle(app: &Rc<App>, event: Event) {
    match event {
        Event::Paired { .. } => {
            app.sync.offer.replace(None);
            app.sync.problem.replace(None);
        }
        Event::SessionDone { peer, summary } => {
            app.sync.problem.replace(None);
            app.sync.last_seen.borrow_mut().entry(peer).or_insert_with(chrono::Local::now);
            if summary.images_received > 0 {
                after_change(app);
            }
        }
        Event::SessionFailed { peer, reason } => {
            let name = peer.and_then(|id| app.store.peers().ok()?.into_iter().find(|p| p.id == id).map(|p| p.name));
            app.sync.problem.replace(Some(match name {
                Some(name) => format!("The last sync with {name} didn't finish: {reason}"),
                None => reason,
            }));
        }
        Event::PairingClosed => {
            app.sync.offer.replace(None);
            app.sync.problem.replace(Some("Pairing ended after 5 wrong codes. Show a new code to try again.".into()));
        }
        Event::Books { .. } | Event::Images { .. } => return,
    }
    app.sync.changed();
}

fn ask_to_pair(app: &Rc<App>, name: &str, reply: mpsc::Sender<bool>) {
    if let Some(answer) = app.sync.auto_confirm.get() {
        let _ = reply.send(answer);
        return;
    }
    let dialog = adw::AlertDialog::new(
        Some(&format!("Pair with \u{201c}{name}\u{201d}?")),
        Some("This phone will receive your whole library, and changes on either side will reach the other while Booklub is open on both."),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("pair", "Pair")]);
    dialog.set_response_appearance("pair", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("pair"));
    dialog.set_close_response("cancel");
    let reply = RefCell::new(Some(reply));
    dialog.connect_response(None, move |_, response| {
        if let Some(reply) = reply.borrow_mut().take() {
            let _ = reply.send(response == "pair");
        }
    });
    dialog.present(Some(&app.window()));
}

/// Names of the library's images, for a test run's checks.
#[cfg(feature = "dev-tools")]
pub fn dev_referenced(app: &App) -> std::collections::BTreeSet<String> {
    referenced(app).into_keys().collect()
}
