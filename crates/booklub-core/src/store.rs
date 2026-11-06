//! The SQLite store (Appendix B). Every multi-row write runs in one transaction (REQ-39c),
//! so a failure leaves prior data unchanged (QA-06).

use std::path::{Path, PathBuf};

use std::cell::RefCell;
use std::collections::HashSet;

use rusqlite::{params, Connection, ErrorCode, OptionalExtension, Row, Transaction};

use crate::model::{default_goals, new_id, Book, Category, CategoryBackground, Goal, GoalId, Library, Priority, Status, SubCategory};
use crate::sync::{self, book_units, category_units, changed_fields, goal_units, sub_units, ApplyReport, Clock, Delta, Identity, PairedPeer, Vector, BOOK, CATEGORY, GOAL, SUB_CATEGORY};

pub const SCHEMA_VERSION: i32 = 4;

/// Schema 4: what sync needs (Android SRS Appendix B).
const SYNC_SCHEMA: &str = "
ALTER TABLE books ADD COLUMN sort_key TEXT NOT NULL DEFAULT '';
CREATE INDEX books_sort_key ON books(sort_key);
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE field_stamps (
  entity TEXT NOT NULL,
  id     TEXT NOT NULL,
  field  TEXT NOT NULL,
  stamp  TEXT NOT NULL,
  origin TEXT NOT NULL,
  PRIMARY KEY (entity, id, field)
);
CREATE INDEX field_stamps_origin ON field_stamps(origin, stamp);
CREATE TABLE tombstones (
  entity TEXT NOT NULL,
  id     TEXT NOT NULL,
  stamp  TEXT NOT NULL,
  origin TEXT NOT NULL,
  PRIMARY KEY (entity, id)
);
CREATE INDEX tombstones_origin ON tombstones(origin, stamp);
CREATE TABLE vectors (origin TEXT PRIMARY KEY, stamp TEXT NOT NULL);
CREATE TABLE peers (
  id           TEXT PRIMARY KEY,
  name         TEXT NOT NULL,
  fingerprint  TEXT NOT NULL,
  addresses    TEXT NOT NULL DEFAULT '',
  last_session TEXT,
  acked        TEXT NOT NULL DEFAULT '{}'
);
";

const SCHEMA: &str = "
CREATE TABLE categories (
  id       TEXT PRIMARY KEY,
  name     TEXT NOT NULL CHECK (length(trim(name)) > 0),
  position INTEGER NOT NULL
);
CREATE TABLE sub_categories (
  id          TEXT PRIMARY KEY,
  category_id TEXT NOT NULL REFERENCES categories(id) ON DELETE CASCADE,
  name        TEXT NOT NULL CHECK (length(trim(name)) > 0),
  position    INTEGER NOT NULL
);
CREATE TABLE books (
  id              TEXT PRIMARY KEY,
  title           TEXT NOT NULL CHECK (length(trim(title)) > 0),
  author          TEXT NOT NULL DEFAULT '',
  category_id     TEXT REFERENCES categories(id) ON DELETE SET NULL,
  sub_category_id TEXT REFERENCES sub_categories(id) ON DELETE SET NULL,
  priority        TEXT NOT NULL DEFAULT 'Medium' CHECK (priority IN ('High','Medium','Low')),
  status          TEXT NOT NULL DEFAULT 'To Read' CHECK (status IN ('To Read','Reading','Finished')),
  total_pages     INTEGER CHECK (total_pages IS NULL OR total_pages > 0),
  current_page    INTEGER NOT NULL DEFAULT 0 CHECK (current_page >= 0),
  start_date      TEXT,
  finish_date     TEXT,
  rating          INTEGER CHECK (rating IS NULL OR rating BETWEEN 1 AND 5),
  notes           TEXT NOT NULL DEFAULT '',
  isbn            TEXT,
  publisher       TEXT NOT NULL DEFAULT '',
  published_year  INTEGER,
  language        TEXT NOT NULL DEFAULT '',
  background_file TEXT,
  position        INTEGER NOT NULL,
  added_at        TEXT NOT NULL,
  CHECK (total_pages IS NULL OR current_page <= total_pages),
  CHECK (start_date IS NULL OR finish_date IS NULL OR finish_date >= start_date)
);
CREATE INDEX books_position ON books(position);
CREATE INDEX books_isbn ON books(isbn);
CREATE TABLE goals (
  id     TEXT PRIMARY KEY CHECK (id IN ('booksPerYear','booksPerMonth')),
  target INTEGER NOT NULL CHECK (target > 0),
  active INTEGER NOT NULL CHECK (active IN (0,1))
);
";

/// REQ-05j: seeded on first run so the Reader has somewhere to file a book immediately.
const SEED_CATEGORIES: &[&str] = &["Fiction", "Non-fiction"];

/// A failed storage operation, carrying the text for the banner (REQ-31f, QA-06, QA-07).
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct StoreError {
    pub operation: String,
    pub message: String,
    pub disk_full: bool,
}

impl StoreError {
    fn new(operation: &str, cause: &rusqlite::Error) -> Self {
        let disk_full = matches!(
            cause,
            rusqlite::Error::SqliteFailure(error, _) if error.code == ErrorCode::DiskFull
        );
        let message = if disk_full {
            format!("Your disk is full, so \"{operation}\" could not be saved. Free some disk space and try again.")
        } else {
            format!("Could not save \"{operation}\". Your existing data has not been changed.")
        };
        Self { operation: operation.to_string(), message, disk_full }
    }
}

