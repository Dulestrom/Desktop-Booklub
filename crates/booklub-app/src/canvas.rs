//! The two containers that size tiles to the space they have (REQ-46).
//!
//! `BookCanvas` shows the books. GtkGridView can't have section headings, and GtkListView keeps
//! about 200 rows of widgets alive, so this does its own recycling: it creates widgets only for
//! what is near the viewport, and works out every position in one layout pass.
//!
//! `FluidGrid` is the same fit for the few tiles on Home, which don't need recycling.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use booklub_core::layout::Fit;
use booklub_core::preferences::BooksView;
use booklub_core::query::{CategoryIndex, Heading, HeadingLevel};
use booklub_core::Book;
use gtk::graphene::Point;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gdk, glib, gsk};

use crate::thumbs::Thumbs;
use crate::tiles::{BookRow, BookTile, BOOK_HEIGHT, BOOK_WIDTH};
use crate::util::{books, hbox, label};

pub const TILE_GAP: i32 = 16;
const SIDE_PADDING: i32 = 24;
const TOP_PADDING: i32 = 16;
const BOTTOM_PADDING: i32 = 24;
const ROW_HEIGHT: i32 = 62;
const MAJOR_HEADING_HEIGHT: i32 = 36;
const MINOR_HEADING_HEIGHT: i32 = 28;
const GAP_ABOVE_MAJOR: i32 = 28;
const GAP_ABOVE_MINOR: i32 = 14;
const GAP_BELOW_HEADING: i32 = 12;
/// Widgets are kept this far past each edge, so a tile is ready before it scrolls into view.
const OVERSCAN: f64 = 300.0;
const SCROLL_STEP: f64 = 48.0;

#[derive(Clone)]
pub enum Slot {
    Tile(BookTile),
    Row(BookRow),
}

impl Slot {
    fn root(&self) -> gtk::Widget {
        match self {
            Slot::Tile(tile) => tile.root.clone().upcast(),
            Slot::Row(row) => row.root.clone().upcast(),
        }
    }

    fn release(&self, thumbs: &Thumbs) {
        match self {
            Slot::Tile(tile) => thumbs.release(&tile.picture),
            Slot::Row(row) => thumbs.release(&row.picture),
        }
    }
}

pub struct HeadingView {
    root: gtk::Box,
    title: gtk::Label,
    count: gtk::Label,
}

impl HeadingView {
    fn new() -> Self {
        let title = label("", &["section-title"]);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_width_chars(1);
        title.set_hexpand(true);
        let count = label("", &["caption"]);
        let root = gtk::Box::builder().accessible_role(gtk::AccessibleRole::Heading).css_classes(["section-heading"]).spacing(8).build();
        root.append(&title);
        root.append(&count);
        Self { root, title, count }
    }

    fn bind(&self, heading: &Heading) {
        self.title.set_label(&heading.title);
        self.count.set_label(&books(heading.count));
        let major = heading.level == HeadingLevel::Major;
        for (class, on) in [("major", major), ("minor", !major)] {
            if on {
                self.root.add_css_class(class);
            } else {
                self.root.remove_css_class(class);
            }
        }
        self.root.update_property(&[
            gtk::accessible::Property::Label(&format!("{}, {}", heading.title, books(heading.count))),
            gtk::accessible::Property::Level(if major { 2 } else { 3 }),
        ]);
    }
}

#[derive(Clone, Copy)]
pub enum RowKind {
    Heading(usize),
    /// The position of the first book, and how many this row holds.
    Books(usize, usize),
}

#[derive(Clone, Copy)]
pub struct Row {
    y: i32,
    height: i32,
    kind: RowKind,
}

