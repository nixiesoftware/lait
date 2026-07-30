//! Immutable content — the durable-reference rung of plan 13 §3.2's ladder.
//!
//! A Body is byte-complete on every full Replica. Content is not: what every
//! Replica carries is the *descriptor* — canonical identity, size, epoch, chunk
//! geometry, and a Merkle root over the sealed chunks. The chunks themselves are
//! residency, and residency is local policy. That split is the whole point:
//! a World can name a gigabyte without every peer downloading a gigabyte.
//!
//! What is committed here is deliberately not what a file *is*. Filename, MIME
//! type, caption, and disposition are product metadata and live in a World Body;
//! two names may reference one [`ContentRef`]. Identity is the bytes.
//!
//! The Merkle tree commits **ciphertexts**, not plaintext. A provider can then
//! prove a chunk belongs to a content without holding the key, and a receiver
//! can verify what it was handed before spending anything on decryption. It
//! also means there is no plaintext-hash identity and so no equality oracle: two
//! ingests of identical bytes produce different `ContentId`s.

use mechanics::crypto::{AuthorizedBodyKey, ContentChunkBinding, BODY_ENVELOPE_OVERHEAD};
use mechanics::ids::SpaceId;
use serde::{Deserialize, Serialize};

/// The encoded generation of the content format. The number is the value; the
/// identifier names what is versioned.
pub const CONTENT_FORMAT_VERSION: u8 = 1;

/// Domain separating a `ContentId` from every other digest lait computes.
pub const CONTENT_ID_DOMAIN: &[u8] = b"lait/content-id/1";
/// Domain for a Merkle leaf over one sealed chunk.
pub const CONTENT_LEAF_DOMAIN: &[u8] = b"lait/content-leaf/1";
/// Domain for an interior Merkle node.
pub const CONTENT_NODE_DOMAIN: &[u8] = b"lait/content-node/1";
/// Domain for the local cache slot a chunk is filed under. Local naming only —
/// nothing derived here crosses the wire or is signed.
pub const CONTENT_SLOT_DOMAIN: &[u8] = b"lait/content-slot/1";

/// The frozen chunk plaintext size. 256 KiB rather than 1 MiB because it fits
/// comfortably inside Contact's 1 MiB frame and bounds what a failed transfer
/// wastes; F0's geometry measurement is the record of that choice.
pub const CHUNK_PLAINTEXT_LEN: u32 = 256 * 1024;

/// Protocol maximum content length (1 TiB). Operator policy may lower it.
pub const MAX_CONTENT_LEN: u64 = 1024 * 1024 * 1024 * 1024;

/// Maximum Merkle proof depth at the frozen geometry. 1 TiB at 256 KiB chunks
/// is 2^22 chunks, so a path is at most 22 siblings.
pub const MAX_PROOF_DEPTH: u8 = 22;

/// The maximum number of chunks any content may declare.
pub const MAX_CHUNK_COUNT: u32 = (MAX_CONTENT_LEN / CHUNK_PLAINTEXT_LEN as u64) as u32;

/// The canonical description of one immutable content. This — not the bytes —
/// is what every full Replica carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentDescriptor {
    pub format_version: u8,
    pub space: String,
    /// Random per-ingest. Bound into every chunk's associated data, and the
    /// reason two ingests of identical bytes are not equal.
    pub content_nonce: [u8; 16],
    pub plaintext_len: u64,
    pub chunk_plaintext_len: u32,
    pub chunk_count: u32,
    pub ciphertext_merkle_root: [u8; 32],
    pub epoch: [u8; 16],
}

/// A durable reference to immutable content: the descriptor's identity, and
/// nothing else. It proves nothing about local availability — residency is a
/// separate question, asked locally and answered by providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContentRef {
    pub content_id: [u8; 32],
}

impl ContentRef {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.content_id
    }
}

