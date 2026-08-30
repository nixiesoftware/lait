//! The directory, as behaviour rather than as transport.
//!
//! Everything AUTH-16, AUTH-17, AUTH-18 and AUTH-24 ask for lands here, so the
//! HTTP surface is a decoder and the store is a place to put bytes.

use addressbook::Announcement;
use mechanics::{
    ids::DeviceId,
    kinship::{ProfileId, Standing},
};

use crate::{
    address::Address,
    bounds,
    chronicle::{Receipt, SharedChronicler},
    store::{Published, Store},
    wire::{self, framed, Challenge, SignedPublish, SignedResolve},
    Issued, Refusal,
};

/// How many spellings a mint will try before giving up.
///
/// Collision is astronomically unlikely — the occupied fraction of a 2^46 space
/// is negligible, which is the same fact the no-enumeration position rests on —
/// so this is a bound on a loop rather than a real retry policy. It exists
/// because an unbounded loop against a store that is answering "taken" for some
/// other reason would spin forever.
const MINT_ATTEMPTS: usize = 8;

/// The directory service.
///
/// Holds a store and no key. Every authorization decision it makes is a
/// signature check over material the caller signed, which is what keeps a
/// compromise a *denial* — detectable — rather than an impersonation, which
/// would not be.
pub struct Service<S: Store> {
    store: S,
    /// The deployment's chronicle, when it keeps one. Optional because a
    /// directory that records nothing is a directory that certifies nobody,
    /// which is a smaller service rather than a broken one — and because the
    /// answer it then gives is an empty [`Receipt`], never a refusal.
    chronicler: Option<SharedChronicler>,
}

impl<S: Store> Service<S> {
    pub const fn new(store: S) -> Self {
        Self {
            store,
            chronicler: None,
        }
    }

    /// A directory that chronicles what it accepts, into the log the whole
    /// deployment shares.
    ///
    /// The same chronicler the label registry holds, deliberately: an identity
    /// that never chose a label publishes a device set and nothing else, and a
    /// registry-only chronicle would certify only the people who happened to
    /// want a name.
    #[must_use]
    pub fn with_chronicler(store: S, chronicler: SharedChronicler) -> Self {
        Self {
            store,
            chronicler: Some(chronicler),
        }
    }

