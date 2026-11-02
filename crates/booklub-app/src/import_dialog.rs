//! Import and Export.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use booklub_core::portability::{self, Corrections, ParsedImport};
use booklub_core::spreadsheet::{self, ImportField, ImportPlan, Sheet};
use booklub_core::{archive, background, Library, Status};
use gtk::{gio, glib};

use crate::app::App;
use crate::util::{books, clear, feedback, hbox, icon, label, text_button, vbox};
use crate::window::refresh;

/// REQ-39d: one line per kind of repair made.
fn corrections_text(corrections: &Corrections) -> Vec<String> {
    let mut lines = Vec::new();
    let mut add = |count: usize, one: &str, many: &str| {
        if count == 1 {
            lines.push(format!("1 {one}"));
        } else if count > 1 {
            lines.push(format!("{count} {many}"));
        }
    };
    add(corrections.excluded_rows, "row excluded", "rows excluded");
    add(corrections.cleared_references, "missing category reference cleared", "missing category references cleared");
    add(corrections.replaced_values, "invalid value replaced", "invalid values replaced");
    add(corrections.cleared_backgrounds, "missing background cleared", "missing backgrounds cleared");
    add(corrections.cleared_dates, "date left empty", "dates left empty");
    add(corrections.cleared_isbns, "invalid ISBN cleared", "invalid ISBNs cleared");
    lines
}

fn result_card(heading: &str, corrections: &Corrections) -> gtk::Box {
    let lines = corrections_text(corrections);
    let message = if lines.is_empty() { "Nothing needed correcting.".to_string() } else { lines.join(" \u{b7} ") };
    feedback("success", heading, &message)
}

struct State {
    app: Rc<App>,
    dialog: adw::Dialog,
    file_host: gtk::Box,
    mapping_host: gtk::Box,
    status_host: gtk::Box,
    preview_host: gtk::Box,
    result_host: gtk::Box,
    footer_label: gtk::Label,
    import_button: gtk::Button,
    sheet: RefCell<Option<Sheet>>,
    mapping: RefCell<Vec<ImportField>>,
    status_choices: RefCell<HashMap<String, Status>>,
    plan: RefCell<Option<ImportPlan>>,
    rebuilding: Cell<bool>,
    backup_message: gtk::Box,
}

pub fn present(app: &Rc<App>) {
    let state = build(app);
    state.dialog.present(Some(&app.window()));
}

