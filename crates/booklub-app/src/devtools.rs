//! Test-run hooks, compiled only with the `dev-tools` feature. Drives the app to a view from the
//! environment, then writes a PNG of the window and quits, so screens can be compared with the design.

use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use booklub_core::{Book, CategoryBackground, Status};
use gtk::{gdk, glib};

use crate::app::{App, SubFilter, View};
use crate::window::navigate;
use crate::{category_background, import_dialog, window};

static LAUNCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

pub fn mark_launch() {
    let _ = LAUNCH.set(std::time::Instant::now());
}

fn since_launch() -> f64 {
    LAUNCH.get().map_or(0.0, |start| start.elapsed().as_secs_f64() * 1000.0)
}

pub fn run(app: &Rc<App>) {
    if std::env::var("BOOKLUB_E2E").is_ok() {
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(500), move || {
            glib::spawn_future_local(async move { e2e(app).await });
        });
        return;
    }
    if std::env::var("BOOKLUB_BENCH").is_ok() {
        // QA-01, QA-30: the real launch path into All Books, including its images.
        navigate(app, View::AllBooks);
        let (shown, images) = (Rc::new(std::cell::Cell::new(false)), Rc::new(std::cell::Cell::new(false)));
        let app_clone = app.clone();
        app.ui().grid_scroller.add_tick_callback(move |_, _| {
            if !shown.replace(true) {
                println!("first frame of tiles: {:.0} ms after launch", since_launch());
                return glib::ControlFlow::Continue;
            }
            if app_clone.thumbs.in_flight() == 0 && since_launch() > 150.0 && !images.replace(true) {
                println!("first-screen images ready: {:.0} ms after launch", since_launch());
                let app = app_clone.clone();
                glib::timeout_add_local_once(Duration::from_millis(100), move || bench(&app));
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        return;
    }
    let Ok(script) = std::env::var("BOOKLUB_SHOT") else { return };
    let app = app.clone();
    // Lets the first layout and thumbnail decodes finish.
    glib::timeout_add_local_once(Duration::from_millis(600), move || step(&app, &script));
}

fn category_named(app: &App, name: &str) -> Option<String> {
    app.library.borrow().categories.iter().find(|c| c.name == name).map(|c| c.id.clone())
}

fn first_book(app: &App, predicate: impl Fn(&Book) -> bool) -> Option<Book> {
    app.library.borrow().books.iter().find(|book| predicate(book)).cloned()
}

/// `view:path.png`, for example `category:Strategy:/tmp/a.png`.
fn step(app: &Rc<App>, script: &str) {
    let (command, path) = script.rsplit_once('=').expect("BOOKLUB_SHOT is command=path");
    let mut parts = command.split(':');
    match parts.next().unwrap_or_default() {
        "home" => navigate(app, View::Home),
        "all" => navigate(app, View::AllBooks),
        "finished" => navigate(app, View::Status(Status::Finished)),
        "progress" => navigate(app, View::Progress),
        "category" => {
            let id = parts.next().and_then(|name| category_named(app, name));
            navigate(app, View::Category { id, sub: SubFilter::All });
        }
        "filtered" => {
            app.filters.borrow_mut().statuses.push(Status::ToRead);
            navigate(app, View::AllBooks);
        }
        "nomatch" => {
            app.filters.borrow_mut().search = "zzzz no such book".into();
            navigate(app, View::AllBooks);
        }
        "inspector" => {
            navigate(app, View::AllBooks);
            if let Some(book) = first_book(app, |book| book.background_file.is_some()) {
                window::open_book(app, book);
            }
        }
        "invalid-isbn" => {
            navigate(app, View::AllBooks);
            if let Some(mut book) = first_book(app, |book| book.background_file.is_none()) {
                book.isbn = Some("9780140328722".into());
                app.ui().inspector.open(book, false);
                let inspector = app.ui().inspector.clone();
                glib::spawn_future_local(async move { inspector.save_for_devtools().await });
            }
        }
        "newbook" => {
            navigate(app, View::Home);
            app.ui().inspector.open(Book::new(0), true);
        }
        "picker" => {
            navigate(app, View::Home);
            let id = parts.next().and_then(|name| category_named(app, name));
            category_background::present(app, id.as_deref().unwrap_or_default());
        }
        "import" => import_dialog::present(app),
        "settings" => crate::dialogs::settings(app),
        "about" => crate::dialogs::about(app),
        // Booklub's own sort, in either view: "sorted:priority", "sorted:category:list".
        "sorted" => {
            app.sort.set(if parts.next() == Some("priority") { booklub_core::query::SortKey::Priority } else { booklub_core::query::SortKey::Category });
            if parts.next() == Some("list") {
                window::set_books_view(app, booklub_core::preferences::BooksView::List);
            }
            navigate(app, View::AllBooks);
        }
        other => panic!("unknown BOOKLUB_SHOT command {other}"),
    }

    if let Some(scale) = std::env::var("BOOKLUB_SCALE").ok().and_then(|text| text.parse().ok()) {
        app.ui().slider.set_value(scale);
    }
    if let Some(offset) = std::env::var("BOOKLUB_SCROLL").ok().and_then(|text| text.parse::<f64>().ok()) {
        let adjustment = app.ui().grid_scroller.vadjustment();
        glib::timeout_add_local_once(Duration::from_millis(500), move || adjustment.set_value(offset));
    }
    let window = app.ui().window.clone();
    if let Some(size) = std::env::var("BOOKLUB_SIZE").ok().and_then(|s| s.split_once('x').map(|(w, h)| (w.parse().ok(), h.parse().ok()))) {
        if let (Some(w), Some(h)) = size {
            window.set_default_size(w, h);
        }
    }
    let path = path.to_string();
    glib::timeout_add_local_once(Duration::from_millis(900), move || capture(&window, &path));
}

fn capture(window: &adw::ApplicationWindow, path: &str) {
    crate::util::dev_header_bounds(window.upcast_ref(), window.upcast_ref());
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let (width, height) = (window.width() as f64, window.height() as f64);
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, width, height);
    let Some(node) = snapshot.to_node() else {
        eprintln!("nothing to capture");
        std::process::exit(1);
    };
    let Some(renderer) = window.native().and_then(|native| native.renderer()) else {
        eprintln!("no renderer");
        std::process::exit(1);
    };
    let texture: gdk::Texture = renderer.render_texture(&node, None);
    texture.save_to_png(path).expect("the capture path is writable");
    println!("captured {path} at {width}x{height}");
    if let Some(application) = window.application() {
        application.quit();
    }
}

async fn pause(ms: u64) {
    glib::timeout_future(Duration::from_millis(ms)).await;
}

struct Checks {
    failed: usize,
    passed: usize,
}

impl Checks {
    fn check(&mut self, name: &str, ok: bool) {
        if ok {
            self.passed += 1;
            println!("PASS  {name}");
        } else {
            self.failed += 1;
            println!("FAIL  {name}");
        }
    }
}

/// Drives the real window through the flows the unit tests cannot reach.
async fn e2e(app: Rc<App>) {
    let mut c = Checks { failed: 0, passed: 0 };
    let ui = app.ui();
    let inspector = ui.inspector.clone();
    let backgrounds = || std::fs::read_dir(&app.paths.backgrounds).map(|d| d.count()).unwrap_or(0);

    c.check("first run seeds Fiction and Non-fiction only (REQ-05j)", app.library.borrow().categories.iter().map(|c| c.name.as_str()).eq(["Fiction", "Non-fiction"]));
    c.check("first run has both goals (REQ-14f)", app.library.borrow().goals.len() == 2);

    // REQ-31d, REQ-32h: the empty message must not take the categories away.
    pause(100).await;
    let fit = app.ui().home_categories.dev_fit();
    c.check("Home's tiles fill the row to its right edge (REQ-46)", fit.x(fit.columns - 1) + fit.width(fit.columns - 1) == app.ui().home_categories.width());
    let (tiles, both_showing) = crate::window::dev_home_category_tiles(&app);
    c.check("an empty Home shows the message and the category tiles (REQ-31d)", both_showing);
    c.check("an empty Home has its 2 categories plus New Category (REQ-32h)", tiles == 3);
    c.check("a category can be created with no books", app.add_category("Прва категорија") && app.store.load_all().unwrap().categories.len() == 3);
    navigate(&app, View::Home);
    pause(50).await;
    c.check("the new tile appears straight away", crate::window::dev_home_category_tiles(&app).0 == 4);
    c.check("it goes before the New tile, still with no books", app.library.borrow().books.is_empty());

    // UC-01
    inspector.open(Book::new(0), true);
    c.check("add opens an inspector with Save pending", ui.inspector_visible());
    inspector.clone().save_for_devtools().await;
    c.check("an empty title blocks the save (REQ-01a)", app.library.borrow().books.is_empty());

    inspector.dev_set(Some("  Ѓердан од ќерамиди  "), None, Some("100"), None);
    c.check("unsaved typing marks the inspector dirty (REQ-44c)", inspector.is_dirty());
    inspector.clone().save_for_devtools().await;
    c.check("the book saves (REQ-01)", app.library.borrow().books.len() == 1);
    c.check("the save reached SQLite", app.store.load_all().map(|l| l.books.len()).unwrap_or(0) == 1);
    c.check("saving clears the dirty state", !inspector.is_dirty());
    c.check("a new book takes the last position (REQ-01c)", app.library.borrow().books[0].order == 0);

    // REQ-44f: the handler behind the Esc key, and the panel's own width.
    let (panel_min, _, _, _) = inspector.root().measure(gtk::Orientation::Horizontal, -1);
    c.check("the panel fits its 320 px without clipping (QA-14)", panel_min <= 320);
    // Saving a new book closes the panel, so this opens the saved one to measure it.
    inspector.open(app.library.borrow().books[0].clone(), false);
    pause(100).await;
    // The width leaves out the panel's 1 px border.
    c.check("the panel is exactly 320 px wide", inspector.root().width() == 319);
    c.check("the toolbar and panel headers line up", ui.dev_header_lines() == (46.0, 46.0));
    // Every painted frame after opening: a slide or a late correction would show as a second position.
    inspector.close();
    pause(50).await;
    let positions = Rc::new(std::cell::RefCell::new(Vec::new()));
    let (recorded, window, panel) = (positions.clone(), ui.window.clone(), inspector.root().clone());
    let clock = ui.window.frame_clock().expect("the window is on screen");
    let painting = clock.connect_after_paint(move |_| {
        if let Some(bounds) = panel.compute_bounds(&window).filter(|_| panel.is_visible()) {
            let position = (bounds.x() as i32, bounds.width() as i32);
            let mut seen = recorded.borrow_mut();
            if seen.last() != Some(&position) {
                seen.push(position);
            }
        }
    });
    inspector.open(app.library.borrow().books[0].clone(), false);
    ui.window.queue_draw();
    pause(400).await;
    clock.disconnect(painting);
    c.check("the panel opens in one step, with no slide or late correction", positions.borrow().len() == 1);
    let (books, sidebar) = ui.dev_collapsed_pages();
    c.check("a narrow window shows the books page alone", books == (false, true));
    c.check("a narrow window can show the sidebar page alone", sidebar == (true, false));
    window::handle_escape(&app);
    pause(50).await;
    c.check("Esc closes a panel with nothing unsaved (REQ-44f)", !ui.inspector_visible());
    c.check("Esc does nothing more once it is closed", window::handle_escape(&app) == glib::Propagation::Proceed);
    let reopened = app.library.borrow().books[0].clone();
    inspector.open(reopened.clone(), false);
    inspector.dev_set(Some("Typed but not saved"), None, None, None);
    window::handle_escape(&app);
    pause(100).await;
    c.check("Esc with unsaved edits asks first (REQ-44c)", ui.inspector_visible() && ui.window.visible_dialog().is_some());
    c.check("Esc leaves that question to the dialog", window::handle_escape(&app) == glib::Propagation::Proceed);
    if let Some(question) = ui.window.visible_dialog() {
        question.close();
    }
    pause(100).await;
    c.check("the edits are still there after that dialog closes", inspector.dev_title() == "Typed but not saved");
    inspector.open(reopened, false);

    // REQ-36b
    inspector.dev_set(None, Some("9780140328722"), None, None);
    inspector.clone().save_for_devtools().await;
    c.check("a bad ISBN blocks the save with the Appendix F text (REQ-36b)", inspector.dev_isbn_error().as_deref() == Some("That ISBN is not valid. Check the digits and try again."));
    c.check("the bad ISBN was not stored", app.store.load_all().unwrap().books[0].isbn.is_none());
    inspector.dev_set(None, Some("978-0-14-032872-1"), None, None);
    inspector.clone().save_for_devtools().await;
    c.check("a hyphenated ISBN saves normalised (REQ-36a)", app.store.load_all().unwrap().books[0].isbn.as_deref() == Some("9780140328721"));

    // REQ-35k to REQ-35r, using the real system clipboard.
    let clipboard = inspector.root().clipboard();
    clipboard.set_text("https://example.com/cover.jpg");
    pause(120).await;
    inspector.dev_paste();
    pause(300).await;
    let feedback = inspector.dev_feedback().join(" ");
    c.check("a copied link gives the no-image message (REQ-35o)", feedback.contains("The clipboard has no image."));
    c.check("a copied link leaves the book unchanged", !inspector.dev_has_pending_image());

    let pixels = vec![200u8; 1000 * 1500 * 4];
    let bytes = glib::Bytes::from_owned(pixels);
    let texture = gdk::MemoryTexture::new(1000, 1500, gdk::MemoryFormat::R8g8b8a8, &bytes, 4000);
    if !app.ui().window.is_active() { println!("NOTE the window lost focus, so paste checks may fail"); }
    clipboard.set_texture(&texture);
    pause(150).await;
    inspector.dev_paste();
    pause(1500).await;
    c.check("a clipboard image previews (REQ-35k)", inspector.dev_has_pending_image());
    c.check("the preview says it is not saved yet", inspector.dev_feedback().iter().any(|t| t == "NOT SAVED YET"));
    c.check("nothing is stored before Save (REQ-35r)", backgrounds() == 0);
    inspector.clone().save_for_devtools().await;
    let stored = app.store.load_all().unwrap().books[0].background_file.clone();
    c.check("Save stores the private copy (REQ-35a)", stored.is_some() && backgrounds() == 1);
    let file = stored.unwrap_or_default();
    c.check("Save writes a thumbnail", booklub_core::background::thumbnail_path(&app.paths.thumbnails, &file).exists());

    let small = gdk::MemoryTexture::new(200, 300, gdk::MemoryFormat::R8g8b8a8, &glib::Bytes::from_owned(vec![90u8; 200 * 300 * 4]), 800);
    if !app.ui().window.is_active() { println!("NOTE the window lost focus, so paste checks may fail"); }
    clipboard.set_texture(&small);
    pause(150).await;
    inspector.dev_paste();
    let mut waited = 0;
    while waited < 8000 && !inspector.dev_feedback().iter().any(|t| t.contains("may look blurry")) {
        pause(50).await;
        waited += 50;
    }
    println!("small image feedback after {waited} ms: {:?}", inspector.dev_feedback());
    c.check("a small image warns and is still allowed (REQ-35q)", inspector.dev_feedback().iter().any(|t| t.contains("may look blurry")) && inspector.dev_has_pending_image());
    // Discard restores the previous background.
    if let Some(book) = app.book(&app.library.borrow().books[0].id.clone()) {
        inspector.open(book, false);
    }
    c.check("discarding keeps the previous background (Appendix F)", backgrounds() == 1 && app.store.load_all().unwrap().books[0].background_file.as_deref() == Some(file.as_str()));

    inspector.dev_remove_background();
    c.check("removing is only pending until Save", backgrounds() == 1);
    inspector.clone().save_for_devtools().await;
    c.check("Save deletes the stored files (REQ-35h)", backgrounds() == 0 && !booklub_core::background::thumbnail_path(&app.paths.thumbnails, &file).exists());

    // Status rules through the real dropdown.
    inspector.dev_set_status(Status::Finished);
    pause(50).await;
    inspector.clone().save_for_devtools().await;
    let book = app.store.load_all().unwrap().books[0].clone();
    c.check("Finished fills the date and pages (REQ-13d, REQ-13e)", book.finish_date.is_some() && book.current_page == 100);
    c.check("Finished leaves the start date empty (REQ-13f)", book.start_date.is_none());

    // REQ-42b: the appearance flips under unsaved edits.
    inspector.dev_set(Some("Edited but not saved"), None, None, None);
    let manager = adw::StyleManager::default();
    let was_dark = manager.is_dark();
    manager.set_color_scheme(if was_dark { adw::ColorScheme::ForceLight } else { adw::ColorScheme::ForceDark });
    pause(200).await;
    c.check("the appearance switched (REQ-42a)", manager.is_dark() != was_dark);
    c.check("unsaved edits survive the switch (REQ-42b)", inspector.dev_title() == "Edited but not saved" && inspector.is_dirty());
    manager.set_color_scheme(adw::ColorScheme::Default);
    pause(100).await;
    if let Some(book) = app.book(&book.id) {
        inspector.open(book, false);
    }
    c.check("reopening discards the edit", !inspector.is_dirty());

    // Categories.
    let first_category = app.library.borrow().categories[0].id.clone();
    app.add_sub_category(&first_category, "Pricing");
    let category = app.library.borrow().categories[0].clone();
    let mut filed = app.book(&book.id).unwrap();
    filed.category_id = Some(category.id.clone());
    filed.sub_category_id = Some(category.sub_categories[0].id.clone());
    app.save_book(&filed);
    c.check("rename reaches the store (REQ-05e)", app.rename_category(&category.id, "Продукт") && app.store.load_all().unwrap().categories[0].name == "Продукт");
    navigate(&app, View::Category { id: Some(category.id.clone()), sub: SubFilter::All });
    pause(100).await;
    c.check("the category view shows the book", ui.canvas.book_count() == 1);
    c.check("delete sub keeps the category (REQ-05g)", app.delete_sub_category(&category.id, &category.sub_categories[0].id) && {
        let b = app.store.load_all().unwrap().books[0].clone();
        b.category_id.is_some() && b.sub_category_id.is_none()
    });
    c.check("delete category clears the book (REQ-05f)", app.delete_category(&category.id) && app.store.load_all().unwrap().books[0].category_id.is_none());

    // Order, by the real keyboard path.
    for title in ["B", "C"] {
        let mut extra = Book::new(app.library.borrow().books.len() as i64);
        extra.title = title.into();
        app.save_book(&extra);
    }
    navigate(&app, View::AllBooks);
    pause(100).await;
    ui.canvas.set_selected(Some(&ui.canvas.books()[0].id));
    crate::window::move_selected(&app, 1);
    pause(50).await;
    let order: Vec<String> = app.store.load_all().unwrap().books.iter().map(|b| b.title.clone()).collect();
    c.check("Alt+Right moves a tile and persists (REQ-33e, REQ-07a)", order[1].contains("Ѓердан") && order[0] == "B");
    app.sort.set(booklub_core::query::SortKey::Title);
    crate::window::populate_grid(&app);
    ui.canvas.set_selected(Some(&ui.canvas.books()[0].id));
    crate::window::move_selected(&app, 1);
    let unchanged: Vec<String> = app.store.load_all().unwrap().books.iter().map(|b| b.title.clone()).collect();
    c.check("a sorted grid refuses to reorder (REQ-08g)", unchanged == order);
    app.sort.set(booklub_core::query::SortKey::Manual);

    display_checks(&app, &mut c).await;

    // REQ-03
    let doomed = app.library.borrow().books[0].clone();
    c.check("delete removes the book and renumbers (REQ-03, REQ-07c)", app.delete_book(&doomed) && {
        let books = app.store.load_all().unwrap().books;
        books.len() == 2 && books.iter().enumerate().all(|(i, b)| b.order == i as i64)
    });

    // QA-16
    let mut sneaky = Book::new(booklub_core::query::next_position(&app.library.borrow().books));
    sneaky.title = "<b>x</b>".into();
    app.save_book(&sneaky);
    navigate(&app, View::AllBooks);
    pause(100).await;
    c.check("markup in a title is stored literally (QA-16)", app.book(&sneaky.id).is_some_and(|b| b.title == "<b>x</b>"));

    // REQ-45: Category backgrounds, through the real picker and the real clipboard.
    let art_category = app.library.borrow().categories.last().map(|c| c.id.clone()).unwrap_or_default();
    let saved_art = |id: &str| app.store.load_all().unwrap().categories.iter().find(|c| c.id == id).and_then(|c| c.background.clone());
    let files = || std::fs::read_dir(&app.paths.backgrounds).map(|d| d.count()).unwrap_or(0);
    let files_before = files();

    let picker = category_background::dev::open(&app, &art_category);
    pause(150).await;
    c.check("the picker offers eight artworks (REQ-45)", picker.choices() == 8);
    picker.click_preset("ink-wash");
    c.check("choosing marks it with more than colour", picker.preset_marked("ink-wash"));
    c.check("a choice persists only on Save (REQ-45e)", saved_art(&art_category).is_none());
    picker.cancel();
    pause(100).await;
    c.check("Cancel leaves the category as it was", saved_art(&art_category).is_none());

    let picker = category_background::dev::open(&app, &art_category);
    pause(100).await;
    picker.click_preset("very-bright");
    picker.save().await;
    pause(100).await;
    c.check("Save stores the built-in artwork", saved_art(&art_category) == Some(CategoryBackground::Preset("very-bright".into())));
    c.check("the tile shows it (REQ-45f)", app.category_art(&art_category).as_deref() == Some("preset:very-bright"));
    c.check("built-in artwork writes no files", files() == files_before);

    let clipboard = inspector.root().clipboard();
    let picker = category_background::dev::open(&app, &art_category);
    pause(100).await;
    c.check("reopening shows the saved choice", picker.preset_marked("very-bright"));
    clipboard.set_text("https://example.com/art.jpg");
    pause(120).await;
    picker.paste();
    pause(300).await;
    c.check("a copied link gives the no-image message (REQ-45b)", picker.feedback_text().contains("The clipboard has no image."));
    c.check("a copied link keeps the earlier choice", !picker.has_pending_image() && saved_art(&art_category).is_some());
    let wide = gdk::MemoryTexture::new(1000, 600, gdk::MemoryFormat::R8g8b8a8, &glib::Bytes::from_owned(vec![180u8; 1000 * 600 * 4]), 4000);
    if !app.ui().window.is_active() { println!("NOTE the window lost focus, so paste checks may fail"); }
    clipboard.set_texture(&wide);
    pause(150).await;
    picker.paste();
    let mut waited = 0;
    while waited < 8000 && !picker.has_pending_image() {
        pause(50).await;
        waited += 50;
    }
    c.check("a pasted image previews (REQ-45b)", picker.has_pending_image());
    c.check("a good size gives no warning (REQ-45h)", !picker.feedback_text().contains("blurry"));
    c.check("a paste stores nothing before Save (REQ-45e)", files() == files_before);
    picker.save().await;
    pause(100).await;
    let stored = match saved_art(&art_category) {
        Some(CategoryBackground::File(name)) => name,
        _ => String::new(),
    };
    c.check("Save stores the private copy (REQ-45a)", !stored.is_empty() && files() == files_before + 1);
    let wide_thumb = booklub_core::background::wide_thumbnail_path(&app.paths.thumbnails, &stored);
    c.check("Save writes a landscape thumbnail (REQ-45g)", wide_thumb.exists());
    let referenced = app.store.referenced_backgrounds().unwrap();
    c.check("the startup sweep keeps it (QA-33)", referenced.contains(&stored));

    let picker = category_background::dev::open(&app, &art_category);
    pause(100).await;
    let small = gdk::MemoryTexture::new(300, 200, gdk::MemoryFormat::R8g8b8a8, &glib::Bytes::from_owned(vec![90u8; 300 * 200 * 4]), 1200);
    if !app.ui().window.is_active() { println!("NOTE the window lost focus, so paste checks may fail"); }
    clipboard.set_texture(&small);
    pause(150).await;
    picker.paste();
    let mut waited = 0;
    while waited < 8000 && !picker.feedback_text().contains("blurry") {
        pause(50).await;
        waited += 50;
    }
    c.check("a small image warns and is still allowed (REQ-45h)", picker.feedback_text().contains("blurry") && picker.has_pending_image());
    picker.cancel();
    pause(100).await;
    c.check("cancelling keeps the stored image", files() == files_before + 1);

    let picker = category_background::dev::open(&app, &art_category);
    pause(100).await;
    picker.click_preset("ochre-fields");
    picker.save().await;
    pause(100).await;
    c.check("replacing an upload deletes its files (REQ-35h)", files() == files_before && !wide_thumb.exists());

    // Fallback: with the choice removed, the first book with a background shows again.
    let mut filed = Book::new(booklub_core::query::next_position(&app.library.borrow().books));
    filed.title = "Has art".into();
    filed.category_id = Some(art_category.clone());
    filed.background_file = Some("fallback.jpg".into());
    app.save_book(&filed);
    c.check("a choice beats the book's background (REQ-45f)", app.category_art(&art_category).as_deref() == Some("preset:ochre-fields"));
    let picker = category_background::dev::open(&app, &art_category);
    pause(100).await;
    picker.remove();
    c.check("Remove waits for Save too", saved_art(&art_category).is_some());
    picker.save().await;
    pause(100).await;
    c.check("Remove falls back to the first book's background (REQ-45c)", saved_art(&art_category).is_none() && app.category_art(&art_category).as_deref() == Some("fallback.jpg"));
    app.delete_book(&filed);

    // A category's upload survives a backup, and goes when the category does.
    let picker = category_background::dev::open(&app, &art_category);
    pause(100).await;
    if !app.ui().window.is_active() { println!("NOTE the window lost focus, so paste checks may fail"); }
    clipboard.set_texture(&wide);
    pause(150).await;
    picker.paste();
    let mut waited = 0;
    while waited < 8000 && !picker.has_pending_image() {
        pause(50).await;
        waited += 50;
    }
    picker.save().await;
    pause(100).await;
    let stored = match saved_art(&art_category) {
        Some(CategoryBackground::File(name)) => name,
        _ => String::new(),
    };
    let archive_path = app.paths.backups.join("e2e.booklub");
    let snapshot = app.library.borrow().clone();
    let exported = booklub_core::archive::export(&snapshot, &app.paths.backgrounds, &archive_path).is_ok();
    let restored = booklub_core::archive::read(&archive_path);
    c.check("a backup carries the category's image (REQ-37)", exported && restored.is_ok_and(|r| r.images.contains_key(&stored)));
    let doomed_category = app.library.borrow().categories.iter().find(|c| c.id == art_category).cloned();
    c.check("deleting the category deletes its files (REQ-45d)", app.delete_category(&art_category) && doomed_category.is_some() && !app.paths.backgrounds.join(&stored).exists());
    navigate(&app, View::Home);
    pause(100).await;

    // Popovers, wizard and restore, through the real UI code.
    let (boxes, peak) = crate::window::dev_exercise_filters(&app);
    c.check("the filter popover builds and every box toggles (REQ-09)", boxes >= 7 && peak == boxes);
    c.check("clearing leaves no filters behind (REQ-09h)", app.filters.borrow().active_count() == 0);

    if let (Ok(xlsx), Ok(v1)) = (std::env::var("BOOKLUB_E2E_XLSX"), std::env::var("BOOKLUB_E2E_V1")) {
        let before = app.library.borrow().books.len();
        let wizard = import_dialog::dev::open(&app);
        pause(150).await;
        c.check("the workbook loads into the wizard (REQ-18)", wizard.load_sheet(std::path::Path::new(&xlsx)).is_ok());
        pause(150).await;
        c.check("headers auto-map (REQ-18a-i)", wizard.mapping().contains(&booklub_core::spreadsheet::ImportField::Title));
        let unknown = wizard.unknown_statuses();
        c.check("unread statuses reach the mapping step (REQ-40a)", unknown.iter().any(|v| v == "Unread") && unknown.iter().any(|v| v == "Done"));
        wizard.choose_status("Done", Status::Finished);
        pause(50).await;
        c.check("the plan counts titled rows only (REQ-18d)", wizard.planned() == 3 && wizard.can_import());
        c.check("the button names the count", wizard.import_label() == "Import 3 Books");
        wizard.run_import();
        pause(150).await;
        let library = app.store.load_all().unwrap();
        c.check("the import reached SQLite and appended (REQ-18f)", library.books.len() == before + 3 && library.books.last().is_some_and(|b| b.order == before as i64 + 2));
        let find = |title: &str| library.books.iter().find(|b| b.title == title).cloned();
        c.check("Cyrillic survives the workbook (QA-19)", find("Ѓердан од ќерамиди").is_some());
        c.check("\"Unread\" stays To Read, the web bug is gone", find("Ѓердан од ќерамиди").is_some_and(|b| b.status == Status::ToRead));
        c.check("\"Done\" follows the Reader's choice", find("Second Row").is_some_and(|b| b.status == Status::Finished && b.finish_date.is_some()));
        c.check("slashed categories split (REQ-18c)", find("Second Row").and_then(|b| b.sub_category_id).is_some());
        c.check("a bad ISBN imports empty (REQ-40c)", find("Third Row").is_some_and(|b| b.isbn.is_none()));
        wizard.close();

        let path = std::path::Path::new(&v1);
        let held = app.library.borrow().books.len();
        let wizard = import_dialog::dev::open(&app);
        pause(100).await;
        c.check("a web v1 export merges (REQ-19d, REQ-38)", wizard.restore_merge(path).is_ok());
        pause(150).await;
        let merged = app.store.load_all().unwrap();
        c.check("merge appends after what is held", merged.books.len() == held + 2 && merged.books.iter().enumerate().all(|(i, b)| b.order == i as i64));
        c.check("merging again adds nothing (REQ-19d)", wizard.restore_merge(path).is_ok() && app.store.load_all().unwrap().books.len() == held + 2);
        c.check("a web v1 export replaces (REQ-19f)", wizard.restore_replace(path).is_ok());
        pause(150).await;
        let replaced = app.store.load_all().unwrap();
        c.check("replace keeps the web manual order (REQ-38)", replaced.books.len() == 2 && replaced.books[0].title == "Ѓердан од ќерамиди" && replaced.books[1].title == "Web Second");
        c.check("replace keeps the file's goals", replaced.goals.iter().any(|g| g.target == 30));
        navigate(&app, View::Home);
        pause(150).await;
        navigate(&app, View::AllBooks);
        pause(100).await;
        c.check("the grid shows the restored books", crate::window::dev_grid_titles(&app).len() == 2);
        wizard.close();
    } else {
        println!("SKIP  wizard and restore (no fixtures given)");
    }

    println!("RESULT {} passed, {} failed", c.passed, c.failed);
    std::process::exit(i32::from(c.failed > 0));
}

/// REQ-08j, REQ-46, REQ-47, REQ-48: section headings, tiles that fill the row, the slider, and
/// the two display settings.
async fn display_checks(app: &Rc<App>, c: &mut Checks) {
    use booklub_core::layout::Fit;
    use booklub_core::preferences::{Appearance, BooksView, Preferences};
    use booklub_core::query::{HeadingLevel, SortKey};
    use booklub_core::{Book, Category, Priority, SubCategory};

    let ui = app.ui();
    let (kept_books, kept_categories) = {
        let library = app.library.borrow();
        (library.books.len(), library.categories.len())
    };
    app.add_category("Zz Shelf");
    let shelf = app.library.borrow().categories.last().cloned().expect("just added");
    app.add_sub_category(&shelf.id, "Zz Sub");
    let shelf: Category = app.library.borrow().categories.iter().find(|c| c.id == shelf.id).cloned().expect("just added");
    let sub: SubCategory = shelf.sub_categories[0].clone();
    let mut added = Vec::new();
    for number in 0..24 {
        let mut book = Book::new(1000 + number);
        book.title = format!("Display {number}");
        book.priority = if number % 2 == 0 { Priority::High } else { Priority::Low };
        book.category_id = Some(shelf.id.clone());
        book.sub_category_id = (number % 3 != 0).then(|| sub.id.clone());
        app.save_book(&book);
        added.push(book);
    }

    app.sort.set(SortKey::Priority);
    navigate(app, View::AllBooks);
    pause(100).await;
    let titles: Vec<String> = ui.canvas.headings().iter().map(|h| h.title.clone()).collect();
    c.check("sorting by priority heads each level (REQ-08j)", titles.iter().any(|t| t == "High Priority") && titles.iter().any(|t| t == "Low Priority"));

    app.sort.set(SortKey::Category);
    crate::window::populate_grid(app);
    pause(100).await;
    let headings = ui.canvas.headings();
    c.check("sorting by category heads each one (REQ-08j)", headings.iter().any(|h| h.title == "Zz Shelf" && h.level == HeadingLevel::Major && h.count == 24));
    c.check("its sub-categories have their own headings (REQ-08k)", headings.iter().any(|h| h.title == "Zz Sub" && h.level == HeadingLevel::Minor && h.count == 16));
    c.check("books without one are named too (REQ-08k)", headings.iter().any(|h| h.title == "No Sub-category" && h.count == 8));

    navigate(app, View::Category { id: Some(shelf.id.clone()), sub: SubFilter::All });
    pause(100).await;
    c.check("inside a category, only its sub-categories head the books", ui.canvas.headings().iter().all(|h| h.level == HeadingLevel::Minor) && !ui.canvas.headings().is_empty());

    app.sort.set(SortKey::Manual);
    navigate(app, View::AllBooks);
    pause(100).await;
    c.check("manual order has no headings", ui.canvas.headings().is_empty());

    // Rows fill the width, with room for one more tile adding a column.
    let inner = ui.canvas.width() - 48;
    let edges = ui.canvas.dev_tile_edges();
    let rightmost = edges.iter().map(|(x, width, _)| x + width).max().unwrap_or(0);
    c.check("the last tile ends at the row's right edge (REQ-46a)", rightmost == 24 + inner);
    let columns = ui.canvas.columns();
    c.check("tiles grow rather than leave a gap (REQ-46b)", edges.first().is_some_and(|(_, width, _)| *width == Fit::new(inner, 148, 16).width(0) && *width >= 148));
    c.check("tiles keep their 2:3 shape (REQ-46)", edges.first().is_some_and(|(_, width, height)| (f64::from(*height) / f64::from(*width) - 1.5).abs() < 0.02));
    println!("{} columns across {inner} px, tile {:?}, {} widgets mounted", columns, edges.first(), ui.canvas.dev_mounted());

    // The slider, through the real control.
    ui.slider.set_value(1.6);
    pause(100).await;
    c.check("a larger size means fewer columns (REQ-46c)", ui.canvas.columns() < columns);
    let large = ui.canvas.dev_tile_edges();
    c.check("the rows still end at the right edge", large.iter().map(|(x, width, _)| x + width).max().unwrap_or(0) == 24 + inner);
    ui.slider.set_value(0.75);
    pause(100).await;
    c.check("a smaller size means more columns (REQ-46c)", ui.canvas.columns() > columns);
    pause(500).await;
    c.check("the size is saved (REQ-47a)", (Preferences::load(&app.paths.preferences).tile_scale - 0.75).abs() < 0.001);
    navigate(app, View::Home);
    pause(100).await;
    let fit = ui.home_categories.dev_fit();
    c.check("the size reaches Home's tiles too (REQ-46c)", fit.width(0) >= 162 && fit.x(fit.columns - 1) + fit.width(fit.columns - 1) == ui.home_categories.width());
    navigate(app, View::AllBooks);
    ui.slider.set_value(1.0);
    pause(100).await;

    // A change of size keeps the same books on screen.
    let adjustment = ui.grid_scroller.vadjustment();
    let row_height = ui.canvas.dev_tile_edges().first().map_or(222, |edge| edge.2);
    // Halfway down the third row, so that row's first book is the one at the top of the view.
    adjustment.set_value(f64::from(16 + 2 * (row_height + 16) + row_height / 2));
    pause(100).await;
    let anchor = ui.canvas.columns() as usize * 2;
    ui.slider.set_value(1.3);
    pause(100).await;
    let new_columns = ui.canvas.columns() as usize;
    let new_height = ui.canvas.dev_tile_edges().first().map_or(0, |edge| edge.2);
    let top = f64::from(16 + (anchor / new_columns) as i32 * (new_height + 16));
    c.check("resizing keeps the same book at the top (REQ-46d)", new_columns < anchor && adjustment.value() >= top && adjustment.value() <= top + f64::from(new_height));
    ui.slider.set_value(1.0);
    adjustment.set_value(0.0);
    pause(100).await;

    // The arrow keys, across a heading.
    app.sort.set(SortKey::Priority);
    crate::window::populate_grid(app);
    pause(100).await;
    let first_low = ui.canvas.headings().iter().find(|h| h.title == "Low Priority").map(|h| h.first_book);
    if let Some(first_low) = first_low {
        let last_high = first_low - 1;
        let columns = ui.canvas.columns() as usize;
        let column = last_high % columns;
        c.check("Down skips a heading and keeps the column (REQ-08j)", ui.canvas.dev_arrow(last_high, gtk::gdk::Key::Down) == Some(first_low + column.min(ui.canvas.book_count() - first_low - 1)));
        c.check("Right moves on to the next book", ui.canvas.dev_arrow(last_high, gtk::gdk::Key::Right) == Some(first_low));
    }
    c.check("the first book has nothing to its left", ui.canvas.dev_arrow(0, gtk::gdk::Key::Left).is_none());
    gtk::prelude::GtkWindowExt::set_focus(&ui.window, None::<&gtk::Widget>);
    c.check("Tab enters the books", ui.canvas.child_focus(gtk::DirectionType::TabForward));
    c.check("the next Tab leaves them", ui.canvas.focused_position().is_some() && !ui.canvas.child_focus(gtk::DirectionType::TabForward));
    app.sort.set(SortKey::Manual);
    crate::window::populate_grid(app);

    // List view
    crate::window::set_books_view(app, BooksView::List);
    pause(100).await;
    c.check("the list holds every book (REQ-48)", ui.canvas.is_list() && ui.canvas.columns() >= 1 && ui.canvas.book_count() >= 24);
    let rows = ui.canvas.dev_tile_edges();
    c.check("list rows span the width (REQ-48a)", rows.iter().all(|(x, width, _)| *x == 24 && *width == inner));
    c.check("the slider has no effect there, so it hides (REQ-48b)", !ui.slider.is_mapped());
    app.sort.set(SortKey::Category);
    crate::window::populate_grid(app);
    pause(100).await;
    c.check("the list has headings too (REQ-48c)", !ui.canvas.headings().is_empty());
    app.sort.set(SortKey::Manual);
    c.check("the list view is saved (REQ-47a)", Preferences::load(&app.paths.preferences).books_view == BooksView::List);
    crate::window::set_books_view(app, BooksView::Pictures);
    navigate(app, View::AllBooks);
    pause(100).await;
    c.check("pictures come back with their tiles", !ui.canvas.is_list() && ui.canvas.dev_tile_edges().iter().all(|edge| edge.2 > edge.1));
    c.check("the slider comes back", ui.slider.is_mapped());

    // Appearance
    let style = adw::StyleManager::default();
    crate::window::set_appearance(app, Appearance::Dark);
    pause(50).await;
    c.check("Dark forces the dark tokens (REQ-47)", style.is_dark());
    crate::window::set_appearance(app, Appearance::Light);
    pause(50).await;
    c.check("Light forces the light tokens (REQ-47)", !style.is_dark());
    c.check("the appearance is saved (REQ-47a)", Preferences::load(&app.paths.preferences).appearance == Appearance::Light);
    crate::window::set_appearance(app, Appearance::System);
    c.check("System hands it back (REQ-42)", style.color_scheme() == adw::ColorScheme::Default);

    navigate(app, View::Progress);
    pause(50).await;
    c.check("the status bar leaves with the tiles", !ui.slider.is_mapped());

    // REQ-31h: reached from the sidebar's bottom left, and About stays separate.
    let button = ui.settings_button.compute_bounds(&ui.window);
    c.check("Settings sits at the sidebar's bottom left (REQ-31h)", button.is_some_and(|b| b.x() < 24.0 && b.x() + b.width() <= 220.0 && f64::from(b.y() + b.height()) >= f64::from(ui.window.height()) - 16.0));
    for (action, title) in [("settings", "Settings"), ("about", "About Booklub")] {
        let _ = gtk::prelude::WidgetExt::activate_action(&ui.window, &format!("win.{action}"), None);
        pause(100).await;
        let shown = ui.window.visible_dialog();
        c.check(&format!("{title} opens from its own action"), shown.as_ref().is_some_and(|dialog| dialog.title() == title));
        if let Some(dialog) = shown {
            dialog.close();
        }
        pause(100).await;
    }
    navigate(app, View::AllBooks);

    // Back to how the checks after this one expect to find the library.
    for book in &added {
        app.delete_book(book);
    }
    app.delete_category(&shelf.id);
    navigate(app, View::AllBooks);
    pause(50).await;
    c.check("the display checks left the library as they found it", app.library.borrow().books.len() == kept_books && app.library.borrow().categories.len() == kept_categories);
}

/// QA-01, QA-02, QA-30, QA-31 against the real grid and real tiles.
fn bench(app: &Rc<App>) {
    use std::time::Instant;
    let start = Instant::now();
    navigate(app, View::AllBooks);
    println!("navigate to All Books: {:.1} ms with {} books", start.elapsed().as_secs_f64() * 1000.0, app.library.borrow().books.len());

    let timed = |name: &str, change: &dyn Fn()| {
        let t = Instant::now();
        change();
        let _ = app.visible_books();
        println!("{name}: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    };
    timed("search", &|| app.filters.borrow_mut().search = "strategy".into());
    timed("clear search", &|| app.filters.borrow_mut().search.clear());
    timed("sort by title", &|| app.sort.set(booklub_core::query::SortKey::Title));
    timed("sort by category", &|| app.sort.set(booklub_core::query::SortKey::Category));
    timed("manual order", &|| app.sort.set(booklub_core::query::SortKey::Manual));
    let t = Instant::now();
    crate::window::populate_grid(app);
    println!("populate grid: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    let t = Instant::now();
    app.sort.set(booklub_core::query::SortKey::Category);
    crate::window::populate_grid(app);
    println!("populate with category headings: {:.1} ms, {} headings", t.elapsed().as_secs_f64() * 1000.0, app.ui().canvas.headings().len());
    let t = Instant::now();
    app.ui().canvas.set_scale(1.3);
    println!("change tile size at 2,000 books: {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
    app.ui().canvas.set_scale(1.0);
    app.sort.set(booklub_core::query::SortKey::Manual);
    crate::window::populate_grid(app);

    let scroller = app.ui().grid_scroller.clone();
    let frames: Rc<std::cell::RefCell<Vec<f64>>> = Rc::default();
    let last = Rc::new(std::cell::Cell::new(0i64));
    let began = Instant::now();
    let app_clone = app.clone();
    let scroller_clone = scroller.clone();
    glib::timeout_add_local_once(Duration::from_millis(800), move || {
        let scroller = scroller_clone.clone();
        scroller_clone.add_tick_callback(move |_, clock| {
            let now = clock.frame_time();
            if last.get() != 0 {
                frames.borrow_mut().push((now - last.get()) as f64 / 1000.0);
            }
            last.set(now);
            let adjustment = scroller.vadjustment();
            let max = adjustment.upper() - adjustment.page_size();
            adjustment.set_value((adjustment.value() + 25.0).min(max));
            if adjustment.value() < max {
                return glib::ControlFlow::Continue;
            }
            let mut sorted = frames.borrow().clone();
            sorted.sort_by(|a, b| a.total_cmp(b));
            let count = sorted.len().max(1);
            let over = sorted.iter().filter(|f| **f > 17.5).count();
            println!(
                "scroll: {count} frames over {:.1}s, {:.1} fps, p95 {:.1} ms, worst {:.1} ms, {:.1}% over budget, monitor {:.1} Hz",
                began.elapsed().as_secs_f64(),
                count as f64 / sorted.iter().sum::<f64>() * 1000.0,
                sorted[count * 95 / 100],
                sorted[count - 1],
                over as f64 * 100.0 / count as f64,
                clock.fps(),
            );
            println!("widgets mounted at the end of the scroll: {}", app_clone.ui().canvas.dev_mounted());
            std::process::exit(0);
        });
    });
}
