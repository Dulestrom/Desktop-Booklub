//! Library statistics (Section 3.6).

use std::collections::HashMap;

use chrono::{Datelike, NaiveDate, NaiveDateTime};

use crate::collate::compare_text;
use crate::goals::{count_finished_in, goal_period};
use crate::model::{Book, Category, GoalId, Priority, Status};
use crate::rules::days_to_finish;

#[derive(Debug, Clone, PartialEq)]
pub struct CategoryCount {
    pub id: String,
    pub name: String,
    pub count: usize,
    /// Share of the categorised library, as a percentage.
    pub share: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LibraryStats {
    /// REQ-16
    pub total: usize,
    /// REQ-16a
    pub by_status: HashMap<Status, usize>,
    /// REQ-16b, REQ-16g: descending count, then name.
    pub by_category: Vec<CategoryCount>,
    /// REQ-16l
    pub uncategorized: usize,
    /// REQ-16d
    pub finished_this_year: u32,
    /// REQ-16e: January to December of the current year.
    pub finished_by_month: [u32; 12],
    /// REQ-16f, REQ-16k: empty when no book qualifies.
    pub mean_days_to_finish: Option<f64>,
    /// REQ-16h
    pub priority_by_status: HashMap<(Priority, Status), usize>,
    /// REQ-16i, REQ-16k
    pub mean_rating: Option<f64>,
    /// REQ-16j: the three Categories holding the fewest books.
    pub thinnest: Vec<CategoryCount>,
}

pub fn compute(books: &[Book], categories: &[Category], now: NaiveDateTime) -> LibraryStats {
    let mut by_status = HashMap::new();
    let mut priority_by_status = HashMap::new();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut uncategorized = 0;
    let (mut rating_total, mut rating_count) = (0u32, 0u32);
    let (mut days_total, mut days_count) = (0i64, 0u32);
    let mut finished_by_month = [0u32; 12];
    let year = now.year();

    for book in books {
        *by_status.entry(book.status).or_insert(0) += 1;
        *priority_by_status.entry((book.priority, book.status)).or_insert(0) += 1;

        match &book.category_id {
            Some(id) => *counts.entry(id).or_insert(0) += 1,
            None => uncategorized += 1,
        }
        if let Some(rating) = book.rating {
            rating_total += u32::from(rating);
            rating_count += 1;
        }
        if let Some(days) = days_to_finish(book) {
            days_total += days;
            days_count += 1;
        }
        if book.status == Status::Finished {
            let finished = book.finish_date.as_deref().and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
            if let Some(date) = finished.filter(|d| d.year() == year) {
                finished_by_month[date.month0() as usize] += 1;
            }
        }
    }

    let categorised = books.len() - uncategorized;
    let mut by_category: Vec<CategoryCount> = categories
        .iter()
        .map(|category| {
            let count = counts.get(category.id.as_str()).copied().unwrap_or(0);
            CategoryCount {
                id: category.id.clone(),
                name: category.name.clone(),
                count,
                share: if categorised > 0 { count as f64 / categorised as f64 * 100.0 } else { 0.0 },
            }
        })
        .collect();
    by_category.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| compare_text(&a.name, &b.name)));

    let (start, end) = goal_period(GoalId::BooksPerYear, now);

    LibraryStats {
        total: books.len(),
        by_status,
        thinnest: by_category.iter().rev().take(3).cloned().collect(),
        by_category,
        uncategorized,
        finished_this_year: count_finished_in(books, start, end),
        finished_by_month,
        mean_days_to_finish: (days_count > 0).then(|| days_total as f64 / f64::from(days_count)),
        priority_by_status,
        mean_rating: (rating_count > 0).then(|| f64::from(rating_total) / f64::from(rating_count)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 6, 15).unwrap().and_hms_opt(9, 0, 0).unwrap()
    }

    fn category(id: &str, name: &str) -> Category {
        Category { id: id.into(), name: name.into(), sub_categories: vec![], background: None }
    }

    #[test]
    fn empty_library_reports_no_means() {
        let stats = compute(&[], &[], now());
        assert_eq!(stats.total, 0);
        assert_eq!(stats.mean_rating, None);
        assert_eq!(stats.mean_days_to_finish, None);
    }

    #[test]
    fn counts_means_and_months() {
        let mut a = Book { title: "a".into(), ..Book::new(0) };
        a.status = Status::Finished;
        a.start_date = Some("2026-02-01".into());
        a.finish_date = Some("2026-02-11".into());
        a.rating = Some(5);
        a.category_id = Some("c1".into());
        let mut b = Book { title: "b".into(), ..Book::new(1) };
        b.status = Status::Finished;
        b.finish_date = Some("2026-03-01".into());
        b.rating = Some(2);
        let c = Book { title: "c".into(), ..Book::new(2) };

        let stats = compute(&[a, b, c], &[category("c1", "Strategy"), category("c2", "Agile")], now());
        assert_eq!(stats.total, 3);
        assert_eq!(stats.by_status[&Status::Finished], 2);
        assert_eq!(stats.uncategorized, 2);
        assert_eq!(stats.finished_this_year, 2);
        assert_eq!(stats.finished_by_month[1], 1);
        assert_eq!(stats.finished_by_month[2], 1);
        assert_eq!(stats.mean_days_to_finish, Some(10.0), "only the book with both dates counts");
        assert_eq!(stats.mean_rating, Some(3.5));
        assert_eq!(stats.by_category[0].name, "Strategy");
        assert_eq!(stats.by_category[0].share, 100.0);
    }

    #[test]
    fn thinnest_lists_the_fewest_first_and_stops_at_three() {
        let categories = [category("a", "A"), category("b", "B"), category("c", "C"), category("d", "D")];
        let mut book = Book { title: "x".into(), ..Book::new(0) };
        book.category_id = Some("a".into());
        let stats = compute(&[book], &categories, now());
        assert_eq!(stats.thinnest.len(), 3);
        assert!(stats.thinnest.iter().all(|c| c.name != "A"));
    }
}
