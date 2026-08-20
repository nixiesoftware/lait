//! Durable home for the kinship registry and the epoch that orders its
//! publications.
//!
//! Both or neither. A registry restored without its epoch would republish at a
//! number it has already used, and [`Registry::absorb`](crate::Registry::absorb)
//! accepts a publication only when its epoch is at least the held one — so a
//! reset counter makes every subsequent announcement silently ignored by every
//! correspondent, while still reporting success. Persisting the pair is the
//! whole point of this file.
//!
//! Envelope shape follows [`crate::store`]: magic, format byte, length header,
//! blake3 trailer, atomic replace with a `.bak`. Unknown formats and corrupt
//! bytes fail closed and leave the previous file alone; nothing is invented to
//! stand in for material that could not be read.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::durable::{atomic_replace, open_or_recover};
use crate::{Error, Registry};

const MAGIC: &[u8; 8] = b"LAITKIN1";
const ENVELOPE_FORMAT: u8 = 1;
const PREFIX: usize = 8 + 1 + 4;

/// The envelope, read from disk. Bounded like every other read in this crate:
/// a file that grew past this is corrupt, not something to load.
const MAX_REACH_BYTES: usize = 8 * 1024 * 1024;

/// The whole reach state, as one durable value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReachState {
    /// The last epoch this identity published at. Never moves backwards.
    pub epoch: u64,
    /// Which of this identity's device seeds composes and avows, by index.
    pub canonical: usize,
    /// The kinship registry: this identity's own log, and every correspondent
    /// profile it has absorbed.
    pub registry: Registry,
    /// What this identity has sent, keyed by the address it was sent to.
    ///
    /// Here because the carrier drops a letter once its recipient acknowledges,
    /// so the sender's copy is the only durable one there will ever be. Without
    /// it, acknowledging on collect would silently destroy half of every
    /// conversation.
    #[serde(default)]
    pub sent: std::collections::BTreeMap<String, Vec<Sent>>,
    /// The short address a directory issued this profile, once one has.
    ///
    /// Held here rather than re-asked, because it is *"minted on first publish
    /// and stable afterwards"* and a person hands it out by voice. A profile
    /// that had to ask a service what its own address was would be a profile
    /// with no address whenever that service was unreachable.
    #[serde(default)]
    pub address: Option<String>,
}

/// One letter this identity composed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sent {
    pub at: u64,
    pub body: String,
    /// Whether it was an invitation rather than a message. Kept so a transcript
    /// redraws what it was, not what it looked like.
    #[serde(default)]
    pub invitation: bool,
}

/// Where reach state lives for one identity.
pub struct ReachStore {
    path: PathBuf,
}

impl ReachStore {
    #[must_use]
    pub fn at(identity_dir: &Path) -> Self {
        Self {
            path: identity_dir.join("kinship.bin"),
        }
    }

    /// Read what is there, or `None` when this identity has never published.
    ///
    /// A corrupt or future-format file is an error, never an empty state: a
    /// silent default here would re-found the profile and change the address a
    /// person has already handed out.
    pub fn load(&self) -> Result<Option<ReachState>, Error> {
        open_or_recover(&self.path, |path| {
            let bytes = fs::read(path)?;
            if bytes.len() > MAX_REACH_BYTES {
                return Err(Error::Bound("reach envelope bytes"));
            }
            decode(&bytes)
        })
    }

    /// Replace what is there. Refuses an epoch below what is already on disk.
    pub fn save(&self, state: &ReachState) -> Result<(), Error> {
        if let Some(held) = self.load()? {
            if state.epoch < held.epoch {
                return Err(Error::Invalid("epoch moved backwards"));
            }
        }
        atomic_replace(&self.path, &encode(state)?)
    }
}

