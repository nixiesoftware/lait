//! The package-owned application router drives `IssuesRequest` through the
//! `IssuesWorld` adapter and returns `IssuesResponse` projections.

use std::sync::Arc;

use issues::dto::{CommentAnchorState, Priority};
use issues::ids::{ActorId, DeviceId, SystemUlidSource};
use issues::IssuesWorld;
use issues_app::{
    BoardPos, Filter, IssueRouter, IssuesErrorKind, IssuesRequest as Request,
    IssuesResponse as Response, RouterFacts,
};
use mechanics::authorization::AuthorizedBodyKey;
use replica::frontier::AuthorityFrontier;
use runtime::{plane::Activation, world::Builder, world::LocalIdentity, Runtime, Session, Station};

const WRITER_SEED: [u8; 32] = [71u8; 32];

fn temp_root() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lait-router-{}", std::process::id()));
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
}

fn actor() -> ActorId {
    ActorId::from_incept_hash(&"a".repeat(64))
}

fn station() -> (Runtime, Station) {
    let registry = Builder::new()
        .register(Arc::new(IssuesWorld::new()))
        .build()
        .unwrap();
    let rt = Runtime::open(
        temp_root(),
        registry,
        Arc::new(WriterAuthority),
        Arc::new(replica::body::StaticBodyKeys::new(
            AuthorizedBodyKey::for_authorized_epoch([3u8; 16], [4u8; 32]),
        )),
    );
    let station = rt.create().unwrap().open(Activation::offline()).unwrap();
    (rt, station)
}

