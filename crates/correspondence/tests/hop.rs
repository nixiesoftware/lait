//! One sealed letter, across a real socket, through the seam.
//!
//! Every other test in this crate proves a rule against `MemCarrier`, which is a
//! `BTreeMap`. This one proves the composition: a payload sealed by `mechanics`,
//! authorized by an `egress` witness, carried by the `PostCarrier` adapter over
//! HTTP to a real `lait-post`, collected by its recipient, and opened.
//!
//! It exists because that is the class of bug this initiative keeps hitting — the
//! guide for this work says it outright: "the class of bug to fear is the
//! composition bug. The client-to-process seam has been wrong twice with every
//! component correct, the composition wrong, and a symptom that named nothing.
//! Assert the chain, not the parts."
//!
//! `POST_SMOKE_URL` points it at a deployed Post instead of a local one, which is
//! how the running service gets exercised by the thing that defines correct. The
//! same variable `lait-post`'s own HTTP test uses, and deliberately not
//! `LAIT_`-prefixed for the reason recorded there.

use std::sync::{Arc, Mutex};

use correspondence::post::{PostCarrier, Signer};
use correspondence::{Carrier, Missed, Sealed};
use lait_post::http::{router, Shared};
use lait_post::{FsStore, Post};
use mechanics::actor::{self, device_from_seed, ActorOp, ConsentCtx};
use mechanics::authorization::{open_as_device, seal_to_devices};
use mechanics::egress;
use mechanics::ids::{ActorId, SpaceId, SystemUlidSource};

const SENDER_SEED: [u8; 32] = [61u8; 32];
const RECIPIENT_SEED: [u8; 32] = [62u8; 32];

/// The context a mailbox seals under.
///
/// A leading part distinct from every other consumer's, which is the obligation
/// `authorization::seal_to_bound` states: the kernel does not own the vocabulary,
/// and what a caller must own is a prefix nobody else uses.
const CONTEXT: &[&[u8]] = &[b"lait/correspondence/1/mailbox"];

/// Real time, because a networked carrier does not share this process's clock.
///
/// A fixed timestamp is right for `MemCarrier` and wrong here, and the failure is
/// instructive rather than cosmetic: the first draft used a constant ~150 days
/// ahead of now, so the expiry it derived exceeded the service's `MAX_RETENTION`
/// *measured against the service's own clock*, and the deposit came back
/// `UnusableExpiry`.
///
/// That is the service being right. A carrier must not take a depositor's word for
/// what time it is — otherwise "hold this for a century" is a client-side decision
/// — so a client's window is only ever a request, checked there.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_secs()
}

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

