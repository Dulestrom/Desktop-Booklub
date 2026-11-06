//! The inspector: every editable attribute of a book, persisted only on Save (REQ-44b).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use booklub_core::background::{self, CheckedImage, ImageError};
use booklub_core::query::CategoryIndex;
use booklub_core::rules::{self, completion_percent, has_progress, initials, FieldErrors};
use booklub_core::{isbn, Book, Priority, Status};
use gtk::{gio, glib};

use crate::app::App;
use crate::util::{self, clear, feedback, hbox, icon, label, text_button, vbox};
use crate::window::refresh;

/// What will happen to the background on Save (REQ-35r).
#[derive(Clone, Default)]
enum PendingBackground {
    #[default]
    Unchanged,
    Set(Rc<CheckedImage>),
    Remove,
}

struct Field {
    root: gtk::Box,
    entry: gtk::Entry,
    error: gtk::Label,
}

impl Field {
    fn new(text: &str) -> Self {
        let root = vbox(6);
        root.set_hexpand(true);
        root.append(&label(text, &["field-label"]));
        let entry = gtk::Entry::builder().css_classes(["field-box"]).hexpand(true).width_chars(4).build();
        entry.update_property(&[gtk::accessible::Property::Label(text)]);
        root.append(&entry);
        let error = label("", &["field-error"]);
        error.set_wrap(true);
        error.set_visible(false);
        root.append(&error);
        Self { root, entry, error }
    }

    fn show_error(&self, message: Option<&str>) {
        self.error.set_label(message.unwrap_or_default());
        self.error.set_visible(message.is_some());
        if message.is_some() {
            self.entry.add_css_class("invalid");
        } else {
            self.entry.remove_css_class("invalid");
        }
    }
}

/// A dropdown whose closed face ellipsizes, because a long category name would otherwise widen
/// the whole 320 px inspector. The open list keeps full names.
fn dropdown(items: &[&str]) -> gtk::DropDown {
    let face = gtk::SignalListItemFactory::new();
    face.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        let text = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).width_chars(1).max_width_chars(1).hexpand(true).build();
        item.set_child(Some(&text));
    });
    face.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        let (Some(text), Some(value)) = (item.child().and_downcast::<gtk::Label>(), item.item().and_downcast::<gtk::StringObject>()) else { return };
        text.set_label(&value.string());
    });
    let list = gtk::SignalListItemFactory::new();
    list.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        item.set_child(Some(&gtk::Label::builder().xalign(0.0).build()));
    });
    list.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        let (Some(text), Some(value)) = (item.child().and_downcast::<gtk::Label>(), item.item().and_downcast::<gtk::StringObject>()) else { return };
        text.set_label(&value.string());
    });
    gtk::DropDown::builder().model(&gtk::StringList::new(items)).factory(&face).list_factory(&list).hexpand(true).build()
}

fn drop_field(text: &str, widget: &gtk::DropDown) -> gtk::Box {
    let root = vbox(6);
    root.set_hexpand(true);
    root.append(&label(text, &["field-label"]));
    widget.update_property(&[gtk::accessible::Property::Label(text)]);
    root.append(widget);
    root
}

fn section(text: &str) -> gtk::Label {
    let heading = label(text, &["eyebrow"]);
    heading.set_margin_top(8);
    heading
}

fn pair(left: &impl IsA<gtk::Widget>, right: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = hbox(12);
    row.set_homogeneous(true);
    row.append(left);
    row.append(right);
    row
}

/// REQ-50: the read-only view of one book that a click shows. Editing is a separate page,
/// reached only through the tile's context menu (D-12).
struct Preview {
    cover: gtk::Picture,
    initials: gtk::Label,
    title: gtk::Label,
    author: gtk::Label,
    status: gtk::Label,
    status_icon: gtk::Image,
    priority: gtk::Label,
    priority_icon: gtk::Image,
    shelf: gtk::Label,
    progress_card: gtk::Box,
    pages: gtk::Label,
    percent: gtk::Label,
    bar: gtk::ProgressBar,
}

impl Preview {
    fn new() -> (Self, gtk::ScrolledWindow) {
        let (cover_host, cover) = crate::tiles::sized_tile(112, 168, "preview-cover");
        cover_host.set_halign(gtk::Align::Center);
        let initials = label("", &["tile-initials"]);
        initials.set_halign(gtk::Align::Center);
        initials.set_valign(gtk::Align::Center);
        cover_host.add_overlay(&initials);

        let centred = |text: &gtk::Label| {
            text.set_xalign(0.5);
            text.set_justify(gtk::Justification::Center);
            text.set_wrap(true);
            text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        };
        let title = label("", &["title-2"]);
        centred(&title);
        title.set_selectable(true);
        let author = label("", &["body", "accent-ink"]);
        centred(&author);

        let chip = |glyph: &str| {
            let image = icon(glyph);
            let text = label("", &["chip-label"]);
            let row = hbox(5);
            row.add_css_class("preview-chip");
            row.append(&image);
            row.append(&text);
            (row, image, text)
        };
        let (status_chip, status_icon, status) = chip("reading");
        let (priority_chip, priority_icon, priority) = chip("priority-high");
        let chips = hbox(6);
        chips.set_halign(gtk::Align::Center);
        chips.append(&status_chip);
        chips.append(&priority_chip);

        let shelf = label("", &["caption-2"]);
        centred(&shelf);

        let pages = label("", &["caption-2"]);
        pages.set_hexpand(true);
        let percent = label("", &["body-strong"]);
        let progress_top = hbox(8);
        progress_top.append(&pages);
        progress_top.append(&percent);
        let bar = util::progress_bar(0, "");
        let progress_card = vbox(8);
        progress_card.add_css_class("preview-card");
        progress_card.append(&progress_top);
        progress_card.append(&bar);

        let hint = hbox(6);
        hint.add_css_class("caption");
        hint.append(&icon("edit"));
        let hint_text = label("Right-click the tile, or press Shift+F10, to edit", &["caption"]);
        hint_text.set_wrap(true);
        hint_text.set_hexpand(true);
        hint.append(&hint_text);

        let body = vbox(10);
        body.add_css_class("inspector-body");
        body.append(&cover_host);
        body.append(&title);
        body.append(&author);
        body.append(&chips);
        body.append(&shelf);
        body.append(&progress_card);
        body.append(&hint);
        let scroller = gtk::ScrolledWindow::builder().child(&body).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
        (Self { cover, initials, title, author, status, status_icon, priority, priority_icon, shelf, progress_card, pages, percent, bar }, scroller)
    }

