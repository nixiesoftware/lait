//! The write gateway's authority check — slice 3 of daemon-less hosting.
//!
//! The bucket is a dumb, public-read transport; a *conditional* write in front
//! of it is the one piece of trusted infrastructure the whole design needs, and
//! it is deliberately tiny. It decides one question — *may this device replace
//! this Space's snapshot?* — and it answers it from the bucket's own history,
//! not from any minted token or shared secret:
//!
//! - the request carries a device signature over `(space, expected_generation,
//!   blob_digest)`, so a body swapped in flight or a stale generation replayed
//!   is caught before any authority reasoning;
//! - the CURRENT snapshot's own signed ledger says who may write the NEXT one.
//!   The gateway replays that ledger's effects — signed history it can verify
//!   without holding a single key — resolves the signing device to an actor,
//!   and admits the write only if that actor is a writing member.
//!
//! The gateway therefore holds no key, reads no plaintext, and mints no
//! credential: it replays public, signed effects exactly as a peer would, and
//! its authority is the same authority every replica already trusts. The bucket
//! stays untrusted on the read side (validation still runs on every downloaded
//! blob) and becomes conditionally trusted on the write side by this check plus
//! GCS's generation-match precondition — which is what serializes two racing
//! writers into a read-merge-write retry rather than a lost update.
//!
//! **Bootstrap.** The first write (no current snapshot) is authorized against
//! the NEW blob's own ledger: the founder is a member of the Space they
//! founded, and the object key is a capability nobody without the ticket can
//! address, so a founder's first PUT stands up the Space and nobody else's
//! forged genesis lands anywhere a real reader looks.

use mechanics::ids::{DeviceId, SpaceId};
use mechanics::space::{Authority as Ledger, Effect};

use crate::authority::AuthorityRecord;
use crate::snapshot::SpaceSnapshot;

/// The domain tag bound into every write-request signature, so a signature
/// minted for one purpose can never be replayed as another.
const WRITE_DOMAIN: &[u8] = b"lait/snapshot-write/1";

/// A signed intent to replace a Space's snapshot at a known generation. The
/// signature covers everything that matters — which Space, replacing which
/// generation, and the exact bytes — so the gateway authorizes the intent, not
/// merely the connection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SnapshotWriteRequest {
    /// The Space whose snapshot this replaces. Must equal the blob's own
    /// genesis and the object the gateway is about to write.
    pub space: SpaceId,
    /// The GCS generation this write expects to replace: `0` to CREATE (the
    /// object must not yet exist), else the exact current generation. The
    /// gateway forwards it as GCS's `ifGenerationMatch` precondition, so a
    /// racing writer gets a 412 and re-reads rather than clobbering.
    pub expected_generation: u64,
    /// blake3 of the snapshot bytes this request authorizes. Binds the
    /// signature to the exact body; a substituted blob no longer verifies.
    pub blob_digest: [u8; 32],
    /// The device asking to write — the ed25519 key whose signature this
    /// carries, resolved to an actor against the prior snapshot's ledger.
    pub device: DeviceId,
    /// ed25519 signature over the canonical preimage.
    pub signature: Vec<u8>,
}

/// What a client PUTs to the gateway: the signed intent and the exact blob it
/// authorizes, together, so the gateway never has to trust that a body matched
/// a separately-carried header. One postcard object on the wire.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WriteEnvelope {
    pub request: SnapshotWriteRequest,
    pub blob: Vec<u8>,
}

impl WriteEnvelope {
    /// Bound so a hostile PUT cannot force an unbounded decode before the size
    /// is known: the blob alone is capped at [`crate::snapshot::MAX_SNAPSHOT_BYTES`],
    /// and the envelope's framing is small beside it.
    pub const MAX_BYTES: usize = crate::snapshot::MAX_SNAPSHOT_BYTES + 4096;

    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("encode write envelope")
    }

    /// Decode a PUT body, size-bounded before any allocation-heavy work.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > Self::MAX_BYTES {
            return Err(format!(
                "write envelope is {} bytes, over the {}-byte ceiling",
                bytes.len(),
                Self::MAX_BYTES
            ));
        }
        postcard::from_bytes(bytes).map_err(|e| format!("write envelope does not decode: {e}"))
    }
}

