//! The kinship lookup registry: profiles held, resolved to device sets.
//!
//! A person is a set of per-Space personas; the one thing that links them is a
//! **profile** they opt into (`mechanics::kinship::ProfileId`, the content
//! address of a genesis [`DeviceLink`]). This holds the profiles this identity
//! knows and answers the one question the correspondence plane cannot proceed
//! without: *given a profile, which devices are it now?*
//!
//! # A device set has two faces, and the registry keeps them apart
//!
//! - The **owner's** own set is the mutual [`DeviceLink`]s of their log, an
//!   `Audience::Own` fact that never leaves the device. This identity resolves
//!   its *own* profiles that way, over [`KinshipLog::devices`].
//! - A **correspondent's** set is what they *avowed* reachable — signed
//!   `Claim::Profile` avowals whose audience includes this reader, committed by
//!   a projection whose head one of their devices signed. Linkage is avowed,
//!   never derived: a correspondent learns nothing the profile did not choose to
//!   tell them.
//!
//! So an *authored* holding resolves over its links; a *known* holding, absorbed
//! from a verified [`Projection`], resolves over its avowals. Both answer the
//! same `resolve` — which set you get depends only on whose profile it is.

use std::collections::BTreeMap;

use mechanics::ids::DeviceId;
use mechanics::kinship::{
    Claim, DeviceLink, Entry, KinshipLog, Party, ProfileId, Projection, Refusal, Standing,
};
use serde::{Deserialize, Serialize};

/// Why a registry operation did not apply.
#[derive(Debug)]
pub enum RegistryError {
    /// The operation named a profile this registry does not hold. Not an error
    /// about the profile — an error about *this* registry's knowledge of it.
    NotHeld,
    /// A kinship artifact refused verification.
    Kinship(Refusal),
    /// Persisted bytes could not be encoded or decoded.
    Codec,
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotHeld => f.write_str("no such profile is held"),
            Self::Kinship(refusal) => write!(f, "kinship refused: {refusal}"),
            Self::Codec => f.write_str("registry bytes are not decodable"),
        }
    }
}

impl std::error::Error for RegistryError {}

impl From<Refusal> for RegistryError {
    fn from(refusal: Refusal) -> Self {
        Self::Kinship(refusal)
    }
}

/// One profile, held with the trust its provenance earns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Holding {
    /// This identity's own profile: the full log, genesis and all, authored
    /// here. Resolves to the owner's private device set.
    Authored(KinshipLog),
    /// A correspondent's profile, absorbed from a verified projection. Carries
    /// only what that projection's audience admitted — never the private links —
    /// and resolves to the devices they avowed reachable.
    Known(Projection),
}

/// The profiles this identity holds, keyed by the self-certifying [`ProfileId`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    holdings: BTreeMap<ProfileId, Holding>,
}

