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

use std::sync::Arc;

use issues::ids::{ActorId, DeviceId, SystemUlidSource};
use issues::IssuesWorld;
use issues_app::{IssueRouter, IssuesRequest as Request, IssuesResponse as Response, RouterFacts};
use mechanics::crypto::AuthorizedBodyKey;
use replica::frontier::AuthorityFrontier;
use runtime::{ActivationOptions, LocalIdentity, Runtime, RuntimeBuilder, Session, Station};

const WRITER_SEED: [u8; 32] = [73u8; 32];

fn temp_root() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lait-history-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct WriterAuthority;
impl runtime::AuthorityView for WriterAuthority {
    fn resolve(&self, _device: &DeviceId) -> Option<runtime::PrincipalResolution> {
        Some(runtime::PrincipalResolution {
            actor: actor(),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![1]),
        })
    }
}

fn actor() -> ActorId {
    ActorId::from_incept_hash(&"b".repeat(64))
}

fn device() -> String {
    mechanics::crypto::device_from_seed(&WRITER_SEED)
        .as_str()
        .to_string()
}

fn station() -> (Runtime, Station) {
    let registry = RuntimeBuilder::new()
        .register(IssuesWorld::registration(), Arc::new(IssuesWorld::new()))
        .build()
        .unwrap();
    let rt = Runtime::open(
        temp_root(),
        registry,
        Arc::new(WriterAuthority),
        Arc::new(replica::StaticBodyKeys::new(
            AuthorizedBodyKey::for_authorized_epoch([5u8; 16], [6u8; 32]),
        )),
    );
    let station = rt
        .form_space(runtime::SpaceFormationOptions::default())
        .unwrap()
        .activate(ActivationOptions::offline())
        .unwrap();
    (rt, station)
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
    let Response::Ref { reff } = created else {
        panic!("expected the new issue's handle, got {created:?}");
    };
    router.route(
        Request::Comment {
            reff: reff.clone(),
            body: "and a word about it".into(),
            reply_to: None,
        },
        &facts(),
    );

    let (activity, _) = router.route(Request::Activity { since: 0 }, &facts());
    let Response::Activity { events: rows, .. } = activity else {
        panic!("expected activity, got {activity:?}");
    };
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
