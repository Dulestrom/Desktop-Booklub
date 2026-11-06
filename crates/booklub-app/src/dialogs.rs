//! Small dialogs: names, delete confirmations, Settings and About.

use std::rc::Rc;

use adw::prelude::*;
use booklub_core::preferences::{Appearance, BooksView};

use crate::app::{App, SubFilter, View};
use crate::util::{books, hbox, label, vbox};
use crate::window::{navigate, refresh};

/// Asks for a Category or Sub-category name.
pub fn ask_name(app: &Rc<App>, heading: &str, field: &str, initial: &str, confirm: &str, on_done: impl Fn(String) + 'static) {
    let dialog = adw::AlertDialog::new(Some(heading), None);
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("ok", confirm);
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");

    let entry = gtk::Entry::builder().text(initial).placeholder_text(field).activates_default(true).build();
    entry.update_property(&[gtk::accessible::Property::Label(field)]);
    dialog.set_extra_child(Some(&entry));
    dialog.set_response_enabled("ok", !initial.trim().is_empty());
    let dialog_clone = dialog.clone();
    entry.connect_changed(move |entry| dialog_clone.set_response_enabled("ok", !entry.text().trim().is_empty()));

    dialog.connect_response(None, move |_, response| {
        if response == "ok" {
            on_done(entry.text().trim().to_string());
        }
    });
    dialog.present(Some(&app.window()));
}

fn confirm(app: &Rc<App>, heading: &str, body: &str, on_confirm: impl Fn() + 'static) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, response| {
        if response == "delete" {
            on_confirm();
        }
    });
    dialog.present(Some(&app.window()));
}

fn clear_text(count: usize, extra: &str) -> String {
    if count == 0 {
        "No books are using it.".to_string()
    } else {
        format!("It will be cleared from {}. {extra}", books(count))
    }
}

/// REQ-05f, REQ-05i
pub fn confirm_delete_category(app: &Rc<App>, id: &str) {
    let (name, count) = {
        let library = app.library.borrow();
        let name = library.categories.iter().find(|c| c.id == id).map(|c| c.name.clone()).unwrap_or_default();
        let count = library.books.iter().filter(|b| b.category_id.as_deref() == Some(id)).count();
        (name, count)
    };
    let body = clear_text(count, "The books themselves are kept.");
    let (app_clone, id) = (app.clone(), id.to_string());
    confirm(app, &format!("Delete \u{201c}{name}\u{201d}?"), &body, move || {
        if app_clone.delete_category(&id) {
            let reader_is_inside = matches!(&*app_clone.view.borrow(), View::Category { id: Some(open), .. } if *open == id);
            if reader_is_inside {
                navigate(&app_clone, View::Home);
            } else {
                refresh(&app_clone);
            }
        }
    });
}

/// REQ-05g, REQ-05i
pub fn confirm_delete_sub(app: &Rc<App>, category_id: &str, sub_id: &str) {
    let (name, count) = {
        let library = app.library.borrow();
        let name = library.categories.iter().flat_map(|c| &c.sub_categories).find(|s| s.id == sub_id).map(|s| s.name.clone()).unwrap_or_default();
        let count = library.books.iter().filter(|b| b.sub_category_id.as_deref() == Some(sub_id)).count();
        (name, count)
    };
    let body = clear_text(count, "Their category is left unchanged.");
    let (app_clone, category, sub) = (app.clone(), category_id.to_string(), sub_id.to_string());
    confirm(app, &format!("Delete \u{201c}{name}\u{201d}?"), &body, move || {
        if app_clone.delete_sub_category(&category, &sub) {
            let mut view = app_clone.view.borrow_mut();
            if let View::Category { sub: selected, .. } = &mut *view {
                if *selected == SubFilter::Sub(sub.clone()) {
                    *selected = SubFilter::All;
                }
            }
            drop(view);
            refresh(&app_clone);
        }
    });
}