/// Why a descriptor, chunk, or proof was refused. Every failure that could
/// distinguish "wrong key" from "wrong bytes" collapses into one variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentError {
    UnsupportedVersion(u8),
    /// A declared geometry that cannot describe any content: a length that
    /// disagrees with the chunk count, a count past the protocol maximum, a
    /// chunk size that is not the frozen one.
    Geometry,
    /// Non-canonical encoding, or a length past a protocol bound.
    NonCanonical,
    BadSpaceId,
    /// A leaf, sibling path, or root that does not verify.
    ProofMismatch,
    /// Ciphertext that fails its length or hash before anything is decrypted.
    ChunkMismatch,
    /// The chunk verified but did not open: wrong epoch, wrong key, or a
    /// binding that disagrees. Deliberately one answer.
    Unopenable,
    /// The chunk is simply not here. Expected — content is descriptor-complete,
    /// not byte-complete — and the caller's response is to fetch it.
    NotResident,
}

impl std::fmt::Display for ContentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ContentError {}

fn framed(domain: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(domain.len() as u16).to_be_bytes());
    out.extend_from_slice(domain);
    for part in parts {
        out.extend_from_slice(&(part.len() as u32).to_be_bytes());
        out.extend_from_slice(part);
    }
    out
}

/// The Merkle leaf over one sealed chunk: its index, its ciphertext length, and
/// its ciphertext hash. Index and length are inside the leaf so a chunk cannot
/// be replayed at another position or truncated without breaking the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkLeaf {
    pub chunk_index: u32,
    pub ciphertext_len: u32,
    pub ciphertext_hash: [u8; 32],
}

impl ChunkLeaf {
    pub fn of(chunk_index: u32, ciphertext: &[u8]) -> Self {
        Self {
            chunk_index,
            ciphertext_len: ciphertext.len() as u32,
            ciphertext_hash: *blake3::hash(ciphertext).as_bytes(),
        }
    }

    pub fn hash(&self) -> [u8; 32] {
        let preimage = framed(
            CONTENT_LEAF_DOMAIN,
            &[
                &self.chunk_index.to_be_bytes(),
                &self.ciphertext_len.to_be_bytes(),
                &self.ciphertext_hash,
            ],
        );
        *blake3::hash(&preimage).as_bytes()
    }
}

fn node_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    *blake3::hash(&framed(CONTENT_NODE_DOMAIN, &[left, right])).as_bytes()
}

/// The canonical fixed-shape Merkle root over an ordered leaf set. An odd level
/// promotes its last node rather than duplicating it — duplicating a node lets
/// a tree of n leaves collide with one of n+1, which is a real attack on
/// Bitcoin-shaped trees and costs nothing to avoid.
pub fn merkle_root(leaves: &[ChunkLeaf]) -> [u8; 32] {
    if leaves.is_empty() {
        return *blake3::hash(&framed(CONTENT_NODE_DOMAIN, &[])).as_bytes();
    }
    let mut level: Vec<[u8; 32]> = leaves.iter().map(ChunkLeaf::hash).collect();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut i = 0;
        while i + 1 < level.len() {
            next.push(node_hash(&level[i], &level[i + 1]));
            i += 2;
        }
        if i < level.len() {
            next.push(level[i]);
        }
        level = next;
    }
    level[0]
}

/// The bounded sidecar that proves one chunk against a descriptor's root: the
/// leaf record plus the sibling path. A provider ships this with the chunk, so
/// a receiver can verify before it decrypts — and a cache entry counts as
/// resident only when both ciphertext and a validated sidecar are present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkProof {
    pub leaf: ChunkLeaf,
    /// Sibling hashes from the leaf upward. `is_left` says which side the
    /// sibling sat on; a promoted node contributes no sibling.
    pub path: Vec<ProofStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofStep {
    pub sibling: [u8; 32],
    pub sibling_is_left: bool,
}

/// Build the proof sidecar for `chunk_index` against the same canonical tree
/// [`merkle_root`] builds.
pub fn chunk_proof(leaves: &[ChunkLeaf], chunk_index: u32) -> Option<ChunkProof> {
    let mut position = leaves.iter().position(|l| l.chunk_index == chunk_index)?;
    let leaf = leaves[position];
    let mut level: Vec<[u8; 32]> = leaves.iter().map(ChunkLeaf::hash).collect();
    let mut path = Vec::new();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut i = 0;
        while i + 1 < level.len() {
            if position == i {
                path.push(ProofStep {
                    sibling: level[i + 1],
                    sibling_is_left: false,
                });
            } else if position == i + 1 {
                path.push(ProofStep {
                    sibling: level[i],
                    sibling_is_left: true,
                });
            }
            next.push(node_hash(&level[i], &level[i + 1]));
            i += 2;
        }
        if i < level.len() {
            // A promoted node: no sibling, so no step.
            next.push(level[i]);
        }
        position /= 2;
        level = next;
    }
    Some(ChunkProof { leaf, path })
}