fn build(app: &Rc<App>) -> Rc<State> {
    let dialog = adw::Dialog::builder().content_width(980).content_height(640).title("Import and Export").build();

    let spreadsheet_toggle = gtk::ToggleButton::builder().active(true).build();
    let spreadsheet_row = hbox(6);
    spreadsheet_row.append(&icon("spreadsheet"));
    spreadsheet_row.append(&gtk::Label::new(Some("Spreadsheet")));
    spreadsheet_toggle.set_child(Some(&spreadsheet_row));
    let backup_toggle = gtk::ToggleButton::builder().group(&spreadsheet_toggle).build();
    let backup_row = hbox(6);
    backup_row.append(&icon("backup"));
    backup_row.append(&gtk::Label::new(Some("Backup and Restore")));
    backup_toggle.set_child(Some(&backup_row));
    let switcher = hbox(0);
    switcher.add_css_class("segmented");
    switcher.append(&spreadsheet_toggle);
    switcher.append(&backup_toggle);

    let header = adw::HeaderBar::builder().title_widget(&switcher).build();
    header.pack_start(&label("Import and Export", &["heading"]));

    // ---------- Spreadsheet ----------
    let left = vbox(10);
    left.set_size_request(360, -1);
    left.append(&label("1 \u{b7} Column mapping", &["heading"]));
    let mapping_note = label("Pre-filled from the column headers. Each field can be claimed once.", &["caption"]);
    mapping_note.set_wrap(true);
    left.append(&mapping_note);
    let mapping_host = vbox(6);
    left.append(&mapping_host);
    left.append(&label("2 \u{b7} Status values", &["heading"]));
    let status_note = label("Values Booklub cannot read are mapped by you. To Read is pre-selected.", &["caption"]);
    status_note.set_wrap(true);
    left.append(&status_note);
    let status_host = vbox(6);
    left.append(&status_host);
    let left_scroller = gtk::ScrolledWindow::builder().child(&left).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
    left_scroller.set_size_request(380, -1);

    let right = vbox(10);
    right.set_hexpand(true);
    let file_host = vbox(0);
    right.append(&file_host);
    right.append(&label("3 \u{b7} Preview \u{b7} first 10 rows", &["heading"]));
    let preview_note = label("Rows marked Skipped have no title and will not be imported.", &["caption"]);
    right.append(&preview_note);
    let preview_host = vbox(0);
    right.append(&preview_host);
    let result_host = vbox(0);
    right.append(&result_host);
    let right_scroller = gtk::ScrolledWindow::builder().child(&right).hscrollbar_policy(gtk::PolicyType::Never).hexpand(true).vexpand(true).build();

    let columns = hbox(20);
    columns.add_css_class("dialog-body");
    columns.append(&left_scroller);
    columns.append(&right_scroller);

    let footer_label = label("Choose a spreadsheet to begin.", &["caption"]);
    footer_label.set_hexpand(true);
    let cancel = text_button("Close", "flat-text");
    let import_button = text_button("Import", "commit");
    import_button.set_sensitive(false);
    let footer = hbox(8);
    footer.add_css_class("dialog-footer");
    footer.append(&footer_label);
    footer.append(&cancel);
    footer.append(&import_button);

    let spreadsheet_page = vbox(0);
    columns.set_vexpand(true);
    spreadsheet_page.append(&columns);
    spreadsheet_page.append(&footer);

    // ---------- Backup and Restore ----------
    let backup_page = vbox(12);
    backup_page.add_css_class("dialog-body");
    backup_page.append(&label("Backup and restore", &["heading"]));
    let backup_note = label("JSON excludes background images; the .booklub archive keeps them.", &["caption"]);
    backup_note.set_wrap(true);
    backup_page.append(&backup_note);
    let actions = hbox(8);
    let export_json = text_button("Export JSON", "secondary");
    let export_archive = text_button("Export .booklub", "secondary");
    let restore = text_button("Restore\u{2026}", "secondary");
    actions.append(&export_json);
    actions.append(&export_archive);
    actions.append(&restore);
    backup_page.append(&actions);
    let backup_message = vbox(10);
    backup_page.append(&backup_message);

    let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::None).vexpand(true).build();
    stack.add_named(&spreadsheet_page, Some("spreadsheet"));
    stack.add_named(&backup_page, Some("backup"));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));
    dialog.set_child(Some(&toolbar));

    let state = Rc::new(State {
        app: app.clone(),
        dialog: dialog.clone(),
        file_host,
        mapping_host,
        status_host,
        preview_host,
        result_host,
        footer_label,
        import_button: import_button.clone(),
        sheet: RefCell::default(),
        mapping: RefCell::default(),
        status_choices: RefCell::default(),
        plan: RefCell::default(),
        rebuilding: Cell::new(false),
        backup_message,
    });

    let stack_clone = stack.clone();
    spreadsheet_toggle.connect_toggled(move |toggle| {
        stack_clone.set_visible_child_name(if toggle.is_active() { "spreadsheet" } else { "backup" });
    });
    let dialog_clone = dialog.clone();
    cancel.connect_clicked(move |_| {
        dialog_clone.close();
    });
    let inner = state.clone();
    import_button.connect_clicked(move |_| inner.run_import());
    let inner = state.clone();
    export_json.connect_clicked(move |_| inner.export_json());
    let inner = state.clone();
    export_archive.connect_clicked(move |_| inner.export_archive());
    let inner = state.clone();
    restore.connect_clicked(move |_| inner.choose_restore());

    state.show_file_chooser_card(None);
    state
}

/// Drives the real wizard and restore code for the end-to-end run, skipping only the file chooser.
#[cfg(feature = "dev-tools")]
pub mod dev {
    use super::*;

    pub struct Driven(Rc<State>);

    pub fn open(app: &Rc<App>) -> Driven {
        let state = build(app);
        state.dialog.present(Some(&app.window()));
        Driven(state)
    }

