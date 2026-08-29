//! Placement proof: a Run started on one Station is performed by another —
//! by consent, never by membership — and the one device that started it is
//! the one that accepts. Two Stations enter one Space over `MemNet` and
//! converge through the public Contact API; nothing here feeds frames.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mechanics::authorization::AuthorizedBodyKey;
use mechanics::ids::{ActorId, DeviceId, SpaceId};
use mechanics::station::Key;
use replica::body::{SchemaId, WorldId};
use replica::frontier::AuthorityFrontier;

use crate::coordinates::{
    ApproachRoute, CoordinatesAdmission, CoordinatesPayload, SignedCoordinates,
};
use crate::dispatch_tests::{
    echo_package, exec_schema, exec_signed_build, temp_root, DescribedWorld, ExecAtomicWorld,
    PermissiveAuthority,
};
use crate::lifecycle::{Activation, Runtime, Station};
use crate::registry::Builder;
use crate::world::{AuthorityView, Intent, LocalIdentity, PrincipalResolution, World};

const FOUNDER_SEED: [u8; 32] = [0x71; 32];
const RECOVERY_SEED: [u8; 32] = [0x72; 32];
const STATION_A_SEED: [u8; 32] = [0x73; 32];
const STATION_B_SEED: [u8; 32] = [0x74; 32];
/// The person on device A who starts work.
const WRITER_A_SEED: [u8; 32] = [0x75; 32];
/// The identity docked on device B that performs it.
const WRITER_B_SEED: [u8; 32] = [0x76; 32];
const SALT: [u8; 16] = [0x77; 16];

fn coordinates() -> (SpaceId, SignedCoordinates) {
    let rc = mechanics::space::recovery_commit(&mechanics::space::recovery_pub_of(&RECOVERY_SEED))
        .unwrap();
    let device = mechanics::space::recovery_pub_of(&FOUNDER_SEED);
    let ws = mechanics::space::derive_space_id(&device, &SALT, &rc);
    let (incept, _actor) =
        mechanics::actor::incept_single(&FOUNDER_SEED, &ws, [1u8; 16], [2u8; 16], None);
    let payload = CoordinatesPayload {
        space: <[u8; 29]>::try_from(ws.as_str().as_bytes()).unwrap(),
        salt: SALT,
        recovery_root: rc,
        founder_inception: postcard::to_stdvec(&incept).unwrap(),
        display_name_hint: "Placement Space".into(),
        approach_station: mechanics::actor::device_from_seed(&STATION_A_SEED)
            .key_bytes()
            .unwrap(),
        approach_nick_hint: "a".into(),
        approach_routes: vec![ApproachRoute::DirectIpv4 {
            ip: [127, 0, 0, 1],
            port: 4243,
        }],
        admission: CoordinatesAdmission::None,
    };
    (ws, SignedCoordinates::sign(payload, &STATION_A_SEED))
}

fn frontier() -> AuthorityFrontier {
    AuthorityFrontier::from_canonical_bytes(vec![3])
}

/// Every known device resolves to its own actor and may mutate: placement is
/// decided by consent and ownership below, never by this coarse gate.
struct PairAuthority;

