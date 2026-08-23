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
    Audience, Avowal, Claim, DeviceLink, Entry, KinshipLog, Party, ProfileId, Projection, Refusal,
    Standing,
};
use serde::{Deserialize, Serialize};

/// Why a registry operation did not apply.
///
/// Named for what it is and qualified by where it lives: callers say
/// `registry::Failure`, so the module supplies the noun the type does not
/// have to repeat.
#[derive(Debug)]
pub enum Failure {
    /// The operation named a profile this registry does not hold. Not an error
    /// about the profile — an error about *this* registry's knowledge of it.
    NotHeld,
    /// A kinship artifact refused verification.
    Kinship(Refusal),
    /// The projection could not be anchored to the profile: the genesis link
    /// does not hash to the claimed profile, or the head was signed by a device
    /// the genesis does not name. Without this check a holder of a public
    /// profile id could forge a projection and substitute the device set.
    Unanchored,
    /// Persisted bytes could not be encoded or decoded.
    Codec,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotHeld => f.write_str("no such profile is held"),
            Self::Kinship(refusal) => write!(f, "kinship refused: {refusal}"),
            Self::Unanchored => {
                f.write_str("the projection is not anchored to the profile's genesis")
            }
            Self::Codec => f.write_str("registry bytes are not decodable"),
        }
    }
}

impl std::error::Error for Failure {}

