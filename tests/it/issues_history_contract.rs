//! The behavioural pin `viewer_parity.rs` names but never had.
//!
//! `viewer_parity` guards the *write* surface — that a TypeScript request names
//! no field the Rust one lacks. It says so itself, and it says what that leaves
//! open: "it does not check the *semantics* of a field (what value the daemon
//! puts there). That gap is not hypothetical: durable history changed
//! `ActivityEvent.actor`/`actor_nick` semantics under the viewer and this test
//! was blind to it."
//!
//! It then pointed at a test that did not exist, and the gap it described was
//! live the whole time. `ActivityEvent.actor` was populated from the committing
//! **device** while the viewer resolves display names by **actor**, so the
//! lookup missed on every row and every author rendered as a hex prefix in a
//! colour derived from hashing that hex — a different colour from the same
//! person's roster chip.
//!
//! A name check cannot catch that: the field is called `actor` either way. Only
//! driving a real Station and reading the value can.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use issues::ids::{ActorId, DeviceId, SystemUlidSource};
use issues::IssuesWorld;
use issues_app::{IssueRouter, IssuesRequest as Request, IssuesResponse as Response, RouterFacts};
use mechanics::authorization::AuthorizedBodyKey;
use replica::frontier::AuthorityFrontier;
use runtime::{plane::Activation, world::Builder, world::LocalIdentity, Runtime, Session, Station};

const WRITER_SEED: [u8; 32] = [73u8; 32];
static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

fn temp_root() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lait-history-{}-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct WriterAuthority;
impl runtime::world::AuthorityView for WriterAuthority {
    fn resolve(&self, _device: &DeviceId) -> Option<runtime::world::PrincipalResolution> {
        Some(runtime::world::PrincipalResolution {
            actor: actor(),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![1]),
        })
    }

    /// The reviewed identity rather than the trait's all-zero fixture default.
    /// Anything that records the publication it was authored at — a Spec
    /// revision, a geometry artifact — refuses one whose implementation digest
    /// is zero, so the authority and the registration must name the same
    /// implementation.
    fn active_implementation(
        &self,
        _world: &replica::body::WorldId,
        _authority_frontier: &AuthorityFrontier,
    ) -> Result<Option<[u8; 32]>, String> {
        Ok(Some(reviewed_implementation()))
    }
}

/// The canonical Issues implementation id, as registered and as the authority
/// reports it active.
fn reviewed_implementation() -> [u8; 32] {
    issues::IssuesWorld::implementation_descriptor()
        .id()
        .expect("the Issues descriptor is canonical")
}

fn actor() -> ActorId {
    ActorId::from_incept_hash(&"b".repeat(64))
}

fn device() -> String {
    mechanics::actor::device_from_seed(&WRITER_SEED)
        .as_str()
        .to_string()
}

fn station() -> (Runtime, Station) {
    let registry = Builder::new()
        .register_reviewed(Arc::new(IssuesWorld::new()), reviewed_implementation())
        .build()
        .unwrap();
    let rt = Runtime::open(
        temp_root(),
        registry,
        Arc::new(WriterAuthority),
        Arc::new(replica::body::StaticBodyKeys::new(
            AuthorizedBodyKey::for_authorized_epoch([5u8; 16], [6u8; 32]),
        )),
    );
    let station = rt.create().unwrap().open(Activation::offline()).unwrap();
    (rt, station)
}

/// The product result inside the durable acknowledgement.
///
/// Writes answer with the operation envelope now: the receipt that makes the
/// operation durable, paired with the result it produced. These tests are
/// about what the write produced — a handle, a link — rather than about the
/// receipt, so they unwrap it here and keep asserting on the result.
fn effect(response: Response) -> Response {
    match response {
        Response::Operation { response, .. } => *response,
        other => other,
    }
}

fn facts() -> RouterFacts {
    RouterFacts {
        device: device(),
        actor: actor().as_str().to_string(),
        project_hint: None,
        default_project: None,
        now: 1_700_000_000,
    }
}

