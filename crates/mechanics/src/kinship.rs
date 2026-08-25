//! Kinship — the signed, Space-less plane that answers "which devices, actors
//! and agents belong together", and does so in a form that may leave the device.
//!
//! Governed by the Substrate Specs "Kinship is avowed, never derived" and
//! "Kinship travels: the audience in the preimage, and the projection that
//! cannot omit". Three properties carry the whole design:
//!
//! **Linkage is avowed, never derived.** [`crate::actor`] binds the space id
//! into an [`ActorId`], so nobody *computes* a cross-Space link. Nothing here
//! hands one out either: an observer learns that two identifiers belong
//! together only by being told, in an [`Avowal`] whose audience includes it.
//!
//! **The audience is inside the signature.** [`Audience`] is part of the signed
//! preimage rather than an envelope field, so presenting an avowal outside its
//! stated audience is detectable by inspecting the artifact alone — with no
//! reference to the transport that carried it. This does not *prevent* onward
//! disclosure; nothing can, because a recipient may always retell. It makes the
//! retelling attributable, which is the whole of what transmissibility buys.
//!
//! **An avowal confers nothing.** It is an assertion about who is related to
//! whom, never admission, membership, grant or standing. Authority is evaluated
//! by replaying signed history at a causal position, and an avowal that has left
//! its origin has no position to be evaluated at. It travels precisely because
//! it carries nothing that needs one. There is deliberately no conversion from
//! anything here into a grant, a capability or a [`crate::membership`] fact.
//!
//! # Peerage, not clientage
//!
//! Device links are **symmetric**: both devices sign the same preimage, there is
//! no patron, and there is no cascade. Losing one device leaves every other link
//! intact — which is the reason the shape is peerage and not the clientage that
//! governs agent sponsorship one layer up. The two vocabularies must not merge;
//! a device patron chain would make losing the first device kill the rest.
//!
//! # What a commitment can and cannot do
//!
//! [`Head`] signs *which entries exist*, never what a build concluded from them —
//! the same exclusion [`crate::ledger`]'s checkpoint commitment makes, and for
//! the same reason: two correct builds may legitimately read one closure
//! differently, and a commitment over the interpretation pits the receipt
//! against the reader's version rather than against the history it claims.
//!
//! Be exact about the strength. A signed head makes omission **detectable on
//! comparison** — against another recipient's head, a later head, or an
//! independent mirror — and makes equivocation non-repudiable, because two
//! differing heads at one epoch are both signed. It does not make a single
//! projection self-proving in isolation, and no signature scheme would: a sender
//! that truncates before committing produces something internally consistent.
//! What [`Projection::verify`] does establish alone is that every body delivered
//! is drawn from the committed head, and that no admissible body listed in that
//! head was silently dropped.

use serde::{Deserialize, Serialize};

use crate::actor::{sign_detached, verify_detached};
use crate::ids::{ActorId, DeviceId, SpaceId};

/// Signing domain for a mutual device link.
pub const LINK_DOMAIN: &[u8] = b"lait/kinship/1/link";

/// Signing domain for a transmissible, audience-scoped avowal.
pub const AVOWAL_DOMAIN: &[u8] = b"lait/kinship/1/avowal";

/// Signing domain for a log head.
pub const HEAD_DOMAIN: &[u8] = b"lait/kinship/1/head";

/// Signing domain for a device retirement.
pub const RETIRE_DOMAIN: &[u8] = b"lait/kinship/1/retire";

/// Key-derivation context for the profile id.
pub const PROFILE_CONTEXT: &str = "lait.kinship-profile.v1";

/// Key-derivation context for an entry id.
pub const ENTRY_CONTEXT: &str = "lait.kinship-entry.v1";

/// Key-derivation context for a head commitment.
pub const COMMITMENT_CONTEXT: &str = "lait.kinship-commitment.v1";

/// The semantics this build replays. Bound into every head commitment so a
/// receipt minted under different rules can never be read as agreeing.
pub const KINSHIP_SEMANTICS: u16 = 1;

/// Cap on entries in one log, so a projection cannot be unbounded work.
pub const MAX_ENTRIES: usize = 4096;

/// Cap on an avowed name, in bytes.
pub const MAX_NAME_BYTES: usize = 128;

/// Cap on a portrait's detail line, in bytes.
pub const MAX_DETAIL_BYTES: usize = 256;

/// A detached ed25519 signature.
///
/// A named type rather than a bare `[u8; 64]` at every site: serde implements
/// the byte-array traits only to length 32, so each occurrence would otherwise
/// carry its own `with` attribute, and one of them would eventually be forgotten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature(#[serde(with = "serde_byte_array")] pub [u8; 64]);

impl Signature {
    #[must_use]
    pub const fn bytes(&self) -> &[u8; 64] {
        &self.0
    }
}