impl ChunkProof {
    /// Recompute this chunk's root. Bounded before it allocates: a path longer
    /// than the protocol depth is refused rather than walked.
    pub fn root(&self) -> Result<[u8; 32], ContentError> {
        if self.path.len() > MAX_PROOF_DEPTH as usize {
            return Err(ContentError::ProofMismatch);
        }
        let mut acc = self.leaf.hash();
        for step in &self.path {
            acc = if step.sibling_is_left {
                node_hash(&step.sibling, &acc)
            } else {
                node_hash(&acc, &step.sibling)
            };
        }
        Ok(acc)
    }
}

impl ContentDescriptor {
    /// Canonical bytes.
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard content descriptor")
    }

    /// Decode, insisting the encoding was canonical — re-encoding must
    /// reproduce the exact input, so there is one representation of a
    /// descriptor and its id is well defined.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, ContentError> {
        let descriptor: Self =
            postcard::from_bytes(bytes).map_err(|_| ContentError::NonCanonical)?;
        if descriptor.encode() != bytes {
            return Err(ContentError::NonCanonical);
        }
        descriptor.validate()?;
        Ok(descriptor)
    }

    /// Structural validation, before signature or content work.
    pub fn validate(&self) -> Result<(), ContentError> {
        if self.format_version != CONTENT_FORMAT_VERSION {
            return Err(ContentError::UnsupportedVersion(self.format_version));
        }
        SpaceId::parse(&self.space).ok_or(ContentError::BadSpaceId)?;
        if self.chunk_plaintext_len != CHUNK_PLAINTEXT_LEN
            || self.plaintext_len > MAX_CONTENT_LEN
            || self.chunk_count > MAX_CHUNK_COUNT
        {
            return Err(ContentError::Geometry);
        }
        if self.chunk_count as u64 != expected_chunk_count(self.plaintext_len) {
            return Err(ContentError::Geometry);
        }
        Ok(())
    }

    /// The domain-separated hash of the canonical descriptor.
    pub fn content_ref(&self) -> ContentRef {
        let preimage = framed(CONTENT_ID_DOMAIN, &[&self.encode()]);
        ContentRef {
            content_id: *blake3::hash(&preimage).as_bytes(),
        }
    }

    /// The associated-data binding for one of this content's chunks.
    pub fn binding(&self, chunk_index: u32) -> ContentChunkBinding<'_> {
        ContentChunkBinding {
            space: &self.space,
            content_nonce: &self.content_nonce,
            chunk_index,
        }
    }

    /// Verify a chunk against this descriptor, then open it. Order is the
    /// point: length, then hash, then proof, then decryption. Every check that
    /// can be made on untrusted bytes happens before the one that costs.
    pub fn open_chunk(
        &self,
        key: &AuthorizedBodyKey,
        proof: &ChunkProof,
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, ContentError> {
        self.verify_chunk(proof, ciphertext)?;
        mechanics::crypto::content_chunk_open(
            key,
            &self.binding(proof.leaf.chunk_index),
            ciphertext,
        )
        .ok_or(ContentError::Unopenable)
    }

    /// Everything `open_chunk` checks except the decryption — what a provider or
    /// a cache does when it has no key and no business having one.
    pub fn verify_chunk(&self, proof: &ChunkProof, ciphertext: &[u8]) -> Result<(), ContentError> {
        if proof.leaf.chunk_index >= self.chunk_count {
            return Err(ContentError::Geometry);
        }
        if proof.leaf.ciphertext_len as usize != ciphertext.len() {
            return Err(ContentError::ChunkMismatch);
        }
        if ciphertext.len() > max_ciphertext_len() {
            return Err(ContentError::ChunkMismatch);
        }
        if *blake3::hash(ciphertext).as_bytes() != proof.leaf.ciphertext_hash {
            return Err(ContentError::ChunkMismatch);
        }
        if proof.root()? != self.ciphertext_merkle_root {
            return Err(ContentError::ProofMismatch);
        }
        Ok(())
    }
}

