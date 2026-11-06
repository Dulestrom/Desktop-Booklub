//! Each device's long-lived identity: a self-signed certificate whose SHA-256 fingerprint the
//! other side stores at pairing (REQ-20e) and checks on every later connection (QA-10).

use std::fs;
use std::io::Write;
use std::path::Path;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

const CERT_FILE: &str = "device-certificate.der";
const KEY_FILE: &str = "device-key.der";

pub struct DeviceKey {
    pub(crate) certificate: CertificateDer<'static>,
    key: Vec<u8>,
    pub fingerprint: String,
}

impl Clone for DeviceKey {
    fn clone(&self) -> Self {
        Self { certificate: self.certificate.clone(), key: self.key.clone(), fingerprint: self.fingerprint.clone() }
    }
}

impl std::fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceKey").field("fingerprint", &self.fingerprint).finish_non_exhaustive()
    }
}

impl DeviceKey {
    /// Reads this device's key from `dir`, creating it the first time. The key never leaves
    /// the device and never syncs (REQ-35a).
    pub fn load_or_create(dir: &Path) -> std::io::Result<Self> {
        let (cert_path, key_path) = (dir.join(CERT_FILE), dir.join(KEY_FILE));
        if let (Ok(certificate), Ok(key)) = (fs::read(&cert_path), fs::read(&key_path)) {
            return Ok(Self::from_parts(certificate, key));
        }
        let fresh = Self::generate();
        fs::create_dir_all(dir)?;
        write_private(&key_path, &fresh.key)?;
        fs::write(&cert_path, fresh.certificate.as_ref())?;
        Ok(fresh)
    }

    /// A new identity held only in memory, as tests and a freshly installed app need.
    pub fn generate() -> Self {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("P-256 keys are always available");
        let params = rcgen::CertificateParams::new(vec!["booklub".to_string()]).expect("a fixed name is valid");
        let certificate = params.self_signed(&key).expect("signing a fresh certificate");
        Self::from_parts(certificate.der().to_vec(), key.serialize_der())
    }

    fn from_parts(certificate: Vec<u8>, key: Vec<u8>) -> Self {
        let fingerprint = fingerprint(&certificate);
        Self { certificate: CertificateDer::from(certificate), key, fingerprint }
    }

    pub(crate) fn private_key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone()))
    }
}

/// SHA-256 of a certificate, as lowercase hex.
pub fn fingerprint(certificate: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, certificate).as_ref())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_created_once_and_read_back_unchanged() {
        let dir = tempfile::TempDir::new().unwrap();
        let first = DeviceKey::load_or_create(dir.path()).unwrap();
        let again = DeviceKey::load_or_create(dir.path()).unwrap();
        assert_eq!(first.fingerprint, again.fingerprint);
        assert_eq!(first.fingerprint.len(), 64);
        assert_ne!(first.fingerprint, DeviceKey::generate().fingerprint);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join(KEY_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "only the owner may read the private key");
        }
    }
}
