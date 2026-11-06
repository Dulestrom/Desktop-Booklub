//! Settings, then Devices: the sync switch (REQ-22e), pairing a phone by QR code or typed code
//! (REQ-20, REQ-20f), and the paired phones with their last session (REQ-34) and Remove
//! (REQ-21).

use std::cell::RefCell;
use std::net::IpAddr;
use std::rc::Rc;
use std::time::Instant;

use adw::prelude::*;
use booklub_sync::PairedPeer;
use gtk::{gdk, glib};

use crate::app::App;
use crate::sync;
use crate::util::{clear, hbox, label, text_button, vbox};

pub fn page(app: &Rc<App>) -> gtk::Widget {
    let root = vbox(16);
    root.set_margin_start(24);
    root.set_margin_end(24);
    root.set_margin_bottom(24);
    root.set_margin_top(4);
    let countdown: Rc<RefCell<Option<gtk::Label>>> = Rc::default();

    let render: Rc<dyn Fn()> = {
        let (app, weak, countdown) = (app.clone(), root.downgrade(), countdown.clone());
        Rc::new(move || {
            if let Some(root) = weak.upgrade() {
                build(&app, &root, &countdown);
            }
        })
    };
    render();
    app.sync.listen(&render);

    // The countdown, and the switch to "expired", while the page is open.
    let (weak_root, weak_render) = (root.downgrade(), Rc::downgrade(&render));
    let app_for_tick = app.clone();
    glib::timeout_add_seconds_local(1, move || {
        let (Some(_root), Some(render)) = (weak_root.upgrade(), weak_render.upgrade()) else { return glib::ControlFlow::Break };
        let expires = app_for_tick.sync.offer.borrow().as_ref().map(|offer| offer.expires);
        if let (Some(expires), Some(label)) = (expires, countdown.borrow().as_ref()) {
            label.set_label(&expiry_text(expires));
        }
        if expires.is_some_and(|at| at <= Instant::now()) && countdown.borrow().is_some() {
            render();
        }
        glib::ControlFlow::Continue
    });

    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&root).build();
    // The page owns its renderer, so sync stops calling it once the page is gone.
    let keep = RefCell::new(Some(render));
    scroller.connect_destroy(move |_| drop(keep.take()));
    scroller.upcast()
}

fn build(app: &Rc<App>, root: &gtk::Box, countdown: &Rc<RefCell<Option<gtk::Label>>>) {
    clear(root);
    countdown.replace(None);
    let running = app.sync.is_running();

    // REQ-22e
    let switch_row = hbox(16);
    let text = vbox(4);
    text.set_hexpand(true);
    text.append(&label("Sync with phones", &["body-strong"]));
    let about = label(
        "Keeps Booklub on your phones in step with this computer while Booklub is open on both. Your library travels only across your home network, never the internet.",
        &["caption", "ink-2"],
    );
    about.set_wrap(true);
    text.append(&about);
    switch_row.append(&text);
    let switch = gtk::Switch::builder().active(app.preferences.get().sync_enabled).valign(gtk::Align::Center).build();
    switch.update_property(&[gtk::accessible::Property::Label("Sync with phones")]);
    let app_clone = app.clone();
    switch.connect_state_set(move |_, on| {
        let app = app_clone.clone();
        // After the switch has settled, because turning sync on rebuilds this page.
        glib::idle_add_local_once(move || sync::set_enabled(&app, on));
        glib::Propagation::Proceed
    });
    switch_row.append(&switch);
    root.append(&switch_row);

    let status = match app.sync.problem.borrow().clone() {
        Some(problem) => label(&problem, &["caption", "error"]),
        None if running => label(&ready_text(&app.sync.addresses()), &["caption", "ink-2"]),
        None => label("Sync is off. Phones can't reach this computer.", &["caption", "ink-2"]),
    };
    status.set_wrap(true);
    root.append(&status);

    if running {
        root.append(&pairing(app, countdown));
    }

    let phones = app.store.peers().unwrap_or_default();
    let heading = label("PHONES", &["caption-2", "ink-2"]);
    heading.set_margin_top(8);
    root.append(&heading);
    if phones.is_empty() {
        root.append(&label("No phones are paired yet.", &["caption", "ink-2"]));
    }
    for phone in &phones {
        root.append(&phone_row(app, phone));
    }
}

