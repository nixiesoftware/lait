#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "content ranges are checked against descriptor geometry and cache bounds before access"
)]
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

use std::io::Read;
use std::sync::Arc;

use mechanics::authorization::AuthorizedBodyKey;
use mechanics::ids::SpaceId;
use replica::content::{
    ChunkProof, ContentDescriptor, ContentIngest, ContentRef, Invalid as ContentInvalid,
    MAX_CONTENT_LEN,
};

use crate::session::StationCore;

/// The default read chunk when streaming from a reader into ingest. Sized to
/// keep one buffer small, not to match the content chunk — ingest reassembles.
const READ_BUFFER: usize = 64 * 1024;

/// Why a content operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Mechanics refused: this actor lacks the standing the operation needs.
    /// Carries what would have been required, so a caller can say how to get
    /// it rather than only that it failed.
    Denied { demand: Vec<u8> },
    /// No committed descriptor for this reference.
    Unknown,
    /// Not held locally. Content is descriptor-complete, not byte-complete.
    NotResident,
    /// Held, and sealed to an epoch this Station has no key for.
    Sealed,
    /// The content or the request exceeded a bound.
    Bounds,
    /// The store or cache refused.
    Storage(Storage),
    /// The material is here but did not verify or open.
    Invalid(ContentInvalid),
}

/// The local resource that prevented a content operation from completing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    KeyUnavailable,
    Input(std::io::ErrorKind),
    Replica,
    Cache,
    Encoding,
}

/// A residency probe that could not be taken is this Station's problem, and
/// never an answer about the content.
fn probed(answer: Result<bool, replica::content::residency::Failure>) -> Result<bool, Failure> {
    answer.map_err(|_| Failure::Storage(Storage::Cache))
}

impl Failure {
    /// Whether fetching could change this answer.
    ///
    /// The question a demand-paged read asks before retrying.
    ///
    /// `Invalid` is deliberately not fetchable, though corrupt local bytes are
    /// repairable in principle: the entry stays resident, so a fetch would
    /// install beside it and the next read would find the same bad bytes. The
    /// repair is evict-then-refetch, and evicting is a data-dropping act that
    /// wants its own authorization rather than being a side effect of reading.
    pub fn fetchable(&self) -> bool {
        matches!(self, Failure::NotResident)
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Denied { .. } => {
                write!(f, "denied: this actor lacks the required standing")
            }
            other => write!(f, "{other:?}"),
        }
    }
}
impl std::error::Error for Failure {}

