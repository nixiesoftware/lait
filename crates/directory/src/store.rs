//! Where the directory keeps what it was handed.
//!
//! Everything mutable is behind this trait, including challenges and rate
//! windows. That is deliberate and it is the one place this design departs from
//! `lait_post`, which keeps both in `HashMap`s beside its store and says so:
//! *"a restart forgets every window. That is the honest shape for a single
//! instance."*
//!
//! The directory cannot take that shape. Its data is permanent and
//! identity-bearing — an address that stops resolving is a person who cannot be
//! reached, with no expiry to heal it — and address issuance needs an atomic
//! claim that an in-process map cannot provide behind more than one replica.
//! Two of them would mint one address twice.
//!
//! So the seam is drawn to include the short-lived state, and a second replica
//! is a deployment decision rather than a rewrite.

use std::collections::BTreeMap;

use mechanics::{ids::DeviceId, kinship::ProfileId};

use crate::{address::Address, wire::Challenge};

/// One profile's published state, as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    /// The announcement exactly as its publisher encoded it. Bytes rather than a
    /// decoded value: the signature covered these, and re-encoding could not
    /// reproduce them faithfully enough to be worth the risk.
    pub announcement: Vec<u8>,
    /// The head epoch the announcement carried. Kept so a replay of an older
    /// publication cannot roll a device set backwards.
    pub epoch: u64,
}

/// The directory's whole persistence surface.
///
/// Sync, matching `lait_post::Store`, because the one implementation that talks
/// to a network does so with `ureq` — the same blocking client the rest of this
/// tree uses — and the HTTP surface hands it to `spawn_blocking` rather than
/// making every caller async for one deployment's benefit.
pub trait Store {
    /// Claim `address` for `profile` if and only if nothing holds it yet.
    ///
    /// `Ok(true)` when this call took it. **This is the atomic mint**, and it is
    /// the operation that decides what a backing store has to be: two replicas
    /// racing must not both succeed. An implementation that cannot promise that
    /// is not a directory store, whatever else it can do.
    fn claim(&mut self, address: &Address, profile: &ProfileId) -> anyhow::Result<bool>;

    /// The address this profile already holds, if it has published before.
    ///
    /// What makes publishing idempotent: an address is *"minted on first publish
    /// and stable afterwards"*, so a republish reads back rather than minting a
    /// second.
    fn address_of(&self, profile: &ProfileId) -> anyhow::Result<Option<Address>>;

    /// Record a publication. `Ok(false)` when `epoch` does not advance what is
    /// already held, which is how a replayed older announcement is refused
    /// without an error a prober could read anything from.
    fn record(&mut self, profile: &ProfileId, published: &Published) -> anyhow::Result<bool>;

    /// What an address currently answers with.
    fn published(&self, address: &Address) -> anyhow::Result<Option<Published>>;

    /// Remember an issued challenge.
    fn open(&mut self, challenge: &Challenge) -> anyhow::Result<()>;

    /// Spend a challenge, exactly once.
    ///
    /// The single-use property lives here rather than above, because "read it
    /// then delete it" is two operations and a second replica can interleave
    /// between them. An implementation must make this one.
    fn spend(&mut self, nonce: &[u8; 32], now: u64) -> anyhow::Result<Option<Challenge>>;

    /// How many unexpired challenges this device is holding open.
    fn open_for(&self, device: &DeviceId, now: u64) -> anyhow::Result<usize>;

    /// Record one resolution by `asker` and answer how many it has made inside
    /// the window, this one included.
    fn note_resolve(&mut self, asker: &DeviceId, now: u64) -> anyhow::Result<usize>;

    /// Drop expired challenges and stale rate windows. Returns how many went.
    fn sweep(&mut self, now: u64) -> anyhow::Result<usize>;
}

/// An in-memory store, for tests and for a service run with no deployment.
///
/// Correct rather than merely convenient: it enforces the same single-use and
/// claim-once rules a real backing store must, so a test that passes here is
/// testing the rule and not the storage.
#[derive(Debug, Default)]
pub struct MemStore {
    /// address → profile. The uniqueness constraint, made explicit.
    holders: BTreeMap<String, ProfileId>,
    /// profile → address.
    addresses: BTreeMap<String, Address>,
    /// profile → what it published.
    published: BTreeMap<String, Published>,
    /// nonce → challenge.
    challenges: BTreeMap<[u8; 32], Challenge>,
    /// asker → resolution timestamps, newest last.
    resolves: BTreeMap<String, Vec<u64>>,
}

impl MemStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Store for MemStore {
    fn claim(&mut self, address: &Address, profile: &ProfileId) -> anyhow::Result<bool> {
        if self.holders.contains_key(address.as_str()) {
            return Ok(false);
        }
        self.holders
            .insert(address.as_str().to_owned(), profile.clone());
        self.addresses
            .insert(profile.as_str().to_owned(), address.clone());
        Ok(true)
    }

    fn address_of(&self, profile: &ProfileId) -> anyhow::Result<Option<Address>> {
        Ok(self.addresses.get(profile.as_str()).cloned())
    }

    fn record(&mut self, profile: &ProfileId, published: &Published) -> anyhow::Result<bool> {
        if let Some(held) = self.published.get(profile.as_str()) {
            if published.epoch < held.epoch {
                return Ok(false);
            }
        }
        self.published
            .insert(profile.as_str().to_owned(), published.clone());
        Ok(true)
    }

    fn published(&self, address: &Address) -> anyhow::Result<Option<Published>> {
        let Some(profile) = self.holders.get(address.as_str()) else {
            return Ok(None);
        };
        Ok(self.published.get(profile.as_str()).cloned())
    }

    fn open(&mut self, challenge: &Challenge) -> anyhow::Result<()> {
        self.challenges.insert(challenge.nonce, challenge.clone());
        Ok(())
    }

    fn spend(&mut self, nonce: &[u8; 32], now: u64) -> anyhow::Result<Option<Challenge>> {
        let Some(challenge) = self.challenges.remove(nonce) else {
            return Ok(None);
        };
        // Removed either way: an expired challenge is spent by being asked
        // about, so a stale nonce cannot be probed repeatedly.
        if now.saturating_sub(challenge.issued_at) > crate::bounds::CHALLENGE_TTL {
            return Ok(None);
        }
        Ok(Some(challenge))
    }

    fn open_for(&self, device: &DeviceId, now: u64) -> anyhow::Result<usize> {
        Ok(self
            .challenges
            .values()
            .filter(|held| {
                held.device == *device
                    && now.saturating_sub(held.issued_at) <= crate::bounds::CHALLENGE_TTL
            })
            .count())
    }

    fn note_resolve(&mut self, asker: &DeviceId, now: u64) -> anyhow::Result<usize> {
        let seen = self.resolves.entry(asker.as_str().to_owned()).or_default();
        seen.retain(|at| now.saturating_sub(*at) < crate::bounds::RATE_WINDOW);
        seen.push(now);
        Ok(seen.len())
    }

    fn sweep(&mut self, now: u64) -> anyhow::Result<usize> {
        let before = self.challenges.len();
        self.challenges
            .retain(|_, held| now.saturating_sub(held.issued_at) <= crate::bounds::CHALLENGE_TTL);
        self.resolves.retain(|_, seen| {
            seen.retain(|at| now.saturating_sub(*at) < crate::bounds::RATE_WINDOW);
            !seen.is_empty()
        });
        Ok(before.saturating_sub(self.challenges.len()))
    }
}
