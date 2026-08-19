#![allow(
    dead_code,
    reason = "the reader lands ahead of the Fetcher adapter that will supply it"
)]
//! A seekable, verified, demand-paged reader over one content.
//!
//! `read_range` answers a whole span or refuses it, which is right for a caller
//! that knows what it wants and wrong for one walking a file it does not hold:
//! it cannot say where it got to, it cannot resume, and a hole in the middle
//! throws away everything opened before it. A cursor is the position, and one
//! step is one chunk — so peak memory is one chunk whatever the content's size,
//! and a hole is an answer that leaves the position where it was.
//!
//! The policy is presented at the pull rather than stored. A cursor is not a
//! capability: holding one across a revocation grants nothing, because the next
//! step asks again — for standing before any byte exists, and for the epoch key
//! before every chunk.
//!
//! Nothing here names the Freight plane. Chunks that are not here come from a
//! [`ChunkSupply`], which knows only how to be asked for chunk indices of one
//! content and how to be told to stop.

use std::sync::Arc;

use replica::content::{ContentDescriptor, ContentRef, Invalid as ContentInvalid};

use crate::content_host::{ContentAction, ContentHost, ContentPolicy, Failure};

/// Verified plaintext, and where in the content it starts.
///
/// There is no public constructor. The only thing that mints one is the private
/// step below, after the chunk it came from verified against the committed
/// Merkle root and opened under the epoch key — so bytes that skipped
/// verification are not expressible here rather than merely discouraged.
pub struct PlaintextSpan {
    offset: u64,
    bytes: Vec<u8>,
}

impl PlaintextSpan {
    /// Where these bytes begin in the content's plaintext.
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Geometry, never content: a span in a log line must not be the file.
impl std::fmt::Debug for PlaintextSpan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaintextSpan")
            .field("offset", &self.offset)
            .field("len", &self.bytes.len())
            .finish()
    }
}

/// Why the next bytes are not here.
///
/// Deliberately without a length, a `Default`, or a number: a hole is not a
/// quantity, and one that could be added to a byte counter would be counted as
/// progress. What it carries is the *kind* of absence, because only some kinds
/// are worth acting on and folding them together is this codebase's
/// false-disconnection defect one layer down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gap {
    /// Asked, and it is on its way. The next step is the one that reads it.
    Fetching,
    /// Asked, and nobody offered it.
    Unoffered,
    /// Nothing is attached that could fetch. A truthful end, not a stall.
    Unsupplied,
    /// The supply refused the ask. Nothing moved, and asking again may work.
    Refused,
    /// The supply could not be asked at all — a seam that is down, not a peer
    /// that said no. Never fold this into [`Gap::Unoffered`]: "could not ask"
    /// and "nobody has it" are different facts, and only one of them is about
    /// the content.
    Unasked,
}

/// The result of one step.
#[must_use]
pub enum Advance {
    Yielded {
        cursor: ContentCursor,
        span: PlaintextSpan,
    },
    /// Not here yet. The position did not move.
    Blocked { cursor: ContentCursor, gap: Gap },
    /// Reached the end of the window. The cursor survives so a player can seek
    /// back.
    Finished { cursor: ContentCursor },
    /// The cursor is consumed, so reading on after a refusal does not compile.
    Refused(Failure),
}

/// The result of a seek.
#[must_use]
pub enum Seek {
    Moved(ContentCursor),
    /// Outside the window. Nothing moved and the cursor survives, because
    /// overshooting is what a scrub bar does — and refusing rather than
    /// clamping is what lets a caller settle a 416 before its status line is
    /// spent.
    Outside(ContentCursor),
}

/// What a span would do if it were read, decided before a byte is opened.
///
/// This exists because the response is decided before the body starts: a ranged
/// caller has one status line and has to spend it knowing whether the bytes are
/// here.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum Readiness {
    /// Every chunk the span touches is here.
    Resident,
    /// Not all here, and this is the chunk a read would stop at.
    Absent { chunk: u32 },
    /// The span will not be read at all: no standing, no key, or not inside
    /// this content.
    Refused(Failure),
}

/// Where a cursor's missing chunks come from.
///
/// Narrow on purpose. A cursor can name chunk indices of one content and can
/// say when it has stopped caring; it cannot name a peer, a transport, or a
/// plane. That is what keeps the reader testable without a network, and what
/// leaves the choice of provider where it belongs.
pub trait ChunkSupply: Send + Sync {
    /// Ask for exactly these chunks, and say why they are still not here.
    ///
    /// Never blocks: a cursor steps, it does not wait. A supply that delivered
    /// synchronously still answers [`Gap::Fetching`] — one step yields at most
    /// one chunk, so the step that had to ask is not also the step that reads.
    fn request(&self, content: &ContentRef, operation: [u8; 16], chunks: &[u32]) -> Gap;

    /// Stop: nobody is waiting for this operation's chunks any more.
    fn abandon(&self, content: &ContentRef, operation: [u8; 16]);
}

