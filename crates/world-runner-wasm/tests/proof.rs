//! The wasm ABI, proven end to end: the wasmtime host runs the proof-World
//! module (built by `build.rs`) through the real `HostedRunner`/`Conversation`
//! seam and the real postcard frames — Describe, a callback round-trip, trap
//! recovery, and a large payload. No TCP, no daemon, no Space.

#![cfg(proof_world_wasm)]

use std::time::Duration;

use world_runner::wasm_abi::GuestInit;
use world_runner::{no_detached_callbacks, HostedRunner, Operation, Reply};
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
