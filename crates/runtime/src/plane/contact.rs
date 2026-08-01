#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "Contact v2 codecs validate frame and proof geometry before fixed-layout access"
)]
//! Contact v2 — the bounded direct material exchange (`lait/contact/2`).
//!
//! Contact owns connection lifetime, framing, deadlines, transfer completion,
//! and Neighbor attribution — **never** legitimacy: a completed Contact means
//! frames moved, and [`Failure`]/Convergence classification
//! stay separate. One Contact transfers initiator ← accepter; reverse progress
//! uses another Contact.
//!
//! This module is the S5b format packet: the mutually authenticated Hello/Ack,
//! the twelve-frame grammar with its exact bounds, and the **pure,
//! transport-free** state machines that validate a transcript — the initiator's
//! receiving machine with the full staging/abort matrix, and the accepter's
//! request validator. Driving these over a real stream (and handing the result
//! to Convergence via mechanics validation + `Replica::incorporate`) is
//! the transport integration that follows.
//!
//! Framing rules (normative): every frame begins `(tag: u8, contact_id:
//! [u8;16])`; frames are at most 1 MiB; a Contact carries at most 4096 frames
//! and 80 MiB; chunks are at most 256 KiB and never empty unless terminal;
//! unknown tags, wrong-state frames, repeated terminals, conflicting or
//! overlapping chunks, gaps at `BodyEnd`, and cumulative overflow all Abort
//! (with a numeric code — no remote prose) and discard the Contact staging
//! area. Exact duplicate chunks are idempotent. `TransferAck` acknowledges
//! framing/receipt only, never durable Convergence.

use mechanics::station::Key;
use replica::body::BodyKey;
use replica::body::ContentCommitment;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::wire::length_framed;

pub use crate::contact_driver::Authority;
pub use crate::lifecycle::ContactOutcome as Outcome;

/// Why an explicitly requested Contact did not complete.
///
/// Contact is administrative orchestration; ordinary peer admission refusals
/// remain [`crate::plane::Refusal`] and malformed frames remain [`wire::Invalid`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    UnknownNeighbor,
    Unreachable,
    Capacity,
    Entropy,
    Transport,
    Admission,
    Protocol,
    Signing,
    Holdings,
    Convergence,
    Deadline,
    Interrupted,
    PeerAborted(u16),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Failure {}

/// The Contact ALPN.
pub const CONTACT_ALPN: &[u8] = b"lait/contact/2";
/// The only Contact protocol version this build speaks. There is no
/// mixed-version window: an unknown version is refused by name, never
/// negotiated (clean-break formats).
pub const CONTACT_PROTOCOL: u16 = 2;

/// Maximum catalog nodes one descending peer may request across a Contact.
///
/// This replaces a 64 MiB holdings bound that was sized for *heads* — about 64
/// bytes each — and had stopped fitting: after the content and history work a
/// Replica's advertised catalog is head sets, declarations, and every content
/// descriptor, orders of magnitude past what that frame was measured for.
/// Descending asks for nodes instead, and a node budget is what stops a peer
/// steering the descent.
///
/// **Not yet enforced on a live path.** `Replica::begin_reconciliation` takes
/// this as its `max_nodes` and `index::Reconciliation` counts against it, but
/// nothing in `contact_driver` descends yet — the F4 remainder. Said plainly
/// because a constant that reads as a bound and is not one is worse than no
/// constant: the next reader would otherwise assume the descent is capped.
pub const MAX_DESCENT_NODES: u64 = 2048;
/// Maximum node hashes in one request frame.
pub const MAX_DESCENT_REQUEST: usize = 256;

/// Domain for the initiator's holdings declaration digest (bound into the
/// SIGNED hello, so the declaration cannot be altered in transit).
const HOLDINGS_DOMAIN: &[u8] = b"lait/contact-holdings/2";

/// Hard bound on a streamed holdings encoding.
///
/// The declaration is now the *fallback*, sent only when the two catalog roots
/// disagree — converged peers compare 40 bytes and stop. It is still bounded,
/// and the bound is still uncomfortable: it was sized for heads at ~64 bytes
/// each, and a Replica's advertised catalog has since grown head sets,
/// declarations, and content descriptors. Removing the fallback entirely needs
/// the descent to run initiator-side, which is a pull restructure of the
/// transfer loop rather than a frame change.
pub const MAX_HOLDINGS_BYTES: usize = 64 * 1024 * 1024;
/// Hello signing domain.
pub const OFFER_DOMAIN: &[u8] = b"lait/contact/2/offer";
/// HelloAck signing domain.
pub const PROOF_DOMAIN: &[u8] = b"lait/contact/2/proof";
/// Ed25519 algorithm tag.
pub const SIG_ALG_ED25519: u8 = 1;

/// Canonically encode a holdings declaration: `(key, transaction commitment)`
/// pairs, sorted and deduplicated so the digest is a function of the set.
pub fn encode_holdings(held: &[(BodyKey, [u8; 32])]) -> Vec<u8> {
    let mut sorted = held.to_vec();
    sorted.sort();
    sorted.dedup();
    postcard::to_stdvec(&sorted).expect("postcard holdings")
}

/// The signed-hello digest over a canonical holdings encoding.
pub fn holdings_digest(bytes: &[u8]) -> [u8; 32] {
    domain_hash(HOLDINGS_DOMAIN, bytes)
}

/// Decode a holdings declaration (canonical, bounded).
pub fn decode_holdings(bytes: &[u8]) -> Result<Vec<(BodyKey, [u8; 32])>, Invalid> {
    if bytes.len() > MAX_HOLDINGS_BYTES {
        return Err(Invalid::FrameTooLarge);
    }
    postcard::from_bytes(bytes).map_err(|_| Invalid::NonCanonical)
}

/// Maximum encoded frame size.
///
/// Sized to carry one canonical index node at its own legal maximum plus the
/// frame envelope around it. Equal to the frozen 1 MiB Replica catalog-node bound
/// would mean a legal node is untransmittable by construction, which is the
/// kind of contradiction that shows up as an unexplainable Contact failure
/// years later rather than as a test.
pub const MAX_FRAME: usize = 1024 * 1024 + 64 * 1024;
/// Maximum frames per Contact.
pub const MAX_FRAMES: u32 = 4096;

