//! Two-way sync between paired devices (Android SRS Section 3.8).
//!
//! Every field a device changes gets a change stamp: a hybrid logical clock value ending in
//! that device's ID. Fields that must agree with each other travel together as one unit, so a
//! merge can never produce an invalid combination (current page above the total, a finish date
//! before the start, a Sub-category from another Category).
//!
//! Peers keep a version vector: for each device, the latest stamp they hold from it. A
//! session sends every unit the other side's vector shows it lacks, and the later stamp wins
//! field by field (REQ-32 to REQ-32b). A deletion leaves a tombstone, and beats any edit
//! (D-17). Nothing here touches the network, so the engine can be tested on its own.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::model::{Book, CategoryBackground, Goal, GoalId, Priority, Status};

pub const PROTOCOL_VERSION: u32 = 1;

// ---------- Hybrid logical clock ----------

/// Stamps sort as text: 13-digit milliseconds, a 4-digit counter, then the peer ID, so equal
/// times fall to the greater peer ID (REQ-32a).
#[derive(Debug, Default)]
pub struct Clock {
    millis: i64,
    counter: u32,
    /// Shifts this device's wall clock, so tests can simulate devices whose clocks disagree.
    pub skew_ms: i64,
}

impl Clock {
    pub fn restore(saved: &str) -> Self {
        let mut parts = saved.split('-');
        let millis = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        let counter = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        Self { millis, counter, skew_ms: 0 }
    }

    pub fn saved(&self) -> String {
        format!("{}-{}", self.millis, self.counter)
    }

    pub fn tick(&mut self, peer: &str) -> String {
        let now = chrono::Utc::now().timestamp_millis() + self.skew_ms;
        if now > self.millis {
            self.millis = now;
            self.counter = 0;
        } else {
            self.counter += 1;
            if self.counter > 9999 {
                self.millis += 1;
                self.counter = 0;
            }
        }
        format!("{:013}-{:04}-{peer}", self.millis, self.counter)
    }

    /// Moves past a stamp from another device, so this device's next stamp is later than it.
    pub fn observe(&mut self, stamp: &str) {
        let mut parts = stamp.splitn(3, '-');
        let (Some(Ok(millis)), Some(Ok(counter))) = (parts.next().map(str::parse::<i64>), parts.next().map(str::parse::<u32>)) else { return };
        if (millis, counter) > (self.millis, self.counter) {
            self.millis = millis;
            self.counter = counter;
        }
    }
}

/// The device a stamp came from.
pub fn origin_of(stamp: &str) -> &str {
    stamp.splitn(3, '-').nth(2).unwrap_or("")
}

// ---------- Position keys ----------

const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

fn digit(c: u8) -> usize {
    DIGITS.iter().position(|d| *d == c).unwrap_or(0)
}

/// A key that sorts strictly between `a` and `b`, where an empty `a` means the start and no `b`
/// means the end. Keys never end in "0", so there is always room between two of them (P5).
pub fn key_between(a: &str, b: Option<&str>) -> String {
    let (a, b) = (a.as_bytes(), b.map(str::as_bytes));
    String::from_utf8(midpoint(a, b)).expect("keys are ASCII")
}

fn midpoint(a: &[u8], b: Option<&[u8]>) -> Vec<u8> {
    if let Some(b) = b {
        let mut n = 0;
        while n < b.len() && a.get(n).copied().unwrap_or(b'0') == b[n] {
            n += 1;
        }
        if n > 0 {
            let mut out = b[..n].to_vec();
            out.extend(midpoint(a.get(n..).unwrap_or(&[]), Some(&b[n..])));
            return out;
        }
    }
    let digit_a = a.first().map_or(0, |c| digit(*c));
    let digit_b = b.and_then(|b| b.first()).map_or(DIGITS.len(), |c| digit(*c));
    if digit_b - digit_a > 1 {
        vec![DIGITS[(digit_a + digit_b + 1) / 2]]
    } else if let Some(b) = b.filter(|b| b.len() > 1) {
        vec![b[0]]
    } else {
        let mut out = vec![DIGITS[digit_a]];
        out.extend(midpoint(a.get(1..).unwrap_or(&[]), None));
        out
    }
}

