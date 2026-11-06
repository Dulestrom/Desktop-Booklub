//! Pairing proves that both sides know the same secret, and binds that proof to both TLS
//! certificates, so a device in the middle can't relay it (REQ-20 to REQ-20g).
//!
//! The exchange is SPAKE2, a password-authenticated key exchange. Each guess at a typed code
//! needs one live attempt against the computer, which allows five (REQ-20g), and nothing on the
//! wire lets an eavesdropper test guesses offline. That is what makes a 40-bit code safe.

use std::io::{Read, Write};

use ring::hmac;
use spake2::{Ed25519Group, Identity, Password, Spake2};

use crate::frame::{receive, send, unexpected, Message, Method};
use crate::key::hex;
use crate::SyncError;

const PHONE: &[u8] = b"booklub phone";
const COMPUTER: &[u8] = b"booklub computer";
pub(crate) const MAX_FAILURES: u8 = 5;

/// The identities the proof covers.
pub(crate) struct Binding<'a> {
    pub computer_fingerprint: &'a str,
    pub phone_fingerprint: &'a str,
    pub computer_id: &'a str,
    pub phone_id: &'a str,
}

fn transcript(role: &str, binding: &Binding) -> String {
    format!(
        "booklub-pair-v1|{role}|{}|{}|{}|{}",
        binding.computer_fingerprint, binding.phone_fingerprint, binding.computer_id, binding.phone_id
    )
}

fn proof(key: &[u8], role: &str, binding: &Binding) -> hmac::Tag {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), transcript(role, binding).as_bytes())
}

/// Compares in constant time.
fn check(key: &[u8], role: &str, binding: &Binding, mac: &str) -> bool {
    let Some(mac) = unhex(mac) else { return false };
    hmac::verify(&hmac::Key::new(hmac::HMAC_SHA256, key), transcript(role, binding).as_bytes(), &mac).is_ok()
}

pub(crate) fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

fn password(secret: &str) -> Password {
    Password::new(secret.as_bytes())
}

/// The phone's side. Returns once the computer's Reader has accepted.
pub(crate) fn as_phone(stream: &mut (impl Read + Write), method: Method, secret: &str, binding: &Binding) -> Result<(), SyncError> {
    let (state, outbound) = Spake2::<Ed25519Group>::start_a(&password(secret), &Identity::new(PHONE), &Identity::new(COMPUTER));
    send(stream, &Message::Pair { method, spake: hex(&outbound) })?;
    let reply = match receive(stream)? {
        Message::PairReply { spake } => unhex(&spake).ok_or_else(|| SyncError::Protocol("bad pairing reply".into()))?,
        other => return Err(unexpected(&other)),
    };
    let key = state.finish(&reply).map_err(|_| SyncError::Protocol("bad pairing reply".into()))?;
    send(stream, &Message::PairProof { mac: hex(proof(&key, "phone", binding).as_ref()) })?;
    match receive(stream)? {
        Message::PairProof { mac } if check(&key, "computer", binding, &mac) => {}
        Message::PairProof { .. } => return Err(SyncError::Protocol("the computer could not prove the code".into())),
        other => return Err(unexpected(&other)),
    }
    match receive(stream)? {
        Message::PairAccepted => Ok(()),
        other => Err(unexpected(&other)),
    }
}

/// The computer's side, up to the proofs. `Ok(true)` means the phone proved the secret, and the
/// caller then asks its Reader to confirm (REQ-20d). `Ok(false)` means the proof was wrong.
pub(crate) fn as_computer(stream: &mut (impl Read + Write), phone_message: &str, secret: &str, binding: &Binding) -> Result<bool, SyncError> {
    let inbound = unhex(phone_message).ok_or_else(|| SyncError::Protocol("bad pairing message".into()))?;
    let (state, outbound) = Spake2::<Ed25519Group>::start_b(&password(secret), &Identity::new(PHONE), &Identity::new(COMPUTER));
    send(stream, &Message::PairReply { spake: hex(&outbound) })?;
    let key = state.finish(&inbound).map_err(|_| SyncError::Protocol("bad pairing message".into()))?;
    let mac = match receive(stream)? {
        Message::PairProof { mac } => mac,
        other => return Err(unexpected(&other)),
    };
    if !check(&key, "phone", binding, &mac) {
        return Ok(false);
    }
    send(stream, &Message::PairProof { mac: hex(proof(&key, "computer", binding).as_ref()) })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> Binding<'static> {
        Binding { computer_fingerprint: "cc", phone_fingerprint: "pp", computer_id: "C", phone_id: "P" }
    }

    #[test]
    fn a_proof_checks_only_with_the_same_key_role_and_certificates() {
        let mac = hex(proof(b"key", "phone", &binding()).as_ref());
        assert!(check(b"key", "phone", &binding(), &mac));
        assert!(!check(b"other key", "phone", &binding(), &mac));
        assert!(!check(b"key", "computer", &binding(), &mac));
        let relayed = Binding { computer_fingerprint: "mitm", ..binding() };
        assert!(!check(b"key", "phone", &relayed, &mac));
        assert!(!check(b"key", "phone", &binding(), "zz"));
    }

    #[test]
    fn both_sides_derive_one_key_only_from_the_same_secret() {
        let (a, a_out) = Spake2::<Ed25519Group>::start_a(&password("K7Q49XMP"), &Identity::new(PHONE), &Identity::new(COMPUTER));
        let (b, b_out) = Spake2::<Ed25519Group>::start_b(&password("K7Q49XMP"), &Identity::new(PHONE), &Identity::new(COMPUTER));
        assert_eq!(a.finish(&b_out).unwrap(), b.finish(&a_out).unwrap());
        let (a, a_out) = Spake2::<Ed25519Group>::start_a(&password("K7Q49XMP"), &Identity::new(PHONE), &Identity::new(COMPUTER));
        let (b, b_out) = Spake2::<Ed25519Group>::start_b(&password("K7Q49XMQ"), &Identity::new(PHONE), &Identity::new(COMPUTER));
        assert_ne!(a.finish(&b_out).unwrap(), b.finish(&a_out).unwrap());
    }
}
