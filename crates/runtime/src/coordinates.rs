//! Approach coordinates live in the `contact` crate now — the ticket is how a
//! peer learns whom to Contact, and a browser initiator must parse the same
//! bytes the daemon mints. Every prior path still works.

pub use ::contact::coordinates::*;
