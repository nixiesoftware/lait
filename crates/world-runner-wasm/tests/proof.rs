//! The wasm ABI, proven end to end: the wasmtime host runs the proof-World
//! module (built by `build.rs`) through the real `HostedRunner`/`Conversation`
//! seam and the real postcard frames — Describe, a callback round-trip, trap
//! recovery, and a large payload. No TCP, no daemon, no Space.

#![cfg(proof_world_wasm)]

use std::sync::Arc;
use std::time::Duration;

use world_runner::wasm_abi::GuestInit;
use world_runner::{no_detached_callbacks, CallbackHandler, HostedRunner, Operation, Reply};
use world_runner_wasm::{Limits, WasmInstance};

fn proof_world() -> Vec<u8> {
    std::fs::read(env!("PROOF_WORLD_WASM")).expect("the build script left a proof-world module")
}

fn launch() -> WasmInstance {
    WasmInstance::launch(
        &proof_world(),
        GuestInit {
            world: "com.lait.proof".into(),
            version: "0.0.0".into(),
            release: "local".into(),
        },
    )
    .expect("the proof World instantiates and answers init")
}

fn launch_bounded(limits: Limits) -> WasmInstance {
    WasmInstance::launch_with_limits(
        &proof_world(),
        GuestInit {
            world: "com.lait.proof".into(),
            version: "0.0.0".into(),
            release: "local".into(),
        },
        limits,
    )
    .expect("the proof World instantiates under tight bounds")
}

fn no_callback(_: &str, _: &[u8]) -> Result<Vec<u8>, String> {
    Err("the World raised an unexpected callback".to_string())
}

/// A detached handler that must never be reached on wasm: a single guest
/// instance is idle once `wr_handle` returns, so it emits no post-reply
/// callback. Any call here is the very defect S7.7 established cannot happen.
struct PanicIfDetached;

impl CallbackHandler for PanicIfDetached {
    fn call(&self, operation: &str, _payload: &[u8]) -> Result<Vec<u8>, String> {
        panic!("a wasm guest reached the detached handler with {operation:?} — it must not");
    }
}

#[test]
fn describe_returns_the_guests_own_service_identity() {
    let mut instance = launch();
    // The identity `init` read back, carried on the runner and re-answered by
    // a live Describe, must agree.
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
    assert_eq!(descriptor.world, "com.lait.proof");
    assert_eq!(descriptor.implementation_version, 1);
}

#[test]
fn a_deferred_lease_drains_inline_on_wasm() {
    // The retained-Find-lease shape — the only first-party path (Issues
    // Geometry) that outlives its query() natively. On wasm the build runs
    // inline, so the whole acquire → query_detached×N → release sequence
    // crosses the SYNCHRONOUS in-request callback and the detached handler is
    // never reached. That is why no `wr_pump` export is owed. If a wasm guest
    // ever emitted a post-reply callback, `PanicIfDetached` would fire.
    let mut instance = launch();
    let mut inline_ops: Vec<String> = Vec::new();
    let reply = {
        let mut callback = |operation: &str, _payload: &[u8]| -> Result<Vec<u8>, String> {
            inline_ops.push(operation.to_string());
            Ok(Vec::new())
        };
        instance
            .open()
            .unwrap()
            .dispatch(
                Operation::Call {
                    operation: "drain".into(),
                    payload: Vec::new(),
                },
                &mut callback,
                Arc::new(PanicIfDetached),
            )
            .expect("drain answers")
    };
    // Every find.* callback was answered inline, before the reply.
    assert!(inline_ops.iter().any(|op| op == "find.acquire_deferred"));
    assert_eq!(
        inline_ops
            .iter()
            .filter(|op| *op == "find.query_detached")
            .count(),
        3,
        "all three detached queries drained in-request"
    );
    assert!(inline_ops.iter().any(|op| op == "find.release"));
    let Reply::Call { payload } = reply else {
        panic!("drain did not return a call reply");
    };
    assert_eq!(
        payload,
        3u32.to_le_bytes(),
        "the guest drained three queries"
    );
}

#[test]
fn a_call_round_trips_one_synchronous_host_callback() {
    let mut instance = launch();
    let mut saw: Option<Vec<u8>> = None;
    let reply = {
        let mut callback = |operation: &str, payload: &[u8]| -> Result<Vec<u8>, String> {
            // The guest's "echo" op asks the host to "ping" and appends the
            // payload to the host's answer.
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
    assert_eq!(
        payload, b"pong:hello",
        "guest returned host answer + payload"
    );
}

#[test]
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

    // The instance re-instantiated the immutable module; Describe answers again
    // with the same identity.
    let reply = instance
        .open()
        .unwrap()
        .dispatch(
            Operation::Describe,
            &mut no_callback,
            no_detached_callbacks(),
        )
        .expect("the runner answers after recovering from a trap");
    let Reply::Descriptor(descriptor) = reply else {
        panic!("post-trap describe did not return a descriptor");
    };
    assert_eq!(descriptor.world, "com.lait.proof");
}

#[test]
fn a_large_payload_crosses_linear_memory_intact() {
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

#[test]
fn a_runaway_guest_traps_on_the_request_deadline_and_the_instance_recovers() {
    // A short deadline and a brisk ticker so a spinning guest is caught in
    // well under a second rather than the production thirty.
    let mut instance = launch_bounded(Limits {
        deadline_ticks: 1,
        tick: Duration::from_millis(50),
        ..Limits::default()
    });
    let ran_away = instance.open().unwrap().dispatch(
        Operation::Call {
            operation: "spin".into(),
            payload: Vec::new(),
        },
        &mut no_callback,
        no_detached_callbacks(),
    );
    assert!(ran_away.is_err(), "a guest past its deadline traps");
    // The deadline is per-request, so the instance is usable again afterward.
    let reply = instance
        .open()
        .unwrap()
        .dispatch(
            Operation::Describe,
            &mut no_callback,
            no_detached_callbacks(),
        )
        .expect("the runner answers after a deadline trap");
    assert!(matches!(reply, Reply::Descriptor(_)));
}

#[test]
fn a_guest_past_its_memory_ceiling_traps() {
    // A 32 MiB ceiling the "hog" op blows through in a few 8 MiB chunks.
    let mut instance = launch_bounded(Limits {
        memory_bytes: 32 * 1024 * 1024,
        ..Limits::default()
    });
    let hogged = instance.open().unwrap().dispatch(
        Operation::Call {
            operation: "hog".into(),
            payload: Vec::new(),
        },
        &mut no_callback,
        no_detached_callbacks(),
    );
    assert!(hogged.is_err(), "a guest past its memory ceiling traps");
    // And the instance recovers to answer again.
    let reply = instance
        .open()
        .unwrap()
        .dispatch(
            Operation::Describe,
            &mut no_callback,
            no_detached_callbacks(),
        )
        .expect("the runner answers after a memory trap");
    assert!(matches!(reply, Reply::Descriptor(_)));
}
