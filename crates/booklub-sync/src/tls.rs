//! TLS 1.3 between paired devices (QA-10). There is no certificate authority: a phone pins the
//! computer's fingerprint from the QR code or from pairing, and the computer checks every
//! phone's certificate against the fingerprints it stored at pairing.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, DistinguishedName, Error, ServerConfig, SignatureScheme};

use crate::key::{fingerprint, DeviceKey};

pub(crate) fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// The server accepts any well-formed certificate at the TLS layer, because an unpaired phone
/// must reach the pairing step. Every message after Hello is refused unless the certificate's
/// fingerprint belongs to a paired phone (QA-12).
pub(crate) fn server_config(key: &DeviceKey) -> Result<Arc<ServerConfig>, Error> {
    let provider = provider();
    let config = ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(Arc::new(AnyCertificate { provider }))
        .with_single_cert(vec![key.certificate.clone()], key.private_key())?;
    Ok(Arc::new(config))
}

/// With `expected`, the handshake fails unless the computer presents exactly that certificate
/// (REQ-22d). Without it, as when pairing by typed code, the pairing proof binds the
/// certificate instead.
pub(crate) fn client_config(key: &DeviceKey, expected: Option<String>) -> Result<Arc<ClientConfig>, Error> {
    let provider = provider();
    let config = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { expected, provider }))
        .with_client_auth_cert(vec![key.certificate.clone()], key.private_key())?;
    Ok(Arc::new(config))
}

pub(crate) fn server_name() -> ServerName<'static> {
    ServerName::try_from("booklub").expect("a fixed name is valid")
}

/// How a pinned-certificate mismatch shows up in a handshake error.
pub(crate) fn is_identity_mismatch(error: &Error) -> bool {
    matches!(error, Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure))
}

#[derive(Debug)]
struct Pinned {
    expected: Option<String>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, Error> {
        match &self.expected {
            Some(expected) if *expected != fingerprint(end_entity) => Err(Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure)),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

#[derive(Debug)]
struct AnyCertificate {
    provider: Arc<CryptoProvider>,
}

impl ClientCertVerifier for AnyCertificate {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(&self, _: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: UnixTime) -> Result<ClientCertVerified, Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    /// The signature proves the phone holds the private key for the certificate it presents,
    /// so a copied certificate is useless without its key.
    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}
