//! Reach a person who shares no Space with you.
//!
//! This is the goal, end to end, over the real pieces: Bob resolves Alice to a
//! device set **through her kinship profile alone** — no Space in common, no
//! third channel — then seals a signed letter to those devices and hands it to
//! a carrier. Alice, on one of her devices, collects it, opens it, and the
//! signature verifies. Nothing here consults a Space membership; the only thing
//! that linked Alice's devices to Bob is a profile she chose to make reachable.
//!
//! The carrier is `MemCarrier`, so this proves the store-and-forward *shape*
//! (deposit now, collect later — the offline case) without a network. The hop
//! tests cover the same over a real HTTP `PostCarrier`.

use addressbook::Registry;
use correspondence::{Carrier, Content, Letter, Mailbox, MemCarrier, Missed};
use mechanics::actor::{
    self, consent_sign, device_from_seed, sign_event, ActorOp, ConsentCtx, SignedEvent,
};
use mechanics::egress;
use mechanics::ids::{ActorId, SpaceId, SystemUlidSource};
use mechanics::kinship::{Audience, DeviceLink, Party, Standing};

const NOW: u64 = 1_800_000_000;

// Alice's two devices, and Bob's one.
const ALICE_A: [u8; 32] = [11u8; 32];
const ALICE_B: [u8; 32] = [12u8; 32];
const BOB: [u8; 32] = [40u8; 32];

/// Incept a single-device actor so `egress` has a real device→actor binding to
/// resolve. Bob needs one to spend his key at the carrier.
fn incept(seed: &[u8; 32], nonce: u8, space: &SpaceId) -> (Vec<SignedEvent>, ActorId) {
    let devices = vec![device_from_seed(seed)];
    let binding = consent_sign(
        seed,
        space.as_str(),
        [nonce; 16],
        &ConsentCtx::Incept {
            incept_nonce: &[nonce; 16],
            devices: &devices,
            recovery_commit: &None,
        },
    );
    let event = sign_event(
        seed,
        &ActorOp::Incept {
            space: space.as_str().to_owned(),
            nonce: [nonce; 16],
            devices: vec![binding],
            recovery_commit: None,
        },
        vec![],
        space,
    );
    let id = ActorId::from_incept_hash(&event.hash());
    (vec![event], id)
}

#[test]
fn a_sender_reaches_a_recipient_by_profile_with_no_shared_space() {
    let bob_device = device_from_seed(&BOB);

    // ── Alice publishes a profile and makes her devices reachable to Bob ──
    let mut alice = Registry::new();
    let genesis = DeviceLink::seal(&ALICE_A, &ALICE_B, [7u8; 16], 1).expect("link");
    let profile = alice.found(genesis).expect("found");

    let to_bob = Audience::Correspondent(Party::Device(bob_device.clone()));
    let avowed = alice
        .avow_reachable(&profile, to_bob, &ALICE_A, 5, [3u8; 16])
        .expect("avow reachable");
    assert_eq!(avowed, 2, "both of Alice's devices are reachable");

    // The reader she projects for: Bob, named by his device.
    let bob_standing = Standing {
        device: Some(bob_device.clone()),
        ..Standing::default()
    };
    let projection = alice
        .project(&profile, &ALICE_A, 5, &bob_standing)
        .expect("project");

    // ── Bob learns Alice from the projection and resolves her devices ──
    let mut bob = Registry::new();
    let learned = bob.absorb(projection, &bob_standing).expect("absorb");
    assert_eq!(learned, profile);
    let recipients = bob.resolve(&profile).expect("resolved");
    assert_eq!(recipients.len(), 2, "Bob resolved both of Alice's devices");

    // ── Bob seals a signed letter to those devices and deposits it ──
    let letter = Letter::compose(
        &BOB,
        Content::Message {
            body: "no Space in common, and yet".into(),
        },
        NOW,
    );
    // Sealed to Alice's whole resolved set; addressed at the device she will
    // collect on. (One deposit reaches one keyed device today; the multi-reader
    // rework, CORR-28, collapses the set to one envelope.)
    let addressed = &recipients[0];
    let sealed = letter
        .seal_to_devices(&recipients, addressed, NOW + 3600)
        .expect("seal");

    // Bob's egress: a real device→actor binding proving whose key is spent.
    let space = SpaceId::mint(&SystemUlidSource);
    let (bob_events, bob_actor) = incept(&BOB, 9, &space);
    let plane = actor::replay(&space, &bob_events);
    let bob_egress = egress::authorize(&plane, &bob_actor, &bob_device).expect("bob's key");

    let mut carrier = MemCarrier::new();
    carrier.deposit(&bob_egress, &sealed, NOW).expect("deposit");

    // ── Later, offline no longer: Alice collects on the addressed device ──
    let alice_seed = if *addressed == device_from_seed(&ALICE_A) {
        ALICE_A
    } else {
        ALICE_B
    };
    let held = carrier.collect(addressed, NOW + 10);
    let waiting = match held {
        Missed::Held(waiting) => waiting,
        Missed::Unasked(why) => panic!("the carrier could not be asked: {why}"),
    };

    let mut mailbox = Mailbox::new();
    let filed = mailbox.ingest(&alice_seed, addressed, &waiting);
    assert_eq!(filed, 1, "Alice opened and filed exactly the one letter");

    let received = mailbox.letters();
    assert_eq!(received.len(), 1);
    let letter = &received[0];
    assert_eq!(letter.letter.from, bob_device, "the proven sender is Bob");
    match &letter.letter.content {
        Content::Message { body } => assert_eq!(body, "no Space in common, and yet"),
        other => panic!("expected a message, got {other:?}"),
    }
}

/// The other device in Alice's set can open the very same envelope — the
/// ciphertext is multi-reader even while the carrier keys one recipient. This is
/// what CORR-28's rework turns into a single deposit fetched by any of them.
#[test]
fn any_avowed_device_opens_the_same_sealed_letter() {
    // Alice's set, resolved the same way.
    let bob_device = device_from_seed(&BOB);
    let mut alice = Registry::new();
    let profile = alice
        .found(DeviceLink::seal(&ALICE_A, &ALICE_B, [7u8; 16], 1).expect("link"))
        .expect("found");
    alice
        .avow_reachable(
            &profile,
            Audience::Correspondent(Party::Device(bob_device.clone())),
            &ALICE_A,
            5,
            [3u8; 16],
        )
        .expect("avow");
    let recipients = {
        let bob_standing = Standing {
            device: Some(bob_device.clone()),
            ..Standing::default()
        };
        let projection = alice.project(&profile, &ALICE_A, 5, &bob_standing).unwrap();
        let mut bob = Registry::new();
        bob.absorb(projection, &bob_standing).unwrap();
        bob.resolve(&profile).unwrap()
    };

    let letter = Letter::compose(
        &BOB,
        Content::Message {
            body: "for both".into(),
        },
        NOW,
    );
    // Address at device A, but open as device B — both are in the wrap set.
    let sealed = letter
        .seal_to_devices(&recipients, &device_from_seed(&ALICE_A), NOW + 3600)
        .expect("seal");

    let opened = Letter::open(&ALICE_B, &device_from_seed(&ALICE_B), &sealed)
        .expect("the other device opens the same envelope");
    assert_eq!(opened.from, bob_device);
}
