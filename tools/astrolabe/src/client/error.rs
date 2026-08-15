//! One error type, carrying what a surface actually needs to decide.
//!
//! `ClientError { code, message, retryable }` is an ordinary Rust type shared
//! in-process. It exists not because anything is serialized — nothing is — but
//! because a surface has to choose between offering a retry, refusing, and
//! explaining, and a bare string makes that choice by guesswork.

use std::fmt;

pub type ClientResult<T> = Result<T, ClientError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientError {
    pub code: ErrorCode,
    pub message: String,
    /// Whether trying the same thing again could plausibly work. A refusal is
    /// not retryable however many times it is asked; an unreachable daemon is.
    pub retryable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// The request was not well formed, or names something that does not exist.
    Invalid,
    /// The request was understood and refused. Trying again changes nothing.
    Refused,
    /// The daemon or a device could not be reached right now.
    Unreachable,
    /// Something went wrong that the caller did not cause.
    Internal,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::Refused => "refused",
            Self::Unreachable => "unreachable",
            Self::Internal => "internal",
        }
    }
}

impl ClientError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Invalid,
            message: message.into(),
            retryable: false,
        }
    }

    pub fn refused(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Refused,
            message: message.into(),
            retryable: false,
        }
    }

    pub fn unreachable(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Unreachable,
            message: message.into(),
            retryable: true,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Internal,
            message: message.into(),
            retryable: false,
        }
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ClientError {}

impl From<lait_workbench::SupervisorError> for ClientError {
    /// The supervisor's own classification is preserved rather than flattened.
    /// A lifecycle conflict is a refusal — "stop it first" does not become true
    /// by asking twice — while an internal failure may not be the caller's
    /// fault at all.
    fn from(error: lait_workbench::SupervisorError) -> Self {
        use lait_workbench::SupervisorError as Supervisor;
        let message = error.to_string();
        match error {
            Supervisor::Invalid(_) | Supervisor::NotFound(_) => Self::invalid(message),
            Supervisor::AlreadyExists(_) | Supervisor::Conflict(_) => Self::refused(message),
            Supervisor::Internal(_) => Self::internal(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The distinction the type exists for: a surface must be able to tell a
    /// refusal from a hiccup, because only one of them is worth a retry button.
    #[test]
    fn a_refusal_is_never_retryable_and_an_unreachable_daemon_always_is() {
        assert!(!ClientError::refused("stop it first").retryable);
        assert!(!ClientError::invalid("no such device").retryable);
        assert!(ClientError::unreachable("no daemon").retryable);
    }

    #[test]
    fn supervisor_conflicts_arrive_as_refusals_not_failures() {
        let refused: ClientError =
            lait_workbench::SupervisorError::Conflict("device is running".into()).into();
        assert_eq!(refused.code, ErrorCode::Refused);
        assert!(!refused.retryable);
        assert_eq!(refused.message, "device is running");

        let missing: ClientError =
            lait_workbench::SupervisorError::NotFound("no such device".into()).into();
        assert_eq!(missing.code, ErrorCode::Invalid);
    }
}