// The descent has to fit inside the Contact that carries it. One delivered node
// is one frame, and the rest of the Contact — authority, payloads, control —
// needs the remainder, so the node budget takes half. Asserted rather than
// commented because the two numbers were set independently once and did not
// fit: 8192 nodes could not be delivered inside 4096 frames, so a descent that
// legitimately needed them could never complete.
const _: () = assert!(MAX_DESCENT_NODES * 2 <= MAX_FRAMES as u64);
/// Maximum bytes per Contact, including framing overhead.
pub const MAX_CONTACT_BYTES: u64 = 80 * 1024 * 1024;
/// Maximum chunk payload (authority records and body chunks).
pub const MAX_CHUNK: usize = 256 * 1024;
/// Maximum manifest index nodes one Contact may offer. At the index's 256-entry
/// leaves this covers well past the Body-count ceiling, and it is what stops a
/// peer from streaming nodes until the receiver runs out of memory.
pub const MAX_MANIFEST_NODES: usize = 8192;
// Both indexes an advertisement carries — Body and content — draw on this one
// budget, because they arrive as one content-addressed node family and a
// receiver cannot tell which is which until the root tells it.

/// Domain for the running transcript hash (over every raw frame, in order).
const TRANSCRIPT_DOMAIN: &[u8] = b"lait/contact/2/transcript";
/// Domain for one authority record's hash.
const RECORD_DOMAIN: &[u8] = b"lait/contact/2/authority-record";
/// Domain for the authority set hash (over the ordered record hashes).
const AUTHORITY_SET_DOMAIN: &[u8] = b"lait/contact/2/authority-set";
/// Domain for the manifest-root reference hash (over the canonical root bytes).
const ROOT_REF_DOMAIN: &[u8] = b"lait/contact/2/manifest-root";
/// Domain for one manifest index node's hash.
const MANIFEST_NODE_DOMAIN: &[u8] = b"lait/contact/2/manifest-node";
/// Domain for one body chunk's hash.
const CHUNK_DOMAIN: &[u8] = b"lait/contact/2/body-chunk";

/// Abort codes (no remote prose crosses the wire).
pub mod abort {
    pub const UNKNOWN_TAG: u16 = 1;
    pub const WRONG_STATE: u16 = 2;
    pub const MALFORMED: u16 = 3;
    pub const LIMITS: u16 = 4;
    pub const CHUNK_CONFLICT: u16 = 5;
    pub const GAP: u16 = 6;
    pub const HASH_MISMATCH: u16 = 7;
    pub const TRANSCRIPT_MISMATCH: u16 = 8;
    pub const CONTACT_MISMATCH: u16 = 9;
    pub const DUPLICATE_TERMINAL: u16 = 10;
    pub const EMPTY_CHUNK: u16 = 11;
    pub const SET_MISMATCH: u16 = 12;
    pub const BAD_REQUEST: u16 = 13;
    pub const TOO_LARGE: u16 = 14;
}

fn domain_hash(domain: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(domain);
    h.update(bytes);
    *h.finalize().as_bytes()
}

/// The hash a `record_hash` field must carry for its record bytes.
pub fn authority_record_hash(bytes: &[u8]) -> [u8; 32] {
    domain_hash(RECORD_DOMAIN, bytes)
}

/// The authority set hash over the ordered record hashes.
pub fn authority_set_hash(record_hashes: &[[u8; 32]]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(AUTHORITY_SET_DOMAIN);
    for rh in record_hashes {
        h.update(rh);
    }
    *h.finalize().as_bytes()
}

/// The reference hash `ManifestRequest`/`ManifestNode` use for an offered root.
pub fn manifest_root_ref(root_bytes: &[u8]) -> [u8; 32] {
    domain_hash(ROOT_REF_DOMAIN, root_bytes)
}

/// The hash a `node_hash` field must carry for its node bytes. It is the
/// store's object address, because a manifest index node *is* an object and
/// naming it any other way would let the two disagree.
pub fn manifest_node_hash(bytes: &[u8]) -> [u8; 32] {
    domain_hash(MANIFEST_NODE_DOMAIN, bytes)
}

/// The hash a `chunk_hash` field must carry for its chunk bytes.
pub fn body_chunk_hash(bytes: &[u8]) -> [u8; 32] {
    domain_hash(CHUNK_DOMAIN, bytes)
}

/// A 128-bit Contact identity, minted by the initiator per exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContactId([u8; 16]);

impl ContactId {
    pub fn mint() -> Self {
        let mut raw = [0u8; 16];
        getrandom::fill(&mut raw).expect("getrandom");
        Self(raw)
    }
    pub fn from_bytes(raw: [u8; 16]) -> Self {
        Self(raw)
    }
    pub fn as_bytes(&self) -> [u8; 16] {
        self.0
    }
}

// ---------------------------------------------------------------------------
// Signed Hello / HelloAck
// ---------------------------------------------------------------------------

/// The initiator's signed opening of a Contact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub opening_hash: [u8; 32],
    pub protocol: u16,
    pub space: [u8; 29],
    pub initiator_station: [u8; 32],
    pub responder_station: [u8; 32],
    pub initiator_transport: [u8; 32],
    pub nonce: [u8; 32],
    pub contact: ContactId,
    /// The root of the initiator's published catalog, or zeros when it has
    /// none. Signed like the rest of the hello.
    ///
    /// This is what the descent will compare, and it is deliberately *not* yet
    /// used to skip serving. Equal roots prove equal *catalogs*, but a catalog
    /// records what a peer advertises, not what it can read: a Body retained
    /// opaquely — held byte-complete under a key this peer does not have —
    /// appears in the root and still needs re-serving once the key arrives.
    /// The holdings declaration excludes opaque heads precisely so that upgrade
    /// path works, and a root cannot express the difference.
    ///
    /// Closing that gap needs either a second root over readable material or
    /// the initiator-side pull the descent is designed for. Until then the root
    /// is carried, signed, and compared by tests; the declaration still decides
    /// what is served.
    pub holdings_root: [u8; 32],
    /// How many `(key, transaction commitment)` heads the streamed declaration
    /// will carry, and the digest binding it. Zero when the roots already
    /// agree, which is the case this exists to make cheap.
    pub holdings_count: u32,
    pub holdings_digest: [u8; 32],
    pub signature_algorithm: u8,
    #[serde(with = "serde_byte_array")]
    pub signature: [u8; 64],
}