type Result<T> = std::result::Result<T, StoreError>;

pub struct Store {
    pub(crate) connection: Connection,
    pub(crate) identity: Identity,
}

pub(crate) fn book_from_row(row: &Row) -> rusqlite::Result<Book> {
    Ok(Book {
        id: row.get("id")?,
        title: row.get("title")?,
        author: row.get("author")?,
        category_id: row.get("category_id")?,
        sub_category_id: row.get("sub_category_id")?,
        priority: Priority::parse(&row.get::<_, String>("priority")?).unwrap_or(Priority::Medium),
        status: Status::parse(&row.get::<_, String>("status")?).unwrap_or(Status::ToRead),
        total_pages: row.get("total_pages")?,
        current_page: row.get("current_page")?,
        start_date: row.get("start_date")?,
        finish_date: row.get("finish_date")?,
        rating: row.get("rating")?,
        notes: row.get("notes")?,
        order: row.get("position")?,
        added_at: row.get("added_at")?,
        isbn: row.get("isbn")?,
        publisher: row.get("publisher")?,
        published_year: row.get("published_year")?,
        language: row.get("language")?,
        background_file: row.get("background_file")?,
        sort_key: row.get("sort_key")?,
    })
}

pub(crate) fn write_book(connection: &Connection, book: &Book) -> rusqlite::Result<()> {
    connection
        .prepare_cached(
            "INSERT INTO books (id, title, author, category_id, sub_category_id, priority, status,
               total_pages, current_page, start_date, finish_date, rating, notes, isbn, publisher,
               published_year, language, background_file, position, added_at, sort_key)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)
             ON CONFLICT(id) DO UPDATE SET
               title=excluded.title, author=excluded.author, category_id=excluded.category_id,
               sub_category_id=excluded.sub_category_id, priority=excluded.priority,
               status=excluded.status, total_pages=excluded.total_pages,
               current_page=excluded.current_page, start_date=excluded.start_date,
               finish_date=excluded.finish_date, rating=excluded.rating, notes=excluded.notes,
               isbn=excluded.isbn, publisher=excluded.publisher,
               published_year=excluded.published_year, language=excluded.language,
               background_file=excluded.background_file, position=excluded.position,
               added_at=excluded.added_at, sort_key=excluded.sort_key",
        )?
        .execute(params![
            book.id,
            book.title,
            book.author,
            book.category_id,
            book.sub_category_id,
            book.priority.as_str(),
            book.status.as_str(),
            book.total_pages,
            book.current_page,
            book.start_date,
            book.finish_date,
            book.rating,
            book.notes,
            book.isbn,
            book.publisher,
            book.published_year,
            book.language,
            book.background_file,
            book.order,
            book.added_at,
            book.sort_key,
        ])?;
    Ok(())
}

/// Writes a book and stamps the units that changed (Android SRS REQ-11d).
fn save_stamped_book(tx: &Transaction, identity: &Identity, book: &Book) -> rusqlite::Result<()> {
    let previous = sync::read_book(tx, &book.id)?;
    // A caller that never saw a key (an old phone build, say) keeps the stored one.
    let mut book = book.clone();
    if book.sort_key.is_empty() {
        book.sort_key = previous.as_ref().map(|old| old.sort_key.clone()).unwrap_or_else(|| sync::new_sort_key(book.order));
    }
    let old = previous.map(|old| book_units(&old));
    write_book(tx, &book)?;
    identity.stamp(tx, BOOK, &book.id, &changed_fields(old, &book_units(&book)))
}

fn is_dead(tx: &Transaction, entity: &str, id: &str) -> rusqlite::Result<bool> {
    tx.query_row("SELECT 1 FROM tombstones WHERE entity = ?1 AND id = ?2", [entity, id], |_| Ok(())).optional().map(|found| found.is_some())
}

/// D-17 means an ID deleted on any device stays deleted everywhere. When a restore or import
/// brings such an item back, it returns under a new ID, with references rewritten, so every
/// device receives it as new.
fn revive(tx: &Transaction, books: &[Book], categories: &[(Category, usize)]) -> rusqlite::Result<(Vec<Book>, Vec<(Category, usize)>)> {
    let mut renamed: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let fresh = |old: &str, renamed: &mut std::collections::HashMap<String, String>| {
        let id = new_id();
        renamed.insert(old.to_string(), id.clone());
        id
    };
    let mut out_categories = Vec::with_capacity(categories.len());
    for (category, position) in categories {
        let mut category = category.clone();
        if is_dead(tx, CATEGORY, &category.id)? {
            category.id = fresh(&category.id, &mut renamed);
        }
        for sub in &mut category.sub_categories {
            if is_dead(tx, SUB_CATEGORY, &sub.id)? {
                sub.id = fresh(&sub.id, &mut renamed);
            }
        }
        out_categories.push((category, *position));
    }
    let mut out_books = Vec::with_capacity(books.len());
    for book in books {
        let mut book = book.clone();
        if is_dead(tx, BOOK, &book.id)? {
            book.id = fresh(&book.id, &mut renamed);
        }
        for reference in [&mut book.category_id, &mut book.sub_category_id] {
            if let Some(new) = reference.as_ref().and_then(|old| renamed.get(old)) {
                *reference = Some(new.clone());
            }
        }
        out_books.push(book);
    }
    Ok((out_books, out_categories))
}

