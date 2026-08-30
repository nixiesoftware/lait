//! The chronicle a deployment keeps, and the marks it signs over what it
//! recorded.
//!
//! Lifted out of [`crate::registry::Registrar`], which used to own the log, the
//! seed and the append race by itself. It is its own module because a
//! deployment has **one** chronicle and more than one thing to put in it: the
//! label registry and the address directory both append here, so an identity
//! that never chose a label is recorded exactly as one that did. Two logs would
//! have been two seeds, two heads, two pins and two surfaces for a reader to
//! follow, for one fact.
//!
//! The cost of sharing is stated rather than hidden: the directory's leaves
//! become hashes in a log anybody may read, so the *existence* of some
//! publication is public. Whose it is never is — a leaf is a hash over bytes
//! only the publisher and this service hold, and [`crate::Refusal::NotAvailable`]
//! still covers absence and denial with one answer. If that trade is ever
//! judged too much, [`ChronicleStore`] is the seam a second, private chronicler
//! goes behind.
//!
//! # A mark says what a head says, about one entry
//!
//! Appending answers a [`Receipt`]: the signed head the entry landed under, the
//! entry's index, the path that proves it, and one **mark** per device the
//! publication is about. A mark is a [`mechanics::kinship::Avowal`] carrying
//! [`Claim::Chronicled`] — the marker's signed statement that this leaf sits at
//! this index of its own log, which is exactly what the head already says and
//! nothing more. It is signed by the **chronicle seed**, the key that already
//! signs which publications were recorded and in which order; the operator key
//! that steers routing never touches a request path and does not sign these.
//!
//! A mark confers nothing. No admission, membership, grant or standing reads
//! one, and there is deliberately no conversion from one into any of those. It
//! is evidence a reader may weigh, and losing every marker is a standing rather
//! than a refusal.
//!
//! # Certification is per receipt, which is what makes it revocable
//!
//! Nothing here stores a mark. A mark is minted for one publication, about the
//! devices *that* publication avows, and handed to the publisher; the newest
//! receipt is therefore the whole certification, and a device the next
//! publication does not avow is simply absent from the next receipt's marks.
//! Revocation costs no retraction and rewrites no history — the older mark
//! stays true about the entry it names, which is all it ever claimed — and a
//! log that could only ever grow could not otherwise say "no longer".

use std::sync::{Arc, Mutex, MutexGuard};

use mechanics::chronicle::{Chronicle, Head, MAX_CHRONICLE_ENTRIES};
use mechanics::ids::DeviceId;
use mechanics::kinship::{Audience, Avowal, Claim, Party};
use serde::{Deserialize, Serialize};

use crate::Refusal;

/// How many times an append retries after losing an index race before the
/// answer is "unavailable". Each loss means another holder appended; losing
/// this many in a row means the store is churning faster than one request
/// deserves to wait.
const MAX_APPEND_RACES: usize = 8;

/// The leaves half of a store, and only that.
///
/// Split out of [`crate::registry::RegistryStore`] so a chronicler is not tied
/// to the registry's collections: the directory's own store implements this and
/// nothing else, and one implementation can be handed to the chronicler both
/// mounts share. `RegistryStore` requires it, so every store that was already a
/// registry store still is.
pub trait ChronicleStore {
    /// Every chronicle leaf hash, in append order. Read once at open, and
    /// again after a raced append.
    fn chronicle_leaves(&mut self) -> anyhow::Result<Vec<[u8; 32]>>;

    /// Append a leaf at `index`. `Ok(false)` when the index is already taken
    /// — another holder appended first; the caller reloads and takes the next
    /// slot. Refusing a taken index is the linearization point: two holders
    /// can never write different leaves at one index, so roots cannot fork.
    fn append_chronicle(&mut self, index: u64, leaf: [u8; 32]) -> anyhow::Result<bool>;
}