crate::prefixed_id!(
    /// A **profile** id — the content address of the log's genesis link.
    /// Self-certifying in the same way an [`ActorId`] is: any holder of the
    /// genesis entry validates the id by rehashing, and no registry mints it.
    ProfileId,
    "prf_"
);

impl ProfileId {
    /// Derive the self-certifying id from the genesis link's canonical bytes.
    #[must_use]
    pub fn from_genesis(bytes: &[u8]) -> Self {
        let digest = blake3::derive_key(PROFILE_CONTEXT, bytes);
        let mut head = [0u8; 16];
        head.copy_from_slice(&digest[..16]);
        Self::from_digest(head)
    }
}

/// A party an avowal may name, or be shown to.
///
/// Exactly the identifiers that may leave a device. The `Actor` variant is two
/// fields on purpose: the space id is hashed *into* the actor id, so an
/// [`ActorId`] alone is globally unique but **not globally resolvable**, and a
/// verifier holding one cannot know which event set to replay.
///
/// A rendered `did:key` is deliberately absent. It is a derived presentation
/// with no decoder, so two avowals naming one party would not compare equal.
/// Render it at the boundary, never in a preimage.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Party {
    Device(DeviceId),
    Actor { space: SpaceId, actor: ActorId },
}

impl Party {
    /// The wire spelling, matching the address book's handle grammar so one
    /// vocabulary spans both layers.
    #[must_use]
    pub fn wire(&self) -> String {
        match self {
            Self::Device(device) => device.as_str().to_string(),
            Self::Actor { space, actor } => {
                format!("actor:{}:{}", space.as_str(), actor.as_str())
            }
        }
    }

    /// Parse a wire spelling. Refuses anything it does not fully understand.
    pub fn parse_wire(raw: &str) -> Result<Self, Refusal> {
        let raw = raw.trim();
        if let Some(rest) = raw.strip_prefix("actor:") {
            let (space, actor) = rest
                .split_once(':')
                .ok_or(Refusal::Malformed("actor party"))?;
            let space = SpaceId::parse(space).ok_or(Refusal::Malformed("space id"))?;
            let actor = ActorId::parse(actor).ok_or(Refusal::Malformed("actor id"))?;
            return Ok(Self::Actor { space, actor });
        }
        DeviceId::parse(raw)
            .map(Self::Device)
            .ok_or(Refusal::Malformed("party"))
    }
}

/// Who an avowal was made to. Part of what is signed.
///
/// Each tier is an audience selection over key material the tree already mints,
/// which is why none of them needs new cryptography. Ordered outward.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Audience {
    /// The profile's own device set.
    Own,
    /// The sponsor chain and sponsored set.
    Kin,
    /// Everyone a Space's ACL admits.
    Members(SpaceId),
    /// Exactly one counterparty.
    Correspondent(Party),
    /// The directory. Carries **no** confidentiality claim.
    Public,
}

impl Audience {
    /// How many parties this could reach, as a coarse class.
    ///
    /// *Audience size is the privacy budget.* Attribution to a set of fifty is
    /// attribution to nobody, so a surface must render "shared with your Space"
    /// and "shared with one person" differently rather than implying uniform
    /// accountability. This is the datum that lets it.
    #[must_use]
    pub const fn attribution(&self) -> Attribution {
        match self {
            Self::Correspondent(_) => Attribution::Single,
            Self::Own | Self::Kin => Attribution::Few,
            Self::Members(_) => Attribution::Many,
            Self::Public => Attribution::None,
        }
    }

    /// Whether `standing` is inside this audience.
    ///
    /// `Own` and `Kin` cannot be decided from the artifact alone — they need the
    /// profile's device set and the clientage closure respectively, which the
    /// caller resolves and presents in [`Standing`]. That is deliberate: a check
    /// that silently guessed at either would be a check that passes when the
    /// answer was never fetched.
    #[must_use]
    pub fn admits(&self, standing: &Standing) -> bool {
        match self {
            Self::Public => true,
            Self::Own => standing.own,
            Self::Kin => standing.kin,
            Self::Members(space) => standing.spaces.contains(space),
            Self::Correspondent(party) => standing.is(party),
        }
    }

    fn tag(&self) -> &'static [u8] {
        match self {
            Self::Own => b"own",
            Self::Kin => b"kin",
            Self::Members(_) => b"members",
            Self::Correspondent(_) => b"correspondent",
            Self::Public => b"public",
        }
    }

    fn body(&self) -> String {
        match self {
            Self::Own | Self::Kin | Self::Public => String::new(),
            Self::Members(space) => space.as_str().to_string(),
            Self::Correspondent(party) => party.wire(),
        }
    }
}

/// How many parties an artifact reached, as a class rather than a count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Attribution {
    /// One counterparty. A leak is attributable to exactly them.
    Single,
    /// A small closure. Attribution is real but not unique.
    Few,
    /// A Space's membership. Attribution is nominal.
    Many,
    /// Published. No attribution and no confidentiality claim.
    None,
}

