//! The kinship plane, exercised the way a consumer sees it.
//!
//! Each test here corresponds to a verification clause in the Substrate Specs
//! "Kinship is avowed, never derived" and "Kinship travels: the audience in the
//! preimage, and the projection that cannot omit". A clause with no test is a
//! clause nobody is holding.

use mechanics::actor::device_from_seed;
use mechanics::ids::{ActorId, SpaceId};
use mechanics::kinship::{
    Attribution, Audience, Avowal, Claim, DeviceLink, Entry, KinshipLog, Party, Refusal,
    Retirement, Signature, Standing,
};

const FIRST: [u8; 32] = [11u8; 32];
const SECOND: [u8; 32] = [22u8; 32];
const THIRD: [u8; 32] = [33u8; 32];
const STRANGER: [u8; 32] = [44u8; 32];

fn space() -> SpaceId {
    SpaceId::from_digest([7u8; 16])
}

/// A log founded on first+second, with third linked in.
fn founded() -> KinshipLog {
    let genesis = DeviceLink::seal(&FIRST, &SECOND, [1u8; 16], 1).expect("genesis");
    let mut log = KinshipLog::found(genesis).expect("found");
    let joining = DeviceLink::seal(&FIRST, &THIRD, [2u8; 16], 1).expect("join");
    log.append(Entry::Link(joining)).expect("append");
    log
}

fn own_standing(seed: &[u8; 32]) -> Standing {
    Standing {
        device: Some(device_from_seed(seed)),
        own: true,
        ..Standing::default()
    }
}

// ---------------------------------------------------------------------------
// The link: peerage, mutual, and survivable
// ---------------------------------------------------------------------------

#[test]
fn a_link_is_symmetric_and_both_devices_signed_it() {
    let link = DeviceLink::seal(&FIRST, &SECOND, [1u8; 16], 1).expect("seal");
    link.verify().expect("a freshly sealed link verifies");

    // Sorted, so the same pair in either order is one fact rather than two.
    let reversed = DeviceLink::seal(&SECOND, &FIRST, [1u8; 16], 1).expect("seal");
    assert_eq!(
        link.devices, reversed.devices,
        "the device pair is sorted, so argument order is not part of the fact"
    );
    assert_eq!(
        link, reversed,
        "and the whole link is identical, signatures included"
    );
}

#[test]
fn a_link_signed_by_only_one_side_is_refused_as_not_mutual() {
    let mut link = DeviceLink::seal(&FIRST, &SECOND, [1u8; 16], 1).expect("seal");
    // Replace the second signature with a valid signature over the same
    // preimage from a key that is not in the link. Structurally well-formed,
    // and exactly the forgery mutual signing exists to refuse.
    let preimage = DeviceLink::preimage(&link.devices, &link.nonce, link.epoch);
    link.signatures[1] = Signature(mechanics::actor::sign_detached(&STRANGER, &preimage));

    assert_eq!(
        link.verify(),
        Err(Refusal::NotMutual),
        "a link one side never signed is refused, and the refusal has a name"
    );
}

#[test]
fn a_device_cannot_link_to_itself() {
    assert_eq!(
        DeviceLink::seal(&FIRST, &FIRST, [1u8; 16], 1).unwrap_err(),
        Refusal::NotDistinct
    );
}

#[test]
fn losing_one_device_leaves_every_other_link_standing() {
    // This is the reason L1 is peerage and not clientage. Under a patron chain,
    // retiring the device that admitted the others would take them with it.
    let mut log = founded();
    let first = device_from_seed(&FIRST);
    let second = device_from_seed(&SECOND);
    let third = device_from_seed(&THIRD);

    assert_eq!(log.devices().len(), 3, "three devices before the loss");

    // FIRST is the device that signed BOTH links — the one a clientage model
    // would have made load-bearing. Retire it.
    let retirement = Retirement::seal(&FIRST, first.clone(), 2, [9u8; 16]).expect("retire");
    log.append(Entry::Retire(retirement)).expect("append");

    let left = log.devices();
    assert!(
        !left.contains(&first),
        "the retired device is gone from the resolved set"
    );
    assert!(
        left.contains(&second) && left.contains(&third),
        "and both devices it admitted are still standing: {left:?}"
    );

    // The links themselves are untouched — retirement supersedes, it does not
    // erase, because an artifact already transmitted cannot be unsaid.
    let links = log
        .entries()
        .iter()
        .filter(|entry| matches!(entry, Entry::Link(_)))
        .count();
    assert_eq!(links, 2, "both links remain in the log");
}

