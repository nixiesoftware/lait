//! The gateway's request logic, over an abstract object store — so the whole
//! decision (authorize, then conditional-write) is proven against an in-memory
//! store with no network, and the GCS binding is a thin, separately-tested
//! adapter.
//!
//! One question, answered from the bucket's own history: *may this device
//! replace this Space's snapshot, and is it replacing the generation it thinks
//! it is?* The first half is [`contact::gateway::authorize_write`]; the second
//! is the store's generation-match precondition, which turns two racing writers
//! into a 412-and-retry rather than a lost update.

use std::sync::Arc;

use contact::gateway::{authorize_write, WriteDenial, WriteEnvelope};

/// One stored object's bytes and its generation — the version token a
/// conditional write must match. `0` is the sentinel for "does not exist".
#[derive(Debug, Clone)]
pub struct Stored {
    pub bytes: Vec<u8>,
    pub generation: u64,
}

/// Why a conditional write did not land.
#[derive(Debug)]
pub enum PutError {
    /// The object's generation is not the one the write expected — a concurrent
    /// writer got there first. Carries the CURRENT generation so a caller can
    /// re-read, merge, and retry.
    Conflict { current: u64 },
    /// The store itself failed (I/O, auth, transport).
    Store(String),
}

/// The object store the gateway writes through. Reads may be public and
/// unconditional; writes MUST honor `expected_generation` atomically, or the
/// whole no-lost-update guarantee collapses.
pub trait ObjectStore: Send + Sync {
    /// The current object at `key`, or `None` if it does not exist.
    fn read(&self, key: &str) -> Result<Option<Stored>, String>;
    /// Write `bytes` at `key` only if its current generation equals
    /// `expected_generation` (`0` meaning "must not yet exist"). Returns the
    /// new generation on success, `Conflict` if the precondition failed.
    fn put_if_generation(
        &self,
        key: &str,
        bytes: &[u8],
        expected_generation: u64,
    ) -> Result<u64, PutError>;
}

/// The gateway's answer to a PUT — mapped to an HTTP status by the shell.
#[derive(Debug)]
pub enum WriteOutcome {
    /// Written. Carries the new generation, which the client records for its
    /// next write's precondition.
    Accepted { generation: u64 },
    /// Authorized, but the generation moved under it: re-read and retry.
    Conflict { current: u64 },
    /// The request is not authorized to write this Space.
    Denied(WriteDenial),
    /// The request body is malformed (undecodable, oversized, or its `space`
    /// does not match the object it was sent to).
    BadRequest(String),
    /// The store failed independently of the request.
    Unavailable(String),
}

/// Apply one PUT: decode the envelope, confirm it is for this object's Space,
/// authorize it against the object's CURRENT snapshot, then write under the
/// generation-match precondition.
///
/// `key` names the object; `expected_space` is the Space that object belongs to
/// (the shell derives both from the capability path). The envelope's own
/// `request.space` must equal `expected_space`, so a signed write for one Space
/// cannot be redirected onto another Space's object.
pub fn apply_write(
    store: &dyn ObjectStore,
    key: &str,
    expected_space: &mechanics::ids::SpaceId,
    body: &[u8],
) -> WriteOutcome {
    let envelope = match WriteEnvelope::decode(body) {
        Ok(envelope) => envelope,
        Err(why) => return WriteOutcome::BadRequest(why),
    };
    if &envelope.request.space != expected_space {
        return WriteOutcome::BadRequest(format!(
            "the write is for Space {}, not the object's Space {}",
            envelope.request.space.as_str(),
            expected_space.as_str()
        ));
    }

    let current = match store.read(key) {
        Ok(current) => current,
        Err(why) => return WriteOutcome::Unavailable(why),
    };
    // The generation the client signed against MUST be the one actually in the
    // bucket: authorizing a write against a prior the client did not see would
    // let a stale read pass a fresh precondition. Mismatch is a conflict, not a
    // denial — the client re-reads and re-signs.
    let current_generation = current.as_ref().map(|s| s.generation).unwrap_or(0);
    if envelope.request.expected_generation != current_generation {
        return WriteOutcome::Conflict {
            current: current_generation,
        };
    }

    let prior = current.as_ref().map(|s| s.bytes.as_slice());
    if let Err(denial) = authorize_write(prior, &envelope.request, &envelope.blob) {
        return WriteOutcome::Denied(denial);
    }

    match store.put_if_generation(key, &envelope.blob, current_generation) {
        Ok(generation) => WriteOutcome::Accepted { generation },
        Err(PutError::Conflict { current }) => WriteOutcome::Conflict { current },
        Err(PutError::Store(why)) => WriteOutcome::Unavailable(why),
    }
}

/// Read the current snapshot at `key` — the public read half. The gateway does
/// not need to serve reads (the bucket/CDN does, directly), but exposing it
/// keeps a single-origin deployment and the tests honest.
pub fn read_snapshot(store: &dyn ObjectStore, key: &str) -> Result<Option<Stored>, String> {
    store.read(key)
}

/// Shared gateway state for the axum handlers.
pub struct Gateway {
    pub store: Arc<dyn ObjectStore>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use contact::gateway::sign_write;
    use contact::snapshot::SpaceSnapshot;
    use mechanics::membership::{self as acl, AclAction, AclOp, Standing};
    use mechanics::space::{
        derive_space_id, mint_recovery_key, recovery_commit, Authority as Ledger, Effect,
    };
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// An in-memory generation-versioned store — the whole 412 discipline with
    /// no bucket. A put at the wrong generation conflicts; a successful put
    /// bumps the generation, exactly as GCS's `ifGenerationMatch` does.
    #[derive(Default)]
    struct MemStore(Mutex<HashMap<String, Stored>>);