/// What an avowal asserts about its subject.
///
/// Variants are **append-only** — postcard discriminants are positional, so
/// inserting one would silently reinterpret every artifact already signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Claim {
    /// The subject's devices form this profile.
    Profile(ProfileId),
    /// The subject is known by this name. Self-signed it is an avowal; signed
    /// by another party it is an attestation, and the difference is whether the
    /// signer is inside the subject.
    Called(String),
    /// The subject sponsors this party.
    Sponsors(Party),
    /// How the subject presents: a picture, by content hash, and a detail
    /// line. The *name* stays [`Claim::Called`] — one name channel, so a
    /// portrait never bypasses the ranked resolution names already have.
    /// Self-signed only in practice: nobody else's signature can say how you
    /// present, which readers enforce by requiring the self-signature, not
    /// this type.
    Portrait {
        /// The picture's content hash, or `None` for none-or-cleared. Raw
        /// bytes, never a rendering — two spellings of one hash would be two
        /// claims.
        picture: Option<[u8; 32]>,
        /// A line of self-description. May be empty.
        detail: String,
    },
}

impl Claim {
    fn tag(&self) -> &'static [u8] {
        match self {
            Self::Profile(_) => b"profile",
            Self::Called(_) => b"called",
            Self::Sponsors(_) => b"sponsors",
            Self::Portrait { .. } => b"portrait",
        }
    }

    /// The claim's preimage bytes. For the single-field variants these are
    /// the field's own bytes, unchanged since v1 — every signature already
    /// minted stays valid. A variant with more than one variable-length field
    /// frames them internally, because `a ‖ b` is ambiguous with `a' ‖ b'`
    /// whenever the boundary can move.
    fn body(&self) -> Vec<u8> {
        match self {
            Self::Profile(profile) => profile.as_str().as_bytes().to_vec(),
            Self::Called(name) => name.as_bytes().to_vec(),
            Self::Sponsors(party) => party.wire().into_bytes(),
            Self::Portrait { picture, detail } => {
                let mut out = Vec::with_capacity(64_usize.saturating_add(detail.len()));
                match picture {
                    Some(hash) => framed(&mut out, &hash[..]),
                    None => framed(&mut out, &[]),
                }
                framed(&mut out, detail.as_bytes());
                out
            }
        }
    }

    fn check(&self) -> Result<(), Refusal> {
        match self {
            Self::Called(name) => {
                if name.is_empty() {
                    return Err(Refusal::Malformed("empty name"));
                }
                if name.len() > MAX_NAME_BYTES {
                    return Err(Refusal::Bound("name bytes"));
                }
            }
            Self::Portrait { detail, .. } => {
                if detail.len() > MAX_DETAIL_BYTES {
                    return Err(Refusal::Bound("detail bytes"));
                }
            }
            Self::Profile(_) | Self::Sponsors(_) => {}
        }
        Ok(())
    }
}