/// Works out each row's place. Rows are in vertical order, which is what lets a scroll find the
/// visible ones with a binary search.
fn layout_rows(book_count: usize, headings: &[Heading], fit: Fit, book_height: i32, list: bool) -> (Vec<Row>, i32) {
    let mut rows = Vec::new();
    let mut y = TOP_PADDING;
    let mut next_heading = 0;
    let mut position = 0;
    let columns = if list { 1 } else { fit.columns as usize };
    let height = if list { ROW_HEIGHT } else { book_height };
    let gap = if list { 0 } else { TILE_GAP };
    while position < book_count {
        while let Some(heading) = headings.get(next_heading).filter(|heading| heading.first_book == position) {
            let major = heading.level == HeadingLevel::Major;
            if !rows.is_empty() {
                y += if major { GAP_ABOVE_MAJOR } else { GAP_ABOVE_MINOR } - gap.min(if major { GAP_ABOVE_MAJOR } else { GAP_ABOVE_MINOR });
            }
            let heading_height = if major { MAJOR_HEADING_HEIGHT } else { MINOR_HEADING_HEIGHT };
            rows.push(Row { y, height: heading_height, kind: RowKind::Heading(next_heading) });
            y += heading_height + GAP_BELOW_HEADING;
            next_heading += 1;
        }
        // Sections start on a fresh row, so a heading's books are the only ones beneath it.
        let section_end = headings.get(next_heading).map_or(book_count, |heading| heading.first_book);
        while position < section_end {
            let count = columns.min(section_end - position);
            rows.push(Row { y, height, kind: RowKind::Books(position, count) });
            y += height + gap;
            position += count;
        }
    }
    (rows, y - gap + BOTTOM_PADDING)
}

mod imp {
    use super::*;
    use glib::ParamSpec;
    use gtk::subclass::prelude::*;
    use std::sync::OnceLock;

    #[derive(Default)]
    pub struct BookCanvas {
        pub books: RefCell<Vec<Book>>,
        pub headings: RefCell<Vec<Heading>>,
        pub index: RefCell<CategoryIndex>,
        pub rows: RefCell<Vec<Row>>,
        /// Which row holds each book.
        pub book_rows: RefCell<Vec<u32>>,
        pub heading_rows: RefCell<Vec<u32>>,
        pub width: Cell<i32>,
        pub height: Cell<i32>,
        pub columns: Cell<i32>,
        pub list: Cell<bool>,
        pub scale: Cell<f64>,
        pub upper: Cell<i32>,
        pub selected: RefCell<Option<String>>,
        pub mounted: RefCell<HashMap<usize, Slot>>,
        pub free_tiles: RefCell<Vec<Slot>>,
        pub free_rows: RefCell<Vec<Slot>>,
        pub mounted_headings: RefCell<HashMap<usize, HeadingView>>,
        pub free_headings: RefCell<Vec<HeadingView>>,
        pub hadjustment: RefCell<Option<gtk::Adjustment>>,
        pub vadjustment: RefCell<Option<gtk::Adjustment>>,
        pub scrolled: RefCell<Option<glib::SignalHandlerId>>,
        pub thumbs: RefCell<Option<Rc<Thumbs>>>,
        pub prepare: RefCell<Option<Rc<dyn Fn(&gtk::Widget, &Rc<RefCell<String>>)>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BookCanvas {
        const NAME: &'static str = "BooklubBookCanvas";
        type Type = super::BookCanvas;
        type ParentType = gtk::Widget;
        type Interfaces = (gtk::Scrollable,);

        fn class_init(class: &mut Self::Class) {
            class.set_css_name("bookcanvas");
            class.set_accessible_role(gtk::AccessibleRole::List);
        }
    }

    impl ScrollableImpl for BookCanvas {}

    impl ObjectImpl for BookCanvas {
        fn constructed(&self) {
            self.parent_constructed();
            self.scale.set(1.0);
            self.columns.set(1);
            self.obj().set_overflow(gtk::Overflow::Hidden);
        }

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: OnceLock<Vec<ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                ["hadjustment", "vadjustment", "hscroll-policy", "vscroll-policy"]
                    .into_iter()
                    .map(|name| glib::ParamSpecOverride::for_interface::<gtk::Scrollable>(name))
                    .collect()
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            match pspec.name() {
                "hadjustment" => {
                    self.hadjustment.replace(value.get().expect("the type GObject checked"));
                    self.obj().queue_allocate();
                }
                "vadjustment" => self.obj().set_vadjustment_inner(value.get().expect("the type GObject checked")),
                // Scrolling is always by pixel, so the policies have nothing to change.
                _ => {}
            }
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            match pspec.name() {
                "hadjustment" => self.hadjustment.borrow().to_value(),
                "vadjustment" => self.vadjustment.borrow().to_value(),
                _ => gtk::ScrollablePolicy::Minimum.to_value(),
            }
        }

        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for BookCanvas {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let canvas = self.obj();
            let resized = width != self.width.get();
            self.height.set(height);
            if resized {
                let anchor = canvas.scroll_anchor();
                self.width.set(width);
                canvas.relayout();
                canvas.configure_adjustments();
                canvas.restore_anchor(anchor);
            } else {
                canvas.configure_adjustments();
            }
            canvas.sync_mounted();
            canvas.place_children();
        }

