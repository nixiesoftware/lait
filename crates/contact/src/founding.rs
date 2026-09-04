//! Founding a Space, factored out of the browser so its ledger contract is
//! native-testable — the counterpart to [`crate::snapshot`]'s join/restore.
//!
//! A daemon-less founder (a bare `foundation.pub/i` visit) mints a Space in the
//! tab: it derives the Space identity from its own seed, incepts the founding
//! actor, mints epoch 0 sealed to its own device, and — the part that is easy to
//! get silently wrong — activates the World and seeds the founder's declared
//! grants. Without that activation the World is inert: every request denies with
//! no epoch and no capability behind it, and the failure names nothing. The
//! whole sequence lives here as ledger operations over an in-memory-or-OPFS
//! `Authority`, so a native test can replay it and assert the resulting
//! authority — founder is admin, the World is active, the grants are effective,
//! the founder holds the epoch key — without a browser, OPFS, or a runner.
//!
//! Deterministic in the seed: the salt, the recovery commitments, and every
//! inception nonce derive from it, so a reload re-computes the SAME Space id and
//! reopens the store it already founded. One device seed founds one Space.

use mechanics::actor::SignedEvent;
use mechanics::authorization::SpaceKey;
use mechanics::ids::{ActorId, SpaceId};
use mechanics::membership::{self as acl, AclAction, AclOp, GrantOrigin};
use mechanics::space::{Authority as Ledger, Effect, Genesis};

/// One founder grant to seed at formation — a World-declared capability on a
/// resource, with the deterministic salt that makes its grant id reproducible.
pub struct FounderGrant {
    pub capability: mechanics::authorization::PolicyCapability,
    pub resource: mechanics::authorization::Resource,
    pub salt: [u8; 16],
}

/// The Space identity a seed founds — everything the genesis commits to, all
/// derived from the seed so it is stable across reloads. Pure: no I/O, no
/// ledger. `found_on_ledger` commits against a ledger created on this `genesis`.
pub struct FoundingIdentity {
    pub space: SpaceId,
    pub genesis: Genesis,
    pub founder_inception: SignedEvent,
    pub founder_actor: ActorId,
}

/// A domain-separated 16-byte nonce from `(seed, space)` — the same derivation
/// discipline the enter path uses, so every founding nonce is reproducible.
fn derive16(context: &str, seed: &[u8; 32], space: &SpaceId) -> [u8; 16] {
    let mut material = Vec::with_capacity(32 + space.as_str().len());
    material.extend_from_slice(seed);
    material.extend_from_slice(space.as_str().as_bytes());
    let full = blake3::derive_key(context, &material);
    let mut out = [0u8; 16];
    out.copy_from_slice(&full[..16]);
    out
}

/// Derive the deterministic Space identity a seed founds. Pure — a reload calls
/// this and gets the same Space id, which is what lets the founder reopen the
/// store it already holds rather than mint a second Space.
pub fn founding_identity(seed: &[u8; 32]) -> FoundingIdentity {
    let me = mechanics::actor::device_from_seed(seed);

    let salt: [u8; 16] = {
        let full = blake3::derive_key("lait.browser-found.salt.v1", seed);
        let mut s = [0u8; 16];
        s.copy_from_slice(&full[..16]);
        s
    };
    // The space recovery key and the actor recovery key are both seed-derived —
    // a browser founder has nowhere else to keep them — trading recovery
    // independence for reload-stability, the same trade the enter path states.
    let space_recovery_seed = blake3::derive_key("lait.browser-found.space-recovery.v1", seed);
    let space_recovery_pub = mechanics::actor::device_from_seed(&space_recovery_seed);
    let recovery_root =
        mechanics::space::recovery_commit(&space_recovery_pub).expect("space recovery commitment");
    let space = mechanics::space::derive_space_id(&me, &salt, &recovery_root);

    let actor_recovery_seed = blake3::derive_key("lait.browser-found.actor-recovery.v1", seed);
    let actor_recovery_commit = mechanics::actor::recovery_commitment(
        &mechanics::actor::device_from_seed(&actor_recovery_seed),
    )
    .expect("actor recovery commitment");
    let (founder_inception, founder_actor) = mechanics::actor::incept_single(
        seed,
        &space,
        derive16("lait.browser-found.incept-nonce.v1", seed, &space),
        derive16("lait.browser-found.binding-nonce.v1", seed, &space),
        Some(actor_recovery_commit),
    );
    let genesis = Genesis {
        space_id: space.clone(),
        founding_actors: vec![founder_actor.clone()],
        salt,
        recovery_root,
    };
    FoundingIdentity {
        space,
        genesis,
        founder_inception,
        founder_actor,
    }
}

