//! C4 / G8 — the product World adapter's mapping/parity harness, over the
//! final public APIs and store.
//!
//! The test plays the daemon's role (minting ids, stamping timestamps,
//! resolving refs from the Snapshot query) and drives every issue-family
//! behavior through `IssuesWorld` Sessions on isolated orbital stores: create/
//! edit/board/assign/label/comment/link/parent/work-state/delete/restore,
//! bounded exact-publication projections, `KEY-n` aliases, idempotent no-ops,
//! restart durability, and
//! two-Station product convergence over the real Contact plane. Legacy
//! production paths are untouched (the C5 cutover switches them atomically).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use issues::dto::{
    BoardPage, IssueRelationDto, IssueView, LabelDto, ProjectDto, Row, StatusCategory,
};
use issues::ids::{
    ActorId, BaselineId, DeviceId, DocId, LabelId, ObservationId, ProjectId, SpecId,
    SystemUlidSource,
};
use lait::world::contract::{self, IssueIntent, IssueQuery, Pos, WorkAction};
use lait::world::IssuesWorld;
use mechanics::authorization::AuthorizedBodyKey;
use replica::frontier::AuthorityFrontier;
use runtime::{
    plane::contact::Authority, plane::Activation, plane::CommsOptions, world::Builder,
    world::Intent, world::LocalIdentity, world::Query, world::Rejection, world::RequestId,
    world::SignedWorldAction, Runtime, Session, Station,
};

const FOUNDER_SEED: [u8; 32] = [7u8; 32];
const RECOVERY_SEED: [u8; 32] = [20u8; 32];
const STATION_A_SEED: [u8; 32] = [61u8; 32];
const STATION_B_SEED: [u8; 32] = [62u8; 32];
const WRITER_SEED: [u8; 32] = [63u8; 32];
const EPOCH: [u8; 16] = [19u8; 16];
const EPOCH_KEY: [u8; 32] = [21u8; 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-parity-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn first_page() -> contract::PageRequest {
    contract::PageRequest::default()
}

fn coordinates() -> runtime::coordinates::SignedCoordinates {
    use runtime::coordinates::{ApproachRoute, CoordinatesAdmission, CoordinatesPayload};
    let rc = mechanics::space::recovery_commit(&mechanics::space::recovery_pub_of(&RECOVERY_SEED))
        .unwrap();
    let device = mechanics::space::recovery_pub_of(&FOUNDER_SEED);
    let ws = mechanics::space::derive_space_id(&device, &[9u8; 16], &rc);
    let (incept, _actor) =
        mechanics::actor::incept_single(&FOUNDER_SEED, &ws, [1u8; 16], [2u8; 16], None);
    let payload = CoordinatesPayload {
        space: <[u8; 29]>::try_from(ws.as_str().as_bytes()).unwrap(),
        salt: [9u8; 16],
        recovery_root: rc,
        founder_inception: postcard::to_stdvec(&incept).unwrap(),
        display_name_hint: "Parity Space".into(),
        approach_station: mechanics::actor::device_from_seed(&STATION_A_SEED)
            .key_bytes()
            .unwrap(),
        approach_nick_hint: "a".into(),
        approach_routes: vec![ApproachRoute::DirectIpv4 {
            ip: [127, 0, 0, 1],
            port: 4242,
        }],
        admission: CoordinatesAdmission::None,
    };
    runtime::coordinates::SignedCoordinates::sign(payload, &STATION_A_SEED)
}

struct WriterAuthority;
impl runtime::world::AuthorityView for WriterAuthority {
    fn resolve(&self, _device: &DeviceId) -> Option<runtime::world::PrincipalResolution> {
        Some(runtime::world::PrincipalResolution {
            actor: my_actor(),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![8]),
        })
    }
}

struct AnyKnownSigner;
impl replica::transaction::AuthoritySource for AnyKnownSigner {
    fn signer_authorized(&self, signer: &[u8; 32], _f: &AuthorityFrontier) -> bool {
        [WRITER_SEED, STATION_A_SEED, STATION_B_SEED]
            .iter()
            .any(|seed| mechanics::actor::device_from_seed(seed).key_bytes() == Some(*signer))
    }
}

struct AcceptingIncorporator;
impl replica::convergence::AuthorityIncorporator for AcceptingIncorporator {
    fn incorporate_authority(
        &mut self,
        _records: &[Vec<u8>],
    ) -> Result<replica::convergence::AuthorityBatchReceipt, replica::convergence::Failure> {
        Ok(replica::convergence::AuthorityBatchReceipt {
            space: coordinates().verify().unwrap().space.clone(),
            prior_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![]),
            resulting_frontier: AuthorityFrontier::from_canonical_bytes(vec![8]),
            batch_digest: *blake3::hash(&_records.concat()).as_bytes(),
        })
    }
}

fn my_actor() -> ActorId {
    ActorId::from_incept_hash(&"f".repeat(64))
}

fn my_device() -> DeviceId {
    mechanics::actor::device_from_seed(&WRITER_SEED)
}

fn product_runtime(root: &std::path::Path) -> Runtime {
    let registry = Builder::new()
        .register(Arc::new(IssuesWorld::new()))
        .build()
        .unwrap();
    Runtime::open(
        root.to_path_buf(),
        registry,
        Arc::new(WriterAuthority),
        Arc::new(replica::body::StaticBodyKeys::new(
            AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
        )),
    )
}

/// The daemon-side driver: docks a session and adapts intents/queries.
struct Driver {
    session: Session,
    writer: LocalIdentity,
    now: u64,
}

impl Driver {
    fn dock(station: &Station) -> Self {
        let writer = Runtime::identity_from_seed(&WRITER_SEED);
        let session = station.dock(&contract::world_id(), &writer).unwrap();
        Self {
            session,
            writer,
            now: 1_700_000_000,
        }
    }

    fn ts(&mut self) -> u64 {
        self.now += 1;
        self.now
    }

    fn signed(&self, intent: &IssueIntent) -> SignedWorldAction {
        self.signed_at(RequestId::mint(), intent)
    }

    fn signed_at(&self, request: RequestId, intent: &IssueIntent) -> SignedWorldAction {
        self.writer
            .sign_action(
                &self.session,
                request,
                Intent {
                    schema: contract::issue_schema(),
                    schema_version: contract::ISSUE_SCHEMA_VERSION,
                    payload: intent.to_json(),
                },
            )
            .unwrap()
    }

