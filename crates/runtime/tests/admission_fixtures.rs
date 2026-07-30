//! Who gets in, and what a replayed opening costs.
//!
//! Pure: no socket, no Station, no clock but the one injected. The cases worth
//! testing are all decisions — a claim that does not match the peer, a Space
//! that is not ours, a generation we do not speak, an opening arriving twice —
//! and none of them need a network to happen.

use std::time::{Duration, Instant};

use mechanics::ids::{ActorId, DeviceId, SpaceId, StationId};
use replica::frontier::AuthorityFrontier;
use runtime::admission::{
    judge, AcceptedOpenings, Admission, OpeningContext, PlanePolicy, Replay, MAX_ACCEPTED_OPENINGS,
};
use runtime::planes::{feature, stream_kind, Plane, SessionOpen, SessionRefusal, SPACE_ID_LEN};
use runtime::world::{AuthorityView, PrincipalResolution};

const PEER_SEED: [u8; 32] = [11u8; 32];
const LOCAL_SEED: [u8; 32] = [12u8; 32];
const STRANGER_SEED: [u8; 32] = [13u8; 32];

fn station(seed: &[u8; 32]) -> StationId {
    StationId::from_device(&mechanics::crypto::device_from_seed(seed)).expect("station id")
}

fn space() -> SpaceId {
    SpaceId::from_digest([31u8; 16])
}

/// A view in which exactly one device is a member.
struct OneMember(DeviceId);

impl AuthorityView for OneMember {
    fn resolve(&self, device: &DeviceId) -> Option<PrincipalResolution> {
        (device == &self.0).then(|| PrincipalResolution {
            actor: ActorId::parse(&format!("act_{}", "ab".repeat(32))).expect("actor"),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![9]),
        })
    }
}

fn context<'a>(space: &'a SpaceId, plane: Plane) -> OpeningContext<'a> {
    OpeningContext {
        space,
        local_station: station(&LOCAL_SEED),
        peer: station(&PEER_SEED),
        plane,
    }
}

fn opening(space: &SpaceId, plane: Plane) -> SessionOpen {
    let mut bytes = [0u8; SPACE_ID_LEN];
    bytes.copy_from_slice(space.as_str().as_bytes());
    SessionOpen {
        plane,
        protocol_version: plane.protocol_version(),
        features: feature::RESIDENCY_HINTS,
        space: bytes,
        initiator_station: station(&PEER_SEED).key_bytes(),
        responder_station: station(&LOCAL_SEED).key_bytes(),
        session_id: [3u8; 16],
        session_epoch: [4u8; 16],
        authority_frontier: vec![9],
        requested_lanes: vec![stream_kind::CONTROL],
    }
}

fn member() -> OneMember {
    OneMember(mechanics::crypto::device_from_seed(&PEER_SEED))
}

#[test]
fn a_member_talking_to_the_right_station_about_the_right_space_is_admitted() {
    let space = space();
    let outcome = judge(
        &opening(&space, Plane::Freight),
        &context(&space, Plane::Freight),
        &member(),
        &PlanePolicy::default(),
    );
    let Admission::Accept(accept, standing) = outcome else {
        panic!("a member is admitted: {outcome:?}");
    };
    assert_eq!(accept.granted_lanes, vec![stream_kind::CONTROL]);
    assert_eq!(standing.station, station(&PEER_SEED));
    assert_eq!(
        accept.capability.features,
        feature::RESIDENCY_HINTS,
        "only bits the peer offered"
    );
    assert_eq!(
        accept.capability.features & feature::UNSOLICITED_PROVIDE,
        0,
        "a capability the peer did not offer is one it will not understand"
    );
}

#[test]
fn a_stranger_who_completed_a_handshake_is_still_not_a_member() {
    // QUIC proves the bytes came from the holder of a key. It says nothing
    // about whether that key belongs here, and a plane that read a completed
    // handshake as membership would admit anyone who could dial.
    let space = space();
    let mut context = context(&space, Plane::Freight);
    context.peer = station(&STRANGER_SEED);
    let mut open = opening(&space, Plane::Freight);
    open.initiator_station = station(&STRANGER_SEED).key_bytes();

    assert_eq!(
        judge(&open, &context, &member(), &PlanePolicy::default()),
        Admission::Refuse(SessionRefusal::Refused)
    );
}

#[test]
fn a_claim_that_does_not_match_the_negotiated_peer_is_malformed_not_refused() {
    // A different kind of wrong. "Refused" is about standing; this opening is
    // asserting an identity the transport already disproved, which is not a
    // question about membership at all.
    let space = space();
    let mut open = opening(&space, Plane::Freight);
    open.initiator_station = station(&STRANGER_SEED).key_bytes();
    assert_eq!(
        judge(
            &open,
            &context(&space, Plane::Freight),
            &member(),
            &PlanePolicy::default()
        ),
        Admission::Refuse(SessionRefusal::Malformed)
    );

    let mut open = opening(&space, Plane::Freight);
    open.responder_station = station(&STRANGER_SEED).key_bytes();
    assert_eq!(
        judge(
            &open,
            &context(&space, Plane::Freight),
            &member(),
            &PlanePolicy::default()
        ),
        Admission::Refuse(SessionRefusal::Malformed),
        "an opening addressed to another Station is not ours to accept"
    );
}