    impl Driven {
        pub fn load_sheet(&self, path: &std::path::Path) -> Result<(), String> {
            let sheet = spreadsheet::read_file(path).map_err(|e| e.to_string())?;
            self.0.load_sheet(sheet, "test.xlsx");
            Ok(())
        }
        pub fn mapping(&self) -> Vec<ImportField> {
            self.0.mapping.borrow().clone()
        }
        pub fn unknown_statuses(&self) -> Vec<String> {
            let sheet = self.0.sheet.borrow().clone();
            sheet.map_or_else(Vec::new, |sheet| spreadsheet::unknown_statuses(&sheet, &self.0.mapping.borrow()))
        }
        pub fn choose_status(&self, value: &str, status: Status) {
            self.0.status_choices.borrow_mut().insert(value.to_string(), status);
            self.0.rebuild_plan_only();
        }
        pub fn planned(&self) -> usize {
            self.0.plan.borrow().as_ref().map_or(0, |plan| plan.books.len())
        }
        pub fn can_import(&self) -> bool {
            self.0.import_button.is_sensitive()
        }
        pub fn import_label(&self) -> String {
            self.0.import_button.label().map(|l| l.to_string()).unwrap_or_default()
        }
        pub fn run_import(&self) {
            self.0.run_import();
        }
        pub fn restore_merge(&self, path: &std::path::Path) -> Result<(), String> {
            let restore = read_restore(path, &self.0.app.paths.backgrounds)?;
            self.0.show_restore(Rc::new(Restore { import: restore.import.clone(), images: restore.images.clone() }));
            self.0.apply_merge(&restore);
            Ok(())
        }
        pub fn restore_replace(&self, path: &std::path::Path) -> Result<(), String> {
            let restore = read_restore(path, &self.0.app.paths.backgrounds)?;
            self.0.apply_replace(&restore);
            Ok(())
        }
        pub fn close(&self) {
            self.0.dialog.close();
        }
    }
}

fn file_filter(name: &str, pattern: &str) -> gio::ListStore {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(name));
    filter.add_pattern(pattern);
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    filters
}

impl State {
    fn window(&self) -> gtk::Window {
        self.app.window()
    }

    fn show_file_chooser_card(self: &Rc<Self>, summary: Option<(String, String)>) {
        clear(&self.file_host);
        let card = hbox(10);
        card.add_css_class("file-card");
        card.append(&icon("spreadsheet"));
        let text = vbox(2);
        text.set_hexpand(true);
        match &summary {
            Some((name, meta)) => {
                let title = label(name, &["body-strong"]);
                title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                text.append(&title);
                text.append(&label(meta, &["caption"]));
            }
            None => {
                text.append(&label("No file chosen", &["body-strong"]));
                text.append(&label("Any column layout works. You map the columns next.", &["caption"]));
            }
        }
        card.append(&text);
        let choose = text_button(if summary.is_some() { "Change" } else { "Choose .xlsx" }, "secondary");
        let this = self.clone();
        choose.connect_clicked(move |_| this.choose_sheet());
        card.append(&choose);
        self.file_host.append(&card);
    }

    fn choose_sheet(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder().title("Choose a Spreadsheet").filters(&file_filter("Excel workbook", "*.xlsx")).build();
        let this = self.clone();
        dialog.open(Some(&self.window()), gio::Cancellable::NONE, move |result| {
            let Some(path) = result.ok().and_then(|file| file.path()) else { return };
            let this = this.clone();
            glib::spawn_future_local(async move {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let read = gio::spawn_blocking(move || spreadsheet::read_file(&path)).await;
                match read {
                    Ok(Ok(sheet)) => this.load_sheet(sheet, &name),
                    Ok(Err(error)) => this.show_sheet_error(&error.to_string()),
                    Err(_) => this.show_sheet_error("That file could not be read as an .xlsx spreadsheet."),
                }
            });
        });
    }

    fn show_sheet_error(self: &Rc<Self>, message: &str) {
        clear(&self.result_host);
        self.result_host.append(&feedback("error", "ERROR", message));
    }