fn ready_text(addresses: &[IpAddr]) -> String {
    match addresses.first() {
        Some(address) => format!("Ready for phones on {address}."),
        None => "Sync is on, but this computer isn't connected to a home network.".to_string(),
    }
}

/// The typed form of the computer's address (REQ-20f).
fn typed_address(addresses: &[IpAddr], port: u16) -> Option<String> {
    let address = addresses.iter().find(|ip| ip.is_ipv4()).or(addresses.first())?;
    Some(match address {
        IpAddr::V4(v4) => format!("{v4}:{port}"),
        IpAddr::V6(v6) => format!("[{v6}]:{port}"),
    })
}

fn expiry_text(expires: Instant) -> String {
    let left = expires.saturating_duration_since(Instant::now()).as_secs();
    format!("Expires in {}:{:02}", left / 60, left % 60)
}

fn pairing(app: &Rc<App>, countdown: &Rc<RefCell<Option<gtk::Label>>>) -> gtk::Widget {
    let offer = app.sync.offer.borrow().clone();
    let Some(offer) = offer.filter(|offer| offer.expires > Instant::now() && app.sync.pairing_open()) else {
        let row = hbox(12);
        let expired = app.sync.offer.borrow().is_some();
        let button = text_button(if expired { "Show a New Code" } else { "Pair a Phone" }, "suggested-action");
        button.add_css_class("pill");
        let app_clone = app.clone();
        button.connect_clicked(move |_| {
            sync::open_pairing(&app_clone);
        });
        row.append(&button);
        if expired {
            row.append(&label("That code has expired.", &["caption", "ink-2"]));
        }
        return row.upcast();
    };

    let card = hbox(20);
    card.add_css_class("pairing-card");
    if let Some(texture) = qr_texture(&offer.uri) {
        let picture = gtk::Picture::for_paintable(&texture);
        picture.set_can_shrink(false);
        picture.add_css_class("pairing-qr");
        picture.set_valign(gtk::Align::Start);
        picture.update_property(&[gtk::accessible::Property::Label("Pairing QR code")]);
        card.append(&picture);
    }
    let text = vbox(8);
    text.set_hexpand(true);
    text.append(&label("Pair a phone", &["body-strong"]));
    let scan = label("In Booklub on your phone, choose Pair with Computer, then scan this code.", &["caption", "ink-2"]);
    scan.set_wrap(true);
    text.append(&scan);
    if let Some(address) = typed_address(&offer.invitation.addresses, offer.invitation.port) {
        let typed = label("Or type this address and code:", &["caption", "ink-2"]);
        typed.set_margin_top(4);
        text.append(&typed);
        let address_label = label(&address, &["pairing-code"]);
        address_label.set_selectable(true);
        text.append(&address_label);
        let code = label(&offer.code, &["pairing-code"]);
        code.set_selectable(true);
        text.append(&code);
    }
    let expiry = label(&expiry_text(offer.expires), &["caption", "ink-2"]);
    countdown.replace(Some(expiry.clone()));
    text.append(&expiry);
    let cancel = text_button("Cancel", "flat");
    cancel.set_halign(gtk::Align::Start);
    let app_clone = app.clone();
    cancel.connect_clicked(move |_| sync::close_pairing(&app_clone));
    text.append(&cancel);
    card.append(&text);
    card.upcast()
}

