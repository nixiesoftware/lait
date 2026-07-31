//! The signed commitment to a Replica's Body and content catalogs.
//!
//! A manifest root binds a Space, a Replica frontier, and two authenticated
//! index roots under an admitted Station's signature. The Body index maps a
//! canonical Body key to that Body's advertised head set; the content index
//! maps a content id to its descriptor. Both are the canonical radix index the
//! journal defines, so the same logical catalog has exactly one root on every
//! replica that holds it.
//!
//! **Why not pages.** The shape this replaces chunked BodyKey-sorted entries
//! into ordinal pages and signed the ordered page hashes. Editing an existing
//! Body was cheap — its entry stayed in its page — but *adding* one was not:
//! a new entry in an early page pushes the last entry into the next page, and
//! every page after it changes. At the 100,000-Body ceiling that is a complete
//! rewrite of the catalog to add one issue. An index has no ordinals, so an
//! insertion rewrites one leaf and its ancestors and nothing else.
//!
//! **Concurrency and equivocation.** Incomparable concurrent roots coexist —
//! Convergence unions their valid transactions before emitting a new local
//! root. *Equivocation* is two **different** roots by the same signer at the
//! same semantic transaction coordinate (Replica frontier); [`ManifestBook`]
//! rejects and reports it.

use fabric::journal::index::{self, ChildRef, IndexEntry, IndexKey, NodeSink, NodeSource};
use mechanics::ids::SpaceId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::frontier::{AuthorityFrontier, ReplicaFrontier};
use crate::ids::BodyKey;

/// Root signature domain.
pub const MANIFEST_DOMAIN: &[u8] = b"lait/manifest/2";
/// Domain separating a Body's index key from every other digest.
pub const BODY_INDEX_KEY_DOMAIN: &[u8] = b"lait/manifest/2/body-key";
/// Domain separating a content descriptor's index key.
pub const CONTENT_INDEX_KEY_DOMAIN: &[u8] = b"lait/manifest/2/content-key";
/// Maximum content references one Body may declare. Bounded because the whole
/// point of the declaration is that it can be validated without decoding the
/// product bytes it describes.
pub const MAX_CONTENT_REFS_PER_BODY: usize = 1024;
/// Ed25519 algorithm tag.
pub const SIG_ALG_ED25519: u8 = 1;
/// The encoded generation of the manifest format.
pub const MANIFEST_FORMAT_VERSION: u8 = 2;
/// Maximum advertised heads for one Body. A Body is advertised as the exact set
/// of author-signed heads whose union is its state; concurrent writers grow it,
/// convergence shrinks it, and this is where an unbounded one is refused.
pub const MAX_HEADS_PER_BODY: usize = 1024;
/// The fixed rendered-SpaceId length.
pub const SPACE_ID_LEN: usize = 29;

/// One advertised head: the hash of its public descriptor and the commitment to
/// its signed BodyTransaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ManifestHead {
    pub descriptor_hash: [u8; 32],
    pub transaction_commitment: [u8; 32],
}

/// One Body's manifest entry: its key, its advertised head set, and the content
/// it declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub key: BodyKey,
    /// Sorted and unique. A Body's heads are a set, so a canonical order is
    /// what lets two replicas holding the same Body publish the same bytes.
    pub heads: Vec<ManifestHead>,
    /// The content ids this Body references, sorted and unique.
    ///
    /// A `ContentRef` committed inside a Body is product-encoded, and the World
    /// boundary forbids the substrate from decoding product bytes to find it.
    /// Without this the content catalog could only ever grow: tombstone every
    /// Body that referenced an upload and its descriptor — size, geometry,
    /// epoch, Merkle root — remains signed state on every peer forever. A
    /// substrate that cannot forget an accidental upload is not acceptable, and
    /// a reachability rule nobody can compute is not a rule.
    ///
    /// So the World *declares* it and Replica validates the declaration:
    /// bounds, sortedness, uniqueness, and that every named descriptor is
    /// committed. That is a statement about opaque bytes, not a decoding of
    /// them, so the boundary holds.
    ///
    /// No reference count is stored anywhere. Counts do not converge across
    /// independently committing replicas; a pure function of the converged Body
    /// set does.
    #[serde(default)]
    pub content_refs: Vec<[u8; 32]>,
}