    /// REQ-18, REQ-18a, REQ-18a-i
    fn load_sheet(self: &Rc<Self>, sheet: Sheet, name: &str) {
        clear(&self.result_host);
        let meta = format!("First sheet \u{b7} {} rows \u{b7} {} columns", sheet.rows.len(), sheet.columns.len());
        self.show_file_chooser_card(Some((name.to_string(), meta)));
        *self.mapping.borrow_mut() = spreadsheet::guess_mapping(&sheet.columns);
        self.status_choices.borrow_mut().clear();
        *self.sheet.borrow_mut() = Some(sheet);
        self.rebuild_mapping();
        self.rebuild_plan();
    }

    fn rebuild_mapping(self: &Rc<Self>) {
        clear(&self.mapping_host);
        let Some(sheet) = self.sheet.borrow().clone() else { return };
        let mapping = self.mapping.borrow().clone();
        let names: Vec<&str> = ImportField::ALL.iter().map(|field| field.label()).collect();

        self.rebuilding.set(true);
        for (column, heading) in sheet.columns.iter().enumerate() {
            let row = hbox(8);
            let name = label(heading, &["body"]);
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            name.set_width_chars(14);
            name.set_max_width_chars(14);
            row.append(&name);
            row.append(&gtk::Label::new(Some("\u{2192}")));
            let dropdown = gtk::DropDown::builder().model(&gtk::StringList::new(&names)).hexpand(true).build();
            dropdown.update_property(&[gtk::accessible::Property::Label(&format!("Map column {heading}"))]);
            let selected = ImportField::ALL.iter().position(|field| *field == mapping[column]).unwrap_or(0);
            dropdown.set_selected(selected as u32);
            let this = self.clone();
            dropdown.connect_selected_notify(move |dropdown| {
                if this.rebuilding.get() {
                    return;
                }
                let chosen = ImportField::ALL[dropdown.selected() as usize];
                {
                    let mut mapping = this.mapping.borrow_mut();
                    // A field can be claimed only once, so the earlier claim gives way.
                    if chosen != ImportField::Ignore {
                        for existing in mapping.iter_mut() {
                            if *existing == chosen {
                                *existing = ImportField::Ignore;
                            }
                        }
                    }
                    mapping[column] = chosen;
                }
                let this = this.clone();
                // Rebuilding destroys the dropdown whose signal is running, so wait for idle.
                glib::idle_add_local_once(move || {
                    this.rebuild_mapping();
                    this.rebuild_plan();
                });
            });
            row.append(&dropdown);
            if mapping[column] == ImportField::Title {
                row.append(&label("Required", &["caption", "accent-ink"]));
            }
            self.mapping_host.append(&row);
        }
        self.rebuilding.set(false);
    }

    /// REQ-40a
    fn rebuild_statuses(self: &Rc<Self>) {
        clear(&self.status_host);
        let Some(sheet) = self.sheet.borrow().clone() else { return };
        let mapping = self.mapping.borrow().clone();
        let unknown = spreadsheet::unknown_statuses(&sheet, &mapping);
        if unknown.is_empty() {
            let row = hbox(8);
            let good = icon("success");
            good.add_css_class("success-ink");
            row.append(&good);
            let message = if mapping.contains(&ImportField::Status) { "Every status value was recognised." } else { "No column is mapped to Status." };
            row.append(&label(message, &["caption"]));
            self.status_host.append(&row);
            return;
        }
        let names: Vec<&str> = Status::ALL.iter().map(|status| status.as_str()).collect();
        for value in unknown {
            let row = hbox(8);
            let name = label(&format!("\u{201c}{value}\u{201d}"), &["body"]);
            name.set_width_chars(14);
            name.set_max_width_chars(14);
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            row.append(&name);
            row.append(&gtk::Label::new(Some("\u{2192}")));
            let dropdown = gtk::DropDown::builder().model(&gtk::StringList::new(&names)).hexpand(true).build();
            dropdown.update_property(&[gtk::accessible::Property::Label(&format!("Status for {value}"))]);
            let current = self.status_choices.borrow().get(&value).copied().unwrap_or(Status::ToRead);
            dropdown.set_selected(Status::ALL.iter().position(|s| *s == current).unwrap_or(0) as u32);
            let (this, key) = (self.clone(), value.clone());
            dropdown.connect_selected_notify(move |dropdown| {
                this.status_choices.borrow_mut().insert(key.clone(), Status::ALL[dropdown.selected() as usize]);
                this.rebuild_plan_only();
            });
            row.append(&dropdown);
            self.status_host.append(&row);
        }
    }

