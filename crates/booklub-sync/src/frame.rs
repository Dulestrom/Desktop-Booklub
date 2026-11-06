//! Messages on the wire: each frame is a 4-byte big-endian length, a kind byte, and either a
//! JSON message or raw image bytes. JSON keeps sessions readable when debugging.

use std::io::{Read, Write};

use booklub_core::sync::{Delta, Vector};
use serde::{Deserialize, Serialize};

use crate::SyncError;

const JSON: u8 = 0;
const BYTES: u8 = 1;
/// A whole library's first delta fits comfortably; anything larger is not from Booklub.
const MAX_FRAME: u32 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// The 128-bit secret from the QR code.
    Qr,
    /// The 8-character code typed from beneath it.
    Code,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Refusal {
    Incompatible { protocol: u32 },
    NotPaired,
    IdentityChanged,
    PairingClosed,
    WrongCode { remaining: u8 },
    Declined,
    Busy,
    Failed { detail: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// Each side's first message (REQ-30e).
    Hello { protocol: u32, peer: String, name: String },
    Refused { refusal: Refusal },
    /// The phone's half of the password-authenticated key exchange.
    Pair { method: Method, spake: String },
    PairReply { spake: String },
    /// Proves knowledge of the shared key, bound to both certificates.
    PairProof { mac: String },
    PairAccepted,
    /// The phone asks for a session and says what it already holds.
    Sync { vector: Vector },
    /// What the other side lacks.
    Changes { delta: Delta },
    /// The background files this side lacks (REQ-33a).
    Wants { images: Vec<String> },
    /// Header for the raw frame that follows: an image and its SHA-256 (REQ-33b).
    Image { name: String, sha256: String },
    ImagesDone,
    /// The phone's vector after applying everything, which acknowledges tombstones (REQ-32e).
    Done { vector: Vector },
}

pub(crate) enum Frame {
    Message(Message),
    Bytes(Vec<u8>),
}

fn write_frame(stream: &mut impl Write, kind: u8, payload: &[u8]) -> Result<(), SyncError> {
    let length = u32::try_from(payload.len()).ok().filter(|n| *n <= MAX_FRAME).ok_or_else(|| SyncError::Protocol("a message is too large".into()))?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(&[kind])?;
    stream.write_all(payload)?;
    stream.flush()?;
    Ok(())
}

pub(crate) fn send(stream: &mut impl Write, message: &Message) -> Result<(), SyncError> {
    let json = serde_json::to_vec(message).map_err(|e| SyncError::Protocol(e.to_string()))?;
    write_frame(stream, JSON, &json)
}

pub(crate) fn send_bytes(stream: &mut impl Write, bytes: &[u8]) -> Result<(), SyncError> {
    write_frame(stream, BYTES, bytes)
}

pub(crate) fn read(stream: &mut impl Read) -> Result<Frame, SyncError> {
    let mut header = [0u8; 5];
    stream.read_exact(&mut header)?;
    let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    if length > MAX_FRAME {
        return Err(SyncError::Protocol("a message is too large".into()));
    }
    let mut payload = vec![0u8; length as usize];
    stream.read_exact(&mut payload)?;
    match header[4] {
        JSON => serde_json::from_slice(&payload).map(Frame::Message).map_err(|e| SyncError::Protocol(e.to_string())),
        BYTES => Ok(Frame::Bytes(payload)),
        other => Err(SyncError::Protocol(format!("unknown frame kind {other}"))),
    }
}

/// Reads a JSON message, turning the other side's refusal into an error.
pub(crate) fn receive(stream: &mut impl Read) -> Result<Message, SyncError> {
    match read(stream)? {
        Frame::Message(Message::Refused { refusal }) => Err(SyncError::from(refusal)),
        Frame::Message(message) => Ok(message),
        Frame::Bytes(_) => Err(SyncError::Protocol("expected a message, got bytes".into())),
    }
}

pub(crate) fn receive_bytes(stream: &mut impl Read) -> Result<Vec<u8>, SyncError> {
    match read(stream)? {
        Frame::Bytes(bytes) => Ok(bytes),
        Frame::Message(Message::Refused { refusal }) => Err(SyncError::from(refusal)),
        Frame::Message(_) => Err(SyncError::Protocol("expected image bytes".into())),
    }
}

pub(crate) fn unexpected(message: &Message) -> SyncError {
    let name = serde_json::to_value(message).ok().and_then(|v| v.get("type").and_then(|t| t.as_str()).map(String::from)).unwrap_or_default();
    SyncError::Protocol(format!("unexpected {name} message"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_refusals_become_errors() {
        let mut wire = Vec::new();
        send(&mut wire, &Message::Hello { protocol: 1, peer: "p".into(), name: "Ѓ".into() }).unwrap();
        send_bytes(&mut wire, &[1, 2, 3]).unwrap();
        send(&mut wire, &Message::Refused { refusal: Refusal::WrongCode { remaining: 3 } }).unwrap();
        let mut reader = wire.as_slice();
        assert_eq!(receive(&mut reader).unwrap(), Message::Hello { protocol: 1, peer: "p".into(), name: "Ѓ".into() });
        assert_eq!(receive_bytes(&mut reader).unwrap(), vec![1, 2, 3]);
        assert!(matches!(receive(&mut reader), Err(SyncError::WrongCode { remaining: 3 })));
    }

    #[test]
    fn an_oversized_length_is_refused_before_reading_it() {
        let mut wire = (MAX_FRAME + 1).to_be_bytes().to_vec();
        wire.push(JSON);
        assert!(matches!(read(&mut wire.as_slice()), Err(SyncError::Protocol(_))));
    }
}
