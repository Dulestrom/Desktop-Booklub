//! The SQLite store (Appendix B). Every multi-row write runs in one transaction (REQ-39c),
//! so a failure leaves prior data unchanged (QA-06).

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, ErrorCode, OptionalExtension, Row, Transaction};

use crate::model::{default_goals, new_id, Book, Category, CategoryBackground, Goal, GoalId, Library, Priority, Status, SubCategory};

pub const SCHEMA_VERSION: i32 = 3;

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
    connection: Connection,
}

fn book_from_row(row: &Row) -> rusqlite::Result<Book> {
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
    })
}

fn write_book(connection: &Connection, book: &Book) -> rusqlite::Result<()> {
    connection
        .prepare_cached(
            "INSERT INTO books (id, title, author, category_id, sub_category_id, priority, status,
               total_pages, current_page, start_date, finish_date, rating, notes, isbn, publisher,
               published_year, language, background_file, position, added_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)
             ON CONFLICT(id) DO UPDATE SET
               title=excluded.title, author=excluded.author, category_id=excluded.category_id,
               sub_category_id=excluded.sub_category_id, priority=excluded.priority,
               status=excluded.status, total_pages=excluded.total_pages,
               current_page=excluded.current_page, start_date=excluded.start_date,
               finish_date=excluded.finish_date, rating=excluded.rating, notes=excluded.notes,
               isbn=excluded.isbn, publisher=excluded.publisher,
               published_year=excluded.published_year, language=excluded.language,
               background_file=excluded.background_file, position=excluded.position,
               added_at=excluded.added_at",
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
        ])?;
    Ok(())
}

fn write_category(tx: &Transaction, category: &Category, position: i64) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO categories (id, name, position, background) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET name=excluded.name, position=excluded.position,
           background=excluded.background",
        params![category.id, category.name, position, category.background.as_ref().map(CategoryBackground::encode)],
    )?;
    for (sub_position, sub) in category.sub_categories.iter().enumerate() {
        tx.execute(
            "INSERT INTO sub_categories (id, category_id, name, position) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET category_id=excluded.category_id, name=excluded.name,
               position=excluded.position",
            params![sub.id, category.id, sub.name, sub_position as i64],
        )?;
    }
    Ok(())
}