fn dock(station: &Station) -> (Session, LocalIdentity) {
    let identity = Runtime::identity_from_seed(&WRITER_SEED);
    let session = station
        .dock(&issues::contract::world_id(), &identity)
        .unwrap();
    (session, identity)
}

#[test]
fn history_is_attributed_to_an_actor_and_not_to_the_device_it_was_committed_on() {
    let (_rt, station) = station();
    let (session, identity) = dock(&station);
    let clock = SystemUlidSource;
    let router = IssueRouter::new(&session, &identity, &clock);

    router.route(
        Request::ProjectNew {
            name: "Engineering".into(),
            key: "eng".into(),
            color: None,
        },
        &facts(),
    );
    let (created, _) = router.route(
        Request::IssueNew {
            title: "a thing to do".into(),
            project: Some("ENG".into()),
            project_hint: None,
            priority: None,
            assignees: vec![],
            labels: vec![],
            body: None,
            due: None,
            estimate: None,
        },
        &facts(),
    );
    let Response::Ref { reff } = effect(created) else {
        panic!("expected the new issue's handle");
    };
    router.route(
        Request::Comment {
            reff: reff.clone(),
            body: "and a word about it".into(),
            reply_to: None,
        },
        &facts(),
    );

    let (activity, _) = router.route(
        Request::Activity {
            page: issues::contract::PageRequest::default(),
        },
        &facts(),
    );
    let Response::Activity { page } = activity else {
        panic!("expected activity, got {activity:?}");
    };
    let rows = page.items;
    assert!(!rows.is_empty(), "creating and commenting is history");

    for row in &rows {
        // The assertion the missing pin owed. Before this landed, `actor`
        // carried the committing device — a well-formed id of the wrong kind,
        // which no field-name check could catch.
        assert_eq!(
            row.actor.as_ref(),
            Some(&actor()),
            "a history row names the actor that committed it: {row:?}"
        );
        // Stated separately so a change that reverts one half without the other
        // fails loudly rather than subtly.
        assert_ne!(
            row.actor.as_ref().map(|a| a.as_str().to_string()),
            Some(device()),
            "a device id is not an actor id, and the viewer resolves by actor"
        );
    }
}

#[test]
fn an_event_written_before_history_carried_an_actor_reads_back_as_no_name() {
    // The migration half. `IssueEvent.a` is absent-means-absent, so a row
    // written by an earlier build decodes with an empty actor — and the honest
    // rendering of that is *no name*, which is what the viewer already does.
    // Inventing one from the device is precisely the defect this replaced.
    let older = serde_json::json!({
        "k": "created",
        "d": device(),
        "t": 1_700_000_000u64,
    });
    let decoded: issues::contract::IssueEvent =
        serde_json::from_value(older).expect("an event from before actors decodes");
    assert!(decoded.a.is_empty(), "no actor, rather than a guessed one");
    assert_eq!(
        decoded.d,
        device(),
        "and the device it was committed on is kept"
    );

    // And it re-encodes without inventing the field, so a Body written by an
    // older build is byte-neutral through this one.
    let re = serde_json::to_value(&decoded).expect("re-encode");
    assert!(re.get("a").is_none(), "an absent actor stays absent: {re}");
}