/// The key for a new book: after every key made earlier on any device, ordered within one
/// moment by its manual-order position, then made unique.
pub fn new_sort_key(order: i64) -> String {
    let unique = &uuid::Uuid::new_v4().simple().to_string()[..6];
    format!("t{:013}{:010}{unique}V", chrono::Utc::now().timestamp_millis(), order.max(0))
}

/// The key an existing library's books get on upgrade, keeping their order.
pub fn legacy_sort_key(position: i64) -> String {
    format!("a{:010}V", position.max(0))
}

// ---------- What travels ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnitChange {
    pub entity: String,
    pub id: String,
    pub field: String,
    pub value: Value,
    pub stamp: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tombstone {
    pub entity: String,
    pub id: String,
    pub stamp: String,
}

pub type Vector = BTreeMap<String, String>;

/// One side's half of a session: everything the other side lacks, and how far it is complete.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Delta {
    pub vector: Vector,
    pub units: Vec<UnitChange>,
    pub tombstones: Vec<Tombstone>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ApplyReport {
    pub units_applied: usize,
    pub deleted: usize,
    /// Background files the merged library refers to. The caller requests any it lacks.
    pub images: Vec<String>,
}

/// A device this one is paired with (REQ-20e). `acked` is the newest vector it reported,
/// which shows the tombstones it has seen (REQ-32e).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PairedPeer {
    pub id: String,
    pub name: String,
    /// SHA-256 of the device's certificate, as lowercase hex.
    pub fingerprint: String,
    pub addresses: Vec<String>,
    pub last_session: Option<String>,
    pub acked: Vector,
}

pub const BOOK: &str = "book";
pub const CATEGORY: &str = "category";
pub const SUB_CATEGORY: &str = "sub_category";
pub const GOAL: &str = "goal";

/// The order entities are applied in, so a book never names a Category not yet created.
fn entity_rank(entity: &str) -> u8 {
    match entity {
        CATEGORY => 0,
        SUB_CATEGORY => 1,
        BOOK => 2,
        _ => 3,
    }
}

fn opt<T: Serialize>(value: &Option<T>) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// A book's units, each a field or a set of fields that must agree.
pub fn book_units(book: &Book) -> Vec<(&'static str, Value)> {
    vec![
        ("title", json!(book.title)),
        ("author", json!(book.author)),
        ("shelf", json!([book.category_id, book.sub_category_id])),
        ("priority", json!(book.priority.as_str())),
        ("status", json!(book.status.as_str())),
        ("pages", json!([book.total_pages, book.current_page])),
        ("dates", json!([book.start_date, book.finish_date])),
        ("rating", opt(&book.rating)),
        ("notes", json!(book.notes)),
        ("isbn", opt(&book.isbn)),
        ("publisher", json!(book.publisher)),
        ("year", opt(&book.published_year)),
        ("language", json!(book.language)),
        ("background", opt(&book.background_file)),
        ("position", json!(book.sort_key)),
        ("added", json!(book.added_at)),
    ]
}

/// Keeps everything from `current`, the stored book, except the units the Reader changed
/// between `original`, what the editor opened, and `edited`. Saving an editor that was open
/// while a phone synced then keeps the phone's changes to other fields (REQ-32b).
pub fn rebase(original: &Book, edited: &Book, current: &Book) -> Book {
    let before = book_units(original);
    let mut out = current.clone();
    for (field, value) in book_units(edited) {
        if before.iter().find(|(name, _)| *name == field).map(|(_, old)| old) != Some(&value) {
            set_book_unit(&mut out, field, &value);
        }
    }
    out
}

