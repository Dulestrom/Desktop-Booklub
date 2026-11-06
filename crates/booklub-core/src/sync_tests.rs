//! Sync's guarantees, tested end to end through real stores (Android SRS QA-20 to QA-22).

use tempfile::TempDir;

use crate::model::{Book, Category, Goal, GoalId, Library, Priority, Status, SubCategory};
use crate::query;
use crate::store::Store;

struct Peer {
    _dir: TempDir,
    store: Store,
}

fn peer() -> Peer {
    let dir = TempDir::new().unwrap();
    let store = Store::open(&dir.path().join("library.sqlite3"), &dir.path().join("backups")).unwrap();
    Peer { _dir: dir, store }
}

/// One session: each side sends what the other's vector shows it lacks, then both apply.
fn session(a: &Store, b: &Store) -> usize {
    let to_b = a.delta_since(&b.sync_vector().unwrap()).unwrap();
    let to_a = b.delta_since(&a.sync_vector().unwrap()).unwrap();
    let applied_b = b.apply_delta(&to_b).unwrap();
    let applied_a = a.apply_delta(&to_a).unwrap();
    applied_a.units_applied + applied_b.units_applied + applied_a.deleted + applied_b.deleted
}

/// A phone pairs: it drops its own seeded library and takes the desktop's (UR-02).
fn pair(desktop: &Store, phone: &Store) {
    phone.reset_for_pairing().unwrap();
    session(desktop, phone);
}

fn library(store: &Store) -> Library {
    store.load_all().unwrap()
}

fn save_new(store: &Store, title: &str) -> Book {
    let order = query::next_position(&library(store).books);
    let book = Book { title: title.into(), ..Book::new(order) };
    store.save_book(&book).unwrap();
    book
}

fn find(store: &Store, id: &str) -> Option<Book> {
    library(store).books.into_iter().find(|book| book.id == id)
}

#[test]
fn a_paired_phone_receives_the_whole_library() {
    let (desktop, phone) = (peer(), peer());
    let book = save_new(&desktop.store, "Ѓердан од ќерамиди");
    let mut goal = library(&desktop.store).goals[0];
    goal.target = 31;
    desktop.store.save_goal(&goal).unwrap();
    pair(&desktop.store, &phone.store);
    assert_eq!(library(&phone.store), library(&desktop.store));
    assert_eq!(find(&phone.store, &book.id).unwrap().title, "Ѓердан од ќерамиди");
    assert_eq!(library(&phone.store).goals[0].target, 31);
    // The phone's own Fiction and Non-fiction are gone, not added as duplicates.
    assert_eq!(library(&phone.store).categories.len(), 2);
}

#[test]
fn edits_flow_both_ways_and_different_fields_both_survive() {
    let (desktop, phone) = (peer(), peer());
    let book = save_new(&desktop.store, "Drive");
    pair(&desktop.store, &phone.store);
    // UC-03: the title on the desktop, the page count on the phone, before they sync.
    let mut on_desktop = find(&desktop.store, &book.id).unwrap();
    on_desktop.title = "Drive (2nd ed.)".into();
    desktop.store.save_book(&on_desktop).unwrap();
    let mut on_phone = find(&phone.store, &book.id).unwrap();
    on_phone.total_pages = Some(300);
    on_phone.current_page = 40;
    phone.store.save_book(&on_phone).unwrap();
    session(&desktop.store, &phone.store);
    for store in [&desktop.store, &phone.store] {
        let merged = find(store, &book.id).unwrap();
        assert_eq!((merged.title.as_str(), merged.total_pages, merged.current_page), ("Drive (2nd ed.)", Some(300), 40));
    }
}

#[test]
fn the_later_edit_to_the_same_field_wins_everywhere() {
    let (desktop, phone) = (peer(), peer());
    let book = save_new(&desktop.store, "First");
    pair(&desktop.store, &phone.store);
    let mut a = find(&desktop.store, &book.id).unwrap();
    a.notes = "desktop".into();
    desktop.store.save_book(&a).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let mut b = find(&phone.store, &book.id).unwrap();
    b.notes = "phone".into();
    phone.store.save_book(&b).unwrap();
    session(&desktop.store, &phone.store);
    assert_eq!(find(&desktop.store, &book.id).unwrap().notes, "phone");
    assert_eq!(find(&phone.store, &book.id).unwrap().notes, "phone");
}

