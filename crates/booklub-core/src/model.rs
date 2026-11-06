//! The domain model, which mirrors the JSON export format.

use serde::{Deserialize, Serialize};

/// BR-04: exactly one of these three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Priority {
    High,
    Medium,
    Low,
}

impl Priority {
    pub const ALL: [Priority; 3] = [Priority::High, Priority::Medium, Priority::Low];

    pub fn as_str(self) -> &'static str {
        match self {
            Priority::High => "High",
            Priority::Medium => "Medium",
            Priority::Low => "Low",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == value)
    }

    /// REQ-08: High before Medium before Low.
    pub fn rank(self) -> u8 {
        match self {
            Priority::High => 0,
            Priority::Medium => 1,
            Priority::Low => 2,
        }
    }
}

/// BR-05: exactly one of these three. An Abandoned value was declined in v0.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Status {
    #[serde(rename = "To Read")]
    ToRead,
    Reading,
    Finished,
}

impl Status {
    pub const ALL: [Status; 3] = [Status::ToRead, Status::Reading, Status::Finished];

    pub fn as_str(self) -> &'static str {
        match self {
            Status::ToRead => "To Read",
            Status::Reading => "Reading",
            Status::Finished => "Finished",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == value)
    }

    /// REQ-08b: Reading before To Read before Finished.
    pub fn rank(self) -> u8 {
        match self {
            Status::Reading => 0,
            Status::ToRead => 1,
            Status::Finished => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubCategory {
    pub id: String,
    pub name: String,
}

/// REQ-45: what a Reader chose for a Category tile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CategoryBackground {
    /// One of the built-in artworks, by its ID in `background::PRESETS`.
    Preset(String),
    /// The file name of a private copy under the backgrounds directory.
    File(String),
}

impl CategoryBackground {
    /// The form stored in the database's `background` column.
    pub fn encode(&self) -> String {
        match self {
            Self::Preset(id) => format!("preset:{id}"),
            Self::File(name) => format!("file:{name}"),
        }
    }

    pub fn decode(value: &str) -> Option<Self> {
        match value.split_once(':')? {
            ("preset", id) => Some(Self::Preset(id.to_string())),
            ("file", name) => Some(Self::File(name.to_string())),
            _ => None,
        }
    }

    pub fn file_name(&self) -> Option<&str> {
        match self {
            Self::File(name) => Some(name),
            Self::Preset(_) => None,
        }
    }
}

/// REQ-05c: sub-categories nest under exactly one Category. Array order is display order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Category {
    pub id: String,
    pub name: String,
    pub sub_categories: Vec<SubCategory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<CategoryBackground>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Book {
    pub id: String,
    /// BR-01: the only required attribute.
    pub title: String,
    pub author: String,
    /// BR-06: at most one Category and, only under it, at most one Sub-category.
    pub category_id: Option<String>,
    pub sub_category_id: Option<String>,
    pub priority: Priority,
    pub status: Status,
    /// REQ-10: a positive integer, or empty when unrecorded.
    pub total_pages: Option<u32>,
    /// BR-02: between zero and the total.
    pub current_page: u32,
    /// Calendar dates as `yyyy-mm-dd`. BR-03: finish on or after start.
    pub start_date: Option<String>,
    pub finish_date: Option<String>,
    /// REQ-02d: 1 to 5 inclusive, or empty.
    pub rating: Option<u8>,
    pub notes: String,
    /// REQ-07a: the persisted manual-order position.
    pub order: i64,
    /// REQ-08d: an ISO 8601 UTC datetime.
    pub added_at: String,
    /// BR-09: a validated ISBN with separators removed.
    pub isbn: Option<String>,
    pub publisher: String,
    pub published_year: Option<i32>,
    pub language: String,
    /// BR-10: the file name of the private copy under the backgrounds directory.
    pub background_file: Option<String>,
    /// The synced position in manual order (Android SRS REQ-32f). `order` is this key's rank,
    /// so a move changes one book's key and no other book's.
    #[serde(default)]
    pub sort_key: String,
}

impl Book {
    /// A new book with the defaults from REQ-04 and REQ-06, placed last by REQ-01c.
    pub fn new(order: i64) -> Self {
        Self {
            id: new_id(),
            title: String::new(),
            author: String::new(),
            category_id: None,
            sub_category_id: None,
            priority: Priority::Medium,
            status: Status::ToRead,
            total_pages: None,
            current_page: 0,
            start_date: None,
            finish_date: None,
            rating: None,
            notes: String::new(),
            order,
            added_at: now_iso(),
            isbn: None,
            publisher: String::new(),
            published_year: None,
            language: String::new(),
            background_file: None,
            sort_key: crate::sync::new_sort_key(order),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GoalId {
    #[serde(rename = "booksPerYear")]
    BooksPerYear,
    #[serde(rename = "booksPerMonth")]
    BooksPerMonth,
}

impl GoalId {
    pub fn as_str(self) -> &'static str {
        match self {
            GoalId::BooksPerYear => "booksPerYear",
            GoalId::BooksPerMonth => "booksPerMonth",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "booksPerYear" => Some(GoalId::BooksPerYear),
            "booksPerMonth" => Some(GoalId::BooksPerMonth),
            _ => None,
        }
    }
}

/// REQ-14, REQ-14a. REQ-14d: any goal can be deactivated, keeping its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    pub id: GoalId,
    pub target: u32,
    pub active: bool,
}

/// REQ-14f: the goals created on first run. The yearly goal lists first (REQ-15h).
pub fn default_goals() -> Vec<Goal> {
    vec![
        Goal { id: GoalId::BooksPerYear, target: 24, active: true },
        Goal { id: GoalId::BooksPerMonth, target: 2, active: true },
    ]
}

/// Everything the application holds, books in manual order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Library {
    pub books: Vec<Book>,
    pub categories: Vec<Category>,
    pub goals: Vec<Goal>,
}

impl Library {
    /// Every private image this library refers to, from books and categories alike.
    pub fn background_files(&self) -> Vec<String> {
        let books = self.books.iter().filter_map(|book| book.background_file.clone());
        let categories = self.categories.iter().filter_map(|category| category.background.as_ref()?.file_name().map(str::to_string));
        books.chain(categories).collect()
    }
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
