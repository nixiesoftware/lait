//! The correspondence plane: reach a person, over the real substrate.
//!
//! This composes the three pieces reach depends on into the operations a client
//! performs, with nothing mocked between them:
//!
//! - the kinship [`Registry`] — *who is this profile, which devices is it now*,
//! - the [`correspondence`] letter and mailbox — sealed, signed carriage,
//! - a [`Carrier`] — the store that holds a letter until the recipient is there.
//!
//! The [`DemoCarrier`](crate::client::correspondence::DemoCarrier) fixture stands
//! in for this while the daemon does not yet hand the client a real seed and
//! actor plane. This is the shape the daemon plugs into: give it the identity's
//! device seeds and the plane does the rest. It is proven here against a real
//! carrier without a daemon — two planes reach each other, no Space in common.
//!
//! Reach is the whole point, so the plane is organised around it:
//! [`ReachPlane::announce`] is how a profile makes its devices reachable to a
//! reader; [`ReachPlane::learn`] is how a correspondent takes that in, anchored;
//! [`ReachPlane::send`] resolves and seals; [`ReachPlane::collect`] fetches and
//! opens. Everything a surface shows is downstream of these four.

use addressbook::{Registry, RegistryError};
use correspondence::{Carrier, Content, Letter, Mailbox, Missed, Refused};
use mechanics::actor::{
    self, consent_sign, device_from_seed, sign_event, ActorOp, ConsentCtx, SignedEvent,
};
use mechanics::egress;
use mechanics::ids::{ActorId, DeviceId, SpaceId, SystemUlidSource};
use mechanics::kinship::{Audience, DeviceLink, Entry, ProfileId, Projection, Standing};

/// How long a letter is worth holding, from when it is sent.
const RETENTION: u64 = 60 * 60 * 24 * 7;

/// Why a plane operation did not apply.
#[derive(Debug)]
pub enum ReachError {
    /// The plane needs at least two device seeds to found a profile — a device
    /// set is assembled by mutual link, and a single device cannot link to
    /// itself.
    TooFewDevices,
    /// The recipient profile is not held, or resolves to no device — there is
    /// nothing to seal to. Never rendered as "the message failed"; it is "we do
    /// not know how to reach them yet".
    NotReachable,
    /// The kinship layer refused.
    Kinship(RegistryError),
    /// The carrier refused.
    Carrier(Refused),
    /// A letter could not be sealed.
    Seal(Refused),
}

impl From<RegistryError> for ReachError {
    fn from(error: RegistryError) -> Self {
        Self::Kinship(error)
    }
}

/// What a profile hands a correspondent so they can reach it: the projection to
/// draw the device set from, and the genesis that anchors it.
#[derive(Debug, Clone)]
pub struct Announcement {
    pub profile: ProfileId,
    pub genesis: DeviceLink,
    pub projection: Projection,
}

/// One identity's correspondence plane.
pub struct ReachPlane {
    /// This identity's device seeds. The first is primary — it composes letters
    /// and proves egress; all of them collect and open.
    seeds: Vec<[u8; 32]>,
    profile: ProfileId,
    genesis: DeviceLink,
    registry: Registry,
    // The egress-proving actor plane. The daemon supplies the identity's real
    // one; a self-incepted single-device plane stands in here.
    egress_space: SpaceId,
    egress_actor: ActorId,
    egress_events: Vec<SignedEvent>,
    mailbox: Mailbox,
    epoch: u64,
}

impl ReachPlane {
    /// Found this identity's profile from its device seeds and stand up the
    /// plane. At least two seeds are required — the genesis is a mutual link
    /// between the first two, and any further seed joins by another link.
    ///
    /// `_now` is reserved: the genesis carries a fixed epoch today, and the
    /// daemon-backed path will stamp it from the wall clock.
    pub fn found(seeds: Vec<[u8; 32]>, _now: u64) -> Result<Self, ReachError> {
        if seeds.len() < 2 {
            return Err(ReachError::TooFewDevices);
        }
        let genesis = DeviceLink::seal(&seeds[0], &seeds[1], [7u8; 16], 1)
            .map_err(|e| ReachError::Kinship(RegistryError::Kinship(e)))?;
        let mut registry = Registry::new();
        let profile = registry.found(genesis.clone())?;
        for (index, seed) in seeds.iter().enumerate().skip(2) {
            let link = DeviceLink::seal(&seeds[0], seed, [7u8; 16], 1 + index as u64)
                .map_err(|e| ReachError::Kinship(RegistryError::Kinship(e)))?;
            registry.extend(&profile, Entry::Link(link))?;
        }

        let egress_space = SpaceId::mint(&SystemUlidSource);
        let (egress_events, egress_actor) = incept(&seeds[0], 1, &egress_space);

        Ok(Self {
            seeds,
            profile,
            genesis,
            registry,
            egress_space,
            egress_actor,
            egress_events,
            mailbox: Mailbox::new(),
            epoch: 1,
        })
    }

