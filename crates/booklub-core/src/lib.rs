//! Domain rules and storage for Booklub.
//!
//! Every rule lives here, with no GTK dependency, so it can be tested on its own (QA-38).

pub mod archive;
pub mod background;
pub mod collate;
pub mod goals;
pub mod isbn;
pub mod layout;
pub mod model;
pub mod paths;
pub mod portability;
pub mod preferences;
pub mod query;
pub mod rules;
pub mod spreadsheet;
pub mod stats;
pub mod sync;
pub mod store;

pub use model::{Book, Category, CategoryBackground, Goal, GoalId, Library, Priority, Status, SubCategory};

#[cfg(test)]
mod sync_tests;
