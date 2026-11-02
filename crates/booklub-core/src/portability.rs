//! JSON export and import (Section 3.7, Appendix B).
//!
//! An import is untrusted input. Each record is rebuilt field by field, so a file can
//! never plant a value the rules forbid, and each repair is counted for the Reader
//! (REQ-39d).

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::background::is_preset;
use crate::collate::compare_text;
use crate::isbn;
use crate::model::{default_goals, new_id, now_iso, Book, Category, CategoryBackground, Goal, GoalId, Library, Priority, Status, SubCategory};
use crate::query::renumber;
use crate::rules::is_valid_date;

pub const EXPORT_SCHEMA_VERSION: i64 = 3;

/// REQ-17 to REQ-17d: the export document.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub schema_version: i64,
    pub exported_at: String,
    pub categories: Vec<Category>,
    pub goals: Vec<Goal>,
    pub books: Vec<Book>,
}

pub fn export_json(library: &Library) -> String {
    let snapshot = Snapshot {
        schema_version: EXPORT_SCHEMA_VERSION,
        exported_at: now_iso(),
        categories: library.categories.clone(),
        goals: library.goals.clone(),
        books: library.books.clone(),
    };
    serde_json::to_string_pretty(&snapshot).expect("the model always serialises")
}

/// REQ-17: the default file name, in local time.
pub fn default_file_name(extension: &str) -> String {
    format!("booklub-{}.{extension}", crate::rules::today())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    #[error("That file is not valid JSON.")]
    NotJson,
    #[error("That file does not contain a Booklub library.")]
    NotLibrary,
    /// REQ-19b, worded as in Appendix F.
    #[error("That file uses schema version {0}. Booklub accepts versions 1, 2, and 3.")]
    UnknownSchema(String),
}

/// REQ-39d: what had to be repaired on the way in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Corrections {
    /// REQ-18d, REQ-39: records with no title.
    pub excluded_rows: usize,
    /// REQ-39a: references to an absent Category or Sub-category.
    pub cleared_references: usize,
    /// REQ-39e: values that broke a business rule and took their default.
    pub replaced_values: usize,
    /// REQ-37d: references to a background file that is absent.
    pub cleared_backgrounds: usize,
    /// REQ-40b: date values in no accepted form.
    pub cleared_dates: usize,
    /// REQ-40c: ISBNs that failed the check digit.
    pub cleared_isbns: usize,
}

impl Corrections {
    pub fn total(&self) -> usize {
        self.excluded_rows + self.cleared_references + self.replaced_values + self.cleared_backgrounds + self.cleared_dates + self.cleared_isbns
    }
}

#[derive(Debug, Clone)]
pub struct ParsedImport {
    pub schema_version: i64,
    pub exported_at: String,
    pub library: Library,
    pub corrections: Corrections,
}

fn text(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_string()
}

fn optional_id(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).filter(|id| !id.is_empty()).map(str::to_string)
}

/// Parses an export at schema version 1, 2, or 3 (REQ-19, REQ-38).
pub fn parse_import(json: &str) -> Result<ParsedImport, ImportError> {
    let raw: Value = serde_json::from_str(json).map_err(|_| ImportError::NotJson)?;
    let object = raw.as_object().ok_or(ImportError::NotLibrary)?;

    let version = match object.get("schemaVersion") {
        Some(Value::Number(number)) if number.as_i64().is_some() => number.as_i64().unwrap_or_default(),
        Some(other) => return Err(ImportError::UnknownSchema(other.to_string())),
        None => return Err(ImportError::UnknownSchema("unknown".into())),
    };
    if !(1..=EXPORT_SCHEMA_VERSION).contains(&version) {
        return Err(ImportError::UnknownSchema(version.to_string()));
    }

    let raw_books = object.get("books").and_then(Value::as_array).ok_or(ImportError::NotLibrary)?;
    let raw_categories = object.get("categories").and_then(Value::as_array).ok_or(ImportError::NotLibrary)?;

    let mut corrections = Corrections::default();
    let mut categories: Vec<Category> = raw_categories.iter().filter_map(|raw| sanitise_category(raw, &mut corrections)).collect();
    // REQ-38b: v1 stored categories in no meaningful order.
    if version == 1 {
        categories.sort_by(|a, b| compare_text(&a.name, &b.name));
    }

    let mut goals: Vec<Goal> = Vec::new();
    for goal in object.get("goals").and_then(Value::as_array).into_iter().flatten() {
        if let Some(goal) = sanitise_goal(goal) {
            if !goals.iter().any(|existing| existing.id == goal.id) {
                goals.push(goal);
            }
        }
    }
    for default in default_goals() {
        if !goals.iter().any(|goal| goal.id == default.id) {
            goals.push(default);
        }
    }
    goals.sort_by_key(|goal| match goal.id {
        GoalId::BooksPerYear => 0,
        GoalId::BooksPerMonth => 1,
    });

    let mut seen = HashSet::new();
    let mut books = Vec::with_capacity(raw_books.len());
    for (position, raw) in raw_books.iter().enumerate() {
        match sanitise_book(raw, position as i64, &mut corrections) {
            Some(mut book) => {
                // A repeated id would silently overwrite the earlier record on save.
                if !seen.insert(book.id.clone()) {
                    book.id = new_id();
                    corrections.replaced_values += 1;
                }
                books.push(book);
            }
            None => corrections.excluded_rows += 1,
        }
    }

    // REQ-38, REQ-19f: stored order is what the Reader arranged, gaps or not.
    books.sort_by_key(|book| book.order);
    renumber(&mut books);
    clear_dangling_references(&mut books, &categories, &mut corrections);

    Ok(ParsedImport {
        schema_version: version,
        exported_at: text(object.get("exportedAt")),
        library: Library { books, categories, goals },
        corrections,
    })
}