    fn submit(
        &self,
        intent: &IssueIntent,
    ) -> Result<contract::IssueEffect, runtime::world::Failure> {
        let committed = self.session.submit(self.signed(intent))?;
        Ok(contract::IssueEffect::from_json(&committed.effect).unwrap())
    }

    fn query_raw(&self, query: &IssueQuery) -> Vec<u8> {
        self.session
            .query(Query {
                schema: contract::issue_schema(),
                schema_version: contract::ISSUE_SCHEMA_VERSION,
                payload: query.to_json(),
                publication: None,
            })
            .unwrap()
            .bytes
    }

    fn query<T: serde::de::DeserializeOwned>(&self, query: &IssueQuery) -> T {
        serde_json::from_slice(&self.query_raw(query)).unwrap()
    }

    fn query_at<T: serde::de::DeserializeOwned>(
        &self,
        query: &IssueQuery,
        publication: runtime::publication::WorldPublicationId,
    ) -> T {
        let projection = self
            .session
            .query(Query {
                schema: contract::issue_schema(),
                schema_version: contract::ISSUE_SCHEMA_VERSION,
                payload: query.to_json(),
                publication: Some(publication.publication),
            })
            .unwrap();
        serde_json::from_slice(&projection.bytes).unwrap()
    }

    /// Resolve a human alias or canonical id through the exact publication's
    /// shared Corpus selector, the same primitive used by the app and MCP.
    fn resolve(&self, reff: &str) -> Option<String> {
        let projection = self
            .session
            .query(Query {
                schema: contract::issue_schema(),
                schema_version: contract::ISSUE_SCHEMA_VERSION,
                payload: IssueQuery::Resolve {
                    entity: contract::ResolveEntity::Issue,
                    selector: reff.to_owned(),
                    project: None,
                }
                .to_json(),
                publication: None,
            })
            .ok()?;
        serde_json::from_slice::<contract::ResolvedEntity>(&projection.bytes)
            .ok()
            .map(|resolved| resolved.id)
    }
}

fn setup(root: &std::path::Path) -> (Runtime, Station) {
    let rt = product_runtime(root);
    let station = rt.create().unwrap().open(Activation::offline()).unwrap();
    (rt, station)
}

fn seed_project(driver: &mut Driver) -> String {
    let ts = driver.ts();
    let project = ProjectId::mint(&SystemUlidSource).as_str().to_string();
    driver
        .submit(&lait::world::contract::initialize_tracker_intent(
            "Parity Space",
            ts,
            &project,
            "Engineering",
            "eng",
            my_device().as_str(),
        ))
        .unwrap();
    project
}

fn seed_space(driver: &mut Driver) -> (String, String, String) {
    let project = seed_project(driver);
    let doc = DocId::mint(&SystemUlidSource).as_str().to_string();
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::IssueNew {
            duedate: None,
            estimate: None,
            doc: doc.clone(),
            project: project.clone(),
            title: "First issue".into(),
            priority: "high".into(),
            assignees: vec![my_actor().as_str().to_string()],
            labels: vec![],
            new_labels: vec![],
            body: Some("the description".into()),
            actor: my_actor().as_str().to_string(),
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    (project, doc, "ENG-1".to_string())
}

fn create_board_issue(driver: &mut Driver, project: &str, title: String) -> String {
    let doc = DocId::mint(&SystemUlidSource).as_str().to_string();
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::IssueNew {
            duedate: None,
            estimate: None,
            doc: doc.clone(),
            project: project.into(),
            title,
            priority: "none".into(),
            assignees: vec![],
            labels: vec![],
            new_labels: vec![],
            body: None,
            actor: my_actor().as_str().into(),
            device: my_device().as_str().into(),
            ts,
        })
        .unwrap();
    doc
}