/// How many chunks a plaintext of this length occupies. Zero-length content is
/// one canonical empty chunk, so every content has at least one leaf and the
/// empty case needs no special path anywhere else.
pub fn expected_chunk_count(plaintext_len: u64) -> u64 {
    if plaintext_len == 0 {
        return 1;
    }
    plaintext_len.div_ceil(CHUNK_PLAINTEXT_LEN as u64)
}

/// The largest a sealed chunk can be: a full plaintext chunk plus the envelope's
/// fixed overhead.
pub fn max_ciphertext_len() -> usize {
    CHUNK_PLAINTEXT_LEN as usize + BODY_ENVELOPE_OVERHEAD
}

/// One content, sealed: its descriptor, its ordered ciphertexts, and a proof
/// sidecar per chunk. The three travel together because a chunk without its
/// sidecar cannot be served and a sidecar without its descriptor proves nothing.
pub struct SealedContent {
    pub descriptor: ContentDescriptor,
    pub ciphertexts: Vec<Vec<u8>>,
    pub proofs: Vec<ChunkProof>,
}

/// Seal a complete plaintext into its descriptor and chunk set. This is the
/// in-memory form — F2 adds the streaming ingest that never holds the whole
/// thing — and it is what fixtures and tests are built from.
pub fn seal_content(
    space: &SpaceId,
    key: &AuthorizedBodyKey,
    content_nonce: [u8; 16],
    plaintext: &[u8],
) -> Result<SealedContent, ContentError> {
    let plaintext_len = plaintext.len() as u64;
    if plaintext_len > MAX_CONTENT_LEN {
        return Err(ContentError::Geometry);
    }
    let chunk_count = expected_chunk_count(plaintext_len) as u32;

    // The binding needs the geometry, and the geometry is known before the
    // root is, which is exactly why the nonce and not the id is bound.
    let mut ciphertexts = Vec::with_capacity(chunk_count as usize);
    let mut leaves = Vec::with_capacity(chunk_count as usize);
    for index in 0..chunk_count {
        let start = index as usize * CHUNK_PLAINTEXT_LEN as usize;
        let end = (start + CHUNK_PLAINTEXT_LEN as usize).min(plaintext.len());
        let slice = plaintext.get(start..end).unwrap_or(&[]);
        let binding = ContentChunkBinding {
            space: space.as_str(),
            content_nonce: &content_nonce,
            chunk_index: index,
        };
        let sealed = mechanics::crypto::content_chunk_seal(key, &binding, slice);
        leaves.push(ChunkLeaf::of(index, &sealed));
        ciphertexts.push(sealed);
    }

    let descriptor = ContentDescriptor {
        format_version: CONTENT_FORMAT_VERSION,
        space: space.as_str().to_string(),
        content_nonce,
        plaintext_len,
        chunk_plaintext_len: CHUNK_PLAINTEXT_LEN,
        chunk_count,
        ciphertext_merkle_root: merkle_root(&leaves),
        epoch: *key.epoch_id(),
    };
    descriptor.validate()?;

    let proofs = (0..chunk_count)
        .map(|i| chunk_proof(&leaves, i).expect("leaf exists"))
        .collect();
    Ok(SealedContent {
        descriptor,
        ciphertexts,
        proofs,
    })
}

/// Streaming ingest: turn a byte source into sealed, verified, resident chunks
/// without ever holding the whole content.
///
/// The reason this can exist at all is that a chunk's binding names its
/// position, not its content's total size. Bind the total and the first chunk
/// cannot be sealed until the last byte has been seen, which means buffering
/// the file — exactly what the content plane exists to avoid.
///
/// Nothing durable survives a cancelled or dropped ingest: chunks go to the
/// cache under this operation's leases, and the descriptor — the only thing
/// that makes them reachable — is returned to the caller to commit, or not.
pub struct ContentIngest<'a> {
    space: SpaceId,
    key: AuthorizedBodyKey,
    content_nonce: [u8; 16],
    operation: [u8; 16],
    cache: &'a fabric::journal::cache::ResidentCache,
    buffer: Vec<u8>,
    leaves: Vec<ChunkLeaf>,
    plaintext_len: u64,
    max_len: u64,
    finished: bool,
}