/// The chronicle surface's answer: the current signed head, and — when a
/// reader named the size it pinned — the path proving this head extends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChronicleAnswer {
    pub head: Head,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consistency: Vec<[u8; 32]>,
}

/// What one accepted publication got back from the chronicle.
///
/// Every field defaults, and this is flattened into the answers that already
/// existed rather than replacing them: a client built before any of this
/// decodes the shape it always did and ignores the rest, which is the same
/// additive move the head field made when the registry first grew one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// The signed head this entry landed under. `None` from a service that
    /// keeps no chronicle — allowed, and never the same thing as a refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<Head>,
    /// This publication's entry index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<u64>,
    /// The path proving `entry` sits under `head`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inclusion: Vec<[u8; 32]>,
    /// One mark per device this publication avows — the marker's signed
    /// statement that it recorded them, checkable with
    /// [`mechanics::chronicle::verify_mark`].
    ///
    /// This set is the *whole* certification for this publication. A device the
    /// next publication does not avow is absent from the next receipt, which is
    /// how a certification is withdrawn without anything being retracted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<Avowal>,
}

/// The chronicle, as the feeders above it use it.
///
/// A trait so the two mounts hold *one* chronicler between them without either
/// naming the other's store type. The alternative — a generic parameter
/// threaded through both services — spells the store into every signature that
/// only ever needed "the log this deployment keeps".
pub trait Chronicling {
    /// Append `entry` and answer its receipt, marking each of `subjects`.
    ///
    /// Called **before** the caller's own write goes live, so the log is a
    /// superset of what is live rather than a lagging shadow of it. A
    /// publication that is chronicled and then loses the record race is
    /// honestly chronicled: it was accepted, and only liveness was decided
    /// elsewhere.
    fn chronicle(&mut self, entry: &[u8], subjects: &[DeviceId]) -> Result<Receipt, Refusal>;

    /// The chronicle surface: always the current signed head, and — when the
    /// reader named a pin size this log still covers — the consistency path
    /// from it.
    ///
    /// A `first` *past* the current head is not an error and must not 404: a
    /// chronicle now shorter than a reader's pin is a **rollback**, the
    /// strongest signal of a rewritten log, and the reader's own
    /// [`mechanics::chronicle::advance`] is what must see it. So the head goes
    /// back regardless (with an empty path), and `offered.size < pinned.size`
    /// is judged where the pin lives, not folded into "not found" here.
    fn answer(&self, first: Option<u64>) -> Result<ChronicleAnswer, Refusal>;
}

/// One chronicle, held by every feeder that appends to it.
pub type SharedChronicler = Arc<Mutex<dyn Chronicling + Send>>;

