//! A whole Space as one portable object — the substrate of daemon-less,
//! CDN-backed hosting.
//!
//! A live Contact moves a Space's material peer-to-peer over QUIC; this module
//! moves the *same* material as a serialized blob a bucket can hold, so a
//! member's device reloads with no daemon and no live peer. It is the serve
//! side of a Contact ([`capture`], = `build_outbound` off the wire) and the
//! initiator's tail ([`restore`], = `pull_whole` with the read replaced by
//! "download the blob"). Validation still runs on the untrusted bytes inside
//! [`Replica::validate_contact`], so the bucket stays a dumb, untrusted
//! transport — exactly like the live peer it replaces.
//!
//! What a member cold-reload needs, and why the snapshot carries all of it:
//! - the **ledger** (ACL/membership/epochs and the sealed epoch-key envelopes)
//!   — carried as the staged material's `authority_records`, produced by the
//!   FULL responder export ([`Ledger::export_effects`]/`export_sealed`/
//!   `export_ceremony`), NOT the joiner-side `LedgerAuthority::export_records`
//!   (which is empty for a member). Restore `commit_batch`es them and
//!   `refresh_keyring` unseals the keys addressed to this device.
//! - the **World** — `Replica::export_material` (retained material only, so the
//!   snapshot is already compacted) + the signed manifest.
//! - the **genesis** header — the one thing the staged material cannot carry: a
//!   cold reader needs `Genesis` to `create_on` a ledger before it can absorb
//!   anything.

use std::sync::Arc;

use mechanics::ids::SpaceId;
use mechanics::space::{Authority as Ledger, Effect, Genesis};
use replica::convergence::StagedContactMaterial;
use replica::transaction::{CommitContext, SeedSigner};
use replica::Replica;

use crate::authority::{AuthorityRecord, LedgerAuthority, SharedLedgerAuthority};
use crate::protocol::Failure;

/// The most a decoded snapshot may be before validation runs. A live Contact
/// caps frames and per-transaction change counts; a raw bucket blob has no such
/// bound, so an unbounded decode is a memory bomb (adversary R4). Bounded here,
/// before any allocation-heavy validation.
pub const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

/// A whole Space, portable: everything a member's device needs to stand the
/// Space back up from cold, and nothing a daemon owns.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SpaceSnapshot {
    /// The founding anchor a cold reader `create_on`s its fresh ledger with.
    pub genesis: Genesis,
    /// The founder's canonical inception (`SignedEvent` bytes), committed
    /// first, exactly as the live enter does.
    pub founder_inception: Vec<u8>,
    /// The ledger + World material, byte-identical to what a live peer serves.
    pub staged: StagedContactMaterial,
}

impl SpaceSnapshot {
    /// Serialize for the bucket.
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("encode space snapshot")
    }

    /// Decode a bucket blob, size-bounded before the untrusted bytes reach any
    /// validation (R4).
    pub fn decode(bytes: &[u8]) -> Result<Self, Failure> {
        if bytes.len() > MAX_SNAPSHOT_BYTES {
            return Err(Failure::Protocol(format!(
                "snapshot is {} bytes, over the {MAX_SNAPSHOT_BYTES}-byte ceiling",
                bytes.len()
            )));
        }
        postcard::from_bytes(bytes)
            .map_err(|e| Failure::Protocol(format!("snapshot does not decode: {e}")))
    }
}

