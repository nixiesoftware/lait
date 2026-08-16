//! Who gets in, and what a replayed opening costs.
//!
//! Pure: no socket, no Station, no clock but the one injected. The cases worth
//! testing are all decisions — a claim that does not match the peer, a Space
//! that is not ours, a generation we do not speak, an opening arriving twice —
//! and none of them need a network to happen.

use std::time::Duration;
use tokio::time::Instant;

use mechanics::{
    ids::{ActorId, DeviceId, SpaceId},
    station::Key,
};
use replica::frontier::AuthorityFrontier;
use runtime::admission::{
    judge, AcceptedOpenings, Admission, OpeningContext, PlanePolicy, Replay, MAX_ACCEPTED_OPENINGS,
};
use runtime::plane::{feature, stream_kind, Open, Plane, Refusal, SPACE_ID_LEN};
use runtime::world::{AuthorityView, PrincipalResolution};

const PEER_SEED: [u8; 32] = [11u8; 32];
const LOCAL_SEED: [u8; 32] = [12u8; 32];
const STRANGER_SEED: [u8; 32] = [13u8; 32];

fn station(seed: &[u8; 32]) -> Key {
    Key::from_device(&mechanics::actor::device_from_seed(seed)).expect("station id")
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

fn opening(space: &SpaceId, plane: Plane) -> Open {
    let mut bytes = [0u8; SPACE_ID_LEN];
    bytes.copy_from_slice(space.as_str().as_bytes());
    Open {
        plane,
        protocol_version: plane.protocol_version(),
        features: feature::RESIDENCY_HINTS,
        space: bytes,
        initiator_station: station(&PEER_SEED).key_bytes(),
        responder_station: station(&LOCAL_SEED).key_bytes(),
        connection_id: [3u8; 16],
        connection_epoch: [4u8; 16],
        authority_frontier: vec![9],
        requested_lanes: vec![stream_kind::CONTROL],
    }
}

fn member() -> OneMember {
    OneMember(mechanics::actor::device_from_seed(&PEER_SEED))
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
    // Empty, on Freight, whatever was asked for. Freight reads no stream-kind
    // byte — the ALPN types the connection — so a granted lane there is a
    // promise nothing can keep.
    assert!(accept.granted_lanes.is_empty());
    assert_eq!(standing.station, station(&PEER_SEED));
    // The intersection of what the peer offered with what this build
    // implements. Written as the concrete bit rather than as
    // `RESIDENCY_HINTS & LOCAL_SUPPORTED`, which is what this used to say and
    // which is a tautology: it compares the answer to the expression that
    // produced it, so it passed when `LOCAL_SUPPORTED` was zero and passes now,
    // and would keep passing whatever either constant became.
    assert_eq!(
        accept.capability.features,
        feature::RESIDENCY_HINTS,
        "both sides have it, so it is advertised"
    );
    assert_eq!(
        accept.capability.features & feature::UNSOLICITED_PROVIDE,
        0,
        "a capability the peer did not offer is one it will not understand"
    );
    // And the standing carries it, because the plane that has to honour a
    // capability is the one that needs to know whether it was negotiated.
    assert_eq!(standing.features, accept.capability.features);
}

#[test]
fn a_capability_this_build_does_not_implement_is_not_echoed_back() {
    // The direction that matters. A peer offering everything must not be told
    // this build agreed to everything — the accept is a promise, and a peer
    // acting on a bit we merely have a name for would be right to be annoyed.
    let space = space();
    let mut open = opening(&space, Plane::Freight);
    open.features = u64::MAX;
    let outcome = judge(
        &open,
        &context(&space, Plane::Freight),
        &member(),
        &PlanePolicy::default(),
    );
    let Admission::Accept(accept, _) = outcome else {
        panic!("a member is admitted");
    };
    // The concrete bit, not `LOCAL_SUPPORTED` — which is the constant the
    // implementation intersects with, so asserting against it is the same
    // tautology the test thirty lines above documents as forbidden. Written out
    // here, this fails the day a bit joins that constant without an oracle
    // behind it, which is the whole point of having the assertion.
    assert_eq!(
        accept.capability.features,
        feature::RESIDENCY_HINTS | feature::NATIVE_LIVE_MEDIA,
        "everything offered, intersected down to what is actually implemented"
    );
    assert_eq!(accept.capability.features & feature::UNSOLICITED_PROVIDE, 0);
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
        Admission::Refuse(Refusal::Refused)
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
        Admission::Refuse(Refusal::Malformed)
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
        Admission::Refuse(Refusal::Malformed),
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
        Admission::Refuse(Refusal::Malformed)
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
        Admission::Refuse(Refusal::Malformed)
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
        Admission::Refuse(Refusal::UnsupportedVersion { supported: 1 })
    );
}

#[test]
fn operator_policy_and_membership_answer_different_questions() {
    // Authority says whether this peer may; policy says whether this Station
    // will. An operator on a metered link is not making a claim about anyone's
    // membership.
    let space = space();
    let off = PlanePolicy {
        auto_accept_offers: false,
        serve_enabled: false,
        fetch_enabled: false,
        live_enabled: false,
    };
    assert_eq!(
        judge(
            &opening(&space, Plane::Freight),
            &context(&space, Plane::Freight),
            &member(),
            &off
        ),
        Admission::Refuse(Refusal::Refused),
        "and the refusal is the same coarse one, so it leaks nothing"
    );
    // Every plane gets its own switch, and each is answerable alone. A Station
    // that will move files but wants nothing to do with other people's cursors
    // is an ordinary configuration, not a contradiction.
    assert_eq!(
        judge(
            &opening(&space, Plane::Live),
            &context(&space, Plane::Live),
            &member(),
            &off
        ),
        Admission::Refuse(Refusal::Refused),
    );
    let files_only = PlanePolicy {
        auto_accept_offers: false,
        serve_enabled: true,
        fetch_enabled: true,
        live_enabled: false,
    };
    assert_eq!(
        judge(
            &opening(&space, Plane::Live),
            &context(&space, Plane::Live),
            &member(),
            &files_only
        ),
        Admission::Refuse(Refusal::Refused),
        "declining Live must not require declining Freight"
    );
    assert!(
        !matches!(
            judge(
                &opening(&space, Plane::Freight),
                &context(&space, Plane::Freight),
                &member(),
                &files_only
            ),
            Admission::Refuse(_)
        ),
        "and Freight is unaffected by the Live switch"
    );
}

