//! Sorting, filtering, and manual order (Section 3.3).

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::collate::{compare_text, contains_text, normalize_for_search};
use crate::model::{Book, Category, Priority, Status};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Manual,
    Priority,
    Category,
    Status,
    Title,
    Added,
}

impl SortKey {
    pub const ALL: [SortKey; 6] = [
        SortKey::Manual,
        SortKey::Priority,
        SortKey::Category,
        SortKey::Status,
        SortKey::Title,
        SortKey::Added,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SortKey::Manual => "Manual order",
            SortKey::Priority => "Priority",
            SortKey::Category => "Category",
            SortKey::Status => "Status",
            SortKey::Title => "Title",
            SortKey::Added => "Date added",
        }
    }
}

/// Name lookups for the id-referenced Category and Sub-category on each book.
#[derive(Debug, Default)]
pub struct CategoryIndex {
    categories: HashMap<String, String>,
    sub_categories: HashMap<String, String>,
}

impl CategoryIndex {
    pub fn new(categories: &[Category]) -> Self {
        let mut index = Self::default();
        for category in categories {
            index.categories.insert(category.id.clone(), category.name.clone());
            for sub in &category.sub_categories {
                index.sub_categories.insert(sub.id.clone(), sub.name.clone());
            }
        }
        index
    }

    pub fn category_name(&self, id: Option<&str>) -> &str {
        id.and_then(|id| self.categories.get(id)).map_or("", String::as_str)
    }

    pub fn sub_category_name(&self, id: Option<&str>) -> &str {
        id.and_then(|id| self.sub_categories.get(id)).map_or("", String::as_str)
    }
}

/// Returns a sorted copy, so each book's stored order is untouched (REQ-08e).
/// Manual order breaks every tie (REQ-08h).
pub fn sort_books(books: &[Book], key: SortKey, index: &CategoryIndex) -> Vec<Book> {
    let mut sorted = books.to_vec();
    let by_order = |a: &Book, b: &Book| a.order.cmp(&b.order);
    match key {
        SortKey::Manual => sorted.sort_by(by_order),
        SortKey::Priority => sorted.sort_by(|a, b| {
            a.priority.rank().cmp(&b.priority.rank()).then_with(|| by_order(a, b))
        }),
        SortKey::Status => sorted.sort_by(|a, b| {
            a.status.rank().cmp(&b.status.rank()).then_with(|| by_order(a, b))
        }),
        SortKey::Title => {
            sorted.sort_by(|a, b| compare_text(&a.title, &b.title).then_with(|| by_order(a, b)))
        }
        // REQ-08d: newest first.
        SortKey::Added => {
            sorted.sort_by(|a, b| b.added_at.cmp(&a.added_at).then_with(|| by_order(a, b)))
        }
        SortKey::Category => sorted.sort_by(|a, b| {
            compare_text(
                index.category_name(a.category_id.as_deref()),
                index.category_name(b.category_id.as_deref()),
            )
            .then_with(|| {
                // REQ-08a-i: within a Category, by Sub-category, books with none last.
                compare_text(
                    index.sub_category_name(a.sub_category_id.as_deref()),
                    index.sub_category_name(b.sub_category_id.as_deref()),
                )
            })
            .then_with(|| by_order(a, b))
        }),
    }
    sorted
}

/// How a heading ranks. A Sub-category sits under its Category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadingLevel {
    Major,
    Minor,
}

/// A label above a run of books (REQ-08j). `first_book` is the position of the run's first book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    pub title: String,
    pub count: usize,
    pub level: HeadingLevel,
    pub first_book: usize,
}

