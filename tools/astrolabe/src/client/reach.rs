//! The reach plane, as this client reaches it.
//!
//! The plane itself is substrate and lives in [`correspondence::plane`] — it
//! serves issue notifications, invitation delivery, agent messages and this
//! chat, and the design is explicit that clients are merely callers. It sat
//! here, which is why a World could not send and why mail arrived only while a
//! window was open.
//!
//! What remains is this re-export, so the client's own modules keep one name
//! for it while the daemon becomes the thing that holds it.

pub use correspondence::plane::{
    Collected, Opened, PostReach, ReachError, ReachPlane, DEFAULT_POST_URL,
};
