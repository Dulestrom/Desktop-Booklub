//! Application state: the single owner of the loaded library and of every write to it.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use booklub_core::paths::Paths;
use booklub_core::preferences::Preferences;
use booklub_core::query::{self, CategoryIndex, Filters, SortKey};
use booklub_core::store::{Store, StoreError};
use booklub_core::{background, Book, Category, CategoryBackground, Goal, Library, Status, SubCategory};
use gtk::prelude::*;

use crate::thumbs::Thumbs;
use crate::window::Ui;

/// Which sub-category tile is selected while a Category is open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SubFilter {
    #[default]
    All,
    Sub(String),
    NoSub,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Home,
    AllBooks,
    Status(Status),
    /// `None` is the Uncategorized tile (REQ-32d).
    Category { id: Option<String>, sub: SubFilter },
    Progress,
}

pub struct App {
    pub store: Store,
    pub paths: Paths,
    pub library: RefCell<Library>,
    pub view: RefCell<View>,
    pub sort: Cell<SortKey>,
    pub filters: RefCell<Filters>,
    pub preferences: Cell<Preferences>,
    pub saving_preferences: Cell<Option<gtk::glib::SourceId>>,
    pub thumbs: Rc<Thumbs>,
    pub ui: OnceCell<Ui>,
    /// True while the grid is being repopulated, so its signals do not fire back.
    pub updating: Cell<bool>,
    pub sync: crate::sync::SyncState,
}

impl App {
    pub fn ui(&self) -> &Ui {
        self.ui.get().expect("the window sets this before any signal can run")
    }

    pub fn index(&self) -> CategoryIndex {
        CategoryIndex::new(&self.library.borrow().categories)
    }

    /// The books the current view shows: its scope, the filters and search, then the sort.
    pub fn visible_books(&self) -> Vec<Book> {
        let library = self.library.borrow();
        let view = self.view.borrow();
        let scoped: Vec<Book> = library
            .books
            .iter()
            .filter(|book| match &*view {
                View::Home | View::Progress | View::AllBooks => true,
                View::Status(status) => book.status == *status,
                View::Category { id: None, .. } => book.category_id.is_none(),
                View::Category { id: Some(id), sub } => {
                    book.category_id.as_deref() == Some(id.as_str())
                        && match sub {
                            SubFilter::All => true,
                            SubFilter::Sub(sub) => book.sub_category_id.as_deref() == Some(sub.as_str()),
                            SubFilter::NoSub => book.sub_category_id.is_none(),
                        }
                }
            })
            .cloned()
            .collect();
        let filters = self.filters.borrow();
        let found: Vec<Book> = query::filter_books(&scoped, &filters).into_iter().cloned().collect();
        query::sort_books(&found, self.sort.get(), &CategoryIndex::new(&library.categories))
    }

    /// QA-06: persists first, then updates state, so a failed write leaves the store and the
    /// screen agreeing on the old value. The banner names the operation (REQ-31f).
    pub fn report(&self, result: Result<(), StoreError>) -> bool {
        match result {
            Ok(()) => {
                self.ui().hide_banner();
                true
            }
            Err(error) => {
                self.ui().show_banner(&error.message);
                false
            }
        }
    }

    pub fn book(&self, id: &str) -> Option<Book> {
        self.library.borrow().books.iter().find(|book| book.id == id).cloned()
    }

    /// Writes a book, then keeps memory in step.
    pub fn save_book(&self, book: &Book) -> bool {
        if !self.report(self.store.save_book(book)) {
            return false;
        }
        let mut library = self.library.borrow_mut();
        match library.books.iter().position(|existing| existing.id == book.id) {
            Some(index) => library.books[index] = book.clone(),
            None => library.books.push(book.clone()),
        }
        true
    }

