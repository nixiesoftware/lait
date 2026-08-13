//! Initial authoring bounds.
//!
//! These bound *locally authored live material* plus card-exchange proposals.
//! They do not make an arbitrary full causal merge safely bounded. A cap is
//! never a reason to discard graves, redirects, or observed-remove history.

/// Live Cards in the projected book.
pub const MAX_CARDS: usize = 4096;
/// Handle links on one Card.
pub const MAX_HANDLES_PER_CARD: usize = 64;
/// One authored note field, in bytes.
pub const MAX_NOTE_BYTES: usize = 4096;
/// One authored name field, in bytes.
pub const MAX_NAME_BYTES: usize = 256;
/// Materialized live projection, encoded.
pub const MAX_BOOK_BYTES: usize = 4 * 1024 * 1024;
/// Pairing fan-out, once pairing exists. Enforced on authored device links so
/// A5 does not inherit an unbounded set.
pub const MAX_SHARED_DEVICES: usize = 8;
/// Graves retained. Crossing it is a warning at the service, never a licence
/// to discard. The leaf crate still refuses a *new* delete that would grow
/// past it so a single device cannot write an unbounded graveyard.
pub const MAX_TOMBSTONES: usize = 8192;
/// Authoritative envelope, including causal history.
pub const MAX_ADDRESSBOOK_HISTORY_BYTES: usize = 16 * 1024 * 1024;
