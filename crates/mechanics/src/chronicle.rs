#![allow(
    clippy::arithmetic_side_effects,
    reason = "tree arithmetic operates on sizes admitted by MAX_CHRONICLE_ENTRIES and indices checked against them at every entry point, so splits, shifts and increments cannot overflow"
)]
//! Chronicle — an append-only committed log whose holder can prove its own
//! consistency. The memory a service references when it attests.
//!
//! A service that merely *mirrors* can serve two readers two different worlds
//! and neither can tell. A service that keeps a chronicle signs a [`Head`]
//! over the ordered whole of what it has served, and every answer it gives can
//! be checked against that head: an entry proves its place with an inclusion
//! path, and a newer head proves it *extends* an older one with a consistency
//! path. Two signed heads that cannot be reconciled are then not a suspicion —
//! they are the lie itself, carried in the liar's own signature.
//!
//! # What a chronicle head can and cannot do
//!
//! A head signs **which entries exist, in which order** — never that any entry
//! is true. Contents remain the business of whoever signed them; the same
//! exclusion [`crate::kinship::Head`] and [`crate::ledger`]'s checkpoint make,
//! for the same reason. Inclusion means "this was said, then". Nothing more.
//!
//! # The reader's ratchet, and its three distinct refusals
//!
//! [`advance`] is the verifier-side monotonic ratchet: a reader pins the first
//! head it accepts and thereafter only moves forward along proven extensions.
//! Its refusals are deliberately three different facts:
//!
//! - [`Refusal::Rollback`] — the offered head is *older* than the pin. A
//!   replayed copy, exactly the freeze a stale-pointer replay attempts against
//!   an update feed, and refused the same way.
//! - [`Refusal::Diverged`] — the offered head names the **same size** as the
//!   pin with a different root. Both artifacts are signed; together they are
//!   non-repudiable proof of equivocation. This is the caught lie.
//! - [`Refusal::Unproven`] — the offered head is larger but could not be shown
//!   to extend the pin. Suspicion, not proof: a correct proof might exist and
//!   the holder failed to produce it. The pin holds; the surface says "could
//!   not be shown to extend", never "diverged" and never "fine".
//!
//! Folding any two of those together is the false-disconnection defect one
//! plane up, and the reason they are variants rather than a boolean.
//!
//! # Shape
//!
//! The tree is the RFC 6962 Merkle shape over append-ordered leaves — splits
//! at the largest power of two strictly below the count, odd subtrees promote
//! rather than duplicate — with blake3 `derive_key` under distinct leaf and
//! node contexts, so a leaf can never be reinterpreted as a node (the n/n+1
//! forgery the duplicate-promotion distinction exists to kill).

use serde::{Deserialize, Serialize};

use crate::actor::{device_from_seed, sign_detached, verify_detached};
use crate::ids::DeviceId;
use crate::kinship::Signature;

/// Signing domain for a chronicle head.
pub const CHRONICLE_HEAD_DOMAIN: &[u8] = b"lait/chronicle/1/head";

/// Key-derivation context for a leaf. Distinct from [`NODE_CONTEXT`] so no
/// served entry can collide with an interior node.
pub const LEAF_CONTEXT: &str = "lait.chronicle-leaf.v1";

/// Key-derivation context for an interior node over two child roots.
pub const NODE_CONTEXT: &str = "lait.chronicle-node.v1";

/// Key-derivation context for the root of the empty chronicle.
pub const EMPTY_CONTEXT: &str = "lait.chronicle-empty.v1";

/// The semantics this build commits to. Bound into every signed head so a
/// head minted under different rules can never be read as agreeing.
pub const CHRONICLE_SEMANTICS: u16 = 1;

/// Cap on entries one chronicle holds. Far above any honest service's need;
/// the point is that proof generation is bounded work.
pub const MAX_CHRONICLE_ENTRIES: u64 = 1 << 20;

/// Cap on one entry's bytes. An entry arrives from a stranger.
pub const MAX_ENTRY_BYTES: usize = 256 * 1024;

/// Cap on hashes in one proof. `2 * log2(MAX_CHRONICLE_ENTRIES)` with slack;
/// a longer proof is malformed before it is wrong.
pub const MAX_PROOF_HASHES: usize = 64;

