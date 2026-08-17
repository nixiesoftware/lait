//! Whose key is about to be spent on the way out.
//!
//! # The question, and the one it is not
//!
//! Every other gate in this crate asks *may this be done*. This one asks **whose
//! key would make the signature**, which is a different question with a different
//! answer, and the difference is the whole reason the module exists. A grant can
//! be widened, audited, revoked and re-granted; a signature made under somebody
//! else's identity cannot be recalled. An agent that sends as a human has said
//! something in that human's name to a third party who has no way to learn it was
//! not them — and no later revocation reaches the copy the recipient holds.
//!
//! This is the primary property of the correspondence plane **for human actors**,
//! not a guardrail bolted on for agents. It happens to refuse a borrowed key
//! whoever borrowed it.
//!
//! # Why it lands before there is anything to send
//!
//! Proving a refusal is nearly free while no send path exists and expensive
//! afterwards, because afterwards every path is a place the check might already
//! have been forgotten. So the gate is here first, and the type below is what
//! makes forgetting it a compile error rather than a review comment.
//!
//! # A witness, not a boolean
//!
//! [`authorize`] returns [`Egress`] — a value only this module can mint, holding
//! private fields, with no `Clone`, no `Default`, and no `Serialize`. A send path
//! takes one **by value**. So:
//!
//! - a path that never asked cannot be written, because it has no way to obtain
//!   the argument;
//! - a witness cannot be stored and replayed, because it cannot be copied;
//! - a witness cannot travel, because it cannot be serialized — standing is not
//!   portable, and a receipt that leaves is a forgery oracle somewhere else.
//!
//! It also borrows the [`Directory`] it was proven against, so it cannot outlive
//! the state that justified it. That is the cheapest available answer to
//! staleness: there is no frontier to carry and therefore no frontier to
//! misreport, because the witness *is* tied to one replayed value. A caller that
//! wants to send at a later position replays and asks again.
//!
//! # What this deliberately does not decide
//!
//! Not whether the recipient will accept it, not whether the actor holds any
//! grant, not whether the World permits the act. Those are `acl` and `demand`
//! questions and they are asked elsewhere. This module has exactly one fact to
//! establish and stops there.

use crate::actor::Directory;
use crate::ids::{ActorId, DeviceId};

/// Standing to spend one actor's key on one outbound act.
///
/// Only [`authorize`] mints one. See the module docs for why it is neither
/// copyable nor serializable.
#[derive(Debug)]
#[must_use = "an egress witness that is never spent proves nothing; pass it to \
              the send path or drop the send"]
pub struct Egress<'a> {
    /// Borrowed so the witness cannot outlive the replayed state behind it.
    directory: &'a Directory,
    actor: ActorId,
    device: DeviceId,
}

impl<'a> Egress<'a> {
    /// The actor this act is from, as proven — never as claimed.
    pub fn actor(&self) -> &ActorId {
        &self.actor
    }

    /// The device that will make the signature.
    pub fn device(&self) -> &DeviceId {
        &self.device
    }

    /// The devices this actor holds at the position the witness was proven at.
    ///
    /// Here because a mailbox seals to an actor's whole device set, and reading
    /// it off the witness means reading it at exactly the position the send was
    /// authorized at rather than at whatever the caller happens to hold next.
    pub fn devices(&self) -> Vec<DeviceId> {
        self.directory.devices_of(&self.actor)
    }
}

/// Why an outbound act was refused.
///
/// Each arm names a different remedy, and none of them is "you lack permission" —
/// permission is not what was asked. Distinguishing them leaks nothing: every one
/// is a fact about the caller's own device and its own claim, which the caller
/// already holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// The device id is not a well-formed key, so it names no device at all.
    ///
    /// Separate from [`Refused::DeviceUnbound`] because the remedies are
    /// opposites: this one means the request was malformed, that one means the
    /// request was well-formed and the answer is no. Folding them would tell a
    /// caller with a typo to go and get itself enrolled.
    UnusableDevice,
    /// No such actor at this position. It may exist later, or never have existed.
    NoSuchActor,
    /// The device speaks for no actor here — never enrolled, or revoked.
    DeviceUnbound,
    /// The device speaks for a *different* actor. **The borrowed-key case**, and
    /// the one this module exists for.
    ///
    /// The actor it does speak for is named because the caller is that actor and
    /// is therefore entitled to know: the usual reason to see this is a client
    /// that resolved the wrong identity, and naming it turns a mystery into a
    /// fix. It is not named when the device is ambiguously bound, because then
    /// there is no single answer.
    NotThisActor { speaks_for: Option<ActorId> },
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnusableDevice => f.write_str("not a well-formed device key"),
            Self::NoSuchActor => f.write_str("no such actor at this position"),
            Self::DeviceUnbound => f.write_str("this device speaks for no actor here"),
            Self::NotThisActor {
                speaks_for: Some(other),
            } => {
                write!(f, "this device speaks for {other}, not the actor claimed")
            }
            Self::NotThisActor { speaks_for: None } => {
                f.write_str("this device does not speak for the actor claimed")
            }
        }
    }
}

