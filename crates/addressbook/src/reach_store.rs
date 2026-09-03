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

use mechanics::ids::DeviceId;
use mechanics::kinship::DeviceLink;

use crate::durable::{atomic_replace, open_or_recover};
use crate::{Error, Registry};

const MAGIC: &[u8; 8] = b"LAITKIN1";
const LEGACY_ENVELOPE_FORMAT: u8 = 1;
const ENVELOPE_FORMAT: u8 = 2;
const PREFIX: usize = 8 + 1 + 4;

/// The envelope, read from disk. Bounded like every other read in this crate:
/// a file that grew past this is corrupt, not something to load.
const MAX_REACH_BYTES: usize = 8 * 1024 * 1024;
/// One device cannot retain an unbounded number of deposit identities in its
/// local inbox. The envelope byte bound remains the tighter limit for ordinary
/// letters; this count also bounds maps made of tiny adversarial entries.
const MAX_RECEIVED: usize = 8192;
/// Deposit ids are carrier-issued input. Current ids are 64 hexadecimal bytes;
/// leave room for another construction without admitting an unbounded map key.
const MAX_DEPOSIT_ID_BYTES: usize = 256;
/// An opened letter came from a sealed envelope bounded to 256 KiB. Its durable
/// encoding must never be larger than the envelope it arrived in.
const MAX_OPENED_LETTER_BYTES: usize = 256 * 1024;

/// The whole reach state, as one durable value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReachState {
    /// The last epoch this identity published at. Never moves backwards.
    pub epoch: u64,
    /// Which seed composed, by index, when a home held several. Kept on disk
    /// for the envelope's shape and ignored on read: a machine holds one seed.
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
    /// The genesis link this profile is the content address of. Carried, never
    /// derived: a profile is founded once and every later device joins by
    /// link, so nothing that could recompute it from a seed exists any more.
    /// `None` is a pre-carriage envelope, and the boot path decides what to do
    /// with one — a plane never founds in its place.
    #[serde(default)]
    pub genesis: Option<DeviceLink>,
    /// How this device came to hold the profile.
    #[serde(default)]
    pub origin: Origin,
    /// Opened, verified correspondence keyed by the carrier's stable deposit id.
    ///
    /// Stored as opaque letter bytes because this leaf crate must not depend on
    /// `correspondence`. That crate decodes and re-verifies them while restoring
    /// its mailbox; this layer owns only bounded, crash-safe carriage.
    #[serde(default)]
    pub received: std::collections::BTreeMap<String, Received>,
}

/// The durable half of one opened correspondence letter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Received {
    /// The encoded, signed `correspondence::Letter`.
    pub letter: Vec<u8>,
    /// What the carrier said about who deposited it. Corroboration, not proof.
    pub deposited_by: DeviceId,
    /// When the carrier observed the deposit.
    pub arrived_at: u64,
}

/// How this device came to hold its profile: founded here, or adopted from a
/// device that already held it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum Origin {
    #[default]
    Founded,
    Adopted {
        from: DeviceId,
        at: u64,
    },
}

/// The envelope body as it was written before the genesis was carried.
///
/// Postcard is positional and not self-describing, so `#[serde(default)]`
/// cannot supply a trailing field the bytes never held; a body that ends after
/// `address` is read in this shape and lifted with `genesis: None`.
#[derive(Serialize, Deserialize)]
struct PreCarriage {
    epoch: u64,
    canonical: usize,
    registry: Registry,
    #[serde(default)]
    sent: std::collections::BTreeMap<String, Vec<Sent>>,
    #[serde(default)]
    address: Option<String>,
}

/// Format-one state after genesis carriage and before the inbox was durable.
///
/// Kept as an exact old shape rather than relying on `serde(default)`: postcard
/// is positional, and an absent trailing field is an unexpected end of input.
#[derive(Serialize, Deserialize)]
struct PreInbox {
    epoch: u64,
    canonical: usize,
    registry: Registry,
    sent: std::collections::BTreeMap<String, Vec<Sent>>,
    address: Option<String>,
    genesis: Option<DeviceLink>,
    origin: Origin,
}

impl From<PreInbox> for ReachState {
    fn from(held: PreInbox) -> Self {
        Self {
            epoch: held.epoch,
            canonical: held.canonical,
            registry: held.registry,
            sent: held.sent,
            address: held.address,
            genesis: held.genesis,
            origin: held.origin,
            received: std::collections::BTreeMap::new(),
        }
    }
}

