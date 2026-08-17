#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::unimplemented,
        clippy::unchecked_time_subtraction,
        clippy::todo,
        clippy::string_slice,
        clippy::panic_in_result_fn,
        clippy::panic,
        clippy::exit,
        clippy::as_conversions
    )
)]

//! **The correspondence adapter** — how a person reaches a person who shares no
//! Space with them, and the only crate that may name a delivery protocol.
//!
//! `comms` is the model, deliberately. It calls itself "the replaceable mechanism
//! that moves their bytes between peers" and is the only crate that names iroh;
//! this is the same shape one layer up, and the payoff the root manifest claims
//! for that one applies here too — swapping the carrier is a manifest change, not
//! a daemon rewrite.
//!
//! # Why a plane below the products rather than a messaging feature
//!
//! Today nothing crosses a Space boundary. Every Body traces to a signed member,
//! and the one artifact that leaves — an invitation — leaves as a couple of
//! thousand base32 characters a person pastes into somebody else's chat app. The
//! product's first act asks the user to leave the product.
//!
//! Once this plane exists, invite delivery, reaching someone outside your Space,
//! agent-to-agent messages and eventually a messaging client are all one
//! mechanism. That is why it is a plane and not a feature.
//!
//! # The two facts this seam refuses to conflate
//!
//! **Who is this person, and which devices do they hold?** The directory's
//! question, asked by the sender *before* anything reaches a carrier. Not this
//! crate's business, and deliberately absent from [`Carrier`] — a carrier that
//! could answer it would be a directory, and then withholding and impersonating
//! would be one capability instead of two.
//!
//! **May you read this?** A signature from a bound device, and nothing else. Not
//! an account session, not a token a carrier minted. That distinction costs one
//! signature verification and buys the whole posture: a carrier that authorizes
//! by its own session can hand a mailbox to whoever it likes, while one that
//! authorizes by device signature can *withhold* a mailbox and can never
//! *impersonate* its owner. It also keeps receiving free of any account
//! requirement, which is what keeps the hosted service optional.
//!
//! # Delivery is not admission
//!
//! An invitation carried through here is still signed by a Space admin, still
//! redeemed against convergent revocation, and still refused if its window has
//! passed. Nothing a carrier does creates standing, and nothing it fails to do
//! destroys any. Say it here because it is the property every future carrier
//! author will be tempted to assume away.
//!
//! # Sealing is above this seam, not in it
//!
//! A [`Sealed`] is opaque bytes. This crate does not seal, cannot open, and holds
//! no key — [`mechanics::authorization::seal_to_devices`] does that, above, and
//! hands the result down. A carrier that could read what it carries would make
//! "the carrier carries and never knows" a deployment promise rather than a
//! structural one.
//!
//! # An absence says which kind it is
//!
//! [`Missed`] separates "the carrier could not be asked" from "the carrier
//! answered and there is nothing". Folding them is the false-disconnection defect,
//! and here it is worse than usual: an unreachable carrier rendered as an empty
//! mailbox tells a person nobody has written to them.

use mechanics::egress::Egress;
use mechanics::ids::DeviceId;
use serde::{Deserialize, Serialize};

pub mod mem;
pub mod post;

pub use mem::MemCarrier;
pub use post::PostCarrier;

/// The largest sealed envelope this plane will carry.
///
/// Matches `lait-post`'s own bound rather than restating a number: a seam whose
/// limit is looser than its carrier's turns a refusal that could have happened
/// locally, before any bytes moved, into a round trip that fails.
pub const MAX_SEALED: usize = 256 * 1024;

/// The longest a deposit may ask to be held, in seconds.
pub const MAX_RETENTION: u64 = 30 * 24 * 60 * 60;

/// One sealed envelope addressed to one device.
///
/// Opaque by construction. The `Vec<u8>` is whatever
/// [`mechanics::authorization::seal_to_devices`] produced, and nothing in this
/// crate looks inside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sealed {
    /// The device this copy is sealed to.
    pub recipient: DeviceId,
    /// The sealed bytes.
    pub bytes: Vec<u8>,
    /// When this stops being worth holding, unix seconds. Taken from the
    /// payload's own validity window rather than invented as carrier policy.
    pub expires_at: u64,
    /// Which sealing construction produced `bytes`.
    ///
    /// On the record rather than in the bytes: a sealed box begins with a
    /// uniformly random byte, so any in-band tag collides with one in 256 of
    /// them. A version that cannot be sniffed has to be carried by its holder.
    pub construction: u16,
}

