//! The window: sidebar, toolbar, tile canvas and inspector (REQ-31), and the code that keeps
//! them showing the current view.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use booklub_core::paths::Paths;
use booklub_core::preferences::{clamp_scale, Appearance, BooksView, Preferences};
use booklub_core::query::{self, Filters, SortKey};
use booklub_core::store::Store;
use booklub_core::{background, Book, Priority, Status};
use gtk::{gdk, gio, glib};

use crate::app::{App, SubFilter, View};
use crate::canvas::{self, BookCanvas, FluidGrid};
use crate::dialogs;
use crate::inspector::Inspector;
use crate::thumbs::Thumbs;
use crate::tiles::{self, BookTile, CategoryArt, BOOK_HEIGHT, BOOK_WIDTH, CATEGORY_HEIGHT, CATEGORY_WIDTH};
use crate::util::{self, books, clear, hbox, icon, label, tool_button, vbox};
use crate::{import_dialog, progress};

/// The header bar pads by 6 px, so this puts titles on the content's 24 px left edge.
const TITLE_INSET: i32 = 18;
const PANEL_WIDTH: i32 = 320;
const SIDEBAR_WIDTH: i32 = 220;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum NavItem {
    Home,
    AllBooks,
    Reading,
    ToRead,
    Finished,
    Progress,
}

pub struct Ui {
    pub window: adw::ApplicationWindow,
    sidebar_view: adw::ToolbarView,
    stage: gtk::Overlay,
    /// Too narrow for the sidebar and the books side by side, so one shows at a time.
    collapsed: Cell<bool>,
    showing_sidebar: Cell<bool>,
    banner: adw::Banner,
    nav: gtk::ListBox,
    nav_rows: Vec<(NavItem, gtk::ListBoxRow)>,
    nav_counts: HashMap<NavItem, gtk::Label>,
    title_box: gtk::Box,
    pub search: gtk::SearchEntry,
    filter_badge: gtk::Label,
    sort_list: gtk::Box,
    filter_list: gtk::Box,
    tools: gtk::Box,
    stack: gtk::Stack,
    pub home_categories: FluidGrid,
    pub home_reading: FluidGrid,
    home_reading_header: gtk::Box,
    home_reading_count: gtk::Label,
    home_empty: gtk::Box,
    strip_scroller: gtk::ScrolledWindow,
    strip: gtk::Box,
    summary: gtk::Label,
    pub canvas: BookCanvas,
    pub grid_scroller: gtk::ScrolledWindow,
    status_bar: gtk::Box,
    status_count: gtk::Label,
    status_sizing: gtk::Box,
    // Only the end-to-end checks read these two back. Their signals are connected in `build`.
    #[cfg_attr(not(feature = "dev-tools"), allow(dead_code))]
    pub slider: gtk::Scale,
    #[cfg_attr(not(feature = "dev-tools"), allow(dead_code))]
    pub settings_button: gtk::Button,
    no_match: gtk::Box,
    pub progress_page: gtk::ScrolledWindow,
    pub inspector: Rc<Inspector>,
    content_header: adw::HeaderBar,
    content_view: adw::ToolbarView,
    narrow: adw::Breakpoint,
    /// The window is too narrow to keep the inspector beside the books.
    panel_overlaid: Cell<bool>,
}

impl Ui {
    pub fn show_banner(&self, message: &str) {
        self.banner.set_title(message);
        self.banner.set_revealed(true);
    }

    pub fn hide_banner(&self) {
        self.banner.set_revealed(false);
    }

    /// Drives the narrow layout's two pages, and leaves the wide layout as it found it.
    #[cfg(feature = "dev-tools")]
    pub fn dev_collapsed_pages(&self) -> ((bool, bool), (bool, bool)) {
        self.collapsed.set(true);
        self.show_sidebar_page(false);
        let books = (self.sidebar_view.is_visible(), self.stage.is_visible());
        self.show_sidebar_page(true);
        let sidebar = (self.sidebar_view.is_visible(), self.stage.is_visible());
        self.show_sidebar_page(false);
        self.collapsed.set(false);
        self.apply_shell_layout();
        (books, sidebar)
    }

    /// Where each header ends, measured from the top of the window.
    #[cfg(feature = "dev-tools")]
    pub fn dev_header_lines(&self) -> (f32, f32) {
        let bottom = |bar: &adw::HeaderBar| bar.compute_bounds(&self.window).map_or(0.0, |bounds| bounds.y() + bounds.height());
        let panel = util::find_header(self.inspector.root().upcast_ref());
        (bottom(&self.content_header), panel.as_ref().map_or(0.0, bottom))
    }

    pub fn show_sidebar_page(&self, show: bool) {
        self.showing_sidebar.set(show);
        self.apply_shell_layout();
    }

    /// Only ever hides and shows, never re-parents: GTK crashes if a focused text field is moved to
    /// a new parent, which is what AdwNavigationSplitView does when it collapses.
    fn apply_shell_layout(&self) {
        let sidebar_only = self.collapsed.get() && self.showing_sidebar.get();
        self.sidebar_view.set_visible(!self.collapsed.get() || sidebar_only);
        self.sidebar_view.set_hexpand(sidebar_only);
        self.stage.set_visible(!sidebar_only);
    }

    pub fn inspector_visible(&self) -> bool {
        self.inspector.root().is_visible()
    }

    pub fn show_inspector(&self, visible: bool) {
        self.inspector.root().set_visible(visible);
        self.apply_panel_layout();
    }

    /// One step with no animation, so the panel is never seen hunting for its place. Beside the
    /// books the content gives up the panel's width; otherwise the panel lies on top of it.
    fn apply_panel_layout(&self) {
        let root = self.inspector.root();
        let overlaid = self.panel_overlaid.get();
        let pinned = root.is_visible() && !overlaid;
        self.content_view.set_margin_end(if pinned { PANEL_WIDTH } else { 0 });
        // Window controls live on whichever header is rightmost.
        self.content_header.set_show_end_title_buttons(!pinned);
        if overlaid {
            root.add_css_class("overlaid");
        } else {
            root.remove_css_class("overlaid");
        }
    }
}

fn nav_row(item_icon: &str, text: &str, count: bool) -> (gtk::ListBoxRow, gtk::Label) {
    let row_box = hbox(10);
    row_box.append(&icon(item_icon));
    let name = gtk::Label::builder().label(text).xalign(0.0).hexpand(true).build();
    row_box.append(&name);
    let counter = label("", &["nav-count"]);
    counter.set_visible(count);
    row_box.append(&counter);
    let row = gtk::ListBoxRow::builder().child(&row_box).build();
    (row, counter)
}