#[test]
fn a_deletion_beats_a_later_edit() {
    let (desktop, phone) = (peer(), peer());
    let book = save_new(&desktop.store, "Doomed");
    pair(&desktop.store, &phone.store);
    desktop.store.delete_book(&find(&desktop.store, &book.id).unwrap()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let mut edited = find(&phone.store, &book.id).unwrap();
    edited.notes = "edited after the deletion".into();
    phone.store.save_book(&edited).unwrap();
    session(&desktop.store, &phone.store);
    assert!(find(&desktop.store, &book.id).is_none(), "D-17");
    assert!(find(&phone.store, &book.id).is_none(), "D-17");
}

#[test]
fn one_phones_change_reaches_another_through_the_desktop() {
    let (desktop, first, second) = (peer(), peer(), peer());
    pair(&desktop.store, &first.store);
    pair(&desktop.store, &second.store);
    let book = save_new(&first.store, "From the first phone");
    session(&desktop.store, &first.store);
    session(&desktop.store, &second.store);
    assert!(find(&second.store, &book.id).is_some(), "REQ-20j");
}

#[test]
fn a_deleted_category_clears_books_everywhere_even_when_assigned_concurrently() {
    let (desktop, phone) = (peer(), peer());
    let book = save_new(&desktop.store, "Shelved");
    pair(&desktop.store, &phone.store);
    let category = library(&desktop.store).categories[0].clone();
    desktop.store.delete_category(&category).unwrap();
    let mut on_phone = find(&phone.store, &book.id).unwrap();
    on_phone.category_id = Some(category.id.clone());
    phone.store.save_book(&on_phone).unwrap();
    session(&desktop.store, &phone.store);
    session(&desktop.store, &phone.store);
    assert_eq!(library(&desktop.store), library(&phone.store));
    assert_eq!(find(&phone.store, &book.id).unwrap().category_id, None);
}

#[test]
fn moving_a_book_changes_only_its_key_and_syncs() {
    let (desktop, phone) = (peer(), peer());
    let ids: Vec<String> = ["a", "b", "c", "d"].iter().map(|t| save_new(&desktop.store, t).id).collect();
    pair(&desktop.store, &phone.store);
    let mut books = library(&desktop.store).books;
    let changed = query::move_book(&mut books, &ids[3], &ids[1]);
    assert_eq!(changed.len(), 1, "REQ-32f");
    desktop.store.save_order(&changed).unwrap();
    session(&desktop.store, &phone.store);
    let order: Vec<String> = library(&phone.store).books.iter().map(|b| b.title.clone()).collect();
    assert_eq!(order, ["a", "d", "b", "c"]);
}

#[test]
fn a_restore_with_replace_deletes_the_removed_books_on_other_devices() {
    let (desktop, phone) = (peer(), peer());
    let kept = save_new(&desktop.store, "Kept");
    let gone = save_new(&desktop.store, "Gone");
    pair(&desktop.store, &phone.store);
    let mut restored = library(&desktop.store);
    restored.books.retain(|book| book.id == kept.id);
    desktop.store.replace_all(&restored).unwrap();
    session(&desktop.store, &phone.store);
    assert!(find(&phone.store, &gone.id).is_none());
    assert!(find(&phone.store, &kept.id).is_some());
}

/// QA-21: a session cut off at any point leaves the receiver exactly as it was.
#[test]
fn an_interrupted_session_changes_nothing() {
    let (desktop, phone) = (peer(), peer());
    for n in 0..30 {
        save_new(&desktop.store, &format!("Book {n}"));
    }
    phone.store.reset_for_pairing().unwrap();
    let delta = desktop.store.delta_since(&phone.store.sync_vector().unwrap()).unwrap();
    assert!(delta.units.len() > 100);
    let before = (library(&phone.store), phone.store.sync_vector().unwrap());
    let mut rng = Rng(7);
    for _ in 0..100 {
        let cut = rng.below(delta.units.len());
        assert!(phone.store.apply_failing_after(&delta, cut).is_err());
        assert_eq!((library(&phone.store), phone.store.sync_vector().unwrap()), before, "cut at unit {cut}");
    }
    phone.store.apply_delta(&delta).unwrap();
    assert_eq!(library(&phone.store), library(&desktop.store));
}

// ---------- QA-20: random operations on three devices always converge ----------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

fn random_operation(store: &Store, rng: &mut Rng, step: usize) {
    let current = library(store);
    let pick_book = |rng: &mut Rng| (!current.books.is_empty()).then(|| current.books[rng.below(current.books.len())].clone());
    match rng.below(12) {
        0 | 1 => {
            let mut book = Book { title: format!("Book {step}"), ..Book::new(query::next_position(&current.books)) };
            if rng.chance(50) && !current.categories.is_empty() {
                let category = &current.categories[rng.below(current.categories.len())];
                book.category_id = Some(category.id.clone());
                if !category.sub_categories.is_empty() && rng.chance(50) {
                    book.sub_category_id = Some(category.sub_categories[rng.below(category.sub_categories.len())].id.clone());
                }
            }
            store.save_book(&book).unwrap();
        }
        2..=5 => {
            let Some(mut book) = pick_book(rng) else { return };
            match rng.below(7) {
                0 => book.title = format!("Retitled {step}"),
                1 => book.notes = format!("Note {step}"),
                2 => {
                    let total = 50 + rng.below(400) as u32;
                    book.total_pages = Some(total);
                    book.current_page = rng.below(total as usize + 1) as u32;
                }
                3 => book.status = [Status::ToRead, Status::Reading, Status::Finished][rng.below(3)],
                4 => book.priority = [Priority::High, Priority::Medium, Priority::Low][rng.below(3)],
                5 => book.rating = if rng.chance(20) { None } else { Some(1 + rng.below(5) as u8) },
                _ => {
                    book.category_id = (!current.categories.is_empty()).then(|| current.categories[rng.below(current.categories.len())].id.clone());
                    book.sub_category_id = None;
                }
            }
            store.save_book(&book).unwrap();
        }
        6 => {
            if let Some(book) = pick_book(rng) {
                store.delete_book(&book).unwrap();
            }
        }
        7 => {
            let mut books = current.books.clone();
            if books.len() >= 2 {
                let (moved, target) = (books[rng.below(books.len())].id.clone(), books[rng.below(books.len())].id.clone());
                let changed = query::move_book(&mut books, &moved, &target);
                store.save_order(&changed).unwrap();
            }
        }
        8 => {
            let category = Category { id: crate::model::new_id(), name: format!("Shelf {step}"), sub_categories: vec![], background: None };
            store.save_category(&category, current.categories.len()).unwrap();
        }
        9 => {
            if current.categories.is_empty() {
                return;
            }
            let index = rng.below(current.categories.len());
            let mut category = current.categories[index].clone();
            if rng.chance(50) {
                category.name = format!("Renamed {step}");
            } else {
                category.sub_categories.push(SubCategory { id: crate::model::new_id(), name: format!("Sub {step}") });
            }
            store.save_category(&category, index).unwrap();
        }
        10 => {
            if current.categories.len() > 1 && rng.chance(40) {
                store.delete_category(&current.categories[rng.below(current.categories.len())]).unwrap();
            } else if let Some(sub) = current.categories.iter().flat_map(|c| c.sub_categories.iter()).next() {
                store.delete_sub_category(&sub.id).unwrap();
            }
        }
        _ => {
            let id = if rng.chance(50) { GoalId::BooksPerYear } else { GoalId::BooksPerMonth };
            store.save_goal(&Goal { id, target: 1 + rng.below(60) as u32, active: rng.chance(80) }).unwrap();
        }
    }
}

fn converge(seed: u64, operations: usize) {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let peers = [peer(), peer(), peer()];
    let (desktop, phones) = (&peers[0].store, [&peers[1].store, &peers[2].store]);
    for phone in phones {
        pair(desktop, phone);
    }
    // Clocks up to 24 hours apart (QA-20).
    for p in &peers {
        p.store.set_clock_skew(rng.below(48 * 3_600_000) as i64 - 24 * 3_600_000);
    }
    for step in 0..operations {
        let who = rng.below(3);
        random_operation(&peers[who].store, &mut rng, step);
        if rng.chance(15) {
            // Sessions happen at random, so devices drift apart and come back (disconnections).
            session(desktop, phones[rng.below(2)]);
        }
    }
    for _ in 0..6 {
        for phone in phones {
            session(desktop, phone);
        }
    }
    let expected = library(desktop);
    for (index, phone) in phones.iter().enumerate() {
        assert_eq!(library(phone), expected, "seed {seed}: phone {index} differs from the desktop");
    }
    assert_eq!(session(desktop, phones[0]) + session(desktop, phones[1]), 0, "seed {seed}: a further session still changed something");
}

/// A quick version for every test run. `qa20_full` is the full measurement.
#[test]
fn three_devices_converge_after_random_edits() {
    for seed in 1..=8 {
        converge(seed, 600);
    }
}

/// QA-20 in full: 100 seeds of 10,000 operations, spread across the processor's cores. Slow,
/// so run it on purpose:
/// `cargo test --release -p booklub-core qa20_full -- --ignored --nocapture`
#[test]
#[ignore]
fn qa20_full() {
    let next = std::sync::atomic::AtomicU64::new(1);
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let seed = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if seed > 100 {
                    break;
                }
                let started = std::time::Instant::now();
                converge(seed, 10_000);
                eprintln!("seed {seed} converged in {:.0} s", started.elapsed().as_secs_f64());
            });
        }
    });
}