impl From<ContentInvalid> for Failure {
    fn from(e: ContentInvalid) -> Self {
        match e {
            ContentInvalid::NotResident => Failure::NotResident,
            ContentInvalid::Geometry => Failure::Bounds,
            other => Failure::Invalid(other),
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

/// What an operation is asking permission to do, and to which bytes. Runtime
/// turns one of these into the Mechanics demand it checks.
///
/// `Publish` names no content because ingest is what mints one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentAction<'a> {
    Publish,
    Read(&'a ContentRef),
    Pin(&'a ContentRef),
    RemoveLocal(&'a ContentRef),
    /// Hand bytes to a peer. Distinct from Read because it is: a member who may
    /// open a file locally has not thereby agreed to become its provider, and
    /// the peer-facing surface is the one remote input reaches.
    Serve(&'a ContentRef),
}

impl<'a> ContentAction<'a> {
    /// The capability name this action needs.
    pub fn capability(self) -> &'static str {
        match self {
            ContentAction::Publish => "content.publish",
            ContentAction::Read(_) => "content.read",
            ContentAction::Pin(_) => "content.pin",
            ContentAction::RemoveLocal(_) => "content.remove-local",
            ContentAction::Serve(_) => "content.serve",
        }
    }

    /// The bytes this action is about, if they exist yet.
    pub fn content(self) -> Option<&'a ContentRef> {
        match self {
            ContentAction::Publish => None,
            ContentAction::Read(content)
            | ContentAction::Pin(content)
            | ContentAction::RemoveLocal(content)
            | ContentAction::Serve(content) => Some(content),
        }
    }
}

/// Why these bytes are being acquired, which decides how long they are held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acquisition {
    /// "I want this file." Held until nothing declares the content.
    Keep,
    /// "I am watching this now." Held for the operation only.
    Stream,
}

/// Who is asking, and what the host should check them against.
///
/// Runtime supplies this; a World never constructs one, which is what stops a
/// World from asking on someone else's behalf.
pub struct ContentPolicy<'a> {
    pub space: &'a SpaceId,
    pub keys: Arc<dyn ContentKeys>,
    /// A predicate that reads only the discriminant is Space-wide; one that
    /// reads the content can scope to what declares it
    /// (`Replica::declaring_worlds`).
    pub authorize: &'a dyn for<'c> Fn(ContentAction<'c>) -> Result<(), Vec<u8>>,
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

/// A Station's Body keys, seen as content keys.
///
/// The two traits have the same shape and different owners: [`ContentKeys`] is
/// what the content plane asks for, `BodyKeySource` is what the Space's key
/// custody offers. They are not merged because the content plane must be able
/// to exist without the Body plane's vocabulary — but on a real Station they
/// are the same epochs, and pretending otherwise would mean a Station holding
/// two answers to "which key is current".
pub struct StationContentKeys(Arc<dyn replica::body::BodyKeySource>);

impl StationContentKeys {
    pub fn new(keys: Arc<dyn replica::body::BodyKeySource>) -> Self {
        Self(keys)
    }
}

impl ContentKeys for StationContentKeys {
    fn sealing_key(&self) -> Option<AuthorizedBodyKey> {
        self.0.sealing_key()
    }
    fn opening_key(&self, epoch: &[u8; 16]) -> Option<AuthorizedBodyKey> {
        self.0.opening_key(epoch)
    }
}

/// The content plane, bound to one Station.
pub struct ContentHost {
    /// Shared rather than owned: the Station holds the same core, and both
    /// commit through the one Replica writer. A host with its own core would
    /// be a second writer, which the store does not have.
    core: Arc<StationCore>,
    cache: Arc<replica::content::Residency>,
}

impl ContentHost {
    pub fn new(core: Arc<StationCore>, cache: Arc<replica::content::Residency>) -> Self {
        Self { core, cache }
    }

    pub fn cache(&self) -> &replica::content::Residency {
        &self.cache
    }

    /// A shared handle to the same cache, for a caller that must outlive a
    /// borrow — a transfer's drop guard has to be able to let go of its leases
    /// after the host reference that created it is gone.
    pub fn cache_handle(&self) -> Arc<replica::content::Residency> {
        self.cache.clone()
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
        ctx: &replica::transaction::CommitContext<'_>,
    ) -> Result<ContentRef, Failure> {
        (policy.authorize)(ContentAction::Publish).map_err(|demand| Failure::Denied { demand })?;
        let key = policy
            .keys
            .sealing_key()
            .ok_or(Failure::Storage(Storage::KeyUnavailable))?;

        let mut ingest = ContentIngest::begin(
            policy.space,
            &key,
            operation,
            &self.cache,
            policy.max_content_len.min(MAX_CONTENT_LEN),
        )?;
        let mut buffer = vec![0u8; READ_BUFFER];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|e| Failure::Storage(Storage::Input(e.kind())))?;
            if read == 0 {
                break;
            }
            ingest.push(&buffer[..read])?;
        }
        let ingested = ingest.finish()?;

        // The hold is taken BEFORE the descriptor is committed, because the
        // commit is what republishes the read snapshot and the hold is what
        // puts this content on it. Taking it afterwards left the just-written
        // content invisible to every reader through that snapshot -- including
        // the World submit that was about to declare it, which then refused
        // its own source as though the caller had invented it.
        //
        // A hold on content whose commit then fails resolves to no descriptor
        // and so appears nowhere; it lapses on its own like any other.
        //
        // `into_std` at the crate boundary: Replica has no tokio dependency
        // and should not grow one for a deadline type. The conversion is
        // free and, importantly, does not lose the simulation — the VALUE
        // still comes from tokio's clock, so a paused test moves this hold's
        // expiry along with everything else. Only the type is std here.
        let _ = self.core.with_replica_control(|replica| {
            replica.hold_content(
                &ingested.content_ref,
                (tokio::time::Instant::now() + PENDING_DECLARATION_TTL).into_std(),
            );
            Ok(())
        });
        let committed = self.core.with_replica_metadata(|replica| {
            replica.commit_content(ctx, std::slice::from_ref(&ingested.descriptor))
        });
        if committed.is_err() {
            // Nothing durable survives a failed ingest. The chunks are already
            // installed and holding two leases, and the descriptor that would
            // have made them reachable does not exist — so without this they
            // would sit in the cache forever, held by a content nobody can name
            // and therefore invisible to every sweep.
            let _ = self
                .cache
                .release_content(&ingested.descriptor.content_nonce);
            let _ = self.cache.release_operation(&operation);
            for (_, slot) in self.resident_entries(&ingested.descriptor)? {
                let _ = self.cache.evict(&slot);
            }
            return Err(Failure::Storage(Storage::Replica));
        }

        // The descriptor is committed, so the ingest's own hold on the chunks
        // hands over to the content-scoped one — those bytes are safe from here.
        let _ = self.cache.release_operation(&operation);

        // Nothing declares this content yet, so by the reachability rule it is
        // already garbage, and it stays garbage until a Body names it — which
        // is a person choosing an issue, not a machine finishing a write. The
        // hold taken above is what buys that window, and it lapses on its own
        // so an upload nobody ever attaches is still collectable.
        Ok(ingested.content_ref)
    }

    /// What is known about one content, and how much of it is here.
    pub fn stat(
        &self,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
    ) -> Result<ContentStatus, Failure> {
        (policy.authorize)(ContentAction::Read(content))
            .map_err(|demand| Failure::Denied { demand })?;
        let descriptor = self.descriptor(content)?;
        // The one call that genuinely walks the whole content, because
        // "resident_chunks" is a question about all of them. Nothing on the
        // read path may use it: a Range request is about a span, and paying
        // chunk_count to answer a question about three chunks is how a 4 GiB
        // file makes every seek cost sixteen thousand existence checks.
        let resident = self.resident_entries(&descriptor)?.len() as u32;
        Ok(ContentStatus {
            content: *content,
            plaintext_len: descriptor.plaintext_len,
            chunk_count: descriptor.chunk_count,
            chunk_plaintext_len: descriptor.chunk_plaintext_len,
            resident_chunks: resident,
            pinned: self.is_pinned(&descriptor)?,
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
    ) -> Result<Vec<u8>, Failure> {
        (policy.authorize)(ContentAction::Read(content))
            .map_err(|demand| Failure::Denied { demand })?;
        if len > MAX_RANGE_BYTES {
            return Err(Failure::Bounds);
        }
        let descriptor = self.descriptor(content)?;
        let key = policy
            .keys
            .opening_key(&descriptor.epoch)
            .ok_or(Failure::Sealed)?;

        let chunk_len = descriptor.chunk_plaintext_len as u64;
        let end = offset
            .saturating_add(len as u64)
            .min(descriptor.plaintext_len);
        if offset >= descriptor.plaintext_len {
            return Ok(Vec::new());
        }

        // Resolve the whole span's residency before opening anything.
        //
        // Two reasons, and the second is the one that matters. First, cost:
        // only the chunks the span touches are asked about, so a range read is
        // proportional to the range and not to the content — the difference
        // between three existence checks and sixteen thousand on a 4 GiB file.
        //
        // Second, a hole is an answer, not a failure partway through. Opening
        // chunk by chunk means the caller learns about a missing chunk after
        // the ones before it were fetched from cache, decrypted, and verified —
        // work thrown away, and on a streaming surface a status line already
        // sent. Deciding first makes `NotResident` arrive before the first byte
        // is produced.
        // A zero-length read spans nothing, and "nothing" has no last chunk.
        // Without this, `end - 1` underflows: a panic under `overflow-checks`
        // and, in release, a `last` of `u32::MAX` that walks the whole chunk
        // space and reports a fully resident content as `NotResident`.
        //
        // Answered as an empty read rather than refused, because asking for
        // zero bytes is a legal thing for a caller to do — a loop whose
        // remaining length has reached zero asks exactly this.
        if end <= offset {
            return Ok(Vec::new());
        }
        let first = (offset / chunk_len) as u32;
        let last = ((end - 1) / chunk_len) as u32;
        let mut spanned = Vec::with_capacity((last - first + 1) as usize);
        for index in first..=last {
            let slot = replica::content::chunk_slot(&descriptor, index);
            if !probed(self.cache.is_resident(&slot))? {
                return Err(Failure::NotResident);
            }
            spanned.push(slot);
        }

        let mut out = Vec::with_capacity((end - offset) as usize);
        let mut cursor = offset;
        while cursor < end {
            let index = (cursor / chunk_len) as u32;
            let within = (cursor % chunk_len) as usize;
            let entry = spanned[(index - first) as usize];
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
    pub fn pin(&self, policy: &ContentPolicy<'_>, content: &ContentRef) -> Result<(), Failure> {
        (policy.authorize)(ContentAction::Pin(content))
            .map_err(|demand| Failure::Denied { demand })?;
        let descriptor = self.descriptor(content)?;
        for (_, entry) in self.resident_entries(&descriptor)? {
            self.cache
                .pin(&entry)
                .map_err(|_| Failure::Storage(Storage::Cache))?;
        }
        Ok(())
    }

    pub fn unpin(&self, policy: &ContentPolicy<'_>, content: &ContentRef) -> Result<(), Failure> {
        (policy.authorize)(ContentAction::Pin(content))
            .map_err(|demand| Failure::Denied { demand })?;
        let descriptor = self.descriptor(content)?;
        for (_, entry) in self.resident_entries(&descriptor)? {
            self.cache
                .unpin(&entry)
                .map_err(|_| Failure::Storage(Storage::Cache))?;
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
    ) -> Result<(), Failure> {
        (policy.authorize)(ContentAction::RemoveLocal(content))
            .map_err(|demand| Failure::Denied { demand })?;
        let descriptor = self.descriptor(content)?;
        let entries = self.resident_entries(&descriptor)?;
        self.cache
            .release_content(&descriptor.content_nonce)
            .map_err(|_| Failure::Storage(Storage::Cache))?;
        for (_, entry) in entries {
            let _ = self.cache.unpin(&entry);
            // Evicted rather than swept: the caller asked for these bytes to
            // go, and waiting for quota pressure that may never come is not an
            // answer. An entry another operation still holds survives, because
            // "I want this gone" does not outrank "someone is reading it".
            self.cache
                .evict(&entry)
                .map_err(|_| Failure::Storage(Storage::Cache))?;
        }
        Ok(())
    }

    // --- The provider half plan 14's Freight consumes -----------------------

    /// The committed descriptor for a content this Station can name.
    ///
    /// A fetcher needs the geometry before it asks for anything — how many
    /// chunks there are, how large the last one is, which root a proof must
    /// reconstruct — and all of that is the descriptor. Reading it is a read,
    /// so it asks the same question a range read does.
    pub fn descriptor_of(
        &self,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
    ) -> Result<ContentDescriptor, Failure> {
        (policy.authorize)(ContentAction::Read(content))
            .map_err(|demand| Failure::Denied { demand })?;
        self.descriptor(content)
    }

    /// Which of the chunks a peer asked about this Station can serve.
    ///
    /// Bounded by what was *asked*, not by what the content has. A peer naming
    /// three indices costs three existence checks even if the content has four
    /// million, so a request cannot be turned into work by being about
    /// something large.
    ///
    /// The answer is deliberately the same shape when the descriptor is
    /// unknown: an empty list. A caller that could tell "I do not hold this
    /// content" from "I have never heard of it" would have an oracle for what
    /// a Space contains, answerable by guessing content ids.
    pub fn resident_among(
        &self,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
        wanted: &[u32],
    ) -> Result<Vec<u32>, Failure> {
        (policy.authorize)(ContentAction::Serve(content))
            .map_err(|demand| Failure::Denied { demand })?;
        let descriptor = match self.descriptor(content) {
            Ok(descriptor) => descriptor,
            // Never heard of it — the same empty answer a known-but-absent
            // content gets, so the two are indistinguishable.
            Err(Failure::Unknown) => return Ok(Vec::new()),
            // Anything else is *our* problem, and reporting it as "I hold
            // nothing" would have a fetcher cache that answer and stop asking.
            Err(other) => return Err(other),
        };
        let mut answer: Vec<u32> = Vec::new();
        for index in wanted
            .iter()
            .copied()
            .filter(|index| *index < descriptor.chunk_count)
        {
            let slot = replica::content::chunk_slot(&descriptor, index);
            if probed(self.cache.is_resident(&slot))? {
                answer.push(index);
            }
        }
        answer.sort_unstable();
        answer.dedup();
        Ok(answer)
    }

    /// A bounded range of one chunk's *ciphertext*, with the proof that binds
    /// it, for serving to a peer.
    ///
    /// Ranged because a transfer that dies at 90% of a chunk should resume at
    /// 90%, not at zero. The proof covers the whole chunk regardless of the
    /// range, which is what lets a resuming peer check that it is still talking
    /// about the same bytes before it appends any.
    pub fn chunk_range(
        &self,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
        chunk_index: u32,
        offset: u64,
        max_len: usize,
    ) -> Result<(Vec<u8>, ChunkProof, u32), Failure> {
        let (bytes, proof) = self.chunk(policy, content, chunk_index)?;
        let total = u32::try_from(bytes.len()).map_err(|_| Failure::Bounds)?;
        let start = usize::try_from(offset).map_err(|_| Failure::Bounds)?;
        if start > bytes.len() {
            return Err(Failure::Bounds);
        }
        let end = start.saturating_add(max_len).min(bytes.len());
        Ok((bytes[start..end].to_vec(), proof, total))
    }

    /// Promote one staged chunk into a resident, servable entry.
    ///
    /// The whole verification happens here and nowhere earlier: the staged
    /// bytes are read back, re-hashed, and checked against the descriptor's
    /// Merkle root before anything is filed where it could be served on. Bytes
    /// that arrived over a wire have no standing until this passes.
    ///
    /// Only *this* part is discarded on success. Discarding the operation would
    /// take every other chunk of the same transfer still in flight with it.
    pub fn install_staged_chunk(
        &self,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
        operation: [u8; 16],
        intent: Acquisition,
        part: u32,
        proof: &ChunkProof,
    ) -> Result<(), Failure> {
        (policy.authorize)(ContentAction::Read(content))
            .map_err(|demand| Failure::Denied { demand })?;
        // The proof's leaf length is authenticated — the caller verified it
        // against the committed root before staging a byte — so it is the right
        // ceiling for this read, and a slot holding anything else is already
        // wrong.
        let staged_len = self.cache.staged_len(&operation, part);
        if staged_len != proof.leaf.ciphertext_len as u64 {
            let _ = self.cache.discard_staged_part(&operation, part);
            return Err(Failure::Bounds);
        }
        let staged = self
            .cache
            .read_staged(&operation, part)
            .map_err(|_| Failure::NotResident)?;

        match self.install_chunk(policy, content, operation, intent, proof, &staged) {
            Ok(()) => {
                self.cache
                    .discard_staged_part(&operation, part)
                    .map_err(|_| Failure::Storage(Storage::Cache))?;
                Ok(())
            }
            // Convicted: these bytes will never verify, so keeping them would
            // hold disk the quota check counts against the next fetch.
            Err(e @ (Failure::Invalid(_) | Failure::Bounds)) => {
                let _ = self.cache.discard_staged_part(&operation, part);
                Err(e)
            }
            // Not the bytes' fault. A storage failure is retryable and the
            // partial is still worth resuming from.
            Err(e) => Err(e),
        }
    }

    /// One chunk's sealed bytes and its proof, for serving to a peer.
    ///
    /// Returns ciphertext. A provider does not need — and must not require —
    /// the key: verification is against the descriptor's Merkle root, which is
    /// exactly why the tree commits ciphertexts.
    pub fn chunk(
        &self,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
        chunk_index: u32,
    ) -> Result<(Vec<u8>, ChunkProof), Failure> {
        (policy.authorize)(ContentAction::Serve(content))
            .map_err(|demand| Failure::Denied { demand })?;
        let descriptor = self.descriptor(content)?;
        if chunk_index >= descriptor.chunk_count {
            return Err(Failure::Bounds);
        }
        // The slot is derived, so one chunk costs one lookup. Finding it by
        // scanning the whole content's residency would mean a one-byte request
        // buying a four-million-entry sweep — which is a peer choosing how much
        // work we do.
        let entry = replica::content::chunk_slot(&descriptor, chunk_index);
        let (bytes, sidecar) = self.cache.read(&entry).map_err(|_| Failure::NotResident)?;
        let proof: ChunkProof = postcard::from_bytes(&sidecar).map_err(|_| Failure::NotResident)?;
        descriptor.verify_chunk(&proof, &bytes)?;
        Ok((bytes, proof))
    }

    /// Install a chunk fetched from a peer, verified against the descriptor
    /// before anything is written where it could be served on.
    pub fn install_chunk(
        &self,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
        operation: [u8; 16],
        intent: Acquisition,
        proof: &ChunkProof,
        ciphertext: &[u8],
    ) -> Result<(), Failure> {
        (policy.authorize)(ContentAction::Read(content))
            .map_err(|demand| Failure::Denied { demand })?;
        let descriptor = self.descriptor(content)?;
        descriptor.verify_chunk(proof, ciphertext)?;
        let entry = replica::content::chunk_slot(&descriptor, proof.leaf.chunk_index);
        let sidecar =
            postcard::to_stdvec(proof).map_err(|_| Failure::Storage(Storage::Encoding))?;
        self.cache
            .install(&entry, ciphertext, &sidecar)
            .map_err(|_| Failure::Storage(Storage::Cache))?;
        // The transfer's hold always. The content's is what outlives the
        // transfer, so only a caller that means to keep the bytes takes it —
        // a chunk behind a playhead has to become reclaimable.
        self.cache
            .hold_operation(operation, entry)
            .map_err(|_| Failure::Storage(Storage::Cache))?;
        if intent == Acquisition::Keep {
            self.cache
                .hold_content(descriptor.content_nonce, entry)
                .map_err(|_| Failure::Storage(Storage::Cache))?;
        }
        Ok(())
    }

    /// Which chunk indices this Station can serve right now.
    pub fn resident_indices(
        &self,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
    ) -> Result<Vec<u32>, Failure> {
        (policy.authorize)(ContentAction::Serve(content))
            .map_err(|demand| Failure::Denied { demand })?;
        let descriptor = self.descriptor(content)?;
        Ok(self
            .resident_entries(&descriptor)?
            .into_iter()
            .map(|(index, _)| index)
            .collect())
    }

    fn descriptor(&self, content: &ContentRef) -> Result<ContentDescriptor, Failure> {
        self.core
            .with_replica_read(|replica| Ok(replica.content_descriptor(content)))
            .map_err(|_| Failure::Storage(Storage::Replica))?
            .ok_or(Failure::Unknown)
    }

    /// The resident chunks of one content, as `(index, cache slot)`.
    ///
    /// Costs one existence check per chunk of *this* content, because the slot
    /// is derived from the descriptor. It used to read and twice-hash the
    /// entire cache on every call — and every call meant every `stat`, every
    /// range read, every pin, and once per chunk served to a peer, so a
    /// provider's cost was quadratic in what it held.
    ///
    /// Presence is the answer here; the proof is checked when the bytes are
    /// actually used, by [`Self::chunk`] and by `open_resident_chunk`.
    fn resident_entries(
        &self,
        descriptor: &ContentDescriptor,
    ) -> Result<Vec<(u32, [u8; 32])>, Failure> {
        let mut out = Vec::new();
        for index in 0..descriptor.chunk_count {
            let slot = replica::content::chunk_slot(descriptor, index);
            if probed(self.cache.is_resident(&slot))? {
                out.push((index, slot));
            }
        }
        Ok(out)
    }

    /// Whether every resident chunk of this content is pinned.
    ///
    /// Pinning is per entry, so a content with no resident chunks is not
    /// pinned — there is nothing holding anything.
    fn is_pinned(&self, descriptor: &ContentDescriptor) -> Result<bool, Failure> {
        let entries = self.resident_entries(descriptor)?;
        Ok(!entries.is_empty() && entries.iter().all(|(_, slot)| self.cache.is_pinned(slot)))
    }
}

/// Maximum bytes one range read may return. A caller wanting more loops, which
/// is what keeps a slow reader from pinning an unbounded buffer.
pub const MAX_RANGE_BYTES: usize = 4 * 1024 * 1024;

/// How long freshly ingested content is held against the sweep while it waits
/// for a Body to declare it.
///
/// Long enough for a person: an upload finishes, and then they pick the issue,
/// write the comment, and press the button. Short enough that an upload nobody
/// ever attaches is not permanent — it is disk held by a decision that was
/// never made, and the only honest ceiling on that is a clock.
pub const PENDING_DECLARATION_TTL: std::time::Duration = std::time::Duration::from_secs(600);