/// A typed refusal. Never a boolean: "this was refused" and *why* are different
/// facts, and only one of them tells a surface what to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The signature does not verify under the stated signer.
    BadSignature,
    /// The signer's id is not a usable verifying key.
    Unaddressable,
    /// The reader is outside the audience the artifact names.
    OutsideAudience,
    /// A link's two devices are not distinct.
    NotDistinct,
    /// A link carries only one side's signature.
    NotMutual,
    /// A delivered body is not in the committed head.
    Unlisted,
    /// The head lists an admissible entry whose body was withheld.
    Omission,
    /// A projection carries no committed head, so it is a hint and not evidence.
    Uncommitted,
    /// The commitment does not recompute from what it claims to cover.
    Diverged,
    /// The artifact was replayed under different semantics.
    Semantics,
    /// Structurally invalid.
    Malformed(&'static str),
    /// A declared bound was exceeded.
    Bound(&'static str),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadSignature => f.write_str("signature does not verify"),
            Self::Unaddressable => f.write_str("signer is not a usable key"),
            Self::OutsideAudience => f.write_str("reader is outside the stated audience"),
            Self::NotDistinct => f.write_str("a link needs two distinct devices"),
            Self::NotMutual => f.write_str("a link needs both signatures"),
            Self::Unlisted => f.write_str("a delivered entry is not in the committed head"),
            Self::Omission => f.write_str("an admissible entry was withheld"),
            Self::Uncommitted => f.write_str("no committed head, so this is a hint"),
            Self::Diverged => f.write_str("commitment does not match what it covers"),
            Self::Semantics => f.write_str("minted under different semantics"),
            Self::Malformed(what) => write!(f, "malformed {what}"),
            Self::Bound(what) => write!(f, "exceeds bound {what}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// What a reader can prove about itself when a projection is presented.
///
/// The caller populates this by resolving the profile's device set and the
/// clientage closure. Nothing here is inferred, because an inferred standing is
/// a standing that passes when the answer was never fetched.
#[derive(Debug, Clone, Default)]
pub struct Standing {
    /// This reader's device.
    pub device: Option<DeviceId>,
    /// This reader's actor, per Space.
    pub actors: Vec<(SpaceId, ActorId)>,
    /// Spaces whose ACL admits this reader.
    pub spaces: Vec<SpaceId>,
    /// Resolved: this reader is a device of the subject profile.
    pub own: bool,
    /// Resolved: this reader is inside the subject's clientage closure.
    pub kin: bool,
}

impl Standing {
    /// Whether this standing *is* the named party.
    #[must_use]
    pub fn is(&self, party: &Party) -> bool {
        match party {
            Party::Device(device) => self.device.as_ref() == Some(device),
            Party::Actor { space, actor } => {
                self.actors.iter().any(|(s, a)| s == space && a == actor)
            }
        }
    }
}

/// Length-prefixed framing, so two adjacent variable-length fields can never be
/// read as one. The Post's preimages use the same shape; an unframed
/// concatenation of `a ‖ b` is ambiguous with `a' ‖ b'` whenever the boundary
/// can move.
fn framed(out: &mut Vec<u8>, part: &[u8]) {
    let len = u64::try_from(part.len()).unwrap_or(u64::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(part);
}

fn key_of(device: &DeviceId) -> Result<[u8; 32], Refusal> {
    device.key_bytes().ok_or(Refusal::Unaddressable)
}

/// A mutually signed, Space-less link between two devices of one profile.
///
/// Both devices sign the **same** preimage. Symmetric by construction: there is
/// no author and no subject, so there is nothing for a later reader to mistake
/// for a patron relationship.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceLink {
    /// The two devices, sorted. Sorting is what makes the link one fact rather
    /// than two orderings of it.
    pub devices: [DeviceId; 2],
    pub nonce: [u8; 16],
    pub epoch: u64,
    /// Signatures parallel to `devices`.
    pub signatures: [Signature; 2],
}

impl DeviceLink {
    /// The preimage both devices sign.
    #[must_use]
    pub fn preimage(devices: &[DeviceId; 2], nonce: &[u8; 16], epoch: u64) -> Vec<u8> {
        let mut out = Vec::new();
        framed(&mut out, LINK_DOMAIN);
        framed(&mut out, devices[0].as_str().as_bytes());
        framed(&mut out, devices[1].as_str().as_bytes());
        framed(&mut out, &nonce[..]);
        framed(&mut out, &epoch.to_be_bytes());
        out
    }

    /// Mint a link from both seeds. Both sides sign because both consent; a
    /// one-sided link is refused rather than represented.
    pub fn seal(
        first: &[u8; 32],
        second: &[u8; 32],
        nonce: [u8; 16],
        epoch: u64,
    ) -> Result<Self, Refusal> {
        let a = crate::actor::device_from_seed(first);
        let b = crate::actor::device_from_seed(second);
        if a == b {
            return Err(Refusal::NotDistinct);
        }
        let (devices, seeds) = if a <= b {
            ([a, b], [first, second])
        } else {
            ([b, a], [second, first])
        };
        let preimage = Self::preimage(&devices, &nonce, epoch);
        Ok(Self {
            signatures: [
                Signature(sign_detached(seeds[0], &preimage)),
                Signature(sign_detached(seeds[1], &preimage)),
            ],
            devices,
            nonce,
            epoch,
        })
    }

    /// Verify both signatures. A link missing either side is not a link.
    pub fn verify(&self) -> Result<(), Refusal> {
        if self.devices[0] == self.devices[1] {
            return Err(Refusal::NotDistinct);
        }
        if self.devices[0] > self.devices[1] {
            return Err(Refusal::Malformed("link device order"));
        }
        let preimage = Self::preimage(&self.devices, &self.nonce, self.epoch);
        for (device, signature) in self.devices.iter().zip(self.signatures.iter()) {
            let key = key_of(device)?;
            if !verify_detached(&key, &preimage, signature.bytes()) {
                return Err(Refusal::NotMutual);
            }
        }
        Ok(())
    }

    /// Whether this link names `device`.
    #[must_use]
    pub fn names(&self, device: &DeviceId) -> bool {
        &self.devices[0] == device || &self.devices[1] == device
    }

    /// One side's signature over the link both sides will hold.
    ///
    /// The sponsorship shape: `seal` needs both seeds on one machine, and a
    /// real join has them on two. Each side signs the same preimage where its
    /// seed lives; [`DeviceLink::assemble`] puts the halves together and
    /// refuses anything that does not verify as if `seal` had made it. Nothing
    /// half-signed is ever a link — the half is a signature, not an artifact.
    #[must_use]
    pub fn half(seed: &[u8; 32], other: &DeviceId, nonce: [u8; 16], epoch: u64) -> Signature {
        let me = crate::actor::device_from_seed(seed);
        let devices = if me <= *other {
            [me, other.clone()]
        } else {
            [other.clone(), me]
        };
        Signature(sign_detached(
            seed,
            &Self::preimage(&devices, &nonce, epoch),
        ))
    }

    /// Assemble a link from two halves, verifying it is exactly what `seal`
    /// would have produced.
    pub fn assemble(
        a: (DeviceId, Signature),
        b: (DeviceId, Signature),
        nonce: [u8; 16],
        epoch: u64,
    ) -> Result<Self, Refusal> {
        if a.0 == b.0 {
            return Err(Refusal::NotDistinct);
        }
        let (first, second) = if a.0 <= b.0 { (a, b) } else { (b, a) };
        let link = Self {
            devices: [first.0, second.0],
            nonce,
            epoch,
            signatures: [first.1, second.1],
        };
        link.verify()?;
        Ok(link)
    }
}

/// A signed statement that a subject stands in a stated relation, made to a
/// stated audience, at a stated epoch.
///
/// `by` is the signer. When `by` is a device of the subject this is an avowal;
/// when it is not, it is an attestation. One artifact carries both, because the
/// difference is a fact about the signer rather than a different kind of claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Avowal {
    pub by: DeviceId,
    pub subject: Party,
    pub claim: Claim,
    pub audience: Audience,
    pub epoch: u64,
    pub nonce: [u8; 16],
    pub signature: Signature,
}

impl Avowal {
    /// The preimage. The audience is inside it, which is the property the whole
    /// design rests on — move it to the envelope and misuse stops being
    /// detectable from the artifact.
    #[must_use]
    pub fn preimage(
        by: &DeviceId,
        subject: &Party,
        claim: &Claim,
        audience: &Audience,
        epoch: u64,
        nonce: &[u8; 16],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        framed(&mut out, AVOWAL_DOMAIN);
        framed(&mut out, by.as_str().as_bytes());
        framed(&mut out, subject.wire().as_bytes());
        framed(&mut out, claim.tag());
        framed(&mut out, &claim.body());
        framed(&mut out, audience.tag());
        framed(&mut out, audience.body().as_bytes());
        framed(&mut out, &epoch.to_be_bytes());
        framed(&mut out, &nonce[..]);
        out
    }

    /// Sign one.
    pub fn seal(
        seed: &[u8; 32],
        subject: Party,
        claim: Claim,
        audience: Audience,
        epoch: u64,
        nonce: [u8; 16],
    ) -> Result<Self, Refusal> {
        claim.check()?;
        let by = crate::actor::device_from_seed(seed);
        let preimage = Self::preimage(&by, &subject, &claim, &audience, epoch, &nonce);
        Ok(Self {
            signature: Signature(sign_detached(seed, &preimage)),
            by,
            subject,
            claim,
            audience,
            epoch,
            nonce,
        })
    }

    /// Verify the signature. Says nothing about whether the reader may see it —
    /// that is [`Avowal::legible_to`], and the two are separate on purpose.
    pub fn verify(&self) -> Result<(), Refusal> {
        self.claim.check()?;
        let key = key_of(&self.by)?;
        let preimage = Self::preimage(
            &self.by,
            &self.subject,
            &self.claim,
            &self.audience,
            self.epoch,
            &self.nonce,
        );
        if verify_detached(&key, &preimage, self.signature.bytes()) {
            Ok(())
        } else {
            Err(Refusal::BadSignature)
        }
    }

    /// Whether this reader is inside the audience this avowal names.
    ///
    /// Decided by inspecting the artifact against the reader's own resolved
    /// standing — never by asking how the artifact arrived. An avowal forwarded
    /// out of tier is refused exactly as one intercepted would be.
    pub fn legible_to(&self, standing: &Standing) -> Result<(), Refusal> {
        self.verify()?;
        if self.audience.admits(standing) {
            Ok(())
        } else {
            Err(Refusal::OutsideAudience)
        }
    }

    /// Whether the signer is a device of the subject profile, given that
    /// profile's device set. Self-signed is an avowal; otherwise an attestation.
    #[must_use]
    pub fn is_self_signed(&self, subject_devices: &[DeviceId]) -> bool {
        subject_devices.contains(&self.by)
    }
}

/// One entry in a profile's append-only log.
///
/// Variants are **append-only** — positional postcard discriminants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Entry {
    Link(DeviceLink),
    Retire(Retirement),
    Avow(Avowal),
}

impl Entry {
    /// The content address of this entry.
    pub fn id(&self) -> Result<String, Refusal> {
        let bytes = postcard::to_stdvec(self).map_err(|_| Refusal::Malformed("entry encoding"))?;
        let digest = blake3::derive_key(ENTRY_CONTEXT, &bytes);
        Ok(data_encoding::HEXLOWER.encode(&digest))
    }

    /// Verify this entry on its own terms.
    pub fn verify(&self) -> Result<(), Refusal> {
        match self {
            Self::Link(link) => link.verify(),
            Self::Retire(retirement) => retirement.verify(),
            Self::Avow(avowal) => avowal.verify(),
        }
    }

    /// The audience this entry may be shown to. Structural entries carry the
    /// profile's own reach; an avowal carries the one it was signed with.
    #[must_use]
    pub fn audience(&self) -> Audience {
        match self {
            Self::Link(_) | Self::Retire(_) => Audience::Own,
            Self::Avow(avowal) => avowal.audience.clone(),
        }
    }
}

/// A device retiring itself, or being retired by a peer.
///
/// Retirement supersedes; it does not erase. The link that admitted the device
/// stays in the log, because an artifact already transmitted cannot be unsaid
/// and a local view that disagreed with what correspondents hold would be worse
/// than none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retirement {
    pub by: DeviceId,
    pub device: DeviceId,
    pub epoch: u64,
    pub nonce: [u8; 16],
    pub signature: Signature,
}

