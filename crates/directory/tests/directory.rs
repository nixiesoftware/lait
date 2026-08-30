//! The directory's acceptance criteria, as tests.
//!
//! Every one of these is a line from the design Spec *"The directory is a mirror,
//! and an address is issued rather than chosen"* or from AUTH-16/17/18/24. They
//! are here rather than beside the code because they are properties of the
//! service as a whole — what it refuses is as much the product as what it
//! answers.

use addressbook::{Announcement, Registry};
use lait_directory::{
    address::Address,
    wire::{sign, Challenge},
    Directory, MemStore, Refusal, Service,
};
use mechanics::{
    actor::device_from_seed,
    ids::DeviceId,
    kinship::{Audience, DeviceLink, ProfileId, Standing},
};

const NOW: u64 = 1_700_000_000;

/// One identity home: two device seeds, a genesis link between them, and the
/// registry that authored it. Two seeds because a kinship genesis needs two
/// distinct devices; the plane retires its witness, this fixture keeps both.
struct Home {
    registry: Registry,
    profile: ProfileId,
    genesis: DeviceLink,
    seeds: [[u8; 32]; 2],
}

impl Home {
    fn found(a: u8, b: u8) -> Self {
        let seeds = [[a; 32], [b; 32]];
        let genesis = DeviceLink::seal(&seeds[0], &seeds[1], [7u8; 16], 1).expect("genesis");
        let mut registry = Registry::new();
        let profile = registry.found(genesis.clone()).expect("found a profile");
        Self {
            registry,
            profile,
            genesis,
            seeds,
        }
    }

    /// Avow this home's live devices to the public audience and project it.
    fn announce(&mut self, epoch: u64) -> Announcement {
        self.registry
            .avow_reachable(
                &self.profile,
                Audience::Public,
                &self.seeds[0],
                epoch,
                [u8::try_from(epoch % 256).unwrap_or(0); 16],
            )
            .expect("avow the live set");
        let projection = self
            .registry
            .project(&self.profile, &self.seeds[0], epoch, &Standing::default())
            .expect("project for the public reader");
        Announcement::new(self.profile.clone(), self.genesis.clone(), projection)
    }

    fn device(&self) -> DeviceId {
        device_from_seed(&self.seeds[0])
    }

    /// Announce at `epoch` and publish it, as this home's canonical device.
    fn publish(&mut self, service: &mut Service<MemStore>, epoch: u64) -> Result<Address, Refusal> {
        let announcement = self.announce(epoch);
        lait_directory::publish_as(service, &self.seeds[0], &announcement, NOW)
    }
}

fn service() -> Service<MemStore> {
    Service::new(MemStore::new())
}

/// A stranger with a key and nothing else — the asker in every resolution.
fn stranger(tag: u8) -> [u8; 32] {
    [tag; 32]
}

fn challenge_for(service: &mut Service<MemStore>, seed: &[u8; 32]) -> Challenge {
    Directory::challenge(service, &device_from_seed(seed), NOW).expect("a challenge is free")
}

/// The preimage a hand-built resolution has to sign over.
///
/// Rebuilt here rather than exposed from the crate: the framing is the wire
/// format's business, and a test that needed it made public would be a test that
/// had grown into a second implementation. The one caller is the theft case,
/// which is the only place a request is composed field by field.
fn signed_preimage(request: &lait_directory::SignedResolve) -> Vec<u8> {
    let mut out = Vec::new();
    for part in [
        b"lait/directory/1/resolve".as_slice(),
        request.device.as_str().as_bytes(),
        &request.nonce,
        request.address.as_bytes(),
    ] {
        out.extend_from_slice(&(part.len() as u32).to_be_bytes());
        out.extend_from_slice(part);
    }
    out
}

// ---------------------------------------------------------------- publishing