fn write_category(tx: &Transaction, identity: &Identity, category: &Category, position: i64) -> rusqlite::Result<()> {
    let old = sync::read_category(tx, &category.id)?.map(|row| category_units(&row));
    let new = sync::CategoryRow { name: category.name.clone(), background: category.background.as_ref().map(CategoryBackground::encode), position };
    tx.execute(
        "INSERT INTO categories (id, name, position, background) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET name=excluded.name, position=excluded.position,
           background=excluded.background",
        params![category.id, category.name, position, category.background.as_ref().map(CategoryBackground::encode)],
    )?;
    identity.stamp(tx, CATEGORY, &category.id, &changed_fields(old, &category_units(&new)))?;
    for (sub_position, sub) in category.sub_categories.iter().enumerate() {
        let old = sync::read_sub(tx, &sub.id)?.map(|row| sub_units(&row));
        let new = sync::SubRow { category_id: category.id.clone(), name: sub.name.clone(), position: sub_position as i64 };
        tx.execute(
            "INSERT INTO sub_categories (id, category_id, name, position) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET category_id=excluded.category_id, name=excluded.name,
               position=excluded.position",
            params![sub.id, category.id, sub.name, sub_position as i64],
        )?;
        identity.stamp(tx, SUB_CATEGORY, &sub.id, &changed_fields(old, &sub_units(&new)))?;
    }
    Ok(())
}

fn write_goal(tx: &Transaction, identity: &Identity, goal: &Goal) -> rusqlite::Result<()> {
    let old = sync::read_goal(tx, goal.id.as_str())?.map(|old| goal_units(&old));
    tx.execute(
        "INSERT INTO goals (id, target, active) VALUES (?1, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET target=excluded.target, active=excluded.active",
        params![goal.id.as_str(), goal.target, goal.active],
    )?;
    identity.stamp(tx, GOAL, goal.id.as_str(), &changed_fields(old, &goal_units(goal)))
}

/// Gives books new keys that follow every existing key, in their `order`. For imports and
/// restores, whose keys came from parsing rather than from where the books now go.
fn rekey_in_order(books: &[Book]) -> Vec<Book> {
    let mut sorted: Vec<Book> = books.to_vec();
    sorted.sort_by_key(|book| book.order);
    for book in &mut sorted {
        book.sort_key = sync::new_sort_key(book.order);
    }
    sorted
}

impl Store {
    /// Opens the database, creating or migrating it. `backups` receives a dated copy before
    /// any migration of an existing store (QA-18).
    pub fn open(path: &Path, backups: &Path) -> Result<Self> {
        let open_error = |e: rusqlite::Error| StoreError::new("open your library", &e);
        let existed = path.exists();
        let connection = Connection::open(path).map_err(open_error)?;
        connection.pragma_update(None, "foreign_keys", "ON").map_err(open_error)?;
        connection.pragma_update(None, "journal_mode", "WAL").map_err(open_error)?;
        let identity = Identity { peer: String::new(), clock: RefCell::new(Clock::default()) };
        let mut store = Self { connection, identity };

        let version: i32 = store
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(open_error)?;

        if version > SCHEMA_VERSION {
            return Err(StoreError {
                operation: "open your library".into(),
                message: format!("This library was saved by a newer version of Booklub (schema {version}). Update Booklub to open it."),
                disk_full: false,
            });
        }
        if existed && version > 0 && version < SCHEMA_VERSION {
            store.backup_before_migration(path, backups, version).map_err(|message| StoreError {
                operation: "back up your library".into(),
                message,
                disk_full: false,
            })?;
        }
        if version < SCHEMA_VERSION {
            store.migrate(version).map_err(open_error)?;
        }
        store.load_identity().map_err(open_error)?;
        Ok(store)
    }

