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
use mechanics::kinship::{Audience, DeviceLink, Entry, ProfileId, Standing};

use crate::{Carrier, Content, Letter, Mailbox, Missed, Refused};

/// How long a letter is worth holding, from when it is sent.
const RETENTION: u64 = 60 * 60 * 24 * 7;

/// The genesis link's nonce and epoch. Fixed so a legacy profile — founded
/// when the genesis was derived from two seeds on one machine — is founded
/// again as the *same* profile from the seed that survived; see
/// [`ReachPlane::found_here`]. Changing either invalidates every issued
/// address in existence.
const GENESIS_NONCE: [u8; 16] = [7u8; 16];
const GENESIS_EPOCH: u64 = 1;

/// Why a plane operation did not apply.
#[derive(Debug)]
pub enum Failure {
    /// The durable state carries no genesis, and nothing here derives one.
    /// A plane that re-founded in its place would answer a new profile under
    /// an old log, and every address a person handed out would stop naming
    /// them; the boot path decides what a genesis-less home is.
    NoGenesis,
    /// This plane has already corresponded as its own profile — learned
    /// somebody, sent a letter, or been issued an address — so adopting another
    /// profile would orphan every one of those under an id nobody answers
    /// for. A device becomes somebody's before it speaks for itself, or not
    /// at all.
    AlreadyCorresponded,
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

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoGenesis => write!(
                f,
                "this store carries no genesis, and a profile is never re-derived"
            ),
            Self::AlreadyCorresponded => write!(
                f,
                "this device has already corresponded as its own profile and cannot be adopted"
            ),
            Self::NotReachable => write!(f, "we do not know how to reach them yet"),
            Self::Kinship(failure) => write!(f, "the kinship plane refused: {failure:?}"),
            Self::Carrier(refused) => write!(f, "the carrier refused: {refused:?}"),
            Self::Seal(refused) => write!(f, "the letter could not be sealed: {refused:?}"),
            Self::Egress(why) => write!(f, "this device could not prove its own key: {why}"),
        }
    }
}

impl From<registry::Failure> for Failure {
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
/// What learning an announcement would change about a profile already held.
///
/// Carries both sets rather than a boolean, because the person answering
/// "accept this?" is being asked to notice a substitution, and a question that
/// does not say what changed is a question that gets answered yes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceChange {
    pub profile: ProfileId,
    /// What this identity currently believes.
    pub held: Vec<DeviceId>,
    /// What the announcement avows.
    pub incoming: Vec<DeviceId>,
}

pub struct ReachPlane {
    /// This identity's device seeds — exactly one since the genesis is carried:
    /// a machine holds one device, and every other device of the profile holds
    /// its own seed on its own machine. The `Vec` outlives its second element
    /// only until the canonical/seed-for surface collapses onto it.
    seeds: Vec<[u8; 32]>,
    /// Which seed composes, signs and avows. Movable: `DeviceLink` is symmetric,
    /// so no device outranks another and the profile survives the handover.
    canonical: usize,
    profile: ProfileId,
    /// Carried, never derived — the link the profile id is the hash of.
    genesis: DeviceLink,
    origin: addressbook::reach_store::Origin,
    registry: Registry,
    // The egress-proving actor plane. The daemon supplies the identity's real
    // one; a self-incepted single-device plane stands in here.
    egress_space: SpaceId,
    egress_actor: ActorId,
    egress_events: Vec<SignedEvent>,
    mailbox: Mailbox,
    epoch: u64,
    /// What this identity has sent, by recipient address. The carrier forgets a
    /// letter once it is acknowledged, so this is the only durable copy of the
    /// half a person wrote.
    sent: std::collections::BTreeMap<String, Vec<addressbook::reach_store::Sent>>,
    /// The address a directory issued, if one has. Never minted locally: an
    /// address a holder could choose is one a holder could squat.
    address: Option<String>,
}

impl ReachPlane {
    /// Found this identity's profile from one seed, and stand up the plane.
    ///
    /// A genesis is a mutual link between two distinct devices, and a machine
    /// holds one. The second signer is a **witness**: it co-signs the birth
    /// certificate and is retired in the same act, so the profile resolves to
    /// `[identity]` from its first moment and the witness seed is never
    /// written anywhere. `witness` is the legacy `kinship.key` seed when a home
    /// is being carried forward — the fixed nonce and epoch then reproduce the
    /// genesis that home already handed out as its address — and a random
    /// in-memory seed otherwise.
    ///
    /// `held` is a pre-carriage store: its registry, sent letters and address
    /// are kept and the retirement is appended to its authored log. A held
    /// registry that authored a profile this seed and witness do not reproduce
    /// is refused as [`Failure::NoGenesis`] rather than founded beside — that
    /// is the home whose profile cannot be reconstructed, and answering a new
    /// one under its log is the silent re-found this refusal exists to stop.
    ///
    /// `_now` is reserved: the genesis carries a fixed epoch today.
    pub fn found_here(
        identity: [u8; 32],
        witness: Option<[u8; 32]>,
        held: Option<addressbook::ReachState>,
        _now: u64,
    ) -> Result<Self, Failure> {
        let witness = match witness {
            Some(seed) => seed,
            None => mechanics::actor::random_seed()
                .map_err(|e| Failure::Egress(format!("no randomness for the witness: {e}")))?,
        };
        let genesis = DeviceLink::seal(&identity, &witness, GENESIS_NONCE, GENESIS_EPOCH)
            .map_err(|e| Failure::Kinship(registry::Failure::Kinship(e)))?;
        let retirement = mechanics::kinship::Retirement::seal(
            &identity,
            device_from_seed(&witness),
            GENESIS_EPOCH.saturating_add(1),
            GENESIS_NONCE,
        )
        .map_err(|e| Failure::Kinship(registry::Failure::Kinship(e)))?;

        let expected = mechanics::kinship::KinshipLog::found(genesis.clone())
            .map_err(|e| Failure::Kinship(registry::Failure::Kinship(e)))?;
        let (mut registry, epoch, sent, address) = match held {
            Some(held) => {
                if held.registry.authored().any(|p| p != expected.profile()) {
                    return Err(Failure::NoGenesis);
                }
                (held.registry, held.epoch, held.sent, held.address)
            }
            None => (Registry::new(), 1, std::collections::BTreeMap::new(), None),
        };
        let profile = registry.found(genesis.clone())?;
        registry.extend(&profile, Entry::Retire(retirement))?;

        Ok(Self::stand(
            identity,
            profile,
            genesis,
            addressbook::ReachState {
                epoch,
                canonical: 0,
                registry,
                sent,
                address,
                genesis: None,
                origin: addressbook::reach_store::Origin::Founded,
            },
        ))
    }

