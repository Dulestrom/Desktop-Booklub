//! The two shapes the whole design rests on: books are vertical, categories horizontal.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use booklub_core::query::CategoryIndex;
use booklub_core::rules::{completion_percent, has_progress, initials};
use booklub_core::{Book, Priority, Status};
use gtk::{gdk, glib};

use crate::thumbs::Thumbs;
use crate::util::{books, hbox, icon, label, progress_bar, vbox};

/// At the default size. The real size comes from the space available (REQ-46).
pub const BOOK_WIDTH: i32 = 148;
pub const BOOK_HEIGHT: i32 = 222;
pub const CATEGORY_WIDTH: i32 = 216;
pub const CATEGORY_HEIGHT: i32 = 122;
const THUMB_WIDTH: i32 = 34;
const THUMB_HEIGHT: i32 = 51;

/// An overlay that is exactly `width` by `height`. It measures only an empty base box, so the
/// picture on top can never stretch the tile to its image's natural size.
pub fn sized_tile(width: i32, height: i32, class: &str) -> (gtk::Overlay, gtk::Picture) {
    let base = gtk::Box::builder().build();
    base.set_size_request(width, height);
    let root = gtk::Overlay::builder().child(&base).css_classes([class]).overflow(gtk::Overflow::Hidden).halign(gtk::Align::Start).valign(gtk::Align::Start).build();
    let picture = gtk::Picture::builder().content_fit(gtk::ContentFit::Cover).can_shrink(true).build();
    root.add_overlay(&picture);
    (root, picture)
}

/// An overlay that takes whatever size its parent gives it, with a focus ring and a button's role.
fn fluid_tile(class: &str) -> (gtk::Overlay, gtk::Picture) {
    let root = gtk::Overlay::builder()
        .child(&gtk::Box::builder().build())
        .css_classes([class])
        .overflow(gtk::Overflow::Hidden)
        .accessible_role(gtk::AccessibleRole::Button)
        .focusable(true)
        .build();
    let picture = gtk::Picture::builder().content_fit(gtk::ContentFit::Cover).can_shrink(true).build();
    root.add_overlay(&picture);
    (root, picture)
}

/// A thin progress bar along a tile's bottom edge. It sizes itself, whatever the tile's width.
fn tile_bar(fraction: f64, finished: bool) -> gtk::ProgressBar {
    let bar = gtk::ProgressBar::builder().fraction(fraction).css_classes(["tile-bar"]).build();
    if finished {
        bar.add_css_class("finished");
    }
    bar
}

/// One book tile, 148 x 222 at the default size. The same widgets are reused as books scroll past.
#[derive(Clone)]
pub struct BookTile {
    pub root: gtk::Overlay,
    pub picture: gtk::Picture,
    initials: gtk::Label,
    title: gtk::Label,
    author: gtk::Label,
    chip: gtk::Label,
    mark: gtk::Box,
    bar: gtk::ProgressBar,
    pub book_id: Rc<RefCell<String>>,
}

impl BookTile {
    pub fn new() -> Self {
        let (root, picture) = fluid_tile("book-tile");

        let initials = label("", &["tile-initials"]);
        initials.set_halign(gtk::Align::Center);
        // Centred in the space above the title, at any tile height.
        initials.set_valign(gtk::Align::Center);
        initials.set_margin_bottom(60);
        root.add_overlay(&initials);

        let title = label("", &["tile-title"]);
        title.set_wrap(true);
        title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        title.set_lines(2);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_width_chars(1);
        title.set_max_width_chars(1);
        title.set_hexpand(true);
        let author = label("", &["tile-author"]);
        author.set_ellipsize(gtk::pango::EllipsizeMode::End);
        author.set_width_chars(1);
        author.set_max_width_chars(1);
        author.set_hexpand(true);

        let text = vbox(2);
        text.add_css_class("tile-text");
        text.append(&title);
        text.append(&author);

        let bar = tile_bar(0.0, false);

        let bottom = vbox(0);
        bottom.set_valign(gtk::Align::End);
        bottom.append(&text);
        bottom.append(&bar);
        root.add_overlay(&bottom);

        let chip = label("", &["status-chip", "chip-text"]);
        chip.set_halign(gtk::Align::Start);
        chip.set_valign(gtk::Align::Start);
        chip.set_margin_start(8);
        chip.set_margin_top(8);
        root.add_overlay(&chip);

        let mark = gtk::Box::builder().css_classes(["priority-mark"]).halign(gtk::Align::End).valign(gtk::Align::Start).margin_end(8).margin_top(8).build();
        let glyph = icon("priority-high");
        glyph.set_hexpand(true);
        mark.append(&glyph);
        mark.set_tooltip_text(Some("High priority"));
        root.add_overlay(&mark);

        // Drawn over everything, so a 3 px selection border never shifts the layout.
        let frame = gtk::Box::builder().css_classes(["tile-frame"]).can_target(false).build();
        root.add_overlay(&frame);

        Self { root, picture, initials, title, author, chip, mark, bar, book_id: Rc::default() }
    }