/// The headings for books already in `key`'s order. Only Priority and Category group books, and
/// a Category lists its Sub-categories only when at least one of its books has one.
pub fn headings(sorted: &[Book], key: SortKey, index: &CategoryIndex) -> Vec<Heading> {
    match key {
        SortKey::Priority => runs(sorted, |book| book.priority)
            .into_iter()
            .map(|(first_book, count)| Heading { title: format!("{} Priority", sorted[first_book].priority.as_str()), count, level: HeadingLevel::Major, first_book })
            .collect(),
        SortKey::Category => {
            let mut found = Vec::new();
            for (start, count) in runs(sorted, |book| book.category_id.as_deref()) {
                let category = &sorted[start..start + count];
                let id = category[0].category_id.as_deref();
                let title = if id.is_some() { index.category_name(id) } else { "Uncategorized" };
                found.push(Heading { title: title.to_string(), count, level: HeadingLevel::Major, first_book: start });
                if category.iter().any(|book| book.sub_category_id.is_some()) {
                    for (offset, count) in runs(category, |book| book.sub_category_id.as_deref()) {
                        let sub = category[offset].sub_category_id.as_deref();
                        let title = if sub.is_some() { index.sub_category_name(sub) } else { "No Sub-category" };
                        found.push(Heading { title: title.to_string(), count, level: HeadingLevel::Minor, first_book: start + offset });
                    }
                }
            }
            found
        }
        _ => Vec::new(),
    }
}

/// Each run of neighbouring books that share a key, as a start and a length.
fn runs<'a, K: PartialEq>(books: &'a [Book], key: impl Fn(&'a Book) -> K) -> Vec<(usize, usize)> {
    let mut found: Vec<(usize, usize)> = Vec::new();
    for (position, book) in books.iter().enumerate() {
        match found.last_mut() {
            Some((start, count)) if key(&books[*start]) == key(book) => *count += 1,
            _ => found.push((position, 1)),
        }
    }
    found
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filters {
    pub search: String,
    pub category_ids: Vec<String>,
    pub sub_category_ids: Vec<String>,
    pub statuses: Vec<Status>,
    pub priorities: Vec<Priority>,
}

impl Filters {
    /// REQ-09i: the number shown beside the filter control. Search is its own control.
    pub fn active_count(&self) -> usize {
        self.category_ids.len() + self.sub_category_ids.len() + self.statuses.len() + self.priorities.len()
    }

    pub fn is_filtering(&self) -> bool {
        self.active_count() > 0 || !self.search.trim().is_empty()
    }

    /// REQ-09h
    pub fn clear_facets(&mut self) {
        self.category_ids.clear();
        self.sub_category_ids.clear();
        self.statuses.clear();
        self.priorities.clear();
    }
}

/// REQ-09e: facets combine with AND. REQ-09f: values within a facet combine with OR.
/// An empty facet imposes no constraint.
pub fn filter_books<'a>(books: &'a [Book], filters: &Filters) -> Vec<&'a Book> {
    let search = filters.search.trim();
    let needle = normalize_for_search(search);
    let categories: HashSet<&str> = filters.category_ids.iter().map(String::as_str).collect();
    let subs: HashSet<&str> = filters.sub_category_ids.iter().map(String::as_str).collect();

    books
        .iter()
        .filter(|book| {
            if !categories.is_empty()
                && !book.category_id.as_deref().is_some_and(|id| categories.contains(id))
            {
                return false;
            }
            if !subs.is_empty()
                && !book.sub_category_id.as_deref().is_some_and(|id| subs.contains(id))
            {
                return false;
            }
            if !filters.statuses.is_empty() && !filters.statuses.contains(&book.status) {
                return false;
            }
            if !filters.priorities.is_empty() && !filters.priorities.contains(&book.priority) {
                return false;
            }
            // REQ-09c: titles, authors, and stored ISBNs.
            search.is_empty()
                || normalize_for_search(&book.title).contains(&needle)
                || normalize_for_search(&book.author).contains(&needle)
                || book.isbn.as_deref().is_some_and(|isbn| contains_text(isbn, search))
        })
        .collect()
}

/// REQ-09g: whole-library counts beside each facet value.
#[derive(Debug, Clone, Default)]
pub struct FacetCounts {
    pub categories: HashMap<String, usize>,
    pub sub_categories: HashMap<String, usize>,
    pub statuses: HashMap<Status, usize>,
    pub priorities: HashMap<Priority, usize>,
    pub uncategorized: usize,
}