fn set_book_unit(book: &mut Book, field: &str, value: &Value) {
    let text = |v: &Value| v.as_str().map(str::to_string);
    let pair = |v: &Value| (v.get(0).cloned().unwrap_or(Value::Null), v.get(1).cloned().unwrap_or(Value::Null));
    match field {
        "title" => book.title = text(value).unwrap_or_default(),
        "author" => book.author = text(value).unwrap_or_default(),
        "shelf" => {
            let (category, sub) = pair(value);
            book.category_id = text(&category);
            book.sub_category_id = text(&sub);
        }
        "priority" => book.priority = value.as_str().and_then(Priority::parse).unwrap_or(Priority::Medium),
        "status" => book.status = value.as_str().and_then(Status::parse).unwrap_or(Status::ToRead),
        "pages" => {
            let (total, current) = pair(value);
            book.total_pages = total.as_u64().map(|v| v as u32);
            book.current_page = current.as_u64().unwrap_or(0) as u32;
        }
        "dates" => {
            let (start, finish) = pair(value);
            book.start_date = text(&start);
            book.finish_date = text(&finish);
        }
        "rating" => book.rating = value.as_u64().map(|v| v as u8),
        "notes" => book.notes = text(value).unwrap_or_default(),
        "isbn" => book.isbn = text(value),
        "publisher" => book.publisher = text(value).unwrap_or_default(),
        "year" => book.published_year = value.as_i64().map(|v| v as i32),
        "language" => book.language = text(value).unwrap_or_default(),
        "background" => book.background_file = text(value),
        "position" => book.sort_key = text(value).unwrap_or_default(),
        "added" => book.added_at = text(value).unwrap_or_default(),
        _ => {}
    }
}

/// Repairs what field-level merging can't rule out, the same way on every device, so the
/// result stays identical everywhere: a total below the current page, or a finish before the
/// start, from a constraint the schema enforces.
fn repair_book(book: &mut Book) {
    if let Some(total) = book.total_pages {
        if total == 0 {
            book.total_pages = None;
        } else if book.current_page > total {
            book.current_page = total;
        }
    }
    if let (Some(start), Some(finish)) = (&book.start_date, &book.finish_date) {
        if finish < start {
            book.finish_date = None;
        }
    }
    if book.title.trim().is_empty() {
        book.title = "Untitled".into();
    }
    if !(1..=5).contains(&book.rating.unwrap_or(1)) {
        book.rating = None;
    }
}

pub struct CategoryRow {
    pub name: String,
    pub background: Option<String>,
    pub position: i64,
}

pub fn category_units(category: &CategoryRow) -> Vec<(&'static str, Value)> {
    vec![("name", json!(category.name)), ("background", opt(&category.background)), ("position", json!(category.position))]
}

pub struct SubRow {
    pub category_id: String,
    pub name: String,
    pub position: i64,
}

pub fn sub_units(sub: &SubRow) -> Vec<(&'static str, Value)> {
    vec![("parent", json!(sub.category_id)), ("name", json!(sub.name)), ("position", json!(sub.position))]
}

pub fn goal_units(goal: &Goal) -> Vec<(&'static str, Value)> {
    vec![("target", json!(goal.target)), ("active", json!(goal.active))]
}

// ---------- Reading rows back, inside a transaction ----------

pub(crate) fn read_book(connection: &Connection, id: &str) -> rusqlite::Result<Option<Book>> {
    connection.query_row("SELECT * FROM books WHERE id = ?1", [id], crate::store::book_from_row).optional()
}

pub(crate) fn read_category(connection: &Connection, id: &str) -> rusqlite::Result<Option<CategoryRow>> {
    connection
        .query_row("SELECT name, background, position FROM categories WHERE id = ?1", [id], |row| {
            Ok(CategoryRow { name: row.get(0)?, background: row.get(1)?, position: row.get(2)? })
        })
        .optional()
}

pub(crate) fn read_sub(connection: &Connection, id: &str) -> rusqlite::Result<Option<SubRow>> {
    connection
        .query_row("SELECT category_id, name, position FROM sub_categories WHERE id = ?1", [id], |row| {
            Ok(SubRow { category_id: row.get(0)?, name: row.get(1)?, position: row.get(2)? })
        })
        .optional()
}