    /// The profile two legacy seeds name, without founding a plane.
    ///
    /// Migration only: the derivation a home used before its genesis was
    /// carried, kept so a boot path can check that a `kinship.key` still
    /// found beside a carried genesis is the witness that signed it, and so a
    /// test can pin that the carried id is the derived one. Nothing at run
    /// time derives a profile from here.
    pub fn profile_for(seeds: &[[u8; 32]]) -> Result<mechanics::kinship::ProfileId, Failure> {
        let log = mechanics::kinship::KinshipLog::found(Self::genesis_for(seeds)?)
            .map_err(|e| Failure::Kinship(registry::Failure::Kinship(e)))?;
        Ok(log.profile().clone())
    }

    /// The fixed genesis two legacy seeds name — [`profile_for`]'s artifact.
    /// Migration only, like it.
    ///
    /// [`profile_for`]: ReachPlane::profile_for
    pub fn genesis_for(seeds: &[[u8; 32]]) -> Result<DeviceLink, Failure> {
        let (Some(first), Some(second)) = (seeds.first(), seeds.get(1)) else {
            return Err(Failure::NoGenesis);
        };
        DeviceLink::seal(first, second, GENESIS_NONCE, GENESIS_EPOCH)
            .map_err(|e| Failure::Kinship(registry::Failure::Kinship(e)))
    }

    /// Stand the plane up again from durable state.
    ///
    /// The genesis is read from the state and never recomputed: `NoGenesis`
    /// when it is absent, because the seed alone cannot name the profile the
    /// log was authored under. `state.canonical` is ignored — a machine holds
    /// one seed. Refuses a state whose profile does not resolve to this seed's
    /// device: a store copied from another machine would otherwise sign heads
    /// no reader takes, silently.
    pub fn restore(
        seed: [u8; 32],
        state: addressbook::ReachState,
        _now: u64,
    ) -> Result<Self, Failure> {
        let mut state = state;
        let genesis = state.genesis.take().ok_or(Failure::NoGenesis)?;
        let profile = state.registry.found(genesis.clone())?;
        let me = device_from_seed(&seed);
        if !state
            .registry
            .resolve(&profile)
            .is_some_and(|devices| devices.contains(&me))
        {
            return Err(Failure::NotReachable);
        }
        Ok(Self::stand(seed, profile, genesis, state))
    }

    /// The one constructor. `genesis` is the link `profile` hashes to and
    /// `state.registry` already holds it authored; `state.genesis` is not
    /// read, the caller having taken it to prove exactly that.
    fn stand(
        seed: [u8; 32],
        profile: ProfileId,
        genesis: DeviceLink,
        state: addressbook::ReachState,
    ) -> Self {
        let egress_space = SpaceId::mint(&SystemUlidSource);
        let (egress_events, egress_actor) = incept(&seed, 1, &egress_space);
        Self {
            seeds: vec![seed],
            canonical: 0,
            profile,
            genesis,
            origin: state.origin,
            registry: state.registry,
            egress_space,
            egress_actor,
            egress_events,
            mailbox: Mailbox::new(),
            epoch: state.epoch,
            sent: state.sent,
            address: state.address,
        }
    }

    /// What has to survive a restart: the learned correspondents and the epoch.
    #[must_use]
    pub fn state(&self) -> addressbook::ReachState {
        addressbook::ReachState {
            epoch: self.epoch,
            canonical: self.canonical,
            registry: self.registry.clone(),
            sent: self.sent.clone(),
            address: self.address.clone(),
            genesis: Some(self.genesis.clone()),
            origin: self.origin.clone(),
        }
    }

    /// The short address a directory issued this identity, if one has.
    #[must_use]
    pub fn address(&self) -> Option<&str> {
        self.address.as_deref()
    }

    /// Record the address a directory issued.
    ///
    /// Takes what the service answered rather than deriving anything: issuance
    /// is the directory's act, and a plane that could compute its own address
    /// would be a plane that could choose one.
    pub fn issued(&mut self, address: String) {
        self.address = Some(address);
    }