pub fn count_facets(books: &[Book]) -> FacetCounts {
    let mut counts = FacetCounts::default();
    for book in books {
        match &book.category_id {
            Some(id) => *counts.categories.entry(id.clone()).or_default() += 1,
            None => counts.uncategorized += 1,
        }
        if let Some(id) = &book.sub_category_id {
            *counts.sub_categories.entry(id.clone()).or_default() += 1;
        }
        *counts.statuses.entry(book.status).or_default() += 1;
        *counts.priorities.entry(book.priority).or_default() += 1;
    }
    counts
}

/// REQ-07c: rewrites positions to run 0..n in the given sequence, returning only the
/// books whose position changed, so a save touches as few rows as it can.
pub fn renumber(books: &mut [Book]) -> Vec<Book> {
    let mut changed = Vec::new();
    for (position, book) in books.iter_mut().enumerate() {
        if book.order != position as i64 {
            book.order = position as i64;
            changed.push(book.clone());
        }
    }
    changed
}

/// REQ-07b: moves a book to the global position of the book it was dropped on, which
/// stays correct when a filter is hiding books in between. `books` is in manual order.
pub fn move_book(books: &mut Vec<Book>, moved_id: &str, target_id: &str) -> Vec<Book> {
    if moved_id == target_id {
        return Vec::new();
    }
    let (Some(from), Some(to)) = (
        books.iter().position(|book| book.id == moved_id),
        books.iter().position(|book| book.id == target_id),
    ) else {
        return Vec::new();
    };
    let mut moved = books.remove(from);
    // Between its new neighbours, so no other book's key changes (Android SRS REQ-32f).
    let before = if to == 0 { String::new() } else { books[to - 1].sort_key.clone() };
    let after = books.get(to).map(|book| book.sort_key.clone());
    moved.sort_key = crate::sync::key_between(&before, after.as_deref());
    books.insert(to, moved);
    renumber(books);
    vec![books[to].clone()]
}

/// REQ-01c, REQ-18f: the position following every existing book. Positions normally run
/// 0..n, but this stays correct if a store ever holds a gap.
pub fn next_position(books: &[Book]) -> i64 {
    books.iter().map(|book| book.order + 1).max().unwrap_or(0)
}

