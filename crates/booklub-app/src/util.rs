//! Small widget helpers shared by the views.

use adw::prelude::*;
use gtk::{gdk, glib};

/// A label with its style classes. Reader text always goes through here as plain text,
/// because Pango markup stays off unless asked for (QA-16).
pub fn label(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder().label(text).xalign(0.0).css_classes(classes.iter().copied().collect::<Vec<_>>()).build()
}

pub fn icon(name: &str) -> gtk::Image {
    gtk::Image::builder().icon_name(format!("booklub-{name}-symbolic")).pixel_size(16).build()
}

pub fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

pub fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}

/// REQ-43e: an icon-only control carries a tooltip and an accessible name.
pub fn tool_button(icon_name: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::builder().child(&icon(icon_name)).tooltip_text(tooltip).css_classes(["tool"]).build();
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    button
}

pub fn text_button(text: &str, class: &str) -> gtk::Button {
    gtk::Button::builder().label(text).css_classes([class]).build()
}

pub fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

/// REQ-16f style counts: "1 book", "12 books".
pub fn books(count: usize) -> String {
    if count == 1 { "1 book".to_string() } else { format!("{count} books") }
}

/// An RGBA texture from decoded pixels. Safe to build on a worker thread.
pub fn texture_from_rgba(image: &booklub_core::background::DecodedRgba) -> gdk::Texture {
    let bytes = glib::Bytes::from(image.pixels.as_slice());
    gdk::MemoryTexture::new(image.width as i32, image.height as i32, gdk::MemoryFormat::R8g8b8a8, &bytes, image.width as usize * 4).upcast()
}

/// Runs `action` on a single click, or on Enter or Space while the widget has focus.
pub fn on_activate(widget: &impl IsA<gtk::Widget>, action: impl Fn() + 'static) {
    let action = std::rc::Rc::new(action);
    // Not a release in the same press as a drag: DragSource cancels this gesture once a drag starts.
    let click = gtk::GestureClick::builder().button(gdk::BUTTON_PRIMARY).build();
    let for_click = action.clone();
    click.connect_released(move |gesture, presses, _, _| {
        if presses == 1 {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            for_click();
        }
    });
    widget.add_controller(click);

    let key = gtk::EventControllerKey::new();
    key.connect_key_pressed(move |_, key, _, _| {
        if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::space) {
            action();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    widget.add_controller(key);
}

/// A determinate bar. A real progress bar sizes its own fill, with no frame-late width updates.
pub fn progress_bar(percent: u32, tone: &str) -> gtk::ProgressBar {
    let bar = gtk::ProgressBar::builder().fraction(f64::from(percent.min(100)) / 100.0).hexpand(true).valign(gtk::Align::Center).css_classes(["bk-progress"]).build();
    if !tone.is_empty() {
        bar.add_css_class(tone);
    }
    bar
}

/// A feedback message: a glyph and a label, so kinds differ by more than colour.
pub fn feedback(kind: &str, heading: &str, message: &str) -> gtk::Box {
    let glyph = match kind {
        "error" => "error",
        "warning" => "warning",
        "success" => "success",
        _ => "background-add",
    };
    let row = hbox(10);
    row.add_css_class("feedback");
    row.add_css_class(kind);
    let image = icon(glyph);
    image.set_valign(gtk::Align::Start);
    row.append(&image);
    let text = vbox(2);
    text.set_hexpand(true);
    text.append(&label(heading, &["chip-text"]));
    let body = label(message, &["caption"]);
    body.add_css_class("ink-1");
    body.set_wrap(true);
    body.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    text.append(&body);
    row.append(&text);
    row
}

#[cfg(feature = "dev-tools")]
pub fn find_header(widget: &gtk::Widget) -> Option<adw::HeaderBar> {
    if let Some(found) = widget.downcast_ref::<adw::HeaderBar>() {
        return Some(found.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_header(&current) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

#[cfg(feature = "dev-tools")]
pub fn dev_header_bounds(widget: &gtk::Widget, window: &gtk::Widget) {
    if widget.is::<adw::HeaderBar>() && widget.is_mapped() {
        if let Some(bounds) = widget.compute_bounds(window) {
            println!("HEADER x={} y={} w={} h={}", bounds.x(), bounds.y(), bounds.width(), bounds.height());
        }
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        dev_header_bounds(&current, window);
        child = current.next_sibling();
    }
}
