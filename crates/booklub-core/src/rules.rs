//! Validation and the side effects of editing a book (Sections 3.2 and 3.4).

use chrono::NaiveDate;

use crate::isbn;
use crate::model::{Book, Status};

/// Today as a calendar date in the local time zone (REQ-13g).
pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// REQ-12a: completion as a whole-number percentage.
pub fn completion_percent(book: &Book) -> u32 {
    match book.total_pages {
        Some(total) if total > 0 => {
            let ratio = f64::from(book.current_page) / f64::from(total);
            (ratio.clamp(0.0, 1.0) * 100.0).round() as u32
        }
        _ => 0,
    }
}

/// REQ-12, REQ-12c: a book with no page count gets a disabled progress control.
pub fn has_progress(book: &Book) -> bool {
    book.total_pages.is_some_and(|total| total > 0)
}

/// One message per invalid field, worded as in Appendix F.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldErrors {
    pub title: Option<String>,
    pub total_pages: Option<String>,
    pub current_page: Option<String>,
    pub finish_date: Option<String>,
    pub start_date: Option<String>,
    pub rating: Option<String>,
    pub isbn: Option<String>,
    pub published_year: Option<String>,
}

impl FieldErrors {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

pub const MSG_TITLE_REQUIRED: &str = "Title is required.";
pub const MSG_TOTAL_PAGES: &str = "Total pages must be a whole number of 1 or more.";
pub const MSG_FINISH_BEFORE_START: &str = "Finish date cannot be earlier than the start date.";
pub const MSG_INVALID_ISBN: &str = "That ISBN is not valid. Check the digits and try again.";
pub const MSG_NO_PAGE_COUNT: &str = "Record a total page count to track progress through this book.";
pub const MSG_LAST_PAGE: &str = "You are on the last page. Mark this book as finished?";
pub const MSG_NEGATIVE_PAGE: &str = "Current page cannot be negative.";

pub fn page_range_message(total: u32) -> String {
    format!("Enter 0 to {total}.")
}

/// True for a real calendar date written as `yyyy-mm-dd`.
pub fn is_valid_date(value: &str) -> bool {
    value.len() == 10 && NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
}

pub fn validate(book: &Book) -> FieldErrors {
    let mut errors = FieldErrors::default();

    // BR-01, REQ-01a
    if book.title.trim().is_empty() {
        errors.title = Some(MSG_TITLE_REQUIRED.into());
    }

    // REQ-10
    if book.total_pages == Some(0) {
        errors.total_pages = Some(MSG_TOTAL_PAGES.into());
    }

    // BR-02, REQ-11a
    if let Some(total) = book.total_pages {
        if book.current_page > total {
            errors.current_page = Some(page_range_message(total));
        }
    }

    // REQ-44e accepts typed dates, so a malformed one has to be caught here.
    if book.start_date.as_deref().is_some_and(|date| !is_valid_date(date)) {
        errors.start_date = Some("Enter the date as yyyy-mm-dd.".into());
    }
    if book.finish_date.as_deref().is_some_and(|date| !is_valid_date(date)) {
        errors.finish_date = Some("Enter the date as yyyy-mm-dd.".into());
    } else if let (Some(start), Some(finish)) = (&book.start_date, &book.finish_date) {
        // BR-03, REQ-13b. ISO dates order correctly as text.
        if finish < start {
            errors.finish_date = Some(MSG_FINISH_BEFORE_START.into());
        }
    }

    // REQ-02d
    if book.rating.is_some_and(|rating| !(1..=5).contains(&rating)) {
        errors.rating = Some("Rating must be between 1 and 5.".into());
    }

    // BR-09, REQ-36b
    if let Some(value) = &book.isbn {
        if isbn::normalize(value).is_err() {
            errors.isbn = Some(MSG_INVALID_ISBN.into());
        }
    }

    // REQ-36e
    if book.published_year.is_some_and(|year| !(1000..=9999).contains(&year)) {
        errors.published_year = Some("Enter a four-digit year.".into());
    }

    errors
}

/// Applies what a status change implies, so the rules hold however it was set.
///
/// REQ-13c: Reading with no start date takes today's date.
/// REQ-13d: Finished with no finish date takes today's date.
/// REQ-13e: Finished sets the current page to the total.
/// REQ-13f: Finished with no start date leaves it empty. The web build stamped the
/// finish date here, which added zero-day entries to REQ-16f.
pub fn apply_status_change(book: &mut Book, status: Status) {
    book.status = status;
    match status {
        Status::Reading if book.start_date.is_none() => book.start_date = Some(today()),
        Status::Finished => {
            if book.finish_date.is_none() {
                book.finish_date = Some(today());
            }
            if let Some(total) = book.total_pages.filter(|total| *total > 0) {
                book.current_page = total;
            }
        }
        _ => {}
    }
}

/// REQ-11b: recording progress on an unstarted book moves it to Reading.
pub fn apply_page_change(book: &mut Book, current_page: u32) {
    book.current_page = current_page;
    if current_page > 0 && book.status == Status::ToRead {
        apply_status_change(book, Status::Reading);
    }
}

/// REQ-10a: lowering the total below the current page lowers the current page too.
pub fn apply_total_pages_change(book: &mut Book, total_pages: Option<u32>) {
    book.total_pages = total_pages;
    if let Some(total) = total_pages {
        book.current_page = book.current_page.min(total);
    }
}

/// REQ-05h: changing the Category clears the Sub-category.
pub fn apply_category_change(book: &mut Book, category_id: Option<String>) {
    if book.category_id != category_id {
        book.sub_category_id = None;
    }
    book.category_id = category_id;
}

/// REQ-11c: reaching the last page offers Mark Finished rather than assuming it.
pub fn is_at_last_page(book: &Book) -> bool {
    has_progress(book) && book.total_pages == Some(book.current_page) && book.status != Status::Finished
}

/// REQ-01d, REQ-01e: same title and author, after trimming and ignoring case.
pub fn find_duplicate<'a>(book: &Book, books: &'a [Book]) -> Option<&'a Book> {
    let key = |text: &str| text.trim().to_lowercase();
    let (title, author) = (key(&book.title), key(&book.author));
    books.iter().find(|other| {
        other.id != book.id && key(&other.title) == title && key(&other.author) == author
    })
}