/// The domain tag separating the object-key derivation from every other hash
/// of a Space id.
const KEY_DOMAIN: &[u8] = b"lait/snapshot-key/1";

/// The bucket object key a Space's snapshot lives at — a CAPABILITY, not the
/// Space id. A public-read blob keyed by the bare Space id would expose the
/// membership graph and activity cadence of any Space whose id could be
/// guessed (the ledger effects inside are signed plaintext). Keyed by a
/// one-way digest of the id instead, the blob is locatable only by someone who
/// already holds the id — which the ticket carries and a passer-by does not.
///
/// Deterministic, so the writing client and the gateway derive the identical
/// key from the same Space with no shared table: the gateway recomputes it from
/// the PUT's declared Space and refuses any write whose path does not match,
/// which is what stops a write for one Space landing on another's object.
pub fn object_key(space: &SpaceId) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(KEY_DOMAIN);
    hasher.update(space.as_str().as_bytes());
    let digest = hasher.finalize();
    let name = data_encoding::BASE32_NOPAD.encode(&digest.as_bytes()[..20]);
    format!("spaces/{name}.snap")
}

/// Why a write was refused. Every variant is a fact the caller (or an operator
/// reading a log) can act on — never a bare "denied".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteDenial {
    /// `blob_digest` does not match the bytes presented. The body was
    /// substituted after signing, or the client hashed the wrong thing.
    DigestMismatch,
    /// The signature does not verify under `device`'s key over the preimage.
    BadSignature,
    /// The request's `space` does not match the blob's own genesis, or the
    /// blob does not decode. The gateway will not write a Space's object with
    /// another Space's (or a corrupt) body.
    SpaceMismatch,
    /// The prior snapshot did not decode or replay. Its authority is
    /// unreadable, so no write against it can be authorized.
    PriorUnreadable(String),
    /// The signing device resolves to no actor on the governing ledger.
    UnknownDevice,
    /// The device's actor is not a writing member of the Space. (A viewer, a
    /// removed member, or an actor that never held write standing.)
    NotAWriter,
}

impl std::fmt::Display for WriteDenial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DigestMismatch => write!(f, "the blob does not match its signed digest"),
            Self::BadSignature => write!(f, "the write signature does not verify"),
            Self::SpaceMismatch => write!(f, "the blob is not this Space's, or does not decode"),
            Self::PriorUnreadable(why) => write!(f, "the prior snapshot is unreadable: {why}"),
            Self::UnknownDevice => write!(f, "the signing device is not on the Space's ledger"),
            Self::NotAWriter => write!(f, "the signing device may not write this Space"),
        }
    }
}

impl std::error::Error for WriteDenial {}

/// The bytes a write signature covers: the domain, the 29-byte rendered Space
/// id, the little-endian generation, and the blob digest. Fixed-width and
/// order-fixed, so there is exactly one preimage per `(space, generation,
/// blob)` and no ambiguity to exploit.
fn write_preimage(space: &SpaceId, expected_generation: u64, blob_digest: &[u8; 32]) -> Vec<u8> {
    let space_bytes = space.as_str().as_bytes();
    let mut preimage = Vec::with_capacity(WRITE_DOMAIN.len() + space_bytes.len() + 8 + 32);
    preimage.extend_from_slice(WRITE_DOMAIN);
    preimage.extend_from_slice(space_bytes);
    preimage.extend_from_slice(&expected_generation.to_le_bytes());
    preimage.extend_from_slice(blob_digest);
    preimage
}