/// QA-33, Appendix A: opens the store, sweeps unreferenced backgrounds, builds the window.
pub fn open(application: &adw::Application) {
    let paths = Paths::for_user();
    let window = adw::ApplicationWindow::builder().application(application).title("Booklub").default_width(1180).default_height(760).width_request(480).height_request(420).build();

    let opened = paths.ensure().map_err(|e| e.to_string()).and_then(|()| {
        Store::open(&paths.database, &paths.backups).map_err(|e| e.message).and_then(|store| {
            let library = store.load_all().map_err(|e| e.message)?;
            Ok((store, library))
        })
    });
    let (store, library) = match opened {
        Ok(opened) => opened,
        Err(message) => {
            // Nothing can work without the store, so say why and leave the data alone.
            let status = adw::StatusPage::builder().title("Booklub could not open your library").description(glib::markup_escape_text(&message)).build();
            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&adw::HeaderBar::new());
            toolbar.set_content(Some(&status));
            window.set_content(Some(&toolbar));
            window.present();
            return;
        }
    };
    if let Ok(referenced) = store.referenced_backgrounds() {
        background::remove_unreferenced(&referenced, &paths.backgrounds, &paths.thumbnails);
    }

    let thumbs = Thumbs::new(paths.backgrounds.clone(), paths.thumbnails.clone());
    let preferences = Preferences::load(&paths.preferences);
    apply_appearance(preferences.appearance);
    let app = Rc::new(App {
        store,
        paths,
        library: RefCell::new(library),
        view: RefCell::default(),
        sort: Cell::new(SortKey::Manual),
        filters: RefCell::new(Filters::default()),
        preferences: Cell::new(preferences),
        saving_preferences: Cell::new(None),
        thumbs,
        ui: OnceCell::new(),
        updating: Cell::new(false),
    });

    let ui = build(&app, &window);
    let _ = app.ui.set(ui);
    wire_after_build(&app);
    refresh(&app);

    // REQ-42b, REQ-43c, REQ-43d: only the mode-specific artwork needs help; tokens swap themselves.
    let app_for_theme = app.clone();
    adw::StyleManager::default().connect_dark_notify(move |_| refresh_artwork(&app_for_theme));

    // REQ-44c: closing the window with unsaved edits asks first.
    let app_for_close = app.clone();
    window.connect_close_request(move |window| {
        let inspector = &app_for_close.ui().inspector;
        if !inspector.is_dirty() {
            return glib::Propagation::Proceed;
        }
        let window = window.clone();
        inspector.guard(move || window.destroy());
        glib::Propagation::Stop
    });

    window.present();
    // The ring appears once the keyboard is used (QA-12), rather than on a row at launch.
    // Mapping the window turns it on, so this has to wait until that has happened.
    let mapped = window.clone();
    glib::idle_add_local_once(move || mapped.set_focus_visible(false));

    #[cfg(feature = "dev-tools")]
    crate::devtools::run(&app);
}