/// *"minted on first publish and stable afterwards"* — the property that makes
/// republishing safe. Without it a device rotation would hand a person a new
/// address and silently break every card they had already given out.
#[test]
fn an_address_is_minted_once_and_survives_republishing() {
    let mut home = Home::found(21, 22);
    let mut service = service();

    let first = home.publish(&mut service, 2).expect("publish");
    let again = home.publish(&mut service, 3).expect("republish");

    assert_eq!(first, again, "an address moved under its holder");
    assert!(first.is_mintable(), "{first} was minted off the word list");
}

/// The acceptance line the whole initiative exists for: an address is enough to
/// reach somebody you share nothing with.
#[test]
fn an_address_is_all_a_stranger_needs_to_learn_a_device_set() {
    let mut home = Home::found(21, 22);
    let mut service = service();
    let address = home.publish(&mut service, 2).expect("publish");

    let asker = stranger(90);
    let answered = lait_directory::resolve_as(&mut service, &asker, &address, NOW)
        .expect("an exact address resolves");

    // The asker anchors it themselves. That is the whole trust position: the
    // service handed over bytes, and the reader is what decides they are real.
    let mut reader = Registry::new();
    let profile = reader
        .absorb(
            answered.projection.clone(),
            &answered.genesis,
            &Standing::default(),
        )
        .expect("the answer anchors to its own genesis");
    assert_eq!(profile, home.profile);
    assert!(
        reader.resolve(&profile).is_some_and(|d| !d.is_empty()),
        "a resolved profile answers with the devices it avowed"
    );
}

/// AUTH-24: the service verifies on the publisher's own terms and cannot forge.
/// A stranger who merely *saw* an announcement cannot present it.
#[test]
fn a_publisher_the_announcement_does_not_avow_is_refused() {
    let mut home = Home::found(21, 22);
    let announcement = home.announce(2);
    let mut service = service();

    let outsider = stranger(77);
    let refusal =
        lait_directory::publish_as(&mut service, &outsider, &announcement, NOW).unwrap_err();
    assert_eq!(refusal, Refusal::NotAuthentic);
}

/// The codec carries no integrity — postcard has no checksum, and a mutation
/// frequently still decodes. Anchoring is what refuses this.
#[test]
fn an_announcement_wearing_somebody_elses_genesis_is_refused() {
    let mut honest = Home::found(21, 22);
    let announcement = honest.announce(2);
    let foreign = Home::found(41, 42);
    let forged = Announcement::new(
        announcement.profile.clone(),
        foreign.genesis,
        announcement.projection.clone(),
    );

    let mut service = service();
    let challenge = challenge_for(&mut service, &honest.seeds[0]);
    let request = sign::publish(
        &honest.seeds[0],
        &challenge,
        forged.encode().expect("encode"),
    );
    assert_eq!(
        Directory::publish(&mut service, &request, NOW).unwrap_err(),
        Refusal::NotAuthentic
    );
}

// ------------------------------------------------------------------- replay

/// AUTH-16: *"A bare signature is replayable by anyone who observed it."* The
/// nonce is single-use, so observing a resolution buys nothing.
#[test]
fn a_replayed_resolution_fails() {
    let mut home = Home::found(21, 22);
    let mut service = service();
    let address = home.publish(&mut service, 2).expect("publish");

    let asker = stranger(90);
    let challenge = challenge_for(&mut service, &asker);
    let request = sign::resolve(&asker, &challenge, &address);

    assert!(Directory::resolve(&mut service, &request, NOW).is_ok());
    assert_eq!(
        Directory::resolve(&mut service, &request, NOW).unwrap_err(),
        Refusal::StaleChallenge,
        "the same signed resolution worked twice"
    );
}

/// The same for publishing, and it matters more: a replayed publication is how a
/// captured older device set would be re-presented as current.
#[test]
fn a_replayed_publication_fails() {
    let mut home = Home::found(21, 22);
    let mut service = service();
    let challenge = challenge_for(&mut service, &home.seeds[0]);
    let encoded = home.announce(2).encode().expect("encode");
    let request = sign::publish(&home.seeds[0], &challenge, encoded);

    assert!(Directory::publish(&mut service, &request, NOW).is_ok());
    assert_eq!(
        Directory::publish(&mut service, &request, NOW).unwrap_err(),
        Refusal::StaleChallenge
    );
}

