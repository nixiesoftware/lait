//! The product-neutral content surface.
//!
//! What a caller gets here is deliberately narrow: ingest from a reader, ask
//! about a reference, read a bounded range, pin, and drop residency. What it
//! does **not** get is a path, a file handle, a transport, or a way to ask for
//! a whole file as one `Vec<u8>` — because a World that could name a path could
//! also name someone else's, and an API that hands back whole files is an API
//! that cannot carry a gigabyte.
//!
//! Every call carries an authorization demand. A `ContentRef` is a name, not a
//! capability: holding one proves nothing about whether this actor may publish,
//! read, or evict, and Mechanics answers that separately each time.
//!
//! Plan 14's Freight consumes the provider half of this — `stat`, `chunk`, and
//! `install_chunk` — so those shapes are frozen here rather than invented
//! there.

use std::collections::BTreeSet;
use std::io::Read;
use std::sync::Arc;

use mechanics::crypto::AuthorizedBodyKey;
use mechanics::ids::SpaceId;
use replica::content::{
    ChunkProof, ContentDescriptor, ContentError, ContentIngest, ContentRef, MAX_CONTENT_LEN,
};

use crate::session::StationCore;

/// The default read chunk when streaming from a reader into ingest. Sized to
/// keep one buffer small, not to match the content chunk — ingest reassembles.
const READ_BUFFER: usize = 64 * 1024;

/// Why a content operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentHostError {
    /// Mechanics refused: this actor lacks the standing the operation needs.
    /// Carries what would have been required, so a caller can say how to get
    /// it rather than only that it failed.
    Denied { demand: Vec<u8> },
    /// No committed descriptor for this reference.
    Unknown,
    /// The chunk is not held locally. Expected — content is
    /// descriptor-complete, not byte-complete — and plan 14 is what fetches it.
    NotResident,
    /// The content or the request exceeded a bound.
    Bounds,
    /// The store or cache refused.
    Storage(String),
    /// The material is here but did not verify or open.
    Invalid(ContentError),
}

impl std::fmt::Display for ContentHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContentHostError::Denied { .. } => {
                write!(f, "denied: this actor lacks the required standing")
            }
            other => write!(f, "{other:?}"),
        }
    }
}
impl std::error::Error for ContentHostError {}

impl From<ContentError> for ContentHostError {
    fn from(e: ContentError) -> Self {
        match e {
            ContentError::NotResident => ContentHostError::NotResident,
            ContentError::Geometry => ContentHostError::Bounds,
            other => ContentHostError::Invalid(other),
        }
    }
}

/// What the host knows about one content: the facts a product may safely see.
///
/// Not the nonce, not the epoch key, not where any byte lives. A World renders
/// "this is 4.2 MB and you hold 3 of its 17 chunks" from this and nothing more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentStatus {
    pub content: ContentRef,
    pub plaintext_len: u64,
    pub chunk_count: u32,
    pub chunk_plaintext_len: u32,
    /// How many chunks are here right now. Local, momentary, and never
    /// replicated — residency is not a property of the content.
    pub resident_chunks: u32,
    pub pinned: bool,
}

impl ContentStatus {
    pub fn is_complete(&self) -> bool {
        self.resident_chunks == self.chunk_count
    }
}

/// What an operation is asking permission to do. Runtime turns one of these
/// into the Mechanics demand it checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentAction {
    Publish,
    Read,
    Pin,
    RemoveLocal,
}

impl ContentAction {
    /// The capability name this action needs.
    pub fn capability(self) -> &'static str {
        match self {
            ContentAction::Publish => "content.publish",
            ContentAction::Read => "content.read",
            ContentAction::Pin => "content.pin",
            ContentAction::RemoveLocal => "content.remove-local",
        }
    }
}