impl AuthorityView for PairAuthority {
    fn resolve(&self, device: &DeviceId) -> Option<PrincipalResolution> {
        let hash = if device == &mechanics::actor::device_from_seed(&WRITER_B_SEED) {
            "b".repeat(64)
        } else {
            "a".repeat(64)
        };
        Some(PrincipalResolution {
            actor: ActorId::from_incept_hash(&hash),
            authority_frontier: frontier(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn authorize_mutation(
        &self,
        space: &SpaceId,
        world: &WorldId,
        actor: &ActorId,
        device: &DeviceId,
        authority_frontier: &AuthorityFrontier,
        parent_manifest_root: [u8; 32],
        implementation_id: [u8; 32],
        intent_digest: [u8; 32],
        demand: &[u8],
        operations_digest: [u8; 32],
        core_digest: [u8; 32],
    ) -> Result<Vec<u8>, mechanics::authorization::Refusal> {
        PermissiveAuthority.authorize_mutation(
            space,
            world,
            actor,
            device,
            authority_frontier,
            parent_manifest_root,
            implementation_id,
            intent_digest,
            demand,
            operations_digest,
            core_digest,
        )
    }
}

struct AnyKnownSigner;

impl replica::transaction::AuthoritySource for AnyKnownSigner {
    fn signer_authorized(&self, signer: &[u8; 32], _f: &AuthorityFrontier) -> bool {
        [WRITER_A_SEED, WRITER_B_SEED, STATION_A_SEED, STATION_B_SEED]
            .iter()
            .any(|seed| mechanics::actor::device_from_seed(seed).key_bytes() == Some(*signer))
    }
}

#[derive(Default)]
struct AcceptingIncorporator;

impl replica::convergence::AuthorityIncorporator for AcceptingIncorporator {
    fn incorporate_authority(
        &mut self,
        records: &[Vec<u8>],
    ) -> Result<replica::convergence::AuthorityBatchReceipt, replica::convergence::Failure> {
        Ok(replica::convergence::AuthorityBatchReceipt {
            space: coordinates().0,
            prior_frontier: AuthorityFrontier::from_canonical_bytes(vec![]),
            resulting_frontier: frontier(),
            batch_digest: *blake3::hash(&records.concat()).as_bytes(),
        })
    }
}

fn test_keys() -> Arc<dyn replica::body::BodyKeySource> {
    Arc::new(replica::body::StaticBodyKeys::new(
        AuthorizedBodyKey::for_authorized_epoch([0x78; 16], [0x79; 32]),
    ))
}

fn world_id() -> WorldId {
    WorldId::parse("com.example.exec-atomic").unwrap()
}

fn build() -> crate::exec::Build {
    exec_signed_build(&world_id())
}

fn runtime_at(root: &std::path::Path) -> Runtime {
    let inner = Arc::new(ExecAtomicWorld::with_build(build().id));
    let registry = Builder::new()
        .register(Arc::new(DescribedWorld {
            descriptor: inner.descriptor(),
            inner,
        }))
        .build()
        .unwrap();
    Runtime::open(
        root.to_path_buf(),
        registry,
        Arc::new(PairAuthority),
        test_keys(),
    )
}

fn activate(
    rt: &Runtime,
    coords: &SignedCoordinates,
    net: &comms::mem::MemNet,
    seed: [u8; 32],
    consent: crate::exec::Consent,
) -> Station {
    let transport: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&seed)));
    rt.materialize(coords)
        .unwrap()
        .open(Activation {
            consent,
            exec: Default::default(),
            planes: Default::default(),
            content: Default::default(),
            find: Default::default(),
            drain_deadline: Duration::from_secs(5),
            comms: Some(crate::contact_driver::CommsOptions {
                transport,
                station_seed: seed,
                authority: crate::plane::contact::Authority {
                    source: Arc::new(AnyKnownSigner),
                    incorporator: Arc::new(Mutex::new(AcceptingIncorporator)),
                    export: Arc::new(Vec::new),
                    frontier: Arc::new(frontier),
                },
                gossip: None,
                whole_deadline: Duration::from_secs(20),
                progress_deadline: Duration::from_secs(5),
                route_lease: Duration::from_secs(60),
            }),
            observation_capacity: 0,
        })
        .unwrap()
}

fn station_key(seed: &[u8; 32]) -> Key {
    Key::from_device(&mechanics::actor::device_from_seed(seed)).unwrap()
}

/// The key an Attempt names: a Session docks under its identity's device,
/// and that is the Station coordinate its leases carry.
fn performer_key(identity: &LocalIdentity) -> Key {
    Key::from_device(identity.device()).unwrap()
}

fn consent_to_implement() -> crate::exec::Consent {
    crate::exec::Consent::for_specs([exec_schema("agent.implement")])
}

/// Start one Run on `station`, directed at the device behind `target_seed`.
fn start_run_targeting(
    station: &Station,
    writer: &LocalIdentity,
    target_seed: &[u8; 32],
) -> crate::exec::RunId {
    let key = mechanics::actor::device_from_seed(target_seed)
        .key_bytes()
        .unwrap();
    let mut payload = b"to:".to_vec();
    payload.extend_from_slice(data_encoding::HEXLOWER.encode(&key).as_bytes());
    payload.extend_from_slice(b":run there");
    start_run(station, writer, &payload)
}

/// Start one Run on `station` as `writer`; returns its id.
fn start_run(station: &Station, writer: &LocalIdentity, payload: &[u8]) -> crate::exec::RunId {
    let session = station.dock(&world_id(), writer).unwrap();
    let request = crate::action::RequestId::mint();
    session
        .submit(
            writer
                .sign_action(
                    &session,
                    request,
                    Intent {
                        schema: SchemaId::parse("agent.request").unwrap(),
                        schema_version: 1,
                        payload: payload.to_vec(),
                    },
                )
                .unwrap(),
        )
        .unwrap();
    crate::exec::derive_run_id(
        station.space_id(),
        &world_id(),
        writer.device(),
        request.as_bytes(),
        0,
    )
}

fn perform(station: &Station, writer: &LocalIdentity) -> crate::exec::PerformReport {
    let session = station.dock(&world_id(), writer).unwrap();
    let package = echo_package(&build());
    // Drain until idle: one pass commits Try, the next Began, the next Returned.
    let mut merged = crate::exec::PerformReport::default();
    for _ in 0..8 {
        let report = session
            .perform(&package, |_| panic!("the echo handler stages no content"))
            .unwrap();
        if report.steps.is_empty() {
            break;
        }
        merged.steps.extend(report.steps);
    }
    merged
}

fn inspect(
    station: &Station,
    writer: &LocalIdentity,
    run: crate::exec::RunId,
    request: [u8; 16],
) -> crate::exec::WorkState {
    let session = station.dock(&world_id(), writer).unwrap();
    match session
        .work(
            crate::exec::WorkRequest::Inspect {
                world: world_id(),
                run,
            },
            request,
        )
        .unwrap()
    {
        crate::exec::WorkReply::State(state) => state,
        other => panic!("inspect answers state, not {other:?}"),
    }
}

fn accept(
    station: &Station,
    writer: &LocalIdentity,
    run: crate::exec::RunId,
    attempt: crate::exec::AttemptId,
) -> Result<(), crate::session::Failure> {
    let session = station.dock(&world_id(), writer).unwrap();
    let mut payload = b"accept:".to_vec();
    payload.extend_from_slice(&run.as_bytes());
    payload.extend_from_slice(&attempt.as_bytes());
    session
        .submit(
            writer
                .sign_action(
                    &session,
                    crate::action::RequestId::mint(),
                    Intent {
                        schema: SchemaId::parse("agent.request").unwrap(),
                        schema_version: 1,
                        payload,
                    },
                )
                .unwrap(),
        )
        .map(|_| ())
}

fn returned(report: &crate::exec::PerformReport) -> bool {
    report
        .steps
        .iter()
        .any(|step| matches!(step, crate::exec::PerformStep::Returned { .. }))
}

struct Pair {
    a: Station,
    b: Station,
    writer_a: LocalIdentity,
    writer_b: LocalIdentity,
}

fn pair(consent_b: crate::exec::Consent) -> Pair {
    let (_space, coords) = coordinates();
    let net = comms::mem::MemNet::new();
    let root_a = temp_root();
    let root_b = temp_root();
    let rt_a = runtime_at(&root_a);
    let rt_b = runtime_at(&root_b);
    let a = activate(
        &rt_a,
        &coords,
        &net,
        STATION_A_SEED,
        crate::exec::Consent::none(),
    );
    let b = activate(&rt_b, &coords, &net, STATION_B_SEED, consent_b);
    Pair {
        a,
        b,
        writer_a: Runtime::identity_from_seed(&WRITER_A_SEED),
        writer_b: Runtime::identity_from_seed(&WRITER_B_SEED),
    }
}

#[test]
fn a_run_started_on_one_station_is_performed_by_another_that_consents() {
    let pair = pair(consent_to_implement());
    let run = start_run(&pair.a, &pair.writer_a, b"perform elsewhere");

    // A never leases its own Run here — it has no handler package running —
    // so the only Attempt that can exist is B's.
    pair.b.contact(&station_key(&STATION_A_SEED)).unwrap();
    let report = perform(&pair.b, &pair.writer_b);
    assert!(returned(&report), "B performs A's Run: {report:?}");

    pair.a.contact(&station_key(&STATION_B_SEED)).unwrap();
    let state = inspect(&pair.a, &pair.writer_a, run, [0x01; 16]);
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(
        state.attempts[0].station,
        performer_key(&pair.writer_b),
        "the Attempt names the device that accepted it"
    );
    assert_eq!(state.attempts[0].returned.len(), 1);
    assert!(
        state.unresolved,
        "a Returned Outcome is not yet an accepted one"
    );
}

#[test]
fn a_station_without_consent_never_leases_another_devices_run() {
    let pair = pair(crate::exec::Consent::none());
    let run = start_run(&pair.a, &pair.writer_a, b"perform elsewhere");
    pair.b.contact(&station_key(&STATION_A_SEED)).unwrap();

    let report = perform(&pair.b, &pair.writer_b);
    assert!(
        report.steps.is_empty(),
        "no consent, no lease — not even a Try: {report:?}"
    );
    let state = inspect(&pair.b, &pair.writer_b, run, [0x02; 16]);
    assert!(state.attempts.is_empty());
    assert!(state.unresolved);
}

#[test]
fn consent_is_per_spec_and_a_device_always_performs_its_own_work() {
    // B consents to a Spec that is not the one A started.
    let pair = pair(crate::exec::Consent::for_specs([exec_schema(
        "agent.other",
    )]));
    let foreign = start_run(&pair.a, &pair.writer_a, b"not consented");
    pair.b.contact(&station_key(&STATION_A_SEED)).unwrap();
    assert!(perform(&pair.b, &pair.writer_b).steps.is_empty());
    assert!(inspect(&pair.b, &pair.writer_b, foreign, [0x03; 16])
        .attempts
        .is_empty());

    // B's own device's Run needs no consent at all.
    let own = start_run(&pair.b, &pair.writer_b, b"my own work");
    let report = perform(&pair.b, &pair.writer_b);
    assert!(
        returned(&report),
        "own work performs without consent: {report:?}"
    );
    assert_eq!(
        inspect(&pair.b, &pair.writer_b, own, [0x04; 16])
            .attempts
            .len(),
        1
    );
}

#[test]
fn two_stations_may_both_lease_and_only_the_owner_accepts() {
    let pair = pair(consent_to_implement());
    let run = start_run(&pair.a, &pair.writer_a, b"raced");
    pair.b.contact(&station_key(&STATION_A_SEED)).unwrap();

    // Both perform before either hears of the other: two Attempts, no
    // coordination, both visible after convergence.
    assert!(returned(&perform(&pair.a, &pair.writer_a)));
    assert!(returned(&perform(&pair.b, &pair.writer_b)));
    pair.a.contact(&station_key(&STATION_B_SEED)).unwrap();
    pair.b.contact(&station_key(&STATION_A_SEED)).unwrap();

    let on_a = inspect(&pair.a, &pair.writer_a, run, [0x05; 16]);
    let on_b = inspect(&pair.b, &pair.writer_b, run, [0x06; 16]);
    assert_eq!(on_a.attempts.len(), 2, "both Attempts are facts: {on_a:?}");
    assert_eq!(
        on_a.attempts, on_b.attempts,
        "and the same facts everywhere"
    );
    assert!(on_a
        .attempts
        .iter()
        .all(|attempt| attempt.returned.len() == 1));
    assert!(on_a.unresolved);

    // B returned an Outcome; B may not accept one. The device that started
    // the Run is its single writer for that choice.
    let by_b = on_a
        .attempts
        .iter()
        .find(|attempt| attempt.station == performer_key(&pair.writer_b))
        .unwrap();
    assert!(
        accept(&pair.b, &pair.writer_b, run, by_b.attempt).is_err(),
        "a non-owner's Accept is refused"
    );
    accept(&pair.a, &pair.writer_a, run, by_b.attempt).expect("the owner accepts");
    let closed = inspect(&pair.a, &pair.writer_a, run, [0x07; 16]);
    assert!(!closed.unresolved);
    assert_eq!(closed.accepted.len(), 1);
    assert_eq!(closed.accepted[0].attempt, by_b.attempt);
}

#[test]
fn a_directed_start_is_performed_only_by_the_named_station() {
    // A directs the Run at B; both consent to the Spec, so consent is not
    // what decides placement here — the target is.
    let pair = pair(consent_to_implement());
    let run = start_run_targeting(&pair.a, &pair.writer_a, &WRITER_B_SEED);
    pair.b.contact(&station_key(&STATION_A_SEED)).unwrap();

    // A holds no handler package running, but even a Station that did would
    // pass this by: A is not the target. B performs it.
    let report = perform(&pair.b, &pair.writer_b);
    assert!(
        returned(&report),
        "the named Station performs it: {report:?}"
    );

    pair.a.contact(&station_key(&STATION_B_SEED)).unwrap();
    let state = inspect(&pair.a, &pair.writer_a, run, [0x08; 16]);
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(state.attempts[0].station, performer_key(&pair.writer_b));
}

#[test]
fn a_station_not_named_by_a_directed_start_never_leases_it() {
    // A directs the Run at A itself; B consents to the Spec and holds the
    // Build, but is not the target, so it never leases.
    let pair = pair(consent_to_implement());
    let run = start_run_targeting(&pair.a, &pair.writer_a, &WRITER_A_SEED);
    pair.b.contact(&station_key(&STATION_A_SEED)).unwrap();

    let report = perform(&pair.b, &pair.writer_b);
    assert!(
        report.steps.is_empty(),
        "B is not the target: no Try, no lease: {report:?}"
    );
    assert!(inspect(&pair.b, &pair.writer_b, run, [0x09; 16])
        .attempts
        .is_empty());
}
