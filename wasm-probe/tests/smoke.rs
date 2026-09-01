//! Runtime validation of the engine core inside a JS host (Node, via
//! wasm-bindgen-test): entropy reaches the wasm build, and fabric's CRDT
//! engine forks, edits concurrently, exchanges material, and converges —
//! the property the whole browser port stands on.

#![cfg(all(
    target_arch = "wasm32",
    feature = "probe-fabric",
    feature = "probe-mechanics"
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