/// What an ingest produced: the descriptor to commit, and the leases holding
/// its chunks resident until the caller decides.
///
/// Two holds exist on these chunks and they hand over. `leases` is the ingest's
/// own, and lasts only while the caller is deciding whether to commit the
/// descriptor at all — release it (`release_operation`) once committed. The
/// content-scoped hold, keyed by the descriptor's nonce, is what keeps the
/// bytes after that, and only a reachability sweep releases it.
pub struct IngestedContent {
    pub descriptor: ContentDescriptor,
    pub content_ref: ContentRef,
    pub leases: Vec<fabric::journal::cache::Lease>,
}

impl<'a> ContentIngest<'a> {
    /// Begin an ingest. `max_len` is operator policy and may only lower the
    /// protocol maximum.
    pub fn begin(
        space: &SpaceId,
        key: &AuthorizedBodyKey,
        operation: [u8; 16],
        cache: &'a fabric::journal::cache::ResidentCache,
        max_len: u64,
    ) -> Self {
        let mut content_nonce = [0u8; 16];
        getrandom::fill(&mut content_nonce).expect("getrandom");
        Self {
            space: space.clone(),
            key: key.clone(),
            content_nonce,
            operation,
            cache,
            buffer: Vec::with_capacity(CHUNK_PLAINTEXT_LEN as usize),
            leaves: Vec::new(),
            plaintext_len: 0,
            max_len: max_len.min(MAX_CONTENT_LEN),
            finished: false,
        }
    }

    /// Feed the next bytes. Whole chunks are sealed as they complete, so peak
    /// memory is one chunk regardless of the content's size.
    pub fn push(&mut self, mut bytes: &[u8]) -> Result<(), ContentError> {
        if self.plaintext_len + bytes.len() as u64 > self.max_len {
            return Err(ContentError::Geometry);
        }
        self.plaintext_len += bytes.len() as u64;
        while !bytes.is_empty() {
            let room = CHUNK_PLAINTEXT_LEN as usize - self.buffer.len();
            let take = room.min(bytes.len());
            self.buffer.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.buffer.len() == CHUNK_PLAINTEXT_LEN as usize {
                self.seal_buffered()?;
            }
        }
        Ok(())
    }

    fn seal_buffered(&mut self) -> Result<(), ContentError> {
        let index = self.leaves.len() as u32;
        if index >= MAX_CHUNK_COUNT {
            return Err(ContentError::Geometry);
        }
        let binding = ContentChunkBinding {
            space: self.space.as_str(),
            content_nonce: &self.content_nonce,
            chunk_index: index,
        };
        let sealed = mechanics::crypto::content_chunk_seal(&self.key, &binding, &self.buffer);
        self.buffer.clear();
        self.leaves.push(ChunkLeaf::of(index, &sealed));
        // Staged, not retained. A sealed chunk cannot be installed until the
        // tree it proves against exists, and the tree is not known until the
        // last chunk — so holding them would make peak memory the whole
        // content, which is the one thing streaming is for. Staging is opaque
        // and never advertised, so a half-finished ingest is not servable.
        self.cache
            .append_staged(&self.operation, index, 0, &sealed)
            .map_err(|_| ContentError::ChunkMismatch)?;
        Ok(())
    }