/// Incept a single-device actor, so there is a real device→actor binding for
/// `egress` to resolve rather than a fixture standing in for one.
fn incept(seed: &[u8; 32], nonce: u8, space: &SpaceId) -> (actor::SignedEvent, ActorId) {
    let devices = vec![device_from_seed(seed)];
    let binding = actor::consent_sign(
        seed,
        space.as_str(),
        [nonce; 16],
        &ConsentCtx::Incept {
            incept_nonce: &[nonce; 16],
            devices: &devices,
            recovery_commit: &None,
        },
    );
    let event = actor::sign_event(
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
    (event, id)
}

/// The whole hop: sealed here, carried over HTTP, opened there.
#[tokio::test(flavor = "multi_thread")]
async fn a_sealed_letter_crosses_a_real_carrier_and_only_its_recipient_opens_it() {
    let (base, _root) = serve().await;

    let space = SpaceId::mint(&SystemUlidSource);
    let (sender_event, sender_actor) = incept(&SENDER_SEED, 1, &space);
    let plane = actor::replay(&space, &[sender_event]);
    let sender_device = device_from_seed(&SENDER_SEED);
    let recipient_device = device_from_seed(&RECIPIENT_SEED);

    // Sealed to the recipient's device set. The carrier never sees a key and this
    // is where that starts being true: the ciphertext exists before any adapter
    // does.
    let plaintext = b"the first thing to cross a Space boundary";
    let sealed = seal_to_devices(std::slice::from_ref(&recipient_device), CONTEXT, plaintext)
        .expect("seal to the recipient");
    let envelope = Sealed {
        recipient: recipient_device.clone(),
        bytes: serde_json::to_vec(&sealed).expect("encode the sealed box"),
        expires_at: now() + 3600,
        construction: 1,
    };

    // The witness. Without it there is no way to call `deposit` at all, which is
    // the property `egress` exists to give this path.
    let standing = egress::authorize(&plane, &sender_actor, &sender_device).expect("her own key");

    // `block_in_place` rather than `spawn_blocking`, and the reason is the witness.
    // `Egress<'a>` borrows the `Directory` it was proven against — that borrow *is*
    // its staleness guarantee — so it cannot be moved into a `'static` closure. The
    // type refused to let this test pretend a witness outlives the state behind it,
    // which is the constraint working rather than one to route around.
    let id = tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base.clone(), Signer::new(SENDER_SEED));
        carrier.deposit(&standing, &envelope, now())
    })
    .expect("deposit over HTTP");
    assert!(!id.is_empty(), "a deposit answers with its id");

    // Collected by the recipient, which is a different carrier with a different
    // seed — the only thing that distinguishes them is which key they hold.
    let base_for_read = base.clone();
    let waiting = tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base_for_read, Signer::new(RECIPIENT_SEED));
        carrier.collect(&device_from_seed(&RECIPIENT_SEED), now())
    });

    let held = match &waiting {
        Missed::Held(held) => held,
        Missed::Unasked(why) => panic!("the carrier could not be asked: {why}"),
    };
    assert_eq!(held.len(), 1, "one letter was sent and one is waiting");
    let letter = &held[0];
    assert_eq!(letter.id, id);
    assert_eq!(
        letter.deposited_by, sender_device,
        "the deposit is attributed to the device that signed it"
    );

    // The end of the chain: the recipient opens it. This is what makes the test
    // about a *sealed* letter rather than about JSON arriving somewhere.
    let restored: mechanics::authorization::DeviceSealed =
        serde_json::from_slice(&letter.sealed.bytes).expect("decode the sealed box");
    assert_eq!(
        open_as_device(&RECIPIENT_SEED, &recipient_device, CONTEXT, &restored).as_deref(),
        Some(&plaintext[..]),
        "the recipient must read what was sent"
    );

    // And nobody else does, even holding the same bytes off the same carrier.
    let stranger_seed = [63u8; 32];
    assert!(
        open_as_device(
            &stranger_seed,
            &device_from_seed(&stranger_seed),
            CONTEXT,
            &restored
        )
        .is_none(),
        "a carrier that hands the bytes to anyone must still hand plaintext to nobody"
    );

    // Acknowledged, and gone. A carrier that could not be told "I have it" would
    // hold every letter until it expired.
    let base_for_ack = base.clone();
    let acked = tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base_for_ack, Signer::new(RECIPIENT_SEED));
        let device = device_from_seed(&RECIPIENT_SEED);
        carrier.acknowledge(&device, std::slice::from_ref(&id), now())
    })
    .expect("acknowledge over HTTP");
    assert_eq!(acked, 1);

    let base_for_recheck = base.clone();
    let after = tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base_for_recheck, Signer::new(RECIPIENT_SEED));
        carrier.collect(&device_from_seed(&RECIPIENT_SEED), now())
    });
    assert_eq!(
        after,
        Missed::Held(vec![]),
        "an acknowledged letter is gone, and the mailbox answers empty rather than \
         failing to be asked"
    );
}

/// A carrier that cannot be reached says so, and never says "nothing is waiting".
///
/// The branch that matters most in the field and the one an in-process carrier can
/// never produce by accident. A person shown an empty mailbox when the truth is
/// that their carrier is unreachable has been told nobody wrote to them.
#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_carrier_is_not_an_empty_mailbox() {
    // A port nothing is listening on. Bound and dropped so it is a real port that
    // is genuinely closed, rather than a number guessed at.
    let dead = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        listener.local_addr().expect("addr").port()
    };

    let missed = tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(
            format!("http://127.0.0.1:{dead}"),
            Signer::new(RECIPIENT_SEED),
        );
        carrier.collect(&device_from_seed(&RECIPIENT_SEED), now())
    });

    match missed {
        Missed::Unasked(why) => assert!(
            !why.is_empty(),
            "an unreachable carrier must carry its reason"
        ),
        Missed::Held(held) => panic!(
            "an unreachable carrier answered as a mailbox holding {} letters",
            held.len()
        ),
    }
}