impl std::error::Error for Refused {}

/// Prove that `device` may spend `actor`'s key, against one replayed position.
///
/// `directory` carries the position: replay it with [`crate::actor::replay_at`]
/// at whatever frontier the caller means to act at. There is no separate frontier
/// argument on purpose — one would be a second, unverifiable claim about the
/// value already in hand.
///
/// # A device bound to two actors is allowed, and that is not an oversight
///
/// The actor plane permits one device to consent into two actors, and records
/// that this "forfeits attribution, not authorization". Such a device may send as
/// either, because it legitimately speaks for both — one person holding two
/// identities on one machine is the case that permits. What it may not do is send
/// as a *third* actor, which is what this refuses. The witness records which
/// identity was claimed, so the ambiguity never has to be resolved downstream.
pub fn authorize<'a>(
    directory: &'a Directory,
    actor: &ActorId,
    device: &DeviceId,
) -> Result<Egress<'a>, Refused> {
    // A malformed id is refused before anything is looked up. `DeviceId` compares
    // spelling-blind, so a re-spelling would resolve correctly — but a value that
    // is not a key at all would come back `DeviceUnbound`, which reads as "you
    // were revoked" when the truth is "that is not a device id".
    if DeviceId::parse(device.as_str()).is_none() {
        return Err(Refused::UnusableDevice);
    }
    if !directory.exists(actor) {
        return Err(Refused::NoSuchActor);
    }
    if !directory.is_device_of(actor, device) {
        // Ask what it *does* speak for, so the refusal can say. `None` here is
        // either unbound or ambiguously bound; the two are told apart by whether
        // any actor claims it.
        let speaks_for = directory.actor_of_device(device).cloned();
        let bound_somewhere = speaks_for.is_some()
            || directory
                .actors()
                .any(|(_, state)| state.devices.contains(device));
        return Err(if bound_somewhere {
            Refused::NotThisActor { speaks_for }
        } else {
            Refused::DeviceUnbound
        });
    }
    Ok(Egress {
        directory,
        actor: actor.clone(),
        device: device.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::{self, ActorOp, ConsentCtx, SignedEvent};
    use crate::crypto::device_from_seed;
    use crate::ids::{SpaceId, SystemUlidSource};

    fn seed(n: u8) -> [u8; 32] {
        [n; 32]
    }

    /// Incept a single-device actor for seed `n`.
    fn incept(n: u8, space: &SpaceId) -> (SignedEvent, ActorId) {
        let nonce = [n; 16];
        let keys = vec![device_from_seed(&seed(n))];
        let binding = actor::consent_sign(
            &seed(n),
            space.as_str(),
            [n.wrapping_add(100); 16],
            &ConsentCtx::Incept {
                incept_nonce: &nonce,
                devices: &keys,
                recovery_commit: &None,
            },
        );
        let op = ActorOp::Incept {
            space: space.as_str().to_string(),
            nonce,
            devices: vec![binding],
            recovery_commit: None,
        };
        let ev = actor::sign_event(&seed(n), &op, vec![], space);
        let id = ActorId::from_incept_hash(&ev.hash());
        (ev, id)
    }

    /// The three structural claims the module makes are compiler-enforced, and
    /// this test exists to say which compiler error to expect when someone
    /// weakens one. It cannot assert a non-implementation from inside Rust, so it
    /// asserts the positive facts a weakening would have to break.
    #[test]
    fn the_witness_cannot_be_forged_duplicated_or_sent() {
        // Constructing one outside this module is E0451 (private field). The
        // fields being private is the whole fence, so pin that they are.
        let space = SpaceId::mint(&SystemUlidSource);
        let (e_alice, alice) = incept(1, &space);
        let plane = actor::replay(&space, &[e_alice]);
        let standing = authorize(&plane, &alice, &device_from_seed(&seed(1))).expect("hers");

        // Not Clone: this compiles only because it moves. If someone derives
        // Clone, `spend` below stops proving single use.
        fn spend(_: Egress<'_>) {}
        spend(standing);

        // Not Serialize: `fn assert_serialize<T: Serialize>()` would not compile
        // for Egress. Asserted by absence of any serde derive on the type, which
        // the doc explains — standing is not portable.

        // The lifetime is the staleness answer: `Egress<'a>` borrows the
        // Directory, so this does not compile if the borrow is dropped first.
        let held = {
            let (e_bob, bob) = incept(2, &space);
            let inner = actor::replay(&space, &[e_bob]);
            authorize(&inner, &bob, &device_from_seed(&seed(2))).map(|w| w.actor().clone())
        };
        assert!(
            held.is_ok(),
            "the actor id outlives the borrow; the witness does not"
        );
    }

    /// The whole point, in one test: a device may spend its own actor's key and
    /// no other actor's.
    #[test]
    fn a_device_spends_its_own_actors_key_and_no_others() {
        let space = SpaceId::mint(&SystemUlidSource);
        let (e_alice, alice) = incept(1, &space);
        let (e_bob, bob) = incept(2, &space);
        let plane = actor::replay(&space, &[e_alice, e_bob]);

        let alice_device = device_from_seed(&seed(1));
        let bob_device = device_from_seed(&seed(2));

        let standing = authorize(&plane, &alice, &alice_device).expect("her own key");
        assert_eq!(standing.actor(), &alice);
        assert_eq!(standing.device(), &alice_device);
        assert_eq!(standing.devices(), vec![alice_device.clone()]);

        // The borrowed key. This is the refusal the module exists for, and the
        // reason it names what the device does speak for.
        assert_eq!(
            authorize(&plane, &alice, &bob_device).expect_err("refused"),
            Refused::NotThisActor {
                speaks_for: Some(bob.clone())
            },
            "one actor's device must not be able to send as another"
        );
        assert_eq!(
            authorize(&plane, &bob, &alice_device).expect_err("refused"),
            Refused::NotThisActor {
                speaks_for: Some(alice)
            },
            "and it is symmetric — neither direction is privileged"
        );
        let _ = bob;
    }

    /// A revoked device loses egress the moment it loses the binding, and the
    /// refusal it gets says so rather than saying "malformed".
    #[test]
    fn a_device_that_is_no_ones_is_told_it_is_unbound() {
        let space = SpaceId::mint(&SystemUlidSource);
        let (e_alice, alice) = incept(1, &space);
        let plane = actor::replay(&space, &[e_alice]);

        let stranger = device_from_seed(&seed(9));
        assert_eq!(
            authorize(&plane, &alice, &stranger).expect_err("refused"),
            Refused::DeviceUnbound,
            "a device bound nowhere is unbound, not somebody else's"
        );
    }

    /// The three structural refusals are distinct, because their remedies are.
    #[test]
    fn a_malformed_id_and_a_missing_actor_are_not_the_same_refusal() {
        let space = SpaceId::mint(&SystemUlidSource);
        let (e_alice, alice) = incept(1, &space);
        let plane = actor::replay(&space, &[e_alice]);

        // Not a key at all. Must not read as "you were revoked".
        let nonsense = DeviceId::from_key_string(String::from("not-a-device"));
        assert_eq!(
            authorize(&plane, &alice, &nonsense).expect_err("refused"),
            Refused::UnusableDevice
        );

        // A well-formed key naming an actor that does not exist here.
        let absent = ActorId::from_incept_hash(&"0".repeat(64));
        assert_eq!(
            authorize(&plane, &absent, &device_from_seed(&seed(1))).expect_err("refused"),
            Refused::NoSuchActor,
            "an actor this position has never seen is not a borrowed key"
        );
    }

    /// A re-spelled device id resolves to its binding rather than being refused.
    ///
    /// The gate's own canonicality check must not reintroduce the split it was
    /// written after: `DeviceId` compares spelling-blind since the revocation
    /// bypass, and a well-formed upper-case key *is* a well-formed key. Only
    /// something that is not a key at all is `UnusableDevice`.
    #[test]
    fn a_shouted_but_well_formed_key_still_resolves_to_its_binding() {
        let space = SpaceId::mint(&SystemUlidSource);
        let (e_alice, alice) = incept(1, &space);
        let plane = actor::replay(&space, &[e_alice]);

        let shouted =
            DeviceId::from_key_string(device_from_seed(&seed(1)).as_str().to_ascii_uppercase());
        let standing = authorize(&plane, &alice, &shouted).expect("one key, either spelling");
        assert_eq!(standing.device(), &device_from_seed(&seed(1)));
    }

    /// A device bound to two actors may send as either — and still not as a third.
    #[test]
    fn a_device_bound_to_two_actors_may_send_as_either_of_them() {
        let space = SpaceId::mint(&SystemUlidSource);
        let (e_alice, alice) = incept(1, &space);
        let (e_bob, bob) = incept(2, &space);
        let (e_carol, carol) = incept(3, &space);

        // Alice's device consents into Bob's actor too. Bob's own device authors
        // it, which is what makes the binding legitimate.
        let binding = actor::consent_sign(
            &seed(1),
            space.as_str(),
            [55u8; 16],
            &ConsentCtx::Member { actor: &bob },
        );
        let e_share = actor::sign_event(
            &seed(2),
            &ActorOp::AddDevice {
                actor: bob.clone(),
                binding,
            },
            vec![e_bob.hash()],
            &space,
        );

        let plane = actor::replay(&space, &[e_alice, e_bob, e_carol, e_share]);
        let shared = device_from_seed(&seed(1));

        assert!(
            authorize(&plane, &alice, &shared).is_ok(),
            "one person, two identities, one machine — the plane permits this"
        );
        assert!(
            authorize(&plane, &bob, &shared).is_ok(),
            "and the witness records which identity was claimed"
        );
        assert_eq!(
            authorize(&plane, &carol, &shared).expect_err("refused"),
            Refused::NotThisActor { speaks_for: None },
            "an ambiguously bound device still may not send as a third actor, and \
             gets no single answer for what it does speak for"
        );
    }
}