/// A typed refusal. Never a boolean: which of these happened is the entire
/// output of the ratchet, and every surface renders them differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The signature does not verify under the stated signer.
    BadSignature,
    /// The signer's id is not a usable verifying key.
    Unaddressable,
    /// The head was minted under different semantics.
    Semantics,
    /// An index or size names something this chronicle does not hold.
    OutOfRange,
    /// The offered head is older than the pinned one — a replayed copy.
    Rollback,
    /// Two signed heads at one size disagree. Non-repudiable equivocation.
    Diverged,
    /// The offered head could not be shown to extend the pin. Suspicion,
    /// never rendered as either "diverged" or "fine".
    Unproven,
    /// A bound was exceeded, named.
    Bound(&'static str),
    /// The artifact does not parse as what it claims to be, named.
    Malformed(&'static str),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadSignature => f.write_str("the signature does not verify"),
            Self::Unaddressable => f.write_str("the signer is not a usable key"),
            Self::Semantics => f.write_str("the head was minted under different semantics"),
            Self::OutOfRange => f.write_str("the chronicle does not hold what was named"),
            Self::Rollback => f.write_str("the offered head is older than the pinned one"),
            Self::Diverged => {
                f.write_str("two signed heads at one size disagree — the holder equivocated")
            }
            Self::Unproven => {
                f.write_str("the offered head could not be shown to extend the pinned one")
            }
            Self::Bound(what) => write!(f, "bound exceeded: {what}"),
            Self::Malformed(what) => write!(f, "malformed: {what}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Length-prefixed framing, the same shape kinship's preimages use: two
/// adjacent variable-length fields can never be read as one.
fn framed(out: &mut Vec<u8>, part: &[u8]) {
    let len = u64::try_from(part.len()).unwrap_or(u64::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(part);
}

/// The root of the empty chronicle. A fixed value, so "has witnessed nothing
/// yet" is a committable, pinnable state rather than an absence.
#[must_use]
pub fn empty_root() -> [u8; 32] {
    blake3::derive_key(EMPTY_CONTEXT, &[])
}

fn leaf_hash(entry: &[u8]) -> [u8; 32] {
    blake3::derive_key(LEAF_CONTEXT, entry)
}

fn node_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut joined = Vec::with_capacity(64);
    joined.extend_from_slice(left);
    joined.extend_from_slice(right);
    blake3::derive_key(NODE_CONTEXT, &joined)
}

/// The largest power of two strictly below `n`. Callers guarantee `n >= 2`.
fn split_point(n: usize) -> usize {
    let shift = usize::BITS - (n - 1).leading_zeros() - 1;
    1 << shift
}

fn subtree_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    match leaves {
        [] => empty_root(),
        [one] => *one,
        many => {
            let (left, right) = many.split_at(split_point(many.len()));
            node_hash(&subtree_root(left), &subtree_root(right))
        }
    }
}

fn audit_path(index: usize, leaves: &[[u8; 32]]) -> Vec<[u8; 32]> {
    if leaves.len() <= 1 {
        return Vec::new();
    }
    let (left, right) = leaves.split_at(split_point(leaves.len()));
    if index < left.len() {
        let mut path = audit_path(index, left);
        path.push(subtree_root(right));
        path
    } else {
        let mut path = audit_path(index - left.len(), right);
        path.push(subtree_root(left));
        path
    }
}

fn subproof(first: usize, leaves: &[[u8; 32]], whole: bool) -> Vec<[u8; 32]> {
    if first == leaves.len() {
        if whole {
            return Vec::new();
        }
        return vec![subtree_root(leaves)];
    }
    let (left, right) = leaves.split_at(split_point(leaves.len()));
    if first <= left.len() {
        let mut proof = subproof(first, left, whole);
        proof.push(subtree_root(right));
        proof
    } else {
        let mut proof = subproof(first - left.len(), right, false);
        proof.push(subtree_root(left));
        proof
    }
}

/// Verify that `leaf` sits at `index` in the tree of `size` leaves whose root
/// is `root`, along `path`. Pure; a reader runs this against a served entry.
pub fn verify_inclusion(
    leaf: &[u8; 32],
    index: u64,
    size: u64,
    root: &[u8; 32],
    path: &[[u8; 32]],
) -> Result<(), Refusal> {
    if path.len() > MAX_PROOF_HASHES {
        return Err(Refusal::Bound("inclusion path hashes"));
    }
    if index >= size {
        return Err(Refusal::OutOfRange);
    }
    let mut fnode = index;
    let mut snode = size - 1;
    let mut running = *leaf;
    for part in path {
        if snode == 0 {
            return Err(Refusal::Malformed("inclusion path too long"));
        }
        if fnode & 1 == 1 || fnode == snode {
            running = node_hash(part, &running);
            if fnode & 1 == 0 {
                while fnode & 1 == 0 && fnode != 0 {
                    fnode >>= 1;
                    snode >>= 1;
                }
            }
        } else {
            running = node_hash(&running, part);
        }
        fnode >>= 1;
        snode >>= 1;
    }
    if snode == 0 && &running == root {
        Ok(())
    } else {
        Err(Refusal::Unproven)
    }
}

/// Verify that the tree of `first_size` leaves under `first_root` is a prefix
/// of the tree of `second_size` leaves under `second_root`, along `proof`.
/// Pure; this is the extension check [`advance`] runs.
pub fn verify_consistency(
    first_size: u64,
    first_root: &[u8; 32],
    second_size: u64,
    second_root: &[u8; 32],
    proof: &[[u8; 32]],
) -> Result<(), Refusal> {
    if proof.len() > MAX_PROOF_HASHES {
        return Err(Refusal::Bound("consistency path hashes"));
    }
    if first_size == 0 || first_size > second_size {
        return Err(Refusal::OutOfRange);
    }
    if first_size == second_size {
        return if first_root == second_root && proof.is_empty() {
            Ok(())
        } else {
            Err(Refusal::Unproven)
        };
    }
    let mut path: Vec<[u8; 32]> = Vec::with_capacity(proof.len() + 1);
    if first_size.is_power_of_two() {
        path.push(*first_root);
    }
    path.extend_from_slice(proof);
    let mut parts = path.iter();
    let Some(seed) = parts.next() else {
        return Err(Refusal::Malformed("empty consistency path"));
    };
    let mut fnode = first_size - 1;
    let mut snode = second_size - 1;
    while fnode & 1 == 1 {
        fnode >>= 1;
        snode >>= 1;
    }
    let mut first_running = *seed;
    let mut second_running = *seed;
    for part in parts {
        if snode == 0 {
            return Err(Refusal::Malformed("consistency path too long"));
        }
        if fnode & 1 == 1 || fnode == snode {
            first_running = node_hash(part, &first_running);
            second_running = node_hash(part, &second_running);
            if fnode & 1 == 0 {
                while fnode & 1 == 0 && fnode != 0 {
                    fnode >>= 1;
                    snode >>= 1;
                }
            }
        } else {
            second_running = node_hash(&second_running, part);
        }
        fnode >>= 1;
        snode >>= 1;
    }
    if snode == 0 && &first_running == first_root && &second_running == second_root {
        Ok(())
    } else {
        Err(Refusal::Unproven)
    }
}

/// A signed chronicle head: the whole memory at one size, as one artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Head {
    pub by: DeviceId,
    pub semantics: u16,
    pub size: u64,
    pub root: [u8; 32],
    pub signature: Signature,
}

impl Head {
    /// The preimage. Every field is inside it; there is no envelope half.
    #[must_use]
    pub fn preimage(by: &DeviceId, semantics: u16, size: u64, root: &[u8; 32]) -> Vec<u8> {
        let mut out = Vec::new();
        framed(&mut out, CHRONICLE_HEAD_DOMAIN);
        framed(&mut out, by.as_str().as_bytes());
        framed(&mut out, &semantics.to_be_bytes());
        framed(&mut out, &size.to_be_bytes());
        framed(&mut out, &root[..]);
        out
    }

    /// Seal a head over `size` and `root` with the device behind `seed`.
    pub fn seal(seed: &[u8; 32], size: u64, root: [u8; 32]) -> Result<Self, Refusal> {
        let by = device_from_seed(seed);
        let preimage = Self::preimage(&by, CHRONICLE_SEMANTICS, size, &root);
        let signature = Signature(sign_detached(seed, &preimage));
        Ok(Self {
            by,
            semantics: CHRONICLE_SEMANTICS,
            size,
            root,
            signature,
        })
    }

    /// Verify semantics and signature. Whether `by` is a device the chronicle
    /// holder's genesis roots is the caller's anchoring job, the same split
    /// kinship's absorb makes.
    pub fn verify(&self) -> Result<(), Refusal> {
        if self.semantics != CHRONICLE_SEMANTICS {
            return Err(Refusal::Semantics);
        }
        let key = self.by.key_bytes().ok_or(Refusal::Unaddressable)?;
        let preimage = Self::preimage(&self.by, self.semantics, self.size, &self.root);
        if verify_detached(&key, &preimage, self.signature.bytes()) {
            Ok(())
        } else {
            Err(Refusal::BadSignature)
        }
    }
}

/// What a reader durably holds about a chronicle it follows. Only the facts
/// the ratchet needs; the head artifact itself may be kept beside it as the
/// evidence half of a divergence claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedHead {
    pub size: u64,
    pub root: [u8; 32],
    pub by: DeviceId,
}

