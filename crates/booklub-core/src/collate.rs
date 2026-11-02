//! Text comparison (QA-20) and search matching (QA-20a).
//!
//! Code-point order would misplace Macedonian letters: the alphabet runs Д, Ѓ, Е and
//! Т, Ќ, У, which only the `mk` collation tailoring knows.

use std::cmp::Ordering;
use std::sync::LazyLock;

use icu_collator::options::{CollatorOptions, Strength};
use icu_collator::preferences::CollationNumericOrdering;
use icu_collator::{Collator, CollatorBorrowed, CollatorPreferences};
use icu_locale_core::locale;
use regex::Regex;
use unicode_normalization::UnicodeNormalization;

fn options() -> CollatorOptions {
    let mut options = CollatorOptions::default();
    options.strength = Some(Strength::Primary);
    options
}

static LATIN: LazyLock<CollatorBorrowed<'static>> = LazyLock::new(|| {
    let mut preferences = CollatorPreferences::from(locale!("en"));
    preferences.numeric_ordering = Some(CollationNumericOrdering::True);
    Collator::try_new(preferences, options()).expect("compiled collation data for en")
});

static CYRILLIC: LazyLock<CollatorBorrowed<'static>> = LazyLock::new(|| {
    let mut preferences = CollatorPreferences::from(locale!("mk"));
    preferences.numeric_ordering = Some(CollationNumericOrdering::True);
    Collator::try_new(preferences, options()).expect("compiled collation data for mk")
});

static DIACRITICS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\p{Diacritic}").expect("valid pattern"));

/// The Cyrillic and Cyrillic Supplement blocks, which hold every Macedonian letter.
fn is_cyrillic(text: &str) -> bool {
    text.chars().any(|c| matches!(c, '\u{0400}'..='\u{052F}'))
}

/// Trims both values, sorts empties last, then Latin before Cyrillic, each collated
/// with its own locale at primary strength with numeric ordering.
pub fn compare_text(a: &str, b: &str) -> Ordering {
    let (left, right) = (a.trim(), b.trim());
    match (left.is_empty(), right.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }

    match (is_cyrillic(left), is_cyrillic(right)) {
        (false, true) => Ordering::Less,
        (true, false) => Ordering::Greater,
        (true, true) => CYRILLIC.compare(left, right),
        (false, false) => LATIN.compare(left, right),
    }
}

/// Case and diacritics removed, so `г` finds `Ѓ` and `resume` finds `résumé`.
pub fn normalize_for_search(text: &str) -> String {
    let decomposed: String = text.nfd().collect();
    DIACRITICS.replace_all(&decomposed, "").to_lowercase()
}

/// QA-20a: the search test. An empty needle matches everything.
pub fn contains_text(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    normalize_for_search(haystack).contains(&normalize_for_search(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_sorted(values: &[&str]) {
        for pair in values.windows(2) {
            assert_eq!(compare_text(pair[0], pair[1]), Ordering::Less, "{} before {}", pair[0], pair[1]);
            assert_eq!(compare_text(pair[1], pair[0]), Ordering::Greater, "{} after {}", pair[1], pair[0]);
        }
    }

    /// The vectors from Appendix F.
    #[test]
    fn latin_then_cyrillic_then_empty() {
        assert_sorted(&["Zen", "Ана", ""]);
    }

    #[test]
    fn numeric_ordering() {
        assert_sorted(&["Book 2", "Book 10"]);
    }

    #[test]
    fn macedonian_alphabet_places_gje_and_kje() {
        assert_sorted(&["Г", "Д", "Ѓ", "Е"]);
        assert_sorted(&["Т", "Ќ", "У"]);
    }

    #[test]
    fn surrounding_space_is_ignored() {
        assert_eq!(compare_text("  Drive ", "Drive"), Ordering::Equal);
    }

    #[test]
    fn search_ignores_case_and_diacritics() {
        assert!(contains_text("ГОЦЕ", "гоце"));
        assert!(contains_text("Ѓ", "г"));
        assert!(contains_text("Résumé", "resume"));
        assert!(!contains_text("Drive", "strategy"));
        assert!(contains_text("Anything", ""));
    }
}
