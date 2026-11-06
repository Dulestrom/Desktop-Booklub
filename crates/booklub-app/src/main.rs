//! Booklub for Ubuntu: a private reading tracker.

mod app;
mod canvas;
mod category_background;
mod devices;
mod dialogs;
mod image_source;
mod import_dialog;
mod inspector;
mod progress;
mod sync;
mod thumbs;
mod tiles;
mod util;
mod window;

#[cfg(feature = "dev-tools")]
mod devtools;

use adw::prelude::*;
use gtk::{gio, glib};

pub const APP_ID: &str = "io.github.dulestrom.Booklub";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> glib::ExitCode {
    #[cfg(feature = "dev-tools")]
    devtools::mark_launch();
    gio::resources_register_include!("booklub.gresource").expect("the resource bundle is built in");

    // The default flags give REQ-30c: a second launch activates the running instance,
    // which raises its window.
    let application = adw::Application::builder().application_id(APP_ID).build();
    // A test run must never hand off to, or raise, a copy the Reader has open.
    #[cfg(feature = "dev-tools")]
    application.set_flags(gio::ApplicationFlags::NON_UNIQUE);
    application.connect_activate(|application| {
        if let Some(window) = application.active_window() {
            window.present();
            return;
        }
        window::open(application);
    });
    application.run()
}
