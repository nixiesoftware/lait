//! Failures that refuse rather than guess.

use std::fmt;
use std::io;

/// Why an address-book operation did not apply.
#[derive(Debug)]
pub enum Error {
    /// A bound on locally authored live material was exceeded.
    Bound(&'static str),
    /// The action named a Card that is not live.
    NoSuchCard,
    /// The action was structurally invalid (empty name, bad handle, cycle).
    Invalid(&'static str),
    /// On-disk material could not be trusted. The previous atomic backup is
    /// left in place; nothing is invented to replace it.
    Corrupt(&'static str),
    /// The envelope declared a format this build does not speak.
    UnsupportedVersion(u8),
    /// Fabric refused the transaction.
    Engine(String),
    /// A durable write failed.
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bound(what) => write!(f, "address-book bound exceeded: {what}"),
            Self::NoSuchCard => write!(f, "no such live Card"),
            Self::Invalid(what) => write!(f, "invalid address-book action: {what}"),
            Self::Corrupt(what) => write!(f, "address-book envelope is corrupt: {what}"),
            Self::UnsupportedVersion(v) => {
                write!(f, "unsupported address-book envelope version {v}")
            }
            Self::Engine(what) => write!(f, "fabric: {what}"),
            Self::Io(err) => write!(f, "address-book io: {err}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<fabric::commit::Failure> for Error {
    fn from(err: fabric::commit::Failure) -> Self {
        Self::Engine(err.to_string())
    }
}