    /// REQ-03, REQ-35h, REQ-07c.
    pub fn delete_book(&self, book: &Book) -> bool {
        if !self.report(self.store.delete_book(book)) {
            return false;
        }
        if let Some(file) = &book.background_file {
            self.thumbs.forget(file);
            background::remove(file, &self.paths.backgrounds, &self.paths.thumbnails);
        }
        let changed = {
            let mut library = self.library.borrow_mut();
            library.books.retain(|existing| existing.id != book.id);
            query::renumber(&mut library.books)
        };
        self.report(self.store.save_order(&changed))
    }

    /// REQ-07, REQ-07b: `moved_id` takes the global position of `target_id`.
    pub fn move_book(&self, moved_id: &str, target_id: &str) -> bool {
        let changed = {
            let mut library = self.library.borrow_mut();
            query::move_book(&mut library.books, moved_id, target_id)
        };
        if changed.is_empty() {
            return false;
        }
        if self.report(self.store.save_order(&changed)) {
            return true;
        }
        // The write failed, so memory goes back to what the store still holds (QA-06).
        if let Ok(stored) = self.store.load_all() {
            *self.library.borrow_mut() = stored;
        }
        false
    }

    pub fn add_category(&self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        let category = Category { id: booklub_core::model::new_id(), name: name.to_string(), sub_categories: vec![], background: None };
        let position = self.library.borrow().categories.len();
        if !self.report(self.store.save_category(&category, position)) {
            return false;
        }
        self.library.borrow_mut().categories.push(category);
        true
    }

    fn update_category(&self, id: &str, change: impl FnOnce(&mut Category)) -> bool {
        let found = {
            let library = self.library.borrow();
            library.categories.iter().position(|category| category.id == id).map(|index| (index, library.categories[index].clone()))
        };
        let Some((position, mut category)) = found else { return false };
        change(&mut category);
        if !self.report(self.store.save_category(&category, position)) {
            return false;
        }
        self.library.borrow_mut().categories[position] = category;
        true
    }

    /// REQ-05d, REQ-05e: books reference the id, so the new name appears everywhere.
    pub fn rename_category(&self, id: &str, name: &str) -> bool {
        let name = name.trim().to_string();
        !name.is_empty() && self.update_category(id, |category| category.name = name)
    }

    pub fn add_sub_category(&self, category_id: &str, name: &str) -> bool {
        let name = name.trim().to_string();
        !name.is_empty()
            && self.update_category(category_id, |category| {
                category.sub_categories.push(SubCategory { id: booklub_core::model::new_id(), name });
            })
    }

    pub fn rename_sub_category(&self, category_id: &str, sub_id: &str, name: &str) -> bool {
        let name = name.trim().to_string();
        !name.is_empty()
            && self.update_category(category_id, |category| {
                if let Some(sub) = category.sub_categories.iter_mut().find(|sub| sub.id == sub_id) {
                    sub.name = name;
                }
            })
    }

    /// REQ-05f: the schema clears it from every book, and memory follows.
    pub fn delete_category(&self, id: &str) -> bool {
        let Some(category) = self.library.borrow().categories.iter().find(|c| c.id == id).cloned() else { return false };
        if !self.report(self.store.delete_category(&category)) {
            return false;
        }
        // REQ-35h
        if let Some(file) = category.background.as_ref().and_then(CategoryBackground::file_name) {
            self.thumbs.forget(&format!("wide:{file}"));
            background::remove(file, &self.paths.backgrounds, &self.paths.thumbnails);
        }
        let mut library = self.library.borrow_mut();
        library.categories.retain(|c| c.id != id);
        for book in library.books.iter_mut().filter(|book| book.category_id.as_deref() == Some(id)) {
            book.category_id = None;
            book.sub_category_id = None;
        }
        true
    }

