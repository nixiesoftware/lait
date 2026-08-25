use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use display_protocol::ids::CoordinatorFingerprint;
use display_protocol::pairing::CoordinatorTrust;
use sha2::{Digest, Sha256};
use ureq::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use ureq::rustls::crypto::CryptoProvider;
use ureq::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use ureq::rustls::{DigitallySignedStruct, SignatureScheme};

#[derive(Debug)]
struct PinnedCertificateVerifier {
    fingerprint: CoordinatorFingerprint,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedCertificateVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, ureq::rustls::Error> {
        let presented = format!("{:x}", Sha256::digest(end_entity.as_ref()));
        if presented == self.fingerprint.as_str() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(ureq::rustls::Error::InvalidCertificate(
                ureq::rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, ureq::rustls::Error> {
        ureq::rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, ureq::rustls::Error> {
        ureq::rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub fn agent(trust: &CoordinatorTrust) -> Result<ureq::Agent> {
    let builder = ureq::builder()
        .redirects(0)
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(35))
        .timeout_write(Duration::from_secs(30));
    match trust {
        CoordinatorTrust::PinnedCertificate { sha256, .. } => {
            let provider = Arc::new(ureq::rustls::crypto::ring::default_provider());
            let verifier = Arc::new(PinnedCertificateVerifier {
                fingerprint: sha256.clone(),
                provider: Arc::clone(&provider),
            });
            let config = ureq::rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .context("select safe TLS protocol versions")?
                .dangerous()
                .with_custom_certificate_verifier(verifier)
                .with_no_client_auth();
            Ok(builder.tls_config(Arc::new(config)).build())
        }
        // Identity-anchored trust reaches its resolved route over the platform
        // Web PKI: the anchor is the profile the pairing offer must report,
        // not any certificate this client could pin.
        CoordinatorTrust::WebPkiOrigin { .. } | CoordinatorTrust::Profile { .. } => {
            Ok(builder.build())
        }
    }
}

pub fn origin(trust: &CoordinatorTrust) -> &str {
    match trust {
        CoordinatorTrust::PinnedCertificate { origin, .. }
        | CoordinatorTrust::WebPkiOrigin { origin }
        | CoordinatorTrust::Profile { origin, .. } => origin,
    }
}