#[test]
fn the_profile_id_is_the_genesis_links_content_address() {
    let genesis = DeviceLink::seal(&FIRST, &SECOND, [1u8; 16], 1).expect("seal");
    let one = KinshipLog::found(genesis.clone()).expect("found");
    let two = KinshipLog::found(genesis).expect("found again");
    assert_eq!(
        one.profile(),
        two.profile(),
        "self-certifying: the same genesis yields the same id, with no registry"
    );

    let other = DeviceLink::seal(&FIRST, &SECOND, [2u8; 16], 1).expect("seal");
    let different = KinshipLog::found(other).expect("found");
    assert_ne!(
        one.profile(),
        different.profile(),
        "a different genesis nonce is a different profile"
    );
}

// ---------------------------------------------------------------------------
// The avowal: the audience is inside the signature
// ---------------------------------------------------------------------------

#[test]
fn an_avowal_shown_outside_its_audience_is_refused_by_the_artifact_alone() {
    let subject = Party::Device(device_from_seed(&FIRST));
    let correspondent = Party::Device(device_from_seed(&SECOND));
    let avowal = Avowal::seal(
        &FIRST,
        subject,
        Claim::Called("omar".into()),
        Audience::Correspondent(correspondent),
        1,
        [5u8; 16],
    )
    .expect("seal");

    // The named correspondent reads it.
    let intended = Standing {
        device: Some(device_from_seed(&SECOND)),
        ..Standing::default()
    };
    avowal.legible_to(&intended).expect("the audience reads it");

    // Anyone else is refused — and note nothing about *how it arrived* is
    // consulted. The artifact alone answers.
    let onlooker = Standing {
        device: Some(device_from_seed(&STRANGER)),
        ..Standing::default()
    };
    assert_eq!(
        avowal.legible_to(&onlooker),
        Err(Refusal::OutsideAudience),
        "a forwarded avowal is refused exactly as an intercepted one is"
    );
}

#[test]
fn widening_the_audience_breaks_the_signature() {
    // The audience is in the preimage, not the envelope. Re-labelling a
    // Correspondent avowal as Public must not survive verification, or the
    // whole detectability claim is decoration.
    let subject = Party::Device(device_from_seed(&FIRST));
    let avowal = Avowal::seal(
        &FIRST,
        subject,
        Claim::Called("omar".into()),
        Audience::Correspondent(Party::Device(device_from_seed(&SECOND))),
        1,
        [5u8; 16],
    )
    .expect("seal");

    let mut widened = avowal.clone();
    widened.audience = Audience::Public;

    assert_eq!(
        widened.verify(),
        Err(Refusal::BadSignature),
        "moving an avowal to a wider audience invalidates it"
    );
    // And the original still verifies, so the failure is the edit and not the
    // construction.
    avowal.verify().expect("the unedited avowal verifies");
}

#[test]
fn an_avowal_and_an_attestation_differ_only_by_who_signed() {
    let subject = Party::Device(device_from_seed(&FIRST));
    let devices = vec![device_from_seed(&FIRST), device_from_seed(&SECOND)];

    let avowed = Avowal::seal(
        &FIRST,
        subject.clone(),
        Claim::Called("omar".into()),
        Audience::Public,
        1,
        [5u8; 16],
    )
    .expect("seal");
    let attested = Avowal::seal(
        &STRANGER,
        subject,
        Claim::Called("omar".into()),
        Audience::Public,
        1,
        [6u8; 16],
    )
    .expect("seal");

    avowed.verify().expect("self-signed verifies");
    attested.verify().expect("third-party signed verifies");

    assert!(
        avowed.is_self_signed(&devices),
        "signed by a device of the subject: an avowal"
    );
    assert!(
        !attested.is_self_signed(&devices),
        "signed by someone else: an attestation, which is the half that carries weight"
    );
}

#[test]
fn a_name_is_bounded_and_never_empty() {
    let subject = Party::Device(device_from_seed(&FIRST));
    assert_eq!(
        Avowal::seal(
            &FIRST,
            subject.clone(),
            Claim::Called(String::new()),
            Audience::Public,
            1,
            [5u8; 16],
        )
        .unwrap_err(),
        Refusal::Malformed("empty name")
    );
    assert_eq!(
        Avowal::seal(
            &FIRST,
            subject,
            Claim::Called("n".repeat(1000)),
            Audience::Public,
            1,
            [5u8; 16],
        )
        .unwrap_err(),
        Refusal::Bound("name bytes")
    );
}