/// Who is asking, and what the host should check them against.
///
/// Runtime supplies this; a World never constructs one, which is what stops a
/// World from asking on someone else's behalf.
pub struct ContentPolicy<'a> {
    pub space: &'a SpaceId,
    pub keys: Arc<dyn ContentKeys>,
    pub authorize: &'a dyn Fn(ContentAction) -> Result<(), Vec<u8>>,
    /// Operator ceiling on one content's length. May only lower the protocol
    /// maximum.
    pub max_content_len: u64,
}

/// Where the host gets the epoch capability to seal and open with.
///
/// A trait rather than a key, so the host never holds material it could leak
/// and the composition root stays the only thing that decides which epoch is
/// authorized.
pub trait ContentKeys: Send + Sync {
    /// The capability to seal new content under.
    fn sealing_key(&self) -> Option<AuthorizedBodyKey>;
    /// The capability for an existing content's epoch, if this Station holds
    /// one. Absent means the content stays sealed — lazy revocation, not an
    /// error.
    fn opening_key(&self, epoch: &[u8; 16]) -> Option<AuthorizedBodyKey>;
}

/// The content plane, bound to one Station.
pub struct ContentHost {
    core: StationCore,
    cache: Arc<replica::journal::cache::ResidentCache>,
}

impl ContentHost {
    pub fn new(core: StationCore, cache: Arc<replica::journal::cache::ResidentCache>) -> Self {
        Self { core, cache }
    }

    pub fn cache(&self) -> &replica::journal::cache::ResidentCache {
        &self.cache
    }