#[test]
fn board_continuation_is_exact_across_a_leaf_split() {
    let root = temp_root("board-block-page");
    let (_rt, station) = setup(&root);
    let mut driver = Driver::dock(&station);
    let project = seed_project(&mut driver);
    let mut expected = Vec::new();

    // Fill the deterministic seed leaf exactly. The captured continuation is
    // pinned to this pre-split publication and must remain usable after a
    // later action changes the block topology.
    for ordinal in 0..issues::records::BOARD_BLOCK_CAPACITY {
        expected.push(create_board_issue(
            &mut driver,
            &project,
            format!("Board issue {ordinal}"),
        ));
    }
    let first_request = contract::PageRequest {
        limit: 64,
        cursor: None,
    };
    let old_first: BoardPage = driver.query(&IssueQuery::Board {
        project: project.clone(),
        me: None,
        page: first_request,
    });
    assert_eq!(old_first.rows.items.len(), 64);
    let old_cursor = old_first.rows.next_cursor.clone().expect("second page");
    let old_publication = contract::page_publication(&contract::PageRequest {
        limit: 64,
        cursor: Some(old_cursor.clone()),
    })
    .expect("exact continuation publication");

    // The next insertion encounters a full leaf. It must split the leaf and
    // commit bounded exact-transition overlays plus the user move rather than
    // refusing because flat labels are dense.
    let newest = create_board_issue(&mut driver, &project, "Split insertion".into());
    expected.push(newest.clone());

    let old_second: BoardPage = driver.query_at(
        &IssueQuery::Board {
            project: project.clone(),
            me: None,
            page: contract::PageRequest {
                limit: 64,
                cursor: Some(old_cursor),
            },
        },
        old_publication,
    );
    assert_eq!(old_second.rows.items.len(), 64);
    assert!(old_second.rows.next_cursor.is_none());
    let old_docs = old_first
        .rows
        .items
        .iter()
        .chain(&old_second.rows.items)
        .map(|row| row.doc_id.as_str().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(old_docs.len(), issues::records::BOARD_BLOCK_CAPACITY);
    assert!(!old_docs.contains(&newest));

    // The current publication walks block order and then local member order.
    // A 64-row page ends at the split seam; resuming must neither interleave
    // equal local labels across blocks nor omit/duplicate a card.
    let mut request = contract::PageRequest {
        limit: 64,
        cursor: None,
    };
    let mut current_docs = Vec::new();
    let mut page_sizes = Vec::new();
    let mut current_publication = None;
    loop {
        let board: BoardPage = driver.query(&IssueQuery::Board {
            project: project.clone(),
            me: None,
            page: request.clone(),
        });
        current_publication.get_or_insert_with(|| board.rows.publication.clone());
        assert_eq!(current_publication.as_ref(), Some(&board.rows.publication));
        page_sizes.push(board.rows.items.len());
        current_docs.extend(
            board
                .rows
                .items
                .iter()
                .map(|row| row.doc_id.as_str().to_owned()),
        );
        let Some(cursor) = board.rows.next_cursor else {
            break;
        };
        request.cursor = Some(cursor);
    }
    assert_eq!(page_sizes, vec![64, 64, 1]);
    let unique = current_docs
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(current_docs.len(), expected.len());
    assert_eq!(unique.len(), expected.len());
    assert!(unique.contains(&newest));
    assert!(expected.iter().all(|doc| unique.contains(doc)));

    let _ = station.vacate();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn verification_binds_the_issue_and_started_run_in_one_effect() {
    let root = temp_root("verification-run");
    let (_rt, station) = setup(&root);
    let mut driver = Driver::dock(&station);
    let (_project, doc, _) = seed_space(&mut driver);
    let source_ref = station
        .content_write(
            &driver.writer,
            [0x51; 16],
            &mut std::io::Cursor::new(b"pinned repository source"),
        )
        .unwrap();
    let source = data_encoding::HEXLOWER.encode(source_ref.as_bytes());
    let build = data_encoding::HEXLOWER.encode(&[0x52; 32]);

    // A caller cannot invent a product link to a different Run. The World
    // recomputes the Runtime coordinate before it stages either half.
    let bad_request = RequestId::from_bytes([0x53; 16]);
    let bad_run = data_encoding::HEXLOWER.encode(&[0x54; 16]);
    let before_bad = station.frontier();
    let bad_ts = driver.ts();
    let bad = IssueIntent::Verify {
        doc: doc.clone(),
        run: bad_run,
        source: source.clone(),
        build: build.clone(),
        actor: my_actor().as_str().into(),
        device: my_device().as_str().into(),
        ts: bad_ts,
    };
    assert!(driver
        .session
        .submit(driver.signed_at(bad_request, &bad))
        .is_err());
    assert_eq!(station.frontier(), before_bad);
    let unchanged: IssueView = driver.query(&IssueQuery::View {
        doc: doc.clone(),
        me: None,
    });
    assert!(unchanged.checks.is_empty());

    let request = RequestId::from_bytes([0x55; 16]);
    let run = runtime::exec::derive_run_id(
        station.space_id(),
        &contract::world_id(),
        driver.writer.device(),
        request.as_bytes(),
        0,
    );
    let run_text = data_encoding::HEXLOWER.encode(&run.as_bytes());
    let ts = driver.ts();
    let intent = IssueIntent::Verify {
        doc: doc.clone(),
        run: run_text.clone(),
        source: source.clone(),
        build: build.clone(),
        actor: my_actor().as_str().into(),
        device: my_device().as_str().into(),
        ts,
    };
    let action = driver.signed_at(request, &intent);
    let replay = action.clone();
    let committed = driver.session.submit(action).unwrap();
    let after_first = station.frontier();
    let replayed = driver.session.submit(replay).unwrap();
    assert_eq!(replayed, committed);
    assert_eq!(station.frontier(), after_first);

    let effect = contract::IssueEffect::from_json(&committed.effect).unwrap();
    assert_eq!(effect.doc.as_deref(), Some(doc.as_str()));
    assert_eq!(effect.run.as_deref(), Some(run_text.as_str()));
    let run_body = replica::body::BodyKey::new(
        contract::world_id(),
        replica::body::BodyId::from_bytes(run.as_bytes()),
    );
    assert!(committed.bodies.contains(&run_body));

    let view: IssueView = driver.query(&IssueQuery::View { doc, me: None });
    assert_eq!(view.checks.len(), 1);
    let check = &view.checks[0];
    assert_eq!(check.run, run_text);
    assert_eq!(check.spec, contract::VERIFY_SPEC);
    assert_eq!(check.version, contract::VERIFY_SPEC_VERSION);
    assert_eq!(check.build, build);
    assert_eq!(check.source, source);
    assert_eq!(check.state, "started");

    let state = driver
        .session
        .work(
            runtime::exec::WorkRequest::Inspect {
                world: contract::world_id(),
                run,
            },
            [0x56; 16],
        )
        .unwrap();
    let runtime::exec::WorkReply::State(state) = state else {
        panic!("inspect must return the started verification Run");
    };
    assert_eq!(state.run, run);
    assert_eq!(state.event_count, 1);
    assert!(state.unresolved);
    assert!(state.attempts.is_empty());
}

#[test]
fn an_issue_packet_stays_pinned_while_the_next_spec_revision_is_drafted() {
    let root = temp_root("spec-packet");
    let (_rt, station) = setup(&root);
    let mut driver = Driver::dock(&station);
    let (project, doc, _) = seed_space(&mut driver);
    let spec = SpecId::mint(&SystemUlidSource).as_str().to_string();

    let ts = driver.ts();
    driver
        .submit(&IssueIntent::SpecCreate {
            spec: spec.clone(),
            project: project.clone(),
            kind: issues::spec::Kind::Requirement,
            title: "Login is race-free".into(),
            text: "A login creates at most one active session.".into(),
            links: vec![],
            actor: my_actor().as_str().into(),
            device: my_device().as_str().into(),
            ts,
        })
        .unwrap();
    let draft: issues::spec::SpecView = driver.query(&IssueQuery::Spec { spec: spec.clone() });

    let ts = driver.ts();
    driver
        .submit(&IssueIntent::SpecState {
            spec: spec.clone(),
            expected: draft.revision,
            state: issues::spec::State::Issued,
            actor: my_actor().as_str().into(),
            device: my_device().as_str().into(),
            ts,
        })
        .unwrap();
    let issued: issues::spec::SpecView = driver.query(&IssueQuery::Spec { spec: spec.clone() });
    assert_eq!(issued.state, issues::spec::State::Issued);

    let baseline = BaselineId::mint(&SystemUlidSource).as_str().to_string();
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::BaselineCreate {
            baseline: baseline.clone(),
            project: project.clone(),
            name: "Login v1".into(),
            members: vec![issues::spec::SpecRef {
                spec: spec.clone(),
                revision: issued.revision.clone(),
            }],
            actor: my_actor().as_str().into(),
            device: my_device().as_str().into(),
            ts,
        })
        .unwrap();
    let draft_baseline: issues::spec::BaselineView = driver.query(&IssueQuery::Baseline {
        baseline: baseline.clone(),
    });

    let ts = driver.ts();
    driver
        .submit(&IssueIntent::BaselineState {
            baseline: baseline.clone(),
            expected: draft_baseline.revision,
            state: issues::spec::State::Issued,
            actor: my_actor().as_str().into(),
            device: my_device().as_str().into(),
            ts,
        })
        .unwrap();
    let issued_baseline: issues::spec::BaselineView = driver.query(&IssueQuery::Baseline {
        baseline: baseline.clone(),
    });

    let binding = issues::spec::BaselineRef {
        baseline,
        revision: issued_baseline.revision,
    };
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::IssueBaseline {
            doc: doc.clone(),
            baseline: Some(binding.clone()),
            device: my_device().as_str().into(),
            ts,
        })
        .unwrap();

    let before: issues::spec::Packet = driver.query(&IssueQuery::Packet { doc: doc.clone() });
    assert_eq!(before.baseline, Some(binding.clone()));
    assert_eq!(before.governing.len(), 1);
    assert_eq!(before.governing[0].revision, issued.revision);
    assert!(before.conflicts.is_empty());

    let ts = driver.ts();
    driver
        .submit(&IssueIntent::SpecRevise {
            spec: spec.clone(),
            expected: issued.revision.clone(),
            title: None,
            text: Some("A login creates exactly one active session.".into()),
            links: None,
            plan: None,
            actor: my_actor().as_str().into(),
            device: my_device().as_str().into(),
            ts,
        })
        .unwrap();
    let current: issues::spec::SpecView = driver.query(&IssueQuery::Spec { spec });
    assert_eq!(current.state, issues::spec::State::Draft);
    assert_eq!(current.issued, vec![issued.revision.clone()]);

    let after: issues::spec::Packet = driver.query(&IssueQuery::Packet { doc });
    assert_eq!(after.baseline, Some(binding));
    assert_eq!(after.governing.len(), 1);
    assert_eq!(after.governing[0].revision, issued.revision);
    assert!(after.conflicts.is_empty());
}