/// A labelled row of mutually exclusive buttons, in the Import dialog's segmented style.
fn segmented<T: Copy + PartialEq + 'static>(heading: &str, options: &[(&str, T)], current: T, on_choose: impl Fn(T) + 'static) -> gtk::Box {
    let on_choose = Rc::new(on_choose);
    let row = hbox(12);
    let title = label(heading, &["body-strong"]);
    title.set_hexpand(true);
    row.append(&title);
    let group = hbox(2);
    group.add_css_class("segmented");
    group.update_property(&[gtk::accessible::Property::Label(heading)]);
    let mut first: Option<gtk::ToggleButton> = None;
    for (text, value) in options {
        let button = gtk::ToggleButton::builder().label(*text).active(*value == current).build();
        if let Some(first) = &first {
            button.set_group(Some(first));
        } else {
            first = Some(button.clone());
        }
        let (value, on_choose) = (*value, on_choose.clone());
        button.connect_toggled(move |button| {
            if button.is_active() {
                on_choose(value);
            }
        });
        group.append(&button);
    }
    row.append(&group);
    row
}

/// Which page Settings opens on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsPage {
    Appearance,
    Devices,
}

/// REQ-31h, REQ-47, REQ-48, and Devices (Android SRS REQ-22e).
pub fn settings(app: &Rc<App>) {
    settings_on(app, SettingsPage::Appearance);
}

pub fn settings_on(app: &Rc<App>, opening: SettingsPage) {
    let dialog = adw::Dialog::builder().content_width(760).content_height(540).title("Settings").build();

    let appearance = vbox(10);
    appearance.set_margin_start(24);
    appearance.set_margin_end(24);
    appearance.set_margin_bottom(24);
    appearance.set_margin_top(4);
    let preferences = app.preferences.get();
    let app_clone = app.clone();
    appearance.append(&segmented("Appearance", &[("System", Appearance::System), ("Light", Appearance::Light), ("Dark", Appearance::Dark)], preferences.appearance, move |choice| {
        crate::window::set_appearance(&app_clone, choice);
    }));
    let app_clone = app.clone();
    appearance.append(&segmented("Show books as", &[("Pictures", BooksView::Pictures), ("List", BooksView::List)], preferences.books_view, move |choice| {
        crate::window::set_books_view(&app_clone, choice);
    }));

    let stack = gtk::Stack::builder().hexpand(true).transition_type(gtk::StackTransitionType::None).build();
    stack.add_named(&appearance, Some("appearance"));
    stack.add_named(&crate::devices::page(app), Some("devices"));

    let sidebar = gtk::ListBox::builder().css_classes(["navigation-sidebar"]).width_request(180).build();
    for (name, title) in [("appearance", "Appearance"), ("devices", "Devices")] {
        let row = gtk::ListBoxRow::builder().child(&label(title, &["body"])).name(name).build();
        sidebar.append(&row);
    }
    let stack_clone = stack.clone();
    sidebar.connect_row_selected(move |_, row| {
        if let Some(row) = row {
            stack_clone.set_visible_child_name(&row.widget_name());
        }
    });
    let first = if opening == SettingsPage::Devices { 1 } else { 0 };
    sidebar.select_row(sidebar.row_at_index(first).as_ref());

    let panes = hbox(0);
    panes.append(&sidebar);
    panes.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    panes.append(&stack);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&panes));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(&app.window()));
}

/// REQ-31g.
pub fn about(app: &Rc<App>) {
    let dialog = adw::Dialog::builder().content_width(360).title("About Booklub").build();
    let body = vbox(10);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.set_margin_bottom(24);

    let logo = gtk::Image::from_resource("/io/github/dulestrom/Booklub/logo/app-icon.svg");
    logo.set_pixel_size(96);
    logo.set_margin_bottom(4);
    body.append(&logo);
    let name = label("Booklub", &["title-2"]);
    name.set_xalign(0.5);
    body.append(&name);
    let version = label(&format!("Version {}", crate::VERSION), &["caption-2"]);
    version.set_xalign(0.5);
    body.append(&version);
    let description = label("A private reading tracker. Your library stays on your own devices.", &["body", "ink-2"]);
    description.set_xalign(0.5);
    description.set_wrap(true);
    description.set_justify(gtk::Justification::Center);
    body.append(&description);

    let table = vbox(0);
    table.add_css_class("about-table");
    table.set_margin_top(6);
    for (key, value) in [("Application ID", crate::APP_ID), ("Platform", "Ubuntu 26.04 \u{b7} GTK4 \u{b7} libadwaita"), ("Typeface", "Manrope")] {
        let row = hbox(8);
        row.add_css_class("about-row");
        let key_label = label(key, &["caption"]);
        key_label.set_hexpand(true);
        row.append(&key_label);
        row.append(&label(value, &["caption-2"]));
        table.append(&row);
    }
    body.append(&table);

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::builder().show_title(false).build();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(&app.window()));
}