    /// This identity's own profile — the address a correspondent reaches it by.
    #[must_use]
    pub fn profile(&self) -> &ProfileId {
        &self.profile
    }

    /// The standing a correspondent presents to be reached: this identity named
    /// by its primary device. Hand it to a correspondent's [`ReachPlane::announce`]
    /// so they can project for you.
    #[must_use]
    pub fn standing(&self) -> Standing {
        Standing {
            device: Some(device_from_seed(&self.seeds[0])),
            ..Standing::default()
        }
    }

    /// Make this identity's devices reachable to `audience`, projected for
    /// `reader`, and hand back what a correspondent needs to learn it.
    pub fn announce(
        &mut self,
        audience: Audience,
        reader: &Standing,
    ) -> Result<Announcement, ReachError> {
        self.epoch = self.epoch.saturating_add(1);
        let nonce = [u8::try_from(self.epoch & 0xff).unwrap_or(0); 16];
        self.registry
            .avow_reachable(&self.profile, audience, &self.seeds[0], self.epoch, nonce)?;
        let projection =
            self.registry
                .project(&self.profile, &self.seeds[0], self.epoch, reader)?;
        Ok(Announcement {
            profile: self.profile.clone(),
            genesis: self.genesis.clone(),
            projection,
        })
    }

    /// Learn a correspondent's profile from their announcement, anchored to its
    /// genesis. `reader` is this identity's own standing toward them.
    pub fn learn(
        &mut self,
        announcement: Announcement,
        reader: &Standing,
    ) -> Result<ProfileId, ReachError> {
        Ok(self
            .registry
            .absorb(announcement.projection, &announcement.genesis, reader)?)
    }

    /// Which devices a held profile resolves to — mine, or a correspondent's.
    #[must_use]
    pub fn resolve(&self, profile: &ProfileId) -> Option<Vec<DeviceId>> {
        self.registry.resolve(profile)
    }

    /// This identity's own devices, as the resolver sees them.
    #[must_use]
    pub fn my_devices(&self) -> Vec<DeviceId> {
        self.registry.resolve(&self.profile).unwrap_or_default()
    }

    /// The seed behind one of this identity's own devices, if it holds it. What a
    /// per-device carrier signer needs to authorize a fetch on that device.
    #[must_use]
    pub fn seed_for(&self, device: &DeviceId) -> Option<[u8; 32]> {
        self.seeds
            .iter()
            .find(|seed| &device_from_seed(seed) == device)
            .copied()
    }

    /// Seal a message to a resolved recipient and deposit it at the carrier.
    ///
    /// Resolution is the reach: without a device set for `recipient`, there is
    /// nothing to seal to, and the plane says so rather than pretending. One
    /// deposit reaches the addressed device today; CORR-28's multi-reader rework
    /// collapses the set to one envelope any of the recipient's devices fetches.
    pub fn send(
        &self,
        carrier: &mut impl Carrier,
        recipient: &ProfileId,
        body: &str,
        now: u64,
    ) -> Result<String, ReachError> {
        let devices = self.resolve(recipient).ok_or(ReachError::NotReachable)?;
        let addressed = devices.first().ok_or(ReachError::NotReachable)?.clone();
        self.send_addressed(carrier, recipient, &addressed, body, now)
    }

