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
    pub fn found(seeds: Vec<[u8; 32]>, now: u64) -> Result<Self, ReachError> {
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
        let letter = Letter::compose(
            &self.seeds[0],
            Content::Message {
                body: body.to_owned(),
            },
            now,
        );
        let sealed = letter
            .seal_to_devices(&devices, &addressed, now + RETENTION)
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

    /// A single device cannot found a profile — a set is a mutual link.
    #[test]
    fn one_device_cannot_found_a_profile() {
        assert!(matches!(
            ReachPlane::found(vec![ALICE_A], NOW),
            Err(ReachError::TooFewDevices)
        ));
    }
}