impl From<Refusal> for Failure {
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
    pub fn found(&mut self, genesis: DeviceLink) -> Result<ProfileId, Failure> {
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
    pub fn extend(&mut self, profile: &ProfileId, entry: Entry) -> Result<(), Failure> {
        match self.holdings.get_mut(profile) {
            Some(Holding::Authored(log)) => {
                log.append(entry)?;
                Ok(())
            }
            _ => Err(Failure::NotHeld),
        }
    }

    /// Learn (or refresh) a correspondent's profile from a signed projection,
    /// anchored to its self-certifying genesis.
    ///
    /// The `genesis` link is the anchor a correspondent is handed alongside the
    /// projection. Three things must hold before a single device is believed,
    /// and the first two are what stop a holder of the public profile id from
    /// forging a device set:
    ///
    /// 1. the genesis hashes to exactly this profile (`ProfileId::from_genesis`),
    /// 2. the projection's head was signed by a device the genesis *names* — so
    ///    the vouching key is provably one of the profile's own roots, not any
    ///    key an attacker minted,
    /// 3. the projection then verifies for this reader — head signed, every body
    ///    listed, no admissible body silently dropped.
    ///
    /// A newer projection (higher head epoch) replaces an older known one; an
    /// authored holding is never overwritten.
    pub fn absorb(
        &mut self,
        projection: Projection,
        genesis: &DeviceLink,
        reader: &Standing,
    ) -> Result<ProfileId, Failure> {
        // Anchor first: the head signer must be a genesis root of *this* profile.
        genesis.verify()?;
        let bytes = postcard::to_stdvec(genesis).map_err(|_| Failure::Codec)?;
        if ProfileId::from_genesis(&bytes) != projection.profile {
            return Err(Failure::Unanchored);
        }
        let head = projection.head.as_ref().ok_or(Failure::Unanchored)?;
        // The anchor, widened to the chain: a genesis root passes as it always
        // did, and a joined device passes when the projection carries the
        // links that root it — verified on their own signatures, retire-wins,
        // and only committable entries reach this point because `verify`
        // below refuses anything the head does not list. A stranger with no
        // chain to carry is refused exactly as before.
        if !genesis.devices.contains(&head.by)
            && !mechanics::kinship::signer_rooted(genesis, &projection.bodies, &head.by)
        {
            return Err(Failure::Unanchored);
        }

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

    /// Avow this identity's live devices reachable to `audience`, so a
    /// correspondent can learn them from a projection.
    ///
    /// The `Own` device links never leave the device; this is how a profile
    /// *chooses* what a correspondent may see. One of the owner's devices signs
    /// (`seed`), which is the profile vouching for the set — the head over the
    /// resulting log carries that signature, and that is what the reader trusts.
    ///
    /// The **whole** live set is avowed today: the "flat, and say so" answer to
    /// CORR-27. Whether a person's device set has a correspondence *subset* is a
    /// decision that is deliberately not made here; when it is, this is the one
    /// call that changes. Returns how many devices were newly avowed.
    pub fn avow_reachable(
        &mut self,
        profile: &ProfileId,
        audience: Audience,
        seed: &[u8; 32],
        epoch: u64,
        nonce: [u8; 16],
    ) -> Result<usize, Failure> {
        let devices = match self.holdings.get(profile) {
            Some(Holding::Authored(log)) => log.devices(),
            _ => return Err(Failure::NotHeld),
        };
        let mut avowed = 0usize;
        for (index, device) in devices.into_iter().enumerate() {
            // A distinct nonce per device, so two avowals are two entries rather
            // than one. Bounded by the device-set bound the log already enforces.
            let mut per = nonce;
            per[15] ^= u8::try_from(index & 0xff).unwrap_or(0);
            let avowal = Avowal::seal(
                seed,
                Party::Device(device),
                Claim::Profile(profile.clone()),
                audience.clone(),
                epoch,
                per,
            )?;
            self.extend(profile, Entry::Avow(avowal))?;
            avowed = avowed.saturating_add(1);
        }
        Ok(avowed)
    }

    /// Project one of this identity's authored profiles for a reader — the
    /// audience-scoped bodies plus a signed head over the whole log, ready to
    /// hand a correspondent who will [`Registry::absorb`] it.
    pub fn project(
        &self,
        profile: &ProfileId,
        seed: &[u8; 32],
        epoch: u64,
        reader: &Standing,
    ) -> Result<Projection, Failure> {
        match self.holdings.get(profile) {
            Some(Holding::Authored(log)) => Ok(log.project(seed, epoch, reader)?),
            _ => Err(Failure::NotHeld),
        }
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
    pub fn to_bytes(&self) -> Result<Vec<u8>, Failure> {
        postcard::to_stdvec(self).map_err(|_| Failure::Codec)
    }

    /// Reconstruct from durable storage.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Failure> {
        postcard::from_bytes(bytes).map_err(|_| Failure::Codec)
    }
}

/// The devices a correspondent avowed into `profile` in their **latest
/// publication**.
///
/// Every body was verified and audience-checked when the projection was
/// absorbed, and the head signer was anchored to a genesis root — so the trust
/// is that anchored head, not each avowal's signer. This collects the device
/// subjects of the `Claim::Profile` avowals naming this profile *at the highest
/// avowal epoch present*, not the union across epochs.
///
/// Taking only the latest publication is what lets a set **shrink**: a device
/// retired from the owner's own links is `Own`-audience and never reaches a
/// correspondent, so the correspondent learns the removal the one way it can —
/// the owner re-avows the whole live set at a new epoch, and that publication
/// supersedes the old. Unioning across epochs would make a correspondent's set
/// grow forever and never drop a compromised device.
fn reachable_devices(profile: &ProfileId, projection: &Projection) -> Vec<DeviceId> {
    // The epoch of the most recent Claim::Profile avowal for this profile.
    let latest = projection
        .bodies
        .iter()
        .filter_map(|entry| match entry {
            Entry::Avow(avowal) => match &avowal.claim {
                Claim::Profile(claimed) if claimed == profile => Some(avowal.epoch),
                _ => None,
            },
            _ => None,
        })
        .max();
    let Some(latest) = latest else {
        return Vec::new();
    };

    let mut devices: Vec<DeviceId> = Vec::new();
    for entry in &projection.bodies {
        let Entry::Avow(avowal) = entry else { continue };
        let Claim::Profile(claimed) = &avowal.claim else {
            continue;
        };
        if claimed != profile || avowal.epoch != latest {
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
            Err(Failure::NotHeld)
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
        let profile = alice.found(link.clone()).expect("found");

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
        let learned = bob
            .absorb(projection, &link, &bob_standing())
            .expect("absorb");
        assert_eq!(learned, profile);
        let mut expected = vec![device_from_seed(&A), device_from_seed(&B)];
        expected.sort();
        assert_eq!(bob.resolve(&profile).as_deref(), Some(expected.as_slice()));
    }

    /// The whole reach handshake in the two calls a caller actually makes: Alice
    /// avows her set reachable to Bob and projects; Bob absorbs and resolves.
    #[test]
    fn the_reach_handshake_in_two_calls() {
        let (link, _) = genesis();
        let mut alice = Registry::new();
        let profile = alice.found(link.clone()).expect("found");
        // A third device, so the avowed set is more than the genesis pair.
        let third = DeviceLink::seal(&A, &C, [8u8; 16], 2).expect("seal");
        alice
            .extend(&profile, Entry::Link(third.clone()))
            .expect("extend");

        let to_bob = Audience::Correspondent(Party::Device(device_from_seed(&BOB)));
        let n = alice
            .avow_reachable(&profile, to_bob, &A, 5, [3u8; 16])
            .expect("avow");
        assert_eq!(n, 3, "all three live devices avowed");

        let projection = alice
            .project(&profile, &A, 5, &bob_standing())
            .expect("project");

        let mut bob = Registry::new();
        bob.absorb(projection, &link, &bob_standing())
            .expect("absorb");

        let mut expected = vec![
            device_from_seed(&A),
            device_from_seed(&B),
            device_from_seed(&C),
        ];
        expected.sort();
        assert_eq!(bob.resolve(&profile).as_deref(), Some(expected.as_slice()));
    }

    /// A retired device is not avowed reachable — the correspondence set follows
    /// the live set.
    #[test]
    fn avow_reachable_follows_the_live_set() {
        let (link, _) = genesis();
        let mut alice = Registry::new();
        let profile = alice.found(link.clone()).expect("found");
        let retire = Retirement::seal(&A, device_from_seed(&B), 3, [9u8; 16]).expect("retire");
        alice
            .extend(&profile, Entry::Retire(retire))
            .expect("extend");

        let to_bob = Audience::Correspondent(Party::Device(device_from_seed(&BOB)));
        let n = alice
            .avow_reachable(&profile, to_bob, &A, 5, [3u8; 16])
            .expect("avow");
        assert_eq!(n, 1, "only A remains live");

        let projection = alice
            .project(&profile, &A, 5, &bob_standing())
            .expect("project");
        let mut bob = Registry::new();
        bob.absorb(projection, &link, &bob_standing())
            .expect("absorb");
        assert_eq!(
            bob.resolve(&profile).as_deref(),
            Some([device_from_seed(&A)].as_slice())
        );
    }

    /// A projection Bob is not the audience of carries no avowal bodies, so he
    /// resolves an empty reachable set — never the owner's private links.
    #[test]
    fn a_stranger_absorbs_a_projection_but_reaches_nothing() {
        let (link, _) = genesis();
        let mut alice = Registry::new();
        let profile = alice.found(link.clone()).expect("found");
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
        them.absorb(projection, &link, &stranger).expect("absorb");
        assert_eq!(
            them.resolve(&profile),
            Some(Vec::new()),
            "a stranger reaches nothing, and the owner's links never leaked"
        );
    }

    /// A newer projection replaces an older known one; an older one does not
    /// regress it. Each publication avows the whole live set at one epoch, so a
    /// device that joined between them is present in the newer and absent in the
    /// older — and absorbing the older must not shrink the known set.
    #[test]
    fn absorb_takes_the_newer_projection_and_never_regresses() {
        let (link, _) = genesis();
        let mut alice = Registry::new();
        let profile = alice.found(link.clone()).expect("found");
        let to_bob = Audience::Correspondent(Party::Device(device_from_seed(&BOB)));

        // Epoch 5: the live pair {A, B} avowed and projected.
        alice
            .avow_reachable(&profile, to_bob.clone(), &A, 5, [1u8; 16])
            .expect("avow");
        let p5 = alice
            .project(&profile, &A, 5, &bob_standing())
            .expect("project");

        // A device joins, and the fuller set {A, B, C} is avowed at a later epoch.
        let third = DeviceLink::seal(&A, &C, [8u8; 16], 2).expect("seal");
        alice
            .extend(&profile, Entry::Link(third.clone()))
            .expect("extend");
        alice
            .avow_reachable(&profile, to_bob, &A, 9, [2u8; 16])
            .expect("avow");
        let p9 = alice
            .project(&profile, &A, 9, &bob_standing())
            .expect("project");

        let mut bob = Registry::new();
        bob.absorb(p9, &link, &bob_standing())
            .expect("absorb newer");
        assert_eq!(bob.resolve(&profile).map(|s| s.len()), Some(3));
        // Absorbing the older projection must not shrink Bob's view back to two.
        bob.absorb(p5, &link, &bob_standing())
            .expect("absorb older");
        assert_eq!(
            bob.resolve(&profile).map(|s| s.len()),
            Some(3),
            "an older projection does not regress the known set"
        );
    }

    // ── The anchor: a forged or mis-signed projection cannot substitute devices ──

    /// The anchor, as device-join left it. (Finding 0, revised.)
    ///
    /// A device the genesis roots *through a consented chain* may head a
    /// projection — the chain rides with it and `signer_rooted` walks it, so
    /// a joined device is not second-class. What Finding 0 actually guards —
    /// an attacker-controlled device injecting a device set — still holds
    /// with the anchor's whole force: every hop of a chain is co-signed by an
    /// already-rooted device, so a head from a device with no consented chain
    /// is refused and teaches a reader nothing.
    #[test]
    fn the_anchor_admits_a_consented_chain_and_nothing_else() {
        let (link, _) = genesis();
        let mut alice = Registry::new();
        let profile = alice.found(link.clone()).expect("found");
        // C is a real device of Alice's, added by a link with a root's consent.
        let third = DeviceLink::seal(&A, &C, [8u8; 16], 2).expect("seal");
        alice
            .extend(&profile, Entry::Link(third.clone()))
            .expect("extend");
        let to_bob = Audience::Correspondent(Party::Device(device_from_seed(&BOB)));
        alice
            .avow_reachable(&profile, to_bob, &A, 5, [3u8; 16])
            .expect("avow");

        // Alice signs the projection's head with C: consented, chained, taken.
        let projection = alice
            .project(&profile, &C, 5, &bob_standing())
            .expect("project");
        assert_eq!(projection.head.as_ref().unwrap().by, device_from_seed(&C));
        let mut bob = Registry::new();
        bob.absorb(projection, &link, &bob_standing())
            .expect("a consented chain heads a projection every reader takes");
        assert!(bob.holds(&profile));

        // An attacker's device has no chain to carry: a head it signs is
        // refused even when the projection is otherwise well-formed.
        let mut mallory = Registry::new();
        let stolen_profile = mallory
            .found(link.clone())
            .expect("found from public genesis");
        let intruder: [u8; 32] = [66u8; 32];
        // The whole chain must be carried: without A↔C her copy roots
        // nothing past the genesis, and absorb refuses (tried; it does).
        mallory
            .extend(&stolen_profile, Entry::Link(third))
            .expect("mallory carries the real link");
        let fake_link = DeviceLink::seal(&C, &intruder, [9u8; 16], 3).expect("seal");
        mallory
            .extend(&stolen_profile, Entry::Link(fake_link))
            .expect("mallory extends her own copy");
        let forged = mallory
            .project(&stolen_profile, &intruder, 6, &bob_standing())
            .expect("project");
        // C consented to the intruder, so this chain IS valid — which is the
        // point: chain admission is exactly device consent, no more, and a
        // "theft" that required a rooted device's signature was that device's
        // act. Without the consent there is nothing to carry:
        let mut carol = Registry::new();
        carol
            .absorb(forged, &link, &bob_standing())
            .expect("a chain through a consenting rooted device is that device's act");
        let lone: [u8; 32] = [77u8; 32];
        let mut walkin = Registry::new();
        let unrooted_profile = walkin.found(link.clone()).expect("found");
        // No link at all: `project` signs with whatever seed it is handed —
        // that was always true — and the refusal lands where it must, at
        // every reader's absorb.
        let headless_authority = walkin
            .project(&unrooted_profile, &lone, 7, &bob_standing())
            .expect("project signs; rootedness is the reader's check");
        let mut dana = Registry::new();
        assert!(matches!(
            dana.absorb(headless_authority, &link, &bob_standing()),
            Err(Failure::Unanchored)
        ));
        assert!(
            !dana.holds(&unrooted_profile),
            "nothing was learned from a chainless head"
        );
    }

    /// A projection for one profile cannot be anchored with a different
    /// genesis: the genesis must hash to the very profile it claims. An attacker
    /// holding a public profile id cannot present their own genesis for it.
    #[test]
    fn a_mismatched_genesis_is_unanchored() {
        // Alice's real profile and a valid projection for it.
        let alice_link = DeviceLink::seal(&A, &B, [7u8; 16], 1).expect("link");
        let mut alice = Registry::new();
        let profile = alice.found(alice_link).expect("found");
        let to_bob = Audience::Correspondent(Party::Device(device_from_seed(&BOB)));
        alice
            .avow_reachable(&profile, to_bob, &A, 5, [3u8; 16])
            .expect("avow");
        let projection = alice
            .project(&profile, &A, 5, &bob_standing())
            .expect("project");

        // Mallory presents a genesis of his own — it hashes to a different id.
        let mallory_link = DeviceLink::seal(&C, &BOB, [1u8; 16], 1).expect("link");
        let mut bob = Registry::new();
        assert!(matches!(
            bob.absorb(projection, &mallory_link, &bob_standing()),
            Err(Failure::Unanchored)
        ));
    }

    /// Retire a device *after* it was avowed reachable: the owner re-avows the
    /// smaller live set at a new epoch, and the correspondent's resolution
    /// shrinks to it. Without latest-publication semantics the retired device
    /// would linger in Bob's set forever. (Q2.)
    #[test]
    fn a_device_retired_after_avowing_is_dropped_on_the_next_publication() {
        let (link, _) = genesis();
        let mut alice = Registry::new();
        let profile = alice.found(link.clone()).expect("found");
        let to_bob = Audience::Correspondent(Party::Device(device_from_seed(&BOB)));

        // Epoch 5: both A and B avowed reachable.
        alice
            .avow_reachable(&profile, to_bob.clone(), &A, 5, [3u8; 16])
            .expect("avow");
        let mut bob = Registry::new();
        let p5 = alice
            .project(&profile, &A, 5, &bob_standing())
            .expect("project");
        bob.absorb(p5, &link, &bob_standing()).expect("absorb");
        assert_eq!(bob.resolve(&profile).map(|s| s.len()), Some(2), "A and B");

        // B is retired (Own — never reaches Bob), then the live set is re-avowed.
        let retire = Retirement::seal(&A, device_from_seed(&B), 6, [9u8; 16]).expect("retire");
        alice
            .extend(&profile, Entry::Retire(retire))
            .expect("extend");
        alice
            .avow_reachable(&profile, to_bob, &A, 9, [4u8; 16])
            .expect("re-avow");
        let p9 = alice
            .project(&profile, &A, 9, &bob_standing())
            .expect("project");
        bob.absorb(p9, &link, &bob_standing())
            .expect("absorb newer");

        let live = bob.resolve(&profile).expect("held");
        assert_eq!(
            live.as_slice(),
            [device_from_seed(&A)].as_slice(),
            "B dropped"
        );
    }
}