/// A carrier signs as one device, and refuses to deposit under another's standing.
///
/// The custody fence at the adapter. `egress` proves whose key may be spent; this
/// proves the adapter cannot then spend a different one, which is the gap between
/// "authorized" and "actually signed by the authorized device".
#[tokio::test(flavor = "multi_thread")]
async fn a_carrier_refuses_a_witness_for_a_device_it_cannot_sign_as() {
    let (base, _root) = serve().await;

    let space = SpaceId::mint(&SystemUlidSource);
    let (sender_event, sender_actor) = incept(&SENDER_SEED, 1, &space);
    let plane = actor::replay(&space, &[sender_event]);
    let sender_device = device_from_seed(&SENDER_SEED);
    let standing = egress::authorize(&plane, &sender_actor, &sender_device).expect("hers");

    let sealed = seal_to_devices(
        std::slice::from_ref(&device_from_seed(&RECIPIENT_SEED)),
        CONTEXT,
        b"x",
    )
    .expect("seal");
    let envelope = Sealed {
        recipient: device_from_seed(&RECIPIENT_SEED),
        bytes: serde_json::to_vec(&sealed).expect("encode"),
        expires_at: now() + 3600,
        construction: 1,
    };

    let refused = tokio::task::block_in_place(|| {
        // Authorized for the sender's device, but holding the *recipient's* seed.
        let mut carrier = PostCarrier::new(base, Signer::new(RECIPIENT_SEED));
        carrier.deposit(&standing, &envelope, now())
    })
    .expect_err("a carrier must not deposit under a device it cannot sign as");

    assert!(
        format!("{refused}").contains("authorized"),
        "the refusal must name the mismatch: {refused}"
    );
}

/// A full mailbox is still collectable.
///
/// The bound that matters most, and the one that was wrong. `collect` reads the
/// reply into memory under a ceiling, so if a mailbox can hold more than the
/// ceiling can carry, a stranger can fill it and the recipient can never read
/// *any* of it — and because `take()` truncates silently, the failure arrives as
/// `Missed::Unasked`, so the person is told their carrier is unreachable while it
/// is up and holding their mail. Unrecoverable through the client, too:
/// `acknowledge` needs deposit ids and the only source of ids is `collect`.
///
/// So this fills a mailbox to the carrier's own ceiling with maximal envelopes and
/// insists the whole thing comes back. It is the relationship between the bounds
/// under test, not any one number.
#[tokio::test(flavor = "multi_thread")]
async fn a_mailbox_filled_to_its_ceiling_is_still_collectable() {
    let (base, _root) = serve().await;

    let space = SpaceId::mint(&SystemUlidSource);
    let (sender_event, sender_actor) = incept(&SENDER_SEED, 1, &space);
    let plane = actor::replay(&space, &[sender_event]);
    let sender_device = device_from_seed(&SENDER_SEED);
    let recipient_device = device_from_seed(&RECIPIENT_SEED);
    let standing = egress::authorize(&plane, &sender_actor, &sender_device).expect("hers");

    // Worst-case bytes: every byte >= 100 costs four characters as a JSON number,
    // so a payload of 0xff is the most expensive encoding of a maximal envelope.
    // Deliberately not random — the bound has to hold for the worst case, and a
    // test that used average bytes would pass while the real ceiling did not.
    let filler = vec![0xffu8; correspondence::MAX_SEALED];
    let expires = now() + 3600;

    // A handful rather than the full ceiling: enough to exceed a wrong bound by a
    // wide margin, few enough that the test stays seconds rather than minutes. The
    // arithmetic relationship is asserted separately and exactly.
    let letters = 24;
    for n in 0..letters {
        let mut bytes = filler.clone();
        // Distinct content, or the content-addressed id collapses them into one.
        bytes[..8].copy_from_slice(&(n as u64).to_be_bytes());
        let envelope = Sealed {
            recipient: recipient_device.clone(),
            bytes,
            expires_at: expires,
            construction: 1,
        };
        tokio::task::block_in_place(|| {
            let mut carrier = PostCarrier::new(base.clone(), Signer::new(SENDER_SEED));
            carrier.deposit(&standing, &envelope, now())
        })
        .unwrap_or_else(|error| panic!("deposit {n} of {letters}: {error}"));
    }

    let base_for_read = base.clone();
    let collected = tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base_for_read, Signer::new(RECIPIENT_SEED));
        carrier.collect(&device_from_seed(&RECIPIENT_SEED), now())
    });

    match collected {
        Missed::Held(held) => assert_eq!(
            held.len(),
            letters,
            "every letter in a mailbox must be collectable"
        ),
        Missed::Unasked(why) => panic!(
            "a mailbox holding {letters} maximal letters could not be collected, so a \
             stranger can censor a device by filling it: {why}"
        ),
    }
}