/// A Station that will never fetch.
///
/// Not a stub. A Station with no Freight plane, and a caller that wants only
/// what is already here, are both real — and "nothing can fetch this" is an
/// answer, where waiting forever is not.
pub struct NoSupply;

impl ChunkSupply for NoSupply {
    fn request(&self, _content: &ContentRef, _operation: [u8; 16], _chunks: &[u32]) -> Gap {
        Gap::Unsupplied
    }

    fn abandon(&self, _content: &ContentRef, _operation: [u8; 16]) {}
}

/// A position in one content, and the supply that keeps it fed.
pub struct ContentCursor {
    host: Arc<ContentHost>,
    content: ContentRef,
    /// Resolved once: it is committed immutable geometry, and it is what every
    /// chunk's proof is checked against. Re-reading it per step would buy
    /// nothing and cost a store read per chunk.
    descriptor: ContentDescriptor,
    supply: Arc<dyn ChunkSupply>,
    /// This cursor's own name at the supply seam, so what it started is
    /// nameable when it is dropped.
    operation: [u8; 16],
    start: u64,
    end: u64,
    position: u64,
    /// Whether anything was ever asked for. A cursor that never asked has
    /// nothing to abandon, and sweeping an operation nobody used would read the
    /// whole tag directory to find nothing.
    asked: bool,
    /// The chunk this cursor holds against the sweep, if any.
    ///
    /// One, not the window: a reader holds what it is reading, and lets go of
    /// what it has passed. The lease is this cursor's own, so two readers of one
    /// film hold one chunk set between them and it survives until the later of
    /// them moves on.
    held: Option<u32>,
}

impl ContentCursor {
    /// Open a cursor over the whole of one content.
    pub fn open(
        host: Arc<ContentHost>,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
        supply: Arc<dyn ChunkSupply>,
    ) -> Result<Self, Failure> {
        Self::open_range(host, policy, content, 0, u64::MAX, supply)
    }

    /// Open a cursor over `[offset, offset + len)`, clamped to the content.
    ///
    /// The window is resolved here and never again: one that could change under
    /// a reader is a length disagreeing with a `Content-Range` already sent. An
    /// offset past the end is refused rather than clamped, because that is a
    /// 416 and it has to be decidable now.
    pub fn open_range(
        host: Arc<ContentHost>,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
        offset: u64,
        len: u64,
        supply: Arc<dyn ChunkSupply>,
    ) -> Result<Self, Failure> {
        let descriptor = host.descriptor_of(policy, content)?;
        if offset > descriptor.plaintext_len {
            return Err(Failure::Bounds);
        }
        let end = offset.saturating_add(len).min(descriptor.plaintext_len);
        let mut operation = [0u8; 16];
        getrandom::fill(&mut operation).map_err(|source| {
            tracing::error!(error = %source, "OS entropy unavailable while opening a content cursor");
            Failure::Invalid(ContentInvalid::Protection(
                mechanics::authorization::Failure::Randomness,
            ))
        })?;
        Ok(Self {
            host,
            content: *content,
            descriptor,
            supply,
            operation,
            start: offset,
            end,
            position: offset,
            asked: false,
            held: None,
        })
    }

    pub const fn content(&self) -> &ContentRef {
        &self.content
    }

    pub const fn position(&self) -> u64 {
        self.position
    }

    /// The content's whole length, which is what a 416 has to report even
    /// though this cursor may only be allowed to see part of it.
    pub const fn plaintext_len(&self) -> u64 {
        self.descriptor.plaintext_len
    }

    /// Bytes left in the window. Geometry, not a promise that they are here.
    pub const fn remaining(&self) -> u64 {
        self.end.saturating_sub(self.position)
    }

    /// Move to `offset`, an absolute position in the content's plaintext.
    ///
    /// No bytes, no authorization, no I/O — a seek is arithmetic, and asking
    /// permission for it would authorize a read that has not happened.
    pub fn seek(mut self, offset: u64) -> Seek {
        if offset < self.start || offset > self.end {
            return Seek::Outside(self);
        }
        self.position = offset;
        Seek::Moved(self)
    }

    /// Whether a span could be read, without reading it.
    ///
    /// `offset` is absolute, in the same coordinates as [`Self::position`].
    /// Costs at most one existence check per chunk *of the span*, and stops at
    /// the first hole — a question about three chunks must not cost the sixteen
    /// thousand a 4 GiB content has.
    pub fn readiness(&self, policy: &ContentPolicy<'_>, offset: u64, len: u64) -> Readiness {
        if let Err(demand) = (policy.authorize)(ContentAction::Read(&self.content)) {
            return Readiness::Refused(Failure::Denied { demand });
        }
        // Sealed is not a hole: no fetch changes it, and a caller told "not here
        // yet" would retry forever.
        if policy.keys.opening_key(&self.descriptor.epoch).is_none() {
            return Readiness::Refused(Failure::Sealed);
        }
        if offset > self.descriptor.plaintext_len {
            return Readiness::Refused(Failure::Bounds);
        }
        let end = offset
            .saturating_add(len)
            .min(self.descriptor.plaintext_len);
        if end <= offset {
            return Readiness::Resident;
        }
        let chunk_len = u64::from(self.descriptor.chunk_plaintext_len);
        let (Some(first), Some(last)) = (
            offset.checked_div(chunk_len),
            end.saturating_sub(1).checked_div(chunk_len),
        ) else {
            return Readiness::Refused(Failure::Bounds);
        };
        for index in first..=last {
            let Ok(index) = u32::try_from(index) else {
                return Readiness::Refused(Failure::Bounds);
            };
            let slot = replica::content::chunk_slot(&self.descriptor, index);
            if !self.host.cache().is_resident(&slot) {
                return Readiness::Absent { chunk: index };
            }
        }
        Readiness::Resident
    }

