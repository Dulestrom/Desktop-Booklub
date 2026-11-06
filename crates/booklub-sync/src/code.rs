//! What the Reader carries from the computer to the phone: the QR code (REQ-20a) with its
//! 128-bit secret (QA-11), and the typed pairing code beneath it (REQ-20f).

use std::net::{IpAddr, SocketAddr};

use ring::rand::{SecureRandom, SystemRandom};

/// Crockford's base32, which leaves out I, L, O, and U so a code reads back unambiguously.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
pub const CODE_LENGTH: usize = 8;
const SCHEME: &str = "booklub:pair";
pub const PROTOCOL: u32 = 1;

pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    SystemRandom::new().fill(&mut bytes).expect("the system random source is always available");
    bytes
}

fn encode(bytes: &[u8], length: usize) -> String {
    let mut out = String::with_capacity(length);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 && out.len() < length {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 && out.len() < length {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// A fresh typed pairing code: 40 random bits as 8 characters.
pub fn new_code() -> String {
    encode(&random_bytes::<5>(), CODE_LENGTH)
}

/// A fresh QR secret: 128 random bits as 26 characters (QA-11).
pub fn new_secret() -> String {
    encode(&random_bytes::<16>(), 26)
}

/// "K7Q49XMP" shown as "K7Q4 9XMP".
pub fn display_code(code: &str) -> String {
    let (left, right) = code.split_at(code.len().min(4));
    format!("{left} {right}")
}

/// Reads back what the Reader typed: case, spaces, and dashes don't matter, and the letters
/// people confuse with digits count as those digits.
pub fn normalize_code(typed: &str) -> Option<String> {
    let mut out = String::new();
    for c in typed.chars().filter(|c| !c.is_whitespace() && *c != '-') {
        let c = match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        };
        if !ALPHABET.contains(&(c as u8)) || !c.is_ascii() {
            return None;
        }
        out.push(c);
    }
    (out.len() == CODE_LENGTH).then_some(out)
}

/// Everything a phone needs to reach and trust a computer, as the QR code carries it.
#[derive(Debug, Clone, PartialEq)]
pub struct Invitation {
    pub peer_id: String,
    pub name: String,
    pub addresses: Vec<IpAddr>,
    pub port: u16,
    pub fingerprint: String,
    pub secret: String,
}

impl Invitation {
    pub fn to_uri(&self) -> String {
        let addresses: Vec<String> = self.addresses.iter().map(IpAddr::to_string).collect();
        format!(
            "{SCHEME}?v={PROTOCOL}&id={}&n={}&a={}&p={}&f={}&s={}",
            escape(&self.peer_id),
            escape(&self.name),
            escape(&addresses.join(",")),
            self.port,
            self.fingerprint,
            self.secret
        )
    }

    pub fn parse(uri: &str) -> Result<Self, crate::SyncError> {
        let invalid = || crate::SyncError::NotAnInvitation;
        let query = uri.trim().strip_prefix(SCHEME).and_then(|rest| rest.strip_prefix('?')).ok_or_else(invalid)?;
        let mut fields = std::collections::HashMap::new();
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=').ok_or_else(invalid)?;
            fields.insert(key, unescape(value).ok_or_else(invalid)?);
        }
        let field = |key: &str| fields.get(key).cloned().ok_or_else(invalid);
        let version: u32 = field("v")?.parse().map_err(|_| invalid())?;
        if version != PROTOCOL {
            return Err(crate::SyncError::Incompatible { ours: PROTOCOL, theirs: version });
        }
        let addresses = field("a")?
            .split(',')
            .filter(|a| !a.is_empty())
            .map(|a| a.parse::<IpAddr>().map_err(|_| invalid()))
            .collect::<Result<Vec<_>, _>>()?;
        let fingerprint = field("f")?;
        let secret = field("s")?;
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) || secret.len() < 26 || addresses.is_empty() {
            return Err(invalid());
        }
        Ok(Self { peer_id: field("id")?, name: field("n")?, addresses, port: field("p")?.parse().map_err(|_| invalid())?, fingerprint, secret })
    }

    pub fn socket_addresses(&self) -> Vec<SocketAddr> {
        self.addresses.iter().map(|ip| SocketAddr::new(*ip, self.port)).collect()
    }
}

fn escape(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~:,".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn unescape(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_eight_unambiguous_characters_and_read_back_forgivingly() {
        let code = new_code();
        assert_eq!(code.len(), 8);
        assert!(code.bytes().all(|b| ALPHABET.contains(&b)));
        assert_eq!(normalize_code(&display_code(&code)), Some(code.clone()));
        assert_eq!(normalize_code(&display_code(&code).to_lowercase()), Some(code));
        assert_eq!(normalize_code("k7q4-9xmo"), Some("K7Q49XM0".into()));
        assert_eq!(normalize_code("IL00 0000"), Some("11000000".into()));
        assert_eq!(normalize_code("K7Q4 9XM"), None);
        assert_eq!(normalize_code("K7Q4 9XMU"), None);
        assert_eq!(display_code("K7Q49XMP"), "K7Q4 9XMP");
    }

    #[test]
    fn a_qr_secret_carries_128_bits() {
        let secret = new_secret();
        assert_eq!(secret.len(), 26);
        assert_ne!(secret, new_secret());
        assert_eq!(encode(&[0xff; 16], 26), "ZZZZZZZZZZZZZZZZZZZZZZZZZW");
    }

    #[test]
    fn an_invitation_survives_the_round_trip() {
        let invitation = Invitation {
            peer_id: "a1b2c3".into(),
            name: "Dule's PC & Ѓорѓи".into(),
            addresses: vec!["192.168.1.20".parse().unwrap(), "fe80::1".parse().unwrap()],
            port: 47120,
            fingerprint: "ab".repeat(32),
            secret: new_secret(),
        };
        let uri = invitation.to_uri();
        assert!(uri.starts_with("booklub:pair?v=1&"));
        assert_eq!(Invitation::parse(&uri).unwrap(), invitation);
        assert!(matches!(Invitation::parse("https://example.com"), Err(crate::SyncError::NotAnInvitation)));
        assert!(matches!(
            Invitation::parse(&uri.replace("v=1", "v=2")),
            Err(crate::SyncError::Incompatible { ours: 1, theirs: 2 })
        ));
    }
}