/// The signed manifest root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestRoot {
    pub format_version: u8,
    pub space: [u8; SPACE_ID_LEN],
    pub replica_frontier: ReplicaFrontier,
    pub body_index_root: Option<ChildRef>,
    pub body_count: u64,
    pub content_index_root: Option<ChildRef>,
    pub content_count: u64,
    pub signer: [u8; 32],
    pub authority_frontier: AuthorityFrontier,
    pub signature_algorithm: u8,
    #[serde(with = "serde_byte_array")]
    pub signature: [u8; 64],
}

/// Why a manifest failed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    UnsupportedVersion(u8),
    UnsupportedSignatureAlgorithm(u8),
    NonCanonical,
    BadSpaceId,
    /// Counts or head sets exceed the frozen bounds.
    Bounds,
    /// A declared count disagrees with the index it names.
    CountMismatch,
    /// The index is missing a node, malformed, non-canonical, or misordered.
    IndexInvalid,
    /// An entry's key does not hash to the index key it sits under.
    KeyMismatch,
    /// Heads unsorted or duplicated within one Body.
    OrderViolation,
    BadSignature,
    /// Two different roots by the same signer at the same frontier coordinate.
    Equivocation,
    /// Structurally valid and correctly signed, but the signer had no standing
    /// at the root's authority frontier.
    AuthorityUnverified,
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ManifestError {}

fn length_framed(domain: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + domain.len() + 4 + body.len());
    out.extend_from_slice(&(domain.len() as u16).to_be_bytes());
    out.extend_from_slice(domain);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// The index key a Body sits under: a domain-separated hash of its canonical
/// logical key. Hashing is what keeps the tree's shape independent of how
/// Worlds and Bodies happen to be named.
pub fn content_index_key(content_id: &[u8; 32]) -> IndexKey {
    let mut h = blake3::Hasher::new();
    h.update(CONTENT_INDEX_KEY_DOMAIN);
    h.update(content_id);
    *h.finalize().as_bytes()
}

pub fn body_index_key(key: &BodyKey) -> IndexKey {
    let mut h = blake3::Hasher::new();
    h.update(BODY_INDEX_KEY_DOMAIN);
    h.update(key.world.as_bytes());
    h.update(&[0x00]);
    h.update(&key.body.as_bytes());
    *h.finalize().as_bytes()
}

impl ManifestEntry {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard manifest entry")
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, ManifestError> {
        let entry: Self = postcard::from_bytes(bytes).map_err(|_| ManifestError::NonCanonical)?;
        if entry.encode() != bytes {
            return Err(ManifestError::NonCanonical);
        }
        entry.validate()?;
        Ok(entry)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.heads.is_empty() || self.heads.len() > MAX_HEADS_PER_BODY {
            return Err(ManifestError::Bounds);
        }
        if self.content_refs.len() > MAX_CONTENT_REFS_PER_BODY {
            return Err(ManifestError::Bounds);
        }
        for w in self.heads.windows(2) {
            if w[0] >= w[1] {
                return Err(ManifestError::OrderViolation);
            }
        }
        for w in self.content_refs.windows(2) {
            if w[0] >= w[1] {
                return Err(ManifestError::OrderViolation);
            }
        }
        Ok(())
    }

    /// Build a canonical entry from an unordered head set and declaration.
    pub fn new(key: BodyKey, heads: Vec<ManifestHead>) -> Result<Self, ManifestError> {
        Self::declaring(key, heads, Vec::new())
    }

    /// Build a canonical entry that also declares content references.
    pub fn declaring(
        key: BodyKey,
        mut heads: Vec<ManifestHead>,
        mut content_refs: Vec<[u8; 32]>,
    ) -> Result<Self, ManifestError> {
        heads.sort();
        heads.dedup();
        content_refs.sort();
        content_refs.dedup();
        let entry = Self {
            key,
            heads,
            content_refs,
        };
        entry.validate()?;
        Ok(entry)
    }
}

impl ManifestRoot {
    fn preimage(&self) -> Vec<u8> {
        let body = postcard::to_stdvec(&(
            self.format_version,
            self.space,
            self.replica_frontier,
            self.body_index_root,
            self.body_count,
            self.content_index_root,
            self.content_count,
            self.signer,
            &self.authority_frontier,
        ))
        .expect("postcard manifest root preimage");
        length_framed(MANIFEST_DOMAIN, &body)
    }