fn build(app: &Rc<App>, window: &adw::ApplicationWindow) -> Ui {
    // ---------- Sidebar ----------
    let brand = hbox(10);
    let mark = gtk::Image::builder().pixel_size(20).build();
    set_mark(&mark);
    brand.append(&mark);
    brand.append(&label("Booklub", &["heading"]));
    let sidebar_header = adw::HeaderBar::builder().show_title(false).show_end_title_buttons(false).show_start_title_buttons(false).build();
    sidebar_header.pack_start(&brand);

    let nav = gtk::ListBox::builder().css_classes(["nav-list"]).selection_mode(gtk::SelectionMode::Single).build();
    let items = [
        (NavItem::Home, "home", "Home", false),
        (NavItem::AllBooks, "all-books", "All Books", true),
        (NavItem::Reading, "reading", "Reading", true),
        (NavItem::ToRead, "to-read", "To Read", true),
        (NavItem::Finished, "finished", "Finished", true),
        (NavItem::Progress, "progress", "Analytics", false),
    ];
    let mut nav_rows = Vec::new();
    let mut nav_counts = HashMap::new();
    for (item, glyph, text, count) in items {
        let (row, counter) = nav_row(glyph, text, count);
        nav.append(&row);
        nav_rows.push((item, row));
        nav_counts.insert(item, counter);
    }
    let sidebar_view = adw::ToolbarView::builder().css_classes(["pane-sidebar"]).build();
    sidebar_view.add_top_bar(&sidebar_header);
    // REQ-31h: Settings keeps the sidebar's bottom edge, however long the list above it grows.
    nav.set_vexpand(true);
    let settings_row = hbox(10);
    settings_row.append(&icon("settings"));
    settings_row.append(&gtk::Label::new(Some("Settings")));
    let settings_button = gtk::Button::builder().child(&settings_row).action_name("win.settings").css_classes(["nav-settings"]).build();
    let sidebar_body = vbox(0);
    sidebar_body.append(&nav);
    sidebar_body.append(&settings_button);
    sidebar_view.set_content(Some(&sidebar_body));

    // ---------- Toolbar ----------
    let title_box = hbox(4);
    title_box.set_margin_start(TITLE_INSET);
    let search = gtk::SearchEntry::builder().placeholder_text("Search title, author, ISBN").css_classes(["search-entry"]).build();
    search.update_property(&[gtk::accessible::Property::Label("Search the library by title, author or ISBN")]);

    // At 600 px the search field folds into an icon button.
    let compact_search = gtk::MenuButton::builder().child(&icon("search")).css_classes(["tool"]).tooltip_text("Search").visible(false).build();
    let compact_entry = gtk::SearchEntry::builder().placeholder_text("Search title, author, ISBN").width_chars(26).build();
    compact_search.set_popover(Some(&gtk::Popover::builder().child(&compact_entry).build()));
    search.bind_property("text", &compact_entry, "text").bidirectional().build();

    let sort_list = vbox(0);
    let sort_button = gtk::MenuButton::builder().child(&icon("sort")).css_classes(["tool"]).tooltip_text("Sort").popover(&gtk::Popover::builder().child(&sort_list).build()).build();
    sort_button.update_property(&[gtk::accessible::Property::Label("Sort")]);

    let filter_list = vbox(0);
    let filter_scroller = gtk::ScrolledWindow::builder().child(&filter_list).hscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true).max_content_height(560).min_content_width(240).build();
    let filter_content = hbox(6);
    filter_content.append(&icon("filter"));
    let filter_badge = label("", &["filter-badge"]);
    filter_badge.set_xalign(0.5);
    filter_badge.set_valign(gtk::Align::Center);
    filter_badge.set_visible(false);
    filter_content.append(&filter_badge);
    let filter_button = gtk::MenuButton::builder().child(&filter_content).css_classes(["tool"]).tooltip_text("Filter").popover(&gtk::Popover::builder().child(&filter_scroller).build()).build();
    filter_button.update_property(&[gtk::accessible::Property::Label("Filter")]);

    let add_button = tool_button("add", "Add book");
    let menu = gio::Menu::new();
    menu.append(Some("Import and Export"), Some("win.import"));
    menu.append(Some("About Booklub"), Some("win.about"));
    let menu_button = gtk::MenuButton::builder().child(&icon("menu")).css_classes(["tool"]).tooltip_text("Main menu").menu_model(&menu).build();
    menu_button.update_property(&[gtk::accessible::Property::Label("Main menu")]);

    let tools = hbox(6);
    tools.append(&search);
    tools.append(&compact_search);
    tools.append(&sort_button);
    tools.append(&filter_button);
    tools.append(&add_button);

    let content_header = adw::HeaderBar::builder().show_title(false).show_start_title_buttons(false).build();
    let sidebar_back = tool_button("back", "Show the sidebar");
    sidebar_back.set_visible(false);
    content_header.pack_start(&sidebar_back);
    content_header.pack_start(&title_box);
    content_header.pack_end(&menu_button);
    content_header.pack_end(&tools);

    let banner = adw::Banner::builder().button_label("Dismiss").build();

    // ---------- Home ----------
    let scale = app.preferences.get().tile_scale;
    let home_categories = FluidGrid::new(scaled(CATEGORY_WIDTH, scale), f64::from(CATEGORY_HEIGHT) / f64::from(CATEGORY_WIDTH));
    let home_reading = FluidGrid::new(scaled(BOOK_WIDTH, scale), f64::from(BOOK_HEIGHT) / f64::from(BOOK_WIDTH));
    let home_reading_header = hbox(8);
    home_reading_header.set_margin_top(24);
    let home_reading_title = label("Continue Reading", &["heading"]);
    let home_reading_count = label("", &["caption"]);
    home_reading_header.append(&home_reading_title);
    home_reading_header.append(&home_reading_count);

    let home_body = vbox(12);
    home_body.add_css_class("section-pad");
    home_body.set_margin_bottom(24);
    home_body.append(&label("Browse Categories", &["heading"]));
    home_body.append(&home_categories);
    home_body.append(&home_reading_header);
    home_body.append(&home_reading);
    let home_empty = vbox(8);
    home_empty.set_halign(gtk::Align::Center);
    home_empty.set_margin_top(32);
    home_body.append(&home_empty);
    let home_scroller = gtk::ScrolledWindow::builder().child(&home_body).hscrollbar_policy(gtk::PolicyType::Never).build();

    let home_page = vbox(0);
    home_scroller.set_vexpand(true);
    home_page.append(&home_scroller);

    // ---------- Book views ----------
    let strip = hbox(16);
    strip.add_css_class("sub-strip");
    let strip_scroller = gtk::ScrolledWindow::builder().child(&strip).vscrollbar_policy(gtk::PolicyType::Never).build();
    let summary = label("", &["summary"]);
    summary.set_visible(false);

    let canvas = book_canvas(app);
    let grid_scroller = gtk::ScrolledWindow::builder().child(&canvas).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
    let no_match = vbox(12);
    no_match.set_vexpand(true);
    no_match.set_valign(gtk::Align::Center);
    no_match.set_halign(gtk::Align::Center);
    no_match.set_visible(false);

    let books_page = vbox(0);
    books_page.append(&strip_scroller);
    books_page.append(&summary);
    books_page.append(&grid_scroller);
    books_page.append(&no_match);

    let progress_page = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).build();

    let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::None).css_classes(["page"]).vexpand(true).hexpand(true).build();
    stack.add_named(&home_page, Some("home"));
    stack.add_named(&books_page, Some("books"));
    stack.add_named(&progress_page, Some("progress"));

    let body = vbox(0);
    body.append(&banner);
    body.append(&stack);

    let content_view = adw::ToolbarView::builder().css_classes(["pane-content"]).build();
    content_view.add_top_bar(&content_header);
    content_view.set_content(Some(&body));
    let (status_bar, status_count, status_sizing, slider) = canvas::status_bar(scale);
    content_view.add_bottom_bar(&status_bar);

    // ---------- Inspector ----------
    let inspector = Inspector::new(app.clone());
    // Not AdwOverlaySplitView: its slide animation fought the layout whenever the panel opened or
    // the window was resized.
    let stage = gtk::Overlay::builder().child(&content_view).build();
    let panel = inspector.root();
    panel.set_width_request(PANEL_WIDTH);
    panel.set_halign(gtk::Align::End);
    panel.set_visible(false);
    stage.add_overlay(panel);

    stage.set_hexpand(true);
    let shell = hbox(0);
    sidebar_view.set_width_request(SIDEBAR_WIDTH);
    shell.append(&sidebar_view);
    shell.append(&stage);
    window.set_content(Some(&shell));

    // QA-14: below 900 px the sidebar collapses and the inspector overlays.
    // With the inspector pinned, the window needs about 1,040 px, and libadwaita cuts off whatever
    // doesn't fit. So the inspector overlays well before that point.
    let medium = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 1099px").expect("valid condition"));
    window.add_breakpoint(medium);

    // When several breakpoints match, the last one added applies.
    let narrow = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 899px").expect("valid condition"));
    // The back arrow sits before the title here, so the wide inset would strand it.
    narrow.add_setter(&sidebar_back, "visible", Some(&true.to_value()));
    narrow.add_setter(&sidebar_header, "show-end-title-buttons", Some(&true.to_value()));
    narrow.add_setter(&title_box, "margin-start", Some(&0.to_value()));
    narrow.add_setter(&search, "visible", Some(&false.to_value()));
    narrow.add_setter(&compact_search, "visible", Some(&true.to_value()));
    window.add_breakpoint(narrow.clone());

    // ---------- Signals that need only local widgets ----------
    let app_clone = app.clone();
    slider.connect_value_changed(move |slider| set_tile_scale(&app_clone, slider.value()));

    let app_clone = app.clone();
    sidebar_back.connect_clicked(move |_| app_clone.ui().show_sidebar_page(true));

    let banner_clone = banner.clone();
    banner.connect_button_clicked(move |_| banner_clone.set_revealed(false));

    let app_clone = app.clone();
    add_button.connect_clicked(move |_| {
        let app = app_clone.clone();
        let inspector = app.ui().inspector.clone();
        inspector.guard(move || {
            let order = query::next_position(&app.library.borrow().books);
            app.ui().inspector.open(Book::new(order), true);
        });
    });

    let app_clone = app.clone();
    nav.connect_row_activated(move |_, row| {
        let target = app_clone.ui().nav_rows.iter().find(|(_, candidate)| candidate == row).map(|(item, _)| *item);
        let Some(target) = target else { return };
        let view = match target {
            NavItem::Home => View::Home,
            NavItem::AllBooks => View::AllBooks,
            NavItem::Reading => View::Status(Status::Reading),
            NavItem::ToRead => View::Status(Status::ToRead),
            NavItem::Finished => View::Status(Status::Finished),
            NavItem::Progress => View::Progress,
        };
        navigate(&app_clone, view);
        app_clone.ui().show_sidebar_page(false);
    });

    let app_clone = app.clone();
    search.connect_search_changed(move |entry| {
        let text = entry.text().to_string();
        if app_clone.filters.borrow().search == text {
            return;
        }
        app_clone.filters.borrow_mut().search = text.clone();
        // REQ-34a: a search from Home shows its results in All Books.
        if !text.trim().is_empty() && *app_clone.view.borrow() == View::Home {
            *app_clone.view.borrow_mut() = View::AllBooks;
        }
        refresh(&app_clone);
    });

    let app_clone = app.clone();
    filter_button.popover().expect("set above").connect_show(move |_| rebuild_filter_list(&app_clone));
    let app_clone = app.clone();
    sort_button.popover().expect("set above").connect_show(move |_| rebuild_sort_list(&app_clone));

    let app_clone = app.clone();
    let action = gio::SimpleAction::new("import", None);
    action.connect_activate(move |_, _| import_dialog::present(&app_clone));
    window.add_action(&action);

    let app_clone = app.clone();
    let action = gio::SimpleAction::new("settings", None);
    action.connect_activate(move |_, _| dialogs::settings(&app_clone));
    window.add_action(&action);

    let app_clone = app.clone();
    let action = gio::SimpleAction::new("about", None);
    action.connect_activate(move |_, _| dialogs::about(&app_clone));
    window.add_action(&action);

    Ui {
        window: window.clone(),
        sidebar_view,
        stage,
        collapsed: Cell::new(false),
        showing_sidebar: Cell::new(false),
        banner,
        nav,
        nav_rows,
        nav_counts,
        title_box,
        search,
        filter_badge,
        sort_list,
        filter_list,
        tools,
        stack,
        home_categories,
        home_reading,
        home_reading_header,
        home_reading_count,
        home_empty,
        strip_scroller,
        strip,
        summary,
        canvas,
        grid_scroller,
        status_bar,
        status_count,
        status_sizing,
        slider,
        settings_button,
        no_match,
        progress_page,
        inspector,
        content_header,
        content_view,
        narrow,
        panel_overlaid: Cell::new(false),
    }
}

