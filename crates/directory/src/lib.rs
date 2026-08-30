//! The directory: a mirror of self-authorised device events, never an authority
//! over them.
//!
//! **Skeleton.** The types, the refusals and the trust position are here; the
//! store and the HTTP surface are not. This exists so the client's publish and
//! resolve paths have something real to be written against, and so the shape is
//! argued once rather than improvised later.
//!
//! # Why a service at all, and what it may never become
//!
//! Two people who share no Space have no way to learn each other's device set.
//! The invitation *is* the introduction, so first contact cannot bootstrap from
//! material they already hold — that circularity is why an address exists.
//!
//! What keeps this from becoming an authority is the asymmetry the Post already
//! has: it **holds no key**. A client publishes events signed under their own
//! domain by the device being bound; this service verifies them on their own
//! terms and stores them append-only. It can withhold, delay, reorder and deny.
//! It cannot author. A compromise is therefore a *denial*, which is detectable,
//! rather than an impersonation, which would not be.
//!
//! Consequences that are not negotiable:
//!
//! - No account-side path may add, edit or reorder a device event. Account
//!   recovery must never become a way to take over correspondence.
//! - An address is **issued**, never chosen. Nobody squats, nobody selects a
//!   confusable, and enumeration is defeated by sparseness before it is defeated
//!   by refusal.
//! - An address is not an identity and confers no trust. It gets a letter
//!   through; it says nothing about who opens it. Trust comes from binding the
//!   key at introduction.
//! - Resolution answers an **exact** address only. No listing, no prefix search,
//!   no existence oracle — and a refusal must not distinguish "no such address"
//!   from "you may not ask".
//!
//! The directory learns who looks up whom, which is contact discovery, and that
//! is a real cost rather than one to deny. It is minimised, not claimed away.
//!
//! One more cost is paid deliberately: an accepted publication is appended to
//! the deployment's [`chronicle`], the log the label registry already keeps, so
//! that a person with no label still gets a receipt somebody can check. A leaf
//! is a hash, so what becomes public is that *some* publication happened — never
//! whose, and never what it said. The alternative was a second log with a second
//! signer for every reader to follow, which buys nothing the refusal shape above
//! does not already protect.

#![forbid(unsafe_code)]

pub mod address;
pub mod chronicle;
pub mod client;
pub mod firestore;
pub mod http;
pub mod registry;
pub mod service;
pub mod store;
pub mod wire;
mod words;

pub use address::Address;
pub use chronicle::{ChronicleStore, Chronicler, Receipt};
pub use client::Remote;
pub use firestore::{Credentials, FirestoreStore};
pub use service::{Service, Shared};
pub use store::{MemStore, Published, Store};
pub use wire::{sign, Challenge, SignedPublish, SignedResolve};

