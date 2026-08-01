//! Recovery authority, custody, and ceremony semantics.

pub use crate::authority::{Authority, Config, ConfigId, Holder, Principal, Scheme};
pub use crate::ceremony::{
    terminal_compactable, Approval, Ceremony, CeremonyProgress, Completion, Custody, CustodyExport,
    CustodyImport, DegradedHolder, Elevation, ElevationApproved, Failure, SpaceRecovered,
    SpaceRecovery, State,
};
pub use crate::custody::Package;
pub use crate::ledger::CeremonyMaterial;

/// Recovery-key artifacts persisted with the platform's private-file policy.
pub mod artifact {
    use std::path::Path;

    /// Why a recovery artifact could not be read or installed.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
    pub enum Failure {
        WrongProtector,
        PermissionDenied,
        Corrupt,
        Io(IoKind),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum IoKind {
        NotFound,
        Interrupted,
        InvalidData,
        Other,
        AlreadyExists,
    }

    impl std::fmt::Display for Failure {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "{self:?}")
        }
    }

    impl std::error::Error for Failure {}

    fn map_io(kind: std::io::ErrorKind) -> Failure {
        match kind {
            std::io::ErrorKind::NotFound => Failure::Io(IoKind::NotFound),
            std::io::ErrorKind::AlreadyExists => Failure::Io(IoKind::AlreadyExists),
            std::io::ErrorKind::Interrupted => Failure::Io(IoKind::Interrupted),
            std::io::ErrorKind::InvalidData => Failure::Io(IoKind::InvalidData),
            std::io::ErrorKind::PermissionDenied => Failure::PermissionDenied,
            _ => Failure::Io(IoKind::Other),
        }
    }

    fn map_read(failure: crate::secretfs::Failure) -> Failure {
        match failure {
            crate::secretfs::Failure::Undecryptable(_) => Failure::WrongProtector,
            crate::secretfs::Failure::Io(error) => map_io(error.kind()),
        }
    }

    fn map_adapter(error: &anyhow::Error) -> Failure {
        error
            .downcast_ref::<std::io::Error>()
            .map_or(Failure::Io(IoKind::Other), |source| map_io(source.kind()))
    }

    /// Read one portable recovery key. Missing material is a typed absence.
    pub fn read(path: &Path) -> Result<Option<[u8; 32]>, Failure> {
        let Some(bytes) = crate::secretfs::read_private(path).map_err(map_read)? else {
            return Ok(None);
        };
        let text = std::str::from_utf8(&bytes).map_err(|_| Failure::Corrupt)?;
        let decoded = data_encoding::HEXLOWER_PERMISSIVE
            .decode(text.trim().as_bytes())
            .map_err(|_| Failure::Corrupt)?;
        <[u8; 32]>::try_from(decoded.as_slice())
            .map(Some)
            .map_err(|_| Failure::Corrupt)
    }

    /// Install one portable recovery key without replacing existing custody.
    pub fn install(dir: &Path, file: &str, secret: &[u8; 32]) -> Result<(), Failure> {
        crate::secretfs::create_private_dir(dir).map_err(|error| map_adapter(&error))?;
        let encoded = data_encoding::HEXLOWER.encode(secret);
        crate::secretfs::write_private(
            &dir.join(file),
            encoded.as_bytes(),
            crate::secretfs::Create::New,
            crate::secretfs::Wrap::Portable,
        )
        .map_err(|error| map_adapter(&error))
    }
}

/// Session-bound evidence about share-holder availability.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Availability {
    Unknown,
    Observed {
        holders: Vec<Holder>,
        qualifies: bool,
        enabling: Vec<Holder>,
    },
}

/// Holder occurrences with durable custody backing for the standing configuration.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Backing {
    pub holders: Vec<Holder>,
    pub satisfies_configuration: bool,
}

impl Availability {
    pub fn recoverable_now(&self) -> Option<bool> {
        match self {
            Self::Unknown => None,
            Self::Observed { qualifies, .. } => Some(*qualifies),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_availability_is_not_observed_failure() {
        assert_eq!(Availability::Unknown.recoverable_now(), None);
        assert_eq!(
            Availability::Observed {
                holders: Vec::new(),
                qualifies: false,
                enabling: Vec::new(),
            }
            .recoverable_now(),
            Some(false)
        );
    }

    #[test]
    fn local_custody_cannot_imply_quorum_readiness() {
        let state = State {
            authority: None,
            configuration: ConfigId::single(),
            generation: 0,
            custody: Custody::Ready,
            backing: Backing {
                holders: Vec::new(),
                satisfies_configuration: false,
            },
            availability: Availability::Unknown,
        };

        assert_eq!(state.recoverable_now(), None);
    }
}