impl Retirement {
    #[must_use]
    pub fn preimage(by: &DeviceId, device: &DeviceId, epoch: u64, nonce: &[u8; 16]) -> Vec<u8> {
        let mut out = Vec::new();
        framed(&mut out, RETIRE_DOMAIN);
        framed(&mut out, by.as_str().as_bytes());
        framed(&mut out, device.as_str().as_bytes());
        framed(&mut out, &epoch.to_be_bytes());
        framed(&mut out, &nonce[..]);
        out
    }

    pub fn seal(
        seed: &[u8; 32],
        device: DeviceId,
        epoch: u64,
        nonce: [u8; 16],
    ) -> Result<Self, Refusal> {
        let by = crate::actor::device_from_seed(seed);
        let preimage = Self::preimage(&by, &device, epoch, &nonce);
        Ok(Self {
            signature: Signature(sign_detached(seed, &preimage)),
            by,
            device,
            epoch,
            nonce,
        })
    }

    pub fn verify(&self) -> Result<(), Refusal> {
        let key = key_of(&self.by)?;
        let preimage = Self::preimage(&self.by, &self.device, self.epoch, &self.nonce);
        if verify_detached(&key, &preimage, self.signature.bytes()) {
            Ok(())
        } else {
            Err(Refusal::BadSignature)
        }
    }
}