fn phone_row(app: &Rc<App>, phone: &PairedPeer) -> gtk::Widget {
    let row = hbox(12);
    row.add_css_class("device-row");
    let text = vbox(2);
    text.set_hexpand(true);
    text.append(&label(&phone.name, &["body-strong"]));
    let seen = app.sync.last_seen.borrow().get(&phone.id).copied().or_else(|| phone.last_session.as_deref().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()).map(|t| t.with_timezone(&chrono::Local)));
    let mut detail = match seen {
        Some(at) => format!("Last synced {}", when(at)),
        None => "Not synced yet".to_string(),
    };
    // REQ-34a
    if sync::pending_for(app, phone) {
        detail.push_str(" \u{b7} Sync pending");
    }
    text.append(&label(&detail, &["caption", "ink-2"]));
    row.append(&text);
    let remove = text_button("Remove", "flat");
    remove.set_valign(gtk::Align::Center);
    remove.update_property(&[gtk::accessible::Property::Label(&format!("Remove {}", phone.name))]);
    let (app_clone, id, name) = (app.clone(), phone.id.clone(), phone.name.clone());
    remove.connect_clicked(move |_| confirm_remove(&app_clone, &id, &name));
    row.append(&remove);
    row.upcast()
}

/// REQ-34: "just now", "5 minutes ago", "today at 14:05", "yesterday at 09:12", or a date.
fn when(at: chrono::DateTime<chrono::Local>) -> String {
    let now = chrono::Local::now();
    let ago = now.signed_duration_since(at);
    if ago < chrono::Duration::minutes(1) {
        "just now".to_string()
    } else if ago < chrono::Duration::hours(1) {
        let minutes = ago.num_minutes();
        if minutes == 1 { "1 minute ago".to_string() } else { format!("{minutes} minutes ago") }
    } else if at.date_naive() == now.date_naive() {
        format!("today at {}", at.format("%H:%M"))
    } else if now.date_naive().pred_opt() == Some(at.date_naive()) {
        format!("yesterday at {}", at.format("%H:%M"))
    } else {
        format!("on {}", at.format("%-d %b %Y"))
    }
}

fn confirm_remove(app: &Rc<App>, id: &str, name: &str) {
    let dialog = adw::AlertDialog::new(
        Some(&format!("Remove \u{201c}{name}\u{201d}?")),
        Some("It will stop syncing with this computer. The books already on the phone stay there."),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("remove", "Remove")]);
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let (app_clone, id) = (app.clone(), id.to_string());
    dialog.connect_response(None, move |_, response| {
        if response == "remove" {
            sync::remove_phone(&app_clone, &id);
        }
    });
    dialog.present(Some(&app.window()));
}

/// Dark modules on white with the standard four-module quiet zone, whatever the theme, so every
/// camera can read it.
fn qr_texture(text: &str) -> Option<gdk::Texture> {
    const QUIET: usize = 4;
    const SCALE: usize = 3;
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M).ok()?;
    let modules = code.width();
    let colors = code.to_colors();
    let side = (modules + 2 * QUIET) * SCALE;
    let mut pixels = vec![255u8; side * side * 4];
    for (index, color) in colors.iter().enumerate() {
        if *color != qrcode::Color::Dark {
            continue;
        }
        let (x, y) = ((index % modules + QUIET) * SCALE, (index / modules + QUIET) * SCALE);
        for row in y..y + SCALE {
            for column in x..x + SCALE {
                let at = (row * side + column) * 4;
                pixels[at..at + 3].fill(0);
            }
        }
    }
    let bytes = glib::Bytes::from_owned(pixels);
    Some(gdk::MemoryTexture::new(side as i32, side as i32, gdk::MemoryFormat::R8g8b8a8, &bytes, side * 4).upcast())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn the_typed_address_prefers_ipv4_and_brackets_ipv6() {
        let v4: IpAddr = "192.168.1.20".parse().unwrap();
        let v6: IpAddr = "fe80::1".parse().unwrap();
        assert_eq!(typed_address(&[v6, v4], 47120).as_deref(), Some("192.168.1.20:47120"));
        assert_eq!(typed_address(&[v6], 47121).as_deref(), Some("[fe80::1]:47121"));
        assert_eq!(typed_address(&[], 47120), None);
    }

    #[test]
    fn the_countdown_reads_minutes_and_seconds() {
        let text = expiry_text(Instant::now() + Duration::from_secs(299));
        assert!(text == "Expires in 4:58" || text == "Expires in 4:59", "{text}");
    }
}
