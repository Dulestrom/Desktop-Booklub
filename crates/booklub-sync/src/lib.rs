//! Pairing and two-way sync between Booklub on a computer and Booklub on phones (Android SRS
//! Sections 3.6 to 3.8). The computer listens and phones connect to it, so a change on one
//! phone reaches another through the computer (REQ-20j). Merging is `booklub_core::sync`'s job;
//! this crate moves its deltas and images between devices, over TLS 1.3, on the home network
//! only.
//!
//! Each app implements [`Host`] over its own store, so the same protocol code runs on the
//! desktop and, through the bridge, on Android (QA-40).

pub mod code;
pub mod discovery;
mod frame;
pub mod key;
pub mod network;
mod pairing;
pub mod server;
mod session;
mod tls;

pub mod client;

pub use booklub_core::sync::{ApplyReport, Delta, PairedPeer, Vector};
pub use code::{display_code, normalize_code, Invitation, PROTOCOL};
pub use frame::Method;
pub use key::DeviceKey;

/// The first port tried; the next free one is used if it is taken (plan P8).
pub const DEFAULT_PORT: u16 = 47120;

/// Everything an app gives the protocol. Calls arrive on a sync thread, one session at a time.
pub trait Host: Send + Sync {
    /// This device's sync origin ID (the core store's peer ID).
    fn peer_id(&self) -> String;
    fn device_name(&self) -> String;
    fn peers(&self) -> Result<Vec<PairedPeer>, String>;
    fn save_peer(&self, peer: &PairedPeer) -> Result<(), String>;
    fn vector(&self) -> Result<Vector, String>;
    fn delta_since(&self, theirs: &Vector) -> Result<Delta, String>;
    /// Merges a delta in one transaction (REQ-30c).
    fn apply(&self, delta: &Delta) -> Result<ApplyReport, String>;
    /// A phone drops its own library before its first session with a computer (UR-02).
    fn reset_for_pairing(&self) -> Result<(), String>;
    /// Background files the library refers to but this device lacks (REQ-33a). Asking every
    /// session, rather than only after new changes, re-requests anything an interrupted session
    /// or a failed check left out (REQ-33c).
    fn missing_images(&self) -> Vec<String>;
    /// The sync thumbnail to send for a background file (BR-08, REQ-33e).
    fn read_image(&self, name: &str) -> Option<Vec<u8>>;
    /// Stores a received thumbnail as the background file `name` (REQ-33f).
    fn store_image(&self, name: &str, bytes: &[u8]) -> Result<(), String>;
    /// After a completed session: record the time and the other side's acknowledged vector,
    /// collect acknowledged tombstones (REQ-32e), and delete unreferenced images (REQ-33d).
    fn session_finished(&self, peer: &str, acked: &Vector) -> Result<(), String>;
    /// REQ-20d: shows the other device's name and returns whether the Reader confirmed it.
    /// This may block until the Reader answers.
    fn confirm_pairing(&self, other_name: &str) -> bool;
    /// Progress for REQ-31a, and anything else worth showing.
    fn notify(&self, _event: Event) {}
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// `received` of `total` books arrived; images follow.
    Books { received: usize, total: usize },
    Images { received: usize, total: usize },
    Paired { peer: PairedPeer },
    SessionDone { peer: String, summary: Summary },
    SessionFailed { peer: Option<String>, reason: String },
    PairingClosed,
}

/// What one session moved, in both directions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub units_received: usize,
    pub units_sent: usize,
    pub deleted: usize,
    pub images_received: usize,
    pub images_sent: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("{0} is not on the home network")]
    NotHomeNetwork(String),
    #[error("The other device can't be reached.")]
    Unreachable,
    /// REQ-22d.
    #[error("This device's identity has changed. Pair it again.")]
    IdentityChanged,
    /// REQ-21b.
    #[error("This device is not paired. Pair it again.")]
    NotPaired,
    #[error("Pairing has ended on the computer. Show a new code and try again.")]
    PairingClosed,
    #[error("That code is not right. {remaining} tries left.")]
    WrongCode { remaining: u8 },
    #[error("Pairing was cancelled.")]
    Declined,
    #[error("The computer is busy with another device. Try again in a moment.")]
    Busy,
    /// REQ-30f: the side with the smaller number needs updating.
    #[error("{}", incompatible_text(*ours, *theirs))]
    Incompatible { ours: u32, theirs: u32 },
    #[error("That is not a Booklub pairing code.")]
    NotAnInvitation,
    #[error("Booklub couldn't save what arrived: {0}")]
    Storage(String),
    #[error("The connection failed: {0}")]
    Io(String),
    #[error("The other device sent something unexpected: {0}")]
    Protocol(String),
}

fn incompatible_text(ours: u32, theirs: u32) -> &'static str {
    if ours < theirs {
        "This copy of Booklub is too old to sync with the other device. Update it."
    } else {
        "Booklub on the other device is too old to sync with this one. Update it there."
    }
}

impl From<std::io::Error> for SyncError {
    fn from(error: std::io::Error) -> Self {
        use std::io::ErrorKind::*;
        match error.kind() {
            ConnectionRefused | HostUnreachable | NetworkUnreachable | TimedOut | WouldBlock => SyncError::Unreachable,
            _ => match error.get_ref().and_then(|inner| inner.downcast_ref::<rustls::Error>()) {
                Some(tls) if tls::is_identity_mismatch(tls) => SyncError::IdentityChanged,
                _ => SyncError::Io(error.to_string()),
            },
        }
    }
}

impl From<frame::Refusal> for SyncError {
    fn from(refusal: frame::Refusal) -> Self {
        use frame::Refusal::*;
        match refusal {
            Incompatible { protocol } => SyncError::Incompatible { ours: PROTOCOL, theirs: protocol },
            NotPaired => SyncError::NotPaired,
            IdentityChanged => SyncError::IdentityChanged,
            PairingClosed => SyncError::PairingClosed,
            WrongCode { remaining } => SyncError::WrongCode { remaining },
            Declined => SyncError::Declined,
            Busy => SyncError::Busy,
            Failed { detail } => SyncError::Protocol(detail),
        }
    }
}