    /// Build and sign a root over already-built index roots. Any admitted
    /// Station may sign; mechanics validates its standing at the authority
    /// frontier separately, like every signed object.
    #[allow(clippy::too_many_arguments)]
    pub fn sign_with(
        space: &SpaceId,
        replica_frontier: ReplicaFrontier,
        body_index_root: Option<ChildRef>,
        content_index_root: Option<ChildRef>,
        authority_frontier: AuthorityFrontier,
        signer: &dyn crate::transaction::TransactionSigner,
    ) -> Option<Self> {
        let mut root = Self {
            format_version: MANIFEST_FORMAT_VERSION,
            space: <[u8; SPACE_ID_LEN]>::try_from(space.as_str().as_bytes()).ok()?,
            replica_frontier,
            body_index_root,
            body_count: body_index_root.map_or(0, |c| c.count),
            content_index_root,
            content_count: content_index_root.map_or(0, |c| c.count),
            signer: signer.signer_key(),
            authority_frontier,
            signature_algorithm: SIG_ALG_ED25519,
            signature: [0u8; 64],
        };
        root.signature = signer.sign_preimage(&root.preimage());
        Some(root)
    }

    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard manifest root")
    }

    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, ManifestError> {
        let root: Self = postcard::from_bytes(bytes).map_err(|_| ManifestError::NonCanonical)?;
        if root.encode() != bytes {
            return Err(ManifestError::NonCanonical);
        }
        Ok(root)
    }

    /// Verify the root itself: version, algorithm, Space shape, declared counts
    /// against the roots they name, and the Station signature. Signer standing
    /// at the authority frontier is mechanics' separate check, and the index's
    /// contents are [`Self::verify_index`].
    pub fn verify(&self) -> Result<(), ManifestError> {
        if self.format_version != MANIFEST_FORMAT_VERSION {
            return Err(ManifestError::UnsupportedVersion(self.format_version));
        }
        if self.signature_algorithm != SIG_ALG_ED25519 {
            return Err(ManifestError::UnsupportedSignatureAlgorithm(
                self.signature_algorithm,
            ));
        }
        std::str::from_utf8(&self.space)
            .ok()
            .and_then(SpaceId::parse)
            .ok_or(ManifestError::BadSpaceId)?;
        if self.body_index_root.map_or(0, |c| c.count) != self.body_count
            || self.content_index_root.map_or(0, |c| c.count) != self.content_count
        {
            return Err(ManifestError::CountMismatch);
        }
        if !mechanics::crypto::verify_detached(&self.signer, &self.preimage(), &self.signature) {
            return Err(ManifestError::BadSignature);
        }
        Ok(())
    }

    /// Verify **both** advertised indexes against this (already-verified)
    /// root: canonical index structure, then every entry's own validity and its
    /// placement under the key it hashes to. Returns the Body count.
    ///
    /// The placement check is what stops a substituted entry. Index validation
    /// proves an entry sits under some key; only re-deriving the key from the
    /// entry's own identity proves it sits under *its* key. For a content
    /// descriptor that identity is its own hash, so the check is stronger than
    /// it looks: an entry cannot sit under a key it does not hash to, and a
    /// descriptor's hash is over the whole descriptor. Substituting a geometry,
    /// a nonce, or a Merkle root moves the entry.
    pub fn verify_index(&self, nodes: &dyn NodeSource) -> Result<u64, ManifestError> {
        let counted = index::validate(nodes, self.body_index_root)
            .map_err(|_| ManifestError::IndexInvalid)?;
        if counted != self.body_count {
            return Err(ManifestError::CountMismatch);
        }
        let mut failure: Option<ManifestError> = None;
        index::stream(nodes, self.body_index_root, &mut |entry| {
            if failure.is_some() {
                return;
            }
            match ManifestEntry::decode_canonical(&entry.value) {
                Ok(decoded) if body_index_key(&decoded.key) == entry.key => {}
                Ok(_) => failure = Some(ManifestError::KeyMismatch),
                Err(e) => failure = Some(e),
            }
        })
        .map_err(|_| ManifestError::IndexInvalid)?;
        if let Some(e) = failure {
            return Err(e);
        }
        self.verify_content_index(nodes)?;
        Ok(counted)
    }

    /// The content half of [`Self::verify_index`].
    ///
    /// Split out because it is the part a peer can advertise as empty and be
    /// telling the truth — a Space with no content has no descriptors, and that
    /// is not a degraded advertisement. The Space check is here rather than in
    /// [`crate::content::ContentDescriptor::validate`] because only the root
    /// knows which Space this catalog claims to be.
    fn verify_content_index(&self, nodes: &dyn NodeSource) -> Result<u64, ManifestError> {
        let counted = index::validate(nodes, self.content_index_root)
            .map_err(|_| ManifestError::IndexInvalid)?;
        if counted != self.content_count {
            return Err(ManifestError::CountMismatch);
        }
        let space = std::str::from_utf8(&self.space).map_err(|_| ManifestError::BadSpaceId)?;
        let mut failure: Option<ManifestError> = None;
        index::stream(nodes, self.content_index_root, &mut |entry| {
            if failure.is_some() {
                return;
            }
            match crate::content::ContentDescriptor::decode_canonical(&entry.value) {
                Ok(descriptor) => {
                    if descriptor.space != space {
                        failure = Some(ManifestError::BadSpaceId);
                    } else if content_index_key(descriptor.content_ref().as_bytes()) != entry.key {
                        failure = Some(ManifestError::KeyMismatch);
                    }
                }
                Err(_) => failure = Some(ManifestError::NonCanonical),
            }
        })
        .map_err(|_| ManifestError::IndexInvalid)?;
        match failure {
            Some(e) => Err(e),
            None => Ok(counted),
        }
    }

    /// The equivocation coordinate: one signer may publish at most one root per
    /// semantic transaction coordinate.
    pub fn coordinate(&self) -> ([u8; 32], ReplicaFrontier) {
        (self.signer, self.replica_frontier)
    }

    /// A stable identity for this exact signed root.
    pub fn root_hash(&self) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(MANIFEST_DOMAIN);
        h.update(&self.encode());
        *h.finalize().as_bytes()
    }

    /// Full verification for retention: the structural [`Self::verify`] **and**
    /// the mechanics authority check — the signer must have had standing at the
    /// root's authority frontier. This is the **only** way to mint an
    /// [`AuthorizedRoot`], and [`ManifestBook`] accepts nothing else, so an
    /// unverified or unauthorized root can never poison a signer's coordinate.
    pub fn verify_authorized(
        self,
        authority: &dyn crate::transaction::AuthoritySource,
    ) -> Result<AuthorizedRoot, ManifestError> {
        self.verify()?;
        if !authority.signer_authorized(&self.signer, &self.authority_frontier) {
            return Err(ManifestError::AuthorityUnverified);
        }
        Ok(AuthorizedRoot { root: self })
    }
}