    /// Issue a single-use nonce for `device`.
    ///
    /// Free and unauthenticated: anyone may ask for one, for any device. It is
    /// the *answer* that must be signed, so an attacker collecting challenges
    /// for somebody else's device collects nonces and nothing else.
    pub fn challenge(&mut self, device: &DeviceId, now: u64) -> Result<Challenge, Refusal> {
        wire::canonical_key(device)?;
        if self.store.open_for(device, now).map_err(unavailable)?
            >= bounds::MAX_CHALLENGES_PER_DEVICE
        {
            return Err(Refusal::TooFast);
        }
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce).map_err(|error| {
            tracing::error!(%error, "OS randomness unavailable");
            Refusal::Unavailable("randomness".into())
        })?;
        let challenge = Challenge {
            device: device.clone(),
            nonce,
            issued_at: now,
        };
        self.store.open(&challenge).map_err(unavailable)?;
        Ok(challenge)
    }

    /// Publish a profile's signed device events and answer the address it holds.
    ///
    /// The verification is the whole point of AUTH-24: this service accepts
    /// events *signed under their own domain by the devices being bound*, checks
    /// them on their own terms, and stores what it is handed. It never authors,
    /// which is why no account-side path can change a published device set.
    pub fn publish(&mut self, request: &SignedPublish, now: u64) -> Result<Issued, Refusal> {
        if request.announcement.len() > bounds::MAX_PUBLISH_BYTES {
            return Err(Refusal::TooLarge);
        }
        wire::verify(&request.device, &request.preimage(), &request.signature)?;
        self.answered(&request.device, &request.nonce, now)?;

        let announcement =
            Announcement::decode(&request.announcement).map_err(|_| Refusal::NotAuthentic)?;
        let (profile, devices, epoch) = anchored(&announcement)?;

        // The presenter has to be one of the devices this announcement avows.
        // Without it, anyone who ever *saw* an announcement could re-present it,
        // and a stranger would be able to keep a profile's entry alive — or, with
        // a captured older one, argue about which is current.
        if !devices.contains(&request.device) {
            return Err(Refusal::NotAuthentic);
        }

        let address = match self.store.address_of(&profile).map_err(unavailable)? {
            Some(held) => held,
            None => self.mint_for(&profile)?,
        };
        // Chronicled before the record goes live, so the log is a superset of
        // what this service serves rather than a lagging shadow of it, and the
        // marks name the devices *this* publication avowed — which is what
        // makes the next receipt able to withdraw one.
        let receipt = self.chronicled(request, &devices)?;
        self.store
            .record(
                &profile,
                &Published {
                    announcement: request.announcement.clone(),
                    epoch,
                },
            )
            .map_err(unavailable)?;
        Ok(Issued { address, receipt })
    }

    /// Append this publication to the shared chronicle, marking every device it
    /// avows — or answer an empty receipt where no chronicle is kept.
    ///
    /// An empty receipt is not a refusal and must never be rendered as one: a
    /// deployment without a chronicler records nothing, which is different from
    /// a chronicler that could not answer (that is [`Refusal::Unavailable`],
    /// and it fails the publish rather than quietly certifying nobody).
    fn chronicled(
        &self,
        request: &SignedPublish,
        devices: &[DeviceId],
    ) -> Result<Receipt, Refusal> {
        let Some(chronicler) = &self.chronicler else {
            return Ok(Receipt::default());
        };
        crate::chronicle::held(chronicler).chronicle(&chronicle_entry_for(request), devices)
    }

    /// Resolve one exact address to the announcement its profile published.
    ///
    /// Exact only. No listing, no prefix, no existence oracle — and the refusal
    /// for "nobody holds this" is the same value as for "you may not ask",
    /// because two distinguishable answers are an oracle however carefully they
    /// are worded.
    pub fn resolve(&mut self, request: &SignedResolve, now: u64) -> Result<Vec<u8>, Refusal> {
        let address = request.parsed_address()?;
        wire::verify(&request.device, &request.preimage(), &request.signature)?;
        self.answered(&request.device, &request.nonce, now)?;

        // Counted before the lookup, and counted on a miss. A rate limit that
        // only charged for hits would make probing free, which is exactly the
        // budget a prober wants.
        //
        // Stated honestly: this bounds one *proven* asker, and an attacker can
        // mint fresh device keys for free, so it does not bound an attacker. The
        // defence against enumeration is sparseness (see `address::keyspace`);
        // this is the second layer AUTH-16 asks for, not the first.
        if self
            .store
            .note_resolve(&request.device, now)
            .map_err(unavailable)?
            > bounds::MAX_RESOLVES_PER_WINDOW
        {
            return Err(Refusal::TooFast);
        }

        self.store
            .published(&address)
            .map_err(unavailable)?
            .map(|published| published.announcement)
            .ok_or(Refusal::NotAvailable)
    }

    /// Drop expired challenges and stale rate windows.
    pub fn sweep(&mut self, now: u64) -> usize {
        self.store.sweep(now).unwrap_or(0)
    }

    /// Spend the challenge this request answers, insisting it was issued to the
    /// device presenting it.
    ///
    /// A challenge issued to one device and answered by another is refused as
    /// stale rather than as a mismatch: whether a given nonce exists is not a
    /// fact worth confirming to whoever is asking.
    ///
    /// **Called after the signature check, deliberately.** Spending is
    /// destructive, so doing it first would let anyone who merely *saw* a nonce
    /// burn it with a garbage signature and deny the holder — and with only
    /// [`bounds::MAX_CHALLENGES_PER_DEVICE`] open at once, that is a cheap way to
    /// stop somebody publishing. Verifying first means burning a nonce costs a
    /// forged signature, which is the whole point of having one.
    fn answered(&mut self, device: &DeviceId, nonce: &[u8; 32], now: u64) -> Result<(), Refusal> {
        let spent = self
            .store
            .spend(nonce, now)
            .map_err(unavailable)?
            .ok_or(Refusal::StaleChallenge)?;
        if spent.device != *device {
            return Err(Refusal::StaleChallenge);
        }
        Ok(())
    }

    /// Mint an address nobody holds and claim it for `profile`.
    fn mint_for(&mut self, profile: &ProfileId) -> Result<Address, Refusal> {
        for _ in 0..MINT_ATTEMPTS {
            let mut entropy = [0u8; 16];
            getrandom::fill(&mut entropy).map_err(|error| {
                tracing::error!(%error, "OS randomness unavailable");
                Refusal::Unavailable("randomness".into())
            })?;
            let candidate = Address::mint(&entropy);
            if self.store.claim(&candidate, profile).map_err(unavailable)? {
                return Ok(candidate);
            }
        }
        Err(Refusal::Unavailable("could not mint an address".into()))
    }
}