#[test]
fn an_opening_for_another_space_is_refused_however_well_formed() {
    let space = space();
    let other = SpaceId::from_digest([99u8; 16]);
    let mut open = opening(&other, Plane::Freight);
    open.initiator_station = station(&PEER_SEED).key_bytes();
    open.responder_station = station(&LOCAL_SEED).key_bytes();
    assert_eq!(
        judge(
            &open,
            &context(&space, Plane::Freight),
            &member(),
            &PlanePolicy::default()
        ),
        Admission::Refuse(SessionRefusal::Malformed)
    );
}

#[test]
fn an_opening_that_disagrees_with_its_own_alpn_is_malformed() {
    // The ALPN already fixed the plane. An opening that says otherwise is not
    // confused; it is trying something.
    let space = space();
    let open = opening(&space, Plane::Live);
    assert_eq!(
        judge(
            &open,
            &context(&space, Plane::Freight),
            &member(),
            &PlanePolicy::default()
        ),
        Admission::Refuse(SessionRefusal::Malformed)
    );
}

#[test]
fn an_unsupported_generation_is_the_one_refusal_that_says_why() {
    // Because it is the one a peer can act on. Everything else is coarse.
    let space = space();
    let mut open = opening(&space, Plane::Freight);
    open.protocol_version = 99;
    assert_eq!(
        judge(
            &open,
            &context(&space, Plane::Freight),
            &member(),
            &PlanePolicy::default()
        ),
        Admission::Refuse(SessionRefusal::UnsupportedVersion { supported: 1 })
    );
}

#[test]
fn operator_policy_and_membership_answer_different_questions() {
    // Authority says whether this peer may; policy says whether this Station
    // will. An operator on a metered link is not making a claim about anyone's
    // membership.
    let space = space();
    let off = PlanePolicy {
        serve_enabled: false,
        fetch_enabled: false,
    };
    assert_eq!(
        judge(
            &opening(&space, Plane::Freight),
            &context(&space, Plane::Freight),
            &member(),
            &off
        ),
        Admission::Refuse(SessionRefusal::Refused),
        "and the refusal is the same coarse one, so it leaks nothing"
    );
}

#[test]
fn a_replayed_opening_returns_the_same_accept_and_mints_nothing() {
    // Acceptance 11. 0.5-RTT data is replayable by anyone who can intercept
    // handshake packets, so accepting has to be idempotent.
    let space = space();
    let open = opening(&space, Plane::Freight);
    let mut ledger = AcceptedOpenings::default();
    let now = Instant::now();

    assert_eq!(ledger.lookup(&open, now), Replay::Fresh);
    let Admission::Accept(first, _) = judge(
        &open,
        &context(&space, Plane::Freight),
        &member(),
        &PlanePolicy::default(),
    ) else {
        panic!("admitted");
    };
    ledger.remember(&open, &first, now);
    assert_eq!(ledger.len(), 1);

    // The replay. Nothing is judged again, nothing is allocated again, and the
    // answer is byte-identical.
    match ledger.lookup(&open, now + Duration::from_millis(50)) {
        Replay::Repeat(again) => {
            assert_eq!(again.encode(), first.encode());
        }
        Replay::Fresh => panic!("a replay must be recognised"),
    }
    assert_eq!(ledger.len(), 1, "a replay mints no second session");
}

#[test]
fn a_reconnect_mints_a_new_epoch_and_is_a_new_session() {
    // The distinction that makes the ledger safe: a replay is the same opening
    // twice, and a reconnect is a different one.
    let space = space();
    let first = opening(&space, Plane::Freight);
    let mut reconnect = first.clone();
    reconnect.session_epoch = [5u8; 16];

    let mut ledger = AcceptedOpenings::default();
    let now = Instant::now();
    let Admission::Accept(accept, _) = judge(
        &first,
        &context(&space, Plane::Freight),
        &member(),
        &PlanePolicy::default(),
    ) else {
        panic!("admitted");
    };
    ledger.remember(&first, &accept, now);
    assert_eq!(ledger.lookup(&reconnect, now), Replay::Fresh);
}

#[test]
fn the_replay_ledger_is_bounded_and_forgets_rather_than_refuses() {
    // A table keyed by remote input. Forgetting an opening only costs a replay
    // being judged afresh; refusing to record would make the ledger useless
    // exactly when it is under pressure, which is when replays are likeliest.
    let space = space();
    let mut ledger = AcceptedOpenings::default();
    let now = Instant::now();
    let base = opening(&space, Plane::Freight);
    let Admission::Accept(accept, _) = judge(
        &base,
        &context(&space, Plane::Freight),
        &member(),
        &PlanePolicy::default(),
    ) else {
        panic!("admitted");
    };

    for n in 0..(MAX_ACCEPTED_OPENINGS + 64) {
        let mut open = base.clone();
        open.session_id = (n as u128).to_be_bytes();
        ledger.remember(&open, &accept, now + Duration::from_millis(n as u64));
    }
    assert!(ledger.len() <= MAX_ACCEPTED_OPENINGS);

    // And an opening older than the window is simply forgotten.
    let mut ledger = AcceptedOpenings::default();
    ledger.remember(&base, &accept, now);
    assert_eq!(
        ledger.lookup(&base, now + Duration::from_secs(600)),
        Replay::Fresh
    );
    assert!(ledger.is_empty());
}
