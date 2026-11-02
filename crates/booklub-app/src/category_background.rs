//! Choosing a Category's background: built-in artwork, an upload, or a paste (REQ-45).

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use booklub_core::background::{self, CheckedImage, ImageError};
use booklub_core::CategoryBackground;
use gtk::{gdk, gio, glib};

use crate::app::App;
use crate::image_source;
use crate::tiles;
use crate::util::{self, clear, feedback, hbox, icon, label, text_button, vbox};
use crate::window::refresh;

const CHOICE_WIDTH: i32 = 120;
const CHOICE_HEIGHT: i32 = 68;

/// What Save will do (REQ-45e). Nothing touches the library or the disk before then.
#[derive(Clone, Default)]
enum Pending {
    #[default]
    Unchanged,
    Preset(String),
    Image(Rc<CheckedImage>),
    Remove,
}

struct Picker {
    app: Rc<App>,
    dialog: adw::Dialog,
    category_id: String,
    pending: RefCell<Pending>,
    preview: tiles::CategoryTile,
    choices: Vec<(String, gtk::Button, gtk::Image)>,
    feedback: gtk::Box,
    remove: gtk::Button,
    save: gtk::Button,
}

/// REQ-45i: opened from the tile's context menu.
pub fn present(app: &Rc<App>, category_id: &str) {
    if let Some(picker) = build(app, category_id) {
        picker.dialog.present(Some(&app.window()));
    }
}

