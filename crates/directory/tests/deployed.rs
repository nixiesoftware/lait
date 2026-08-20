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
use lait_directory::{address::Address, Directory, Refusal, Remote};
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