/// An Observation is a note about the graph, and the whole point of the concept
/// is what it *cannot* do: it must never govern an Issue, and it must never be
/// mistaken for the verification coverage a Link asserts. Both are one-line
/// changes away from being wrong forever, so both are asserted here rather than
/// left to the comments that explain them.
#[test]
fn an_observation_never_governs_and_never_becomes_a_link() {
    let root = temp_root("spec-observation");
    let (_rt, station) = setup(&root);
    let mut driver = Driver::dock(&station);
    let (project, doc, reff) = seed_space(&mut driver);
    let spec = SpecId::mint(&SystemUlidSource).as_str().to_string();

    let ts = driver.ts();
    driver
        .submit(&IssueIntent::SpecCreate {
            spec: spec.clone(),
            project,
            kind: issues::spec::Kind::Requirement,
            title: "Login is race-free".into(),
            text: String::new(),
            links: vec![],
            actor: my_actor().as_str().into(),
            device: my_device().as_str().into(),
            ts,
        })
        .unwrap();
    let draft: issues::spec::SpecView = driver.query(&IssueQuery::Spec { spec: spec.clone() });
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::SpecState {
            spec: spec.clone(),
            expected: draft.revision,
            state: issues::spec::State::Issued,
            actor: my_actor().as_str().into(),
            device: my_device().as_str().into(),
            ts,
        })
        .unwrap();

    // The same shape a `governs` Link would take, filed as a note instead.
    let observation = ObservationId::mint(&SystemUlidSource).as_str().to_string();
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::SpecObserve {
            observation: observation.clone(),
            spec: spec.clone(),
            rel: issues::spec::Rel::Governs,
            target: issues::spec::Target::Issue { issue: doc.clone() },
            note: format!("this looks like it constrains {reff}"),
            actor: my_actor().as_str().into(),
            device: my_device().as_str().into(),
            ts,
        })
        .unwrap();

    let notes: contract::Page<issues::spec::Observation> =
        driver.query(&IssueQuery::SpecObservations {
            project: None,
            page: first_page(),
        });
    assert_eq!(notes.items.len(), 1);
    assert_eq!(notes.items[0].observation, observation);
    assert_eq!(notes.items[0].observer, my_actor().as_str());

    // Filed, readable — and governing nothing.
    let packet: issues::spec::Packet = driver.query(&IssueQuery::Packet { doc });
    assert!(packet.governing.is_empty());
    assert!(packet.guidance.is_empty());
    assert!(packet.conflicts.is_empty());

    // And invisible to the link graph, which is what coverage is read from.
    let references: contract::Page<issues::spec::SpecReference> =
        driver.query(&IssueQuery::SpecReferences {
            project: None,
            page: first_page(),
        });
    assert!(references.items.is_empty());
    let view: issues::spec::SpecView = driver.query(&IssueQuery::Spec { spec: spec.clone() });
    assert!(view.body.links.is_empty());

    // Retracting takes it back without touching the revision trail.
    let before: contract::Page<issues::spec::Revision> = driver.query(&IssueQuery::SpecHistory {
        spec: spec.clone(),
        page: first_page(),
    });
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::SpecRetract {
            spec: spec.clone(),
            observation,
            actor: my_actor().as_str().into(),
            device: my_device().as_str().into(),
            ts,
        })
        .unwrap();
    let after: contract::Page<issues::spec::Observation> =
        driver.query(&IssueQuery::SpecObservations {
            project: None,
            page: first_page(),
        });
    assert!(after.items.is_empty());
    let trail: contract::Page<issues::spec::Revision> = driver.query(&IssueQuery::SpecHistory {
        spec,
        page: first_page(),
    });
    assert_eq!(trail.items.len(), before.items.len());
}