/// Build a Body index from a complete catalog. Used when publishing from a
/// catalog held whole in memory; incremental publication updates the prior root
/// through the index's own `apply` instead.
pub fn build_body_index(
    entries: Vec<ManifestEntry>,
    sink: &mut NodeSink,
) -> Result<Option<ChildRef>, ManifestError> {
    let indexed: Vec<IndexEntry> = entries
        .into_iter()
        .map(|entry| {
            entry.validate()?;
            Ok(IndexEntry {
                key: body_index_key(&entry.key),
                value: entry.encode(),
            })
        })
        .collect::<Result<_, ManifestError>>()?;
    index::build_index(indexed, sink).map_err(|_| ManifestError::IndexInvalid)
}

/// Build the content catalog a Contact advertises: every committed descriptor,
/// under its own content id.
///
/// The mirror of [`build_body_index`], and deliberately the same shape — a
/// descriptor is catalog material exactly as a Body's manifest entry is. What
/// makes it safe to serve is that a descriptor says nothing a peer could not
/// already derive from bytes it is entitled to: geometry, an epoch id, a
/// per-ingest nonce, and a Merkle root over ciphertext. None of it opens
/// anything.
pub fn build_content_index(
    descriptors: Vec<crate::content::ContentDescriptor>,
    sink: &mut NodeSink,
) -> Result<Option<ChildRef>, ManifestError> {
    let indexed: Vec<IndexEntry> = descriptors
        .into_iter()
        .map(|descriptor| {
            descriptor
                .validate()
                .map_err(|_| ManifestError::NonCanonical)?;
            Ok(IndexEntry {
                key: content_index_key(descriptor.content_ref().as_bytes()),
                value: descriptor.encode(),
            })
        })
        .collect::<Result<_, ManifestError>>()?;
    index::build_index(indexed, sink).map_err(|_| ManifestError::IndexInvalid)
}

