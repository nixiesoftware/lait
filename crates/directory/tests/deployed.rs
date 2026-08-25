//! The directory against a **deployed** service, when one is pointed at by
//! `DIRECTORY_SMOKE_URL` (e.g. `https://post.foundation.pub`).
//!
//! Skipped when unset, so the offline suite never depends on the network —
//! the same shape `correspondence`'s deployed-Post test takes, and for the same
//! reason.
//!
//! What this proves that `over_http` cannot: that the *storage* keeps its
//! promises. `MemStore` claims an address in a `BTreeMap`, where uniqueness is
//! free; the deployed store claims it in Firestore, where it is the whole
//! reason that store was chosen. A publish/resolve round trip here is the only
//! evidence that the atomic mint and the conditional-delete spend behave
//! against the real thing.

use addressbook::{Announcement, Registry};
use lait_directory::registry::{
    chronicle_entry, Label, Registrar, RegistryStore, Resolved, RoutePublish,
};
use lait_directory::{address::Address, Credentials, Directory, FirestoreStore, Refusal, Remote};
use mechanics::{
    actor::device_from_seed,
    kinship::{Audience, DeviceLink, Standing},
};

fn deployed() -> Option<Remote> {
    let base = std::env::var("DIRECTORY_SMOKE_URL").ok()?;
    let base = base.trim().to_owned();
    (!base.is_empty()).then(|| Remote::at(&base))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A profile founded from fresh randomness, so a re-run does not collide with
/// what the last one published.
fn fresh_profile() -> ([u8; 32], Announcement) {
    let first = mechanics::actor::random_seed().expect("randomness");
    let second = mechanics::actor::random_seed().expect("randomness");
    let genesis = DeviceLink::seal(&first, &second, [11u8; 16], 1).expect("genesis");
    let mut registry = Registry::new();
    let profile = registry.found(genesis.clone()).expect("found");
    registry
        .avow_reachable(&profile, Audience::Public, &first, 2, [3u8; 16])
        .expect("avow");
    let projection = registry
        .project(&profile, &first, 2, &Standing::default())
        .expect("project");
    (first, Announcement::new(profile, genesis, projection))
}

/// The acceptance line: a person hands somebody an address out loud, and that
/// somebody learns their device set with no other channel.
#[test]
fn a_deployed_directory_issues_an_address_and_answers_it() {
    let Some(mut directory) = deployed() else {
        eprintln!("DIRECTORY_SMOKE_URL unset — skipping the deployed directory smoke");
        return;
    };

    let (seed, announcement) = fresh_profile();
    let address = lait_directory::publish_as(&mut directory, &seed, &announcement, now())
        .expect("the deployed directory took the publication");
    assert!(
        address.is_mintable(),
        "{address} came back off the word list"
    );

    // Stable afterwards — the property a person's card depends on.
    let again =
        lait_directory::publish_as(&mut directory, &seed, &announcement, now()).expect("republish");
    assert_eq!(address, again, "the address moved under its holder");

    // A stranger, holding nothing but the spoken address.
    let asker = mechanics::actor::random_seed().expect("randomness");
    let answered = lait_directory::resolve_as(&mut directory, &asker, &address, now())
        .expect("an exact address resolves");

    let mut reader = Registry::new();
    let profile = reader
        .absorb(
            answered.projection.clone(),
            &answered.genesis,
            &Standing::default(),
        )
        .expect("the answer anchors to its own genesis");
    assert_eq!(profile, announcement.profile);
    assert!(
        reader
            .resolve(&profile)
            .is_some_and(|devices| devices.contains(&device_from_seed(&seed))),
        "the device that published is not in what the directory answered"
    );
}

/// Against the real store, not a map: absence and denial are one answer.
#[test]
fn a_deployed_directory_says_the_same_thing_about_everyone_it_does_not_hold() {
    let Some(mut directory) = deployed() else {
        eprintln!("DIRECTORY_SMOKE_URL unset — skipping the deployed directory smoke");
        return;
    };

    let asker = mechanics::actor::random_seed().expect("randomness");
    let mut answers = Vec::new();
    for entropy in [[0x41u8; 16], [0x42u8; 16]] {
        let unheld = Address::mint(&entropy);
        answers.push(
            lait_directory::resolve_as(&mut directory, &asker, &unheld, now())
                .expect_err("nobody holds a freshly minted address"),
        );
    }
    assert_eq!(answers[0], Refusal::NotAvailable);
    assert_eq!(answers[0], answers[1]);
}

/// The conditional-delete spend, against Firestore rather than a `BTreeMap`.
/// A nonce that worked once must not work twice, and that is the property the
/// backing store was chosen for.
#[test]
fn a_deployed_directory_spends_a_challenge_exactly_once() {
    let Some(mut directory) = deployed() else {
        eprintln!("DIRECTORY_SMOKE_URL unset — skipping the deployed directory smoke");
        return;
    };

    let (seed, announcement) = fresh_profile();
    let address =
        lait_directory::publish_as(&mut directory, &seed, &announcement, now()).expect("publish");

    let asker = mechanics::actor::random_seed().expect("randomness");
    let device = device_from_seed(&asker);
    let challenge = directory.challenge(&device, now()).expect("challenge");
    let request = lait_directory::wire::sign::resolve(&asker, &challenge, &address);

    assert!(
        Directory::resolve(&mut directory, &request, now()).is_ok(),
        "the first use of a challenge was refused"
    );
    assert_eq!(
        Directory::resolve(&mut directory, &request, now()).unwrap_err(),
        Refusal::StaleChallenge,
        "the deployed store honoured one nonce twice"
    );
}

// ── The registrar's chronicle, against real Firestore ────────────────────────
//
// `CHRONICLE_FIRESTORE_BASE` (a `.../databases/<db>/documents` URL) plus
// `CHRONICLE_FIRESTORE_TOKEN` (a bearer token) point these at a real Firestore
// database — deliberately NOT the production `(default)` one, because a
// chronicle is append-only and a test entry in it is permanent. Skipped when
// unset.
//
// What these prove that `registry`'s `MemRegistry` tests cannot: the chronicle
// rests on Firestore's create-with-chosen-id being an atomic linearization
// point, and `chronicle_leaves` on a real paged `list` that returns documents
// in name order. `MemRegistry` gets both for free from a `Vec` and a
// `BTreeMap`; only the real store is evidence they hold where it counts.

fn firestore_chronicle() -> Option<FirestoreStore> {
    let base = std::env::var("CHRONICLE_FIRESTORE_BASE").ok()?;
    let token = std::env::var("CHRONICLE_FIRESTORE_TOKEN").ok()?;
    let base = base.trim();
    let token = token.trim();
    (!base.is_empty() && !token.is_empty())
        .then(|| FirestoreStore::at(base, Credentials::Fixed(token.to_owned())))
}

/// A bound label and the publication that answers it, both fresh per run so a
/// re-run against the same database never collides with the last.
fn bound_publication(store: &mut FirestoreStore) -> (Label, RoutePublish, Announcement) {
    let (seed, announcement) = fresh_profile();
    // A label unique to this run. The profile id is random per run, but ULID
    // Crockford base32 is uppercase and a label is `[a-z0-9-]`, so lower-filter
    // the tail into the grammar rather than feeding the id in raw.
    let tail: String = announcement
        .profile
        .as_str()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .rev()
        .take(16)
        .collect();
    let label = Label::parse(format!("smoke-{tail}")).expect("a valid label");
    assert!(
        store
            .bind(&label, &announcement.profile)
            .expect("bind writes to Firestore"),
        "the label was already bound — a run collided"
    );
    let endpoint = device_from_seed(&[251u8; 32]).as_str().to_owned();
    let publish = RoutePublish::sign(
        label.clone(),
        announcement.encode().expect("encode"),
        endpoint,
        now(),
        &seed,
    );
    (label, publish, announcement)
}

/// The whole chain against the real store: publish records a route AND appends
/// a chronicle entry whose inclusion the receipt proves, a second publication
/// extends the signed head provably, and the head verifies under its signer.
#[test]
fn a_real_firestore_chronicle_proves_every_publication_it_records() {
    let Some(store) = firestore_chronicle() else {
        eprintln!("CHRONICLE_FIRESTORE_* unset — skipping the real-Firestore chronicle smoke");
        return;
    };
    // A stable-per-process signer, so heads across the run share one identity.
    let mut registrar = Registrar::open(store, [131u8; 32]).expect("open over Firestore");

    // First publication: the receipt must prove its own entry is in the head.
    let (_label_a, publish_a, _ann_a) = bound_publication(registrar.store());
    let receipt = registrar
        .publish(&publish_a)
        .expect("Firestore took the route");
    let head = receipt.head.clone().expect("a chronicled receipt");
    head.verify().expect("the head signature verifies");
    let entry = receipt.entry.expect("an entry index");
    let leaf = mechanics::chronicle::Chronicle::leaf_of(&chronicle_entry(&publish_a));
    mechanics::chronicle::verify_inclusion(&leaf, entry, head.size, &head.root, &receipt.inclusion)
        .expect("the inclusion path verifies against the real head");

    // A reader pins this head; a second publication must provably extend it.
    let pin = mechanics::chronicle::PinnedHead::from(&head);
    let (_label_b, publish_b, _ann_b) = bound_publication(registrar.store());
    registrar.publish(&publish_b).expect("second route");
    let answer = registrar
        .answer(Some(pin.size))
        .expect("the chronicle surface answers");
    assert_eq!(
        mechanics::chronicle::advance(Some(&pin), &answer.head, &answer.consistency),
        Ok(mechanics::chronicle::Advance::Extended),
        "the real-Firestore head does not provably extend the pinned one"
    );

    // And the route the publication authorized actually resolves from Firestore.
    let resolved: Resolved = registrar
        .store()
        .route(&_label_a)
        .expect("route read")
        .expect("the bound label resolves");
    assert_eq!(resolved.endpoint, publish_a.endpoint);
}

/// The linearization point itself: two independent store handles race to append
/// a leaf at one index. Firestore's create-with-chosen-id must let exactly one
/// win, or two readers could pin forking roots at the same size. This is the
/// property `MemRegistry`'s `Vec::push` cannot exhibit and the real store was
/// chosen for.
#[test]
fn real_firestore_admits_exactly_one_leaf_per_index() {
    let Some(mut a) = firestore_chronicle() else {
        eprintln!("CHRONICLE_FIRESTORE_* unset — skipping the real-Firestore linearization smoke");
        return;
    };
    let Some(mut b) = firestore_chronicle() else {
        return;
    };

    // Both handles read the same current size — the index they will contend.
    let size_a = a.chronicle_leaves().expect("list a").len() as u64;
    let size_b = b.chronicle_leaves().expect("list b").len() as u64;
    assert_eq!(size_a, size_b, "two handles disagree on the current size");
    let index = size_a;

    let won_a = a
        .append_chronicle(index, [0xAAu8; 32])
        .expect("append a did not error");
    let won_b = b
        .append_chronicle(index, [0xBBu8; 32])
        .expect("append b did not error");

    assert_ne!(
        won_a, won_b,
        "both handles claimed index {index} (or both were refused) — not a linearization point"
    );

    // Whichever won, the leaf now at that index is theirs, and the loser's
    // reload sees a size one greater with a single, unforked leaf.
    let after = a.chronicle_leaves().expect("re-list");
    assert_eq!(
        after.len() as u64,
        index + 1,
        "the index took exactly one leaf"
    );
    let expected = if won_a { [0xAAu8; 32] } else { [0xBBu8; 32] };
    assert_eq!(
        after[index as usize], expected,
        "the winner's leaf is not the one Firestore kept"
    );
}