impl From<&Head> for PinnedHead {
    fn from(head: &Head) -> Self {
        Self {
            size: head.size,
            root: head.root,
            by: head.by.clone(),
        }
    }
}

/// How an offered head related to the pin. Refusals are [`Refusal`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advance {
    /// Nothing was pinned; the offered head is the first.
    Pinned,
    /// Same size, same root. Nothing new.
    Unchanged,
    /// Provably extends the pin.
    Extended,
}

/// The reader's monotonic ratchet. `consistency` is the path the holder
/// served alongside the head; it is ignored except where an extension has to
/// be proven. On `Ok`, the caller replaces its pin with `offered`.
pub fn advance(
    held: Option<&PinnedHead>,
    offered: &Head,
    consistency: &[[u8; 32]],
) -> Result<Advance, Refusal> {
    offered.verify()?;
    let Some(held) = held else {
        return Ok(Advance::Pinned);
    };
    if offered.size < held.size {
        return Err(Refusal::Rollback);
    }
    if offered.size == held.size {
        return if offered.root == held.root {
            Ok(Advance::Unchanged)
        } else {
            Err(Refusal::Diverged)
        };
    }
    if held.size == 0 {
        // The empty chronicle is a prefix of every chronicle — but only the
        // empty root may claim to be it.
        return if held.root == empty_root() {
            Ok(Advance::Extended)
        } else {
            Err(Refusal::Diverged)
        };
    }
    verify_consistency(
        held.size,
        &held.root,
        offered.size,
        &offered.root,
        consistency,
    )
    .map(|()| Advance::Extended)
}