/// Stable ordering helper for callers that hold unsorted books.
pub fn in_manual_order(a: &Book, b: &Book) -> Ordering {
    a.order.cmp(&b.order)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SubCategory;

    fn book(id: &str, title: &str, order: i64) -> Book {
        Book { id: id.into(), title: title.into(), ..Book::new(order) }
    }

    fn ids(books: &[Book]) -> Vec<&str> {
        books.iter().map(|b| b.id.as_str()).collect()
    }

    fn index() -> CategoryIndex {
        CategoryIndex::new(&[Category {
            id: "c1".into(),
            name: "Strategy".into(),
            sub_categories: vec![SubCategory { id: "s1".into(), name: "Pricing".into() }],
            background: None,
        }])
    }

    #[test]
    fn priority_sort_uses_manual_order_for_ties_and_keeps_stored_order() {
        let mut a = book("a", "A", 2);
        a.priority = Priority::High;
        let mut b = book("b", "B", 1);
        b.priority = Priority::High;
        let mut c = book("c", "C", 0);
        c.priority = Priority::Low;
        let input = vec![a, b, c];
        let sorted = sort_books(&input, SortKey::Priority, &index());
        assert_eq!(ids(&sorted), ["b", "a", "c"]);
        assert_eq!(input[0].order, 2, "REQ-08e");
        assert_eq!(ids(&sort_books(&input, SortKey::Manual, &index())), ["c", "b", "a"]);
    }

    #[test]
    fn status_sort_runs_reading_to_read_finished() {
        let mut a = book("a", "A", 0);
        a.status = Status::Finished;
        let b = book("b", "B", 1);
        let mut c = book("c", "C", 2);
        c.status = Status::Reading;
        assert_eq!(ids(&sort_books(&[a, b, c], SortKey::Status, &index())), ["c", "b", "a"]);
    }

    #[test]
    fn category_sort_places_no_sub_category_last() {
        let mut a = book("a", "A", 0);
        a.category_id = Some("c1".into());
        let mut b = book("b", "B", 1);
        b.category_id = Some("c1".into());
        b.sub_category_id = Some("s1".into());
        assert_eq!(ids(&sort_books(&[a, b], SortKey::Category, &index())), ["b", "a"]);
    }

    #[test]
    fn added_sort_is_newest_first() {
        let mut a = book("a", "A", 0);
        a.added_at = "2026-01-01T00:00:00.000Z".into();
        let mut b = book("b", "B", 1);
        b.added_at = "2026-06-01T00:00:00.000Z".into();
        assert_eq!(ids(&sort_books(&[a, b], SortKey::Added, &index())), ["b", "a"]);
    }

    #[test]
    fn facets_and_together_and_values_or_together() {
        let mut a = book("a", "Drive", 0);
        a.status = Status::Reading;
        a.category_id = Some("c1".into());
        let mut b = book("b", "Inspired", 1);
        b.status = Status::Finished;
        b.category_id = Some("c1".into());
        let c = book("c", "Other", 2);
        let books = [a, b, c];

        let mut filters = Filters { statuses: vec![Status::Reading, Status::Finished], ..Filters::default() };
        assert_eq!(filter_books(&books, &filters).len(), 2);
        filters.category_ids = vec!["c1".into()];
        filters.statuses = vec![Status::Reading];
        let found = filter_books(&books, &filters);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "a");
    }

    #[test]
    fn search_covers_title_author_and_isbn() {
        let mut a = book("a", "Ѓердан", 0);
        a.author = "Ќирил".into();
        a.isbn = Some("9780140328721".into());
        let books = [a];
        for query in ["ѓердан", "гердан", "ќирил", "кирил", "03287"] {
            let filters = Filters { search: query.into(), ..Filters::default() };
            assert_eq!(filter_books(&books, &filters).len(), 1, "{query}");
        }
        let filters = Filters { search: "absent".into(), ..Filters::default() };
        assert!(filter_books(&books, &filters).is_empty());
    }

    #[test]
    fn active_count_excludes_search() {
        let filters = Filters { search: "x".into(), statuses: vec![Status::Reading], ..Filters::default() };
        assert_eq!(filters.active_count(), 1);
        assert!(filters.is_filtering());
    }

    #[test]
    fn move_book_lands_on_the_target_and_renumbers_without_gaps() {
        let mut books: Vec<Book> = ["a", "b", "c", "d"].iter().enumerate().map(|(i, id)| book(id, id, i as i64)).collect();
        let changed = move_book(&mut books, "d", "b");
        assert_eq!(ids(&books), ["a", "d", "b", "c"]);
        assert_eq!(books.iter().map(|b| b.order).collect::<Vec<_>>(), [0, 1, 2, 3]);
        // Only the moved book's key changes, and the keys now sort the way the list reads.
        assert_eq!(ids(&changed), ["d"]);
        assert!(books.windows(2).all(|pair| pair[0].sort_key < pair[1].sort_key));
    }

    #[test]
    fn moving_past_hidden_books_uses_global_positions() {
        // b and c are hidden by a filter; a is dropped on d.
        let mut books: Vec<Book> = ["a", "b", "c", "d"].iter().enumerate().map(|(i, id)| book(id, id, i as i64)).collect();
        move_book(&mut books, "a", "d");
        assert_eq!(ids(&books), ["b", "c", "d", "a"]);
    }

    #[test]
    fn next_position_follows_every_book_even_past_a_gap() {
        assert_eq!(next_position(&[]), 0);
        assert_eq!(next_position(&[book("a", "A", 0), book("b", "B", 1)]), 2);
        assert_eq!(next_position(&[book("a", "A", 0), book("b", "B", 9)]), 10);
    }

    #[test]
    fn moving_onto_itself_changes_nothing() {
        let mut books = vec![book("a", "A", 0)];
        assert!(move_book(&mut books, "a", "a").is_empty());
    }
}