    /// One step: at most one chunk, verified and opened.
    ///
    /// Standing is asked for before any byte exists, and the epoch key is
    /// resolved again here rather than held — so revoking either stops playback
    /// at the next chunk instead of at the next open.
    pub fn next(mut self, policy: &ContentPolicy<'_>) -> Advance {
        if let Err(demand) = (policy.authorize)(ContentAction::Read(&self.content)) {
            return Advance::Refused(Failure::Denied { demand });
        }
        if self.position >= self.end {
            return Advance::Finished { cursor: self };
        }
        let Some(key) = policy.keys.opening_key(&self.descriptor.epoch) else {
            return Advance::Refused(Failure::Sealed);
        };

        let chunk_len = u64::from(self.descriptor.chunk_plaintext_len);
        let (Some(index), Some(within)) = (
            self.position.checked_div(chunk_len),
            self.position.checked_rem(chunk_len),
        ) else {
            return Advance::Refused(Failure::Bounds);
        };
        let (Ok(index), Ok(within)) = (u32::try_from(index), usize::try_from(within)) else {
            return Advance::Refused(Failure::Bounds);
        };
        if index >= self.descriptor.chunk_count {
            return Advance::Refused(Failure::Bounds);
        }

        let slot = replica::content::chunk_slot(&self.descriptor, index);
        if !self.host.cache().is_resident(&slot) {
            return self.ask(index);
        }
        self.hold(index, &slot);
        let plaintext = match replica::content::open_resident_chunk(
            &self.descriptor,
            &key,
            self.host.cache(),
            &slot,
        ) {
            Ok(plaintext) => plaintext,
            Err(invalid) => {
                let failure = Failure::from(invalid);
                // Evicted between the probe and the read. Fetching changes that
                // answer, so it is a hole rather than an end.
                return if failure.fetchable() {
                    self.ask(index)
                } else {
                    Advance::Refused(failure)
                };
            }
        };

        let want = self.end.saturating_sub(self.position);
        let available = plaintext.len().saturating_sub(within);
        let take = usize::try_from(want).unwrap_or(usize::MAX).min(available);
        let Some(bytes) = plaintext.get(within..within.saturating_add(take)) else {
            return Advance::Refused(Failure::Bounds);
        };
        // A chunk too short to cover the position it was filed under means the
        // descriptor and the stored bytes disagree, which is not a hole.
        if bytes.is_empty() {
            return Advance::Refused(Failure::Bounds);
        }
        let span = PlaintextSpan {
            offset: self.position,
            bytes: bytes.to_vec(),
        };
        self.position = self
            .position
            .saturating_add(u64::try_from(take).unwrap_or(u64::MAX));
        Advance::Yielded { cursor: self, span }
    }

    /// Hold the chunk being read, and let go of the one before it.
    ///
    /// Taken before the open rather than after, so the sweep cannot take the
    /// bytes between the probe and the read.
    fn hold(&mut self, index: u32, slot: &[u8; 32]) {
        if self.held == Some(index) {
            return;
        }
        let cache = self.host.cache();
        if let Some(previous) = self.held {
            let stale = replica::content::chunk_slot(&self.descriptor, previous);
            let _ = cache.release_operation_entry(self.operation, stale);
        }
        if cache.hold_operation(self.operation, *slot).is_ok() {
            self.held = Some(index);
        }
    }

    fn ask(mut self, index: u32) -> Advance {
        let gap = self.supply.request(&self.content, self.operation, &[index]);
        self.asked = true;
        Advance::Blocked { cursor: self, gap }
    }
}

impl std::fmt::Debug for ContentCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentCursor")
            .field("content", &self.content)
            .field("position", &self.position)
            .field("window", &(self.start, self.end))
            .finish()
    }
}