    /// Seal to a recipient, keyed at a chosen one of their devices.
    ///
    /// The carrier keys one recipient device today, so the caller picks which —
    /// and a hosted carrier fences a deposit's signer to the *sender's* egress
    /// device, so a self-message must be addressed at the sender's own device
    /// for the signer, the egress, and the later fetch to agree.
    pub fn send_addressed(
        &self,
        carrier: &mut impl Carrier,
        recipient: &ProfileId,
        addressed: &DeviceId,
        body: &str,
        now: u64,
    ) -> Result<String, ReachError> {
        let devices = self.resolve(recipient).ok_or(ReachError::NotReachable)?;
        if !devices.contains(addressed) {
            return Err(ReachError::NotReachable);
        }
        let letter = Letter::compose(
            &self.seeds[0],
            Content::Message {
                body: body.to_owned(),
            },
            now,
        );
        let sealed = letter
            .seal_to_devices(&devices, addressed, now + RETENTION)
            .map_err(ReachError::Seal)?;
        let plane = actor::replay(&self.egress_space, &self.egress_events);
        let witness = egress::authorize(
            &plane,
            &self.egress_actor,
            &device_from_seed(&self.seeds[0]),
        )
        .map_err(|_| ReachError::NotReachable)?;
        carrier
            .deposit(&witness, &sealed, now)
            .map_err(ReachError::Carrier)
    }

    /// Collect on exactly one device, with the seed that opens for it — what a
    /// hosted, per-device-signed carrier needs: one device, one signer.
    pub fn collect_on(
        &mut self,
        carrier: &mut impl Carrier,
        device: &DeviceId,
        seed: &[u8; 32],
        now: u64,
    ) -> usize {
        if let Missed::Held(waiting) = carrier.collect(device, now) {
            self.mailbox.ingest(seed, device, &waiting)
        } else {
            0
        }
    }

    /// Collect anything waiting on any of this identity's devices, open it, and
    /// file it. Returns how many were newly filed.
    ///
    /// Every device is asked because a sender addresses whichever the resolution
    /// named, and this identity does not know in advance which; the mailbox
    /// dedups, so asking them all is safe.
    pub fn collect(&mut self, carrier: &mut impl Carrier, now: u64) -> usize {
        let mut filed = 0usize;
        for seed in &self.seeds {
            let device = device_from_seed(seed);
            if let Missed::Held(waiting) = carrier.collect(&device, now) {
                filed = filed.saturating_add(self.mailbox.ingest(seed, &device, &waiting));
            }
        }
        filed
    }

    /// The messages this identity has opened, as (proven sender device, body).
    #[must_use]
    pub fn messages(&self) -> Vec<(DeviceId, String)> {
        self.mailbox
            .letters()
            .into_iter()
            .filter_map(|received| match &received.letter.content {
                Content::Message { body } => Some((received.letter.from.clone(), body.clone())),
                Content::Invitation { .. } => None,
            })
            .collect()
    }

    /// The opened text messages in the richest form a surface draws:
    /// (proven sender device, body, when it was written, whether the carrier's
    /// word matched the proof).
    #[must_use]
    pub fn inbox(&self) -> Vec<(DeviceId, String, u64, bool)> {
        self.mailbox
            .letters()
            .into_iter()
            .filter_map(|received| match &received.letter.content {
                Content::Message { body } => Some((
                    received.letter.from.clone(),
                    body.clone(),
                    received.letter.sent_at,
                    received.provenance_agrees(),
                )),
                Content::Invitation { .. } => None,
            })
            .collect()
    }
}

/// The default hosted Post — The Foundation's, unless `LAIT_POST_URL` overrides.
pub const DEFAULT_POST_URL: &str = "https://post.foundation.pub";

/// The client's live correspondence over a hosted Post.
///
/// Wraps a [`ReachPlane`] and a hosted carrier's base URL, and runs the real
/// carriage over HTTP. v1 proves the whole pipe against real infrastructure the
/// one way that needs no directory: a person reaches **themselves** — seals to
/// their own profile's devices, deposits over the Post, and fetches it back.
/// Reaching another person is the same `send`/`collect` once their profile is
/// learned (announce/learn), which the directory (AUTH-12) will carry.
///
/// Client-direct: the person's own device seed signs the person's own mail on
/// the person's own device. The `Carrier` seam is a `PostCarrier`, so nothing
/// about the plane changed — only where the letter lands.
pub struct PostReach {
    plane: ReachPlane,
    base: String,
}