    fn rebuild_plan(self: &Rc<Self>) {
        self.rebuild_statuses();
        self.rebuild_plan_only();
    }

    /// REQ-18a-ii, REQ-18b, REQ-18f
    fn rebuild_plan_only(self: &Rc<Self>) {
        let Some(sheet) = self.sheet.borrow().clone() else { return };
        let mapping = self.mapping.borrow().clone();
        let title_mapped = mapping.contains(&ImportField::Title);
        let (categories, start) = {
            let library = self.app.library.borrow();
            (library.categories.clone(), booklub_core::query::next_position(&library.books))
        };
        let plan = spreadsheet::build_plan(&sheet, &mapping, &self.status_choices.borrow(), &categories, start);

        clear(&self.preview_host);
        if title_mapped {
            self.preview_host.append(&preview_table(&plan));
        } else {
            self.preview_host.append(&feedback("warning", "WARNING", "Map one column to Title before importing. A book without a title cannot be stored."));
        }

        let count = plan.books.len();
        self.import_button.set_sensitive(title_mapped && count > 0);
        self.import_button.set_label(&format!("Import {}", title_case_books(count)));
        self.footer_label.set_label(&if title_mapped {
            format!("{} will be appended to the end of your manual order.", books(count))
        } else {
            "Map a column to Title to continue.".to_string()
        });
        *self.plan.borrow_mut() = Some(plan);
    }

    /// REQ-18e, REQ-39c, REQ-39d
    fn run_import(self: &Rc<Self>) {
        let Some(plan) = self.plan.borrow().clone() else { return };
        if !self.app.report(self.app.store.save_import(&plan.books, &plan.categories)) {
            return;
        }
        self.app.reload();
        refresh(&self.app);
        clear(&self.result_host);
        self.result_host.append(&result_card(
            &format!("LAST IMPORT: {} IMPORTED, {} EXCLUDED", plan.books.len(), plan.corrections.excluded_rows),
            &plan.corrections,
        ));
        // Importing the same plan twice would duplicate every row.
        self.sheet.borrow_mut().take();
        self.plan.borrow_mut().take();
        self.import_button.set_sensitive(false);
        clear(&self.preview_host);
        clear(&self.mapping_host);
        clear(&self.status_host);
        self.show_file_chooser_card(None);
        self.footer_label.set_label("Import finished.");
    }

    fn backup_feedback(self: &Rc<Self>, widget: &impl IsA<gtk::Widget>) {
        clear(&self.backup_message);
        self.backup_message.append(widget);
    }

    /// REQ-17, REQ-37c
    fn export_json(self: &Rc<Self>) {
        let library = self.app.library.borrow().clone();
        let with_images = library.books.iter().filter(|book| book.background_file.is_some()).count();
        let dialog = gtk::FileDialog::builder().title("Export JSON").initial_name(portability::default_file_name("json")).build();
        let this = self.clone();
        dialog.save(Some(&self.window()), gio::Cancellable::NONE, move |result| {
            let Some(path) = result.ok().and_then(|file| file.path()) else { return };
            match std::fs::write(&path, portability::export_json(&library)) {
                Ok(()) if with_images > 0 => this.backup_feedback(&feedback(
                    "warning",
                    "EXPORTED WITHOUT IMAGES",
                    &format!("{} exported. Background images on {} are excluded from JSON. Use .booklub to keep them.", books(library.books.len()), books(with_images)),
                )),
                Ok(()) => this.backup_feedback(&feedback("success", "EXPORTED", &format!("{} exported.", books(library.books.len())))),
                Err(_) => this.backup_feedback(&feedback("error", "ERROR", "That file could not be written.")),
            }
        });
    }

