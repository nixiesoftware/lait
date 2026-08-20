//! The reach plane: turning a person into devices you can seal to.
//!
//! Substrate rather than client code, and the layering says why: this plane
//! serves issue notifications, invitation delivery, agent messages and a chat
//! client, and every one of those is merely a caller. It lived in the desktop
//! app, which made a World unable to send at all and made mail arrive only while
//! a window was open.
//!
//! It composes the three pieces reach depends on, with nothing mocked between:
//!
//! - the kinship [`Registry`] — *who is this profile, which devices is it now*,
//! - the letter and mailbox — sealed, signed carriage,
//! - a [`Carrier`] — the store that holds a letter until the recipient is there.
//!
//! [`ReachPlane::announce`] makes a profile's devices reachable to a reader;
//! [`ReachPlane::learn`] takes that in, anchored; [`ReachPlane::send`] resolves
//! and seals; [`ReachPlane::collect`] fetches and opens. Everything a surface
//! shows is downstream of those four.

use addressbook::{registry, Announcement, Registry};
use mechanics::actor::{
    self, consent_sign, device_from_seed, sign_event, ActorOp, ConsentCtx, SignedEvent,
};
use mechanics::egress;
use mechanics::ids::{ActorId, DeviceId, SpaceId, SystemUlidSource};
use mechanics::kinship::{Audience, DeviceLink, Entry, ProfileId, Projection, Standing};

use crate::{Carrier, Content, Letter, Mailbox, Missed, Refused};

/// How long a letter is worth holding, from when it is sent.
const RETENTION: u64 = 60 * 60 * 24 * 7;

/// The genesis link's nonce and epoch. Fixed so a profile id is reproducible
/// from its seeds; see [`ReachPlane::found`].
const GENESIS_NONCE: [u8; 16] = [7u8; 16];
const GENESIS_EPOCH: u64 = 1;

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
    Kinship(registry::Failure),
    /// The carrier refused.
    Carrier(Refused),
    /// A letter could not be sealed.
    Seal(Refused),
    /// This identity could not prove its own key. Nothing to do with whether the
    /// recipient is reachable — reporting it as `NotReachable` would blame them
    /// for a fault on this side.
    Egress(String),
}

impl std::fmt::Display for ReachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewDevices => write!(f, "a profile needs two devices to be founded"),
            Self::NotReachable => write!(f, "we do not know how to reach them yet"),
            Self::Kinship(failure) => write!(f, "the kinship plane refused: {failure:?}"),
            Self::Carrier(refused) => write!(f, "the carrier refused: {refused:?}"),
            Self::Seal(refused) => write!(f, "the letter could not be sealed: {refused:?}"),
            Self::Egress(why) => write!(f, "this device could not prove its own key: {why}"),
        }
    }
}

impl From<registry::Failure> for ReachError {
    fn from(error: registry::Failure) -> Self {
        Self::Kinship(error)
    }
}

/// One letter this identity has opened and verified.
///
/// Every kind, not only the ones a transcript knows how to draw — dropping an
/// invitation here is what made it invisible to the hosted arm while the
/// component to draw it already existed.
#[derive(Debug, Clone)]
pub struct Opened {
    /// The carrier's deposit id. The handle an action names.
    pub id: String,
    /// The device that signed it, proven by the letter itself.
    pub from: DeviceId,
    pub sent_at: u64,
    /// Whether the carrier's word about the sender matched the proof.
    pub provenance_agrees: bool,
    pub content: Content,
}

/// What one pass over a carrier learned.
///
/// Two facts, because they are two facts. A carrier that answered and is holding
/// nothing gives `filed: 0, unasked: None`; a carrier that could not be reached
/// gives `filed: 0, unasked: Some(why)`. Collapsing those into one number is the
/// false-disconnection defect — an outage rendered as an empty mailbox — and it
/// is the reason this is a struct rather than the `usize` it used to be.
///
/// `unasked` being `Some` does not mean nothing was filed: a plane asks every
/// device it holds, and one going dark does not undo another's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collected {
    /// How many letters were newly filed by this pass.
    pub filed: usize,
    /// Why a device could not be asked, if one could not.
    pub unasked: Option<String>,
}

