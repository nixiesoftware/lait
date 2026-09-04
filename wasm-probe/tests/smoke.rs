//! Runtime validation of the engine core inside a JS host (Node, via
//! wasm-bindgen-test): entropy reaches the wasm build, and fabric's CRDT
//! engine forks, edits concurrently, exchanges material, and converges —
//! the property the whole browser port stands on.

#![cfg(all(
    target_arch = "wasm32",
    feature = "probe-fabric",
    feature = "probe-mechanics",
    feature = "probe-journal"
))]

use fabric::{Engine, Key, Op, Transaction};
use wasm_bindgen_test::wasm_bindgen_test;

fn key() -> Key {
    Key::from_bytes(b"body/smoke".to_vec())
}

#[wasm_bindgen_test]
fn entropy_reaches_the_wasm_build() {
    let (_device, secret) =
        mechanics::space::mint_recovery_key().expect("the js getrandom backend answers");
    assert!(
        secret.iter().any(|b| *b != 0),
        "a minted key must not be the zero key"
    );
}

/// The whole store — authenticated requirement indexes, manifest, GC — over
/// the pack in a JS host: `journal::Store::open_on` is the browser door, and
/// the semantic layer above the pack runs here unchanged.
#[wasm_bindgen_test]
fn the_store_runs_in_a_js_host() {
    use journal::{Index, MemMedium, Store};
    use std::sync::Arc;

    let medium = Arc::new(MemMedium::new());
    let mut store = Store::open_on(medium.clone()).expect("store opens");
    let sequence = store
        .commit(&[b"issue-one".to_vec()], &[], Index::NONE, b"meta".to_vec())
        .expect("commit lands");

    drop(store);
    let store = Store::open_on(medium).expect("store reopens");
    assert_eq!(store.manifest().map(|m| m.sequence), Some(sequence));
    assert_eq!(
        store.caller_meta().expect("meta reads"),
        Some(b"meta".to_vec())
    );
    let required = store.required_objects().expect("required lists");
    assert_eq!(
        store.read_object(&required[0]).expect("object reads"),
        b"issue-one"
    );
    store.collect_unreachable().expect("compaction runs");
    assert_eq!(
        store
            .read_object(&required[0])
            .expect("live data survives GC"),
        b"issue-one"
    );
}

/// The pack log — the storage format the browser port rides on — commits,
/// reads back, survives a reopen, and compacts, all inside a JS host on the
/// memory medium. The OPFS medium swaps in beneath this same seam.
#[wasm_bindgen_test]
fn the_pack_log_runs_in_a_js_host() {
    use journal::{MemMedium, PackStore};
    use std::sync::Arc;

    let medium = Arc::new(MemMedium::new());
    let mut pack = PackStore::open(medium.clone(), "hot").expect("pack opens");
    let alpha = b"alpha".to_vec();
    let beta = b"beta".to_vec();
    let alpha_hash = journal::object_content_hash(&alpha);
    let beta_hash = journal::object_content_hash(&beta);
    pack.commit(&[alpha.clone(), beta], b"m1".to_vec())
        .expect("commit lands");
    drop(pack);

    let mut pack = PackStore::open(medium, "hot").expect("pack reopens");
    assert_eq!(pack.sequence(), 1);
    assert_eq!(pack.manifest(), Some(b"m1".as_slice()));
    pack.compact(&|hash| *hash == alpha_hash).expect("compacts");
    assert_eq!(pack.read(&alpha_hash).expect("alpha survives"), alpha);
    assert!(!pack.contains(&beta_hash), "the dead object is gone");
}

#[wasm_bindgen_test]
fn replicas_converge_in_a_js_host() {
    let mut origin = Engine::new();
    origin
        .commit(Transaction::new(
            "ancestor",
            vec![
                Op::CreateBody { key: key() },
                Op::RegisterSet {
                    key: key(),
                    path: "reg0".into(),
                    value: b"0".to_vec(),
                },
                Op::TextSplice {
                    key: key(),
                    path: "text0".into(),
                    index: 0,
                    delete: 0,
                    insert: "seed".into(),
                },
            ],
        ))
        .expect("ancestor commits");
    let export = origin.export_body(&key()).expect("ancestor exports");

    let mut a = Engine::new();
    a.import_body(&key(), &export)
        .expect("a adopts the ancestor");
    let mut b = Engine::new();
    b.import_body(&key(), &export)
        .expect("b adopts the ancestor");

    a.commit(Transaction::new(
        "a-edit",
        vec![Op::RegisterSet {
            key: key(),
            path: "reg0".into(),
            value: b"a".to_vec(),
        }],
    ))
    .expect("a commits");
    b.commit(Transaction::new(
        "b-edit",
        vec![Op::TextSplice {
            key: key(),
            path: "text0".into(),
            index: 4,
            delete: 0,
            insert: " grown".into(),
        }],
    ))
    .expect("b commits");

    let from_a = a.export_body(&key()).expect("a exports");
    let from_b = b.export_body(&key()).expect("b exports");
    a.import_body(&key(), &from_b).expect("a absorbs b");
    b.import_body(&key(), &from_a).expect("b absorbs a");

    let view_a = a.read_collaborative(&key()).expect("a projects");
    let view_b = b.read_collaborative(&key()).expect("b projects");
    assert_eq!(
        view_a, view_b,
        "replicas that saw the same material must project the same view"
    );
}