    /// REQ-37
    fn export_archive(self: &Rc<Self>) {
        let library = self.app.library.borrow().clone();
        let backgrounds = self.app.paths.backgrounds.clone();
        let dialog = gtk::FileDialog::builder().title("Export Archive").initial_name(portability::default_file_name("booklub")).build();
        let this = self.clone();
        dialog.save(Some(&self.window()), gio::Cancellable::NONE, move |result| {
            let Some(path) = result.ok().and_then(|file| file.path()) else { return };
            let (library, backgrounds, this) = (library.clone(), backgrounds.clone(), this.clone());
            glib::spawn_future_local(async move {
                let count = library.books.len();
                let written = gio::spawn_blocking(move || archive::export(&library, &backgrounds, &path)).await;
                match written {
                    Ok(Ok(())) => this.backup_feedback(&feedback("success", "EXPORTED", &format!("{} and their backgrounds exported.", books(count)))),
                    _ => this.backup_feedback(&feedback("error", "ERROR", "The archive could not be written.")),
                }
            });
        });
    }

    /// REQ-19, REQ-37a, REQ-38
    fn choose_restore(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Booklub exports"));
        filter.add_pattern("*.json");
        filter.add_pattern("*.booklub");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder().title("Restore").filters(&filters).build();
        let this = self.clone();
        dialog.open(Some(&self.window()), gio::Cancellable::NONE, move |result| {
            let Some(path) = result.ok().and_then(|file| file.path()) else { return };
            let this = this.clone();
            let backgrounds = this.app.paths.backgrounds.clone();
            glib::spawn_future_local(async move {
                let loaded = gio::spawn_blocking(move || read_restore(&path, &backgrounds)).await;
                match loaded {
                    Ok(Ok(restore)) => this.show_restore(Rc::new(restore)),
                    Ok(Err(message)) => this.backup_feedback(&feedback("error", "ERROR", &message)),
                    Err(_) => this.backup_feedback(&feedback("error", "ERROR", "That file could not be read.")),
                }
            });
        });
    }

    fn show_restore(self: &Rc<Self>, restore: Rc<Restore>) {
        let card = vbox(10);
        let library = &restore.import.library;
        let date = restore.import.exported_at.get(..10).map_or(String::new(), |date| format!(" \u{b7} exported {date}"));
        card.append(&feedback(
            "info",
            "READY TO IMPORT",
            &format!("{} and {} categories{date}. Merge keeps your current books and adds any it does not hold. Replace discards them.", books(library.books.len()), library.categories.len()),
        ));
        let lines = corrections_text(&restore.import.corrections);
        if !lines.is_empty() {
            card.append(&feedback("warning", "CORRECTIONS", &lines.join(" \u{b7} ")));
        }
        let actions = hbox(8);
        let merge = text_button("Merge into Library", "secondary");
        let replace = text_button("Replace Library", "destructive");
        actions.append(&merge);
        actions.append(&replace);
        card.append(&actions);

        let (this, chosen) = (self.clone(), restore.clone());
        merge.connect_clicked(move |_| this.apply_merge(&chosen));
        let (this, chosen) = (self.clone(), restore);
        replace.connect_clicked(move |_| this.confirm_replace(&chosen));
        self.backup_feedback(&card);
    }

    fn write_images(&self, restore: &Restore, wanted: &[String]) -> usize {
        let mut cleared = 0;
        for name in wanted {
            let Some(bytes) = restore.images.get(name) else { continue };
            let valid = background::check(bytes.clone()).is_ok();
            if !valid || std::fs::write(self.app.paths.backgrounds.join(name), bytes).is_err() {
                cleared += 1;
            }
        }
        cleared
    }

    /// REQ-19d, REQ-19e, REQ-39b
    fn apply_merge(self: &Rc<Self>, restore: &Restore) {
        let current = self.app.library.borrow().clone();
        let mut plan = portability::plan_merge(&current, &restore.import);
        let mut wanted: Vec<String> = plan.books.iter().filter_map(|book| book.background_file.clone()).collect();
        wanted.extend(plan.categories.iter().filter_map(|(category, _)| category.background.as_ref()?.file_name().map(str::to_string)));
        let failed = self.write_images(restore, &wanted);
        plan.corrections.cleared_backgrounds += failed;
        if !self.app.report(self.app.store.save_import(&plan.books, &plan.categories)) {
            return;
        }
        self.finish_restore(&format!("MERGED: {} ADDED", plan.books.len()), &plan.corrections);
    }