// ---------- QA-22: an existing library upgrades with nothing lost ----------

#[test]
fn a_schema_3_library_upgrades_keeping_everything_and_ready_to_sync() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("library.sqlite3");
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE categories (id TEXT PRIMARY KEY, name TEXT NOT NULL, position INTEGER NOT NULL, background TEXT);
                 CREATE TABLE sub_categories (id TEXT PRIMARY KEY, category_id TEXT NOT NULL REFERENCES categories(id) ON DELETE CASCADE, name TEXT NOT NULL, position INTEGER NOT NULL);
                 CREATE TABLE books (id TEXT PRIMARY KEY, title TEXT NOT NULL, author TEXT NOT NULL DEFAULT '', category_id TEXT REFERENCES categories(id) ON DELETE SET NULL,
                   sub_category_id TEXT REFERENCES sub_categories(id) ON DELETE SET NULL, priority TEXT NOT NULL DEFAULT 'Medium', status TEXT NOT NULL DEFAULT 'To Read',
                   total_pages INTEGER, current_page INTEGER NOT NULL DEFAULT 0, start_date TEXT, finish_date TEXT, rating INTEGER, notes TEXT NOT NULL DEFAULT '',
                   isbn TEXT, publisher TEXT NOT NULL DEFAULT '', published_year INTEGER, language TEXT NOT NULL DEFAULT '', background_file TEXT,
                   position INTEGER NOT NULL, added_at TEXT NOT NULL);
                 CREATE TABLE goals (id TEXT PRIMARY KEY, target INTEGER NOT NULL, active INTEGER NOT NULL);
                 INSERT INTO categories VALUES ('c1', 'Strategy', 0, 'preset:teal-geometry');
                 INSERT INTO sub_categories VALUES ('s1', 'c1', 'Pricing', 0);
                 INSERT INTO books (id, title, author, category_id, sub_category_id, total_pages, current_page, background_file, position, added_at)
                   VALUES ('b2', 'Second', 'B', 'c1', 's1', 300, 40, 'x.jpg', 7, '2026-01-02T00:00:00Z'),
                          ('b1', 'First', 'A', NULL, NULL, NULL, 0, NULL, 3, '2026-01-01T00:00:00Z');
                 INSERT INTO goals VALUES ('booksPerYear', 30, 1), ('booksPerMonth', 3, 0);
                 PRAGMA user_version = 3;",
            )
            .unwrap();
    }
    let backups = dir.path().join("backups");
    let store = Store::open(&path, &backups).unwrap();
    assert_eq!(std::fs::read_dir(&backups).unwrap().count(), 1, "QA-18");
    let upgraded = library(&store);
    let titles: Vec<&str> = upgraded.books.iter().map(|b| b.title.as_str()).collect();
    assert_eq!(titles, ["First", "Second"], "manual order kept");
    let second = &upgraded.books[1];
    assert_eq!((second.category_id.as_deref(), second.sub_category_id.as_deref(), second.total_pages, second.current_page), (Some("c1"), Some("s1"), Some(300), 40));
    assert_eq!(second.background_file.as_deref(), Some("x.jpg"));
    assert_eq!(upgraded.categories[0].sub_categories[0].name, "Pricing");
    assert_eq!(upgraded.goals, vec![Goal { id: GoalId::BooksPerYear, target: 30, active: true }, Goal { id: GoalId::BooksPerMonth, target: 3, active: false }]);
    // Everything is stamped, so a phone paired now receives the whole library.
    let phone = peer();
    pair(&store, &phone.store);
    assert_eq!(library(&phone.store), upgraded);
    assert!(!store.peer_id().is_empty());
}