fn build(app: &Rc<App>, category_id: &str) -> Option<Rc<Picker>> {
    let (name, count, finished) = {
        let library = app.library.borrow();
        let category = library.categories.iter().find(|c| c.id == category_id)?;
        let in_category = library.books.iter().filter(|b| b.category_id.as_deref() == Some(category_id));
        let (mut count, mut finished) = (0, 0);
        for book in in_category {
            count += 1;
            finished += usize::from(book.status == booklub_core::Status::Finished);
        }
        (category.name.clone(), count, finished)
    };

    let dialog = adw::Dialog::builder().content_width(600).title("Category Background").build();
    let header = adw::HeaderBar::new();

    let preview = tiles::category_tile_parts(&name, count, finished, None);
    preview.root.set_size_request(tiles::CATEGORY_WIDTH, tiles::CATEGORY_HEIGHT);
    preview.root.set_halign(gtk::Align::Center);
    preview.root.set_focusable(true);
    preview.root.update_property(&[gtk::accessible::Property::Description("Preview of this category's tile.")]);

    let gallery = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).column_spacing(12).row_spacing(12).min_children_per_line(2).max_children_per_line(4).homogeneous(true).halign(gtk::Align::Center).build();
    let mut choices = Vec::new();
    for (id, title) in background::PRESETS {
        let (root, picture) = tiles::sized_tile(CHOICE_WIDTH, CHOICE_HEIGHT, "art-choice");
        app.thumbs.request_now(&format!("preset:{id}"), &picture);
        let frame = gtk::Box::builder().css_classes(["tile-frame"]).can_target(false).build();
        root.add_overlay(&frame);
        // The check sits in a chip, so selection never relies on the border colour alone.
        let check = icon("check");
        check.add_css_class("art-check");
        check.set_halign(gtk::Align::End);
        check.set_valign(gtk::Align::Start);
        check.set_margin_top(4);
        check.set_margin_end(4);
        check.set_visible(false);
        root.add_overlay(&check);
        let button = gtk::Button::builder().child(&root).css_classes(["art-button"]).tooltip_text(title).build();
        button.update_property(&[gtk::accessible::Property::Label(title)]);
        let child = gtk::FlowBoxChild::builder().child(&button).focusable(false).build();
        gallery.append(&child);
        choices.push((id.to_string(), button, check));
    }

    let upload = text_button("Upload", "secondary");
    let paste = text_button("Paste Image", "secondary");
    let remove = text_button("Remove", "flat-text");
    let actions = hbox(8);
    actions.set_halign(gtk::Align::Center);
    actions.append(&upload);
    actions.append(&paste);
    actions.append(&remove);

    let feedback_host = vbox(6);

    let cancel = text_button("Cancel", "flat-text");
    let save = text_button("Save", "commit");
    let footer = hbox(8);
    footer.add_css_class("dialog-footer");
    footer.set_halign(gtk::Align::End);
    footer.append(&cancel);
    footer.append(&save);

    let body = vbox(14);
    body.add_css_class("dialog-body");
    body.append(&preview.root);
    let built_in = label("BUILT-IN ARTWORK", &["eyebrow"]);
    body.append(&built_in);
    body.append(&gallery);
    body.append(&label("YOUR OWN IMAGE", &["eyebrow"]));
    body.append(&actions);
    body.append(&feedback_host);

    let scroller = gtk::ScrolledWindow::builder().child(&body).hscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true).vexpand(true).build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroller));
    toolbar.add_bottom_bar(&footer);
    dialog.set_child(Some(&toolbar));

    let picker = Rc::new(Picker {
        app: app.clone(),
        dialog: dialog.clone(),
        category_id: category_id.to_string(),
        pending: RefCell::default(),
        preview,
        choices,
        feedback: feedback_host,
        remove: remove.clone(),
        save: save.clone(),
    });

    let this = picker.clone();
    cancel.connect_clicked(move |_| {
        this.dialog.close();
    });
    let this = picker.clone();
    save.connect_clicked(move |_| {
        let this = this.clone();
        glib::spawn_future_local(async move { this.commit().await });
    });
    let this = picker.clone();
    remove.connect_clicked(move |_| this.remove_background());
    let this = picker.clone();
    upload.connect_clicked(move |_| {
        let inner = this.clone();
        image_source::choose_file(&this.app.window(), move |result| inner.accept_image(result));
    });
    let this = picker.clone();
    paste.connect_clicked(move |_| this.paste());
    for (id, button, _) in &picker.choices {
        let (this, id) = (picker.clone(), id.clone());
        button.connect_clicked(move |_| this.choose_preset(&id));
    }

    // This dialog has no text fields, so Ctrl+V can only mean the image (REQ-45b).
    let key = gtk::EventControllerKey::new();
    key.set_propagation_phase(gtk::PropagationPhase::Capture);
    let this = picker.clone();
    key.connect_key_pressed(move |_, key, _, state| {
        if state.contains(gdk::ModifierType::CONTROL_MASK) && (key == gdk::Key::v || key == gdk::Key::V) {
            this.paste();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    dialog.add_controller(key);

    picker.show_saved();
    Some(picker)
}

impl Picker {
    fn saved_background(&self) -> Option<CategoryBackground> {
        let library = self.app.library.borrow();
        library.categories.iter().find(|c| c.id == self.category_id).and_then(|c| c.background.clone())
    }

    fn mark_choice(&self, selected: Option<&str>) {
        for (id, button, check) in &self.choices {
            let on = selected == Some(id.as_str());
            check.set_visible(on);
            if on {
                button.add_css_class("selected");
            } else {
                button.remove_css_class("selected");
            }
        }
    }

    fn show_art_key(&self, key: Option<&str>) {
        self.preview.set_has_art(key.is_some());
        match key {
            Some(key) => self.app.thumbs.request_now(key, &self.preview.picture),
            None => self.app.thumbs.release(&self.preview.picture),
        }
    }

    fn show_saved(&self) {
        let saved = self.saved_background();
        self.mark_choice(match &saved {
            Some(CategoryBackground::Preset(id)) => Some(id),
            _ => None,
        });
        self.remove.set_visible(saved.is_some());
        self.show_art_key(self.app.category_art(&self.category_id).as_deref());
    }

    fn show_feedback(&self, kind: &str, heading: &str, message: &str) {
        self.feedback.append(&feedback(kind, heading, message));
    }

    fn choose_preset(&self, id: &str) {
        clear(&self.feedback);
        *self.pending.borrow_mut() = Pending::Preset(id.to_string());
        self.mark_choice(Some(id));
        self.remove.set_visible(true);
        self.show_art_key(Some(&format!("preset:{id}")));
    }

    fn paste(self: &Rc<Self>) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let result = image_source::read_clipboard(&this.preview.root).await;
            this.accept_image(result);
        });
    }

    /// A rejected image leaves the earlier choice in place (REQ-35b, REQ-35o, REQ-35p).
    fn accept_image(self: &Rc<Self>, result: Result<CheckedImage, ImageError>) {
        clear(&self.feedback);
        let image = match result {
            Ok(image) => image,
            Err(error) => {
                self.show_feedback("error", "ERROR", &error.to_string());
                return;
            }
        };
        // REQ-45h
        if background::is_small_for_category(&image) {
            self.show_feedback("warning", "WARNING", background::MSG_SMALL_IMAGE);
        }
        let image = Rc::new(image);
        *self.pending.borrow_mut() = Pending::Image(image.clone());
        self.mark_choice(None);
        self.remove.set_visible(true);
        self.preview.set_has_art(true);
        // Stops any queued thumbnail from painting over this preview.
        self.app.thumbs.release(&self.preview.picture);
        let bytes = image.bytes.clone();
        let this = self.clone();
        glib::spawn_future_local(async move {
            let decoded = gio::spawn_blocking(move || background::decode_for_display(&bytes, background::WIDE_WIDTH, background::WIDE_WIDTH)).await;
            let still_this_image = matches!(&*this.pending.borrow(), Pending::Image(current) if Rc::ptr_eq(current, &image));
            if let (true, Ok(Ok(decoded))) = (still_this_image, decoded) {
                this.preview.picture.set_paintable(Some(&util::texture_from_rgba(&decoded)));
            }
        });
    }

    /// REQ-45c: the tile falls back as soon as this is saved, so the preview shows that now.
    fn remove_background(&self) {
        clear(&self.feedback);
        let had_saved = self.saved_background().is_some();
        *self.pending.borrow_mut() = if had_saved { Pending::Remove } else { Pending::Unchanged };
        self.mark_choice(None);
        self.remove.set_visible(false);
        self.show_art_key(self.app.fallback_art(&self.category_id).as_deref());
    }

    /// REQ-45e, REQ-35h: new files first, the old one only after the save has succeeded.
    async fn commit(self: &Rc<Self>) {
        let pending = self.pending.borrow().clone();
        let previous = self.saved_background();
        let chosen = match pending {
            Pending::Unchanged => {
                self.dialog.close();
                return;
            }
            Pending::Remove => None,
            Pending::Preset(id) => Some(CategoryBackground::Preset(id)),
            Pending::Image(image) => {
                self.save.set_sensitive(false);
                let (backgrounds, thumbnails) = (self.app.paths.backgrounds.clone(), self.app.paths.thumbnails.clone());
                let image = (*image).clone();
                let stored = gio::spawn_blocking(move || background::store_for_category(&image, &backgrounds, &thumbnails)).await;
                self.save.set_sensitive(true);
                match stored {
                    Ok(Ok(file)) => Some(CategoryBackground::File(file)),
                    _ => {
                        clear(&self.feedback);
                        self.show_feedback("error", "ERROR", &ImageError::Unsaved.to_string());
                        return;
                    }
                }
            }
        };

        if !self.app.set_category_background(&self.category_id, chosen.clone()) {
            // The banner names the failure. A copy nothing refers to should not linger.
            if let Some(file) = chosen.as_ref().and_then(CategoryBackground::file_name) {
                background::remove(file, &self.app.paths.backgrounds, &self.app.paths.thumbnails);
            }
            return;
        }
        if let Some(old) = previous.as_ref().and_then(CategoryBackground::file_name) {
            self.app.thumbs.forget(&format!("wide:{old}"));
            background::remove(old, &self.app.paths.backgrounds, &self.app.paths.thumbnails);
        }
        *self.pending.borrow_mut() = Pending::Unchanged;
        refresh(&self.app);
        self.dialog.close();
    }
}