/// One identity's correspondence plane.
///
/// Every method that carries takes `&mut (impl Carrier + ?Sized)` rather than
/// `&mut impl Carrier`. The relaxation is the point of the contractor seam: a
/// holder that decides at run time which contractor is carrying — memory in
/// tests, a hosted Post today, a direct peer later — keeps it behind `dyn`, and
/// a `Sized` bound would refuse exactly that holder while claiming the plane is
/// indifferent to where material waits.
pub struct ReachPlane {
    /// This identity's device seeds. All of them collect and open; the
    /// **canonical** one composes letters and proves egress.
    seeds: Vec<[u8; 32]>,
    /// Which seed composes, signs and avows. Movable: `DeviceLink` is symmetric,
    /// so no device outranks another and the profile survives the handover.
    canonical: usize,
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
    pub fn found(seeds: Vec<[u8; 32]>, now: u64) -> Result<Self, ReachError> {
        Self::restore(seeds, None, now)
    }

    /// Found the plane, reusing durable state when there is any.
    ///
    /// The genesis is recomputed from the seeds either way — it is deterministic,
    /// so the profile id is the same address across restarts. What `state` adds
    /// is the correspondents this identity has learned, and the epoch its own
    /// publications have reached.
    pub fn restore(
        seeds: Vec<[u8; 32]>,
        state: Option<addressbook::ReachState>,
        _now: u64,
    ) -> Result<Self, ReachError> {
        if seeds.len() < 2 {
            return Err(ReachError::TooFewDevices);
        }
        // Named rather than indexed: the length check above is what makes the
        // pair present, and a bare index asks the reader to hold that in their
        // head at every later edit.
        let (Some(first), Some(second)) = (seeds.first(), seeds.get(1)) else {
            return Err(ReachError::TooFewDevices);
        };
        let (first, second) = (*first, *second);
        // The nonce and epoch are fixed, and that is load-bearing: the profile
        // id is the hash of this link, so the same seeds must produce the same
        // genesis on every launch or the address a person handed out stops
        // naming them. Changing either constant invalidates every issued
        // address in existence.
        let genesis = DeviceLink::seal(&first, &second, GENESIS_NONCE, GENESIS_EPOCH)
            .map_err(|e| ReachError::Kinship(registry::Failure::Kinship(e)))?;
        let (mut registry, epoch, canonical) = match state {
            Some(held) => (held.registry, held.epoch, held.canonical),
            None => (Registry::new(), 1, 0),
        };
        let profile = registry.found(genesis.clone())?;
        // Deterministic like the genesis, and for the same reason: a restore
        // re-seals the same links from the same seeds, so `Registry::extend`
        // sees entries it already holds rather than duplicates. Break that
        // determinism and every restart grows the log by one entry per device.
        for (index, seed) in seeds.iter().enumerate().skip(2) {
            let link = DeviceLink::seal(
                &first,
                seed,
                GENESIS_NONCE,
                GENESIS_EPOCH.saturating_add(u64::try_from(index).unwrap_or(u64::MAX)),
            )
            .map_err(|e| ReachError::Kinship(registry::Failure::Kinship(e)))?;
            registry.extend(&profile, Entry::Link(link))?;
        }

        let egress_space = SpaceId::mint(&SystemUlidSource);
        let (egress_events, egress_actor) = incept(&first, 1, &egress_space);

        Ok(Self {
            // Refuse rather than repair: durable state naming a device this
            // identity does not hold would otherwise start composing under a
            // different key than the state recorded, silently.
            canonical: {
                if canonical >= seeds.len() {
                    return Err(ReachError::NotReachable);
                }
                canonical
            },
            seeds,
            profile,
            genesis,
            registry,
            egress_space,
            egress_actor,
            egress_events,
            mailbox: Mailbox::new(),
            epoch,
        })
    }