    /// Ingest from a reader, returning the reference a World may then name.
    ///
    /// The reader is consumed incrementally and never materialised: peak memory
    /// is one chunk plus the read buffer, whatever the content's size. Failure
    /// or cancellation anywhere leaves nothing durable, because the descriptor
    /// is what makes chunks reachable and it is only committed at the end.
    pub fn ingest(
        &self,
        policy: &ContentPolicy<'_>,
        operation: [u8; 16],
        reader: &mut dyn Read,
        ctx: &replica::CommitContext<'_>,
    ) -> Result<ContentRef, ContentHostError> {
        (policy.authorize)(ContentAction::Publish)
            .map_err(|demand| ContentHostError::Denied { demand })?;
        let key = policy
            .keys
            .sealing_key()
            .ok_or_else(|| ContentHostError::Storage("no authorized sealing key".into()))?;

        let mut ingest = ContentIngest::begin(
            policy.space,
            &key,
            operation,
            &self.cache,
            policy.max_content_len.min(MAX_CONTENT_LEN),
        );
        let mut buffer = vec![0u8; READ_BUFFER];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|e| ContentHostError::Storage(e.to_string()))?;
            if read == 0 {
                break;
            }
            ingest.push(&buffer[..read])?;
        }
        let ingested = ingest.finish()?;

        self.core
            .with_replica(|replica| {
                replica.commit_content(ctx, std::slice::from_ref(&ingested.descriptor))
            })
            .map_err(|e| ContentHostError::Storage(e.to_string()))?;

        // The descriptor is committed, so the ingest's own hold hands over to
        // the content-scoped one.
        let _ = self.cache.release_operation(&operation);
        Ok(ingested.content_ref)
    }

    /// What is known about one content, and how much of it is here.
    pub fn stat(
        &self,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
    ) -> Result<ContentStatus, ContentHostError> {
        (policy.authorize)(ContentAction::Read)
            .map_err(|demand| ContentHostError::Denied { demand })?;
        let descriptor = self.descriptor(content)?;
        let resident = self.resident_entries(&descriptor).len() as u32;
        Ok(ContentStatus {
            content: *content,
            plaintext_len: descriptor.plaintext_len,
            chunk_count: descriptor.chunk_count,
            chunk_plaintext_len: descriptor.chunk_plaintext_len,
            resident_chunks: resident,
            pinned: false,
        })
    }

    /// Read one bounded range of a content's plaintext.
    ///
    /// Bounded by construction: the caller names an offset and a length, and
    /// only the chunks that span it are opened. There is deliberately no "read
    /// it all" — a surface that offered one could not carry a large file, and a
    /// caller that wants the whole thing loops.
    pub fn read_range(
        &self,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>, ContentHostError> {
        (policy.authorize)(ContentAction::Read)
            .map_err(|demand| ContentHostError::Denied { demand })?;
        if len > MAX_RANGE_BYTES {
            return Err(ContentHostError::Bounds);
        }
        let descriptor = self.descriptor(content)?;
        let key = policy
            .keys
            .opening_key(&descriptor.epoch)
            .ok_or(ContentHostError::NotResident)?;

        let chunk_len = descriptor.chunk_plaintext_len as u64;
        let end = offset
            .saturating_add(len as u64)
            .min(descriptor.plaintext_len);
        if offset >= descriptor.plaintext_len {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity((end - offset) as usize);
        let mut cursor = offset;
        let entries = self.resident_entries(&descriptor);
        while cursor < end {
            let index = (cursor / chunk_len) as u32;
            let within = (cursor % chunk_len) as usize;
            let entry = entries
                .iter()
                .find(|(i, _)| *i == index)
                .map(|(_, e)| *e)
                .ok_or(ContentHostError::NotResident)?;
            let plaintext =
                replica::content::open_resident_chunk(&descriptor, &key, &self.cache, &entry)?;
            let take = ((end - cursor) as usize).min(plaintext.len().saturating_sub(within));
            if take == 0 {
                break;
            }
            out.extend_from_slice(&plaintext[within..within + take]);
            cursor += take as u64;
        }
        Ok(out)
    }

    /// Hold this content against quota pressure until unpinned.
    pub fn pin(
        &self,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
    ) -> Result<(), ContentHostError> {
        (policy.authorize)(ContentAction::Pin)
            .map_err(|demand| ContentHostError::Denied { demand })?;
        let descriptor = self.descriptor(content)?;
        for (_, entry) in self.resident_entries(&descriptor) {
            self.cache
                .pin(&entry)
                .map_err(|e| ContentHostError::Storage(e.to_string()))?;
        }
        Ok(())
    }

    pub fn unpin(
        &self,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
    ) -> Result<(), ContentHostError> {
        (policy.authorize)(ContentAction::Pin)
            .map_err(|demand| ContentHostError::Denied { demand })?;
        let descriptor = self.descriptor(content)?;
        for (_, entry) in self.resident_entries(&descriptor) {
            self.cache
                .unpin(&entry)
                .map_err(|e| ContentHostError::Storage(e.to_string()))?;
        }
        Ok(())
    }

    /// Drop this content's bytes locally. The descriptor is untouched, so the
    /// content is still named, still referenced, and still fetchable — this
    /// reclaims space, it does not forget anything.
    pub fn remove_local(
        &self,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
    ) -> Result<(), ContentHostError> {
        (policy.authorize)(ContentAction::RemoveLocal)
            .map_err(|demand| ContentHostError::Denied { demand })?;
        let descriptor = self.descriptor(content)?;
        let entries = self.resident_entries(&descriptor);
        self.cache
            .release_operation(&descriptor.content_nonce)
            .map_err(|e| ContentHostError::Storage(e.to_string()))?;
        for (_, entry) in entries {
            let _ = self.cache.unpin(&entry);
            // Evicted rather than swept: the caller asked for these bytes to
            // go, and waiting for quota pressure that may never come is not an
            // answer. An entry another operation still holds survives, because
            // "I want this gone" does not outrank "someone is reading it".
            self.cache
                .evict(&entry)
                .map_err(|e| ContentHostError::Storage(e.to_string()))?;
        }
        Ok(())
    }

    // --- The provider half plan 14's Freight consumes -----------------------

    /// One chunk's sealed bytes and its proof, for serving to a peer.
    ///
    /// Returns ciphertext. A provider does not need — and must not require —
    /// the key: verification is against the descriptor's Merkle root, which is
    /// exactly why the tree commits ciphertexts.
    pub fn chunk(
        &self,
        content: &ContentRef,
        chunk_index: u32,
    ) -> Result<(Vec<u8>, ChunkProof), ContentHostError> {
        let descriptor = self.descriptor(content)?;
        let entry = self
            .resident_entries(&descriptor)
            .into_iter()
            .find(|(i, _)| *i == chunk_index)
            .map(|(_, e)| e)
            .ok_or(ContentHostError::NotResident)?;
        let (bytes, sidecar) = self
            .cache
            .read(&entry)
            .map_err(|_| ContentHostError::NotResident)?;
        let proof: ChunkProof =
            postcard::from_bytes(&sidecar).map_err(|_| ContentHostError::NotResident)?;
        descriptor.verify_chunk(&proof, &bytes)?;
        Ok((bytes, proof))
    }

    /// Install a chunk fetched from a peer, verified against the descriptor
    /// before anything is written where it could be served on.
    pub fn install_chunk(
        &self,
        content: &ContentRef,
        operation: [u8; 16],
        proof: &ChunkProof,
        ciphertext: &[u8],
    ) -> Result<(), ContentHostError> {
        let descriptor = self.descriptor(content)?;
        descriptor.verify_chunk(proof, ciphertext)?;
        let entry = replica::content::chunk_entry(ciphertext);
        let sidecar =
            postcard::to_stdvec(proof).map_err(|e| ContentHostError::Storage(e.to_string()))?;
        self.cache
            .install(&entry, ciphertext, &sidecar)
            .map_err(|e| ContentHostError::Storage(e.to_string()))?;
        // Both holds, as ingest takes them: the transfer's, and the content's.
        for hold in [operation, descriptor.content_nonce] {
            self.cache
                .lease(&replica::journal::cache::Lease {
                    operation: hold,
                    entry,
                })
                .map_err(|e| ContentHostError::Storage(e.to_string()))?;
        }
        Ok(())
    }

    /// Which chunk indices this Station can serve right now.
    pub fn resident_indices(&self, content: &ContentRef) -> Result<Vec<u32>, ContentHostError> {
        let descriptor = self.descriptor(content)?;
        Ok(self
            .resident_entries(&descriptor)
            .into_iter()
            .map(|(index, _)| index)
            .collect())
    }

    fn descriptor(&self, content: &ContentRef) -> Result<ContentDescriptor, ContentHostError> {
        self.core
            .with_replica(|replica| Ok(replica.content_descriptor(content)))
            .map_err(|e| ContentHostError::Storage(e.to_string()))?
            .ok_or(ContentHostError::Unknown)
    }

    /// The resident chunks of one content, as `(index, cache entry)`.
    ///
    /// Derived from the sidecars rather than from an index, because a sidecar
    /// is what makes a chunk servable and holding a second map of the same fact
    /// would be a second thing to keep true.
    fn resident_entries(&self, descriptor: &ContentDescriptor) -> Vec<(u32, [u8; 32])> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for entry in self.cache.entries() {
            let Ok((bytes, sidecar)) = self.cache.read(&entry) else {
                continue;
            };
            let Ok(proof) = postcard::from_bytes::<ChunkProof>(&sidecar) else {
                continue;
            };
            if descriptor.verify_chunk(&proof, &bytes).is_ok()
                && seen.insert(proof.leaf.chunk_index)
            {
                out.push((proof.leaf.chunk_index, entry));
            }
        }
        out.sort_by_key(|(index, _)| *index);
        out
    }
}

/// Maximum bytes one range read may return. A caller wanting more loops, which
/// is what keeps a slow reader from pinning an unbounded buffer.
pub const MAX_RANGE_BYTES: usize = 4 * 1024 * 1024;
