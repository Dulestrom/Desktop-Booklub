//! Goal periods and pacing (Section 3.5). Periods are calendar years and months (D-07).

use chrono::{Datelike, NaiveDate, NaiveDateTime};

use crate::model::{Book, Goal, GoalId, Status};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pacing {
    Ahead,
    OnTrack,
    Behind,
}

impl Pacing {
    pub fn label(self) -> &'static str {
        match self {
            Pacing::Ahead => "Ahead",
            Pacing::OnTrack => "On Track",
            Pacing::Behind => "Behind",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GoalProgress {
    pub id: GoalId,
    pub label: &'static str,
    pub target: u32,
    /// REQ-15
    pub achieved: u32,
    /// REQ-15a: capped at 100.
    pub percent: u32,
    /// REQ-15d
    pub remaining: u32,
    /// REQ-15e
    pub per_week_needed: f64,
    pub pacing: Pacing,
}

pub fn goal_label(id: GoalId) -> &'static str {
    match id {
        GoalId::BooksPerYear => "Books this year",
        GoalId::BooksPerMonth => "Books this month",
    }
}

/// The half-open period containing `now`.
pub fn goal_period(id: GoalId, now: NaiveDateTime) -> (NaiveDate, NaiveDate) {
    let date = now.date();
    let first = |year, month| NaiveDate::from_ymd_opt(year, month, 1).expect("first of a month");
    match id {
        GoalId::BooksPerYear => (first(date.year(), 1), first(date.year() + 1, 1)),
        GoalId::BooksPerMonth => {
            let (year, month) = (date.year(), date.month());
            let next = if month == 12 { first(year + 1, 1) } else { first(year, month + 1) };
            (first(year, month), next)
        }
    }
}

/// REQ-15c: Finished, with a finish date inside the period.
pub fn count_finished_in(books: &[Book], start: NaiveDate, end: NaiveDate) -> u32 {
    books
        .iter()
        .filter(|book| {
            book.status == Status::Finished
                && book
                    .finish_date
                    .as_deref()
                    .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
                    .is_some_and(|date| date >= start && date < end)
        })
        .count() as u32
}

/// REQ-15g, with `expected = target x elapsed share of period`.
pub fn classify(achieved: u32, target: u32, elapsed_share: f64) -> Pacing {
    if target == 0 {
        return Pacing::OnTrack;
    }
    let expected = f64::from(target) * elapsed_share.clamp(0.0, 1.0);
    let achieved = f64::from(achieved);
    if achieved >= expected + 1.0 {
        Pacing::Ahead
    } else if achieved >= expected.floor() {
        Pacing::OnTrack
    } else {
        Pacing::Behind
    }
}

pub fn goal_progress(goal: &Goal, books: &[Book], now: NaiveDateTime) -> GoalProgress {
    let (start, end) = goal_period(goal.id, now);
    let achieved = count_finished_in(books, start, end);
    let target = goal.target;
    let remaining = target.saturating_sub(achieved);
    let percent = if target > 0 {
        ((f64::from(achieved) / f64::from(target) * 100.0).round() as u32).min(100)
    } else {
        0
    };

    let start_at = start.and_hms_opt(0, 0, 0).expect("midnight");
    let end_at = end.and_hms_opt(0, 0, 0).expect("midnight");
    let span = (end_at - start_at).num_seconds() as f64;
    let elapsed = (now - start_at).num_seconds() as f64;
    let weeks_left = ((end_at - now).num_seconds().max(0) as f64) / 604_800.0;

    GoalProgress {
        id: goal.id,
        label: goal_label(goal.id),
        target,
        achieved,
        percent,
        remaining,
        // REQ-15e: dividing by a fraction of a week would report an absurd rate.
        per_week_needed: if remaining == 0 { 0.0 } else { f64::from(remaining) / weeks_left.max(1.0) },
        pacing: classify(achieved, target, elapsed / span),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(12, 0, 0).unwrap()
    }

    fn finished(date: &str) -> Book {
        let mut book = Book { title: "x".into(), ..Book::new(0) };
        book.status = Status::Finished;
        book.finish_date = Some(date.into());
        book
    }

    /// The vectors from Appendix F.
    #[test]
    fn appendix_f_pacing() {
        assert_eq!(classify(13, 24, 0.50), Pacing::Ahead);
        assert_eq!(classify(12, 24, 0.50), Pacing::OnTrack);
        assert_eq!(classify(12, 24, 0.52), Pacing::OnTrack);
        assert_eq!(classify(11, 24, 0.50), Pacing::Behind);
    }

    #[test]
    fn periods_are_calendar_aligned_and_december_rolls_over() {
        let d = |y, m| NaiveDate::from_ymd_opt(y, m, 1).unwrap();
        assert_eq!(goal_period(GoalId::BooksPerYear, at(2026, 6, 15)), (d(2026, 1), d(2027, 1)));
        assert_eq!(goal_period(GoalId::BooksPerMonth, at(2026, 12, 31)), (d(2026, 12), d(2027, 1)));
    }

    #[test]
    fn only_finished_books_inside_the_period_count() {
        let mut unfinished = finished("2026-03-01");
        unfinished.status = Status::Reading;
        let books = [finished("2026-03-01"), finished("2025-12-31"), unfinished];
        let goal = Goal { id: GoalId::BooksPerYear, target: 24, active: true };
        assert_eq!(goal_progress(&goal, &books, at(2026, 6, 1)).achieved, 1);
    }

    #[test]
    fn percent_caps_at_one_hundred_and_remaining_at_zero() {
        let books: Vec<Book> = (0..5).map(|_| finished("2026-03-01")).collect();
        let goal = Goal { id: GoalId::BooksPerMonth, target: 2, active: true };
        let progress = goal_progress(&goal, &books, at(2026, 3, 20));
        assert_eq!((progress.percent, progress.remaining), (100, 0));
        assert_eq!(progress.per_week_needed, 0.0);
    }

    #[test]
    fn pace_needed_uses_at_least_one_week() {
        let goal = Goal { id: GoalId::BooksPerYear, target: 10, active: true };
        let late = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap().and_hms_opt(23, 0, 0).unwrap();
        assert_eq!(goal_progress(&goal, &[], late).per_week_needed, 10.0);
    }
}