    /// What `announcement` would change about a profile this identity already
    /// holds, without absorbing it.
    ///
    /// AUTH-18's v1 rung, and the reason it is a *question* rather than a
    /// side effect: a directory that answered with a substituted key must not
    /// be able to cause a seal to it. Learning is what commits, so a caller asks
    /// this first and a person answers.
    ///
    /// `None` when this profile is new, or when the avowed set is unchanged —
    /// both of which are safe to take silently. `Some` names the two sets, and a
    /// caller must **block** rather than badge: field studies put manual
    /// verification at 13 to 14 percent, so a warning nobody reads is a warning
    /// that does not exist.
    #[must_use]
    pub fn change_on_learning(
        &self,
        announcement: &Announcement,
        reader: &Standing,
    ) -> Option<DeviceChange> {
        let held = self.registry.resolve(&announcement.profile)?;
        let mut scratch = Registry::new();
        let absorbed = scratch
            .absorb(
                announcement.projection.clone(),
                &announcement.genesis,
                reader,
            )
            .ok()?;
        let incoming = scratch.resolve(&absorbed).unwrap_or_default();
        let same = held.len() == incoming.len() && held.iter().all(|d| incoming.contains(d));
        (!same).then(|| DeviceChange {
            profile: announcement.profile.clone(),
            held,
            incoming,
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
    ) -> Result<Announcement, Failure> {
        self.publish(audience, reader, None)
    }

    /// [`Self::announce`], presenting: the portrait's avowals are sealed at
    /// the same bumped epoch, so who-this-is rides the identical rail as
    /// which-devices-are-it and supersedes the same way.
    pub fn announce_presenting(
        &mut self,
        audience: Audience,
        reader: &Standing,
        portrait: &addressbook::Portrait,
    ) -> Result<Announcement, Failure> {
        self.publish(audience, reader, Some(portrait))
    }

    fn publish(
        &mut self,
        audience: Audience,
        reader: &Standing,
        portrait: Option<&addressbook::Portrait>,
    ) -> Result<Announcement, Failure> {
        self.epoch = self.epoch.saturating_add(1);
        // Derived from the epoch rather than sampled, so a republication is
        // reproducible from durable state alone. It carries 8 bits and repeats
        // every 256 epochs, which is safe only because `Avowal`'s preimage is
        // domain-separated and already unique per (device, epoch, claim) — the
        // nonce distinguishes nothing a collision could confuse. It is not a
        // secret and must never become one; a real source belongs here the
        // moment anything depends on it being unguessable.
        let nonce = [u8::try_from(self.epoch & 0xff).unwrap_or(0); 16];
        if let Some(portrait) = portrait {
            self.registry.avow_portrait(
                &self.profile,
                portrait,
                audience.clone(),
                &self.canonical_seed(),
                self.epoch,
                nonce,
            )?;
        }
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

    /// This identity's card for one of its **own** devices — what a pairing
    /// hands the joiner so it can stand as this profile.
    ///
    /// Projects under `Standing { own: true }`, so the structural bodies ride:
    /// the genesis link and the witness retirement are `Audience::Own` facts,
    /// and a joiner given a card without them holds a head it cannot walk from
    /// the genesis. Unlike [`Self::card`] this answers at epoch 1 — a profile
    /// that has never avowed anything still has devices, and the joiner is
    /// adopting the profile, not reaching it.
    pub fn own_card(&self, for_device: &DeviceId) -> Result<Announcement, Failure> {
        let reader = Standing {
            own: true,
            device: Some(for_device.clone()),
            ..Standing::default()
        };
        let projection =
            self.registry
                .project(&self.profile, &self.canonical_seed(), self.epoch, &reader)?;
        Ok(Announcement::new(
            self.profile.clone(),
            self.genesis.clone(),
            projection,
        ))
    }

    /// How this device came to hold its profile.
    #[must_use]
    pub fn origin(&self) -> &addressbook::reach_store::Origin {
        &self.origin
    }

    /// Learn a correspondent's profile from their announcement, anchored to its
    /// genesis. `reader` is this identity's own standing toward them.
    pub fn learn(
        &mut self,
        announcement: Announcement,
        reader: &Standing,
    ) -> Result<ProfileId, Failure> {
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

    /// The name a held profile declares for itself, as `reader` may read it.
    #[must_use]
    pub fn declared_name(&self, profile: &ProfileId, reader: &Standing) -> Option<String> {
        self.registry.declared_name(profile, reader)
    }

    /// A held profile's portrait, as `reader` may read it.
    #[must_use]
    pub fn portrait(
        &self,
        profile: &ProfileId,
        reader: &Standing,
    ) -> Option<addressbook::ResolvedPortrait> {
        self.registry.portrait(profile, reader)
    }

    /// Every profile this identity holds, its own included.
    #[must_use]
    /// The registry beneath, read-only — for a caller projecting with a seed
    /// this plane does not hold, which is exactly an adopted placement's case.
    #[must_use]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

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

    /// Adopt a device this identity does not hold the seed for — a placement
    /// on another machine, or the printed recovery device — by appending its
    /// consented link.
    ///
    /// The link is the sponsorship artifact: both sides signed the same
    /// preimage where their seeds live (`DeviceLink::half` + `assemble`), and
    /// one of them must already be rooted here, or the append is an unrelated
    /// pair wearing this profile's log. The adopted device can then *sign
    /// heads every reader takes* — `project` carries its chain and `absorb`
    /// walks it — but it cannot compose from this plane, which holds no seed
    /// for it and says so rather than pretending.
    pub fn adopt_device(&mut self, link: mechanics::kinship::DeviceLink) -> Result<(), Failure> {
        Self::adopt_into(&mut self.registry, &self.profile, link)
    }

    /// The append behind [`Self::adopt_device`], on a registry that is not
    /// yet this plane's — so a joiner can build its adopted holding whole and
    /// take it on only once every step has passed.
    fn adopt_into(
        registry: &mut Registry,
        profile: &ProfileId,
        link: mechanics::kinship::DeviceLink,
    ) -> Result<(), Failure> {
        link.verify()
            .map_err(|e| Failure::Kinship(registry::Failure::Kinship(e)))?;
        let rooted = registry.resolve(profile).unwrap_or_default();
        if !link.devices.iter().any(|device| rooted.contains(device)) {
            return Err(Failure::NotReachable);
        }
        registry.extend(profile, mechanics::kinship::Entry::Link(link))?;
        Ok(())
    }

    /// Become a device of the profile `card` carries — the joiner's half of
    /// a pairing, once the sponsor has assembled the link.
    ///
    /// The throwaway profile this plane was founded under is dropped, not
    /// merged: nothing was ever said under it, or this refuses with
    /// [`Failure::AlreadyCorresponded`] — a profile that has learned somebody,
    /// sent a letter or been issued an address has correspondents who would
    /// otherwise be sealing to an id nobody answers for any more.
    ///
    /// The card is taken as **authored**, not absorbed: the sponsor projected
    /// it under `Standing { own: true }` so the structural bodies ride, and
    /// this device will sign heads over that log from now on. Every step is
    /// checked against a holding built beside the current one, and the plane
    /// takes it on only at the end — a refusal half-way leaves the throwaway
    /// profile standing, which is the state that can be retried.
    pub fn become_device_of(
        &mut self,
        card: Announcement,
        from: DeviceId,
        link: DeviceLink,
        now: u64,
    ) -> Result<(), Failure> {
        if self.registry.profiles().count() > 1 || !self.sent.is_empty() || self.address.is_some() {
            return Err(Failure::AlreadyCorresponded);
        }
        let log = mechanics::kinship::KinshipLog::found(card.genesis.clone())
            .map_err(|e| Failure::Kinship(registry::Failure::Kinship(e)))?;
        if log.profile() != &card.profile {
            return Err(Failure::Kinship(registry::Failure::Unanchored));
        }
        let reader = Standing {
            own: true,
            device: Some(from.clone()),
            ..Standing::default()
        };
        card.projection
            .verify(&reader)
            .map_err(|e| Failure::Kinship(registry::Failure::Kinship(e)))?;

        let mut registry = Registry::new();
        let profile = registry.found(card.genesis.clone())?;
        for body in &card.projection.bodies {
            match body {
                Entry::Link(carried) if carried == &card.genesis => {}
                Entry::Link(_) | Entry::Retire(_) => registry.extend(&profile, body.clone())?,
                Entry::Avow(_) => {}
            }
        }
        if !registry
            .resolve(&profile)
            .is_some_and(|devices| devices.contains(&from))
        {
            return Err(Failure::NotReachable);
        }
        let me = self.canonical_device();
        if !link.names(&me) {
            return Err(Failure::NotReachable);
        }
        Self::adopt_into(&mut registry, &profile, link)?;

        let theirs = card.projection.head.as_ref().map_or(0, |head| head.epoch);
        self.epoch = self.epoch.max(theirs).saturating_add(1);
        self.profile = profile;
        self.genesis = card.genesis;
        self.registry = registry;
        self.origin = addressbook::reach_store::Origin::Adopted { from, at: now };
        Ok(())
    }

    /// Hand the canonical role to another of this identity's devices.
    ///
    /// The profile is unchanged — it is the hash of a genesis link that names no
    /// primary. Refuses a device this identity does not hold the seed for.
    pub fn make_canonical(&mut self, device: &DeviceId) -> Result<(), Failure> {
        let at = self
            .seeds
            .iter()
            .position(|seed| &device_from_seed(seed) == device)
            .ok_or(Failure::NotReachable)?;
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
        &mut self,
        carrier: &mut (impl Carrier + ?Sized),
        recipient: &ProfileId,
        body: &str,
        now: u64,
    ) -> Result<String, Failure> {
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
        &mut self,
        carrier: &mut (impl Carrier + ?Sized),
        recipient: &ProfileId,
        content: Content,
        now: u64,
    ) -> Result<String, Failure> {
        let remembered = match &content {
            Content::Message { body } => Some(addressbook::reach_store::Sent {
                at: now,
                body: body.clone(),
                invitation: false,
            }),
            Content::Invitation { .. } => Some(addressbook::reach_store::Sent {
                at: now,
                body: String::new(),
                invitation: true,
            }),
        };
        let deposited = self.send_content_inner(carrier, recipient, content, now)?;
        if let Some(remembered) = remembered {
            self.sent
                .entry(recipient.as_str().to_owned())
                .or_default()
                .push(remembered);
        }
        Ok(deposited)
    }

    /// Send through a contractor, signing as the device that composes.
    pub fn send_via(
        &mut self,
        contractor: &dyn crate::Contractor,
        recipient: &ProfileId,
        content: Content,
        now: u64,
    ) -> Result<String, Failure> {
        let mut carrier = contractor.carrier_for(&self.canonical_seed());
        self.send_content(&mut *carrier, recipient, content, now)
    }

    /// Collect through a contractor, asking as **every** device this identity
    /// holds.
    ///
    /// A sender addresses whichever device resolution named, and a carrier may
    /// only fetch its own signer's mailbox — so asking as one device leaves the
    /// rest of the mail to expire unread, which a surface cannot tell from
    /// nobody having written.
    pub fn collect_via(&mut self, contractor: &dyn crate::Contractor, now: u64) -> Collected {
        let mut collected = Collected {
            filed: 0,
            unasked: None,
        };
        for seed in self.seeds.clone() {
            let device = device_from_seed(&seed);
            let mut carrier = contractor.carrier_for(&seed);
            let one = self.collect_on(&mut *carrier, &device, &seed, now);
            collected.filed = collected.filed.saturating_add(one.filed);
            if let Some(why) = one.unasked {
                collected.unasked.get_or_insert(why);
            }
        }
        collected
    }

    /// What this identity has sent to one correspondent.
    #[must_use]
    pub fn sent_to(&self, recipient: &str) -> &[addressbook::reach_store::Sent] {
        self.sent.get(recipient).map_or(&[], Vec::as_slice)
    }

    fn send_content_inner(
        &self,
        carrier: &mut (impl Carrier + ?Sized),
        recipient: &ProfileId,
        content: Content,
        now: u64,
    ) -> Result<String, Failure> {
        let devices = self.resolve(recipient).ok_or(Failure::NotReachable)?;
        let addressed = devices.first().ok_or(Failure::NotReachable)?.clone();
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
    ) -> Result<String, Failure> {
        let devices = self.resolve(recipient).ok_or(Failure::NotReachable)?;
        if !devices.contains(addressed) {
            return Err(Failure::NotReachable);
        }
        let letter = Letter::compose(&self.canonical_seed(), content, now);
        let sealed = letter
            .seal_to_devices(&devices, addressed, now.saturating_add(RETENTION))
            .map_err(Failure::Seal)?;
        let plane = actor::replay(&self.egress_space, &self.egress_events);
        let witness = egress::authorize(&plane, &self.egress_actor, &self.canonical_device())
            .map_err(|refused| Failure::Egress(refused.to_string()))?;
        carrier
            .deposit(&witness, &sealed, now)
            .map_err(Failure::Carrier)
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
    /// Found this identity's profile from its seed, pointed at a Post.
    pub fn found(identity: [u8; 32], base: String, now: u64) -> Result<Self, Failure> {
        Ok(Self {
            plane: ReachPlane::found_here(identity, None, None, now)?,
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
    pub fn send_self(&self, body: &str, now: u64) -> Result<String, Failure> {
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

    /// Stand the hosted plane up again from durable state.
    pub fn restore(
        seed: [u8; 32],
        state: addressbook::ReachState,
        base: String,
        now: u64,
    ) -> Result<Self, Failure> {
        Ok(Self {
            plane: ReachPlane::restore(seed, state, now)?,
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
    ) -> Result<Announcement, Failure> {
        self.plane.announce(audience, reader)
    }

    /// [`Self::announce`], carrying the identity's portrait on the same rail.
    pub fn announce_presenting(
        &mut self,
        audience: Audience,
        reader: &Standing,
        portrait: &addressbook::Portrait,
    ) -> Result<Announcement, Failure> {
        self.plane.announce_presenting(audience, reader, portrait)
    }

    /// Take in a correspondent's announcement, anchored to its genesis.
    pub fn learn(
        &mut self,
        announcement: Announcement,
        reader: &Standing,
    ) -> Result<ProfileId, Failure> {
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
    ) -> Result<String, Failure> {
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
    ) -> Result<String, Failure> {
        use crate::post::{PostCarrier, Signer};
        let devices = self.plane.resolve(recipient).ok_or(Failure::NotReachable)?;
        let addressed = devices.first().ok_or(Failure::NotReachable)?.clone();
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
    const NOW: u64 = 1_800_000_000;

    /// One seed founds a profile; the witness co-signs the genesis and is
    /// retired in the same act; and standing the plane up again reads the
    /// genesis back rather than deriving one. If restore still derived, the
    /// second founding below would be indistinguishable from the first; if the
    /// witness stayed live, the profile would resolve to a device nobody holds.
    #[test]
    fn a_profile_is_founded_by_one_seed_and_a_witness_that_leaves() {
        let plane = ReachPlane::found_here(ALICE_A, None, None, NOW).expect("found");
        let me = device_from_seed(&ALICE_A);
        assert_eq!(plane.my_devices(), vec![me.clone()], "the witness left");
        assert!(plane.profile().as_str().starts_with("prf_"));

        let state = plane.state();
        let genesis = state.genesis.clone().expect("the genesis is carried");
        assert!(genesis.names(&me));
        let witness = genesis
            .devices
            .iter()
            .find(|device| **device != me)
            .expect("a witness co-signed the genesis");
        assert!(
            !plane.my_devices().contains(witness),
            "and was retired in the same act"
        );

        let again = ReachPlane::restore(ALICE_A, state.clone(), NOW).expect("restore");
        assert_eq!(
            again.profile(),
            plane.profile(),
            "the same profile, from the file"
        );
        assert_eq!(again.my_devices(), vec![me]);

        // Founding is an act, not a derivation: the same seed founded twice is
        // two profiles, which is why the first has to be carried.
        let other = ReachPlane::found_here(ALICE_A, None, None, NOW).expect("found again");
        assert_ne!(other.profile(), plane.profile());

        let mut bare = state;
        bare.genesis = None;
        assert!(
            matches!(
                ReachPlane::restore(ALICE_A, bare, NOW),
                Err(Failure::NoGenesis)
            ),
            "a state without a genesis is refused, never re-derived"
        );
    }

    /// The legacy shape — two seeds on one machine — founds the *same* profile
    /// when the second seed returns as the witness: same nonce, same epoch,
    /// sorted pair, deterministic signatures. Every issued address depends on
    /// this equality; the retirement rides on top of it.
    #[test]
    fn a_legacy_witness_founds_the_profile_its_two_seeds_derived() {
        let derived = ReachPlane::profile_for(&[ALICE_A, ALICE_B]).expect("derive");
        let plane = ReachPlane::found_here(ALICE_A, Some(ALICE_B), None, NOW).expect("found");
        assert_eq!(&derived, plane.profile(), "the address survived the carry");
        assert_eq!(plane.my_devices(), vec![device_from_seed(&ALICE_A)]);
        assert!(matches!(
            ReachPlane::profile_for(&[ALICE_A]),
            Err(Failure::NoGenesis)
        ));

        // A held store from that legacy home is carried forward whole — and
        // one whose authored log this seed cannot reproduce is refused, not
        // founded beside.
        let mut held = plane.state();
        held.genesis = None;
        held.address = Some("ada.example".into());
        let carried = ReachPlane::found_here(ALICE_A, Some(ALICE_B), Some(held.clone()), NOW)
            .expect("carried");
        assert_eq!(carried.profile(), &derived);
        assert_eq!(carried.address(), Some("ada.example"));
        assert!(matches!(
            ReachPlane::found_here(ALICE_A, None, Some(held), NOW),
            Err(Failure::NoGenesis)
        ));
    }

    /// A joiner is handed the profile as its own device sees it. The card is
    /// evidence to a reader standing as `own`, and it carries the structural
    /// bodies — the genesis link and the witness retirement — because a head
    /// nobody can walk from the genesis is a hint, not a profile. `card`
    /// refuses at epoch 1; this must not, or a fresh profile could never be
    /// paired from.
    #[test]
    fn an_own_card_carries_the_structural_bodies_for_an_own_device() {
        let plane = ReachPlane::found_here(ALICE_A, None, None, NOW).expect("found");
        let me = device_from_seed(&ALICE_A);
        assert!(
            plane.card(&plane.standing()).is_none(),
            "nothing avowed yet"
        );

        let card = plane.own_card(&me).expect("an own card at epoch 1");
        assert_eq!(&card.profile, plane.profile());
        let reader = Standing {
            own: true,
            device: Some(me.clone()),
            ..Standing::default()
        };
        card.projection
            .verify(&reader)
            .expect("evidence to an own reader");

        let genesis = plane.state().genesis.expect("carried");
        let witness = genesis
            .devices
            .iter()
            .find(|device| **device != me)
            .cloned()
            .expect("the witness");
        assert!(
            card.projection
                .bodies
                .iter()
                .any(|entry| matches!(entry, Entry::Link(link) if link == &genesis)),
            "the genesis link rides"
        );
        assert!(
            card.projection.bodies.iter().any(
                |entry| matches!(entry, Entry::Retire(retirement) if retirement.device == witness)
            ),
            "and so does the witness retirement: {:?}",
            card.projection.bodies
        );
        assert!(matches!(
            plane.origin(),
            addressbook::reach_store::Origin::Founded
        ));
    }

    /// The user-facing consequence of adoption: a correspondent's address
    /// book learns the placement. Not automatic — the *next announcement*
    /// avows the whole current device set, adopted device included, and the
    /// correspondent's `resolve` answers with it. If this test fails, joins
    /// are real but invisible, which is the worse defect.
    #[test]
    fn a_correspondents_address_book_learns_an_adopted_device() {
        let a = [81u8; 32];
        let placement: [u8; 32] = [84u8; 32];
        let mut plane = ReachPlane::found_here(a, None, None, NOW).expect("found");

        let reader = Standing {
            device: Some(device_from_seed(&[91u8; 32])),
            ..Standing::default()
        };
        // The correspondent holds the pre-adoption card.
        let before = plane.announce(Audience::Public, &reader).expect("announce");
        let mut theirs = Registry::new();
        theirs
            .absorb(before.projection, &before.genesis, &reader)
            .expect("absorb the pre-adoption card");
        let placement_device = device_from_seed(&placement);
        assert!(
            !theirs
                .resolve(plane.profile())
                .expect("held")
                .contains(&placement_device),
            "not yet adopted, not yet resolvable"
        );

        // Adopt, then announce again — the epoch advances, the avowals cover
        // the grown device set, and the correspondent's view follows.
        let (nonce, epoch) = ([13u8; 16], 9);
        let link = mechanics::kinship::DeviceLink::assemble(
            (
                device_from_seed(&a),
                mechanics::kinship::DeviceLink::half(&a, &placement_device, nonce, epoch),
            ),
            (
                placement_device.clone(),
                mechanics::kinship::DeviceLink::half(
                    &placement,
                    &device_from_seed(&a),
                    nonce,
                    epoch,
                ),
            ),
            nonce,
            epoch,
        )
        .expect("assemble");
        plane.adopt_device(link).expect("adopt");
        let after = plane
            .announce(Audience::Public, &reader)
            .expect("announce again");
        theirs
            .absorb(after.projection, &after.genesis, &reader)
            .expect("absorb the post-adoption card");
        assert!(
            theirs
                .resolve(plane.profile())
                .expect("held")
                .contains(&placement_device),
            "the correspondent's address book resolves the placement"
        );
    }

    /// The sponsorship round trip: halves signed on two machines, assembled,
    /// adopted — and the adopted device signs a head a stranger takes. The
    /// full remote-join flow minus only the transport that carries the half.
    #[test]
    fn an_adopted_device_publishes_from_its_own_seed() {
        let a = [81u8; 32];
        let placement: [u8; 32] = [84u8; 32];
        let mut plane = ReachPlane::found_here(a, None, None, NOW).expect("found");

        // The sponsor (a) and the placement each sign the same preimage where
        // their seed lives; nobody's seed crosses a machine boundary.
        let sponsor_device = device_from_seed(&a);
        let placement_device = device_from_seed(&placement);
        let (nonce, epoch) = ([13u8; 16], 9);
        let sponsor_half =
            mechanics::kinship::DeviceLink::half(&a, &placement_device, nonce, epoch);
        let placement_half =
            mechanics::kinship::DeviceLink::half(&placement, &sponsor_device, nonce, epoch);
        let link = mechanics::kinship::DeviceLink::assemble(
            (sponsor_device, sponsor_half),
            (placement_device, placement_half),
            nonce,
            epoch,
        )
        .expect("two halves make the link seal would have made");
        plane.adopt_device(link).expect("adopt the placement");

        // An unrelated pair is refused: adoption is rooted or it is nothing.
        let unrelated =
            mechanics::kinship::DeviceLink::seal(&[85u8; 32], &[86u8; 32], [14u8; 16], 10)
                .expect("seal");
        assert!(plane.adopt_device(unrelated).is_err());

        // The adopted device signs from its own seed on its own machine: the
        // registry projections carry its chain, so any reader takes the head.
        let reader = Standing {
            device: Some(device_from_seed(&[91u8; 32])),
            ..Standing::default()
        };
        let projection = plane
            .registry()
            .project(plane.profile(), &placement, 11, &reader)
            .expect("project as the placement");
        let mut theirs = Registry::new();
        let genesis = plane.state().genesis.expect("the carried genesis");
        theirs
            .absorb(projection, &genesis, &reader)
            .expect("an adopted placement's head is evidence to a stranger");
    }

    /// The pairing's two ends, minus the transport: the sponsor hands its own
    /// card, both sides sign the same preimage where their seeds live, the
    /// sponsor assembles and adopts, and the joiner becomes a device of the
    /// carried profile. Both then resolve to the same set under the same id,
    /// and a stranger takes the joiner's head — which fails if the Own gate
    /// withheld the links the joiner has to walk from the genesis.
    #[test]
    fn a_device_becomes_a_device_of_a_carried_profile() {
        let mut sponsor = ReachPlane::found_here(ALICE_A, None, None, NOW).expect("found");
        let mut joiner = ReachPlane::found_here(ALICE_B, None, None, NOW).expect("founded alone");
        let sponsor_device = device_from_seed(&ALICE_A);
        let joiner_device = device_from_seed(&ALICE_B);
        let throwaway = joiner.profile().clone();
        assert_ne!(&throwaway, sponsor.profile());

        let card = sponsor.own_card(&sponsor_device).expect("own card");
        let (nonce, epoch) = ([21u8; 16], 2);
        let joiner_half = DeviceLink::half(&ALICE_B, &sponsor_device, nonce, epoch);
        let sponsor_half = DeviceLink::half(&ALICE_A, &joiner_device, nonce, epoch);
        let link = DeviceLink::assemble(
            (sponsor_device.clone(), sponsor_half),
            (joiner_device.clone(), joiner_half),
            nonce,
            epoch,
        )
        .expect("assemble");
        sponsor
            .adopt_device(link.clone())
            .expect("the sponsor adopts");
        joiner
            .become_device_of(card, sponsor_device.clone(), link, NOW)
            .expect("the joiner becomes a device of the profile");

        assert_eq!(joiner.profile(), sponsor.profile());
        assert_ne!(
            joiner.profile(),
            &throwaway,
            "the throwaway profile is dropped"
        );
        assert_eq!(joiner.my_devices(), sponsor.my_devices());
        assert_eq!(joiner.my_devices(), {
            let mut both = vec![sponsor_device.clone(), joiner_device.clone()];
            both.sort();
            both
        });
        assert!(matches!(
            joiner.origin(),
            addressbook::reach_store::Origin::Adopted { from, at: NOW } if from == &sponsor_device
        ));
        assert_eq!(joiner.canonical_device(), joiner_device);
        assert!(
            joiner.state().genesis.as_ref() == sponsor.state().genesis.as_ref(),
            "the carried genesis is the sponsor's"
        );

        // A stranger takes the joiner's head: the chain from the genesis to
        // the joiner rides the projection.
        let reader = Standing {
            device: Some(device_from_seed(&BOB_A)),
            ..Standing::default()
        };
        let announced = joiner
            .announce(Audience::Public, &reader)
            .expect("the joiner announces as the profile");
        let mut theirs = Registry::new();
        theirs
            .absorb(announced.projection, &announced.genesis, &reader)
            .expect("a joined device's head is evidence to a stranger");
        let resolved = theirs.resolve(sponsor.profile()).expect("held");
        assert!(resolved.contains(&joiner_device) && resolved.contains(&sponsor_device));

        // Becoming a device twice is refused, and refused as "already
        // corresponded" would be wrong too: what is held is the adopted
        // profile, which a second card cannot replace.
        let again = sponsor.own_card(&sponsor_device).expect("card");
        let unrelated = DeviceLink::seal(&[85u8; 32], &[86u8; 32], [14u8; 16], 10).expect("seal");
        assert!(joiner
            .become_device_of(again, sponsor_device, unrelated, NOW)
            .is_err());
    }

    /// A joiner that has spoken as its own profile keeps it. Learning
    /// somebody and being issued an address are each enough: either leaves a
    /// correspondent holding an id this device would stop answering for.
    #[test]
    fn a_joiner_that_has_corresponded_refuses_adoption() {
        let sponsor = ReachPlane::found_here(ALICE_A, None, None, NOW).expect("found");
        let sponsor_device = device_from_seed(&ALICE_A);
        let joiner_device = device_from_seed(&ALICE_B);
        let (nonce, epoch) = ([22u8; 16], 2);
        let link = DeviceLink::assemble(
            (
                sponsor_device.clone(),
                DeviceLink::half(&ALICE_A, &joiner_device, nonce, epoch),
            ),
            (
                joiner_device,
                DeviceLink::half(&ALICE_B, &sponsor_device, nonce, epoch),
            ),
            nonce,
            epoch,
        )
        .expect("assemble");
        let card = sponsor.own_card(&sponsor_device).expect("card");

        let mut learned = ReachPlane::found_here(ALICE_B, None, None, NOW).expect("found");
        let mut bob = ReachPlane::found_here(BOB_A, None, None, NOW).expect("found");
        let bobs = bob
            .announce(Audience::Public, &learned.standing())
            .expect("announce");
        learned.learn(bobs, &bob.standing()).expect("learn bob");
        assert!(matches!(
            learned.become_device_of(card.clone(), sponsor_device.clone(), link.clone(), NOW),
            Err(Failure::AlreadyCorresponded)
        ));
        assert_ne!(learned.profile(), sponsor.profile(), "nothing changed");

        let mut addressed = ReachPlane::found_here(ALICE_B, None, None, NOW).expect("found");
        addressed.issued("tin-harbor-quiet-4417".into());
        assert!(matches!(
            addressed.become_device_of(card, sponsor_device, link, NOW),
            Err(Failure::AlreadyCorresponded)
        ));
    }

    /// A device set that grew is a question, not a badge: the correspondent
    /// who learned the old set is asked before the new one is sealed to.
    #[test]
    fn a_device_set_that_grew_is_a_change_a_person_has_to_answer_for() {
        // Ada announced once and was learned by Bob.
        let ada_seed = [71u8; 32];
        let mut ada = ReachPlane::found_here(ada_seed, None, None, NOW).expect("found");
        let mut bob = ReachPlane::found_here([81u8; 32], None, None, NOW).expect("found");
        let first = ada
            .announce(Audience::Public, &bob.standing())
            .expect("announce");
        bob.learn(first, &bob.standing()).expect("learn");

        // Nothing changed: re-announcing the same set is not a question.
        let unchanged = ada
            .announce(Audience::Public, &bob.standing())
            .expect("announce again");
        assert_eq!(
            bob.change_on_learning(&unchanged, &bob.standing()),
            None,
            "an unchanged device set was raised as a change, which trains a person to say yes"
        );

        // A second device joins by link, and now it is.
        let joined: [u8; 32] = [73u8; 32];
        let (ada_device, joined_device) = (device_from_seed(&ada_seed), device_from_seed(&joined));
        let (nonce, epoch) = ([13u8; 16], 9);
        let link = mechanics::kinship::DeviceLink::assemble(
            (
                ada_device.clone(),
                mechanics::kinship::DeviceLink::half(&ada_seed, &joined_device, nonce, epoch),
            ),
            (
                joined_device.clone(),
                mechanics::kinship::DeviceLink::half(&joined, &ada_device, nonce, epoch),
            ),
            nonce,
            epoch,
        )
        .expect("assemble");
        ada.adopt_device(link).expect("adopt");
        let grown = ada
            .announce(Audience::Public, &bob.standing())
            .expect("announce");
        let change = bob
            .change_on_learning(&grown, &bob.standing())
            .expect("a device set that grew is a change");
        assert_eq!(change.profile, *ada.profile());
        assert!(
            change.incoming.len() > change.held.len(),
            "the change did not carry both sets: {change:?}"
        );

        // And asking is not learning — the answer must not have been absorbed
        // as a side effect of the question.
        assert_eq!(
            bob.resolve(ada.profile()).map(|d| d.len()),
            Some(change.held.len()),
            "asking about a change absorbed it"
        );
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
        let (a0, b0) = (mk(1), mk(3));

        let mut alice = ReachPlane::found_here(a0, None, None, now).expect("alice");
        let mut bob = ReachPlane::found_here(b0, None, None, now).expect("bob");

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
        let mut alice = ReachPlane::found_here(ALICE_A, None, None, NOW).expect("alice");
        let mut bob = ReachPlane::found_here(BOB_A, None, None, NOW).expect("bob");

        // Alice makes herself reachable to Bob; Bob learns her, anchored.
        let to_bob = Audience::Correspondent(Party::Device(device_from_seed(&BOB_A)));
        let announcement = alice.announce(to_bob, &bob.standing()).expect("announce");
        let learned = bob.learn(announcement, &bob.standing()).expect("learn");
        assert_eq!(&learned, alice.profile());

        // Bob resolves Alice and sends.
        assert_eq!(
            bob.resolve(alice.profile()),
            Some(vec![device_from_seed(&ALICE_A)]),
            "one device, and not the witness"
        );
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
        let mut alice = ReachPlane::found_here(ALICE_A, None, None, NOW).expect("alice");
        let stranger = ReachPlane::found_here(BOB_A, None, None, NOW).expect("bob");
        let mut carrier = MemCarrier::new();
        assert!(matches!(
            alice.send(&mut carrier, &stranger.profile().clone(), "hi", NOW),
            Err(Failure::NotReachable)
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

        let mut me = PostReach::found(mk(1), base, now).expect("found");
        me.send_self("a note to future me", now).expect("send self");
        let collected = me.collect(now);
        assert_eq!(
            collected.filed, 1,
            "the note came back over the deployed Post"
        );
        assert_eq!(collected.unasked, None, "the deployed Post answered");
        assert_eq!(me.messages()[0].1, "a note to future me");
    }

    /// The portrait rides the announcement rail: whoever can learn the
    /// devices learns the presentation, from the same anchored projection,
    /// and a later announcement supersedes it the same way the device set
    /// does.
    #[test]
    fn an_announcement_carries_the_portrait_to_whoever_can_learn_it() {
        let mut plane = ReachPlane::found_here([61u8; 32], None, None, NOW).expect("found");
        let reader = Standing {
            device: Some(device_from_seed(&[71u8; 32])),
            ..Standing::default()
        };

        let card = plane
            .announce_presenting(
                Audience::Public,
                &reader,
                &addressbook::Portrait {
                    name: Some("Alice".to_string()),
                    picture: Some([7u8; 32]),
                    detail: "keeps the lighthouse".to_string(),
                },
            )
            .expect("announce presenting");

        let mut theirs = Registry::new();
        theirs
            .absorb(card.projection, &card.genesis, &reader)
            .expect("absorb");
        assert_eq!(
            theirs.declared_name(plane.profile(), &reader).as_deref(),
            Some("Alice")
        );
        let portrait = theirs
            .portrait(plane.profile(), &reader)
            .expect("the portrait arrived with the devices");
        assert_eq!(portrait.picture, Some([7u8; 32]));
        assert_eq!(portrait.detail, "keeps the lighthouse");

        // A plain announce later does not erase the presentation — absence
        // of a portrait in one publication is not the cleared portrait.
        let replaced = plane.announce(Audience::Public, &reader).expect("announce");
        theirs
            .absorb(replaced.projection, &replaced.genesis, &reader)
            .expect("absorb again");
        assert!(
            theirs.portrait(plane.profile(), &reader).is_some(),
            "not presenting is not un-presenting"
        );
    }
}
