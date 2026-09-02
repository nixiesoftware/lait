//! Contact v2, portable: one crate holding the protocol both halves of a
//! Contact speak, so a browser initiator and the daemon's accepter can never
//! drift. The runtime re-exports this as `plane::contact` and keeps what is
//! genuinely its own — the scheduler, the Neighbor registry, the accept
//! side's serving loop.

pub mod admission;
pub mod authority;
pub mod coordinates;
mod protocol;
/// The initiator that drives the transcript over a comms stream. Behind the
/// `wire` feature, because it links comms (and thus iroh); the transcript
/// machines it drives are transport-free and always available in `protocol`.
#[cfg(feature = "wire")]
pub mod pull;
pub mod wire;

pub use protocol::*;