fn wire_after_build(app: &Rc<App>) {
    let ui = app.ui();

    let app_clone = app.clone();
    ui.grid_scroller.add_tick_callback(move |scroller, _| {
        app_clone.thumbs.pump(scroller);
        glib::ControlFlow::Continue
    });

    // Any active breakpoint means there is no room to pin the panel beside the books.
    let app_clone = app.clone();
    ui.window.connect_current_breakpoint_notify(move |window| {
        let ui = app_clone.ui();
        let current = window.current_breakpoint();
        ui.panel_overlaid.set(current.is_some());
        ui.collapsed.set(current.as_ref() == Some(&ui.narrow));
        if !ui.collapsed.get() {
            ui.showing_sidebar.set(false);
        }
        ui.apply_panel_layout();
        ui.apply_shell_layout();
    });

    // REQ-44f: bubbling, so an open popover or a focused search field gets the key first.
    let key = gtk::EventControllerKey::new();
    let app_clone = app.clone();
    key.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            return handle_escape(&app_clone);
        }
        glib::Propagation::Proceed
    });
    ui.window.add_controller(key);

    // The search field keeps Esc for itself, so when it's already empty, pass the request on.
    let app_clone = app.clone();
    ui.search.connect_stop_search(move |search| {
        if search.text().is_empty() {
            let _ = handle_escape(&app_clone);
        }
    });

    // REQ-33e, QA-13: Alt with an arrow key moves the focused tile.
    let key = gtk::EventControllerKey::new();
    key.set_propagation_phase(gtk::PropagationPhase::Capture);
    let app_clone = app.clone();
    key.connect_key_pressed(move |_, key, _, state| {
        if !state.contains(gdk::ModifierType::ALT_MASK) {
            return glib::Propagation::Proceed;
        }
        let columns = app_clone.ui().canvas.columns() as i64;
        let step = match key {
            gdk::Key::Left => -1,
            gdk::Key::Right => 1,
            gdk::Key::Up => -columns,
            gdk::Key::Down => columns,
            _ => return glib::Propagation::Proceed,
        };
        move_selected(&app_clone, step);
        glib::Propagation::Stop
    });
    ui.canvas.add_controller(key);
}

/// REQ-44f: Esc closes the inspector. Dialogs on top of the window handle their own Esc.
pub fn handle_escape(app: &Rc<App>) -> glib::Propagation {
    let ui = app.ui();
    if ui.window.visible_dialog().is_some() || !ui.inspector_visible() {
        return glib::Propagation::Proceed;
    }
    ui.inspector.request_close();
    glib::Propagation::Stop
}

/// REQ-33e: moves the selected or focused tile by `step` visible positions.
pub fn move_selected(app: &Rc<App>, step: i64) {
    let ui = app.ui();
    if app.sort.get() != SortKey::Manual {
        return; // REQ-08g
    }
    let position = ui.canvas.selected_position().or_else(|| ui.canvas.focused_position());
    let Some(position) = position else { return };
    let target = position as i64 + step;
    let visible = ui.canvas.books();
    if target < 0 || target >= visible.len() as i64 {
        return;
    }
    let (moved, over) = (visible[position].id.clone(), visible[target as usize].id.clone());
    if !app.move_book(&moved, &over) {
        return;
    }
    let title = app.book(&moved).map(|book| book.title).unwrap_or_default();
    populate_grid(app);
    ui.canvas.set_selected(Some(&moved));
    ui.canvas.focus_position(target as usize);
    // QA-13a
    ui.canvas.announce(&format!("{title} moved to position {} of {}.", target + 1, visible.len()), gtk::AccessibleAnnouncementPriority::Medium);
}

/// The scrolling book area. Every tile and row it creates gets the same click and drag behaviour.
fn book_canvas(app: &Rc<App>) -> BookCanvas {
    let app_clone = app.clone();
    let canvas = BookCanvas::new(app.thumbs.clone(), move |root, id| {
        let id_for_drag = id.clone();
        let app_for_drag = app_clone.clone();
        let app_for_drop = app_clone.clone();
        tiles::attach_reorder(
            root,
            move || (app_for_drag.sort.get() == SortKey::Manual).then(|| id_for_drag.borrow().clone()),
            move |from, to| {
                if app_for_drop.move_book(&from, &to) {
                    populate_grid(&app_for_drop);
                }
            },
        );
        // Not single_click_activate: that also selects whatever the pointer hovers over.
        let (id_for_click, app_for_click) = (id.clone(), app_clone.clone());
        util::on_activate(root, move || {
            if let Some(book) = app_for_click.book(&id_for_click.borrow()) {
                open_book(&app_for_click, book);
            }
        });
    });
    canvas.set_mode(app.preferences.get().books_view);
    canvas.set_scale(app.preferences.get().tile_scale);
    canvas
}

fn scaled(width: i32, scale: f64) -> i32 {
    (f64::from(width) * scale).round() as i32
}

/// REQ-46c: every tile resizes in this one layout pass, on Home and among the books.
pub fn set_tile_scale(app: &Rc<App>, scale: f64) {
    let scale = clamp_scale(scale);
    let mut preferences = app.preferences.get();
    if preferences.tile_scale == scale {
        return;
    }
    preferences.tile_scale = scale;
    app.preferences.set(preferences);
    let ui = app.ui();
    ui.canvas.set_scale(scale);
    ui.home_categories.set_min_width(scaled(CATEGORY_WIDTH, scale));
    ui.home_reading.set_min_width(scaled(BOOK_WIDTH, scale));
    app.save_preferences_soon();
}

fn apply_appearance(appearance: Appearance) {
    adw::StyleManager::default().set_color_scheme(match appearance {
        Appearance::System => adw::ColorScheme::Default,
        Appearance::Light => adw::ColorScheme::ForceLight,
        Appearance::Dark => adw::ColorScheme::ForceDark,
    });
}

