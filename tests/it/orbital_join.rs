//! C5 — real mechanics formation, invitation, entry, admission redemption,
//! and E2EE convergence over the orbital plane. No fixture authorities: every
//! seam is `SpaceAuthority` over real signed membership material.
//!
//! The flow proven here is the product's guided-join heir:
//! 1. the founder FORMS a Space (genesis, founding inception, epoch-0 sealed
//!    to itself) and commits product issues under real keys;
//! 2. an uninvited entrant converges the founder's material but holds no
//!    epoch key: every Body stays opaque — E2EE is the access control;
//! 3. the founder mints admission-bearing Coordinates; the joiner enters,
//!    self-incepts, and serves its admission redemption over Contact;
//! 4. the founder's Contact pull auto-approves (AddMember + epoch sealing);
//! 5. the joiner's next Contact imports membership + sealed keys FIRST (the
//!    authority-first durable phase), then the SAME pass upgrades previously
//!    opaque Bodies to interpreted product state;
//! 6. the admitted joiner docks, writes, and converges back.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use issues::contract::{self, IssueIntent, IssueQuery};
use issues::dto::IssueView;
use issues::IssuesWorld;
use lait::orbital::SpaceAuthority;
use replica::convergence::AuthorityIncorporator;
use runtime::{
    plane::contact::Authority, plane::Activation, plane::CommsOptions, world::Builder,
    world::Intent, world::Query, world::RequestId, Runtime, Session, Station,
};