/// Drives the real picker for the end-to-end run, skipping only the file chooser.
#[cfg(feature = "dev-tools")]
pub mod dev {
    use super::*;

    pub struct Driven(Rc<Picker>);

    pub fn open(app: &Rc<App>, category_id: &str) -> Driven {
        let picker = build(app, category_id).expect("the category exists");
        picker.dialog.present(Some(&app.window()));
        Driven(picker)
    }

    impl Driven {
        pub fn choices(&self) -> usize {
            self.0.choices.len()
        }
        pub fn click_preset(&self, id: &str) {
            let found = self.0.choices.iter().find(|(choice, _, _)| choice == id).expect("a known preset");
            found.1.emit_clicked();
        }
        pub fn preset_marked(&self, id: &str) -> bool {
            self.0.choices.iter().any(|(choice, button, check)| choice == id && button.has_css_class("selected") && check.is_visible())
        }
        pub fn paste(&self) {
            self.0.paste();
        }
        pub fn remove(&self) {
            self.0.remove.emit_clicked();
        }
        pub fn has_pending_image(&self) -> bool {
            matches!(&*self.0.pending.borrow(), Pending::Image(_))
        }
        pub fn feedback_text(&self) -> String {
            fn collect(widget: &gtk::Widget, out: &mut String) {
                if let Some(found) = widget.downcast_ref::<gtk::Label>() {
                    out.push_str(&found.label());
                    out.push(' ');
                }
                let mut child = widget.first_child();
                while let Some(current) = child {
                    collect(&current, out);
                    child = current.next_sibling();
                }
            }
            let mut out = String::new();
            collect(self.0.feedback.upcast_ref(), &mut out);
            out
        }
        pub async fn save(&self) {
            self.0.commit().await;
        }
        pub fn cancel(&self) {
            self.0.dialog.close();
        }
    }
}
