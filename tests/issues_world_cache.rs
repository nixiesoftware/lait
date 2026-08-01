//! The IssuesWorld derived read-model cache — the mixed-root rejection proof
//! `tests/mixed_root_guard.rs` registers.
//!
//! The cache is keyed by the EXACT Manifest root a query context is pinned
//! to, so a lookup at one root can never serve bytes derived at another; the
//! per-issue parse memo is reusable across roots only under a reader-issued
//! Body version stamp whose equality guarantees byte-equivalent Bodies. The
//! tests drive ONE `IssuesWorld` instance across snapshots that disagree
//! about the same document and assert every answer matches the root it was
//! asked at — including a stamped body whose content changes root-to-root.

use std::collections::BTreeMap;

use lait::world::contract::{self, IssueQuery};
use runtime::{world::Context, world::Query, world::World};

const FOUNDER_SEED: [u8; 32] = [151u8; 32];

#[derive(Default)]
struct StubReader {
    views: BTreeMap<replica::body::BodyKey, fabric::CollaborativeView>,
    stamps: BTreeMap<replica::body::BodyKey, Vec<u8>>,
    catalog_bodies: Vec<replica::body::BodyKey>,
}

impl runtime::world::BodyReader for StubReader {
    fn read_body(&self, _key: &replica::body::BodyKey) -> Option<Vec<u8>> {
        None
    }
    fn read_collaborative_body(
        &self,
        key: &replica::body::BodyKey,
    ) -> Result<fabric::CollaborativeView, fabric::projection::Failure> {
        self.views
            .get(key)
            .cloned()
            .ok_or(fabric::projection::Failure::NotCollaborative)
    }
    fn body_version(&self, _key: &replica::body::BodyKey) -> Option<fabric::Version> {
        None
    }
    fn anchor_in_body(
        &self,
        _key: &replica::body::BodyKey,
        _path: &str,
        _position: u64,
    ) -> Option<fabric::Anchor> {
        None
    }
    fn resolve_anchor(
        &self,
        _key: &replica::body::BodyKey,
        _anchor: &fabric::Anchor,
    ) -> fabric::AnchorResolution {
        fabric::AnchorResolution::Drifted
    }
    fn content_status(
        &self,
        _content: &replica::content::ContentRef,
    ) -> Option<runtime::world::ContentStatus> {
        None
    }

    fn bodies_with_schema(
        &self,
        _world: &replica::body::WorldId,
        _schema: &replica::body::SchemaId,
    ) -> Vec<replica::body::BodyKey> {
        self.catalog_bodies.clone()
    }
    fn body_stamp(&self, key: &replica::body::BodyKey) -> Option<Vec<u8>> {
        self.stamps.get(key).cloned()
    }
}

fn facts(space: &mechanics::ids::SpaceId) -> runtime::world::PrincipalFacts {
    let device = mechanics::actor::device_from_seed(&FOUNDER_SEED);
    runtime::world::PrincipalFacts {
        actor: mechanics::ids::ActorId::from_incept_hash(&"ab".repeat(32)),
        station: mechanics::station::Key::from_device(&device).unwrap(),
        device,
        space: space.clone(),
        authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![]),
    }
}

const DOC: &str = "iss_00000000000000000000000001";

/// A minimal valid catalog view registering `DOC` under one project.
fn catalog_view() -> fabric::CollaborativeView {
    let mut v = fabric::CollaborativeView::default();
    let mut seqs = BTreeMap::new();
    seqs.insert(DOC.to_string(), b"1".to_vec());
    v.maps.insert("seqs".into(), seqs);
    let mut projects = BTreeMap::new();
    projects.insert(
        "prj_00000000000000000000000001".to_string(),
        br#"{"name":"Core","key":"CORE","color":"blue"}"#.to_vec(),
    );
    v.maps.insert("projects".into(), projects);
    v
}

/// An issue view whose title identifies the snapshot it belongs to.
fn issue_view(title: &str) -> fabric::CollaborativeView {
    let mut v = fabric::CollaborativeView::default();
    v.registers
        .insert("title".into(), title.as_bytes().to_vec());
    v.registers
        .insert("project".into(), b"prj_00000000000000000000000001".to_vec());
    v.registers.insert("status".into(), b"backlog".to_vec());
    v
}