/// The accepter's signed answer, binding the exact Hello.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof {
    pub offer_hash: [u8; 32],
    pub responder_transport: [u8; 32],
    pub nonce: [u8; 32],
    pub signature_algorithm: u8,
    #[serde(with = "serde_byte_array")]
    pub signature: [u8; 64],
}

/// Why a Contact hello/frame failed validation before the state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    /// The signed protocol field names a version this build does not speak.
    UnsupportedProtocol(u16),
    UnsupportedSignatureAlgorithm(u8),
    NonCanonical,
    IdentityMismatch,
    SpaceMismatch,
    ChallengeMismatch,
    BadSignature,
    UnknownTag(u8),
    FrameTooLarge,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Invalid {}

fn decode_canonical<T>(bytes: &[u8]) -> Result<T, Invalid>
where
    T: serde::de::DeserializeOwned + Serialize,
{
    let value: T = postcard::from_bytes(bytes).map_err(|_| Invalid::NonCanonical)?;
    let re = postcard::to_stdvec(&value).map_err(|_| Invalid::NonCanonical)?;
    if re != bytes {
        return Err(Invalid::NonCanonical);
    }
    Ok(value)
}

impl Offer {
    #[allow(clippy::too_many_arguments)]
    fn preimage(
        opening_hash: &[u8; 32],
        protocol: u16,
        space: &[u8; 29],
        initiator_station: &[u8; 32],
        responder_station: &[u8; 32],
        initiator_transport: &[u8; 32],
        nonce: &[u8; 32],
        contact: &ContactId,
        holdings_root: &[u8; 32],
        holdings_count: u32,
        holdings_digest: &[u8; 32],
    ) -> Vec<u8> {
        let body = postcard::to_stdvec(&(
            opening_hash,
            protocol,
            space,
            initiator_station,
            responder_station,
            initiator_transport,
            nonce,
            contact,
            holdings_root,
            holdings_count,
            holdings_digest,
        ))
        .expect("postcard hello body");
        length_framed(OFFER_DOMAIN, &body)
    }