fn sanitise_category(raw: &Value, corrections: &mut Corrections) -> Option<Category> {
    let object = raw.as_object()?;
    let name = text(object.get("name")).trim().to_string();
    if name.is_empty() {
        return None;
    }
    let sub_categories = object
        .get("subCategories")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|sub| {
            let sub = sub.as_object()?;
            let name = text(sub.get("name")).trim().to_string();
            if name.is_empty() {
                return None;
            }
            Some(SubCategory { id: optional_id(sub.get("id")).unwrap_or_else(new_id), name })
        })
        .collect();
    let background = match object.get("background") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let chosen = serde_json::from_value::<CategoryBackground>(value.clone()).ok().filter(is_safe_background);
            if chosen.is_none() {
                corrections.replaced_values += 1;
            }
            chosen
        }
    };
    Some(Category { id: optional_id(object.get("id")).unwrap_or_else(new_id), name, sub_categories, background })
}

/// A preset must be one that exists, and a file must be a bare name that cannot leave the store.
fn is_safe_background(background: &CategoryBackground) -> bool {
    match background {
        CategoryBackground::Preset(id) => is_preset(id),
        CategoryBackground::File(name) => is_safe_file_name(name),
    }
}

fn is_safe_file_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && !name.contains('\\') && !name.starts_with('.')
}

fn sanitise_goal(raw: &Value) -> Option<Goal> {
    let object = raw.as_object()?;
    let id = GoalId::parse(object.get("id")?.as_str()?)?;
    let target = u32::try_from(object.get("target")?.as_u64()?).ok().filter(|t| *t > 0)?;
    Some(Goal { id, target, active: object.get("active").and_then(Value::as_bool).unwrap_or(true) })
}