/// A disconnected client lets go of what it started without the caller
/// remembering to.
impl Drop for ContentCursor {
    fn drop(&mut self) {
        if !self.asked && self.held.is_none() {
            return;
        }
        // Told before swept: the supply gets to stop first, so tearing down its
        // residue does not race a task that is still writing into it.
        self.supply.abandon(&self.content, self.operation);
        let cache = self.host.cache();
        let _ = cache.release_operation(&self.operation);
        let _ = cache.discard_staged(&self.operation);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use mechanics::authorization::AuthorizedBodyKey;
    use mechanics::ids::SpaceId;
    use replica::content::{ChunkProof, ContentRef, Residency, CHUNK_PLAINTEXT_LEN};

    use super::*;
    use crate::content_host::{Acquisition, ContentKeys};

    const EPOCH: [u8; 16] = [7u8; 16];
    const EPOCH_KEY: [u8; 32] = [11u8; 32];
    const WRITER_SEED: [u8; 32] = [29u8; 32];
    const CHUNK: usize = CHUNK_PLAINTEXT_LEN as usize;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("lait-cursor-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn space() -> SpaceId {
        SpaceId::from_digest([44u8; 16])
    }

    fn epoch_key() -> AuthorizedBodyKey {
        AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY)
    }

    struct Keys;
    impl ContentKeys for Keys {
        fn sealing_key(&self) -> Option<AuthorizedBodyKey> {
            Some(epoch_key())
        }
        fn opening_key(&self, epoch: &[u8; 16]) -> Option<AuthorizedBodyKey> {
            (epoch == &EPOCH).then(epoch_key)
        }
    }

    /// Keys that stop answering after `budget` chunks — a custody revocation
    /// that lands mid-playback.
    struct FadingKeys {
        budget: usize,
        opened: AtomicUsize,
    }
    impl ContentKeys for FadingKeys {
        fn sealing_key(&self) -> Option<AuthorizedBodyKey> {
            Some(epoch_key())
        }
        fn opening_key(&self, _epoch: &[u8; 16]) -> Option<AuthorizedBodyKey> {
            (self.opened.fetch_add(1, Ordering::SeqCst) < self.budget).then(epoch_key)
        }
    }

    /// Content sealed to an epoch this Station holds no capability for.
    struct SealedKeys;
    impl ContentKeys for SealedKeys {
        fn sealing_key(&self) -> Option<AuthorizedBodyKey> {
            Some(epoch_key())
        }
        fn opening_key(&self, _epoch: &[u8; 16]) -> Option<AuthorizedBodyKey> {
            None
        }
    }

    /// A supply that answers whatever it was told to, and remembers being asked.
    struct Recording {
        answer: Gap,
        asked: Mutex<Vec<Vec<u32>>>,
        abandoned: AtomicUsize,
    }

    impl Recording {
        fn new(answer: Gap) -> Arc<Self> {
            Arc::new(Self {
                answer,
                asked: Mutex::new(Vec::new()),
                abandoned: AtomicUsize::new(0),
            })
        }
    }