/// The reply ceiling must cover the worst mailbox the carrier will accept.
///
/// Asserted as arithmetic rather than left to a comment, because the two constants
/// live in different crates and drifted the moment they were written: the doc said
/// `MAX_SEALED × MAX_MAILBOX` and the code multiplied by 64.
#[test]
fn the_reply_ceiling_covers_a_full_mailbox() {
    let worst = correspondence::post::worst_reply_bytes();
    assert!(
        correspondence::post::max_reply() >= worst,
        "a full mailbox encodes to {worst} bytes and the reply ceiling is {}; the \
         difference is a censorship window",
        correspondence::post::max_reply()
    );
}

/// Blocking works across a real carrier: a blocked sender's letter never lands,
/// and unblocking restores it.
///
/// The carrier-side half of what makes a readable address survivable. Runs against
/// a local Post, and against the deployed one under `POST_SMOKE_URL`.
#[tokio::test(flavor = "multi_thread")]
async fn a_blocked_sender_is_refused_at_a_real_carrier() {
    let (base, _root) = serve().await;

    let space = SpaceId::mint(&SystemUlidSource);
    let (sender_event, sender_actor) = incept(&SENDER_SEED, 1, &space);
    let (recipient_event, recipient_actor) = incept(&RECIPIENT_SEED, 2, &space);
    let plane = actor::replay(&space, &[sender_event, recipient_event]);
    let sender_device = device_from_seed(&SENDER_SEED);
    let recipient_device = device_from_seed(&RECIPIENT_SEED);

    // The recipient blocks the sender, on the recipient's own witnessed authority.
    let recipient_standing =
        egress::authorize(&plane, &recipient_actor, &recipient_device).expect("recipient");
    let sender_for_block = sender_device.clone();
    tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base.clone(), Signer::new(RECIPIENT_SEED));
        carrier.block(&recipient_standing, &sender_for_block, true, now())
    })
    .expect("block over HTTP");

    // The sender deposits — accept-shaped — and the recipient collects nothing.
    let sender_standing = egress::authorize(&plane, &sender_actor, &sender_device).expect("sender");
    let sealed = seal_to_devices(
        std::slice::from_ref(&recipient_device),
        CONTEXT,
        b"unwanted",
    )
    .expect("seal");
    let envelope = Sealed {
        recipient: recipient_device.clone(),
        bytes: serde_json::to_vec(&sealed).expect("encode"),
        expires_at: now() + 3600,
        construction: 1,
    };
    tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base.clone(), Signer::new(SENDER_SEED));
        carrier.deposit(&sender_standing, &envelope, now())
    })
    .expect("a blocked deposit is accept-shaped");

    let base_for_read = base.clone();
    let blocked_view = tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base_for_read, Signer::new(RECIPIENT_SEED));
        carrier.collect(&device_from_seed(&RECIPIENT_SEED), now())
    });
    match blocked_view {
        Missed::Held(held) => assert!(
            held.is_empty(),
            "a blocked sender's material must never reach the recipient's device"
        ),
        Missed::Unasked(why) => panic!("the carrier could not be asked: {why}"),
    }

    // Unblock, and the next letter lands.
    let recipient_standing =
        egress::authorize(&plane, &recipient_actor, &recipient_device).expect("recipient");
    let sender_for_unblock = sender_device.clone();
    tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base.clone(), Signer::new(RECIPIENT_SEED));
        carrier.block(&recipient_standing, &sender_for_unblock, false, now())
    })
    .expect("unblock over HTTP");

    let sealed = seal_to_devices(std::slice::from_ref(&recipient_device), CONTEXT, b"welcome")
        .expect("seal");
    let envelope = Sealed {
        recipient: recipient_device.clone(),
        bytes: serde_json::to_vec(&sealed).expect("encode"),
        expires_at: now() + 3600,
        construction: 1,
    };
    tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base.clone(), Signer::new(SENDER_SEED));
        carrier.deposit(&sender_standing, &envelope, now())
    })
    .expect("deposit");

    let after = tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base.clone(), Signer::new(RECIPIENT_SEED));
        carrier.collect(&device_from_seed(&RECIPIENT_SEED), now())
    });
    assert_eq!(
        after.held().map(<[_]>::len),
        Some(1),
        "an unblocked sender is delivered again"
    );

    // Clean up after ourselves on a shared deployed carrier: acknowledge the one
    // letter so the mailbox is left as empty as we found it.
    if let Missed::Held(held) = after {
        let ids: Vec<String> = held.iter().map(|w| w.id.clone()).collect();
        let _ = tokio::task::block_in_place(|| {
            let mut carrier = PostCarrier::new(base.clone(), Signer::new(RECIPIENT_SEED));
            carrier.acknowledge(&device_from_seed(&RECIPIENT_SEED), &ids, now())
        });
    }
}