/// One envelope as it sits waiting, with what the carrier added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiting {
    /// Stable id, for acknowledging exactly this one.
    pub id: String,
    /// Who deposited it.
    ///
    /// Recorded because abuse control needs a subject, **not** because the
    /// recipient should trust it: the seal inside is what says who wrote this. A
    /// reader that treats this field as authorship has been handed the sender's
    /// own claim about themselves.
    pub deposited_by: DeviceId,
    pub sealed: Sealed,
    /// When the carrier observed it, unix seconds.
    pub arrived_at: u64,
}

/// Why a carrier could not do what was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// The envelope is larger than [`MAX_SEALED`].
    TooLarge,
    /// The expiry is in the past, or beyond [`MAX_RETENTION`].
    UnusableExpiry,
    /// The recipient is not a well-formed device key.
    UnusableRecipient,
    /// At capacity. Ask again later.
    ///
    /// Coarse on purpose, and the only arm that is: a full mailbox and a sender
    /// over its allowance must not be distinguishable, because telling a
    /// depositor which limit it hit tells it about the recipient.
    AtCapacity,
    /// The carrier could not be reached, or answered unusably.
    ///
    /// Never rendered as "nothing waiting" — see [`Missed`].
    Unreachable(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => f.write_str("the envelope is larger than this plane carries"),
            Self::UnusableExpiry => f.write_str("the expiry is past, or further out than allowed"),
            Self::UnusableRecipient => f.write_str("the recipient is not a well-formed device key"),
            Self::AtCapacity => f.write_str("at capacity; ask again later"),
            Self::Unreachable(why) => write!(f, "the carrier could not be asked: {why}"),
        }
    }
}

impl std::error::Error for Refused {}

/// What a fetch learned, with the two absences kept apart.
///
/// The whole reason this is not `Result<Vec<Waiting>, _>` with an empty vector on
/// failure: "nobody has written to you" and "we could not ask" are different
/// facts, only one is worth acting on, and a person shown the first when the
/// second is true has been lied to about their own correspondence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Missed {
    /// The carrier answered. The mailbox holds exactly this.
    Held(Vec<Waiting>),
    /// The carrier could not be asked. **Not** an empty mailbox.
    Unasked(String),
}

impl Missed {
    /// What is waiting, or `None` when the carrier could not be asked.
    ///
    /// Deliberately `Option<&[Waiting]>` rather than a defaulting accessor. A
    /// caller has to name what it wants to do about not knowing, and
    /// `unwrap_or_default` at least says so out loud.
    pub fn held(&self) -> Option<&[Waiting]> {
        match self {
            Self::Held(waiting) => Some(waiting),
            Self::Unasked(_) => None,
        }
    }
}

