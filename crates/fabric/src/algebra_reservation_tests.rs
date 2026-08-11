//! Plan 13 F0 item 7 — reserved algebra space and the unknown-tag policy.
//!
//! The algebra was six types when this was written, with `tree` and `log`
//! reserved against the day the product needed them. Both days came: it is
//! eight now, and the reservation list is empty. What the list bought is
//! recorded here rather than in a changelog — F3 froze checkpoint and delta
//! encodings on top of this algebra, so those two types cost a version bump
//! each instead of a format migration, which is the entire return on having
//! named them years before writing them.
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
fn the_implemented_algebra_is_exactly_eight_types() {
    for tag in ["reg", "map", "list", "text", "set", "cnt", "tree", "log"] {
        assert!(is_implemented_type_tag(tag), "`{tag}` must be implemented");
        assert!(
            !is_reserved_type_tag(tag),
            "`{tag}` cannot be both implemented and reserved"
        );
    }
}

/// Both reservations were spent, and on exactly what they were taken for:
/// `tree` for the hierarchies the product hand-encoded through parent fields,
/// `log` for the activity feeds it stored as unbounded Lists. An empty list is
/// the honest state and not a lapse — but it is also the expensive state, so
/// this test says so rather than quietly passing over nothing.
#[test]
fn nothing_is_reserved_and_the_next_type_should_change_that() {
    let unspent: Vec<&str> = ["tree", "log"]
        .into_iter()
        .filter(|tag| is_reserved_type_tag(tag))
        .collect();
    assert!(
        unspent.is_empty(),
        "{unspent:?} are implemented and must no longer be reserved"
    );
    // The reservation mechanism itself still has to work, because the next
    // foreseen type belongs on the list before the encoding that would have to
    // migrate around it ships.
    assert!(!is_reserved_type_tag("quaternion"));
    assert!(!is_implemented_type_tag("quaternion"));
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
        .import_body(&key(), &body_binding_type("quaternion"))
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
