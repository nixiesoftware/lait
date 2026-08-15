//! Card identities and the opaque store hash a LocalAgent handle carries.

use serde::{Deserialize, Serialize};

mechanics::prefixed_id!(
    /// A locally minted Card. Random: `crd_` plus 26 Crockford characters.
    /// Never derived from a name, a key, or a handle.
    CardId,
    "crd_"
);

/// Digest of an identity-home path, as `config::home_hash` spells it: 16
/// lowercase hex characters. The crate does not compute it — the daemon does —
/// so this type never depends on how a home is named.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PathHash(String);

impl PathHash {
    /// Parse a 16-character lowercase hex digest.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.len() == 16 && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
            Some(Self(raw.to_ascii_lowercase()))
        } else {
            None
        }
    }

    /// The digest as stored.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PathHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