pub(crate) fn read_goal(connection: &Connection, id: &str) -> rusqlite::Result<Option<Goal>> {
    let Some(goal_id) = GoalId::parse(id) else { return Ok(None) };
    connection
        .query_row("SELECT target, active FROM goals WHERE id = ?1", [id], |row| Ok(Goal { id: goal_id, target: row.get(0)?, active: row.get(1)? }))
        .optional()
}

fn units_of(connection: &Connection, entity: &str, id: &str) -> rusqlite::Result<Option<HashMap<&'static str, Value>>> {
    let units = match entity {
        BOOK => read_book(connection, id)?.map(|book| book_units(&book)),
        CATEGORY => read_category(connection, id)?.map(|row| category_units(&row)),
        SUB_CATEGORY => read_sub(connection, id)?.map(|row| sub_units(&row)),
        GOAL => read_goal(connection, id)?.map(|goal| goal_units(&goal)),
        _ => None,
    };
    Ok(units.map(|list| list.into_iter().collect()))
}

// ---------- Stamping local writes ----------

/// The device's identity and clock, which the store carries.
pub struct Identity {
    pub peer: String,
    pub clock: RefCell<Clock>,
}

impl Identity {
    /// Records new stamps for the given units of one item, and advances this device's own
    /// entry in its version vector.
    pub(crate) fn stamp(&self, tx: &Transaction, entity: &str, id: &str, fields: &[&str]) -> rusqlite::Result<()> {
        if fields.is_empty() {
            return Ok(());
        }
        let stamp = self.clock.borrow_mut().tick(&self.peer);
        for field in fields {
            tx.execute(
                "INSERT INTO field_stamps (entity, id, field, stamp, origin) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(entity, id, field) DO UPDATE SET stamp = excluded.stamp, origin = excluded.origin",
                params![entity, id, field, stamp, self.peer],
            )?;
        }
        self.advance(tx, &stamp)
    }

    /// Records that this device deleted an item. The deletion travels; the stamps no longer do.
    pub(crate) fn tombstone(&self, tx: &Transaction, entity: &str, id: &str) -> rusqlite::Result<()> {
        let stamp = self.clock.borrow_mut().tick(&self.peer);
        tx.execute("DELETE FROM field_stamps WHERE entity = ?1 AND id = ?2", params![entity, id])?;
        tx.execute(
            "INSERT INTO tombstones (entity, id, stamp, origin) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(entity, id) DO NOTHING",
            params![entity, id, stamp, self.peer],
        )?;
        self.advance(tx, &stamp)
    }

    fn advance(&self, tx: &Transaction, stamp: &str) -> rusqlite::Result<()> {
        tx.execute(
            "INSERT INTO vectors (origin, stamp) VALUES (?1, ?2) ON CONFLICT(origin) DO UPDATE SET stamp = max(stamp, excluded.stamp)",
            params![self.peer, stamp],
        )?;
        tx.execute("INSERT INTO meta (key, value) VALUES ('clock', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [self.clock.borrow().saved()])?;
        Ok(())
    }
}

/// The units whose values differ between two versions of an item. A new item has every unit.
pub fn changed_fields(old: Option<Vec<(&'static str, Value)>>, new: &[(&'static str, Value)]) -> Vec<&'static str> {
    match old {
        None => new.iter().map(|(field, _)| *field).collect(),
        Some(old) => {
            let old: HashMap<&str, Value> = old.into_iter().collect();
            new.iter().filter(|(field, value)| old.get(field) != Some(value)).map(|(field, _)| *field).collect()
        }
    }
}

// ---------- Sessions ----------

pub fn read_vector(connection: &Connection) -> rusqlite::Result<Vector> {
    let mut statement = connection.prepare_cached("SELECT origin, stamp FROM vectors")?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
    rows.collect()
}