/// The log itself, held by the service side. Leaf hashes only: the entries'
/// bytes live wherever the holder stores what it serves.
#[derive(Debug, Clone, Default)]
pub struct Chronicle {
    leaves: Vec<[u8; 32]>,
}

impl Chronicle {
    #[must_use]
    pub const fn new() -> Self {
        Self { leaves: Vec::new() }
    }

    /// Restore from persisted leaf hashes.
    pub fn from_leaves(leaves: Vec<[u8; 32]>) -> Result<Self, Refusal> {
        let count = u64::try_from(leaves.len()).map_err(|_| Refusal::Bound("chronicle entries"))?;
        if count > MAX_CHRONICLE_ENTRIES {
            return Err(Refusal::Bound("chronicle entries"));
        }
        Ok(Self { leaves })
    }

    #[must_use]
    pub fn leaves(&self) -> &[[u8; 32]] {
        &self.leaves
    }

    #[must_use]
    pub fn size(&self) -> u64 {
        u64::try_from(self.leaves.len()).unwrap_or(u64::MAX)
    }

    #[must_use]
    pub fn root(&self) -> [u8; 32] {
        subtree_root(&self.leaves)
    }

    /// The leaf hash a given entry's bytes commit to. Public so a reader can
    /// hash the entry it was served and verify inclusion of *that*.
    #[must_use]
    pub fn leaf_of(entry: &[u8]) -> [u8; 32] {
        leaf_hash(entry)
    }

    /// Append an entry; returns its index. The entry is hashed, not kept.
    pub fn append(&mut self, entry: &[u8]) -> Result<u64, Refusal> {
        if entry.len() > MAX_ENTRY_BYTES {
            return Err(Refusal::Bound("entry bytes"));
        }
        let index = self.size();
        if index >= MAX_CHRONICLE_ENTRIES {
            return Err(Refusal::Bound("chronicle entries"));
        }
        self.leaves.push(leaf_hash(entry));
        Ok(index)
    }