fn encode(state: &ReachState) -> Result<Vec<u8>, Error> {
    let body = postcard::to_stdvec(state).map_err(|_| Error::Invalid("reach encode"))?;
    let body_len = u32::try_from(body.len()).map_err(|_| Error::Bound("reach body"))?;
    let mut out = Vec::with_capacity(PREFIX.saturating_add(body.len()).saturating_add(32));
    out.extend_from_slice(MAGIC);
    out.push(ENVELOPE_FORMAT);
    out.extend_from_slice(&body_len.to_le_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(blake3::hash(&out).as_bytes());
    Ok(out)
}

fn decode(bytes: &[u8]) -> Result<ReachState, Error> {
    if bytes.len() < PREFIX + 32 {
        return Err(Error::Corrupt("truncated reach envelope"));
    }
    if bytes.get(..8) != Some(MAGIC.as_slice()) {
        return Err(Error::Corrupt("reach magic"));
    }
    let format = *bytes.get(8).ok_or(Error::Corrupt("reach format"))?;
    if format != ENVELOPE_FORMAT {
        return Err(Error::UnsupportedVersion(format));
    }
    let len_bytes: [u8; 4] = bytes
        .get(9..13)
        .and_then(|s| s.try_into().ok())
        .ok_or(Error::Corrupt("reach length"))?;
    let body_len = usize::try_from(u32::from_le_bytes(len_bytes))
        .map_err(|_| Error::Corrupt("reach length"))?;
    let body_end = PREFIX
        .checked_add(body_len)
        .ok_or(Error::Corrupt("reach length"))?;
    if bytes.len() != body_end.saturating_add(32) {
        return Err(Error::Corrupt("reach length disagrees with the file"));
    }
    let expected: [u8; 32] = bytes
        .get(body_end..)
        .and_then(|tail| tail.try_into().ok())
        .ok_or(Error::Corrupt("reach digest"))?;
    let signed = bytes
        .get(..body_end)
        .ok_or(Error::Corrupt("reach digest"))?;
    if blake3::hash(signed) != expected {
        return Err(Error::Corrupt("reach digest"));
    }
    let body = bytes
        .get(PREFIX..body_end)
        .ok_or(Error::Corrupt("reach body"))?;
    postcard::from_bytes(body).map_err(|_| Error::Corrupt("reach decode"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mechanics::kinship::DeviceLink;

    fn state(epoch: u64) -> ReachState {
        let genesis = DeviceLink::seal(&[51u8; 32], &[52u8; 32], [7u8; 16], 1).expect("genesis");
        let mut registry = Registry::new();
        registry.found(genesis).expect("found");
        ReachState {
            epoch,
            canonical: 0,
            registry,
            sent: std::collections::BTreeMap::new(),
            address: None,
        }
    }

    fn dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kin-store-{}-{tag}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reach_state_survives_a_round_trip_and_an_absent_file_is_not_an_error() {
        let home = dir("round");
        let store = ReachStore::at(&home);
        assert!(store.load().expect("absent is not corrupt").is_none());

        let written = state(7);
        store.save(&written).expect("save");
        let read = store.load().expect("load").expect("present");
        assert_eq!(read.epoch, 7);
        assert_eq!(read.canonical, 0);
        assert_eq!(
            read.registry.to_bytes().unwrap(),
            written.registry.to_bytes().unwrap()
        );
        let _ = fs::remove_dir_all(&home);
    }

    /// The defect this file exists for: a restart that forgot the epoch would
    /// republish at a number correspondents have already seen, and every one of
    /// them would ignore it while reporting success.
    #[test]
    fn an_epoch_that_moved_backwards_is_refused_rather_than_written() {
        let home = dir("epoch");
        let store = ReachStore::at(&home);
        store.save(&state(9)).expect("save");
        assert!(matches!(
            store.save(&state(8)),
            Err(Error::Invalid("epoch moved backwards"))
        ));
        assert_eq!(
            store.load().unwrap().unwrap().epoch,
            9,
            "the held one stands"
        );
        store
            .save(&state(9))
            .expect("republishing at the same epoch is fine");
        let _ = fs::remove_dir_all(&home);
    }

    /// The window `atomic_replace` opens on purpose, and the reason the recovery
    /// has to ship with it: a crash between the remove and the rename leaves a
    /// fully-synced `.tmp` and no main file. Read as absence, that re-founds the
    /// profile and drops the epoch to 1 — which is the exact defect this module
    /// exists to prevent, arriving through the file that prevents it.
    #[test]
    fn a_crash_between_the_remove_and_the_rename_recovers_the_survivor() {
        let home = dir("crash");
        let store = ReachStore::at(&home);
        store.save(&state(11)).expect("save");

        // Stage the interrupted swap by hand: the survivor is written and
        // synced, the main file is gone, the rename never happened.
        let survivor = fs::read(&store.path).unwrap();
        fs::write(store.path.with_extension("bin.tmp"), &survivor).unwrap();
        fs::remove_file(&store.path).unwrap();

        let read = store
            .load()
            .expect("a survivor is not absence")
            .expect("present");
        assert_eq!(read.epoch, 11, "the epoch survived the crash");
        assert!(store.path.exists(), "and was put back in place");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn a_corrupt_file_fails_closed_rather_than_re_founding_the_profile() {
        let home = dir("corrupt");
        let store = ReachStore::at(&home);
        store.save(&state(3)).expect("save");

        let mut bytes = fs::read(&store.path).unwrap();
        let last = bytes.len().saturating_sub(1);
        bytes[last] ^= 0xff;
        fs::write(&store.path, &bytes).unwrap();
        assert!(matches!(store.load(), Err(Error::Corrupt(_))));

        bytes[8] = 99;
        fs::write(&store.path, &bytes).unwrap();
        assert!(matches!(store.load(), Err(Error::UnsupportedVersion(99))));
        let _ = fs::remove_dir_all(&home);
    }
}