/// Sign a write request as `seed`'s device. The device is derived from the
/// seed, so a request always names the key that actually signed it.
pub fn sign_write(
    seed: &[u8; 32],
    space: &SpaceId,
    expected_generation: u64,
    blob: &[u8],
) -> SnapshotWriteRequest {
    let blob_digest = *blake3::hash(blob).as_bytes();
    let preimage = write_preimage(space, expected_generation, &blob_digest);
    let signing = ed25519_dalek::SigningKey::from_bytes(seed);
    let signature = ed25519_dalek::Signer::sign(&signing, &preimage);
    SnapshotWriteRequest {
        space: space.clone(),
        expected_generation,
        blob_digest,
        device: mechanics::actor::device_from_seed(seed),
        signature: signature.to_bytes().to_vec(),
    }
}

/// Replay a snapshot's signed effects into an in-memory ledger and hand back
/// its ACL state and actor directory — the governing authority, reconstructed
/// from public bytes with no key and no plaintext. Sealed keys and the World
/// bodies are irrelevant to *who may write* and are never touched.
fn govern(
    snapshot: &SpaceSnapshot,
) -> Result<(mechanics::membership::AclState, ActorPlane), String> {
    let mut ledger = Ledger::create_on(
        std::sync::Arc::new(journal::MemMedium::new()),
        snapshot.genesis.clone(),
    )
    .map_err(|f| format!("create ledger: {f:?}"))?;
    let founder: mechanics::actor::SignedEvent = postcard::from_bytes(&snapshot.founder_inception)
        .map_err(|e| format!("founder inception: {e}"))?;
    ledger
        .commit_batch(&[Effect::Actor(founder).encode()], &[])
        .map_err(|f| format!("commit founder: {f:?}"))?;

    // The authority section interleaves signed effects with body transactions;
    // `decode_canonical` demands exact re-encode equality, so it separates the
    // two the same way `validate_contact` does. Only effects govern writing.
    let mut effects = Vec::new();
    for record in &snapshot.staged.authority_records {
        if replica::transaction::Transaction::decode_canonical(record).is_ok() {
            continue;
        }
        if let Some(AuthorityRecord::Effect(bytes)) = AuthorityRecord::decode(record) {
            effects.push(bytes);
        }
    }
    ledger
        .commit_batch(&effects, &[])
        .map_err(|f| format!("commit effects: {f:?}"))?;

    let acl = ledger
        .acl_state()
        .map_err(|f| format!("acl state: {f:?}"))?;
    let plane = ActorPlane {
        space: snapshot.genesis.space_id.clone(),
        events: ledger.actor_events(),
    };
    Ok((acl, plane))
}

/// A snapshot's actor directory, kept as its signed events so the resolution
/// is a pure replay a test can reproduce without the ledger.
struct ActorPlane {
    space: SpaceId,
    events: Vec<mechanics::actor::SignedEvent>,
}

impl ActorPlane {
    fn actor_of(&self, device: &DeviceId) -> Option<mechanics::ids::ActorId> {
        mechanics::actor::replay(&self.space, &self.events)
            .actor_of_device(device)
            .cloned()
    }
}