/// Rebuilds a book from untrusted JSON. `None` means the record has no usable title.
fn sanitise_book(raw: &Value, position: i64, corrections: &mut Corrections) -> Option<Book> {
    let object = raw.as_object()?;
    let title = text(object.get("title"));
    if title.trim().is_empty() {
        return None;
    }

    let mut book = Book::new(position);
    book.title = title;
    book.author = text(object.get("author"));
    book.notes = text(object.get("notes"));
    book.publisher = text(object.get("publisher"));
    book.language = text(object.get("language"));
    if let Some(id) = optional_id(object.get("id")) {
        book.id = id;
    }
    book.category_id = optional_id(object.get("categoryId"));
    book.sub_category_id = optional_id(object.get("subCategoryId"));
    if let Some(order) = object.get("order").and_then(Value::as_f64) {
        book.order = order as i64;
    }
    if let Some(added) = object.get("addedAt").and_then(Value::as_str) {
        book.added_at = added.to_string();
    }
    // A background must be a bare file name; anything else could point outside the store.
    book.background_file = optional_id(object.get("backgroundFile")).filter(|name| is_safe_file_name(name));

    // REQ-39e: BR-04 and BR-05.
    match object.get("priority").and_then(Value::as_str).and_then(Priority::parse) {
        Some(priority) => book.priority = priority,
        None if object.contains_key("priority") => corrections.replaced_values += 1,
        None => {}
    }
    match object.get("status").and_then(Value::as_str).and_then(Status::parse) {
        Some(status) => book.status = status,
        None if object.contains_key("status") => corrections.replaced_values += 1,
        None => {}
    }

    match object.get("totalPages") {
        None | Some(Value::Null) => {}
        Some(value) => match value.as_f64().filter(|pages| pages.is_finite() && *pages >= 1.0 && *pages <= f64::from(u32::MAX)) {
            Some(pages) => book.total_pages = Some(pages as u32),
            None => corrections.replaced_values += 1,
        },
    }

    // BR-02: REQ-39e gives an out-of-range value its default of zero.
    match object.get("currentPage") {
        None | Some(Value::Null) => {}
        Some(value) => match value.as_f64().filter(|page| page.is_finite() && *page >= 0.0 && *page <= f64::from(u32::MAX)) {
            Some(page) if book.total_pages.is_none_or(|total| page as u32 <= total) => book.current_page = page as u32,
            _ => corrections.replaced_values += 1,
        },
    }

    book.start_date = sanitise_date(object.get("startDate"), corrections);
    book.finish_date = sanitise_date(object.get("finishDate"), corrections);
    // BR-03
    if let (Some(start), Some(finish)) = (&book.start_date, &book.finish_date) {
        if finish < start {
            book.finish_date = None;
            corrections.replaced_values += 1;
        }
    }

    match object.get("rating") {
        None | Some(Value::Null) => {}
        Some(value) => match value.as_f64().filter(|rating| (1.0..=5.0).contains(rating)) {
            Some(rating) => book.rating = Some(rating.round() as u8),
            None => corrections.replaced_values += 1,
        },
    }

    // BR-09
    if let Some(value) = object.get("isbn").and_then(Value::as_str).filter(|v| !v.is_empty()) {
        match isbn::normalize(value) {
            Ok(normalised) => book.isbn = Some(normalised),
            Err(_) => corrections.replaced_values += 1,
        }
    }

    match object.get("publishedYear") {
        None | Some(Value::Null) => {}
        Some(value) => match value.as_i64().filter(|year| (1000..=9999).contains(year)) {
            Some(year) => book.published_year = Some(year as i32),
            None => corrections.replaced_values += 1,
        },
    }

    Some(book)
}

fn sanitise_date(value: Option<&Value>, corrections: &mut Corrections) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::String(date) if date.is_empty() => None,
        Value::String(date) if is_valid_date(date) => Some(date.clone()),
        _ => {
            corrections.replaced_values += 1;
            None
        }
    }
}

/// REQ-39a: clears a reference to an absent Category or Sub-category, and a Sub-category
/// that does not belong to the book's Category (BR-06).
fn clear_dangling_references(books: &mut [Book], categories: &[Category], corrections: &mut Corrections) {
    let mut parents: HashMap<&str, &str> = HashMap::new();
    let mut known: HashSet<&str> = HashSet::new();
    for category in categories {
        known.insert(&category.id);
        for sub in &category.sub_categories {
            parents.insert(&sub.id, &category.id);
        }
    }
    for book in books {
        if book.category_id.as_deref().is_some_and(|id| !known.contains(id)) {
            book.category_id = None;
            corrections.cleared_references += 1;
        }
        if let Some(sub) = book.sub_category_id.as_deref() {
            let parent = parents.get(sub).copied();
            if parent.is_none() || parent != book.category_id.as_deref() {
                book.sub_category_id = None;
                corrections.cleared_references += 1;
            }
        }
    }
}

/// What a Merge will write, in one transaction by the caller (REQ-39c).
#[derive(Debug, Clone, Default)]
pub struct MergePlan {
    /// REQ-19d: books whose IDs are absent, positioned after the existing library.
    pub books: Vec<Book>,
    /// New categories and known ones extended by REQ-39b, with their positions.
    pub categories: Vec<(Category, usize)>,
    pub corrections: Corrections,
}