#[test]
fn every_refusal_carries_a_name_rather_than_a_boolean() {
    // The Spec clause is "the refusal is a named variant, not a boolean".
    // Distinctness is the property: a surface that cannot tell these apart
    // cannot say anything useful, and folding any pair together would be the
    // absence defect one layer down.
    let refusals = [
        Refusal::BadSignature,
        Refusal::Unaddressable,
        Refusal::OutsideAudience,
        Refusal::NotDistinct,
        Refusal::NotMutual,
        Refusal::Unlisted,
        Refusal::Omission,
        Refusal::Uncommitted,
        Refusal::Diverged,
        Refusal::Semantics,
    ];
    for (index, one) in refusals.iter().enumerate() {
        for other in refusals.iter().skip(index + 1) {
            assert_ne!(one, other, "refusals must stay distinct");
            assert_ne!(
                one.to_string(),
                other.to_string(),
                "and must read differently: {one} vs {other}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The party: resolvable, not merely unique
// ---------------------------------------------------------------------------

#[test]
fn an_actor_party_carries_its_space_because_an_actor_id_is_not_resolvable_alone() {
    // `from_incept_hash` takes the caller's word for the shape; `parse` does
    // not. A round-trip test has to use a hash that is actually 64 hex chars,
    // or it tests the fixture rather than the wire.
    let party = Party::Actor {
        space: space(),
        actor: ActorId::from_incept_hash(&"ab".repeat(32)),
    };
    let wire = party.wire();
    assert!(
        wire.contains(space().as_str()),
        "the space is on the wire: {wire}"
    );
    assert_eq!(
        Party::parse_wire(&wire).expect("round trip"),
        party,
        "and it round-trips"
    );
}

#[test]
fn a_party_refuses_a_spelling_it_does_not_fully_understand() {
    assert!(Party::parse_wire("").is_err());
    assert!(Party::parse_wire("actor:not-a-space:nor-an-actor").is_err());
    assert!(
        Party::parse_wire("agent:deadbeef:claude").is_err(),
        "a local-agent handle is not a party and must never become an audience"
    );
}

// ---------------------------------------------------------------------------
// The projection: what a commitment does and does not buy
// ---------------------------------------------------------------------------

#[test]
fn a_projection_without_a_head_is_a_hint_and_says_which() {
    let log = founded();
    let mut projection = log
        .project(&FIRST, 1, &own_standing(&FIRST))
        .expect("project");
    projection.head = None;

    assert_eq!(
        projection.verify(&own_standing(&FIRST)),
        Err(Refusal::Uncommitted),
        "no committed head means it cannot reach a path that treats it as proof"
    );
}

#[test]
fn a_projection_verifies_against_its_committed_head() {
    let log = founded();
    let standing = own_standing(&FIRST);
    let projection = log.project(&FIRST, 1, &standing).expect("project");

    projection.verify(&standing).expect("a faithful projection");
    assert_eq!(
        projection.undelivered(),
        Vec::<String>::new(),
        "an Own-tier reader is admitted to every structural entry, so nothing is held back"
    );
}

#[test]
fn a_body_not_in_the_committed_head_is_refused() {
    let log = founded();
    let standing = own_standing(&FIRST);
    let mut projection = log.project(&FIRST, 1, &standing).expect("project");

    // Splice in a genuine, correctly signed link that the head never covered.
    let smuggled = DeviceLink::seal(&SECOND, &STRANGER, [77u8; 16], 1).expect("seal");
    projection.bodies.push(Entry::Link(smuggled));

    assert_eq!(
        projection.verify(&standing),
        Err(Refusal::Unlisted),
        "every delivered body must be drawn from the head, signature or not"
    );
}

#[test]
fn a_withheld_body_is_named_as_an_omission() {
    // The Spec clause: "a projection with an entry removed fails against its
    // commitment". The head still covers the entry; the body is gone.
    let log = founded();
    let standing = own_standing(&FIRST);
    let full = log.project(&FIRST, 1, &standing).expect("project");

    let dropped = full.bodies.last().cloned().expect("a body to drop");
    let dropped_id = dropped.id().expect("id");

    let mut truncated = full.clone();
    truncated.bodies.retain(|entry| entry != &dropped);

    assert!(
        truncated.undelivered().contains(&dropped_id),
        "the head still names what the bodies no longer carry"
    );
    assert_eq!(
        truncated.withheld(&dropped_id),
        Err(Refusal::Omission),
        "and asking about it by id names the omission"
    );

    // The same question about the full projection is satisfied, so the check
    // discriminates rather than always accusing.
    full.withheld(&dropped_id)
        .expect("the full projection delivered it");
}

#[test]
fn an_id_the_head_never_covered_is_unlisted_rather_than_withheld() {
    // Three absences, three answers. This is the one that would otherwise get
    // folded into "omission" and turn every unknown id into an accusation.
    let log = founded();
    let standing = own_standing(&FIRST);
    let projection = log.project(&FIRST, 1, &standing).expect("project");

    assert_eq!(
        projection.withheld("0000000000000000000000000000000000000000000000000000000000000000"),
        Err(Refusal::Unlisted),
        "an id the head never covered is not evidence of withholding"
    );
}

#[test]
fn editing_the_committed_entry_set_breaks_the_commitment() {
    let log = founded();
    let standing = own_standing(&FIRST);
    let mut projection = log.project(&FIRST, 1, &standing).expect("project");

    if let Some(head) = projection.head.as_mut() {
        head.entries.pop();
    }

    assert_eq!(
        projection.verify(&standing),
        Err(Refusal::Diverged),
        "truncating the head after signing does not recompute"
    );
}

#[test]
fn a_head_minted_under_other_semantics_is_refused() {
    let log = founded();
    let mut head = log.head(&FIRST, 1).expect("head");
    head.semantics = 999;
    assert_eq!(head.verify(), Err(Refusal::Semantics));
}

// ---------------------------------------------------------------------------
// Audience tiers
// ---------------------------------------------------------------------------

#[test]
fn a_member_tier_projection_excludes_a_reader_from_another_space() {
    let mut log = founded();
    let subject = Party::Device(device_from_seed(&FIRST));
    let avowal = Avowal::seal(
        &FIRST,
        subject,
        Claim::Called("omar".into()),
        Audience::Members(space()),
        1,
        [5u8; 16],
    )
    .expect("seal");
    log.append(Entry::Avow(avowal)).expect("append");

    let outsider = Standing {
        device: Some(device_from_seed(&STRANGER)),
        spaces: vec![SpaceId::from_digest([99u8; 16])],
        ..Standing::default()
    };
    let projection = log.project(&FIRST, 1, &outsider).expect("project");

    assert!(
        projection.bodies.is_empty(),
        "a reader of another Space gets no bodies at all"
    );
    assert_eq!(
        projection.undelivered().len(),
        3,
        "but the head still commits to all three entries, so the filtering is visible"
    );
    projection
        .verify(&outsider)
        .expect("and what it did receive is consistent");
}

#[test]
fn own_and_kin_cannot_be_decided_without_being_resolved() {
    // A check that guessed at these would pass when the answer was never
    // fetched, which is the absence defect in its most dangerous form.
    let nothing_resolved = Standing {
        device: Some(device_from_seed(&FIRST)),
        ..Standing::default()
    };
    assert!(!Audience::Own.admits(&nothing_resolved));
    assert!(!Audience::Kin.admits(&nothing_resolved));
    assert!(
        Audience::Public.admits(&nothing_resolved),
        "only Public is decidable with nothing resolved"
    );
}

#[test]
fn attribution_states_what_the_audience_size_actually_buys() {
    // Audience size is the privacy budget: attribution to a Space is nominal,
    // and a surface must be able to say so rather than implying it is uniform.
    assert_eq!(
        Audience::Correspondent(Party::Device(device_from_seed(&SECOND))).attribution(),
        Attribution::Single
    );
    assert_eq!(Audience::Kin.attribution(), Attribution::Few);
    assert_eq!(Audience::Members(space()).attribution(), Attribution::Many);
    assert_eq!(Audience::Public.attribution(), Attribution::None);
}

// ---------------------------------------------------------------------------
// The boundary: an avowal confers nothing
// ---------------------------------------------------------------------------

#[test]
fn a_sponsorship_avowal_is_an_assertion_and_not_a_grant() {
    // There is deliberately no conversion from anything in this module into a
    // grant, capability or membership fact — "authority is not portable;
    // capability is". This test pins the shape that makes that true: a
    // sponsorship avowal verifies, and what it yields is a Claim, full stop.
    let sponsor = Party::Device(device_from_seed(&FIRST));
    let agent = Party::Device(device_from_seed(&THIRD));
    let avowal = Avowal::seal(
        &FIRST,
        sponsor,
        Claim::Sponsors(agent.clone()),
        Audience::Public,
        1,
        [5u8; 16],
    )
    .expect("seal");

    avowal.verify().expect("it verifies");
    assert_eq!(
        avowal.claim,
        Claim::Sponsors(agent),
        "and all it carries is the assertion itself"
    );
}
