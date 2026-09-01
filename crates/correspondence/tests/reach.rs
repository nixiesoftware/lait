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
    let profile = alice.found(genesis.clone()).expect("found");

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
    let learned = bob
        .absorb(projection, &genesis, &bob_standing)
        .expect("absorb");
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
    let genesis = DeviceLink::seal(&ALICE_A, &ALICE_B, [7u8; 16], 1).expect("link");
    let profile = alice.found(genesis.clone()).expect("found");
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
        bob.absorb(projection, &genesis, &bob_standing).unwrap();
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

// ── The same reach, carried over a real HTTP carrier ───────────────────────────

use correspondence::post::{PostCarrier, Signer};
use lait_post::http::{router, Shared};
use lait_post::{FsStore, Post};
use std::sync::{Arc, Mutex};

/// Real seconds — a networked carrier checks a deposit's window against its own
/// clock, so a fixed timestamp would be refused as an unusable expiry.
fn wall_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_secs()
}

/// A local `lait-post` over HTTP, or the deployed one when `POST_SMOKE_URL` is set.
async fn serve() -> (String, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a deposit root");
    if let Ok(remote) = std::env::var("POST_SMOKE_URL") {
        return (remote.trim_end_matches('/').to_owned(), dir);
    }
    let store: lait_post::store::BoxedStore =
        Box::new(FsStore::open(dir.path()).expect("open the store"));
    let shared: Shared = Arc::new(Mutex::new(Post::new(store)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router(shared)).await;
    });
    (format!("http://127.0.0.1:{port}"), dir)
}

/// Reach resolved through kinship, then carried over a real HTTP `PostCarrier`:
/// Bob deposits to the device his resolution named, Alice fetches it there. The
/// only thing that told Bob where to send was Alice's profile.
#[tokio::test(flavor = "multi_thread")]
async fn reach_by_profile_then_carry_over_a_real_post() {
    let (base, _root) = serve().await;
    let bob_device = device_from_seed(&BOB);

    // Alice publishes; Bob resolves — the kinship half, identical to in-process.
    let mut alice = Registry::new();
    let genesis = DeviceLink::seal(&ALICE_A, &ALICE_B, [7u8; 16], 1).expect("link");
    let profile = alice.found(genesis.clone()).expect("found");
    alice
        .avow_reachable(
            &profile,
            Audience::Correspondent(Party::Device(bob_device.clone())),
            &ALICE_A,
            5,
            [3u8; 16],
        )
        .expect("avow");
    let bob_standing = Standing {
        device: Some(bob_device.clone()),
        ..Standing::default()
    };
    let projection = alice.project(&profile, &ALICE_A, 5, &bob_standing).unwrap();
    let mut bob = Registry::new();
    bob.absorb(projection, &genesis, &bob_standing).unwrap();
    let recipients = bob.resolve(&profile).expect("resolved");
    let addressed = recipients[0].clone();
    let alice_seed = if addressed == device_from_seed(&ALICE_A) {
        ALICE_A
    } else {
        ALICE_B
    };

    // Bob seals and deposits over HTTP under his egress.
    let letter = Letter::compose(
        &BOB,
        Content::Message {
            body: "carried, not in-process".into(),
        },
        wall_now(),
    );
    let sealed = letter
        .seal_to_devices(&recipients, &addressed, wall_now() + 3600)
        .expect("seal");

    let space = SpaceId::mint(&SystemUlidSource);
    let (bob_events, bob_actor) = incept(&BOB, 9, &space);
    let plane = actor::replay(&space, &bob_events);
    let bob_egress = egress::authorize(&plane, &bob_actor, &bob_device).expect("bob's key");

    // `block_in_place`, not `spawn_blocking`: the egress borrows the plane and
    // cannot move into a 'static closure — the staleness guarantee, working.
    let base_deposit = base.clone();
    let id = tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base_deposit, Signer::new(BOB));
        carrier.deposit(&bob_egress, &sealed, wall_now())
    })
    .expect("deposit over HTTP");
    assert!(!id.is_empty());

    // Alice fetches on the addressed device, authorized by her own key.
    let base_read = base.clone();
    let addressed_read = addressed.clone();
    let waiting = tokio::task::block_in_place(move || {
        let mut carrier = PostCarrier::new(base_read, Signer::new(alice_seed));
        carrier.collect(&addressed_read, wall_now())
    });
    let held = match &waiting {
        Missed::Held(held) => held,
        Missed::Unasked(why) => panic!("the carrier could not be asked: {why}"),
    };
    assert_eq!(
        held.len(),
        1,
        "the letter Bob addressed by resolution is waiting"
    );

    let mut mailbox = Mailbox::new();
    assert_eq!(mailbox.ingest(&alice_seed, &addressed, held), 1);
    let received = mailbox.letters();
    assert_eq!(
        received[0].letter.from, bob_device,
        "the proven sender is Bob"
    );
}

// ── Closing the loop: a shared Space bootstraps bidirectional in-band reach ─────

use mechanics::ids::SpaceId as SpaceIdForLoop;

const BOB_B: [u8; 32] = [41u8; 32];

/// After first contact puts Alice and Bob in one Space (the one-time paste), they
/// exchange profiles over the Members audience and each can reach the other
/// in-band — no third channel again, in either direction. Each holds the other's
/// profile in its own registry, so the reach survives leaving the Space.
#[test]
fn a_shared_space_closes_the_loop_both_directions() {
    let space = SpaceIdForLoop::mint(&SystemUlidSource);
    let member = |device: mechanics::ids::DeviceId| Standing {
        device: Some(device),
        spaces: vec![space.clone()],
        ..Standing::default()
    };
    let alice_device = device_from_seed(&ALICE_A);
    let bob_device = device_from_seed(&BOB);

    // Alice publishes her profile to the Space's members.
    let mut alice = Registry::new();
    let a_genesis = DeviceLink::seal(&ALICE_A, &ALICE_B, [7u8; 16], 1).expect("link");
    let alice_profile = alice.found(a_genesis.clone()).expect("found");
    alice
        .avow_reachable(
            &alice_profile,
            Audience::Members(space.clone()),
            &ALICE_A,
            5,
            [3u8; 16],
        )
        .expect("avow");

    // Bob publishes his.
    let mut bob = Registry::new();
    let b_genesis = DeviceLink::seal(&BOB, &BOB_B, [8u8; 16], 1).expect("link");
    let bob_profile = bob.found(b_genesis.clone()).expect("found");
    bob.avow_reachable(
        &bob_profile,
        Audience::Members(space.clone()),
        &BOB,
        5,
        [4u8; 16],
    )
    .expect("avow");

    // Each projects for the other as a fellow member, and absorbs it.
    let alice_for_bob = alice
        .project(&alice_profile, &ALICE_A, 5, &member(bob_device.clone()))
        .expect("project");
    let bob_for_alice = bob
        .project(&bob_profile, &BOB, 5, &member(alice_device.clone()))
        .expect("project");
    bob.absorb(alice_for_bob, &a_genesis, &member(bob_device))
        .expect("bob learns alice");
    alice
        .absorb(bob_for_alice, &b_genesis, &member(alice_device))
        .expect("alice learns bob");

    // The loop is closed: each resolves the other, in-band, either direction.
    assert_eq!(
        bob.resolve(&alice_profile).map(|d| d.len()),
        Some(2),
        "bob reaches alice"
    );
    assert_eq!(
        alice.resolve(&bob_profile).map(|d| d.len()),
        Some(2),
        "alice reaches bob"
    );
}