    pub fn bind(&self, book: &Book, thumbs: &Thumbs, in_grid: bool) {
        *self.book_id.borrow_mut() = book.id.clone();

        match &book.background_file {
            Some(file) => {
                self.root.add_css_class("has-image");
                self.initials.set_visible(false);
                if in_grid {
                    thumbs.request(file, &self.picture);
                } else {
                    thumbs.request_now(file, &self.picture);
                }
            }
            None => {
                self.root.remove_css_class("has-image");
                self.initials.set_visible(true);
                self.initials.set_label(&initials(&book.title));
                thumbs.release(&self.picture);
            }
        }

        self.title.set_label(&book.title);
        self.author.set_label(&book.author);
        self.author.set_visible(!book.author.is_empty());
        self.chip.set_label(&book.status.as_str().to_uppercase());
        self.mark.set_visible(book.priority == Priority::High);

        self.bar.set_visible(has_progress(book));
        let percent = completion_percent(book) as i32;
        self.bar.set_fraction(f64::from(percent) / 100.0);
        if book.status == Status::Finished {
            self.bar.add_css_class("finished");
        } else {
            self.bar.remove_css_class("finished");
        }

        // QA-35: everything colour says here is also said in the accessible name.
        let progress = if has_progress(book) { format!(", {percent}% read") } else { String::new() };
        let priority = if book.priority == Priority::High { ", high priority" } else { "" };
        self.root.update_property(&[gtk::accessible::Property::Label(&format!(
            "{} by {}, {}{progress}{priority}",
            book.title,
            if book.author.is_empty() { "unknown author" } else { &book.author },
            book.status.as_str(),
        ))]);
    }
}

/// One book in List view (REQ-48). Columns that don't fit are hidden, least important first.
#[derive(Clone)]
pub struct BookRow {
    pub root: gtk::Box,
    pub picture: gtk::Picture,
    cover: gtk::Overlay,
    initials: gtk::Label,
    title: gtk::Label,
    author: gtk::Label,
    category_column: gtk::Box,
    category: gtk::Label,
    sub_category: gtk::Label,
    status: gtk::Label,
    priority_column: gtk::Box,
    priority_mark: gtk::Image,
    priority: gtk::Label,
    progress_column: gtk::Box,
    bar: gtk::ProgressBar,
    percent: gtk::Label,
    pub book_id: Rc<RefCell<String>>,
}

impl BookRow {
    pub fn new() -> Self {
        let root = gtk::Box::builder().spacing(12).css_classes(["book-row"]).accessible_role(gtk::AccessibleRole::Button).focusable(true).build();
        let (cover, picture) = sized_tile(THUMB_WIDTH, THUMB_HEIGHT, "row-cover");
        cover.set_valign(gtk::Align::Center);
        let initials = label("", &["row-initials"]);
        initials.set_halign(gtk::Align::Center);
        initials.set_valign(gtk::Align::Center);
        cover.add_overlay(&initials);
        root.append(&cover);

        let squeeze = |text: &gtk::Label| {
            text.set_ellipsize(gtk::pango::EllipsizeMode::End);
            text.set_width_chars(1);
            // With no natural width of their own, every row's columns line up.
            text.set_max_width_chars(1);
            text.set_hexpand(true);
        };
        let title = label("", &["body-strong"]);
        squeeze(&title);
        let author = label("", &["caption"]);
        squeeze(&author);
        let names = vbox(2);
        names.set_hexpand(true);
        names.set_valign(gtk::Align::Center);
        names.append(&title);
        names.append(&author);
        root.append(&names);

        let category = label("", &["caption-2"]);
        squeeze(&category);
        let sub_category = label("", &["caption"]);
        squeeze(&sub_category);
        let category_column = vbox(2);
        // Set outright, or the labels' expanding would spread to the column and squeeze the title.
        category_column.set_hexpand(false);
        category_column.set_width_request(170);
        category_column.set_valign(gtk::Align::Center);
        category_column.append(&category);
        category_column.append(&sub_category);
        root.append(&category_column);

        let status = label("", &["caption-2"]);
        status.set_width_request(66);
        root.append(&status);

        let priority_mark = icon("priority-high");
        priority_mark.add_css_class("warning-ink");
        let priority = label("", &["caption-2"]);
        let priority_column = hbox(2);
        priority_column.set_width_request(74);
        priority_column.append(&priority_mark);
        priority_column.append(&priority);
        root.append(&priority_column);

        let bar = progress_bar(0, "");
        let percent = label("", &["caption"]);
        percent.set_width_request(36);
        percent.set_xalign(1.0);
        let progress_column = hbox(8);
        progress_column.set_hexpand(false);
        progress_column.set_width_request(130);
        progress_column.append(&bar);
        progress_column.append(&percent);
        root.append(&progress_column);

        Self { root, picture, cover, initials, title, author, category_column, category, sub_category, status, priority_column, priority_mark, priority, progress_column, bar, percent, book_id: Rc::default() }
    }