#[test]
fn project_topology_changes_geometry_without_mutating_its_prior_generation() {
    let (_rt, station) = station();
    let (session, identity) = dock(&station);
    let clock = SystemUlidSource;
    let router = IssueRouter::new(&session, &identity, &clock);

    router.route(
        Request::ProjectNew {
            name: "Client".into(),
            key: "client".into(),
            color: None,
        },
        &facts(),
    );
    let project_id = match router
        .route(
            Request::ProjectList {
                page: issues::contract::PageRequest::default(),
            },
            &facts(),
        )
        .0
    {
        Response::Projects { page } => page
            .items
            .into_iter()
            .find(|project| project.key == "CLIENT")
            .expect("client project")
            .id
            .as_str()
            .to_owned(),
        response => panic!("expected project list, got {response:?}"),
    };
    let create = |title: &str| {
        let (reply, _) = router.route(
            Request::IssueNew {
                title: title.into(),
                project: Some("CLIENT".into()),
                project_hint: None,
                priority: None,
                assignees: vec![],
                labels: vec![],
                body: None,
                due: None,
                estimate: None,
            },
            &facts(),
        );
        let Response::Ref { reff } = effect(reply) else {
            panic!("expected issue reference");
        };
        reff
    };
    let foundation = create("Connect to a served World");
    let workspace = create("Operate the local workspace");
    let before_link: issues::contract::Page<issues::dto::ProjectDto> = serde_json::from_slice(
        &session
            .query(runtime::world::Query {
                schema: issues::contract::issue_schema(),
                schema_version: issues::contract::ISSUE_SCHEMA_VERSION,
                payload: issues::contract::IssueQuery::Projects {
                    page: issues::contract::PageRequest {
                        limit: 1,
                        cursor: None,
                    },
                }
                .to_json(),
                publication: None,
            })
            .expect("publication before topology edit")
            .bytes,
    )
    .expect("project page before topology edit");
    let before_link = before_link.publication;

    let (linked, _) = router.route(
        Request::IssueLink {
            reff: foundation,
            kind: "blocks".into(),
            target: workspace,
        },
        &facts(),
    );
    assert!(
        matches!(effect(linked), Response::Ref { .. }),
        "link reply did not answer with the issue handle"
    );

    let geometry_query = |publication| runtime::world::Query {
        schema: issues::contract::issue_schema(),
        schema_version: issues::contract::ISSUE_SCHEMA_VERSION,
        payload: issues::contract::IssueQuery::Geometry {
            project: project_id.clone(),
            roots: vec![],
            page: Some(issues::geometry::GeometryPageRequest::first(
                issues::geometry::GeometrySection::Edges,
                16,
            )),
        }
        .to_json(),
        publication,
    };
    // Resolve the human project key once through the router's product
    // snapshot, then use the canonical id selected by deterministic fixtures.
    // Geometry itself is now a bounded artifact response: summary and one
    // explicit page, never a whole graph serialized by accident.
    // Geometry is built off the request now, so the first ask can legitimately
    // answer `Pending` with neither summary nor page. Waiting for the artifact
    // is the caller's job — the projection says which state it is in.
    let ready_geometry = |query: runtime::world::Query| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let projection: issues::contract::GeometryProjection =
                serde_json::from_slice(&session.query(query.clone()).expect("geometry").bytes)
                    .expect("geometry response");
            match projection.readiness {
                issues::geometry::GeometryReadiness::Ready => break projection,
                issues::geometry::GeometryReadiness::Pending
                    if std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                other => panic!("geometry never became ready: {other:?}"),
            }
        }
    };
    let current = ready_geometry(geometry_query(None));
    let current_summary = current.summary.as_ref().expect("ready summary");
    assert_eq!(current_summary.nodes, 2);
    assert_eq!(current_summary.components, 1);
    assert_eq!(current_summary.edges, 1);
    let Some(issues::geometry::GeometryPage {
        rows: issues::geometry::GeometryRows::Edges(edges),
        ..
    }) = current.page.as_ref()
    else {
        panic!("expected current edge page, got {:?}", current.page);
    };
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].relation, issues::geometry::RelationKind::Blocks);

    let historical = ready_geometry(geometry_query(Some(before_link.publication.clone())));
    let historical_summary = historical.summary.as_ref().expect("ready summary");
    assert_eq!(historical_summary.nodes, 2);
    assert_eq!(historical_summary.components, 2);
    assert_eq!(historical_summary.edges, 0);
    let Some(issues::geometry::GeometryPage {
        rows: issues::geometry::GeometryRows::Edges(edges),
        ..
    }) = historical.page.as_ref()
    else {
        panic!("expected historical edge page, got {:?}", historical.page);
    };
    assert!(edges.is_empty());
    assert_eq!(
        data_encoding::HEXLOWER.encode(&historical.source.publication.manifest_root),
        data_encoding::HEXLOWER.encode(&before_link.publication.manifest_root)
    );
}
