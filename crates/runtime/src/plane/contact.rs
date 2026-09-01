//! Contact v2 lives in the `contact` crate now — one set of types for the
//! daemon and every portable initiator (a browser tab above all). This shim
//! keeps every `plane::contact::…` path working; the scheduler, registry,
//! and accept side remain this crate's own.

pub use ::contact::*;