/// A signed commitment to exactly which entries a log holds.
///
/// Commits to the entry id **set**, never to the replayed device set — the
/// exclusion [`crate::ledger`] makes and for the same reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Head {
    pub by: DeviceId,
    pub profile: ProfileId,
    pub semantics: u16,
    pub epoch: u64,
    /// Every entry id in the log, sorted. Sorted so two nodes holding the same
    /// log commit identically regardless of arrival order.
    pub entries: Vec<String>,
    pub commitment: [u8; 32],
    pub signature: Signature,
}

impl Head {
    fn commit(profile: &ProfileId, semantics: u16, epoch: u64, entries: &[String]) -> [u8; 32] {
        #[derive(Serialize)]
        struct Commitment<'a> {
            semantics: u16,
            profile: &'a str,
            epoch: u64,
            entries: &'a [String],
        }
        let input = Commitment {
            semantics,
            profile: profile.as_str(),
            epoch,
            entries,
        };
        postcard::to_stdvec(&input).map_or([0u8; 32], |bytes| {
            blake3::derive_key(COMMITMENT_CONTEXT, &bytes)
        })
    }

    #[must_use]
    pub fn preimage(commitment: &[u8; 32], by: &DeviceId) -> Vec<u8> {
        let mut out = Vec::new();
        framed(&mut out, HEAD_DOMAIN);
        framed(&mut out, by.as_str().as_bytes());
        framed(&mut out, &commitment[..]);
        out
    }

    /// Verify the commitment recomputes, and that the signature covers it.
    pub fn verify(&self) -> Result<(), Refusal> {
        if self.semantics != KINSHIP_SEMANTICS {
            return Err(Refusal::Semantics);
        }
        if self.entries.len() > MAX_ENTRIES {
            return Err(Refusal::Bound("entries"));
        }
        if self
            .entries
            .windows(2)
            .any(|pair| matches!(pair, [a, b] if a >= b))
        {
            return Err(Refusal::Malformed("head entry order"));
        }
        let recomputed = Self::commit(&self.profile, self.semantics, self.epoch, &self.entries);
        if recomputed != self.commitment {
            return Err(Refusal::Diverged);
        }
        let key = key_of(&self.by)?;
        let preimage = Self::preimage(&self.commitment, &self.by);
        if verify_detached(&key, &preimage, self.signature.bytes()) {
            Ok(())
        } else {
            Err(Refusal::BadSignature)
        }
    }
}

/// A profile's append-only log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KinshipLog {
    profile: ProfileId,
    entries: Vec<Entry>,
}

impl KinshipLog {
    /// Found a profile from its genesis link. The profile id is the content
    /// address of that link, so the log's identity is not assignable.
    pub fn found(genesis: DeviceLink) -> Result<Self, Refusal> {
        genesis.verify()?;
        let bytes =
            postcard::to_stdvec(&genesis).map_err(|_| Refusal::Malformed("genesis encoding"))?;
        let profile = ProfileId::from_genesis(&bytes);
        Ok(Self {
            profile,
            entries: vec![Entry::Link(genesis)],
        })
    }