    fn load_identity(&mut self) -> rusqlite::Result<()> {
        let value = |key: &str| -> rusqlite::Result<Option<String>> {
            self.connection.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| row.get(0)).optional()
        };
        self.identity.peer = value("peer")?.unwrap_or_default();
        self.identity.clock = RefCell::new(Clock::restore(&value("clock")?.unwrap_or_default()));
        Ok(())
    }

    fn backup_before_migration(&self, path: &Path, backups: &Path, version: i32) -> std::result::Result<(), String> {
        std::fs::create_dir_all(backups).map_err(|e| e.to_string())?;
        let stamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S");
        let target: PathBuf = backups.join(format!("library-v{version}-{stamp}.sqlite3"));
        // VACUUM INTO captures a consistent copy, including anything still in the WAL.
        self.connection
            .execute("VACUUM INTO ?1", params![target.to_string_lossy()])
            .map_err(|e| format!("Could not back up {} before upgrading it: {e}", path.display()))?;
        Ok(())
    }

    /// QA-39: each step moves one version forward, in one transaction.
    fn migrate(&mut self, from: i32) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        if from < 2 {
            tx.execute_batch(SCHEMA)?;
        }
        if from < 3 {
            // REQ-45: what the Reader chose for a Category tile.
            tx.execute_batch("ALTER TABLE categories ADD COLUMN background TEXT;")?;
        }
        if from < 4 {
            tx.execute_batch(SYNC_SCHEMA)?;
            // Keeps today's manual order exactly.
            let positions: Vec<(String, i64)> = {
                let mut statement = tx.prepare("SELECT id, position FROM books ORDER BY position, id")?;
                let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            for (rank, (id, _)) in positions.iter().enumerate() {
                tx.execute("UPDATE books SET sort_key = ?1 WHERE id = ?2", params![sync::legacy_sort_key(rank as i64), id])?;
            }
            let peer = new_id();
            tx.execute("INSERT INTO meta (key, value) VALUES ('peer', ?1)", [&peer])?;
            self.identity.peer = peer;
        }
        // Only a brand new library gets these, so an upgrade never adds to a Reader's categories.
        if from < 2 {
            for (position, name) in SEED_CATEGORIES.iter().enumerate() {
                let category = Category { id: new_id(), name: (*name).to_string(), sub_categories: vec![], background: None };
                write_category(&tx, &self.identity, &category, position as i64)?;
            }
            for goal in default_goals() {
                write_goal(&tx, &self.identity, &goal)?;
            }
        }
        if from < 4 && from >= 2 {
            // An existing library becomes this device's first changes, so a paired phone gets it all.
            self.stamp_everything(&tx)?;
        }
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()
    }

    fn stamp_everything(&self, tx: &Transaction) -> rusqlite::Result<()> {
        let library = self.load_from(tx)?;
        for book in &library.books {
            self.identity.stamp(tx, BOOK, &book.id, &changed_fields(None, &book_units(book)))?;
        }
        for (position, category) in library.categories.iter().enumerate() {
            let row = sync::CategoryRow { name: category.name.clone(), background: category.background.as_ref().map(CategoryBackground::encode), position: position as i64 };
            self.identity.stamp(tx, CATEGORY, &category.id, &changed_fields(None, &category_units(&row)))?;
            for (sub_position, sub) in category.sub_categories.iter().enumerate() {
                let row = sync::SubRow { category_id: category.id.clone(), name: sub.name.clone(), position: sub_position as i64 };
                self.identity.stamp(tx, SUB_CATEGORY, &sub.id, &changed_fields(None, &sub_units(&row)))?;
            }
        }
        for goal in &library.goals {
            self.identity.stamp(tx, GOAL, goal.id.as_str(), &changed_fields(None, &goal_units(goal)))?;
        }
        Ok(())
    }

    pub fn load_all(&self) -> Result<Library> {
        self.load().map_err(|e| StoreError::new("open your library", &e))
    }

    fn load(&self) -> rusqlite::Result<Library> {
        self.load_from(&self.connection)
    }

    /// Ties fall to the id, so every device lists items in the same order.
    fn load_from(&self, connection: &Connection) -> rusqlite::Result<Library> {
        let mut statement = connection.prepare_cached("SELECT * FROM books ORDER BY sort_key, id")?;
        let mut books = statement.query_map([], book_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        for (rank, book) in books.iter_mut().enumerate() {
            book.order = rank as i64;
        }

        let mut categories = Vec::new();
        let mut statement = connection.prepare_cached("SELECT id, name, background FROM categories ORDER BY position, id")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?))
        })?;
        for row in rows {
            let (id, name, background) = row?;
            let background = background.as_deref().and_then(CategoryBackground::decode);
            let mut subs = connection.prepare_cached(
                "SELECT id, name FROM sub_categories WHERE category_id = ?1 ORDER BY position, id",
            )?;
            let sub_categories = subs
                .query_map([&id], |row| Ok(SubCategory { id: row.get(0)?, name: row.get(1)? }))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            categories.push(Category { id, name, sub_categories, background });
        }

        // Yearly goal first (REQ-15h).
        let mut goals = Vec::new();
        for id in [GoalId::BooksPerYear, GoalId::BooksPerMonth] {
            let goal = connection
                .query_row("SELECT target, active FROM goals WHERE id = ?1", [id.as_str()], |row| {
                    Ok(Goal { id, target: row.get(0)?, active: row.get(1)? })
                })
                .optional()?;
            goals.extend(goal);
        }
        Ok(Library { books, categories, goals })
    }

    pub fn save_book(&self, book: &Book) -> Result<()> {
        let operation = if book.title.trim().is_empty() { "untitled book" } else { book.title.trim() };
        let run = || -> rusqlite::Result<bool> {
            let tx = self.connection.unchecked_transaction()?;
            // D-17: another device deleted it, and that deletion wins.
            if is_dead(&tx, BOOK, &book.id)? {
                return Ok(false);
            }
            save_stamped_book(&tx, &self.identity, book)?;
            tx.commit()?;
            Ok(true)
        };
        match run() {
            Ok(true) => Ok(()),
            Ok(false) => Err(StoreError {
                operation: operation.to_string(),
                message: format!("\"{operation}\" was deleted on another device, so these changes can't be saved."),
                disk_full: false,
            }),
            Err(e) => Err(StoreError::new(operation, &e)),
        }
    }

    pub fn delete_book(&self, book: &Book) -> Result<()> {
        let run = || -> rusqlite::Result<()> {
            let tx = self.connection.unchecked_transaction()?;
            tx.execute("DELETE FROM books WHERE id = ?1", [&book.id])?;
            self.identity.tombstone(&tx, BOOK, &book.id)?;
            tx.commit()
        };
        run().map_err(|e| StoreError::new(&format!("delete {}", book.title), &e))
    }

    /// REQ-07a: persists the positions produced by a reposition.
    pub fn save_order(&self, changed: &[Book]) -> Result<()> {
        let operation = "the new book order";
        let run = || -> rusqlite::Result<()> {
            let tx = self.connection.unchecked_transaction()?;
            for book in changed {
                let old_key: Option<String> = tx.query_row("SELECT sort_key FROM books WHERE id = ?1", [&book.id], |row| row.get(0)).optional()?;
                tx.execute("UPDATE books SET position = ?1, sort_key = ?2 WHERE id = ?3", params![book.order, book.sort_key, book.id])?;
                if old_key.as_deref() != Some(book.sort_key.as_str()) {
                    self.identity.stamp(&tx, BOOK, &book.id, &["position"])?;
                }
            }
            tx.commit()
        };
        run().map_err(|e| StoreError::new(operation, &e))
    }

    /// Writes one category and its sub-categories at the given position. REQ-05e needs no
    /// other work on rename, because books reference categories by id.
    pub fn save_category(&self, category: &Category, position: usize) -> Result<()> {
        let run = || -> rusqlite::Result<()> {
            let tx = self.connection.unchecked_transaction()?;
            write_category(&tx, &self.identity, category, position as i64)?;
            tx.commit()
        };
        run().map_err(|e| StoreError::new(&format!("category {}", category.name), &e))
    }

    /// REQ-32f, REQ-32g: rewrites every category and sub-category position.
    pub fn save_category_order(&self, categories: &[Category]) -> Result<()> {
        let run = || -> rusqlite::Result<()> {
            let tx = self.connection.unchecked_transaction()?;
            for (position, category) in categories.iter().enumerate() {
                write_category(&tx, &self.identity, category, position as i64)?;
            }
            tx.commit()
        };
        run().map_err(|e| StoreError::new("the new category order", &e))
    }

    /// REQ-05f: the schema clears the Category and its Sub-category from every book.
    pub fn delete_category(&self, category: &Category) -> Result<()> {
        let run = || -> rusqlite::Result<()> {
            let tx = self.connection.unchecked_transaction()?;
            let subs: Vec<String> = {
                let mut statement = tx.prepare("SELECT id FROM sub_categories WHERE category_id = ?1")?;
                let rows = statement.query_map([&category.id], |row| row.get(0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            tx.execute("DELETE FROM categories WHERE id = ?1", [&category.id])?;
            for sub in subs {
                self.identity.tombstone(&tx, SUB_CATEGORY, &sub)?;
            }
            self.identity.tombstone(&tx, CATEGORY, &category.id)?;
            tx.commit()
        };
        run().map_err(|e| StoreError::new(&format!("delete category {}", category.name), &e))
    }

    /// REQ-05g: the schema clears the Sub-category and leaves each book's Category.
    pub fn delete_sub_category(&self, sub_category_id: &str) -> Result<()> {
        let run = || -> rusqlite::Result<()> {
            let tx = self.connection.unchecked_transaction()?;
            tx.execute("DELETE FROM sub_categories WHERE id = ?1", [sub_category_id])?;
            self.identity.tombstone(&tx, SUB_CATEGORY, sub_category_id)?;
            tx.commit()
        };
        run().map_err(|e| StoreError::new("delete sub-category", &e))
    }

    pub fn save_goal(&self, goal: &Goal) -> Result<()> {
        let run = || -> rusqlite::Result<()> {
            let tx = self.connection.unchecked_transaction()?;
            write_goal(&tx, &self.identity, goal)?;
            tx.commit()
        };
        run().map_err(|e| StoreError::new("goal", &e))
    }

    /// Appends imported books and any new or extended categories as one transaction.
    pub fn save_import(&self, books: &[Book], categories: &[(Category, usize)]) -> Result<()> {
        let run = || -> rusqlite::Result<()> {
            let tx = self.connection.unchecked_transaction()?;
            let (books, categories) = revive(&tx, books, categories)?;
            for (category, position) in &categories {
                write_category(&tx, &self.identity, category, *position as i64)?;
            }
            for book in rekey_in_order(&books) {
                save_stamped_book(&tx, &self.identity, &book)?;
            }
            tx.commit()
        };
        run().map_err(|e| StoreError::new(&format!("{} imported books", books.len()), &e))
    }

    /// REQ-19a Replace: discards everything stored and writes the given library.
    pub fn replace_all(&self, library: &Library) -> Result<()> {
        let run = || -> rusqlite::Result<()> {
            let tx = self.connection.unchecked_transaction()?;
            let ids = |sql: &str| -> rusqlite::Result<Vec<String>> {
                let mut statement = tx.prepare(sql)?;
                let rows = statement.query_map([], |row| row.get(0))?;
                rows.collect()
            };
            let before = [(BOOK, ids("SELECT id FROM books")?), (CATEGORY, ids("SELECT id FROM categories")?), (SUB_CATEGORY, ids("SELECT id FROM sub_categories")?)];
            tx.execute_batch("DELETE FROM books; DELETE FROM sub_categories; DELETE FROM categories; DELETE FROM goals; DELETE FROM field_stamps;")?;
            let positioned: Vec<(Category, usize)> = library.categories.iter().cloned().enumerate().map(|(position, c)| (c, position)).collect();
            let (books, categories) = revive(&tx, &library.books, &positioned)?;
            let library = Library { books, categories: categories.into_iter().map(|(c, _)| c).collect(), goals: library.goals.clone() };
            for (position, category) in library.categories.iter().enumerate() {
                write_category(&tx, &self.identity, category, position as i64)?;
            }
            for book in rekey_in_order(&library.books) {
                save_stamped_book(&tx, &self.identity, &book)?;
            }
            for goal in &library.goals {
                write_goal(&tx, &self.identity, goal)?;
            }
            // What the restore removed is deleted on paired devices too.
            let kept: HashSet<&str> = library
                .books
                .iter()
                .map(|book| book.id.as_str())
                .chain(library.categories.iter().map(|c| c.id.as_str()))
                .chain(library.categories.iter().flat_map(|c| c.sub_categories.iter().map(|s| s.id.as_str())))
                .collect();
            for (entity, list) in &before {
                for id in list.iter().filter(|id| !kept.contains(id.as_str())) {
                    self.identity.tombstone(&tx, entity, id)?;
                }
            }
            tx.commit()
        };
        run().map_err(|e| StoreError::new("replace the library", &e))
    }

    // ---------- Sync ----------

    pub fn peer_id(&self) -> &str {
        &self.identity.peer
    }

    pub fn sync_vector(&self) -> Result<Vector> {
        sync::read_vector(&self.connection).map_err(|e| StoreError::new("read the sync state", &e))
    }

    /// Everything this device holds that a peer with `theirs` lacks (REQ-30b).
    pub fn delta_since(&self, theirs: &Vector) -> Result<Delta> {
        sync::delta_since(&self.connection, theirs).map_err(|e| StoreError::new("prepare changes for sync", &e))
    }

    /// Merges a peer's changes in one transaction (REQ-30c), keeping the library unchanged if
    /// anything fails.
    pub fn apply_delta(&self, delta: &Delta) -> Result<ApplyReport> {
        sync::apply(&self.connection, &self.identity, delta).map_err(|e| StoreError::new("apply changes from sync", &e))
    }

    /// A newly paired phone starts from its computer's library (UR-02), so it drops its own
    /// first, along with every record of what it has synced.
    pub fn reset_for_pairing(&self) -> Result<()> {
        self.connection
            .execute_batch(
                "BEGIN; DELETE FROM books; DELETE FROM sub_categories; DELETE FROM categories; DELETE FROM goals;
                 DELETE FROM field_stamps; DELETE FROM tombstones; DELETE FROM vectors; COMMIT;",
            )
            .map_err(|e| StoreError::new("prepare this phone for pairing", &e))
    }

    /// Every device this one is paired with, in the order they were paired.
    pub fn peers(&self) -> Result<Vec<PairedPeer>> {
        let run = || -> rusqlite::Result<Vec<PairedPeer>> {
            let mut statement = self.connection.prepare("SELECT id, name, fingerprint, addresses, last_session, acked FROM peers ORDER BY rowid")?;
            let rows = statement.query_map([], |row| {
                let addresses: String = row.get(3)?;
                let acked: String = row.get(5)?;
                Ok(PairedPeer {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    fingerprint: row.get(2)?,
                    addresses: addresses.split(' ').filter(|a| !a.is_empty()).map(String::from).collect(),
                    last_session: row.get(4)?,
                    acked: serde_json::from_str(&acked).unwrap_or_default(),
                })
            })?;
            rows.collect()
        };
        run().map_err(|e| StoreError::new("read the paired devices", &e))
    }

    /// Stores a newly paired device, or updates one already paired (REQ-20e).
    pub fn save_peer(&self, peer: &PairedPeer) -> Result<()> {
        self.connection
            .execute(
                "INSERT INTO peers (id, name, fingerprint, addresses, last_session, acked) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET name=excluded.name, fingerprint=excluded.fingerprint,
                   addresses=excluded.addresses, last_session=excluded.last_session, acked=excluded.acked",
                params![peer.id, peer.name, peer.fingerprint, peer.addresses.join(" "), peer.last_session, serde_json::to_string(&peer.acked).unwrap_or_default()],
            )
            .map(|_| ())
            .map_err(|e| StoreError::new(&format!("pairing with {}", peer.name), &e))
    }

    /// REQ-21a: forgets a paired device and its key.
    pub fn remove_peer(&self, id: &str) -> Result<()> {
        self.connection
            .execute("DELETE FROM peers WHERE id = ?1", [id])
            .map(|_| ())
            .map_err(|e| StoreError::new("removing the paired device", &e))
    }

    /// REQ-32e: deletes every tombstone that each paired device has acknowledged, and returns
    /// how many went. A device acknowledges by reporting a vector that covers the stamp.
    pub fn collect_tombstones(&self) -> Result<usize> {
        let peers = self.peers()?;
        let run = || -> rusqlite::Result<usize> {
            let tx = self.connection.unchecked_transaction()?;
            let doomed: Vec<(String, String)> = {
                let mut statement = tx.prepare("SELECT entity, id, stamp, origin FROM tombstones")?;
                let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)))?;
                rows.filter_map(|row| row.ok())
                    .filter(|(_, _, stamp, origin)| peers.iter().all(|peer| peer.acked.get(origin).is_some_and(|seen| seen >= stamp)))
                    .map(|(entity, id, _, _)| (entity, id))
                    .collect()
            };
            for (entity, id) in &doomed {
                tx.execute("DELETE FROM tombstones WHERE entity = ?1 AND id = ?2", [entity, id])?;
            }
            tx.commit()?;
            Ok(doomed.len())
        };
        run().map_err(|e| StoreError::new("tidying sync records", &e))
    }

    #[cfg(test)]
    pub(crate) fn set_clock_skew(&self, skew_ms: i64) {
        self.identity.clock.borrow_mut().skew_ms = skew_ms;
    }

    #[cfg(test)]
    pub(crate) fn apply_failing_after(&self, delta: &Delta, units: usize) -> rusqlite::Result<ApplyReport> {
        sync::apply_inner(&self.connection, &self.identity, delta, Some(units))
    }

    /// QA-33: every background file a book still references.
    pub fn referenced_backgrounds(&self) -> Result<Vec<String>> {
        let run = || -> rusqlite::Result<Vec<String>> {
            let mut statement = self.connection.prepare(
                "SELECT background_file FROM books WHERE background_file IS NOT NULL
                 UNION SELECT substr(background, 6) FROM categories WHERE background LIKE 'file:%'",
            )?;
            let rows = statement.query_map([], |row| row.get(0))?;
            rows.collect()
        };
        run().map_err(|e| StoreError::new("read your library", &e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open(dir: &TempDir) -> Store {
        Store::open(&dir.path().join("library.sqlite3"), &dir.path().join("backups")).unwrap()
    }

    fn book(title: &str, order: i64) -> Book {
        Book { title: title.into(), ..Book::new(order) }
    }

    /// The first seeded category, given a sub-category for tests that need one.
    fn category_with_sub(store: &Store) -> Category {
        let mut category = store.load_all().unwrap().categories[0].clone();
        category.sub_categories.push(SubCategory { id: new_id(), name: "Pricing".into() });
        store.save_category(&category, 0).unwrap();
        category
    }

    #[test]
    fn first_run_seeds_categories_and_goals() {
        let dir = TempDir::new().unwrap();
        let library = open(&dir).load_all().unwrap();
        let names: Vec<&str> = library.categories.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Fiction", "Non-fiction"], "REQ-05j");
        assert!(library.categories.iter().all(|c| c.sub_categories.is_empty() && c.background.is_none()));
        assert_eq!(library.goals[0], Goal { id: GoalId::BooksPerYear, target: 24, active: true });
        assert_eq!(library.goals[1], Goal { id: GoalId::BooksPerMonth, target: 2, active: true });
        assert!(library.books.is_empty());
    }

    #[test]
    fn reopening_keeps_data_and_does_not_reseed() {
        let dir = TempDir::new().unwrap();
        {
            let store = open(&dir);
            store.save_book(&book("Ѓердан од ќерамиди", 0)).unwrap();
        }
        let library = open(&dir).load_all().unwrap();
        assert_eq!(library.categories.len(), 2);
        assert_eq!(library.books[0].title, "Ѓердан од ќерамиди");
    }

    #[test]
    fn every_book_attribute_round_trips() {
        let dir = TempDir::new().unwrap();
        let store = open(&dir);
        let category = category_with_sub(&store);
        let original = Book {
            title: "Drive".into(),
            author: "Daniel H. Pink".into(),
            category_id: Some(category.id.clone()),
            sub_category_id: Some(category.sub_categories[0].id.clone()),
            priority: Priority::High,
            status: Status::Reading,
            total_pages: Some(288),
            current_page: 40,
            start_date: Some("2026-10-01".into()),
            finish_date: None,
            rating: Some(4),
            notes: "<b>x</b>".into(),
            isbn: Some("9781594484803".into()),
            publisher: "Riverhead".into(),
            published_year: Some(2011),
            language: "English".into(),
            background_file: Some("a.jpg".into()),
            ..Book::new(5)
        };
        store.save_book(&original).unwrap();
        // `order` is the book's rank in manual order, and it is the only book.
        assert_eq!(store.load_all().unwrap().books[0], Book { order: 0, ..original });
    }

    #[test]
    fn saving_twice_updates_in_place() {
        let dir = TempDir::new().unwrap();
        let store = open(&dir);
        let mut b = book("Drive", 0);
        store.save_book(&b).unwrap();
        b.title = "Drive 2".into();
        store.save_book(&b).unwrap();
        let books = store.load_all().unwrap().books;
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].title, "Drive 2");
    }

    #[test]
    fn deleting_a_category_clears_books_and_keeps_them() {
        let dir = TempDir::new().unwrap();
        let store = open(&dir);
        let category = category_with_sub(&store);
        let mut b = book("Drive", 0);
        b.category_id = Some(category.id.clone());
        b.sub_category_id = Some(category.sub_categories[0].id.clone());
        store.save_book(&b).unwrap();
        store.delete_category(&category).unwrap();
        let library = store.load_all().unwrap();
        assert_eq!(library.books[0].category_id, None);
        assert_eq!(library.books[0].sub_category_id, None);
        assert_eq!(library.categories.len(), 1);
    }

    #[test]
    fn deleting_a_sub_category_keeps_the_category() {
        let dir = TempDir::new().unwrap();
        let store = open(&dir);
        let category = category_with_sub(&store);
        let mut b = book("Drive", 0);
        b.category_id = Some(category.id.clone());
        b.sub_category_id = Some(category.sub_categories[0].id.clone());
        store.save_book(&b).unwrap();
        store.delete_sub_category(&category.sub_categories[0].id).unwrap();
        let books = store.load_all().unwrap().books;
        assert_eq!(books[0].category_id, Some(category.id));
        assert_eq!(books[0].sub_category_id, None);
    }

    #[test]
    fn the_schema_rejects_rule_breaking_books_and_leaves_data_alone() {
        let dir = TempDir::new().unwrap();
        let store = open(&dir);
        store.save_book(&book("Keep", 0)).unwrap();

        let error = store.save_book(&book("   ", 1)).unwrap_err();
        assert!(error.message.contains("Your existing data has not been changed."));
        assert!(!error.disk_full);

        let mut bad_dates = book("Bad", 1);
        bad_dates.start_date = Some("2026-05-02".into());
        bad_dates.finish_date = Some("2026-05-01".into());
        assert!(store.save_book(&bad_dates).is_err());

        let mut bad_pages = book("Bad", 1);
        bad_pages.total_pages = Some(10);
        bad_pages.current_page = 11;
        assert!(store.save_book(&bad_pages).is_err());

        assert_eq!(store.load_all().unwrap().books.len(), 1);
    }

    #[test]
    fn a_failed_import_writes_nothing() {
        let dir = TempDir::new().unwrap();
        let store = open(&dir);
        let batch = vec![book("Good", 0), book("  ", 1)];
        assert!(store.save_import(&batch, &[]).is_err());
        assert!(store.load_all().unwrap().books.is_empty(), "REQ-39c");
    }

    #[test]
    fn replace_all_discards_what_was_there() {
        let dir = TempDir::new().unwrap();
        let store = open(&dir);
        store.save_book(&book("Old", 0)).unwrap();
        let replacement = Library { books: vec![book("New", 0)], categories: vec![], goals: default_goals() };
        store.replace_all(&replacement).unwrap();
        let library = store.load_all().unwrap();
        assert_eq!(library.books[0].title, "New");
        assert!(library.categories.is_empty());
    }

    #[test]
    fn a_newer_database_is_refused_rather_than_damaged() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("library.sqlite3");
        {
            let connection = Connection::open(&path).unwrap();
            connection.pragma_update(None, "user_version", 99).unwrap();
        }
        let result = Store::open(&path, &dir.path().join("backups"));
        assert!(result.err().unwrap().message.contains("newer version"));
    }

    #[test]
    fn category_backgrounds_round_trip_and_count_as_referenced() {
        let dir = TempDir::new().unwrap();
        let store = open(&dir);
        let library = store.load_all().unwrap();
        let (mut first, mut second) = (library.categories[0].clone(), library.categories[1].clone());
        first.background = Some(CategoryBackground::Preset("ink-wash".into()));
        second.background = Some(CategoryBackground::File("abc.jpg".into()));
        store.save_category(&first, 0).unwrap();
        store.save_category(&second, 1).unwrap();
        let mut b = book("Drive", 0);
        b.background_file = Some("def.png".into());
        store.save_book(&b).unwrap();

        let reloaded = store.load_all().unwrap();
        assert_eq!(reloaded.categories[0].background, first.background);
        assert_eq!(reloaded.categories[1].background, second.background);
        let mut files = store.referenced_backgrounds().unwrap();
        files.sort();
        assert_eq!(files, ["abc.jpg", "def.png"], "QA-33 must not sweep a Category's image");
    }

    #[test]
    fn a_version_1_0_library_upgrades_with_a_backup_and_nothing_lost() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("library.sqlite3");
        {
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch(SCHEMA).unwrap();
            connection
                .execute_batch(
                    "INSERT INTO categories (id, name, position) VALUES ('c1', 'Product Management', 0);
                     INSERT INTO sub_categories (id, category_id, name, position) VALUES ('s1', 'c1', 'Discovery', 0);
                     INSERT INTO books (id, title, category_id, sub_category_id, position, added_at, background_file)
                       VALUES ('b1', 'Drive', 'c1', 's1', 0, '2026-01-01T00:00:00.000Z', 'keep.jpg');
                     INSERT INTO goals (id, target, active) VALUES ('booksPerYear', 30, 1);
                     PRAGMA user_version = 2;",
                )
                .unwrap();
        }
        let backups = dir.path().join("backups");
        let library = Store::open(&path, &backups).unwrap().load_all().unwrap();

        assert_eq!(std::fs::read_dir(&backups).unwrap().count(), 1, "QA-18");
        let names: Vec<&str> = library.categories.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Product Management"], "an upgrade never seeds new defaults");
        assert_eq!(library.categories[0].background, None);
        assert_eq!(library.categories[0].sub_categories[0].name, "Discovery");
        assert_eq!(library.books[0].background_file.as_deref(), Some("keep.jpg"));
        assert_eq!(library.goals[0].target, 30);
    }

    #[test]
    fn migrating_an_older_store_backs_it_up_first() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("library.sqlite3");
        {
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch("CREATE TABLE legacy (x); PRAGMA user_version = 1;").unwrap();
        }
        let backups = dir.path().join("backups");
        Store::open(&path, &backups).unwrap();
        let copies: Vec<_> = std::fs::read_dir(&backups).unwrap().collect();
        assert_eq!(copies.len(), 1, "QA-18");
    }
}