const FOUNDER_SEED: [u8; 32] = [81u8; 32];
const JOINER_SEED: [u8; 32] = [82u8; 32];
const STRANGER_SEED: [u8; 32] = [83u8; 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A throwaway root that removes itself — see [`crate::head::temp_root`],
/// which is the one place that knows how.
fn temp_root(tag: &str) -> crate::head::TempRoot {
    crate::head::temp_root(&format!("join-{tag}"))
}

fn registry() -> runtime::world::Catalog {
    Builder::new()
        .register(Arc::new(IssuesWorld::new()))
        .build()
        .unwrap()
}

fn comms_for(
    transport: Arc<dyn comms::Transport>,
    seed: [u8; 32],
    mech: &SpaceAuthority,
) -> CommsOptions {
    let export_mech = mech.clone();
    let frontier_mech = mech.clone();
    CommsOptions {
        transport,
        station_seed: seed,
        authority: Authority {
            source: Arc::new(mech.clone()),
            incorporator: Arc::new(Mutex::new(mech.clone()))
                as Arc<Mutex<dyn AuthorityIncorporator + Send>>,
            export: Arc::new(move || export_mech.export_records()),
            frontier: Arc::new(move || frontier_mech.current_frontier()),
        },
        gossip: None,
        whole_deadline: Duration::from_secs(20),
        progress_deadline: Duration::from_secs(5),
        route_lease: Duration::from_secs(60),
    }
}

fn activate(
    root: &std::path::Path,
    seed: [u8; 32],
    mech: &SpaceAuthority,
    coords: &runtime::coordinates::SignedCoordinates,
    transport: Arc<dyn comms::Transport>,
) -> (Runtime, Station) {
    let rt = Runtime::open(
        root.to_path_buf(),
        registry(),
        Arc::new(mech.clone()),
        Arc::new(mech.clone()),
    );
    let station = rt
        .materialize(coords)
        .unwrap()
        .open(Activation {
            consent: Default::default(),
            exec: Default::default(),
            planes: Default::default(),
            content: Default::default(),
            find: Default::default(),
            drain_deadline: Duration::from_secs(5),
            comms: Some(comms_for(transport, seed, mech)),
            observation_capacity: 0,
        })
        .unwrap();
    (rt, station)
}

fn dock(station: &Station, seed: &[u8; 32]) -> Session {
    let identity = Runtime::identity_from_seed(seed);
    station.dock(&contract::world_id(), &identity).unwrap()
}

fn submit(
    session: &Session,
    seed: &[u8; 32],
    intent: &IssueIntent,
) -> Result<(), runtime::world::Failure> {
    let identity = Runtime::identity_from_seed(seed);
    let action = identity.sign_action(
        session,
        RequestId::mint(),
        Intent {
            schema: contract::issue_schema(),
            schema_version: contract::ISSUE_SCHEMA_VERSION,
            payload: intent.to_json(),
        },
    )?;
    session.submit(action).map(|_| ())
}

fn query<T: serde::de::DeserializeOwned>(session: &Session, q: &IssueQuery) -> T {
    let bytes = session
        .query(Query {
            schema: contract::issue_schema(),
            schema_version: contract::ISSUE_SCHEMA_VERSION,
            payload: q.to_json(),
            publication: None,
        })
        .unwrap()
        .bytes;
    serde_json::from_slice(&bytes).unwrap()
}

fn station_id(seed: &[u8; 32]) -> mechanics::station::Key {
    mechanics::station::Key::from_device(&mechanics::actor::device_from_seed(seed)).unwrap()
}

#[test]
fn form_invite_join_autoapprove_and_e2ee_convergence() {
    let net = comms::mem::MemNet::new();
    let t_founder: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&FOUNDER_SEED)));
    let t_joiner: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&JOINER_SEED)));
    let t_stranger: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&STRANGER_SEED)));

    // 1. Formation: real genesis + founding inception + epoch-0.
    let root_f = temp_root("founder");
    let (mech_f, coords) =
        SpaceAuthority::form(&root_f, &FOUNDER_SEED, "Joined Space", vec![]).unwrap();
    // The founder product-authority bootstrap the CLI composition root runs:
    // activate the IssuesWorld implementation + grant the founder Space caps,
    // so its own admin/contributor submits are authorized.
    crate::world_fixture::seed_founder_policy(&mech_f).unwrap();
    assert!(mech_f.am_i_member(), "the founder holds standing at birth");
    let (_rt_f, station_f) = activate(&root_f, FOUNDER_SEED, &mech_f, &coords, t_founder);
    let session_f = dock(&station_f, &FOUNDER_SEED);
    // Seed product state under real keys.
    let project = issues::ids::ProjectId::mint(&issues::ids::SystemUlidSource)
        .as_str()
        .to_string();
    submit(
        &session_f,
        &FOUNDER_SEED,
        &issues::contract::initialize_tracker_intent(
            "Joined Space",
            1,
            &project,
            "Core",
            "core",
            mechanics::actor::device_from_seed(&FOUNDER_SEED).as_str(),
        ),
    )
    .unwrap();
    let doc = issues::ids::DocId::mint(&issues::ids::SystemUlidSource)
        .as_str()
        .to_string();
    let founder_actor = {
        // The founder's actor id, from its own resolution.
        use runtime::world::AuthorityView;
        mech_f
            .resolve(&mechanics::actor::device_from_seed(&FOUNDER_SEED))
            .unwrap()
            .actor
    };
    submit(
        &session_f,
        &FOUNDER_SEED,
        &IssueIntent::IssueNew {
            duedate: None,
            estimate: None,
            doc: doc.clone(),
            project: project.clone(),
            title: "Secret plan".into(),
            priority: "high".into(),
            assignees: vec![],
            labels: vec![],
            new_labels: vec![],
            body: Some("the sealed body".into()),
            actor: founder_actor.as_str().to_string(),
            device: mechanics::actor::device_from_seed(&FOUNDER_SEED)
                .as_str()
                .to_string(),
            ts: 3,
        },
    )
    .unwrap();

    // 2. An UNINVITED entrant converges the founder's material but can read
    //    nothing: no admission, no epoch key — every Body stays opaque.
    let root_s = temp_root("stranger");
    let mech_s = SpaceAuthority::enter(&root_s, &STRANGER_SEED, &coords).unwrap();
    assert!(!mech_s.am_i_member());
    let (_rt_s, station_s) = activate(&root_s, STRANGER_SEED, &mech_s, &coords, t_stranger);
    let outcome = station_s.contact(&station_id(&FOUNDER_SEED)).unwrap();
    assert!(
        outcome.convergence.unsupported_retained >= 1,
        "material is retained opaquely, not read"
    );
    assert_eq!(outcome.convergence.accepted, 0, "nothing interpretable");
    // The stranger cannot even dock: no standing resolves.
    let stranger_identity = Runtime::identity_from_seed(&STRANGER_SEED);
    assert!(station_s
        .dock(&contract::world_id(), &stranger_identity)
        .is_err());

    // 3. The founder mints single-use admission-bearing Coordinates.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let admission = mech_f
        .mint_admission(
            &FOUNDER_SEED,
            3600,
            true,
            now,
            crate::world_fixture::role_evidence("contributor", [0u8; 32]),
        )
        .unwrap();
    let invite = mech_f
        .mint_coordinates(&FOUNDER_SEED, "Joined Space", vec![], Some(admission))
        .unwrap();

    // 4. The joiner enters with the invite; its FIRST pull (before admission)
    //    retains the founder's Bodies opaquely.
    let root_j = temp_root("joiner");
    let mech_j = SpaceAuthority::enter(&root_j, &JOINER_SEED, &invite).unwrap();
    assert!(!mech_j.am_i_member());
    let (_rt_j, station_j) = activate(&root_j, JOINER_SEED, &mech_j, &invite, t_joiner);
    let before = station_j.contact(&station_id(&FOUNDER_SEED)).unwrap();
    assert!(before.convergence.unsupported_retained >= 1);

    // 5. The founder pulls the joiner: the admission redemption rides the
    //    authority records and auto-approves (AddMember + epoch sealing).
    let outcome = station_f.contact(&station_id(&JOINER_SEED)).unwrap();
    let _ = outcome;
    // The founder's replay now admits the joiner's actor.
    {
        use runtime::world::AuthorityView;
        assert!(
            mech_f
                .resolve(&mechanics::actor::device_from_seed(&JOINER_SEED))
                .is_some(),
            "the joiner is admitted on the founder's replay"
        );
    }

    // 6. The joiner converges again. Under symmetric convergence the
    //    membership, sealed keys, and body upgrade may ALREADY have arrived on
    //    the reverse phase of the founder's step-5 dial (the responder
    //    incorporates the push asynchronously after acking it), so the claim
    //    is the OUTCOME — standing held, previously opaque material now
    //    interpreted — never which pass delivered it.
    let _ = station_j.contact(&station_id(&FOUNDER_SEED)).unwrap();
    let mut member = false;
    for _ in 0..100 {
        if mech_j.am_i_member() {
            member = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(member, "the joiner holds standing");
    let session_j = dock(&station_j, &JOINER_SEED);
    let view: IssueView = query(
        &session_j,
        &IssueQuery::View {
            doc: doc.clone(),
            me: None,
        },
    );
    assert_eq!(view.title, "Secret plan");
    assert_eq!(view.description, "the sealed body");

    // 7. The admitted joiner writes; the founder converges it back.
    let joiner_actor = {
        use runtime::world::AuthorityView;
        mech_j
            .resolve(&mechanics::actor::device_from_seed(&JOINER_SEED))
            .unwrap()
            .actor
    };
    submit(
        &session_j,
        &JOINER_SEED,
        &IssueIntent::Comment {
            id: None,
            parent: None,
            doc: doc.clone(),
            body: "joined and commenting".into(),
            actor: joiner_actor.as_str().to_string(),
            device: mechanics::actor::device_from_seed(&JOINER_SEED)
                .as_str()
                .to_string(),
            ts: 9,
        },
    )
    .unwrap();
    station_f.contact(&station_id(&JOINER_SEED)).unwrap();
    // Discussion is its own page; `View` is the bounded summary.
    let detail: contract::IssueDetailProjection = query(
        &session_f,
        &IssueQuery::Detail {
            doc: doc.clone(),
            me: None,
            pages: contract::IssueDetailPages::default(),
        },
    );
    assert_eq!(detail.comments.items.len(), 1);
    assert_eq!(detail.comments.items[0].body, "joined and commenting");

    let _ = station_f.vacate();
    let _ = station_j.vacate();
    let _ = station_s.vacate();
    let _ = std::fs::remove_dir_all(&root_f);
    let _ = std::fs::remove_dir_all(&root_j);
    let _ = std::fs::remove_dir_all(&root_s);
}

#[test]
fn a_joiners_own_dial_pushes_its_admission_and_the_founder_redeems() {
    // The admission courier for a peer nothing can dial — a browser tab. The
    // joiner's OWN dial pushes its pending admission request on the symmetric
    // reverse phase (an unadmitted signer builds an authority-only transfer),
    // and the founder's incorporator redeems it. Gossip is off and the founder
    // NEVER dials the joiner, so the reverse push is the only way the request
    // can travel.
    let net = comms::mem::MemNet::new();
    let t_founder: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&FOUNDER_SEED)));
    let t_joiner: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&JOINER_SEED)));

    let root_f = temp_root("push-founder");
    let (mech_f, coords) =
        SpaceAuthority::form(&root_f, &FOUNDER_SEED, "Pushed Space", vec![]).unwrap();
    crate::world_fixture::seed_founder_policy(&mech_f).unwrap();
    let (_rt_f, _station_f) = activate(&root_f, FOUNDER_SEED, &mech_f, &coords, t_founder);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // Single-use, exactly the shape the browser harness ships — the reverse
    // push must redeem under the nonce cap, not around it.
    let admission = mech_f
        .mint_admission(
            &FOUNDER_SEED,
            3600,
            false,
            now,
            crate::world_fixture::role_evidence("contributor", [0u8; 32]),
        )
        .unwrap();
    let invite = mech_f
        .mint_coordinates(&FOUNDER_SEED, "Pushed Space", vec![], Some(admission))
        .unwrap();

    let root_j = temp_root("push-joiner");
    let mech_j = SpaceAuthority::enter(&root_j, &JOINER_SEED, &invite).unwrap();
    assert!(!mech_j.am_i_member(), "entry holds the admission pending");
    let (_rt_j, station_j) = activate(&root_j, JOINER_SEED, &mech_j, &invite, t_joiner);

    // The joiner dials ONCE. The forward pull retains the founder's material;
    // the reverse phase pushes the joiner's pending admission request.
    station_j.contact(&station_id(&FOUNDER_SEED)).unwrap();

    // The responder incorporates the push after acking receipt, so the
    // founder's ledger admits the joiner a beat later — poll for it.
    let joiner_device = mechanics::actor::device_from_seed(&JOINER_SEED);
    let mut admitted = false;
    for _ in 0..100 {
        use runtime::world::AuthorityView;
        if mech_f.resolve(&joiner_device).is_some() {
            admitted = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        admitted,
        "the founder redeemed the admission the joiner's own dial pushed"
    );

    // The joiner's NEXT dial imports its membership + sealed keys — the loop a
    // tab's await-admission runs.
    station_j.contact(&station_id(&FOUNDER_SEED)).unwrap();
    assert!(
        mech_j.am_i_member(),
        "the joiner holds standing after re-pulling the redeemed admission"
    );

    let _ = station_j.vacate();
    let _ = std::fs::remove_dir_all(&root_f);
    let _ = std::fs::remove_dir_all(&root_j);
}