/// Two people who share no Space hold a conversation across a real carrier.
///
/// "Instant messaging", at the mechanism: a sealed, signed message crosses, is
/// read, and is replied to. Each letter is confidential (sealed to the
/// recipient's device) and authentic (signed by the sender, verified on open),
/// and neither party is in the other's Space — the whole point of the plane.
#[tokio::test(flavor = "multi_thread")]
async fn two_strangers_exchange_sealed_messages() {
    use correspondence::{Content, Letter};

    let (base, _root) = serve().await;

    let space = SpaceId::mint(&SystemUlidSource);
    let (alice_event, alice_actor) = incept(&SENDER_SEED, 1, &space);
    let (bob_event, bob_actor) = incept(&RECIPIENT_SEED, 2, &space);
    let plane = actor::replay(&space, &[alice_event, bob_event]);
    let alice_device = device_from_seed(&SENDER_SEED);
    let bob_device = device_from_seed(&RECIPIENT_SEED);

    // Alice writes to Bob.
    let hello = Letter::compose(
        &SENDER_SEED,
        Content::Message {
            body: "are you there?".into(),
        },
        now(),
    );
    let sealed = hello.seal_to(&bob_device, now() + 3600).expect("seal");
    let alice_standing = egress::authorize(&plane, &alice_actor, &alice_device).expect("alice");
    tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base.clone(), Signer::new(SENDER_SEED));
        carrier.deposit(&alice_standing, &sealed, now())
    })
    .expect("deposit alice's message");

    // Bob collects, opens, verifies, reads.
    let base_bob = base.clone();
    let for_bob = tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base_bob, Signer::new(RECIPIENT_SEED));
        carrier.collect(&device_from_seed(&RECIPIENT_SEED), now())
    });
    let waiting = for_bob.held().expect("asked").to_vec();
    assert_eq!(waiting.len(), 1);
    let opened = Letter::open(&RECIPIENT_SEED, &bob_device, &waiting[0].sealed)
        .expect("bob opens and the signature verifies");
    assert_eq!(
        opened.from, alice_device,
        "the message is from alice, proven"
    );
    match &opened.content {
        Content::Message { body } => assert_eq!(body, "are you there?"),
        other => panic!("expected a message, got {other:?}"),
    }
    let hello_id = waiting[0].id.clone();

    // Bob replies.
    let reply = Letter::compose(
        &RECIPIENT_SEED,
        Content::Message {
            body: "I am. hello.".into(),
        },
        now(),
    );
    let sealed = reply.seal_to(&alice_device, now() + 3600).expect("seal");
    let bob_standing = egress::authorize(&plane, &bob_actor, &bob_device).expect("bob");
    tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base.clone(), Signer::new(RECIPIENT_SEED));
        carrier.deposit(&bob_standing, &sealed, now())
    })
    .expect("deposit bob's reply");

    // Alice reads the reply.
    let base_alice = base.clone();
    let for_alice = tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base_alice, Signer::new(SENDER_SEED));
        carrier.collect(&device_from_seed(&SENDER_SEED), now())
    });
    let reply_waiting = for_alice.held().expect("asked").to_vec();
    assert_eq!(reply_waiting.len(), 1);
    let opened = Letter::open(&SENDER_SEED, &alice_device, &reply_waiting[0].sealed)
        .expect("alice opens the reply");
    assert_eq!(opened.from, bob_device);
    match &opened.content {
        Content::Message { body } => assert_eq!(body, "I am. hello."),
        other => panic!("expected a message, got {other:?}"),
    }
    let reply_id = reply_waiting[0].id.clone();

    // Leave the shared carrier as empty as we found it.
    let _ = tokio::task::block_in_place(|| {
        let mut a = PostCarrier::new(base.clone(), Signer::new(RECIPIENT_SEED));
        let _ = a.acknowledge(&bob_device, std::slice::from_ref(&hello_id), now());
        let mut b = PostCarrier::new(base.clone(), Signer::new(SENDER_SEED));
        b.acknowledge(&alice_device, std::slice::from_ref(&reply_id), now())
    });
}