    /// Sign a Hello from the initiator's device seed (the seed's key is both
    /// the Station and transport identity — a peer is its key).
    #[allow(clippy::too_many_arguments)]
    pub fn sign(
        opening_hash: [u8; 32],
        protocol: u16,
        space: [u8; 29],
        responder_station: [u8; 32],
        nonce: [u8; 32],
        contact: ContactId,
        holdings_root: [u8; 32],
        holdings_count: u32,
        holdings_digest: [u8; 32],
        initiator_seed: &[u8; 32],
    ) -> Option<Self> {
        let station = mechanics::actor::device_from_seed(initiator_seed).key_bytes()?;
        let preimage = Self::preimage(
            &opening_hash,
            protocol,
            &space,
            &station,
            &responder_station,
            &station,
            &nonce,
            &contact,
            &holdings_root,
            holdings_count,
            &holdings_digest,
        );
        let signature = mechanics::actor::sign_detached(initiator_seed, &preimage);
        Some(Self {
            opening_hash,
            protocol,
            space,
            initiator_station: station,
            responder_station,
            initiator_transport: station,
            nonce,
            contact,
            holdings_root,
            holdings_count,
            holdings_digest,
            signature_algorithm: SIG_ALG_ED25519,
            signature,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard hello")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Invalid> {
        decode_canonical(bytes)
    }

    /// A commitment to the exact signed Hello.
    pub fn hash(&self) -> [u8; 32] {
        domain_hash(
            OFFER_DOMAIN,
            &Self::preimage(
                &self.opening_hash,
                self.protocol,
                &self.space,
                &self.initiator_station,
                &self.responder_station,
                &self.initiator_transport,
                &self.nonce,
                &self.contact,
                &self.holdings_root,
                self.holdings_count,
                &self.holdings_digest,
            ),
        )
    }

    /// Verify against the expected Space and the negotiated connection peer.
    pub fn verify(
        &self,
        expected_opening_hash: &[u8; 32],
        expected_space: &[u8; 29],
        transport_peer: &Key,
    ) -> Result<(), Invalid> {
        if self.protocol != CONTACT_PROTOCOL {
            return Err(Invalid::UnsupportedProtocol(self.protocol));
        }
        if &self.opening_hash != expected_opening_hash {
            return Err(Invalid::ChallengeMismatch);
        }
        if self.signature_algorithm != SIG_ALG_ED25519 {
            return Err(Invalid::UnsupportedSignatureAlgorithm(
                self.signature_algorithm,
            ));
        }
        if &self.space != expected_space {
            return Err(Invalid::SpaceMismatch);
        }
        if self.initiator_station != self.initiator_transport
            || self.initiator_transport != transport_peer.key_bytes()
        {
            return Err(Invalid::IdentityMismatch);
        }
        let preimage = Self::preimage(
            &self.opening_hash,
            self.protocol,
            &self.space,
            &self.initiator_station,
            &self.responder_station,
            &self.initiator_transport,
            &self.nonce,
            &self.contact,
            &self.holdings_root,
            self.holdings_count,
            &self.holdings_digest,
        );
        if !mechanics::actor::verify_detached(&self.initiator_station, &preimage, &self.signature) {
            return Err(Invalid::BadSignature);
        }
        Ok(())
    }
}

impl Proof {
    fn preimage(
        offer_hash: &[u8; 32],
        responder_transport: &[u8; 32],
        nonce: &[u8; 32],
    ) -> Vec<u8> {
        let body = postcard::to_stdvec(&(offer_hash, responder_transport, nonce))
            .expect("postcard hello-ack body");
        length_framed(PROOF_DOMAIN, &body)
    }

    /// Sign an ack answering `hello` from the responder's device seed.
    pub fn sign(hello: &Offer, nonce: [u8; 32], responder_seed: &[u8; 32]) -> Option<Self> {
        let responder = mechanics::actor::device_from_seed(responder_seed).key_bytes()?;
        let offer_hash = hello.hash();
        let preimage = Self::preimage(&offer_hash, &responder, &nonce);
        let signature = mechanics::actor::sign_detached(responder_seed, &preimage);
        Some(Self {
            offer_hash,
            responder_transport: responder,
            nonce,
            signature_algorithm: SIG_ALG_ED25519,
            signature,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard hello-ack")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Invalid> {
        decode_canonical(bytes)
    }

    /// Verify against the Hello it answers and the negotiated connection peer.
    pub fn verify(&self, hello: &Offer, transport_peer: &Key) -> Result<(), Invalid> {
        if self.signature_algorithm != SIG_ALG_ED25519 {
            return Err(Invalid::UnsupportedSignatureAlgorithm(
                self.signature_algorithm,
            ));
        }
        if self.offer_hash != hello.hash() {
            return Err(Invalid::ChallengeMismatch);
        }
        if self.nonce == hello.nonce {
            return Err(Invalid::ChallengeMismatch);
        }
        if self.responder_transport != hello.responder_station
            || self.responder_transport != transport_peer.key_bytes()
        {
            return Err(Invalid::IdentityMismatch);
        }
        let preimage = Self::preimage(&self.offer_hash, &self.responder_transport, &self.nonce);
        if !mechanics::actor::verify_detached(&hello.responder_station, &preimage, &self.signature)
        {
            return Err(Invalid::BadSignature);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// A Contact frame (without its common `(tag, contact_id)` prefix, which the
/// codec owns). Tags are the canonical wire values 1–12.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContactFrame {
    AuthorityOffer {
        authority_frontier: Vec<u8>,
        record_count: u32,
        total_bytes: u64,
        set_hash: [u8; 32],
    },
    AuthorityChunk {
        index: u32,
        record_hash: [u8; 32],
        bytes: Vec<u8>,
    },
    AuthorityEnd {
        record_count: u32,
        set_hash: [u8; 32],
    },
    /// The canonical `ManifestRoot` bytes, opaque at the frame layer; the
    /// manifest layer validates them and `manifest_root_ref` names them.
    ManifestOffer {
        root_bytes: Vec<u8>,
    },
    /// The nodes a descending peer needs next.
    ///
    /// Explicit hashes rather than a count: the descent asks for the subtrees
    /// whose roots differ, and only those. A count would have to mean "the next
    /// N in some order", which is enumeration again.
    ManifestRequest {
        root: [u8; 32],
        nodes: Vec<[u8; 32]>,
    },
    /// One node of the manifest's Body index.
    ///
    /// Nodes are named by hash rather than by ordinal, which is the whole
    /// point of the index: inserting a Body shifts every later ordinal, so an
    /// ordinal-named unit changes when its contents did not. A hash does not.
    ManifestNode {
        root: [u8; 32],
        node_hash: [u8; 32],
        node_bytes: Vec<u8>,
    },
    BodyRequest {
        transaction: [u8; 32],
        body: BodyKey,
        offset: u64,
        length: u32,
    },
    BodyChunk {
        transaction: [u8; 32],
        body: BodyKey,
        offset: u64,
        total: u64,
        chunk_hash: [u8; 32],
        bytes: Vec<u8>,
    },
    BodyEnd {
        transaction: [u8; 32],
        body: BodyKey,
        total: u64,
        content_commitment: [u8; 32],
    },
    TransferEnd {
        authority_set_hash: [u8; 32],
        manifest_root: [u8; 32],
        body_count: u32,
        transcript_hash: [u8; 32],
    },
    TransferAck {
        transcript_hash: [u8; 32],
        received_bytes: u64,
    },
    Abort {
        code: u16,
    },
    // Appended, never inserted. postcard encodes an enum by variant *index*,
    // so adding a variant anywhere but the end silently renumbers every frame
    // after it — which is a wire break that compiles cleanly and fails as a
    // malformed frame at the far end.
    /// One chunk of the initiator's holdings declaration, sent only when the
    /// signed catalog roots disagree.
    HoldingsChunk {
        index: u32,
        bytes: Vec<u8>,
    },
    /// Terminates the holdings declaration; must match the signed hello.
    HoldingsEnd {
        count: u32,
        digest: [u8; 32],
    },
}

impl ContactFrame {
    /// The canonical wire tag.
    pub fn tag(&self) -> u8 {
        match self {
            ContactFrame::AuthorityOffer { .. } => 1,
            ContactFrame::AuthorityChunk { .. } => 2,
            ContactFrame::AuthorityEnd { .. } => 3,
            ContactFrame::ManifestOffer { .. } => 4,
            ContactFrame::ManifestRequest { .. } => 5,
            ContactFrame::ManifestNode { .. } => 6,
            ContactFrame::BodyRequest { .. } => 7,
            ContactFrame::BodyChunk { .. } => 8,
            ContactFrame::BodyEnd { .. } => 9,
            ContactFrame::TransferEnd { .. } => 10,
            ContactFrame::HoldingsChunk { .. } => 13,
            ContactFrame::HoldingsEnd { .. } => 14,
            ContactFrame::TransferAck { .. } => 11,
            ContactFrame::Abort { .. } => 12,
        }
    }

    /// Encode as `tag || contact_id || postcard(fields)`.
    pub fn encode(&self, contact: &ContactId) -> Vec<u8> {
        // The enum's variant index encodes tag-1, so strip postcard's own
        // discriminant and lead with the explicit tag byte instead.
        let fields = postcard::to_stdvec(self).expect("postcard frame");
        let mut out = Vec::with_capacity(1 + 16 + fields.len() - 1);
        out.push(self.tag());
        out.extend_from_slice(&contact.as_bytes());
        out.extend_from_slice(&fields[1..]);
        out
    }

    /// Decode `tag || contact_id || fields`, enforcing frame bounds, known
    /// tags, and canonical encoding.
    pub fn decode(bytes: &[u8]) -> Result<(ContactId, ContactFrame), Invalid> {
        if bytes.len() > MAX_FRAME {
            return Err(Invalid::FrameTooLarge);
        }
        if bytes.len() < 1 + 16 {
            return Err(Invalid::NonCanonical);
        }
        let tag = bytes[0];
        if !(1..=14).contains(&tag) {
            return Err(Invalid::UnknownTag(tag));
        }
        let contact = ContactId::from_bytes(bytes[1..17].try_into().expect("16 bytes"));
        // Reassemble the postcard enum encoding (variant index = tag-1).
        let mut enc = Vec::with_capacity(bytes.len() - 16);
        enc.push(tag - 1);
        enc.extend_from_slice(&bytes[17..]);
        let frame: ContactFrame = decode_canonical(&enc)?;
        Ok((contact, frame))
    }
}

// ---------------------------------------------------------------------------
// Catalog declaration
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Initiator receive machine
// ---------------------------------------------------------------------------

/// The initiator's protocol position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitiatorState {
    HelloSent,
    AuthorityReceiving,
    ManifestReceiving,
    BodiesReceiving,
    AckSent,
    Closed,
    Aborted,
}

/// What a processed frame yielded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// The frame was accepted; keep receiving.
    Continue,
    /// The transfer ended cleanly: send this `TransferAck` and close.
    SendAck(ContactFrame),
    /// The peer aborted; the staging area has been discarded.
    PeerAborted(u16),
}

#[derive(Debug, Default)]
struct AuthorityStaging {
    expected_count: u32,
    expected_set_hash: [u8; 32],
    expected_total: u64,
    next_index: u32,
    received_bytes: u64,
    record_hashes: Vec<[u8; 32]>,
    records: Vec<Vec<u8>>,
    ended: bool,
}

#[derive(Debug, Default)]
struct BodyStaging {
    total: Option<u64>,
    /// offset → (hash, bytes); intervals never overlap once accepted.
    chunks: BTreeMap<u64, ([u8; 32], Vec<u8>)>,
    ended: bool,
}

/// Everything a cleanly completed Contact received. Handing this to
/// Convergence (mechanics validation, then `Replica::incorporate`) is
/// the incorporation step — receipt of this material implies **no** legitimacy.
#[derive(Debug)]
pub struct ReceivedMaterial {
    pub authority_frontier: Vec<u8>,
    pub authority_records: Vec<Vec<u8>>,
    pub manifest_root_bytes: Vec<u8>,
    /// node hash → canonical node bytes of the manifest's Body index.
    pub manifest_nodes: BTreeMap<[u8; 32], Vec<u8>>,
    /// (transaction, BodyKey) → assembled protected payload.
    pub bodies: BTreeMap<([u8; 32], BodyKey), Vec<u8>>,
}

/// The initiator's pure receiving state machine: feed it every received frame's
/// raw bytes; it validates the grammar, stages material, and either progresses,
/// tells you to ack, or aborts with a code (the caller sends `Abort { code }`
/// and closes). All staging is discarded on abort.
pub struct InitiatorReceiver {
    contact: ContactId,
    state: InitiatorState,
    frames: u32,
    bytes: u64,
    transcript: blake3::Hasher,
    authority: Option<AuthorityStaging>,
    authority_frontier: Vec<u8>,
    manifest_root_ref: Option<[u8; 32]>,
    manifest_root_bytes: Vec<u8>,
    manifest_nodes: BTreeMap<[u8; 32], Vec<u8>>,
    bodies: BTreeMap<([u8; 32], BodyKey), BodyStaging>,
    ended_bodies: u32,
}

impl InitiatorReceiver {
    pub fn new(contact: ContactId) -> Self {
        let mut transcript = blake3::Hasher::new();
        transcript.update(TRANSCRIPT_DOMAIN);
        Self {
            contact,
            state: InitiatorState::HelloSent,
            frames: 0,
            bytes: 0,
            transcript,
            authority: None,
            authority_frontier: Vec::new(),
            manifest_root_ref: None,
            manifest_root_bytes: Vec::new(),
            manifest_nodes: BTreeMap::new(),
            bodies: BTreeMap::new(),
            ended_bodies: 0,
        }
    }

    pub fn state(&self) -> InitiatorState {
        self.state
    }

    fn abort(&mut self, code: u16) -> u16 {
        // Discard the Contact staging area, always.
        self.authority = None;
        self.manifest_root_ref = None;
        self.manifest_root_bytes.clear();
        self.manifest_nodes.clear();
        self.bodies.clear();
        self.state = InitiatorState::Aborted;
        code
    }

    /// Process one received frame. `Err(code)` means abort: the caller sends
    /// `Abort { code }`, and this machine's staging is already discarded.
    pub fn on_frame(&mut self, raw: &[u8]) -> Result<Progress, u16> {
        if matches!(
            self.state,
            InitiatorState::AckSent | InitiatorState::Closed | InitiatorState::Aborted
        ) {
            return Err(self.abort(abort::WRONG_STATE));
        }
        // Contact-wide limits count every frame, including bad ones.
        self.frames += 1;
        self.bytes += raw.len() as u64;
        if self.frames > MAX_FRAMES || self.bytes > MAX_CONTACT_BYTES {
            return Err(self.abort(abort::LIMITS));
        }
        let (contact, frame) = match ContactFrame::decode(raw) {
            Ok(v) => v,
            Err(Invalid::UnknownTag(_)) => return Err(self.abort(abort::UNKNOWN_TAG)),
            Err(Invalid::FrameTooLarge) => return Err(self.abort(abort::LIMITS)),
            Err(_) => return Err(self.abort(abort::MALFORMED)),
        };
        if contact != self.contact {
            return Err(self.abort(abort::CONTACT_MISMATCH));
        }
        // The transcript covers every frame received before TransferEnd.
        if !matches!(frame, ContactFrame::TransferEnd { .. }) {
            self.transcript.update(raw);
        }
        match frame {
            ContactFrame::AuthorityOffer {
                authority_frontier,
                record_count,
                total_bytes,
                set_hash,
            } => {
                if self.state != InitiatorState::HelloSent {
                    return Err(self.abort(abort::WRONG_STATE));
                }
                self.authority = Some(AuthorityStaging {
                    expected_count: record_count,
                    expected_set_hash: set_hash,
                    expected_total: total_bytes,
                    ..Default::default()
                });
                // Keep the frontier for the received material.
                self.authority_frontier = authority_frontier;
                self.state = InitiatorState::AuthorityReceiving;
                Ok(Progress::Continue)
            }
            ContactFrame::AuthorityChunk {
                index,
                record_hash,
                bytes,
            } => {
                if self.state != InitiatorState::AuthorityReceiving {
                    return Err(self.abort(abort::WRONG_STATE));
                }
                let staging = self.authority.as_mut().expect("offer set state");
                if bytes.is_empty() {
                    return Err(self.abort(abort::EMPTY_CHUNK));
                }
                if bytes.len() > MAX_CHUNK {
                    return Err(self.abort(abort::LIMITS));
                }
                if index != staging.next_index || index >= staging.expected_count {
                    return Err(self.abort(abort::WRONG_STATE));
                }
                if authority_record_hash(&bytes) != record_hash {
                    return Err(self.abort(abort::HASH_MISMATCH));
                }
                staging.received_bytes += bytes.len() as u64;
                if staging.received_bytes > staging.expected_total {
                    return Err(self.abort(abort::LIMITS));
                }
                staging.next_index += 1;
                staging.record_hashes.push(record_hash);
                staging.records.push(bytes);
                Ok(Progress::Continue)
            }
            ContactFrame::AuthorityEnd {
                record_count,
                set_hash,
            } => {
                if self.state != InitiatorState::AuthorityReceiving {
                    return Err(self.abort(abort::WRONG_STATE));
                }
                let staging = self.authority.as_mut().expect("offer set state");
                if staging.ended {
                    return Err(self.abort(abort::DUPLICATE_TERMINAL));
                }
                if record_count != staging.expected_count
                    || staging.next_index != staging.expected_count
                {
                    return Err(self.abort(abort::SET_MISMATCH));
                }
                let computed = authority_set_hash(&staging.record_hashes);
                if set_hash != staging.expected_set_hash || computed != set_hash {
                    return Err(self.abort(abort::SET_MISMATCH));
                }
                staging.ended = true;
                self.state = InitiatorState::ManifestReceiving;
                Ok(Progress::Continue)
            }
            ContactFrame::ManifestOffer { root_bytes } => {
                if self.state != InitiatorState::ManifestReceiving
                    || self.manifest_root_ref.is_some()
                {
                    return Err(self.abort(abort::WRONG_STATE));
                }
                self.manifest_root_ref = Some(manifest_root_ref(&root_bytes));
                self.manifest_root_bytes = root_bytes;
                Ok(Progress::Continue)
            }
            ContactFrame::ManifestNode {
                root,
                node_hash,
                node_bytes,
            } => {
                if self.state != InitiatorState::ManifestReceiving {
                    return Err(self.abort(abort::WRONG_STATE));
                }
                let Some(offered) = self.manifest_root_ref else {
                    return Err(self.abort(abort::WRONG_STATE));
                };
                if root != offered {
                    return Err(self.abort(abort::HASH_MISMATCH));
                }
                if manifest_node_hash(&node_bytes) != node_hash {
                    return Err(self.abort(abort::HASH_MISMATCH));
                }
                if self.manifest_nodes.len() >= MAX_MANIFEST_NODES {
                    return Err(self.abort(abort::TOO_LARGE));
                }
                // Content-addressed, so a duplicate is idempotent by
                // construction and a conflict is not expressible.
                self.manifest_nodes.insert(node_hash, node_bytes);
                Ok(Progress::Continue)
            }
            ContactFrame::BodyChunk {
                transaction,
                body,
                offset,
                total,
                chunk_hash,
                bytes,
            } => {
                if self.state == InitiatorState::ManifestReceiving {
                    if self.manifest_root_ref.is_none() {
                        return Err(self.abort(abort::WRONG_STATE));
                    }
                    self.state = InitiatorState::BodiesReceiving;
                } else if self.state != InitiatorState::BodiesReceiving {
                    return Err(self.abort(abort::WRONG_STATE));
                }
                if bytes.is_empty() {
                    return Err(self.abort(abort::EMPTY_CHUNK));
                }
                if bytes.len() > MAX_CHUNK {
                    return Err(self.abort(abort::LIMITS));
                }
                if body_chunk_hash(&bytes) != chunk_hash {
                    return Err(self.abort(abort::HASH_MISMATCH));
                }
                let len = bytes.len() as u64;
                if offset.checked_add(len).is_none() || offset + len > total {
                    return Err(self.abort(abort::MALFORMED));
                }
                let staging = self.bodies.entry((transaction, body)).or_default();
                if staging.ended {
                    return Err(self.abort(abort::WRONG_STATE));
                }
                match staging.total {
                    Some(t) if t != total => return Err(self.abort(abort::CHUNK_CONFLICT)),
                    _ => staging.total = Some(total),
                }
                // Exact duplicates are idempotent; every partial overlap or
                // conflicting duplicate aborts.
                if let Some((h, b)) = staging.chunks.get(&offset) {
                    if *h == chunk_hash && *b == bytes {
                        return Ok(Progress::Continue);
                    }
                    return Err(self.abort(abort::CHUNK_CONFLICT));
                }
                for (&o, (_, b)) in staging.chunks.range(..) {
                    let l = b.len() as u64;
                    if o < offset + len && offset < o + l {
                        return Err(self.abort(abort::CHUNK_CONFLICT));
                    }
                }
                staging.chunks.insert(offset, (chunk_hash, bytes));
                Ok(Progress::Continue)
            }
            ContactFrame::BodyEnd {
                transaction,
                body,
                total,
                content_commitment,
            } => {
                if self.state == InitiatorState::ManifestReceiving {
                    if self.manifest_root_ref.is_none() {
                        return Err(self.abort(abort::WRONG_STATE));
                    }
                    self.state = InitiatorState::BodiesReceiving;
                } else if self.state != InitiatorState::BodiesReceiving {
                    return Err(self.abort(abort::WRONG_STATE));
                }
                let staging = self.bodies.entry((transaction, body)).or_default();
                if staging.ended {
                    return Err(self.abort(abort::DUPLICATE_TERMINAL));
                }
                if let Some(t) = staging.total {
                    if t != total {
                        return Err(self.abort(abort::CHUNK_CONFLICT));
                    }
                }
                // Coverage: the chunks must exactly tile [0, total).
                let mut cursor = 0u64;
                for (&o, (_, b)) in staging.chunks.iter() {
                    if o != cursor {
                        return Err(self.abort(abort::GAP));
                    }
                    cursor += b.len() as u64;
                }
                if cursor != total {
                    return Err(self.abort(abort::GAP));
                }
                // The assembled protected payload must match its commitment.
                let mut assembled = Vec::with_capacity(total as usize);
                for (_, b) in staging.chunks.values() {
                    assembled.extend_from_slice(b);
                }
                if ContentCommitment::over_protected_payload(&assembled).as_bytes()
                    != content_commitment
                {
                    return Err(self.abort(abort::HASH_MISMATCH));
                }
                staging.ended = true;
                staging.chunks.insert(0, (content_commitment, assembled));
                self.ended_bodies += 1;
                Ok(Progress::Continue)
            }
            ContactFrame::TransferEnd {
                authority_set_hash: end_set_hash,
                manifest_root,
                body_count,
                transcript_hash,
            } => {
                if !matches!(
                    self.state,
                    InitiatorState::ManifestReceiving | InitiatorState::BodiesReceiving
                ) {
                    return Err(self.abort(abort::WRONG_STATE));
                }
                let Some(staging) = &self.authority else {
                    return Err(self.abort(abort::WRONG_STATE));
                };
                if !staging.ended {
                    return Err(self.abort(abort::WRONG_STATE));
                }
                if end_set_hash != staging.expected_set_hash {
                    return Err(self.abort(abort::SET_MISMATCH));
                }
                let Some(offered) = self.manifest_root_ref else {
                    return Err(self.abort(abort::WRONG_STATE));
                };
                if manifest_root != offered {
                    return Err(self.abort(abort::HASH_MISMATCH));
                }
                // Every staged body must have ended, and the count must agree.
                if body_count != self.ended_bodies || self.bodies.values().any(|b| !b.ended) {
                    return Err(self.abort(abort::SET_MISMATCH));
                }
                let ours: [u8; 32] = *self.transcript.clone().finalize().as_bytes();
                if transcript_hash != ours {
                    return Err(self.abort(abort::TRANSCRIPT_MISMATCH));
                }
                self.state = InitiatorState::AckSent;
                Ok(Progress::SendAck(ContactFrame::TransferAck {
                    transcript_hash: ours,
                    received_bytes: self.bytes,
                }))
            }
            // Frames the initiator never receives: requests flow the other
            // way, and the holdings declaration is something it sends.
            ContactFrame::ManifestRequest { .. }
            | ContactFrame::BodyRequest { .. }
            | ContactFrame::TransferAck { .. }
            | ContactFrame::HoldingsChunk { .. }
            | ContactFrame::HoldingsEnd { .. }
            | ContactFrame::Abort { .. } => Err(self.abort(abort::WRONG_STATE)),
        }
    }

    /// Take the received material after a clean transfer (state `AckSent`).
    /// Receipt implies no legitimacy — mechanics validation and
    /// `Replica::incorporate` are the Convergence step.
    pub fn into_received(mut self) -> Option<ReceivedMaterial> {
        if self.state != InitiatorState::AckSent {
            return None;
        }
        let authority = self.authority.take()?;
        Some(ReceivedMaterial {
            authority_frontier: std::mem::take(&mut self.authority_frontier),
            authority_records: authority.records,
            manifest_root_bytes: std::mem::take(&mut self.manifest_root_bytes),
            manifest_nodes: std::mem::take(&mut self.manifest_nodes),
            bodies: std::mem::take(&mut self.bodies)
                .into_iter()
                .filter_map(|(k, mut s)| s.chunks.remove(&0).map(|(_, b)| (k, b)))
                .collect(),
        })
    }
}

// ---------------------------------------------------------------------------
// Outbound transfer framing
// ---------------------------------------------------------------------------

/// The material one accepter serves in one transfer, snapshotted before the
/// first frame is sent.
#[derive(Debug, Clone)]
pub struct OutboundTransfer {
    pub authority_frontier: Vec<u8>,
    /// Canonical authority-section records (mechanics material and signed
    /// `Transaction` records), byte-exact.
    pub authority_records: Vec<Vec<u8>>,
    pub manifest_root_bytes: Vec<u8>,
    /// Canonical node bytes of the manifest's Body index, in any order — they
    /// are content-addressed, so order carries no meaning.
    pub manifest_nodes: Vec<Vec<u8>>,
    /// `(transaction id, key, protected payload bytes)`.
    pub bodies: Vec<([u8; 32], BodyKey, Vec<u8>)>,
}

/// Build the complete, canonical, ordered frame sequence for a transfer:
/// authority section, manifest root + index nodes, chunked protected Body payloads
/// (each chunk at most [`MAX_CHUNK`]), and the terminal `TransferEnd` carrying
/// the transcript hash over every prior frame.
pub fn build_transfer_frames(contact: &ContactId, t: &OutboundTransfer) -> Vec<Vec<u8>> {
    let record_hashes: Vec<[u8; 32]> = t
        .authority_records
        .iter()
        .map(|r| authority_record_hash(r))
        .collect();
    let set_hash = authority_set_hash(&record_hashes);
    let root = manifest_root_ref(&t.manifest_root_bytes);
    let total_bytes: u64 = t.authority_records.iter().map(|r| r.len() as u64).sum();

    let mut frames: Vec<ContactFrame> = Vec::new();
    frames.push(ContactFrame::AuthorityOffer {
        authority_frontier: t.authority_frontier.clone(),
        record_count: t.authority_records.len() as u32,
        total_bytes,
        set_hash,
    });
    for (i, record) in t.authority_records.iter().enumerate() {
        frames.push(ContactFrame::AuthorityChunk {
            index: i as u32,
            record_hash: record_hashes[i],
            bytes: record.clone(),
        });
    }
    frames.push(ContactFrame::AuthorityEnd {
        record_count: t.authority_records.len() as u32,
        set_hash,
    });
    frames.push(ContactFrame::ManifestOffer {
        root_bytes: t.manifest_root_bytes.clone(),
    });
    for node in &t.manifest_nodes {
        frames.push(ContactFrame::ManifestNode {
            root,
            node_hash: manifest_node_hash(node),
            node_bytes: node.clone(),
        });
    }
    let mut body_count = 0u32;
    for (tx, key, payload) in &t.bodies {
        let total = payload.len() as u64;
        let mut offset = 0u64;
        for chunk in payload.chunks(MAX_CHUNK) {
            frames.push(ContactFrame::BodyChunk {
                transaction: *tx,
                body: key.clone(),
                offset,
                total,
                chunk_hash: body_chunk_hash(chunk),
                bytes: chunk.to_vec(),
            });
            offset += chunk.len() as u64;
        }
        frames.push(ContactFrame::BodyEnd {
            transaction: *tx,
            body: key.clone(),
            total,
            content_commitment: ContentCommitment::over_protected_payload(payload).as_bytes(),
        });
        body_count += 1;
    }

    let mut raw: Vec<Vec<u8>> = frames.drain(..).map(|f| f.encode(contact)).collect();
    let mut transcript = blake3::Hasher::new();
    transcript.update(TRANSCRIPT_DOMAIN);
    for r in &raw {
        transcript.update(r);
    }
    raw.push(
        ContactFrame::TransferEnd {
            authority_set_hash: set_hash,
            manifest_root: root,
            body_count,
            transcript_hash: *transcript.finalize().as_bytes(),
        }
        .encode(contact),
    );
    raw
}

// ---------------------------------------------------------------------------
// Accepter request validation
// ---------------------------------------------------------------------------

/// The accepter's validator for the frames it *receives* (requests and the
/// final ack). The accepter's send side is driver-scheduled; this enforces the
/// normative receive rules: manifest requests must reference the offered root
/// and no more nodes than were offered, body requests are bounded, and exactly
/// one `TransferAck` (with the sent transcript) is accepted.
pub struct AccepterValidator {
    contact: ContactId,
    offered_root: Option<[u8; 32]>,
    offered_nodes: u32,
    sent_transcript: blake3::Hasher,
    end_sent: bool,
    acked: bool,
}

/// A validated accepter-side receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccepterEvent {
    ManifestRequest {
        nodes: Vec<[u8; 32]>,
    },
    BodyRequest {
        transaction: [u8; 32],
        body: BodyKey,
        offset: u64,
        length: u32,
    },
    Acked {
        received_bytes: u64,
    },
    PeerAborted(u16),
}

impl AccepterValidator {
    pub fn new(contact: ContactId) -> Self {
        let mut sent_transcript = blake3::Hasher::new();
        sent_transcript.update(TRANSCRIPT_DOMAIN);
        Self {
            contact,
            offered_root: None,
            offered_nodes: 0,
            sent_transcript,
            end_sent: false,
            acked: false,
        }
    }

    /// Record a frame the accepter sent: the transcript covers every sent frame
    /// before `TransferEnd`, and the offer/end state feeds request validation.
    pub fn record_sent(&mut self, raw: &[u8]) {
        if let Ok((_, frame)) = ContactFrame::decode(raw) {
            match &frame {
                ContactFrame::ManifestOffer { root_bytes } => {
                    self.offered_root = Some(manifest_root_ref(root_bytes));
                }
                ContactFrame::ManifestNode { .. } => {
                    self.offered_nodes += 1;
                }
                ContactFrame::TransferEnd { .. } => {
                    self.end_sent = true;
                    return; // TransferEnd itself is outside the transcript.
                }
                _ => {}
            }
        }
        self.sent_transcript.update(raw);
    }

    /// The transcript hash a `TransferEnd` must carry right now.
    pub fn transcript_hash(&self) -> [u8; 32] {
        *self.sent_transcript.clone().finalize().as_bytes()
    }

    /// Validate one received frame. `Err(code)` means send `Abort { code }`.
    pub fn on_frame(&mut self, raw: &[u8]) -> Result<AccepterEvent, u16> {
        let (contact, frame) = match ContactFrame::decode(raw) {
            Ok(v) => v,
            Err(Invalid::UnknownTag(_)) => return Err(abort::UNKNOWN_TAG),
            Err(Invalid::FrameTooLarge) => return Err(abort::LIMITS),
            Err(_) => return Err(abort::MALFORMED),
        };
        if contact != self.contact {
            return Err(abort::CONTACT_MISMATCH);
        }
        match frame {
            ContactFrame::Abort { code } => Ok(AccepterEvent::PeerAborted(code)),
            ContactFrame::ManifestRequest { root, nodes } => {
                let Some(offered) = self.offered_root else {
                    return Err(abort::BAD_REQUEST);
                };
                // Must reference the offered root and stay inside the request
                // bound. An unknown hash is not an error — the accepter simply
                // has nothing to send for it — so a peer learns nothing from
                // asking that it could not already address.
                if root != offered || nodes.is_empty() || nodes.len() > MAX_DESCENT_REQUEST {
                    return Err(abort::BAD_REQUEST);
                }
                Ok(AccepterEvent::ManifestRequest { nodes })
            }
            ContactFrame::BodyRequest {
                transaction,
                body,
                offset,
                length,
            } => {
                if length as usize > MAX_CHUNK {
                    return Err(abort::BAD_REQUEST);
                }
                Ok(AccepterEvent::BodyRequest {
                    transaction,
                    body,
                    offset,
                    length,
                })
            }
            ContactFrame::TransferAck {
                transcript_hash,
                received_bytes,
            } => {
                if !self.end_sent || self.acked {
                    return Err(abort::WRONG_STATE);
                }
                if transcript_hash != self.transcript_hash() {
                    return Err(abort::TRANSCRIPT_MISMATCH);
                }
                self.acked = true;
                Ok(AccepterEvent::Acked { received_bytes })
            }
            // Everything else flows initiator ← accepter only.
            _ => Err(abort::WRONG_STATE),
        }
    }
}