    /// Close the ingest: seal the tail, build the tree, install every chunk with
    /// its proof, and return the descriptor.
    ///
    /// Chunks install only here, once the root exists — a proof cannot be
    /// written before the tree it proves against, and the cache refuses to
    /// advertise an entry without one.
    pub fn finish(mut self) -> Result<IngestedContent, ContentError> {
        // Zero-length content is one canonical empty chunk, so a tail is sealed
        // whenever the buffer holds something or nothing has been sealed yet.
        if !self.buffer.is_empty() || self.leaves.is_empty() {
            self.seal_buffered()?;
        }
        let descriptor = ContentDescriptor {
            format_version: CONTENT_FORMAT_VERSION,
            space: self.space.as_str().to_string(),
            content_nonce: self.content_nonce,
            plaintext_len: self.plaintext_len,
            chunk_plaintext_len: CHUNK_PLAINTEXT_LEN,
            chunk_count: self.leaves.len() as u32,
            ciphertext_merkle_root: merkle_root(&self.leaves),
            epoch: *self.key.epoch_id(),
        };
        descriptor.validate()?;

        let mut leases = Vec::with_capacity(self.leaves.len());
        for leaf in &self.leaves {
            let index = leaf.chunk_index;
            let ciphertext = self
                .cache
                .read_staged(&self.operation, index)
                .map_err(|_| ContentError::ChunkMismatch)?;
            // The staged bytes round-tripped through the filesystem, so check
            // them against the leaf that was built from them before they become
            // an entry anything can serve.
            if ChunkLeaf::of(index, &ciphertext) != *leaf {
                return Err(ContentError::ChunkMismatch);
            }
            let proof = chunk_proof(&self.leaves, index).ok_or(ContentError::ProofMismatch)?;
            let sidecar = postcard::to_stdvec(&proof).map_err(|_| ContentError::NonCanonical)?;
            let entry = chunk_slot(&descriptor, index);
            self.cache
                .install(&entry, &ciphertext, &sidecar)
                .map_err(|_| ContentError::ChunkMismatch)?;
            // Two holds, for two different lifetimes. The ingest's own lease
            // lasts while the caller decides whether to commit the descriptor
            // at all. The content-scoped one lasts until the content becomes
            // unreferenced, and is keyed by the nonce so it is recoverable from
            // the descriptor alone — a sweep that has only a descriptor can
            // still let its bytes go.
            //
            // They cannot be one hold. Keying residency by transfer would let
            // the first of two concurrent fetches collect the other's bytes;
            // keying a transfer by content would make a cancelled fetch drop
            // content someone else committed.
            let lease = fabric::journal::cache::Lease::operation(self.operation, entry);
            self.cache
                .lease(&lease)
                .map_err(|_| ContentError::ChunkMismatch)?;
            self.cache
                .lease(&fabric::journal::cache::Lease::content(
                    self.content_nonce,
                    entry,
                ))
                .map_err(|_| ContentError::ChunkMismatch)?;
            leases.push(lease);
        }
        let _ = self.cache.discard_staged(&self.operation);
        self.finished = true;
        Ok(IngestedContent {
            content_ref: descriptor.content_ref(),
            descriptor,
            leases,
        })
    }

    /// Abandon the ingest. Explicit, but dropping does the same — an ingest
    /// that never finished has installed nothing.
    pub fn cancel(mut self) {
        self.finished = true;
        let _ = self.cache.discard_staged(&self.operation);
        let _ = self.cache.release_operation(&self.operation);
    }
}

impl Drop for ContentIngest<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.cache.discard_staged(&self.operation);
            let _ = self.cache.release_operation(&self.operation);
        }
    }
}

/// The cache slot one of a content's chunks is filed under.
///
/// Derived from the descriptor and the index, not from the bytes. A holder of a
/// descriptor can therefore ask "do I have chunk 7" with one lookup — filing
/// chunks under their own ciphertext hash meant the only way to answer was to
/// read and hash the entire cache, once per question.
///
/// The root is in the preimage so two contents never collide, and the index is
/// in it so a chunk cannot be filed at another position. What the slot does not
/// do is prove anything: the sidecar proves the bytes, and every read of them
/// checks the proof.
pub fn chunk_slot(descriptor: &ContentDescriptor, chunk_index: u32) -> [u8; 32] {
    let preimage = framed(
        CONTENT_SLOT_DOMAIN,
        &[
            &descriptor.ciphertext_merkle_root,
            &chunk_index.to_be_bytes(),
        ],
    );
    *blake3::hash(&preimage).as_bytes()
}

/// Read one chunk of committed content out of the cache, verified and opened.
pub fn open_resident_chunk(
    descriptor: &ContentDescriptor,
    key: &AuthorizedBodyKey,
    cache: &fabric::journal::cache::ResidentCache,
    entry: &[u8; 32],
) -> Result<Vec<u8>, ContentError> {
    let (ciphertext, sidecar) = cache.read(entry).map_err(|e| match e {
        fabric::journal::cache::CacheError::NotResident => ContentError::NotResident,
        _ => ContentError::ChunkMismatch,
    })?;
    let proof: ChunkProof =
        postcard::from_bytes(&sidecar).map_err(|_| ContentError::NonCanonical)?;
    descriptor.open_chunk(key, &proof, &ciphertext)
}