    fn fill(&self, book: &Book, index: &CategoryIndex, thumbs: &crate::thumbs::Thumbs) {
        match &book.background_file {
            Some(file) => {
                self.initials.set_visible(false);
                thumbs.request_now(file, &self.cover);
            }
            None => {
                thumbs.release(&self.cover);
                self.initials.set_visible(true);
                self.initials.set_label(&initials(&book.title));
            }
        }
        self.title.set_label(&book.title);
        self.author.set_label(&book.author);
        self.author.set_visible(!book.author.is_empty());

        self.status.set_label(book.status.as_str());
        let glyph = match book.status {
            Status::Reading => "reading",
            Status::ToRead => "to-read",
            Status::Finished => "finished",
        };
        self.status_icon.set_icon_name(Some(&format!("booklub-{glyph}-symbolic")));
        self.priority.set_label(&format!("{} priority", book.priority.as_str()));
        self.priority_icon.set_visible(book.priority == Priority::High);

        let category = index.category_name(book.category_id.as_deref());
        let sub_category = index.sub_category_name(book.sub_category_id.as_deref());
        self.shelf.set_label(&match (category.is_empty(), sub_category.is_empty()) {
            (true, _) => "Uncategorized".to_string(),
            (false, true) => category.to_string(),
            (false, false) => format!("{category} \u{203a} {sub_category}"),
        });

        self.progress_card.set_visible(has_progress(book));
        let percent = completion_percent(book);
        self.pages.set_label(&format!("Page {} of {}", book.current_page, book.total_pages.unwrap_or(0)));
        self.percent.set_label(&format!("{percent}%"));
        self.bar.set_fraction(f64::from(percent.min(100)) / 100.0);
        if book.status == Status::Finished {
            self.bar.add_css_class("success");
        } else {
            self.bar.remove_css_class("success");
        }
    }
}

pub struct Inspector {
    app: Rc<App>,
    root: adw::ToolbarView,
    pages: gtk::Stack,
    preview_page: Preview,
    /// The book the preview shows, when the preview is the page in view.
    previewing: RefCell<Option<String>>,
    heading: gtk::Label,
    draft: RefCell<Book>,
    original: RefCell<Book>,
    is_new: Cell<bool>,
    loading: Cell<bool>,
    /// A book is loaded. Separate from the pane showing, because an overlaid pane can be
    /// dismissed with edits still in it, and those must not be lost silently (REQ-44c).
    has_book: Cell<bool>,
    /// Bumped per load, so a slow image decode cannot paint over a newer book.
    generation: Cell<u64>,
    pending: RefCell<PendingBackground>,
    preview: gtk::Picture,
    preview_host: gtk::Overlay,
    empty_hint: gtk::Box,
    add_button: gtk::Button,
    remove_button: gtk::Button,
    bg_feedback: gtk::Box,
    title: Field,
    author: Field,
    category: gtk::DropDown,
    sub_category: gtk::DropDown,
    category_ids: RefCell<Vec<Option<String>>>,
    sub_ids: RefCell<Vec<Option<String>>>,
    isbn: Field,
    publisher: Field,
    year: Field,
    language: Field,
    status: gtk::DropDown,
    priority: gtk::DropDown,
    current_page: Field,
    total_pages: Field,
    progress_host: gtk::Box,
    progress_note: gtk::Box,
    start_date: Field,
    finish_date: Field,
    stars: Vec<gtk::Button>,
    rating_label: gtk::Label,
    notes: gtk::TextView,
    delete_box: gtk::Box,
    save_button: gtk::Button,
}

/// An entry with a calendar popover beside it (REQ-44e).
fn add_calendar(field: &Field, label_text: &str) {
    let calendar = gtk::Calendar::new();
    let popover = gtk::Popover::builder().child(&calendar).build();
    let button = gtk::MenuButton::builder().child(&icon("calendar")).popover(&popover).css_classes(["tool"]).tooltip_text(label_text).build();
    button.update_property(&[gtk::accessible::Property::Label(label_text)]);

    // Rebuild the field's row as entry + button, keeping the entry the Field already holds.
    let row = hbox(4);
    let parent = field.root.clone();
    parent.remove(&field.entry);
    row.append(&field.entry);
    row.append(&button);
    parent.insert_child_after(&row, parent.first_child().as_ref());

    let entry = field.entry.clone();
    calendar.connect_day_selected(move |calendar| {
        if let Ok(text) = calendar.date().format("%Y-%m-%d") {
            entry.set_text(&text);
        }
    });
    let entry = field.entry.clone();
    popover.connect_show(move |_| {
        if let Some(date) = parse_glib_date(&entry.text()) {
            calendar.set_date(&date);
        }
    });
}

fn parse_glib_date(text: &str) -> Option<glib::DateTime> {
    let mut parts = text.trim().split('-').map(str::parse::<i32>);
    let (Some(Ok(year)), Some(Ok(month)), Some(Ok(day))) = (parts.next(), parts.next(), parts.next()) else { return None };
    glib::DateTime::from_local(year, month, day, 0, 0, 0.0).ok()
}

