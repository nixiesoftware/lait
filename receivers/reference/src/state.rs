use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use display_protocol::ids::{
    Challenge, CoordinatorFingerprint, DisplayDeviceId, DisplayPairingId, PollKey, ProofKey,
    ReceiverNonce,
};
use display_protocol::pairing::CoordinatorTrust;
use mechanics::secretfs::{self, Create, Wrap};
use serde::{Deserialize, Serialize};

const MAX_STATE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialState {
    Pairing {
        trust: CoordinatorTrust,
        pairing: DisplayPairingId,
        receiver_nonce: ReceiverNonce,
        poll_key: PollKey,
        fingerprint: CoordinatorFingerprint,
        phrase: Vec<String>,
        user_confirmed: bool,
    },
    Enrolling {
        trust: CoordinatorTrust,
        pairing: DisplayPairingId,
        device: DisplayDeviceId,
        proof_key: ProofKey,
        enrollment_challenge: Challenge,
    },
    Paired {
        trust: CoordinatorTrust,
        device: DisplayDeviceId,
        proof_key: ProofKey,
    },
    Revoked {
        trust: CoordinatorTrust,
        device: DisplayDeviceId,
    },
}

impl CredentialState {
    pub fn trust(&self) -> &CoordinatorTrust {
        match self {
            Self::Pairing { trust, .. }
            | Self::Enrolling { trust, .. }
            | Self::Paired { trust, .. }
            | Self::Revoked { trust, .. } => trust,
        }
    }
}

pub struct Vault {
    directory: PathBuf,
    state_path: PathBuf,
}

impl Vault {
    pub fn open(directory: PathBuf) -> Result<Self> {
        secretfs::create_private_dir(&directory).context("create private receiver state")?;
        Ok(Self {
            state_path: directory.join("credential.json"),
            directory,
        })
    }

    pub fn load(&self) -> Result<Option<CredentialState>> {
        let Some(bytes) = secretfs::read_private(&self.state_path)
            .map_err(anyhow::Error::new)
            .context("read receiver credential")?
        else {
            return Ok(None);
        };
        if bytes.len() > MAX_STATE_BYTES {
            return Err(anyhow!("receiver credential exceeds its storage bound"));
        }
        serde_json::from_slice(&bytes)
            .context("decode receiver credential")
            .map(Some)
    }

    pub fn save(&self, state: &CredentialState) -> Result<()> {
        let bytes = serde_json::to_vec(state).context("encode receiver credential")?;
        if bytes.len() > MAX_STATE_BYTES {
            return Err(anyhow!("receiver credential exceeds its storage bound"));
        }
        let temporary = self.directory.join("credential.json.tmp");
        secretfs::write_private(&temporary, &bytes, Create::Replace, Wrap::DeviceBound)
            .context("write receiver credential candidate")?;
        secretfs::persist_replace(&temporary, &self.state_path)
            .context("commit receiver credential")
    }

    pub fn path(&self) -> &Path {
        &self.state_path
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.state_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("remove receiver credential"),
        }
    }
}
