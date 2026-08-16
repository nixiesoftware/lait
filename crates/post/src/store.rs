//! Where the Post keeps what it is holding.
//!
//! Two implementations, and the trait exists so the carrier's rules can be
//! tested without either: [`MemStore`] for tests, [`FsStore`] for a running
//! service. Neither can read an envelope — both move opaque bytes keyed by
//! recipient device, which is the only thing the Post is entitled to know.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mechanics::ids::DeviceId;
use serde::{Deserialize, Serialize};

use crate::Envelope;

/// One envelope as it sits waiting, with the facts the carrier added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deposited {
    /// Stable id, for acknowledging exactly this one.
    pub id: String,
    /// Who deposited it. Recorded because abuse control needs a subject, not
    /// because the recipient has to trust it — the envelope's own seal is what
    /// says who wrote it.
    pub sender: DeviceId,
    pub envelope: Envelope,
    /// When it arrived, unix seconds.
    pub deposited_at: u64,
}

/// What a Post keeps its deposits in.
pub trait Store {
    /// Accept one envelope and return its id.
    fn put(&mut self, sender: &DeviceId, envelope: &Envelope) -> anyhow::Result<String>;
    /// Everything waiting for this device that has not expired.
    fn list(&self, device: &DeviceId, now: u64) -> anyhow::Result<Vec<Deposited>>;
    /// Drop the named deposits belonging to this device. Returns how many went.
    fn drop_all(&mut self, device: &DeviceId, ids: &[String]) -> anyhow::Result<usize>;
    /// Drop everything past its window. Returns how many went.
    fn sweep(&mut self, now: u64) -> anyhow::Result<usize>;
}

/// A deterministic id for a deposit: the content, so the same envelope
/// deposited twice is one deposit rather than two.
///
/// That de-duplication is deliberate. A sender retrying a delivery it is not
/// sure landed should not double a recipient's mailbox, and the retry is the
/// common case for a carrier whose whole purpose is reaching someone who is not
/// there.
fn deposit_id(sender: &DeviceId, envelope: &Envelope) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lait/post/1/deposit-id");
    hasher.update(sender.as_str().as_bytes());
    hasher.update(envelope.recipient.as_str().as_bytes());
    hasher.update(&envelope.expires_at.to_be_bytes());
    hasher.update(&envelope.sealed);
    data_encoding::HEXLOWER.encode(&hasher.finalize().as_bytes()[..16])
}

/// In memory, for tests and for a Post that is not meant to survive a restart.
#[derive(Debug, Default)]
pub struct MemStore {
    /// Keyed by recipient, then by deposit id — so a fetch is one lookup and
    /// never a scan of everybody's mail.
    held: BTreeMap<DeviceId, BTreeMap<String, Deposited>>,
}

impl Store for MemStore {
    fn put(&mut self, sender: &DeviceId, envelope: &Envelope) -> anyhow::Result<String> {
        let id = deposit_id(sender, envelope);
        self.held
            .entry(envelope.recipient.clone())
            .or_default()
            .insert(
                id.clone(),
                Deposited {
                    id: id.clone(),
                    sender: sender.clone(),
                    envelope: envelope.clone(),
                    deposited_at: envelope.expires_at,
                },
            );
        Ok(id)
    }