impl Registry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Found — or re-find — one of *this identity's own* profiles from its
    /// genesis link, and hold it as authored.
    ///
    /// The returned id is the content address of the link, so it is not
    /// assignable and founding the same genesis twice is idempotent: the same
    /// id, and an existing authored log is left untouched (re-founding must not
    /// discard entries appended since).
    pub fn found(&mut self, genesis: DeviceLink) -> Result<ProfileId, RegistryError> {
        let log = KinshipLog::found(genesis)?;
        let profile = log.profile().clone();
        match self.holdings.get(&profile) {
            Some(Holding::Authored(_)) => {} // keep the fuller authored log
            _ => {
                self.holdings
                    .insert(profile.clone(), Holding::Authored(log));
            }
        }
        Ok(profile)
    }

    /// Append a verified entry to one of this identity's *authored* profiles.
    ///
    /// For the profiles this identity holds the seeds behind. A correspondent's
    /// entries never arrive this way — they come as a verified projection
    /// through [`Registry::absorb`] — so `extend` refuses a profile that is not
    /// held as authored rather than silently mutating a correspondent's view.
    pub fn extend(&mut self, profile: &ProfileId, entry: Entry) -> Result<(), RegistryError> {
        match self.holdings.get_mut(profile) {
            Some(Holding::Authored(log)) => {
                log.append(entry)?;
                Ok(())
            }
            _ => Err(RegistryError::NotHeld),
        }
    }

    /// Learn (or refresh) a correspondent's profile from a signed projection.
    ///
    /// The projection is admitted only if it *verifies* for this reader — head
    /// signed, every body listed in it, and no admissible body silently dropped.
    /// A newer projection (higher head epoch) replaces an older known one; an
    /// authored holding is never overwritten, because this identity's own log is
    /// always the fuller truth about its own profile.
    pub fn absorb(
        &mut self,
        projection: Projection,
        reader: &Standing,
    ) -> Result<ProfileId, RegistryError> {
        projection.verify(reader)?;
        // verify established the head is present; take its epoch for freshness.
        let epoch = projection.head.as_ref().map_or(0, |head| head.epoch);
        let profile = projection.profile.clone();
        match self.holdings.get(&profile) {
            Some(Holding::Authored(_)) => {} // never regress my own truth
            Some(Holding::Known(held)) => {
                let held_epoch = held.head.as_ref().map_or(0, |head| head.epoch);
                if epoch >= held_epoch {
                    self.holdings
                        .insert(profile.clone(), Holding::Known(projection));
                }
            }
            None => {
                self.holdings
                    .insert(profile.clone(), Holding::Known(projection));
            }
        }
        Ok(profile)
    }

    /// The device set a held profile resolves to now.
    ///
    /// An authored profile resolves to the owner's own devices (its live links);
    /// a known profile resolves to the devices the correspondent avowed
    /// reachable. `None` when the profile is not held — which a caller must never
    /// render as "the person has no devices".
    #[must_use]
    pub fn resolve(&self, profile: &ProfileId) -> Option<Vec<DeviceId>> {
        match self.holdings.get(profile)? {
            Holding::Authored(log) => Some(log.devices()),
            Holding::Known(projection) => Some(reachable_devices(profile, projection)),
        }
    }

    /// Whether this registry holds the profile at all.
    #[must_use]
    pub fn holds(&self, profile: &ProfileId) -> bool {
        self.holdings.contains_key(profile)
    }

    /// The held profiles.
    pub fn profiles(&self) -> impl Iterator<Item = &ProfileId> {
        self.holdings.keys()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.holdings.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.holdings.is_empty()
    }

    /// Serialize for durable storage. The caller owns where the bytes land; the
    /// registry owns their shape.
    pub fn to_bytes(&self) -> Result<Vec<u8>, RegistryError> {
        postcard::to_stdvec(self).map_err(|_| RegistryError::Codec)
    }

    /// Reconstruct from durable storage.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, RegistryError> {
        postcard::from_bytes(bytes).map_err(|_| RegistryError::Codec)
    }
}

/// The devices a correspondent avowed into `profile`, drawn from a projection's
/// already-admitted bodies.
///
/// Every body was verified and audience-checked when the projection was
/// absorbed, and inclusion in the signed head is the profile's own device
/// vouching for the membership — so the trust is the head signature, not each
/// avowal's signer. This collects the device subjects of the `Claim::Profile`
/// avowals that name this exact profile.
fn reachable_devices(profile: &ProfileId, projection: &Projection) -> Vec<DeviceId> {
    let mut devices: Vec<DeviceId> = Vec::new();
    for entry in &projection.bodies {
        let Entry::Avow(avowal) = entry else { continue };
        let Claim::Profile(claimed) = &avowal.claim else {
            continue;
        };
        if claimed != profile {
            continue;
        }
        if let Party::Device(device) = &avowal.subject {
            if !devices.contains(device) {
                devices.push(device.clone());
            }
        }
    }
    devices.sort();
    devices
}

#[cfg(test)]
mod tests {
    use super::*;
    use mechanics::actor::device_from_seed;
    use mechanics::kinship::{Audience, Avowal, Retirement};

    const A: [u8; 32] = [1u8; 32];
    const B: [u8; 32] = [2u8; 32];
    const C: [u8; 32] = [3u8; 32];
    const BOB: [u8; 32] = [40u8; 32];