    /// REQ-05g
    pub fn delete_sub_category(&self, category_id: &str, sub_id: &str) -> bool {
        if !self.report(self.store.delete_sub_category(sub_id)) {
            return false;
        }
        let mut library = self.library.borrow_mut();
        if let Some(category) = library.categories.iter_mut().find(|c| c.id == category_id) {
            category.sub_categories.retain(|sub| sub.id != sub_id);
        }
        for book in library.books.iter_mut().filter(|book| book.sub_category_id.as_deref() == Some(sub_id)) {
            book.sub_category_id = None;
        }
        true
    }

    /// REQ-32f
    pub fn move_category(&self, moved_id: &str, target_id: &str) -> bool {
        let categories = {
            let library = self.library.borrow();
            let mut categories = library.categories.clone();
            let (Some(from), Some(to)) = (
                categories.iter().position(|c| c.id == moved_id),
                categories.iter().position(|c| c.id == target_id),
            ) else {
                return false;
            };
            let moved = categories.remove(from);
            categories.insert(to, moved);
            categories
        };
        if !self.report(self.store.save_category_order(&categories)) {
            return false;
        }
        self.library.borrow_mut().categories = categories;
        true
    }

    /// REQ-32g
    pub fn move_sub_category(&self, category_id: &str, moved_id: &str, target_id: &str) -> bool {
        self.update_category(category_id, |category| {
            let subs = &mut category.sub_categories;
            if let (Some(from), Some(to)) = (subs.iter().position(|s| s.id == moved_id), subs.iter().position(|s| s.id == target_id)) {
                let moved = subs.remove(from);
                subs.insert(to, moved);
            }
        })
    }

    pub fn save_goal(&self, goal: Goal) -> bool {
        if !self.report(self.store.save_goal(&goal)) {
            return false;
        }
        let mut library = self.library.borrow_mut();
        if let Some(existing) = library.goals.iter_mut().find(|existing| existing.id == goal.id) {
            *existing = goal;
        }
        true
    }

    /// Re-reads everything after a bulk write, so memory is exactly what was stored.
    pub fn reload(&self) {
        match self.store.load_all() {
            Ok(library) => *self.library.borrow_mut() = library,
            Err(error) => self.ui().show_banner(&error.message),
        }
    }

    /// REQ-45f: the image key for a Category tile. The Reader's choice wins, then the first book
    /// in manual order with a background, then nothing, which draws the plain surface.
    pub fn category_art(&self, category_id: &str) -> Option<String> {
        let chosen = {
            let library = self.library.borrow();
            library.categories.iter().find(|c| c.id == category_id).and_then(|c| c.background.clone())
        };
        match chosen {
            Some(CategoryBackground::Preset(id)) => Some(format!("preset:{id}")),
            Some(CategoryBackground::File(name)) => Some(format!("wide:{name}")),
            None => self.fallback_art(category_id),
        }
    }

    /// What a Category tile shows when the Reader has chosen nothing.
    pub fn fallback_art(&self, category_id: &str) -> Option<String> {
        self.library
            .borrow()
            .books
            .iter()
            .find(|book| book.category_id.as_deref() == Some(category_id) && book.background_file.is_some())
            .and_then(|book| book.background_file.clone())
    }

    /// REQ-45e
    pub fn set_category_background(&self, id: &str, background: Option<CategoryBackground>) -> bool {
        self.update_category(id, |category| category.background = background)
    }

    /// REQ-47a: a slider drag changes this many times a second, so it's written once it settles.
    pub fn save_preferences_soon(self: &Rc<Self>) {
        let app = self.clone();
        let timer = gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
            app.saving_preferences.take();
            app.save_preferences();
        });
        if let Some(previous) = self.saving_preferences.replace(Some(timer)) {
            previous.remove();
        }
    }

    pub fn save_preferences(&self) {
        // Losing a display choice isn't worth a banner, and the next change will try again.
        if let Err(error) = self.preferences.get().save(&self.paths.preferences) {
            eprintln!("could not save preferences: {error}");
        }
    }

    pub fn window(&self) -> gtk::Window {
        self.ui().window.clone().upcast()
    }
}