/// REQ-47: takes effect at once. The dark-notify handler redraws the artwork.
pub fn set_appearance(app: &Rc<App>, appearance: Appearance) {
    let mut preferences = app.preferences.get();
    preferences.appearance = appearance;
    app.preferences.set(preferences);
    apply_appearance(appearance);
    app.save_preferences();
}

/// REQ-48
pub fn set_books_view(app: &Rc<App>, view: BooksView) {
    let mut preferences = app.preferences.get();
    preferences.books_view = view;
    app.preferences.set(preferences);
    app.ui().canvas.set_mode(view);
    refresh_status_bar(app);
    app.save_preferences();
}

/// The size slider applies wherever tiles are showing, so it leaves with them.
fn refresh_status_bar(app: &Rc<App>) {
    let ui = app.ui();
    let view = app.view.borrow().clone();
    ui.status_bar.set_visible(view != View::Progress);
    match view {
        View::Home => {
            let count = app.library.borrow().categories.len();
            ui.status_count.set_label(&if count == 1 { "1 category".to_string() } else { format!("{count} categories") });
            ui.status_sizing.set_visible(true);
        }
        _ => {
            ui.status_count.set_label(&books(ui.canvas.book_count()));
            ui.status_sizing.set_visible(!ui.canvas.is_list());
        }
    }
}

pub fn open_book(app: &Rc<App>, book: Book) {
    let app = app.clone();
    let inspector = app.ui().inspector.clone();
    inspector.guard(move || {
        select_in_grid(&app, &book.id);
        app.ui().inspector.open(book.clone(), false);
    });
}

fn select_in_grid(app: &App, id: &str) {
    app.ui().canvas.set_selected(Some(id));
}

pub fn navigate(app: &Rc<App>, view: View) {
    *app.view.borrow_mut() = view;
    app.ui().grid_scroller.vadjustment().set_value(0.0);
    refresh(app);
}

/// Everything derived from the library. Inspector edits and scroll position are not touched,
/// so a refresh is safe at any moment (REQ-42b).
pub fn refresh(app: &Rc<App>) {
    let ui = app.ui();
    refresh_counts(app);
    refresh_title(app);
    refresh_nav_selection(app);

    let view = app.view.borrow().clone();
    let is_empty_library = app.library.borrow().books.is_empty();
    match view {
        View::Home => {
            ui.stack.set_visible_child_name("home");
            ui.tools.set_visible(true);
            ui.home_empty.set_visible(is_empty_library);
            if is_empty_library {
                rebuild_empty_library(app);
            }
            rebuild_home(app);
        }
        View::Progress => {
            ui.stack.set_visible_child_name("progress");
            ui.tools.set_visible(false);
            progress::rebuild(app);
        }
        _ => {
            ui.stack.set_visible_child_name("books");
            ui.tools.set_visible(true);
            rebuild_strip(app);
            populate_grid(app);
        }
    }
    refresh_filter_badge(app);
    refresh_status_bar(app);
}

fn refresh_counts(app: &Rc<App>) {
    let ui = app.ui();
    let library = app.library.borrow();
    let facets = query::count_facets(&library.books);
    let status = |s: Status| facets.statuses.get(&s).copied().unwrap_or(0).to_string();
    let set = |item: NavItem, text: String| {
        if let Some(label) = ui.nav_counts.get(&item) {
            label.set_label(&text);
        }
    };
    set(NavItem::AllBooks, library.books.len().to_string());
    set(NavItem::Reading, status(Status::Reading));
    set(NavItem::ToRead, status(Status::ToRead));
    set(NavItem::Finished, status(Status::Finished));
}

fn refresh_nav_selection(app: &Rc<App>) {
    let ui = app.ui();
    let current = match &*app.view.borrow() {
        View::Home | View::Category { .. } => NavItem::Home,
        View::AllBooks => NavItem::AllBooks,
        View::Status(Status::Reading) => NavItem::Reading,
        View::Status(Status::ToRead) => NavItem::ToRead,
        View::Status(Status::Finished) => NavItem::Finished,
        View::Progress => NavItem::Progress,
    };
    if let Some((_, row)) = ui.nav_rows.iter().find(|(item, _)| *item == current) {
        ui.nav.select_row(Some(row));
    }
}

/// REQ-31c: a breadcrumb while a Category is open, the view's name otherwise.
fn refresh_title(app: &Rc<App>) {
    let ui = app.ui();
    clear(&ui.title_box);
    let view = app.view.borrow().clone();
    match view {
        View::Category { id, .. } => {
            let back = tool_button("back", "Back to Home");
            let app_clone = app.clone();
            back.connect_clicked(move |_| navigate(&app_clone, View::Home));
            ui.title_box.append(&back);
            let home = gtk::Button::builder().label("Home").css_classes(["crumb-link"]).build();
            let app_clone = app.clone();
            home.connect_clicked(move |_| navigate(&app_clone, View::Home));
            ui.title_box.append(&home);
            ui.title_box.append(&label("/", &["crumb"]));
            let name = id.as_ref().map_or_else(|| "Uncategorized".to_string(), |id| app.index().category_name(Some(id)).to_string());
            ui.title_box.append(&label(&name, &["heading"]));
        }
        other => {
            let name = match other {
                View::AllBooks => "All Books",
                View::Status(Status::Reading) => "Reading",
                View::Status(Status::ToRead) => "To Read",
                View::Status(Status::Finished) => "Finished",
                View::Progress => "Analytics",
                _ => "Home",
            };
            ui.title_box.append(&label(name, &["heading"]));
        }
    }
}

fn refresh_filter_badge(app: &Rc<App>) {
    let ui = app.ui();
    let active = app.filters.borrow().active_count();
    ui.filter_badge.set_label(&active.to_string());
    ui.filter_badge.set_visible(active > 0);
}

/// Repopulates the grid, keeping the selection on the same book when it is still visible.
pub fn populate_grid(app: &Rc<App>) {
    let ui = app.ui();
    let visible = app.visible_books();
    let total_in_view = scope_total(app);
    let shown = visible.len();
    let index = app.index();
    let sort = app.sort.get();
    // REQ-08j: inside one Category its name is already in the title, so only Sub-categories remain.
    let view = app.view.borrow().clone();
    let found = query::headings(&visible, sort, &index);
    let found = match view {
        View::Category { sub: SubFilter::All, id: Some(_) } => found.into_iter().filter(|heading| heading.level == query::HeadingLevel::Minor).collect(),
        View::Category { .. } if sort == SortKey::Category => Vec::new(),
        _ => found,
    };
    app.updating.set(true);
    ui.canvas.set_books(visible, found, index);
    app.updating.set(false);

    // REQ-09d
    let filtering = app.filters.borrow().is_filtering();
    ui.grid_scroller.set_visible(shown > 0);
    ui.no_match.set_visible(shown == 0);
    if shown == 0 {
        rebuild_no_match(app, filtering);
    }

    // "Showing 5 of 96 · Strategy, To Read".
    if filtering {
        ui.summary.set_label(&summary_text(app, shown, total_in_view));
    }
    ui.summary.set_visible(filtering && shown > 0);
}