impl PostReach {
    /// Stand up the plane from this identity's device seeds, pointed at a Post.
    pub fn found(seeds: Vec<[u8; 32]>, base: String, now: u64) -> Result<Self, ReachError> {
        Ok(Self {
            plane: ReachPlane::found(seeds, now)?,
            base,
        })
    }

    /// This identity's own profile — the address it is reached by.
    #[must_use]
    pub fn profile(&self) -> &ProfileId {
        self.plane.profile()
    }

    /// This identity's primary device — the one it composes, signs and collects
    /// on over the hosted Post.
    #[must_use]
    pub fn my_device(&self) -> DeviceId {
        device_from_seed(&self.plane.seeds[0])
    }

    /// The opened text messages, richest form. See [`ReachPlane::inbox`].
    #[must_use]
    pub fn inbox(&self) -> Vec<(DeviceId, String, u64, bool)> {
        self.plane.inbox()
    }

    /// Seal a message to *yourself* and deposit it over the hosted Post.
    ///
    /// Addressed at the primary device — the same device the egress authorizes
    /// and the same one `collect` fetches on — so signer, sender, and reader all
    /// agree under the hosted carrier's custody fence.
    pub fn send_self(&self, body: &str, now: u64) -> Result<String, ReachError> {
        use correspondence::post::{PostCarrier, Signer};
        let seed = self.plane.seeds[0];
        let primary = device_from_seed(&seed);
        let mut carrier = PostCarrier::new(self.base.clone(), Signer::new(seed));
        let profile = self.plane.profile().clone();
        self.plane
            .send_addressed(&mut carrier, &profile, &primary, body, now)
    }

    /// Fetch anything waiting for you over the hosted Post, open it, and file it.
    /// Returns how many were newly filed. Asks the primary device — the one a
    /// self-message is addressed and signed for.
    pub fn collect(&mut self, now: u64) -> usize {
        use correspondence::post::{PostCarrier, Signer};
        let seed = self.plane.seeds[0];
        let primary = device_from_seed(&seed);
        let mut carrier = PostCarrier::new(self.base.clone(), Signer::new(seed));
        self.plane.collect_on(&mut carrier, &primary, &seed, now)
    }

    /// The messages this identity has opened, as (proven sender device, body).
    #[must_use]
    pub fn messages(&self) -> Vec<(DeviceId, String)> {
        self.plane.messages()
    }
}

/// Incept a single-device actor so `egress` has a real device→actor binding to
/// resolve — the same shape the correspondence crate's tests use. The daemon
/// supplies the identity's real actor plane in place of this.
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

#[cfg(test)]
mod tests {
    use super::*;
    use correspondence::MemCarrier;
    use mechanics::kinship::{Audience, Party};

    const ALICE_A: [u8; 32] = [11u8; 32];
    const ALICE_B: [u8; 32] = [12u8; 32];
    const BOB_A: [u8; 32] = [40u8; 32];
    const BOB_B: [u8; 32] = [41u8; 32];
    const NOW: u64 = 1_800_000_000;

    /// The client's own plane reaches over the **deployed** Post, when one is
    /// pointed at by `POST_SMOKE_URL` (e.g. `https://post.foundation.pub`).
    /// Skipped when unset so the offline suite never depends on the network.
    #[test]
    fn a_plane_reaches_over_the_deployed_post() {
        let Ok(base) = std::env::var("POST_SMOKE_URL") else {
            return;
        };
        use correspondence::post::{PostCarrier, Signer};
        let base = base.trim_end_matches('/').to_owned();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs();

        // Unique seeds per run, so this test's mailbox is its own and the
        // persistent Post never crosses one run's assertions with another's.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
            .to_le_bytes();
        let mk = |tag: u8| {
            let mut s = [0u8; 32];
            s[..16].copy_from_slice(&stamp);
            s[16] = tag;
            s
        };
        let (a0, a1, b0, b1) = (mk(1), mk(2), mk(3), mk(4));

        let mut alice = ReachPlane::found(vec![a0, a1], now).expect("alice");
        let mut bob = ReachPlane::found(vec![b0, b1], now).expect("bob");

        let to_bob = Audience::Correspondent(Party::Device(device_from_seed(&b0)));
        let announcement = alice.announce(to_bob, &bob.standing()).expect("announce");
        bob.learn(announcement, &bob.standing()).expect("learn");

        // Bob sends to Alice over the deployed carrier; Alice collects it there.
        let mut carrier = PostCarrier::new(base, Signer::new(b0));
        bob.send(&mut carrier, &alice.profile().clone(), "over the wire", now)
            .expect("deposit over HTTP");

        let mut alice_carrier = PostCarrier::new(
            std::env::var("POST_SMOKE_URL")
                .unwrap()
                .trim_end_matches('/')
                .to_owned(),
            Signer::new(a0),
        );
        let filed = alice.collect(&mut alice_carrier, now);
        assert_eq!(
            filed, 1,
            "Alice fetched Bob's letter from the deployed Post"
        );
        assert_eq!(alice.messages()[0].1, "over the wire");
    }