/// Everything this device holds that a peer with `theirs` lacks (REQ-30b).
pub fn delta_since(connection: &Connection, theirs: &Vector) -> rusqlite::Result<Delta> {
    let vector = read_vector(connection)?;
    let mut wanted: Vec<(String, String, String, String)> = Vec::new();
    let mut tombstones = Vec::new();
    for origin in vector.keys() {
        let floor = theirs.get(origin).cloned().unwrap_or_default();
        let mut statement = connection.prepare_cached("SELECT entity, id, field, stamp FROM field_stamps WHERE origin = ?1 AND stamp > ?2")?;
        let rows = statement.query_map(params![origin, floor], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?;
        for row in rows {
            wanted.push(row?);
        }
        let mut statement = connection.prepare_cached("SELECT entity, id, stamp FROM tombstones WHERE origin = ?1 AND stamp > ?2")?;
        let rows = statement.query_map(params![origin, floor], |row| Ok(Tombstone { entity: row.get(0)?, id: row.get(1)?, stamp: row.get(2)? }))?;
        for row in rows {
            tombstones.push(row?);
        }
    }
    let mut cache: HashMap<(String, String), Option<HashMap<&'static str, Value>>> = HashMap::new();
    let mut units = Vec::with_capacity(wanted.len());
    for (entity, id, field, stamp) in wanted {
        let key = (entity.clone(), id.clone());
        if !cache.contains_key(&key) {
            cache.insert(key.clone(), units_of(connection, &entity, &id)?);
        }
        if let Some(value) = cache[&key].as_ref().and_then(|values| values.get(field.as_str())).cloned() {
            units.push(UnitChange { entity, id, field, value, stamp });
        }
    }
    Ok(Delta { vector, units, tombstones })
}

/// Merges a peer's delta in one transaction (REQ-30c). The later stamp wins each unit, and a
/// deletion beats any edit (D-17).
pub fn apply(connection: &Connection, identity: &Identity, delta: &Delta) -> rusqlite::Result<ApplyReport> {
    apply_inner(connection, identity, delta, None)
}

pub(crate) fn apply_inner(connection: &Connection, identity: &Identity, delta: &Delta, fail_after: Option<usize>) -> rusqlite::Result<ApplyReport> {
    let tx = connection.unchecked_transaction()?;
    let mut report = ApplyReport::default();
    let mut touched_books: HashSet<String> = HashSet::new();

    let mut tombstones = delta.tombstones.clone();
    tombstones.sort_by_key(|t| std::cmp::Reverse(entity_rank(&t.entity)));
    for tombstone in &tombstones {
        let fresh = tx.execute(
            "INSERT INTO tombstones (entity, id, stamp, origin) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(entity, id) DO NOTHING",
            params![tombstone.entity, tombstone.id, tombstone.stamp, origin_of(&tombstone.stamp)],
        )?;
        tx.execute("DELETE FROM field_stamps WHERE entity = ?1 AND id = ?2", params![tombstone.entity, tombstone.id])?;
        let table = match tombstone.entity.as_str() {
            BOOK => "books",
            CATEGORY => "categories",
            SUB_CATEGORY => "sub_categories",
            _ => continue,
        };
        // Deleting a Category clears it from books through the schema, as on the device that did it.
        let removed = tx.execute(&format!("DELETE FROM {table} WHERE id = ?1"), [&tombstone.id])?;
        if fresh > 0 || removed > 0 {
            report.deleted += 1;
        }
        identity.clock.borrow_mut().observe(&tombstone.stamp);
    }

    let mut units = delta.units.clone();
    // Parents first, and within an item its parent unit first, so references always resolve.
    units.sort_by(|a, b| entity_rank(&a.entity).cmp(&entity_rank(&b.entity)).then((b.field == "parent").cmp(&(a.field == "parent"))));
    let dead: HashSet<(String, String)> = {
        let mut statement = tx.prepare("SELECT entity, id FROM tombstones")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (count, unit) in units.iter().enumerate() {
        if fail_after == Some(count) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if dead.contains(&(unit.entity.clone(), unit.id.clone())) {
            continue;
        }
        let local: Option<String> = tx
            .query_row("SELECT stamp FROM field_stamps WHERE entity = ?1 AND id = ?2 AND field = ?3", params![unit.entity, unit.id, unit.field], |row| row.get(0))
            .optional()?;
        if local.as_deref().is_some_and(|local| local >= unit.stamp.as_str()) {
            continue;
        }
        let written = write_unit(&tx, unit)?;
        if !written {
            continue;
        }
        tx.execute(
            "INSERT INTO field_stamps (entity, id, field, stamp, origin) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(entity, id, field) DO UPDATE SET stamp = excluded.stamp, origin = excluded.origin",
            params![unit.entity, unit.id, unit.field, unit.stamp, origin_of(&unit.stamp)],
        )?;
        if unit.entity == BOOK {
            touched_books.insert(unit.id.clone());
        }
        identity.clock.borrow_mut().observe(&unit.stamp);
        report.units_applied += 1;
    }

    // References that point at something deleted elsewhere are cleared, the same everywhere.
    for id in &touched_books {
        if let Some(mut book) = read_book(&tx, id)? {
            let before = book.clone();
            resolve_shelf(&tx, &mut book)?;
            repair_book(&mut book);
            if book != before {
                crate::store::write_book(&tx, &book)?;
            }
        }
    }

    for (origin, stamp) in &delta.vector {
        tx.execute(
            "INSERT INTO vectors (origin, stamp) VALUES (?1, ?2) ON CONFLICT(origin) DO UPDATE SET stamp = max(stamp, excluded.stamp)",
            params![origin, stamp],
        )?;
    }
    tx.execute("INSERT INTO meta (key, value) VALUES ('clock', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [identity.clock.borrow().saved()])?;

    let mut statement = tx.prepare(
        "SELECT background_file FROM books WHERE background_file IS NOT NULL
         UNION SELECT substr(background, 6) FROM categories WHERE background LIKE 'file:%'",
    )?;
    report.images = statement.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
    drop(statement);
    tx.commit()?;
    Ok(report)
}

fn resolve_shelf(tx: &Transaction, book: &mut Book) -> rusqlite::Result<()> {
    if let Some(category) = &book.category_id {
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM categories WHERE id = ?1)", [category], |row| row.get(0))?;
        if !exists {
            book.category_id = None;
            book.sub_category_id = None;
        }
    }
    if let Some(sub) = &book.sub_category_id {
        let parent: Option<String> = tx.query_row("SELECT category_id FROM sub_categories WHERE id = ?1", [sub], |row| row.get(0)).optional()?;
        if parent.is_none() || parent != book.category_id {
            book.sub_category_id = None;
        }
    }
    Ok(())
}

/// Writes one unit into its item, creating the item with neutral values when it is new.
/// Returns false when the unit can't apply, such as a Sub-category whose Category has gone.
fn write_unit(tx: &Transaction, unit: &UnitChange) -> rusqlite::Result<bool> {
    match unit.entity.as_str() {
        BOOK => {
            let mut book = read_book(tx, &unit.id)?.unwrap_or_else(|| Book { id: unit.id.clone(), title: "Untitled".into(), ..Book::new(0) });
            set_book_unit(&mut book, &unit.field, &unit.value);
            resolve_shelf(tx, &mut book)?;
            repair_book(&mut book);
            crate::store::write_book(tx, &book)?;
            Ok(true)
        }
        CATEGORY => {
            let mut row = read_category(tx, &unit.id)?.unwrap_or(CategoryRow { name: "Untitled".into(), background: None, position: 0 });
            match unit.field.as_str() {
                "name" => row.name = unit.value.as_str().filter(|name| !name.trim().is_empty()).unwrap_or("Untitled").to_string(),
                "background" => row.background = unit.value.as_str().filter(|b| CategoryBackground::decode(b).is_some()).map(str::to_string),
                "position" => row.position = unit.value.as_i64().unwrap_or(0),
                _ => return Ok(false),
            }
            tx.execute(
                "INSERT INTO categories (id, name, position, background) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, position = excluded.position, background = excluded.background",
                params![unit.id, row.name, row.position, row.background],
            )?;
            Ok(true)
        }
        SUB_CATEGORY => {
            let existing = read_sub(tx, &unit.id)?;
            let mut row = match existing {
                Some(row) => row,
                None => {
                    // A new Sub-category can only be created from its parent unit.
                    let Some(parent) = unit.value.as_str().filter(|_| unit.field == "parent") else { return Ok(false) };
                    SubRow { category_id: parent.to_string(), name: "Untitled".into(), position: 0 }
                }
            };
            match unit.field.as_str() {
                "parent" => row.category_id = unit.value.as_str().unwrap_or_default().to_string(),
                "name" => row.name = unit.value.as_str().filter(|name| !name.trim().is_empty()).unwrap_or("Untitled").to_string(),
                "position" => row.position = unit.value.as_i64().unwrap_or(0),
                _ => return Ok(false),
            }
            let parent_exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM categories WHERE id = ?1)", [&row.category_id], |r| r.get(0))?;
            if !parent_exists {
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO sub_categories (id, category_id, name, position) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET category_id = excluded.category_id, name = excluded.name, position = excluded.position",
                params![unit.id, row.category_id, row.name, row.position],
            )?;
            Ok(true)
        }
        GOAL => {
            let Some(goal_id) = GoalId::parse(&unit.id) else { return Ok(false) };
            let mut goal = read_goal(tx, &unit.id)?.unwrap_or(Goal { id: goal_id, target: 1, active: true });
            match unit.field.as_str() {
                "target" => goal.target = unit.value.as_u64().filter(|t| *t > 0).unwrap_or(1) as u32,
                "active" => goal.active = unit.value.as_bool().unwrap_or(true),
                _ => return Ok(false),
            }
            tx.execute(
                "INSERT INTO goals (id, target, active) VALUES (?1, ?2, ?3) ON CONFLICT(id) DO UPDATE SET target = excluded.target, active = excluded.active",
                params![goal.id.as_str(), goal.target, goal.active],
            )?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_between_always_sort_between_and_never_end_in_zero() {
        let mut keys = vec![legacy_sort_key(0), legacy_sort_key(1)];
        // Keep inserting just after the first key, the hardest case for running out of room.
        for _ in 0..200 {
            let next = key_between(&keys[0], Some(&keys[1]));
            assert!(keys[0] < next && next < keys[1], "{} < {next} < {}", keys[0], keys[1]);
            assert!(!next.ends_with('0'));
            keys.insert(1, next);
        }
        let end = key_between(keys.last().unwrap(), None);
        assert!(end > *keys.last().unwrap());
        let start = key_between("", Some(&keys[0]));
        assert!(start < keys[0]);
    }

    #[test]
    fn new_keys_follow_upgraded_keys_and_keep_their_order() {
        let old = legacy_sort_key(9_999);
        let first = new_sort_key(10);
        let second = new_sort_key(11);
        assert!(old < first && first < second);
    }

    #[test]
    fn a_clock_never_goes_backwards_and_moves_past_what_it_sees() {
        let mut clock = Clock::default();
        let a = clock.tick("p1");
        let b = clock.tick("p1");
        assert!(a < b);
        clock.observe("9999999999999-0000-p2");
        assert!(clock.tick("p1").starts_with("9999999999999-0001"));
        assert_eq!(origin_of(&a), "p1");
    }

    #[test]
    fn unchanged_units_are_not_restamped() {
        let book = Book { title: "A".into(), ..Book::new(0) };
        let mut edited = book.clone();
        edited.current_page = 0;
        edited.notes = "n".into();
        assert_eq!(changed_fields(Some(book_units(&book)), &book_units(&edited)), ["notes"]);
    }
}