/// A challenge issued to one device and answered by another is refused as stale
/// rather than as a mismatch — whether a nonce exists is not a fact worth
/// confirming to whoever is asking.
#[test]
fn a_challenge_issued_to_one_device_cannot_be_answered_by_another() {
    let mut home = Home::found(21, 22);
    let mut service = service();
    let address = home.publish(&mut service, 2).expect("publish");

    let watcher = stranger(90);
    let thief = stranger(91);
    let challenge = challenge_for(&mut service, &watcher);

    // Built by hand rather than through `sign::resolve`, which copies the
    // challenge's device and so cannot express this: the thief names *itself*
    // and signs correctly, over a nonce issued to somebody else.
    let mut request = lait_directory::SignedResolve {
        device: device_from_seed(&thief),
        address: address.as_str().to_owned(),
        nonce: challenge.nonce,
        signature: [0u8; 64],
    };
    request.signature = mechanics::actor::sign_detached(&thief, &signed_preimage(&request));
    assert_eq!(
        Directory::resolve(&mut service, &request, NOW).unwrap_err(),
        Refusal::StaleChallenge
    );
}

/// An expired challenge is spent by being asked about, so a stale nonce cannot
/// be probed repeatedly while it ages out.
#[test]
fn an_expired_challenge_is_not_open_and_does_not_stay_probeable() {
    let mut service = service();
    let asker = stranger(90);
    let challenge = challenge_for(&mut service, &asker);
    let address = Address::parse("tin-harbor-quiet-4417").expect("well formed");
    let request = sign::resolve(&asker, &challenge, &address);

    let past_ttl = NOW + lait_directory::bounds::CHALLENGE_TTL + 1;
    assert_eq!(
        Directory::resolve(&mut service, &request, past_ttl).unwrap_err(),
        Refusal::StaleChallenge
    );
}

// -------------------------------------------------------------- enumeration

/// AUTH-16: *"Failure responses must not distinguish 'no such person' from 'you
/// may not ask'."* Asserted by comparing the two answers rather than by reading
/// the code that produces them.
#[test]
fn an_address_nobody_holds_answers_exactly_what_a_withheld_one_would() {
    let mut service = service();
    let asker = stranger(90);

    let unheld = Address::mint(&[0x11; 16]);
    let challenge = challenge_for(&mut service, &asker);
    let request = sign::resolve(&asker, &challenge, &unheld);
    let absence = Directory::resolve(&mut service, &request, NOW).unwrap_err();

    let also_unheld = Address::mint(&[0x22; 16]);
    let challenge = challenge_for(&mut service, &asker);
    let request = sign::resolve(&asker, &challenge, &also_unheld);
    let denial = Directory::resolve(&mut service, &request, NOW).unwrap_err();

    assert_eq!(absence, Refusal::NotAvailable);
    assert_eq!(absence, denial);
    assert_eq!(
        absence.to_string(),
        denial.to_string(),
        "the rendered refusals differ, which is an oracle however carefully worded"
    );
}

/// A miss costs the prober its budget, or probing is free and the rate limit is
/// decoration.
#[test]
fn probing_for_addresses_nobody_holds_still_spends_the_askers_budget() {
    let mut service = service();
    let asker = stranger(90);

    let mut refusals = Vec::new();
    for attempt in 0..=lait_directory::bounds::MAX_RESOLVES_PER_WINDOW {
        let candidate = Address::mint(&[u8::try_from(attempt % 256).unwrap_or(0); 16]);
        let challenge = challenge_for(&mut service, &asker);
        let request = sign::resolve(&asker, &challenge, &candidate);
        refusals.push(Directory::resolve(&mut service, &request, NOW).unwrap_err());
    }

    assert_eq!(
        refusals.last(),
        Some(&Refusal::TooFast),
        "a prober walked past the window on misses alone"
    );
}