impl From<PreCarriage> for ReachState {
    fn from(held: PreCarriage) -> Self {
        Self {
            epoch: held.epoch,
            canonical: held.canonical,
            registry: held.registry,
            sent: held.sent,
            address: held.address,
            genesis: None,
            origin: Origin::Founded,
            received: std::collections::BTreeMap::new(),
        }
    }
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
        validate(state)?;
        if let Some(held) = self.load()? {
            if state.epoch < held.epoch {
                return Err(Error::Invalid("epoch moved backwards"));
            }
        }
        atomic_replace(&self.path, &encode(state)?)
    }
}

fn encode(state: &ReachState) -> Result<Vec<u8>, Error> {
    validate(state)?;
    let body = postcard::to_stdvec(state).map_err(|_| Error::Invalid("reach encode"))?;
    let body_len = u32::try_from(body.len()).map_err(|_| Error::Bound("reach body"))?;
    let mut out = Vec::with_capacity(PREFIX.saturating_add(body.len()).saturating_add(32));
    out.extend_from_slice(MAGIC);
    out.push(ENVELOPE_FORMAT);
    out.extend_from_slice(&body_len.to_le_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(blake3::hash(&out).as_bytes());
    if out.len() > MAX_REACH_BYTES {
        return Err(Error::Bound("reach envelope bytes"));
    }
    Ok(out)
}

fn validate(state: &ReachState) -> Result<(), Error> {
    if state.received.len() > MAX_RECEIVED {
        return Err(Error::Bound("received letters"));
    }
    for (id, received) in &state.received {
        if id.is_empty() || id.len() > MAX_DEPOSIT_ID_BYTES {
            return Err(Error::Bound("deposit id bytes"));
        }
        if received.letter.is_empty() || received.letter.len() > MAX_OPENED_LETTER_BYTES {
            return Err(Error::Bound("opened letter bytes"));
        }
    }
    Ok(())
}

fn decode(bytes: &[u8]) -> Result<ReachState, Error> {
    if bytes.len() < PREFIX + 32 {
        return Err(Error::Corrupt("truncated reach envelope"));
    }
    if bytes.get(..8) != Some(MAGIC.as_slice()) {
        return Err(Error::Corrupt("reach magic"));
    }
    let format = *bytes.get(8).ok_or(Error::Corrupt("reach format"))?;
    if !matches!(format, LEGACY_ENVELOPE_FORMAT | ENVELOPE_FORMAT) {
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
    let state = match format {
        ENVELOPE_FORMAT => {
            postcard::from_bytes::<ReachState>(body).map_err(|_| Error::Corrupt("reach decode"))?
        }
        LEGACY_ENVELOPE_FORMAT => postcard::from_bytes::<PreInbox>(body)
            .map(ReachState::from)
            .or_else(|_| postcard::from_bytes::<PreCarriage>(body).map(ReachState::from))
            .map_err(|_| Error::Corrupt("reach decode"))?,
        _ => return Err(Error::UnsupportedVersion(format)),
    };
    validate(&state)?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mechanics::kinship::DeviceLink;

    fn state(epoch: u64) -> ReachState {
        let genesis = DeviceLink::seal(&[51u8; 32], &[52u8; 32], [7u8; 16], 1).expect("genesis");
        let mut registry = Registry::new();
        registry.found(genesis.clone()).expect("found");
        ReachState {
            epoch,
            canonical: 0,
            registry,
            sent: std::collections::BTreeMap::new(),
            address: None,
            genesis: Some(genesis),
            origin: Origin::Founded,
            received: std::collections::BTreeMap::new(),
        }
    }

    fn dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kin-store-{}-{tag}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn envelope(format: u8, body: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.push(format);
        bytes.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(blake3::hash(&bytes).as_bytes());
        bytes
    }

    #[test]
    fn reach_state_survives_a_round_trip_and_an_absent_file_is_not_an_error() {
        let home = dir("round");
        let store = ReachStore::at(&home);
        assert!(store.load().expect("absent is not corrupt").is_none());

        let mut written = state(7);
        written.received.insert(
            "deposit-7".into(),
            Received {
                letter: vec![1, 2, 3],
                deposited_by: mechanics::actor::device_from_seed(&[70u8; 32]),
                arrived_at: 70,
            },
        );
        store.save(&written).expect("save");
        let read = store.load().expect("load").expect("present");
        assert_eq!(read.epoch, 7);
        assert_eq!(read.canonical, 0);
        assert_eq!(
            read.registry.to_bytes().unwrap(),
            written.registry.to_bytes().unwrap()
        );
        assert_eq!(
            read.genesis, written.genesis,
            "the genesis is carried whole"
        );
        assert_eq!(read.received, written.received, "the opened inbox survived");
        let _ = fs::remove_dir_all(&home);
    }

    /// The immediately preceding on-disk shape carried genesis and origin but
    /// no inbox. It remains a valid identity and starts with an empty mailbox.
    #[test]
    fn a_format_one_pre_inbox_envelope_loads_with_an_empty_inbox() {
        let home = dir("pre-inbox");
        let store = ReachStore::at(&home);
        let legacy = state(6);
        let body = postcard::to_stdvec(&PreInbox {
            epoch: legacy.epoch,
            canonical: legacy.canonical,
            registry: legacy.registry.clone(),
            sent: legacy.sent.clone(),
            address: Some("alice.example".into()),
            genesis: legacy.genesis.clone(),
            origin: Origin::Adopted {
                from: mechanics::actor::device_from_seed(&[71u8; 32]),
                at: 17,
            },
        })
        .unwrap();
        fs::write(&store.path, envelope(LEGACY_ENVELOPE_FORMAT, &body)).unwrap();

        let read = store.load().unwrap().unwrap();
        assert_eq!(read.genesis, legacy.genesis);
        assert_eq!(read.address.as_deref(), Some("alice.example"));
        assert!(read.received.is_empty());
        assert!(matches!(read.origin, Origin::Adopted { at: 17, .. }));
        let _ = fs::remove_dir_all(&home);
    }

    /// An envelope written before the genesis was carried is the migration's
    /// input, and it has to read as *held with no genesis* — not as corrupt,
    /// which would refuse a working home, and not as absent, which would
    /// re-found it under a new address.
    #[test]
    fn a_pre_carriage_envelope_loads_with_no_genesis_and_its_registry_intact() {
        let home = dir("pre-carriage");
        let store = ReachStore::at(&home);
        let legacy = state(5);
        let body = postcard::to_stdvec(&PreCarriage {
            epoch: legacy.epoch,
            canonical: 1,
            registry: legacy.registry.clone(),
            sent: legacy.sent.clone(),
            address: Some("ada.example".into()),
        })
        .unwrap();
        let bytes = envelope(LEGACY_ENVELOPE_FORMAT, &body);
        fs::write(&store.path, &bytes).unwrap();

        let read = store
            .load()
            .expect("an older shape is not corrupt")
            .expect("present");
        assert!(read.genesis.is_none(), "nothing invented a genesis");
        assert!(matches!(read.origin, Origin::Founded));
        assert_eq!(read.epoch, 5);
        assert_eq!(read.address.as_deref(), Some("ada.example"));
        assert_eq!(
            read.registry.to_bytes().unwrap(),
            legacy.registry.to_bytes().unwrap(),
            "the authored log survived the lift"
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

    #[test]
    fn unbounded_received_material_is_refused_before_it_is_written() {
        let home = dir("received-bounds");
        let store = ReachStore::at(&home);
        let mut held = state(4);
        held.received.insert(
            "x".repeat(MAX_DEPOSIT_ID_BYTES + 1),
            Received {
                letter: vec![1],
                deposited_by: mechanics::actor::device_from_seed(&[72u8; 32]),
                arrived_at: 1,
            },
        );
        assert!(matches!(
            store.save(&held),
            Err(Error::Bound("deposit id bytes"))
        ));
        assert!(!store.path.exists(), "a refused state was not staged");

        held.received.clear();
        held.received.insert(
            "deposit".into(),
            Received {
                letter: vec![0; MAX_OPENED_LETTER_BYTES + 1],
                deposited_by: mechanics::actor::device_from_seed(&[72u8; 32]),
                arrived_at: 1,
            },
        );
        assert!(matches!(
            store.save(&held),
            Err(Error::Bound("opened letter bytes"))
        ));
        assert!(!store.path.exists(), "a refused state was not staged");
        let _ = fs::remove_dir_all(&home);
    }
}