fn reader(title: &str, stamp: Option<&[u8]>, space: &mechanics::ids::SpaceId) -> StubReader {
    let catalog = contract::catalog_key(space);
    let issue = contract::issue_key(DOC);
    let mut r = StubReader {
        catalog_bodies: vec![catalog.clone()],
        ..Default::default()
    };
    r.views.insert(catalog, catalog_view());
    r.views.insert(issue.clone(), issue_view(title));
    if let Some(stamp) = stamp {
        r.stamps.insert(issue, stamp.to_vec());
    }
    r
}

fn list_titles(world: &lait::world::IssuesWorld, ctx: &Context<'_>) -> Vec<String> {
    let projection = world
        .query(
            ctx,
            Query {
                schema: contract::issue_schema(),
                schema_version: contract::ISSUE_SCHEMA_VERSION,
                payload: IssueQuery::List {
                    project: None,
                    label: None,
                    status: None,
                    milestone: None,
                    mine: None,
                    all: true,
                    me: None,
                }
                .to_json(),
            },
        )
        .expect("list");
    let rows: serde_json::Value = serde_json::from_slice(&projection.bytes).unwrap();
    rows.as_array()
        .unwrap()
        .iter()
        .map(|r| r["title"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn the_issues_world_cache_never_serves_across_roots() {
    let space = mechanics::ids::SpaceId::from_digest([7u8; 16]);
    let world = lait::world::IssuesWorld::new();
    let facts = facts(&space);
    let root_a = [1u8; 32];
    let root_b = [2u8; 32];

    // Root A: the doc is titled "at-root-a". Ask twice (the second answer is
    // the cache hit) — both must be A's content.
    let ra = reader("at-root-a", None, &space);
    let ctx_a = Context::with_reads(&facts, &ra, root_a);
    assert_eq!(list_titles(&world, &ctx_a), vec!["at-root-a"]);
    assert_eq!(list_titles(&world, &ctx_a), vec!["at-root-a"]);

    // Root B on the SAME World instance: the same doc reads differently. A
    // lookup pinned to B must never surface A's cached derivation.
    let rb = reader("at-root-b", None, &space);
    let ctx_b = Context::with_reads(&facts, &rb, root_b);
    assert_eq!(list_titles(&world, &ctx_b), vec!["at-root-b"]);

    // And back: A's root still serves A's content (both roots stay warm).
    assert_eq!(list_titles(&world, &ctx_a), vec!["at-root-a"]);
}

#[test]
fn the_per_issue_memo_honors_the_version_stamp() {
    let space = mechanics::ids::SpaceId::from_digest([8u8; 16]);
    let world = lait::world::IssuesWorld::new();
    let facts = facts(&space);

    // Root A parses the issue under stamp s1.
    let ra = reader("stamped-one", Some(b"s1"), &space);
    let ctx_a = Context::with_reads(&facts, &ra, [3u8; 32]);
    assert_eq!(list_titles(&world, &ctx_a), vec!["stamped-one"]);

    // Root B changes the body AND its stamp: the memo entry must not be
    // reused — the new content is served.
    let rb = reader("stamped-two", Some(b"s2"), &space);
    let ctx_b = Context::with_reads(&facts, &rb, [4u8; 32]);
    assert_eq!(list_titles(&world, &ctx_b), vec!["stamped-two"]);

    // Root C keeps stamp s2: reuse is allowed precisely because the reader
    // vouches byte-equivalence — the answer is still B/C's content.
    let rc = reader("stamped-two", Some(b"s2"), &space);
    let ctx_c = Context::with_reads(&facts, &rc, [5u8; 32]);
    assert_eq!(list_titles(&world, &ctx_c), vec!["stamped-two"]);
}

#[test]
fn a_zero_root_context_is_never_cached() {
    // Fixture contexts without a snapshot identity (root == 0) must not
    // poison the cache: two zero-root readers with different content each see
    // their own.
    let space = mechanics::ids::SpaceId::from_digest([9u8; 16]);
    let world = lait::world::IssuesWorld::new();
    let facts = facts(&space);
    let ra = reader("zero-one", None, &space);
    let ctx_a = Context::with_reads(&facts, &ra, [0u8; 32]);
    assert_eq!(list_titles(&world, &ctx_a), vec!["zero-one"]);
    let rb = reader("zero-two", None, &space);
    let ctx_b = Context::with_reads(&facts, &rb, [0u8; 32]);
    assert_eq!(list_titles(&world, &ctx_b), vec!["zero-two"]);
}
