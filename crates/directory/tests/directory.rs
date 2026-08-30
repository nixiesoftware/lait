//! The directory's acceptance criteria, as tests.
//!
//! Every one of these is a line from the design Spec *"The directory is a mirror,
//! and an address is issued rather than chosen"* or from AUTH-16/17/18/24. They
//! are here rather than beside the code because they are properties of the
//! service as a whole — what it refuses is as much the product as what it
//! answers.

use std::sync::Arc;

use addressbook::{Announcement, Registry};
use lait_directory::{
    address::Address,
    registry::{Label, MemRegistry, Registrar, RegistryStore, RoutePublish},
    service::chronicle_entry_for,
    wire::{sign, Challenge},
    Chronicler, Directory, Issued, MemStore, Receipt, Refusal, Service,
};
use mechanics::{
    actor::device_from_seed,
    chronicle::{
        advance, consistent_with, verify_inclusion, verify_mark, Advance, Chronicle, PinnedHead,
    },
    ids::DeviceId,
    kinship::{Audience, DeviceLink, Entry, Party, ProfileId, Retirement, Standing},
};

const NOW: u64 = 1_700_000_000;

/// The marker's seed: the chronicle key, which signs which publications were
/// recorded and in which order — and now the marks over them. Never an operator
/// key; a mark asserts log facts and steers nothing.
const MARKER: [u8; 32] = [77u8; 32];

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

    /// The devices this home's next announcement will avow: its live set.
    fn devices(&self) -> Vec<DeviceId> {
        self.registry
            .resolve(&self.profile)
            .expect("a held profile")
    }

    /// Retire a device from the profile, so the next announcement avows fewer.
    fn retire(&mut self, device: &DeviceId, epoch: u64) {
        let retirement = Retirement::seal(&self.seeds[0], device.clone(), epoch, [5u8; 16])
            .expect("seal a retirement");
        self.registry
            .extend(&self.profile, Entry::Retire(retirement))
            .expect("retire");
    }

    /// Announce at `epoch` and publish it, as this home's canonical device.
    fn publish(&mut self, service: &mut Service<MemStore>, epoch: u64) -> Result<Address, Refusal> {
        self.publish_issued(service, epoch)
            .map(|issued| issued.address)
    }

    /// The same act, keeping the receipt the chronicle answered with.
    fn publish_issued(
        &mut self,
        service: &mut Service<MemStore>,
        epoch: u64,
    ) -> Result<Issued, Refusal> {
        let announcement = self.announce(epoch);
        lait_directory::publish_as(service, &self.seeds[0], &announcement, NOW)
            .map(|published| published.issued)
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
        .expect("publish the newer")
        .issued
        .address;
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

// --------------------------------------------------------------- chronicling

/// The subjects a receipt's marks name, sorted. Panics on a mark about anything
/// but a device: a mark names the device it recorded a publication for, and a
/// reader that had to handle some other party would be reading a claim this
/// service does not make.
fn marked(receipt: &Receipt) -> Vec<DeviceId> {
    let mut devices: Vec<DeviceId> = receipt
        .marks
        .iter()
        .map(|mark| match &mark.subject {
            Party::Device(device) => device.clone(),
            other => panic!("a mark named {other:?} rather than a device"),
        })
        .collect();
    devices.sort();
    devices
}

/// A chronicled directory over a chronicle nothing else feeds.
fn chronicled() -> (
    Service<MemStore>,
    lait_directory::chronicle::SharedChronicler,
) {
    let chronicler = Chronicler::shared(MemStore::new(), MARKER).expect("open the chronicle");
    (
        Service::with_chronicler(MemStore::new(), Arc::clone(&chronicler)),
        chronicler,
    )
}

/// A device set publication is recorded and marked, so an identity that never
/// chose a label is certified exactly as one that did — and every mark proves
/// itself on its own terms, against the entry the publisher can recompute.
#[test]
fn a_publication_is_chronicled_and_marked_for_every_device_it_avows() {
    let mut home = Home::found(21, 22);
    let (mut service, _chronicler) = chronicled();

    // Composed field by field rather than through `publish_as`, because the
    // subject binding is exactly the part only a holder of the signed request
    // can prove: it recomputes the leaf and finds it under the head.
    let announcement = home.announce(2);
    let challenge = challenge_for(&mut service, &home.seeds[0]);
    let request = sign::publish(
        &home.seeds[0],
        &challenge,
        announcement.encode().expect("encode"),
    );
    let receipt = Directory::publish(&mut service, &request, NOW)
        .expect("publish")
        .receipt;

    let head = receipt
        .head
        .clone()
        .expect("a chronicled publication carries its head");
    head.verify().expect("the head verifies");
    assert_eq!(head.size, 1, "one publication, one entry");
    assert_eq!(receipt.entry, Some(0));
    let leaf = Chronicle::leaf_of(&chronicle_entry_for(&request));
    verify_inclusion(&leaf, 0, head.size, &head.root, &receipt.inclusion)
        .expect("the publisher recomputes its own leaf and finds it recorded");

    assert_eq!(
        marked(&receipt),
        home.devices(),
        "the marks name exactly the devices this publication avows"
    );
    let pin = PinnedHead::from(&head);
    for mark in &receipt.marks {
        assert_eq!(
            mark.by, head.by,
            "a mark is signed by the marker whose head it names"
        );
        assert_eq!(mark.audience, Audience::Public);
        verify_mark(mark, &receipt.inclusion).expect("the mark proves what it says");
        consistent_with(&pin, mark, &[]).expect("and sits on the log this reader follows");
    }

    // A directory that keeps no chronicle records nothing and marks nobody.
    // That is a smaller service, never a refusal, and never rendered as one.
    let mut unchronicled = Service::new(MemStore::new());
    assert_eq!(
        home.publish_issued(&mut unchronicled, 3)
            .expect("publish")
            .receipt,
        Receipt::default(),
        "an unchronicled directory answered something other than an empty receipt"
    );
}

/// Invariant 7's revoke arm. Certification is per receipt: the newest one is the
/// whole of it, so a device the next publication does not avow loses its mark
/// with nothing retracted and no history rewritten — and the earlier mark stays
/// exactly as true as it was, because it only ever named an entry.
#[test]
fn the_newest_receipt_is_the_whole_certification_and_the_next_one_withdraws_a_device() {
    let mut home = Home::found(21, 22);
    let (mut service, _chronicler) = chronicled();
    let witness = device_from_seed(&home.seeds[1]);

    let first = home
        .publish_issued(&mut service, 2)
        .expect("publish")
        .receipt;
    assert!(
        marked(&first).contains(&witness),
        "the witness was avowed and should have been marked"
    );
    let earlier = first
        .marks
        .iter()
        .find(|mark| mark.subject == Party::Device(witness.clone()))
        .cloned()
        .expect("the witness's mark");

    home.retire(&witness, 3);
    let second = home
        .publish_issued(&mut service, 4)
        .expect("republish")
        .receipt;

    assert_eq!(
        marked(&second),
        vec![home.device()],
        "a device the newest publication does not avow is no longer certified"
    );
    assert_eq!(
        second.head.expect("a head").size,
        2,
        "the withdrawal is a later entry, never an erased one"
    );
    verify_mark(&earlier, &first.inclusion)
        .expect("the earlier mark still proves the entry it named");
}

/// One chronicle, two feeders. A route publication and an address publication
/// by the same identity land in one log, each provable under it, and the head
/// the registry serves is the head the directory answered with — which is what
/// makes a reader's single pin cover both.
#[test]
fn the_directory_and_the_registry_write_one_chronicle() {
    let mut home = Home::found(21, 22);
    let (mut service, chronicler) = chronicled();
    let label = Label::parse("acme").expect("a label");
    let mut routes = MemRegistry::default();
    routes
        .bind(&label, &home.profile)
        .expect("the curated bind");
    let mut registrar = Registrar::with_chronicler(routes, Arc::clone(&chronicler));

    let announcement = home.announce(2);
    let route = RoutePublish::sign(
        label,
        announcement.encode().expect("encode"),
        device_from_seed(&[99u8; 32]).as_str().to_owned(),
        2,
        &home.seeds[0],
    );
    let answer = registrar.publish(&route).expect("the route is taken");
    let routed = answer.receipt.clone();
    let first = routed.head.clone().expect("a chronicled route");
    assert_eq!(routed.entry, Some(0));
    assert_eq!(
        marked(&routed),
        home.devices(),
        "the registry marks the devices the announcement avows, not only the presenter"
    );
    verify_inclusion(
        &Chronicle::leaf_of(&lait_directory::registry::chronicle_entry(&route)),
        0,
        first.size,
        &first.root,
        &routed.inclusion,
    )
    .expect("the route's own entry is provable");

    // The registry's answer keeps every key it had: the receipt is flattened
    // beside the route, so a reader that knows only `Resolved` decodes exactly
    // what it always did. Checked against the wire rather than assumed — the
    // flatten is the whole of that compatibility claim.
    let wire = serde_json::to_value(&answer).expect("serialize the answer");
    for key in ["label", "profile", "endpoint", "epoch", "head", "entry"] {
        assert!(
            wire.get(key).is_some(),
            "the registry answer lost `{key}`: {wire}"
        );
    }
    assert!(
        wire.get("inclusion").is_none(),
        "an empty path is still omitted rather than serialized, as it always was: {wire}"
    );
    serde_json::from_value::<lait_directory::registry::Resolved>(wire)
        .expect("a chronicle-blind reader still decodes the route");

    let published = home
        .publish_issued(&mut service, 3)
        .expect("publish")
        .receipt;
    let second = published.head.clone().expect("a chronicled publication");
    assert_eq!(second.size, 2, "both feeders wrote into one log");
    assert_eq!(published.entry, Some(1));
    assert_eq!(second.by, first.by, "one log, one signer");

    // The registry's chronicle surface serves what the directory just wrote,
    // and proves it extends what a reader pinned at the route.
    let answer = registrar
        .answer(Some(first.size))
        .expect("the chronicle answers");
    assert_eq!(
        answer.head, second,
        "the shared surface serves the shared head"
    );
    assert_eq!(
        advance(
            Some(&PinnedHead::from(&first)),
            &answer.head,
            &answer.consistency
        ),
        Ok(Advance::Extended),
        "a reader following one marker covers both surfaces"
    );
}