/// A carrier that holds sealed correspondence for a recipient who is not online.
///
/// # What a carrier is never asked
///
/// It is never asked who a person is, never asked to resolve a name, never asked
/// to adjudicate standing, and never given a key. Deposit takes an [`Egress`]
/// witness, so the *caller* has already proven whose key is being spent — the
/// carrier does not evaluate that and could not.
///
/// # Deposit is unauthenticated, and that is the design
///
/// Anyone may write to anyone. That is what makes a first contact possible at
/// all, and it is why every implementation owes a per-recipient capacity bound:
/// without one, a mailbox is write amplification any stranger can drive.
pub trait Carrier {
    /// Leave an envelope for later collection.
    ///
    /// The [`Egress`] argument is the point of this signature. It cannot be
    /// forged, copied or deserialized, so a send path that never asked
    /// `mechanics::egress::authorize` whose key it is spending **cannot be
    /// written** — it has no way to obtain the argument. Taken by reference
    /// because one authorization may legitimately fan out to several of a
    /// recipient's devices; taken at all because the alternative is a comment
    /// asking the next author to remember.
    fn deposit(&mut self, from: &Egress<'_>, sealed: &Sealed, now: u64) -> Result<String, Refused>;

    /// Collect what is waiting for one device.
    ///
    /// Authorization is the implementation's business and is a signature from
    /// this device over something the carrier issued — never a session. The seam
    /// does not model the challenge because a carrier that needs no round trip
    /// (the in-process one) should not have to fake one.
    fn collect(&mut self, device: &DeviceId, now: u64) -> Missed;

    /// Drop what the recipient confirms it holds. Returns how many went.
    fn acknowledge(
        &mut self,
        device: &DeviceId,
        ids: &[String],
        now: u64,
    ) -> Result<usize, Refused>;

    /// Block, or unblock, a sender on the recipient's own authority.
    ///
    /// `by` is the recipient, proven — the same `Egress` witness a deposit takes,
    /// for the same reason: authority over a mailbox is a key, and this is where
    /// the key is shown. A carrier that let anyone block anyone would be deciding
    /// who may reach whom, which is exactly the adjudication a carrier must not do.
    ///
    /// Blocking lives on the seam rather than being a Post detail because it is
    /// what makes a readable address survivable: the address is an
    /// unsolicited-contact surface the moment it exists, and a block that refused
    /// material *at the carrier* is the difference between a stranger costing you a
    /// glance and costing you your device. A review queue built above this seam
    /// calls it without caring which contractor is behind it.
    fn block(
        &mut self,
        by: &Egress<'_>,
        sender: &DeviceId,
        blocked: bool,
        now: u64,
    ) -> Result<(), Refused>;
}

/// Check what any carrier may check before it has moved a byte.
///
/// Shared so the in-process carrier and a hosted one refuse the same things for
/// the same reasons, and so a refusal that can be decided locally is not paid for
/// with a round trip.
pub fn admissible(sealed: &Sealed, now: u64) -> Result<(), Refused> {
    if sealed.bytes.len() > MAX_SEALED {
        return Err(Refused::TooLarge);
    }
    if sealed.expires_at <= now || sealed.expires_at.saturating_sub(now) > MAX_RETENTION {
        return Err(Refused::UnusableExpiry);
    }
    // Canonical spelling insisted on at the boundary, as `lait-post` does. A
    // `DeviceId` compares spelling-blind, so a re-spelling would not split a map
    // here — but a carrier's store may key a directory on the string, where an
    // `Eq` impl cannot help.
    match DeviceId::parse(sealed.recipient.as_str()) {
        Some(canonical) if canonical.as_str() == sealed.recipient.as_str() => Ok(()),
        _ => Err(Refused::UnusableRecipient),
    }
}

/// A deterministic id for a deposit: its content, so a retry is not a duplicate.
///
/// A sender retrying a delivery it is not sure landed must not double a
/// recipient's mailbox, and for a carrier whose whole purpose is reaching someone
/// who is not there, the retry is the common case.
///
/// Every field the id covers is one a signature over this envelope would cover,
/// **including `construction`** — omitting it is how two envelopes that differ
/// only in their sealing construction collapse into one and the second silently
/// overwrites the first.
pub fn deposit_id(deposited_by: &DeviceId, sealed: &Sealed) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lait/correspondence/1/deposit-id");
    // Framed, because two of these are variable-length strings and an unframed
    // concatenation lets one field's tail read as the next field's head.
    for part in [
        deposited_by.as_str().as_bytes(),
        sealed.recipient.as_str().as_bytes(),
    ] {
        // `try_from` rather than `as`, and a fixed width rather than `usize`,
        // because the id has to be identical on a 32- and a 64-bit peer. A length
        // that does not fit in `u64` cannot exist, and saturating is still
        // deterministic if one somehow did.
        let len = u64::try_from(part.len()).unwrap_or(u64::MAX);
        hasher.update(&len.to_be_bytes());
        hasher.update(part);
    }
    hasher.update(&sealed.expires_at.to_be_bytes());
    hasher.update(&sealed.construction.to_be_bytes());
    hasher.update(&sealed.bytes);
    data_encoding::HEXLOWER.encode(&hasher.finalize().as_bytes()[..16])
}