/// A manifest root whose structure, signature, **and signer authority** have
/// been verified. Constructible only through
/// [`ManifestRoot::verify_authorized`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedRoot {
    root: ManifestRoot,
}

impl AuthorizedRoot {
    pub fn root(&self) -> &ManifestRoot {
        &self.root
    }
}

/// How [`ManifestBook::observe`] classified a verified root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootObservation {
    /// A new root; it coexists with other known roots until Convergence unions.
    Accepted,
    /// An exact replay of an already-known root.
    AlreadyKnown,
    /// Accepted, and an older coordinate was evicted to stay within the bound.
    /// Reported rather than silent, because eviction is the one thing that can
    /// make a later equivocation undetectable.
    AcceptedWithEviction { evicted: usize },
}

/// The per-Space record of observed manifest roots, keyed by signer + frontier
/// coordinate. Detects equivocation (two different roots by the same signer at
/// the same coordinate) and dedupes replays.
///
/// **Bounded, and the bound has a cost worth naming.** This map used to retain
/// every observed root forever by design, which is unbounded growth driven by
/// remote input. It is now capped per signer. But a coordinate this book has
/// forgotten is a coordinate at which a signer can equivocate undetected, so
/// eviction is reported rather than silent, retention is per-signer (one noisy
/// peer cannot evict another's history), and the newest coordinates — where a
/// live equivocation would actually be exploitable — are the ones kept.
#[derive(Debug)]
pub struct ManifestBook {
    /// Keyed by `(signer, frontier root, frontier count)` — raw bytes, so no
    /// ordering semantics are implied for frontiers (they are equality tokens).
    seen: BTreeMap<([u8; 32], [u8; 32], u64), [u8; 32]>,
    per_signer_limit: usize,
}

/// Default retained coordinates per signer.
pub const DEFAULT_ROOTS_PER_SIGNER: usize = 4096;

impl Default for ManifestBook {
    fn default() -> Self {
        Self::new()
    }
}

impl ManifestBook {
    pub fn new() -> Self {
        Self::with_limit(DEFAULT_ROOTS_PER_SIGNER)
    }

    /// Build with an explicit per-signer retention limit. An operator may lower
    /// it; zero is treated as one, because a book that retains nothing cannot
    /// detect an equivocation at all.
    pub fn with_limit(per_signer_limit: usize) -> Self {
        Self {
            seen: BTreeMap::new(),
            per_signer_limit: per_signer_limit.max(1),
        }
    }

    /// Observe an authority-verified root — the type makes verification
    /// non-optional. Equivocation is rejected and reported; the caller audits
    /// it (the book keeps the first-seen root).
    pub fn observe(&mut self, root: &AuthorizedRoot) -> Result<RootObservation, ManifestError> {
        let root = root.root();
        let (signer, frontier) = root.coordinate();
        let coordinate = (signer, frontier.root, frontier.transaction_count);
        let hash = root.root_hash();
        match self.seen.get(&coordinate) {
            Some(known) if *known == hash => Ok(RootObservation::AlreadyKnown),
            Some(_) => Err(ManifestError::Equivocation),
            None => {
                self.seen.insert(coordinate, hash);
                let evicted = self.trim(&signer);
                if evicted > 0 {
                    Ok(RootObservation::AcceptedWithEviction { evicted })
                } else {
                    Ok(RootObservation::Accepted)
                }
            }
        }
    }

    /// Drop this signer's oldest coordinates past the limit. Ordering is by
    /// frontier transaction count, so "oldest" means least advanced rather than
    /// least recently seen — a peer cannot protect a coordinate it wants
    /// forgotten by re-announcing it.
    fn trim(&mut self, signer: &[u8; 32]) -> usize {
        let mut coordinates: Vec<([u8; 32], [u8; 32], u64)> = self
            .seen
            .range((*signer, [0u8; 32], 0)..=(*signer, [0xFFu8; 32], u64::MAX))
            .map(|(k, _)| *k)
            .collect();
        if coordinates.len() <= self.per_signer_limit {
            return 0;
        }
        coordinates.sort_by_key(|(_, _, count)| *count);
        let excess = coordinates.len() - self.per_signer_limit;
        for coordinate in coordinates.into_iter().take(excess) {
            self.seen.remove(&coordinate);
        }
        excess
    }

    /// The number of distinct roots retained.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}
