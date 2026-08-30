//! What a person has said one of their devices must not hold.
//!
//! Everything is held everywhere by default: a Space a person enters on one
//! device appears on every device they own, and nothing is opted into. This
//! file is the one exception a person can write — *not this Space, not on
//! that machine* — and it is a decision, not a policy engine: one record per
//! (device, Space), and a pure [`ReplicaPolicy::admits`] that every side asks
//! before it offers or consents.
//!
//! It lives on both sides of the decision on purpose, and the two records
//! answer different questions. On the holder it says **stop offering**; on
//! the device it is about it says **refuse the offer**, whoever makes it. A
//! device that only trusted the holder to stop asking would take the Space
//! back from the next device of the profile that offered it, and one where
//! only the holder remembered would take it back after the holder restarted.
//!
//! `told` is the third thing, and the reason a lifted exclusion is written
//! down at all: the machine a decision is about may be off when it is made.
//! Until it has heard, the decision is owed to it — through a restart of
//! either side — and the loop carries it on the ordinary retry. An exclusion
//! nobody carried would be a Space the device goes on refusing while the
//! person who lifted it watches a row that says it holds it.
//!
//! Removal is never deletion. Excluding a Space forgets its registration and
//! vacates its Orbit; the store's bytes stay exactly where they are, to be
//! deleted where deletion lives if a person means that instead.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mechanics::ids::DeviceId;
use runtime::poison::LockRecovering;
use serde::{Deserialize, Serialize};

/// Held across every read-modify-write of the file.
///
/// Two host requests about different pairs are ordinary — a person clicking
/// down a list — and both are load, edit, store. Without this the second
/// read happens before the first write and one decision is lost with nothing
/// said about it. Process-wide because the file is: one identity home, one
/// daemon.
static WRITING: Mutex<()> = Mutex::new(());

/// One decision about one Space on one device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Exclusion {
    pub device: DeviceId,
    pub space: String,
    /// What was decided. `false` is an exclusion being lifted, kept only
    /// until the device it is about has been told — after that there is
    /// nothing left to remember and the record goes.
    pub excluded: bool,
    /// Whether the device it is about has heard this. A decision made while
    /// that machine was off is owed to it, and the loop keeps carrying it.
    pub told: bool,
}

/// The exclusions this device holds, as `<identity>/replica.json`.
///
/// Keyed by Space id rather than by store path, so an exclusion survives the
/// store being removed and the Space being entered again — which is exactly
/// what excluding does to it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReplicaPolicy {
    #[serde(default)]
    excluded: Vec<Exclusion>,
}

fn file(identity: &Path) -> PathBuf {
    identity.join("replica.json")
}