/// Capture a Space into a portable snapshot — the serve side of a Contact, off
/// the wire. Takes the raw `ledger` + `frontier` a member store already holds
/// (daemon `SpaceAuthority` or browser `LedgerAuthority` alike), so it is not
/// tied to either wrapper. The authority section is the FULL export every time
/// (adversary R2: a partial authority capture would break a later merge with
/// `MissingHistory`).
pub fn capture(
    space: &SpaceId,
    seed: &[u8; 32],
    genesis: Genesis,
    founder_inception: Vec<u8>,
    ledger: &Ledger,
    frontier: replica::frontier::AuthorityFrontier,
    replica: &Replica,
) -> Result<SpaceSnapshot, Failure> {
    let commit_ctx = CommitContext {
        space,
        signer: &SeedSigner(seed),
        authority_frontier: frontier,
    };
    let material = replica
        .export_material()
        .map_err(|f| Failure::Convergence(format!("export material: {f:?}")))?;
    let (root_bytes, nodes) = replica
        .export_manifest(&commit_ctx)
        .map_err(|f| Failure::Convergence(format!("export manifest: {f:?}")))?;

    // The whole ledger, as authority records — the responder's forward serve,
    // not the joiner's reverse push.
    let mut authority_records = Vec::new();
    for effect in ledger.export_effects() {
        authority_records.push(AuthorityRecord::Effect(effect).encode());
    }
    for sealed in ledger.export_sealed() {
        authority_records.push(AuthorityRecord::SealedKey(sealed).encode());
    }
    for ceremony in ledger.export_ceremony() {
        authority_records.push(AuthorityRecord::Ceremony(ceremony).encode());
    }
    let mut bodies = Vec::new();
    for (tx, closures) in &material {
        authority_records.push(tx.encode());
        for (key, artifact_pack) in closures {
            bodies.push((tx.id(), key.clone(), artifact_pack.clone()));
        }
    }

    Ok(SpaceSnapshot {
        genesis,
        founder_inception,
        staged: StagedContactMaterial {
            authority_records,
            manifest_root_bytes: root_bytes,
            manifest_nodes: nodes,
            bodies,
        },
    })
}

/// Restore a Space from a snapshot into fresh storage — the initiator's tail
/// with the download in place of a live pull. The device `seed` unseals the
/// epoch keys the ledger carries, so an already-admitted member reads and
/// decrypts; a device the Space never admitted gets a ledger with no key it can
/// open, which is the correct outcome (admission is a separate, later act).
pub fn restore(
    snapshot: SpaceSnapshot,
    seed: [u8; 32],
    ledger_medium: Arc<dyn journal::Medium>,
    replica_medium: Arc<dyn journal::Medium>,
) -> Result<(Replica, SharedLedgerAuthority), Failure> {
    let space = snapshot.genesis.space_id.clone();
    let mut ledger = Ledger::create_on(ledger_medium, snapshot.genesis)
        .map_err(|f| Failure::Convergence(format!("create ledger: {f:?}")))?;
    let founder: mechanics::actor::SignedEvent = postcard::from_bytes(&snapshot.founder_inception)
        .map_err(|e| Failure::Protocol(format!("founder inception does not decode: {e}")))?;
    ledger
        .commit_batch(&[Effect::Actor(founder).encode()], &[])
        .map_err(|f| Failure::Convergence(format!("commit founder inception: {f:?}")))?;

    let authority = SharedLedgerAuthority::new(LedgerAuthority::new(space.clone(), ledger, seed));
    let mut replica = Replica::open_on(replica_medium, Arc::new(authority.clone()))
        .map_err(|f| Failure::Convergence(format!("open replica: {f:?}")))?;

    // Commit the authority (which refreshes the keyring with the sealed epoch
    // keys the ledger carries) and incorporate the World material.
    //
    // KNOWN GAP (proven by porthole's verify_snapshot_roundtrip): a body whose
    // epoch key is not in the keyring at import lands OPAQUE and will not read
    // collaboratively — and re-import does not flip it. `refresh_keyring`
    // recovers the epochs sealed to this device, but a member that decrypts N
    // bodies live holds the epoch *history* (older keys, re-sealed across
    // rotations); recovering only the latest sealed epoch leaves older-epoch
    // bodies opaque. Closing this needs the full per-device epoch-key history in
    // the snapshot (or its recovery at restore) — the next slice.
    let bundle = authority.bundle();
    let validated = {
        let mut incorporator = bundle
            .incorporator
            .lock()
            .map_err(|_| Failure::Convergence("the incorporator lock is poisoned".into()))?;
        replica
            .validate_contact(&snapshot.staged, bundle.source.as_ref(), &mut *incorporator)
            .map_err(|f| Failure::Convergence(format!("validate snapshot: {f:?}")))?
    };
    let commit_ctx = CommitContext {
        space: &space,
        signer: &SeedSigner(&seed),
        authority_frontier: (bundle.frontier)(),
    };
    replica
        .incorporate_bundle(&commit_ctx, validated, bundle.source.as_ref())
        .map_err(|f| Failure::Convergence(format!("incorporate snapshot: {f:?}")))?;

    Ok((replica, authority))
}