impl Inspector {
    pub fn new(app: Rc<App>) -> Rc<Self> {
        let close = util::tool_button("close", "Close panel (Esc)");
        close.update_property(&[gtk::accessible::Property::Label("Close panel")]);
        let heading = label("Book", &["heading"]);
        heading.set_hexpand(true);
        // The top bar holds only the window's own buttons. The panel's close button sits on a
        // row of its own beneath them, so it can't be mistaken for closing the application.
        let header = adw::HeaderBar::builder().show_title(false).show_start_title_buttons(false).show_back_button(false).build();
        let title_row = hbox(0);
        title_row.add_css_class("inspector-title");
        title_row.append(&heading);
        title_row.append(&close);

        // ---------- Background ----------
        // A small base width: it fills the panel, and must not set the panel's minimum width.
        let (preview_host, preview) = crate::tiles::sized_tile(200, 156, "bg-preview");
        preview_host.set_halign(gtk::Align::Fill);
        preview_host.set_focusable(true);
        preview_host.update_property(&[gtk::accessible::Property::Label("Book background. Press Control V to paste an image.")]);
        let empty_hint = vbox(6);
        empty_hint.add_css_class("bg-empty");
        empty_hint.set_valign(gtk::Align::Fill);
        let empty_inner = vbox(6);
        empty_inner.set_valign(gtk::Align::Center);
        empty_inner.set_vexpand(true);
        let hint_icon = icon("background-add");
        hint_icon.set_pixel_size(24);
        empty_inner.append(&hint_icon);
        let hint = label("No background yet", &["caption"]);
        hint.set_xalign(0.5);
        empty_inner.append(&hint);
        empty_hint.append(&empty_inner);
        preview_host.add_overlay(&empty_hint);

        let add_button = text_button("Add", "secondary");
        let paste_button = text_button("Paste Image", "secondary");
        let remove_button = text_button("Remove", "flat-text");
        let bg_actions = hbox(6);
        bg_actions.add_css_class("bg-actions");
        bg_actions.append(&add_button);
        bg_actions.append(&paste_button);
        bg_actions.append(&remove_button);
        let bg_feedback = vbox(6);

        // ---------- Fields ----------
        let title = Field::new("Title");
        let author = Field::new("Author");
        let category = dropdown(&[]);
        let sub_category = dropdown(&[]);
        let isbn_field = Field::new("ISBN");
        let publisher = Field::new("Publisher");
        let year = Field::new("Year");
        let language = Field::new("Language");
        let status = dropdown(&Status::ALL.map(Status::as_str));
        let priority = dropdown(&Priority::ALL.map(Priority::as_str));
        let current_page = Field::new("Current page");
        let total_pages = Field::new("of total pages");
        let progress_host = hbox(10);
        let progress_note = vbox(6);
        let start_date = Field::new("Started");
        let finish_date = Field::new("Finished");
        add_calendar(&start_date, "Choose the start date");
        add_calendar(&finish_date, "Choose the finish date");
        start_date.entry.set_placeholder_text(Some("yyyy-mm-dd"));
        finish_date.entry.set_placeholder_text(Some("yyyy-mm-dd"));

        let stars_row = hbox(2);
        let stars: Vec<gtk::Button> = (1..=5)
            .map(|value| {
                let button = gtk::Button::builder().child(&icon("star")).css_classes(["star"]).tooltip_text(format!("{value} of 5")).build();
                button.update_property(&[gtk::accessible::Property::Label(&format!("Rate {value} of 5"))]);
                stars_row.append(&button);
                button
            })
            .collect();
        let rating_label = label("", &["caption"]);
        rating_label.set_margin_start(6);
        stars_row.append(&rating_label);
        let notes = gtk::TextView::builder().wrap_mode(gtk::WrapMode::WordChar).accepts_tab(false).build();
        notes.update_property(&[gtk::accessible::Property::Label("Notes")]);
        let notes_frame = gtk::ScrolledWindow::builder().child(&notes).min_content_height(72).css_classes(["notes-box"]).hscrollbar_policy(gtk::PolicyType::Never).build();

        let cancel = text_button("Cancel", "flat-text");
        let save_button = text_button("Save", "commit");
        let footer = hbox(8);
        footer.set_halign(gtk::Align::End);
        footer.set_margin_top(8);
        footer.append(&cancel);
        footer.append(&save_button);

        let delete_button = text_button("Delete Book", "destructive");
        delete_button.set_halign(gtk::Align::Start);
        let delete_box = vbox(12);
        delete_box.set_margin_top(4);
        delete_box.append(&gtk::Box::builder().css_classes(["divider"]).build());
        delete_box.append(&delete_button);

        let body = vbox(10);
        body.add_css_class("inspector-body");
        body.append(&preview_host);
        body.append(&bg_actions);
        body.append(&bg_feedback);
        body.append(&section("DETAILS"));
        body.append(&title.root);
        body.append(&author.root);
        body.append(&pair(&drop_field("Category", &category), &drop_field("Sub-category", &sub_category)));
        body.append(&section("CATALOGUE"));
        body.append(&isbn_field.root);
        body.append(&publisher.root);
        body.append(&pair(&year.root, &language.root));
        body.append(&section("READING"));
        body.append(&pair(&drop_field("Status", &status), &drop_field("Priority", &priority)));
        body.append(&pair(&current_page.root, &total_pages.root));
        body.append(&progress_host);
        body.append(&progress_note);
        body.append(&pair(&start_date.root, &finish_date.root));
        body.append(&section("YOUR TAKE"));
        let rating_field = vbox(6);
        rating_field.append(&label("Rating", &["field-label"]));
        rating_field.append(&stars_row);
        body.append(&rating_field);
        let notes_field = vbox(6);
        notes_field.append(&label("Notes", &["field-label"]));
        notes_field.append(&notes_frame);
        body.append(&notes_field);
        body.append(&footer);
        body.append(&delete_box);

        let scroller = gtk::ScrolledWindow::builder().child(&body).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
        let root = adw::ToolbarView::builder().css_classes(["pane-inspector"]).build();
        // One top bar: with two, libadwaita pushes the first 3 px down, out of line with the toolbar.
        let top_bar = vbox(0);
        top_bar.append(&header);
        top_bar.append(&title_row);
        root.add_top_bar(&top_bar);
        let (preview_page, preview_scroller) = Preview::new();
        let pages = gtk::Stack::builder().transition_type(gtk::StackTransitionType::None).build();
        pages.add_named(&preview_scroller, Some("preview"));
        pages.add_named(&scroller, Some("edit"));
        root.set_content(Some(&pages));

        let this = Rc::new(Self {
            app,
            root,
            pages,
            preview_page,
            previewing: RefCell::new(None),
            heading,
            draft: RefCell::new(Book::new(0)),
            original: RefCell::new(Book::new(0)),
            is_new: Cell::new(false),
            loading: Cell::new(false),
            has_book: Cell::new(false),
            generation: Cell::new(0),
            pending: RefCell::default(),
            preview,
            preview_host,
            empty_hint,
            add_button,
            remove_button,
            bg_feedback,
            title,
            author,
            category,
            sub_category,
            category_ids: RefCell::default(),
            sub_ids: RefCell::default(),
            isbn: isbn_field,
            publisher,
            year,
            language,
            status,
            priority,
            current_page,
            total_pages,
            progress_host,
            progress_note,
            start_date,
            finish_date,
            stars,
            rating_label,
            notes,
            delete_box,
            save_button,
        });
        Self::wire(&this, &close, &cancel, &paste_button, &delete_button);
        this
    }

