//! One sync session, after Hello and any pairing. Strictly one side talks at a time, so neither
//! can block the other on a full network buffer:
//!
//! 1. phone: `Sync` with its vector
//! 2. computer: `Changes` the phone lacks
//! 3. phone applies them, then sends `Changes` the computer lacks and `Wants` (its missing images)
//! 4. computer applies them, sends the wanted images and `ImagesDone`, then its own `Wants`
//! 5. phone stores the images, sends the wanted ones and `ImagesDone`, then `Done` with its vector
//! 6. computer stores the images, records the phone's vector as acknowledged, and answers `Done`
//!
//! Each `apply` is one transaction (REQ-30c), and each side's vector moves only with it, so a
//! session cut off anywhere resumes from what was committed (REQ-30d, QA-21).

use std::collections::{BTreeSet, HashSet};
use std::io::{Read, Write};

use booklub_core::sync::{Delta, Vector, BOOK};

use crate::frame::{receive, receive_bytes, send, send_bytes, unexpected, Message};
use crate::key::hex;
use crate::{Event, Host, Summary, SyncError};

fn storage(error: String) -> SyncError {
    SyncError::Storage(error)
}

fn books_in(delta: &Delta) -> usize {
    delta.units.iter().filter(|unit| unit.entity == BOOK).map(|unit| unit.id.as_str()).collect::<BTreeSet<_>>().len()
}

fn apply(host: &dyn Host, delta: &Delta, summary: &mut Summary) -> Result<(), SyncError> {
    let report = host.apply(delta).map_err(storage)?;
    summary.units_received += report.units_applied;
    summary.deleted += report.deleted;
    let books = books_in(delta);
    if books > 0 {
        host.notify(Event::Books { received: books, total: books });
    }
    Ok(())
}

fn send_changes(stream: &mut impl Write, host: &dyn Host, theirs: &Vector, summary: &mut Summary) -> Result<(), SyncError> {
    let delta = host.delta_since(theirs).map_err(storage)?;
    summary.units_sent += delta.units.len() + delta.tombstones.len();
    send(stream, &Message::Changes { delta })
}

fn receive_changes(stream: &mut impl Read) -> Result<Delta, SyncError> {
    match receive(stream)? {
        Message::Changes { delta } => Ok(delta),
        other => Err(unexpected(&other)),
    }
}

fn receive_wants(stream: &mut impl Read) -> Result<Vec<String>, SyncError> {
    match receive(stream)? {
        Message::Wants { images } => Ok(images),
        other => Err(unexpected(&other)),
    }
}

/// A background reference is only ever a bare file name; anything else is refused, so a peer
/// can't name a path outside the image folder.
pub(crate) fn is_safe_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 255 && !name.starts_with('.') && name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

fn sha256(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

fn send_images(stream: &mut impl Write, host: &dyn Host, wanted: &[String], summary: &mut Summary) -> Result<(), SyncError> {
    for name in wanted.iter().filter(|name| is_safe_name(name)) {
        if let Some(bytes) = host.read_image(name) {
            send(stream, &Message::Image { name: name.clone(), sha256: sha256(&bytes) })?;
            send_bytes(stream, &bytes)?;
            summary.images_sent += 1;
        }
    }
    send(stream, &Message::ImagesDone)
}

/// Stores only images this side asked for and whose fingerprint matches (REQ-33b). Anything
/// that fails is simply missing again next session (REQ-33c).
fn receive_images(stream: &mut (impl Read + Write), host: &dyn Host, wanted: &[String], summary: &mut Summary) -> Result<(), SyncError> {
    let wanted: HashSet<&str> = wanted.iter().map(String::as_str).collect();
    let total = wanted.len();
    loop {
        match receive(stream)? {
            Message::Image { name, sha256: expected } => {
                let bytes = receive_bytes(stream)?;
                if wanted.contains(name.as_str()) && is_safe_name(&name) && sha256(&bytes) == expected {
                    host.store_image(&name, &bytes).map_err(storage)?;
                    summary.images_received += 1;
                    host.notify(Event::Images { received: summary.images_received, total });
                }
            }
            Message::ImagesDone => return Ok(()),
            other => return Err(unexpected(&other)),
        }
    }
}

/// The phone's side, from step 1.
pub(crate) fn as_phone(stream: &mut (impl Read + Write), host: &dyn Host, computer: &str) -> Result<Summary, SyncError> {
    let mut summary = Summary::default();
    send(stream, &Message::Sync { vector: host.vector().map_err(storage)? })?;
    let incoming = receive_changes(stream)?;
    apply(host, &incoming, &mut summary)?;
    send_changes(stream, host, &incoming.vector, &mut summary)?;
    let wants = host.missing_images();
    send(stream, &Message::Wants { images: wants.clone() })?;
    receive_images(stream, host, &wants, &mut summary)?;
    let theirs = receive_wants(stream)?;
    send_images(stream, host, &theirs, &mut summary)?;
    send(stream, &Message::Done { vector: host.vector().map_err(storage)? })?;
    let acked = match receive(stream)? {
        Message::Done { vector } => vector,
        other => return Err(unexpected(&other)),
    };
    host.session_finished(computer, &acked).map_err(storage)?;
    Ok(summary)
}

/// The computer's side, from step 2, given the phone's `Sync` vector.
pub(crate) fn as_computer(stream: &mut (impl Read + Write), host: &dyn Host, phone: &str, theirs: &Vector) -> Result<Summary, SyncError> {
    let mut summary = Summary::default();
    send_changes(stream, host, theirs, &mut summary)?;
    let incoming = receive_changes(stream)?;
    apply(host, &incoming, &mut summary)?;
    let wanted_by_phone = receive_wants(stream)?;
    send_images(stream, host, &wanted_by_phone, &mut summary)?;
    let wants = host.missing_images();
    send(stream, &Message::Wants { images: wants.clone() })?;
    receive_images(stream, host, &wants, &mut summary)?;
    let acked = match receive(stream)? {
        Message::Done { vector } => vector,
        other => return Err(unexpected(&other)),
    };
    host.session_finished(phone, &acked).map_err(storage)?;
    send(stream, &Message::Done { vector: host.vector().map_err(storage)? })?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bare_file_names_are_accepted() {
        for good in ["3f2a.jpg", "a-b_c.webp", "book.png"] {
            assert!(is_safe_name(good), "{good}");
        }
        for bad in ["", "../x.jpg", "a/b.jpg", "/etc/passwd", ".hidden", "a\\b", "ü.jpg"] {
            assert!(!is_safe_name(bad), "{bad}");
        }
    }
}