/// Why the directory would not answer.
///
/// Deliberately coarse where it faces a prober: [`Refusal::NotAvailable`] covers
/// both "no such address" and "you may not ask", because two distinguishable
/// answers are an existence oracle however carefully they are worded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The address is not resolvable to you. Says nothing about whether it
    /// exists — "no such address" and "you may not ask" are deliberately the
    /// same answer, because two distinguishable ones are an existence oracle.
    NotAvailable,
    /// What was typed is not an address at all. Safe to distinguish: it is a
    /// statement about the input, not about who exists.
    Malformed,
    /// A published event did not verify under its own domain, or was not signed
    /// by the device it binds.
    NotAuthentic,
    /// The challenge was unknown, already spent, or expired.
    StaleChallenge,
    /// The asker is going too fast. Cost rises under probing.
    TooFast,
    /// The material was larger than this service will read.
    TooLarge,
    /// The service could not answer. Never rendered as "no".
    Unavailable(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAvailable => write!(f, "not available"),
            Self::Malformed => write!(f, "that is not an address"),
            Self::NotAuthentic => write!(f, "the published material did not verify"),
            Self::StaleChallenge => write!(f, "that challenge is not open"),
            Self::TooFast => write!(f, "too many requests"),
            Self::TooLarge => write!(f, "too large"),
            Self::Unavailable(why) => write!(f, "the directory could not answer: {why}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Bounds. Every one of these faces a stranger.
pub mod bounds {
    /// One published announcement, encoded.
    pub const MAX_PUBLISH_BYTES: usize = 256 * 1024;
    /// How long a challenge is open, in seconds. Short enough that a captured
    /// one is worthless, long enough for a cold TLS handshake.
    pub const CHALLENGE_TTL: u64 = 60;
    /// Outstanding challenges for one device.
    pub const MAX_CHALLENGES_PER_DEVICE: usize = 8;
    /// Resolutions one asker may make per window.
    pub const MAX_RESOLVES_PER_WINDOW: usize = 32;
    /// The resolve rate window, in seconds.
    pub const RATE_WINDOW: u64 = 60;
}

/// What publishing answered: the address, and the chronicle's receipt for it.
///
/// The address alone is what a person needs; the receipt is what a *reader*
/// needs to check that this publication was recorded, by a marker it follows,
/// before it renders anything as certified. Carried together because they are
/// answers to one act — a receipt fetched separately would be a receipt for
/// some other publication.
#[derive(Debug, Clone)]
pub struct Issued {
    pub address: Address,
    pub receipt: Receipt,
}

/// What the service does, independent of how it is reached.
///
/// A trait so the HTTP surface, the client's carrier-side adapter and the tests
/// speak one vocabulary — the shape `correspondence::Carrier` has, and for the
/// same reason: the layer above must not learn which implementation is under it.
pub trait Directory {
    /// Issue a single-use nonce for `device`. Free and unauthenticated: it
    /// proves nothing and grants nothing, and needing no prior agreement is why
    /// a challenge is used rather than a signed timestamp.
    fn challenge(
        &mut self,
        device: &mechanics::ids::DeviceId,
        now: u64,
    ) -> Result<Challenge, Refusal>;

    /// Publish a profile's signed device events, verified on their own terms.
    ///
    /// Returns the address this profile answers to — minted on first publish and
    /// stable afterwards. The service chooses it; the publisher does not — and,
    /// beside it, the receipt for the chronicle entry this publication became.
    fn publish(&mut self, request: &SignedPublish, now: u64) -> Result<Issued, Refusal>;

    /// Resolve one exact address to the announcement its profile published.
    ///
    /// Never a listing and never a prefix. The asker signs a statement naming
    /// the operation, the subject and a nonce this service issued, so a captured
    /// resolution cannot be replayed by whoever saw it.
    ///
    /// Answers the announcement's own bytes rather than a decoded value: the
    /// publisher's signature covered exactly those, and the *reader* is the party
    /// that must anchor them to a genesis it already holds.
    fn resolve(&mut self, request: &SignedResolve, now: u64) -> Result<Vec<u8>, Refusal>;
}

impl<S: Store> Directory for Service<S> {
    fn challenge(
        &mut self,
        device: &mechanics::ids::DeviceId,
        now: u64,
    ) -> Result<Challenge, Refusal> {
        Service::challenge(self, device, now)
    }

    fn publish(&mut self, request: &SignedPublish, now: u64) -> Result<Issued, Refusal> {
        Service::publish(self, request, now)
    }

    fn resolve(&mut self, request: &SignedResolve, now: u64) -> Result<Vec<u8>, Refusal> {
        Service::resolve(self, request, now)
    }
}

/// Publish `announcement` and learn the address it answers to.
///
/// The round trip in one call, because challenge-then-sign-then-send is the
/// *protocol* and a caller reimplementing it is a caller that can get the order
/// wrong. Takes a seed and runs in the caller's process; nothing here is the
/// service's.
pub fn publish_as(
    directory: &mut dyn Directory,
    seed: &[u8; 32],
    announcement: &addressbook::Announcement,
    now: u64,
) -> Result<Issued, Refusal> {
    let device = mechanics::actor::device_from_seed(seed);
    let challenge = directory.challenge(&device, now)?;
    let encoded = announcement.encode().map_err(|_| Refusal::TooLarge)?;
    let request = wire::sign::publish(seed, &challenge, encoded);
    directory.publish(&request, now)
}

/// Ask for one exact address, and get back what its holder published.
///
/// The answer is **not** trusted here. It is an announcement the caller must
/// anchor against a genesis it already holds — which is what makes the directory
/// a mirror rather than an authority, and what stops a substituted answer from
/// becoming a seal to the wrong key (AUTH-18).
pub fn resolve_as(
    directory: &mut dyn Directory,
    seed: &[u8; 32],
    address: &Address,
    now: u64,
) -> Result<addressbook::Announcement, Refusal> {
    let device = mechanics::actor::device_from_seed(seed);
    let challenge = directory.challenge(&device, now)?;
    let request = wire::sign::resolve(seed, &challenge, address);
    let bytes = directory.resolve(&request, now)?;
    addressbook::Announcement::decode(&bytes).map_err(|_| Refusal::NotAuthentic)
}