#[test]
fn the_admitted_peer_carries_the_session_the_accept_names() {
    // The driver judges the opening, writes the accept, and then hands the
    // service an `AdmittedPeer`. Without these two fields that hand-off dropped
    // the only thing that identifies *which* session this is — and a transient
    // item's epoch is checked for equality against exactly this one, so a
    // service that could not see it could not tell a live datagram from one
    // belonging to a session that has already reconnected.
    let space = space();
    let open = opening(&space, Plane::Freight);
    let Admission::Accept(accept, peer) = judge(
        &open,
        &context(&space, Plane::Freight),
        &member(),
        &PlanePolicy::default(),
    ) else {
        panic!("a member is admitted");
    };
    assert_eq!(peer.connection_id, open.connection_id);
    assert_eq!(peer.connection_epoch, open.connection_epoch);
    assert_eq!(
        accept.connection_id, open.connection_id,
        "and the accept names the same session the peer proposed"
    );
    assert_eq!(accept.connection_epoch, open.connection_epoch);
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
    reconnect.connection_epoch = [5u8; 16];

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
        open.connection_id = (n as u128).to_be_bytes();
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

#[test]
fn media_lanes_are_granted_only_as_a_negotiated_pair() {
    let space = space();
    let mut open = opening(&space, Plane::Live);
    open.plane = Plane::Live;
    open.protocol_version = Plane::Live.protocol_version();
    open.features = feature::LOCAL_SUPPORTED;
    open.requested_lanes = vec![
        stream_kind::CONTROL,
        stream_kind::MEDIA_GROUP,
        stream_kind::MEDIA_CONTROL,
        stream_kind::RELIABLE_SIGNAL,
    ];

    let outcome = judge(
        &open,
        &context(&space, Plane::Live),
        &member(),
        &PlanePolicy::default(),
    );
    let Admission::Accept(accept, peer) = outcome else {
        panic!("a member asking for one thing we cannot do is still a member");
    };
    assert_eq!(
        accept.granted_lanes,
        vec![
            stream_kind::CONTROL,
            stream_kind::MEDIA_GROUP,
            stream_kind::MEDIA_CONTROL,
            stream_kind::RELIABLE_SIGNAL,
        ],
        "the implemented pair is granted when its feature was negotiated"
    );
    assert_eq!(peer.granted_lanes, accept.granted_lanes);

    let mut half = open.clone();
    half.requested_lanes = vec![stream_kind::CONTROL, stream_kind::MEDIA_GROUP];
    let Admission::Accept(accept, _) = judge(
        &half,
        &context(&space, Plane::Live),
        &member(),
        &PlanePolicy::default(),
    ) else {
        panic!("the ordinary control lane remains usable");
    };
    assert_eq!(accept.granted_lanes, vec![stream_kind::CONTROL]);

    let mut no_feature = open.clone();
    no_feature.features = feature::RESIDENCY_HINTS;
    let Admission::Accept(accept, _) = judge(
        &no_feature,
        &context(&space, Plane::Live),
        &member(),
        &PlanePolicy::default(),
    ) else {
        panic!("the ordinary lanes remain usable");
    };
    assert_eq!(
        accept.granted_lanes,
        vec![stream_kind::CONTROL, stream_kind::RELIABLE_SIGNAL]
    );
}

#[test]
fn a_freight_opening_that_names_lanes_is_granted_none_and_still_admitted() {
    // Freight has no lanes to give. Granting one would be a promise nothing can
    // keep: a peer taking it at its word writes a stream-kind byte, and
    // Freight's reader consumes that as the first quarter of its length prefix,
    // so the flow desynchronises on its first frame.
    //
    // And it is still admitted. Asking for a lane on a plane that has none is a
    // harmless mistake, and turning it into a failed connection would be a wire
    // rule this protocol deliberately does not have.
    let space = space();
    let mut open = opening(&space, Plane::Freight);
    open.requested_lanes = vec![stream_kind::CONTROL, stream_kind::RELIABLE_SIGNAL];
    let outcome = judge(
        &open,
        &context(&space, Plane::Freight),
        &member(),
        &PlanePolicy::default(),
    );
    let Admission::Accept(accept, peer) = outcome else {
        panic!("a member is admitted: {outcome:?}");
    };
    assert!(accept.granted_lanes.is_empty());
    assert!(peer.granted_lanes.is_empty());
}

#[test]
fn asking_for_only_unservable_lanes_is_still_a_refusal_where_lanes_exist() {
    // The rule above must not soften the one it sits beside. On Live, a peer
    // that asked for only things this build cannot serve gets a refusal rather
    // than an empty grant it would sit on waiting for.
    let space = space();
    let mut open = opening(&space, Plane::Live);
    open.plane = Plane::Live;
    open.protocol_version = Plane::Live.protocol_version();
    open.requested_lanes = vec![stream_kind::MEDIA_GROUP];
    assert_eq!(
        judge(
            &open,
            &context(&space, Plane::Live),
            &member(),
            &PlanePolicy::default()
        ),
        Admission::Refuse(Refusal::Refused)
    );
}
