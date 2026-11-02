//! `.xlsx` import (Section 3.7, Appendix F). Which column means what is the Reader's
//! decision, so rows come back unmapped and the plan is built from the Reader's choices.

use std::collections::{BTreeSet, HashMap};
use std::io::{Cursor, Read, Seek};
use std::path::Path;

use calamine::{Data, Reader, Xlsx};

use crate::isbn;
use crate::model::{new_id, Book, Category, Priority, Status, SubCategory};
use crate::portability::Corrections;
use crate::rules::{apply_page_change, apply_status_change, is_valid_date};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ImportField {
    #[default]
    Ignore,
    Title,
    Author,
    Category,
    Priority,
    Status,
    TotalPages,
    CurrentPage,
    Started,
    Finished,
    Rating,
    Notes,
    Isbn,
    Publisher,
    Year,
    Language,
}

impl ImportField {
    pub const ALL: [ImportField; 16] = [
        ImportField::Ignore,
        ImportField::Title,
        ImportField::Author,
        ImportField::Category,
        ImportField::Priority,
        ImportField::Status,
        ImportField::TotalPages,
        ImportField::CurrentPage,
        ImportField::Started,
        ImportField::Finished,
        ImportField::Rating,
        ImportField::Notes,
        ImportField::Isbn,
        ImportField::Publisher,
        ImportField::Year,
        ImportField::Language,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ImportField::Ignore => "Ignore",
            ImportField::Title => "Title",
            ImportField::Author => "Author",
            ImportField::Category => "Category / Sub-category",
            ImportField::Priority => "Priority",
            ImportField::Status => "Status",
            ImportField::TotalPages => "Total pages",
            ImportField::CurrentPage => "Current page",
            ImportField::Started => "Started",
            ImportField::Finished => "Finished",
            ImportField::Rating => "Rating",
            ImportField::Notes => "Notes",
            ImportField::Isbn => "ISBN",
            ImportField::Publisher => "Publisher",
            ImportField::Year => "Year",
            ImportField::Language => "Language",
        }
    }
}

/// One spreadsheet cell, keeping the distinction between typed dates and text (REQ-40b).
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Empty,
    Text(String),
    Number(f64),
    /// A typed date cell, as `yyyy-mm-dd`.
    Date(String),
}