    /// Two planes, no Space in common, reach each other over one carrier: Alice
    /// announces to Bob, Bob learns her and sends, Alice collects and reads it.
    #[test]
    fn two_planes_reach_each_other_over_a_carrier() {
        let mut alice = ReachPlane::found(vec![ALICE_A, ALICE_B], NOW).expect("alice");
        let mut bob = ReachPlane::found(vec![BOB_A, BOB_B], NOW).expect("bob");

        // Alice makes herself reachable to Bob; Bob learns her, anchored.
        let to_bob = Audience::Correspondent(Party::Device(device_from_seed(&BOB_A)));
        let announcement = alice.announce(to_bob, &bob.standing()).expect("announce");
        let learned = bob.learn(announcement, &bob.standing()).expect("learn");
        assert_eq!(&learned, alice.profile());

        // Bob resolves Alice and sends.
        assert!(bob.resolve(alice.profile()).is_some_and(|d| d.len() == 2));
        let mut carrier = MemCarrier::new();
        bob.send(&mut carrier, &alice.profile().clone(), "reached you", NOW)
            .expect("send");

        // Alice was elsewhere; now she collects and opens it.
        let filed = alice.collect(&mut carrier, NOW + 10);
        assert_eq!(filed, 1);
        let messages = alice.messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].0, device_from_seed(&BOB_A), "proven from Bob");
        assert_eq!(messages[0].1, "reached you");
    }

    /// Sending to a profile the plane has never learned is `NotReachable`, never
    /// a silent success.
    #[test]
    fn sending_to_an_unlearned_profile_is_not_reachable() {
        let alice = ReachPlane::found(vec![ALICE_A, ALICE_B], NOW).expect("alice");
        let stranger = ReachPlane::found(vec![BOB_A, BOB_B], NOW).expect("bob");
        let mut carrier = MemCarrier::new();
        assert!(matches!(
            alice.send(&mut carrier, &stranger.profile().clone(), "hi", NOW),
            Err(ReachError::NotReachable)
        ));
    }

    /// The live backend round-trips a message to yourself over the **deployed**
    /// Post, when `POST_SMOKE_URL` points at one. This is the real client path:
    /// seal, deposit over HTTP, fetch back, open.
    #[test]
    fn post_reach_self_round_trips_over_the_deployed_post() {
        let Ok(base) = std::env::var("POST_SMOKE_URL") else {
            return;
        };
        let base = base.trim_end_matches('/').to_owned();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        // Per-run seeds, so the persistent Post never crosses runs.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
            .to_le_bytes();
        let mk = |tag: u8| {
            let mut s = [0u8; 32];
            s[..16].copy_from_slice(&stamp);
            s[16] = tag;
            s
        };

        let mut me = PostReach::found(vec![mk(1), mk(2)], base, now).expect("found");
        me.send_self("a note to future me", now).expect("send self");
        let filed = me.collect(now);
        assert_eq!(filed, 1, "the note came back over the deployed Post");
        assert_eq!(me.messages()[0].1, "a note to future me");
    }

    /// A single device cannot found a profile — a set is a mutual link.
    #[test]
    fn one_device_cannot_found_a_profile() {
        assert!(matches!(
            ReachPlane::found(vec![ALICE_A], NOW),
            Err(ReachError::TooFewDevices)
        ));
    }
}
