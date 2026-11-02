//! Getting an image from the Reader, shared by book and Category backgrounds.

use adw::prelude::*;
use booklub_core::background::{self, CheckedImage, ImageError};
use gtk::{gio, glib};

/// REQ-35, UC-30: opens the file chooser and checks the chosen image off the UI thread.
pub fn choose_file(parent: &gtk::Window, on_done: impl Fn(Result<CheckedImage, ImageError>) + 'static) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Images"));
    for mime in background::CLIPBOARD_TYPES {
        filter.add_mime_type(mime);
    }
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder().title("Choose a Background").filters(&filters).build();
    dialog.open(Some(parent), gio::Cancellable::NONE, move |result| {
        let Some(path) = result.ok().and_then(|file| file.path()) else { return };
        glib::spawn_future_local(async move {
            let checked = gio::spawn_blocking(move || background::check_file(&path)).await;
            on_done(checked.unwrap_or(Err(ImageError::Unreadable)));
        });
    });
}

/// REQ-35k, REQ-35o, REQ-35p: asks only for image types, so a copied link is "no image" and
/// never reaches the network (BR-07). Reading stops as soon as the size limit is passed.
pub async fn read_clipboard(widget: &impl IsA<gtk::Widget>) -> Result<CheckedImage, ImageError> {
    let clipboard = widget.as_ref().clipboard();
    let (stream, _) = clipboard
        .read_future(&background::CLIPBOARD_TYPES, glib::Priority::DEFAULT)
        .await
        .map_err(|_| ImageError::NoClipboardImage)?;
    let mut bytes: Vec<u8> = Vec::new();
    loop {
        let chunk = stream.read_bytes_future(256 * 1024, glib::Priority::DEFAULT).await.map_err(|_| ImageError::Unreadable)?;
        if chunk.is_empty() {
            break;
        }
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 > background::MAX_BYTES {
            return Err(ImageError::TooLarge);
        }
    }
    gio::spawn_blocking(move || background::check(bytes)).await.unwrap_or(Err(ImageError::Unreadable))
}