/// Domain separator for a chronicle entry over a device-set publication.
/// Distinct from the registry's, so one log holding both kinds can never read
/// an entry of one as an entry of the other.
const CHRONICLE_ENTRY_DOMAIN: &[u8] = b"lait-directory/chronicle-entry/v1";

/// The canonical bytes one accepted publication contributes to the chronicle.
///
/// Deterministic and serde-free, so the publisher recomputes the same leaf from
/// the request it signed. That recomputation is the only way anybody but this
/// service proves a mark is about *them*: a mark on its own establishes the
/// marker's memory — something was recorded, there — and the leaf is what binds
/// that entry to a publication somebody holds the bytes of.
#[must_use]
pub fn chronicle_entry_for(request: &SignedPublish) -> Vec<u8> {
    let mut out = Vec::with_capacity(160_usize.saturating_add(request.announcement.len()));
    framed(&mut out, CHRONICLE_ENTRY_DOMAIN);
    framed(&mut out, request.device.as_str().as_bytes());
    framed(&mut out, &request.nonce);
    framed(&mut out, &request.announcement);
    framed(&mut out, &request.signature);
    out
}

/// Verify an announcement on its own terms, and answer what it establishes.
///
/// A scratch registry, deliberately: `absorb` is the one implementation of
/// "anchor this projection to its genesis and check the head", and a second one
/// here would be a second thing to keep right. The registry is discarded, so the
/// service learns nothing and keeps nothing — it is a verifier, not a reader.
///
/// The reader is `Standing::default()`, which is the public audience: no device,
/// no actor, no Space. That is what the directory is, and it is why only material
/// avowed to `Audience::Public` is visible in what it stores.
pub(crate) fn anchored(
    announcement: &Announcement,
) -> Result<(ProfileId, Vec<DeviceId>, u64), Refusal> {
    let mut scratch = addressbook::Registry::new();
    let profile = scratch
        .absorb(
            announcement.projection.clone(),
            &announcement.genesis,
            &Standing::default(),
        )
        .map_err(|_| Refusal::NotAuthentic)?;
    if profile != announcement.profile {
        return Err(Refusal::NotAuthentic);
    }
    let devices = scratch.resolve(&profile).ok_or(Refusal::NotAuthentic)?;
    if devices.is_empty() {
        return Err(Refusal::NotAuthentic);
    }
    let epoch = announcement
        .projection
        .head
        .as_ref()
        .map_or(0, |head| head.epoch);
    Ok((profile, devices, epoch))
}

/// A store that could not answer is never a "no".
///
/// The distinction `correspondence::Missed` draws one layer up, kept here for the
/// same reason: "nobody holds this address" and "we could not look" are different
/// facts, and a caller shown the first when the second is true has been told
/// somebody does not exist.
fn unavailable(error: anyhow::Error) -> Refusal {
    tracing::warn!(%error, "the directory store could not answer");
    Refusal::Unavailable("store".into())
}

/// One directory that several callers both reach.
///
/// The shape a deployed service has, without one — and the same reasoning
/// `correspondence::SharedMem` gives for existing beside `MemCarrier`: a
/// directory is a place two people both publish into and resolve from, and one
/// owned by a single caller is not that. A test built on a private directory
/// per identity proves nothing about the thing it stands in for.
#[derive(Clone)]
pub struct Shared(std::sync::Arc<std::sync::Mutex<Service<crate::MemStore>>>);

impl Shared {
    #[must_use]
    pub fn new() -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(Service::new(
            crate::MemStore::new(),
        ))))
    }

    fn held(&self) -> std::sync::MutexGuard<'_, Service<crate::MemStore>> {
        match self.0.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl Default for Shared {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::Directory for Shared {
    fn challenge(&mut self, device: &DeviceId, now: u64) -> Result<Challenge, Refusal> {
        self.held().challenge(device, now)
    }

    fn publish(&mut self, request: &SignedPublish, now: u64) -> Result<Issued, Refusal> {
        self.held().publish(request, now)
    }

    fn resolve(&mut self, request: &SignedResolve, now: u64) -> Result<Vec<u8>, Refusal> {
        self.held().resolve(request, now)
    }
}