fn scope_total(app: &Rc<App>) -> usize {
    let library = app.library.borrow();
    let view = app.view.borrow();
    library
        .books
        .iter()
        .filter(|book| match &*view {
            View::Status(status) => book.status == *status,
            View::Category { id: None, .. } => book.category_id.is_none(),
            View::Category { id: Some(id), .. } => book.category_id.as_deref() == Some(id.as_str()),
            _ => true,
        })
        .count()
}

fn summary_text(app: &Rc<App>, shown: usize, total: usize) -> String {
    let filters = app.filters.borrow();
    let index = app.index();
    let mut parts: Vec<String> = Vec::new();
    parts.extend(filters.category_ids.iter().map(|id| index.category_name(Some(id)).to_string()));
    parts.extend(filters.sub_category_ids.iter().map(|id| index.sub_category_name(Some(id)).to_string()));
    parts.extend(filters.statuses.iter().map(|status| status.as_str().to_string()));
    parts.extend(filters.priorities.iter().map(|priority| format!("{} priority", priority.as_str())));
    if !filters.search.trim().is_empty() {
        parts.push(format!("\u{201c}{}\u{201d}", filters.search.trim()));
    }
    format!("Showing {shown} of {total} \u{b7} {}", parts.join(", "))
}

fn set_mark(image: &gtk::Image) {
    let dark = adw::StyleManager::default().is_dark();
    let name = if dark { "booklub-mark-dark" } else { "booklub-mark" };
    image.set_resource(Some(&format!("/io/github/dulestrom/Booklub/logo/{name}.svg")));
    image.set_pixel_size(20);
}

/// REQ-43c, REQ-43d: the artwork with a light and a dark variant.
fn refresh_artwork(app: &Rc<App>) {
    let ui = app.ui();
    // The brand mark sits at the start of the sidebar header's first box.
    if let Some(image) = find_first::<gtk::Image>(ui.sidebar_view.upcast_ref()) {
        set_mark(&image);
    }
    let view = app.view.borrow().clone();
    if view == View::Home && app.library.borrow().books.is_empty() {
        rebuild_empty_library(app);
    }
    if ui.no_match.is_visible() {
        rebuild_no_match(app, app.filters.borrow().is_filtering());
    }
}