#[test]
fn a_save_without_a_sort_key_keeps_the_stored_one() {
    let desktop = peer();
    let book = save_new(&desktop.store, "Middlemarch");
    let stored = find(&desktop.store, &book.id).unwrap().sort_key;
    assert!(!stored.is_empty());
    let mut edited = book.clone();
    edited.title = "Middlemarch (Penguin)".into();
    edited.sort_key.clear();
    desktop.store.save_book(&edited).unwrap();
    assert_eq!(find(&desktop.store, &book.id).unwrap().sort_key, stored);
    // A brand-new book without a key gets one.
    let mut fresh = Book { title: "Persuasion".into(), ..Book::new(5) };
    fresh.sort_key.clear();
    desktop.store.save_book(&fresh).unwrap();
    assert!(!find(&desktop.store, &fresh.id).unwrap().sort_key.is_empty());
}

#[test]
fn paired_devices_are_stored_updated_and_removed() {
    use crate::sync::PairedPeer;
    let desktop = peer();
    let mut phone = PairedPeer { id: "p1".into(), name: "Galaxy A54".into(), fingerprint: "ab".repeat(32), addresses: vec!["192.168.1.7".into()], ..Default::default() };
    desktop.store.save_peer(&phone).unwrap();
    desktop.store.save_peer(&PairedPeer { id: "p2".into(), name: "S25".into(), fingerprint: "cd".repeat(32), ..Default::default() }).unwrap();
    phone.last_session = Some("2026-11-03T20:00:00Z".into());
    phone.acked.insert("origin".into(), "stamp".into());
    desktop.store.save_peer(&phone).unwrap();
    let peers = desktop.store.peers().unwrap();
    assert_eq!(peers.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Galaxy A54", "S25"]);
    assert_eq!(peers[0], phone);
    desktop.store.remove_peer("p1").unwrap();
    assert_eq!(desktop.store.peers().unwrap().len(), 1);
}

#[test]
fn a_tombstone_stays_until_every_paired_phone_acknowledges_it() {
    use crate::sync::PairedPeer;
    let (desktop, a54, s25) = (peer(), peer(), peer());
    let book = save_new(&desktop.store, "Doomed");
    pair(&desktop.store, &a54.store);
    pair(&desktop.store, &s25.store);
    for (phone, id) in [(&a54, "a54"), (&s25, "s25")] {
        let acked = phone.store.sync_vector().unwrap();
        desktop.store.save_peer(&PairedPeer { id: id.into(), name: id.into(), fingerprint: "00".repeat(32), acked, ..Default::default() }).unwrap();
    }
    desktop.store.delete_book(&find(&desktop.store, &book.id).unwrap()).unwrap();
    let tombstones = |store: &Store| store.delta_since(&Default::default()).unwrap().tombstones.len();
    assert_eq!(desktop.store.collect_tombstones().unwrap(), 0);
    session(&desktop.store, &a54.store);
    let mut record = desktop.store.peers().unwrap()[0].clone();
    record.acked = a54.store.sync_vector().unwrap();
    desktop.store.save_peer(&record).unwrap();
    assert_eq!(desktop.store.collect_tombstones().unwrap(), 0, "the S25 has not seen it yet");
    session(&desktop.store, &s25.store);
    let mut record = desktop.store.peers().unwrap()[1].clone();
    record.acked = s25.store.sync_vector().unwrap();
    desktop.store.save_peer(&record).unwrap();
    assert_eq!(desktop.store.collect_tombstones().unwrap(), 1);
    assert_eq!(tombstones(&desktop.store), 0);
    // Nothing comes back: both phones had already deleted it.
    session(&desktop.store, &a54.store);
    session(&desktop.store, &s25.store);
    assert!(find(&desktop.store, &book.id).is_none() && find(&a54.store, &book.id).is_none());
}

#[test]
fn saving_an_editor_opened_before_a_sync_keeps_the_synced_fields() {
    let (desktop, phone) = (peer(), peer());
    let book = save_new(&desktop.store, "Drive");
    pair(&desktop.store, &phone.store);
    // The Reader opens the editor on the desktop.
    let opened = find(&desktop.store, &book.id).unwrap();
    // Meanwhile the phone records progress, and syncs.
    let mut on_phone = find(&phone.store, &book.id).unwrap();
    on_phone.total_pages = Some(250);
    on_phone.current_page = 90;
    phone.store.save_book(&on_phone).unwrap();
    session(&desktop.store, &phone.store);
    // The Reader saves a new title.
    let mut edited = opened.clone();
    edited.title = "Drive (revised)".into();
    let current = find(&desktop.store, &book.id).unwrap();
    desktop.store.save_book(&crate::sync::rebase(&opened, &edited, &current)).unwrap();
    session(&desktop.store, &phone.store);
    for store in [&desktop.store, &phone.store] {
        let merged = find(store, &book.id).unwrap();
        assert_eq!((merged.title.as_str(), merged.current_page, merged.total_pages), ("Drive (revised)", 90, Some(250)));
    }
}

#[test]
fn a_book_deleted_elsewhere_cant_be_saved_back() {
    let (desktop, phone) = (peer(), peer());
    let book = save_new(&desktop.store, "Gone");
    pair(&desktop.store, &phone.store);
    let mut stale = find(&desktop.store, &book.id).unwrap();
    phone.store.delete_book(&find(&phone.store, &book.id).unwrap()).unwrap();
    session(&desktop.store, &phone.store);
    stale.notes = "typed while it was being deleted".into();
    let error = desktop.store.save_book(&stale).unwrap_err();
    assert!(error.message.contains("deleted on another device"), "{}", error.message);
    assert!(find(&desktop.store, &book.id).is_none());
}

#[test]
fn restoring_a_deleted_book_brings_it_back_on_every_device() {
    let (desktop, phone) = (peer(), peer());
    let book = save_new(&desktop.store, "Lost and found");
    let backup = library(&desktop.store);
    pair(&desktop.store, &phone.store);
    phone.store.delete_book(&find(&phone.store, &book.id).unwrap()).unwrap();
    session(&desktop.store, &phone.store);
    assert!(find(&desktop.store, &book.id).is_none());
    desktop.store.replace_all(&backup).unwrap();
    session(&desktop.store, &phone.store);
    for store in [&desktop.store, &phone.store] {
        let titles: Vec<String> = library(store).books.into_iter().map(|b| b.title).collect();
        assert_eq!(titles, ["Lost and found"]);
    }
    assert_eq!(library(&phone.store), library(&desktop.store));
}