impl ReplicaPolicy {
    /// What this device has been told to hold back. An unreadable or absent
    /// file is no exclusions — the default is to hold everything, so failing
    /// open here is failing to the design rather than around it, and a file
    /// that will not read says so in the journal.
    pub(crate) fn load(identity: &Path) -> Self {
        match addressbook::durable::open_or_recover(&file(identity), |path| {
            let bytes = std::fs::read(path)?;
            serde_json::from_slice::<Self>(&bytes)
                .map_err(|_| addressbook::Error::Corrupt("replica policy"))
        }) {
            Ok(policy) => policy.unwrap_or_default(),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "the exclusions file did not read; this device holds everything it is offered"
                );
                Self::default()
            }
        }
    }

    /// Whether `space` may be held on `device`, as this file has it.
    ///
    /// Pure, and asked from both sides: the holder before it offers, and the
    /// offered device before it consents.
    pub(crate) fn admits(&self, device: &DeviceId, space: &str) -> bool {
        !self
            .excluded
            .iter()
            .any(|row| row.excluded && row.device == *device && row.space == space)
    }

    /// The decisions the device they are about has not heard yet.
    pub(crate) fn untold(&self) -> Vec<Exclusion> {
        self.excluded
            .iter()
            .filter(|row| !row.told)
            .cloned()
            .collect()
    }

    /// Every standing exclusion, as the view draws it. A lift waiting to be
    /// carried is not one: the Space is admitted again here, and the row it
    /// leaves is about delivery rather than about holding.
    pub(crate) fn decided(&self) -> Vec<Exclusion> {
        self.excluded
            .iter()
            .filter(|row| row.excluded)
            .cloned()
            .collect()
    }

    /// Whether this pair is excluded, and whether the device has heard.
    ///
    /// This is the *only* home of that bit. The fan-out's standings are one
    /// process's memory of asking and are empty after a restart; a decision a
    /// person made is not, and a surface reading it from anywhere else would
    /// draw an excluded Space as one nothing has offered yet — which is the
    /// fold the whole file exists to prevent.
    pub(crate) fn decided_for(&self, device: &DeviceId, space: &str) -> Option<bool> {
        self.excluded
            .iter()
            .find(|row| row.excluded && row.device == *device && row.space == space)
            .map(|row| row.told)
    }

    /// Write one decision down, replacing whatever stood for that pair.
    ///
    /// A lift the device has heard leaves nothing behind: there is no such
    /// thing as a recorded permission here, only a recorded refusal and a
    /// refusal on its way to being lifted.
    pub(crate) fn decide(
        identity: &Path,
        device: &DeviceId,
        space: &str,
        excluded: bool,
        told: bool,
    ) -> Result<Self, String> {
        let _held = WRITING.lock_recovering();
        let mut policy = Self::load(identity);
        policy
            .excluded
            .retain(|row| row.device != *device || row.space != space);
        if excluded || !told {
            policy.excluded.push(Exclusion {
                device: device.clone(),
                space: space.to_string(),
                excluded,
                told,
            });
        }
        policy
            .excluded
            .sort_by(|one, two| (&one.device, &one.space).cmp(&(&two.device, &two.space)));
        let bytes = serde_json::to_vec_pretty(&policy)
            .map_err(|error| format!("the exclusions did not encode: {error}"))?;
        addressbook::durable::atomic_replace(&file(identity), &bytes)
            .map_err(|error| format!("the exclusions could not be written: {error}"))?;
        Ok(policy)
    }

    /// Note that the device has heard the decision *this* caller carried.
    ///
    /// `expected` is the decision that was put on the wire. Carrying one takes
    /// a round trip, and a person can change their mind inside it: writing
    /// "told" against whatever the file says now would silently discard the
    /// second decision and leave the device holding the answer to the first,
    /// with nobody told and nothing to retry. A row that has moved is left
    /// exactly as it is, and the loop carries it next.
    pub(crate) fn decide_told(
        identity: &Path,
        device: &DeviceId,
        space: &str,
        expected: bool,
    ) -> Result<(), String> {
        let _held = WRITING.lock_recovering();
        let mut policy = Self::load(identity);
        let Some(row) = policy
            .excluded
            .iter_mut()
            .find(|row| row.device == *device && row.space == space)
        else {
            return Ok(());
        };
        if row.excluded != expected {
            return Ok(());
        }
        row.told = true;
        // A lift the device has heard leaves nothing behind: there is no such
        // thing as a recorded permission here.
        policy.excluded.retain(|row| row.excluded || !row.told);
        let bytes = serde_json::to_vec_pretty(&policy)
            .map_err(|error| format!("the exclusions did not encode: {error}"))?;
        addressbook::durable::atomic_replace(&file(identity), &bytes)
            .map_err(|error| format!("the exclusions could not be written: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mechanics::actor::device_from_seed;

    fn home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lait-replica-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("home");
        dir
    }

    /// Everything is held until somebody says otherwise, and what they said
    /// outlives the process that heard it — which is the whole reason this is
    /// a file rather than a standing in memory. A lift stays written down
    /// until the device it is about has heard it, and then leaves nothing:
    /// there is no recorded permission here, only a recorded refusal.
    #[test]
    fn an_exclusion_is_written_down_per_device_and_survives_the_process() {
        let home = home("decide");
        let pi = device_from_seed(&[61; 32]);
        let laptop = device_from_seed(&[62; 32]);

        // Nothing written: everything is admitted, and the file need not exist.
        let policy = ReplicaPolicy::load(&home);
        assert!(policy.admits(&pi, "ws_one"));
        assert!(policy.untold().is_empty());

        let policy = ReplicaPolicy::decide(&home, &pi, "ws_one", true, false).expect("decide");
        assert!(!policy.admits(&pi, "ws_one"));
        // One device, one Space: neither the other machine nor the other
        // Space is touched by a decision about this pair.
        assert!(policy.admits(&laptop, "ws_one"));
        assert!(policy.admits(&pi, "ws_two"));
        // Owed to the device it is about until it has heard.
        assert_eq!(policy.untold().len(), 1);
        assert!(ReplicaPolicy::load(&home).untold().len() == 1);

        let policy = ReplicaPolicy::decide(&home, &pi, "ws_one", true, true).expect("told");
        assert!(!policy.admits(&pi, "ws_one"));
        assert!(policy.untold().is_empty());
        // Read back from disk by a process that never saw the decision.
        assert!(!ReplicaPolicy::load(&home).admits(&pi, "ws_one"));

        // Lifting it: still written down, because the device may be off, and
        // a lift nobody carried is a Space it goes on refusing.
        let policy = ReplicaPolicy::decide(&home, &pi, "ws_one", false, false).expect("lift");
        assert!(policy.admits(&pi, "ws_one"), "a lift admits at once here");
        assert_eq!(policy.untold().len(), 1, "and is owed to the device");
        let policy = ReplicaPolicy::decide(&home, &pi, "ws_one", false, true).expect("carried");
        assert!(policy.excluded.is_empty(), "a carried lift leaves nothing");
        assert!(ReplicaPolicy::load(&home).admits(&pi, "ws_one"));

        let _ = std::fs::remove_dir_all(&home);
    }

    /// A person can change their mind while a decision is on the wire.
    /// Marking "told" against whatever the file says *now* would discard the
    /// second decision silently — the device would be holding the answer to
    /// the first, nobody would be told, and nothing would be left to carry.
    /// So a carry only settles the decision it actually carried.
    #[test]
    fn a_decision_that_changed_inside_the_round_trip_is_not_marked_told() {
        let home = home("told");
        let pi = device_from_seed(&[63; 32]);

        ReplicaPolicy::decide(&home, &pi, "ws_one", true, false).expect("exclude");
        // The lift lands while the exclusion is still being carried.
        ReplicaPolicy::decide(&home, &pi, "ws_one", false, false).expect("lift");
        ReplicaPolicy::decide_told(&home, &pi, "ws_one", true).expect("the stale carry returns");
        let policy = ReplicaPolicy::load(&home);
        assert!(
            policy.admits(&pi, "ws_one"),
            "a carry that finished after the person changed their mind overwrote it"
        );
        assert_eq!(
            policy.untold().len(),
            1,
            "and the lift is still owed to the device"
        );

        // The lift's own carry settles it, and leaves nothing behind.
        ReplicaPolicy::decide_told(&home, &pi, "ws_one", false).expect("carried");
        assert!(ReplicaPolicy::load(&home).untold().is_empty());
        assert!(ReplicaPolicy::load(&home).decided().is_empty());

        // Nothing recorded for a pair is not an error to settle: a decision
        // may have been lifted and carried before this carry came back.
        ReplicaPolicy::decide_told(&home, &pi, "ws_two", true).expect("no row is not a failure");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The exclusion a surface draws comes from here, whichever process is
    /// asking. `decided_for` is the only home of "is this excluded, and has
    /// the device heard" — a second copy in the fan-out's memory would be
    /// empty after a restart and read as a Space nothing had offered yet.
    #[test]
    fn what_a_surface_draws_is_read_from_the_file_and_only_from_the_file() {
        let home = home("drawn");
        let pi = device_from_seed(&[64; 32]);
        let laptop = device_from_seed(&[65; 32]);

        ReplicaPolicy::decide(&home, &pi, "ws_one", true, false).expect("exclude");
        let fresh = ReplicaPolicy::load(&home);
        assert_eq!(fresh.decided_for(&pi, "ws_one"), Some(false));
        assert_eq!(fresh.decided_for(&laptop, "ws_one"), None);
        assert_eq!(fresh.decided_for(&pi, "ws_two"), None);
        assert_eq!(
            fresh.decided().len(),
            1,
            "the Space is a row even though nothing in this process has asked about it"
        );

        ReplicaPolicy::decide_told(&home, &pi, "ws_one", true).expect("carried");
        assert_eq!(
            ReplicaPolicy::load(&home).decided_for(&pi, "ws_one"),
            Some(true)
        );

        // A lift waiting to be carried is not an exclusion to draw: the Space
        // is admitted here again, and the row that remains is about delivery.
        ReplicaPolicy::decide(&home, &pi, "ws_one", false, false).expect("lift");
        let lifting = ReplicaPolicy::load(&home);
        assert_eq!(lifting.decided_for(&pi, "ws_one"), None);
        assert!(lifting.decided().is_empty());
        assert_eq!(lifting.untold().len(), 1);
        let _ = std::fs::remove_dir_all(&home);
    }
}
