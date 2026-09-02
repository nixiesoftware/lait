//! The honest browser control vocabulary.
//!
//! A browser-composed backend — the daemon's own Session machinery over a
//! pulled Replica, in a Worker — answers the viewer's control plane. It is
//! not a daemon: it has no identity home, no orbit registry on disk, no
//! address book, no processes to spawn, and no probe to take of anything.
//! This crate says, exhaustively, what such a backend answers, what it
//! refuses, and the exact words it refuses with — so the Worker composition
//! root wires a vocabulary rather than deciding one, and so a control
//! command added to the wire fails a test until somebody places it here.
//!
//! Three commitments, each held by a test:
//!
//! - **Nothing is unclassified.** [`cmd::disposition`] places every control
//!   command; the root crate's `browser_control_vocabulary` test holds the
//!   set equal to the wire enum, both directions — the `mcp::ShellTool`
//!   pattern, one layer up.
//! - **No fabricated readings.** The spaces reply is a [`reply::ServedSpaceRow`],
//!   a shape with no `path`, no `origin`, no probe `status` and no probe
//!   `unnamed` taxonomy — "this backend serves this Space" is a construction
//!   fact stated by the reply's existence, not a reading it pretends to have
//!   taken. The daemon's `SpaceRow` is deliberately unrepresentable here.
//! - **Refusals tell the truth.** A daemon-only command is refused as *the
//!   daemon's act by nature*; an unimplemented reading promises nothing; and
//!   no refusal ever wears the native head's wrong-mount refusal, which the
//!   viewer replays against ([`refuse`] holds the contract).

pub mod answer;
pub mod cmd;
pub mod refuse;
pub mod reply;

pub use cmd::{disposition, Disposition};
pub use refuse::Refusal;