    /// What has to survive a restart: the learned correspondents and the epoch.
    #[must_use]
    pub fn state(&self) -> addressbook::ReachState {
        addressbook::ReachState {
            epoch: self.epoch,
            canonical: self.canonical,
            registry: self.registry.clone(),
        }
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
            device: Some(self.canonical_device()),
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
        // Derived from the epoch rather than sampled, so a republication is
        // reproducible from durable state alone. It carries 8 bits and repeats
        // every 256 epochs, which is safe only because `Avowal`'s preimage is
        // domain-separated and already unique per (device, epoch, claim) — the
        // nonce distinguishes nothing a collision could confuse. It is not a
        // secret and must never become one; a real source belongs here the
        // moment anything depends on it being unguessable.
        let nonce = [u8::try_from(self.epoch & 0xff).unwrap_or(0); 16];
        self.registry.avow_reachable(
            &self.profile,
            audience,
            &self.canonical_seed(),
            self.epoch,
            nonce,
        )?;
        let projection =
            self.registry
                .project(&self.profile, &self.canonical_seed(), self.epoch, reader)?;
        Ok(Announcement::new(
            self.profile.clone(),
            self.genesis.clone(),
            projection,
        ))
    }

    /// This identity's current card, without publishing a new one.
    ///
    /// Projects at the epoch already reached rather than bumping it: showing
    /// your own address should not append to your log. `None` until something
    /// has been avowed, because a projection with no avowal in it names no
    /// devices and would be a card that reaches nobody.
    pub fn card(&self, reader: &Standing) -> Option<Announcement> {
        if self.epoch <= GENESIS_EPOCH {
            return None;
        }
        let projection = self
            .registry
            .project(&self.profile, &self.canonical_seed(), self.epoch, reader)
            .ok()?;
        Some(Announcement::new(
            self.profile.clone(),
            self.genesis.clone(),
            projection,
        ))
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

    /// Whose profile avows this device, among the ones this identity holds.
    ///
    /// The reverse of [`Self::resolve`], and what routes a received letter into
    /// the conversation of whoever signed it: a letter proves its sending
    /// *device*, and a person is a device set.
    #[must_use]
    pub fn profile_of_device(&self, device: &DeviceId) -> Option<ProfileId> {
        self.registry
            .profiles()
            .find(|profile| {
                self.registry
                    .resolve(profile)
                    .is_some_and(|devices| devices.contains(device))
            })
            .cloned()
    }

    /// Which devices a held profile resolves to — mine, or a correspondent's.
    #[must_use]
    pub fn resolve(&self, profile: &ProfileId) -> Option<Vec<DeviceId>> {
        self.registry.resolve(profile)
    }

    /// Every profile this identity holds, its own included.
    #[must_use]
    pub fn registry_profiles(&self) -> Vec<ProfileId> {
        self.registry.profiles().cloned().collect()
    }

    /// This identity's own devices, as the resolver sees them.
    #[must_use]
    pub fn my_devices(&self) -> Vec<DeviceId> {
        self.registry.resolve(&self.profile).unwrap_or_default()
    }

    /// The seed that composes, signs and avows for this identity.
    #[must_use]
    fn canonical_seed(&self) -> [u8; 32] {
        // `restore` refuses an index this identity does not hold, so the seed is
        // there; falling back to the first rather than panicking keeps that a
        // construction-time refusal instead of a run-time one.
        self.seeds
            .get(self.canonical)
            .or_else(|| self.seeds.first())
            .copied()
            .unwrap_or([0u8; 32])
    }

    /// The device this identity currently composes and is addressed as.
    #[must_use]
    pub fn canonical_device(&self) -> DeviceId {
        device_from_seed(&self.canonical_seed())
    }

    /// Hand the canonical role to another of this identity's devices.
    ///
    /// The profile is unchanged — it is the hash of a genesis link that names no
    /// primary. Refuses a device this identity does not hold the seed for.
    pub fn make_canonical(&mut self, device: &DeviceId) -> Result<(), ReachError> {
        let at = self
            .seeds
            .iter()
            .position(|seed| &device_from_seed(seed) == device)
            .ok_or(ReachError::NotReachable)?;
        self.canonical = at;
        Ok(())
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
        carrier: &mut (impl Carrier + ?Sized),
        recipient: &ProfileId,
        body: &str,
        now: u64,
    ) -> Result<String, ReachError> {
        self.send_content(
            carrier,
            recipient,
            Content::Message {
                body: body.to_owned(),
            },
            now,
        )
    }

    /// Seal any content to a resolved recipient. A message and an invitation
    /// travel the same way; only one of them is read.
    pub fn send_content(
        &self,
        carrier: &mut (impl Carrier + ?Sized),
        recipient: &ProfileId,
        content: Content,
        now: u64,
    ) -> Result<String, ReachError> {
        let devices = self.resolve(recipient).ok_or(ReachError::NotReachable)?;
        let addressed = devices.first().ok_or(ReachError::NotReachable)?.clone();
        self.send_addressed(carrier, recipient, &addressed, content, now)
    }

    /// Seal to a recipient, keyed at a chosen one of their devices.
    ///
    /// The carrier keys one recipient device today, so the caller picks which —
    /// and a hosted carrier fences a deposit's signer to the *sender's* egress
    /// device, so a self-message must be addressed at the sender's own device
    /// for the signer, the egress, and the later fetch to agree.
    pub fn send_addressed(
        &self,
        carrier: &mut (impl Carrier + ?Sized),
        recipient: &ProfileId,
        addressed: &DeviceId,
        content: Content,
        now: u64,
    ) -> Result<String, ReachError> {
        let devices = self.resolve(recipient).ok_or(ReachError::NotReachable)?;
        if !devices.contains(addressed) {
            return Err(ReachError::NotReachable);
        }
        let letter = Letter::compose(&self.canonical_seed(), content, now);
        let sealed = letter
            .seal_to_devices(&devices, addressed, now.saturating_add(RETENTION))
            .map_err(ReachError::Seal)?;
        let plane = actor::replay(&self.egress_space, &self.egress_events);
        let witness = egress::authorize(&plane, &self.egress_actor, &self.canonical_device())
            .map_err(|refused| ReachError::Egress(refused.to_string()))?;
        carrier
            .deposit(&witness, &sealed, now)
            .map_err(ReachError::Carrier)
    }

    /// Collect on exactly one device, with the seed that opens for it — what a
    /// hosted, per-device-signed carrier needs: one device, one signer.
    pub fn collect_on(
        &mut self,
        carrier: &mut (impl Carrier + ?Sized),
        device: &DeviceId,
        seed: &[u8; 32],
        now: u64,
    ) -> Collected {
        match carrier.collect(device, now) {
            Missed::Held(waiting) => Collected {
                filed: self.mailbox.ingest(seed, device, &waiting),
                unasked: None,
            },
            Missed::Unasked(why) => Collected {
                filed: 0,
                unasked: Some(why),
            },
        }
    }

    /// Collect anything waiting on any of this identity's devices, open it, and
    /// file it. Returns how many were newly filed.
    ///
    /// Every device is asked because a sender addresses whichever the resolution
    /// named, and this identity does not know in advance which; the mailbox
    /// dedups, so asking them all is safe.
    pub fn collect(&mut self, carrier: &mut (impl Carrier + ?Sized), now: u64) -> Collected {
        let mut collected = Collected {
            filed: 0,
            unasked: None,
        };
        for seed in &self.seeds {
            let device = device_from_seed(seed);
            match carrier.collect(&device, now) {
                Missed::Held(waiting) => {
                    collected.filed = collected
                        .filed
                        .saturating_add(self.mailbox.ingest(seed, &device, &waiting));
                }
                // One device going dark does not undo what another answered, and
                // it does not become quiet either. Keep the first reason: a run
                // of failures reads better dated from where it started.
                Missed::Unasked(why) => {
                    collected.unasked.get_or_insert(why);
                }
            }
        }
        collected
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

    /// One opened letter, as a surface draws it.
    ///
    /// Carries the deposit id because acting on an invitation means naming
    /// *which* one, and a transcript row has no other handle a person could
    /// point at.
    #[must_use]
    pub fn opened(&self) -> Vec<Opened> {
        self.mailbox
            .letters()
            .into_iter()
            .map(|received| Opened {
                id: received.id.clone(),
                from: received.letter.from.clone(),
                sent_at: received.letter.sent_at,
                provenance_agrees: received.provenance_agrees(),
                content: received.letter.content.clone(),
            })
            .collect()
    }

    /// The coordinates carried by one opened invitation, by deposit id.
    #[must_use]
    pub fn invitation(&self, id: &str) -> Option<Vec<u8>> {
        self.mailbox.letters().into_iter().find_map(|received| {
            match (&received.letter.content, received.id == id) {
                (Content::Invitation { coordinates }, true) => Some(coordinates.clone()),
                _ => None,
            }
        })
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
/// carriage over HTTP: `announce` publishes a card, `learn` takes a
/// correspondent's in, and `send_to` seals to the device set that card named.
/// A directory (AUTH-12) replaces the card with a short spoken address; the
/// carriage under it is unchanged.
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
        self.plane.canonical_device()
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
        use crate::post::{PostCarrier, Signer};
        let seed = self.plane.canonical_seed();
        let primary = self.plane.canonical_device();
        let mut carrier = PostCarrier::new(self.base.clone(), Signer::new(seed));
        let profile = self.plane.profile().clone();
        self.plane.send_addressed(
            &mut carrier,
            &profile,
            &primary,
            Content::Message {
                body: body.to_owned(),
            },
            now,
        )
    }

    /// Reuse durable state when founding the hosted plane.
    pub fn restore(
        seeds: Vec<[u8; 32]>,
        state: Option<addressbook::ReachState>,
        base: String,
        now: u64,
    ) -> Result<Self, ReachError> {
        Ok(Self {
            plane: ReachPlane::restore(seeds, state, now)?,
            base,
        })
    }

    /// What has to survive a restart.
    #[must_use]
    pub fn state(&self) -> addressbook::ReachState {
        self.plane.state()
    }

    /// Publish this identity's device set for `reader`, as an artifact they can
    /// carry. `Audience::Public` is the tier a stranger can read.
    pub fn announce(
        &mut self,
        audience: Audience,
        reader: &Standing,
    ) -> Result<Announcement, ReachError> {
        self.plane.announce(audience, reader)
    }

    /// Take in a correspondent's announcement, anchored to its genesis.
    pub fn learn(
        &mut self,
        announcement: Announcement,
        reader: &Standing,
    ) -> Result<ProfileId, ReachError> {
        self.plane.learn(announcement, reader)
    }

    /// The standing to hand a correspondent so they can project for you.
    #[must_use]
    pub fn standing(&self) -> Standing {
        self.plane.standing()
    }

    /// Whether a profile resolves to devices this identity can reach.
    #[must_use]
    pub fn resolve(&self, profile: &ProfileId) -> Option<Vec<DeviceId>> {
        self.plane.resolve(profile)
    }

    /// Whose profile avows this device, among those this identity holds.
    #[must_use]
    pub fn profile_of_device(&self, device: &DeviceId) -> Option<ProfileId> {
        self.plane.profile_of_device(device)
    }

    /// This identity's current card, rendered, without publishing a new one.
    #[must_use]
    pub fn card(&self) -> Option<String> {
        let reader = self.plane.standing();
        self.plane.card(&reader).and_then(|card| card.render().ok())
    }

    /// Every correspondent profile this identity holds, as address spellings.
    #[must_use]
    pub fn correspondents(&self) -> Vec<String> {
        let mine = self.plane.profile().clone();
        self.plane
            .registry_profiles()
            .into_iter()
            .filter(|profile| profile != &mine)
            .map(|profile| profile.as_str().to_owned())
            .collect()
    }

    /// Resolve an address spelling to the devices it names, if it is held.
    #[must_use]
    pub fn resolve_str(&self, address: &str) -> Option<Vec<String>> {
        let profile = ProfileId::parse(address)?;
        self.plane
            .resolve(&profile)
            .map(|devices| devices.iter().map(|d| d.as_str().to_owned()).collect())
    }

    /// Seal to a learned correspondent and deposit it over the hosted Post.
    ///
    /// The carrier signs with the canonical seed because a hosted deposit is
    /// fenced to the *sender's* egress device; the recipient is whichever of
    /// their devices resolution named.
    pub fn send_to(
        &mut self,
        recipient: &ProfileId,
        body: &str,
        now: u64,
    ) -> Result<String, ReachError> {
        use crate::post::{PostCarrier, Signer};
        let mut carrier =
            PostCarrier::new(self.base.clone(), Signer::new(self.plane.canonical_seed()));
        self.plane.send(&mut carrier, recipient, body, now)
    }

    /// Seal an invitation to a learned correspondent and deposit it.
    ///
    /// The coordinates are opaque bytes here, exactly as they are to the carrier
    /// and to `crates/correspondence`: an invitation verifies against its own
    /// Space and needs no prior state, which is what lets it ride anything.
    pub fn send_invitation(
        &mut self,
        recipient: &ProfileId,
        coordinates: Vec<u8>,
        now: u64,
    ) -> Result<String, ReachError> {
        use crate::post::{PostCarrier, Signer};
        let devices = self
            .plane
            .resolve(recipient)
            .ok_or(ReachError::NotReachable)?;
        let addressed = devices.first().ok_or(ReachError::NotReachable)?.clone();
        let mut carrier =
            PostCarrier::new(self.base.clone(), Signer::new(self.plane.canonical_seed()));
        self.plane.send_addressed(
            &mut carrier,
            recipient,
            &addressed,
            Content::Invitation { coordinates },
            now,
        )
    }

    /// Every letter this identity has opened, invitations included.
    #[must_use]
    pub fn opened(&self) -> Vec<Opened> {
        self.plane.opened()
    }

    /// The coordinates one opened invitation carries.
    #[must_use]
    pub fn invitation(&self, id: &str) -> Option<Vec<u8>> {
        self.plane.invitation(id)
    }

    /// Fetch anything waiting for you over the hosted Post, open it, and file it.
    ///
    /// Asks **every** device this identity holds, one carrier each: a sender
    /// addresses whichever device resolution named, and a Post signer may only
    /// fetch its own mailbox. Asking one device leaves the others' mail to
    /// expire unread, which is indistinguishable from nobody having written.
    pub fn collect(&mut self, now: u64) -> Collected {
        use crate::post::{PostCarrier, Signer};
        let mut collected = Collected {
            filed: 0,
            unasked: None,
        };
        for seed in self.plane.seeds.clone() {
            let device = device_from_seed(&seed);
            let mut carrier = PostCarrier::new(self.base.clone(), Signer::new(seed));
            let one = self.plane.collect_on(&mut carrier, &device, &seed, now);
            collected.filed = collected.filed.saturating_add(one.filed);
            if let Some(why) = one.unasked {
                collected.unasked.get_or_insert(why);
            }
        }
        collected
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
    use crate::MemCarrier;
    use mechanics::kinship::{Audience, Party};

    const ALICE_A: [u8; 32] = [11u8; 32];
    const ALICE_B: [u8; 32] = [12u8; 32];
    const BOB_A: [u8; 32] = [40u8; 32];
    const BOB_B: [u8; 32] = [41u8; 32];
    const NOW: u64 = 1_800_000_000;

    /// **A device outside the genesis pair cannot publish**, and this is the
    /// constraint device-join has to design around.
    ///
    /// `Registry::project` signs the head with whichever seed it is handed, and
    /// `absorb` refuses any head whose signer is not one of the two devices in
    /// the genesis link (`Failure::Unanchored`) — deliberately, since that
    /// anchor is what stops a stranger substituting a device set. So `canonical`
    /// today conflates two authorities that are not the same: *composing and
    /// sealing a letter*, which any live device may do, and *signing a head a
    /// correspondent will accept*, which only a genesis root may do.
    ///
    /// Pinned rather than fixed: splitting the two is the next piece of work,
    /// and it is worth this being a failing expectation somebody reads rather
    /// than a surprise somebody hits.
    #[test]
    fn a_device_outside_the_genesis_pair_cannot_publish_a_head_anyone_will_take() {
        let (a, b, c) = ([81u8; 32], [82u8; 32], [83u8; 32]);
        let mut plane = ReachPlane::found(vec![a, b, c], NOW).expect("found");

        let joined_later = device_from_seed(&c);
        plane
            .make_canonical(&joined_later)
            .expect("it is a device this identity holds");

        let reader = Standing {
            device: Some(device_from_seed(&[91u8; 32])),
            ..Standing::default()
        };
        let announcement = plane.announce(Audience::Public, &reader).expect("announce");

        let mut theirs = Registry::new();
        assert!(
            matches!(
                theirs.absorb(announcement.projection, &announcement.genesis, &reader),
                Err(registry::Failure::Unanchored)
            ),
            "a head signed off the genesis pair is refused by every reader"
        );
    }

    /// Handing the canonical role to another device leaves the address alone.
    ///
    /// This is what lets a real second machine take over from the seed a client
    /// founded with: the profile is the hash of a genesis link that names no
    /// primary, so who composes is a local choice and not part of the identity.
    #[test]
    fn the_canonical_device_moves_and_the_profile_does_not() {
        let mut plane = ReachPlane::found(vec![ALICE_A, ALICE_B], NOW).expect("found");
        let profile = plane.profile().clone();
        let first = plane.canonical_device();
        let second = device_from_seed(&ALICE_B);
        assert_ne!(first, second);

        plane.make_canonical(&second).expect("a device it holds");
        assert_eq!(plane.canonical_device(), second);
        assert_eq!(plane.standing().device, Some(second));
        assert_eq!(plane.profile(), &profile, "the address is unchanged");

        let stranger = device_from_seed(&BOB_A);
        assert!(
            plane.make_canonical(&stranger).is_err(),
            "a device this identity holds no seed for cannot compose for it"
        );
    }

    /// Every device this identity holds gets asked, not just the canonical one.
    ///
    /// A sender addresses whichever device resolution named, and the device set
    /// this identity publishes has more than one in it. Asking only the
    /// canonical device leaves the rest of the mail to expire unread — which a
    /// surface cannot tell from nobody having written.
    #[test]
    fn a_collect_asks_every_device_the_identity_holds() {
        let mut carrier = MemCarrier::new();
        let mut alice = ReachPlane::found(vec![ALICE_A, ALICE_B], NOW).expect("alice");
        let mut bob = ReachPlane::found(vec![BOB_A, BOB_B], NOW).expect("bob");

        let to_alice = alice
            .announce(Audience::Public, &bob.standing())
            .expect("announce");
        bob.learn(to_alice, &bob.standing()).expect("learn");

        // Address the device Alice does NOT compose on, which is exactly what a
        // resolution may choose.
        let elsewhere = device_from_seed(&ALICE_B);
        assert_ne!(elsewhere, alice.canonical_device());
        bob.send_addressed(
            &mut carrier,
            &alice.profile().clone(),
            &elsewhere,
            Content::Message {
                body: "addressed to your other device".into(),
            },
            NOW,
        )
        .expect("send");

        let collected = alice.collect(&mut carrier, NOW + 1);
        assert_eq!(
            collected.filed, 1,
            "the letter was found on the other device"
        );
        assert_eq!(collected.unasked, None);
        assert_eq!(alice.messages()[0].1, "addressed to your other device");
    }

    /// The client's own plane reaches over the **deployed** Post, when one is
    /// pointed at by `POST_SMOKE_URL` (e.g. `https://post.foundation.pub`).
    /// Skipped when unset so the offline suite never depends on the network.
    #[test]
    fn a_plane_reaches_over_the_deployed_post() {
        let Ok(base) = std::env::var("POST_SMOKE_URL") else {
            return;
        };
        use crate::post::{PostCarrier, Signer};
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
        let collected = alice.collect(&mut alice_carrier, now);
        assert_eq!(
            collected.filed, 1,
            "Alice fetched Bob's letter from the deployed Post"
        );
        assert_eq!(
            collected.unasked, None,
            "the deployed Post answered, so nothing went unasked"
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
        let collected = alice.collect(&mut carrier, NOW + 10);
        assert_eq!(collected.filed, 1);
        assert_eq!(collected.unasked, None);
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
        let collected = me.collect(now);
        assert_eq!(
            collected.filed, 1,
            "the note came back over the deployed Post"
        );
        assert_eq!(collected.unasked, None, "the deployed Post answered");
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