/// REQ-36c: another book carrying the same stored ISBN.
pub fn find_isbn_duplicate<'a>(book: &Book, books: &'a [Book]) -> Option<&'a Book> {
    let isbn = book.isbn.as_deref()?;
    books.iter().find(|other| other.id != book.id && other.isbn.as_deref() == Some(isbn))
}

/// REQ-16f: whole days between start and finish, for Finished books with both dates.
pub fn days_to_finish(book: &Book) -> Option<i64> {
    if book.status != Status::Finished {
        return None;
    }
    let start = NaiveDate::parse_from_str(book.start_date.as_deref()?, "%Y-%m-%d").ok()?;
    let finish = NaiveDate::parse_from_str(book.finish_date.as_deref()?, "%Y-%m-%d").ok()?;
    Some((finish - start).num_days())
}

/// The large letters on a tile with no background (REQ-35g).
pub fn initials(title: &str) -> String {
    title
        .split_whitespace()
        .take(2)
        .filter_map(|word| word.chars().next())
        .flat_map(char::to_uppercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book() -> Book {
        Book { title: "Drive".into(), author: "Daniel H. Pink".into(), ..Book::new(0) }
    }

    #[test]
    fn completion_rounds_to_the_nearest_percent() {
        let mut b = book();
        assert_eq!(completion_percent(&b), 0);
        b.total_pages = Some(3);
        b.current_page = 1;
        assert_eq!(completion_percent(&b), 33);
        b.current_page = 2;
        assert_eq!(completion_percent(&b), 67);
    }

    #[test]
    fn appendix_f_messages() {
        let mut b = book();
        b.title = "   ".into();
        b.total_pages = Some(100);
        b.current_page = 101;
        b.start_date = Some("2026-05-02".into());
        b.finish_date = Some("2026-05-01".into());
        b.isbn = Some("9780140328722".into());
        let errors = validate(&b);
        assert_eq!(errors.title.as_deref(), Some("Title is required."));
        assert_eq!(errors.current_page.as_deref(), Some("Enter 0 to 100."));
        assert_eq!(errors.finish_date.as_deref(), Some("Finish date cannot be earlier than the start date."));
        assert_eq!(errors.isbn.as_deref(), Some("That ISBN is not valid. Check the digits and try again."));
    }

    #[test]
    fn a_title_alone_is_valid() {
        assert!(validate(&book()).is_empty());
    }

    #[test]
    fn malformed_dates_are_caught() {
        let mut b = book();
        b.start_date = Some("2026-13-40".into());
        assert!(validate(&b).start_date.is_some());
    }

    #[test]
    fn reading_stamps_the_start_date_once() {
        let mut b = book();
        apply_status_change(&mut b, Status::Reading);
        assert_eq!(b.start_date, Some(today()));
        b.start_date = Some("2020-01-01".into());
        apply_status_change(&mut b, Status::Reading);
        assert_eq!(b.start_date.as_deref(), Some("2020-01-01"));
    }

    #[test]
    fn finished_fills_the_finish_date_and_pages_but_not_the_start_date() {
        let mut b = book();
        b.total_pages = Some(288);
        apply_status_change(&mut b, Status::Finished);
        assert_eq!(b.finish_date, Some(today()));
        assert_eq!(b.current_page, 288);
        assert_eq!(b.start_date, None, "REQ-13f");
        assert_eq!(days_to_finish(&b), None);
    }

    #[test]
    fn recording_pages_starts_an_unstarted_book() {
        let mut b = book();
        apply_page_change(&mut b, 10);
        assert_eq!(b.status, Status::Reading);
        assert_eq!(b.start_date, Some(today()));
    }

    #[test]
    fn recording_zero_pages_leaves_status_alone() {
        let mut b = book();
        apply_page_change(&mut b, 0);
        assert_eq!(b.status, Status::ToRead);
    }

    #[test]
    fn lowering_the_total_lowers_the_current_page() {
        let mut b = book();
        b.total_pages = Some(300);
        b.current_page = 250;
        apply_total_pages_change(&mut b, Some(200));
        assert_eq!(b.current_page, 200);
    }

    #[test]
    fn changing_category_clears_sub_category() {
        let mut b = book();
        b.category_id = Some("a".into());
        b.sub_category_id = Some("s".into());
        apply_category_change(&mut b, Some("a".into()));
        assert_eq!(b.sub_category_id.as_deref(), Some("s"));
        apply_category_change(&mut b, Some("b".into()));
        assert_eq!(b.sub_category_id, None);
    }

    #[test]
    fn last_page_offers_finish_only_when_unfinished() {
        let mut b = book();
        b.total_pages = Some(10);
        b.current_page = 10;
        assert!(is_at_last_page(&b));
        b.status = Status::Finished;
        assert!(!is_at_last_page(&b));
    }

    #[test]
    fn duplicates_ignore_case_and_spacing_but_not_self() {
        let a = book();
        let mut b = book();
        b.title = "  dRIVE ".into();
        b.author = "daniel h. pink".into();
        let books = vec![a.clone()];
        assert!(find_duplicate(&b, &books).is_some());
        assert!(find_duplicate(&a, &books).is_none());
    }

    #[test]
    fn duplicate_isbn_is_found() {
        let mut a = book();
        a.isbn = Some("9780140328721".into());
        let mut b = book();
        b.isbn = Some("9780140328721".into());
        assert!(find_isbn_duplicate(&b, std::slice::from_ref(&a)).is_some());
        b.isbn = None;
        assert!(find_isbn_duplicate(&b, std::slice::from_ref(&a)).is_none());
    }

    #[test]
    fn initials_take_two_words_in_any_script() {
        assert_eq!(initials("Thinking, Fast and Slow"), "TF");
        assert_eq!(initials("Ѓердан од ќерамиди"), "ЃО");
        assert_eq!(initials("Drive"), "D");
    }
}