/// REQ-19d, REQ-19e, REQ-39b. Existing books and goals are left untouched.
pub fn plan_merge(current: &Library, incoming: &ParsedImport) -> MergePlan {
    let mut corrections = incoming.corrections;
    let mut merged = current.categories.clone();
    let mut touched: Vec<usize> = Vec::new();

    for category in &incoming.library.categories {
        match merged.iter().position(|existing| existing.id == category.id) {
            Some(index) => {
                let missing: Vec<SubCategory> = category
                    .sub_categories
                    .iter()
                    .filter(|sub| !merged[index].sub_categories.iter().any(|known| known.id == sub.id))
                    .cloned()
                    .collect();
                if !missing.is_empty() {
                    merged[index].sub_categories.extend(missing);
                    touched.push(index);
                }
            }
            None => {
                merged.push(category.clone());
                touched.push(merged.len() - 1);
            }
        }
    }

    let known: HashSet<&str> = current.books.iter().map(|book| book.id.as_str()).collect();
    let start = crate::query::next_position(&current.books);
    let mut books: Vec<Book> = incoming
        .library
        .books
        .iter()
        .filter(|book| !known.contains(book.id.as_str()))
        .cloned()
        .collect();
    for (offset, book) in books.iter_mut().enumerate() {
        book.order = start + offset as i64;
    }
    clear_dangling_references(&mut books, &merged, &mut corrections);

    MergePlan {
        books,
        categories: touched.into_iter().map(|index| (merged[index].clone(), index)).collect(),
        corrections,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The shape the web build writes: no catalogue fields, schema version 1.
    fn v1_file() -> Value {
        json!({
            "schemaVersion": 1,
            "exportedAt": "2026-08-05T10:00:00.000Z",
            "categories": [
                { "id": "c2", "name": "Strategy", "subCategories": [{ "id": "s1", "name": "Pricing" }] },
                { "id": "c1", "name": "Agile", "subCategories": [] }
            ],
            "goals": [
                { "id": "booksPerMonth", "target": 3, "active": false },
                { "id": "booksPerYear", "target": 30, "active": true }
            ],
            "books": [
                { "id": "b2", "title": "Second", "author": "A", "categoryId": "c2", "subCategoryId": "s1",
                  "priority": "High", "status": "Reading", "totalPages": 100, "currentPage": 50,
                  "startDate": "2026-01-01", "finishDate": null, "rating": 4, "notes": "n",
                  "order": 7, "addedAt": "2026-01-01T00:00:00.000Z" },
                { "id": "b1", "title": "Ѓердан од ќерамиди", "author": "Ќирил", "categoryId": null, "subCategoryId": null,
                  "priority": "Low", "status": "To Read", "totalPages": null, "currentPage": 0,
                  "startDate": null, "finishDate": null, "rating": null, "notes": "",
                  "order": 2, "addedAt": "2026-01-02T00:00:00.000Z" }
            ]
        })
    }

    #[test]
    fn a_v1_export_keeps_relative_order_and_gets_empty_catalogue_fields() {
        let parsed = parse_import(&v1_file().to_string()).unwrap();
        let books = &parsed.library.books;
        assert_eq!(books[0].title, "Ѓердан од ќерамиди", "REQ-38");
        assert_eq!(books[1].title, "Second");
        assert_eq!((books[0].order, books[1].order), (0, 1), "REQ-19f");
        assert_eq!(books[1].isbn, None, "REQ-38a");
        assert_eq!(books[1].publisher, "");
        assert_eq!(books[1].background_file, None);
        assert_eq!(books[1].current_page, 50);
        assert_eq!(parsed.corrections, Corrections::default());
    }

    #[test]
    fn v1_categories_sort_alphabetically_and_goals_list_yearly_first() {
        let parsed = parse_import(&v1_file().to_string()).unwrap();
        assert_eq!(parsed.library.categories[0].name, "Agile", "REQ-38b");
        assert_eq!(parsed.library.goals[0].id, GoalId::BooksPerYear);
        assert_eq!(parsed.library.goals[1], Goal { id: GoalId::BooksPerMonth, target: 3, active: false });
    }

    #[test]
    fn unknown_schema_versions_are_rejected_with_the_appendix_f_message() {
        let error = parse_import(r#"{"schemaVersion": 9, "books": [], "categories": []}"#).unwrap_err();
        assert_eq!(error.to_string(), "That file uses schema version 9. Booklub accepts versions 1, 2, and 3.");
    }

    #[test]
    fn non_json_and_non_libraries_are_rejected() {
        assert_eq!(parse_import("nope").unwrap_err(), ImportError::NotJson);
        assert_eq!(parse_import("[]").unwrap_err(), ImportError::NotLibrary);
        assert_eq!(parse_import(r#"{"schemaVersion":1}"#).unwrap_err(), ImportError::NotLibrary);
    }

    #[test]
    fn blank_titles_are_excluded_and_counted() {
        let file = json!({"schemaVersion": 1, "categories": [], "books": [
            { "id": "a", "title": "   " }, { "id": "b", "title": "Kept" }
        ]});
        let parsed = parse_import(&file.to_string()).unwrap();
        assert_eq!(parsed.library.books.len(), 1);
        assert_eq!(parsed.corrections.excluded_rows, 1, "REQ-39");
    }

    #[test]
    fn dangling_and_mismatched_references_are_cleared_and_counted() {
        let file = json!({"schemaVersion": 1, "categories": [
            { "id": "c1", "name": "One", "subCategories": [{ "id": "s1", "name": "S" }] },
            { "id": "c2", "name": "Two", "subCategories": [] }
        ], "books": [
            { "id": "a", "title": "Absent", "categoryId": "gone", "subCategoryId": "gone-too" },
            { "id": "b", "title": "Mismatch", "categoryId": "c2", "subCategoryId": "s1" },
            { "id": "c", "title": "Good", "categoryId": "c1", "subCategoryId": "s1" }
        ]});
        let parsed = parse_import(&file.to_string()).unwrap();
        let book = |id: &str| parsed.library.books.iter().find(|b| b.id == id).unwrap();
        assert_eq!(book("a").category_id, None);
        assert_eq!(book("a").sub_category_id, None);
        assert_eq!(book("b").category_id.as_deref(), Some("c2"));
        assert_eq!(book("b").sub_category_id, None);
        assert_eq!(book("c").sub_category_id.as_deref(), Some("s1"));
        assert_eq!(parsed.corrections.cleared_references, 3, "REQ-39a");
    }

    #[test]
    fn rule_breaking_values_take_their_defaults_and_are_counted() {
        let file = json!({"schemaVersion": 2, "categories": [], "books": [{
            "id": "a", "title": "T", "priority": "Urgent", "status": "Abandoned",
            "totalPages": 100, "currentPage": 150, "startDate": "2026-05-02", "finishDate": "2026-05-01",
            "rating": 9, "isbn": "9780140328722", "publishedYear": 12
        }]});
        let parsed = parse_import(&file.to_string()).unwrap();
        let book = &parsed.library.books[0];
        assert_eq!(book.priority, Priority::Medium);
        assert_eq!(book.status, Status::ToRead);
        assert_eq!(book.current_page, 0);
        assert_eq!(book.finish_date, None);
        assert_eq!(book.start_date.as_deref(), Some("2026-05-02"));
        assert_eq!(book.rating, None);
        assert_eq!(book.isbn, None);
        assert_eq!(book.published_year, None);
        assert_eq!(parsed.corrections.replaced_values, 7, "REQ-39e");
    }

    #[test]
    fn background_paths_cannot_escape_the_store() {
        let file = json!({"schemaVersion": 2, "categories": [], "books": [
            { "id": "a", "title": "T", "backgroundFile": "../../etc/passwd" },
            { "id": "b", "title": "U", "backgroundFile": "ok.jpg" }
        ]});
        let parsed = parse_import(&file.to_string()).unwrap();
        assert_eq!(parsed.library.books[0].background_file, None);
        assert_eq!(parsed.library.books[1].background_file.as_deref(), Some("ok.jpg"));
    }

    #[test]
    fn repeated_ids_do_not_collapse_records() {
        let file = json!({"schemaVersion": 1, "categories": [], "books": [
            { "id": "a", "title": "One" }, { "id": "a", "title": "Two" }
        ]});
        let parsed = parse_import(&file.to_string()).unwrap();
        assert_eq!(parsed.library.books.len(), 2);
        assert_ne!(parsed.library.books[0].id, parsed.library.books[1].id);
    }

    #[test]
    fn export_round_trips_every_attribute_including_cyrillic() {
        let mut book = Book { title: "Ѓердан од ќерамиди".into(), author: "Ќирил".into(), ..Book::new(0) };
        book.isbn = Some("9780140328721".into());
        book.publisher = "Култура".into();
        book.published_year = Some(1999);
        book.language = "Macedonian".into();
        book.rating = Some(5);
        book.notes = "белешка".into();
        book.background_file = Some("x.webp".into());
        let library = Library {
            books: vec![book.clone()],
            categories: vec![Category { id: "c".into(), name: "Дизајн".into(), sub_categories: vec![], background: None }],
            goals: default_goals(),
        };
        let parsed = parse_import(&export_json(&library)).unwrap();
        assert_eq!(parsed.schema_version, 3);
        assert_eq!(parsed.library.books[0], book);
        assert_eq!(parsed.library.categories, library.categories);
        assert_eq!(parsed.library.goals, library.goals);
        assert_eq!(parsed.corrections, Corrections::default());
    }

    #[test]
    fn merge_appends_only_new_books_and_leaves_goals_and_existing_books_alone() {
        let existing = Book { id: "b1".into(), title: "Mine".into(), ..Book::new(0) };
        let current = Library { books: vec![existing], categories: vec![], goals: default_goals() };
        let parsed = parse_import(&v1_file().to_string()).unwrap();
        let plan = plan_merge(&current, &parsed);
        assert_eq!(plan.books.len(), 1, "REQ-19d: b1 is already held");
        assert_eq!(plan.books[0].id, "b2");
        assert_eq!(plan.books[0].order, 1);
        assert_eq!(plan.categories.len(), 2);
    }

    #[test]
    fn merge_adds_missing_sub_categories_to_a_known_category() {
        let current = Library {
            books: vec![],
            categories: vec![Category { id: "c2".into(), name: "Strategy".into(), sub_categories: vec![], background: None }],
            goals: default_goals(),
        };
        let parsed = parse_import(&v1_file().to_string()).unwrap();
        let plan = plan_merge(&current, &parsed);
        let (strategy, position) = plan.categories.iter().find(|(c, _)| c.id == "c2").unwrap();
        assert_eq!(strategy.sub_categories[0].id, "s1", "REQ-39b");
        assert_eq!(*position, 0);
        // The merged book keeps its sub-category instead of dangling.
        assert_eq!(plan.books.iter().find(|b| b.id == "b2").unwrap().sub_category_id.as_deref(), Some("s1"));
    }

    #[test]
    fn category_backgrounds_round_trip_through_json() {
        let library = Library {
            books: vec![],
            categories: vec![
                Category { id: "a".into(), name: "A".into(), sub_categories: vec![], background: Some(CategoryBackground::Preset("ink-wash".into())) },
                Category { id: "b".into(), name: "B".into(), sub_categories: vec![], background: Some(CategoryBackground::File("x.jpg".into())) },
                Category { id: "c".into(), name: "C".into(), sub_categories: vec![], background: None },
            ],
            goals: default_goals(),
        };
        let parsed = parse_import(&export_json(&library)).unwrap();
        assert_eq!(parsed.library.categories, library.categories);
        assert_eq!(parsed.corrections, Corrections::default());
    }

    #[test]
    fn hostile_category_backgrounds_are_cleared_and_counted() {
        let file = json!({"schemaVersion": 3, "books": [], "categories": [
            { "id": "a", "name": "A", "background": { "preset": "not-a-real-one" } },
            { "id": "b", "name": "B", "background": { "file": "../../etc/passwd" } },
            { "id": "c", "name": "C", "background": "nonsense" },
            { "id": "d", "name": "D", "background": { "preset": "ink-wash" } }
        ]});
        let parsed = parse_import(&file.to_string()).unwrap();
        let backgrounds: Vec<_> = parsed.library.categories.iter().map(|c| c.background.clone()).collect();
        assert_eq!(backgrounds, [None, None, None, Some(CategoryBackground::Preset("ink-wash".into()))]);
        assert_eq!(parsed.corrections.replaced_values, 3, "REQ-39e");
    }

    #[test]
    fn merge_never_changes_the_background_of_a_category_already_held() {
        let mine = Some(CategoryBackground::Preset("ink-wash".into()));
        let current = Library {
            books: vec![],
            categories: vec![Category { id: "c".into(), name: "Mine".into(), sub_categories: vec![], background: mine.clone() }],
            goals: default_goals(),
        };
        let file = json!({"schemaVersion": 3, "books": [], "categories": [
            { "id": "c", "name": "Mine", "subCategories": [{ "id": "s", "name": "New" }], "background": { "preset": "dusk-dunes" } },
            { "id": "n", "name": "New", "background": { "preset": "dusk-dunes" } }
        ]});
        let plan = plan_merge(&current, &parse_import(&file.to_string()).unwrap());
        let held = plan.categories.iter().find(|(c, _)| c.id == "c").unwrap();
        assert_eq!(held.0.background, mine);
        assert_eq!(held.0.sub_categories.len(), 1, "REQ-39b still applies");
        assert!(plan.categories.iter().find(|(c, _)| c.id == "n").unwrap().0.background.is_some());
    }

    #[test]
    fn default_file_name_has_the_date() {
        let name = default_file_name("json");
        assert!(name.starts_with("booklub-") && name.ends_with(".json") && name.len() == "booklub-2026-11-01.json".len());
    }
}