    impl ObjectStore for MemStore {
        fn read(&self, key: &str) -> Result<Option<Stored>, String> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        fn put_if_generation(
            &self,
            key: &str,
            bytes: &[u8],
            expected_generation: u64,
        ) -> Result<u64, PutError> {
            let mut map = self.0.lock().unwrap();
            let current = map.get(key).map(|s| s.generation).unwrap_or(0);
            if current != expected_generation {
                return Err(PutError::Conflict { current });
            }
            let generation = current + 1;
            map.insert(
                key.to_string(),
                Stored {
                    bytes: bytes.to_vec(),
                    generation,
                },
            );
            Ok(generation)
        }
    }

    /// A minimal real Space founded by `founder_seed`, admitting `members` as
    /// writing contributors — the same construction the gateway crate's own
    /// tests use, minted here so the whole PUT path runs on genuine signed
    /// material.
    fn mint_space(
        founder_seed: [u8; 32],
        members: &[[u8; 32]],
    ) -> (Vec<u8>, mechanics::ids::SpaceId) {
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
        let mut ledger =
            Ledger::create_on(Arc::new(journal::MemMedium::new()), genesis.clone()).unwrap();
        ledger
            .commit_batch(&[Effect::Actor(founder_inception.clone()).encode()], &[])
            .unwrap();
        for (i, seed) in members.iter().enumerate() {
            let n = (i as u8) + 10;
            let (inception, actor) =
                mechanics::actor::incept_single(seed, &space, [n; 16], [n + 1; 16], None);
            let add = acl::sign_op(
                &founder_seed,
                &AclOp {
                    action: AclAction::AddMember {
                        actor,
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
                    &[Effect::Actor(inception).encode(), Effect::Acl(add).encode()],
                    &[],
                )
                .unwrap();
        }
        let mut authority_records = Vec::new();
        for effect in ledger.export_effects() {
            authority_records.push(contact::authority::AuthorityRecord::Effect(effect).encode());
        }
        let snapshot = SpaceSnapshot {
            genesis,
            founder_inception: postcard::to_stdvec(&founder_inception).unwrap(),
            staged: replica::convergence::StagedContactMaterial {
                authority_records,
                manifest_root_bytes: Vec::new(),
                manifest_nodes: Vec::new(),
                bodies: Vec::new(),
            },
        };
        (snapshot.encode(), space)
    }

    fn envelope(
        seed: &[u8; 32],
        space: &mechanics::ids::SpaceId,
        generation: u64,
        blob: &[u8],
    ) -> Vec<u8> {
        WriteEnvelope {
            request: sign_write(seed, space, generation, blob),
            blob: blob.to_vec(),
        }
        .encode()
    }

    #[test]
    fn the_founders_first_write_creates_generation_one() {
        let founder = [3u8; 32];
        let (blob, space) = mint_space(founder, &[]);
        let store = MemStore::default();
        let key = "spaces/cap.snap";
        let outcome = apply_write(&store, key, &space, &envelope(&founder, &space, 0, &blob));
        assert!(
            matches!(outcome, WriteOutcome::Accepted { generation: 1 }),
            "{outcome:?}"
        );
        assert_eq!(store.read(key).unwrap().unwrap().generation, 1);
    }

    #[test]
    fn a_stale_generation_conflicts_and_names_the_current_one() {
        let founder = [3u8; 32];
        let (blob, space) = mint_space(founder, &[]);
        let store = MemStore::default();
        let key = "spaces/cap.snap";
        // First write lands (gen 0 -> 1).
        apply_write(&store, key, &space, &envelope(&founder, &space, 0, &blob));
        // A second write that still thinks it is replacing gen 0 conflicts, and
        // is told the generation actually there.
        let outcome = apply_write(&store, key, &space, &envelope(&founder, &space, 0, &blob));
        assert!(
            matches!(outcome, WriteOutcome::Conflict { current: 1 }),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_re_read_at_the_current_generation_lands() {
        let founder = [3u8; 32];
        let (blob, space) = mint_space(founder, &[]);
        let store = MemStore::default();
        let key = "spaces/cap.snap";
        apply_write(&store, key, &space, &envelope(&founder, &space, 0, &blob));
        // The retry re-reads (gen 1) and re-signs against it.
        let outcome = apply_write(&store, key, &space, &envelope(&founder, &space, 1, &blob));
        assert!(
            matches!(outcome, WriteOutcome::Accepted { generation: 2 }),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_stranger_is_denied_not_conflicted() {
        let founder = [3u8; 32];
        let stranger = [9u8; 32];
        let (blob, space) = mint_space(founder, &[]);
        let store = MemStore::default();
        let key = "spaces/cap.snap";
        apply_write(&store, key, &space, &envelope(&founder, &space, 0, &blob));
        // The stranger reads gen 1 honestly (no conflict) but cannot write it.
        let outcome = apply_write(&store, key, &space, &envelope(&stranger, &space, 1, &blob));
        assert!(matches!(outcome, WriteOutcome::Denied(_)), "{outcome:?}");
        // And the object is unchanged.
        assert_eq!(store.read(key).unwrap().unwrap().generation, 1);
    }

    #[test]
    fn a_write_for_another_space_is_a_bad_request() {
        let founder = [3u8; 32];
        let (blob, space) = mint_space(founder, &[]);
        let (_other_blob, other_space) = mint_space([5u8; 32], &[]);
        let store = MemStore::default();
        // The envelope is signed for `space`, but sent to `other_space`'s object.
        let outcome = apply_write(
            &store,
            "spaces/other.snap",
            &other_space,
            &envelope(&founder, &space, 0, &blob),
        );
        assert!(
            matches!(outcome, WriteOutcome::BadRequest(_)),
            "{outcome:?}"
        );
    }
}