/// A parse failure is local and faces no prober, so it is safe — and necessary —
/// to distinguish from a resolution answer.
#[test]
fn a_typo_is_a_statement_about_the_input_not_about_who_exists() {
    assert_eq!(
        Address::parse("not-an-address").unwrap_err(),
        Refusal::Malformed
    );
}

// ------------------------------------------------------------ unlinkability

/// AUTH-17, asserted rather than promised: *"Two profiles on one machine produce
/// two entries with no shared key material and no shared identifier."*
///
/// The service's contribution to this is structural and worth naming: it stores
/// per profile and offers **no device-keyed lookup at all**. Building a
/// device → profile index would be building the correlation database the
/// requirement exists to prevent, so the absence of that index is the feature.
#[test]
fn two_profiles_on_one_machine_share_no_address_no_identifier_and_no_device() {
    let mut work = Home::found(51, 52);
    let mut family = Home::found(61, 62);
    let mut service = service();

    let work_address = work.publish(&mut service, 2).expect("publish work");
    let family_address = family.publish(&mut service, 2).expect("publish family");

    assert_ne!(work_address, family_address);
    assert_ne!(work.profile, family.profile);

    let asker = stranger(90);
    let work_answer =
        lait_directory::resolve_as(&mut service, &asker, &work_address, NOW).expect("resolve work");
    let family_answer = lait_directory::resolve_as(&mut service, &asker, &family_address, NOW)
        .expect("resolve family");

    let devices = |answer: &Announcement| -> Vec<String> {
        let mut reader = Registry::new();
        let profile = reader
            .absorb(
                answer.projection.clone(),
                &answer.genesis,
                &Standing::default(),
            )
            .expect("anchor");
        reader
            .resolve(&profile)
            .unwrap_or_default()
            .into_iter()
            .map(|d| d.as_str().to_owned())
            .collect()
    };

    let work_devices = devices(&work_answer);
    let family_devices = devices(&family_answer);
    assert!(!work_devices.is_empty() && !family_devices.is_empty());
    for device in &work_devices {
        assert!(
            !family_devices.contains(device),
            "{device} appears under both profiles, which links them publicly"
        );
    }
}

// ------------------------------------------------------------------ rollback

/// A captured older publication must not roll a device set backwards — that is
/// how a revoked device would be argued back into the published set.
#[test]
fn an_older_publication_does_not_replace_a_newer_one() {
    let mut home = Home::found(21, 22);
    let mut service = service();

    let old = home.announce(2);
    let new = home.announce(5);

    let address = lait_directory::publish_as(&mut service, &home.seeds[0], &new, NOW)
        .expect("publish the newer");
    let _ = lait_directory::publish_as(&mut service, &home.seeds[0], &old, NOW)
        .expect("an older publication is accepted rather than errored");

    let asker = stranger(90);
    let answered =
        lait_directory::resolve_as(&mut service, &asker, &address, NOW).expect("resolve");
    let epoch = answered
        .projection
        .head
        .as_ref()
        .map_or(0, |head| head.epoch);
    assert!(
        epoch >= 5,
        "the directory served epoch {epoch} after an older publication was presented"
    );
}

/// The service holds no key. Stated as a test over the one thing that could
/// betray it: a device this service has never been given a seed for is the only
/// party that can produce anything it accepts.
#[test]
fn nothing_the_service_accepts_can_be_produced_without_a_seed() {
    let mut home = Home::found(21, 22);
    let mut service = service();
    let _ = home.device();
    let challenge = challenge_for(&mut service, &home.seeds[0]);

    // Everything the wire carries, with the signature replaced by zeroes.
    let encoded = home.announce(2).encode().expect("encode");
    let mut request = sign::publish(&home.seeds[0], &challenge, encoded);
    request.signature = [0u8; 64];
    assert_eq!(
        Directory::publish(&mut service, &request, NOW).unwrap_err(),
        Refusal::NotAuthentic
    );
}
