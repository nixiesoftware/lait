//! Plan 13 F0 item 7 — reserved algebra space and the unknown-tag policy.
//!
//! The collaborative algebra stays at six types in this docket. What it must
//! not do is make a seventh expensive: F3 freezes checkpoint and delta
//! encodings on top of this algebra, and after that an unreserved tag is a
//! migration rather than a version bump.
//!
//! The second half matters more. `read_collaborative` used to fall through an
//! unknown root tag with `_ => {}`, silently omitting its data from the
//! projection. Nobody chose that; it was the shape of a match arm. The declared
//! policy is to refuse, and to scope the refusal to the Body: the bytes stay
//! stored, forwarded, and converged — that is what byte-completeness means —
//! but no caller is ever handed a view that looks complete and is not.

use crate::fabric::{is_implemented_type_tag, is_reserved_type_tag};
use crate::projection::Failure as ProjectionFailure;
use crate::{BodyExport, Engine, Key, Op, Transaction};
use loro::{ExportMode, LoroDoc};

fn key() -> Key {
    Key::from_bytes(b"body-under-test".to_vec())
}

/// A Body as a peer on a later build would write it: ordinary text, plus a root
/// bound to a collaborative type this build does not implement. Built through
/// Loro directly because there is deliberately no production op that mints an
/// unimplemented type — the only way to receive one is from a peer.
fn body_binding_type(tag: &str) -> BodyExport {
    let doc = LoroDoc::new();
    doc.set_record_timestamp(true);
    doc.set_change_merge_interval(-1);
    doc.set_peer_id(4242).expect("fresh doc");
    doc.get_text("text:body").insert(0, "hello").expect("text");
    doc.get_map(format!("{tag}:children"))
        .insert("node", "value")
        .expect("future-typed root");
    doc.commit();
    BodyExport::Collaborative(doc.export(ExportMode::Snapshot).expect("export"))
}

fn engine_with_text() -> Engine {
    let mut fabric = Engine::new();
    fabric
        .commit(Transaction {
            request: "seed".into(),
            ops: vec![Op::TextSplice {
                key: key(),
                path: "body".into(),
                index: 0,
                delete: 0,
                insert: "hello".into(),
            }],
        })
        .expect("seed commit");
    fabric
}

#[test]
fn the_implemented_algebra_is_exactly_six_types() {
    for tag in ["reg", "map", "list", "text", "set", "cnt"] {
        assert!(is_implemented_type_tag(tag), "`{tag}` must be implemented");
        assert!(
            !is_reserved_type_tag(tag),
            "`{tag}` cannot be both implemented and reserved"
        );
    }
}

#[test]
fn tag_space_is_reserved_for_the_types_the_product_is_working_around() {
    // `tree`: sub-issues, milestone nesting, and threaded comments are all
    // hierarchies, and Issues hand-encodes threading over flat storage today.
    // `log`: activity feeds are unbounded Lists re-checkpointed on every
    // append. Reserving costs nothing; not reserving costs a migration.
    for tag in ["tree", "log"] {
        assert!(is_reserved_type_tag(tag), "`{tag}` must be reserved");
        assert!(
            !is_implemented_type_tag(tag),
            "a reserved tag must not be projectable — reserving it is the \
             promise that it is *not* implemented yet"
        );
    }
}

#[test]
fn a_reserved_tag_refuses_the_projection_rather_than_omitting_it() {
    // A peer on a later build writes a Body binding a reserved type. This
    // replica stores and converges it, and must refuse to project it.
    let mut fabric = Engine::new();
    fabric
        .import_body(&key(), &body_binding_type("tree"))
        .expect("a Body we cannot project still imports");

    assert_eq!(
        fabric.read_collaborative(&key()),
        Err(ProjectionFailure::SchemaAhead)
    );
}

#[test]
fn an_unknown_tag_refuses_the_projection() {
    let mut fabric = Engine::new();
    fabric
        .import_body(&key(), &body_binding_type("quaternion"))
        .expect("import");

    assert_eq!(
        fabric.read_collaborative(&key()),
        Err(ProjectionFailure::SchemaAhead)
    );
}

#[test]
fn an_unprojectable_body_still_exports_and_converges() {
    // The half that makes refusal acceptable. A replica must be able to carry
    // material it cannot interpret, or byte-completeness is a lie and the
    // network partitions on version skew.
    let mut ahead = Engine::new();
    ahead
        .import_body(&key(), &body_binding_type("tree"))
        .expect("import");
    let export = ahead
        .export_body(&key())
        .expect("a Body we cannot project still exports");

    let mut behind = Engine::new();
    behind
        .import_body(&key(), &export)
        .expect("import succeeds")
        .expect("the import is new material");

    assert!(
        matches!(
            behind.read_collaborative(&key()),
            Err(ProjectionFailure::SchemaAhead)
        ),
        "the receiving replica refuses the projection"
    );
    assert_eq!(
        behind.export_body(&key()),
        Some(export),
        "and forwards the material byte-identically to the next peer"
    );
}

#[test]
fn a_body_without_a_future_type_still_projects() {
    // The control: refusal must be scoped to Bodies that actually carry an
    // unimplemented type, not to every Body once one exists.
    let fabric = engine_with_text();
    let view = fabric.read_collaborative(&key()).expect("projects");
    assert_eq!(view.texts["body"], "hello");
}