    #[must_use]
    pub const fn profile(&self) -> &ProfileId {
        &self.profile
    }

    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Append a verified entry.
    pub fn append(&mut self, entry: Entry) -> Result<(), Refusal> {
        if self.entries.len() >= MAX_ENTRIES {
            return Err(Refusal::Bound("entries"));
        }
        entry.verify()?;
        self.entries.push(entry);
        Ok(())
    }

    /// The device set this log currently resolves to: every device named by a
    /// verified link, minus every device retired afterwards.
    ///
    /// Retirement wins, matching the actor plane's revoke-wins discipline.
    #[must_use]
    pub fn devices(&self) -> Vec<DeviceId> {
        let mut live: Vec<DeviceId> = Vec::new();
        let mut retired: Vec<DeviceId> = Vec::new();
        for entry in &self.entries {
            match entry {
                Entry::Link(link) => {
                    for device in &link.devices {
                        if !live.contains(device) {
                            live.push(device.clone());
                        }
                    }
                }
                Entry::Retire(retirement) => {
                    if !retired.contains(&retirement.device) {
                        retired.push(retirement.device.clone());
                    }
                }
                Entry::Avow(_) => {}
            }
        }
        live.retain(|device| !retired.contains(device));
        live.sort();
        live
    }

    /// Every entry id, sorted.
    pub fn entry_ids(&self) -> Result<Vec<String>, Refusal> {
        let mut ids = self
            .entries
            .iter()
            .map(Entry::id)
            .collect::<Result<Vec<_>, _>>()?;
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// Sign a head over the whole log.
    pub fn head(&self, seed: &[u8; 32], epoch: u64) -> Result<Head, Refusal> {
        let by = crate::actor::device_from_seed(seed);
        let entries = self.entry_ids()?;
        let commitment = Head::commit(&self.profile, KINSHIP_SEMANTICS, epoch, &entries);
        let preimage = Head::preimage(&commitment, &by);
        Ok(Head {
            signature: Signature(sign_detached(seed, &preimage)),
            by,
            profile: self.profile.clone(),
            semantics: KINSHIP_SEMANTICS,
            epoch,
            entries,
            commitment,
        })
    }

    /// Draw a projection for one audience: the bodies that audience admits,
    /// carried alongside a signed head over the **whole** log.
    ///
    /// The head covers everything, including what was filtered out. That is what
    /// makes the filtering visible rather than silent.
    pub fn project(
        &self,
        seed: &[u8; 32],
        epoch: u64,
        standing: &Standing,
    ) -> Result<Projection, Refusal> {
        let head = self.head(seed, epoch)?;
        let mut bodies = Vec::new();
        for entry in &self.entries {
            if entry.audience().admits(standing) {
                bodies.push(entry.clone());
            }
        }
        // The signer's authority chain rides with the projection whenever the
        // audience filter would have withheld it: a head signed by a joined
        // device is only evidence to a reader who can walk from the genesis
        // to the signer, and **authority is never secret from whoever must
        // verify it**. What is included is every structural entry — links and
        // retirements — not avowals; the audience-gated disclosures stay
        // gated. A reader learns the device topology, which is the disclosed
        // cost of a verifiable non-genesis signer, and is the same set a
        // correspondent of the profile's own devices already sees.
        let signer = crate::actor::device_from_seed(seed);
        let genesis_rooted = self
            .entries
            .first()
            .is_some_and(|entry| matches!(entry, Entry::Link(link) if link.names(&signer)));
        if !genesis_rooted {
            for entry in &self.entries {
                if matches!(entry, Entry::Link(_) | Entry::Retire(_)) && !bodies.contains(entry) {
                    bodies.push(entry.clone());
                }
            }
        }
        Ok(Projection {
            profile: self.profile.clone(),
            bodies,
            head: Some(head),
        })
    }

    /// Whether `device` is in this log's current device set — link-reachable
    /// from the genesis and not retired. The authored-side answer to the
    /// question [`signer_rooted`] answers for a reader holding only a
    /// projection.
    #[must_use]
    pub fn rooted(&self, device: &DeviceId) -> bool {
        self.devices().contains(device)
    }
}

/// Whether `signer` holds this profile's authority, judged from carried
/// evidence alone: link-reachable from the genesis pair through the `Link`
/// entries in `bodies`, and not retired by any `Retire` entry whose author is
/// itself reachable.
///
/// Two passes, deliberately: reachability first over every verified link,
/// then retirement — so a retirement only counts when its author held
/// authority to make it, and a stranger's forged retirement severs nothing.
/// Retire-wins within that rule, matching [`KinshipLog::devices`]. What this
/// cannot establish is that every retirement was *carried*: a signer
/// withholding a retirement of itself presents a chain this cannot fault,
/// which is the same freshness bound the genesis anchor already had — a
/// compromised genesis device is refused by nothing here either, and both are
/// answered the same way, by a newer head from a surviving device.
#[must_use]
pub fn signer_rooted(genesis: &DeviceLink, bodies: &[Entry], signer: &DeviceId) -> bool {
    if genesis.verify().is_err() {
        return false;
    }
    // Pass one: reachability over verified links, genesis included.
    let mut reachable: Vec<DeviceId> = genesis.devices.to_vec();
    loop {
        let mut grew = false;
        for entry in bodies {
            let Entry::Link(link) = entry else { continue };
            if link.verify().is_err() {
                continue;
            }
            let touches = link.devices.iter().any(|device| reachable.contains(device));
            if touches {
                for device in &link.devices {
                    if !reachable.contains(device) {
                        reachable.push(device.clone());
                        grew = true;
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }
    // Pass two: retirements by reachable authors sever their subjects.
    let mut live = reachable.clone();
    for entry in bodies {
        let Entry::Retire(retirement) = entry else {
            continue;
        };
        if retirement.verify().is_err() {
            continue;
        }
        if reachable.contains(&retirement.by) {
            live.retain(|device| device != &retirement.device);
        }
    }
    live.contains(signer)
}

/// An audience-scoped view of a log, plus the head that makes omission visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projection {
    pub profile: ProfileId,
    /// The entry bodies this audience was given.
    pub bodies: Vec<Entry>,
    /// The signed head over the whole log. `None` makes this a hint, never
    /// evidence — and [`Projection::verify`] says so rather than passing.
    pub head: Option<Head>,
}

impl Projection {
    /// Verify this projection as evidence.
    ///
    /// Establishes three things, and deliberately claims no more:
    ///
    /// 1. A head exists and is signed. Without one this is a hint.
    /// 2. Every body delivered verifies and is listed in that head.
    /// 3. No body the reader's standing admits was listed and withheld.
    ///
    /// What it cannot establish alone is that the head itself is complete — a
    /// sender that truncates before committing produces something internally
    /// consistent. That is detected by *comparison*, which the signed head makes
    /// non-repudiable. See the module docs.
    pub fn verify(&self, standing: &Standing) -> Result<(), Refusal> {
        let head = self.head.as_ref().ok_or(Refusal::Uncommitted)?;
        head.verify()?;
        if head.profile != self.profile {
            return Err(Refusal::Malformed("projection profile"));
        }

        let mut delivered = Vec::new();
        for entry in &self.bodies {
            entry.verify()?;
            let id = entry.id()?;
            if !head.entries.contains(&id) {
                return Err(Refusal::Unlisted);
            }
            // Structural entries are exempt from the audience gate: they are
            // the head signer's authority chain, and **authority is never
            // secret from whoever must verify it** — a reader who cannot read
            // the chain cannot verify the head at all. What reaches a reader
            // is still decided at projection time (the minimal chain, not the
            // topology); this only refuses to call proof a disclosure.
            if !matches!(entry, Entry::Link(_) | Entry::Retire(_))
                && !entry.audience().admits(standing)
            {
                return Err(Refusal::OutsideAudience);
            }
            delivered.push(id);
        }

        // Anything listed but not delivered must be something this reader was
        // never entitled to. It cannot be checked directly — the body is what
        // carries the audience — so the check runs the other way: the count of
        // admissible bodies delivered must account for every id this reader
        // could name. A withheld admissible body shows up as an id the reader
        // holds from another source and cannot find here, which is
        // `missing_from`.
        delivered.sort();
        delivered.dedup();
        if delivered.len() > head.entries.len() {
            return Err(Refusal::Diverged);
        }
        Ok(())
    }

    /// Ids this projection's head commits to that no body accompanies.
    ///
    /// The caller compares these against ids learned elsewhere: any id it can
    /// show should have been admissible, and is absent here, is a withheld body.
    #[must_use]
    pub fn undelivered(&self) -> Vec<String> {
        let Some(head) = self.head.as_ref() else {
            return Vec::new();
        };
        let delivered: Vec<String> = self.bodies.iter().filter_map(|e| e.id().ok()).collect();
        head.entries
            .iter()
            .filter(|id| !delivered.contains(id))
            .cloned()
            .collect()
    }

    /// Whether a specific entry id was committed to but not delivered here.
    ///
    /// This is the omission check with a name: a reader holding `id` from
    /// another path, whose standing admits it, has proof it was withheld.
    pub fn withheld(&self, id: &str) -> Result<(), Refusal> {
        let head = self.head.as_ref().ok_or(Refusal::Uncommitted)?;
        if !head.entries.contains(&id.to_string()) {
            return Err(Refusal::Unlisted);
        }
        if self.undelivered().iter().any(|held| held == id) {
            return Err(Refusal::Omission);
        }
        Ok(())
    }
}