fn find_first<T: IsA<gtk::Widget>>(widget: &gtk::Widget) -> Option<T> {
    if let Some(found) = widget.downcast_ref::<T>() {
        return Some(found.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_first::<T>(&current) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

fn illustration(name: &str) -> gtk::Picture {
    let suffix = if adw::StyleManager::default().is_dark() { "dark" } else { "light" };
    let picture = gtk::Picture::for_resource(&format!("/io/github/dulestrom/Booklub/illustrations/{name}-{suffix}.svg"));
    picture.set_can_shrink(false);
    picture
}

/// REQ-31d
fn rebuild_empty_library(app: &Rc<App>) {
    let ui = app.ui();
    clear(&ui.home_empty);
    ui.home_empty.append(&illustration("empty-library"));
    let title = label("Your library is empty", &["title-2"]);
    title.set_xalign(0.5);
    ui.home_empty.append(&title);
    let body = label("Add a book, or import the spreadsheet you have been keeping.", &["body", "ink-2"]);
    body.set_xalign(0.5);
    body.set_wrap(true);
    body.set_max_width_chars(44);
    body.set_justify(gtk::Justification::Center);
    ui.home_empty.append(&body);
    let actions = hbox(8);
    actions.set_halign(gtk::Align::Center);
    actions.set_margin_top(8);
    let add = crate::util::text_button("Add Book", "commit");
    let app_clone = app.clone();
    add.connect_clicked(move |_| {
        let order = query::next_position(&app_clone.library.borrow().books);
        app_clone.ui().inspector.open(Book::new(order), true);
    });
    let import = crate::util::text_button("Import Spreadsheet", "secondary");
    let app_clone = app.clone();
    import.connect_clicked(move |_| import_dialog::present(&app_clone));
    actions.append(&add);
    actions.append(&import);
    ui.home_empty.append(&actions);
}

/// REQ-09d, REQ-09h
fn rebuild_no_match(app: &Rc<App>, filtering: bool) {
    let ui = app.ui();
    clear(&ui.no_match);
    ui.no_match.append(&illustration("no-matches"));
    let text = if filtering { "No books match the active criteria." } else { "No books here yet." };
    let title = label(text, &["title-2"]);
    title.set_xalign(0.5);
    ui.no_match.append(&title);
    if filtering {
        let clear_button = crate::util::text_button("Clear Filters", "secondary");
        clear_button.set_halign(gtk::Align::Center);
        let app_clone = app.clone();
        clear_button.connect_clicked(move |_| {
            app_clone.filters.borrow_mut().clear_facets();
            app_clone.filters.borrow_mut().search.clear();
            app_clone.ui().search.set_text("");
            refresh(&app_clone);
        });
        ui.no_match.append(&clear_button);
    }
}

/// REQ-32, REQ-32d, REQ-32h, REQ-33f
fn rebuild_home(app: &Rc<App>) {
    let ui = app.ui();
    ui.home_categories.clear();
    ui.home_reading.clear();

    let (categories, counts, finished, reading) = {
        let library = app.library.borrow();
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut finished: HashMap<String, usize> = HashMap::new();
        let mut none = (0usize, 0usize);
        for book in &library.books {
            match &book.category_id {
                Some(id) => {
                    *counts.entry(id.clone()).or_default() += 1;
                    if book.status == Status::Finished {
                        *finished.entry(id.clone()).or_default() += 1;
                    }
                }
                None => {
                    none.0 += 1;
                    if book.status == Status::Finished {
                        none.1 += 1;
                    }
                }
            }
        }
        let reading: Vec<Book> = library.books.iter().filter(|book| book.status == Status::Reading).cloned().collect();
        (library.categories.clone(), counts, (finished, none), reading)
    };
    let (finished_counts, uncategorized) = finished;

    for category in &categories {
        let count = counts.get(&category.id).copied().unwrap_or(0);
        let done = finished_counts.get(&category.id).copied().unwrap_or(0);
        let art = app.category_art(&category.id);
        let tile = tiles::category_tile(&category.name, count, done, None, CategoryArt { key: art.as_deref(), thumbs: &app.thumbs });
        attach_category_menu(app, &tile, &category.id);
        let (app_for_open, id_for_open) = (app.clone(), category.id.clone());
        util::on_activate(&tile, move || navigate(&app_for_open, View::Category { id: Some(id_for_open.clone()), sub: SubFilter::All }));
        let id = category.id.clone();
        let app_for_drop = app.clone();
        tiles::attach_reorder(&tile, move || Some(id.clone()), move |from, to| {
            if app_for_drop.move_category(&from, &to) {
                refresh(&app_for_drop);
            }
        });
        ui.home_categories.append(&tile);
    }

    if uncategorized.0 > 0 {
        let tile = tiles::category_tile("Uncategorized", uncategorized.0, uncategorized.1, Some("uncategorized"), CategoryArt { key: None, thumbs: &app.thumbs });
        let app_for_open = app.clone();
        util::on_activate(&tile, move || navigate(&app_for_open, View::Category { id: None, sub: SubFilter::All }));
        ui.home_categories.append(&tile);
    }

    let app_clone = app.clone();
    let new_tile = tiles::new_tile("New Category", -1, -1, move || {
        let app = app_clone.clone();
        dialogs::ask_name(&app_clone, "New Category", "Name", "", "Create", move |name| {
            if app.add_category(&name) {
                refresh(&app);
            }
        });
    });
    ui.home_categories.append(&new_tile);

    ui.home_reading_header.set_visible(!reading.is_empty());
    ui.home_reading.set_visible(!reading.is_empty());
    ui.home_reading_count.set_label(&books(reading.len()));
    for book in &reading {
        let tile = BookTile::new();
        tile.bind(book, &app.thumbs, false);
        let (app_for_open, book_for_open) = (app.clone(), book.clone());
        util::on_activate(&tile.root, move || open_book(&app_for_open, book_for_open.clone()));
        ui.home_reading.append(&tile.root);
    }
}

/// REQ-32j: Rename and Delete, by right-click or the Menu key.
fn attach_context_menu(widget: &impl IsA<gtk::Widget>, focus_host: Option<&gtk::Widget>, actions: Vec<(&'static str, Rc<dyn Fn()>)>) {
    let widget = widget.upcast_ref::<gtk::Widget>().clone();
    let popover = gtk::Popover::builder().has_arrow(false).build();
    let list = vbox(0);
    for (text, action) in actions {
        let row = gtk::Button::builder().label(text).css_classes(["menu-row"]).build();
        let popover_clone = popover.clone();
        row.connect_clicked(move |_| {
            popover_clone.popdown();
            action();
        });
        list.append(&row);
    }
    popover.set_child(Some(&list));
    popover.set_parent(&widget);

    let click = gtk::GestureClick::builder().button(gdk::BUTTON_SECONDARY).build();
    let popover_clone = popover.clone();
    click.connect_pressed(move |gesture, _, x, y| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        popover_clone.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover_clone.popup();
    });
    widget.add_controller(click);

    let key = gtk::EventControllerKey::new();
    let popover_clone = popover.clone();
    key.connect_key_pressed(move |_, key, _, state| {
        let shift_f10 = key == gdk::Key::F10 && state.contains(gdk::ModifierType::SHIFT_MASK);
        if key == gdk::Key::Menu || shift_f10 {
            popover_clone.set_pointing_to(None);
            popover_clone.popup();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    focus_host.unwrap_or(&widget).add_controller(key);
}

fn attach_category_menu(app: &Rc<App>, tile: &gtk::Overlay, id: &str) {
    let (rename_app, rename_id) = (app.clone(), id.to_string());
    let (delete_app, delete_id) = (app.clone(), id.to_string());
    let (art_app, art_id) = (app.clone(), id.to_string());
    attach_context_menu(
        tile,
        None,
        vec![
            // REQ-45i
            ("Choose Background\u{2026}", Rc::new(move || crate::category_background::present(&art_app, &art_id))),
            ("Rename", Rc::new(move || {
                let name = rename_app.index().category_name(Some(&rename_id)).to_string();
                let (app, id) = (rename_app.clone(), rename_id.clone());
                dialogs::ask_name(&rename_app, "Rename Category", "Name", &name, "Rename", move |name| {
                    if app.rename_category(&id, &name) {
                        refresh(&app);
                    }
                });
            })),
            ("Delete", Rc::new(move || dialogs::confirm_delete_category(&delete_app, &delete_id))),
        ],
    );
}

/// REQ-32b, REQ-32e, REQ-32i: All, each Sub-category, No Sub-category, New.
fn rebuild_strip(app: &Rc<App>) {
    let ui = app.ui();
    clear(&ui.strip);
    let view = app.view.borrow().clone();
    let View::Category { id: Some(category_id), sub } = view else {
        ui.strip_scroller.set_visible(false);
        return;
    };
    ui.strip_scroller.set_visible(true);

    let (category, in_category) = {
        let library = app.library.borrow();
        let category = library.categories.iter().find(|c| c.id == category_id).cloned();
        let books: Vec<Book> = library.books.iter().filter(|b| b.category_id.as_deref() == Some(category_id.as_str())).cloned().collect();
        (category, books)
    };
    let Some(category) = category else {
        navigate(app, View::Home);
        return;
    };

    let select = |target: SubFilter| {
        let (app, category_id) = (app.clone(), category_id.clone());
        move || {
            *app.view.borrow_mut() = View::Category { id: Some(category_id.clone()), sub: target.clone() };
            app.ui().grid_scroller.vadjustment().set_value(0.0);
            refresh(&app);
        }
    };

    ui.strip.append(&tiles::sub_tile("All", in_category.len(), sub == SubFilter::All, false, select(SubFilter::All)));

    for entry in &category.sub_categories {
        let count = in_category.iter().filter(|b| b.sub_category_id.as_deref() == Some(entry.id.as_str())).count();
        let selected = sub == SubFilter::Sub(entry.id.clone());
        let tile = tiles::sub_tile(&entry.name, count, selected, false, select(SubFilter::Sub(entry.id.clone())));

        let (rename_app, rename_cat, rename_sub, rename_name) = (app.clone(), category_id.clone(), entry.id.clone(), entry.name.clone());
        let (delete_app, delete_cat, delete_sub) = (app.clone(), category_id.clone(), entry.id.clone());
        attach_context_menu(
            &tile,
            None,
            vec![
                ("Rename", Rc::new(move || {
                    let (app, category, sub) = (rename_app.clone(), rename_cat.clone(), rename_sub.clone());
                    dialogs::ask_name(&rename_app, "Rename Sub-category", "Name", &rename_name, "Rename", move |name| {
                        if app.rename_sub_category(&category, &sub, &name) {
                            refresh(&app);
                        }
                    });
                })),
                ("Delete", Rc::new(move || dialogs::confirm_delete_sub(&delete_app, &delete_cat, &delete_sub))),
            ],
        );

        let id = entry.id.clone();
        let (drop_app, drop_cat) = (app.clone(), category_id.clone());
        tiles::attach_reorder(&tile, move || Some(id.clone()), move |from, to| {
            if drop_app.move_sub_category(&drop_cat, &from, &to) {
                refresh(&drop_app);
            }
        });
        ui.strip.append(&tile);
    }

    let without = in_category.iter().filter(|b| b.sub_category_id.is_none()).count();
    if without > 0 && !category.sub_categories.is_empty() {
        ui.strip.append(&tiles::sub_tile("No Sub-category", without, sub == SubFilter::NoSub, true, select(SubFilter::NoSub)));
    }

    let (new_app, new_cat) = (app.clone(), category_id.clone());
    ui.strip.append(&tiles::new_tile("New Sub-category", 180, 60, move || {
        let (app, category) = (new_app.clone(), new_cat.clone());
        dialogs::ask_name(&new_app, "New Sub-category", "Name", "", "Create", move |name| {
            if app.add_sub_category(&category, &name) {
                refresh(&app);
            }
        });
    }));
}

fn rebuild_sort_list(app: &Rc<App>) {
    let ui = app.ui();
    clear(&ui.sort_list);
    for key in SortKey::ALL {
        let row_box = hbox(10);
        let check = icon("check");
        check.set_opacity(if app.sort.get() == key { 1.0 } else { 0.0 });
        row_box.append(&check);
        row_box.append(&gtk::Label::new(Some(key.label())));
        let row = gtk::Button::builder().child(&row_box).css_classes(["menu-row"]).build();
        if app.sort.get() == key {
            row.add_css_class("current");
        }
        let app_clone = app.clone();
        row.connect_clicked(move |button| {
            app_clone.sort.set(key);
            if let Some(popover) = button.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() {
                popover.popdown();
            }
            if *app_clone.view.borrow() == View::Home {
                *app_clone.view.borrow_mut() = View::AllBooks;
            }
            refresh(&app_clone);
        });
        ui.sort_list.append(&row);
    }
}

fn filter_section(parent: &gtk::Box, heading: &str) {
    let title = label(heading, &["eyebrow"]);
    title.set_margin_top(10);
    title.set_margin_bottom(4);
    title.set_margin_start(6);
    parent.append(&title);
}

fn filter_row(parent: &gtk::Box, name: &str, count: usize, active: bool, toggle: impl Fn(bool) + 'static) {
    let row = hbox(8);
    let check = gtk::CheckButton::builder().active(active).hexpand(true).build();
    let name_label = gtk::Label::builder().label(name).xalign(0.0).build();
    check.set_child(Some(&name_label));
    row.append(&check);
    let counter = label(&count.to_string(), &["nav-count"]);
    counter.set_margin_end(6);
    row.append(&counter);
    check.connect_toggled(move |check| toggle(check.is_active()));
    parent.append(&row);
}

/// Adds or removes a value from a filter facet.
fn apply_toggle<T: PartialEq + Clone>(list: &mut Vec<T>, value: &T, on: bool) {
    list.retain(|existing| existing != value);
    if on {
        list.push(value.clone());
    }
}

/// REQ-09 to REQ-09i
fn rebuild_filter_list(app: &Rc<App>) {
    let ui = app.ui();
    let list = &ui.filter_list;
    clear(list);
    let (categories, facets) = {
        let library = app.library.borrow();
        (library.categories.clone(), query::count_facets(&library.books))
    };
    let current = app.filters.borrow().clone();

    let after = {
        let app = app.clone();
        move || {
            if *app.view.borrow() == View::Home {
                *app.view.borrow_mut() = View::AllBooks;
            }
            refresh(&app);
            update_filter_footer(&app);
        }
    };
    let after = Rc::new(after);

    filter_section(list, "CATEGORY");
    for category in &categories {
        let (id, app_clone, after) = (category.id.clone(), app.clone(), after.clone());
        filter_row(list, &category.name, facets.categories.get(&category.id).copied().unwrap_or(0), current.category_ids.contains(&category.id), move |on| {
            apply_toggle(&mut app_clone.filters.borrow_mut().category_ids, &id, on);
            after();
        });
    }

    let has_subs = categories.iter().any(|c| !c.sub_categories.is_empty());
    if has_subs {
        filter_section(list, "SUB-CATEGORY");
        for sub in categories.iter().flat_map(|c| &c.sub_categories) {
            let (id, app_clone, after) = (sub.id.clone(), app.clone(), after.clone());
            filter_row(list, &sub.name, facets.sub_categories.get(&sub.id).copied().unwrap_or(0), current.sub_category_ids.contains(&sub.id), move |on| {
                apply_toggle(&mut app_clone.filters.borrow_mut().sub_category_ids, &id, on);
                after();
            });
        }
    }

    filter_section(list, "STATUS");
    for status in [Status::Reading, Status::ToRead, Status::Finished] {
        let (app_clone, after) = (app.clone(), after.clone());
        filter_row(list, status.as_str(), facets.statuses.get(&status).copied().unwrap_or(0), current.statuses.contains(&status), move |on| {
            apply_toggle(&mut app_clone.filters.borrow_mut().statuses, &status, on);
            after();
        });
    }

    filter_section(list, "PRIORITY");
    for priority in Priority::ALL {
        let (app_clone, after) = (app.clone(), after.clone());
        filter_row(list, priority.as_str(), facets.priorities.get(&priority).copied().unwrap_or(0), current.priorities.contains(&priority), move |on| {
            apply_toggle(&mut app_clone.filters.borrow_mut().priorities, &priority, on);
            after();
        });
    }

    let divider = gtk::Box::builder().css_classes(["divider"]).margin_top(8).margin_bottom(8).build();
    list.append(&divider);
    let footer = hbox(8);
    footer.set_widget_name("filter-footer");
    footer.set_margin_start(6);
    let active = label(&active_text(current.active_count()), &["caption"]);
    active.set_hexpand(true);
    footer.append(&active);
    let clear_all = crate::util::text_button("Clear All", "secondary");
    let app_clone = app.clone();
    clear_all.connect_clicked(move |_| {
        app_clone.filters.borrow_mut().clear_facets();
        rebuild_filter_list(&app_clone);
        refresh(&app_clone);
    });
    footer.append(&clear_all);
    list.append(&footer);
}

fn active_text(count: usize) -> String {
    match count {
        0 => "No filters active".to_string(),
        1 => "1 filter active".to_string(),
        count => format!("{count} filters active"),
    }
}

fn update_filter_footer(app: &Rc<App>) {
    let ui = app.ui();
    if let Some(footer) = ui.filter_list.last_child() {
        if let Some(active) = footer.first_child().and_downcast::<gtk::Label>() {
            active.set_label(&active_text(app.filters.borrow().active_count()));
        }
    }
}

/// Builds the real sort and filter popover contents, and toggles every filter on and off.
#[cfg(feature = "dev-tools")]
pub fn dev_exercise_filters(app: &Rc<App>) -> (usize, usize) {
    fn checks(widget: &gtk::Widget, out: &mut Vec<gtk::CheckButton>) {
        if let Some(check) = widget.downcast_ref::<gtk::CheckButton>() {
            out.push(check.clone());
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            checks(&current, out);
            child = current.next_sibling();
        }
    }
    rebuild_sort_list(app);
    rebuild_filter_list(app);
    let mut found = Vec::new();
    checks(app.ui().filter_list.upcast_ref(), &mut found);
    let mut peak = 0;
    for check in &found {
        check.set_active(true);
        peak = peak.max(app.filters.borrow().active_count());
    }
    for check in &found {
        check.set_active(false);
    }
    (found.len(), peak)
}

#[cfg(feature = "dev-tools")]
pub fn dev_grid_titles(app: &App) -> Vec<String> {
    app.ui().canvas.books().into_iter().map(|book| book.title).collect()
}

/// REQ-32h: how many tiles Home's category area shows, and whether the page is visible.
#[cfg(feature = "dev-tools")]
pub fn dev_home_category_tiles(app: &App) -> (usize, bool) {
    let flow = &app.ui().home_categories;
    let count = flow.dev_child_count();
    (count, flow.is_visible() && app.ui().home_empty.is_visible())
}
