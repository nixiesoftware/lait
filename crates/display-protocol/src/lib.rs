#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::unimplemented,
        clippy::unchecked_time_subtraction,
        clippy::todo,
        clippy::string_slice,
        clippy::panic_in_result_fn,
        clippy::panic,
        clippy::exit,
        clippy::as_conversions
    )
)]

//! The product-neutral wire contract between an Astrolabe display coordinator
//! and an attached receiver.
//!
//! This crate deliberately imports no World, Space, package, controller, or
//! platform type. A receiver can identify only itself, its current assignment
//! and program, an opaque revision or asset, its playback cursor, capabilities,
//! and bounded health. Cryptographic transcripts are binary and length-delimited
//! so their meaning never depends on a JSON implementation.

pub mod auth;
pub mod bounds;
pub mod ids;
pub mod pairing;
pub mod program;
pub mod receiver;
pub mod wire;

use std::error::Error;
use std::fmt;

/// The only protocol major implemented by this release.
pub const PROTOCOL_MAJOR: u32 = 1;

/// A stable refusal class suitable for a receiver's native error chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    InvalidIdentifier(&'static str),
    InvalidEncoding(&'static str),
    InvalidShape(&'static str),
    BoundExceeded(&'static str),
    Unsupported(&'static str),
    Integrity(&'static str),
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentifier(name) => write!(formatter, "invalid {name}"),
            Self::InvalidEncoding(name) => write!(formatter, "invalid {name} encoding"),
            Self::InvalidShape(name) => write!(formatter, "invalid {name} shape"),
            Self::BoundExceeded(name) => write!(formatter, "{name} exceeds its protocol bound"),
            Self::Unsupported(name) => write!(formatter, "unsupported {name}"),
            Self::Integrity(name) => write!(formatter, "{name} integrity check failed"),
        }
    }
}

impl Error for Refusal {}