    /// Sign the current head with the device behind `seed`.
    pub fn head(&self, seed: &[u8; 32]) -> Result<Head, Refusal> {
        Head::seal(seed, self.size(), self.root())
    }

    /// The inclusion path for the entry at `index` under the current head.
    pub fn inclusion(&self, index: u64) -> Result<Vec<[u8; 32]>, Refusal> {
        let at = usize::try_from(index).map_err(|_| Refusal::OutOfRange)?;
        if at >= self.leaves.len() {
            return Err(Refusal::OutOfRange);
        }
        Ok(audit_path(at, &self.leaves))
    }

    /// The consistency path from the head at `first_size` to the current one.
    pub fn consistency(&self, first_size: u64) -> Result<Vec<[u8; 32]>, Refusal> {
        let first = usize::try_from(first_size).map_err(|_| Refusal::OutOfRange)?;
        if first == 0 || first > self.leaves.len() {
            return Err(Refusal::OutOfRange);
        }
        if first == self.leaves.len() {
            return Ok(Vec::new());
        }
        Ok(subproof(first, &self.leaves, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(tag: u8) -> [u8; 32] {
        [tag; 32]
    }

    fn filled(count: usize) -> Chronicle {
        let mut log = Chronicle::new();
        for index in 0..count {
            log.append(format!("entry-{index}").as_bytes())
                .expect("append");
        }
        log
    }

    #[test]
    fn every_entry_proves_its_place_at_every_size() {
        for size in 1..=33usize {
            let log = filled(size);
            let root = log.root();
            for index in 0..size {
                let path = log.inclusion(index as u64).expect("path");
                let leaf = Chronicle::leaf_of(format!("entry-{index}").as_bytes());
                verify_inclusion(&leaf, index as u64, size as u64, &root, &path).unwrap_or_else(
                    |refusal| panic!("inclusion {index} of {size} refused: {refusal}"),
                );
            }
        }
    }

    #[test]
    fn a_wrong_leaf_index_or_truncated_path_does_not_verify() {
        let log = filled(9);
        let root = log.root();
        let path = log.inclusion(4).expect("path");
        let leaf = Chronicle::leaf_of(b"entry-4");
        assert!(verify_inclusion(&leaf, 4, 9, &root, &path).is_ok());
        let wrong_leaf = Chronicle::leaf_of(b"entry-5");
        assert!(verify_inclusion(&wrong_leaf, 4, 9, &root, &path).is_err());
        assert!(verify_inclusion(&leaf, 5, 9, &root, &path).is_err());
        let truncated: Vec<[u8; 32]> = path.iter().skip(1).copied().collect();
        assert!(verify_inclusion(&leaf, 4, 9, &root, &truncated).is_err());
        let mut grown = path;
        grown.push([9u8; 32]);
        assert!(verify_inclusion(&leaf, 4, 9, &root, &grown).is_err());
    }

    #[test]
    fn every_prefix_proves_consistency_at_every_size() {
        for second in 1..=33usize {
            let log = filled(second);
            let second_root = log.root();
            for first in 1..=second {
                let first_root = filled(first).root();
                let proof = log.consistency(first as u64).expect("proof");
                verify_consistency(
                    first as u64,
                    &first_root,
                    second as u64,
                    &second_root,
                    &proof,
                )
                .unwrap_or_else(|refusal| {
                    panic!("consistency {first} -> {second} refused: {refusal}")
                });
            }
        }
    }

    #[test]
    fn a_rewritten_history_cannot_prove_it_extends_the_honest_one() {
        let honest = filled(4);
        let honest_root = honest.root();
        // A holder that rewrote entry 0 after the fact and grew to 8.
        let mut rewritten = Chronicle::new();
        rewritten.append(b"entry-x").expect("append");
        for index in 1..8 {
            rewritten
                .append(format!("entry-{index}").as_bytes())
                .expect("append");
        }
        let proof = rewritten.consistency(4).expect("proof");
        let refusal = verify_consistency(4, &honest_root, 8, &rewritten.root(), &proof)
            .expect_err("a rewritten prefix must not verify");
        assert_eq!(refusal, Refusal::Unproven);
    }

    #[test]
    fn a_head_signs_and_verifies_and_a_tampered_one_does_not() {
        let log = filled(5);
        let head = log.head(&seed(1)).expect("head");
        head.verify().expect("verifies");
        let mut wrong_semantics = head.clone();
        wrong_semantics.semantics = CHRONICLE_SEMANTICS + 1;
        assert_eq!(wrong_semantics.verify(), Err(Refusal::Semantics));
        let mut wrong_root = head.clone();
        wrong_root.root = [7u8; 32];
        assert_eq!(wrong_root.verify(), Err(Refusal::BadSignature));
        let mut wrong_size = head;
        wrong_size.size += 1;
        assert_eq!(wrong_size.verify(), Err(Refusal::BadSignature));
    }

    #[test]
    fn the_ratchet_pins_extends_refuses_rollback_and_catches_the_fork() {
        let signer = seed(3);
        let mut log = filled(4);
        let first = log.head(&signer).expect("head");

        // Nothing pinned: the first head pins.
        assert_eq!(advance(None, &first, &[]), Ok(Advance::Pinned));
        let pin = PinnedHead::from(&first);

        // Served again unchanged.
        assert_eq!(advance(Some(&pin), &first, &[]), Ok(Advance::Unchanged));

        // Grown honestly: extends, with the proof checked.
        for index in 4..7 {
            log.append(format!("entry-{index}").as_bytes())
                .expect("append");
        }
        let second = log.head(&signer).expect("head");
        let proof = log.consistency(pin.size).expect("proof");
        assert_eq!(advance(Some(&pin), &second, &proof), Ok(Advance::Extended));
        let later = PinnedHead::from(&second);

        // The old head replayed against the moved pin: rollback.
        assert_eq!(advance(Some(&later), &first, &[]), Err(Refusal::Rollback));

        // A fork at the pinned size with different contents: caught cold.
        let mut fork = filled(3);
        fork.append(b"entry-forged").expect("append");
        let forked = fork.head(&signer).expect("head");
        assert_eq!(advance(Some(&pin), &forked, &[]), Err(Refusal::Diverged));

        // The fork grown past the pin: its own proof is valid for *its*
        // history and still cannot link to the pin — unproven, pin holds.
        for index in 4..9 {
            fork.append(format!("entry-{index}").as_bytes())
                .expect("append");
        }
        let grown_fork = fork.head(&signer).expect("head");
        let fork_proof = fork.consistency(pin.size).expect("proof");
        assert_eq!(
            advance(Some(&pin), &grown_fork, &fork_proof),
            Err(Refusal::Unproven)
        );
    }

    #[test]
    fn the_empty_chronicle_is_a_pinnable_prefix_of_everything() {
        let signer = seed(5);
        let empty = Chronicle::new();
        assert_eq!(empty.size(), 0);
        let bare = empty.head(&signer).expect("head");
        bare.verify().expect("verifies");
        assert_eq!(advance(None, &bare, &[]), Ok(Advance::Pinned));
        let pin = PinnedHead::from(&bare);
        let grown = filled(6).head(&signer).expect("head");
        assert_eq!(advance(Some(&pin), &grown, &[]), Ok(Advance::Extended));
    }

    #[test]
    fn bounds_hold_at_the_edges() {
        let mut log = Chronicle::new();
        let oversize = vec![0u8; MAX_ENTRY_BYTES + 1];
        assert_eq!(log.append(&oversize), Err(Refusal::Bound("entry bytes")));
        assert_eq!(log.inclusion(0), Err(Refusal::OutOfRange));
        assert_eq!(log.consistency(0), Err(Refusal::OutOfRange));
        assert_eq!(log.consistency(1), Err(Refusal::OutOfRange));
        let long = vec![[0u8; 32]; MAX_PROOF_HASHES + 1];
        assert_eq!(
            verify_inclusion(&[0u8; 32], 0, 2, &[0u8; 32], &long),
            Err(Refusal::Bound("inclusion path hashes"))
        );
        assert_eq!(
            verify_consistency(1, &[0u8; 32], 2, &[0u8; 32], &long),
            Err(Refusal::Bound("consistency path hashes"))
        );
    }
}
