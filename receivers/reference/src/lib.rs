//! The reference receiver's library face: exactly the shell.
//!
//! The receiver itself is a binary on purpose — an executable oracle, not a
//! surface. The shell's planner and reconciler live behind this lib target
//! only so the `astrolabe-display-shell` binary and the tests share one
//! implementation; nothing else is exported, and nothing else should be.

pub mod shell;
