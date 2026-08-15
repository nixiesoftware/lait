//! The device-set sealing seam, exercised the way a consumer sees it.
//!
//! This file names `mechanics::authorization` and never `mechanics::crypto` —
//! that is the point of it. The primitive was built inside the kernel and
//! unreachable from outside for long enough that two projects planned to build
//! it again; these tests are what says the seam is actually cut.

use mechanics::actor::device_from_seed;
use mechanics::authorization::{
    add_device_to_sealed, open_as_device, seal_to_devices, DeviceSealed, Failure,
};

const CONTEXT: &[&[u8]] = &[b"lait/test/mailbox", b"epoch-1"];

#[test]
fn a_consumer_seals_to_a_device_set_and_reads_it_back() {
    let seed = [1u8; 32];
    let me = device_from_seed(&seed);

    let sealed = seal_to_devices(&[me.clone()], CONTEXT, b"the payload").expect("seal");

    assert_eq!(sealed.reader_count(), 1);
    assert!(sealed.addresses(&me));
    assert!(
        !sealed.ciphertext().is_empty(),
        "the payload is there, as ciphertext"
    );
    assert_eq!(
        open_as_device(&seed, &me, CONTEXT, &sealed).as_deref(),
        Some(&b"the payload"[..])
    );
}

#[test]
fn admitting_a_reader_needs_a_reader_and_re_encrypts_nothing() {
    let first_seed = [2u8; 32];
    let first = device_from_seed(&first_seed);
    let second_seed = [3u8; 32];
    let second = device_from_seed(&second_seed);
    let stranger_seed = [4u8; 32];
    let stranger = device_from_seed(&stranger_seed);

    let mut sealed = seal_to_devices(&[first.clone()], CONTEXT, b"a second device").expect("seal");
    let before = sealed.ciphertext().to_vec();

    // A device that cannot read cannot admit anyone — the only way to learn the
    // data key is to hold a wrap, so that is the whole authority this has.
    assert!(
        !add_device_to_sealed(
            &stranger_seed,
            &stranger,
            CONTEXT,
            &mut sealed,
            &second.clone()
        )
        .expect("no failure"),
        "a stranger must not be able to admit a reader"
    );
    assert_eq!(sealed.reader_count(), 1, "and must not have changed it");

    assert!(
        add_device_to_sealed(&first_seed, &first, CONTEXT, &mut sealed, &second)
            .expect("no failure"),
        "a device that can read admits another"
    );

    assert_eq!(sealed.reader_count(), 2);
    assert_eq!(
        sealed.ciphertext(),
        before.as_slice(),
        "admitting a reader must not re-encrypt the payload"
    );
    assert_eq!(
        open_as_device(&second_seed, &second, CONTEXT, &sealed).as_deref(),
        Some(&b"a second device"[..]),
        "and the newcomer reads it"
    );
    assert!(
        open_as_device(&stranger_seed, &stranger, CONTEXT, &sealed).is_none(),
        "while the stranger still cannot"
    );
}

#[test]
fn a_sealed_payload_survives_persistence() {
    let seed = [5u8; 32];
    let me = device_from_seed(&seed);
    let sealed = seal_to_devices(&[me.clone()], CONTEXT, b"stored and restored").expect("seal");

    let stored = serde_json::to_vec(&sealed).expect("serialize");
    let restored: DeviceSealed = serde_json::from_slice(&stored).expect("deserialize");

    assert_eq!(
        open_as_device(&seed, &me, CONTEXT, &restored).as_deref(),
        Some(&b"stored and restored"[..])
    );
}

#[test]
fn a_deserialized_envelope_whose_commitment_disagrees_is_inert() {
    let seed = [6u8; 32];
    let me = device_from_seed(&seed);
    let sealed = seal_to_devices(&[me.clone()], CONTEXT, b"honest").expect("seal");

    // A consumer cannot build an inconsistent `DeviceSealed` with a struct
    // literal — the fields are not public, and that is a compile-time fact this
    // test cannot express. What it can express is the runtime half: a value that
    // arrived over the wire, with a commitment that does not match the key its
    // wrap yields, opens as nothing at all.
    let mut value: serde_json::Value =
        serde_json::from_slice(&serde_json::to_vec(&sealed).expect("serialize"))
            .expect("as a value");
    let commitment = value
        .get_mut("dek_commitment")
        .expect("the commitment is a field")
        .as_array_mut()
        .expect("32 bytes");
    commitment[0] = serde_json::json!(commitment[0].as_u64().expect("byte") ^ 0xFF);

    let tampered: DeviceSealed = serde_json::from_value(value).expect("still well-formed");
    assert!(
        open_as_device(&seed, &me, CONTEXT, &tampered).is_none(),
        "a wrap that yields a key the envelope does not vouch for must not open"
    );
}

#[test]
fn sealing_to_nobody_is_a_failure_not_an_empty_success() {
    assert!(
        matches!(
            seal_to_devices(&[], CONTEXT, b"addressed to no one"),
            Err(Failure::Unaddressable)
        ),
        "an empty audience yields a payload nobody can open; that is a failure"
    );
}