    impl ChunkSupply for Recording {
        fn request(&self, _content: &ContentRef, _operation: [u8; 16], chunks: &[u32]) -> Gap {
            self.asked.lock().unwrap().push(chunks.to_vec());
            self.answer
        }
        fn abandon(&self, _content: &ContentRef, _operation: [u8; 16]) {
            self.abandoned.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn filler(seed: u64, len: usize) -> Vec<u8> {
        let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) as u8
            })
            .collect()
    }

    struct Fixture {
        host: Arc<ContentHost>,
        _dir: PathBuf,
    }

    impl Fixture {
        /// A cursor over the whole content, which is what most of these want.
        fn cursor(
            &self,
            policy: &ContentPolicy<'_>,
            content: &ContentRef,
            supply: Arc<dyn ChunkSupply>,
        ) -> ContentCursor {
            ContentCursor::open(self.host.clone(), policy, content, supply).expect("open")
        }
    }

    fn fixture(tag: &str) -> Fixture {
        let dir = temp_dir(tag);
        let core = Arc::new(crate::session::StationCore::for_test(
            replica::Replica::open(
                dir.join("store"),
                Arc::new(replica::body::StaticBodyKeys::new(epoch_key())),
            )
            .unwrap(),
        ));
        let cache = Arc::new(Residency::open(dir.join("cache"), 1 << 30).unwrap());
        Fixture {
            host: Arc::new(ContentHost::new(core, cache)),
            _dir: dir,
        }
    }

    fn policy_with<'a>(
        space: &'a SpaceId,
        keys: Arc<dyn ContentKeys>,
        authorize: &'a dyn for<'c> Fn(ContentAction<'c>) -> Result<(), Vec<u8>>,
    ) -> ContentPolicy<'a> {
        ContentPolicy {
            space,
            keys,
            authorize,
            max_content_len: u64::MAX,
        }
    }

    fn ingest(
        fx: &Fixture,
        policy: &ContentPolicy<'_>,
        space: &SpaceId,
        bytes: &[u8],
    ) -> ContentRef {
        let signer = replica::transaction::SeedSigner(&WRITER_SEED);
        let ctx = replica::transaction::CommitContext {
            space,
            signer: &signer,
            authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![9]),
        };
        let mut operation = [0u8; 16];
        operation[..8].copy_from_slice(&COUNTER.fetch_add(1, Ordering::SeqCst).to_le_bytes());
        fx.host
            .ingest(
                policy,
                operation,
                &mut std::io::Cursor::new(bytes.to_vec()),
                &ctx,
            )
            .expect("ingest")
    }

    /// Drop one chunk's bytes, keeping the descriptor and every other chunk.
    ///
    /// The content lease holds every chunk of committed content, so it has to go
    /// before anything is evictable — which is what a Station that forgot these
    /// bytes and can refetch them looks like. The served chunk comes back so a
    /// test can put it right again through the verifying install path.
    fn evict_chunk(
        fx: &Fixture,
        policy: &ContentPolicy<'_>,
        content: &ContentRef,
        index: u32,
    ) -> (Vec<u8>, ChunkProof) {
        let saved = fx.host.chunk(policy, content, index).expect("serve");
        let descriptor = fx.host.descriptor_of(policy, content).expect("descriptor");
        fx.host
            .cache()
            .release_content(&descriptor.content_nonce)
            .expect("release");
        let slot = replica::content::chunk_slot(&descriptor, index);
        assert!(fx.host.cache().evict(&slot).expect("evict"));
        saved
    }

    /// Step until the window ends or a hole stops it, checking as it goes that
    /// spans arrive in order and leave no bytes behind.
    fn read_on(mut cursor: ContentCursor, policy: &ContentPolicy<'_>) -> (Vec<u8>, Option<Gap>) {
        let mut at = cursor.position();
        let mut out = Vec::new();
        loop {
            match cursor.next(policy) {
                Advance::Yielded { cursor: next, span } => {
                    assert_eq!(span.offset(), at, "spans arrive in order and without gaps");
                    at += span.len() as u64;
                    out.extend_from_slice(span.bytes());
                    cursor = next;
                }
                Advance::Blocked { gap, .. } => return (out, Some(gap)),
                Advance::Finished { .. } => return (out, None),
                Advance::Refused(failure) => panic!("refused: {failure:?}"),
            }
        }
    }

    fn describe(advance: &Advance) -> String {
        match advance {
            Advance::Yielded { span, .. } => format!("yielded {span:?}"),
            Advance::Blocked { gap, .. } => format!("blocked {gap:?}"),
            Advance::Finished { .. } => "finished".into(),
            Advance::Refused(failure) => format!("refused {failure:?}"),
        }
    }

    #[test]
    fn verified_bytes_come_back_in_order_and_match_what_was_ingested() {
        let fx = fixture("order");
        let space = space();
        let allow = |_: ContentAction<'_>| Ok(());
        let read = policy_with(&space, Arc::new(Keys), &allow);
        let plaintext = filler(1, CHUNK * 2 + 777);
        let content = ingest(&fx, &read, &space, &plaintext);

        let cursor = fx.cursor(&read, &content, Arc::new(NoSupply));
        let (got, gap) = read_on(cursor, &read);
        assert_eq!(gap, None, "everything just ingested is here");
        assert_eq!(blake3::hash(&got), blake3::hash(&plaintext));
    }

    #[test]
    fn one_step_yields_at_most_one_chunk() {
        // The reason there is no length parameter: a caller cannot ask for four
        // megabytes, so peak memory is one chunk whatever the content's size.
        let fx = fixture("one-chunk");
        let space = space();
        let allow = |_: ContentAction<'_>| Ok(());
        let read = policy_with(&space, Arc::new(Keys), &allow);
        let plaintext = filler(2, CHUNK * 3);
        let content = ingest(&fx, &read, &space, &plaintext);

        let mut cursor = fx.cursor(&read, &content, Arc::new(NoSupply));
        let mut steps = 0usize;
        loop {
            match cursor.next(&read) {
                Advance::Yielded { cursor: next, span } => {
                    assert!(span.len() <= CHUNK, "{}", span.len());
                    steps += 1;
                    cursor = next;
                }
                Advance::Finished { .. } => break,
                other => panic!("unexpected step: {}", describe(&other)),
            }
        }
        assert_eq!(steps, 3, "three chunks, three steps");
    }

    #[test]
    fn a_missing_chunk_blocks_and_the_position_does_not_move() {
        let fx = fixture("hole");
        let space = space();
        let allow = |_: ContentAction<'_>| Ok(());
        let read = policy_with(&space, Arc::new(Keys), &allow);
        let plaintext = filler(3, CHUNK * 2 + 10);
        let content = ingest(&fx, &read, &space, &plaintext);
        let _saved = evict_chunk(&fx, &read, &content, 1);

        let supply = Recording::new(Gap::Fetching);
        let cursor = fx.cursor(&read, &content, supply.clone());
        let Advance::Yielded { cursor, span } = cursor.next(&read) else {
            panic!("the first chunk is here");
        };
        assert_eq!(span.len(), CHUNK);
        let boundary = cursor.position();

        let Advance::Blocked { cursor, gap } = cursor.next(&read) else {
            panic!("the second chunk is not here");
        };
        assert_eq!(gap, Gap::Fetching);
        assert_eq!(cursor.position(), boundary, "a hole moves nothing");
        assert_eq!(
            supply.asked.lock().unwrap().as_slice(),
            &[vec![1u32]],
            "asked for exactly the chunk that is missing"
        );

        // Asking again is the same position, not a short read that creeps.
        let Advance::Blocked { cursor, .. } = cursor.next(&read) else {
            panic!("still not here");
        };
        assert_eq!(cursor.position(), boundary);
    }

    #[test]
    fn a_blocked_read_resumes_at_the_same_byte_once_the_chunk_lands() {
        let fx = fixture("resume");
        let space = space();
        let allow = |_: ContentAction<'_>| Ok(());
        let read = policy_with(&space, Arc::new(Keys), &allow);
        let plaintext = filler(4, CHUNK * 2 + 64);
        let content = ingest(&fx, &read, &space, &plaintext);
        let (ciphertext, proof) = evict_chunk(&fx, &read, &content, 1);

        let cursor = fx.cursor(&read, &content, Recording::new(Gap::Fetching));
        let (first, gap) = read_on(cursor, &read);
        assert_eq!(gap, Some(Gap::Fetching));
        assert_eq!(first.len(), CHUNK);

        // The chunk arrives, verified against the committed root on the way in.
        fx.host
            .install_chunk(
                &read,
                &content,
                [77u8; 16],
                Acquisition::Keep,
                &proof,
                &ciphertext,
            )
            .expect("install");

        let cursor = fx.cursor(&read, &content, Arc::new(NoSupply));
        let Seek::Moved(cursor) = cursor.seek(first.len() as u64) else {
            panic!("seek back to where the hole was");
        };
        let (rest, gap) = read_on(cursor, &read);
        assert_eq!(gap, None, "the rest is here now");
        let mut whole = first;
        whole.extend_from_slice(&rest);
        assert_eq!(blake3::hash(&whole), blake3::hash(&plaintext));
    }

    #[test]
    fn sealed_content_refuses_rather_than_reporting_a_hole() {
        // No fetch changes an epoch this Station has no key for, so a caller
        // told "not here yet" would retry forever.
        let fx = fixture("sealed");
        let space = space();
        let allow = |_: ContentAction<'_>| Ok(());
        let read = policy_with(&space, Arc::new(Keys), &allow);
        let plaintext = filler(5, CHUNK + 5);
        let content = ingest(&fx, &read, &space, &plaintext);
        let _saved = evict_chunk(&fx, &read, &content, 1);

        let sealed = policy_with(&space, Arc::new(SealedKeys), &allow);
        let supply = Recording::new(Gap::Fetching);
        let cursor = fx.cursor(&sealed, &content, supply.clone());
        assert!(
            matches!(cursor.next(&sealed), Advance::Refused(Failure::Sealed)),
            "sealed content is a refusal, not a gap"
        );
        assert!(
            supply.asked.lock().unwrap().is_empty(),
            "and nothing was asked for, because nothing could help"
        );
    }

    #[test]
    fn the_key_is_resolved_again_for_every_chunk() {
        // Holding a cursor across a key revocation grants nothing: playback
        // stops at the next chunk, not at the next open.
        let fx = fixture("revoke");
        let space = space();
        let allow = |_: ContentAction<'_>| Ok(());
        let seeded = policy_with(&space, Arc::new(Keys), &allow);
        let plaintext = filler(6, CHUNK * 3);
        let content = ingest(&fx, &seeded, &space, &plaintext);

        let fading = policy_with(
            &space,
            Arc::new(FadingKeys {
                budget: 2,
                opened: AtomicUsize::new(0),
            }),
            &allow,
        );
        let mut cursor = fx.cursor(&fading, &content, Arc::new(NoSupply));
        let mut yielded = 0usize;
        loop {
            match cursor.next(&fading) {
                Advance::Yielded { cursor: next, .. } => {
                    yielded += 1;
                    cursor = next;
                }
                Advance::Refused(Failure::Sealed) => break,
                other => panic!("unexpected step: {}", describe(&other)),
            }
        }
        assert_eq!(yielded, 2, "two chunks of key, two chunks of playback");
    }

    #[test]
    fn every_step_asks_permission_again() {
        // A cursor is not a capability. Standing is asked for before any byte
        // exists, so a revocation lands on the next step.
        let fx = fixture("authz");
        let space = space();
        let allow = |_: ContentAction<'_>| Ok(());
        let permissive = policy_with(&space, Arc::new(Keys), &allow);
        let plaintext = filler(7, CHUNK * 2);
        let content = ingest(&fx, &permissive, &space, &plaintext);

        let asked = AtomicUsize::new(0);
        let counted = |action: ContentAction<'_>| {
            assert_eq!(action.capability(), "content.read");
            if asked.fetch_add(1, Ordering::SeqCst) < 2 {
                Ok(())
            } else {
                Err(b"content.read".to_vec())
            }
        };
        let counting = policy_with(&space, Arc::new(Keys), &counted);
        // Opening spends the first question.
        let cursor = fx.cursor(&counting, &content, Arc::new(NoSupply));
        let Advance::Yielded { cursor, .. } = cursor.next(&counting) else {
            panic!("the first step is allowed");
        };
        match cursor.next(&counting) {
            Advance::Refused(Failure::Denied { demand }) => assert_eq!(demand, b"content.read"),
            other => panic!("a revoked read must refuse: {}", describe(&other)),
        }
        assert_eq!(asked.load(Ordering::SeqCst), 3, "one question per pull");
    }

    #[test]
    fn a_refusal_ends_the_read_and_hands_back_no_cursor() {
        // The type is the guarantee: `Refused` carries a Failure and nothing
        // else, so there is no cursor to read on with.
        let fx = fixture("consumed");
        let space = space();
        let allow = |_: ContentAction<'_>| Ok(());
        let permissive = policy_with(&space, Arc::new(Keys), &allow);
        let content = ingest(&fx, &permissive, &space, b"payload");

        let deny = |_: ContentAction<'_>| Err(b"content.read".to_vec());
        let denied = policy_with(&space, Arc::new(Keys), &deny);
        let cursor = fx.cursor(&permissive, &content, Arc::new(NoSupply));
        match cursor.next(&denied) {
            Advance::Refused(Failure::Denied { demand }) => assert_eq!(demand, b"content.read"),
            other => panic!("expected a refusal: {}", describe(&other)),
        }
    }

    #[test]
    fn the_end_of_the_window_is_finished_and_never_a_short_read() {
        let fx = fixture("window");
        let space = space();
        let allow = |_: ContentAction<'_>| Ok(());
        let read = policy_with(&space, Arc::new(Keys), &allow);
        let plaintext = filler(8, CHUNK * 2);
        let content = ingest(&fx, &read, &space, &plaintext);

        // A window strictly inside one chunk, so its end is not a chunk edge.
        let cursor = ContentCursor::open_range(
            fx.host.clone(),
            &read,
            &content,
            100,
            50,
            Arc::new(NoSupply),
        )
        .expect("open");
        let Advance::Yielded { cursor, span } = cursor.next(&read) else {
            panic!("the window's bytes are here");
        };
        assert_eq!(span.offset(), 100);
        assert_eq!(span.bytes(), &plaintext[100..150]);
        let Advance::Finished { cursor } = cursor.next(&read) else {
            panic!("the window is done");
        };
        assert_eq!(cursor.remaining(), 0);
        // And it stays finished rather than degrading into a gap.
        assert!(matches!(cursor.next(&read), Advance::Finished { .. }));
    }

    #[test]
    fn a_seek_past_the_end_is_refused_and_keeps_the_cursor() {
        let fx = fixture("seek");
        let space = space();
        let allow = |_: ContentAction<'_>| Ok(());
        let read = policy_with(&space, Arc::new(Keys), &allow);
        let plaintext = filler(9, 4_000);
        let content = ingest(&fx, &read, &space, &plaintext);

        let cursor = fx.cursor(&read, &content, Arc::new(NoSupply));
        let Seek::Outside(cursor) = cursor.seek(4_001) else {
            panic!("past the end must refuse rather than clamp");
        };
        assert_eq!(cursor.position(), 0, "a refused seek moved nothing");
        assert_eq!(cursor.plaintext_len(), 4_000, "and still reports the total");

        let Seek::Moved(cursor) = cursor.seek(3_990) else {
            panic!("inside is fine");
        };
        let Advance::Yielded { span, .. } = cursor.next(&read) else {
            panic!("the tail is here");
        };
        assert_eq!(span.bytes(), &plaintext[3_990..]);

        // A window past the end is refused at the door, before a status line is
        // spent on it.
        assert_eq!(
            ContentCursor::open_range(
                fx.host.clone(),
                &read,
                &content,
                4_001,
                10,
                Arc::new(NoSupply)
            )
            .err(),
            Some(Failure::Bounds)
        );
    }

    #[test]
    fn readiness_settles_a_span_before_any_byte_is_opened() {
        let fx = fixture("readiness");
        let space = space();
        let allow = |_: ContentAction<'_>| Ok(());
        let read = policy_with(&space, Arc::new(Keys), &allow);
        let plaintext = filler(10, CHUNK * 5);
        let content = ingest(&fx, &read, &space, &plaintext);

        let cursor = fx.cursor(&read, &content, Arc::new(NoSupply));

        // One chunk's worth of question costs one probe, not five.
        let before = fx.host.cache().residency_probes();
        assert_eq!(cursor.readiness(&read, 0, 1_000), Readiness::Resident);
        assert_eq!(
            fx.host.cache().residency_probes() - before,
            1,
            "a question about one chunk must not cost the content"
        );

        // A hole is named, and named before anything is read.
        let _saved = evict_chunk(&fx, &read, &content, 3);
        assert_eq!(
            cursor.readiness(&read, CHUNK as u64 * 2, CHUNK as u64 * 2),
            Readiness::Absent { chunk: 3 }
        );
        assert_eq!(
            cursor.readiness(&read, 0, CHUNK as u64),
            Readiness::Resident,
            "a span that misses the hole is still ready"
        );

        // Past the end is a refusal a caller turns into a 416.
        assert_eq!(
            cursor.readiness(&read, plaintext.len() as u64 + 1, 10),
            Readiness::Refused(Failure::Bounds)
        );

        // And the two answers that must arrive before a body starts.
        let deny = |_: ContentAction<'_>| Err(b"content.read".to_vec());
        let denied = policy_with(&space, Arc::new(Keys), &deny);
        assert!(matches!(
            cursor.readiness(&denied, 0, 10),
            Readiness::Refused(Failure::Denied { .. })
        ));
        let sealed = policy_with(&space, Arc::new(SealedKeys), &allow);
        assert_eq!(
            cursor.readiness(&sealed, 0, 10),
            Readiness::Refused(Failure::Sealed)
        );
    }

    #[test]
    fn nothing_attached_that_could_fetch_says_so() {
        // A Station that will never fetch is a real thing, and "nothing can
        // fetch this" is an answer where waiting forever is not.
        let fx = fixture("nosupply");
        let space = space();
        let allow = |_: ContentAction<'_>| Ok(());
        let read = policy_with(&space, Arc::new(Keys), &allow);
        let plaintext = filler(11, CHUNK + 1);
        let content = ingest(&fx, &read, &space, &plaintext);
        let _saved = evict_chunk(&fx, &read, &content, 0);

        let cursor = fx.cursor(&read, &content, Arc::new(NoSupply));
        let Advance::Blocked { gap, .. } = cursor.next(&read) else {
            panic!("nothing can fetch it");
        };
        assert_eq!(gap, Gap::Unsupplied);
    }

    #[test]
    fn a_supply_that_could_not_be_asked_is_not_nobody_has_it() {
        // The false-disconnection defect, one layer down. Unmeasured is absent,
        // never zero — and an absence has to say which kind it is.
        let fx = fixture("unasked");
        let space = space();
        let allow = |_: ContentAction<'_>| Ok(());
        let read = policy_with(&space, Arc::new(Keys), &allow);
        let plaintext = filler(12, CHUNK + 1);
        let content = ingest(&fx, &read, &space, &plaintext);
        let _saved = evict_chunk(&fx, &read, &content, 0);

        for answer in [Gap::Unasked, Gap::Unoffered, Gap::Refused] {
            let cursor = fx.cursor(&read, &content, Recording::new(answer));
            let Advance::Blocked { gap, .. } = cursor.next(&read) else {
                panic!("the chunk is not here");
            };
            assert_eq!(gap, answer, "the kind of absence survives the seam");
        }
        assert_ne!(Gap::Unasked, Gap::Unoffered);
    }

    #[test]
    fn dropping_a_cursor_abandons_what_it_asked_for() {
        let fx = fixture("drop");
        let space = space();
        let allow = |_: ContentAction<'_>| Ok(());
        let read = policy_with(&space, Arc::new(Keys), &allow);
        let plaintext = filler(13, CHUNK + 1);
        let content = ingest(&fx, &read, &space, &plaintext);
        let _saved = evict_chunk(&fx, &read, &content, 0);

        // A cursor that never asked for anything has nothing to let go of.
        let supply = Recording::new(Gap::Fetching);
        let idle = fx.cursor(&read, &content, supply.clone());
        drop(idle);
        assert_eq!(supply.abandoned.load(Ordering::SeqCst), 0);

        // One that did is cleaned up without the caller remembering to.
        let cursor = fx.cursor(&read, &content, supply.clone());
        let Advance::Blocked { cursor, .. } = cursor.next(&read) else {
            panic!("the chunk is not here");
        };
        let operation = cursor.operation;
        fx.host
            .cache()
            .hold_operation(operation, [5u8; 32])
            .expect("a lease the supply would have taken");
        fx.host
            .cache()
            .append_staged(&operation, 0, 0, b"partial")
            .expect("staging the supply would have written");

        drop(cursor);
        assert_eq!(supply.abandoned.load(Ordering::SeqCst), 1);
        assert!(
            !fx.host.cache().is_held(&[5u8; 32]).expect("held"),
            "a disconnected reader releases its leases"
        );
        assert_eq!(
            fx.host.cache().staged_len(&operation, 0),
            0,
            "and its staging"
        );
    }

    #[test]
    fn an_unknown_content_is_refused_at_the_door() {
        let fx = fixture("unknown");
        let space = space();
        let allow = |_: ContentAction<'_>| Ok(());
        let read = policy_with(&space, Arc::new(Keys), &allow);
        let never_heard_of = ContentRef {
            content_id: [3u8; 32],
        };
        assert_eq!(
            ContentCursor::open(fx.host.clone(), &read, &never_heard_of, Arc::new(NoSupply)).err(),
            Some(Failure::Unknown)
        );
    }
}