/// Decide whether `request` may replace this Space's snapshot with `blob`.
///
/// `prior` is the CURRENT snapshot bytes the bucket holds, or `None` on a
/// create (generation 0). The check, in order — each step a different, named
/// refusal:
///
/// 1. `blob` hashes to the signed `blob_digest` (the body is the signed body);
/// 2. the signature verifies under `device` over the `(space, generation,
///    blob_digest)` preimage (the intent is authentic);
/// 3. `blob` decodes and its genesis Space equals `request.space` (the body is
///    this Space's);
/// 4. the governing ledger — the PRIOR snapshot's, or the NEW blob's on create
///    — resolves `device` to an actor that is a writing member.
///
/// Returns `Ok(())` to authorize the conditional PUT; the caller still applies
/// the `expected_generation` precondition at the bucket, which is what makes
/// concurrent writers retry rather than clobber.
pub fn authorize_write(
    prior: Option<&[u8]>,
    request: &SnapshotWriteRequest,
    blob: &[u8],
) -> Result<(), WriteDenial> {
    // 1. The bytes are the signed bytes.
    if *blake3::hash(blob).as_bytes() != request.blob_digest {
        return Err(WriteDenial::DigestMismatch);
    }

    // 2. The signature is authentic for the named device over the exact intent.
    let preimage = write_preimage(
        &request.space,
        request.expected_generation,
        &request.blob_digest,
    );
    let key_bytes = request
        .device
        .key_bytes()
        .ok_or(WriteDenial::UnknownDevice)?;
    let verifying = ed25519_dalek::VerifyingKey::from_bytes(&key_bytes)
        .map_err(|_| WriteDenial::BadSignature)?;
    let signature_bytes: [u8; 64] = request
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| WriteDenial::BadSignature)?;
    let signature = ed25519_dalek::Signature::from_bytes(&signature_bytes);
    ed25519_dalek::Verifier::verify(&verifying, &preimage, &signature)
        .map_err(|_| WriteDenial::BadSignature)?;

    // 3. The incoming blob is this Space's, and decodes.
    let incoming = SpaceSnapshot::decode(blob).map_err(|_| WriteDenial::SpaceMismatch)?;
    if incoming.genesis.space_id != request.space {
        return Err(WriteDenial::SpaceMismatch);
    }

    // 4. The governing ledger says the signer may write.
    //    Create: the blob's own ledger (the founder is a member of it).
    //    Replace: the PRIOR snapshot's ledger — the current members decide the
    //    next generation, so a write cannot admit its own author.
    let governing = match prior {
        None => incoming,
        Some(prior_bytes) => SpaceSnapshot::decode(prior_bytes)
            .map_err(|e| WriteDenial::PriorUnreadable(e.to_string()))?,
    };
    // A prior snapshot for a different Space is not this Space's authority.
    if governing.genesis.space_id != request.space {
        return Err(WriteDenial::SpaceMismatch);
    }
    let (acl, plane) = govern(&governing).map_err(WriteDenial::PriorUnreadable)?;
    let actor = plane
        .actor_of(&request.device)
        .ok_or(WriteDenial::UnknownDevice)?;
    if !acl.can_write(&actor) {
        return Err(WriteDenial::NotAWriter);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mechanics::membership::{self as acl, AclAction, AclOp, Standing};
    use mechanics::space::{derive_space_id, mint_recovery_key, recovery_commit};
    use replica::convergence::StagedContactMaterial;

    /// A member of a freshly minted Space, and the material to sign as them.
    struct Member {
        seed: [u8; 32],
        actor: mechanics::ids::ActorId,
        inception: mechanics::actor::SignedEvent,
    }

    /// Mint a real Space founded by `founder`, admitting each of `members` as a
    /// writing contributor, and return the SNAPSHOT bytes plus the founder.
    /// Every effect is a genuine signed event replayed exactly as the gateway
    /// replays it — no shortcut through unsigned state.
    fn mint_space(founder_seed: [u8; 32], members: &[[u8; 32]]) -> (Vec<u8>, SpaceId, Vec<Member>) {
        let founder_device = mechanics::actor::device_from_seed(&founder_seed);
        let salt = [7u8; 16];
        let (recovery_pub, _) = mint_recovery_key().expect("recovery key");
        let recovery_root = recovery_commit(&recovery_pub).expect("recovery commit");
        let space = derive_space_id(&founder_device, &salt, &recovery_root);

        let (founder_inception, founder_actor) =
            mechanics::actor::incept_single(&founder_seed, &space, [1u8; 16], [2u8; 16], None);
        let genesis = mechanics::space::Genesis {
            space_id: space.clone(),
            founding_actors: vec![founder_actor.clone()],
            salt,
            recovery_root,
        };

        let mut ledger = Ledger::create_on(
            std::sync::Arc::new(journal::MemMedium::new()),
            genesis.clone(),
        )
        .expect("ledger");
        ledger
            .commit_batch(&[Effect::Actor(founder_inception.clone()).encode()], &[])
            .expect("founder inception");

        let mut admitted = Vec::new();
        for (i, seed) in members.iter().enumerate() {
            let n = (i as u8) + 10;
            let (inception, actor) =
                mechanics::actor::incept_single(seed, &space, [n; 16], [n + 1; 16], None);
            let add = acl::sign_op(
                &founder_seed,
                &AclOp {
                    action: AclAction::AddMember {
                        actor: actor.clone(),
                        grants: vec![Standing::Write],
                    },
                    by: founder_actor.clone(),
                    actor_asof: ledger.actor_heads(&founder_actor),
                    nonce: None,
                },
                vec![],
                &space,
            );
            ledger
                .commit_batch(
                    &[
                        Effect::Actor(inception.clone()).encode(),
                        Effect::Acl(add).encode(),
                    ],
                    &[],
                )
                .expect("admit member");
            admitted.push(Member {
                seed: *seed,
                actor,
                inception,
            });
        }

        // Build the snapshot the gateway sees: genesis + founder inception +
        // the whole ledger's effects as authority records. Bodies/manifest are
        // empty — authorization reads none of them.
        let mut authority_records = Vec::new();
        for effect in ledger.export_effects() {
            authority_records.push(AuthorityRecord::Effect(effect).encode());
        }
        let snapshot = SpaceSnapshot {
            genesis,
            founder_inception: postcard::to_stdvec(&founder_inception).expect("encode founder"),
            staged: StagedContactMaterial {
                authority_records,
                manifest_root_bytes: Vec::new(),
                manifest_nodes: Vec::new(),
                bodies: Vec::new(),
            },
        };
        (snapshot.encode(), space, admitted)
    }

    #[test]
    fn a_founder_may_create_the_first_generation() {
        let founder = [3u8; 32];
        let (blob, space, _) = mint_space(founder, &[]);
        let request = sign_write(&founder, &space, 0, &blob);
        // Create: no prior snapshot; the founder is authorized by the blob's
        // own ledger.
        assert_eq!(authorize_write(None, &request, &blob), Ok(()));
    }

    #[test]
    fn an_admitted_member_may_replace_a_later_generation() {
        let founder = [3u8; 32];
        let member = [4u8; 32];
        let (prior, space, _) = mint_space(founder, &[member]);
        // The member signs to replace generation 1 with a valid same-Space
        // body. The gateway authorizes the WRITER against the prior generation;
        // that the body's own ledger also admits them is beside the point.
        let request = sign_write(&member, &space, 1, &prior);
        assert_eq!(authorize_write(Some(&prior), &request, &prior), Ok(()));
    }

    #[test]
    fn a_stranger_may_not_write() {
        let founder = [3u8; 32];
        let stranger = [9u8; 32];
        let (prior, space, _) = mint_space(founder, &[]);
        // The stranger's device is on no ledger the prior snapshot carries.
        let request = sign_write(&stranger, &space, 1, &prior);
        assert_eq!(
            authorize_write(Some(&prior), &request, &prior),
            Err(WriteDenial::UnknownDevice)
        );
    }

    #[test]
    fn a_removed_member_may_not_write() {
        // A member admitted then removed resolves to an actor, but not a
        // writing one — the distinct NotAWriter refusal, not UnknownDevice.
        let founder_seed = [3u8; 32];
        let member_seed = [4u8; 32];
        let founder_device = mechanics::actor::device_from_seed(&founder_seed);
        let salt = [7u8; 16];
        let (recovery_pub, _) = mint_recovery_key().unwrap();
        let recovery_root = recovery_commit(&recovery_pub).unwrap();
        let space = derive_space_id(&founder_device, &salt, &recovery_root);
        let (founder_inception, founder_actor) =
            mechanics::actor::incept_single(&founder_seed, &space, [1u8; 16], [2u8; 16], None);
        let genesis = mechanics::space::Genesis {
            space_id: space.clone(),
            founding_actors: vec![founder_actor.clone()],
            salt,
            recovery_root,
        };
        let mut ledger = Ledger::create_on(
            std::sync::Arc::new(journal::MemMedium::new()),
            genesis.clone(),
        )
        .unwrap();
        ledger
            .commit_batch(&[Effect::Actor(founder_inception.clone()).encode()], &[])
            .unwrap();
        let (member_inception, member_actor) =
            mechanics::actor::incept_single(&member_seed, &space, [10u8; 16], [11u8; 16], None);
        let add = acl::sign_op(
            &founder_seed,
            &AclOp {
                action: AclAction::AddMember {
                    actor: member_actor.clone(),
                    grants: vec![Standing::Write],
                },
                by: founder_actor.clone(),
                actor_asof: ledger.actor_heads(&founder_actor),
                nonce: None,
            },
            vec![],
            &space,
        );
        ledger
            .commit_batch(
                &[
                    Effect::Actor(member_inception).encode(),
                    Effect::Acl(add).encode(),
                ],
                &[],
            )
            .unwrap();
        let remove = acl::sign_op(
            &founder_seed,
            &AclOp {
                action: AclAction::RemoveMember {
                    actor: member_actor.clone(),
                },
                by: founder_actor.clone(),
                actor_asof: ledger.actor_heads(&founder_actor),
                nonce: None,
            },
            vec![],
            &space,
        );
        ledger
            .commit_batch(&[Effect::Acl(remove).encode()], &[])
            .unwrap();

        let mut authority_records = Vec::new();
        for effect in ledger.export_effects() {
            authority_records.push(AuthorityRecord::Effect(effect).encode());
        }
        let prior = SpaceSnapshot {
            genesis,
            founder_inception: postcard::to_stdvec(&founder_inception).unwrap(),
            staged: StagedContactMaterial {
                authority_records,
                manifest_root_bytes: Vec::new(),
                manifest_nodes: Vec::new(),
                bodies: Vec::new(),
            },
        }
        .encode();
        let request = sign_write(&member_seed, &space, 1, &prior);
        assert_eq!(
            authorize_write(Some(&prior), &request, &prior),
            Err(WriteDenial::NotAWriter)
        );
    }

    #[test]
    fn a_substituted_body_fails_the_digest() {
        let founder = [3u8; 32];
        let (blob, space, _) = mint_space(founder, &[]);
        let mut request = sign_write(&founder, &space, 0, &blob);
        // The signature stands, but the digest names bytes we no longer hold.
        request.blob_digest = [0u8; 32];
        assert_eq!(
            authorize_write(None, &request, &blob),
            Err(WriteDenial::DigestMismatch)
        );
    }

    #[test]
    fn a_tampered_signature_is_refused() {
        let founder = [3u8; 32];
        let (blob, space, _) = mint_space(founder, &[]);
        let mut request = sign_write(&founder, &space, 0, &blob);
        request.signature[0] ^= 0xff;
        assert_eq!(
            authorize_write(None, &request, &blob),
            Err(WriteDenial::BadSignature)
        );
    }

    #[test]
    fn the_signature_is_bound_to_its_generation() {
        // A request signed to replace gen 1 cannot be replayed against gen 2:
        // the generation is in the preimage.
        let founder = [3u8; 32];
        let (blob, space, _) = mint_space(founder, &[]);
        let request = sign_write(&founder, &space, 1, &blob);
        let mut replayed = request.clone();
        replayed.expected_generation = 2;
        assert_eq!(
            authorize_write(Some(&blob), &replayed, &blob),
            Err(WriteDenial::BadSignature)
        );
    }

    #[test]
    fn a_blob_for_another_space_is_refused() {
        let founder = [3u8; 32];
        let (blob_a, space_a, _) = mint_space(founder, &[]);
        let (blob_b, _space_b, _) = mint_space([5u8; 32], &[]);
        // Sign a write to Space A's object, but present Space B's body.
        let request = sign_write(&founder, &space_a, 0, &blob_b);
        assert_eq!(
            authorize_write(None, &request, &blob_b),
            Err(WriteDenial::SpaceMismatch)
        );
    }
}