    pub fn set_available_width(&self, width: i32) {
        self.progress_column.set_visible(width >= 620);
        self.category_column.set_visible(width >= 520);
        self.priority_column.set_visible(width >= 740);
        self.status.set_visible(width >= 400);
    }

    pub fn bind(&self, book: &Book, thumbs: &Thumbs, index: &CategoryIndex) {
        *self.book_id.borrow_mut() = book.id.clone();
        match &book.background_file {
            Some(file) => {
                self.cover.add_css_class("has-image");
                self.initials.set_visible(false);
                thumbs.request(file, &self.picture);
            }
            None => {
                self.cover.remove_css_class("has-image");
                self.initials.set_visible(true);
                self.initials.set_label(&initials(&book.title));
                thumbs.release(&self.picture);
            }
        }
        self.title.set_label(&book.title);
        self.author.set_label(&book.author);
        self.author.set_visible(!book.author.is_empty());

        let category = index.category_name(book.category_id.as_deref());
        let sub_category = index.sub_category_name(book.sub_category_id.as_deref());
        self.category.set_label(if category.is_empty() { "Uncategorized" } else { category });
        self.sub_category.set_label(sub_category);
        self.sub_category.set_visible(!sub_category.is_empty());

        self.status.set_label(book.status.as_str());
        self.priority.set_label(book.priority.as_str());
        self.priority_mark.set_visible(book.priority == Priority::High);

        let percent = completion_percent(book) as i32;
        let tracked = has_progress(book);
        self.bar.set_visible(tracked);
        self.percent.set_visible(tracked);
        self.bar.set_fraction(f64::from(percent) / 100.0);
        self.percent.set_label(&format!("{percent}%"));
        if book.status == Status::Finished {
            self.bar.add_css_class("success");
        } else {
            self.bar.remove_css_class("success");
        }

        let progress = if tracked { format!(", {percent}% read") } else { String::new() };
        self.root.update_property(&[gtk::accessible::Property::Label(&format!(
            "{} by {}, {}, {} priority{progress}",
            book.title,
            if book.author.is_empty() { "unknown author" } else { &book.author },
            book.status.as_str(),
            book.priority.as_str(),
        ))]);
    }
}

/// Wires drag and drop onto a tile. `current` reports the tile's id and whether reordering is
/// allowed right now (REQ-08g), and `on_drop` receives the dragged id and the target id.
pub fn attach_reorder(
    widget: &impl IsA<gtk::Widget>,
    current: impl Fn() -> Option<String> + 'static,
    on_drop: impl Fn(String, String) + 'static,
) {
    let widget = widget.upcast_ref::<gtk::Widget>();
    let current = Rc::new(current);
    let on_drop: Rc<dyn Fn(String, String)> = Rc::new(on_drop);

    let source = gtk::DragSource::new();
    source.set_actions(gdk::DragAction::MOVE);
    let id_source = current.clone();
    source.connect_prepare(move |_, _, _| id_source().map(|id| gdk::ContentProvider::for_value(&id.to_value())));
    let dragged = widget.clone();
    source.connect_drag_begin(move |source, _| {
        let paintable = gtk::WidgetPaintable::new(Some(&dragged));
        source.set_icon(Some(&paintable), dragged.width() / 2, dragged.height() / 2);
        dragged.add_css_class("dragging");
    });
    let dragged = widget.clone();
    source.connect_drag_end(move |_, _, _| dragged.remove_css_class("dragging"));
    widget.add_controller(source);

    let target = gtk::DropTarget::new(String::static_type(), gdk::DragAction::MOVE);
    let hovered = widget.clone();
    target.connect_enter(move |_, _, _| {
        hovered.add_css_class("drop-target");
        gdk::DragAction::MOVE
    });
    let hovered = widget.clone();
    target.connect_leave(move |_| hovered.remove_css_class("drop-target"));
    let hovered = widget.clone();
    target.connect_drop(move |_, value, _, _| {
        hovered.remove_css_class("drop-target");
        let (Ok(from), Some(to)) = (value.get::<String>(), current()) else { return false };
        if from == to {
            return false;
        }
        // The grid repopulates inside the handler, so finish the drag first.
        let on_drop = on_drop.clone();
        glib::idle_add_local_once(move || on_drop(from, to));
        true
    });
    widget.add_controller(target);
}