impl Cell {
    /// The cell as plain text. Whole numbers lose their decimal point, so a numeric ISBN
    /// cell reads as digits.
    pub fn text(&self) -> String {
        match self {
            Cell::Empty => String::new(),
            Cell::Text(text) => text.clone(),
            Cell::Number(number) if number.fract() == 0.0 && number.abs() < 1e15 => format!("{}", *number as i64),
            Cell::Number(number) => number.to_string(),
            Cell::Date(date) => date.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sheet {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SheetError {
    #[error("That file could not be read as an .xlsx spreadsheet.")]
    Unreadable,
    #[error("That workbook contains no sheets.")]
    NoSheets,
    #[error("The first sheet has no column headers in its first row.")]
    NoHeaders,
}

pub fn read_file(path: &Path) -> Result<Sheet, SheetError> {
    let bytes = std::fs::read(path).map_err(|_| SheetError::Unreadable)?;
    read_bytes(&bytes)
}

pub fn read_bytes(bytes: &[u8]) -> Result<Sheet, SheetError> {
    read_workbook(Cursor::new(bytes))
}

/// REQ-18: the first sheet, with column headers in the first row.
fn read_workbook<R: Read + Seek>(reader: R) -> Result<Sheet, SheetError> {
    let mut workbook: Xlsx<_> = Xlsx::new(reader).map_err(|_| SheetError::Unreadable)?;
    let name = workbook.sheet_names().first().cloned().ok_or(SheetError::NoSheets)?;
    let range = workbook.worksheet_range(&name).map_err(|_| SheetError::Unreadable)?;

    let mut rows = range.rows();
    let header = rows.next().ok_or(SheetError::NoHeaders)?;
    let columns: Vec<String> = header.iter().map(|cell| convert(cell).text().trim().to_string()).collect();
    if columns.iter().all(String::is_empty) {
        return Err(SheetError::NoHeaders);
    }
    let rows = rows
        .map(|row| row.iter().map(convert).collect::<Vec<_>>())
        .filter(|row: &Vec<Cell>| row.iter().any(|cell| *cell != Cell::Empty))
        .collect();
    Ok(Sheet { columns, rows })
}

fn convert(cell: &Data) -> Cell {
    match cell {
        Data::Empty | Data::Error(_) => Cell::Empty,
        Data::String(text) => {
            if text.trim().is_empty() { Cell::Empty } else { Cell::Text(text.clone()) }
        }
        Data::Int(number) => Cell::Number(*number as f64),
        Data::Float(number) => Cell::Number(*number),
        Data::Bool(value) => Cell::Text(value.to_string()),
        Data::DateTime(date) => match date.as_datetime() {
            Some(date) => Cell::Date(date.format("%Y-%m-%d").to_string()),
            None => Cell::Empty,
        },
        Data::DateTimeIso(date) => Cell::Text(date.get(..10).unwrap_or(date).to_string()),
        Data::DurationIso(text) => Cell::Text(text.clone()),
    }
}

/// `true` when the keyword appears as a whole word.
fn whole_word(header: &str, word: &str) -> bool {
    header.split(|c: char| !c.is_alphanumeric()).any(|part| part == word)
}

/// REQ-18a-i: pre-fills the mapping from the rules in Appendix F. For each column, the first
/// unclaimed row whose keywords match wins, and a field can be claimed only once.
pub fn guess_mapping(columns: &[String]) -> Vec<ImportField> {
    let rules: [(ImportField, fn(&str) -> bool); 15] = [
        (ImportField::Isbn, |h| h.contains("isbn")),
        (ImportField::Author, |h| h.contains("author") || h.contains("writer")),
        (ImportField::Title, |h| h.contains("title") || h.contains("name")),
        (ImportField::Category, |h| ["categor", "topic", "subject", "genre", "domain"].iter().any(|k| h.contains(k))),
        (ImportField::Priority, |h| h.contains("priorit")),
        (ImportField::Status, |h| h.contains("status") || h.contains("state")),
        (ImportField::CurrentPage, |h| ["current page", "pages read", "page read"].iter().any(|k| h.contains(k))),
        (ImportField::TotalPages, |h| ["total page", "page count", "pages", "length"].iter().any(|k| h.contains(k))),
        (ImportField::Started, |h| ["start", "began", "begun"].iter().any(|k| h.contains(k))),
        (ImportField::Finished, |h| ["finish", "complet", "done"].iter().any(|k| h.contains(k)) || whole_word(h, "end")),
        (ImportField::Rating, |h| ["rating", "score", "star"].iter().any(|k| h.contains(k))),
        (ImportField::Notes, |h| ["note", "comment", "remark"].iter().any(|k| h.contains(k))),
        (ImportField::Publisher, |h| h.contains("publisher")),
        (ImportField::Year, |h| whole_word(h, "year") || h.contains("published")),
        (ImportField::Language, |h| h.contains("language")),
    ];

    let mut claimed = Vec::new();
    columns
        .iter()
        .map(|column| {
            let header = column.to_lowercase();
            let hit = rules.iter().find(|(field, matches)| !claimed.contains(field) && matches(&header));
            match hit {
                Some((field, _)) => {
                    claimed.push(*field);
                    *field
                }
                None => ImportField::Ignore,
            }
        })
        .collect()
}

/// REQ-40: the three exact statuses, ignoring case and surrounding space.
pub fn exact_status(raw: &str) -> Option<Status> {
    Status::ALL.into_iter().find(|status| status.as_str().eq_ignore_ascii_case(raw.trim()))
}

/// REQ-40a: every other distinct status value, for the Reader to assign. The web build
/// guessed from patterns, reading "Unread" as Finished and "Not started" as Reading.
pub fn unknown_statuses(sheet: &Sheet, mapping: &[ImportField]) -> Vec<String> {
    let Some(column) = mapping.iter().position(|field| *field == ImportField::Status) else {
        return Vec::new();
    };
    let values: BTreeSet<String> = sheet
        .rows
        .iter()
        .filter_map(|row| row.get(column))
        .map(|cell| cell.text().trim().to_string())
        .filter(|value| !value.is_empty() && exact_status(value).is_none())
        .collect();
    values.into_iter().collect()
}

/// REQ-40d
pub fn parse_priority(raw: &str) -> Priority {
    match raw.trim().chars().next().map(|c| c.to_ascii_lowercase()) {
        Some('h') => Priority::High,
        Some('l') => Priority::Low,
        _ => Priority::Medium,
    }
}

/// REQ-18c, REQ-18c-i: "A / B / C" imports as A and B.
pub fn split_category(raw: &str) -> (String, String) {
    let mut segments = raw.split('/').map(str::trim);
    (segments.next().unwrap_or_default().to_string(), segments.next().unwrap_or_default().to_string())
}

fn positive_int(cell: &Cell) -> Option<u32> {
    let number = match cell {
        Cell::Number(number) => *number,
        Cell::Text(text) => {
            let digits: String = text.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
            digits.parse().ok()?
        }
        _ => return None,
    };
    (number >= 1.0 && number <= f64::from(u32::MAX)).then_some(number as u32)
}

/// REQ-40e
fn rating(cell: &Cell) -> Option<u8> {
    let value = match cell {
        Cell::Number(number) => *number,
        Cell::Text(text) => text.trim().parse().ok()?,
        _ => return None,
    };
    value.is_finite().then(|| value.round().clamp(1.0, 5.0) as u8)
}

/// REQ-40b: typed date cells and `yyyy-mm-dd` text. The web build also guessed at slashed
/// dates, reading 03/04/2025 as March and dropping 25/12/2025.
fn date(cell: &Cell, corrections: &mut Corrections) -> Option<String> {
    match cell {
        Cell::Empty => None,
        Cell::Date(date) => Some(date.clone()),
        Cell::Text(text) if is_valid_date(text.trim()) => Some(text.trim().to_string()),
        _ => {
            corrections.cleared_dates += 1;
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRow {
    pub title: String,
    pub author: String,
    pub category: String,
    pub sub_category: String,
    /// REQ-18b: marked when the row will be skipped.
    pub skipped: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ImportPlan {
    pub books: Vec<Book>,
    /// Categories created or extended by the sheet, with the position each should save at.
    pub categories: Vec<(Category, usize)>,
    /// REQ-18b: the first ten rows as they will be stored.
    pub preview: Vec<PreviewRow>,
    /// REQ-18e, REQ-39d
    pub corrections: Corrections,
}

/// Turns mapped rows into books. `start_order` is the current library size, so imports
/// append after every existing book (REQ-18f).
pub fn build_plan(
    sheet: &Sheet,
    mapping: &[ImportField],
    status_choices: &HashMap<String, Status>,
    existing: &[Category],
    start_order: i64,
) -> ImportPlan {
    let mut plan = ImportPlan::default();
    let mut categories = existing.to_vec();
    let mut touched: Vec<usize> = Vec::new();

    for row in &sheet.rows {
        let cell = |field: ImportField| -> &Cell {
            mapping
                .iter()
                .position(|mapped| *mapped == field)
                .and_then(|column| row.get(column))
                .unwrap_or(&Cell::Empty)
        };
        let title = cell(ImportField::Title).text().trim().to_string();
        let author = cell(ImportField::Author).text().trim().to_string();
        let (category, sub_category) = split_category(&cell(ImportField::Category).text());

        if plan.preview.len() < 10 {
            plan.preview.push(PreviewRow {
                title: title.clone(),
                author: author.clone(),
                category: category.clone(),
                sub_category: sub_category.clone(),
                skipped: title.is_empty(),
            });
        }

        // REQ-18d
        if title.is_empty() {
            plan.corrections.excluded_rows += 1;
            continue;
        }

        let (category_id, sub_category_id) = resolve_category(&mut categories, &mut touched, &category, &sub_category);

        let mut book = Book::new(start_order + plan.books.len() as i64);
        book.title = title;
        book.author = author;
        book.category_id = category_id;
        book.sub_category_id = sub_category_id;
        book.priority = parse_priority(&cell(ImportField::Priority).text());
        book.total_pages = positive_int(cell(ImportField::TotalPages));
        book.rating = rating(cell(ImportField::Rating));
        book.notes = cell(ImportField::Notes).text().trim().to_string();
        book.publisher = cell(ImportField::Publisher).text().trim().to_string();
        book.language = cell(ImportField::Language).text().trim().to_string();
        book.published_year = positive_int(cell(ImportField::Year))
            .map(|year| year as i32)
            .filter(|year| (1000..=9999).contains(year));
        book.start_date = date(cell(ImportField::Started), &mut plan.corrections);
        book.finish_date = date(cell(ImportField::Finished), &mut plan.corrections);

        // BR-03
        if let (Some(start), Some(finish)) = (&book.start_date, &book.finish_date) {
            if finish < start {
                book.finish_date = None;
                plan.corrections.replaced_values += 1;
            }
        }

        // REQ-40c: the row imports with an empty ISBN.
        let raw_isbn = cell(ImportField::Isbn).text();
        if !raw_isbn.trim().is_empty() {
            match isbn::normalize(&raw_isbn) {
                Ok(value) => book.isbn = Some(value),
                Err(_) => plan.corrections.cleared_isbns += 1,
            }
        }

        // REQ-40f
        let current = positive_int(cell(ImportField::CurrentPage)).unwrap_or(0);
        let current = book.total_pages.map_or(current, |total| current.min(total));

        // REQ-18g: routed through the same rules as a hand-entered book. The page rule runs
        // first, so a mapped status of Reading or Finished still has the final say.
        let raw_status = cell(ImportField::Status).text();
        let status = exact_status(&raw_status)
            .or_else(|| status_choices.get(raw_status.trim()).copied())
            .unwrap_or(Status::ToRead);
        apply_page_change(&mut book, current);
        if status != Status::ToRead {
            apply_status_change(&mut book, status);
        }

        plan.books.push(book);
    }

    touched.sort_unstable();
    touched.dedup();
    plan.categories = touched.into_iter().map(|index| (categories[index].clone(), index)).collect();
    plan
}

/// REQ-18c-ii, REQ-18c-iii: reuses a name ignoring case, creating it otherwise.
fn resolve_category(
    categories: &mut Vec<Category>,
    touched: &mut Vec<usize>,
    category: &str,
    sub_category: &str,
) -> (Option<String>, Option<String>) {
    if category.is_empty() {
        return (None, None);
    }
    let same = |a: &str, b: &str| a.trim().to_lowercase() == b.trim().to_lowercase();

    let index = match categories.iter().position(|existing| same(&existing.name, category)) {
        Some(index) => index,
        None => {
            categories.push(Category { id: new_id(), name: category.to_string(), sub_categories: vec![], background: None });
            touched.push(categories.len() - 1);
            categories.len() - 1
        }
    };
    let category_id = categories[index].id.clone();
    if sub_category.is_empty() {
        return (Some(category_id), None);
    }

    let existing_sub = categories[index].sub_categories.iter().find(|sub| same(&sub.name, sub_category)).map(|sub| sub.id.clone());
    let sub_id = match existing_sub {
        Some(id) => id,
        None => {
            let id = new_id();
            categories[index].sub_categories.push(SubCategory { id: id.clone(), name: sub_category.to_string() });
            touched.push(index);
            id
        }
    };
    (Some(category_id), Some(sub_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_xlsxwriter::{ExcelDateTime, Format, Workbook};

    fn text(value: &str) -> Cell {
        Cell::Text(value.into())
    }

    fn columns(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    fn plan(sheet: &Sheet, mapping: &[ImportField]) -> ImportPlan {
        build_plan(sheet, mapping, &HashMap::new(), &[], 0)
    }

    /// The vectors from Appendix F.
    #[test]
    fn appendix_f_category_split() {
        assert_eq!(split_category("Product Management / Strategy"), ("Product Management".into(), "Strategy".into()));
        assert_eq!(split_category("A / B / C"), ("A".into(), "B".into()));
        assert_eq!(split_category("Leadership"), ("Leadership".into(), String::new()));
    }

    #[test]
    fn appendix_f_exact_statuses() {
        assert_eq!(exact_status("Finished"), Some(Status::Finished));
        assert_eq!(exact_status("finished "), Some(Status::Finished));
        assert_eq!(exact_status("TO READ"), Some(Status::ToRead));
        assert_eq!(exact_status("Reading"), Some(Status::Reading));
        for value in ["Unread", "Not started", "Not done", "Read", "Done"] {
            assert_eq!(exact_status(value), None, "{value} must reach the mapping step");
        }
    }

    #[test]
    fn unknown_statuses_are_listed_once_and_sorted() {
        let sheet = Sheet {
            columns: columns(&["Status"]),
            rows: vec![vec![text("Unread")], vec![text("Done")], vec![text("Unread")], vec![text("Reading")]],
        };
        assert_eq!(unknown_statuses(&sheet, &[ImportField::Status]), ["Done", "Unread"]);
    }

    #[test]
    fn the_readers_status_choice_wins_and_unchosen_values_fall_to_to_read() {
        let sheet = Sheet {
            columns: columns(&["Title", "Status"]),
            rows: vec![vec![text("A"), text("Done")], vec![text("B"), text("Unread")]],
        };
        let choices = HashMap::from([("Done".to_string(), Status::Finished)]);
        let result = build_plan(&sheet, &[ImportField::Title, ImportField::Status], &choices, &[], 0);
        assert_eq!(result.books[0].status, Status::Finished);
        assert_eq!(result.books[1].status, Status::ToRead, "REQ-40a pre-selects To Read");
    }

    #[test]
    fn auto_mapping_follows_appendix_f_order_and_claims_once() {
        let mapping = guess_mapping(&columns(&[
            "Author Name", "Book Title", "Topic", "ISBN-13", "Pages Read", "Total Pages", "Priority",
            "Status", "Date Started", "Date Finished", "Stars", "Notes", "Publisher", "Year", "Language", "Mystery",
        ]));
        assert_eq!(mapping, [
            ImportField::Author, ImportField::Title, ImportField::Category, ImportField::Isbn,
            ImportField::CurrentPage, ImportField::TotalPages, ImportField::Priority, ImportField::Status,
            ImportField::Started, ImportField::Finished, ImportField::Rating, ImportField::Notes,
            ImportField::Publisher, ImportField::Year, ImportField::Language, ImportField::Ignore,
        ]);
        // A field is claimed only once, and "end" is matched as a whole word, not in "Vendor".
        let second = guess_mapping(&columns(&["Title", "Name", "Vendor", "End"]));
        assert_eq!(second, [ImportField::Title, ImportField::Ignore, ImportField::Ignore, ImportField::Finished]);
    }

    #[test]
    fn priority_parsing_uses_the_first_letter() {
        assert_eq!(parse_priority("High"), Priority::High);
        assert_eq!(parse_priority("  high priority"), Priority::High);
        assert_eq!(parse_priority("LOW"), Priority::Low);
        assert_eq!(parse_priority("Whenever"), Priority::Medium);
        assert_eq!(parse_priority(""), Priority::Medium);
    }

    #[test]
    fn rows_with_no_title_are_excluded_and_marked_in_the_preview() {
        let sheet = Sheet { columns: columns(&["Title"]), rows: vec![vec![text("A")], vec![Cell::Empty, text("x")]] };
        let result = plan(&sheet, &[ImportField::Title]);
        assert_eq!(result.books.len(), 1);
        assert_eq!(result.corrections.excluded_rows, 1);
        assert!(result.preview[1].skipped);
    }

    #[test]
    fn preview_stops_at_ten_rows() {
        let sheet = Sheet { columns: columns(&["Title"]), rows: (0..25).map(|i| vec![text(&format!("B{i}"))]).collect() };
        let result = plan(&sheet, &[ImportField::Title]);
        assert_eq!((result.preview.len(), result.books.len()), (10, 25));
    }

    #[test]
    fn imports_append_after_existing_books() {
        let sheet = Sheet { columns: columns(&["Title"]), rows: vec![vec![text("A")], vec![text("B")]] };
        let result = build_plan(&sheet, &[ImportField::Title], &HashMap::new(), &[], 96);
        assert_eq!((result.books[0].order, result.books[1].order), (96, 97), "REQ-18f");
    }

    #[test]
    fn categories_are_reused_ignoring_case_and_created_otherwise() {
        let existing = [Category {
            id: "c1".into(),
            name: "Strategy".into(),
            sub_categories: vec![SubCategory { id: "s1".into(), name: "Pricing".into() }],
            background: None,
        }];
        let sheet = Sheet {
            columns: columns(&["Title", "Category"]),
            rows: vec![
                vec![text("A"), text("strategy / PRICING")],
                vec![text("B"), text("Strategy / Positioning")],
                vec![text("C"), text("Дизајн")],
                vec![text("D"), text("Дизајн")],
            ],
        };
        let result = build_plan(&sheet, &[ImportField::Title, ImportField::Category], &HashMap::new(), &existing, 0);
        assert_eq!(result.books[0].category_id.as_deref(), Some("c1"));
        assert_eq!(result.books[0].sub_category_id.as_deref(), Some("s1"));
        assert_eq!(result.books[1].category_id.as_deref(), Some("c1"));
        assert!(result.books[1].sub_category_id.is_some());
        assert_eq!(result.books[2].category_id, result.books[3].category_id);
        assert_eq!(result.categories.len(), 2, "Strategy extended, Дизајн created");
        assert_eq!(result.categories[0].0.sub_categories.len(), 2);
        assert_eq!(result.categories[1].1, 1, "new category saves after existing ones");
    }

    #[test]
    fn finished_rows_follow_the_hand_entry_rules() {
        let sheet = Sheet {
            columns: columns(&["Title", "Status", "Total"]),
            rows: vec![vec![text("A"), text("Finished"), Cell::Number(200.0)]],
        };
        let result = plan(&sheet, &[ImportField::Title, ImportField::Status, ImportField::TotalPages]);
        let book = &result.books[0];
        assert_eq!(book.current_page, 200, "REQ-13e");
        assert!(book.finish_date.is_some(), "REQ-13d");
        assert_eq!(book.start_date, None, "REQ-13f");
    }

    #[test]
    fn pages_on_a_to_read_row_start_the_book() {
        let sheet = Sheet {
            columns: columns(&["Title", "Current"]),
            rows: vec![vec![text("A"), Cell::Number(10.0)]],
        };
        let result = plan(&sheet, &[ImportField::Title, ImportField::CurrentPage]);
        assert_eq!(result.books[0].status, Status::Reading, "REQ-11b");
    }

    #[test]
    fn current_page_is_reduced_to_the_total() {
        let sheet = Sheet {
            columns: columns(&["Title", "Current", "Total"]),
            rows: vec![vec![text("A"), Cell::Number(500.0), Cell::Number(100.0)]],
        };
        let result = plan(&sheet, &[ImportField::Title, ImportField::CurrentPage, ImportField::TotalPages]);
        assert_eq!(result.books[0].current_page, 100, "REQ-40f");
    }

    #[test]
    fn ratings_round_and_clamp_and_non_numbers_stay_empty() {
        assert_eq!(rating(&Cell::Number(4.6)), Some(5));
        assert_eq!(rating(&Cell::Number(9.0)), Some(5));
        assert_eq!(rating(&Cell::Number(0.0)), Some(1));
        assert_eq!(rating(&text("3")), Some(3));
        assert_eq!(rating(&text("great")), None);
    }

    #[test]
    fn dates_accept_typed_cells_and_iso_text_only() {
        let mut corrections = Corrections::default();
        assert_eq!(date(&Cell::Date("2026-01-02".into()), &mut corrections).as_deref(), Some("2026-01-02"));
        assert_eq!(date(&text(" 2026-01-02 "), &mut corrections).as_deref(), Some("2026-01-02"));
        assert_eq!(corrections.cleared_dates, 0);
        assert_eq!(date(&text("03/04/2025"), &mut corrections), None);
        assert_eq!(date(&text("25/12/2025"), &mut corrections), None);
        assert_eq!(date(&Cell::Number(45000.0), &mut corrections), None);
        assert_eq!(corrections.cleared_dates, 3, "REQ-40b");
    }

    #[test]
    fn a_failed_isbn_imports_the_row_with_an_empty_isbn() {
        let sheet = Sheet {
            columns: columns(&["Title", "ISBN"]),
            rows: vec![vec![text("A"), text("9780140328722")], vec![text("B"), Cell::Number(9780140328721.0)]],
        };
        let result = plan(&sheet, &[ImportField::Title, ImportField::Isbn]);
        assert_eq!(result.books[0].isbn, None);
        assert_eq!(result.corrections.cleared_isbns, 1, "REQ-40c");
        assert_eq!(result.books[1].isbn.as_deref(), Some("9780140328721"), "numeric cells read as digits");
    }

    #[test]
    fn a_real_workbook_reads_with_typed_dates_and_cyrillic() {
        let mut workbook = Workbook::new();
        let worksheet = workbook.add_worksheet();
        for (column, name) in ["Title", "Pages", "Started"].iter().enumerate() {
            worksheet.write_string(0, column as u16, *name).unwrap();
        }
        worksheet.write_string(1, 0, "Ѓердан од ќерамиди").unwrap();
        worksheet.write_number(1, 1, 320).unwrap();
        let format = Format::new().set_num_format("yyyy-mm-dd");
        let date = ExcelDateTime::parse_from_str("2026-03-14").unwrap();
        worksheet.write_datetime_with_format(1, 2, &date, &format).unwrap();
        // A blank row in the middle must not become a book or an exclusion.
        worksheet.write_string(3, 0, "After a gap").unwrap();
        let bytes = workbook.save_to_buffer().unwrap();

        let sheet = read_bytes(&bytes).unwrap();
        assert_eq!(sheet.columns, ["Title", "Pages", "Started"]);
        assert_eq!(sheet.rows.len(), 2);
        assert_eq!(sheet.rows[0][0], text("Ѓердан од ќерамиди"));
        assert_eq!(sheet.rows[0][1], Cell::Number(320.0));
        assert_eq!(sheet.rows[0][2], Cell::Date("2026-03-14".into()));
    }

    #[test]
    fn garbage_is_reported_as_unreadable() {
        assert_eq!(read_bytes(b"not a workbook").unwrap_err(), SheetError::Unreadable);
    }

    #[test]
    fn five_hundred_rows_plan_inside_the_budget() {
        let sheet = Sheet {
            columns: columns(&["Title", "Category"]),
            rows: (0..500).map(|i| vec![text(&format!("Book {i}")), text("Strategy / Pricing")]).collect(),
        };
        let start = std::time::Instant::now();
        let result = plan(&sheet, &[ImportField::Title, ImportField::Category]);
        assert_eq!(result.books.len(), 500);
        assert!(start.elapsed().as_secs() < 10, "QA-04");
    }
}
