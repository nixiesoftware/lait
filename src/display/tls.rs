//! Stable self-hosted TLS identity.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use display_protocol::ids::CoordinatorFingerprint;
use display_protocol::pairing::{CoordinatorInstance, CoordinatorTrust};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const TLS_STATE_VERSION: u32 = 1;
pub const DEFAULT_DISPLAY_PORT: u16 = 7443;

#[derive(Serialize, Deserialize)]
struct StoredTlsIdentity {
    version: u32,
    instance: String,
    label: String,
    origin: String,
    certificate_der: Vec<u8>,
    private_key_der: Vec<u8>,
}

/// Stable self-hosted TLS identity advertised to receivers during pairing.
pub struct DisplayTlsIdentity {
    instance: CoordinatorInstance,
    fingerprint: CoordinatorFingerprint,
    certificate_pem: String,
    server_config: Arc<rustls::ServerConfig>,
    bind: SocketAddr,
    path: PathBuf,
}

impl DisplayTlsIdentity {
    pub fn load_or_create(root: &Path, label: &str, port: u16) -> Result<Self> {
        if port == 0 {
            return Err(anyhow!("display coordinator port must be non-zero"));
        }
        mechanics::secretfs::create_private_dir(root)
            .with_context(|| format!("protect display TLS directory {}", root.display()))?;
        let path = root.join("coordinator-tls.json");
        let stored =
            match mechanics::secretfs::read_private(&path).map_err(|error| anyhow!(error))? {
                Some(bytes) => serde_json::from_slice::<StoredTlsIdentity>(&bytes)
                    .with_context(|| format!("decode {}", path.display()))?,
                None => {
                    let created = create_identity(label, port)?;
                    let bytes = serde_json::to_vec(&created)?;
                    mechanics::secretfs::write_private(
                        &path,
                        &bytes,
                        mechanics::secretfs::Create::New,
                        mechanics::secretfs::Wrap::DeviceBound,
                    )
                    .with_context(|| format!("write {}", path.display()))?;
                    created
                }
            };
        validate_stored(&stored, port)?;
        let digest = Sha256::digest(&stored.certificate_der);
        let fingerprint = CoordinatorFingerprint::parse(data_encoding::HEXLOWER.encode(&digest))
            .context("parse display certificate fingerprint")?;
        let certificate = CertificateDer::from(stored.certificate_der.clone());
        let certificate_pem = encode_certificate_pem(&stored.certificate_der);
        let private_key =
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(stored.private_key_der.clone()));
        // This workspace legitimately enables both rustls providers through
        // different transports. Choosing the coordinator's provider here
        // avoids process-global provider inference and keeps daemon startup
        // independent of which other TLS client happened to initialize first.
        let server_config = rustls::ServerConfig::builder_with_provider(
            rustls::crypto::ring::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .context("select display TLS protocol versions")?
        .with_no_client_auth()
        .with_single_cert(vec![certificate], private_key)
        .context("configure display TLS certificate")?;
        let instance = CoordinatorInstance {
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            instance: stored.instance,
            label: stored.label,
            trust: CoordinatorTrust::PinnedCertificate {
                origin: stored.origin,
                sha256: fingerprint.clone(),
            },
        };
        display_protocol::pairing::validate_instance(&instance)
            .context("validate stored display TLS identity")?;
        Ok(Self {
            instance,
            fingerprint,
            certificate_pem,
            server_config: Arc::new(server_config),
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port),
            path,
        })
    }

    pub fn instance(&self) -> &CoordinatorInstance {
        &self.instance
    }

    pub fn fingerprint(&self) -> &CoordinatorFingerprint {
        &self.fingerprint
    }

    pub fn certificate_pem(&self) -> &str {
        &self.certificate_pem
    }

    pub fn server_config(&self) -> Arc<rustls::ServerConfig> {
        self.server_config.clone()
    }

    pub fn bind(&self) -> SocketAddr {
        self.bind
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn encode_certificate_pem(certificate: &[u8]) -> String {
    let encoded = data_encoding::BASE64.encode(certificate);
    let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
    for line in encoded.as_bytes().chunks(64) {
        for byte in line {
            pem.push(char::from(*byte));
        }
        pem.push('\n');
    }
    pem.push_str("-----END CERTIFICATE-----\n");
    pem
}

fn create_identity(label: &str, port: u16) -> Result<StoredTlsIdentity> {
    let label = label.trim();
    if label.is_empty()
        || label.len() > display_protocol::bounds::MAX_LABEL_BYTES
        || label.chars().any(char::is_control)
    {
        return Err(anyhow!("display coordinator label is invalid"));
    }
    let address = advertised_address();
    let host = address.to_string();
    let origin = format!("https://{host}:{port}");
    let rcgen::CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(vec![
        host,
        "localhost".into(),
        "astrolabe.local".into(),
    ])
    .context("generate display TLS identity")?;
    Ok(StoredTlsIdentity {
        version: TLS_STATE_VERSION,
        instance: random_hex::<16>()?,
        label: label.to_string(),
        origin,
        certificate_der: cert.der().to_vec(),
        private_key_der: signing_key.serialize_der(),
    })
}

fn validate_stored(stored: &StoredTlsIdentity, port: u16) -> Result<()> {
    if stored.version != TLS_STATE_VERSION
        || stored.instance.len() != 32
        || !stored
            .instance
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || stored.certificate_der.is_empty()
        || stored.private_key_der.is_empty()
        || !stored.origin.ends_with(&format!(":{port}"))
    {
        return Err(anyhow!("stored display TLS identity is invalid"));
    }
    Ok(())
}

fn advertised_address() -> Ipv4Addr {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0));
    if let Ok(socket) = socket {
        if socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).is_ok() {
            if let Ok(SocketAddr::V4(address)) = socket.local_addr() {
                if !address.ip().is_unspecified() {
                    return *address.ip();
                }
            }
        }
    }
    Ipv4Addr::LOCALHOST
}

fn random_hex<const N: usize>() -> Result<String> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).context("obtain display TLS randomness")?;
    Ok(data_encoding::HEXLOWER.encode(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_identity_is_stable_and_private() {
        let root = std::env::temp_dir().join(format!(
            "lait-display-tls-{}-{}",
            std::process::id(),
            mechanics::wallclock::now_millis()
        ));
        let first = DisplayTlsIdentity::load_or_create(&root, "Home Astrolabe", 7443).unwrap();
        let second = DisplayTlsIdentity::load_or_create(&root, "Ignored rename", 7443).unwrap();
        assert_eq!(first.instance(), second.instance());
        assert_eq!(first.fingerprint(), second.fingerprint());
        assert_eq!(first.certificate_pem(), second.certificate_pem());
        let bootstrap = display_protocol::pairing::ReceiverBootstrap {
            protocol_major: display_protocol::PROTOCOL_MAJOR,
            trust: first.instance().trust.clone(),
            certificate_pem: Some(first.certificate_pem().to_string()),
            rendezvous: None,
        };
        display_protocol::pairing::validate_bootstrap(&bootstrap).unwrap();
        assert!(first.path().exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
