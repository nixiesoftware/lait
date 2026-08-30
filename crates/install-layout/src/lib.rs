//! The names in an installed client's root, spelled once.
//!
//! Two processes act on this layout and neither may depend on the other: the
//! **stub** swaps trees by these names at a launch, and the **daemon**
//! recognises an installation by them and stages into it. The stub must stay
//! free of the engine, so for a long time the answer was to spell the layout
//! twice and assert the two agreed in a test — which held for the three names
//! somebody remembered to add and silently did not for the rest.
//!
//! A dependency-free leaf crate costs the stub nothing and removes the
//! category: there is one spelling, so there is nothing to drift.
//!
//! ```text
//! astrolabe(.exe)          the stub, under the APPLICATION's installed name —
//!                          the one file an update never moves
//! current/                 the live tree: the astrolabe+lait pair, flat
//! previous/                the prior live tree, kept as the rollback target
//! staged/                  a downloaded tree waiting to become current
//! staged.manifest.json     what staged/ must hash to
//! instance.lock            held while a client is alive here
//! staging.lock             held while staged/ is written or consumed
//! steward.lock             held by the daemon that stages for this root
//! steward-v1               who holds it, for a client that wants to say so
//! launched-from            which tree the stub last started
//! stub.log                 every named refusal and recovery
//! ```

/// The live tree's directory name.
pub const CURRENT_DIR: &str = "current";

/// The rollback tree's directory name.
///
/// The stub starts this tree when the live one will not run, and a daemon
/// started from it is still this installation's resident updater — running, in
/// fact, on the machine that most needs the next release.
pub const PREVIOUS_DIR: &str = "previous";

/// A downloaded tree waiting for a launch to accept it. Never a tree anything
/// runs from: the stub swaps it into [`CURRENT_DIR`] first.
pub const STAGED_DIR: &str = "staged";

/// What [`STAGED_DIR`] must hash to, written by the stager and re-proved by
/// the stub before every swap.
pub const STAGE_MANIFEST: &str = "staged.manifest.json";

/// Held while a client is alive in this root.
pub const INSTANCE_LOCK: &str = "instance.lock";

/// Held while `staged/` is being written or consumed. A different fact from
/// [`INSTANCE_LOCK`], and therefore a different file: staging must keep
/// working under a live client, and consuming a stage must not race it.
pub const STAGING_LOCK: &str = "staging.lock";

/// Held for its lifetime by the one daemon that stages for this root.
///
/// The authority to update an installation is a **held lock naming its
/// holder**, not a shape inferred from an executable path. A path can only
/// locate this file; taking it is what grants the right to act, and a second
/// daemon that cannot take it stages nothing and can say who did.
pub const STEWARD_LOCK: &str = "steward.lock";

/// Who holds [`STEWARD_LOCK`], readable by a client that wants to say so.
///
/// Beside the lock rather than inside it: Windows locks are mandatory, so a
/// reader would fail there and pass on unix — the same reason the daemon's pid
/// does not live inside `daemon.lock`.
pub const STEWARD_RECORD: &str = "steward-v1";

/// Which tree the stub last started, written *before* the child is spawned.
///
/// The swap needs this. When the stub falls back to [`PREVIOUS_DIR`] because
/// the live tree would not run, the next swap must discard the tree that
/// failed and leave the one that works — and without a record of which tree
/// was actually launched, the swap has no way to tell that case from an
/// ordinary one, and rotates the last known-good tree out of existence.
pub const LAUNCHED_FROM: &str = "launched-from";

/// Every named refusal and recovery, appended.
pub const STUB_LOG: &str = "stub.log";

/// Written by the canonical installer; its absence means an installation that
/// cannot cross the independent-World compatibility boundary in place.
pub const CANONICAL_LAYOUT: &str = "canonical-layout-v1";

/// A tree in an install root that something can be started from.
///
/// [`STAGED_DIR`] is deliberately absent: it is bytes waiting for a swap, and
/// a daemon claiming to steward an installation from a tree that was never
/// launched would be claiming it from bytes nothing has accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tree {
    /// The live tree.
    Current,
    /// The rollback tree, started because the live one would not run.
    Previous,
}

impl Tree {
    /// The directory name this tree lives under.
    pub fn dir(self) -> &'static str {
        match self {
            Self::Current => CURRENT_DIR,
            Self::Previous => PREVIOUS_DIR,
        }
    }

    /// The tree a directory name denotes, when it denotes one.
    pub fn from_dir(name: &str) -> Option<Self> {
        match name {
            CURRENT_DIR => Some(Self::Current),
            PREVIOUS_DIR => Some(Self::Previous),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names are the contract two processes read. Pinned as literals
    /// because that is what an installed machine on disk holds: changing one
    /// is changing the layout of every installation already out there, and it
    /// should take deleting an assertion to do it by accident.
    #[test]
    fn the_layout_names_are_the_contract_and_are_pinned_as_literals() {
        assert_eq!(CURRENT_DIR, "current");
        assert_eq!(PREVIOUS_DIR, "previous");
        assert_eq!(STAGED_DIR, "staged");
        assert_eq!(STAGE_MANIFEST, "staged.manifest.json");
        assert_eq!(INSTANCE_LOCK, "instance.lock");
        assert_eq!(STAGING_LOCK, "staging.lock");
        assert_eq!(STEWARD_LOCK, "steward.lock");
        assert_eq!(STEWARD_RECORD, "steward-v1");
        assert_eq!(LAUNCHED_FROM, "launched-from");
        assert_eq!(STUB_LOG, "stub.log");
        assert_eq!(CANONICAL_LAYOUT, "canonical-layout-v1");
    }

    /// A tree is a directory something ran from. `staged/` is not one, and the
    /// round trip must not quietly acquire it.
    #[test]
    fn only_a_tree_something_runs_from_round_trips() {
        assert_eq!(Tree::from_dir(CURRENT_DIR), Some(Tree::Current));
        assert_eq!(Tree::from_dir(PREVIOUS_DIR), Some(Tree::Previous));
        assert_eq!(Tree::from_dir(STAGED_DIR), None);
        assert_eq!(Tree::from_dir("anything"), None);
        assert_eq!(Tree::Current.dir(), CURRENT_DIR);
        assert_eq!(Tree::Previous.dir(), PREVIOUS_DIR);
    }
}