/// Reach a shared chronicler. A poisoned lock is entered rather than panicked
/// on: the chronicle's consistency lives in the store's refusal to take an
/// index twice, so a thread that died mid-request left no half-state here that
/// a later one could read wrongly.
pub fn held(chronicler: &SharedChronicler) -> MutexGuard<'_, dyn Chronicling + Send + 'static> {
    match chronicler.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The log, the store under it, and the one key — which signs the chronicle's
/// heads and the marks over its own entries, and nothing else.
pub struct Chronicler<C> {
    chronicle: Chronicle,
    store: C,
    seed: [u8; 32],
}

impl<C: ChronicleStore> Chronicler<C> {
    /// Open over a store, restoring the log from its persisted leaves.
    pub fn open(mut store: C, seed: [u8; 32]) -> anyhow::Result<Self> {
        let leaves = store.chronicle_leaves()?;
        let chronicle = Chronicle::from_leaves(leaves)
            .map_err(|refusal| anyhow::anyhow!("chronicle restore: {refusal}"))?;
        Ok(Self {
            chronicle,
            store,
            seed,
        })
    }

    /// Open one every feeder can hold. The shape a deployment has: one log,
    /// one signer, however many surfaces append to it.
    pub fn shared(store: C, seed: [u8; 32]) -> anyhow::Result<SharedChronicler>
    where
        C: Send + 'static,
    {
        Ok(Arc::new(Mutex::new(Self::open(store, seed)?)))
    }

    /// The store, for the operator acts that bypass the request surface.
    pub fn store(&mut self) -> &mut C {
        &mut self.store
    }

    fn reload(&mut self) -> Result<(), Refusal> {
        let leaves = self
            .store
            .chronicle_leaves()
            .map_err(|error| Refusal::Unavailable(error.to_string()))?;
        self.chronicle = Chronicle::from_leaves(leaves)
            .map_err(|refusal| Refusal::Unavailable(refusal.to_string()))?;
        Ok(())
    }

    fn head(&self) -> Result<Head, Refusal> {
        self.chronicle
            .head(&self.seed)
            .map_err(|refusal| Refusal::Unavailable(refusal.to_string()))
    }

    /// Take the next free index for `leaf`, reloading around whoever won a
    /// race for it.
    fn append(&mut self, leaf: [u8; 32]) -> Result<u64, Refusal> {
        let mut races = 0;
        loop {
            let index = self.chronicle.size();
            if index >= MAX_CHRONICLE_ENTRIES {
                return Err(Refusal::Unavailable("the chronicle is full".into()));
            }
            match self.store.append_chronicle(index, leaf) {
                Ok(true) => {
                    self.chronicle
                        .append_leaf(leaf)
                        .map_err(|refusal| Refusal::Unavailable(refusal.to_string()))?;
                    return Ok(index);
                }
                Ok(false) => {
                    races += 1;
                    if races > MAX_APPEND_RACES {
                        return Err(Refusal::Unavailable("chronicle append raced out".into()));
                    }
                    self.reload()?;
                }
                Err(error) => return Err(Refusal::Unavailable(error.to_string())),
            }
        }
    }

    /// Sign what this log now remembers about one entry, about one device.
    ///
    /// `epoch` is the log's size, which only ever grows, so a reader keeping
    /// the latest-epoch mark per subject keeps the newest one without a clock.
    /// The nonce is derived from the leaf rather than drawn, so marking one
    /// publication twice produces one artifact instead of two that disagree
    /// about nothing.
    fn mark(
        &self,
        subject: &DeviceId,
        entry: u64,
        leaf: [u8; 32],
        head: &Head,
    ) -> Result<Avowal, Refusal> {
        let mut nonce = [0u8; 16];
        nonce.copy_from_slice(&leaf[..16]);
        Avowal::seal(
            &self.seed,
            Party::Device(subject.clone()),
            Claim::Chronicled {
                size: head.size,
                root: head.root,
                entry,
                leaf,
            },
            Audience::Public,
            head.size,
            nonce,
        )
        .map_err(|refusal| Refusal::Unavailable(format!("mark: {refusal}")))
    }
}

impl<C: ChronicleStore> Chronicling for Chronicler<C> {
    fn chronicle(&mut self, entry: &[u8], subjects: &[DeviceId]) -> Result<Receipt, Refusal> {
        let leaf = Chronicle::leaf_of(entry);
        let index = self.append(leaf)?;
        let head = self.head()?;
        let inclusion = self
            .chronicle
            .inclusion(index)
            .map_err(|refusal| Refusal::Unavailable(refusal.to_string()))?;
        let marks = subjects
            .iter()
            .map(|subject| self.mark(subject, index, leaf, &head))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Receipt {
            head: Some(head),
            entry: Some(index),
            inclusion,
            marks,
        })
    }

    fn answer(&self, first: Option<u64>) -> Result<ChronicleAnswer, Refusal> {
        let head = self.head()?;
        let consistency = match first {
            Some(first) if first <= self.chronicle.size() && first > 0 => self
                .chronicle
                .consistency(first)
                .map_err(|refusal| Refusal::Unavailable(refusal.to_string()))?,
            _ => Vec::new(),
        };
        Ok(ChronicleAnswer { head, consistency })
    }
}