fn write_goal(connection: &Connection, goal: &Goal) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO goals (id, target, active) VALUES (?1, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET target=excluded.target, active=excluded.active",
        params![goal.id.as_str(), goal.target, goal.active],
    )?;
    Ok(())
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
        let store = Self { connection };

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
        Ok(store)
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
    fn migrate(&self, from: i32) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        if from < 2 {
            tx.execute_batch(SCHEMA)?;
        }
        if from < 3 {
            // REQ-45: what the Reader chose for a Category tile.
            tx.execute_batch("ALTER TABLE categories ADD COLUMN background TEXT;")?;
        }
        // Only a brand new library gets these, so an upgrade never adds to a Reader's categories.
        if from < 2 {
            for (position, name) in SEED_CATEGORIES.iter().enumerate() {
                let category = Category { id: new_id(), name: (*name).to_string(), sub_categories: vec![], background: None };
                write_category(&tx, &category, position as i64)?;
            }
            for goal in default_goals() {
                write_goal(&tx, &goal)?;
            }
        }
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()
    }

    pub fn load_all(&self) -> Result<Library> {
        self.load().map_err(|e| StoreError::new("open your library", &e))
    }

    fn load(&self) -> rusqlite::Result<Library> {
        let mut statement = self.connection.prepare_cached("SELECT * FROM books ORDER BY position")?;
        let books = statement.query_map([], book_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;

        let mut categories = Vec::new();
        let mut statement = self.connection.prepare_cached("SELECT id, name, background FROM categories ORDER BY position")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?))
        })?;
        for row in rows {
            let (id, name, background) = row?;
            let background = background.as_deref().and_then(CategoryBackground::decode);
            let mut subs = self.connection.prepare_cached(
                "SELECT id, name FROM sub_categories WHERE category_id = ?1 ORDER BY position",
            )?;
            let sub_categories = subs
                .query_map([&id], |row| Ok(SubCategory { id: row.get(0)?, name: row.get(1)? }))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            categories.push(Category { id, name, sub_categories, background });
        }

        // Yearly goal first (REQ-15h).
        let mut goals = Vec::new();
        for id in [GoalId::BooksPerYear, GoalId::BooksPerMonth] {
            let goal = self
                .connection
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
        write_book(&self.connection, book).map_err(|e| StoreError::new(operation, &e))
    }

    pub fn delete_book(&self, book: &Book) -> Result<()> {
        self.connection
            .execute("DELETE FROM books WHERE id = ?1", [&book.id])
            .map(drop)
            .map_err(|e| StoreError::new(&format!("delete {}", book.title), &e))
    }

    /// REQ-07a: persists the positions produced by a reposition.
    pub fn save_order(&self, changed: &[Book]) -> Result<()> {
        let operation = "the new book order";
        let run = || -> rusqlite::Result<()> {
            let tx = self.connection.unchecked_transaction()?;
            for book in changed {
                tx.execute("UPDATE books SET position = ?1 WHERE id = ?2", params![book.order, book.id])?;
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
            write_category(&tx, category, position as i64)?;
            tx.commit()
        };
        run().map_err(|e| StoreError::new(&format!("category {}", category.name), &e))
    }

    /// REQ-32f, REQ-32g: rewrites every category and sub-category position.
    pub fn save_category_order(&self, categories: &[Category]) -> Result<()> {
        let run = || -> rusqlite::Result<()> {
            let tx = self.connection.unchecked_transaction()?;
            for (position, category) in categories.iter().enumerate() {
                write_category(&tx, category, position as i64)?;
            }
            tx.commit()
        };
        run().map_err(|e| StoreError::new("the new category order", &e))
    }

    /// REQ-05f: the schema clears the Category and its Sub-category from every book.
    pub fn delete_category(&self, category: &Category) -> Result<()> {
        self.connection
            .execute("DELETE FROM categories WHERE id = ?1", [&category.id])
            .map(drop)
            .map_err(|e| StoreError::new(&format!("delete category {}", category.name), &e))
    }

    /// REQ-05g: the schema clears the Sub-category and leaves each book's Category.
    pub fn delete_sub_category(&self, sub_category_id: &str) -> Result<()> {
        self.connection
            .execute("DELETE FROM sub_categories WHERE id = ?1", [sub_category_id])
            .map(drop)
            .map_err(|e| StoreError::new("delete sub-category", &e))
    }

    pub fn save_goal(&self, goal: &Goal) -> Result<()> {
        write_goal(&self.connection, goal).map_err(|e| StoreError::new("goal", &e))
    }

    /// Appends imported books and any new or extended categories as one transaction.
    pub fn save_import(&self, books: &[Book], categories: &[(Category, usize)]) -> Result<()> {
        let run = || -> rusqlite::Result<()> {
            let tx = self.connection.unchecked_transaction()?;
            for (category, position) in categories {
                write_category(&tx, category, *position as i64)?;
            }
            for book in books {
                write_book(&tx, book)?;
            }
            tx.commit()
        };
        run().map_err(|e| StoreError::new(&format!("{} imported books", books.len()), &e))
    }

    /// REQ-19a Replace: discards everything stored and writes the given library.
    pub fn replace_all(&self, library: &Library) -> Result<()> {
        let run = || -> rusqlite::Result<()> {
            let tx = self.connection.unchecked_transaction()?;
            tx.execute_batch("DELETE FROM books; DELETE FROM sub_categories; DELETE FROM categories; DELETE FROM goals;")?;
            for (position, category) in library.categories.iter().enumerate() {
                write_category(&tx, category, position as i64)?;
            }
            for book in &library.books {
                write_book(&tx, book)?;
            }
            for goal in &library.goals {
                write_goal(&tx, goal)?;
            }
            tx.commit()
        };
        run().map_err(|e| StoreError::new("replace the library", &e))
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
        assert_eq!(store.load_all().unwrap().books[0], original);
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