/// Commit the founding sequence onto a `ledger` freshly created on the
/// identity's genesis: the founding batch (inception + epoch 0, sealed to this
/// device) and then the activation batch (activate the World, seed the founder
/// grants). Returns the epoch key so the caller can hold it directly; it is
/// also persisted sealed, so a reopen recovers it through `refresh_keyring`.
///
/// Idempotent by construction on the parts that can be — an already-effective
/// grant authors nothing — but the caller is expected to run this ONCE, on the
/// first found; a reload reopens the store and skips it.
pub fn found_on_ledger(
    ledger: &mut Ledger,
    seed: &[u8; 32],
    identity: &FoundingIdentity,
    world: &str,
    implementation_id: [u8; 32],
    implementation_version: u32,
    grants: &[FounderGrant],
) -> Result<SpaceKey, String> {
    let me = mechanics::actor::device_from_seed(seed);
    let founder_actor = &identity.founder_actor;
    let space = &identity.space;

    // Founding batch: the founder's inception and epoch 0, sealed to this
    // device — one atomic commit, the browser port of the daemon's `form`.
    let key =
        mechanics::authorization::random_key().map_err(|f| format!("founding epoch key: {f:?}"))?;
    let epoch0 = derive16("lait.browser-found.epoch0.v1", seed, space);
    let key_commit = *blake3::hash(&key).as_bytes();
    let mint = acl::sign_op(
        seed,
        &AclOp {
            action: AclAction::MintEpoch {
                id: epoch0,
                gen: 0,
                key_commit,
                members: vec![founder_actor.clone()],
            },
            by: founder_actor.clone(),
            actor_asof: vec![identity.founder_inception.hash()],
            nonce: None,
        },
        vec![],
        space,
    );
    let sealed = mechanics::authorization::seal_to(&me, &key)
        .map_err(|f| format!("seal founding epoch key: {f:?}"))?
        .ok_or_else(|| "founder device cannot receive its own sealed key".to_string())?;
    ledger
        .commit_batch(
            &[
                Effect::Actor(identity.founder_inception.clone()).encode(),
                Effect::Acl(mint).encode(),
            ],
            &[mechanics::authorization::SealedKeyRecord {
                epoch: epoch0,
                device: me.clone(),
                sealed,
            }
            .encode()],
        )
        .map_err(|f| format!("founding batch: {f:?}"))?;

    // Activation batch: activate the World and seed the founder's grants,
    // causally chained (each op's parent is the previous op's hash). Without it
    // the World answers nothing.
    let actor_asof = ledger.actor_heads(founder_actor);
    let mut parents = ledger.acl_heads();
    let mut actions = vec![AclAction::ActivateWorldImplementation {
        world: world.to_string(),
        implementation_id,
        implementation_version,
    }];
    for grant in grants {
        let grant_id = acl::capability_grant_id(
            founder_actor,
            &grant.capability,
            &grant.resource,
            &grant.salt,
        )
        .ok_or_else(|| "founder grant id".to_string())?;
        actions.push(AclAction::GrantCapabilityFrom {
            grant_id,
            actor: founder_actor.clone(),
            capability: grant.capability.clone(),
            resource: grant.resource.clone(),
            salt: grant.salt,
            origin: GrantOrigin::Founder,
        });
    }
    let mut effects = Vec::with_capacity(actions.len());
    for action in actions {
        let op = acl::sign_op(
            seed,
            &AclOp {
                action,
                by: founder_actor.clone(),
                actor_asof: actor_asof.clone(),
                nonce: None,
            },
            parents,
            space,
        );
        parents = vec![op.hash()];
        effects.push(Effect::Acl(op).encode());
    }
    ledger
        .commit_batch(&effects, &[])
        .map_err(|f| format!("activation batch: {f:?}"))?;

    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(name: &str) -> FounderGrant {
        FounderGrant {
            capability: mechanics::authorization::PolicyCapability::new("lait", name),
            resource: mechanics::authorization::Resource::root("lait"),
            salt: [0u8; 16],
        }
    }

    /// The founding baseline: a seed founds a Space whose founder is an admin
    /// writer, whose World is active, and whose declared grants are effective —
    /// and whose founder holds the epoch key. This is the authority a founded
    /// Space stands on; if any of it regresses, a founded tab looks fine and
    /// denies every write with no explanation.
    #[test]
    fn founding_yields_an_admin_writer_an_active_world_and_effective_grants() {
        let seed = [7u8; 32];
        let world = "com.lait.issues";
        let implementation_id = [0x11; 32];
        let grants = [grant("space.admin"), grant("space.contributor")];

        let identity = founding_identity(&seed);
        let mut ledger = Ledger::create_on(
            std::sync::Arc::new(journal::MemMedium::new()),
            identity.genesis.clone(),
        )
        .expect("ledger");
        let key = found_on_ledger(
            &mut ledger,
            &seed,
            &identity,
            world,
            implementation_id,
            3,
            &grants,
        )
        .expect("founding commits");

        let acl = ledger.acl_state().expect("acl state");
        let founder = &identity.founder_actor;
        assert!(acl.is_member(founder), "founder is a member");
        assert!(acl.is_admin(founder), "founder is an admin");
        assert!(acl.can_write(founder), "founder may write");
        assert_eq!(
            acl.active_implementation(world),
            Some(implementation_id),
            "the World is activated with the founder's implementation"
        );
        for g in &grants {
            assert!(
                !acl.effective_capability_grants(founder, &g.capability, &g.resource)
                    .is_empty(),
                "the founder grant {:?} is effective",
                g.capability
            );
        }
        // The epoch key the founder minted opens under its own commitment — the
        // keyring a body encrypts and reads under.
        assert_eq!(
            *blake3::hash(&key).as_bytes(),
            acl.epochs()
                .into_iter()
                .find(|e| e.gen == 0)
                .expect("epoch 0 exists")
                .key_commit,
            "the returned key matches the minted epoch's commitment"
        );
    }

    /// Deterministic in the seed: founding twice from the same seed derives the
    /// identical Space, which is what lets a reload reopen its store.
    #[test]
    fn founding_is_deterministic_in_the_seed() {
        let seed = [9u8; 32];
        assert_eq!(
            founding_identity(&seed).space.as_str(),
            founding_identity(&seed).space.as_str()
        );
        // A different seed founds a different Space.
        assert_ne!(
            founding_identity(&[9u8; 32]).space.as_str(),
            founding_identity(&[10u8; 32]).space.as_str()
        );
    }
}