        /// Tab enters the books once and leaves on the next press. Inside, the arrow keys move.
        fn focus(&self, _direction: gtk::DirectionType) -> bool {
            let canvas = self.obj();
            if canvas.focused_position().is_some() {
                return false;
            }
            let target = canvas.selected_position().filter(|position| canvas.is_near_view(*position)).or_else(|| canvas.first_visible_position());
            target.is_some_and(|position| canvas.focus_position(position))
        }

        fn grab_focus(&self) -> bool {
            let canvas = self.obj();
            let target = canvas.selected_position().or_else(|| canvas.first_visible_position());
            target.is_some_and(|position| canvas.focus_position(position))
        }
    }
}

glib::wrapper! {
    pub struct BookCanvas(ObjectSubclass<imp::BookCanvas>) @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

impl BookCanvas {
    /// `prepare` runs once on each new tile or row, to attach what happens when it's used.
    pub fn new(thumbs: Rc<Thumbs>, prepare: impl Fn(&gtk::Widget, &Rc<RefCell<String>>) + 'static) -> Self {
        let canvas: Self = glib::Object::new();
        canvas.imp().thumbs.replace(Some(thumbs));
        canvas.imp().prepare.replace(Some(Rc::new(prepare)));
        canvas.attach_keys();
        canvas
    }

    pub fn book_count(&self) -> usize {
        self.imp().books.borrow().len()
    }

    pub fn books(&self) -> Vec<Book> {
        self.imp().books.borrow().clone()
    }

    pub fn headings(&self) -> Vec<Heading> {
        self.imp().headings.borrow().clone()
    }

    pub fn columns(&self) -> i32 {
        self.imp().columns.get()
    }

    pub fn is_list(&self) -> bool {
        self.imp().list.get()
    }

    pub fn set_books(&self, books: Vec<Book>, headings: Vec<Heading>, index: CategoryIndex) {
        let focused = self.focused_position().and_then(|position| self.imp().books.borrow().get(position).map(|book| book.id.clone()));
        self.imp().books.replace(books);
        self.imp().headings.replace(headings);
        self.imp().index.replace(index);
        self.release_all();
        self.relayout();
        self.configure_adjustments();
        self.sync_mounted();
        if let Some(position) = focused.and_then(|id| self.position_of(&id)) {
            self.focus_position(position);
        }
        self.queue_allocate();
    }

    pub fn set_mode(&self, view: BooksView) {
        let list = view == BooksView::List;
        if self.imp().list.replace(list) == list {
            return;
        }
        let anchor = self.scroll_anchor();
        self.release_all();
        self.relayout();
        self.configure_adjustments();
        self.restore_anchor(anchor);
        self.sync_mounted();
        self.queue_allocate();
    }

    /// The slider sets the smallest size a tile may be. Positions follow in the same pass.
    pub fn set_scale(&self, scale: f64) {
        if self.imp().scale.replace(scale) == scale {
            return;
        }
        let anchor = self.scroll_anchor();
        self.relayout();
        self.configure_adjustments();
        self.restore_anchor(anchor);
        self.sync_mounted();
        self.queue_allocate();
    }

    pub fn set_selected(&self, id: Option<&str>) {
        self.imp().selected.replace(id.map(str::to_string));
        for (position, slot) in self.imp().mounted.borrow().iter() {
            self.apply_selected(*position, slot);
        }
    }

    pub fn selected_id(&self) -> Option<String> {
        self.imp().selected.borrow().clone()
    }

    pub fn position_of(&self, id: &str) -> Option<usize> {
        self.imp().books.borrow().iter().position(|book| book.id == id)
    }

    pub fn selected_position(&self) -> Option<usize> {
        self.selected_id().and_then(|id| self.position_of(&id))
    }

    pub fn focused_position(&self) -> Option<usize> {
        self.imp().mounted.borrow().iter().find(|(_, slot)| slot.root().has_focus()).map(|(position, _)| *position)
    }

    /// Scrolls the book into view and gives it keyboard focus.
    pub fn focus_position(&self, position: usize) -> bool {
        self.reveal(position);
        self.sync_mounted();
        let slot = self.imp().mounted.borrow().get(&position).cloned();
        slot.is_some_and(|slot| slot.root().grab_focus())
    }

    fn reveal(&self, position: usize) {
        let imp = self.imp();
        let Some(adjustment) = imp.vadjustment.borrow().clone() else { return };
        let Some(row) = imp.book_rows.borrow().get(position).and_then(|row| imp.rows.borrow().get(*row as usize).copied()) else { return };
        let (top, bottom) = (f64::from(row.y - TILE_GAP), f64::from(row.y + row.height + TILE_GAP));
        if top < adjustment.value() {
            adjustment.set_value(top);
        } else if bottom > adjustment.value() + adjustment.page_size() {
            adjustment.set_value(bottom - adjustment.page_size());
        }
    }

    fn is_near_view(&self, position: usize) -> bool {
        self.imp().mounted.borrow().contains_key(&position)
    }

    fn first_visible_position(&self) -> Option<usize> {
        let imp = self.imp();
        let value = imp.vadjustment.borrow().as_ref().map_or(0.0, gtk::Adjustment::value);
        let rows = imp.rows.borrow();
        let start = rows.partition_point(|row| f64::from(row.y) < value);
        rows[start..].iter().chain(rows[..start].iter().rev()).find_map(|row| match row.kind {
            RowKind::Books(first, _) => Some(first),
            RowKind::Heading(_) => None,
        })
    }

    fn fit(&self) -> Fit {
        let imp = self.imp();
        let min_width = (f64::from(BOOK_WIDTH) * imp.scale.get()).round() as i32;
        Fit::new(imp.width.get() - 2 * SIDE_PADDING, min_width, TILE_GAP)
    }

    fn relayout(&self) {
        let imp = self.imp();
        let fit = self.fit();
        imp.columns.set(fit.columns);
        let ratio = f64::from(BOOK_HEIGHT) / f64::from(BOOK_WIDTH);
        let (rows, upper) = layout_rows(imp.books.borrow().len(), &imp.headings.borrow(), fit, fit.height(ratio), imp.list.get());
        let mut book_rows = vec![0; imp.books.borrow().len()];
        let mut heading_rows = vec![0; imp.headings.borrow().len()];
        for (index, row) in rows.iter().enumerate() {
            match row.kind {
                RowKind::Heading(heading) => heading_rows[heading] = index as u32,
                RowKind::Books(first, count) => book_rows[first..first + count].fill(index as u32),
            }
        }
        imp.rows.replace(rows);
        imp.book_rows.replace(book_rows);
        imp.heading_rows.replace(heading_rows);
        imp.upper.set(upper);
        for slot in imp.mounted.borrow().values() {
            if let Slot::Row(row) = slot {
                row.set_available_width(imp.width.get());
            }
        }
    }

    fn configure_adjustments(&self) {
        let imp = self.imp();
        let (width, height) = (f64::from(imp.width.get()), f64::from(imp.height.get()));
        if let Some(adjustment) = imp.hadjustment.borrow().as_ref() {
            adjustment.configure(0.0, 0.0, width, SCROLL_STEP, width * 0.9, width);
        }
        if let Some(adjustment) = imp.vadjustment.borrow().as_ref() {
            let upper = f64::from(imp.upper.get()).max(height);
            adjustment.configure(adjustment.value().clamp(0.0, upper - height), 0.0, upper, SCROLL_STEP, height * 0.9, height);
        }
    }

    /// The book at the top of the view and how far through its row the view starts, so a change
    /// of column count keeps the same books on screen.
    fn scroll_anchor(&self) -> Option<(usize, f64)> {
        let imp = self.imp();
        let value = imp.vadjustment.borrow().as_ref().map_or(0.0, gtk::Adjustment::value);
        if value <= 0.0 {
            return None;
        }
        let rows = imp.rows.borrow();
        let start = rows.partition_point(|row| f64::from(row.y + row.height) <= value);
        rows[start..].iter().find_map(|row| match row.kind {
            RowKind::Books(first, _) => Some((first, (value - f64::from(row.y)) / f64::from(row.height.max(1)))),
            RowKind::Heading(_) => None,
        })
    }

    fn restore_anchor(&self, anchor: Option<(usize, f64)>) {
        let imp = self.imp();
        let Some((position, through)) = anchor else { return };
        let Some(row) = imp.book_rows.borrow().get(position).and_then(|row| imp.rows.borrow().get(*row as usize).copied()) else { return };
        if let Some(adjustment) = imp.vadjustment.borrow().as_ref() {
            adjustment.set_value(f64::from(row.y) + through * f64::from(row.height));
        }
    }

    fn release_all(&self) {
        let imp = self.imp();
        let released: Vec<Slot> = imp.mounted.borrow_mut().drain().map(|(_, slot)| slot).collect();
        for slot in released {
            self.free_slot(slot);
        }
        let headings: Vec<HeadingView> = imp.mounted_headings.borrow_mut().drain().map(|(_, view)| view).collect();
        imp.free_headings.borrow_mut().extend(headings);
    }

    fn free_slot(&self, slot: Slot) {
        let imp = self.imp();
        if let Some(thumbs) = imp.thumbs.borrow().as_ref() {
            slot.release(thumbs);
        }
        match slot {
            Slot::Tile(_) => imp.free_tiles.borrow_mut().push(slot),
            Slot::Row(_) => imp.free_rows.borrow_mut().push(slot),
        }
    }

    fn new_slot(&self) -> Slot {
        let imp = self.imp();
        let (slot, id) = if imp.list.get() {
            let row = BookRow::new();
            let id = row.book_id.clone();
            (Slot::Row(row), id)
        } else {
            let tile = BookTile::new();
            let id = tile.book_id.clone();
            (Slot::Tile(tile), id)
        };
        let root = slot.root();
        root.set_parent(self);
        root.set_focusable(true);
        if let Some(prepare) = imp.prepare.borrow().clone() {
            prepare(&root, &id);
        }
        // Tab can land on a tile that's only partly in view.
        let focus = gtk::EventControllerFocus::new();
        let canvas = self.downgrade();
        let id_for_focus = id;
        focus.connect_enter(move |controller| {
            let Some(canvas) = canvas.upgrade() else { return };
            let window = controller.widget().and_then(|widget| widget.root()).and_downcast::<gtk::Window>();
            if window.is_some_and(|window| gtk::prelude::GtkWindowExt::gets_focus_visible(&window)) {
                if let Some(position) = canvas.position_of(&id_for_focus.borrow()) {
                    canvas.reveal(position);
                }
            }
        });
        root.add_controller(focus);
        slot
    }

    fn apply_selected(&self, position: usize, slot: &Slot) {
        let selected = self.imp().selected.borrow();
        let is_selected = selected.as_deref().is_some_and(|id| self.imp().books.borrow().get(position).is_some_and(|book| book.id == id));
        let root = slot.root();
        if is_selected {
            root.add_css_class("selected");
        } else {
            root.remove_css_class("selected");
        }
    }

    /// Makes the mounted widgets match what's near the view.
    fn sync_mounted(&self) {
        let imp = self.imp();
        let value = imp.vadjustment.borrow().as_ref().map_or(0.0, gtk::Adjustment::value);
        let (top, bottom) = (value - OVERSCAN, value + f64::from(imp.height.get()) + OVERSCAN);
        let (mut wanted_books, mut wanted_headings) = (Vec::new(), Vec::new());
        {
            let rows = imp.rows.borrow();
            let first = rows.partition_point(|row| f64::from(row.y + row.height) <= top);
            let last = rows.partition_point(|row| f64::from(row.y) < bottom);
            for row in &rows[first..last.max(first)] {
                match row.kind {
                    RowKind::Heading(heading) => wanted_headings.push(heading),
                    RowKind::Books(start, count) => wanted_books.extend(start..start + count),
                }
            }
        }

        // The focused tile stays, even off screen, so scrolling never drops keyboard focus.
        let leaving: Vec<usize> = imp.mounted.borrow().iter().filter(|(position, slot)| !wanted_books.contains(position) && !slot.root().has_focus()).map(|(position, _)| *position).collect();
        for position in leaving {
            if let Some(slot) = imp.mounted.borrow_mut().remove(&position) {
                self.free_slot(slot);
            }
        }
        let leaving: Vec<usize> = imp.mounted_headings.borrow().keys().filter(|heading| !wanted_headings.contains(heading)).copied().collect();
        for heading in leaving {
            if let Some(view) = imp.mounted_headings.borrow_mut().remove(&heading) {
                imp.free_headings.borrow_mut().push(view);
            }
        }

        let thumbs = imp.thumbs.borrow().clone();
        let books_list = imp.books.borrow();
        for position in wanted_books {
            if imp.mounted.borrow().contains_key(&position) {
                continue;
            }
            let Some(book) = books_list.get(position) else { continue };
            let recycled = if imp.list.get() { imp.free_rows.borrow_mut().pop() } else { imp.free_tiles.borrow_mut().pop() };
            let slot = recycled.unwrap_or_else(|| self.new_slot());
            if let Some(thumbs) = &thumbs {
                match &slot {
                    Slot::Tile(tile) => tile.bind(book, thumbs, true),
                    Slot::Row(row) => {
                        row.bind(book, thumbs, &imp.index.borrow());
                        row.set_available_width(imp.width.get());
                    }
                }
            }
            slot.root().set_visible(true);
            self.apply_selected(position, &slot);
            imp.mounted.borrow_mut().insert(position, slot);
        }

        let headings = imp.headings.borrow();
        for index in wanted_headings {
            if imp.mounted_headings.borrow().contains_key(&index) {
                continue;
            }
            let view = imp.free_headings.borrow_mut().pop().unwrap_or_else(|| {
                let view = HeadingView::new();
                view.root.set_parent(self);
                view
            });
            view.bind(&headings[index]);
            view.root.set_visible(true);
            imp.mounted_headings.borrow_mut().insert(index, view);
        }

        // Hidden last, so a widget reused in this same pass doesn't flicker or lose its size.
        for slot in imp.free_tiles.borrow().iter().chain(imp.free_rows.borrow().iter()) {
            slot.root().set_visible(false);
        }
        for view in imp.free_headings.borrow().iter() {
            view.root.set_visible(false);
        }
    }

    fn place_children(&self) {
        let imp = self.imp();
        let value = imp.vadjustment.borrow().as_ref().map_or(0.0, gtk::Adjustment::value);
        let fit = self.fit();
        let inner = imp.width.get() - 2 * SIDE_PADDING;
        let rows = imp.rows.borrow();
        let place = |widget: &gtk::Widget, x: i32, y: i32, width: i32, height: i32| {
            // GTK requires a measure before every allocation.
            let _ = widget.measure(gtk::Orientation::Horizontal, -1);
            let transform = gsk::Transform::new().translate(&Point::new(x as f32, (f64::from(y) - value) as f32));
            widget.allocate(width, height, -1, Some(transform));
        };
        let book_rows = imp.book_rows.borrow();
        for (position, slot) in imp.mounted.borrow().iter() {
            let Some(row) = book_rows.get(*position).and_then(|row| rows.get(*row as usize)) else { continue };
            let RowKind::Books(first, _) = row.kind else { continue };
            if imp.list.get() {
                place(&slot.root(), SIDE_PADDING, row.y, inner, row.height);
            } else {
                let column = (*position - first) as i32;
                place(&slot.root(), SIDE_PADDING + fit.x(column), row.y, fit.width(column), row.height);
            }
        }
        let heading_rows = imp.heading_rows.borrow();
        for (index, view) in imp.mounted_headings.borrow().iter() {
            let Some(row) = heading_rows.get(*index).and_then(|row| rows.get(*row as usize)) else { continue };
            place(view.root.upcast_ref(), SIDE_PADDING, row.y, inner, row.height);
        }
    }

    fn set_vadjustment_inner(&self, adjustment: Option<gtk::Adjustment>) {
        let imp = self.imp();
        if let (Some(old), Some(handler)) = (imp.vadjustment.borrow().as_ref(), imp.scrolled.borrow_mut().take()) {
            old.disconnect(handler);
        }
        if let Some(adjustment) = &adjustment {
            let canvas = self.downgrade();
            let handler = adjustment.connect_value_changed(move |_| {
                if let Some(canvas) = canvas.upgrade() {
                    canvas.sync_mounted();
                    canvas.queue_allocate();
                }
            });
            imp.scrolled.replace(Some(handler));
        }
        imp.vadjustment.replace(adjustment);
        self.queue_allocate();
    }

    /// Where an arrow key goes from `position`. Up and Down keep the column, skipping headings.
    fn arrow_target(&self, position: usize, key: gdk::Key) -> Option<usize> {
        let imp = self.imp();
        let total = imp.books.borrow().len();
        match key {
            gdk::Key::Left => position.checked_sub(1),
            gdk::Key::Right => (position + 1 < total).then_some(position + 1),
            gdk::Key::Up | gdk::Key::Down => {
                let rows = imp.rows.borrow();
                let here = *imp.book_rows.borrow().get(position)? as usize;
                let RowKind::Books(first, _) = rows[here].kind else { return None };
                let column = position - first;
                let target_row = |row: &Row| match row.kind {
                    RowKind::Books(start, count) => Some(start + column.min(count - 1)),
                    RowKind::Heading(_) => None,
                };
                if key == gdk::Key::Up {
                    rows[..here].iter().rev().find_map(target_row)
                } else {
                    rows[here + 1..].iter().find_map(target_row)
                }
            }
            _ => None,
        }
    }

    fn attach_keys(&self) {
        let key = gtk::EventControllerKey::new();
        let canvas = self.downgrade();
        key.connect_key_pressed(move |_, key, _, state| {
            let Some(canvas) = canvas.upgrade() else { return glib::Propagation::Proceed };
            if state.intersects(gdk::ModifierType::ALT_MASK | gdk::ModifierType::CONTROL_MASK) {
                return glib::Propagation::Proceed;
            }
            let Some(position) = canvas.focused_position() else { return glib::Propagation::Proceed };
            match canvas.arrow_target(position, key) {
                Some(target) => {
                    canvas.focus_position(target);
                    glib::Propagation::Stop
                }
                // At an end of the list, the key still belongs to the books.
                None if matches!(key, gdk::Key::Left | gdk::Key::Right | gdk::Key::Up | gdk::Key::Down) => glib::Propagation::Stop,
                None => glib::Propagation::Proceed,
            }
        });
        self.add_controller(key);
    }

    #[cfg(feature = "dev-tools")]
    pub fn dev_arrow(&self, position: usize, key: gdk::Key) -> Option<usize> {
        self.arrow_target(position, key)
    }

    /// Where each mounted tile sits, for checking that rows end at the right edge.
    #[cfg(feature = "dev-tools")]
    pub fn dev_tile_edges(&self) -> Vec<(i32, i32, i32)> {
        let imp = self.imp();
        let mut edges: Vec<(i32, i32, i32)> = imp
            .mounted
            .borrow()
            .values()
            .filter_map(|slot| slot.root().compute_bounds(self).map(|bounds| (bounds.x().round() as i32, bounds.width().round() as i32, bounds.height().round() as i32)))
            .collect();
        edges.sort_unstable();
        edges
    }

    #[cfg(feature = "dev-tools")]
    pub fn dev_mounted(&self) -> usize {
        self.imp().mounted.borrow().len()
    }
}

mod grid_imp {
    use super::*;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub struct FluidGrid {
        pub min_width: Cell<i32>,
        pub ratio: Cell<f64>,
    }

    impl FluidGrid {
        fn children(&self) -> Vec<gtk::Widget> {
            let mut found = Vec::new();
            let mut child = self.obj().first_child();
            while let Some(current) = child {
                child = current.next_sibling();
                if current.should_layout() {
                    found.push(current);
                }
            }
            found
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FluidGrid {
        const NAME: &'static str = "BooklubFluidGrid";
        type Type = super::FluidGrid;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for FluidGrid {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for FluidGrid {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if orientation == gtk::Orientation::Horizontal {
                return (0, self.min_width.get(), -1, -1);
            }
            let count = self.children().len() as i32;
            if count == 0 {
                return (0, 0, -1, -1);
            }
            let available = if for_size < 0 { self.min_width.get() } else { for_size };
            let fit = Fit::new(available, self.min_width.get(), TILE_GAP);
            let lines = (count + fit.columns - 1) / fit.columns;
            let height = lines * fit.height(self.ratio.get()) + (lines - 1) * TILE_GAP;
            (height, height, -1, -1)
        }

        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            let fit = Fit::new(width, self.min_width.get(), TILE_GAP);
            let tile_height = fit.height(self.ratio.get());
            for (index, child) in self.children().iter().enumerate() {
                let (column, line) = (index as i32 % fit.columns, index as i32 / fit.columns);
                let _ = child.measure(gtk::Orientation::Horizontal, -1);
                let transform = gsk::Transform::new().translate(&Point::new(fit.x(column) as f32, (line * (tile_height + TILE_GAP)) as f32));
                child.allocate(fit.width(column), tile_height, -1, Some(transform));
            }
        }
    }
}

glib::wrapper! {
    pub struct FluidGrid(ObjectSubclass<grid_imp::FluidGrid>) @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl FluidGrid {
    pub fn new(min_width: i32, ratio: f64) -> Self {
        let grid: Self = glib::Object::new();
        grid.imp().min_width.set(min_width);
        grid.imp().ratio.set(ratio);
        grid
    }

    pub fn set_min_width(&self, min_width: i32) {
        self.imp().min_width.set(min_width);
        self.queue_resize();
    }

    pub fn append(&self, child: &impl IsA<gtk::Widget>) {
        child.set_parent(self);
    }

    pub fn clear(&self) {
        while let Some(child) = self.first_child() {
            child.unparent();
        }
    }

    #[cfg(feature = "dev-tools")]
    pub fn dev_child_count(&self) -> usize {
        let mut count = 0;
        let mut child = self.first_child();
        while let Some(current) = child {
            count += 1;
            child = current.next_sibling();
        }
        count
    }

    /// The first child's width, and the columns that width implies.
    #[cfg(feature = "dev-tools")]
    pub fn dev_fit(&self) -> Fit {
        Fit::new(self.width(), self.imp().min_width.get(), TILE_GAP)
    }
}

/// The bar under the books: how many there are, and the size slider.
pub fn status_bar(scale: f64) -> (gtk::Box, gtk::Label, gtk::Box, gtk::Scale) {
    use booklub_core::preferences::{MAX_TILE_SCALE, MIN_TILE_SCALE};
    let bar = hbox(8);
    bar.add_css_class("status-bar");
    let count = label("", &["caption"]);
    count.set_hexpand(true);
    bar.append(&count);

    let sizing = hbox(6);
    let smaller = gtk::Image::builder().icon_name("booklub-tile-small-symbolic").pixel_size(16).css_classes(["status-glyph"]).build();
    let larger = gtk::Image::builder().icon_name("booklub-tile-large-symbolic").pixel_size(16).css_classes(["status-glyph"]).build();
    let slider = gtk::Scale::with_range(gtk::Orientation::Horizontal, MIN_TILE_SCALE, MAX_TILE_SCALE, 0.05);
    slider.set_value(scale);
    slider.set_draw_value(false);
    slider.set_width_request(140);
    slider.set_tooltip_text(Some("Tile size"));
    slider.update_property(&[gtk::accessible::Property::Label("Tile size")]);
    sizing.append(&smaller);
    sizing.append(&slider);
    sizing.append(&larger);
    bar.append(&sizing);
    (bar, count, sizing, slider)
}
