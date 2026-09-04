//! The four-function World-runner ABI, proven in a real browser: the guest
//! module runs under the browser's own `WebAssembly`, driven by the engine
//! module through JS glue, rather than under wasmtime. This is the S7.1 exit
//! criterion — the mechanism the in-browser engine will use to run a World
//! runner — held against the same proof-World the native host proves against.
//!
//! Runs in a dedicated Worker under `wasm-pack test --headless --chrome
//! --test runner`. Skips honestly (no test) when the proof-World was not built
//! for wasm32.

#![cfg(all(target_arch = "wasm32", feature = "probe-runner", proof_world_wasm))]

use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use wasm_probe::runner::WebInstance;
use world_runner::wasm_abi::GuestInit;
use world_runner::{no_detached_callbacks, HostedRunner, Operation, Reply};

wasm_bindgen_test_configure!(run_in_dedicated_worker);

const PROOF_WORLD: &[u8] = include_bytes!(env!("PROOF_WORLD_WASM"));

fn launch() -> WebInstance {
    WebInstance::launch(
        PROOF_WORLD,
        GuestInit {
            world: "com.lait.proof".into(),
            version: "0.0.0".into(),
            release: "local".into(),
        },
    )
    .expect("the proof World instantiates in the browser and answers init")
}

fn no_callback(_: &str, _: &[u8]) -> Result<Vec<u8>, String> {
    Err("the World raised an unexpected callback".to_string())
}

#[wasm_bindgen_test]
fn describe_returns_the_guests_own_service_identity() {
    let mut instance = launch();
    assert_eq!(instance.descriptor().world, "com.lait.proof");
    assert_eq!(instance.descriptor().implementation, [7; 32]);

    let reply = instance
        .open()
        .unwrap()
        .dispatch(
            Operation::Describe,
            &mut no_callback,
            no_detached_callbacks(),
        )
        .expect("describe answers");
    let Reply::Descriptor(descriptor) = reply else {
        panic!("describe did not return a descriptor");
    };
    assert_eq!(descriptor.implementation_version, 1);
}

#[wasm_bindgen_test]
fn a_call_round_trips_one_synchronous_host_callback() {
    let mut instance = launch();
    let mut saw: Option<Vec<u8>> = None;
    let reply = {
        let mut callback = |operation: &str, payload: &[u8]| -> Result<Vec<u8>, String> {
            assert_eq!(operation, "ping");
            saw = Some(payload.to_vec());
            Ok(b"pong:".to_vec())
        };
        instance
            .open()
            .unwrap()
            .dispatch(
                Operation::Call {
                    operation: "echo".into(),
                    payload: b"hello".to_vec(),
                },
                &mut callback,
                no_detached_callbacks(),
            )
            .expect("echo answers")
    };
    assert_eq!(
        saw.as_deref(),
        Some(&b"hello"[..]),
        "the host saw the payload"
    );
    let Reply::Call { payload } = reply else {
        panic!("echo did not return a call reply");
    };
    assert_eq!(payload, b"pong:hello");
}

#[wasm_bindgen_test]
fn a_guest_trap_surfaces_as_an_error_and_the_instance_recovers() {
    let mut instance = launch();
    let trapped = instance.open().unwrap().dispatch(
        Operation::Call {
            operation: "trap".into(),
            payload: Vec::new(),
        },
        &mut no_callback,
        no_detached_callbacks(),
    );
    assert!(
        trapped.is_err(),
        "a trapping guest is an error, not a reply"
    );

    let reply = instance
        .open()
        .unwrap()
        .dispatch(
            Operation::Describe,
            &mut no_callback,
            no_detached_callbacks(),
        )
        .expect("the runner answers after recovering from a trap");
    assert!(matches!(reply, Reply::Descriptor(_)));
}

#[wasm_bindgen_test]
fn a_large_payload_crosses_the_two_memories_intact() {
    let mut instance = launch();
    let payload = vec![0xa5_u8; 4 * 1024 * 1024];
    let reply = instance
        .open()
        .unwrap()
        .dispatch(
            Operation::Call {
                operation: "len".into(),
                payload: payload.clone(),
            },
            &mut no_callback,
            no_detached_callbacks(),
        )
        .expect("len answers");
    let Reply::Call { payload: answer } = reply else {
        panic!("len did not return a call reply");
    };
    let measured = u64::from_le_bytes(answer.try_into().expect("eight-byte length"));
    assert_eq!(measured, payload.len() as u64, "the guest saw every byte");
}