    /// A genesis link between two of my devices, and the resolved-sorted set it
    /// stands for.
    fn genesis() -> (DeviceLink, Vec<DeviceId>) {
        let link = DeviceLink::seal(&A, &B, [7u8; 16], 1).expect("seal");
        let mut set = vec![device_from_seed(&A), device_from_seed(&B)];
        set.sort();
        (link, set)
    }

    /// Bob's reader standing: a correspondent named by his own device.
    fn bob_standing() -> Standing {
        Standing {
            device: Some(device_from_seed(&BOB)),
            ..Standing::default()
        }
    }

    #[test]
    fn founding_a_profile_resolves_to_its_two_devices() {
        let (link, set) = genesis();
        let mut registry = Registry::new();
        let profile = registry.found(link).expect("found");
        assert_eq!(registry.resolve(&profile).as_deref(), Some(set.as_slice()));
    }

    #[test]
    fn founding_the_same_genesis_twice_is_idempotent_and_keeps_appends() {
        let (link, _) = genesis();
        let mut registry = Registry::new();
        let profile = registry.found(link.clone()).expect("found");
        let third = DeviceLink::seal(&A, &C, [8u8; 16], 2).expect("seal");
        registry
            .extend(&profile, Entry::Link(third))
            .expect("extend");
        let again = registry.found(link).expect("re-found");
        assert_eq!(again, profile);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.resolve(&profile).map(|set| set.len()), Some(3));
    }

    #[test]
    fn a_link_adds_a_device_and_a_retirement_removes_one() {
        let (link, _) = genesis();
        let mut registry = Registry::new();
        let profile = registry.found(link).expect("found");
        let third = DeviceLink::seal(&A, &C, [8u8; 16], 2).expect("seal");
        registry
            .extend(&profile, Entry::Link(third))
            .expect("extend");
        assert_eq!(registry.resolve(&profile).map(|s| s.len()), Some(3));
        let retire = Retirement::seal(&A, device_from_seed(&B), 3, [9u8; 16]).expect("retire");
        registry
            .extend(&profile, Entry::Retire(retire))
            .expect("extend");
        let live = registry.resolve(&profile).expect("held");
        assert_eq!(live.len(), 2);
        assert!(!live.contains(&device_from_seed(&B)));
    }

    #[test]
    fn resolving_an_unheld_profile_is_none_never_empty() {
        let (link, _) = genesis();
        let mut held = Registry::new();
        let profile = held.found(link).expect("found");
        let empty = Registry::new();
        assert_eq!(empty.resolve(&profile), None);
        assert!(!empty.holds(&profile));
    }

    #[test]
    fn extending_a_profile_not_held_as_authored_refuses() {
        let (link, _) = genesis();
        let mut registry = Registry::new();
        let profile = registry.found(link).expect("found");
        let mut empty = Registry::new();
        let third = DeviceLink::seal(&A, &C, [8u8; 16], 2).expect("seal");
        assert!(matches!(
            empty.extend(&profile, Entry::Link(third)),
            Err(RegistryError::NotHeld)
        ));
    }

    #[test]
    fn round_trips_through_bytes() {
        let (link, set) = genesis();
        let mut registry = Registry::new();
        let profile = registry.found(link).expect("found");
        let bytes = registry.to_bytes().expect("encode");
        let back = Registry::from_bytes(&bytes).expect("decode");
        assert_eq!(back.resolve(&profile).as_deref(), Some(set.as_slice()));
    }

    // ── The reach path: a correspondent learns Alice's device set ──────────────

    /// Alice avows two of her devices reachable to Bob, projects to him; Bob
    /// absorbs and resolves Alice's profile to exactly those devices.
    #[test]
    fn a_correspondent_resolves_the_devices_avowed_to_them() {
        // Alice authors her profile.
        let (link, _) = genesis();
        let mut alice = Registry::new();
        let profile = alice.found(link).expect("found");

        // She avows A and B into her profile, to Bob specifically.
        let to_bob = Audience::Correspondent(Party::Device(device_from_seed(&BOB)));
        for (seed, device) in [(&A, &A), (&A, &B)] {
            let avowal = Avowal::seal(
                seed,
                Party::Device(device_from_seed(device)),
                Claim::Profile(profile.clone()),
                to_bob.clone(),
                5,
                [1u8; 16],
            )
            .expect("avow");
            alice.extend(&profile, Entry::Avow(avowal)).expect("extend");
        }

        // She projects to Bob's standing and hands it over.
        let Holding::Authored(log) = alice.holdings.get(&profile).unwrap() else {
            panic!("authored");
        };
        let projection = log.project(&A, 5, &bob_standing()).expect("project");

        // Bob absorbs and resolves.
        let mut bob = Registry::new();
        let learned = bob.absorb(projection, &bob_standing()).expect("absorb");
        assert_eq!(learned, profile);
        let mut expected = vec![device_from_seed(&A), device_from_seed(&B)];
        expected.sort();
        assert_eq!(bob.resolve(&profile).as_deref(), Some(expected.as_slice()));
    }

    /// A projection Bob is not the audience of carries no avowal bodies, so he
    /// resolves an empty reachable set — never the owner's private links.
    #[test]
    fn a_stranger_absorbs_a_projection_but_reaches_nothing() {
        let (link, _) = genesis();
        let mut alice = Registry::new();
        let profile = alice.found(link).expect("found");
        let to_bob = Audience::Correspondent(Party::Device(device_from_seed(&BOB)));
        let avowal = Avowal::seal(
            &A,
            Party::Device(device_from_seed(&A)),
            Claim::Profile(profile.clone()),
            to_bob,
            5,
            [1u8; 16],
        )
        .expect("avow");
        alice.extend(&profile, Entry::Avow(avowal)).expect("extend");

        // Project to a stranger (not Bob): the correspondent avowal is filtered
        // out, and the private links were never admissible to begin with.
        let stranger = Standing {
            device: Some(device_from_seed(&C)),
            ..Standing::default()
        };
        let Holding::Authored(log) = alice.holdings.get(&profile).unwrap() else {
            panic!("authored");
        };
        let projection = log.project(&A, 5, &stranger).expect("project");

        let mut them = Registry::new();
        them.absorb(projection, &stranger).expect("absorb");
        assert_eq!(
            them.resolve(&profile),
            Some(Vec::new()),
            "a stranger reaches nothing, and the owner's links never leaked"
        );
    }

    /// A newer projection replaces an older known one; an older one does not
    /// regress it.
    #[test]
    fn absorb_takes_the_newer_projection_and_never_regresses() {
        let (link, _) = genesis();
        let mut alice = Registry::new();
        let profile = alice.found(link).expect("found");
        let to_bob = Audience::Correspondent(Party::Device(device_from_seed(&BOB)));

        // Epoch 5: only A reachable.
        let a5 = Avowal::seal(
            &A,
            Party::Device(device_from_seed(&A)),
            Claim::Profile(profile.clone()),
            to_bob.clone(),
            5,
            [1u8; 16],
        )
        .expect("avow");
        alice.extend(&profile, Entry::Avow(a5)).expect("extend");
        let Holding::Authored(log5) = alice.holdings.get(&profile).unwrap().clone() else {
            panic!()
        };
        let p5 = log5.project(&A, 5, &bob_standing()).expect("project");

        // Epoch 9: A and B reachable.
        let b9 = Avowal::seal(
            &A,
            Party::Device(device_from_seed(&B)),
            Claim::Profile(profile.clone()),
            to_bob,
            9,
            [2u8; 16],
        )
        .expect("avow");
        alice.extend(&profile, Entry::Avow(b9)).expect("extend");
        let Holding::Authored(log9) = alice.holdings.get(&profile).unwrap() else {
            panic!()
        };
        let p9 = log9.project(&A, 9, &bob_standing()).expect("project");

        let mut bob = Registry::new();
        bob.absorb(p9, &bob_standing()).expect("absorb newer");
        assert_eq!(bob.resolve(&profile).map(|s| s.len()), Some(2));
        // Absorbing the older projection must not shrink Bob's view.
        bob.absorb(p5, &bob_standing()).expect("absorb older");
        assert_eq!(
            bob.resolve(&profile).map(|s| s.len()),
            Some(2),
            "an older projection does not regress the known set"
        );
    }
}