/// A real, self-authenticating invitation crosses the carrier and verifies.
///
/// "Invitation working", end to end: a `SignedCoordinates` minted by a founder,
/// sealed inside a letter, deposited, collected, opened — and then it
/// self-authenticates against its own Space id, needing no prior state, which is
/// exactly the property that lets it ride any carrier and lets a total stranger
/// check it.
#[tokio::test(flavor = "multi_thread")]
async fn a_real_invitation_crosses_the_carrier_and_self_authenticates() {
    use correspondence::{Content, Letter};
    use runtime::coordinates::{
        ApproachRoute, CoordinatesAdmission, CoordinatesPayload, SignedCoordinates,
    };

    const FOUNDER_SEED: [u8; 32] = [71u8; 32];
    const RECOVERY_SEED: [u8; 32] = [72u8; 32];
    const STATION_SEED: [u8; 32] = [73u8; 32];
    const SALT: [u8; 16] = [9u8; 16];

    let (base, _root) = serve().await;

    // Found a Space and mint a genuine invitation into it.
    let rc = mechanics::space::recovery_commit(&mechanics::space::recovery_pub_of(&RECOVERY_SEED))
        .expect("recovery commit");
    let founder_device = mechanics::space::recovery_pub_of(&FOUNDER_SEED);
    let ws = mechanics::space::derive_space_id(&founder_device, &SALT, &rc);
    let (incept_event, _actor) =
        mechanics::actor::incept_single(&FOUNDER_SEED, &ws, [1u8; 16], [2u8; 16], None);
    let station_key = device_from_seed(&STATION_SEED)
        .key_bytes()
        .expect("station key");
    let payload = CoordinatesPayload {
        space: <[u8; 29]>::try_from(ws.as_str().as_bytes()).expect("space bytes"),
        salt: SALT,
        recovery_root: rc,
        founder_inception: postcard::to_stdvec(&incept_event).expect("encode inception"),
        display_name_hint: "A Space".into(),
        approach_station: station_key,
        approach_nick_hint: "host".into(),
        approach_routes: vec![ApproachRoute::DirectIpv4 {
            ip: [10, 0, 0, 1],
            port: 4242,
        }],
        admission: CoordinatesAdmission::None,
    };
    let invitation = SignedCoordinates::sign(payload, &STATION_SEED);
    // It self-authenticates before it is even sent — the property the letter is
    // only the carriage for.
    invitation
        .verify()
        .expect("a freshly minted invitation verifies");

    // Alice seals it to Bob and deposits it.
    let space = SpaceId::mint(&SystemUlidSource);
    let (alice_event, alice_actor) = incept(&SENDER_SEED, 1, &space);
    let plane = actor::replay(&space, &[alice_event]);
    let alice_device = device_from_seed(&SENDER_SEED);
    let bob_device = device_from_seed(&RECIPIENT_SEED);

    let letter = Letter::compose(
        &SENDER_SEED,
        Content::Invitation {
            coordinates: invitation.encode(),
        },
        now(),
    );
    let sealed = letter.seal_to(&bob_device, now() + 3600).expect("seal");
    let alice_standing = egress::authorize(&plane, &alice_actor, &alice_device).expect("alice");
    tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base.clone(), Signer::new(SENDER_SEED));
        carrier.deposit(&alice_standing, &sealed, now())
    })
    .expect("deposit the invitation");

    // Bob collects, opens, and the invitation self-authenticates for him — a
    // stranger to that Space, with no prior state.
    let base_bob = base.clone();
    let for_bob = tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base_bob, Signer::new(RECIPIENT_SEED));
        carrier.collect(&device_from_seed(&RECIPIENT_SEED), now())
    });
    let waiting = for_bob.held().expect("asked").to_vec();
    assert_eq!(waiting.len(), 1);
    let opened =
        Letter::open(&RECIPIENT_SEED, &bob_device, &waiting[0].sealed).expect("bob opens it");
    let carried = match &opened.content {
        Content::Invitation { coordinates } => {
            SignedCoordinates::decode_canonical(coordinates).expect("decode the carried invitation")
        }
        other => panic!("expected an invitation, got {other:?}"),
    };
    let verified = carried
        .verify()
        .expect("the carried invitation self-authenticates for a stranger");
    assert_eq!(
        verified.space.as_str(),
        ws.as_str(),
        "it is an invitation to the Space it was minted for"
    );

    let ack_id = waiting[0].id.clone();
    let _ = tokio::task::block_in_place(|| {
        let mut carrier = PostCarrier::new(base.clone(), Signer::new(RECIPIENT_SEED));
        carrier.acknowledge(&bob_device, std::slice::from_ref(&ack_id), now())
    });
}