    pub fn root(&self) -> &adw::ToolbarView {
        &self.root
    }

    fn wire(this: &Rc<Self>, close: &gtk::Button, cancel: &gtk::Button, paste: &gtk::Button, delete: &gtk::Button) {
        let inspector = this.clone();
        close.connect_clicked(move |_| {
            inspector.request_close();
        });
        let inspector = this.clone();
        cancel.connect_clicked(move |_| {
            let id = inspector.draft.borrow().id.clone();
            if inspector.is_new.get() {
                inspector.close();
            } else if let Some(book) = inspector.app.book(&id) {
                inspector.open(book, false);
            }
        });
        let inspector = this.clone();
        this.save_button.connect_clicked(move |_| {
            let inspector = inspector.clone();
            glib::spawn_future_local(async move {
                inspector.save(None).await;
            });
        });
        let inspector = this.clone();
        delete.connect_clicked(move |_| inspector.confirm_delete());
        let inspector = this.clone();
        this.add_button.connect_clicked(move |_| inspector.choose_file());
        let inspector = this.clone();
        paste.connect_clicked(move |_| inspector.paste());
        let inspector = this.clone();
        this.remove_button.connect_clicked(move |_| inspector.remove_background());

        // REQ-35m: Ctrl+V on the focused background section. REQ-35n keeps it for text
        // elsewhere, because this controller only sees keys while this widget has focus.
        let key = gtk::EventControllerKey::new();
        let inspector = this.clone();
        key.connect_key_pressed(move |_, key, _, state| {
            if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) && (key == gtk::gdk::Key::v || key == gtk::gdk::Key::V) {
                inspector.paste();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        this.preview_host.add_controller(key);
        let click = gtk::GestureClick::new();
        let host = this.preview_host.clone();
        click.connect_pressed(move |_, _, _, _| {
            host.grab_focus();
        });
        this.preview_host.add_controller(click);

        // Field rules that take effect while typing, as the web build did.
        let inspector = this.clone();
        this.category.connect_selected_notify(move |dropdown| {
            if inspector.loading.get() {
                return;
            }
            let id = inspector.category_ids.borrow().get(dropdown.selected() as usize).cloned().flatten();
            rules::apply_category_change(&mut inspector.draft.borrow_mut(), id);
            inspector.fill_sub_categories();
        });
        let inspector = this.clone();
        this.status.connect_selected_notify(move |dropdown| {
            if inspector.loading.get() {
                return;
            }
            let status = Status::ALL[dropdown.selected() as usize];
            inspector.collect_fields();
            rules::apply_status_change(&mut inspector.draft.borrow_mut(), status);
            inspector.fill_from_draft_after_rules();
        });
        let inspector = this.clone();
        this.current_page.entry.connect_changed(move |_| inspector.on_pages_changed());
        let inspector = this.clone();
        this.total_pages.entry.connect_changed(move |_| inspector.on_pages_changed());

        for (index, star) in this.stars.iter().enumerate() {
            let inspector = this.clone();
            star.connect_clicked(move |_| {
                let value = index as u8 + 1;
                // REQ-02e: choosing the current rating again clears it.
                let mut draft = inspector.draft.borrow_mut();
                draft.rating = if draft.rating == Some(value) { None } else { Some(value) };
                let rating = draft.rating;
                drop(draft);
                inspector.show_rating(rating);
            });
        }
    }

    fn show_rating(&self, rating: Option<u8>) {
        for (index, star) in self.stars.iter().enumerate() {
            let filled = rating.is_some_and(|value| index < value as usize);
            star.set_child(Some(&icon(if filled { "star-filled" } else { "star" })));
        }
        self.rating_label.set_label(&rating.map_or(String::new(), |value| format!("{value} of 5")));
    }

    fn parse_number(entry: &gtk::Entry) -> Result<Option<u32>, ()> {
        let text = entry.text();
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        text.parse::<u32>().map(Some).map_err(|_| ())
    }

    /// REQ-10a, REQ-11b, REQ-11c, REQ-12c: progress reacts as the numbers change.
    fn on_pages_changed(&self) {
        if self.loading.get() {
            return;
        }
        let total = Self::parse_number(&self.total_pages.entry);
        let current = Self::parse_number(&self.current_page.entry);
        {
            let mut draft = self.draft.borrow_mut();
            if let Ok(total) = total {
                draft.total_pages = total;
            }
            if let Ok(current) = current {
                let before = draft.status;
                rules::apply_page_change(&mut draft, current.unwrap_or(0));
                if before != draft.status {
                    self.loading.set(true);
                    let position = Status::ALL.iter().position(|s| *s == draft.status).unwrap_or(0);
                    self.status.set_selected(position as u32);
                    self.start_date.entry.set_text(draft.start_date.as_deref().unwrap_or_default());
                    self.loading.set(false);
                }
            }
        }
        self.update_progress();
    }

    fn update_progress(&self) {
        let draft = self.draft.borrow().clone();
        clear(&self.progress_host);
        clear(&self.progress_note);
        let enabled = rules::has_progress(&draft);
        self.current_page.entry.set_sensitive(enabled);
        if !enabled {
            let note = label(rules::MSG_NO_PAGE_COUNT, &["caption"]);
            note.set_wrap(true);
            self.progress_note.append(&note);
            return;
        }
        let percent = rules::completion_percent(&draft);
        let tone = if draft.status == Status::Finished { "success" } else { "" };
        self.progress_host.append(&util::progress_bar(percent, tone));
        self.progress_host.append(&label(&format!("{percent}%"), &["body-strong"]));

        if rules::is_at_last_page(&draft) {
            let row = hbox(8);
            let message = label(rules::MSG_LAST_PAGE, &["caption"]);
            message.set_wrap(true);
            message.set_hexpand(true);
            row.append(&message);
            let finish = text_button("Mark Finished", "secondary");
            let this_status = self.status.clone();
            finish.connect_clicked(move |_| {
                let position = Status::ALL.iter().position(|s| *s == Status::Finished).unwrap_or(0);
                this_status.set_selected(position as u32);
            });
            row.append(&finish);
            self.progress_note.append(&row);
        }
    }

    /// Reads every free-text widget into the draft. Numbers that do not parse become `None`
    /// and are caught by `field_errors`.
    fn collect_fields(&self) {
        let mut draft = self.draft.borrow_mut();
        let text = |field: &Field| field.entry.text().trim().to_string();
        let optional = |field: &Field| Some(text(field)).filter(|value| !value.is_empty());
        draft.title = self.title.entry.text().to_string();
        draft.author = text(&self.author);
        draft.isbn = optional(&self.isbn);
        draft.publisher = text(&self.publisher);
        draft.language = text(&self.language);
        draft.published_year = text(&self.year).parse().ok();
        draft.start_date = optional(&self.start_date);
        draft.finish_date = optional(&self.finish_date);
        draft.total_pages = Self::parse_number(&self.total_pages.entry).ok().flatten();
        draft.current_page = Self::parse_number(&self.current_page.entry).ok().flatten().unwrap_or(0);
        draft.priority = Priority::ALL[self.priority.selected() as usize];
        draft.status = Status::ALL[self.status.selected() as usize];
        draft.category_id = self.category_ids.borrow().get(self.category.selected() as usize).cloned().flatten();
        draft.sub_category_id = self.sub_ids.borrow().get(self.sub_category.selected() as usize).cloned().flatten();
        let buffer = self.notes.buffer();
        draft.notes = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
    }

    /// Validation including what the core rules cannot see: text that is not a number.
    fn field_errors(&self) -> FieldErrors {
        let draft = self.draft.borrow();
        let mut errors = rules::validate(&draft);
        if Self::parse_number(&self.total_pages.entry).is_err() {
            errors.total_pages = Some(rules::MSG_TOTAL_PAGES.into());
        }
        if Self::parse_number(&self.current_page.entry).is_err() {
            let message = draft.total_pages.map_or_else(|| rules::MSG_NEGATIVE_PAGE.to_string(), rules::page_range_message);
            errors.current_page = Some(message);
        }
        if !self.year.entry.text().trim().is_empty() && draft.published_year.is_none() {
            errors.published_year = Some("Enter a four-digit year.".into());
        }
        errors
    }

    fn show_errors(&self, errors: &FieldErrors) {
        self.title.show_error(errors.title.as_deref());
        self.total_pages.show_error(errors.total_pages.as_deref());
        self.current_page.show_error(errors.current_page.as_deref());
        self.start_date.show_error(errors.start_date.as_deref());
        self.finish_date.show_error(errors.finish_date.as_deref());
        self.isbn.show_error(errors.isbn.as_deref());
        self.year.show_error(errors.published_year.as_deref());
    }

    /// True when leaving would lose something (REQ-44c).
    pub fn is_dirty(&self) -> bool {
        if !self.has_book.get() {
            return false;
        }
        self.collect_fields();
        if !matches!(*self.pending.borrow(), PendingBackground::Unchanged) {
            return true;
        }
        let draft = self.draft.borrow();
        let original = self.original.borrow();
        let mut comparable = draft.clone();
        // A typed ISBN counts as unchanged when it normalises to what is stored.
        if let Some(value) = comparable.isbn.as_deref().and_then(|value| isbn::normalize(value).ok()) {
            comparable.isbn = Some(value);
        }
        comparable != *original
    }

    /// Runs `then` once any unsaved edits have been saved or discarded (REQ-44c).
    pub fn guard(self: &Rc<Self>, then: impl Fn() + 'static) {
        if !self.is_dirty() {
            then();
            return;
        }
        let dialog = adw::AlertDialog::new(Some("Save your changes?"), Some("This book has edits that have not been saved."));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("discard", "Discard");
        dialog.add_response("save", "Save");
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let this = self.clone();
        let then: Rc<dyn Fn()> = Rc::new(then);
        dialog.connect_response(None, move |_, response| match response {
            "discard" => {
                this.reset_to_original();
                then();
            }
            "save" => {
                let this = this.clone();
                let then = then.clone();
                glib::spawn_future_local(async move {
                    this.save(Some(then)).await;
                });
            }
            _ => {}
        });
        dialog.present(Some(&self.app.window()));
    }

    fn reset_to_original(&self) {
        *self.pending.borrow_mut() = PendingBackground::Unchanged;
        *self.draft.borrow_mut() = self.original.borrow().clone();
        self.fill(&self.original.borrow().clone());
    }

    /// REQ-44f: the close button and Esc, asking first when there are unsaved edits (REQ-44c).
    pub fn request_close(self: &Rc<Self>) {
        let inner = self.clone();
        self.guard(move || inner.close());
    }

    pub fn close(&self) {
        self.has_book.set(false);
        self.previewing.replace(None);
        *self.pending.borrow_mut() = PendingBackground::Unchanged;
        self.app.ui().show_inspector(false);
        // Keyboard users carry on from the books, rather than from a panel that has gone.
        self.app.ui().canvas.grab_focus();
        self.app.ui().canvas.set_selected(None);
    }

    /// REQ-33c, REQ-44d
    /// REQ-50: shows the book read-only. Nothing here can be edited, so there is nothing to lose.
    pub fn show_preview(&self, book: &Book) {
        self.has_book.set(false);
        *self.pending.borrow_mut() = PendingBackground::Unchanged;
        self.previewing.replace(Some(book.id.clone()));
        self.heading.set_label("Book");
        self.preview_page.fill(book, &self.app.index(), &self.app.thumbs);
        self.pages.set_visible_child_name("preview");
        self.app.ui().show_inspector(true);
    }

    /// After a sync: a book being edited that another device deleted closes, because that
    /// deletion wins (Android SRS D-17). Edits in progress to a book that still exists stay.
    pub fn after_sync(&self) {
        self.refresh_preview();
        if self.has_book.get() && !self.is_new.get() {
            let id = self.original.borrow().id.clone();
            if self.app.book(&id).is_none() {
                let title = self.original.borrow().title.clone();
                self.close();
                self.app.ui().show_banner(&format!("\"{title}\" was deleted on another device."));
            }
        }
    }

    /// Keeps the preview in step with the library, and closes it when its book has gone.
    pub fn refresh_preview(&self) {
        let Some(id) = self.previewing.borrow().clone() else { return };
        match self.app.book(&id) {
            Some(book) => self.preview_page.fill(&book, &self.app.index(), &self.app.thumbs),
            None => self.close(),
        }
    }

    #[cfg(feature = "dev-tools")]
    pub fn is_previewing(&self) -> bool {
        self.previewing.borrow().is_some()
    }

    #[cfg(feature = "dev-tools")]
    pub fn dev_preview_text(&self) -> (String, String, String, String, String) {
        let page = &self.preview_page;
        (page.title.label().into(), page.author.label().into(), page.shelf.label().into(), page.status.label().into(), page.priority.label().into())
    }

    pub fn open(&self, book: Book, is_new: bool) {
        self.previewing.replace(None);
        self.pages.set_visible_child_name("edit");
        self.has_book.set(true);
        self.is_new.set(is_new);
        self.generation.set(self.generation.get() + 1);
        *self.pending.borrow_mut() = PendingBackground::Unchanged;
        *self.original.borrow_mut() = book.clone();
        *self.draft.borrow_mut() = book.clone();
        self.heading.set_label(if is_new { "New Book" } else { "Book" });
        self.delete_box.set_visible(!is_new);
        self.fill(&book);
        self.app.ui().show_inspector(true);
        if is_new {
            self.title.entry.grab_focus();
        }
    }

    fn fill_categories(&self, book: &Book) {
        let library = self.app.library.borrow();
        let mut names = vec!["None".to_string()];
        let mut ids: Vec<Option<String>> = vec![None];
        for category in &library.categories {
            names.push(category.name.clone());
            ids.push(Some(category.id.clone()));
        }
        let selected = ids.iter().position(|id| *id == book.category_id).unwrap_or(0);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        self.category.set_model(Some(&gtk::StringList::new(&refs)));
        self.category.set_selected(selected as u32);
        *self.category_ids.borrow_mut() = ids;
    }

    /// REQ-05a: only the Sub-categories under this book's Category.
    fn fill_sub_categories(&self) {
        let draft = self.draft.borrow().clone();
        let library = self.app.library.borrow();
        let subs: Vec<(String, String)> = library
            .categories
            .iter()
            .find(|category| Some(&category.id) == draft.category_id.as_ref())
            .map(|category| category.sub_categories.iter().map(|sub| (sub.id.clone(), sub.name.clone())).collect())
            .unwrap_or_default();
        let was_loading = self.loading.replace(true);
        let mut names = vec!["None".to_string()];
        let mut ids: Vec<Option<String>> = vec![None];
        for (id, name) in subs {
            names.push(name);
            ids.push(Some(id));
        }
        let selected = ids.iter().position(|id| *id == draft.sub_category_id).unwrap_or(0);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        self.sub_category.set_model(Some(&gtk::StringList::new(&refs)));
        self.sub_category.set_selected(selected as u32);
        self.sub_category.set_sensitive(ids.len() > 1);
        *self.sub_ids.borrow_mut() = ids;
        self.loading.set(was_loading);
    }

    fn fill(&self, book: &Book) {
        self.loading.set(true);
        self.title.entry.set_text(&book.title);
        self.author.entry.set_text(&book.author);
        self.fill_categories(book);
        self.fill_sub_categories();
        self.isbn.entry.set_text(book.isbn.as_deref().unwrap_or_default());
        self.publisher.entry.set_text(&book.publisher);
        self.year.entry.set_text(&book.published_year.map_or(String::new(), |year| year.to_string()));
        self.language.entry.set_text(&book.language);
        self.status.set_selected(Status::ALL.iter().position(|s| *s == book.status).unwrap_or(0) as u32);
        self.priority.set_selected(Priority::ALL.iter().position(|p| *p == book.priority).unwrap_or(1) as u32);
        self.total_pages.entry.set_text(&book.total_pages.map_or(String::new(), |pages| pages.to_string()));
        self.current_page.entry.set_text(&if book.current_page > 0 { book.current_page.to_string() } else { String::new() });
        self.start_date.entry.set_text(book.start_date.as_deref().unwrap_or_default());
        self.finish_date.entry.set_text(book.finish_date.as_deref().unwrap_or_default());
        self.notes.buffer().set_text(&book.notes);
        self.show_rating(book.rating);
        self.loading.set(false);
        self.show_errors(&FieldErrors::default());
        clear(&self.bg_feedback);
        self.update_progress();
        self.show_saved_background(book);
    }

    /// After a status change stamps dates and pages (REQ-13c to REQ-13f).
    fn fill_from_draft_after_rules(&self) {
        let draft = self.draft.borrow().clone();
        self.loading.set(true);
        self.start_date.entry.set_text(draft.start_date.as_deref().unwrap_or_default());
        self.finish_date.entry.set_text(draft.finish_date.as_deref().unwrap_or_default());
        self.current_page.entry.set_text(&if draft.current_page > 0 { draft.current_page.to_string() } else { String::new() });
        self.loading.set(false);
        self.update_progress();
    }

    fn set_background_controls(&self, has_image: bool) {
        self.empty_hint.set_visible(!has_image);
        self.add_button.set_label(if has_image { "Replace" } else { "Add" });
        self.remove_button.set_visible(has_image);
    }

    fn show_saved_background(&self, book: &Book) {
        self.preview.set_paintable(gtk::gdk::Paintable::NONE);
        let Some(file) = book.background_file.clone() else {
            self.set_background_controls(false);
            return;
        };
        self.set_background_controls(true);
        let path = self.app.paths.backgrounds.join(file);
        let generation = self.generation.get();
        let preview = self.preview.clone();
        let inspector_generation = self.generation_probe();
        glib::spawn_future_local(async move {
            let decoded = gio::spawn_blocking(move || {
                let bytes = std::fs::read(path).ok()?;
                background::decode_for_display(&bytes, 640, 640).ok()
            })
            .await;
            if inspector_generation() == generation {
                if let Ok(Some(image)) = decoded {
                    preview.set_paintable(Some(&util::texture_from_rgba(&image)));
                }
            }
        });
    }

    /// A cheap way for async work to ask whether this inspector has moved on.
    fn generation_probe(&self) -> impl Fn() -> u64 + 'static {
        let inspector = self.app.ui.get().map(|ui| ui.inspector.clone());
        let fallback = self.generation.get();
        move || inspector.as_ref().map_or(fallback, |inspector| inspector.generation.get())
    }

    fn show_feedback(&self, kind: &str, heading: &str, message: &str) {
        self.bg_feedback.append(&feedback(kind, heading, message));
    }

    /// REQ-35r: a checked image becomes a preview, stored only on Save.
    fn accept_image(self: &Rc<Self>, result: Result<CheckedImage, ImageError>) {
        clear(&self.bg_feedback);
        let image = match result {
            Ok(image) => image,
            Err(error) => {
                // REQ-35b, REQ-35p: the book and any earlier preview stay as they were.
                self.show_feedback("error", "ERROR", &error.to_string());
                return;
            }
        };
        if image.small {
            self.show_feedback("warning", "WARNING", background::MSG_SMALL_IMAGE);
        }
        self.show_feedback("info", "NOT SAVED YET", "Choose Save to keep this background.");
        let image = Rc::new(image);
        *self.pending.borrow_mut() = PendingBackground::Set(image.clone());
        self.set_background_controls(true);
        let (bytes, preview, generation) = (image.bytes.clone(), self.preview.clone(), self.generation.get());
        let probe = self.generation_probe();
        glib::spawn_future_local(async move {
            let decoded = gio::spawn_blocking(move || background::decode_for_display(&bytes, 640, 640)).await;
            if probe() == generation {
                if let Ok(Ok(image)) = decoded {
                    preview.set_paintable(Some(&util::texture_from_rgba(&image)));
                }
            }
        });
    }

    /// REQ-35, UC-30
    fn choose_file(self: &Rc<Self>) {
        let this = self.clone();
        crate::image_source::choose_file(&self.app.window(), move |result| this.accept_image(result));
    }

    /// REQ-35k, REQ-35o, REQ-35p
    fn paste(self: &Rc<Self>) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let result = crate::image_source::read_clipboard(&this.preview_host).await;
            this.accept_image(result);
        });
    }

    fn remove_background(&self) {
        clear(&self.bg_feedback);
        let had_saved = self.original.borrow().background_file.is_some();
        *self.pending.borrow_mut() = if had_saved { PendingBackground::Remove } else { PendingBackground::Unchanged };
        self.preview.set_paintable(gtk::gdk::Paintable::NONE);
        self.set_background_controls(false);
        if had_saved {
            self.show_feedback("info", "NOT SAVED YET", "Choose Save to remove this background.");
        }
    }

    fn confirm_delete(self: &Rc<Self>) {
        let book = self.original.borrow().clone();
        let dialog = adw::AlertDialog::new(Some("Delete book"), Some(&format!("Delete \u{201c}{}\u{201d} from your library? This cannot be undone.", book.title)));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("delete", "Delete");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let this = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "delete" && this.app.delete_book(&book) {
                this.close();
                let app = this.app.clone();
                glib::idle_add_local_once(move || refresh(&app));
            }
        });
        dialog.present(Some(&self.app.window()));
    }

    /// REQ-01d, REQ-36c: resolves to true when the Reader chooses Add Anyway.
    async fn confirm_duplicate(&self, other: &Book) -> bool {
        let by = if other.author.is_empty() { String::new() } else { format!(" by {}", other.author) };
        let dialog = adw::AlertDialog::new(Some("Possible duplicate"), Some(&format!("\u{201c}{}\u{201d}{by} is already in your library. Add this entry as well?", other.title)));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("add", "Add Anyway");
        dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.choose_future(Some(&self.app.window())).await == "add"
    }

    /// UC-01. `then` runs only when the save succeeded, for the guard's Save path.
    async fn save(self: &Rc<Self>, then: Option<Rc<dyn Fn()>>) {
        self.collect_fields();

        // REQ-36a
        let typed_isbn = self.draft.borrow().isbn.clone();
        if let Some(Ok(normalised)) = typed_isbn.map(|value| isbn::normalize(&value)) {
            self.draft.borrow_mut().isbn = Some(normalised);
        }
        let errors = self.field_errors();
        self.show_errors(&errors);
        if !errors.is_empty() {
            return;
        }

        let draft = self.draft.borrow().clone();
        let duplicate = {
            let library = self.app.library.borrow();
            let original = self.original.borrow();
            // Only a changed title, author or ISBN can newly collide with another book.
            let identity_changed = self.is_new.get() || draft.title != original.title || draft.author != original.author;
            let title_clash = if identity_changed { rules::find_duplicate(&draft, &library.books).cloned() } else { None };
            let isbn_changed = self.is_new.get() || draft.isbn != original.isbn;
            title_clash.or_else(|| if isbn_changed { rules::find_isbn_duplicate(&draft, &library.books).cloned() } else { None })
        };
        if let Some(other) = duplicate {
            if !self.confirm_duplicate(&other).await {
                return;
            }
        }

        self.save_button.set_sensitive(false);
        let saved = self.commit(draft).await;
        self.save_button.set_sensitive(true);
        if !saved {
            return; // UC-01 6a: the banner is up and the entries stay in the form.
        }
        if let Some(then) = then {
            then();
        }
    }

    #[cfg(feature = "dev-tools")]
    pub async fn save_for_devtools(self: Rc<Self>) {
        self.save(None).await;
    }

    #[cfg(feature = "dev-tools")]
    pub fn dev_set(&self, title: Option<&str>, isbn: Option<&str>, total: Option<&str>, current: Option<&str>) {
        if let Some(text) = title {
            self.title.entry.set_text(text);
        }
        if let Some(text) = isbn {
            self.isbn.entry.set_text(text);
        }
        if let Some(text) = total {
            self.total_pages.entry.set_text(text);
        }
        if let Some(text) = current {
            self.current_page.entry.set_text(text);
        }
    }

    #[cfg(feature = "dev-tools")]
    pub fn dev_title(&self) -> String {
        self.title.entry.text().to_string()
    }

    #[cfg(feature = "dev-tools")]
    pub fn dev_isbn_error(&self) -> Option<String> {
        self.isbn.error.is_visible().then(|| self.isbn.error.label().to_string())
    }

    #[cfg(feature = "dev-tools")]
    pub fn dev_has_pending_image(&self) -> bool {
        matches!(*self.pending.borrow(), PendingBackground::Set(_))
    }

    #[cfg(feature = "dev-tools")]
    pub fn dev_feedback(&self) -> Vec<String> {
        fn collect(widget: &gtk::Widget, out: &mut Vec<String>) {
            if let Some(found) = widget.downcast_ref::<gtk::Label>() {
                out.push(found.label().to_string());
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                collect(&current, out);
                child = current.next_sibling();
            }
        }
        let mut out = Vec::new();
        collect(self.bg_feedback.upcast_ref(), &mut out);
        out
    }

    #[cfg(feature = "dev-tools")]
    pub fn dev_paste(self: &Rc<Self>) {
        self.paste();
    }

    #[cfg(feature = "dev-tools")]
    pub fn dev_remove_background(&self) {
        self.remove_background();
    }

    #[cfg(feature = "dev-tools")]
    pub fn dev_set_status(&self, status: Status) {
        self.status.set_selected(Status::ALL.iter().position(|s| *s == status).unwrap_or(0) as u32);
    }

    async fn commit(self: &Rc<Self>, mut book: Book) -> bool {
        let pending = self.pending.borrow().clone();
        let previous = self.original.borrow().background_file.clone();
        let (backgrounds, thumbnails) = (self.app.paths.backgrounds.clone(), self.app.paths.thumbnails.clone());

        // QA-32: the image work happens off the UI thread.
        match &pending {
            PendingBackground::Unchanged => {}
            PendingBackground::Remove => book.background_file = None,
            PendingBackground::Set(image) => {
                let image = (**image).clone();
                let stored = gio::spawn_blocking(move || background::store(&image, &backgrounds, &thumbnails)).await;
                match stored {
                    Ok(Ok(file)) => book.background_file = Some(file),
                    _ => {
                        self.app.ui().show_banner(&ImageError::Unsaved.to_string());
                        return false;
                    }
                }
            }
        }

        // A phone may have changed this book since the editor opened. Only the fields changed
        // here are written, so its changes to the others stay (Android SRS REQ-32b).
        if !self.is_new.get() {
            if let Some(current) = self.app.book(&book.id) {
                book = booklub_core::sync::rebase(&self.original.borrow(), &book, &current);
            }
        }

        if !self.app.save_book(&book) {
            // The new copy was never referenced, so it should not linger until the next sweep.
            if let (PendingBackground::Set(_), Some(file)) = (&pending, &book.background_file) {
                background::remove(file, &self.app.paths.backgrounds, &self.app.paths.thumbnails);
            }
            return false;
        }

        // REQ-35h: a replaced or removed background's files go only after the save succeeded.
        if !matches!(pending, PendingBackground::Unchanged) {
            if let Some(old) = previous {
                self.app.thumbs.forget(&old);
                background::remove(&old, &self.app.paths.backgrounds, &self.app.paths.thumbnails);
            }
        }

        *self.pending.borrow_mut() = PendingBackground::Unchanged;
        let rebased = *self.draft.borrow() != book;
        *self.original.borrow_mut() = book.clone();
        *self.draft.borrow_mut() = book.clone();
        if rebased {
            // Shows what was merged in, so the form doesn't look unsaved.
            self.fill(&book);
        }
        self.is_new.set(false);
        self.heading.set_label("Book");
        self.delete_box.set_visible(true);
        clear(&self.bg_feedback);

        let adjustment = self.app.ui().grid_scroller.vadjustment();
        let scroll = adjustment.value();
        refresh(&self.app);
        adjustment.set_value(scroll);
        true
    }
}