#[test]
fn the_full_issue_surface_round_trips_with_legacy_shapes() {
    let root = temp_root("surface");
    let (_rt, station) = setup(&root);
    let mut driver = Driver::dock(&station);
    let (project, doc, alias) = seed_space(&mut driver);

    // Aliases: ENG-1 resolves to the doc; the canonical prefix resolves too.
    assert_eq!(driver.resolve(&alias).as_deref(), Some(doc.as_str()));
    assert_eq!(driver.resolve(&doc[..12]).as_deref(), Some(doc.as_str()));

    // The IssueView carries the legacy shape.
    let view: IssueView = driver.query(&IssueQuery::View {
        doc: doc.clone(),
        me: Some(my_actor().as_str().to_string()),
    });
    assert_eq!(view.schema_version, 5);
    assert_eq!(view.title, "First issue");
    assert_eq!(view.description, "the description");
    assert_eq!(view.status, "backlog");
    assert_eq!(view.priority, issues::dto::Priority::High);
    assert_eq!(view.assignees, vec![my_actor()]);
    assert_eq!(view.key_alias.as_deref(), Some("ENG-1"));

    // A second issue gets ENG-2 and sits above on the board (insert-at-top).
    let doc2 = DocId::mint(&SystemUlidSource).as_str().to_string();
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::IssueNew {
            duedate: None,
            estimate: None,
            doc: doc2.clone(),
            project: project.clone(),
            title: "Second issue".into(),
            priority: "low".into(),
            assignees: vec![],
            labels: vec![],
            new_labels: vec![],
            body: None,
            actor: my_actor().as_str().to_string(),
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    assert_eq!(driver.resolve("ENG-2").as_deref(), Some(doc2.as_str()));

    // List: priority desc (high first), then DocId asc.
    let rows: contract::Page<Row> = driver.query(&IssueQuery::List {
        project: Some(project.clone()),
        label: None,
        status: None,
        milestone: None,
        mine: None,
        all: false,
        me: Some(my_actor().as_str().to_string()),
        page: first_page(),
    });
    assert_eq!(rows.items.len(), 2);
    assert_eq!(rows.items[0].title, "First issue");
    assert_eq!(rows.items[0].assignee_summary, "you");
    assert_eq!(rows.items[1].title, "Second issue");

    // Board: backlog column holds both, newest insert on top.
    let board: BoardPage = driver.query(&IssueQuery::Board {
        project: project.clone(),
        me: None,
        page: first_page(),
    });
    assert_eq!(board.schema_version, 5);
    assert_eq!(board.workflow.len(), 4);
    assert_eq!(board.workflow[0].id, "backlog");
    assert_eq!(board.rows.items.len(), 2);
    assert_eq!(board.rows.items[0].title, "Second issue");

    // Move ENG-2 after ENG-1 (the legacy Before/After math).
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::IssueMove {
            doc: doc2.clone(),
            project: None,
            pos: Some(Pos::After { doc: doc.clone() }),
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    let board: BoardPage = driver.query(&IssueQuery::Board {
        project: project.clone(),
        me: None,
        page: first_page(),
    });
    assert_eq!(board.rows.items[0].title, "First issue");
    assert_eq!(board.rows.items[1].title, "Second issue");

    // Labels create-on-first-use; label filter applies.
    let label_id = LabelId::mint(&SystemUlidSource).as_str().to_string();
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::Label {
            doc: doc.clone(),
            add: vec![],
            new_labels: vec![contract::NewLabel {
                id: label_id.clone(),
                name: "bug".into(),
                color: "red".into(),
            }],
            remove: vec![],
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    let labels: contract::Page<LabelDto> = driver.query(&IssueQuery::Labels { page: first_page() });
    assert_eq!(labels.items.len(), 1);
    assert_eq!(labels.items[0].name, "bug");
    let rows: contract::Page<Row> = driver.query(&IssueQuery::List {
        project: None,
        label: Some(label_id.clone()),
        status: None,
        milestone: None,
        mine: None,
        all: false,
        me: None,
        page: first_page(),
    });
    assert_eq!(rows.items.len(), 1);
    assert_eq!(rows.items[0].title, "First issue");

    // Comment lands append-only with author attribution.
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::Comment {
            id: None,
            parent: None,
            doc: doc.clone(),
            body: "a comment".into(),
            actor: my_actor().as_str().to_string(),
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    let view: IssueView = driver.query(&IssueQuery::View {
        doc: doc.clone(),
        me: None,
    });
    assert_eq!(view.comments.len(), 1);
    assert_eq!(view.comments[0].body, "a comment");
    assert_eq!(view.comments[0].author, my_actor());

    // Links + graph: blocks with transitive open blockers.
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::Link {
            doc: doc2.clone(),
            kind: "blocks".into(),
            target: doc.clone(),
            add: true,
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    let graph: contract::Page<IssueRelationDto> = driver.query(&IssueQuery::Relations {
        doc: doc.clone(),
        direction: issues::dto::RelationDirection::In,
        page: first_page(),
    });
    assert_eq!(graph.items.len(), 1);
    assert_eq!(graph.items[0].direction, issues::dto::RelationDirection::In);
    assert_eq!(graph.items[0].kind, "blocks");
    assert_eq!(graph.items[0].row.title, "Second issue");

    // Self-link and unknown-kind links are refused.
    let ts = driver.ts();
    assert_eq!(
        driver.submit(&IssueIntent::Link {
            doc: doc.clone(),
            kind: "blocks".into(),
            target: doc.clone(),
            add: true,
            device: my_device().as_str().to_string(),
            ts,
        }),
        Err(runtime::world::Failure::Rejected(Rejection::InvalidRequest))
    );

    // Parent hierarchy with ancestor-cycle refusal.
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::Parent {
            doc: doc2.clone(),
            parent: Some(doc.clone()),
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    let ts = driver.ts();
    assert_eq!(
        driver.submit(&IssueIntent::Parent {
            doc: doc.clone(),
            parent: Some(doc2.clone()),
            device: my_device().as_str().to_string(),
            ts,
        }),
        Err(runtime::world::Failure::Rejected(Rejection::Conflict))
    );

    // Work state: done moves off the board; an idempotent repeat stages
    // nothing; stop returns to backlog and self-unassigns.
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::WorkState {
            doc: doc.clone(),
            action: WorkAction::Done,
            actor: my_actor().as_str().to_string(),
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    let board: BoardPage = driver.query(&IssueQuery::Board {
        project: project.clone(),
        me: None,
        page: first_page(),
    });
    assert_eq!(
        board
            .rows
            .items
            .iter()
            .filter(|row| row.status == "done")
            .count(),
        1
    );
    let ts = driver.ts();
    let repeat = driver
        .submit(&IssueIntent::WorkState {
            doc: doc.clone(),
            action: WorkAction::Done,
            actor: my_actor().as_str().to_string(),
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    assert!(repeat.unchanged, "an idempotent no-op commits nothing");

    // Delete tombstones and hides from default lists; restore brings it back.
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::SetTombstone {
            doc: doc2.clone(),
            on: true,
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    let rows: contract::Page<Row> = driver.query(&IssueQuery::List {
        project: None,
        label: None,
        status: None,
        milestone: None,
        mine: None,
        all: false,
        me: None,
        page: first_page(),
    });
    assert!(rows.items.iter().all(|r| r.title != "Second issue"));
    let all_rows: contract::Page<Row> = driver.query(&IssueQuery::List {
        project: None,
        label: None,
        status: None,
        milestone: None,
        mine: None,
        all: true,
        me: None,
        page: first_page(),
    });
    assert!(all_rows
        .items
        .iter()
        .any(|r| r.title == "Second issue" && r.tombstone));
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::SetTombstone {
            doc: doc2.clone(),
            on: false,
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();

    // History: the durable per-issue event log, oldest first, attributed.
    let history: contract::Page<issues::dto::ActivityEvent> = driver.query(&IssueQuery::History {
        doc: doc.clone(),
        page: first_page(),
    });
    let events = history.items;
    assert!(events.len() >= 3);
    assert_eq!(events[0].kind, "created");
    assert_eq!(events[0].seq, 1);

    // Projects list.
    let projects: contract::Page<ProjectDto> =
        driver.query(&IssueQuery::Projects { page: first_page() });
    assert_eq!(projects.items.len(), 1);
    assert_eq!(projects.items[0].key, "ENG");

    // Restart durability: everything above survives a cold reactivation.
    let space = station.space_id().clone();
    let orbit = station.vacate().unwrap();
    drop(orbit);
    let rt = product_runtime(&root);
    let station = rt
        .acquire(&space)
        .unwrap()
        .open(Activation::offline())
        .unwrap();
    let driver = Driver::dock(&station);
    let view: IssueView = driver.query(&IssueQuery::View {
        doc: doc.clone(),
        me: None,
    });
    assert_eq!(view.title, "First issue");
    assert_eq!(view.comments.len(), 1);
    assert_eq!(driver.resolve("ENG-2").as_deref(), Some(doc2.as_str()));
    let _ = station.vacate();
    let _ = std::fs::remove_dir_all(&root);
}

/// The World resolves caller-proposed labels against the catalog the write
/// lands on, not against the caller's snapshot.
///
/// A caller resolves label names against its own view and proposes a `NewLabel`
/// for every name that misses. On a lagging Station that view is older than the
/// Replica, so the staler it gets the more rival ids it mints for labels the
/// Space already has — and every mint is another write to the Catalog, the one
/// Space-wide Body every concurrent writer contends on. The desync widened
/// itself. Worse, no caller loop can see its own mints, so the same name twice
/// in one request minted two ids deterministically, with no concurrency at all.
#[test]
fn proposed_labels_resolve_against_the_catalog_the_write_lands_on() {
    let root = temp_root("label-reconcile");
    let (_rt, station) = setup(&root);
    let mut driver = Driver::dock(&station);
    let (_project, doc, _alias) = seed_space(&mut driver);

    // The same name twice in ONE request. No concurrency, no staleness — the
    // caller simply cannot see the id it is about to mint.
    let first = LabelId::mint(&SystemUlidSource).as_str().to_string();
    let second = LabelId::mint(&SystemUlidSource).as_str().to_string();
    assert_ne!(first, second);
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::Label {
            doc: doc.clone(),
            add: vec![],
            new_labels: vec![
                contract::NewLabel {
                    id: first.clone(),
                    name: "bug".into(),
                    color: "red".into(),
                },
                contract::NewLabel {
                    id: second.clone(),
                    name: "bug".into(),
                    color: "blue".into(),
                },
            ],
            remove: vec![],
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    let labels: contract::Page<LabelDto> = driver.query(&IssueQuery::Labels { page: first_page() });
    assert_eq!(labels.items.len(), 1, "one name is one label: {labels:?}");
    assert_eq!(labels.items[0].name, "bug");
    assert_eq!(
        labels.items[0].id.as_str(),
        first,
        "the first proposal wins deterministically"
    );

    // Now a caller whose snapshot never saw "bug" proposes a rival id for it.
    // The catalog this write lands on has it, so the rival is dropped and the
    // existing label is adopted instead.
    let rival = LabelId::mint(&SystemUlidSource).as_str().to_string();
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::Label {
            doc: doc.clone(),
            add: vec![],
            new_labels: vec![contract::NewLabel {
                id: rival.clone(),
                name: "BUG".into(),
                color: "green".into(),
            }],
            remove: vec![],
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    let labels: contract::Page<LabelDto> = driver.query(&IssueQuery::Labels { page: first_page() });
    assert_eq!(
        labels.items.len(),
        1,
        "a stale proposal must not mint a rival for a label the Space has: {labels:?}"
    );
    assert_eq!(labels.items[0].id.as_str(), first);

    // And the issue carries the adopted id, not the rival that was proposed.
    let rows: contract::Page<Row> = driver.query(&IssueQuery::List {
        project: None,
        label: Some(first.clone()),
        status: None,
        milestone: None,
        mine: None,
        all: false,
        me: None,
        page: first_page(),
    });
    assert_eq!(
        rows.items.len(),
        1,
        "the issue is labelled with the adopted id"
    );
    let rows: contract::Page<Row> = driver.query(&IssueQuery::List {
        project: None,
        label: Some(rival),
        status: None,
        milestone: None,
        mine: None,
        all: false,
        me: None,
        page: first_page(),
    });
    assert!(rows.items.is_empty(), "the rival id was never applied");

    drop(driver);
    let _ = station.vacate();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_denied_or_invalid_request_commits_and_publishes_nothing() {
    let root = temp_root("refusals");
    let (_rt, station) = setup(&root);
    let mut driver = Driver::dock(&station);
    let (_project, doc, _alias) = seed_space(&mut driver);
    let frontier = station.frontier();

    // Unknown project refuses; empty title refuses; unknown status refuses.
    let ts = driver.ts();
    assert_eq!(
        driver.submit(&IssueIntent::IssueNew {
            duedate: None,
            estimate: None,
            doc: DocId::mint(&SystemUlidSource).as_str().to_string(),
            project: "prj_00000000000000000000000000".into(),
            title: "x".into(),
            priority: "high".into(),
            assignees: vec![],
            labels: vec![],
            new_labels: vec![],
            body: None,
            actor: my_actor().as_str().to_string(),
            device: my_device().as_str().to_string(),
            ts,
        }),
        Err(runtime::world::Failure::Rejected(Rejection::InvalidRequest))
    );
    let ts = driver.ts();
    assert_eq!(
        driver.submit(&IssueIntent::IssueEdit {
            duedate: None,
            estimate: None,
            doc: doc.clone(),
            title: None,
            status: Some("nonexistent".into()),
            priority: None,
            description: None,
            device: my_device().as_str().to_string(),
            ts,
        }),
        Err(runtime::world::Failure::Rejected(Rejection::InvalidRequest))
    );
    assert_eq!(station.frontier(), frontier, "nothing committed");
    let _ = station.vacate();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn two_stations_converge_product_issues_over_the_contact_plane() {
    let coords = coordinates();
    let net = comms::mem::MemNet::new();
    let ta: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&STATION_A_SEED)));
    let tb: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&STATION_B_SEED)));
    let comms_options = |transport: Arc<dyn comms::Transport>, seed: [u8; 32]| CommsOptions {
        transport,
        station_seed: seed,
        authority: Authority {
            source: Arc::new(AnyKnownSigner),
            incorporator: Arc::new(Mutex::new(AcceptingIncorporator)),
            export: Arc::new(Vec::new),
            frontier: Arc::new(|| AuthorityFrontier::from_canonical_bytes(vec![8])),
        },
        gossip: None,
        whole_deadline: Duration::from_secs(20),
        progress_deadline: Duration::from_secs(5),
        route_lease: Duration::from_secs(60),
    };

    let root_a = temp_root("conv-a");
    let root_b = temp_root("conv-b");
    let station_a = product_runtime(&root_a)
        .materialize(&coords)
        .unwrap()
        .open(Activation {
            planes: Default::default(),
            content: Default::default(),
            find: Default::default(),
            drain_deadline: Duration::from_secs(5),
            comms: Some(comms_options(ta, STATION_A_SEED)),
            observation_capacity: 0,
        })
        .unwrap();
    let mut driver_a = Driver::dock(&station_a);
    let (project, doc, _alias) = seed_space(&mut driver_a);

    let station_b = product_runtime(&root_b)
        .materialize(&coords)
        .unwrap()
        .open(Activation {
            planes: Default::default(),
            content: Default::default(),
            find: Default::default(),
            drain_deadline: Duration::from_secs(5),
            comms: Some(comms_options(tb, STATION_B_SEED)),
            observation_capacity: 0,
        })
        .unwrap();
    let a_station_id =
        mechanics::station::Key::from_device(&mechanics::actor::device_from_seed(&STATION_A_SEED))
            .unwrap();
    let outcome = station_b.contact(&a_station_id).unwrap();
    assert!(outcome.convergence.accepted >= 1);

    // B sees A's product state through the SAME World adapter.
    let driver_b = Driver::dock(&station_b);
    let view: IssueView = driver_b.query(&IssueQuery::View {
        doc: doc.clone(),
        me: None,
    });
    assert_eq!(view.title, "First issue");
    assert_eq!(driver_b.resolve("ENG-1").as_deref(), Some(doc.as_str()));

    // B comments; A contacts back; the comment converges with stable
    // identity (no duplication on re-contact).
    let mut driver_b = driver_b;
    driver_b.now = 1_700_100_000;
    let ts = driver_b.ts();
    driver_b
        .submit(&IssueIntent::Comment {
            id: None,
            parent: None,
            doc: doc.clone(),
            body: "from b".into(),
            actor: my_actor().as_str().to_string(),
            device: mechanics::actor::device_from_seed(&STATION_B_SEED)
                .as_str()
                .to_string(),
            ts,
        })
        .unwrap();
    let b_station_id =
        mechanics::station::Key::from_device(&mechanics::actor::device_from_seed(&STATION_B_SEED))
            .unwrap();
    let outcome = station_a.contact(&b_station_id).unwrap();
    assert!(outcome.convergence.accepted >= 1);
    let view: IssueView = driver_a.query(&IssueQuery::View {
        doc: doc.clone(),
        me: None,
    });
    assert_eq!(view.comments.len(), 1);
    assert_eq!(view.comments[0].body, "from b");

    // A long thread, written from both sides while B is behind — the case the
    // hierarchy exists for. A carries the conversation on; B, which has synced
    // none of it, adds a comment of its own. Under the flat list this was an
    // insert at "the end" as B could see it, which was the middle of A's
    // thread; and the further ahead A got, the further back B's comment
    // landed. Neither station may lose or duplicate a comment, and both must
    // agree on the order, which is the order they were written in.
    let mut a_bodies = Vec::new();
    for i in 0..12 {
        driver_a.now = 1_700_200_000 + i;
        let ts = driver_a.ts();
        let body = format!("from a #{i}");
        driver_a
            .submit(&IssueIntent::Comment {
                id: Some(issues::ids::mint_comment_id(&SystemUlidSource)),
                parent: None,
                doc: doc.clone(),
                body: body.clone(),
                actor: my_actor().as_str().to_string(),
                device: mechanics::actor::device_from_seed(&STATION_A_SEED)
                    .as_str()
                    .to_string(),
                ts,
            })
            .unwrap();
        a_bodies.push(body);
    }
    // B is still at its own clock and has seen none of those twelve.
    driver_b.now = 1_700_200_006;
    let ts = driver_b.ts();
    driver_b
        .submit(&IssueIntent::Comment {
            id: Some(issues::ids::mint_comment_id(&SystemUlidSource)),
            parent: None,
            doc: doc.clone(),
            body: "from b, while behind".into(),
            actor: my_actor().as_str().to_string(),
            device: mechanics::actor::device_from_seed(&STATION_B_SEED)
                .as_str()
                .to_string(),
            ts,
        })
        .unwrap();

    station_a.contact(&b_station_id).unwrap();
    station_b.contact(&a_station_id).unwrap();

    let mut expected: Vec<String> = vec!["from b".to_string()];
    expected.extend(a_bodies.iter().take(7).cloned());
    expected.push("from b, while behind".into());
    expected.extend(a_bodies.iter().skip(7).cloned());
    for (station, driver) in [("a", &driver_a), ("b", &driver_b)] {
        let view: IssueView = driver.query(&IssueQuery::View {
            doc: doc.clone(),
            me: None,
        });
        let bodies: Vec<String> = view.comments.iter().map(|c| c.body.clone()).collect();
        assert_eq!(
            bodies, expected,
            "station {station} read the thread in a different order than it was written"
        );
    }

    // The board converged too.
    let board: BoardPage = driver_a.query(&IssueQuery::Board {
        project: project.clone(),
        me: None,
        page: first_page(),
    });
    assert_eq!(board.rows.items.len(), 1);

    let _ = station_a.vacate();
    let _ = station_b.vacate();
    let _ = std::fs::remove_dir_all(&root_a);
    let _ = std::fs::remove_dir_all(&root_b);
    let _ = BTreeMap::<String, String>::new();
    let _ = StatusCategory::Done;
}

#[test]
fn due_dates_estimates_and_comment_reactions_round_trip() {
    let root = temp_root("enriched");
    let (_rt, station) = setup(&root);
    let mut driver = Driver::dock(&station);
    let (_project, doc, _alias) = seed_space(&mut driver);

    // ---- due date + estimate: set, project, change, clear ----
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::IssueEdit {
            doc: doc.clone(),
            title: None,
            status: None,
            priority: None,
            description: None,
            duedate: Some(Some(1_800_000_000)),
            estimate: Some(Some(5)),
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    let view: IssueView = driver.query(&IssueQuery::View {
        doc: doc.clone(),
        me: None,
    });
    assert_eq!(view.due_date, Some(1_800_000_000));
    assert_eq!(view.estimate, Some(5));
    let rows: contract::Page<Row> = driver.query(&IssueQuery::List {
        project: None,
        label: None,
        status: None,
        milestone: None,
        mine: None,
        all: true,
        me: None,
        page: first_page(),
    });
    let row = rows
        .items
        .iter()
        .find(|r| r.doc_id.as_str() == doc)
        .unwrap();
    assert_eq!(row.due_date, Some(1_800_000_000));
    assert_eq!(row.estimate, Some(5));

    // Clearing goes back to absent — the register is removed, not zeroed.
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::IssueEdit {
            doc: doc.clone(),
            title: None,
            status: None,
            priority: None,
            description: None,
            duedate: Some(None),
            estimate: Some(None),
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    let view: IssueView = driver.query(&IssueQuery::View {
        doc: doc.clone(),
        me: None,
    });
    assert_eq!(view.due_date, None);
    assert_eq!(view.estimate, None);
    // A due date of 0 is a typo, not an epoch-midnight deadline.
    let ts = driver.ts();
    let refused = driver.submit(&IssueIntent::IssueEdit {
        doc: doc.clone(),
        title: None,
        status: None,
        priority: None,
        description: None,
        duedate: Some(Some(0)),
        estimate: None,
        device: my_device().as_str().to_string(),
        ts,
    });
    assert!(matches!(
        refused,
        Err(runtime::world::Failure::Rejected(Rejection::InvalidRequest))
    ));

    // ---- comment identity, replies, reactions ----
    let cid = issues::ids::mint_comment_id(&SystemUlidSource);
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::Comment {
            doc: doc.clone(),
            body: "root comment".into(),
            id: Some(cid.clone()),
            parent: None,
            actor: my_actor().as_str().to_string(),
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    // A duplicate id would fuse two comments' reactions — refused.
    let ts = driver.ts();
    let refused = driver.submit(&IssueIntent::Comment {
        doc: doc.clone(),
        body: "same id".into(),
        id: Some(cid.clone()),
        parent: None,
        actor: my_actor().as_str().to_string(),
        device: my_device().as_str().to_string(),
        ts,
    });
    assert!(matches!(
        refused,
        Err(runtime::world::Failure::Rejected(Rejection::InvalidRequest))
    ));

    let reply = issues::ids::mint_comment_id(&SystemUlidSource);
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::Comment {
            doc: doc.clone(),
            body: "a reply".into(),
            id: Some(reply.clone()),
            parent: Some(cid.clone()),
            actor: my_actor().as_str().to_string(),
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    // One level only: replying to the reply is refused, not laddered.
    let ts = driver.ts();
    let refused = driver.submit(&IssueIntent::Comment {
        doc: doc.clone(),
        body: "reply to reply".into(),
        id: Some(issues::ids::mint_comment_id(&SystemUlidSource)),
        parent: Some(reply.clone()),
        actor: my_actor().as_str().to_string(),
        device: my_device().as_str().to_string(),
        ts,
    });
    assert!(matches!(
        refused,
        Err(runtime::world::Failure::Rejected(Rejection::InvalidRequest))
    ));

    let ts = driver.ts();
    driver
        .submit(&IssueIntent::React {
            doc: doc.clone(),
            comment: cid.clone(),
            emoji: "👍".into(),
            actor: my_actor().as_str().to_string(),
            on: true,
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    let view: IssueView = driver.query(&IssueQuery::View {
        doc: doc.clone(),
        me: None,
    });
    let root_comment = view
        .comments
        .iter()
        .find(|c| c.id.as_deref() == Some(cid.as_str()))
        .unwrap();
    assert_eq!(root_comment.reactions.len(), 1);
    assert_eq!(root_comment.reactions[0].emoji, "👍");
    assert_eq!(root_comment.reactions[0].actors, vec![my_actor()]);
    let reply_comment = view
        .comments
        .iter()
        .find(|c| c.id.as_deref() == Some(reply.as_str()))
        .unwrap();
    assert_eq!(reply_comment.parent.as_deref(), Some(cid.as_str()));

    // Un-react removes the pair; the set converges to empty.
    let ts = driver.ts();
    driver
        .submit(&IssueIntent::React {
            doc: doc.clone(),
            comment: cid.clone(),
            emoji: "👍".into(),
            actor: my_actor().as_str().to_string(),
            on: false,
            device: my_device().as_str().to_string(),
            ts,
        })
        .unwrap();
    let view: IssueView = driver.query(&IssueQuery::View {
        doc: doc.clone(),
        me: None,
    });
    let root_comment = view
        .comments
        .iter()
        .find(|c| c.id.as_deref() == Some(cid.as_str()))
        .unwrap();
    assert!(root_comment.reactions.is_empty());

    // Reacting to a comment that does not exist is refused.
    let ts = driver.ts();
    let refused = driver.submit(&IssueIntent::React {
        doc: doc.clone(),
        comment: issues::ids::mint_comment_id(&SystemUlidSource),
        emoji: "🎉".into(),
        actor: my_actor().as_str().to_string(),
        on: true,
        device: my_device().as_str().to_string(),
        ts,
    });
    assert!(matches!(
        refused,
        Err(runtime::world::Failure::Rejected(Rejection::InvalidRequest))
    ));
}
