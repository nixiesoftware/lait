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
    let store = FsStore::open(dir.path()).expect("open the store");
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