pub struct CategoryArt<'a> {
    /// An image key for `Thumbs`, from `App::category_art`.
    pub key: Option<&'a str>,
    pub thumbs: &'a Thumbs,
}

/// The parts of a Category tile that change with its artwork.
pub struct CategoryTile {
    pub root: gtk::Overlay,
    pub picture: gtk::Picture,
    scrim: gtk::Box,
}

impl CategoryTile {
    pub fn set_has_art(&self, has_art: bool) {
        self.scrim.set_visible(has_art);
        if has_art {
            self.root.add_css_class("has-art");
        } else {
            self.root.remove_css_class("has-art");
        }
    }
}

/// A landscape Category tile, 216 x 122 (REQ-32), with its artwork applied (REQ-45f).
pub fn category_tile(name: &str, count: usize, finished: usize, glyph: Option<&str>, art: CategoryArt) -> gtk::Overlay {
    let tile = category_tile_parts(name, count, finished, glyph);
    tile.set_has_art(art.key.is_some());
    if let Some(key) = art.key {
        art.thumbs.request_now(key, &tile.picture);
    }
    tile.root
}

/// Full-bleed artwork under a scrim when it has any, the raised surface otherwise, and a 3 px
/// finished share along the bottom edge.
pub fn category_tile_parts(name: &str, count: usize, finished: usize, glyph: Option<&str>) -> CategoryTile {
    let (root, picture) = fluid_tile("category-tile");
    let scrim = gtk::Box::builder().css_classes(["category-scrim"]).can_target(false).visible(false).build();
    root.add_overlay(&scrim);

    if let Some(glyph) = glyph {
        let image = icon(glyph);
        image.add_css_class("category-glyph");
        image.set_pixel_size(24);
        image.set_halign(gtk::Align::Start);
        image.set_valign(gtk::Align::Start);
        image.set_margin_start(14);
        image.set_margin_top(14);
        root.add_overlay(&image);
    }

    let text = vbox(1);
    text.set_valign(gtk::Align::End);
    text.set_margin_start(14);
    text.set_margin_end(14);
    text.set_margin_bottom(17);
    let title = label(name, &["category-name"]);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_width_chars(1);
    title.set_max_width_chars(1);
    title.set_hexpand(true);
    text.append(&title);
    text.append(&label(&format!("{} · {finished} finished", books(count)), &["category-count"]));
    root.add_overlay(&text);

    let share = if count > 0 { finished as f64 / count as f64 } else { 0.0 };
    let bar = tile_bar(share, true);
    bar.set_valign(gtk::Align::End);
    root.add_overlay(&bar);

    let frame = gtk::Box::builder().css_classes(["tile-frame"]).can_target(false).build();
    root.add_overlay(&frame);

    root.update_property(&[gtk::accessible::Property::Label(&format!("{name}, {}, {finished} finished", books(count)))]);
    CategoryTile { root, picture, scrim }
}

/// The dashed New tile that ends a strip of Category or Sub-category tiles. A size of -1 leaves it
/// to take whatever its parent gives it.
pub fn new_tile(text: &str, width: i32, height: i32, on_click: impl Fn() + 'static) -> gtk::Button {
    let row = hbox(8);
    row.set_halign(gtk::Align::Center);
    row.append(&icon("add"));
    row.append(&gtk::Label::new(Some(text)));
    let button = gtk::Button::builder().child(&row).css_classes(["tile-new"]).build();
    button.set_size_request(width, height);
    button.connect_clicked(move |_| on_click());
    button
}

/// A Sub-category tile, 180 x 60. Selection adds a check glyph and a 3 px border, so it never
/// relies on colour alone.
pub fn sub_tile(name: &str, count: usize, selected: bool, muted: bool, on_click: impl Fn() + 'static) -> gtk::Button {
    let content = vbox(2);
    content.set_valign(gtk::Align::Center);
    let row = hbox(6);
    if selected {
        row.append(&icon("check"));
    }
    let title = label(name, &["sub-name"]);
    if muted {
        title.add_css_class("muted");
    }
    row.append(&title);
    content.append(&row);
    content.append(&label(&books(count), &["sub-count"]));

    let button = gtk::Button::builder().child(&content).css_classes(["sub-tile"]).build();
    button.set_size_request(180, 60);
    button.set_valign(gtk::Align::Start);
    if selected {
        button.add_css_class("selected");
    }
    button.update_property(&[gtk::accessible::Property::Label(&format!(
        "{name}, {}{}",
        books(count),
        if selected { ", selected" } else { "" }
    ))]);
    button.connect_clicked(move |_| on_click());
    button
}