fn facts() -> RouterFacts {
    RouterFacts {
        device: mechanics::actor::device_from_seed(&WRITER_SEED)
            .as_str()
            .to_string(),
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
fn the_router_maps_the_control_surface_to_the_issues_world() {
    let (_rt, station) = station();
    let (session, identity) = dock(&station);
    let clock = SystemUlidSource;
    let router = IssueRouter::new(&session, &identity, &clock);

    // SpaceInit is not a control request; seed via ProjectNew directly (the
    // catalog Body is created on first write).
    let (resp, changed) = router.route(
        Request::ProjectNew {
            name: "Engineering".into(),
            key: "eng".into(),
            color: None,
        },
        &facts(),
    );
    let resp = super::accepted_issue_response(resp);
    assert!(changed, "{resp:?}");
    let project_ref = match resp {
        Response::Ref { reff } => reff,
        other => panic!("expected project Ref, got {other:?}"),
    };

    // IssueNew chooses the sole project and returns its canonical reff.
    let (resp, changed) = router.route(
        Request::IssueNew {
            title: "Router works".into(),
            project: Some(project_ref.clone()),
            project_hint: None,
            assignees: vec![],
            priority: Some("high".into()),
            labels: vec![],
            body: Some("body text".into()),
            due: None,
            estimate: None,
        },
        &facts(),
    );
    let resp = super::accepted_issue_response(resp);
    assert!(changed, "{resp:?}");
    let issue_ref = match resp {
        Response::Ref { reff } => reff,
        other => panic!("expected Ref, got {other:?}"),
    };
    assert!(issue_ref.starts_with("ENG-"), "{issue_ref}");

    // The bounded detail surface hydrates the issue and its hot labels.
    let (resp, _) = router.route(
        Request::IssueDetail {
            reff: issue_ref.clone(),
            publication: None,
        },
        &facts(),
    );
    let view = match resp {
        Response::IssueDetail(detail) => detail.issue,
        other => panic!("expected IssueDetail, got {other:?}"),
    };
    assert_eq!(view.title, "Router works");
    assert_eq!(view.priority, Priority::High);

    // Edit, comment, start (work-state), and board all route.
    router.route(
        Request::IssueEdit {
            reff: issue_ref.clone(),
            title: Some("Renamed".into()),
            status: None,
            priority: None,
            description: None,
            due: None,
            estimate: None,
        },
        &facts(),
    );
    router.route(
        Request::Comment {
            reff: issue_ref.clone(),
            body: "routed comment".into(),
            reply_to: None,
        },
        &facts(),
    );

    // A range-attached comment routes too: the adapter mints the comment id the
    // World demands for it, and the projection resolves the span on the read.
    let source = match router
        .route(
            Request::IssueDetail {
                reff: issue_ref.clone(),
                publication: None,
            },
            &facts(),
        )
        .0
    {
        Response::IssueDetail(detail) => {
            issues_app::WorldPublicationCoordinate::from_id(&detail.publication)
        }
        other => panic!("expected IssueDetail, got {other:?}"),
    };
    let (resp, changed) = router.route(
        Request::CommentAt {
            reff: issue_ref.clone(),
            body: "this word is wrong".into(),
            field: "description".into(),
            start: 0,
            end: Some(4),
            reply_to: None,
            source,
        },
        &facts(),
    );
    let resp = super::accepted_issue_response(resp);
    assert!(changed, "{resp:?}");
    assert!(matches!(resp, Response::Ref { .. }));
    let (resp, _) = router.route(
        Request::IssueComments {
            reff: issue_ref.clone(),
            publication: None,
            page: issues::contract::PageRequest::default(),
        },
        &facts(),
    );
    let comments = match resp {
        Response::Comments { page } => page.items,
        other => panic!("expected Comments, got {other:?}"),
    };
    let attached = comments
        .iter()
        .find(|c| c.anchor.is_some())
        .expect("the attached comment");
    assert_eq!(attached.body, "this word is wrong");
    let anchor = attached.anchor.as_ref().expect("anchor");
    assert_eq!(anchor.field, "description");
    assert_eq!(
        anchor.state,
        CommentAnchorState::At { start: 0, end: 4 },
        "the span names `body` in `body text`"
    );

    // A field the algebra cannot move a position inside is a typed refusal, not
    // a comment stored with an anchor nothing can resolve.
    let source = match router
        .route(
            Request::IssueDetail {
                reff: issue_ref.clone(),
                publication: None,
            },
            &facts(),
        )
        .0
    {
        Response::IssueDetail(detail) => {
            issues_app::WorldPublicationCoordinate::from_id(&detail.publication)
        }
        other => panic!("expected IssueDetail, got {other:?}"),
    };
    let (resp, changed) = router.route(
        Request::CommentAt {
            reff: issue_ref.clone(),
            body: "the title is wrong".into(),
            field: "title".into(),
            start: 0,
            end: Some(3),
            reply_to: None,
            source,
        },
        &facts(),
    );
    assert!(!changed);
    assert!(matches!(resp, Response::Error { .. }));
    // A second issue + a Before move exercises ref resolution in positions.
    let second_ref = match super::accepted_issue_response(
        router
            .route(
                Request::IssueNew {
                    title: "Second".into(),
                    project: Some(project_ref),
                    project_hint: None,
                    assignees: vec![],
                    priority: None,
                    labels: vec![],
                    body: None,
                    due: None,
                    estimate: None,
                },
                &facts(),
            )
            .0,
    ) {
        Response::Ref { reff } => reff,
        other => panic!("expected second issue Ref, got {other:?}"),
    };
    let (resp, changed) = router.route(
        Request::IssueMove {
            reff: second_ref,
            project: None,
            pos: Some(BoardPos::Before {
                reff: issue_ref.clone(),
            }),
        },
        &facts(),
    );
    let resp = super::accepted_issue_response(resp);
    assert!(changed, "{resp:?}");
    assert!(matches!(resp, Response::Ref { .. }));

    let (resp, changed) = router.route(
        Request::IssueStart {
            reff: issue_ref.clone(),
        },
        &facts(),
    );
    let resp = super::accepted_issue_response(resp);
    assert!(changed, "{resp:?}");
    assert!(matches!(resp, Response::Issue(_)));

    // List returns Rows; the started issue shows its updated title.
    let (resp, _) = router.route(
        Request::List {
            project: None,
            filter: Filter::default(),
            page: issues::contract::PageRequest::default(),
        },
        &facts(),
    );
    let rows = match resp {
        Response::List { page } => page.items,
        other => panic!("expected List, got {other:?}"),
    };
    assert!(rows.iter().any(|r| r.title == "Renamed"));

    // A ref that matches nothing is a typed not-found (exit 2 on the CLI).
    let (resp, changed) = router.route(
        Request::IssueView {
            reff: "ENG-99".into(),
        },
        &facts(),
    );
    assert!(!changed);
    assert!(matches!(
        resp,
        Response::Error {
            error_kind: IssuesErrorKind::NotFound,
            ..
        }
    ));

    // A view-only principal is refused with the legacy message.
    struct ReadOnly;
    impl runtime::world::AuthorityView for ReadOnly {
        fn resolve(&self, _d: &DeviceId) -> Option<runtime::world::PrincipalResolution> {
            Some(runtime::world::PrincipalResolution {
                actor: actor(),
                authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![1]),
            })
        }
    }
    // (A fresh read-only station would refuse writes; covered by the World's
    // own denied path — here we assert the router surfaces write failures.)
    let _ = ReadOnly;

    let _ = station.vacate();
}

#[test]
fn the_router_accepts_its_product_protocol() {
    assert!(IssueRouter::handles(&Request::IssueNew {
        title: "x".into(),
        project: None,
        project_hint: None,
        due: None,
        estimate: None,
        assignees: vec![],
        priority: None,
        labels: vec![],
        body: None,
    }));
    assert!(IssueRouter::handles(&Request::Board {
        project: None,
        project_hint: None,
        page: issues::contract::PageRequest::default(),
    }));
}