    /// REQ-19c
    fn confirm_replace(self: &Rc<Self>, restore: &Rc<Restore>) {
        let (count, categories) = {
            let library = self.app.library.borrow();
            (library.books.len(), library.categories.len())
        };
        let dialog = adw::AlertDialog::new(
            Some("Replace your library?"),
            Some(&format!("This discards all {}, {categories} categories, and your goals, replacing them with the contents of the file. This cannot be undone.", books(count))),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("replace", "Discard and Replace");
        dialog.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let (this, restore) = (self.clone(), restore.clone());
        dialog.connect_response(None, move |_, response| {
            if response == "replace" {
                this.apply_replace(&restore);
            }
        });
        dialog.present(Some(&self.dialog));
    }

    /// REQ-19f
    fn apply_replace(self: &Rc<Self>, restore: &Restore) {
        let library: Library = restore.import.library.clone();
        let wanted = library.background_files();
        let mut corrections = restore.import.corrections;
        corrections.cleared_backgrounds += self.write_images(restore, &wanted);
        if !self.app.report(self.app.store.replace_all(&library)) {
            return;
        }
        // QA-33 would catch these at the next launch, but the space is the Reader's now.
        if let Ok(referenced) = self.app.store.referenced_backgrounds() {
            background::remove_unreferenced(&referenced, &self.app.paths.backgrounds, &self.app.paths.thumbnails);
        }
        self.finish_restore(&format!("REPLACED: {} RESTORED", library.books.len()), &corrections);
    }

    fn finish_restore(self: &Rc<Self>, heading: &str, corrections: &Corrections) {
        self.app.reload();
        *self.app.view.borrow_mut() = crate::app::View::Home;
        refresh(&self.app);
        self.backup_feedback(&result_card(heading, corrections));
    }
}

fn title_case_books(count: usize) -> String {
    if count == 1 { "1 Book".to_string() } else { format!("{count} Books") }
}

fn preview_table(plan: &ImportPlan) -> gtk::Box {
    let table = vbox(0);
    table.add_css_class("preview-table");
    let make_row = |class: &str, cells: [&str; 4], text_class: &[&str]| {
        let row = gtk::Grid::builder().column_spacing(10).column_homogeneous(true).css_classes([class]).build();
        for (index, text) in cells.iter().enumerate() {
            let cell = label(text, text_class);
            cell.set_ellipsize(gtk::pango::EllipsizeMode::End);
            cell.set_width_chars(8);
            cell.set_max_width_chars(8);
            cell.set_hexpand(true);
            row.attach(&cell, index as i32, 0, 1, 1);
        }
        row
    };
    table.append(&make_row("preview-head", ["Title", "Author", "Category", "Sub-category"], &["eyebrow"]));
    for row in &plan.preview {
        if row.skipped {
            let skipped = hbox(6);
            skipped.add_css_class("preview-row");
            let warning = icon("warning");
            warning.add_css_class("warning-ink");
            skipped.append(&warning);
            skipped.append(&label("Skipped \u{b7} no title", &["caption", "warning-ink"]));
            table.append(&skipped);
        } else {
            table.append(&make_row("preview-row", [&row.title, &row.author, &row.category, &row.sub_category], &["caption-2"]));
        }
    }
    table
}

#[derive(Clone)]
pub struct Restore {
    pub import: ParsedImport,
    pub images: HashMap<String, Vec<u8>>,
}

fn read_restore(path: &std::path::Path, backgrounds: &std::path::Path) -> Result<Restore, String> {
    if path.extension().is_some_and(|extension| extension == "booklub") {
        let restored = archive::read(path).map_err(|error| error.to_string())?;
        return Ok(Restore { import: restored.import, images: restored.images });
    }
    let text = std::fs::read_to_string(path).map_err(|_| "That file could not be read.".to_string())?;
    let mut import = portability::parse_import(&text).map_err(|error| error.to_string())?;
    archive::clear_absent_backgrounds(&mut import, backgrounds);
    Ok(Restore { import, images: HashMap::new() })
}