    fn list(&self, device: &DeviceId, now: u64) -> anyhow::Result<Vec<Deposited>> {
        Ok(self
            .held
            .get(device)
            .map(|box_of| {
                box_of
                    .values()
                    .filter(|held| held.envelope.expires_at > now)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    fn drop_all(&mut self, device: &DeviceId, ids: &[String]) -> anyhow::Result<usize> {
        let Some(box_of) = self.held.get_mut(device) else {
            return Ok(0);
        };
        let mut gone = 0;
        for id in ids {
            if box_of.remove(id).is_some() {
                gone += 1;
            }
        }
        Ok(gone)
    }

    fn sweep(&mut self, now: u64) -> anyhow::Result<usize> {
        let mut gone = 0;
        for box_of in self.held.values_mut() {
            let before = box_of.len();
            box_of.retain(|_, held| held.envelope.expires_at > now);
            gone += before.saturating_sub(box_of.len());
        }
        self.held.retain(|_, box_of| !box_of.is_empty());
        Ok(gone)
    }
}

/// On disk: one directory per recipient, one file per deposit.
///
/// A directory rather than a database because the shape genuinely is a blob
/// store keyed by device, and the operational cost of a database is the thing
/// that made the earlier mail-server design unaffordable. The recipient's
/// directory name is a hash of the device id, not the id itself — a carrier's
/// filesystem should not be a browsable list of who holds a mailbox here.
#[derive(Debug)]
pub struct FsStore {
    root: PathBuf,
}

impl FsStore {
    pub fn open(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn box_dir(&self, device: &DeviceId) -> PathBuf {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"lait/post/1/mailbox");
        hasher.update(device.as_str().as_bytes());
        self.root
            .join(data_encoding::HEXLOWER.encode(&hasher.finalize().as_bytes()[..16]))
    }
}

impl Store for FsStore {
    fn put(&mut self, sender: &DeviceId, envelope: &Envelope) -> anyhow::Result<String> {
        let id = deposit_id(sender, envelope);
        let dir = self.box_dir(&envelope.recipient);
        std::fs::create_dir_all(&dir)?;
        let held = Deposited {
            id: id.clone(),
            sender: sender.clone(),
            envelope: envelope.clone(),
            deposited_at: envelope.expires_at,
        };
        // Written beside and renamed, so a reader never sees half a deposit.
        let staged = dir.join(format!("{id}.staged"));
        std::fs::write(&staged, serde_json::to_vec(&held)?)?;
        std::fs::rename(staged, dir.join(format!("{id}.json")))?;
        Ok(id)
    }

    fn list(&self, device: &DeviceId, now: u64) -> anyhow::Result<Vec<Deposited>> {
        let dir = self.box_dir(device);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(held) = serde_json::from_slice::<Deposited>(&bytes) else {
                continue;
            };
            if held.envelope.expires_at > now {
                out.push(held);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    fn drop_all(&mut self, device: &DeviceId, ids: &[String]) -> anyhow::Result<usize> {
        let dir = self.box_dir(device);
        let mut gone = 0;
        for id in ids {
            // The id is a hex digest this service produced, but it arrives from
            // the network, so it is checked rather than trusted: anything else
            // is a path fragment reaching for another mailbox.
            if id.len() != 32 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }
            if std::fs::remove_file(dir.join(format!("{id}.json"))).is_ok() {
                gone += 1;
            }
        }
        Ok(gone)
    }

    fn sweep(&mut self, now: u64) -> anyhow::Result<usize> {
        let Ok(boxes) = std::fs::read_dir(&self.root) else {
            return Ok(0);
        };
        let mut gone = 0;
        for mailbox in boxes.flatten() {
            let Ok(entries) = std::fs::read_dir(mailbox.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let expired = match serde_json::from_slice::<Deposited>(&bytes) {
                    Ok(held) => held.envelope.expires_at <= now,
                    // Unreadable is collected too: a file this service cannot
                    // parse is one nobody will ever be handed, and leaving it
                    // is a slow leak with no upside.
                    Err(_) => true,
                };
                if expired && std::fs::remove_file(&path).is_ok() {
                    gone += 1;
                }
            }
        }
        Ok(gone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mechanics::actor::device_from_seed;

    const NOW: u64 = 1_800_000_000;

    fn envelope(recipient: &DeviceId, body: &[u8], expires_at: u64) -> Envelope {
        Envelope {
            recipient: recipient.clone(),
            sealed: body.to_vec(),
            expires_at,
            envelope_version: 1,
        }
    }

    fn exercise<S: Store>(mut store: S) {
        let sender = device_from_seed(&[1u8; 32]);
        let alice = device_from_seed(&[2u8; 32]);
        let bob = device_from_seed(&[3u8; 32]);

        let first = store
            .put(&sender, &envelope(&alice, b"one", NOW + 100))
            .expect("put");
        store
            .put(&sender, &envelope(&alice, b"two", NOW + 100))
            .expect("put");
        store
            .put(&sender, &envelope(&bob, b"bob's", NOW + 100))
            .expect("put");

        assert_eq!(store.list(&alice, NOW).expect("list").len(), 2);
        assert_eq!(store.list(&bob, NOW).expect("list").len(), 1);

        // The same envelope again is the same deposit, not a second one.
        let again = store
            .put(&sender, &envelope(&alice, b"one", NOW + 100))
            .expect("put");
        assert_eq!(again, first, "a retry is not a duplicate");
        assert_eq!(store.list(&alice, NOW).expect("list").len(), 2);

        assert_eq!(store.drop_all(&alice, &[first]).expect("drop"), 1);
        assert_eq!(store.list(&alice, NOW).expect("list").len(), 1);

        // Another device cannot drop this one's mail by naming its id.
        let bobs = store.list(&bob, NOW).expect("list")[0].id.clone();
        assert_eq!(
            store.drop_all(&alice, &[bobs]).expect("drop"),
            0,
            "an id only means anything inside its own mailbox"
        );
        assert_eq!(store.list(&bob, NOW).expect("list").len(), 1);

        store
            .put(&sender, &envelope(&alice, b"brief", NOW + 10))
            .expect("put");
        assert_eq!(store.list(&alice, NOW + 50).expect("list").len(), 1);
        assert_eq!(store.sweep(NOW + 50).expect("sweep"), 1);
    }

    #[test]
    fn the_memory_store_holds_its_end() {
        exercise(MemStore::default());
    }

    #[test]
    fn the_filesystem_store_holds_the_same_end() {
        let dir = tempfile::tempdir().expect("a root");
        exercise(FsStore::open(dir.path()).expect("open"));
    }

    #[test]
    fn a_mailbox_directory_does_not_name_its_owner() {
        let dir = tempfile::tempdir().expect("a root");
        let mut store = FsStore::open(dir.path()).expect("open");
        let alice = device_from_seed(&[2u8; 32]);
        store
            .put(
                &device_from_seed(&[1u8; 32]),
                &envelope(&alice, b"x", NOW + 10),
            )
            .expect("put");

        let names: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1);
        assert!(
            !names[0].contains(alice.as_str()),
            "a carrier's filesystem must not be a browsable list of who has a mailbox here"
        );
    }

    #[test]
    fn a_deposit_id_reaching_for_another_path_is_refused() {
        let dir = tempfile::tempdir().expect("a root");
        let mut store = FsStore::open(dir.path()).expect("open");
        let alice = device_from_seed(&[2u8; 32]);
        store
            .put(
                &device_from_seed(&[1u8; 32]),
                &envelope(&alice, b"x", NOW + 10),
            )
            .expect("put");

        for hostile in ["../../etc/passwd", "..", "a/b", &"f".repeat(64)] {
            assert_eq!(
                store
                    .drop_all(&alice, &[hostile.to_string()])
                    .expect("drop"),
                0
            );
        }
        assert_eq!(store.list(&alice, NOW).expect("list").len(), 1);
    }
}
